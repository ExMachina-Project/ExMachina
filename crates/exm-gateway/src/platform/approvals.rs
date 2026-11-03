//! gateway/platform/approvals —— 自 platform.rs 按资源域拆出（纯搬移，零行为变更）

// ---------------------------------------------------------------- 执行审批


use super::*;
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalsQuery {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

pub async fn list_approvals(
    State(st): State<AppState>,
    Query(q): Query<ApprovalsQuery>,
) -> impl IntoResponse {
    match st
        .core
        .approval_list(q.status.as_deref(), q.limit.unwrap_or(50))
    {
        Ok(items) => Json(serde_json::to_value(items).unwrap_or(Value::Null)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn approve_request(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match st.core.approval_decide(&id, true).await {
        Ok(r) => Json(serde_json::to_value(r).unwrap_or(Value::Null)).into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn deny_request(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    match st.core.approval_decide(&id, false).await {
        Ok(r) => Json(serde_json::to_value(r).unwrap_or(Value::Null)).into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------- 通道网关（webhook 通道）
