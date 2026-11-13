//! gateway/memory_admin —— 自 lib.rs 按资源域拆出（纯搬移，零行为变更）

use super::*;

pub(crate) async fn list_memory(
    State(st): State<AppState>,
    Query(q): Query<MemoryQuery>,
) -> impl IntoResponse {
    let kind = q
        .kind
        .as_deref()
        .and_then(exm_core::memory::MemoryKind::parse);
    match st
        .core
        .memory_list_filtered(kind, q.agent.as_deref(), q.limit.unwrap_or(30))
    {
        Ok(items) => Json(serde_json::to_value(items).unwrap_or(Value::Null)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct MemoryQuery {
    #[serde(default)]
    kind: Option<String>,
    /// 个体归属过滤：传入 identifier = 该个体私有 + 群体共享
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

/// 记忆全量导出（memory-lifecycle 6.3）：JSONL 流式返回全量条目，
/// 字段与 REST list 单条一致；?agent= 过滤同 list 口径。
/// 不含派生索引数据（倒排/哈希索引）——仅条目本体。
/// Content-Disposition 带日期文件名（memory-export-<日期>.jsonl）。
pub(crate) async fn export_memory(
    State(st): State<AppState>,
    Query(q): Query<MemoryQuery>,
) -> impl IntoResponse {
    match st.core.memory_list_filtered(None, q.agent.as_deref(), usize::MAX) {
        Ok(items) => {
            let body = items
                .iter()
                .map(|e| serde_json::to_string(e).unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n");
            let date = chrono::Local::now().format("%Y%m%d");
            let mut resp = axum::response::Response::new(body);
            let h = resp.headers_mut();
            h.insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/x-ndjson; charset=utf-8"),
            );
            if let Ok(v) = axum::http::HeaderValue::from_str(&format!(
                "attachment; filename=\"memory-export-{date}.jsonl\""
            )) {
                h.insert(axum::http::header::CONTENT_DISPOSITION, v);
            }
            resp.into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MemoryBody {
    #[serde(default)]
    kind: Option<String>,
    title: String,
    body: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    pin: bool,
    #[serde(default)]
    importance: Option<f64>,
    #[serde(default)]
    session_id: Option<String>,
    /// 个体归属：传入则为该智能体的私有记忆；缺省为群体共享
    #[serde(default)]
    agent_id: Option<String>,
    /// 跨组全局记忆（缺省归属激活组）
    #[serde(default)]
    global: bool,
}

pub(crate) async fn add_memory(State(st): State<AppState>, Json(b): Json<MemoryBody>) -> impl IntoResponse {
    let kind = b
        .kind
        .as_deref()
        .and_then(exm_core::memory::MemoryKind::parse)
        .unwrap_or(exm_core::memory::MemoryKind::Fact);
    let mut draft = exm_core::memory::MemoryDraft::new(kind, b.title, b.body)
        .importance(b.importance.unwrap_or(0.6))
        .tags(&[]);
    draft.tags = b.tags.clone();
    if let Some(sid) = &b.session_id {
        draft.session_id = Some(sid.clone());
    }
    if let Some(aid) = &b.agent_id {
        draft.agent_id = Some(aid.clone());
    }
    let result = if b.global {
        st.core.memory_remember_global(draft)
    } else {
        st.core.memory_remember(draft)
    };
    match result {
        Ok(entry) => {
            if b.pin {
                let _ = st.core.memory_pin(&entry.id, true);
            }
            Json(serde_json::to_value(entry).unwrap_or(Value::Null)).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct SearchBody {
    query: String,
    #[serde(default)]
    limit: Option<usize>,
    /// 个体检索：传入 identifier = 该个体私有 + 群体共享
    #[serde(default)]
    agent: Option<String>,
}

pub(crate) async fn search_memory(State(st): State<AppState>, Json(b): Json<SearchBody>) -> impl IntoResponse {
    // 调试检索（8.4）：语义检索启用时与对话内召回同口径（词项+语义混合），
    // 并透出命中词项明细与语义分数数值，供记忆页调试开关呈现
    let result = st
        .core
        .memory_recall_debug(&b.query, b.limit, b.agent.as_deref())
        .await;
    match result {
        Ok(hits) => Json(json!({
            "query": b.query,
            "hits": hits.iter().map(|h| {
                let mut m = json!({
                    "entry": serde_json::to_value(&h.entry).unwrap_or(Value::Null),
                    "score": h.score,
                    "reasons": h.reasons,
                });
                // 调试视图（8.4）：命中词项明细与语义分数数值（有才透出）
                if let Some(t) = &h.matched_terms {
                    m["matchedTerms"] = json!(t);
                }
                if let Some(s) = h.semantic_score {
                    m["semanticScore"] = json!(s);
                }
                m
            }).collect::<Vec<_>>()
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub(crate) async fn memory_stats(State(st): State<AppState>) -> impl IntoResponse {
    match st.core.memory_stats() {
        Ok(s) => {
            let stats = st.core.memory.agent_stats(20).unwrap_or_default();
            Json(json!({ "memory": s, "agentStats": stats })).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub(crate) async fn memory_reindex(State(st): State<AppState>) -> impl IntoResponse {
    match st.core.memory_reindex() {
        Ok(n) => Json(json!({ "ok": true, "reindexed": n })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub(crate) async fn memory_decay(State(st): State<AppState>) -> impl IntoResponse {
    match st.core.memory_decay(0.05) {
        Ok(n) => Json(json!({ "ok": true, "changed": n })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// 读取 memory.md（深层记忆关闭时的唯一记忆载体；文件记忆模式）
pub(crate) async fn get_memory_md(State(st): State<AppState>) -> impl IntoResponse {
    let path = st.core.config().memory_md_path.clone();
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    Json(json!({
        "content": content,
        "path": path.display().to_string(),
        "deepEnabled": !st.core.config().memory_enabled,
    }))
}

/// 保存 memory.md
pub(crate) async fn put_memory_md(
    State(st): State<AppState>,
    Json(b): Json<serde_json::Value>,
) -> impl IntoResponse {
    let Some(content) = b.get("content").and_then(|c| c.as_str()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "缺少 content" })),
        )
            .into_response();
    };
    let path = st.core.config().memory_md_path.clone();
    if let Err(e) = std::fs::write(&path, content) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("写入失败: {e}") })),
        )
            .into_response();
    }
    // 超限自主压缩：后台执行（LLM 简略 + 旧文归档），写入立即返回
    let over = content.chars().count() > st.core.config().memory_md_max_chars;
    if over {
        let core = st.core.clone();
        tokio::spawn(async move {
            if let Err(e) = core.compact_memory_md().await {
                eprintln!("[memory] memory.md 自动压缩失败：{e}");
            }
        });
    }
    Json(json!({ "ok": true, "compacting": over })).into_response()
}

pub(crate) async fn memory_render(State(st): State<AppState>) -> impl IntoResponse {
    match st.core.render_memory_md() {
        Ok(n) => Json(json!({ "ok": true, "pinned": n })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub(crate) async fn forget_memory(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    match st.core.memory_forget(&id) {
        Ok(_) => Json(json!({ "ok": true })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct PinBody {
    #[serde(default)]
    pinned: bool,
}

pub(crate) async fn pin_memory(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<PinBody>,
) -> impl IntoResponse {
    match st.core.memory_pin(&id, b.pinned) {
        Ok(_) => Json(json!({ "ok": true, "pinned": b.pinned })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// 条目编辑（assistant-parity T3）：title/body/tags/kind 可选字段——
/// 缺省字段不改动；id 不存在 404；非法 kind 400。
#[derive(Deserialize)]
pub(crate) struct MemoryUpdateBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    kind: Option<String>,
}

pub(crate) async fn update_memory(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<MemoryUpdateBody>,
) -> impl IntoResponse {
    // 空串视为「未提供」：面板清空编辑框不等于清空正文（删除走 forget）
    let title = b.title.filter(|s| !s.trim().is_empty());
    let body = b.body.filter(|s| !s.trim().is_empty());
    match st
        .core
        .memory_update(&id, title.as_deref(), body.as_deref(), b.tags, b.kind.as_deref())
    {
        Ok(Some(e)) => Json(serde_json::to_value(e).unwrap_or_default()).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("记忆条目不存在：{id}") })),
        )
            .into_response(),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": err.to_string() })),
        )
            .into_response(),
    }
}
