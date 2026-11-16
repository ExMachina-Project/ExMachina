//! Recovery contracts shared by the browser UI and desktop shell.
use axum::body::Body;
use axum::http::{Request, StatusCode};
use exm_core::config::ExmConfig;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

struct Fixture {
    core: Arc<exm_core::Core>,
    dir: std::path::PathBuf,
}

impl Fixture {
    fn new(key: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("exm-recovery-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        cfg.security.auth_key = key.into();
        Self {
            core: Arc::new(exm_core::Core::with_config(cfg).unwrap()),
            dir,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test]
async fn session_detail_reports_live_run_state_without_a_task_graph() {
    let f = Fixture::new("fixture-key");
    let s = f.core.create_session("direct chat").unwrap();
    let app = exm_gateway::build_router(f.core.clone());
    for running in [false, true, false] {
        if running {
            exm_core::round_trace::begin(&s.id);
        } else {
            exm_core::round_trace::end(&s.id);
        }
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/sessions/{}", s.id))
                    .header("x-auth-key", "fixture-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1_000_000)
            .await
            .unwrap();
        let detail: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(detail["running"], running);
        assert_eq!(detail["id"], s.id);
    }
}

#[tokio::test]
async fn websocket_text_heartbeat_acknowledges_authenticated_idle_connections() {
    let f = Fixture::new("a&b+中文");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = exm_gateway::build_router(f.core.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let result = async {
        let (mut socket, _) = tokio_tungstenite::connect_async(format!(
            "ws://{addr}/ws?sessionId=idle&key=a%26b%2B%E4%B8%AD%E6%96%87"
        ))
        .await
        .unwrap();
        socket.send(Message::Text("ping".into())).await.unwrap();
        let pong = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while let Some(frame) = socket.next().await {
                if let Message::Text(text) = frame.unwrap() {
                    return text;
                }
            }
            panic!("connection ended before text Pong");
        })
        .await
        .unwrap();
        assert_eq!(pong, "pong");
        socket.close(None).await.unwrap();
    };
    result.await;
    server.abort();
    let _ = server.await;
}
