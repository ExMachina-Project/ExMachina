//! Real HTTP failures and model names must agree with agent-scoped assignments.
use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{Method, Request, StatusCode},
    routing::post,
    Json, Router,
};
use exm_core::{config::ExmConfig, types::MessageRole, Core};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

#[derive(Default)]
struct Upstream {
    requests: Mutex<Vec<Value>>,
    quota: std::sync::atomic::AtomicU16,
}
async fn upstream(
    State(state): State<Arc<Upstream>>,
    Json(body): Json<Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    state.requests.lock().unwrap().push(body.clone());
    let status = state.quota.load(std::sync::atomic::Ordering::Relaxed);
    if status != 0 {
        return (
            StatusCode::from_u16(status).unwrap(),
            Json(
                json!({"error":{"code":"insufficient_quota","message":"fixture quota exhausted"}}),
            ),
        )
            .into_response();
    }
    if body["stream"] == true {
        return (
            [("content-type", "text/event-stream")],
            format!(
                "data: {}\n\ndata: [DONE]\n\n",
                json!({"choices":[{"delta":{"content":"fixture answer"},"finish_reason":"stop"}]})
            ),
        )
            .into_response();
    }
    Json(json!({"choices":[{"message":{"content":"fixture answer"}}]})).into_response()
}

struct Fixture {
    core: Arc<Core>,
    app: Router,
    state: Arc<Upstream>,
    task: tokio::task::JoinHandle<()>,
    root: std::path::PathBuf,
}
impl Fixture {
    async fn new() -> Self {
        let state = Arc::new(Upstream::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let service = Router::new()
            .route("/chat/completions", post(upstream))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, service).await.unwrap();
        });
        let root = std::env::temp_dir().join(format!("exm-model-routing-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut cfg = ExmConfig::load(&root);
        cfg.workspace_root = root.clone();
        cfg.agents_dir = root.join("entities");
        cfg.data_dir = root.join("data");
        cfg.use_mock = false;
        cfg.memory_enabled = false;
        cfg.llm.base_url = endpoint.clone();
        cfg.llm.api_key = "fixture-key".into();
        cfg.llm.api_keys.clear();
        cfg.llm.model = "provider-default".into();
        cfg.active_profile = "fixture".into();
        cfg.llm_profiles=vec![serde_json::from_value(json!({"id":"fixture","name":"Fixture","baseUrl":endpoint,"apiKey":"fixture-key","model":"provider-default","models":[{"model":"provider-default"},{"model":"small"}]})).unwrap()];
        let core = Arc::new(Core::with_config(cfg).unwrap());
        core.registry()
            .create_group(Some("fixture-group".into()), "Fixture Group", "test")
            .unwrap();
        let member=serde_json::from_value(json!({"identifier":"commander","name":"Commander","description":"fixture","tier":"orchestrator","domain":"test"})).unwrap();
        core.registry()
            .upsert_agent("fixture-group", member, Some("answer briefly".into()))
            .unwrap();
        core.registry()
            .set_primary("fixture-group", "commander")
            .unwrap();
        core.registry().set_active_group("fixture-group").unwrap();
        let single=serde_json::from_value(json!({"identifier":"unit","name":"Unit","description":"fixture","tier":"unit","domain":"test","modelHint":"fixture/small"})).unwrap();
        core.registry()
            .upsert_single(single, Some("answer briefly".into()))
            .unwrap();
        core.registry().set_active_single(Some("unit")).unwrap();
        Self {
            app: exm_gateway::build_router(core.clone()),
            core,
            state,
            task,
            root,
        }
    }
    async fn request(&self, method: Method, path: &str, body: Value) -> (StatusCode, Value) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let data = to_bytes(response.into_body(), 2_000_000).await.unwrap();
        (status, serde_json::from_slice(&data).unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn quota_exhaustion_never_creates_a_successful_greeting() {
    let f = Fixture::new().await;
    let mut config = (*f.core.config()).clone();
    config.llm_profiles[0].fallback = Some("backup".into());
    let mut backup = config.llm_profiles[0].clone();
    backup.id = "backup".into();
    backup.model = "backup-model".into();
    backup.fallback = None;
    config.llm_profiles.push(backup);
    f.core.apply_config(config).unwrap();
    for status in [402, 429] {
        f.state
            .quota
            .store(status, std::sync::atomic::Ordering::Relaxed);
        for group in [false, true] {
            f.core
                .registry()
                .set_active_single(if group { None } else { Some("unit") })
                .unwrap();
            for text in ["在吗", "你好！", "晚安", "谢谢", "OK", "test", "@bot 在吗"] {
                let id = f.core.create_session("quota fixture").unwrap().id;
                exm_core::orchestrator::mark_im_session(&id);
                let before = f.state.requests.lock().unwrap().len();
                let error = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    f.core.chat(&id, text),
                )
                .await
                .unwrap()
                .unwrap_err();
                assert!(error.to_string().contains(&status.to_string()), "{error}");
                let attempted: Vec<_> = f.state.requests.lock().unwrap()[before..]
                    .iter()
                    .map(|request| request["model"].as_str().unwrap().to_string())
                    .collect();
                assert_eq!(
                    attempted,
                    vec![
                        if group { "provider-default" } else { "small" },
                        "backup-model"
                    ],
                    "The greeting must reach both allowed upstream candidates exactly once"
                );
                let messages = f.core.store.list_messages(&id, 0).unwrap();
                assert!(!messages
                    .iter()
                    .any(|m| matches!(m.role, MessageRole::Orchestrator | MessageRole::Unit)));
                assert!(messages.iter().any(|m| m.role == MessageRole::System
                    && m.statements
                        .iter()
                        .any(|s| s.text.contains(&format!("HTTP {status}")))));
                assert_eq!(
                    f.core
                        .store
                        .list_turn_snapshots(&id)
                        .unwrap()
                        .last()
                        .unwrap()
                        .status,
                    "failed"
                );
                assert!(!exm_core::round_trace::is_running(&id));
                exm_core::orchestrator::unmark_im_session(&id);
            }
        }
    }
}

#[tokio::test]
async fn explicit_agent_model_reaches_upstream_and_new_inheritance_replaces_legacy_hint() {
    let f = Fixture::new().await;
    let id = f.core.create_session("explicit model").unwrap().id;
    f.core.chat(&id, "hello").await.unwrap();
    assert_eq!(
        f.state.requests.lock().unwrap().last().unwrap()["model"],
        "small"
    );
    let (status, _) = f
        .request(
            Method::PUT,
            "/api/singles/unit/model-routing",
            json!({"default":{"mode":"inherit"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let id = f.core.create_session("inherited model").unwrap().id;
    f.core.chat(&id, "hello").await.unwrap();
    assert_eq!(
        f.state.requests.lock().unwrap().last().unwrap()["model"],
        "provider-default"
    );
}

#[tokio::test]
async fn owner_scoped_assignments_validate_and_persist_without_changing_active_target() {
    let f = Fixture::new().await;
    let group = f.core.active_group();
    let member = f
        .core
        .registry()
        .agents_in_group(&group)
        .into_iter()
        .next()
        .unwrap()
        .identifier;
    let path = format!("/api/groups/{group}/agents/{member}/model-routing");
    let routing = json!({"default":{"mode":"fixed","model":"fixture/small"},"purposes":{"speech":{"mode":"disabled"}},"instructions":"Prefer economy models for routine tasks."});
    assert_eq!(
        f.request(Method::PUT, &path, routing.clone()).await.0,
        StatusCode::OK
    );
    assert_eq!(f.core.registry().active_single().as_deref(), Some("unit"));
    assert_eq!(
        serde_json::to_value(
            f.core
                .registry()
                .agents_in_group(&group)
                .iter()
                .find(|a| a.identifier == member)
                .unwrap()
                .model_routing
                .as_ref()
                .unwrap()
        )
        .unwrap(),
        json!({"default":{"mode":"fixed","model":"fixture/small","candidates":[]},"purposes":{"speech":{"mode":"disabled","model":"","candidates":[]}},"instructions":"Prefer economy models for routine tasks."})
    );
    f.core.registry().reload().unwrap();
    assert!(f
        .core
        .registry()
        .agents_in_group(&group)
        .iter()
        .find(|a| a.identifier == member)
        .unwrap()
        .model_routing
        .is_some());
    assert_eq!(
        f.request(
            Method::PUT,
            &path,
            json!({"default":{"mode":"fixed","model":"missing/model"}})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        f.request(
            Method::PUT,
            &path,
            json!({"purposes":{"embedding":{"mode":"auto"}}})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        f.request(Method::PUT, &path, json!({"default":{"mode":"auto"}}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn duplicate_member_ids_use_the_session_owner_during_concurrent_rounds() {
    let f = Fixture::new().await;
    let registry = f.core.registry();
    registry
        .create_group(Some("other-group".into()), "Other", "fixture")
        .unwrap();
    let mut member = registry.agents_in_group("fixture-group").remove(0);
    member.model_hint = Some("fixture/provider-default".into());
    registry
        .upsert_agent("other-group", member, Some("answer briefly".into()))
        .unwrap();
    registry.set_primary("other-group", "commander").unwrap();
    registry
        .set_agent_model_routing(
            Some("fixture-group"),
            "commander",
            serde_json::from_value(json!({"default":{"mode":"fixed","model":"fixture/small"}}))
                .unwrap(),
        )
        .unwrap();
    registry.set_active_single(None).unwrap();
    let first = f.core.create_session("first group").unwrap().id;
    registry.set_active_group("other-group").unwrap();
    let second = f.core.create_session("second group").unwrap().id;
    registry.set_active_single(Some("unit")).unwrap();
    let mut events = f.core.subscribe();
    let (a, b) = tokio::join!(
        f.core.chat_with_agent(&first, "commander", "fixture one"),
        f.core.chat_with_agent(&second, "commander", "fixture two")
    );
    a.unwrap();
    b.unwrap();
    let mut used = std::collections::HashMap::new();
    while let Ok(event) = events.try_recv() {
        if event.kind == "model.used" {
            used.insert(event.session_id, event.payload["model"].clone());
        }
    }
    assert_eq!(used[&first], "small");
    assert_eq!(used[&second], "provider-default");
    assert_eq!(registry.active_single().as_deref(), Some("unit"));
    assert_eq!(registry.active_group(), "other-group");
}

#[tokio::test]
async fn spawned_execution_inherits_the_owner_model_and_keeps_its_tool_identity() {
    let f = Fixture::new().await;
    let session = f.core.create_session("spawn fixture").unwrap().id;
    let result = f.core.orchestrator().tools.execute("unit", &session,
        &[exm_core::types::ToolName::Spawn, exm_core::types::ToolName::Read], exm_core::types::ToolName::Spawn,
        &json!({"task":"Read fixture material and summarize", "tools":["read","spawn","agent_manage"]})).await;
    assert!(result.ok, "{:?}", result.error);
    let requests = f.state.requests.lock().unwrap();
    let request = requests.last().unwrap();
    assert_eq!(request["model"], "small");
    let tools: Vec<_> = request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(tools, vec!["read"]);
}
