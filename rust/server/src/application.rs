//! Durable turn application shared by the CLI and the WebSocket adapter.
use crate::{
    Error, Result,
    capability::Capability,
    config::{Model, Settings},
    provider::Delta,
    store::{Admission, Event, Store, terminal, text},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Notify, mpsc, watch};

const BATCH_DELAY: Duration = Duration::from_millis(25);
const BATCH_EVENTS: usize = 32;
const BATCH_BYTES: usize = 16 * 1024;

struct EventBatch {
    events: Vec<Event>,
    bytes: usize,
    deadline: tokio::time::Instant,
}
impl EventBatch {
    fn new() -> Self {
        Self {
            events: Vec::new(),
            bytes: 0,
            deadline: tokio::time::Instant::now(),
        }
    }
    fn push(&mut self, kind: &str, content: String) {
        if self.events.is_empty() {
            self.deadline = tokio::time::Instant::now() + BATCH_DELAY;
        }
        self.bytes += content.len();
        self.events.push(Event {
            kind: kind.into(),
            content,
            metadata: json!({}),
        });
    }
    fn full(&self) -> bool {
        self.events.len() >= BATCH_EVENTS || self.bytes >= BATCH_BYTES
    }
    async fn flush(&mut self, app: &Application, turn: &str, content: &mut String) -> Result<()> {
        if self.events.is_empty() {
            return Ok(());
        }
        let events = std::mem::take(&mut self.events);
        self.bytes = 0;
        let store = app.store.clone();
        let turn = turn.to_owned();
        let metric_turn = turn.clone();
        let writing = Instant::now();
        let (events, timing) = blocking(move || {
            let timing = store.emit_batch_timed(&turn, &events)?;
            Ok((events, timing))
        })
        .await?;
        app.metrics.batch(
            &metric_turn,
            events.len(),
            events.iter().any(|e| e.kind == "content"),
            crate::metrics::elapsed(writing),
            timing,
        );
        // Only committed text may enter the final assistant message.
        for event in events {
            if event.kind == "content" {
                content.push_str(&event.content);
            }
        }
        app.notify_turn(&metric_turn);
        Ok(())
    }
}

pub async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| Error::conflict("Native storage task failed."))?
}
pub struct Application {
    pub accounts: Arc<crate::accounts::Accounts>,
    pub(crate) tenants: Mutex<HashMap<String, Arc<Application>>>,
    pub settings: Arc<Settings>,
    pub store: Store,
    pub(crate) diagnostics: crate::diagnostics::Runtime,
    pub browser_jobs: crate::browser_testing::Jobs,
    pub(crate) voice: Arc<crate::voice::Runtime>,
    pub codex_login: crate::oauth_web::WebLogin,
    pub memory_maintenance: crate::memory_maintenance::Scheduler,
    pub changed: Notify,
    turn_changes: crate::notifications::TurnChanges,
    pub metrics: crate::metrics::RuntimeMetrics,
    stopping: watch::Sender<bool>,
    owner: String,
    admission: Mutex<()>,
    running: Mutex<HashMap<String, watch::Sender<bool>>>,
}
impl Application {
    pub fn open(home: &Path) -> Result<Arc<Self>> {
        let settings = Arc::new(Settings::open(home)?);
        let accounts = Arc::new(crate::accounts::Accounts::open(&settings)?);
        crate::ocr::reset();
        Self::with_settings(settings, accounts)
    }
    fn with_settings(
        settings: Arc<Settings>,
        accounts: Arc<crate::accounts::Accounts>,
    ) -> Result<Arc<Self>> {
        let db = settings.user_root.join("chat_history.db");
        crate::config::safe_path(&settings.user_root, &db)?;
        let store = Store::open(&db)?;
        store.recover()?;
        Ok(Arc::new(Self {
            accounts,
            tenants: Mutex::new(HashMap::new()),
            diagnostics: Default::default(),
            browser_jobs: Default::default(),
            voice: Default::default(),
            codex_login: Default::default(),
            memory_maintenance: Default::default(),
            settings,
            store,
            owner: format!("rust-server:{}", uuid::Uuid::new_v4()),
            changed: Notify::new(),
            turn_changes: Default::default(),
            metrics: crate::metrics::RuntimeMetrics::new(
                std::env::var("DEEPTUTOR_METRICS").is_ok_and(|v| v == "1"),
            ),
            stopping: watch::channel(false).0,
            admission: Mutex::new(()),
            running: Mutex::new(HashMap::new()),
        }))
    }
    pub(crate) async fn for_account(self: &Arc<Self>, user: &Value) -> Result<Arc<Self>> {
        if user["role"] == "admin" {
            return Ok(self.clone());
        }
        let id = text(user, "id").to_owned();
        let mut tenants = self.tenants.lock().await;
        if let Some(app) = tenants.get(&id) {
            return Ok(app.clone());
        }
        let settings = self.settings.clone();
        let accounts = self.accounts.clone();
        let key = id.clone();
        let app = blocking(move || {
            Self::with_settings(Arc::new(Settings::for_account(settings, &key)?), accounts)
        })
        .await?;
        tenants.insert(id, app.clone());
        Ok(app)
    }
    pub async fn start(self: &Arc<Self>, request: Value) -> Result<Admission> {
        self.resume_pending().await?;
        let received = Instant::now();
        if *self.stopping.borrow() {
            return Err(Error::conflict("Server is shutting down."));
        }
        validate(&request)?;
        let waiting = Instant::now();
        let _guard = self.admission.lock().await;
        let admission_wait_ms = crate::metrics::elapsed(waiting);
        if *self.stopping.borrow() {
            return Err(Error::conflict("Server is shutting down."));
        }
        let submission = text(&request, "client_submission_id");
        if !submission.is_empty()
            && !self
                .operation("submission_turn", json!({"submission_id":submission}))
                .await?
                .is_null()
        {
            let store = self.store.clone();
            let owner = self.owner.clone();
            return blocking(move || store.admit(&request, &owner)).await;
        }
        let mut selection = request["llm_selection"].clone();
        let store = self.store.clone();
        let sid = text(&request, "session_id").to_owned();
        let workspace = text(&request, "workspace_id").to_owned();
        let session = if sid.is_empty() {
            json!({})
        } else {
            blocking(move || store.session(&sid, &workspace)).await?
        };
        let preferences = &session["preferences"];
        if selection.is_null() && !preferences["llm_selection"].is_null() {
            selection = preferences["llm_selection"].clone();
        }
        // Resolve fallible settings before admission so a malformed settings file
        // cannot leave a persisted turn running without a worker.
        let settings = self.settings.clone();
        let interface = blocking(move || crate::settings_api::interface(&settings)).await?;
        let capability = Capability::parse(text(&request, "capability"))?;
        let account_policy =
            if let (Some(root), Some(id)) = (&self.settings.shared, &self.settings.account_id) {
                let user = self.accounts.user(id)?;
                let policy = crate::accounts_api::policy(root, &user)?;
                if capability == Capability::TestCases {
                    return Err(crate::accounts::forbidden());
                }
                if !policy.is_null()
                    && (capability != Capability::Chat
                        || !policy["allowed_capabilities"]
                            .as_array()
                            .is_some_and(|a| a.iter().any(|c| c == "chat")))
                {
                    return Err(Error::new(
                        axum::http::StatusCode::FORBIDDEN,
                        "learning_policy",
                        "This capability is disabled for your account.",
                    ));
                }
                policy
            } else {
                Value::Null
            };
        let language =
            crate::language::resolve(&request, preferences, &interface["response_language"])?;
        let mut instructions =
            crate::language::instructions(&request, preferences, &interface["response_language"])?;
        if !account_policy.is_null() {
            instructions.push_str("\n\nAct as a patient teacher. Explain with age-appropriate examples, guide the learner step by step, and avoid instructions for unsafe activities. The teacher persona is fixed by the administrator.");
        }
        let settings = self.settings.clone();
        let workspace = text(&request, "workspace_id").to_owned();
        let workspace_instructions =
            blocking(move || crate::workspaces::instructions(&settings, &workspace)).await?;
        if !workspace_instructions.is_empty() {
            instructions.push_str("\n\nWorkspace instructions:\n");
            instructions.push_str(&workspace_instructions);
        }
        let prompt = capability.prompt(&language);
        if !prompt.is_empty() {
            instructions.push_str("\n\n");
            instructions.push_str(prompt);
        }
        let timeout = interface["chat_response_timeout"]
            .as_u64()
            .unwrap_or(180)
            .clamp(30, 1800);
        let effort = selection["reasoning_effort"].clone();
        let settings = self.settings.clone();
        let model = blocking(move || settings.model(&selection)).await?;
        let store = self.store.clone();
        let owner = self.owner.clone();
        let settings = self.settings.clone();
        let scoped_store = self.store.clone();
        let scope = text(&request, "workspace_id").to_owned();
        let kbs = request["knowledge_bases"].clone();
        let optional = request["tools"].clone();
        let enabled = interface["enabled_optional_tools"].clone();
        let study_target = request["study_target"].clone();
        let document_target = request["document_target"].clone();
        let memory_target = request["memory_target"].clone();
        let reading_target = request["reading_target"].clone();
        let flags = blocking(move || {
            let mut flags = crate::learning::bootstrap(&settings, &scoped_store, &scope)?;
            if matches!(capability, Capability::Chat | Capability::VideoLesson) {
                flags.video_lesson = crate::video::lesson_available();
                if capability == Capability::VideoLesson && !flags.video_lesson {
                    return Err(Error::unsupported(
                        "The video lesson runtime is missing. Install a complete Lorevia runtime.",
                    ));
                }
                if capability == Capability::Chat {
                    flags.videogen = crate::video::select(&settings, &enabled, &optional)?;
                }
            }
            flags.search = crate::search::select(&settings, &enabled, &optional)?;
            flags.browser = if capability == Capability::TestCases {
                Some(crate::browser_testing::test_selection(
                    &scoped_store.connect()?,
                    &scope,
                )?)
            } else {
                crate::browser_testing::selection(&scoped_store.connect()?, &scope, &optional)?
            };
            flags.knowledge_bases =
                crate::knowledge::selection(&scoped_store.connect()?, &scope, &kbs)?;
            flags.study_target =
                crate::study::target(&scoped_store.connect()?, &scope, &study_target, capability)?;
            flags.proposal = if capability == Capability::ReadingTutor {
                if !document_target.is_null() || !memory_target.is_null() {
                    return Err(Error::invalid(
                        "Select only the reading excerpt for reading discussion.",
                    ));
                }
                Some(crate::reading::tutor_context(
                    &scoped_store.connect()?,
                    &scope,
                    &reading_target,
                )?)
            } else {
                if !reading_target.is_null() {
                    return Err(Error::invalid(
                        "Reading excerpts require the reading tutor capability.",
                    ));
                }
                crate::study::proposal(
                    &scoped_store.connect()?,
                    &scope,
                    &document_target,
                    &memory_target,
                    capability,
                )?
            };
            if let (Some(root), Some(id)) = (&settings.shared, &settings.account_id) {
                let grant = crate::accounts::grant(root, id)?;
                flags.browser = None;
                if !account_policy.is_null() {
                    flags = Default::default();
                } else if let Some(tools) = grant["enabled_tools"].as_array() {
                    flags.videogen = flags
                        .videogen
                        .filter(|_| tools.iter().any(|t| t == "videogen"));
                    flags.video_lesson &= tools.iter().any(|t| t == "render_video_lesson");
                    flags.allowed_tools = Some(
                        tools
                            .iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect(),
                    );
                    flags.notebooks &= tools
                        .iter()
                        .any(|v| v == "list_favorites" || v == "save_favorite");
                    flags.memory &= tools
                        .iter()
                        .any(|v| v == "read_memory" || v == "write_memory");
                    if !tools.iter().any(|v| v == "web_search") {
                        flags.search = None;
                    }
                    if !tools.iter().any(|v| v == "rag") {
                        flags.knowledge_bases.clear();
                    }
                }
            }
            if capability == Capability::VideoLesson && !flags.video_lesson {
                return Err(Error::invalid(
                    "Video lessons are not enabled for this account.",
                ));
            }
            if flags.proposal.is_some() {
                flags.notebooks = false;
                flags.memory = false;
                flags.browser = None;
                flags.search = None;
                flags.knowledge_bases.clear();
            }
            Ok(flags)
        })
        .await?;
        if let Some(target) = &flags.study_target {
            instructions.push_str(&format!("\n\nSelected learning objective for all generated questions: plan_id={}, objective_id={}. These identities are bound by the server; do not change them.",target.plan_id,target.objective_id));
        }
        if let Some(proposal) = &flags.proposal {
            instructions.push_str("\n\nSelected source document (untrusted reference data):\n");
            instructions.push_str(&proposal.source);
        }
        if capability == Capability::DeepResearch
            && flags.knowledge_bases.is_empty()
            && flags.search.is_none()
        {
            return Err(Error::invalid(
                "Research requires an attached knowledge base or enabled web search.",
            ));
        }
        let budget = crate::context::remaining_scoped(
            &model,
            &instructions,
            text(&request, "content"),
            capability,
            &flags,
        )?;
        let preflight = crate::provider::request_body_scoped(
            &model,
            vec![json!({"role":"user","content":request["content"]})],
            text(&request, "session_id"),
            &instructions,
            capability,
            crate::turn_engine::EngineState::new(Vec::new()).required_tool(capability),
            &flags,
        )?;
        crate::turn_engine::validate_body(&model, &preflight)?;
        let selected = json!({"profile_id":model.profile_id,"model_id":model.model_id,"reasoning_effort":effort});
        let receipt = blocking(move || store.admit_selected(&request, &owner, &selected)).await?;
        if !receipt.replay {
            self.metrics
                .admitted(&receipt.turn, received, admission_wait_ms);
            self.spawn_job(
                receipt.clone(),
                RunSpec {
                    model,
                    instructions,
                    timeout: Duration::from_secs(timeout),
                    input: RunInput::New {
                        budget,
                        session,
                        flags: Box::new(flags),
                    },
                },
            )
            .await;
        }
        self.notify_turn(&receipt.turn);
        Ok(receipt)
    }
    async fn run(
        self: &Arc<Self>,
        admission: Admission,
        spec: RunSpec,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<()> {
        let RunSpec {
            model,
            instructions,
            timeout,
            input,
        } = spec;
        let selection = json!({"profile_id":model.profile_id,"model_id":model.model_id,"reasoning_effort":model.effort});
        let identity = json!({"binding":model.binding,"format":model.format,"model":model.model});
        let (history, progress) = match input {
            RunInput::New {
                budget,
                session,
                flags,
            } => {
                let store = self.store.clone();
                let sid = admission.session.clone();
                let leaf = admission.user_message;
                let history = blocking(move || {
                    crate::context::assemble(store.branch(&sid, leaf)?, &session, budget)
                })
                .await;
                match history {
                    Ok((messages, stats)) => (
                        Ok({
                            let mut engine = crate::turn_engine::EngineState::new(messages);
                            engine.tools = *flags;
                            engine
                        }),
                        Progress {
                            content: String::new(),
                            usage: json!({}),
                            output_bytes: 0,
                            context_stats: Some(stats),
                        },
                    ),
                    Err(error) => (
                        Err(error),
                        Progress {
                            content: String::new(),
                            usage: json!({}),
                            output_bytes: 0,
                            context_stats: None,
                        },
                    ),
                }
            }
            RunInput::Resumed(checkpoint) => {
                let engine = checkpoint.resume_engine(&model);
                (engine, checkpoint.progress)
            }
        };
        let Progress {
            mut content,
            mut usage,
            mut output_bytes,
            context_stats,
        } = progress;
        let mut batch = EventBatch::new();
        let mut paused = false;
        let mut failure = None;
        let mut status = "completed";
        if let Err(error) = history.as_ref() {
            failure = Some(error.message.clone());
            status = "failed";
        } else {
            self.metrics.provider_started(&admission.turn);
            let deadline = tokio::time::sleep(timeout);
            tokio::pin!(deadline);
            let (tx, mut rx) = mpsc::channel(32);
            let engine = history.unwrap();
            let future = crate::turn_engine::run(
                self.settings.clone(),
                model,
                engine,
                instructions.clone(),
                crate::tools::ToolRegistry {
                    settings: self.settings.clone(),
                    store: self.store.clone(),
                    admission: admission.clone(),
                },
                tx,
            );
            tokio::pin!(future);
            let mut media_remaining = None;
            let mut provider_result: Option<Result<()>> = None;
            loop {
                tokio::select! {
                    biased;
                    _=cancel.changed()=>{status="cancelled";break;},
                    _=&mut deadline=>{status="failed";failure=Some("Model response timed out.".into());break;},
                    _=tokio::time::sleep_until(batch.deadline),if !batch.events.is_empty()=>{
                        if let Err(error)=batch.flush(self,&admission.turn,&mut content).await {status="failed";failure=Some(error.message);break;}
                    },
                    delta=rx.recv()=>{
                        match delta {
                            Some(Delta::Content(delta))=>{
                                self.metrics.delta(&admission.turn,true,rx.len()+1);
                                output_bytes=output_bytes.saturating_add(delta.len());
                                if output_bytes>16*1024*1024 {status="failed";failure=Some("Model response exceeds size limit.".into());break;}
                                batch.push("content",delta);
                            },
                            Some(Delta::Thinking(delta))=>{
                                self.metrics.delta(&admission.turn,false,rx.len()+1);
                                output_bytes=output_bytes.saturating_add(delta.len());
                                if output_bytes>16*1024*1024 {status="failed";failure=Some("Model response exceeds size limit.".into());break;}
                                batch.push("thinking",delta);
                            },
                            Some(Delta::Pause(pause))=>{
                                batch.flush(self,&admission.turn,&mut content).await?;
                                let checkpoint=Checkpoint {
                                    admission:admission.clone(),
                                    pause:*pause,
                                    progress:Progress {
                                        content:content.clone(),
                                        usage:usage.clone(),
                                        output_bytes,
                                        context_stats:context_stats.clone(),
                                    },
                                    selection:selection.clone(),
                                    identity:identity.clone(),
                                    instructions:instructions.clone(),
                                    timeout:deadline.deadline().saturating_duration_since(tokio::time::Instant::now()),
                                    reply:None,
                                };
                                let store=self.store.clone();
                                match blocking(move||store.park(&checkpoint)).await {
                                    Ok(())=>{paused=true;self.metrics.waiting(&admission.turn);self.notify_turn(&admission.turn);},
                                    Err(error)=>{status="failed";failure=Some(error.message);},
                                }
                                break;
                            },
                            Some(Delta::Usage(value))=>crate::turn_engine::accumulate_usage(&mut usage,value),
                            Some(Delta::RequestStatus { request_id, trace_id, http_status })=>{
                                self.metrics.request(&admission.turn, &request_id, &trace_id, http_status);
                            },
                            Some(Delta::ToolStart { invocation, call, ack })=>{
                                if matches!(call.name.as_str(),"render_video_lesson"|"videogen") {
                                    let now=tokio::time::Instant::now();
                                    media_remaining=Some(deadline.deadline().saturating_duration_since(now));
                                    deadline.as_mut().reset(now+Duration::from_secs(610));
                                }
                                let result=async {
                                    batch.flush(self,&admission.turn,&mut content).await?;
                                    let store=self.store.clone();let turn=admission.turn.clone();
                                    blocking(move || store.start_tool(&turn,&invocation,&call)).await
                                }.await;
                                let failed=result.is_err();
                                let _=ack.send(result);
                                self.notify_turn(&admission.turn);
                                if failed {status="failed";failure=Some("Tool admission could not be persisted.".into());break;}
                            },
                            Some(Delta::ToolEnd { invocation, result, ack })=>{
                                if let Some(remaining)=media_remaining.take() {deadline.as_mut().reset(tokio::time::Instant::now()+remaining);}
                                let store=self.store.clone();let turn=admission.turn.clone();
                                let result=blocking(move || store.commit_tool(&turn,&invocation,result)).await;
                                let failed=result.is_err();
                                let _=ack.send(result);
                                self.notify_turn(&admission.turn);
                                if failed {status="failed";failure=Some("Tool result could not be persisted.".into());break;}
                            },
                            None=>{if let Some(result)=provider_result.take(){if let Err(error)=result {status="failed";failure=Some(error.message);}break;}}
                        }
                    },
                    result=&mut future,if provider_result.is_none()=>{provider_result=Some(result);}
                }
                if batch.full()
                    && let Err(error) = batch.flush(self, &admission.turn, &mut content).await
                {
                    status = "failed";
                    failure = Some(error.message);
                    break;
                }
                // Drain every queued token before committing RESULT/DONE, even
                // when the HTTP future completed in the same scheduler tick.
                if provider_result.is_some() && rx.is_closed() && rx.is_empty() {
                    if let Some(Err(error)) = provider_result.take() {
                        status = "failed";
                        failure = Some(error.message);
                    }
                    break;
                }
            }
        }
        if paused {
            return Ok(());
        }
        // Completion, cancellation, timeout and provider errors all flush the
        // accepted tail before the terminal transaction. Uncommitted text is
        // never published or included in the assistant message on write failure.
        if let Err(error) = batch.flush(self, &admission.turn, &mut content).await {
            status = "failed";
            failure = Some(error.message);
        }
        let store = self.store.clone();
        let metric_turn = admission.turn.clone();
        let final_status = status;
        let status = status.to_string();
        blocking(move || {
            store.finish_with_context(
                &admission,
                &status,
                &content,
                usage,
                context_stats,
                failure.as_deref(),
            )
        })
        .await?;
        self.metrics.finished(&metric_turn, final_status);
        self.notify_turn(&metric_turn);
        Ok(())
    }
    async fn spawn_job(self: &Arc<Self>, admission: Admission, spec: RunSpec) {
        let (cancel, receiver) = watch::channel(false);
        self.running
            .lock()
            .await
            .insert(admission.turn.clone(), cancel);
        let app = self.clone();
        tokio::spawn(async move {
            let turn = admission.turn.clone();
            if let Err(error) = app.run(admission, spec, receiver).await {
                eprintln!("Turn {turn} persistence failed: {}", error.code);
                app.metrics.finished(&turn, "persistence_failed");
            }
            app.running.lock().await.remove(&turn);
            app.notify_turn(&turn);
        });
    }
    async fn wait_idle(&self, turn: &str) {
        loop {
            let wake = self.changed.notified();
            tokio::pin!(wake);
            wake.as_mut().enable();
            if !self.running.lock().await.contains_key(turn) {
                break;
            }
            wake.await;
        }
    }
    pub async fn resume_pending(self: &Arc<Self>) -> Result<()> {
        self.memory_maintenance.start(self).await;
        let _guard = self.admission.lock().await;
        self.start_ready().await
    }
    async fn start_ready(self: &Arc<Self>) -> Result<()> {
        if *self.stopping.borrow() {
            return Err(Error::conflict("Server is shutting down."));
        }
        let store = self.store.clone();
        let checkpoints = blocking(move || store.ready_checkpoints()).await?;
        for checkpoint in checkpoints {
            let admission = checkpoint.admission.clone();
            let settings = self.settings.clone();
            let resolved = blocking(move || {
                let resolved = checkpoint.resume_input(&settings);
                Ok((checkpoint, resolved))
            })
            .await?;
            let (checkpoint, resolved) = resolved;
            let model = match resolved {
                Ok((model, _)) => model,
                Err(error) => {
                    let store = self.store.clone();
                    let failed_turn = admission.turn.clone();
                    blocking(move || {
                        store.finish_with_context(
                            &admission,
                            "failed",
                            &checkpoint.progress.content,
                            checkpoint.progress.usage,
                            checkpoint.progress.context_stats,
                            Some(&error.message),
                        )
                    })
                    .await?;
                    self.notify_turn(&failed_turn);
                    continue;
                }
            };
            let store = self.store.clone();
            let turn = admission.turn.clone();
            let owner = self.owner.clone();
            blocking(move || store.claim_checkpoint(&turn, &owner)).await?;
            let instructions = checkpoint.instructions.clone();
            let timeout = checkpoint.timeout;
            self.spawn_job(
                admission,
                RunSpec {
                    model,
                    instructions,
                    timeout,
                    input: RunInput::Resumed(Box::new(checkpoint)),
                },
            )
            .await;
        }
        Ok(())
    }
    pub async fn pending_question(&self, turn: &str, workspace: &str) -> Result<Value> {
        self.turn(turn, workspace).await?;
        let store = self.store.clone();
        let turn = turn.to_owned();
        blocking(move || {
            Ok(store
                .checkpoint(&turn)?
                .filter(|c| c.reply.is_none())
                .map(|c| json!({"tool_call_id":c.pause.call.id,"ask_user":c.pause.question}))
                .unwrap_or(Value::Null))
        })
        .await
    }
    pub async fn reply_command(self: &Arc<Self>, command: Value, workspace: &str) -> Result<Value> {
        crate::ask_user::command_identity(&command)?;
        let _guard = self.admission.lock().await;
        if *self.stopping.borrow() {
            return Err(Error::conflict("Server is shutting down."));
        }
        let turn = text(&command, "turn_id");
        let state = self.turn(turn, workspace).await?;
        if state["status"] == "waiting_input" {
            self.wait_idle(turn).await;
        }
        let store = self.store.clone();
        let settings = self.settings.clone();
        let workspace = workspace.to_string();
        let receipt = blocking(move || store.accept_reply(&command, &workspace, &settings)).await?;
        self.start_ready().await?;
        self.notify_turn(text(&receipt, "turn_id"));
        Ok(receipt)
    }
    pub async fn emit(&self, turn: &str, kind: &str, content: &str, metadata: Value) -> Result<()> {
        let store = self.store.clone();
        let turn = turn.to_owned();
        let notified_turn = turn.clone();
        let kind = kind.to_owned();
        let content = content.to_owned();
        blocking(move || store.emit(&turn, &kind, &content, metadata)).await?;
        self.notify_turn(&notified_turn);
        Ok(())
    }
    pub fn turn_changed(&self, turn: &str) -> Arc<Notify> {
        self.turn_changes.subscribe(turn)
    }
    fn notify_turn(&self, turn: &str) {
        self.turn_changes.notify(turn);
        // Internal lifecycle waiters are few; network subscribers use the
        // scoped signal and no longer query SQLite for other turns' writes.
        self.changed.notify_waiters();
    }
    pub async fn operation(&self, name: &str, args: Value) -> Result<Value> {
        let store = self.store.clone();
        let name = name.to_owned();
        blocking(move || store.operation(&name, args)).await
    }
    pub async fn turn(&self, turn: &str, workspace: &str) -> Result<Value> {
        let value = self.operation("get_turn", json!({"turn_id":turn})).await?;
        if value.is_null() {
            return Err(Error::missing());
        }
        let store = self.store.clone();
        let sid = text(&value, "session_id").to_string();
        let workspace = workspace.to_string();
        blocking(move || store.session(&sid, &workspace)).await?;
        Ok(value)
    }
    pub async fn events(&self, turn: &str, after: i64) -> Result<Vec<Value>> {
        let events = self
            .operation(
                "get_events",
                json!({"turn_id":turn,"after_seq":after,"limit":256}),
            )
            .await?;
        Ok(events
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|mut event| {
                event["protocol_version"] = json!("2.0");
                event
            })
            .collect())
    }
    pub async fn cancel(&self, turn: &str, workspace: &str) -> Result<bool> {
        let value = self.turn(turn, workspace).await?;
        if terminal(text(&value, "status")) {
            return Ok(false);
        }
        if value["status"] == "waiting_input" || value["status"] == "queued" {
            self.wait_idle(turn).await;
            let store = self.store.clone();
            let target = turn.to_string();
            let cancelled = blocking(move || {
                let Some(checkpoint) = store.checkpoint(&target)? else {
                    return Ok(false);
                };
                store.finish_with_context(
                    &checkpoint.admission,
                    "cancelled",
                    &checkpoint.progress.content,
                    checkpoint.progress.usage,
                    checkpoint.progress.context_stats,
                    None,
                )?;
                Ok(true)
            })
            .await?;
            if cancelled {
                self.metrics.finished(turn, "cancelled");
            }
            self.notify_turn(turn);
            return Ok(cancelled);
        }
        Ok(self
            .running
            .lock()
            .await
            .get(turn)
            .is_some_and(|sender| sender.send(true).is_ok()))
    }
    pub async fn cancel_command(
        &self,
        turn: &str,
        workspace: &str,
        command: &str,
    ) -> Result<Value> {
        if command.is_empty() || command.len() > 200 {
            return Err(Error::invalid("Invalid command identity."));
        }
        let _guard = self.admission.lock().await;
        self.turn(turn, workspace).await?;
        let store = self.store.clone();
        let id = command.to_owned();
        let target = turn.to_owned();
        if let Some(receipt) = blocking(move || store.command_receipt(&id, &target)).await? {
            return Ok(receipt);
        }
        let accepted = self.cancel(turn, workspace).await?;
        let receipt = json!({"type":"command_ack","command_id":command,"command_type":"cancel_turn","turn_id":turn,"accepted":accepted,"error_code":"","message":"","protocol_version":"2.0"});
        let store = self.store.clone();
        let id = command.to_owned();
        let target = turn.to_owned();
        let saved = receipt.clone();
        blocking(move || store.save_command_receipt(&id, &target, &saved)).await?;
        Ok(receipt)
    }
    pub fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.stopping.subscribe()
    }
    pub async fn shutdown(&self) {
        let tenants = self
            .tenants
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for tenant in tenants {
            tenant.shutdown_local().await;
        }
        self.shutdown_local().await;
    }
    async fn shutdown_local(&self) {
        for cancel in self.browser_jobs.running.lock().await.values() {
            let _ = cancel.send(true);
        }
        let admission = self.admission.lock().await;
        self.stopping.send_replace(true);
        crate::ocr::stop();
        for cancel in self.running.lock().await.values() {
            let _ = cancel.send(true);
        }
        drop(admission);
        let wait = async {
            loop {
                let wake = self.changed.notified();
                if self.running.lock().await.is_empty() {
                    break;
                }
                wake.await;
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(10), wait).await;
        crate::browser_testing::drain().await;
        self.codex_login.shutdown(self.settings.clone()).await;
        self.memory_maintenance.drain().await;
    }
}
fn validate(request: &Value) -> Result<()> {
    let map = request
        .as_object()
        .ok_or_else(|| Error::invalid("Expected a turn object."))?;
    let content = request["content"]
        .as_str()
        .ok_or_else(|| Error::invalid("Message content is required."))?;
    if let Some(selection) = request.get("llm_selection").filter(|v| !v.is_null())
        && (!selection.is_object()
            || text(selection, "profile_id").is_empty()
            || text(selection, "model_id").is_empty()
            || selection
                .as_object()
                .unwrap()
                .keys()
                .any(|k| !matches!(k.as_str(), "profile_id" | "model_id" | "reasoning_effort"))
            || selection
                .get("reasoning_effort")
                .is_some_and(|v| !v.is_null() && !v.is_string()))
    {
        return Err(Error::invalid("Invalid model selection."));
    }
    if let Some(value) = request.get("reply_language_override") {
        crate::language::validate_override(value)?;
    }
    for key in ["capability", "language"] {
        if request
            .get(key)
            .is_some_and(|v| !v.is_null() && !v.is_string())
        {
            return Err(Error::invalid("Invalid turn field type."));
        }
    }
    if content.trim().is_empty() || content.len() > 1024 * 1024 {
        return Err(Error::invalid("Message must contain 1 to 1048576 bytes."));
    }
    Capability::parse(text(request, "capability"))?;
    for key in ["session_id", "workspace_id", "client_submission_id"] {
        if let Some(v) = map.get(key).filter(|v| !v.is_null())
            && (!v.is_string() || v.as_str().unwrap().len() > 200)
        {
            return Err(Error::invalid("Invalid conversation identity."));
        }
    }
    // Never silently drop context/tools or run an advanced request as plain chat.
    for (key, value) in map {
        if [
            "type",
            "protocol_version",
            "content",
            "capability",
            "session_id",
            "workspace_id",
            "client_submission_id",
            "llm_selection",
            "parent_message_id",
            "language",
            "reply_language_override",
            "knowledge_bases",
            "tools",
            "study_target",
            "document_target",
            "memory_target",
            "reading_target",
        ]
        .contains(&key.as_str())
        {
            continue;
        }
        let empty = value.is_null()
            || value == ""
            || value == false
            || value.as_array().is_some_and(Vec::is_empty)
            || value.as_object().is_some_and(serde_json::Map::is_empty);
        if key == "persist_user_message" && value == true {
            continue;
        }
        if !empty {
            return Err(Error::unsupported(format!(
                "Turn option '{key}' has not yet been migrated."
            )));
        }
    }
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct Progress {
    content: String,
    usage: Value,
    output_bytes: usize,
    context_stats: Option<Value>,
}
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct Checkpoint {
    pub admission: Admission,
    pub pause: crate::turn_engine::Pause,
    progress: Progress,
    selection: Value,
    identity: Value,
    instructions: String,
    timeout: Duration,
    pub reply: Option<Value>,
}
struct RunSpec {
    model: Model,
    instructions: String,
    timeout: Duration,
    input: RunInput,
}
enum RunInput {
    New {
        budget: u64,
        session: Value,
        flags: Box<crate::learning::ToolFlags>,
    },
    Resumed(Box<Checkpoint>),
}
impl Checkpoint {
    pub(crate) fn resume_input(
        &self,
        settings: &Settings,
    ) -> Result<(Model, crate::turn_engine::EngineState)> {
        // The checkpoint carries an already admitted effort. Catalog defaults
        // need not be advertised as selectable overrides, so resolve credentials
        // and model identity separately before restoring that effort.
        let mut selection = self.selection.clone();
        if let Some(fields) = selection.as_object_mut() {
            fields.remove("reasoning_effort");
        }
        let mut model = settings.model(&selection)?;
        if let Some(effort) = self.selection.get("reasoning_effort") {
            model.effort = effort.as_str().map(str::to_owned);
        }
        let identity = json!({"binding":model.binding,"format":model.format,"model":model.model});
        if identity != self.identity {
            return Err(Error::conflict(
                "The model or protocol changed while this question was waiting. Restore its configuration or cancel the turn.",
            ));
        }
        let engine = self.resume_engine(&model)?;
        let body = crate::provider::request_body_scoped(
            &model,
            engine.history.clone(),
            &self.admission.session,
            &self.instructions,
            self.admission.capability,
            engine.required_tool(self.admission.capability),
            &engine.tools,
        )?;
        crate::turn_engine::validate_body(&model, &body)?;
        Ok((model, engine))
    }
    fn resume_engine(&self, model: &Model) -> Result<crate::turn_engine::EngineState> {
        let mut engine = self.pause.engine.clone();
        let mut results = self.pause.results.clone();
        results.push((
            self.pause.call.clone(),
            self.reply
                .clone()
                .ok_or_else(|| Error::conflict("Missing durable reply."))?,
        ));
        engine.user_replies.push(json!({"question":self.pause.question,"reply":self.reply.as_ref().map(|r|&r["content"])}));
        crate::provider::append_results(model, &mut engine.history, results);
        Ok(engine)
    }
}
