use super::Artifact;
use crate::{Error, Result, learning::string};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{process::Command, sync::Semaphore};

static RENDER_SLOT: Semaphore = Semaphore::const_new(1);
fn runtime() -> PathBuf {
    std::env::var_os("DEEPTUTOR_VIDEO_RUNTIME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/video-lesson")
        })
}
pub(crate) fn available() -> bool {
    let root = runtime();
    [
        "build.mjs",
        "kit.tsx",
        "node_modules/esbuild/package.json",
        "node_modules/@remotion/player/package.json",
        "node_modules/quickjs-emscripten/package.json",
    ]
    .iter()
    .all(|p| root.join(p).is_file())
}
fn wav_frames(b: &[u8]) -> Result<usize> {
    let invalid = || Error::provider("Narration service did not return a valid WAV recording.");
    if b.get(..4) != Some(b"RIFF") || b.get(8..12) != Some(b"WAVE") {
        return Err(invalid());
    }
    let mut i = 12;
    let mut rate = 0;
    let mut size = 0;
    while i + 8 <= b.len() {
        let n = u32::from_le_bytes(b[i + 4..i + 8].try_into().unwrap()) as usize;
        let chunk = b.get(i + 8..).ok_or_else(invalid)?;
        if &b[i..i + 4] == b"data" {
            size = n.min(chunk.len());
            break;
        }
        if n > chunk.len() {
            return Err(invalid());
        }
        if &b[i..i + 4] == b"fmt " && n >= 16 {
            rate = u32::from_le_bytes(chunk[8..12].try_into().unwrap()) as usize;
        }
        i += 8 + n + (n % 2);
    }
    if rate == 0 || size == 0 {
        return Err(invalid());
    }
    Ok((size * 30).div_ceil(rate))
}
fn captions(text: &str, frames: usize) -> Vec<Value> {
    let total = text.chars().count();
    let mut lines = vec![];
    let mut line = String::new();
    let mut at = 0;
    let mut count = 0;
    for ch in text.chars() {
        line.push(ch);
        count += 1;
        if "。！？!?；;，,\n".contains(ch)
            || count - at >= 68
            || (count - at >= 36 && ch.is_whitespace())
            || count == total
        {
            if !line.trim().is_empty() {
                lines.push(json!({"at":frames*at/total,"text":line.trim()}));
            }
            line.clear();
            at = count;
        }
    }
    lines
}
async fn build(root: &Path, dir: &Path, voices: &Value) -> Result<(String, Value)> {
    std::fs::write(
        dir.join("voice.ts"),
        format!("export const VOICE = {voices};"),
    )?;
    let cmd = std::env::var_os("DEEPTUTOR_BROWSER_NODE").unwrap_or_else(|| "node".into());
    let output = tokio::time::timeout(
        Duration::from_secs(195),
        Command::new(cmd)
            .arg("--max-old-space-size=256")
            .arg(root.join("build.mjs"))
            .arg(dir.join("Lesson.tsx"))
            .arg(dir.join("player.js"))
            .env("DEEPTUTOR_REMOTION_RUNTIME", root)
            .current_dir(dir)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| Error::invalid("Frame checking timed out; simplify this lesson."))??;
    let report: Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| Error::provider("Lesson compiler did not return its validation report."))?;
    if !output.status.success() || report["ok"] != true {
        let detail = report["errors"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .take(3)
            .collect::<Vec<_>>()
            .join("; ");
        return Err(Error::new(
            axum::http::StatusCode::BAD_REQUEST,
            match report["stage"].as_str() {
                Some("compile") => "lesson_compile",
                Some("bundle") => "lesson_bundle",
                _ => "lesson_render",
            },
            format!(
                "Lesson {}: {}",
                report["stage"],
                detail.chars().take(1100).collect::<String>()
            ),
        ));
    }
    let js = dir.join("player.js");
    if std::fs::metadata(&js)?.len() > 3 * 1024 * 1024 {
        return Err(Error::invalid("Lesson player exceeds the bundle limit."));
    }
    Ok((std::fs::read_to_string(js)?, report))
}
pub(super) async fn render(catalog: &Value, args: &Value) -> Result<Artifact> {
    let title = string(args, "title", 150, true)?;
    let code = string(args, "code", 50000, true)?;
    let items = args["narration"]
        .as_array()
        .filter(|a| (1..=7).contains(&a.len()))
        .ok_or_else(|| Error::invalid("Provide 1–7 narration segments."))?;
    let mut voices = json!({});
    let mut segments = vec![];
    for item in items {
        let id = string(item, "id", 40, true)?;
        let text = string(item, "text", 500, true)?;
        if !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || !voices[&id].is_null()
            || matches!(id.as_str(), "__proto__" | "constructor" | "prototype")
        {
            return Err(Error::invalid("Use unique alphanumeric narration IDs."));
        }
        let frames = (text.chars().count() * 5).max(60);
        voices[&id] = json!({"text":text,"frames":frames,"captions":captions(&text,frames)});
        segments.push((id, text));
    }
    let _permit = RENDER_SLOT.acquire().await.expect("renderer semaphore");
    let dir = tempfile::tempdir()?;
    let root = runtime().canonicalize()?;
    for name in ["kit.tsx", "china_outline.json"] {
        std::fs::copy(root.join(name), dir.path().join(name))?;
    }
    std::fs::write(dir.path().join("Lesson.tsx"), code)?;
    // Catch invalid compositions before issuing a potentially billable TTS call.
    let (mut js, mut report) = build(&root, dir.path(), &voices).await?;
    let mut media = vec![];
    let mut size = 0;
    for (id, text) in segments {
        let Some(audio) = crate::speech::lesson_voice(catalog, &text)
            .await
            .map_err(|e| super::stage(e, "lesson_narration"))?
        else {
            break;
        };
        size += audio.len();
        if size > 12 * 1024 * 1024 {
            return Err(Error::provider(
                "Lesson narration exceeds its audio budget.",
            ));
        }
        let frames = wav_frames(&audio)?;
        // Keep bulk audio out of the frame-check VM. Replace only this random
        // token after the exact recorded timeline passes validation.
        let token = format!("LOREVIAVOICE{}", uuid::Uuid::new_v4().simple());
        voices[&id] = json!({"text":text,"frames":frames,"captions":captions(&text,frames),"src":format!("data:audio/wav;base64,{token}")});
        media.push((token, STANDARD.encode(audio)));
    }
    if !media.is_empty() {
        (js, report) = build(&root, dir.path(), &voices).await?;
        for (token, data) in &media {
            js = js.replace(token, data);
        }
    }
    let html = format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Video lesson</title><style>html,body,#player{{margin:0;background:#111;width:100%}}</style></head><body><div id="player"></div><script>const s=document.createElement('script');s.textContent=new TextDecoder().decode(Uint8Array.from(atob('{}'),c=>c.charCodeAt(0)));document.body.appendChild(s)</script></body></html>"#,
        STANDARD.encode(js)
    );
    Ok(Artifact {
        title,
        bytes: html.into_bytes(),
        lesson: Some(
            json!({"duration_seconds":(report["durationInFrames"].as_u64().unwrap_or_default() as f64/30.0*10.0).round()/10.0,"checked_frames":report["frames"].as_array().map(Vec::len),"narration":if media.is_empty(){"captions_only"}else{"voice_and_captions"}}),
        ),
    })
}
