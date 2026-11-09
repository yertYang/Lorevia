//! Video provider transports shared by chat and Settings. Never forward a
//! provider credential when downloading the CDN URL from a completed job.
use crate::{Error, Result, config, service_preview as svc, store::text};
use serde_json::{Value, json};
use std::time::Duration;

pub(super) const LIMIT: usize = 32 * 1024 * 1024;
pub(crate) async fn generate(c: &Value, m: &Value, prompt: &str) -> Result<Vec<u8>> {
    let data = tokio::time::timeout(Duration::from_secs(600), async {
        match text(c, "binding") {
            "volcengine" | "xai" | "dashscope" => native(c, m, prompt).await,
            _ => svc::openai_video(c, m, prompt).await,
        }
    })
    .await
    .map_err(|_| Error::provider("Video generation timed out."))??;
    if data.get(4..8) != Some(b"ftyp") {
        return Err(Error::provider("Video provider returned an invalid MP4."));
    }
    Ok(data)
}
async fn send(req: reqwest::RequestBuilder, stage: &str) -> Result<Value> {
    let res = req
        .send()
        .await
        .map_err(|_| Error::provider(format!("Video {stage}: connection failed.")))?;
    if !res.status().is_success() {
        return Err(Error::provider(format!(
            "Video {stage}: provider HTTP {}.",
            res.status().as_u16()
        )));
    }
    config::response_json(res, 1024 * 1024).await
}
fn size(res: &str, ratio: &str) -> Result<String> {
    let (w, h) = match (res, ratio) {
        ("480p", "16:9") => (832, 480),
        ("480p", "9:16") => (480, 832),
        ("480p", "1:1") => (624, 624),
        ("720p", "16:9") => (1280, 720),
        ("720p", "9:16") => (720, 1280),
        ("720p", "1:1") => (960, 960),
        ("720p", "4:3") => (1088, 832),
        ("720p", "3:4") => (832, 1088),
        ("1080p", "16:9") => (1920, 1080),
        ("1080p", "9:16") => (1080, 1920),
        ("1080p", "1:1") => (1440, 1440),
        ("1080p", "4:3") => (1632, 1248),
        ("1080p", "3:4") => (1248, 1632),
        _ => {
            return Err(Error::invalid(
                "Unsupported video resolution or aspect ratio.",
            ));
        }
    };
    Ok(format!("{w}*{h}"))
}
fn wan_size(model: &str, res: &str, ratio: &str) -> Result<String> {
    let (default, permitted): (&str, &[&str]) = match model {
        "wanx2.1-t2v-turbo" => ("720p", &["480p", "720p"]),
        "wanx2.1-t2v-plus" => ("720p", &["720p"]),
        "wan2.2-t2v-plus" => ("1080p", &["480p", "1080p"]),
        "wan2.6-t2v" => ("1080p", &["720p", "1080p"]),
        "wan2.5-t2v-preview" => ("1080p", &["480p", "720p", "1080p"]),
        _ => ("", &["480p", "720p", "1080p"]),
    };
    let tier = if res.is_empty() { default } else { res };
    for &candidate in permitted {
        for r in ["16:9", "9:16", "4:3", "3:4", "1:1"] {
            if let Ok(sz) = size(candidate, r)
                && (sz == res && (ratio.is_empty() || ratio == r)
                    || candidate == tier && r == if ratio.is_empty() { "16:9" } else { ratio })
            {
                return Ok(sz);
            }
        }
    }
    Err(Error::invalid(
        "Select a supported Wan resolution and aspect ratio for this model.",
    ))
}
async fn native(c: &Value, m: &Value, prompt: &str) -> Result<Vec<u8>> {
    let binding = text(c, "binding");
    let name = text(m, "model");
    let ratio = text(m, "aspect_ratio");
    let res = text(m, "resolution");
    let duration = if text(m, "duration").is_empty() {
        None
    } else {
        Some(
            text(m, "duration")
                .parse::<u32>()
                .ok()
                .filter(|n| (1..=120).contains(n))
                .ok_or_else(|| Error::invalid("Invalid video duration."))?,
        )
    };
    let (path, payload) = match binding {
        "volcengine" => {
            let mut prompt = prompt.to_owned();
            for (k, v) in [
                ("ratio", ratio),
                ("resolution", res),
                ("duration", text(m, "duration")),
            ] {
                if !v.is_empty() {
                    prompt.push_str(&format!(" --{k} {v}"));
                }
            }
            (
                "contents/generations/tasks",
                json!({"model":name,"content":[{"type":"text","text":prompt}]}),
            )
        }
        "xai" => {
            let mut b = json!({"model":name,"prompt":prompt});
            for k in ["aspect_ratio", "resolution"] {
                if !text(m, k).is_empty() {
                    b[k] = m[k].clone();
                }
            }
            if let Some(n) = duration {
                b["duration"] = json!(n);
            }
            ("videos/generations", b)
        }
        _ => {
            let mut p = json!({});
            if !ratio.is_empty() || !res.is_empty() {
                p["size"] = json!(wan_size(name, res, ratio)?);
            }
            if let Some(n) = duration {
                if matches!(
                    name,
                    "wanx2.1-t2v-turbo" | "wanx2.1-t2v-plus" | "wan2.2-t2v-plus"
                ) {
                    if n != 5 {
                        return Err(Error::invalid("This Wan model only supports five seconds."));
                    }
                } else {
                    p["duration"] = json!(n);
                }
            }
            (
                "services/aigc/video-generation/video-synthesis",
                json!({"model":name,"input":{"prompt":prompt},"parameters":p}),
            )
        }
    };
    let mut req = svc::request(c, path)?.json(&payload);
    if binding == "dashscope" {
        req = req.header("X-DashScope-Async", "enable");
    }
    let submitted = send(req, "submit")
        .await
        .map_err(|e| super::stage(e, "video_submit"))?;
    let id = match binding {
        "xai" => text(&submitted, "request_id"),
        "dashscope" => text(&submitted["output"], "task_id"),
        _ => [
            text(&submitted, "id"),
            text(&submitted, "task_id"),
            text(&submitted["data"], "id"),
        ]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or_default(),
    };
    if id.is_empty()
        || id.len() > 256
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err(Error::provider("Video submit: invalid task ID."));
    }
    let poll = match binding {
        "xai" => format!("videos/{id}"),
        "dashscope" => format!("tasks/{id}"),
        _ => format!("{path}/{id}"),
    };
    loop {
        let response = send(
            svc::authorized(
                config::http_client()?
                    .get(svc::endpoint(c, &poll)?)
                    .timeout(Duration::from_secs(30)),
                c,
            )?,
            "poll",
        )
        .await
        .map_err(|e| super::stage(e, "video_poll"))?;
        let (status, url) = match binding {
            "xai" => (text(&response, "status"), text(&response["video"], "url")),
            "dashscope" => (
                text(&response["output"], "task_status"),
                text(&response["output"], "video_url"),
            ),
            _ => (
                [text(&response, "status"), text(&response, "state")]
                    .into_iter()
                    .find(|s| !s.is_empty())
                    .unwrap_or_default(),
                [&response["content"], &response["data"], &response]
                    .into_iter()
                    .flat_map(|v| [text(v, "video_url"), text(v, "url")])
                    .find(|s| !s.is_empty())
                    .unwrap_or_default(),
            ),
        };
        match status.to_ascii_lowercase().as_str() {
            "completed" | "succeeded" | "success" | "done" => {
                return download(text(c, "base_url"), url)
                    .await
                    .map_err(|e| super::stage(e, "video_download"));
            }
            "pending" | "queued" | "processing" | "running" | "in_progress" => {
                tokio::time::sleep(Duration::from_secs(2)).await
            }
            _ => {
                return Err(super::stage(
                    Error::provider(
                        "Video polling returned a failed, expired or unknown job status.",
                    ),
                    "video_poll",
                ));
            }
        }
    }
}
async fn download(base: &str, input: &str) -> Result<Vec<u8>> {
    let bad = || Error::provider("Invalid video download address.");
    let origin = reqwest::Url::parse(base).map_err(|_| bad())?.origin();
    let mut url = reqwest::Url::parse(input).map_err(|_| bad())?;
    for _ in 0..=4 {
        if !matches!(url.scheme(), "https" | "http")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(bad());
        }
        let mut client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(90));
        if url.origin() != origin {
            let host = url.host_str().ok_or_else(bad)?.trim_matches(['[', ']']);
            let ips = tokio::net::lookup_host((host, url.port_or_known_default().ok_or_else(bad)?))
                .await
                .map_err(|_| bad())?
                .collect::<Vec<_>>();
            if ips.is_empty() || ips.iter().any(|a| !crate::web_reading::public(a.ip())) {
                return Err(bad());
            }
            client = client.resolve_to_addrs(host, &ips);
        }
        let response = client
            .build()
            .map_err(|_| bad())?
            .get(url.clone())
            .send()
            .await
            .map_err(|_| Error::provider("Video download failed."))?;
        if response.status().is_redirection() {
            url = url
                .join(
                    response
                        .headers()
                        .get("location")
                        .and_then(|h| h.to_str().ok())
                        .ok_or_else(bad)?,
                )
                .map_err(|_| bad())?;
            continue;
        }
        svc::successful(&response)?;
        return config::response_bytes(response, LIMIT).await;
    }
    Err(bad())
}
