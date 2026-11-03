//! gateway/platform/skills —— 自 platform.rs 按资源域拆出（纯搬移，零行为变更）

// ---------------------------------------------------------------- 技能包


use super::*;
pub async fn list_skills(State(st): State<AppState>) -> impl IntoResponse {
    match st.core.skills() {
        Ok(s) => Json(serde_json::to_value(s).unwrap_or(Value::Null)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillBody {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub triggers: Vec<String>,
    pub instructions: String,
    #[serde(default)]
    pub agents: Vec<String>,
}

pub async fn create_skill(
    State(st): State<AppState>,
    Json(b): Json<SkillBody>,
) -> impl IntoResponse {
    let skill = exm_core::types::SkillDef {
        id: b.id.trim().to_string(),
        name: b.name.trim().to_string(),
        description: b.description,
        triggers: b
            .triggers
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        instructions: b.instructions.trim().to_string(),
        agents: b
            .agents
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        created_at: exm_core::types::now_iso(),
    };
    match st.core.add_skill(skill.clone()) {
        Ok(_) => Json(serde_json::to_value(skill).unwrap_or(Value::Null)).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn delete_skill(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    match st.core.remove_skill(&id) {
        // 幂等语义（对齐 sessions DELETE）：不存在也 200——重复删除/并发删除安全，
        // 客户端无需区分「刚删过」与「从未存在」
        Ok(_) => Json(json!({ "ok": true })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
