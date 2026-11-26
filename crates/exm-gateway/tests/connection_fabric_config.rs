//! 新连接实验配置不覆盖旧来源，非法限额在保存前拒绝。
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
        let dir = std::env::temp_dir().join(format!(
            "exm-connection-config-http-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = ExmConfig::load(&dir);
        cfg.data_dir = dir.join("data");
        cfg.agents_dir = dir.join("entities");
        cfg.use_mock = true;
        cfg.security.auth_key = "fixture-key".into();
        cfg.experiments.connection_fabric.max_calls_per_root = 7;
        let core = Arc::new(Core::with_config(cfg).unwrap());
        let app = exm_gateway::build_router(core.clone());
        Self { core, app, dir }
    }
    async fn request(&self, method: Method, body: Value) -> (StatusCode, Value) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/api/config")
                    .header("x-auth-key", "fixture-key")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
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
async fn connection_fabric_config_patch_is_independent_and_preserves_limits() {
    let f = Fixture::new();
    let (_, body) = f.request(Method::GET, Value::Null).await;
    assert_eq!(body["experiments"]["connectionFabric"]["enabled"], false);
    let (status, body) = f
        .request(
            Method::PUT,
            json!({"experiments":{"connectionFabric":{"enabled":true}}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(f.core.config().experiments.connection_fabric.enabled);
    assert_eq!(
        f.core
            .config()
            .experiments
            .connection_fabric
            .max_calls_per_root,
        7
    );
    assert!(!f.core.config().experiments.device_connectivity.enabled);
    assert!(!f.core.config().experiments.auto_agent);
    let (_, body) = f.request(Method::GET, Value::Null).await;
    assert_eq!(
        body["experiments"]["connectionFabric"]["maxCallsPerRoot"],
        7
    );
    let (status, _) = f.request(Method::PUT, json!({"maxConcurrency":2})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        f.core
            .config()
            .experiments
            .connection_fabric
            .max_calls_per_root,
        7
    );
}

#[tokio::test]
async fn connection_fabric_invalid_config_is_rejected_without_saving() {
    let f = Fixture::new();
    f.core.config().save().unwrap();
    let path = f.core.config().config_path.clone();
    let before = std::fs::read(&path).unwrap();
    for patch in [
        Value::Null,
        json!({"maxCallsPerRoot":0}),
        json!({"maxEventBytes":65537}),
        json!({"enabled":null}),
        json!({"unexpected":true}),
        json!("invalid"),
    ] {
        let (status, _) = f
            .request(
                Method::PUT,
                json!({"experiments":{"connectionFabric":patch}}),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!f.core.config().experiments.connection_fabric.enabled);
    }
}
