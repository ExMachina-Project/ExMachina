//! Client identity is shared by admin probes, live providers and persisted profiles.
use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{HeaderMap, Method, Request, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use exm_core::{config::ExmConfig, provider::ChatMessage, provider::ChatRequest, Core};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tower::ServiceExt;

struct Seen {
    path: String,
    headers: HeaderMap,
}

async fn upstream(State(seen): State<Arc<Mutex<Vec<Seen>>>>, request: Request<Body>) -> Response {
    let path = request.uri().path().to_string();
    let headers = request.headers().clone();
    let body = to_bytes(request.into_body(), 100_000).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    seen.lock().unwrap().push(Seen {
        path: path.clone(),
        headers,
    });
    if path == "/models" || path == "/v1/models" || path == "/v1beta/models" {
        return Json(json!({ "data": [{ "id": "test" }] })).into_response();
    }
    match path.as_str() {
        "/embeddings" => {
            return Json(json!({ "data": [{ "index": 0, "embedding": [0.5, 1.0] }] }))
                .into_response()
        }
        "/audio/transcriptions" => return Json(json!({ "text": "ok" })).into_response(),
        "/audio/speech" | "/asset" => return (StatusCode::OK, "fixture bytes").into_response(),
        "/images/generations" => {
            let host = seen.lock().unwrap().last().unwrap().headers["host"]
                .to_str()
                .unwrap()
                .to_string();
            return Json(json!({ "data": [{ "url": format!("http://{host}/asset") }] }))
                .into_response();
        }
        _ => {}
    }
    let response = if path == "/v1/messages" {
        if body["stream"] == true {
            return ([("content-type", "text/event-stream")], "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\ndata: [DONE]\n\n").into_response();
        }
        json!({ "content": [{ "type": "text", "text": "ok" }] })
    } else if path.contains("GenerateContent") || path.contains("generateContent") {
        let value = json!({ "candidates": [{ "content": { "parts": [{ "text": "ok" }] } }] });
        if path.contains("streamGenerateContent") {
            return (
                [("content-type", "text/event-stream")],
                format!("data: {value}\n\ndata: [DONE]\n\n"),
            )
                .into_response();
        }
        value
    } else if body["stream"] == true {
        return (
            [("content-type", "text/event-stream")],
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n",
        )
            .into_response();
    } else {
        json!({ "choices": [{ "message": { "content": "ok" } }] })
    };
    Json(response).into_response()
}

struct Fixture {
    core: Arc<Core>,
    app: Router,
    dir: PathBuf,
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    server: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new() -> Self {
        let dir =
            std::env::temp_dir().join(format!("exm-client-identity-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = ExmConfig::load(&dir);
        cfg.memory_enabled = false;
        cfg.security.auth_key = "fixture-key".into();
        let core = Arc::new(Core::with_config(cfg).unwrap());
        let app = exm_gateway::build_router(core.clone());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let service = Router::new().fallback(upstream).with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
        Self {
            core,
            app,
            dir,
            base,
            seen,
            server,
        }
    }

    async fn request(&self, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("x-auth-key", "fixture-key")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn ok(&self, method: Method, uri: &str, body: Value) -> Value {
        let (status, value) = self.request(method, uri, body).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {value}");
        value
    }

    async fn save(&self, format: &str, identity: Value) {
        self.ok(Method::POST, "/api/llm/profiles", json!({
            "id": "fixture", "baseUrl": self.base, "apiFormat": format, "apiKey": "fixture-api-key",
            "model": "test", "clientIdentity": identity,
        })).await;
        self.ok(Method::PUT, "/api/llm/active", json!({ "id": "fixture" }))
            .await;
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn chat() -> ChatRequest {
    ChatRequest::new("test", vec![ChatMessage::user("ping")])
}

#[tokio::test]
async fn probes_and_live_requests_share_identity_across_protocols() {
    let f = Fixture::new().await;
    for (format, auth, key) in [
        ("openai", "authorization", "Bearer fixture-api-key"),
        ("anthropic", "x-api-key", "fixture-api-key"),
        ("azure", "api-key", "fixture-api-key"),
        ("gemini", "x-goog-api-key", "fixture-api-key"),
    ] {
        f.save(format, json!({ "preset": "claude-code", "userAgent": "fixture-agent/1.2", "headers": { "X-App": "custom-cli", "x-client-name": format } })).await;
        f.seen.lock().unwrap().clear();
        assert_eq!(
            f.ok(Method::POST, "/api/llm/test", json!({ "id": "fixture" }))
                .await["ok"],
            true
        );
        let models = f
            .ok(Method::POST, "/api/llm/models", json!({ "id": "fixture" }))
            .await;
        assert_eq!(models["ok"], true);
        let orch = f.core.orchestrator();
        assert_eq!(orch.orch_provider.chat(chat()).await.unwrap().content, "ok");
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        assert_eq!(
            orch.orch_provider.stream(chat(), tx).await.unwrap().content,
            "ok"
        );
        assert_eq!(
            orch.model_pool
                .entry("fixture")
                .unwrap()
                .0
                .chat(chat())
                .await
                .unwrap()
                .content,
            "ok"
        );
        let seen = f.seen.lock().unwrap();
        assert_eq!(seen.len(), if format == "azure" { 4 } else { 5 });
        for call in seen.iter() {
            assert_eq!(call.headers["user-agent"], "fixture-agent/1.2");
            assert_eq!(call.headers["x-app"], "custom-cli");
            assert_eq!(call.headers["x-client-name"], format);
            // Gemini model discovery currently authenticates via query parameter.
            if format != "gemini" || call.path != "/v1beta/models" {
                assert_eq!(call.headers[auth], key);
            }
            if format == "anthropic" {
                assert_eq!(call.headers["anthropic-version"], "2023-06-01");
            }
        }
    }
}

#[tokio::test]
async fn identity_persists_and_partial_updates_keep_it_until_explicitly_disabled() {
    let f = Fixture::new().await;
    f.save("openai", json!({ "preset": "codex" })).await;
    f.ok(
        Method::POST,
        "/api/llm/profiles",
        json!({ "id": "fixture", "name": "Renamed" }),
    )
    .await;
    f.ok(Method::PUT, "/api/config", json!({ "maxConcurrency": 3 }))
        .await;
    let cfg = ExmConfig::load(&f.dir);
    assert_eq!(cfg.llm.client_identity, f.core.config().llm.client_identity);
    assert_eq!(
        cfg.llm_profiles
            .iter()
            .find(|p| p.id == "fixture")
            .unwrap()
            .client_identity,
        cfg.llm.client_identity
    );
    let reloaded = Core::with_config(cfg).unwrap();
    reloaded
        .orchestrator()
        .orch_provider
        .chat(chat())
        .await
        .unwrap();
    assert_eq!(
        f.seen.lock().unwrap().last().unwrap().headers["originator"],
        "codex_cli_rs"
    );
    let profiles = f.ok(Method::GET, "/api/llm/profiles", Value::Null).await;
    let profile = profiles["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "fixture")
        .unwrap();
    assert_eq!(profile["apiKey"], "***已配置***");
    assert_eq!(profile["clientIdentity"]["preset"], "codex");

    // Unsaved discovery uses the form identity while keeping stored credentials.
    f.ok(
        Method::POST,
        "/api/llm/models",
        json!({ "id": "fixture", "clientIdentity": { "preset": "cline" } }),
    )
    .await;
    assert_eq!(
        f.seen.lock().unwrap().last().unwrap().headers["x-title"],
        "Cline"
    );
    f.ok(
        Method::POST,
        "/api/llm/profiles",
        json!({ "id": "fixture", "clientIdentity": {} }),
    )
    .await;
    f.core
        .orchestrator()
        .orch_provider
        .chat(chat())
        .await
        .unwrap();
    let seen = f.seen.lock().unwrap();
    let headers = &seen.last().unwrap().headers;
    assert!(!headers.contains_key("user-agent"));
    assert!(!headers.contains_key("originator"));
    assert_eq!(headers["authorization"], "Bearer fixture-api-key");
    assert!(ExmConfig::load(&f.dir).llm.client_identity.is_default());
}

#[tokio::test]
async fn invalid_identity_is_rejected_before_configuration_changes() {
    let f = Fixture::new().await;
    f.save(
        "openai",
        json!({ "preset": "custom", "userAgent": "valid/1" }),
    )
    .await;
    let before = std::fs::read(&f.core.config().config_path).unwrap();
    for identity in [
        json!({ "preset": "custom", "headers": { "Authorization": "private-value" } }),
        json!({ "preset": "custom", "headers": { "content-length": "0" } }),
        json!({ "preset": "custom", "headers": { "anthropic-version": "invalid" } }),
        json!({ "preset": "custom", "headers": { "x-title": "private-value\r\nx-api-key: injected" } }),
        json!({ "preset": "custom", "headers": { "x-title": "a", "X-Title": "b" } }),
        json!({ "preset": "custom", "userAgent": "private-value\n" }),
        json!({ "preset": "custom", "userAgent": "x".repeat(2049) }),
        json!({ "preset": "unknown" }),
    ] {
        let (status, body) = f
            .request(
                Method::POST,
                "/api/llm/profiles",
                json!({ "id": "fixture", "clientIdentity": identity }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(!body.to_string().contains("private-value"));
        assert_eq!(std::fs::read(&f.core.config().config_path).unwrap(), before);
        assert_eq!(f.core.config().llm.client_identity.user_agent, "valid/1");
    }
}

#[tokio::test]
async fn auxiliary_provider_calls_use_identity_without_forwarding_it_to_asset_downloads() {
    let f = Fixture::new().await;
    f.save(
        "openai",
        json!({ "preset": "opencode", "headers": { "x-client-name": "fixture" } }),
    )
    .await;
    let provider = f.core.orchestrator().orch_provider.clone();
    assert_eq!(
        provider.embed("test", &["hello".into()]).await.unwrap(),
        vec![vec![0.5, 1.0]]
    );
    assert_eq!(
        provider
            .transcribe("test", b"fixture", "audio.wav")
            .await
            .unwrap(),
        "ok"
    );
    assert_eq!(
        provider.speak("test", "hello").await.unwrap(),
        b"fixture bytes"
    );
    assert_eq!(
        provider
            .image_generate("test", "hello", "1024x1024")
            .await
            .unwrap(),
        b"fixture bytes"
    );
    let seen = f.seen.lock().unwrap();
    assert_eq!(seen.len(), 5);
    for call in seen.iter() {
        if call.path == "/asset" {
            assert!(!call.headers.contains_key("x-client-name"));
            assert!(!call.headers.contains_key("authorization"));
        } else {
            assert_eq!(call.headers["user-agent"], "opencode");
            assert_eq!(call.headers["x-client-name"], "fixture");
            assert_eq!(call.headers["authorization"], "Bearer fixture-api-key");
        }
    }
}
