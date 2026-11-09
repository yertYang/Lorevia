//! Video tools publish their media together with the durable tool result.
//! Only the workspace URL and playback metadata enter the model context.
mod lesson;
pub(crate) mod provider;
use crate::{
    Application, Error, Result,
    application::blocking,
    config::Settings,
    data_api::Scope,
    learning::{Mutation, ToolFlags, string},
    service_preview as svc,
    store::{Admission, text},
    tools::{ToolCall, ToolOutcome},
};
use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, Method, StatusCode},
    response::Response,
    routing::get,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};

pub(crate) fn stage(mut error: Error, code: &'static str) -> Error {
    error.code = code;
    error
}

pub(crate) use lesson::available as lesson_available;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Selection {
    profile: String,
    model: String,
    identity: String,
}
fn selected(catalog: &Value, ids: &Value) -> Result<(Selection, Value, Value)> {
    let (p, m) = svc::selection(catalog, "videogen", ids)?;
    let c = svc::connection(catalog, p, m, "videogen")?;
    svc::endpoint(&c, "videos")?;
    if text(&c, "binding") != "custom"
        && text(&c, "binding") != "vllm"
        && text(&c, "api_key").is_empty()
        && c["extra_headers"].as_object().is_none_or(|h| h.is_empty())
    {
        return Err(Error::invalid(
            "Configure the video provider credentials first.",
        ));
    }
    let identity = format!(
        "{:x}",
        Sha256::digest(json!([c["binding"], c["base_url"], m]).to_string())
    );
    Ok((
        Selection {
            profile: text(p, "id").into(),
            model: text(m, "id").into(),
            identity,
        },
        c,
        m.clone(),
    ))
}
fn contains(v: &Value, name: &str) -> bool {
    v.as_array().is_some_and(|a| a.iter().any(|v| v == name))
}
pub fn select(
    settings: &Settings,
    enabled: &Value,
    requested: &Value,
) -> Result<Option<Selection>> {
    let explicit = contains(requested, "videogen");
    if !contains(enabled, "videogen") {
        if explicit {
            return Err(Error::invalid("Enable videogen in tool settings first."));
        }
        return Ok(None);
    }
    if !requested.is_null() && !explicit {
        return Ok(None);
    }
    match selected(&settings.catalog()?, &Value::Null) {
        Ok((s, _, _)) => Ok(Some(s)),
        Err(e) if explicit => Err(e),
        Err(_) => Ok(None),
    }
}
pub fn lesson_definition() -> Value {
    json!({"name":"render_video_lesson","description":"Create a playable HTML animation only when the user asks for a video lesson or explainer. For a downloadable MP4 clip use videogen if available. Return complete TSX, importing Scenes, Stage, Title, Body, lessonLength from ./kit and optionally React and remotion. Define scenes like [{voice:\"s1\",frames:180,content:<Stage><Title>...</Title></Stage>}]; export const LESSON={fps:30,width:1920,height:1080,durationInFrames:lessonLength(scenes)} and export const Lesson=()=> <Scenes scenes={scenes}/>; supply matching narration IDs. Animate meaningful diagrams, leave the bottom 200px for captions, and fix errors before reporting success.","parameters":{"type":"object","properties":{"title":{"type":"string"},"code":{"type":"string","description":"Complete Lesson.tsx source, without Markdown fences"},"narration":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"text":{"type":"string"}},"required":["id","text"],"additionalProperties":false}}},"required":["title","code","narration"],"additionalProperties":false}})
}
pub fn video_definition() -> Value {
    json!({"name":"videogen","description":"Generate a downloadable MP4 using the configured video model when the user requests a video clip. For an animated teaching presentation prefer render_video_lesson. Never claim success without a returned artifact.","parameters":{"type":"object","properties":{"prompt":{"type":"string"},"duration":{"type":"integer","minimum":1,"maximum":120},"aspect_ratio":{"type":"string","enum":["16:9","9:16","1:1","4:3","3:4"]},"resolution":{"type":"string","enum":["480p","720p","1080p"]}},"required":["prompt"],"additionalProperties":false}})
}
#[derive(Clone, Debug)]
pub struct Artifact {
    title: String,
    bytes: Vec<u8>,
    lesson: Option<Value>,
}
async fn clip(catalog: &Value, selection: &Selection, args: &Value) -> Result<Artifact> {
    let (s, c, mut m) = selected(
        catalog,
        &json!({"profile_id":selection.profile,"model_id":selection.model}),
    )?;
    if s != *selection {
        return Err(Error::conflict(
            "Video configuration changed; start a new turn.",
        ));
    }
    if args.as_object().is_none_or(|o| {
        o.keys().any(|k| {
            !matches!(
                k.as_str(),
                "prompt" | "duration" | "aspect_ratio" | "resolution"
            )
        })
    }) {
        return Err(Error::invalid("Unexpected video parameters."));
    }
    let prompt = string(args, "prompt", 4000, true)?;
    if let Some(v) = args.get("duration") {
        let n = v
            .as_u64()
            .filter(|n| (1..=120).contains(n))
            .ok_or_else(|| Error::invalid("Invalid video duration."))?;
        m["duration"] = json!(n.to_string());
    }
    for (key, valid) in [
        ("aspect_ratio", &["16:9", "9:16", "1:1", "4:3", "3:4"][..]),
        ("resolution", &["480p", "720p", "1080p"][..]),
    ] {
        if let Some(v) = args.get(key) {
            if !v.as_str().is_some_and(|s| valid.contains(&s)) {
                return Err(Error::invalid(format!("Invalid video {key}.")));
            }
            m[key] = v.clone();
            m["size"] = Value::Null;
        }
    }
    Ok(Artifact {
        title: "video.mp4".into(),
        bytes: provider::generate(&c, &m, &prompt).await?,
        lesson: None,
    })
}
pub async fn execute(
    settings: Arc<Settings>,
    admission: &Admission,
    invocation: &str,
    call: &ToolCall,
    flags: &ToolFlags,
) -> ToolOutcome {
    let outcome = tokio::time::timeout(Duration::from_secs(600), async {
        let args = serde_json::from_str(&call.arguments)
            .map_err(|_| Error::invalid("Invalid video arguments."))?;
        let catalog = blocking(move || settings.catalog()).await?;
        if call.name == "render_video_lesson" {
            lesson::render(&catalog, &args).await
        } else {
            clip(
                &catalog,
                flags.videogen.as_ref().ok_or_else(Error::missing)?,
                &args,
            )
            .await
        }
    })
    .await
    .unwrap_or_else(|_| Err(Error::provider("Video task timed out.")));
    match outcome {
        Ok(a) => ToolOutcome {
            result: Value::Null,
            mutation: Some(Mutation::Video(a)),
        },
        Err(e) => {
            eprintln!(
                "{}",
                json!({"event":"video_tool_error","time":crate::store::now(),"turn":admission.turn,"operation":invocation,"tool":call.name,"error_code":e.code})
            );
            json!({"success":false,"error":e.code,"content":e.message}).into()
        }
    }
}
pub fn publish(conn: &Connection, scope: &str, id: &str, a: Artifact) -> Result<Value> {
    let query = reqwest::Url::parse_with_params("http://localhost", &[("dt_workspace", scope)])
        .expect("fixed URL");
    let url = format!(
        "/api/video/artifacts/{id}?{}",
        query.query().unwrap_or_default()
    );
    let mime = if a.lesson.is_some() {
        "text/html"
    } else {
        "video/mp4"
    };
    conn.execute(
        "INSERT INTO native_video_artifacts(id,workspace_id,mime,content) VALUES(?,?,?,?)",
        params![id, scope, mime, a.bytes],
    )?;
    Ok(if let Some(render) = a.lesson {
        json!({"success":true,"content":format!("Published playable lesson: {url} ({})",render["narration"]),"video_lesson":{"title":a.title,"artifacts":[{"type":"html","filename":"lesson.html","url":url,"content_type":mime}],"render":render}})
    } else {
        json!({"success":true,"content":format!("Published MP4: {url}"),"artifacts":[{"filename":"video.mp4","url":url,"mime_type":mime,"size_bytes":a.bytes.len()}]})
    })
}
pub fn router() -> Router<Arc<Application>> {
    Router::new().route("/api/video/artifacts/{id}", get(fetch))
}
const CSP: &str = "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; media-src data: blob:; font-src data:; connect-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'";
fn range(input: &str, size: usize) -> Option<(usize, usize)> {
    let (a, b) = input.strip_prefix("bytes=")?.split_once('-')?;
    if size == 0 {
        return None;
    }
    if a.is_empty() {
        let suffix = b.parse::<usize>().ok()?;
        return (suffix > 0).then_some((size.saturating_sub(suffix), size - 1));
    }
    let start = a.parse::<usize>().ok()?;
    let end = if b.is_empty() {
        size - 1
    } else {
        b.parse::<usize>().ok()?.min(size - 1)
    };
    (start <= end).then_some((start, end))
}
async fn fetch(
    State(app): State<Arc<Application>>,
    Path(id): Path<String>,
    Query(scope): Query<Scope>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response> {
    let scope = scope.id()?;
    blocking(move||{
        let mut conn=app.store.connect()?;let tx=conn.transaction()?;
        let (mime,size):(String,usize)=tx.query_row("SELECT mime,length(content) FROM native_video_artifacts WHERE workspace_id=? AND id=?",params![scope,id],|r|Ok((r.get(0)?,r.get::<_,u32>(1)? as usize))).optional()?.ok_or_else(Error::missing)?;
        let mut builder=Response::builder().header("cache-control","private, no-store").header("x-content-type-options","nosniff").header("content-type",&mime);
        let mut offset=0;let mut len=size;
        if mime=="video/mp4"{builder=builder.header("accept-ranges","bytes");
            if let Some(h)=headers.get("range"){
                let Some((start,end))=h.to_str().ok().and_then(|s|range(s,size)) else{return Ok(builder.status(StatusCode::RANGE_NOT_SATISFIABLE).header("content-range",format!("bytes */{size}")).body(Body::empty()).unwrap())};
                offset=start;len=end-start+1;builder=builder.status(StatusCode::PARTIAL_CONTENT).header("content-range",format!("bytes {start}-{end}/{size}"));
            }
        }else{builder=builder.header("content-security-policy",CSP);}
        let body=if method==Method::HEAD{Body::empty()}else{Body::from(tx.query_row("SELECT substr(content,?,?) FROM native_video_artifacts WHERE workspace_id=? AND id=?",params![offset as i64+1,len as i64,scope,id],|r|r.get::<_,Vec<u8>>(0))?)};
        Ok(builder.header("content-length",len).body(body).unwrap())
    }).await
}
pub async fn tool_settings(State(app): State<Arc<Application>>) -> Result<Json<Value>> {
    Ok(Json(blocking(move||{
        let s=&app.settings;
        let mut enabled=crate::settings_api::interface(s)?["enabled_optional_tools"].clone();
        let grant=if let (Some(root),Some(id))=(&s.shared,&s.account_id){crate::accounts::grant(root,id)?}else{Value::Null};
        let allowed=|name:&str|!grant["enabled_tools"].is_array()||contains(&grant["enabled_tools"],name);
        let mut tools=vec![];
        for (name,en,zh,available,reason) in [
            ("web_search","Search the web","联网检索",crate::search::select(s,&json!(["web_search"]),&json!(["web_search"])).is_ok(),"search_provider_not_configured"),
            ("videogen","Generate an MP4 clip","生成 MP4 短片",selected(&s.catalog()?,&Value::Null).is_ok(),"video_provider_not_configured")
        ]{
            if !allowed(name){continue}
            let hints=|desc:&str|json!({"short_description":desc,"when_to_use":"","input_format":"","guideline":"","note":"","phase":"","aliases":[]});
            tools.push(json!({"name":name,"description":en,"parameters":[],"aliases":[],"hints":{"en":hints(en),"zh":hints(zh)},"available":available,"unavailable_reason":if available{None}else{Some(reason)},"enabled":available&&contains(&enabled,name),"toggleable":true}));
        }
        if let Some(list)=enabled.as_array_mut(){list.retain(|n|tools.iter().any(|t|t["name"]==*n&&t["available"]==true));}
        Ok(json!({"tools":tools,"enabled_optional_tools":enabled}))
    }).await?))
}
