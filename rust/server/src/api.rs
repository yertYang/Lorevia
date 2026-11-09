use crate::{
    Application, Error, Result,
    application::blocking,
    capability::Capability,
    config::{http_client, redact},
    store::{terminal, text},
};
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, Request, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{sync::mpsc, task::JoinHandle};

type App = Arc<Application>;
pub fn router(app: App) -> Router {
    crate::accounts_api::router(app.clone())
        .layer(middleware::from_fn_with_state(app, local_request))
}
pub(crate) fn workspace_router(app: App) -> Router {
    Router::new()
        .merge(crate::accounts_api::protected())
        .merge(crate::update_api::router())
        .merge(crate::settings_api::router())
        .merge(crate::learning_api::router())
        .merge(crate::video::router())
        .merge(crate::knowledge_api::router())
        .merge(crate::reading_api::router())
        .merge(crate::calendar_api::router())
        .merge(crate::task_board_api::router())
        .merge(crate::workspaces_api::router())
        .merge(crate::study_api::router())
        .merge(crate::client_api::router())
        .merge(crate::documents_api::router())
        .merge(crate::flashcards_api::router())
        .merge(crate::browser_testing_api::router())
        .merge(crate::voice::router())
        .route("/health", get(health))
        .route("/health/ready", get(health))
        .route("/health/live", get(health))
        .route("/api/health", get(health))
        .route("/api/native/status", get(native_status))
        .route("/api/native/metrics", get(native_metrics))
        .route("/api/settings/catalog", get(catalog).put(save_catalog))
        .route("/api/settings/llm-options", get(options))
        .route("/api/settings/fetch-models", post(discover))
        .route("/api/settings/test-provider", post(probe))
        .route("/api/sessions", get(sessions))
        .route(
            "/api/sessions/capabilities",
            get(|| async { Json(json!({"supports_submission_replacement":false})) }),
        )
        .route(
            "/api/sessions/{session}",
            get(session).patch(rename).delete(delete_session),
        )
        .route(
            "/api/sessions/{session}/organization",
            get(organization).patch(organize),
        )
        .route(
            "/api/sessions/{session}/reply-language",
            patch(reply_language),
        )
        .route(
            "/api/sessions/{session}/messages/{message}/events",
            get(message_trace),
        )
        .route(
            "/api/sessions/{session}/branch-selection",
            axum::routing::put(selected_branches),
        )
        .route("/api/turns/{turn}/events", get(turn_events))
        .route("/api/capabilities/registered", get(capabilities))
        .route("/ws", get(websocket))
        .fallback(|| async {
            Error::unsupported("This endpoint has not yet been migrated to the Rust server.")
        })
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .with_state(app)
}
fn loopback_host(host: &str) -> bool {
    host == "localhost"
        || host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}
async fn local_request(State(app): State<App>, request: Request, next: Next) -> Response {
    // The API stays on loopback; Next forwards the original frontend authority.
    // Multi-user deployments accept their same-origin frontend and explicitly
    // configured direct API origins, never an arbitrary credentialed origin.
    let host = request
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| reqwest::Url::parse(&format!("http://{s}")).ok());
    if !host
        .as_ref()
        .and_then(|u| u.host_str())
        .is_some_and(loopback_host)
    {
        return Error::new(
            StatusCode::FORBIDDEN,
            "invalid_host",
            "Native server accepts loopback hosts only.",
        )
        .into_response();
    }
    let origin_header = request.headers().get("origin").cloned();
    if let Some(origin) = &origin_header {
        let origin = origin
            .to_str()
            .ok()
            .and_then(|s| reqwest::Url::parse(s).ok());
        if !origin.as_ref().is_some_and(|u| {
            if !matches!(u.scheme(), "http" | "https") {
                return false;
            }
            if !app.accounts.is_enabled() {
                return u.host_str().is_some_and(loopback_host);
            }
            let frontend = request
                .headers()
                .get("x-deeptutor-frontend-host")
                .and_then(|h| h.to_str().ok());
            frontend
                .and_then(|h| reqwest::Url::parse(&format!("{}://{h}", u.scheme())).ok())
                .is_some_and(|h| {
                    h.host_str() == u.host_str()
                        && h.port_or_known_default() == u.port_or_known_default()
                })
                || host.as_ref().is_some_and(|h| {
                    h.host_str() == u.host_str()
                        && h.port_or_known_default() == u.port_or_known_default()
                })
                || app
                    .accounts
                    .origins
                    .iter()
                    .any(|s| s == u.origin().ascii_serialization().as_str())
        }) {
            return Error::new(
                StatusCode::FORBIDDEN,
                "invalid_origin",
                "Cross-origin access is not allowed.",
            )
            .into_response();
        }
    }
    let requested_headers = request
        .headers()
        .get("access-control-request-headers")
        .cloned();
    let mut response = if request.method() == axum::http::Method::OPTIONS && origin_header.is_some()
    {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(request).await
    };
    if let Some(origin) = origin_header {
        let headers = response.headers_mut();
        headers.insert("access-control-allow-origin", origin);
        headers.insert(
            "access-control-allow-credentials",
            axum::http::HeaderValue::from_static("true"),
        );
        headers.insert(
            "access-control-allow-methods",
            axum::http::HeaderValue::from_static("GET, POST, PUT, PATCH, DELETE, OPTIONS"),
        );
        if let Some(requested) = requested_headers {
            headers.insert("access-control-allow-headers", requested);
        }
        headers.append("vary", axum::http::HeaderValue::from_static("Origin"));
    }
    response.headers_mut().insert(
        "cache-control",
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}
async fn health() -> Json<Value> {
    Json(json!({"status":"ok","ready":true,"backend":"rust","protocol_version":"2.0"}))
}
async fn native_status(State(app): State<App>) -> Json<Value> {
    let capability_tools = Capability::ALL
        .into_iter()
        .map(|capability| {
            let tools = crate::tools::definitions(capability)
                .into_iter()
                .map(|tool| tool["name"].clone())
                .collect::<Vec<_>>();
            (capability.id().to_string(), json!(tools))
        })
        .collect::<serde_json::Map<_, _>>();
    Json(json!({
        "backend":"rust",
        "python_required":false,
        "protocol_version":"2.0",
        "capabilities":Capability::ALL.map(Capability::id),
        "tools":["session_info","ask_user","solve_plan","solve_finish_step","solve_replan"],
        "capability_tools":capability_tools,
        "context_tools":{"notebooks":["list_favorites","save_favorite"],"memory":["read_memory","write_memory"],"selected_knowledge_bases":["rag"],"optional":["web_search","browser_test","videogen"]},
        "browser_testing":{"declarative_cases":true,"screenshots":true,"playwright_export":true,"isolated_contexts":true,"interactive_sessions":true,"user_provided_urls":true,"file_inputs":true,"xlsx_results":true,"preserves_originals":true},
        "realtime_voice":{"provider":"personaplex","external_service_required":true,"languages":["en"],"audio_storage":false,"automatic_tools":false},
        "learning_features":{"authored_documents":true,"document_versions":true,"document_block_editor":true,"document_ai":true,"favorites_crud":true,"model_summary":true,"memory_documents":true,"memory_versions":true,"legacy_learning_import":true,"native_lexical_knowledge":true,"knowledge_binary_parsers":true,"knowledge_source_clips":true,"knowledge_ocr":crate::ocr::available(),"memory_consolidation":true,"memory_provenance_graph":true,"memory_cleanup_proposals":true},
        "client_transports":["websocket_v2","http_polling"],
        "database_backups":{"consistent_wal_snapshot":true,"schema_version":crate::backup::SCHEMA_VERSION},
        "waiting_input_recovery":true,
        "tool_limits":{
            "model_rounds":8,
            "capability_rounds":{"chat":8,"ask_questions":8,"deep_solve":12,"test_cases":32},
            "capability_calls":{"test_cases":64},
            "calls":16,
            "argument_bytes":65536
        },
        "migration_complete":false,
        "mode":if app.accounts.is_enabled(){"multi-user"}else{"local-single-user"},
        "account_features":{"authentication":true,"user_management":true,"model_grants":true,"data_isolation":true,"shared_books":false,"guardian_relationships":false}
    }))
}

async fn native_metrics(State(app): State<App>) -> Json<Value> {
    Json(app.metrics.snapshot())
}
async fn catalog(State(app): State<App>) -> Result<Json<Value>> {
    let s = app.settings.clone();
    Ok(Json(
        json!({"catalog":blocking(move||s.catalog().map(redact)).await?}),
    ))
}
async fn save_catalog(State(app): State<App>, Json(body): Json<Value>) -> Result<Json<Value>> {
    let s = app.settings.clone();
    Ok(Json(
        json!({"catalog":blocking(move||s.save_catalog(body["catalog"].clone())).await?}),
    ))
}
async fn options(State(app): State<App>) -> Result<Json<Value>> {
    let s = app.settings.clone();
    Ok(Json(blocking(move || s.options()).await?))
}
#[derive(Deserialize, Default)]
struct Page {
    workspace_id: Option<String>,
    dt_workspace: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
    after_seq: Option<i64>,
    all_workspaces: Option<bool>,
    archived: Option<bool>,
}
async fn message_trace(
    State(app): State<App>,
    Path((session, message)): Path<(String, i64)>,
    Query(page): Query<Page>,
) -> Result<Json<Value>> {
    let store = app.store.clone();
    Ok(Json(
        blocking(move || {
            store.message_trace(
                &session,
                page.dt_workspace
                    .as_deref()
                    .or(page.workspace_id.as_deref())
                    .unwrap_or(""),
                message,
                page.after_seq.unwrap_or(0).max(0),
                page.limit.unwrap_or(500).clamp(1, 1000) as u32,
            )
        })
        .await?,
    ))
}
async fn sessions(State(app): State<App>, Query(page): Query<Page>) -> Result<Json<Value>> {
    let s = app.store.clone();
    Ok(Json(
        json!({"sessions":blocking(move||s.list(if page.all_workspaces==Some(true){None}else{Some(page.dt_workspace.as_deref().or(page.workspace_id.as_deref()).unwrap_or(""))},page.limit.unwrap_or(50),page.offset.unwrap_or(0),page.archived)).await?}),
    ))
}
async fn session(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(page): Query<Page>,
) -> Result<Json<Value>> {
    let s = app.store.clone();
    Ok(Json(
        blocking(move || {
            s.detail(
                &id,
                page.dt_workspace
                    .as_deref()
                    .or(page.workspace_id.as_deref())
                    .unwrap_or(""),
            )
        })
        .await?,
    ))
}
async fn rename(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(page): Query<Page>,
    Json(body): Json<Value>,
) -> Result<Json<Value>> {
    let title = body["title"]
        .as_str()
        .ok_or_else(|| Error::invalid("Title is required."))?
        .to_string();
    let s = app.store.clone();
    Ok(Json(
        blocking(move || {
            let workspace = page
                .dt_workspace
                .as_deref()
                .or(page.workspace_id.as_deref())
                .unwrap_or("");
            s.rename(&id, workspace, &title)?;
            Ok(json!({"session":s.detail(&id, workspace)?}))
        })
        .await?,
    ))
}
async fn organization(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(page): Query<Page>,
) -> Result<Json<Value>> {
    let store = app.store.clone();
    Ok(Json(
        blocking(move || {
            let workspace = page
                .dt_workspace
                .as_deref()
                .or(page.workspace_id.as_deref())
                .unwrap_or("");
            Ok(json!({"session":store.session(&id, workspace)?}))
        })
        .await?,
    ))
}
async fn reply_language(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(page): Query<Page>,
    Json(body): Json<Value>,
) -> Result<Json<Value>> {
    let language = body.get("language").ok_or_else(|| {
        Error::invalid("Reply language is required; use null to follow the default.")
    })?;
    crate::language::validate_override(language)?;
    let patch = json!({"reply_language_override":language});
    let store = app.store.clone();
    Ok(Json(
        blocking(move || {
            let workspace = page
                .dt_workspace
                .as_deref()
                .or(page.workspace_id.as_deref())
                .unwrap_or("");
            store.preferences(&id, workspace, patch)?;
            Ok(json!({"session":store.session(&id, workspace)?}))
        })
        .await?,
    ))
}
async fn organize(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(page): Query<Page>,
    Json(body): Json<Value>,
) -> Result<Json<Value>> {
    let patch = body
        .as_object()
        .ok_or_else(|| Error::invalid("Expected organization object."))?;
    if patch
        .iter()
        .any(|(k, v)| !matches!(k.as_str(), "archived" | "pinned") || !v.is_boolean())
    {
        return Err(Error::unsupported(
            "Native organization currently supports archived and pinned flags.",
        ));
    }
    let s = app.store.clone();
    Ok(Json(
        blocking(move || {
            let workspace = page
                .dt_workspace
                .as_deref()
                .or(page.workspace_id.as_deref())
                .unwrap_or("");
            s.preferences(&id, workspace, body)?;
            Ok(json!({"session":s.detail(&id, workspace)?}))
        })
        .await?,
    ))
}
async fn selected_branches(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(page): Query<Page>,
    Json(body): Json<Value>,
) -> Result<Json<Value>> {
    let store = app.store.clone();
    Ok(Json(
        blocking(move || {
            store.branch_selection(
                &id,
                page.dt_workspace
                    .as_deref()
                    .or(page.workspace_id.as_deref())
                    .unwrap_or(""),
                body["selected_branches"].clone(),
            )
        })
        .await?,
    ))
}
async fn delete_session(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(page): Query<Page>,
) -> Result<Json<Value>> {
    let store = app.store.clone();
    let sid = id.clone();
    blocking(move || {
        store.delete_session(
            &sid,
            page.dt_workspace
                .as_deref()
                .or(page.workspace_id.as_deref())
                .unwrap_or(""),
        )
    })
    .await?;
    Ok(Json(json!({"deleted":true,"session_id":id})))
}
async fn turn_events(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(page): Query<Page>,
) -> Result<Json<Value>> {
    app.turn(
        &id,
        page.dt_workspace
            .as_deref()
            .or(page.workspace_id.as_deref())
            .unwrap_or(""),
    )
    .await?;
    Ok(Json(
        json!({"events":app.events(&id,page.after_seq.unwrap_or(0).max(0)).await?}),
    ))
}
async fn capabilities(State(app): State<App>) -> Result<Json<Value>> {
    let mut value = Capability::registered();
    if let (Some(root), Some(id)) = (&app.settings.shared, &app.settings.account_id) {
        let policy = crate::accounts_api::policy(root, &app.accounts.user(id)?)?;
        value["capabilities"]
            .as_array_mut()
            .unwrap()
            .retain(|entry| {
                entry["id"] != "test_cases" && (policy.is_null() || entry["id"] == "chat")
            });
    }
    Ok(Json(value))
}

async fn discover(State(app): State<App>, Json(body): Json<Value>) -> Result<Json<Value>> {
    Ok(Json(discover_models(app, body).await?))
}
async fn probe(State(app): State<App>, Json(body): Json<Value>) -> Result<Json<Value>> {
    let value = discover_models(app, body).await?;
    let models = value["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| json!({"id":id}))
        .collect::<Vec<_>>();
    Ok(Json(
        json!({"status":"connected","models":models,"capabilities":[],"http_status":200}),
    ))
}
async fn discover_models(app: App, body: Value) -> Result<Value> {
    let settings = app.settings.clone();
    let body = blocking(move || crate::settings_api::probe_secrets(&settings, &body)).await?;
    if !text(&body, "proxy").is_empty() {
        return Err(Error::unsupported(
            "Per-provider proxy discovery has not yet been migrated.",
        ));
    }
    if !matches!(
        text(&body, "binding"),
        "" | "openai"
            | "openai_compatible"
            | "custom"
            | "custom_anthropic"
            | "anthropic"
            | "openrouter"
            | "deepseek"
            | "ollama"
    ) {
        return Err(Error::unsupported(
            "Model discovery for this provider has not been migrated.",
        ));
    }
    let base = text(&body, "base_url").trim_end_matches('/');
    let url = reqwest::Url::parse(base)
        .map_err(|_| Error::invalid("A provider base URL is required."))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::invalid("Invalid provider URL."));
    }
    let mut request = http_client()?
        .get(format!("{base}/models"))
        .timeout(Duration::from_secs(30));
    let key = body["api_key"]
        .as_str()
        .or_else(|| {
            body["api_key"]
                .as_array()
                .and_then(|v| v.first())
                .and_then(Value::as_str)
        })
        .unwrap_or("");
    if key == "***" {
        return Err(Error::invalid(
            "Saved credentials were not found; enter the key again.",
        ));
    }
    if text(&body, "api_format") == "anthropic" || text(&body, "binding").contains("anthropic") {
        request = request
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01");
    } else if !key.is_empty() {
        request = request.bearer_auth(key);
    }
    let headers = crate::config::normalize_headers(body["extra_headers"].clone())?;
    if let Some(headers) = headers.as_object() {
        for (k, v) in headers {
            let value = v
                .as_str()
                .filter(|s| *s != "***")
                .ok_or_else(|| Error::invalid("Invalid extra header value."))?;
            request = request.header(k, value);
        }
    }
    let response = request
        .send()
        .await
        .map_err(|_| Error::provider("Model discovery connection failed."))?;
    if !response.status().is_success() {
        return Err(Error::provider(format!(
            "Model discovery returned HTTP {}.",
            response.status().as_u16()
        )));
    }
    let payload = crate::config::response_json(response, 8 * 1024 * 1024).await?;
    let data = payload["data"]
        .as_array()
        .ok_or_else(|| Error::provider("Provider returned no model list."))?;
    let mut models = data
        .iter()
        .filter_map(|m| m["id"].as_str())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    models.sort_unstable();
    models.dedup();
    Ok(json!({"models":models}))
}
async fn websocket(
    State(app): State<App>,
    Query(page): Query<Page>,
    headers: HeaderMap,
    principal: Option<axum::Extension<crate::accounts_api::Principal>>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let workspace = page.dt_workspace.or(page.workspace_id).unwrap_or_default();
    // Workspace is fixed for the socket; callers cannot switch ownership by
    // changing a field in a later command.
    let _ = headers;
    upgrade
        .max_message_size(2 * 1024 * 1024)
        .on_upgrade(move |socket| socket_loop(app, socket, workspace, principal.map(|p| p.0)))
}
async fn socket_loop(
    app: App,
    socket: WebSocket,
    workspace: String,
    principal: Option<crate::accounts_api::Principal>,
) {
    let (mut sink, mut source) = socket.split();
    let (tx, mut rx) = mpsc::channel::<Value>(64);
    let writer_app = app.clone();
    let writer_principal = principal.clone();
    let writer = tokio::spawn(async move {
        while let Some(value) = rx.recv().await {
            if !crate::accounts_api::session_valid(&writer_app, &writer_principal).await {
                break;
            }
            if sink
                .send(Message::Text(value.to_string().into()))
                .await
                .is_err()
            {
                break;
            }
            if value["type"] == "content" {
                writer_app.metrics.sent(text(&value, "turn_id"));
            }
        }
    });
    let mut subscriptions: HashMap<String, JoinHandle<()>> = HashMap::new();
    let mut stopping = app.shutdown_receiver();
    let mut auth_check = tokio::time::interval(Duration::from_secs(1));
    loop {
        if *stopping.borrow() {
            break;
        }
        let message = tokio::select! {
            _=stopping.changed()=>break,
            _=auth_check.tick()=>{
                if !crate::accounts_api::session_valid(&app,&principal).await {break;}
                continue;
            },
            message=source.next()=>message
        };
        if !crate::accounts_api::session_valid(&app, &principal).await {
            break;
        }
        let Some(Ok(message)) = message else { break };
        let Message::Text(raw) = message else {
            if matches!(message, Message::Close(_)) {
                break;
            }
            continue;
        };
        let parsed: Result<Value> =
            serde_json::from_str(&raw).map_err(|_| Error::invalid("Invalid command JSON."));
        let invalid_json = parsed.is_err();
        let context = parsed.as_ref().cloned().unwrap_or(Value::Null);
        let result=async {
            let mut command=parsed?;
            if !command.is_object() { return Err(Error::invalid("Expected command object.")); }
            if command["protocol_version"]!="2.0"{return Err(Error::invalid("Expected protocol_version 2.0."));}
            let kind=text(&command,"type").to_string();
            if let Some(value)=command.get("workspace_id").filter(|v| !v.is_null())
                && value.as_str()!=Some(workspace.as_str()) {return Err(Error::invalid("Workspace is fixed for this socket."));}
            match kind.as_str() {
                "ping"=>{let _=tx.send(json!({"type":"pong","protocol_version":"2.0"})).await;},
                "message"|"start_turn"=>{
                    command["workspace_id"]=json!(workspace);
                    let receipt=app.start(command).await?;
                    subscribe(&app,&tx,&mut subscriptions,&receipt.turn,0);
                },
                "subscribe_turn"|"resume_from"=>{
                    let turn=text(&command,"turn_id");app.turn(turn,&workspace).await?;
                    let cursor=if kind=="resume_from"{&command["seq"]}else{&command["after_seq"]};
                    let after=replay_cursor(cursor)?;
                    subscribe(&app,&tx,&mut subscriptions,turn,after);
                },
                "subscribe_session"|"check_active_turn"=>{
                    let sid=text(&command,"session_id").to_string();let store=app.store.clone();let wid=workspace.clone();let check=sid.clone();blocking(move||store.session(&check,&wid)).await?;
                    let after=replay_cursor(&command["after_seq"])?;
                    let active=app.operation("get_active_turn",json!({"session_id":sid})).await?;
                    if kind=="subscribe_session"&&!active.is_null(){subscribe(&app,&tx,&mut subscriptions,text(&active,"id"),after);}
                    else {let _=tx.send(json!({"type":"active_turn_info","turn_id":text(&active,"id"),"status":active["status"].as_str().unwrap_or("none"),"owner_id":text(&active,"owner_id"),"protocol_version":"2.0"})).await;}
                },
                "unsubscribe"=>{if let Some(task)=subscriptions.remove(text(&command,"turn_id")){task.abort();}},
                "submit_user_reply"=>{
                    let turn=text(&command,"turn_id").to_string();
                    let receipt=app.reply_command(command,&workspace).await?;
                    let _=tx.send(receipt).await;
                    if !subscriptions.contains_key(&turn) {subscribe(&app,&tx,&mut subscriptions,&turn,0);}
                },
                "cancel_turn"=>{
                    let cid=text(&command,"command_id");if cid.is_empty(){return Err(Error::invalid("Cancellation requires command_id."));}
                    let turn=text(&command,"turn_id");
                    let receipt=app.cancel_command(turn,&workspace,cid).await?;
                    let _=tx.send(receipt).await;
                },
                _=>return Err(Error::unsupported("This command has not yet been migrated."))
            }
            Ok::<(),Error>(())
        }.await;
        if let Err(error) = result {
            let kind = text(&context, "type");
            let response = if matches!(kind, "cancel_turn" | "submit_user_reply")
                && !text(&context, "command_id").is_empty()
            {
                json!({"type":"command_ack","command_id":context["command_id"],"command_type":kind,"turn_id":text(&context,"turn_id"),"accepted":false,"error_code":error.code,"message":error.message,"protocol_version":"2.0"})
            } else {
                // Admission has no durable stream/DONE. Use the shared protocol
                // code that tells the Web reducer to finish its pending state.
                let code = if invalid_json {
                    "invalid_json"
                } else if !context.is_object() {
                    "invalid_command"
                } else if context["protocol_version"] != "2.0" {
                    "unsupported_protocol_version"
                } else if matches!(kind, "start_turn" | "message") {
                    "start_turn_rejected"
                } else if kind == "regenerate" {
                    "regenerate_rejected"
                } else if !matches!(
                    kind,
                    "ping"
                        | "subscribe_turn"
                        | "resume_from"
                        | "subscribe_session"
                        | "check_active_turn"
                        | "unsubscribe"
                        | "cancel_turn"
                ) {
                    "unknown_message_type"
                } else {
                    error.code
                };
                json!({"type":"protocol_error","error_code":code,"message":error.message,"retryable":false,"session_id":text(&context,"session_id"),"turn_id":text(&context,"turn_id"),"protocol_version":"2.0"})
            };
            let _ = tx.send(response).await;
        }
        if writer.is_finished() {
            break;
        }
    }
    for task in subscriptions.into_values() {
        task.abort();
    }
    writer.abort();
}
fn replay_cursor(value: &Value) -> Result<i64> {
    if value.is_null() {
        return Ok(0);
    }
    value
        .as_i64()
        .filter(|n| *n >= 0)
        .ok_or_else(|| Error::invalid("Invalid replay cursor."))
}
fn subscribe(
    app: &App,
    tx: &mpsc::Sender<Value>,
    tasks: &mut HashMap<String, JoinHandle<()>>,
    turn: &str,
    after: i64,
) {
    tasks.retain(|_, task| !task.is_finished());
    if let Some(task) = tasks.remove(turn) {
        task.abort();
    }
    let app = app.clone();
    let tx = tx.clone();
    let id = turn.to_string();
    tasks.insert(
        turn.to_string(),
        tokio::spawn(async move {
            let mut cursor = after;
            let changed = app.turn_changed(&id);
            loop {
                let wake = changed.notified();
                tokio::pin!(wake);
                wake.as_mut().enable();
                let events = match app.events(&id, cursor).await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let full_page = events.len() == 256;
                for event in events {
                    cursor = event["seq"].as_i64().unwrap_or(cursor);
                    let done = event["type"] == "done";
                    if tx.send(event).await.is_err() || done {
                        return;
                    }
                }
                if full_page {
                    continue;
                }
                let state = app.operation("get_turn", json!({"turn_id":id})).await;
                if state.as_ref().is_ok_and(|v| {
                    terminal(text(v, "status")) && v["last_seq"].as_i64().unwrap_or(0) <= cursor
                }) {
                    break;
                }
                if state
                    .as_ref()
                    .is_ok_and(|v| v["last_seq"].as_i64().unwrap_or(0) > cursor)
                {
                    continue;
                }
                let _ = tokio::time::timeout(Duration::from_secs(1), wake).await;
            }
        }),
    );
}
