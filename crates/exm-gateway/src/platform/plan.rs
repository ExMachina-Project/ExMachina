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

/// goal/spec/plan/todo 关联视图（capability-parity 4.5，GET
/// /api/sessions/:id/workflow）：四者并存的「目标/规格/方案/执行进度」关系。
/// 只读聚合（关系是派生视图，不是第二事实源——各自状态仍以 workflow.* 事件、
/// plan 事实源与 todo 快照为准）；WebUI/CLI/TUI 同一口径。
pub async fn workflow_view(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match st.core.store.get_session(&id) {
        Ok(Some(_)) => {
            let relation = st.core.workflow_relation(&id);
            match relation {
                Ok(r) => Json(json!({
                    "sessionId": id,
                    "goal": r.goal,
                    "spec": r.spec,
                    "plan": {
                        "mode": if r.plan_mode { "plan" } else { "execute" },
                        "items": st.core.plan_list(&id),
                        "count": r.plan_items,
                    },
                    "todoPresent": r.todo_present,
                }))
                .into_response(),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": e.to_string() })),
                )
                    .into_response(),
            }
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "会话不存在" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
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

/// 上下文来源清单（capability-parity 6.3，GET /api/sessions/:id/context）：
/// 文件/记忆/检索/技能四类来源状态 + 禁用位（只读）。
///
/// `gaps`：整层缺席的文件来源及其原因（未绑定项目 / 项目目录不可用 / 没有
/// `~/.exmachina`）。清单里一个 `file:` 都没有时，光看清单无从知道为什么——
/// 由 Core 给出事实。它是说明不是来源：不在 `sources` 里，不可停用。
pub async fn context_sources(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match st.core.context_sources(&id) {
        Ok(sources) => {
            // 清单已经拿到了：缺席说明取不到（理论上只可能是会话此刻被删）时
            // 不让整张清单失败——给空数组，不编造原因
            let gaps = st.core.context_source_gaps(&id).unwrap_or_default();
            Json(json!({
                "sessionId": id,
                "sources": sources,
                "count": sources.len(),
                "gaps": gaps,
            }))
            .into_response()
        }
        Err(e) if e.to_string().contains("会话不存在") => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSourceBody {
    /// disable = 禁用来源的自动注入；enable = 恢复；refresh = 重读（带 key 只重读该来源）
    pub action: String,
    /// 来源键：file:<name> / memory / retrieval / skills（disable/enable 必填；
    /// refresh 省略时重读整表）
    pub key: Option<String>,
}

/// 上下文来源治理（POST /api/sessions/:id/context）：
/// `{action, key?}`。
/// - `disable` / `enable`：持久化会话级清单并广播 `context.sources.updated`。
///   **未知 key → 400**（不把不存在的来源记成已禁用）。
/// - `refresh`：带 key 时只重读这一个来源（`{key, ok, error, source, others}`，
///   失败只体现在该来源上，其余来源原样返回）；不带 key 时重读整表。
///
/// 禁用/启用的作用范围：停止该来源的**自动注入**（规划/派发/智能体直连各注入点）。
/// 模型主动发起的记忆召回、用户显式 `/skill <id>` 不受此开关影响——回复里如实说明，
/// 不写没有验证过的保证。
pub async fn context_source_decide(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<ContextSourceBody>,
) -> impl IntoResponse {
    let key = b
        .key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_string);
    let bad =
        |msg: String| (StatusCode::BAD_REQUEST, Json(json!({ "error": msg }))).into_response();
    let not_found = |e: anyhow::Error| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response()
    };
    match b.action.as_str() {
        "disable" | "enable" => {
            let Some(key) = key else {
                return bad(
                    "disable/enable 需要来源 key（file:<name> / memory / retrieval / skills）"
                        .into(),
                );
            };
            let disable = b.action == "disable";
            match st.core.context_source_set(&id, &key, disable) {
                Ok(disabled) => match st.core.context_sources(&id) {
                    Ok(sources) => Json(json!({
                        "sessionId": id,
                        "key": key,
                        "disabled": disable,
                        "disabledKeys": disabled,
                        "sources": sources,
                    }))
                    .into_response(),
                    Err(e) => not_found(e),
                },
                Err(e) if e.to_string().contains("会话不存在") => not_found(e),
                Err(e) => bad(e.to_string()),
            }
        }
        "refresh" => match key {
            Some(key) => match st.core.context_source_refresh(&id, &key) {
                // 单来源刷新失败不是请求失败：200 + ok=false（其余来源照常返回）
                Ok(mut report) => {
                    let refreshed = report["ok"].as_bool() == Some(true);
                    if let serde_json::Value::Object(map) = &mut report {
                        map.insert("sessionId".into(), json!(id));
                        map.insert("refreshed".into(), json!(refreshed));
                    }
                    (StatusCode::OK, Json(report)).into_response()
                }
                Err(e) if e.to_string().contains("会话不存在") => not_found(e),
                Err(e) => bad(e.to_string()),
            },
            // 整表刷新（不带 key）：逐个来源**真的**刷新——与 CLI `exm context <sid> refresh`
            // 同一语义，每个来源各留一条 context.source.refreshed 事件。一个失败不中断
            // 其余：失败的来源照常出现在 results 里（ok=false + error）。
            None => {
                let sources = match st.core.context_sources(&id) {
                    Ok(s) => s,
                    Err(e) if e.to_string().contains("会话不存在") => return not_found(e),
                    Err(e) => return bad(e.to_string()),
                };
                let mut results = Vec::with_capacity(sources.len());
                let mut failed = 0usize;
                for s in &sources {
                    let key = s["key"].as_str().unwrap_or_default().to_string();
                    match st.core.context_source_refresh(&id, &key) {
                        Ok(mut report) => {
                            if report["ok"].as_bool() != Some(true) {
                                failed += 1;
                            }
                            // 整表里每个来源的「其余来源」就是清单的其余项，逐份回传是冗余
                            if let serde_json::Value::Object(map) = &mut report {
                                map.remove("others");
                            }
                            results.push(report);
                        }
                        // 清单里有它、刷新却报错（会话中途消失等）：如实记为失败，继续其余
                        Err(e) => {
                            failed += 1;
                            results
                                .push(json!({ "key": key, "ok": false, "error": e.to_string() }));
                        }
                    }
                }
                let latest = st.core.context_sources(&id).unwrap_or_default();
                Json(json!({
                    "sessionId": id,
                    "refreshed": failed == 0,
                    "failed": failed,
                    "results": results,
                    "sources": latest,
                }))
                .into_response()
            }
        },
        other => bad(format!(
            "未知 action：{other}（支持 disable/enable/refresh）"
        )),
    }
}

/// 学习候选清单（capability-parity 6.4，GET /api/learning/candidates）：
/// 状态过滤 query `?status=pending|approved|rejected`（缺省全部）。
pub async fn learning_candidates_list(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let status = q.get("status").map(|s| s.as_str());
    match st.core.learning_candidates(status) {
        Ok(list) => Json(json!({
            "candidates": list,
            "count": list.len(),
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LearningDecideBody {
    /// approve / reject
    pub action: String,
    /// 决定者标识（通道用户 / 控制台操作者）
    pub decided_by: Option<String>,
    pub reason: Option<String>,
}

/// 学习候选人工裁决（POST /api/learning/candidates/:id）：
/// {action: "approve"|"reject", decidedBy, reason}。一次定局（不可重复裁决）。
pub async fn learning_candidates_decide(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<LearningDecideBody>,
) -> impl IntoResponse {
    let decided_by = b.decided_by.clone().unwrap_or_else(|| "console".into());
    match b.action.as_str() {
        "approve" => match st.core.learning_decide(&id, true, &decided_by, b.reason.as_deref()) {
            Ok(c) => Json(json!({
                "id": c.id, "status": c.status, "decidedBy": c.decided_by, "decidedAt": c.decided_at,
                // 转正落点与版本号（capability-parity 6.6）：治理面据此展示「写进了哪条版本链」；
                // ok=false 时带 error——批准 ≠ 生效，必须如实透出
                "promotion": c.promotion.as_ref().map(|p| json!({
                    "ok": p.ok, "refId": p.ref_id, "version": p.version, "error": p.error,
                })),
            }))
            .into_response(),
            Err(e) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response(),
        },
        "reject" => match st.core.learning_decide(&id, false, &decided_by, b.reason.as_deref()) {
            Ok(c) => Json(json!({
                "id": c.id, "status": c.status, "decidedBy": c.decided_by,
                "decidedAt": c.decided_at, "rejectReason": c.reject_reason,
            }))
            .into_response(),
            Err(e) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response(),
        },
        other => (
            StatusCode::BAD_REQUEST,
            Json(json!(
                { "error": format!("未知 action：{other}（支持 approve/reject）") }
            )),
        )
            .into_response(),
    }
}

/// 已删除（历史保留、可回滚恢复）的技能清单（capability-parity 6.6，GET /api/skills/deleted）。
/// 技能删除后从常规列表消失——没有这个入口，治理面就无处提供「回滚恢复」。
/// 最近删除的在前。
pub async fn skill_deleted_list(State(st): State<AppState>) -> impl IntoResponse {
    match st.core.list_deleted_skills() {
        Ok(rows) => Json(json!({
            "skills": rows.iter().map(|d| json!({
                "id": d.id, "deletedAt": d.deleted_at,
            })).collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// 技能版本历史（capability-parity 6.5，GET /api/skills/:id/versions）。
pub async fn skill_versions(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let deleted = st.core.skill_is_deleted(&id).unwrap_or(false);
    match st.core.skill_history_detailed(&id) {
        Ok(history) => Json(json!({
            "id": id,
            // 每行带该版本的完整定义：治理面要能看出「这一版写了什么」才谈得上选回滚目标
            "versions": history.iter().map(|r| json!({
                "version": r.version, "name": r.name,
                "description": r.description, "instructions": r.instructions,
            })).collect::<Vec<_>>(),
            // 已删除：没有「当前生效版本」——current 置 null，不能报成最后一个归档版本，
            // 否则客户端会把「已删除」读成「vN 生效中」。
            "current": if deleted { serde_json::Value::Null }
                else { json!(history.last().map(|r| r.version).unwrap_or(0)) },
            "deleted": deleted,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillRollbackBody {
    pub to_version: u64,
    pub by: Option<String>,
}

/// 技能回滚（capability-parity 6.5，POST /api/skills/:id/rollback）：
/// 历史版本经同一校验成为新版本行（用户撤销学习；审计链不断）。
pub async fn skill_rollback(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<SkillRollbackBody>,
) -> impl IntoResponse {
    let by = b.by.clone().unwrap_or_else(|| "console".into());
    match st.core.skill_rollback(&id, b.to_version, &by) {
        Ok(s) => Json(json!({
            "id": s.id, "version": s.version, "updatedAt": s.updated_at,
            "rolledBackTo": b.to_version, "by": by,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
