//! gateway/mcp_admin —— 自 lib.rs 按资源域拆出（纯搬移，零行为变更）

use super::*;

// ---------------------------------------------------------------- MCP（第三方工具生态）

pub(crate) async fn mcp_servers(State(st): State<AppState>) -> impl IntoResponse {
    Json(json!({ "servers": st.core.mcp().servers_brief() }))
}

pub(crate) async fn mcp_refresh(State(st): State<AppState>) -> impl IntoResponse {
    let results = st.core.mcp().refresh_all().await;
    let out: Vec<serde_json::Value> = results
        .into_iter()
        .map(|(id, r)| match r {
            Ok(n) => json!({ "id": id, "ok": true, "tools": n }),
            Err(e) => json!({ "id": id, "ok": false, "error": e.to_string() }),
        })
        .collect();
    Json(json!({ "refreshed": out }))
}

// ---------------------------------------------------------------- MCP 服务端（HTTP 挂载，组 10.3）

/// MCP HTTP 挂载路由（10.3）：配置的 http_path（默认 /mcp）；未启用 = 空路由（挂载点不可用，spec：默认关闭）。
/// 位于鉴权中间件之前挂载（build_router 内 merge），复用全局鉴权。
pub(crate) fn mcp_mount(core: Arc<Core>) -> Router<AppState> {
    if !core.config().mcp_serve.enabled {
        return Router::new();
    }
    let path = core
        .config()
        .mcp_serve
        .http_path
        .trim_matches('/')
        .to_string();
    if path.is_empty() {
        return Router::new();
    }
    Router::new().route(&format!("/{path}"), axum::routing::post(mcp_http_endpoint))
}

/// MCP HTTP 端点：JSON-RPC 单请求 → 单响应（与 stdio 共用 `McpServer` 处理面）。
/// 鉴权由全局中间件承担（启用 authKey 时须 X-Auth-Key）；此处叠加 API 维度限流（外部入口口径）。
/// pub 供挂载与接口测试复用（同一处理面，不出现第二套语义）。
pub async fn mcp_http_endpoint(
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    State(st): State<AppState>,
    body: String,
) -> impl IntoResponse {
    // 未启用：挂载点不存在语义（404），不泄露端点存在性
    if !st.core.config().mcp_serve.enabled {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "未找到" }))).into_response();
    }
    // 用量治理：与 /api 同维度限流（外部入口）
    let limits = st.core.config().limits.clone();
    if limits.enabled && limits.max_requests > 0 {
        let now = exm_core::types::now_ms();
        if !crate::limits::RateLimiter::check(
            "api",
            limits.window_secs * 1000,
            limits.max_requests,
            now,
        ) {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({ "error": "请求过于频繁", "retrySecs": limits.window_secs })),
            )
                .into_response();
        }
    }
    let req: Value = match serde_json::from_str::<Value>(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("解析失败: {e}") } })),
            )
                .into_response()
        }
    };
    let srv = exm_core::mcp_server::McpServer::new(st.core.clone(), format!("http:{addr}"));
    match srv.handle_request(&req).await {
        Some(resp) => Json(resp).into_response(),
        // 通知类请求：无响应体（204）
        None => StatusCode::NO_CONTENT.into_response(),
    }
}
