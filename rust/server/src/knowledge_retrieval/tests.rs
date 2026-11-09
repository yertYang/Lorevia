use super::*;
use axum::{
    Json, Router,
    extract::State,
    response::{IntoResponse, Response},
    routing::post,
};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Mock {
    calls: AtomicUsize,
    items: AtomicUsize,
    mode: AtomicUsize,
    reranks: AtomicUsize,
}
async fn embeddings(
    State(mock): State<Arc<Mock>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    assert_eq!(headers["authorization"], "Bearer retrieval-test-secret");
    assert_eq!(body["encoding_format"], "float");
    let input = body["input"].as_array().unwrap();
    mock.calls.fetch_add(1, Ordering::SeqCst);
    mock.items.fetch_add(input.len(), Ordering::SeqCst);
    let mode = mock.mode.load(Ordering::SeqCst);
    if mode == 1 {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "secret provider response must not be logged",
        )
            .into_response();
    }
    let rows = input
        .iter()
        .enumerate()
        .rev()
        .map(|(index, v)| {
            let content = v.as_str().unwrap().to_lowercase();
            let vector = match mode {
                2 => json!([0., 0.]),
                3 => json!([1., 0., 0.]),
                _ => {
                    if content.contains("banana") || content.contains("fruit") {
                        json!([0., 1.])
                    } else {
                        json!([1., 0.])
                    }
                }
            };
            json!({"index":if mode==4 {0}else{index},"embedding":vector})
        })
        .collect::<Vec<_>>();
    Json(json!({"data":rows})).into_response()
}
async fn rerank(State(mock): State<Arc<Mock>>, Json(body): Json<Value>) -> Response {
    mock.reranks.fetch_add(1, Ordering::SeqCst);
    assert!(body.get("tools").is_none());
    let input: Value = serde_json::from_str(
        body["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let len = input["passages"].as_array().unwrap().len();
    let ranking = if mock.mode.load(Ordering::SeqCst) == 5 {
        vec![0; len]
    } else {
        (0..len).rev().collect::<Vec<_>>()
    };
    let content = json!({"ranking":ranking}).to_string();
    let event = json!({"choices":[{"index":0,"delta":{"content":content},"finish_reason":null}]})
        .to_string();
    ([("content-type","text/event-stream")],format!("data: {event}\n\ndata: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n")).into_response()
}
struct Fixture {
    _home: tempfile::TempDir,
    app: Arc<crate::Application>,
    mock: Arc<Mock>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn api(&self, method: &str, path: &str, body: Value) -> (u16, Value) {
        use tower::ServiceExt;
        let request = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
        let response = crate::api::router(self.app.clone())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status().as_u16();
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }
    async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let mock = Arc::new(Mock::default());
        let router = Router::new()
            .route("/v1/embeddings", post(embeddings))
            .route("/v1/chat/completions", post(rerank))
            .with_state(mock.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let home = tempfile::tempdir().unwrap();
        let app = crate::Application::open(home.path()).unwrap();
        let mut catalog = json!({"version":1,"services":{}});
        for service in ["llm", "embedding"] {
            catalog["services"][service] = json!({"active_profile_id":"p","active_model_id":"m","profiles":[{"id":"p","binding":"custom","base_url":base,"api_key":"retrieval-test-secret","models":[{"id":"m","model":"test-model","dimension":"2","send_dimensions":true}]}]});
        }
        app.settings.save_catalog(catalog).unwrap();
        Self {
            _home: home,
            app,
            mock,
            task,
        }
    }
    fn ingest(&self, scope: &str, kb: &str, files: &[(&str, &str)], create: bool) {
        let mut c = self.app.store.connect().unwrap();
        let tx = c.transaction().unwrap();
        let documents = files
            .iter()
            .map(|(p, t)| knowledge::Document {
                path: p.to_string(),
                content: t.to_string(),
                original: t.as_bytes().to_vec(),
            })
            .collect::<Vec<_>>();
        knowledge::ingest(&tx, scope, kb, &documents, create).unwrap();
        tx.commit().unwrap();
    }
    fn status(&self) -> Value {
        status(
            &self.app.store.connect().unwrap(),
            "space-a",
            "library",
            &self.app.settings,
        )
        .unwrap()
    }
    async fn build(&self) -> Result<Value> {
        rebuild(
            self.app.settings.clone(),
            self.app.store.clone(),
            "space-a".into(),
            "library".into(),
            false,
        )
        .await
    }
    async fn search(&self, body: Value) -> Result<Value> {
        search(
            self.app.settings.clone(),
            self.app.store.clone(),
            "space-a".into(),
            "library".into(),
            body,
        )
        .await
    }
}
#[tokio::test]
async fn semantic_hybrid_rerank_and_generation_lifecycle() {
    let f = Fixture::new().await;
    let files = [
        ("auto.txt", "An automobile uses gasoline."),
        ("fruit.txt", "Banana fruit salad."),
        ("manual.txt", "Car manuals mention engines."),
    ];
    f.ingest("space-a", "library", &files, true);
    f.ingest(
        "space-b",
        "library",
        &[("private.txt", "Secret vehicle transport details.")],
        true,
    );
    assert_eq!(f.status()["status"], "not_indexed");
    let error = f
        .search(json!({"query":"vehicle transport","mode":"vector"}))
        .await
        .unwrap_err();
    assert_eq!(error.status, axum::http::StatusCode::CONFLICT);
    assert_eq!(f.mock.calls.load(Ordering::SeqCst), 0);
    f.build().await.unwrap();
    assert_eq!(f.status()["status"], "ready");
    assert_eq!(f.status()["indexed_chunks"], 3);
    // A rebuild of unchanged chunks performs no embedding requests.
    let calls = f.mock.calls.load(Ordering::SeqCst);
    f.build().await.unwrap();
    assert_eq!(f.mock.calls.load(Ordering::SeqCst), calls);
    let lexical = f
        .search(json!({"query":"vehicle transport","mode":"lexical"}))
        .await
        .unwrap();
    assert!(lexical["chunks"].as_array().unwrap().is_empty());
    let vector = f
        .search(json!({"query":"vehicle transport","mode":"vector","top_k":2}))
        .await
        .unwrap();
    assert_eq!(vector["chunks"].as_array().unwrap().len(), 2);
    for row in vector["chunks"].as_array().unwrap() {
        assert_ne!(row["source_path"], "fruit.txt");
        assert_ne!(row["source_path"], "private.txt");
        assert!(text(row, "citation").contains("dt_workspace=space-a"));
    }
    let hybrid = f
        .search(json!({"query":"automobile","mode":"hybrid"}))
        .await
        .unwrap();
    assert_eq!(hybrid["chunks"][0]["source_path"], "auto.txt");
    assert!(hybrid["chunks"][0]["lexical_score"].is_number());
    assert!(hybrid["chunks"][0]["vector_score"].is_number());
    let ranked = f
        .search(json!({"query":"automobile","mode":"hybrid","rerank":true}))
        .await
        .unwrap();
    assert_eq!(
        ranked["chunks"][0]["id"],
        hybrid["chunks"].as_array().unwrap().last().unwrap()["id"]
    );
    assert_eq!(ranked["chunks"][0]["rerank_position"], 1);
    assert_eq!(f.mock.reranks.load(Ordering::SeqCst), 1);
    let filtered = f
        .search(json!({"query":"vehicle","mode":"vector","source_path":"fruit.txt"}))
        .await
        .unwrap();
    assert_eq!(filtered["chunks"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["chunks"][0]["source_path"], "fruit.txt");
    f.mock.mode.store(5, Ordering::SeqCst);
    assert!(
        f.search(json!({"query":"vehicle","mode":"hybrid","rerank":true}))
            .await
            .unwrap_err()
            .message
            .contains("duplicate")
    );
    f.mock.mode.store(0, Ordering::SeqCst);
    let previous:String=f.app.store.connect().unwrap().query_row("SELECT generation FROM native_knowledge_retrieval WHERE workspace_id='space-a' AND kb='library'",[],|r|r.get(0)).unwrap();
    f.ingest(
        "space-a",
        "library",
        &[("new.txt", "Vehicle repair procedures.")],
        false,
    );
    assert_eq!(f.status()["status"], "stale");
    let calls = f.mock.calls.load(Ordering::SeqCst);
    assert!(
        f.search(json!({"query":"repair","mode":"hybrid"}))
            .await
            .is_err()
    );
    assert_eq!(calls, f.mock.calls.load(Ordering::SeqCst));
    f.mock.mode.store(1, Ordering::SeqCst);
    let failure = f.build().await.unwrap_err();
    assert!(failure.message.contains("503"));
    assert!(!failure.message.contains("secret provider"));
    assert_eq!(f.status()["error_code"], "provider_error");
    assert!(!text(&f.status(), "operation_id").is_empty());
    let generation:String=f.app.store.connect().unwrap().query_row("SELECT generation FROM native_knowledge_retrieval WHERE workspace_id='space-a' AND kb='library'",[],|r|r.get(0)).unwrap();
    assert_eq!(previous, generation);
    f.mock.mode.store(0, Ordering::SeqCst);
    let items = f.mock.items.load(Ordering::SeqCst);
    f.build().await.unwrap();
    assert_eq!(f.mock.items.load(Ordering::SeqCst) - items, 1);
    assert_eq!(f.status()["status"], "ready");
    let c = f.app.store.connect().unwrap();
    configure(
        &c,
        "space-a",
        "library",
        &json!({"mode":"hybrid","rerank":false,"candidate_k":20}),
        &f.app.settings,
    )
    .unwrap();
    assert_eq!(
        f.search(json!({"query":"vehicle"})).await.unwrap()["retrieval_mode"],
        "hybrid"
    );
    let admission = f
        .app
        .store
        .admit(
            &json!({"workspace_id":"space-a","content":"Search my library","capability":"chat"}),
            "test-owner",
        )
        .unwrap();
    let registry = crate::tools::ToolRegistry {
        settings: f.app.settings.clone(),
        store: f.app.store.clone(),
        admission,
    };
    let flags = crate::learning::ToolFlags {
        knowledge_bases: vec!["library".into()],
        ..Default::default()
    };
    let tool = crate::tools::ToolCall {
        id: "retrieval-tool".into(),
        name: "rag".into(),
        arguments: json!({"query":"vehicle transport","kb_name":"library"}).to_string(),
    };
    let outcome = registry
        .execute(
            &tool,
            "retrieval-invocation",
            &mut Default::default(),
            &flags,
        )
        .await;
    assert_eq!(outcome.result["success"], true, "{}", outcome.result);
    assert_eq!(outcome.result["content"]["retrieval_mode"], "hybrid");
    // The vector space is pinned even when dimensions remain identical.
    let mut catalog = f.app.settings.catalog().unwrap();
    catalog["services"]["embedding"]["profiles"][0]["models"][0]["model"] =
        json!("different-model");
    f.app.settings.save_catalog(catalog).unwrap();
    assert_eq!(f.status()["status"], "stale");
    assert!(f.search(json!({"query":"vehicle"})).await.is_err());
    f.build().await.unwrap();
    let old_id = hybrid["chunks"][0]["id"].as_str().unwrap();
    // Old citations remain readable after replacement, as before semantic indexing.
    let hash = text(&hybrid["chunks"][0], "source_hash");
    f.ingest(
        "space-a",
        "library",
        &[("auto.txt", "An updated electric automobile.")],
        false,
    );
    assert_eq!(
        knowledge::source(&c, "space-a", "library", "auto.txt", hash).unwrap(),
        files[0].1
    );
    knowledge::delete_file(&c, "space-a", "library", "auto.txt").unwrap();
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM native_knowledge_vectors WHERE chunk_id=?",
            [old_id],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(f.status()["status"], "stale");
    // Failed/zero/different-dimension/duplicate embedding responses are rejected.
    for mode in [2, 3, 4] {
        f.mock.mode.store(mode, Ordering::SeqCst);
        let model = Embedding::load(&f.app.settings).unwrap();
        assert!(
            model
                .embed(&["one".into(), "two".into()], Some(2))
                .await
                .is_err()
        );
    }
    f.mock.mode.store(0, Ordering::SeqCst);
    // A competing builder cannot publish a stale snapshot after an import.
    let model = Embedding::load(&f.app.settings).unwrap();
    let build = begin(
        &f.app.store,
        "space-a",
        "library",
        &model,
        "concurrency-test",
    )
    .unwrap();
    assert!(begin(&f.app.store, "space-a", "library", &model, "other").is_err());
    f.ingest(
        "space-a",
        "library",
        &[("changed.txt", "A concurrent import.")],
        false,
    );
    assert!(
        publish(
            &f.app.store,
            "space-a",
            "library",
            &build.generation,
            &model,
            2
        )
        .unwrap_err()
        .message
        .contains("Documents changed")
    );
    c.execute(
        "UPDATE native_knowledge_retrieval SET build_started=0 WHERE workspace_id='space-a'",
        [],
    )
    .unwrap();
    assert_eq!(f.status()["status"], "interrupted");
    f.build().await.unwrap();
    assert_eq!(f.status()["status"], "ready");
    // Exercise the same HTTP routes used by the native page, including detached builds.
    let path = "/api/knowledge-bases/library";
    let (code, response) = f
        .api(
            "GET",
            &format!("{path}/retrieval?dt_workspace=space-a"),
            Value::Null,
        )
        .await;
    assert_eq!(code, 200);
    assert_eq!(response["status"], "ready");
    assert!(!response.to_string().contains("retrieval-test-secret"));
    let (code, response) = f
        .api(
            "POST",
            &format!("{path}/search?dt_workspace=space-a"),
            json!({"query":"vehicle transport"}),
        )
        .await;
    assert_eq!(code, 200);
    assert_eq!(response["retrieval_mode"], "hybrid");
    assert_eq!(
        f.api(
            "POST",
            &format!("{path}/search?dt_workspace=space-a"),
            json!({"query":"vehicle","top_k":11})
        )
        .await
        .0,
        400
    );
    assert_eq!(
        f.api(
            "PUT",
            &format!("{path}/retrieval?dt_workspace=space-a"),
            json!({"mode":"fake-provider"})
        )
        .await
        .0,
        400
    );
    assert_eq!(
        f.api(
            "GET",
            &format!("{path}/retrieval?dt_workspace=space-a&workspace_id=space-b"),
            Value::Null
        )
        .await
        .0,
        400
    );
    assert_eq!(
        f.api(
            "POST",
            &format!("{path}/search?dt_workspace=space-b"),
            json!({"query":"vehicle","mode":"vector"})
        )
        .await
        .0,
        409
    );
    let (code, response) = f
        .api(
            "POST",
            &format!("{path}/reindex?dt_workspace=space-a"),
            Value::Null,
        )
        .await;
    assert_eq!(code, 200);
    assert_eq!(response["status"], "building");
    for _ in 0..100 {
        if f.status()["status"] != "building" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(f.status()["status"], "ready");
}
#[test]
fn fusion_is_rank_based_and_rejects_invalid_rerankings() {
    let rows = fuse(
        vec![
            json!({"id":"a","score":0.01}),
            json!({"id":"b","score":0.001}),
        ],
        vec![
            json!({"id":"b","score":0.99}),
            json!({"id":"c","score":0.9}),
        ],
        10,
    );
    assert_eq!(rows[0]["id"], "b");
    assert_eq!(rows.len(), 3);
    assert!(reranked(rows.clone(), r#"{"ranking":[0,0,2]}"#).is_err());
    assert!(reranked(rows.clone(), r#"{"ranking":[0]}"#).is_err());
    assert!(reranked(rows.clone(), r#"{"ranking":[0,1,99]}"#).is_err());
    assert!(reranked(rows, "not JSON").is_err());
}
