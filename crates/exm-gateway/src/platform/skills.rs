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
        source: "manual".into(),
        ..Default::default()
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

#[derive(Deserialize)]
pub struct SkillImportBody {
    /// SKILL.md 原文（与 url 二选一）
    #[serde(default)]
    pub content: Option<String>,
    /// 远端 SKILL.md 地址（http/https；与 content 二选一）
    #[serde(default)]
    pub url: Option<String>,
}

/// POST /api/skills/import —— 从 SKILL.md 原文或远端 URL 导入技能（同 id 覆盖）。
pub async fn import_skill(
    State(st): State<AppState>,
    Json(b): Json<SkillImportBody>,
) -> impl IntoResponse {
    let raw = if let Some(c) = b.content.as_deref().filter(|c| !c.trim().is_empty()) {
        c.to_string()
    } else if let Some(url) = b.url.as_deref().filter(|u| !u.trim().is_empty()) {
        let url = url.trim();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "url 必须以 http(s):// 开头" })),
            )
                .into_response();
        }
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("HTTP 客户端构建失败: {e}") })),
                )
                    .into_response()
            }
        };
        let resp = match client.get(url).send().await {
            Ok(r) => r,
            Err(e) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({ "error": format!("抓取失败: {e}") })),
                )
                    .into_response()
            }
        };
        if !resp.status().is_success() {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": format!("远端返回 HTTP {}", resp.status().as_u16()) })),
            )
                .into_response();
        }
        match resp.text().await {
            Ok(t) if t.len() <= 1024 * 1024 => t,
            Ok(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "SKILL.md 超过 1MB 上限" })),
                )
                    .into_response()
            }
            Err(e) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({ "error": format!("读取响应失败: {e}") })),
                )
                    .into_response()
            }
        }
    } else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "content 与 url 至少提供一个" })),
        )
            .into_response();
    };
    match st.core.import_skill_md(&raw) {
        Ok(skill) => Json(serde_json::to_value(skill).unwrap_or(Value::Null)).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// GET /api/skills/:id/export —— 导出 SKILL.md 原文（可分享/迁移）。
pub async fn export_skill(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    match st.core.export_skill_md(&id) {
        Ok(Some(md)) => {
            // 文件名只保留安全字符（legacy json 技能 id 可能含引号/分隔符）
            let safe_id: String = id
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            let safe_id = if safe_id.is_empty() { "skill".to_string() } else { safe_id };
            (
                StatusCode::OK,
                [
                    ("content-type", "text/markdown; charset=utf-8"),
                    (
                        "content-disposition",
                        Box::leak(
                            format!("attachment; filename=\"SKILL-{safe_id}.md\"")
                                .into_boxed_str(),
                        ) as &str,
                    ),
                ],
                md,
            )
                .into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("技能不存在: {id}") })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
