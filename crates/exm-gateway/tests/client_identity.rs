//! Client identity is shared by admin probes, live providers and persisted profiles.
use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{HeaderMap, Method, Request, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use exm_core::{
    config::ExmConfig,
    provider::{ChatMessage, ChatRequest, StreamDelta, ToolSpec},
    Core,
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tower::ServiceExt;

struct Seen {
    path: String,
    headers: HeaderMap,
    body: Value,
}

async fn upstream(State(seen): State<Arc<Mutex<Vec<Seen>>>>, request: Request<Body>) -> Response {
    let path = request.uri().path().to_string();
    let headers = request.headers().clone();
    let body = to_bytes(request.into_body(), 100_000).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    seen.lock().unwrap().push(Seen {
        path: path.clone(),
        headers,
        body: body.clone(),
    });
    if path == "/models" || path == "/v1/models" || path == "/v1beta/models" {
        return Json(json!({ "data": [{ "id": "test" }] })).into_response();
    }
    if body["model"] == "client-rejected" {
        return (
            StatusCode::FORBIDDEN,
            Json(
                json!({"error":{"code":"client_not_allowed","message":"private-fixture-content"}}),
            ),
        )
            .into_response();
    }
    if path == "/responses" || path == "/v1/responses" {
        if body["model"] == "http402" || body["model"] == "http429" {
            let status = if body["model"] == "http402" {
                StatusCode::PAYMENT_REQUIRED
            } else {
                StatusCode::TOO_MANY_REQUESTS
            };
            return (status, Json(json!({"error":{"message":"fixture quota"}}))).into_response();
        }
        let response = json!({"status":"completed","output":[{"type":"reasoning","summary":[{"type":"summary_text","text":"检查"}]},{"type":"message","content":[{"type":"output_text","text":"你好"}]}],"usage":{"input_tokens":11,"output_tokens":7}});
        if body["stream"] != true {
            return Json(response).into_response();
        }
        let events = if body["model"] == "failed-event" {
            vec![
                json!({"type":"response.failed","response":{"error":{"message":"private-fixture-content"}}}),
            ]
        } else if body["model"] == "truncated" {
            vec![json!({"type":"response.output_text.delta","delta":"unfinished"})]
        } else if body["tools"].is_array()
            && !body["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["type"] == "function_call_output")
        {
            let arguments = if body["model"] == "invalid-args" {
                "{invalid"
            } else {
                "{\"path\":\"fixture.txt\"}"
            };
            vec![
                json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read","arguments":""}}),
                json!({"type":"response.function_call_arguments.delta","output_index":0,"delta":arguments}),
                json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read","arguments":arguments}],"usage":{"input_tokens":8,"output_tokens":5}}}),
            ]
        } else {
            vec![
                json!({"type":"response.output_text.delta","delta":"你"}),
                json!({"type":"response.reasoning_summary_text.delta","delta":"检"}),
                json!({"type":"response.completed","response":response}),
            ]
        };
        return (
            [("content-type", "text/event-stream")],
            events
                .iter()
                .map(|event| format!("data: {event}\r\n\r\n"))
                .collect::<String>(),
        )
            .into_response();
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
        f.save(format, json!({ "preset": "claude-code", "requestMode":"headers", "userAgent": "fixture-agent/1.2", "headers": { "X-App": "custom-cli", "x-client-name": format } })).await;
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
        json!({ "preset": "codex", "version": "private-value\n" }),
        json!({ "preset": "codex", "version": "a".repeat(65) }),
        json!({ "preset": "codex", "requestMode": "unknown" }),
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
async fn session_identity_is_stable_per_caller_and_isolated_between_concurrent_calls() {
    use exm_core::model_routing::{ModelCaller, MODEL_CALLER};
    let f = Fixture::new().await;
    f.save("openai", json!({"preset":"codex"})).await;
    let provider = f.core.orchestrator().orch_provider.clone();
    let call = |session: &str| {
        MODEL_CALLER.scope(
            ModelCaller {
                agent_id: "fixture".into(),
                session_id: session.into(),
            },
            provider.chat(ChatRequest::new("test", vec![ChatMessage::user(session)])),
        )
    };
    let (a, b) = tokio::join!(call("session-a"), call("session-b"));
    a.unwrap();
    b.unwrap();
    call("session-a").await.unwrap();
    let seen = f.seen.lock().unwrap();
    let a: Vec<_> = seen
        .iter()
        .filter(|call| call.body["input"][0]["content"][0]["text"] == "session-a")
        .map(|call| &call.headers["session_id"])
        .collect();
    let b = &seen
        .iter()
        .find(|call| call.body["input"][0]["content"][0]["text"] == "session-b")
        .unwrap()
        .headers["session_id"];
    assert_eq!(a.len(), 2);
    assert_eq!(a[0], a[1]);
    assert_ne!(a[0], b);
    assert!(uuid::Uuid::parse_str(a[0].to_str().unwrap()).is_ok());
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
            assert_eq!(call.headers["user-agent"], "opencode/2.3.0");
            assert_eq!(call.headers["x-client-name"], "fixture");
            assert_eq!(call.headers["authorization"], "Bearer fixture-api-key");
        }
    }
}

#[tokio::test]
async fn native_codex_uses_responses_for_probes_chat_stream_and_tool_roundtrips() {
    let f = Fixture::new().await;
    f.save("openai", json!({"preset":"codex","version":"0.169.0"}))
        .await;
    let config_before = std::fs::read(&f.core.config().config_path).unwrap();
    let preview = f
        .ok(Method::POST, "/api/llm/identity", json!({"id":"fixture"}))
        .await;
    assert_eq!(preview["effectiveFormat"], "responses");
    assert_eq!(preview["path"], "/responses");
    assert!(
        f.seen.lock().unwrap().is_empty(),
        "preview must not contact upstream"
    );
    let probe = f
        .ok(Method::POST, "/api/llm/test", json!({"id":"fixture"}))
        .await;
    assert_eq!(probe["ok"], true, "{probe}");
    assert!(!probe.to_string().contains("fixture-api-key"));
    let provider = f.core.orchestrator().orch_provider.clone();
    let response = provider.chat(chat()).await.unwrap();
    assert_eq!(response.content, "你好");
    assert_eq!(
        (response.prompt_tokens, response.completion_tokens),
        (11, 7)
    );
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    provider.stream(chat(), tx).await.unwrap();
    let mut text = String::new();
    let mut reasoning = String::new();
    while let Ok(delta) = rx.try_recv() {
        match delta {
            StreamDelta::Text(s) => text.push_str(&s),
            StreamDelta::Thinking(s) => reasoning.push_str(&s),
        }
    }
    assert_eq!(text, "你好");
    assert_eq!(reasoning, "检查");
    let mut request = chat();
    request
        .messages
        .insert(0, ChatMessage::system("Fixture instructions unchanged"));
    request.messages[1]
        .images
        .push("data:image/png;base64,AA==".into());
    request.tools.push(ToolSpec {name:"read".into(),description:"Read fixture".into(),parameters:json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]})});
    let tool_response = provider.chat(request.clone()).await.unwrap();
    assert_eq!(tool_response.tool_calls[0].id, "call_1");
    assert_eq!(tool_response.tool_calls[0].arguments["path"], "fixture.txt");
    let mut assistant = ChatMessage::assistant("");
    assistant.tool_calls = tool_response.tool_calls;
    request.messages.push(assistant);
    request
        .messages
        .push(ChatMessage::tool_result("call_1", "fixture result"));
    assert_eq!(provider.chat(request).await.unwrap().content, "你好");
    let seen = f.seen.lock().unwrap();
    for call in seen.iter() {
        assert_eq!(call.path, "/responses");
        assert!(call.headers["user-agent"]
            .to_str()
            .unwrap()
            .starts_with("codex_cli_rs/0.169.0 ("));
        assert_eq!(call.headers["originator"], "codex_cli_rs");
        assert!(uuid::Uuid::parse_str(call.headers["session_id"].to_str().unwrap()).is_ok());
        assert_eq!(call.headers["authorization"], "Bearer fixture-api-key");
        assert_eq!(call.body["stream"], true);
        assert_eq!(call.body["store"], false);
        assert!(call.body.get("messages").is_none());
    }
    let tool_body = &seen[3].body;
    assert_eq!(tool_body["instructions"], "Fixture instructions unchanged");
    assert_eq!(tool_body["tools"][0]["name"], "read");
    assert_eq!(tool_body["input"][0]["content"][1]["type"], "input_image");
    assert_eq!(
        seen.last().unwrap().body["input"][2],
        json!({"type":"function_call_output","call_id":"call_1","output":"fixture result"})
    );
    assert_eq!(
        std::fs::read(&f.core.config().config_path).unwrap(),
        config_before
    );
}

#[tokio::test]
async fn native_claude_matches_recognition_signals_and_session_overrides() {
    let f = Fixture::new().await;
    let session = uuid::Uuid::new_v4().to_string();
    f.save("openai",json!({"preset":"claude-code","version":"2.1.326","headers":{"X-Claude-Code-Session-Id":session}})).await;
    assert_eq!(
        f.ok(Method::POST, "/api/llm/test", json!({"id":"fixture"}))
            .await["ok"],
        true
    );
    f.core
        .orchestrator()
        .orch_provider
        .chat(chat())
        .await
        .unwrap();
    for call in f.seen.lock().unwrap().iter() {
        assert_eq!(call.path, "/v1/messages");
        assert_eq!(
            call.headers["user-agent"],
            "claude-cli/2.1.326 (external, cli)"
        );
        assert_eq!(call.headers["x-app"], "cli");
        assert_eq!(call.headers["anthropic-beta"], "claude-code-20250219");
        assert_eq!(call.headers["x-api-key"], "fixture-api-key");
        assert_eq!(call.headers["x-stainless-lang"], "js");
        let user: Value =
            serde_json::from_str(call.body["metadata"]["user_id"].as_str().unwrap()).unwrap();
        assert_eq!(user["session_id"], session);
        assert_eq!(user["device_id"].as_str().unwrap().len(), 64);
        assert_eq!(user["account_uuid"], "");
    }
    let preview = f
        .ok(
            Method::POST,
            "/api/llm/identity",
            json!({"id":"fixture","baseUrl":format!("{}/v1",f.base)}),
        )
        .await;
    assert_eq!(preview["path"], "/v1/messages");
    assert_eq!(preview["metadata"], true);
}

#[tokio::test]
async fn drafts_can_change_native_protocol_without_changing_saved_identity() {
    let f = Fixture::new().await;
    f.save("openai", json!({"preset":"codex"})).await;
    let before = std::fs::read(&f.core.config().config_path).unwrap();
    let result = f
        .ok(
            Method::POST,
            "/api/llm/test",
            json!({"id":"fixture","clientIdentity":{"preset":"claude-code"}}),
        )
        .await;
    assert_eq!(result["ok"], true);
    assert_eq!(result["identity"]["effectiveFormat"], "anthropic");
    assert_eq!(f.seen.lock().unwrap().last().unwrap().path, "/v1/messages");
    let result = f
        .ok(
            Method::POST,
            "/api/llm/test",
            json!({"id":"fixture","clientIdentity":{"preset":"codex","requestMode":"headers"}}),
        )
        .await;
    assert_eq!(result["ok"], true);
    assert_eq!(result["identity"]["effectiveFormat"], "openai");
    assert_eq!(
        f.seen.lock().unwrap().last().unwrap().path,
        "/chat/completions"
    );
    assert_eq!(std::fs::read(&f.core.config().config_path).unwrap(), before);
}

#[tokio::test]
async fn responses_failures_never_become_successful_probe_results() {
    let f = Fixture::new().await;
    f.save("openai", json!({"preset":"codex"})).await;
    for model in ["failed-event", "truncated", "http402", "http429"] {
        let result = f
            .ok(
                Method::POST,
                "/api/llm/test",
                json!({"id":"fixture","model":model}),
            )
            .await;
        assert_eq!(result["ok"], false, "{model}: {result}");
        assert!(result["error"].is_string());
        assert!(!result.to_string().contains("private-fixture-content"));
        let provider = f.core.orchestrator().orch_provider.clone();
        let request = ChatRequest::new(model, vec![ChatMessage::user("ping")]);
        assert!(provider.chat(request).await.is_err(), "{model}");
    }
    let mut request = ChatRequest::new("invalid-args", vec![ChatMessage::user("ping")]);
    request.tools.push(ToolSpec {
        name: "read".into(),
        description: "Read fixture".into(),
        parameters: json!({"type":"object"}),
    });
    assert!(f
        .core
        .orchestrator()
        .orch_provider
        .chat(request)
        .await
        .unwrap_err()
        .to_string()
        .contains("JSON"));
}

#[tokio::test]
async fn client_rejection_is_distinct_from_quota_and_does_not_rotate_keys() {
    let f = Fixture::new().await;
    f.save("openai", json!({"preset":"codex"})).await;
    f.ok(
        Method::POST,
        "/api/llm/profiles",
        json!({"id":"fixture","apiKeys":["fixture-one","fixture-two"]}),
    )
    .await;
    let probe = f
        .ok(
            Method::POST,
            "/api/llm/test",
            json!({"id":"fixture","model":"client-rejected"}),
        )
        .await;
    assert_eq!(probe["ok"], false);
    assert_eq!(probe["status"], 403);
    assert!(probe["error"]
        .as_str()
        .unwrap()
        .contains("client_not_allowed"));
    let provider = f.core.orchestrator().orch_provider.clone();
    let error = provider
        .chat(ChatRequest::new(
            "client-rejected",
            vec![ChatMessage::user("ping")],
        ))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("client_not_allowed"));
    assert!(!error.contains("额度"));
    assert!(!error.contains("private-fixture-content"));
    assert_eq!(
        f.seen.lock().unwrap().len(),
        2,
        "one probe and one inference; no key rotation for client rejection"
    );
}

#[tokio::test]
async fn custom_openclaw_attribution_headers_reach_probes_and_live_calls() {
    let f = Fixture::new().await;
    f.save("openai",json!({"preset":"custom","userAgent":"openclaw/2026.11.4","headers":{"originator":"openclaw","version":"2026.11.4","HTTP-Referer":"https://openclaw.ai","X-OpenRouter-Title":"OpenClaw","X-OpenRouter-Categories":"personal-agent,cli-agent"}})).await;
    assert_eq!(
        f.ok(Method::POST, "/api/llm/test", json!({"id":"fixture"}))
            .await["ok"],
        true
    );
    f.core
        .orchestrator()
        .orch_provider
        .chat(chat())
        .await
        .unwrap();
    for call in f.seen.lock().unwrap().iter() {
        assert_eq!(call.headers["user-agent"], "openclaw/2026.11.4");
        assert_eq!(call.headers["version"], "2026.11.4");
        assert_eq!(call.headers["originator"], "openclaw");
        assert_eq!(call.headers["x-openrouter-title"], "OpenClaw");
        assert_eq!(
            call.headers["x-openrouter-categories"],
            "personal-agent,cli-agent"
        );
    }
}
