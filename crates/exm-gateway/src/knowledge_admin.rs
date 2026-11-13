//! gateway/knowledge_admin —— 知识库管理面 REST（knowledge-base 任务 1.3）。
//!
//! GET/POST /api/knowledge（清单 + q= 检索参数）、DELETE /api/knowledge/:id。
//! 管理面写入 source 标注 manual（区分模型自我沉淀与人工治理）。

use super::*;

#[derive(Deserialize)]
pub(crate) struct KnowledgeQuery {
    /// 检索关键词：提供时走倒排检索（相关性与热度合排），缺省列最近条目
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

pub(crate) async fn list_knowledge(
    State(st): State<AppState>,
    Query(q): Query<KnowledgeQuery>,
) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let items = match q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(query) => match st.core.knowledge.search(query, limit) {
            Ok(h) => h,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": e.to_string() })),
                )
                    .into_response()
            }
        },
        None => st.core.knowledge.recent(limit),
    };
    Json(json!({ "items": items, "total": items.len() })).into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KnowledgeBody {
    title: String,
    body: String,
    #[serde(default)]
    tags: Vec<String>,
}

pub(crate) async fn add_knowledge(
    State(st): State<AppState>,
    Json(b): Json<KnowledgeBody>,
) -> impl IntoResponse {
    let title = b.title.trim().to_string();
    let body = b.body.trim().to_string();
    if title.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "title 不能为空" })),
        )
            .into_response();
    }
    if body.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "body 不能为空" })),
        )
            .into_response();
    }
    if body.chars().count() > exm_core::knowledge::KNOWLEDGE_BODY_CAP_CHARS {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!(
                    "body 过长（{} 字符 > {} 上限）：请精炼或拆分为多条",
                    body.chars().count(),
                    exm_core::knowledge::KNOWLEDGE_BODY_CAP_CHARS
                )
            })),
        )
            .into_response();
    }
    let draft = exm_core::knowledge::KnowledgeDraft {
        title,
        body,
        tags: b.tags,
        source_agent: "manual".into(),
        source_session: None,
    };
    match st.core.knowledge.write(&draft) {
        Ok(outcome) => {
            let e = outcome.entry();
            Json(json!({
                "entry": serde_json::to_value(e).unwrap_or(Value::Null),
                "deduped": outcome.deduped(),
            }))
            .into_response()
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": err.to_string() })),
        )
            .into_response(),
    }
}

pub(crate) async fn delete_knowledge(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match st.core.knowledge.delete(&id) {
        Ok(true) => Json(json!({ "ok": true })).into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("知识条目不存在：{id}") })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
