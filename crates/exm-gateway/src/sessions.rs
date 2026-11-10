//! gateway/sessions —— 自 lib.rs 按资源域拆出（纯搬移，零行为变更）

use super::*;

#[derive(Deserialize)]
pub(crate) struct EffortBody {
    /// low | medium | high；空/null = 恢复端点默认
    effort: Option<String>,
    /// 推理 token 硬上限（≥1024 生效；null = 端点默认）。reasoning+content
    /// 共享预算，防推理模型思考无限烧（anti-runaway-thinking）
    #[serde(default)]
    reasoning_token_cap: Option<u32>,
}

/// 会话内容检索参数（session-search）
#[derive(Deserialize)]
pub(crate) struct SearchParams {
    /// 关键词（去空白后为空 → 400）
    q: Option<String>,
    /// 结果会话上限（缺省 20，钳制 1~50）
    limit: Option<usize>,
}

/// 会话内容检索（GET /api/sessions/search）：标题 + 最近 200 条消息正文，
/// 大小写不敏感；标题命中优先、同组按活跃倒序；返回命中片段（含 messageId）。
pub(crate) async fn search_sessions(
    State(st): State<AppState>,
    Query(p): Query<SearchParams>,
) -> impl IntoResponse {
    let query = p.q.unwrap_or_default().trim().to_string();
    if query.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "q 不能为空" })),
        )
            .into_response();
    }
    let limit = p.limit.unwrap_or(20).clamp(1, 50);
    match st.core.search_sessions(&query, limit) {
        Ok(hits) => Json(json!({ "query": query, "hits": hits })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// 当前模型思考强度
pub(crate) async fn get_effort() -> impl IntoResponse {
    Json(json!({
        "effort": exm_core::provider::reasoning_effort(),
        "reasoningTokenCap": exm_core::provider::reasoning_token_cap(),
    }))
}

/// 设置模型思考强度（进程级，重启后回到端点默认；客户端可自行记忆并在启动时重设）。
/// body.reasoningTokenCap：推理 token 硬上限（≥1024 生效；null/缺省 = 端点默认）——
/// 对推理模型（reasoning+content 共享预算）这是防「思考无限烧」的唯一硬闸。
pub(crate) async fn put_effort(Json(body): Json<EffortBody>) -> impl IntoResponse {
    if let Some(raw) = body.effort.as_deref() {
        exm_core::provider::set_reasoning_effort(Some(raw));
    }
    if let Some(cap) = body.reasoning_token_cap {
        exm_core::provider::set_reasoning_token_cap(Some(cap));
    }
    Json(json!({
        "ok": true,
        "effort": exm_core::provider::reasoning_effort(),
        "reasoningTokenCap": exm_core::provider::reasoning_token_cap(),
    }))
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
        // 「会话不存在」→ 404（REST 状态码矩阵：路径资源不存在不落 500；审计 REST P1-1）
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
        // 「会话不存在」→ 404（REST 状态码矩阵；审计 REST P1-1）
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
pub(crate) struct SessionBatchBody {
    /// batch 动作：archive（归档）/ unarchive（恢复）/ delete（删除）
    action: String,
    ids: Vec<String>,
}

/// 会话批量操作（POST /api/sessions/batch）。
/// 逐条独立执行（单条失败不断整批），返回作用计数与失败明细——
/// 批量语义 = 「尽力而为 + 全量反馈」，前端据 failed 提示部分失败。
pub(crate) async fn session_batch(
    State(st): State<AppState>,
    Json(b): Json<SessionBatchBody>,
) -> impl IntoResponse {
    if b.ids.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "ids 不能为空" })),
        )
            .into_response();
    }
    let mut ok_count = 0usize;
    let mut failed: Vec<Value> = Vec::new();
    for id in &b.ids {
        let r = match b.action.as_str() {
            "archive" => st.core.set_session_archived(id, true),
            "unarchive" => st.core.set_session_archived(id, false),
            "delete" => st.core.delete_session(id),
            other => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": format!("未知动作: {other}") })),
                )
                    .into_response();
            }
        };
        match r {
            Ok(()) => ok_count += 1,
            Err(e) => failed.push(json!({ "id": id, "error": e.to_string() })),
        }
    }
    Json(json!({
        "ok": failed.is_empty(),
        "action": b.action,
        "affected": ok_count,
        "failed": failed,
    }))
    .into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FlowArchiveBody {
    /// 归档范围：flow（单流，channelId+chatKey）/ channel（整通道）
    scope: String,
    channel_id: String,
    #[serde(default)]
    chat_key: Option<String>,
}

/// 按流/按通道归档旧会话（POST /api/sessions/flow-archive，session-management）。
/// 圈定范围 = title 前缀解析；各流活跃绑定会话豁免（下条消息继续复用）。
/// 返回归档/豁免计数，前端回执展示。
pub(crate) async fn session_flow_archive(
    State(st): State<AppState>,
    Json(b): Json<FlowArchiveBody>,
) -> impl IntoResponse {
    let scope = match (b.scope.as_str(), b.chat_key.as_deref()) {
        ("flow", Some(k)) if !k.is_empty() => exm_core::session_origin::ArchiveScope::Flow {
            channel_id: b.channel_id.clone(),
            chat_key: k.to_string(),
        },
        ("channel", _) => exm_core::session_origin::ArchiveScope::Channel {
            channel_id: b.channel_id.clone(),
        },
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "scope 应为 flow（需 chatKey）或 channel" })),
            )
                .into_response();
        }
    };
    match st.core.archive_flow_sessions(scope) {
        Ok((archived, skipped)) => Json(json!({
            "ok": true,
            "archived": archived,
            "skipped": skipped,
        }))
        .into_response(),
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
    /// 审计类别：缺省 = 工具审计；dispatch = 子代理派发审计（agent-budget-audit）
    #[serde(default)]
    kind: Option<String>,
}

pub(crate) async fn list_audit(State(st): State<AppState>, Query(q): Query<AuditQuery>) -> impl IntoResponse {
    // kind=dispatch（agent-budget-audit 3.4）：子代理派发审计（跨会话聚合，
    // 异常条目由前端按 ok/status 高亮）；缺省 = 既有工具审计
    if q.kind.as_deref() == Some("dispatch") {
        return match st.core.store.list_dispatch_audit_all(q.limit.unwrap_or(200).min(1000)) {
            Ok(items) => Json(json!({ "items": items })).into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response(),
        };
    }
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
            // 错误分类（REST 状态码矩阵，审计 P1-1）：处理中→409；会话不存在→404；其余→400
            let msg = e.to_string();
            let code = if msg.contains("处理中") {
                StatusCode::CONFLICT
            } else if msg.contains("会话不存在") || msg.contains("快照不存在") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::BAD_REQUEST
            };
            (code, Json(json!({ "error": msg }))).into_response()
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
            // 错误分类（REST 状态码矩阵，审计 P1-1）：处理中→409；会话/快照不存在→404；其余→400
            let msg = e.to_string();
            let code = if msg.contains("处理中") {
                StatusCode::CONFLICT
            } else if msg.contains("会话不存在") || msg.contains("快照不存在") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::BAD_REQUEST
            };
            (code, Json(json!({ "error": msg }))).into_response()
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
        // 错误分类（REST 状态码矩阵，审计 P1-1）：会话/快照不存在→404；其余（轮次越界等）→400
        Err(e) => {
            let msg = e.to_string();
            let code = if msg.contains("会话不存在") || msg.contains("快照不存在") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::BAD_REQUEST
            };
            (code, Json(json!({ "error": msg }))).into_response()
        }
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


/// 轮次快照的输入预览：轮次 = 第 N 条用户消息（与聊天页编号同口径），
/// 截断至 ≤80 字符；无对应用户消息时给空串。
pub(crate) fn snapshot_preview(user_texts: &[String], turn: usize) -> String {
    if turn == 0 {
        return String::new();
    }
    user_texts
        .get(turn - 1)
        .map(|t| t.chars().take(80).collect())
        .unwrap_or_default()
}

/// 轮次快照清单（GET /api/sessions/:id/snapshots）：undo / fork 的可选轮次与游标。
/// 增量附加 `preview`（该轮用户输入摘要，≤80 字）：轮次 = 第 N 条用户消息，
/// 与聊天页消息编号同口径；不落库、不改既有字段（旧客户端忽略即可）。
pub(crate) async fn session_snapshots(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match st.core.store.list_turn_snapshots(&id) {
        Ok(snaps) => {
            // 全量读取：轮次 = 第 N 条用户消息，截断窗口会让老轮次的 preview 错位
            let msgs = st.core.store.list_messages(&id, 0).unwrap_or_default();
            let user_texts: Vec<String> = msgs
                .iter()
                .filter(|m| m.role == exm_core::types::MessageRole::User)
                .map(|m| {
                    m.statements
                        .iter()
                        .map(|s| s.text.as_str())
                        .collect::<Vec<_>>()
                        .join("；")
                })
                .collect();
            let rows: Vec<Value> = snaps
                .into_iter()
                .map(|s| {
                    let preview = snapshot_preview(&user_texts, s.turn);
                    let mut v = serde_json::to_value(&s).unwrap_or(Value::Null);
                    if let Some(obj) = v.as_object_mut() {
                        obj.insert("preview".into(), json!(preview));
                    }
                    v
                })
                .collect();
            Json(Value::Array(rows)).into_response()
        }
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
    // 组是交互对象：会话列表归属激活组（?all=1 管理视角看全部，附消息数）；
    // ?archived=true 显式查归档区（归档会话不进默认列表）
    let result = if q.archived.unwrap_or(false) {
        st.core.list_archived_sessions()
    } else if q.all.unwrap_or(false) {
        st.core.list_sessions_admin()
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
    #[serde(default)]
    archived: Option<bool>,
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

// ---------------------------------------------------------------- 会话导出

#[derive(Deserialize)]
pub(crate) struct ExportQuery {
    /// markdown | json（缺省 markdown）
    format: Option<String>,
}

/// GET /api/sessions/:id/export?format=markdown|json
/// Markdown：消息 + 思考/工具摘要 + 审批记录（人读归档）；JSON：结构化全量（机读回放）。
pub(crate) async fn export_session(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<ExportQuery>,
) -> impl IntoResponse {
    let Some(session) = st.core.store.get_session(&id).ok().flatten() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "会话不存在" })),
        )
            .into_response();
    };
    let msgs = st.core.store.list_messages(&id, 100_000).unwrap_or_default();
    let approvals = st
        .core
        .approval_list(None, 100_000)
        .unwrap_or_default()
        .into_iter()
        .filter(|a| a.session_id == id)
        .collect::<Vec<_>>();

    let fname = format!(
        "exm-session-{}-{}.{}",
        session.title.replace('/', "-").chars().take(24).collect::<String>(),
        id.chars().take(8).collect::<String>(),
        if q.format.as_deref() == Some("json") { "json" } else { "md" }
    );

    if q.format.as_deref() == Some("json") {
        let body = json!({
            "session": session,
            "messages": msgs,
            "approvals": approvals,
        });
        let body = serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".into());
        (
            StatusCode::OK,
            [
                ("content-type", "application/json; charset=utf-8"),
                (
                    "content-disposition",
                    Box::leak(format!("attachment; filename=\"{fname}\"").into_boxed_str())
                        as &str,
                ),
            ],
            body,
        )
            .into_response()
    } else {
        let mut md = String::new();
        md.push_str(&format!("# 会话导出：{}\n\n", session.title));
        md.push_str(&format!(
            "- 会话 ID：`{}`\n- 创建：{}\n- 状态：{}\n- 消息数：{}\n\n---\n\n",
            session.id,
            session.created_at,
            session.status,
            msgs.len()
        ));
        for m in &msgs {
            let role = match m.role {
                exm_core::types::MessageRole::User => "用户",
                exm_core::types::MessageRole::Orchestrator => "指挥体",
                exm_core::types::MessageRole::Unit => "子个体",
                exm_core::types::MessageRole::System => "系统",
            };
            let agent = m.agent_id.as_deref().unwrap_or("");
            md.push_str(&format!(
                "## {}{} · {}\n\n",
                role,
                if agent.is_empty() {
                    String::new()
                } else {
                    format!("（{agent}）")
                },
                m.created_at
            ));
            if let Some(th) = &m.thinking {
                if !th.trim().is_empty() {
                    md.push_str(&format!(
                        "<details><summary>思考过程（{} 字符）</summary>\n\n```\n{}\n```\n\n</details>\n\n",
                        th.chars().count(),
                        th
                    ));
                }
            }
            if !m.tool_calls.is_empty() {
                md.push_str(&format!(
                    "**工具调用 × {}**：{}\n\n",
                    m.tool_calls.len(),
                    m.tool_calls
                        .iter()
                        .map(|c| format!("`{}`", c.tool))
                        .collect::<Vec<_>>()
                        .join(" ")
                ));
            }
            for s in &m.statements {
                let text = s.text.trim();
                if text.is_empty() {
                    continue;
                }
                md.push_str(&format!("{text}\n\n"));
            }
        }
        if !approvals.is_empty() {
            md.push_str("---\n\n## 审批记录\n\n");
            for a in &approvals {
                md.push_str(&format!(
                    "- **{}** `{}`（{}）— {}{}\n",
                    a.status,
                    a.command.chars().take(120).collect::<String>(),
                    a.created_at,
                    a.agent_id,
                    a.result
                        .as_deref()
                        .map(|r| format!(" → {}", r.chars().take(200).collect::<String>()))
                        .unwrap_or_default()
                ));
            }
        }
        (
            StatusCode::OK,
            [
                ("content-type", "text/markdown; charset=utf-8"),
                (
                    "content-disposition",
                    Box::leak(format!("attachment; filename=\"{fname}\"").into_boxed_str())
                        as &str,
                ),
            ],
            md,
        )
            .into_response()
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::snapshot_preview;

    /// 预览与轮次对应：第 N 轮取第 N 条用户消息；越界给空串；超长截断 80 字。
    #[test]
    fn 快照预览_轮次对应与截断() {
        let texts = vec!["第一轮输入".to_string(), "帮我重构压缩模块".to_string()];
        assert_eq!(snapshot_preview(&texts, 1), "第一轮输入");
        assert_eq!(snapshot_preview(&texts, 2), "帮我重构压缩模块");
        assert_eq!(snapshot_preview(&texts, 3), "");
        assert_eq!(snapshot_preview(&texts, 0), "");
        let long = "字".repeat(200);
        assert_eq!(snapshot_preview(&[long], 1).chars().count(), 80);
    }
}
