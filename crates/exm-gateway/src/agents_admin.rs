//! gateway/agents_admin —— 自 lib.rs 按资源域拆出（纯搬移，零行为变更）

use super::*;

// ---------------------------------------------------------------- 编制

pub(crate) async fn list_agents(State(st): State<AppState>) -> impl IntoResponse {
    Json(serde_json::to_value(st.core.registry().list_active()).unwrap_or(Value::Null))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateAgentBody {
    name: String,
    identifier: String,
    description: String,
    #[serde(default)]
    domain: Option<String>,
    #[serde(default)]
    tier: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    /// 目标组；缺省 = 激活组（内置组受保护，写入会被拒绝）
    #[serde(default)]
    group: Option<String>,
    /// 编成模板 id(entities/templates/unit/<id>):提供后仅 identifier 必填,工具面/能力/提示词由模板提供
    #[serde(default)]
    template: Option<String>,
    /// 默认模型（"档案ID" 或 "档案ID/模型名"；缺省 = 跟随所属组/全局生效档案）
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUpdateBody {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub capabilities: Option<Vec<String>>,
    /// 工具清单（snake_case 标识：read/filesystem/terminal/edit/grep/glob/web_search/web_fetch/agent_manage/schedule/browser）
    #[serde(default)]
    pub tools: Option<Vec<exm_core::types::ToolName>>,
    /// 默认模型（"档案ID" 或 "档案ID/模型名"；空串清除 = 跟随所属组/全局）
    #[serde(default)]
    pub model_hint: Option<String>,
    /// 连结体代理绑定（智能连结网络）：Some(id) 绑定（校验存在）；None = 不动该字段；
    /// 空串 = 解绑
    #[serde(default)]
    pub link: Option<String>,
}

impl AgentUpdateBody {
    pub fn into_patch(self) -> exm_core::registry::AgentPatch {
        exm_core::registry::AgentPatch {
            name: self.name,
            domain: self.domain,
            description: self.description,
            capabilities: self.capabilities,
            tools: self.tools,
            model_hint: self.model_hint,
            // 语义映射：None（未传）= 不动；Some("")（空串）= 解绑；Some(id) = 绑定
            link: self.link.map(|v| {
                let t = v.trim();
                if t.is_empty() {
                    None
                } else {
                    Some(t.to_string())
                }
            }),
        }
    }
}

pub(crate) async fn create_agent(
    State(st): State<AppState>,
    Json(b): Json<CreateAgentBody>,
) -> impl IntoResponse {
    // 模板创建:工具面/能力/提示词由模板提供,仅 identifier 必填(name/description 缺省用模板默认)
    if let Some(tpl) = b
        .template
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        let Some(gid) = b.group.clone() else {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "模板创建必须指定目标组 group" })),
            )
                .into_response();
        };
        let was_empty = st.core.registry().group_agent_count(&gid) == 0;
        let result = st
            .core
            .registry()
            .create_agent_from_template(&gid, tpl, &b.identifier, &b.name, &b.description)
            .inspect(|saved| {
                if was_empty {
                    let _ = st.core.registry().set_primary(&gid, &saved.identifier);
                }
            });
        return match result {
            Ok(saved) => Json(serde_json::to_value(saved).unwrap_or(Value::Null)).into_response(),
            Err(e) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response(),
        };
    }
    let def = exm_core::types::AgentDefinition {
        name: b.name,
        identifier: b.identifier,
        domain: b.domain.unwrap_or_else(|| "自定义".into()),
        tier: if b.tier.as_deref() == Some("orchestrator") {
            exm_core::types::Tier::Orchestrator
        } else {
            exm_core::types::Tier::Unit
        },
        description: b.description,
        capabilities: vec![],
        tools: vec![],
        when_to_call: String::new(),
        dependencies: vec![],
        composable_with: vec![],
        input_schema: Default::default(),
        output_schema: Default::default(),
        prompt_file: String::new(),
        model_hint: b.model.filter(|s| !s.trim().is_empty()),
        link: None,
        max_steps: None,
    };
    // 目标组：显式指定时写入该组（首个个体自动成为该组主智能体），否则激活组
    let result = match &b.group {
        Some(gid) => {
            let was_empty = st.core.registry().group_agent_count(gid) == 0;
            st.core
                .registry()
                .upsert_agent(gid, def, b.prompt.clone())
                .inspect(|saved| {
                    // 主智能体由系统指定：仅组的首个个体自动成为主智能体（tier 不再触发换主）
                    if was_empty {
                        let _ = st.core.registry().set_primary(gid, &saved.identifier);
                    }
                })
        }
        None => st.core.upsert_agent(def, b.prompt).inspect(|saved| {
            if st.core.registry().count() == 1 {
                let _ = st.core.set_primary(&saved.identifier);
            }
        }),
    };
    match result {
        Ok(saved) => Json(serde_json::to_value(saved).unwrap_or(Value::Null)).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// 按组删除个体（子个体页可跨组管理，不依赖激活组）
pub(crate) async fn remove_agent_from_group(
    State(st): State<AppState>,
    Path((gid, identifier)): Path<(String, String)>,
) -> impl IntoResponse {
    match st.core.registry().remove_agent(&gid, &identifier) {
        Ok(_) => Json(json!({ "ok": true })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub(crate) async fn remove_agent(
    State(st): State<AppState>,
    Path(identifier): Path<String>,
) -> impl IntoResponse {
    match st.core.remove_agent(&identifier) {
        Ok(_) => Json(json!({ "ok": true })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentModelBody {
    #[serde(default)]
    model: Option<String>,
}

/// 设置个体默认模型（智能体/个体设置页用；空 = 跟随所属组/全局生效档案）
pub(crate) async fn set_agent_model(
    State(st): State<AppState>,
    Path(identifier): Path<String>,
    Json(b): Json<AgentModelBody>,
) -> impl IntoResponse {
    let model = b.model.unwrap_or_default();
    match st.core.registry().set_agent_model(&identifier, &model) {
        Ok(_) => Json(json!({ "ok": true, "model": if model.trim().is_empty() { Value::Null } else { json!(model) } }))
            .into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

/// 更新个体可编辑字段（name/domain/description/capabilities/tools/model_hint）。
/// identifier/tier/prompt_file 不可改；智能体与组内个体（含内置组受限编辑）统一走 registry::update_agent。
pub(crate) async fn update_agent(
    State(st): State<AppState>,
    Path(identifier): Path<String>,
    Json(b): Json<AgentUpdateBody>,
) -> impl IntoResponse {
    // 连结体代理绑定校验（gateway 持有 config：link_id 必须指向已存在的连结体）
    if let Some(id) = b.link.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        if !st.core.config().nexus.links.iter().any(|l| l.id == id) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("连结体不存在: {id}（先在「智能连结」页添加）") })),
            )
                .into_response();
        }
    }
    match st
        .core
        .registry()
        .update_agent(&identifier, &b.into_patch())
    {
        Ok(saved) => Json(serde_json::to_value(saved).unwrap_or(Value::Null)).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
