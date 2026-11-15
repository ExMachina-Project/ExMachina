//! 资产端点鉴权集成测试（im-inbound-attach 破图修复）：
//! `<img src>` / `<a target="_blank">` 无法携带自定义请求头——部署实测
//! 聊天流全部附件图恒 401。修复语义锚定：
//! 1. GET /api/asset* 的查询参数密钥通道（与 /ws 同款，仅此两端点）；
//! 2. 带头认证（无 ?key=）的请求不再被 handler 拒（曾写反成「必须带 ?key=」）；
//! 3. 其余 /api/* 维持「仅头部」口径（查询参数密钥不降级到全站）。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use exm_core::config::ExmConfig;
use tower::ServiceExt as _;

async fn app_with(auth_key: &str) -> (axum::Router, std::path::PathBuf) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("exm-asset-auth-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    let mut cfg = ExmConfig::load(&dir);
    cfg.use_mock = true;
    cfg.memory_enabled = false;
    cfg.security.auth_key = auth_key.into();
    let core = std::sync::Arc::new(exm_core::Core::with_config(cfg).unwrap());
    let router = exm_gateway::build_router(core.clone());
    (router, core.config().workspace_root.clone())
}

fn png_bytes() -> Vec<u8> {
    vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4]
}

/// 查询参数密钥通道：<img> 场景只带 ?key= 不带头——应 200 返回图片
#[tokio::test]
async fn 资产端点_查询参数密钥放行() {
    let (app, root) = app_with("sk-test").await;
    let inbox = root.join(".exmachina/inbox/ch1");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::write(inbox.join("pic.png"), png_bytes()).unwrap();
    let rel = ".exmachina/inbox/ch1/pic.png";

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/asset?path={}&key=sk-test", rel))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "<img> 查询参数密钥应放行");
}

/// 头部认证无 ?key=：不被 handler 二次拒（曾写反成必须带 ?key=）
#[tokio::test]
async fn 资产端点_头部认证无需查询参数() {
    let (app, root) = app_with("sk-test").await;
    let inbox = root.join(".exmachina/inbox/ch1");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::write(inbox.join("pic.png"), png_bytes()).unwrap();
    let rel = ".exmachina/inbox/ch1/pic.png";

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/asset?path={rel}"))
        .header("x-auth-key", "sk-test")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "带头认证（无 ?key=）应放行");
}

/// 下载端点同通道：<a target="_blank"> 导航同样无法带头
#[tokio::test]
async fn 下载端点_查询参数密钥放行() {
    let (app, root) = app_with("sk-test").await;
    let inbox = root.join(".exmachina/inbox/ch1");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::write(inbox.join("report.bin"), b"file-bytes").unwrap();
    let rel = ".exmachina/inbox/ch1/report.bin";

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/asset/download?path={}&key=sk-test", rel))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "下载链接查询参数密钥应放行");
}

/// 错误密钥仍 401：查询参数通道不是免鉴权
#[tokio::test]
async fn 资产端点_错误密钥拒绝() {
    let (app, _root) = app_with("sk-test").await;
    for (path, note) in [
        ("/api/asset?path=x.png&key=wrong", "inline 错误 key"),
        ("/api/asset/download?path=x.png&key=wrong", "download 错误 key"),
    ] {
        let req = Request::builder()
            .method("GET")
            .uri(path)
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{note}");
    }
}

/// 查询参数密钥不降级到全站：其余 /api/* 只带头部密钥
#[tokio::test]
async fn 查询密钥_仅资产端点开放() {
    let (app, _root) = app_with("sk-test").await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/sessions?key=sk-test")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "其余 API 的查询参数密钥不应放行"
    );
}
