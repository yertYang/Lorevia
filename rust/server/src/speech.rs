//! Recorded speech and playback use their own catalog selections.
//! Audio stays in memory and is discarded after the provider request.
use crate::{
    Application, Error, Result,
    application::blocking,
    config::{effective_service_profile, http_client, normalize_headers, response_json},
    store::text,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Multipart, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
};
use base64::Engine;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

const MAX_AUDIO: usize = 25 * 1024 * 1024;
const NOT_CONFIGURED: &str =
    "Choose a speech-to-text model in Settings → Voice and set it as default before recording.";

struct SpeechModel {
    endpoint: reqwest::Url,
    key: String,
    headers: Value,
    model: String,
    language: String,
    voice: String,
    speed: Option<f64>,
    format: String,
    instructions: String,
    timeout: u64,
}
fn resolve(catalog: &Value, selection: &Value, kind: &str) -> Result<SpeechModel> {
    let synthesis = kind == "tts";
    let service = &catalog["services"][kind];
    let pid = selection["profile_id"]
        .as_str()
        .unwrap_or(text(service, "active_profile_id"));
    let mid = selection["model_id"]
        .as_str()
        .unwrap_or(text(service, "active_model_id"));
    let missing = || {
        Error::new(
            StatusCode::BAD_REQUEST,
            if synthesis {
                "tts_not_configured"
            } else {
                "stt_not_configured"
            },
            if synthesis {
                "Choose a text-to-speech model in Settings → Voice and set it as default before playback."
            } else {
                NOT_CONFIGURED
            },
        )
    };
    let profile = service["profiles"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| p["id"] == pid)
        .ok_or_else(missing)?;
    let model = profile["models"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|m| m["id"] == mid && !text(m, "model").trim().is_empty())
        .ok_or_else(missing)?;
    let effective = effective_service_profile(catalog, profile, model, kind)?;
    if text(model, "model").len() > 512
        || text(model, "language").len() > 64
        || text(model, "voice").len() > 512
        || text(model, "instructions").len() > 8192
    {
        return Err(Error::invalid(
            "Speech model or language exceeds the size limit.",
        ));
    }
    let binding = text(&effective, "binding").replace('-', "_");
    if synthesis && !matches!(binding.as_str(), "openai" | "custom" | "vllm") {
        return Err(Error::unsupported(
            "Choose an OpenAI-compatible speech synthesis provider.",
        ));
    }
    if !matches!(
        binding.as_str(),
        "openai" | "custom" | "groq" | "mistral" | "siliconflow" | "vllm" | "xai"
    ) {
        return Err(Error::unsupported(
            "This speech provider is not supported by the native server. Use an OpenAI-compatible audio transcription provider in Settings → Voice.",
        ));
    }
    if !text(&effective, "proxy").is_empty() || effective["api_key"].is_array() {
        return Err(Error::unsupported(
            "Speech providers require a single API key and no per-provider proxy.",
        ));
    }
    let default = match binding.as_str() {
        "openai" => "https://api.openai.com/v1",
        "groq" => "https://api.groq.com/openai/v1",
        "mistral" => "https://api.mistral.ai/v1",
        "siliconflow" => "https://api.siliconflow.cn/v1",
        "xai" => "https://api.x.ai/v1",
        _ => "",
    };
    let base = text(&effective, "base_url");
    let base = if base.trim().is_empty() {
        default
    } else {
        base.trim()
    };
    let mut endpoint = reqwest::Url::parse(base).map_err(|_| {
        Error::invalid("Configure a valid speech provider base URL in Settings → Voice.")
    })?;
    if !matches!(endpoint.scheme(), "http" | "https")
        || endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(Error::invalid(
            "Speech provider URLs must use HTTP(S) without credentials, query parameters or fragments.",
        ));
    }
    let path = endpoint.path().trim_end_matches('/');
    let suffix = if synthesis {
        "/audio/speech"
    } else {
        "/audio/transcriptions"
    };
    let path = if path.ends_with(suffix) {
        path.to_owned()
    } else {
        format!("{path}{suffix}")
    };
    endpoint.set_path(&path);
    let key = text(&effective, "api_key").to_owned();
    reqwest::header::HeaderValue::from_str(&format!("Bearer {key}"))
        .map_err(|_| Error::invalid("Invalid speech provider API key."))?;
    if key.is_empty() && !matches!(binding.as_str(), "custom" | "vllm") {
        return Err(Error::new(
            StatusCode::BAD_REQUEST,
            if synthesis {
                "tts_not_configured"
            } else {
                "stt_not_configured"
            },
            "Configure the speech provider API key in Settings → Providers.",
        ));
    }
    Ok(SpeechModel {
        endpoint,
        key,
        headers: normalize_headers(effective["extra_headers"].clone())?,
        model: text(model, "model").to_owned(),
        language: text(model, "language").to_owned(),
        voice: text(model, "voice").to_owned(),
        speed: model["speed"]
            .as_f64()
            .or_else(|| text(model, "speed").parse().ok()),
        format: text(model, "response_format").to_lowercase(),
        instructions: text(model, "instructions").to_owned(),
        timeout: model["request_timeout"]
            .as_u64()
            .or_else(|| text(model, "request_timeout").parse().ok())
            .filter(|seconds| (5..=600).contains(seconds))
            .unwrap_or(60),
    })
}

fn audio_format(content_type: &str) -> Result<(&'static str, &'static str)> {
    match content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "audio/webm" | "video/webm" => Ok(("audio/webm", "webm")),
        "audio/ogg" | "application/ogg" => Ok(("audio/ogg", "ogg")),
        "audio/mp4" | "video/mp4" | "audio/x-m4a" => Ok(("audio/mp4", "mp4")),
        "audio/wav" | "audio/wave" | "audio/x-wav" => Ok(("audio/wav", "wav")),
        "audio/mpeg" | "audio/mp3" => Ok(("audio/mpeg", "mp3")),
        "audio/flac" | "audio/x-flac" => Ok(("audio/flac", "flac")),
        _ => Err(Error::invalid(
            "Unsupported recording format. Use WebM, Ogg, MP4, WAV, MP3 or FLAC audio.",
        )),
    }
}

fn speech_request(model: &SpeechModel) -> Result<reqwest::RequestBuilder> {
    let mut request = http_client()?
        .post(model.endpoint.clone())
        .timeout(Duration::from_secs(120));
    if !model.key.is_empty() {
        request = request.bearer_auth(&model.key);
    }
    for (name, value) in model.headers.as_object().into_iter().flatten() {
        if [
            "host",
            "content-type",
            "content-length",
            "transfer-encoding",
            "connection",
        ]
        .contains(&name.to_ascii_lowercase().as_str())
        {
            return Err(Error::invalid(
                "Speech extra headers cannot override HTTP transport headers.",
            ));
        }
        request = request.header(name, value.as_str().unwrap());
    }
    Ok(request)
}

async fn transcribe(model: SpeechModel, audio: Vec<u8>, mime: &str) -> Result<Value> {
    let (mime, ext) = audio_format(mime)?;
    if audio.is_empty() || audio.len() > MAX_AUDIO {
        return Err(Error::invalid(
            "The recording must contain audio and be at most 25 MiB.",
        ));
    }
    // A bounded multipart body avoids retaining uploads on disk. All field names
    // and the filename are server-owned; the boundary is unique per request.
    let boundary = format!("speech-{}", uuid::Uuid::new_v4());
    let mut body = Vec::with_capacity(audio.len() + 1024);
    for (name, value) in [
        ("model", model.model.as_str()),
        ("response_format", "json"),
        ("language", model.language.as_str()),
    ] {
        if value.is_empty() || (name == "language" && value == "auto") {
            continue;
        }
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"recording.{ext}\"\r\nContent-Type: {mime}\r\n\r\n").as_bytes());
    body.extend_from_slice(&audio);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let request = speech_request(&model)?;
    let response = request.header("content-type", format!("multipart/form-data; boundary={boundary}")).body(body)
        .send().await.map_err(|e| {
            if e.is_timeout() {
                Error::new(StatusCode::GATEWAY_TIMEOUT, "stt_timeout", "Speech recognition timed out. Try a shorter recording or check the provider connection.")
            } else { Error::provider("Unable to reach the speech recognition provider. Check its base URL and connection.") }
        })?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let message = match status {
            401 | 403 => {
                "Speech recognition provider rejected the credentials. Check its API key and access permissions."
            }
            404 => {
                "Speech recognition endpoint or model was not found. Check the provider base URL and speech model ID."
            }
            429 => {
                "Speech recognition provider rate limit or quota exceeded. Check the provider quota and retry."
            }
            400 | 415 | 422 => {
                "Speech recognition provider rejected the recording or model. Check the speech model and audio format."
            }
            _ => "Speech recognition provider is unavailable. Try again later.",
        };
        // Never expose raw provider bodies: they may contain credentials/audio.
        return Err(Error::provider(format!("{message} (HTTP {status})")));
    }
    let value = response_json(response, 1024 * 1024).await?;
    let text = value["text"]
        .as_str()
        .ok_or_else(|| Error::provider("Speech recognition provider returned no text field."))?;
    Ok(json!({"text":text.trim()}))
}

pub(crate) fn router() -> Router<Arc<Application>> {
    Router::new()
        .route("/api/voice/tts", post(playback))
        .route("/api/settings/voice/preview", post(playback_preview))
        .route("/api/settings/voice/voices", post(voices))
        .route("/api/voice/stt", post(recording))
        .route("/api/settings/services/stt/preview", post(preview))
        .layer(DefaultBodyLimit::max(
            MAX_AUDIO.div_ceil(3) * 4 + 1024 * 1024,
        ))
}

fn draft_model(app: &Application, body: &Value, kind: &str) -> Result<SpeechModel> {
    let _guard = app.settings.settings_lock()?;
    let draft = crate::settings_api::read_draft(&app.settings)?;
    let current = app.settings.catalog_locked()?;
    let reference = if draft["catalog"].is_object() {
        &draft["catalog"]
    } else {
        &current
    };
    let catalog = crate::config::restore_catalog(body["catalog"].clone(), reference)?;
    resolve(&catalog, body, kind)
}

/// An absent voice service leaves captions enabled. Request WAV so the timeline
/// can use the recorded duration instead of estimating when a scene ends.
pub(crate) async fn lesson_voice(catalog: &Value, input: &str) -> Result<Option<Vec<u8>>> {
    let mut model = match resolve(catalog, &Value::Null, "tts") {
        Ok(m) => m,
        Err(e) if e.code == "tts_not_configured" => return Ok(None),
        Err(e) => return Err(e),
    };
    model.format = "wav".into();
    let response = synthesize(model, input, 1200).await?;
    Ok(Some(
        axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .map_err(|_| Error::provider("Lesson narration exceeds its size limit."))?
            .to_vec(),
    ))
}
async fn playback(
    State(app): State<Arc<Application>>,
    Json(body): Json<Value>,
) -> Result<axum::response::Response> {
    let input = text(&body, "text").to_owned();
    if input.chars().count() > 4096 {
        return Err(Error::invalid(
            "Speech input must contain 1–4096 characters.",
        ));
    }
    let (model, input) = blocking(move || {
        let preferences = crate::settings_api::interface(&app.settings)?;
        let input = crate::speech_text::prepare(
            &input,
            preferences["voice_math_speak"] != false,
            text(&preferences, "language").starts_with("zh"),
        );
        Ok((
            resolve(&app.settings.catalog()?, &Value::Null, "tts")?,
            input,
        ))
    })
    .await?;
    synthesize(model, &input, 16384).await
}

async fn voices(
    State(app): State<Arc<Application>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>> {
    let model = blocking(move || draft_model(&app, &body, "tts")).await?;
    let voices = if model.endpoint.host_str() == Some("api.openai.com") {
        let legacy = ["alloy", "echo", "fable", "onyx", "nova", "shimmer"];
        let current = [
            "alloy", "ash", "ballad", "coral", "echo", "fable", "onyx", "nova", "sage", "shimmer",
            "verse", "marin", "cedar",
        ];
        let names = if model.model.starts_with("tts-1") {
            legacy.as_slice()
        } else {
            current.as_slice()
        };
        names
            .iter()
            .map(|id| json!({"id":id,"label":id}))
            .collect::<Vec<_>>()
    } else {
        let mut endpoint = model.endpoint.clone();
        let path = endpoint.path().trim_end_matches("/speech");
        endpoint.set_path(&format!("{path}/voices"));
        let client = http_client()?;
        let mut request = client.get(endpoint).timeout(Duration::from_secs(20));
        if !model.key.is_empty() {
            request = request.bearer_auth(&model.key);
        }
        for (key, value) in model.headers.as_object().into_iter().flatten() {
            request = request.header(key, value.as_str().unwrap_or(""));
        }
        let response = request
            .send()
            .await
            .map_err(|_| Error::provider("Voice lookup connection failed."))?;
        if matches!(response.status().as_u16(), 404 | 405 | 501) {
            return Ok(Json(
                json!({"status":"unsupported","scope":"custom","voices":[]}),
            ));
        }
        if !response.status().is_success() {
            return Err(Error::provider(format!(
                "Voice lookup returned HTTP {}.",
                response.status().as_u16()
            )));
        }
        let value = response_json(response, 1024 * 1024).await?;
        let rows = value["voices"]
            .as_array()
            .or_else(|| value["data"].as_array())
            .or_else(|| value.as_array())
            .ok_or_else(|| Error::provider("Provider returned an invalid voice inventory."))?;
        rows.iter()
            .take(500)
            .filter_map(|row| {
                let id = row.as_str().or_else(|| row["id"].as_str())?;
                if id.is_empty() || id.len() > 512 {
                    return None;
                }
                Some(json!({"id":id,"label":row["name"].as_str().unwrap_or(id)}))
            })
            .collect()
    };
    Ok(Json(
        json!({"status":"ready","scope":"custom","voices":voices}),
    ))
}

async fn playback_preview(
    State(app): State<Arc<Application>>,
    Json(body): Json<Value>,
) -> Result<axum::response::Response> {
    let input = text(&body, "text").to_owned();
    let model = blocking(move || draft_model(&app, &body, "tts")).await?;
    synthesize(model, &input, 500).await
}

async fn synthesize(
    model: SpeechModel,
    input: &str,
    limit: usize,
) -> Result<axum::response::Response> {
    let input = input.trim();
    if input.is_empty() || input.chars().count() > limit {
        return Err(Error::invalid(format!(
            "Speech input must contain 1–{limit} characters."
        )));
    }
    let format = if model.format.is_empty() {
        "mp3"
    } else {
        &model.format
    };
    let fallback_mime = match format {
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "aac" => "audio/aac",
        "opus" => "audio/ogg",
        "flac" => "audio/flac",
        _ => {
            return Err(Error::invalid(
                "Choose MP3, WAV, AAC, Opus or FLAC for browser playback.",
            ));
        }
    };
    let mut payload = json!({"input":input,"model":model.model,"voice":if model.voice.is_empty() { "alloy" } else { &model.voice },"response_format":format});
    if !model.instructions.is_empty() {
        payload["instructions"] = json!(model.instructions);
    }
    if let Some(speed) = model.speed {
        if !speed.is_finite() || !(0.25..=4.0).contains(&speed) {
            return Err(Error::invalid("Speech speed must be between 0.25 and 4."));
        }
        payload["speed"] = json!(speed);
    }
    let response = speech_request(&model)?
        .timeout(Duration::from_secs(model.timeout))
        .json(&payload)
        .send()
        .await
        .map_err(|error| if error.is_timeout() { Error::new(StatusCode::GATEWAY_TIMEOUT, "tts_timeout", "Speech synthesis timed out. Increase Request timeout (seconds) in the speech model settings or try shorter text.") } else { Error::provider("Unable to reach the speech synthesis provider.") })?;
    if !response.status().is_success() {
        return Err(Error::provider(format!(
            "Speech synthesis provider returned HTTP {}.",
            response.status().as_u16()
        )));
    }
    let mime = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim();
    let mime = match mime {
        "application/octet-stream" => fallback_mime.to_owned(),
        other if other.starts_with("audio/") => other.to_owned(),
        _ => return Err(Error::provider("The provider returned no playable audio.")),
    };
    let bytes = crate::config::response_bytes(response, MAX_AUDIO).await?;
    if bytes.is_empty() {
        return Err(Error::provider("The provider returned no playable audio."));
    }
    Ok(([("content-type", mime)], bytes).into_response())
}

async fn preview(
    State(app): State<Arc<Application>>,
    Json(body): Json<Value>,
) -> Result<axum::response::Response> {
    let (model, audio, mime) = blocking(move || {
        let model = draft_model(&app, &body, "stt")?;
        let audio = text(&body, "audio");
        if audio.len() > MAX_AUDIO.div_ceil(3) * 4 {
            return Err(Error::invalid("Audio exceeds the size limit."));
        }
        let audio = base64::engine::general_purpose::STANDARD
            .decode(audio)
            .map_err(|_| Error::invalid("Invalid base64 audio."))?;
        Ok((model, audio, text(&body, "content_type").to_owned()))
    })
    .await?;
    let result = transcribe(model, audio, &mime).await?;
    Ok((
        [("content-type", "application/x-ndjson")],
        format!(
            "{}\n",
            json!({"type":"result","kind":"transcript","text":result["text"]})
        ),
    )
        .into_response())
}
async fn recording(
    State(app): State<Arc<Application>>,
    mut upload: Multipart,
) -> Result<Json<Value>> {
    let mut audio = None;
    while let Some(mut field) = upload
        .next_field()
        .await
        .map_err(|_| Error::invalid("Invalid audio upload."))?
    {
        if field.name() != Some("file") || audio.is_some() {
            return Err(Error::invalid("Upload exactly one audio file."));
        }
        let mime = field.content_type().unwrap_or("").to_owned();
        let mut bytes = Vec::new();
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|_| Error::invalid("Invalid or oversized audio upload."))?
        {
            if bytes.len() + chunk.len() > MAX_AUDIO {
                return Err(Error::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "audio_too_large",
                    "Recordings must be at most 25 MiB.",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        audio = Some((bytes, mime));
    }
    let (audio, mime) = audio.ok_or_else(|| Error::invalid("Upload an audio file."))?;
    // Drain a valid, bounded upload before returning configuration errors.
    // An early response closes HTTP/1.1 while Next/undici is still writing the
    // audio, turning an actionable 400 into an EPIPE and a generic HTTP 500.
    audio_format(&mime)?;
    let model = blocking(move || resolve(&app.settings.catalog()?, &Value::Null, "stt")).await?;
    Ok(Json(transcribe(model, audio, &mime).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;

    fn catalog(base: &str) -> Value {
        json!({"version":1,"connections":[{"id":"provider","provider":"openai","base_url":"https://unused.invalid/v1","api_key":"synthetic-secret","extra_headers":{"x-test":"synthetic-header"},"service_overrides":{"llm":{"enabled":true,"base_url":"https://wrong.invalid/v1"},"stt":{"enabled":true,"binding":"custom","base_url":base}}}],"services":{"llm":{"profiles":[]},"stt":{"active_profile_id":"stt-profile","active_model_id":"stt-model","profiles":[{"id":"stt-profile","provider_ref":{"connection_id":"provider"},"models":[{"id":"stt-model","model":"synthetic-whisper","language":"zh"}]}]}}})
    }
    async fn request(
        app: Arc<Application>,
        method: &str,
        path: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> (StatusCode, Vec<u8>) {
        let response = crate::api::router(app)
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("host", "localhost")
                    .header("content-type", content_type)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        (
            status,
            to_bytes(response.into_body(), 4 * 1024 * 1024)
                .await
                .unwrap()
                .to_vec(),
        )
    }
    fn upload(audio: &[u8], mime: &str) -> Vec<u8> {
        let mut body = format!("--test-boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"unsafe-user-name.webm\"\r\nContent-Type: {mime}\r\n\r\n").into_bytes();
        body.extend_from_slice(audio);
        body.extend_from_slice(b"\r\n--test-boundary--\r\n");
        body
    }
    const MULTIPART: &str = "multipart/form-data; boundary=test-boundary";

    #[test]
    fn speech_uses_default_model_provider_reference_and_stt_overrides() {
        let value = catalog("http://localhost:8199/v1/");
        let model = resolve(&value, &Value::Null, "stt").unwrap();
        assert_eq!(
            model.endpoint.as_str(),
            "http://localhost:8199/v1/audio/transcriptions"
        );
        assert_eq!(model.key, "synthetic-secret");
        assert_eq!(model.model, "synthetic-whisper");
        assert_eq!(model.language, "zh");
        for base in [
            "http://localhost:8199/v1/audio/transcriptions",
            "http://localhost:8199/v1/audio/transcriptions/",
        ] {
            assert_eq!(
                resolve(&catalog(base), &Value::Null, "stt")
                    .unwrap()
                    .endpoint
                    .as_str(),
                "http://localhost:8199/v1/audio/transcriptions"
            );
        }
        assert_eq!(
            resolve(&json!({}), &Value::Null, "stt").err().unwrap().code,
            "stt_not_configured"
        );
        for url in [
            "file:///tmp/audio",
            "https://user:secret@example.com/v1",
            "https://example.com/v1?key=secret",
        ] {
            assert!(resolve(&catalog(url), &Value::Null, "stt").is_err());
        }
        assert_eq!(
            audio_format("audio/webm;codecs=opus").unwrap(),
            ("audio/webm", "webm")
        );
        assert_eq!(audio_format("audio/mp4;codecs=mp4a.40.2").unwrap().1, "mp4");
        assert!(audio_format("text/html").is_err());
    }

    #[tokio::test]
    async fn missing_configuration_is_actionable_and_voice_preferences_persist() {
        let home = tempfile::tempdir().unwrap();
        let app = Application::open(home.path()).unwrap();
        let (status, body) = request(
            app.clone(),
            "POST",
            "/api/voice/stt",
            upload(b"synthetic", "audio/webm"),
            MULTIPART,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error_code"], "stt_not_configured");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("Settings → Voice")
        );
        for (path, key, value) in [
            ("voice-autoplay", "voice_autoplay", true),
            ("voice-math-speak", "voice_math_speak", false),
        ] {
            let (status, _) = request(
                app.clone(),
                "PUT",
                &format!("/api/settings/{path}"),
                json!({key:value}).to_string().into_bytes(),
                "application/json",
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                crate::settings_api::interface(&app.settings).unwrap()[key],
                value
            );
        }
        let (status, body) = request(app, "GET", "/api/modules", vec![], "application/json").await;
        assert_eq!(status, StatusCode::OK);
        let body: Value = serde_json::from_slice(&body).unwrap();
        let core = body["modules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == "core")
            .unwrap();
        assert_eq!(core["features"]["speech_to_text"], true);
        assert_eq!(core["features"]["speech_synthesis"], true);
    }

    #[tokio::test]
    async fn playback_and_preview_use_the_tts_profile_and_return_audio() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let upstream = Router::new().route("/v1/audio/speech", post(|headers: axum::http::HeaderMap, Json(payload): Json<Value>| async move {
            assert_eq!(headers["authorization"], "Bearer synthetic-secret");
            assert_eq!(payload, json!({"input":"Hello","model":"tts-1","voice":"nova","response_format":"mp3","speed":1.25}));
            ([("content-type","audio/mpeg")], b"mock audio".to_vec())
        }));
        let upstream = upstream.route(
            "/v1/audio/voices",
            axum::routing::get(|headers: axum::http::HeaderMap| async move {
                assert_eq!(headers["authorization"], "Bearer synthetic-secret");
                Json(json!({"voices":[{"id":"nova","name":"Nova"}]}))
            }),
        );
        let task = tokio::spawn(axum::serve(listener, upstream).into_future());
        let home = tempfile::tempdir().unwrap();
        let app = Application::open(home.path()).unwrap();
        let mut value = catalog(&base);
        value["connections"][0]["service_overrides"]["tts"] =
            json!({"enabled":true,"binding":"custom","base_url":base});
        value["services"]["tts"] = value["services"]["stt"].clone();
        value["services"]["tts"]["profiles"][0]["models"][0] =
            json!({"id":"stt-model","model":"tts-1","voice":"nova","speed":"1.25"});
        app.settings.save_catalog(value.clone()).unwrap();
        for (path, body) in [
            ("/api/voice/tts", json!({"text":"Hello"})),
            (
                "/api/settings/voice/preview",
                json!({"text":"Hello","catalog":crate::config::redact(value.clone()),"profile_id":"stt-profile","model_id":"stt-model"}),
            ),
        ] {
            let (status, audio) = request(
                app.clone(),
                "POST",
                path,
                body.to_string().into_bytes(),
                "application/json",
            )
            .await;
            assert_eq!(
                status,
                StatusCode::OK,
                "{}",
                String::from_utf8_lossy(&audio)
            );
            assert_eq!(audio, b"mock audio");
        }
        let (status, body) = request(app,"POST","/api/settings/voice/voices",json!({"catalog":crate::config::redact(value.clone()),"profile_id":"stt-profile","model_id":"stt-model"}).to_string().into_bytes(),"application/json").await;
        assert_eq!(status, StatusCode::OK);
        let inventory: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(inventory["voices"][0]["id"], "nova");
        task.abort();
    }

    #[tokio::test]
    async fn recording_and_draft_preview_forward_audio_and_redact_provider_errors() {
        async fn mock(
            headers: axum::http::HeaderMap,
            mut upload: Multipart,
        ) -> axum::response::Response {
            assert_eq!(headers["authorization"], "Bearer synthetic-secret");
            assert_eq!(headers["x-test"], "synthetic-header");
            let mut fields = std::collections::HashMap::new();
            let mut size = 0;
            while let Some(field) = upload.next_field().await.unwrap() {
                let name = field.name().unwrap().to_owned();
                if name == "file" {
                    assert_eq!(field.file_name(), Some("recording.webm"));
                    assert_eq!(field.content_type(), Some("audio/webm"));
                    size = field.bytes().await.unwrap().len();
                } else {
                    fields.insert(name, field.text().await.unwrap());
                }
            }
            assert!(size > 0);
            assert_eq!(fields["language"], "zh");
            assert_eq!(fields["response_format"], "json");
            if fields["model"] == "denied" {
                return (
                    StatusCode::UNAUTHORIZED,
                    "synthetic-secret must never escape",
                )
                    .into_response();
            }
            Json(json!({"text":"  合成测试转写  "})).into_response()
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let task = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/audio/transcriptions", post(mock))
                    .layer(DefaultBodyLimit::max(4 * 1024 * 1024)),
            )
            .into_future(),
        );
        let home = tempfile::tempdir().unwrap();
        let app = Application::open(home.path()).unwrap();
        let value = catalog(&base);
        app.settings.save_catalog(value.clone()).unwrap();
        // MediaRecorder MIME parameters and recordings over the global 2 MiB
        // JSON limit both need to survive the actual native HTTP route.
        let audio = vec![7; 3 * 1024 * 1024];
        let (status, body) = request(
            app.clone(),
            "POST",
            "/api/voice/stt",
            upload(&audio, "audio/webm;codecs=opus"),
            MULTIPART,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["text"],
            "合成测试转写"
        );
        for (audio, mime) in [
            (b"".as_slice(), "audio/webm"),
            (b"synthetic".as_slice(), "text/html"),
        ] {
            assert_eq!(
                request(
                    app.clone(),
                    "POST",
                    "/api/voice/stt",
                    upload(audio, mime),
                    MULTIPART
                )
                .await
                .0,
                StatusCode::BAD_REQUEST
            );
        }
        let mut draft = crate::config::redact(value);
        draft["services"]["stt"]["profiles"][0]["models"][0]["model"] = json!("draft-whisper");
        let body = json!({"catalog":draft,"profile_id":"stt-profile","model_id":"stt-model","audio":base64::engine::general_purpose::STANDARD.encode(b"synthetic"),"content_type":"audio/webm;codecs=opus"});
        let (status, result) = request(
            app.clone(),
            "POST",
            "/api/settings/services/stt/preview",
            body.to_string().into_bytes(),
            "application/json",
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&result)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&result).unwrap()["type"],
            "result"
        );
        assert_eq!(
            app.settings.catalog().unwrap()["services"]["stt"]["profiles"][0]["models"][0]["model"],
            "synthetic-whisper"
        );
        draft["services"]["stt"]["profiles"][0]["models"][0]["model"] = json!("denied");
        app.settings.save_catalog(draft).unwrap();
        let (status, body) = request(
            app,
            "POST",
            "/api/voice/stt",
            upload(b"synthetic", "audio/webm"),
            MULTIPART,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(String::from_utf8_lossy(&body).contains("credentials"));
        assert!(!String::from_utf8_lossy(&body).contains("synthetic-secret"));
        task.abort();
    }
}
