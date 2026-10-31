//! gateway/checkpoints —— 自 lib.rs 按资源域拆出（纯搬移，零行为变更）

use super::*;

/// 文件检查点清单（写工具落笔前的自动快照）
pub(crate) async fn list_checkpoints(State(st): State<AppState>) -> impl IntoResponse {
    let rows: Vec<serde_json::Value> = st
        .core
        .list_checkpoints()
        .into_iter()
        .map(|(date, path, size)| json!({ "date": date, "path": path, "size": size }))
        .collect();
    Json(json!({ "checkpoints": rows }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RestoreBody {
    path: String,
    #[serde(default)]
    date: Option<String>,
}

/// 回滚某文件到检查点版本（date 缺省 = 最近一份）
pub(crate) async fn restore_checkpoint(
    State(st): State<AppState>,
    Json(b): Json<RestoreBody>,
) -> impl IntoResponse {
    match st.core.restore_checkpoint(b.date.as_deref(), &b.path) {
        Ok(msg) => Json(json!({ "ok": true, "message": msg })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
