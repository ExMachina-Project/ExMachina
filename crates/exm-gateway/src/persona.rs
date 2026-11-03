//! gateway/persona —— 自 lib.rs 按资源域拆出（纯搬移，零行为变更）

use super::*;

pub(crate) async fn get_agent(
    State(st): State<AppState>,
    Path(identifier): Path<String>,
) -> impl IntoResponse {
    match st.core.agent(&identifier) {
        Some(a) => Json(serde_json::to_value(a).unwrap_or(Value::Null)).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "个体不存在" })),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------- 人设（说话风格）

// ---------------------------------------------------------------- 组内个体系统提示词（PROMPT 读写）

/// 组内个体系统提示词读取（GET /api/groups/:gid/agents/:identifier/prompt）
pub(crate) async fn get_agent_prompt(
    State(st): State<AppState>,
    Path((gid, identifier)): Path<(String, String)>,
) -> impl IntoResponse {
    let Some(def) = st
        .core
        .registry()
        .agents_in_group(&gid)
        .into_iter()
        .find(|a| a.identifier == identifier)
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "个体不存在" })),
        )
            .into_response();
    };
    match st.core.registry().load_group_prompt(&gid, &def.prompt_file) {
        Ok(prompt) => Json(
            json!({ "identifier": identifier, "promptFile": def.prompt_file, "prompt": prompt }),
        )
        .into_response(),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "提示词文件不存在", "promptFile": def.prompt_file })),
        )
            .into_response(),
    }
}

/// 组内个体系统提示词保存（PUT）：热生效，下一轮派发即装载
pub(crate) async fn put_agent_prompt(
    State(st): State<AppState>,
    Path((gid, identifier)): Path<(String, String)>,
    Json(b): Json<crate::singles::PromptBody>,
) -> impl IntoResponse {
    let Some(def) = st
        .core
        .registry()
        .agents_in_group(&gid)
        .into_iter()
        .find(|a| a.identifier == identifier)
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "个体不存在" })),
        )
            .into_response();
    };
    if b.prompt.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "提示词不能为空" })),
        )
            .into_response();
    }
    match st
        .core
        .registry()
        .write_group_prompt(&gid, &def.prompt_file, &b.prompt)
    {
        Ok(final_name) => {
            // 旧名 → 新名：def 回写（读/导出/语言变体解析都走 promptFile）
            if final_name != def.prompt_file {
                let mut updated = def.clone();
                updated.prompt_file = final_name.clone();
                let _ = st.core.registry().upsert_agent(&gid, updated, None);
            }
            Json(json!({ "ok": true, "identifier": identifier, "promptFile": final_name }))
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub(crate) async fn get_persona(
    State(st): State<AppState>,
    Path(identifier): Path<String>,
) -> impl IntoResponse {
    match st.core.persona(&identifier) {
        Ok(persona) => {
            let custom = st.core.persona_is_custom(&identifier).unwrap_or(false);
            Json(json!({
                "identifier": identifier,
                "persona": persona,
                "custom": custom,
                "default": exm_core::registry::LocalRegistry::DEFAULT_PERSONA,
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct PersonaBody {
    persona: String,
}

pub(crate) async fn put_persona(
    State(st): State<AppState>,
    Path(identifier): Path<String>,
    Json(b): Json<PersonaBody>,
) -> impl IntoResponse {
    match st.core.set_persona(&identifier, &b.persona) {
        Ok(_) => Json(json!({ "ok": true, "custom": true })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub(crate) async fn reset_persona(
    State(st): State<AppState>,
    Path(identifier): Path<String>,
) -> impl IntoResponse {
    match st.core.reset_persona(&identifier) {
        Ok(_) => Json(json!({ "ok": true, "custom": false })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
