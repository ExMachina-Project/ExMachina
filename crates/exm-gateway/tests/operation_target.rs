//! 操作默认对象的真实路由契约：显式绑定不切全局，鉴权/项目边界仍生效。
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use exm_core::{config::ExmConfig, Core};
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc};
use tower::ServiceExt;

struct Fixture {
    core: Arc<Core>,
    app: axum::Router,
    dir: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("exm-operation-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("project")).unwrap();
        let mut cfg = ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        cfg.security.auth_key = "fixture-key".into();
        let core = Arc::new(Core::with_config(cfg).unwrap());
        for id in ["target-a", "target-b"] {
            core.registry()
                .create_group(Some(id.into()), id, "")
                .unwrap();
        }
        core.registry().upsert_single(serde_json::from_value(json!({
            "identifier": "solo-fixture", "name": "Solo", "domain": "test", "tier": "unit", "description": "fixture"
        })).unwrap(), None).unwrap();
        let app = exm_gateway::build_router(core.clone());
        Self { core, app, dir }
    }
    async fn request(
        &self,
        method: Method,
        uri: &str,
        body: Value,
        auth: bool,
        workspace: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json");
        if auth {
            builder = builder.header("x-auth-key", "fixture-key");
        }
        if let Some(workspace) = workspace {
            builder = builder.header("x-exm-workspace", workspace);
        }
        let response = self
            .app
            .clone()
            .oneshot(builder.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 2_000_000)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test]
async fn explicit_target_create_query_and_archive_do_not_mutate_global_target() {
    let f = Fixture::new();
    let active = f.core.registry().active_scope();
    let (status, a) = f
        .request(
            Method::POST,
            "/api/sessions",
            json!({"title":"A", "target":"target-a"}),
            true,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{a}");
    assert_eq!(a["groupId"], "target-a");
    let (status, solo) = f
        .request(
            Method::POST,
            "/api/sessions",
            json!({"title":"Solo", "target":"single:solo-fixture"}),
            true,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{solo}");
    let (_, list) = f
        .request(
            Method::GET,
            "/api/sessions?target=single:solo-fixture",
            Value::Null,
            true,
            None,
        )
        .await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], solo["id"]);
    let (_, b) = f
        .request(
            Method::POST,
            "/api/sessions",
            json!({"title":"B", "target":"target-b"}),
            true,
            None,
        )
        .await;
    let (_, list) = f
        .request(
            Method::GET,
            "/api/sessions?target=target-a",
            Value::Null,
            true,
            None,
        )
        .await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], a["id"]);
    f.core
        .set_session_archived(a["id"].as_str().unwrap(), true)
        .unwrap();
    let (_, list) = f
        .request(
            Method::GET,
            "/api/sessions?target=target-a&archived=true",
            Value::Null,
            true,
            None,
        )
        .await;
    assert_eq!(list[0]["id"], a["id"]);
    let (_, list) = f
        .request(
            Method::GET,
            "/api/sessions?target=target-b",
            Value::Null,
            true,
            None,
        )
        .await;
    assert_eq!(list[0]["id"], b["id"]);
    assert_eq!(f.core.registry().active_scope(), active);
    let (_, health) = f
        .request(Method::GET, "/api/health", Value::Null, true, None)
        .await;
    assert!(health["features"]
        .as_array()
        .unwrap()
        .contains(&json!("session-target-v1")));
}

#[tokio::test]
async fn explicit_target_requests_reject_missing_objects_and_unauthenticated_clients() {
    let f = Fixture::new();
    for target in ["", "missing", "single:missing"] {
        let (status, _) = f
            .request(
                Method::POST,
                "/api/sessions",
                json!({"target":target}),
                true,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = f
            .request(
                Method::GET,
                &format!("/api/sessions?target={target}"),
                Value::Null,
                true,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    for method in [Method::GET, Method::POST] {
        let (status, _) = f
            .request(
                method,
                "/api/sessions?target=target-a",
                json!({"target":"target-a"}),
                false,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    assert!(f.core.list_sessions_admin().unwrap().is_empty());
}

#[tokio::test]
async fn explicit_target_does_not_bypass_project_binding_or_openapi_contract() {
    let f = Fixture::new();
    let project = f.dir.join("project").canonicalize().unwrap();
    let root = project.to_str().unwrap();
    let (status, session) = f
        .request(
            Method::POST,
            "/api/sessions",
            json!({"target":"target-a", "workspace":root}),
            true,
            Some(root),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{session}");
    let (_, global) = f
        .request(
            Method::GET,
            "/api/sessions?target=target-a",
            Value::Null,
            true,
            None,
        )
        .await;
    assert!(global.as_array().unwrap().is_empty());
    let uri = format!("/api/sessions?target=target-a&workspace={root}");
    let (_, list) = f
        .request(Method::GET, &uri, Value::Null, true, Some(root))
        .await;
    assert_eq!(list[0]["id"], session["id"]);
    let (status, _) = f
        .request(
            Method::POST,
            "/api/sessions",
            json!({"target":"target-b"}),
            true,
            Some(root),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, doc) = f
        .request(Method::GET, "/api/openapi.json", Value::Null, false, None)
        .await;
    assert!(doc["paths"]["/api/sessions"]["get"]["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["name"] == "target"));
    assert_eq!(
        doc["paths"]["/api/sessions"]["post"]["requestBody"]["content"]["application/json"]
            ["schema"]["properties"]["target"]["type"],
        "string"
    );
}
