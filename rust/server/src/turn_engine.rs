//! In-process asynchronous chat capability. Dropping this future cancels model
//! I/O and stops dispatch. The application owns the deadline and durable events.
use crate::{
    Error, Result,
    capability::Capability,
    config::{Model, Settings},
    provider::{self, Delta},
    tools::{MAX_CALLS, ToolRegistry},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc};
use tokio::sync::{mpsc, oneshot};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EngineState {
    pub history: Vec<Value>,
    #[serde(default)]
    pub tools: crate::learning::ToolFlags,
    pub rounds: usize,
    pub ids: HashSet<String>,
    #[serde(default)]
    pub asked: bool,
    #[serde(default)]
    pub solve: crate::solve::SolveState,
    #[serde(default)]
    pub base_history: usize,
    #[serde(default)]
    pub user_replies: Vec<Value>,
    #[serde(default)]
    pub study_phase: u8,
    #[serde(default)]
    pub lesson_published: bool,
}
impl EngineState {
    pub fn new(history: Vec<Value>) -> Self {
        let base_history = history.len();
        Self {
            tools: Default::default(),
            base_history,
            user_replies: Vec::new(),
            study_phase: 0,
            lesson_published: false,
            asked: false,
            solve: Default::default(),
            history,
            rounds: 0,
            ids: HashSet::new(),
        }
    }
    pub fn required_tool(&self, capability: Capability) -> Option<&'static str> {
        if capability.is_study() && self.study_phase == 0 {
            return Some("study_plan");
        }
        match capability {
            Capability::VideoLesson if !self.lesson_published => Some("render_video_lesson"),
            Capability::AskQuestions if !self.asked => Some("ask_user"),
            Capability::DeepSolve if self.solve.steps.is_empty() => Some("solve_plan"),
            _ => None,
        }
    }
    fn compact_solve(&mut self, last_round_start: usize) {
        let snapshot = json!({"solve_progress":self.solve,"user_replies":self.user_replies});
        self.history.drain(self.base_history..last_round_start);
        self.history.insert(self.base_history,json!({"role":"user","content":format!("Recorded progress and user replies from earlier tool rounds:\n{snapshot}")}));
    }
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Pause {
    pub engine: EngineState,
    pub invocation: String,
    pub call: crate::tools::ToolCall,
    pub results: Vec<(crate::tools::ToolCall, Value)>,
    pub question: Value,
}

pub async fn run(
    settings: Arc<Settings>,
    model: Model,
    mut engine: EngineState,
    instructions: String,
    registry: ToolRegistry,
    output: mpsc::Sender<Delta>,
) -> Result<()> {
    let capability = registry.admission.capability;
    let context = if model.binding == "openai_codex" {
        let store = registry.store.clone();
        let admission = registry.admission.clone();
        let session =
            crate::application::blocking(move || store.tool_session_info(&admission)).await?;
        Some(crate::codex_session::Context {
            workspace: crate::store::text(&session, "workspace_id").into(),
            admission: registry.admission.clone(),
        })
    } else {
        None
    };
    while engine.rounds < capability.max_rounds() {
        let forced = engine.required_tool(capability);
        // Keep the current question and every tool call/result together. We fail
        // explicitly when the continuation cannot fit instead of orphaning pairs.
        let body = provider::request_body_scoped(
            &model,
            engine.history.clone(),
            &registry.admission.session,
            &instructions,
            capability,
            forced,
            &engine.tools,
        )?;
        validate_body(&model, &body)?;
        let mut response = provider::round(
            settings.clone(),
            &model,
            body,
            output.clone(),
            forced.is_some(),
            context.as_ref(),
        )
        .await?;
        engine.rounds += 1;
        if let Some(name) = forced
            && (response.has_text || response.calls.len() != 1 || response.calls[0].name != name)
        {
            return Err(Error::provider(format!(
                "This capability requires a {name} call before a textual response or other work."
            )));
        }
        if response.calls.is_empty() {
            if capability.is_study() && engine.study_phase != 2 {
                return Err(Error::provider(
                    "The structured study artifact must be published before the final answer.",
                ));
            }
            if capability == Capability::DeepSolve && !engine.solve.all_done() {
                return Err(Error::provider(
                    "Solve steps must be recorded as complete before the final answer.",
                ));
            }
            return Ok(());
        }
        if engine.rounds == capability.max_rounds()
            || engine.ids.len() + response.calls.len()
                > if capability == Capability::TestCases {
                    64
                } else {
                    MAX_CALLS
                }
        {
            return Err(Error::provider(
                "Native turn reached its model round or tool call limit.",
            ));
        }
        for call in &response.calls {
            call.validate()?;
            if !engine.ids.insert(call.id.clone()) {
                return Err(Error::provider("Model reused a tool call identifier."));
            }
        }
        if response
            .calls
            .iter()
            .filter(|call| call.name == "ask_user")
            .count()
            > 1
        {
            return Err(Error::provider(
                "Bundle all questions into one ask_user call per model response.",
            ));
        }
        let last_round_start = engine.history.len();
        engine.history.extend(response.continuation);
        response.calls.sort_by_key(|call| call.name == "ask_user");
        let mut results = Vec::new();
        let mut compact = false;
        for call in response.calls {
            let invocation = uuid::Uuid::new_v4().to_string();
            let (ack, committed) = oneshot::channel();
            output
                .send(Delta::ToolStart {
                    invocation: invocation.clone(),
                    call: call.clone(),
                    ack,
                })
                .await
                .map_err(|_| Error::conflict("Turn consumer stopped."))?;
            committed
                .await
                .map_err(|_| Error::conflict("Tool admission was interrupted."))??;
            let result = if call.name == "ask_user" {
                match crate::ask_user::questions(&call.arguments) {
                    Ok(question) => {
                        engine.asked = true;
                        output
                            .send(Delta::Pause(Box::new(Pause {
                                engine,
                                invocation,
                                call,
                                results,
                                question,
                            })))
                            .await
                            .map_err(|_| Error::conflict("Turn consumer stopped."))?;
                        return Ok(());
                    }
                    Err(error) => {
                        json!({"success":false,"error":"invalid_arguments","content":error.message})
                            .into()
                    }
                }
            } else {
                registry
                    .execute(&call, &invocation, &mut engine.solve, &engine.tools)
                    .await
            };
            let (ack, committed) = oneshot::channel();
            output
                .send(Delta::ToolEnd {
                    invocation,
                    result,
                    ack,
                })
                .await
                .map_err(|_| Error::conflict("Turn consumer stopped."))?;
            let result = committed
                .await
                .map_err(|_| Error::conflict("Tool completion was interrupted."))??;
            if call.name == "render_video_lesson" {
                if result["success"] == true {
                    engine.lesson_published = true;
                } else if capability == Capability::VideoLesson
                    && (engine.rounds >= 3
                        || !matches!(
                            result["error"].as_str(),
                            Some(
                                "invalid_request"
                                    | "lesson_compile"
                                    | "lesson_render"
                                    | "lesson_bundle"
                            )
                        ))
                {
                    return Err(Error::provider(format!(
                        "Video lesson failed: {}",
                        crate::store::text(&result, "content")
                    )));
                }
            }
            if result["success"] == true && capability.is_study() {
                if call.name == "study_plan" {
                    engine.study_phase = 1;
                }
                if call.name == "study_publish" {
                    engine.study_phase = 2;
                }
            }
            compact |= capability == Capability::DeepSolve
                && call.name == "solve_finish_step"
                && result["success"] == true;
            results.push((call, result));
        }
        provider::append_results(&model, &mut engine.history, results);
        if compact {
            engine.compact_solve(last_round_start);
        }
    }
    Err(Error::provider(
        "Native turn exceeded its model round limit.",
    ))
}

pub fn validate_body(model: &Model, body: &Value) -> Result<()> {
    let available = model
        .context_window
        .saturating_sub(crate::context::output_reserve(model)?);
    if body.to_string().len() as u64 + 128 > available {
        return Err(Error::invalid(
            "Current question or tool continuation exceeds the model context budget.",
        ));
    }
    Ok(())
}

/// Add canonical per-request usage without treating unknown usage as zero cost.
pub fn accumulate_usage(total: &mut Value, raw: Value) {
    let summary = provider::usage_summary(raw);
    if summary.as_object().is_none_or(|m| m.is_empty()) {
        return;
    }
    if total.as_object().is_none_or(|m| m.is_empty()) {
        *total = summary;
        return;
    }
    for field in [
        "prompt_tokens",
        "completion_tokens",
        "total_tokens",
        "total_calls",
        "estimated_calls",
        "cache_reported_calls",
        "cache_input_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
        "reasoning_tokens",
    ] {
        if let Some(n) = summary[field].as_u64() {
            total[field] = json!(total[field].as_u64().unwrap_or(0).saturating_add(n));
        }
    }
    total["cache_hit_rate"] = total["cache_read_input_tokens"]
        .as_u64()
        .zip(total["cache_input_tokens"].as_u64())
        .filter(|(_, n)| *n > 0)
        .map(|(hit, n)| json!(hit as f64 / n as f64))
        .unwrap_or(Value::Null);
    // Raw usage is per request and cannot represent a multi-request turn.
    total.as_object_mut().unwrap().remove("usage");
}
