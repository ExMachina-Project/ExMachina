//! gateway/assets —— 自 lib.rs 按资源域拆出（纯搬移，零行为变更）

use super::*;

// ---------------------------------------------------------------- 健康与配置

/// 工作区资产读取（GET /api/asset?path=）：截图 / 工具输出落盘的内联预览。
/// 安全边界：拒绝绝对路径与 `..`；规范化后必须落在工作区内；扩展名白名单；8MB 上限。
pub(crate) async fn get_asset(State(st): State<AppState>, Query(q): Query<AssetQuery>) -> impl IntoResponse {
    use axum::response::AppendHeaders;
    // 鉴权纵深（中间件已校验请求头或 /api/asset 的查询参数密钥）：
    // <img> 标签只带查询参数不带头，此处保留查询参数通道校验（与 /ws 同款）；
    // 经头部认证的请求（无 ?key=）不再要求查询参数——曾写成「必须带 ?key=」，
    // 把带头认证的合法请求也拒成 401。
    let auth = st.core.config().security.auth_key.clone();
    if !auth.is_empty() {
        if let Some(k) = q.key.as_deref() {
            if k != auth {
                return (StatusCode::UNAUTHORIZED, "需要访问密钥").into_response();
            }
        }
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

/// 工作区文件下载（GET /api/asset/download?path=）：会话产物投递（send-file
/// 能力）的 WebUI 侧下载端点。与 /api/asset（内联预览）的差别：
/// · 扩展名不限（任意类型可发——xlsx/pdf/zip…；MIME 缺省 application/octet-stream）
/// · 上限 200MB（与 filesystem send_file 登记护栏同口径）
/// · Content-Disposition: attachment（浏览器落盘而非内联打开）
/// · 鉴权在全局中间件：请求头或查询参数密钥（<a target="_blank"> 导航
///   同样无法携带请求头，中间件对 GET /api/asset* 开放查询参数通道）
pub(crate) async fn download_asset(
    State(st): State<AppState>,
    Query(q): Query<AssetQuery>,
) -> impl IntoResponse {
    use axum::response::AppendHeaders;
    let raw = q.path.trim();
    if raw.is_empty() {
        return (StatusCode::BAD_REQUEST, "path 不能为空").into_response();
    }
    let root = st.core.config().workspace_root.clone();
    let candidate = root.join(raw);
    // 路径校验与 /api/asset 同口径（相对路径 + 无 .. + 目标必须已存在于工作区内）
    // 语义区分（REST 通用约定）：不存在的文件 → 404；路径越界（绝对路径/..）→ 403
    let rel_ok =
        std::path::Path::new(raw).is_relative() && !raw.split(['/', '\\']).any(|seg| seg == "..");
    if !rel_ok {
        return (StatusCode::FORBIDDEN, "路径越界（仅允许工作区内的文件）").into_response();
    }
    if !candidate.exists() {
        return (StatusCode::NOT_FOUND, "文件不存在").into_response();
    }
    let meta = match tokio::fs::metadata(&candidate).await {
        Ok(m) if m.is_file() => m,
        _ => return (StatusCode::NOT_FOUND, "文件不存在").into_response(),
    };
    if meta.len() > 200 * 1024 * 1024 {
        return (StatusCode::PAYLOAD_TOO_LARGE, "文件超过 200MB").into_response();
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
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "csv" => "text/csv; charset=utf-8",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    };
    let name = candidate
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".into());
    match tokio::fs::read(&candidate).await {
        Ok(bytes) => (
            AppendHeaders([
                (axum::http::header::CONTENT_TYPE, mime),
                (
                    axum::http::header::CONTENT_DISPOSITION,
                    Box::leak(format!("attachment; filename=\"{name}\"").into_boxed_str()),
                ),
            ]),
            bytes,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "文件不存在").into_response(),
    }
}

pub(crate) async fn health(State(st): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "ok": true,
        "mock": st.core.is_mock(),
        "agents": st.core.registry.count(),
        "provider": if st.core.is_mock() { "mock" } else { "openai-compatible" },
    }))
}
