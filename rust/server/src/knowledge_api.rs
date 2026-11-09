use crate::{
    Application, Error, Result, application::blocking, data_api::Scope, knowledge as data,
    store::text,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Multipart, Path, Query, State},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rusqlite::params;
use serde_json::{Value, json};
use std::sync::Arc;
type App = Arc<Application>;
pub fn router() -> Router<App> {
    Router::new()
        .route("/api/knowledge-bases", get(list).post(create))
        .route("/api/knowledge-bases/list", get(list))
        .route("/api/knowledge-bases/rag-providers", get(providers))
        .route("/api/knowledge-bases/supported-file-types", get(policy))
        .route("/api/knowledge-bases/{kb}", get(detail).delete(delete))
        .route("/api/knowledge-bases/{kb}/upload", post(upload))
        .route("/api/knowledge-bases/{kb}/search", post(search))
        .route(
            "/api/knowledge-bases/{kb}/retrieval",
            get(retrieval).put(configure_retrieval),
        )
        .route("/api/knowledge-bases/{kb}/reindex", post(reindex))
        .route("/api/knowledge-bases/{kb}/source", get(source))
        .route("/api/knowledge-bases/{kb}/original", get(original))
        .route("/api/knowledge-bases/{kb}/passage", get(passage))
        .route("/api/knowledge-bases/{kb}/clip", post(clip))
        .route("/api/knowledge-bases/{kb}/files", get(files))
        .route(
            "/api/knowledge-bases/{kb}/files/{*file}",
            get(raw).delete(delete_file),
        )
        .route(
            "/api/knowledge-bases/{kb}/file-preview-text/{*file}",
            get(extracted),
        )
        .route("/api/knowledge-bases/{kb}/progress", get(progress))
        .route("/api/knowledge-bases/{kb}/indexing-run", get(indexing))
        .layer(DefaultBodyLimit::max(data::MAX_BATCH + 1024 * 1024))
}
async fn run(
    app: App,
    scope: Scope,
    write: bool,
    f: impl FnOnce(&rusqlite::Connection, &str) -> Result<Value> + Send + 'static,
) -> Result<Json<Value>> {
    crate::data_api::run(app, scope, write, |_, _| Ok(()), f).await
}
async fn list(State(app): State<App>, Query(scope): Query<Scope>) -> Result<Json<Value>> {
    run(app, scope, false, |c, s| Ok(json!(data::list(c, s)?))).await
}
async fn providers() -> Json<Value> {
    Json(
        json!([{"id":"native_lexical","name":"Native retrieval","description":"Local lexical search, persistent semantic vectors, hybrid rank fusion and optional LLM reranking with immutable source citations. Semantic retrieval requires a configured embedding service and a built index.","configured":true,"requires_api_key":false,"modes":["lexical","vector","hybrid"],"default_mode":"lexical","linkable":false,"setup_required":false,"ocr_available":crate::ocr::available()}]),
    )
}
async fn policy() -> Json<Value> {
    let extensions = crate::document_parser::EXTENSIONS
        .into_iter()
        .filter(|ext| crate::ocr::available() || !matches!(*ext, ".png" | ".jpg" | ".jpeg"))
        .collect::<Vec<_>>();
    Json(
        json!({"extensions":extensions,"accept":extensions.join(","),"max_file_size_bytes":crate::document_parser::MAX_UPLOAD,"allow_any_extension":false,"native_text_only":false,"strict_extensions":true,"ocr_available":crate::ocr::available(),"ocr_max_pixels":12_000_000,"ocr_pdf_max_pages":50}),
    )
}
async fn detail(
    State(app): State<App>,
    Path(kb): Path<String>,
    Query(scope): Query<Scope>,
) -> Result<Json<Value>> {
    run(app, scope, false, move |c, s| data::summary(c, s, &kb)).await
}
async fn files(
    State(app): State<App>,
    Path(kb): Path<String>,
    Query(scope): Query<Scope>,
) -> Result<Json<Value>> {
    run(app, scope, false, move |c, s| {
        Ok(json!({"files":data::files(c,s,&kb)?}))
    })
    .await
}
async fn raw(
    State(app): State<App>,
    Path((kb, file)): Path<(String, String)>,
    Query(scope): Query<Scope>,
) -> Result<Response> {
    let scope = scope.id()?;
    let bytes =
        blocking(move || data::original(&app.store.connect()?, &scope, &kb, &file, None)).await?;
    Ok(download(bytes))
}
fn download(bytes: Vec<u8>) -> Response {
    (
        [
            ("content-type", "application/octet-stream"),
            ("content-disposition", "attachment"),
            ("x-content-type-options", "nosniff"),
        ],
        bytes,
    )
        .into_response()
}
async fn original(
    State(app): State<App>,
    Path(kb): Path<String>,
    Query(scope): Query<Scope>,
    Query(input): Query<Source>,
) -> Result<Response> {
    let scope = scope.id()?;
    let bytes = blocking(move || {
        data::original(
            &app.store.connect()?,
            &scope,
            &kb,
            &input.path,
            Some(&input.hash),
        )
    })
    .await?;
    Ok(download(bytes))
}
async fn clip(
    State(app): State<App>,
    Path(kb): Path<String>,
    Query(scope): Query<Scope>,
    Json(input): Json<Value>,
) -> Result<Json<Value>> {
    run(app, scope, true, move |c, s| data::clip(c, s, &kb, &input)).await
}
async fn passage(
    State(app): State<App>,
    Path(kb): Path<String>,
    Query(scope): Query<Scope>,
    Query(input): Query<Source>,
) -> Result<Json<Value>> {
    run(app, scope, false, move |c,s| Ok(json!({"path":input.path,"hash":input.hash,"content":data::source(c,s,&kb,&input.path,&input.hash)?}))).await
}
async fn extracted(
    State(app): State<App>,
    Path((kb, file)): Path<(String, String)>,
    Query(scope): Query<Scope>,
) -> Result<Response> {
    let scope = scope.id()?;
    let content = blocking(move || data::read(&app.store.connect()?, &scope, &kb, &file)).await?;
    Ok((
        [
            ("content-type", "text/plain; charset=utf-8"),
            ("x-content-type-options", "nosniff"),
        ],
        content,
    )
        .into_response())
}
async fn delete_file(
    State(app): State<App>,
    Path((kb, file)): Path<(String, String)>,
    Query(scope): Query<Scope>,
) -> Result<Json<Value>> {
    run(app, scope, true, move |c, s| {
        data::read(c, s, &kb, &file)?;
        data::delete_file(c, s, &kb, &file)?;
        Ok(json!({"success":true}))
    })
    .await
}
async fn delete(
    State(app): State<App>,
    Path(kb): Path<String>,
    Query(scope): Query<Scope>,
) -> Result<Json<Value>> {
    run(app, scope, true, move |c, s| {
        data::require(c, s, &kb)?;
        for file in data::files(c, s, &kb)? {
            data::delete_file(c, s, &kb, text(&file, "name"))?;
        }
        c.execute(
            "DELETE FROM native_knowledge_bases WHERE workspace_id=? AND name=?",
            params![s, kb],
        )?;
        Ok(json!({"success":true}))
    })
    .await
}
async fn search(
    State(app): State<App>,
    Path(kb): Path<String>,
    Query(scope): Query<Scope>,
    Json(body): Json<Value>,
) -> Result<Json<Value>> {
    Ok(Json(
        crate::knowledge_retrieval::search(
            app.settings.clone(),
            app.store.clone(),
            scope.id()?,
            kb,
            body,
        )
        .await?,
    ))
}
async fn retrieval(
    State(app): State<App>,
    Path(kb): Path<String>,
    Query(scope): Query<Scope>,
) -> Result<Json<Value>> {
    let settings = app.settings.clone();
    run(app, scope, false, move |c, s| {
        crate::knowledge_retrieval::status(c, s, &kb, &settings)
    })
    .await
}
async fn configure_retrieval(
    State(app): State<App>,
    Path(kb): Path<String>,
    Query(scope): Query<Scope>,
    Json(body): Json<Value>,
) -> Result<Json<Value>> {
    let settings = app.settings.clone();
    run(app, scope, true, move |c, s| {
        crate::knowledge_retrieval::configure(c, s, &kb, &body, &settings)
    })
    .await
}
async fn reindex(
    State(app): State<App>,
    Path(kb): Path<String>,
    Query(scope): Query<Scope>,
) -> Result<Json<Value>> {
    Ok(Json(
        crate::knowledge_retrieval::rebuild(
            app.settings.clone(),
            app.store.clone(),
            scope.id()?,
            kb,
            true,
        )
        .await?,
    ))
}
async fn progress(
    State(app): State<App>,
    Path(kb): Path<String>,
    Query(scope): Query<Scope>,
) -> Result<Json<Value>> {
    run(app,scope,false,move|c,s|{data::require(c,s,&kb)?;Ok(json!({"status":"completed","progress":100,"message":"Native lexical index ready.","task_id":null}))}).await
}
async fn indexing(
    State(app): State<App>,
    Path(kb): Path<String>,
    Query(scope): Query<Scope>,
) -> Result<Json<Value>> {
    run(app, scope, false, move |c, s| {
        data::require(c, s, &kb)?;
        Ok(json!({"run":null}))
    })
    .await
}
struct Upload {
    name: String,
    provider: String,
    storage: Option<String>,
    documents: Vec<data::Document>,
}
async fn parse(mut multipart: Multipart) -> Result<Upload> {
    let mut name = String::new();
    let mut provider = String::new();
    let mut storage = None;
    let mut directory = String::new();
    let mut paths = Vec::new();
    let mut documents = Vec::new();
    let mut bytes = 0usize;
    let mut fields = 0;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| Error::invalid("Malformed or oversized multipart upload."))?
    {
        fields += 1;
        if fields > 220 {
            return Err(Error::invalid("Too many upload fields."));
        }
        let key = field.name().unwrap_or("").to_owned();
        if key == "files" {
            let file = field
                .file_name()
                .ok_or_else(|| Error::invalid("Upload filename is required."))?
                .to_owned();
            let raw = field
                .bytes()
                .await
                .map_err(|_| Error::invalid("Malformed upload file."))?;
            bytes += raw.len();
            if raw.len() > crate::document_parser::MAX_UPLOAD || bytes > data::MAX_BATCH {
                return Err(Error::invalid("Upload size limit exceeded."));
            }
            let original = raw.to_vec();
            let parser_file = file.clone();
            let (content, original) = blocking(move || {
                Ok((
                    crate::document_parser::parse(&parser_file, &original)?,
                    original,
                ))
            })
            .await?;
            documents.push(data::Document {
                path: file,
                content,
                original,
            });
        } else {
            let value = field
                .text()
                .await
                .map_err(|_| Error::invalid("Invalid upload field."))?;
            if value.len() > 2048 {
                return Err(Error::invalid("Upload field is too long."));
            }
            match key.as_str() {
                "name" => name = value,
                "rag_provider" => provider = value,
                "storage_workspace_id" => storage = Some(value),
                "dest_subdir" => directory = value,
                "rel_paths" => paths.push(value),
                _ => {
                    return Err(Error::unsupported(
                        "This upload option is not supported by the native lexical provider.",
                    ));
                }
            }
        }
    }
    if !paths.is_empty() && paths.len() != documents.len() {
        return Err(Error::invalid(
            "Relative paths must match the uploaded files.",
        ));
    }
    if !directory.is_empty() {
        data::path(&directory)?;
    }
    for (i, doc) in documents.iter_mut().enumerate() {
        let extension = doc
            .path
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if let Some(path) = paths.get(i).filter(|p| !p.is_empty()) {
            doc.path = path.clone();
        }
        if !directory.is_empty() {
            doc.path = format!("{directory}/{}", doc.path);
        }
        if doc
            .path
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
            != extension
        {
            return Err(Error::invalid(
                "Relative paths must preserve the uploaded file format.",
            ));
        }
        data::validate(doc)?;
    }
    Ok(Upload {
        name,
        provider,
        storage,
        documents,
    })
}
async fn save(app: App, scope: Scope, upload: Upload, kb: Option<String>) -> Result<Json<Value>> {
    let selected = scope.id()?;
    if upload.storage.as_ref().is_some_and(|s| s != &selected) {
        return Err(Error::invalid(
            "Upload storage must match the selected workspace.",
        ));
    }
    if !matches!(upload.provider.as_str(), "" | "native_lexical") {
        return Err(Error::unsupported(
            "Select the native_lexical provider; legacy retrieval engines have not been migrated.",
        ));
    }
    let create = kb.is_none();
    let kb = kb.unwrap_or(upload.name);
    run(app, scope, true, move |c, s| {
        data::ingest(c, s, &kb, &upload.documents, create)
    })
    .await
}
async fn create(
    State(app): State<App>,
    Query(scope): Query<Scope>,
    multipart: Multipart,
) -> Result<Json<Value>> {
    save(app, scope, parse(multipart).await?, None).await
}
async fn upload(
    State(app): State<App>,
    Path(kb): Path<String>,
    Query(scope): Query<Scope>,
    multipart: Multipart,
) -> Result<Json<Value>> {
    save(app, scope, parse(multipart).await?, Some(kb)).await
}

#[derive(serde::Deserialize)]
struct Source {
    path: String,
    hash: String,
}
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
async fn source(
    State(app): State<App>,
    Path(kb): Path<String>,
    Query(scope): Query<Scope>,
    Query(input): Query<Source>,
) -> Result<Response> {
    let scope = scope.id()?;
    let content = blocking(move || {
        data::source(&app.store.connect()?, &scope, &kb, &input.path, &input.hash)
    })
    .await?;
    let mut html = String::from(
        "<!doctype html><meta charset=utf-8><title>Knowledge source</title><style>body{font:16px/1.6 monospace;margin:2rem}pre{white-space:pre-wrap}.line{display:block;scroll-margin-top:1rem}.line:target{background:#ffe899}a{color:#777;text-decoration:none;margin-right:1rem;user-select:none}</style><pre>",
    );
    for (index, line) in content.split('\n').enumerate() {
        let n = index + 1;
        html.push_str(&format!(
            "<span class=line id=L{n}><a href=#L{n}>{n}</a>{}</span>",
            escape(line)
        ));
    }
    html.push_str("</pre>");
    Ok((
        [
            ("content-type", "text/html; charset=utf-8"),
            (
                "content-security-policy",
                "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'self'",
            ),
            ("x-content-type-options", "nosniff"),
        ],
        html,
    )
        .into_response())
}
