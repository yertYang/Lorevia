//! Preview saved or edited service selections without changing runtime defaults.
use crate::{Application, Error, Result, application::blocking, config, store::text};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
};
use base64::Engine;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

const MAX_MEDIA: usize = 32 * 1024 * 1024;
pub(crate) fn router() -> Router<Arc<Application>> {
    Router::new().route("/api/settings/services/{service}/preview", post(preview))
}
pub(crate) fn selection<'a>(
    catalog: &'a Value,
    service: &str,
    body: &Value,
) -> Result<(&'a Value, &'a Value)> {
    let settings = &catalog["services"][service];
    let pid = body["profile_id"]
        .as_str()
        .unwrap_or(text(settings, "active_profile_id"));
    let mid = body["model_id"]
        .as_str()
        .unwrap_or(text(settings, "active_model_id"));
    let profile = settings["profiles"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| p["id"] == pid)
        .ok_or_else(|| Error::invalid("Select a configured service profile."))?;
    let model = profile["models"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|m| m["id"] == mid && !text(m, "model").is_empty())
        .ok_or_else(|| Error::invalid("Select a configured service model."))?;
    Ok((profile, model))
}
pub(crate) fn connection(
    catalog: &Value,
    profile: &Value,
    model: &Value,
    service: &str,
) -> Result<Value> {
    let mut effective = config::effective_service_profile(catalog, profile, model, service)?;
    let binding = text(&effective, "binding");
    let native_video =
        service == "videogen" && matches!(binding, "volcengine" | "dashscope" | "xai");
    if !native_video && !matches!(binding, "openai" | "custom" | "vllm") {
        return Err(Error::unsupported(
            "Choose an OpenAI-compatible provider for this service.",
        ));
    }
    if effective["api_key"].is_array() || !text(&effective, "proxy").is_empty() {
        return Err(Error::unsupported(
            "Service previews require one API key and no per-provider proxy.",
        ));
    }
    if text(&effective, "base_url").is_empty() && effective["binding"] == "openai" {
        effective["base_url"] = json!("https://api.openai.com/v1");
    }
    if native_video && text(&effective, "base_url").is_empty() {
        effective["base_url"] = json!(match text(&effective, "binding") {
            "xai" => "https://api.x.ai/v1",
            "dashscope" => "https://dashscope.aliyuncs.com/api/v1",
            _ => "https://ark.cn-beijing.volces.com/api/v3",
        });
    }
    valid_url(text(&effective, "base_url"))?;
    effective["extra_headers"] = config::normalize_headers(effective["extra_headers"].clone())?;
    Ok(effective)
}
fn valid_url(value: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value)
        .map_err(|_| Error::invalid("Configure a valid HTTP(S) service URL."))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::invalid(
            "Service URLs must use HTTP(S) without credentials or fragments.",
        ));
    }
    Ok(url)
}
pub(crate) fn endpoint(connection: &Value, suffix: &str) -> Result<reqwest::Url> {
    let mut url = valid_url(text(connection, "base_url"))?;
    if url.query().is_some() {
        return Err(Error::invalid(
            "Service base URLs cannot contain query parameters.",
        ));
    }
    let path = url.path().trim_end_matches('/');
    let path = if suffix.starts_with("videos/") {
        path.strip_suffix("/videos").unwrap_or(path)
    } else {
        path
    };
    if !path.ends_with(&format!("/{suffix}")) {
        url.set_path(&format!("{path}/{suffix}"));
    }
    Ok(url)
}
pub(crate) fn request(connection: &Value, suffix: &str) -> Result<reqwest::RequestBuilder> {
    authorized(
        config::http_client()?
            .post(endpoint(connection, suffix)?)
            .timeout(Duration::from_secs(120)),
        connection,
    )
}
pub(crate) fn authorized(
    mut request: reqwest::RequestBuilder,
    connection: &Value,
) -> Result<reqwest::RequestBuilder> {
    if !text(connection, "api_key").is_empty() {
        request = request.bearer_auth(text(connection, "api_key"));
    }
    for (key, value) in connection["extra_headers"]
        .as_object()
        .into_iter()
        .flatten()
    {
        if [
            "host",
            "content-type",
            "content-length",
            "transfer-encoding",
            "connection",
        ]
        .contains(&key.to_ascii_lowercase().as_str())
        {
            return Err(Error::invalid(
                "Extra headers cannot override transport headers.",
            ));
        }
        request = request.header(key, value.as_str().unwrap_or(""));
    }
    Ok(request)
}
pub(crate) fn successful(response: &reqwest::Response) -> Result<()> {
    if response.status().is_success() {
        Ok(())
    } else {
        Err(Error::provider(format!(
            "Service provider returned HTTP {}.",
            response.status().as_u16()
        )))
    }
}
async fn preview(
    State(app): State<Arc<Application>>,
    Path(service): Path<String>,
    Json(body): Json<Value>,
) -> Result<axum::response::Response> {
    if !["search", "imagegen", "videogen"].contains(&service.as_str()) {
        return Err(Error::invalid("Unsupported service preview."));
    }
    let prompt = crate::learning::string(&body, "text", 2000, true)?;
    let source = app.clone();
    let (catalog, body) =
        blocking(move || Ok((crate::diagnostics::draft_catalog(&source, &body)?, body))).await?;
    let output = if service == "search" {
        let id = body["profile_id"]
            .as_str()
            .unwrap_or(text(&catalog["services"]["search"], "active_profile_id"));
        let result = crate::search::preview(&catalog, id, &prompt).await?;
        json!({"type":"result","kind":"search","results":result["sources"]})
    } else {
        let (profile, model) = selection(&catalog, &service, &body)?;
        let connection = connection(&catalog, profile, model, &service)?;
        tokio::time::timeout(
            Duration::from_secs(if service == "videogen" { 600 } else { 120 }),
            generate(&connection, model, &service, &prompt),
        )
        .await
        .map_err(|_| {
            Error::new(
                StatusCode::GATEWAY_TIMEOUT,
                "timeout",
                "Service preview timed out.",
            )
        })??
    };
    Ok((
        [
            ("content-type", "application/x-ndjson"),
            ("cache-control", "no-store"),
        ],
        format!("{output}\n"),
    )
        .into_response())
}
async fn generate(connection: &Value, model: &Value, service: &str, prompt: &str) -> Result<Value> {
    if service == "imagegen" {
        let mut payload = json!({"model":model["model"],"prompt":prompt,"n":1});
        for key in ["size", "quality", "style", "response_format"] {
            if !text(model, key).is_empty() {
                payload[key] = model[key].clone();
            }
        }
        let response = request(connection, "images/generations")?
            .json(&payload)
            .send()
            .await
            .map_err(|_| Error::provider("Image provider connection failed."))?;
        successful(&response)?;
        let result =
            config::response_json(response, MAX_MEDIA.div_ceil(3) * 4 + 1024 * 1024).await?;
        let row = &result["data"][0];
        let bytes = if let Some(data) = row["b64_json"].as_str() {
            if data.len() > MAX_MEDIA.div_ceil(3) * 4 {
                return Err(Error::provider("Generated media exceeds the size limit."));
            }
            base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|_| Error::provider("Invalid generated image data."))?
        } else {
            download(text(row, "url")).await?
        };
        return media("image", bytes);
    }
    media(
        "video",
        crate::video::provider::generate(connection, model, prompt).await?,
    )
}
pub(crate) async fn openai_video(
    connection: &Value,
    model: &Value,
    prompt: &str,
) -> Result<Vec<u8>> {
    // Video-compatible providers expose a multipart creation request and a
    // status/content pair. Poll only the selected provider's own job id.
    let boundary = format!("video-{}", uuid::Uuid::new_v4());
    let mut form = Vec::new();
    let duration = if text(model, "duration").is_empty() {
        "4"
    } else {
        text(model, "duration")
    };
    let resolution = text(model, "resolution");
    let size = if !text(model, "size").is_empty() {
        text(model, "size")
    } else {
        match (resolution, text(model, "aspect_ratio")) {
            ("720p", "16:9") => "1280x720",
            ("720p", _) => "720x1280",
            ("1080p", "16:9") => "1920x1080",
            ("1080p", _) => "1080x1920",
            _ => resolution,
        }
    };
    for (key, value) in [
        ("model", text(model, "model")),
        ("prompt", prompt),
        ("seconds", duration),
        ("size", size),
    ] {
        if !value.is_empty() {
            form.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{key}\"\r\n\r\n{value}\r\n").as_bytes());
        }
    }
    form.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    let response = request(connection, "videos")?
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(form)
        .send()
        .await
        .map_err(|_| Error::provider("Video provider connection failed."))?;
    successful(&response).map_err(|e| crate::video::stage(e, "video_submit"))?;
    let mut result = config::response_json(response, 1024 * 1024).await?;
    let id = text(&result, "id").to_owned();
    if id.is_empty()
        || id.len() > 256
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
    {
        return Err(Error::provider(
            "Video provider returned an invalid job identifier.",
        ));
    }
    while result["status"] != "completed" {
        if !matches!(
            text(&result, "status"),
            "queued" | "in_progress" | "pending" | "processing"
        ) {
            return Err(crate::video::stage(
                Error::provider("Video generation did not complete successfully."),
                "video_poll",
            ));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        let response = authorized(
            config::http_client()?
                .get(endpoint(connection, &format!("videos/{id}"))?)
                .timeout(Duration::from_secs(30)),
            connection,
        )?
        .send()
        .await
        .map_err(|_| Error::provider("Unable to check video generation status."))?;
        successful(&response).map_err(|e| crate::video::stage(e, "video_poll"))?;
        result = config::response_json(response, 1024 * 1024).await?;
    }
    let response = authorized(
        config::http_client()?
            .get(endpoint(connection, &format!("videos/{id}/content"))?)
            .timeout(Duration::from_secs(90)),
        connection,
    )?
    .send()
    .await
    .map_err(|_| Error::provider("Unable to retrieve the generated video."))?;
    successful(&response).map_err(|e| crate::video::stage(e, "video_download"))?;
    config::response_bytes(response, MAX_MEDIA).await
}
async fn download(url: &str) -> Result<Vec<u8>> {
    let url = valid_url(url)?;
    // Signed download URLs carry their own authorization. Deployment API keys
    // must never be sent to an image CDN or any returned URL.
    let response = config::http_client()?
        .get(url)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .map_err(|_| Error::provider("Generated media download failed."))?;
    successful(&response)?;
    config::response_bytes(response, MAX_MEDIA).await
}
fn media(kind: &str, bytes: Vec<u8>) -> Result<Value> {
    let mime = match kind {
        "image" if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => "image/png",
        "image" if bytes.starts_with(b"\xff\xd8\xff") => "image/jpeg",
        "image" if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") => "image/webp",
        "video" if bytes.get(4..8) == Some(b"ftyp") => "video/mp4",
        _ => {
            return Err(Error::provider(
                "Provider returned unsupported or empty media.",
            ));
        }
    };
    if bytes.len() > MAX_MEDIA {
        return Err(Error::provider("Generated media exceeds the size limit."));
    }
    Ok(
        json!({"type":"result","kind":kind,"content_type":mime,"data":base64::engine::general_purpose::STANDARD.encode(bytes)}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
        routing::get,
    };
    use tower::ServiceExt;
    #[tokio::test]
    async fn previews_use_selected_connections_and_return_real_results() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let upstream = Router::new()
            .route("/v1/search",post(|Json(body):Json<Value>| async move { assert_eq!(body["api_key"],"preview-secret"); Json(json!({"results":[{"title":"source","url":"https://example.com/","content":"preview result"}]})) }))
            .route("/v1/images/generations",post(|headers: axum::http::HeaderMap,Json(body):Json<Value>| async move { assert_eq!(headers["authorization"],"Bearer preview-secret"); assert_eq!(body["model"],"image-test"); Json(json!({"data":[{"b64_json":base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nsynthetic")}]})) }))
            .route("/v1/embeddings",post(|| async { Json(json!({"data":[{"embedding":[0.1,0.2,0.3]}]})) }))
            .route("/v1/videos",post(|| async { Json(json!({"id":"test_video","status":"queued"})) }))
            .route("/v1/videos/test_video",get(|| async { Json(json!({"id":"test_video","status":"completed"})) }))
            .route("/v1/videos/test_video/content",get(|| async { b"\0\0\0\x18ftypmp42synthetic".to_vec() }));
        let task = tokio::spawn(axum::serve(listener, upstream).into_future());
        let home = tempfile::tempdir().unwrap();
        let app = Application::open(home.path()).unwrap();
        let mut catalog = json!({"version":1,"connections":[{"id":"source","provider":"custom","base_url":base,"api_key":"preview-secret"}],"services":{}});
        for service in ["imagegen", "videogen", "embedding"] {
            catalog["services"][service] = json!({"active_profile_id":"p","active_model_id":"m","profiles":[{"id":"p","provider_ref":{"connection_id":"source"},"models":[{"id":"m","model":if service == "imagegen" {"image-test"} else {"test-model"}}]}]});
        }
        catalog["services"]["search"] = json!({"active_profile_id":"search","profiles":[{"id":"search","provider":"tavily","base_url":base,"api_key":"preview-secret"}]});
        let router = crate::api::router(app.clone());
        for (service, kind) in [
            ("search", "search"),
            ("imagegen", "image"),
            ("videogen", "video"),
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::post(format!("/api/settings/services/{service}/preview"))
                        .header("host", "127.0.0.1")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({"catalog":catalog,"text":"test prompt"}).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
            assert_eq!(
                status,
                StatusCode::OK,
                "{}",
                String::from_utf8_lossy(&bytes)
            );
            let event: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(event["kind"], kind);
            assert_eq!(event["type"], "result");
            assert!(!String::from_utf8_lossy(&bytes).contains("preview-secret"));
        }
        let (profile, model) = selection(&catalog, "embedding", &Value::Null).unwrap();
        assert_eq!(
            connection(&catalog, profile, model, "embedding").unwrap()["api_key"],
            "preview-secret"
        );
        assert!(media("image", b"<html>not an image</html>".to_vec()).is_err());
        assert!(valid_url("file:///tmp/no").is_err());
        task.abort();
    }
}
