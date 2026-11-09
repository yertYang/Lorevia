//! Native settings handlers preserve the existing draft/apply distinction.
use crate::{
    Application, Error, Result,
    application::blocking,
    config::{Settings, read_json, redact, safe_path, write_json},
    store::text,
};
use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use serde_json::{Value, json};
use std::sync::Arc;
type App = Arc<Application>;
pub fn router() -> Router<App> {
    Router::new()
        .route("/api/settings", get(settings))
        .route("/api/settings/ui", get(ui).put(update_ui))
        .route(
            "/api/settings/voice-autoplay",
            axum::routing::put(update_ui),
        )
        .route(
            "/api/settings/voice-math-speak",
            axum::routing::put(update_ui),
        )
        .route(
            "/api/settings/draft",
            get(draft).put(save_draft).delete(discard_draft),
        )
        .route("/api/settings/apply", post(apply))
        .route("/api/settings/apply/registry", post(apply_registry))
        .route(
            "/api/settings/providers/openai-codex/oauth/status",
            get(codex_status),
        )
        .route(
            "/api/settings/providers/openai-codex/oauth/start",
            post(codex_start),
        )
        .route(
            "/api/settings/providers/openai-codex/oauth/cancel",
            post(codex_cancel),
        )
        .route(
            "/api/settings/providers/openai-codex/oauth/complete",
            post(codex_complete),
        )
        .route(
            "/api/settings/providers/openai-codex/oauth/logout",
            post(codex_logout),
        )
        .route(
            "/api/settings/providers/openai-codex/models/reasoning-effort",
            post(codex_reasoning),
        )
        .route(
            "/api/settings/providers/openai-codex/models/refresh",
            post(codex_models),
        )
        .route(
            "/api/settings/enabled-tools",
            get(enabled_tool_status).put(enabled_tools),
        )
        .route("/api/tools", get(crate::video::tool_settings))
        .route("/api/modules", get(modules))
        .merge(crate::diagnostics::router())
        .merge(crate::service_preview::router())
}
pub fn interface(settings: &Settings) -> Result<Value> {
    let path = settings.user_root.join("settings/interface.json");
    safe_path(&settings.user_root, &path)?;
    let stored = read_json(&path, json!({}))?;
    if !stored.is_object() {
        return Err(Error::conflict("Invalid stored interface settings."));
    }
    let mut value = json!({"theme":"system","language":"en","response_language":"en","enabled_optional_tools":[],"voice_autoplay":false,"voice_math_speak":true,"chat_response_timeout":180,"sidebar_description":"Lorevia","sidebar_nav_order":{"start":["/","/history"],"learnResearch":[]},"content_font_scale":100,"content_line_spacing":"normal","code_block_theme":"oneDark","code_block_show_line_numbers":false,"code_block_wrap_long_lines":false});
    if let Some(map) = stored.as_object() {
        value.as_object_mut().unwrap().extend(map.clone());
    }
    // Older interface files used one language for both UI and replies.
    if stored.get("response_language").is_none()
        && stored["language"]
            .as_str()
            .and_then(crate::language::label)
            .is_some()
    {
        value["response_language"] = stored["language"].clone();
    }
    // Tools are advertised from the native registry, not from stale Python flags.
    value["enabled_optional_tools"] = json!(
        value["enabled_optional_tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| *v == "web_search" || *v == "videogen")
            .cloned()
            .collect::<Vec<_>>()
    );
    Ok(value)
}
async fn settings(State(app): State<App>) -> Result<Json<Value>> {
    let settings = app.settings.clone();
    Ok(Json(blocking(move||{
        let providers=provider_choices();
        let connections=providers["llm"].as_array().unwrap().iter().filter(|p|p["auth_mode"]!="oauth").map(|p|{
            let mut services=json!({"llm":{"provider":p["value"],"base_url":p["base_url"],"default_model":""}});
            if let Some(speech)=providers["stt"].as_array().unwrap().iter().find(|s|s["value"]==p["value"]){
                services["stt"]=json!({"provider":speech["value"],"base_url":speech["base_url"],"default_model":speech["default_model"]});
            }
            json!({"provider":p["value"],"label":p["label"],"default_base_url":p["base_url"],"services":services})
        }).collect::<Vec<_>>();
        let mut payload=json!({"ui":interface(&settings)?,"providers":providers,"connection_targets":connections,"task_kinds":[]});
        if settings.shared.is_none(){payload["catalog"]=redact(settings.catalog()?);}
        Ok(payload)
    }).await?))
}
async fn enabled_tool_status(State(app): State<App>) -> Result<Json<Value>> {
    let settings = app.settings.clone();
    Ok(Json(
        blocking(move || {
            Ok(json!({"enabled_optional_tools":interface(&settings)?["enabled_optional_tools"]}))
        })
        .await?,
    ))
}
fn provider_choices() -> Value {
    let specs = [
        ("openai", "OpenAI", "https://api.openai.com/v1"),
        ("custom", "Custom (OpenAI API)", ""),
        ("anthropic", "Anthropic", "https://api.anthropic.com/v1"),
        ("custom_anthropic", "Custom (Anthropic API)", ""),
        ("deepseek", "DeepSeek", "https://api.deepseek.com/v1"),
        ("openrouter", "OpenRouter", "https://openrouter.ai/api/v1"),
        ("ollama", "Ollama", "http://localhost:11434/v1"),
    ];
    let mut llm=specs.into_iter().map(|(id,label,url)|{
        let anthropic=id.contains("anthropic");
        json!({"value":id,"label":label,"base_url":url,"auth_mode":"api_key","requires_api_key":id!="ollama","supports_wire_api_selection":!anthropic,"api_formats":if anthropic{json!(["anthropic"])}else{json!(["auto","openai_chat","openai_responses"])},"default_api_format":if anthropic{"anthropic"}else{"auto"},"status":"supported"})
    }).collect::<Vec<_>>();
    llm.push(json!({"value":"openai_codex","label":"OpenAI Codex","base_url":"https://chatgpt.com/backend-api","auth_mode":"oauth","requires_api_key":false,"supports_wire_api_selection":false,"api_formats":[],"status":"supported"}));
    let stt = json!([
        {"value":"openai","label":"OpenAI","base_url":"https://api.openai.com/v1","default_model":"whisper-1","requires_api_key":true,"status":"supported"},
        {"value":"custom","label":"Custom (OpenAI-compatible audio)","base_url":"","default_model":"","requires_api_key":false,"status":"supported"},
        {"value":"groq","label":"Groq","base_url":"https://api.groq.com/openai/v1","default_model":"whisper-large-v3-turbo","requires_api_key":true,"status":"supported"}
    ]);
    json!({"llm":llm,"task":llm,"embedding":[{"value":"openai","label":"OpenAI","base_url":"https://api.openai.com/v1","requires_api_key":true,"status":"supported"},{"value":"custom","label":"Custom (OpenAI-compatible)","base_url":"","requires_api_key":false,"status":"supported"}],"search":[{"value":"tavily","label":"Tavily","base_url":"https://api.tavily.com","requires_api_key":true,"status":"supported"},{"value":"serper","label":"Serper","base_url":"https://google.serper.dev","requires_api_key":true,"status":"supported"},{"value":"searxng","label":"SearXNG","base_url":"","requires_api_key":false,"status":"supported"}],"tts":[{"value":"openai","label":"OpenAI","base_url":"https://api.openai.com/v1","default_model":"tts-1","requires_api_key":true,"status":"supported"},{"value":"custom","label":"Custom (OpenAI-compatible audio)","base_url":"","requires_api_key":false,"status":"supported"}],"stt":stt,"imagegen":[{"value":"openai","label":"OpenAI","base_url":"https://api.openai.com/v1","requires_api_key":true,"status":"supported"},{"value":"custom","label":"Custom (OpenAI-compatible)","base_url":"","requires_api_key":false,"status":"supported"}],"videogen":[{"value":"volcengine","label":"Volcengine Seedance","base_url":"https://ark.cn-beijing.volces.com/api/v3","requires_api_key":true,"status":"supported"},{"value":"dashscope","label":"DashScope Wan","base_url":"https://dashscope.aliyuncs.com/api/v1","requires_api_key":true,"status":"supported"},{"value":"xai","label":"xAI","base_url":"https://api.x.ai/v1","requires_api_key":true,"status":"supported"},{"value":"openai","label":"OpenAI","base_url":"https://api.openai.com/v1","requires_api_key":true,"status":"supported"},{"value":"custom","label":"Custom (OpenAI-compatible)","base_url":"","requires_api_key":false,"status":"supported"}]})
}
async fn ui(State(app): State<App>) -> Result<Json<Value>> {
    let settings = app.settings.clone();
    Ok(Json(
        blocking(move || {
            let ui = interface(&settings)?;
            let keys = [
                "theme",
                "language",
                "response_language",
                "content_font_scale",
                "content_line_spacing",
                "code_block_theme",
                "code_block_show_line_numbers",
                "code_block_wrap_long_lines",
                "sidebar_description",
                "sidebar_nav_order",
            ];
            Ok(Value::Object(
                keys.into_iter()
                    .map(|key| (key.to_string(), ui[key].clone()))
                    .collect(),
            ))
        })
        .await?,
    ))
}
async fn update_ui(State(app): State<App>, Json(patch): Json<Value>) -> Result<Json<Value>> {
    let map = patch
        .as_object()
        .ok_or_else(|| Error::invalid("Expected UI settings object."))?;
    for (key, value) in map {
        match key.as_str() {
            "theme"
                if ["system", "light", "dark", "glass", "snow"]
                    .iter()
                    .any(|s| value == s) => {}
            "language"
                if value.as_str().is_some_and(|s| {
                    ["en", "zh", "fr", "de", "uk", "pl", "pt", "vi", "ja", "tr"].contains(&s)
                }) => {}
            "response_language" if value.as_str().and_then(crate::language::label).is_some() => {}
            "voice_autoplay"
            | "voice_math_speak"
            | "code_block_show_line_numbers"
            | "code_block_wrap_long_lines"
                if value.is_boolean() => {}
            "code_block_theme"
                if value.as_str().is_some_and(|s| {
                    !s.is_empty()
                        && s.len() <= 80
                        && s.chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
                }) => {}
            "chat_response_timeout" if value.as_u64().is_some_and(|n| (30..=1800).contains(&n)) => {
            }
            "content_font_scale"
                if value
                    .as_u64()
                    .is_some_and(|n| [90, 100, 110, 125, 150].contains(&n)) => {}
            "content_line_spacing"
                if ["compact", "normal", "relaxed"].iter().any(|s| value == s) => {}
            _ => return Err(Error::invalid(format!("Unsupported UI setting: {key}"))),
        }
    }
    let settings = app.settings.clone();
    Ok(Json(
        blocking(move || {
            let _guard = settings.settings_lock()?;
            let path = settings.user_root.join("settings/interface.json");
            safe_path(&settings.user_root, &path)?;
            let mut value = read_json(&path, json!({}))?;
            value
                .as_object_mut()
                .ok_or_else(|| Error::conflict("Invalid stored interface settings."))?
                .extend(patch.as_object().unwrap().clone());
            write_json(&path, &value)?;
            Ok(patch)
        })
        .await?,
    ))
}
pub fn read_draft(settings: &Settings) -> Result<Value> {
    let path = settings.user_root.join("settings/settings_draft.json");
    safe_path(&settings.user_root, &path)?;
    let draft = read_json(
        &path,
        json!({"version":1,"updated_at":"","catalog":null,"extensions":{}}),
    )?;
    if !draft.is_object()
        || (!draft["catalog"].is_null() && !draft["catalog"].is_object())
        || (!draft["extensions"].is_null() && !draft["extensions"].is_object())
    {
        return Err(Error::conflict("Invalid stored settings draft."));
    }
    Ok(draft)
}
async fn draft(State(app): State<App>) -> Result<Json<Value>> {
    let settings = app.settings.clone();
    Ok(Json(
        json!({"draft":blocking(move||{let _guard=settings.settings_lock()?;read_draft(&settings).map(redact)}).await?}),
    ))
}
async fn save_draft(State(app): State<App>, Json(body): Json<Value>) -> Result<Json<Value>> {
    let settings = app.settings.clone();
    let saved=blocking(move||{
        let _guard = settings.settings_lock()?;
        if !body.is_object(){return Err(Error::invalid("Expected settings draft object."));}
        if body.get("extensions").is_some_and(|v| !v.is_object()) { return Err(Error::invalid("Draft extensions must be an object.")); }
        let prior=read_draft(&settings)?;let current=settings.catalog_locked()?;
        let reference=if prior["catalog"].is_object(){&prior["catalog"]}else{&current};
        let proposed=if body["catalog"].is_null(){Value::Null}else{crate::config::restore_catalog(body["catalog"].clone(),reference)?};
        let draft=json!({"version":1,"updated_at":"","catalog":proposed,"extensions":body.get("extensions").cloned().unwrap_or(json!({}))});
        write_json(&settings.user_root.join("settings/settings_draft.json"),&draft)?;Ok(redact(draft))
    }).await?;
    Ok(Json(json!({"draft":saved})))
}
async fn discard_draft(State(app): State<App>) -> Result<Json<Value>> {
    let settings = app.settings.clone();
    blocking(move || {
        let _guard = settings.settings_lock()?;
        safe_path(
            &settings.user_root,
            &settings.user_root.join("settings/settings_draft.json"),
        )?;
        write_json(
            &settings.user_root.join("settings/settings_draft.json"),
            &json!({"version":1,"catalog":null,"extensions":{}}),
        )
    })
    .await?;
    Ok(Json(json!({"draft":null})))
}
async fn apply(State(app): State<App>, body: Option<Json<Value>>) -> Result<Json<Value>> {
    let settings = app.settings.clone();
    let saved=blocking(move||{
        let _guard = settings.settings_lock()?;
        let draft=read_draft(&settings)?;
        for (key,payload) in draft["extensions"].as_object().into_iter().flatten() {
            match key.as_str() {
                "ui" => {
                    let live=interface(&settings)?;
                    if !payload.as_object().is_some_and(|patch|patch.iter().all(|(k,v)|live.get(k)==Some(v))) {
                        return Err(Error::conflict("Apply the pending UI settings before applying the catalog; the draft was preserved."));
                    }
                },
                "tools" => {
                    let live=interface(&settings)?;
                    if !payload["enabled_tools"].is_array() || payload["enabled_tools"]!=live["enabled_optional_tools"] {
                        return Err(Error::conflict("Apply the pending tool switches before applying the catalog; the draft was preserved."));
                    }
                },
                // Layout is owned by browser storage; the frontend applies it
                // before calling this catalog endpoint.
                "ui-layout" if payload=="classic" || payload=="modern" => {},
                _ => return Err(Error::unsupported("This draft has extension settings whose native handlers are not migrated; the draft was preserved.")),
            }
        }
        let proposed = match body {
            None => draft["catalog"].clone(),
            Some(Json(value)) if value.is_object() && value.as_object().unwrap().is_empty() => draft["catalog"].clone(),
            Some(Json(value)) if value.is_object() && value.as_object().unwrap().len()==1 && value["catalog"].is_object() => {
                // Submitted redacted drafts refer to credentials in the draft,
                // which can differ from the currently applied catalog.
                let reference = if draft["catalog"].is_object() { &draft["catalog"] } else { &settings.catalog_locked()? };
                crate::config::restore_catalog(value["catalog"].clone(), reference)?
            },
            Some(_) => return Err(Error::invalid("Apply expects an empty object or a catalog object.")),
        };
        let result=if proposed.is_null(){settings.catalog_locked()?}else{settings.prepare_catalog(proposed)?};
        settings.publish_apply_locked(&result,&json!({"version":1,"catalog":null,"extensions":{}}))?;Ok(redact(result))
    }).await?;
    Ok(Json(
        json!({"message":"Catalog applied to the Rust runtime.","catalog":saved,"runtime":{}}),
    ))
}
async fn codex_models(State(app): State<App>) -> Result<Json<Value>> {
    crate::oauth::refresh_models(app.settings.clone()).await?;
    codex_status(State(app)).await
}
async fn codex_status(State(app): State<App>) -> Result<Json<Value>> {
    Ok(Json(app.codex_login.status(app.settings.clone()).await?))
}
async fn codex_start(State(app): State<App>) -> Result<Json<Value>> {
    if *app.shutdown_receiver().borrow() {
        return Err(Error::conflict("Server is shutting down."));
    }
    Ok(Json(app.codex_login.start(app.settings.clone()).await?))
}
async fn codex_cancel(State(app): State<App>) -> Result<Json<Value>> {
    app.codex_login.cancel(app.settings.clone()).await?;
    codex_status(State(app)).await
}
async fn codex_complete(State(app): State<App>, Json(payload): Json<Value>) -> Result<Json<Value>> {
    app.codex_login
        .complete(text(&payload, "callback_url"))
        .await?;
    codex_status(State(app)).await
}
async fn codex_logout(State(app): State<App>) -> Result<Json<Value>> {
    app.codex_login.logout(app.settings.clone()).await?;
    codex_status(State(app)).await
}
async fn codex_reasoning(
    State(app): State<App>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>> {
    let settings = app.settings.clone();
    blocking(move || crate::oauth::set_reasoning(&settings, &payload)).await?;
    codex_status(State(app)).await
}
async fn apply_registry(State(app): State<App>, Json(edit): Json<Value>) -> Result<Json<Value>> {
    let settings = app.settings.clone();
    Ok(Json(
        blocking(move || {
            let _guard = settings.settings_lock()?;
            let mut stored = read_draft(&settings)?;
            let current = settings.catalog_locked()?;
            let proposed = crate::catalog_edit::registry(&current, &edit, &stored["catalog"])?;
            if stored["catalog"].is_object() {
                stored["catalog"] =
                    crate::catalog_edit::registry(&stored["catalog"], &edit, &proposed)?;
            }
            let catalog = settings.prepare_catalog(proposed)?;
            if stored["catalog"] == catalog {
                stored["catalog"] = Value::Null;
            }
            settings.publish_apply_locked(&catalog, &stored)?;
            let draft = if stored["catalog"].is_null()
                && stored["extensions"]
                    .as_object()
                    .is_none_or(|v| v.is_empty())
            {
                Value::Null
            } else {
                redact(stored)
            };
            Ok(json!({"catalog":redact(catalog),"draft":draft}))
        })
        .await?,
    ))
}
async fn modules(State(app): State<App>) -> Json<Value> {
    let mut value: Value = serde_json::from_str(include_str!("../assets/modules.json"))
        .expect("checked module inventory");
    if let Some(module) = value["modules"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|m| m["id"] == "video-lesson")
    {
        let ready = crate::video::lesson_available();
        for key in ["installed", "enabled", "ready"] {
            module[key] = json!(ready);
        }
        if !ready {
            module["issues"] = json!(["Video lesson runtime dependencies are missing."]);
        }
    }
    if let Some(knowledge) = value["modules"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|m| m["id"] == "knowledge")
    {
        knowledge["features"]["ocr"] = json!(crate::ocr::available());
    }
    if app.settings.shared.is_some() {
        for module in value["modules"].as_array_mut().unwrap() {
            if module["id"] == "browser-testing" || module["id"] == "realtime-voice" {
                module["enabled"] = json!(false);
                module["ready"] = json!(false);
                module["issues"] = json!(["This service is managed by the administrator."]);
            }
        }
    }
    Json(value)
}

pub fn probe_secrets(settings: &Settings, body: &Value) -> Result<Value> {
    if !body.is_object() {
        return Err(Error::invalid("Expected provider settings object."));
    }
    let _guard = settings.settings_lock()?;
    let current = settings.catalog_locked()?;
    let draft = read_draft(settings)?;
    let catalog = if draft["catalog"].is_object() {
        &draft["catalog"]
    } else {
        &current
    };
    let connection = text(body, "connection_id");
    let profile = text(body, "profile_id");
    let source = if !connection.is_empty() {
        catalog["connections"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|p| p["id"] == connection)
    } else {
        catalog["services"][body["service"].as_str().unwrap_or("llm")]["profiles"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|p| p["id"] == profile)
    };
    let mut result = body.clone();
    for key in ["api_key", "extra_headers"] {
        if result[key] == "***" {
            result[key] = source
                .filter(|s| !s[key].is_null())
                .map(|s| s[key].clone())
                .ok_or_else(|| {
                    Error::invalid("Saved credentials were not found; enter them again.")
                })?;
        } else if let Some(map) = result[key].as_object_mut() {
            for (k, v) in map {
                if *v == "***" {
                    *v = source
                        .map(|s| s[key][k.as_str()].clone())
                        .unwrap_or(Value::Null)
                }
            }
        }
    }
    Ok(result)
}

async fn enabled_tools(State(app): State<App>, Json(body): Json<Value>) -> Result<Json<Value>> {
    let tools = body["enabled_tools"]
        .as_array()
        .filter(|ts| ts.len() <= 2 && ts.iter().all(|t| t == "web_search" || t == "videogen"))
        .ok_or_else(|| Error::invalid("Select web_search or videogen in optional tool settings."))?
        .clone();
    let settings = app.settings.clone();
    blocking(move || {
        let _guard = settings.settings_lock()?;
        let path = settings.user_root.join("settings/interface.json");
        safe_path(&settings.user_root, &path)?;
        let mut value = read_json(&path, json!({}))?;
        if !value.is_object() {
            return Err(Error::conflict("Invalid interface settings."));
        }
        value["enabled_optional_tools"] = json!(tools);
        write_json(&path, &value)?;
        Ok(Json(json!({"enabled_tools":tools})))
    })
    .await
}
