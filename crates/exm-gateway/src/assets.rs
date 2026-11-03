//! gateway/assets —— 自 lib.rs 按资源域拆出（纯搬移，零行为变更）

use super::*;

// ---------------------------------------------------------------- 健康与配置

/// 工作区资产读取（GET /api/asset?path=）：截图 / 工具输出落盘的内联预览。
/// 安全边界：拒绝绝对路径与 `..`；规范化后必须落在工作区内；扩展名白名单；8MB 上限。
pub(crate) async fn get_asset(State(st): State<AppState>, Query(q): Query<AssetQuery>) -> impl IntoResponse {
    use axum::response::AppendHeaders;
    // 鉴权：中间件已校验请求头；此处补 <img> 标签场景的查询参数通道（与 /ws 同款）
    let auth = st.core.config().security.auth_key.clone();
    if !auth.is_empty() && q.key.as_deref() != Some(auth.as_str()) {
        return (StatusCode::UNAUTHORIZED, "需要访问密钥").into_response();
    }
    let raw = q.path.trim();
    if raw.is_empty() {
        return (StatusCode::BAD_REQUEST, "path 不能为空").into_response();
    }
    let root = st.core.config().workspace_root.clone();
    let candidate = root.join(raw);
    // 词法 + 规范化双重校验（与工具面 resolve_safe 同口径）
    let ok = std::path::Path::new(raw).is_relative()
        && !raw.split(['/', '\\']).any(|seg| seg == "..")
        && std::fs::canonicalize(&candidate)
            .map(|c| c.starts_with(std::fs::canonicalize(&root).unwrap_or(root.clone())))
            .unwrap_or(false);
    if !ok {
        return (StatusCode::FORBIDDEN, "路径越界（仅允许工作区内的资产）").into_response();
    }
    let ext = candidate
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "txt" | "log" => "text/plain; charset=utf-8",
        "json" => "application/json",
        "md" => "text/markdown; charset=utf-8",
        _ => return (StatusCode::UNSUPPORTED_MEDIA_TYPE, "不支持的资产类型").into_response(),
    };
    match tokio::fs::read(&candidate).await {
        Ok(bytes) => {
            if bytes.len() > 8 * 1024 * 1024 {
                return (StatusCode::PAYLOAD_TOO_LARGE, "资产超过 8MB").into_response();
            }
            (
                AppendHeaders([(axum::http::header::CONTENT_TYPE, mime)]),
                bytes,
            )
                .into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "资产不存在").into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct AssetQuery {
    path: String,
    /// <img> 无法携带请求头：与 /ws 同款查询参数密钥
    #[serde(default)]
    key: Option<String>,
}

pub(crate) async fn health(State(st): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "ok": true,
        "mock": st.core.is_mock(),
        "agents": st.core.registry.count(),
        "provider": if st.core.is_mock() { "mock" } else { "openai-compatible" },
    }))
}
