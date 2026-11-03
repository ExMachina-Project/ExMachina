//! gateway/platform/groups —— 自 platform.rs 按资源域拆出（纯搬移，零行为变更）


use super::*;
/// 组总览（组是交互对象）：编成 / 链路 / 会话 / 记忆 / 成员可靠性 一块仪表数据
pub async fn group_overview(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if st.core.group_meta(&id).is_none() {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "组不存在" }))).into_response();
    }
    let agents = st.core.registry().agents_in_group(&id);
    let playbooks = st
        .core
        .registry()
        .playbooks_in_group(&id)
        .unwrap_or_default();
    let sessions = st.core.list_sessions_in_group(&id).unwrap_or_default();
    let stats = st.core.memory_stats().unwrap_or(json!({}));
    // memory_stats() 返回原始统计：byGroup / shared 在顶层
    let group_memory = stats
        .pointer("/byGroup")
        .and_then(|m| m.get(&id))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let global_memory = stats.get("shared").and_then(|v| v.as_u64()).unwrap_or(0);
    let member_ids: std::collections::HashSet<String> =
        agents.iter().map(|a| a.identifier.clone()).collect();
    let agent_stats: Vec<Value> = stats["agentStats"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter(|s| {
                    s["agentId"]
                        .as_str()
                        .map(|a| member_ids.contains(a))
                        .unwrap_or(false)
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    Json(json!({
        "group": st.core.group_meta(&id),
        "agents": agents.len(),
        "playbooks": playbooks.len(),
        "sessions": sessions.len(),
        "memory": { "group": group_memory, "shared": global_memory },
        "agentStats": agent_stats,
    }))
    .into_response()
}

/// 指定组的个体清单（组管理页用，不依赖激活组）
pub async fn group_agents(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    Json(serde_json::to_value(st.core.registry().agents_in_group(&id)).unwrap_or(Value::Null))
        .into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupPrimaryBody {
    pub identifier: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupWorkspaceBody {
    #[serde(default)]
    pub workspace: Option<String>,
}

/// 设置组工作区（空 = 回退全局工作区）
pub async fn set_group_workspace(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<GroupWorkspaceBody>,
) -> impl IntoResponse {
    let ws = b.workspace.unwrap_or_default();
    match st.core.registry().set_workspace(&id, &ws) {
        Ok(_) => Json(json!({ "ok": true, "workspace": if ws.trim().is_empty() { Value::Null } else { json!(ws) } }))
            .into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

/// 设置组内主智能体：**已停用**——主智能体由系统指定（组的首个个体），不提供手动更换。
/// 路由保留以便旧客户端得到明确拒绝，而非静默 404。
pub async fn set_group_primary(
    State(_st): State<AppState>,
    Path(_id): Path<String>,
    Json(_b): Json<GroupPrimaryBody>,
) -> impl IntoResponse {
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "error": "主智能体由系统指定（组的首个个体），不支持手动更换" })),
    )
        .into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupModelBody {
    #[serde(default)]
    pub model: Option<String>,
}

/// 设置组默认模型（空 = 跟随全局生效档案；组内未显式指定模型的个体随组）
pub async fn set_group_model(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<GroupModelBody>,
) -> impl IntoResponse {
    let m = b.model.unwrap_or_default();
    match st.core.registry().set_group_model(&id, &m) {
        Ok(_) => Json(json!({ "ok": true, "model": if m.trim().is_empty() { Value::Null } else { json!(m) } }))
            .into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupCapsBody {
    #[serde(default)]
    pub speech: Option<String>,
    #[serde(default)]
    pub transcribe: Option<String>,
    #[serde(default)]
    pub vision_relay: Option<String>,
    #[serde(default)]
    pub embedding: Option<String>,
}

/// 设置组级能力模型覆盖（每组可用不同模型栈；空串 = 清除该项跟随全局）
pub async fn set_group_capabilities(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<GroupCapsBody>,
) -> impl IntoResponse {
    let caps = exm_core::types::GroupCapabilities {
        speech: b.speech,
        transcribe: b.transcribe,
        vision_relay: b.vision_relay,
        embedding: b.embedding,
    };
    match st.core.registry().set_group_capabilities(&id, caps) {
        Ok(_) => Json(json!({
            "ok": true,
            "capabilities": st.core.group_meta(&id).and_then(|m| m.capabilities),
        }))
        .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------- 经验优化（子个体自适应）

/// 手动触发：基于教训与统计合成个体的经验改进要点
pub async fn optimize_agent(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match st.core.orchestrator().optimize_agent(&id).await {
        Ok(a) => Json(serde_json::to_value(a).unwrap_or(Value::Null)).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn get_adaptation(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match st.core.registry().load_adaptation(&id) {
        Ok(Some(a)) => Json(serde_json::to_value(a).unwrap_or(Value::Null)).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "该个体尚无经验要点" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn reset_adaptation(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match st.core.registry().reset_adaptation(&id) {
        Ok(true) => Json(json!({ "ok": true })).into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "该个体尚无经验要点" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
