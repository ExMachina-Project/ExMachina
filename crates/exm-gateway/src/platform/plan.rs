//! gateway/platform/plan —— 自 platform.rs 按资源域拆出（纯搬移，零行为变更）


use super::*;
/// 计划清单（GET /api/sessions/:id/plan）：当前条目 + 模式位
pub async fn plan_list(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let items = st.core.plan_list(&id);
    Json(json!({
        "sessionId": id,
        "mode": if exm_core::plan::is_plan_mode(&id) { "plan" } else { "execute" },
        "items": items,
        "count": items.len(),
    }))
    .into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanDecideBody {
    /// approve = 批准回放；reject = 拒绝保留；setMode = 切换模式
    pub action: String,
    #[serde(default)]
    pub plan: Option<bool>,
}

/// 计划裁决（POST /api/sessions/:id/plan）：{action: "approve"|"reject"|"setMode"}
pub async fn plan_decide(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<PlanDecideBody>,
) -> impl IntoResponse {
    match b.action.as_str() {
        "approve" => match st.core.plan_approve(&id).await {
            Ok(results) => Json(json!({
                "sessionId": id,
                "applied": results.iter().map(|(item, r)| json!({
                    "itemId": item.id, "tool": item.tool, "target": item.target, "ok": r.ok,
                })).collect::<Vec<_>>(),
                "okCount": results.iter().filter(|(_, r)| r.ok).count(),
                "failCount": results.iter().filter(|(_, r)| !r.ok).count(),
            }))
            .into_response(),
            Err(e) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response(),
        },
        "reject" => {
            let items = st.core.plan_reject(&id);
            Json(json!({
                "sessionId": id,
                "kept": items.len(),
                "mode": if exm_core::plan::is_plan_mode(&id) { "plan" } else { "execute" },
            }))
            .into_response()
        }
        "setMode" => {
            let plan = b.plan.unwrap_or(true);
            st.core.set_plan_mode(&id, plan);
            Json(json!({
                "sessionId": id,
                "mode": if exm_core::plan::is_plan_mode(&id) { "plan" } else { "execute" },
            }))
            .into_response()
        }
        other => (
            StatusCode::BAD_REQUEST,
            Json(
                json!({ "error": format!("未知 action：{other}（支持 approve/reject/setMode）") }),
            ),
        )
            .into_response(),
    }
}
