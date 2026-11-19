//! Real router and model transport against disposable projects and a local upstream.
use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{Method, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use exm_core::{config::ExmConfig, types::ApprovalRequest, Core};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Semaphore;
use tower::ServiceExt;

const ROOMS: &str = "/api/collaborations";

struct Provider {
    requests: Mutex<Vec<Value>>,
    release: Semaphore,
}

async fn upstream(State(state): State<Arc<Provider>>, Json(request): Json<Value>) -> Response {
    state.requests.lock().unwrap().push(request.clone());
    let messages = request["messages"].as_array().unwrap();
    let system = messages.first().unwrap()["content"]
        .as_str()
        .unwrap_or_default();
    let input = messages.last().unwrap()["content"]
        .as_str()
        .unwrap_or_default();
    if input.contains("hold-fixture") {
        state.release.acquire().await.unwrap().forget();
    }
    let text = if system.contains("OrchestratorPlan") {
        json!({ "routeLevel": "L0", "goal": "fixture", "boundary": {"inScope":["fixture"], "forbidden":[]},
            "acceptance": ["answer"], "nodes": [], "finalAnswer": [{"tag": "报告", "text": "fixture group answer"}] }).to_string()
    } else if input.contains("marker-from-alpha") {
        "verified alpha file".into()
    } else if input.contains("marker-from-beta") {
        "verified beta file".into()
    } else if input.contains("read-scope-fixture") {
        json!({"tool": "read", "args": {"path": "scope.txt"}}).to_string()
    } else {
        "fixture single answer".into()
    };
    if request["stream"] == true {
        (
            [("content-type", "text/event-stream")],
            format!(
                "data: {}\n\ndata: [DONE]\n\n",
                json!({"choices":[{"delta":{"content":text},"finish_reason":"stop"}]})
            ),
        )
            .into_response()
    } else {
        Json(json!({"choices":[{"message":{"content":text}}]})).into_response()
    }
}

struct Fixture {
    core: Arc<Core>,
    app: Router,
    provider: Arc<Provider>,
    server: tokio::task::JoinHandle<()>,
    root: PathBuf,
    alpha: String,
    beta: String,
}

impl Fixture {
    async fn new() -> Self {
        let provider = Arc::new(Provider {
            requests: Mutex::new(vec![]),
            release: Semaphore::new(0),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let service = Router::new()
            .route("/chat/completions", post(upstream))
            .with_state(provider.clone());
        let server = tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
        let root = std::env::temp_dir().join(format!("exm-collab-{}", uuid::Uuid::new_v4()));
        let project = |name: &str| {
            let path = root.join(name);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("scope.txt"), format!("marker-from-{name}")).unwrap();
            path.canonicalize().unwrap().to_str().unwrap().to_string()
        };
        let alpha = project("alpha");
        let beta = project("beta");
        let mut config = ExmConfig::load(&root);
        config.workspace_root = root.clone();
        config.agents_dir = root.join("entities");
        config.data_dir = root.join("data");
        config.use_mock = false;
        config.memory_enabled = false;
        config.security.auth_key = "collaboration-fixture".into();
        config.active_profile = "fixture".into();
        config.llm_profiles = vec![serde_json::from_value(json!({"id":"fixture", "name":"Fixture",
            "baseUrl":endpoint, "apiKey":"unused", "model":"team-model", "models":[{"model":"team-model"},{"model":"solo-model"}] })).unwrap()];
        let core = Arc::new(Core::with_config(config).unwrap());
        core.registry()
            .create_group(Some("team".into()), "Project Team", "fixture")
            .unwrap();
        core.registry()
            .upsert_agent(
                "team",
                serde_json::from_value(json!({"identifier":"chief", "name":"Chief",
            "description":"fixture", "tier":"orchestrator", "domain":"test"}))
                .unwrap(),
                None,
            )
            .unwrap();
        core.registry().set_primary("team", "chief").unwrap();
        core.registry().upsert_single(serde_json::from_value(json!({"identifier":"solo", "name":"Solo",
            "description":"fixture", "tier":"unit", "domain":"test", "modelHint":"fixture/solo-model"})).unwrap(), None).unwrap();
        Self {
            app: exm_gateway::build_router(core.clone()),
            core,
            provider,
            server,
            root,
            alpha,
            beta,
        }
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        workspace: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value) {
        call(&self.app, method, path, workspace, body).await
    }

    fn roster(&self) -> Value {
        json!({"title":"Mixed Room", "members":[{"target":"single:solo","workspace":self.alpha},
            {"target":"team","workspace":self.beta}, {"target":"single:solo","workspace":self.beta}]})
    }

    async fn create(&self) -> Value {
        let (status, body) = self
            .request(Method::POST, ROOMS, Some(&self.alpha), self.roster())
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body
    }

    async fn finished(&self, path: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                let (status, page) = self
                    .request(Method::GET, path, Some(&self.alpha), Value::Null)
                    .await;
                assert_eq!(status, StatusCode::OK, "{page}");
                if page["activeTurnId"].is_null() {
                    return page;
                }
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
        })
        .await
        .expect("room never finished")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

async fn call(
    router: &Router,
    method: Method,
    path: &str,
    workspace: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("x-auth-key", "collaboration-fixture")
        .header("content-type", "application/json");
    if let Some(workspace) = workspace {
        request = request.header("x-exm-workspace", workspace);
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    (
        status,
        serde_json::from_slice(&to_bytes(response.into_body(), 5_000_000).await.unwrap()).unwrap(),
    )
}

fn turn(text: &str, recipients: &[(&Value, &str)]) -> Value {
    json!({"requestId":uuid::Uuid::new_v4().to_string(), "text":text, "recipients":recipients.iter().map(|(session, instruction)|
        json!({"sessionId":session,"instruction":instruction})).collect::<Vec<_>>()})
}

#[tokio::test]
async fn mixed_targets_use_pinned_projects_and_private_history_and_reload() {
    let f = Fixture::new().await;
    let global = f.core.registry().active_scope();
    let room = f.create().await;
    let path = format!("{ROOMS}/{}", room["id"].as_str().unwrap());
    let send = format!("{path}/turns");
    let (solo, team, other) = (
        &room["members"][0]["sessionId"],
        &room["members"][1]["sessionId"],
        &room["members"][2]["sessionId"],
    );
    let input = turn(
        "shared note",
        &[(solo, "read-scope-fixture"), (team, "team-only input")],
    );
    assert_eq!(
        f.request(Method::POST, &send, Some(&f.alpha), input.clone())
            .await
            .0,
        StatusCode::ACCEPTED
    );
    let page = f.finished(&path).await;
    let deliveries = &page["turns"][0]["deliveries"];
    assert_eq!(deliveries[0]["status"], "done", "{deliveries}");
    assert!(
        deliveries[0]["messages"]
            .to_string()
            .contains("verified alpha file"),
        "{deliveries}"
    );
    assert_eq!(deliveries[1]["status"], "done", "{deliveries}");
    assert!(
        deliveries[1]["messages"]
            .to_string()
            .contains("fixture group answer"),
        "{deliveries}"
    );
    assert_eq!(f.core.store.message_count(other.as_str().unwrap()), 0);
    assert_eq!(f.core.registry().active_scope(), global);
    for (model, own, other) in [
        ("solo-model", "read-scope-fixture", "team-only input"),
        ("team-model", "team-only input", "read-scope-fixture"),
    ] {
        let requests = f.provider.requests.lock().unwrap();
        assert!(
            requests
                .iter()
                .any(|r| r["model"] == model && r["messages"].to_string().contains(own)),
            "{model}"
        );
        assert!(
            !requests
                .iter()
                .any(|r| r["model"] == model && r["messages"].to_string().contains(other)),
            "{model}"
        );
    }
    // A matching retry is accepted without a second model call/turn, even with another Core.
    let reloaded = exm_gateway::build_router(Arc::new(
        Core::with_config((*f.core.config()).clone()).unwrap(),
    ));
    let (status, persisted) =
        call(&reloaded, Method::GET, &path, Some(&f.alpha), Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(persisted, page);
    assert_eq!(
        call(&reloaded, Method::POST, &send, Some(&f.alpha), input)
            .await
            .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(f.core.store.message_count(solo.as_str().unwrap()), 2);

    assert_eq!(
        f.request(
            Method::POST,
            &send,
            Some(&f.alpha),
            turn("read-scope-fixture", &[(other, "")])
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    let page = f.finished(&path).await;
    assert!(
        page["turns"][1]["deliveries"][0]["messages"]
            .to_string()
            .contains("verified beta file"),
        "{page}"
    );
    assert_eq!(f.core.store.message_count(solo.as_str().unwrap()), 2);
}

#[tokio::test]
async fn room_scope_and_roster_gate_execution_and_descendant_approval() {
    let f = Fixture::new().await;
    assert_eq!(
        f.request(Method::POST, ROOMS, None, f.roster()).await.0,
        StatusCode::BAD_REQUEST
    );
    for invalid in [
        json!({"target":"missing","workspace":f.alpha}),
        json!({"target":"team","workspace":"."}),
        json!({"target":"single:solo","workspace":f.alpha}),
    ] {
        let mut body = f.roster();
        body["members"][1] = invalid;
        assert_eq!(
            f.request(Method::POST, ROOMS, Some(&f.alpha), body).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        f.request(Method::GET, ROOMS, Some(&f.alpha), Value::Null)
            .await
            .1,
        json!([])
    );
    let room = f.create().await;
    let path = format!("{ROOMS}/{}", room["id"].as_str().unwrap());
    let send = format!("{path}/turns");
    let team = &room["members"][1]["sessionId"];
    let sid = team.as_str().unwrap();
    assert_eq!(
        f.request(Method::GET, ROOMS, Some(&f.beta), Value::Null)
            .await
            .1,
        json!([])
    );
    assert_eq!(
        f.request(Method::GET, &path, Some(&f.beta), Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.request(
            Method::POST,
            &send,
            Some(&f.beta),
            turn("room task", &[(team, "")])
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.request(
            Method::POST,
            &send,
            Some(&f.alpha),
            turn("room task", &[(&json!(uuid::Uuid::new_v4().to_string()), "")])
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        f.request(Method::POST, &send, Some(&f.alpha), turn("", &[(team, "")]))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        f.request(
            Method::POST,
            &send,
            Some(&f.alpha),
            turn("duplicate", &[(team, ""), (team, "x")])
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let session_route = format!("/api/sessions/{sid}");
    assert_eq!(
        f.request(Method::GET, &session_route, Some(&f.alpha), Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.request(
            Method::POST,
            &format!("{session_route}/chat"),
            Some(&f.beta),
            json!({"text":"outside room"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        f.request(Method::DELETE, &session_route, Some(&f.beta), Value::Null)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        f.request(
            Method::POST,
            &send,
            Some(&f.alpha),
            turn("room task", &[(team, "")])
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    f.finished(&path).await;

    let child = f
        .core
        .store
        .create_child_session(sid, "child task")
        .unwrap();
    assert_eq!(child.workspace.as_deref(), Some(f.beta.as_str()));
    let other = f
        .core
        .store
        .create_session_in("ordinary", "team", Some(f.beta.clone()))
        .unwrap();
    for (id, source) in [("child-approval", child.id), ("other-approval", other.id)] {
        let approval: ApprovalRequest =
            serde_json::from_value(json!({"id":id,"sessionId":source,"agentId":"worker",
            "command":"echo review","status":"pending","createdAt":"2026-11-19T00:00:00Z"}))
            .unwrap();
        f.core.store.add_approval(&approval).unwrap();
    }
    let page = f.finished(&path).await;
    assert_eq!(page["approvals"].as_array().unwrap().len(), 1);
    assert_eq!(page["approvals"][0]["memberSessionId"], *team);
    let endpoint = "/api/approvals/child-approval/deny";
    assert_eq!(
        f.request(Method::POST, endpoint, Some(&f.alpha), json!({}))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.request(Method::POST, endpoint, Some(&f.beta), json!({}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(f.finished(&path).await["approvals"], json!([]));
}

#[tokio::test]
async fn different_projects_run_in_parallel_and_stop_is_bound_to_exact_turn_and_member() {
    let f = Fixture::new().await;
    let room = f.create().await;
    let path = format!("{ROOMS}/{}", room["id"].as_str().unwrap());
    let send = format!("{path}/turns");
    let solo = &room["members"][0]["sessionId"];
    let team = &room["members"][1]["sessionId"];
    let input = turn("hold-fixture", &[(solo, ""), (team, "")]);
    assert_eq!(
        f.request(Method::POST, &send, Some(&f.alpha), input.clone())
            .await
            .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        f.request(Method::POST, &send, Some(&f.alpha), input.clone())
            .await
            .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        f.request(
            Method::POST,
            &send,
            Some(&f.alpha),
            turn("already busy", &[(solo, "")])
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    // Both calls must reach the deliberately blocked upstream before either reply is sent.
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if f.provider.requests.lock().unwrap().len() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    })
    .await
    .expect("room appears to be running projects serially");
    let old = format!("{send}/{}/stop", uuid::Uuid::new_v4());
    assert_eq!(
        f.request(Method::POST, &old, Some(&f.alpha), json!({}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let stop = format!("{send}/{}/stop", input["requestId"].as_str().unwrap());
    assert_eq!(
        f.request(
            Method::POST,
            &stop,
            Some(&f.alpha),
            json!({"sessionId":solo})
        )
        .await,
        (StatusCode::OK, json!({"stopped":1}))
    );
    f.provider.release.add_permits(8);
    let page = f.finished(&path).await;
    assert_eq!(
        page["turns"][0]["deliveries"][0]["status"], "stopped",
        "{page}"
    );
    assert_eq!(
        page["turns"][0]["deliveries"][1]["status"], "done",
        "{page}"
    );
}
