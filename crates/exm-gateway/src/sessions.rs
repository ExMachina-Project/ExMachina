//! gateway/sessions —— 自 lib.rs 按资源域拆出（纯搬移，零行为变更）

use super::*;

#[derive(Deserialize)]
pub(crate) struct EffortBody {
    /// low | medium | high；空/null = 恢复端点默认
    effort: Option<String>,
}

/// 当前模型思考强度
pub(crate) async fn get_effort() -> impl IntoResponse {
    Json(json!({ "effort": exm_core::provider::reasoning_effort() }))
}

/// 设置模型思考强度（进程级，重启后回到端点默认；客户端可自行记忆并在启动时重设）
pub(crate) async fn put_effort(Json(body): Json<EffortBody>) -> impl IntoResponse {
    let raw = body.effort.unwrap_or_default();
    exm_core::provider::set_reasoning_effort(Some(&raw));
    let effort = exm_core::provider::reasoning_effort();
    Json(json!({ "ok": true, "effort": effort }))
}

// ---------------------------------------------------------------- 工作目录（干活的项目）

/// 当前生效工作区根：显式工作目录 > 激活组声明 > 全局根
pub(crate) async fn get_workspace(State(st): State<AppState>) -> impl IntoResponse {
    let global = st.core.config().workspace_root.clone();
    let explicit = st.core.registry().active_workspace();
    let path = st.core.registry().effective_workspace(&global);
    Json(json!({ "workspace": path.to_string_lossy(), "explicit": explicit }))
}

#[derive(Deserialize)]
pub(crate) struct WorkspaceBody {
    /// 项目目录路径；空/null = 清除（回退组声明 / 全局根）
    path: Option<String>,
}

/// 设置工作目录（智能体与智能体组通用；目录必须已存在）
pub(crate) async fn put_workspace(
    State(st): State<AppState>,
    Json(body): Json<WorkspaceBody>,
) -> impl IntoResponse {
    let path = body
        .path
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());
    match st.core.registry().set_active_workspace(path.as_deref()) {
        Ok(()) => {
            let global = st.core.config().workspace_root.clone();
            let eff = st.core.registry().effective_workspace(&global);
            Json(json!({ "ok": true, "workspace": eff.to_string_lossy() })).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct ChatBody {
    text: String,
    /// 图片附件（data URL，多模态输入；随本轮进入规划）
    #[serde(default)]
    images: Vec<String>,
    /// 本轮模式（思考模式）：direct = 强制 L0 直答；full = 强制拆解派发。缺省 = 跟随指挥体规划
    #[serde(default)]
    mode: Option<String>,
    /// 子代理直聊目标：非空 = 本轮绕过编排，直接与该子个体对话（桌面端子代理会话）
    #[serde(default)]
    agent: Option<String>,
}

pub(crate) async fn chat(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ChatBody>,
) -> impl IntoResponse {
    if body.text.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "text 不能为空" })),
        )
            .into_response();
    }
    if st.core.store.get_session(&id).ok().flatten().is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "会话不存在" })),
        )
            .into_response();
    }
    // 202 受理，运行过程经 WS 推流（core.chat：会话串行 + followup/collect）
    let core = st.core.clone();
    let text = body.text.clone();
    let images = body.images.clone();
    if !images.is_empty() {
        core.stage_images(&id, images);
    }
    if let Some(mode) = &body.mode {
        exm_core::orchestrator::set_session_mode(&id, mode);
    }
    let agent = body.agent.clone();
    tokio::spawn(async move {
        let result = match &agent {
            // 子代理直聊：绕过编排，与该子个体单独对话（用户可见派发任务与传回内容，可直接追问）
            Some(a) => core.orchestrator().unit_direct_round(&id, a, &text).await,
            None => core.chat(&id, &text).await,
        };
        if let Err(e) = result {
            let _ = core.events.send(CoreEvent {
                kind: "run.error".into(),
                session_id: id.clone(),
                payload: json!({ "message": e.to_string() }),
            });
        }
    });
    (StatusCode::ACCEPTED, Json(json!({ "accepted": true }))).into_response()
}

pub(crate) async fn get_graph(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    match st.core.store.latest_graph(&id) {
        Ok(Some(g)) => Json(serde_json::to_value(g).unwrap_or(Value::Null)).into_response(),
        Ok(None) => Json(json!({ "nodes": [] })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct RenameSessionBody {
    title: String,
}

pub(crate) async fn rename_session(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<RenameSessionBody>,
) -> impl IntoResponse {
    let title = b.title.trim().to_string();
    if title.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "标题不能为空" })),
        )
            .into_response();
    }
    match st.core.store.update_session_title(&id, &title) {
        Ok(_) => Json(json!({ "ok": true, "title": title })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct PinSessionBody {
    pinned: bool,
}

/// 会话置顶切换（PUT /api/sessions/:id/pin）
pub(crate) async fn pin_session(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<PinSessionBody>,
) -> impl IntoResponse {
    match st.core.toggle_session_pin(&id, b.pinned) {
        Ok(_) => Json(json!({ "ok": true, "pinned": b.pinned })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------- 工具审计聚合（ops-visibility）

/// 工具审计查询（GET /api/audit）：跨个体聚合既有审计分片，服务端过滤
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuditQuery {
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

pub(crate) async fn list_audit(State(st): State<AppState>, Query(q): Query<AuditQuery>) -> impl IntoResponse {
    match st.core.store.list_tool_audit_filtered(
        q.agent.as_deref(),
        q.tool.as_deref(),
        q.limit.unwrap_or(200).min(1000),
    ) {
        Ok(items) => Json(json!({ "items": items })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// 停止会话运行中的轮次（POST /api/sessions/:id/stop）：运行中返回已执行工具数，空闲返回 409
pub(crate) async fn stop_session(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    match st.core.stop_session(&id) {
        Ok(tools) => Json(json!({ "ok": true, "toolsExecuted": tools })).into_response(),
        Err(e) => {
            let idle = e.to_string().contains("没有运行中的任务");
            let code = if idle {
                StatusCode::CONFLICT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (code, Json(json!({ "error": e.to_string() }))).into_response()
        }
    }
}

pub(crate) async fn get_evidence(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    match st.core.store.list_evidence(&id) {
        Ok(v) => Json(Value::Array(v)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------- 会话历史管控（组 9）

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UndoBody {
    /// 撤销目标轮次（保留至该轮，其后全部回退）
    turn: usize,
    /// 是否联动回滚该轮之后由写类工具产生的文件变更（经检查点；默认 true）
    #[serde(default)]
    restore_files: Option<bool>,
}

/// 消息撤销（POST /api/sessions/:id/undo）：处理中返回 409
pub(crate) async fn session_undo(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<UndoBody>,
) -> impl IntoResponse {
    match st
        .core
        .undo_session(&id, b.turn, b.restore_files.unwrap_or(true))
        .await
    {
        Ok(info) => Json(info).into_response(),
        Err(e) => {
            let busy = e.to_string().contains("处理中");
            let code = if busy {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            };
            (code, Json(json!({ "error": e.to_string() }))).into_response()
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EditResendBody {
    /// 被编辑的用户消息所在轮次
    turn: usize,
    /// 编辑后的新内容
    text: String,
}

/// 编辑重发（POST /api/sessions/:id/edit）：= 撤销至该轮之前 + 新内容重执行（原内容归档可查）
pub(crate) async fn session_edit(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<EditResendBody>,
) -> impl IntoResponse {
    if b.text.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "text 不能为空" })),
        )
            .into_response();
    }
    match st.core.edit_resend(&id, b.turn, &b.text).await {
        Ok(info) => (StatusCode::ACCEPTED, Json(info)).into_response(),
        Err(e) => {
            let busy = e.to_string().contains("处理中");
            let code = if busy {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_REQUEST
            };
            (code, Json(json!({ "error": e.to_string() }))).into_response()
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ForkBody {
    /// 分支截至的历史轮次（携带前 N 轮完整副本）
    turn: usize,
}

/// 分支派生（POST /api/sessions/:id/fork）：原会话不变，返回新会话
pub(crate) async fn session_fork(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<ForkBody>,
) -> impl IntoResponse {
    match st.core.fork_session(&id, b.turn) {
        Ok(s) => (
            StatusCode::CREATED,
            Json(serde_json::to_value(s).unwrap_or(Value::Null)),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// 归档视图（GET /api/sessions/:id/archive）：被撤销/编辑回退内容的可查留痕
pub(crate) async fn session_archive(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    match st.core.store.list_archived_messages(&id) {
        Ok(msgs) => Json(serde_json::to_value(msgs).unwrap_or(Value::Null)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// 轮次快照清单（GET /api/sessions/:id/snapshots）：undo / fork 的可选轮次与游标
pub(crate) async fn session_snapshots(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match st.core.store.list_turn_snapshots(&id) {
        Ok(snaps) => Json(serde_json::to_value(snaps).unwrap_or(Value::Null)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// 会话 token 用量（真实用量优先，无记录时字符估算）与预算
pub(crate) async fn session_tokens(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let used = st.core.session_tokens_estimate(&id);
    let budget = st.core.config().max_session_tokens;
    let (prompt, completion, calls) = st.core.session_usage(&id);
    Json(json!({
        "estimate": used,
        "budget": budget,
        "unlimited": budget == 0,
        // 真实用量（provider 上报）：calls > 0 时 estimate 即为实际 token 总量
        "promptTokens": prompt,
        "completionTokens": completion,
        "calls": calls,
        "measured": calls > 0,
    }))
}

// ---------------------------------------------------------------- 会话基础

// ---------------------------------------------------------------- 会话

pub(crate) async fn create_session(
    State(st): State<AppState>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    // 用 Bytes 而非 Option<Json>：Option 会吞掉内层提取器的拒绝（含 413 超限），
    // 使全局请求体上限对这一端点失效（审计 5.3 实证）。Bytes 层超限直接 413。
    // 空 body / 无 title 字段 / title 非 string 均兜底「新会话」（原兼容语义）。
    let v: Value = if body.is_empty() {
        Value::Null
    } else {
        match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "请求体不是合法 JSON" })),
                )
                    .into_response()
            }
        }
    };
    let explicit_title = v.get("title").and_then(|t| t.as_str()).map(|s| s.to_string());
    // 显式传了 title 字段但 trim 后为空 → 400（「新会话」兜底只覆盖缺省场景；
    // 纯空白标题在会话列表里是不可辨识的垃圾行）
    if explicit_title.as_deref().is_some_and(|t| t.trim().is_empty()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "标题不能为空（空白字符不算）" })),
        )
            .into_response();
    }
    let title = explicit_title.unwrap_or_else(|| "新会话".to_string());
    match st.core.create_session(&title) {
        Ok(s) => Json(serde_json::to_value(s).unwrap_or(Value::Null)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub(crate) async fn list_sessions(
    State(st): State<AppState>,
    Query(q): Query<SessionListQuery>,
) -> impl IntoResponse {
    // 组是交互对象：会话列表归属激活组（?all=1 管理视角看全部）
    let result = if q.all.unwrap_or(false) {
        st.core.list_sessions()
    } else {
        st.core.list_sessions_in_scope()
    };
    match result {
        Ok(list) => Json(serde_json::to_value(list).unwrap_or(Value::Null)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionListQuery {
    #[serde(default)]
    all: Option<bool>,
}

pub(crate) async fn delete_session(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    match st.core.delete_session(&id) {
        Ok(_) => Json(json!({ "ok": true })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub(crate) async fn get_session(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    match st.core.store.get_session(&id) {
        Ok(Some(s)) => Json(serde_json::to_value(s).unwrap_or(Value::Null)).into_response(),
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

pub(crate) async fn list_messages(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    match st.core.store.list_messages(&id, 200) {
        Ok(msgs) => Json(serde_json::to_value(msgs).unwrap_or(Value::Null)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
