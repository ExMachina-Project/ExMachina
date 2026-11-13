//! gateway/platform/events —— 自 platform.rs 按资源域拆出（纯搬移，零行为变更）


use super::*;
/// 会话事件溯源（活动页用，取最近 limit 条）
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventsQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}

pub async fn session_events(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<EventsQuery>,
) -> impl IntoResponse {
    // id=global：全局事件流（session_id 为空的落库事件——auto.eval/auto.act/
    // memory.digest/memory.decay.ran 等实验可观测事件的查询面；设置页
    // 实验区聚合最近运行记录用）。无新端点——复用会话事件溯源路径。
    let id = if id == "global" { String::new() } else { id };
    match st.core.store.list_events(&id) {
        Ok(mut events) => {
            let limit = q.limit.unwrap_or(200).min(1000);
            if events.len() > limit {
                events = events.split_off(events.len() - limit);
            }
            Json(serde_json::to_value(events).unwrap_or(Value::Null)).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
