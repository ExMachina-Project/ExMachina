//! gateway/platform/workbench —— 自 platform.rs 按资源域拆出（纯搬移，零行为变更）


use super::*;
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchTreeQuery {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub depth: Option<usize>,
}

/// 工作台文件树（GET /api/workbench/tree）：.gitignore 口径过滤 + 工作区边界校验
pub async fn workbench_tree(
    State(st): State<AppState>,
    Query(q): Query<WorkbenchTreeQuery>,
) -> impl IntoResponse {
    match st
        .core
        .workbench_tree(q.path.as_deref().unwrap_or(""), q.depth.unwrap_or(6))
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchFileQuery {
    pub path: String,
    #[serde(default)]
    pub offset: Option<u64>,
    #[serde(default)]
    pub limit: Option<u64>,
}

/// 工作台文件读取（GET /api/workbench/file）：read 工具同口径分页
pub async fn workbench_file(
    State(st): State<AppState>,
    Query(q): Query<WorkbenchFileQuery>,
) -> impl IntoResponse {
    match st
        .core
        .workbench_file(&q.path, q.offset.unwrap_or(1), q.limit.unwrap_or(400))
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
