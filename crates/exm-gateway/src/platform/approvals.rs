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
    body: Option<Json<ApproveBody>>,
) -> impl IntoResponse {
    // 档位缺省 once（旧客户端不带 body 的兼容口径）
    let scope = body
        .and_then(|Json(b)| b.scope)
        .unwrap_or_else(|| "once".to_string());
    let decision = match scope.as_str() {
        "session" => exm_core::types::ApprovalScope::Session,
        "allowlist" => exm_core::types::ApprovalScope::Allowlist,
        "once" => exm_core::types::ApprovalScope::Once { approve: true },
        other => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("未知档位: {other}（可选 once/session/allowlist）") })),
            )
                .into_response()
        }
    };
    match st.core.approval_decide_scoped(&id, decision).await {
        Ok(r) => Json(serde_json::to_value(r).unwrap_or(Value::Null)).into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// approve 请求体（approval-ergonomics）：`scope` 缺省 once（不带 body 的旧客户端兼容）
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ApproveBody {
    #[serde(default)]
    pub scope: Option<String>,
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
