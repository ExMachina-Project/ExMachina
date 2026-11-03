//! gateway/playbooks —— 自 lib.rs 按资源域拆出（纯搬移，零行为变更）

use super::*;

pub(crate) async fn list_playbooks(State(st): State<AppState>) -> impl IntoResponse {
    match st.core.playbooks() {
        Ok(p) => Json(serde_json::to_value(p).unwrap_or(Value::Null)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
