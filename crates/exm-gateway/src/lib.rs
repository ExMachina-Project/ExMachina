//! EXMACHINA Gateway —— HTTP REST + WebSocket 接入层（docs/02 M4）
//!
//! 边界：本层只做「协议翻译 + 事件转发」，不含任何调度/提示词逻辑。
//! 所有渠道（WebUI、桌面端、第三方 IM bot）共用同一 REST/WS 契约。

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use exm_core::config::{ExmConfig, LlmConfig};
use exm_core::types::CoreEvent;
use exm_core::Core;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

pub mod channel_test;
pub mod channel_util;

pub mod discord;
pub mod identity;
pub mod limits;
pub mod llm_admin;
pub mod matrix;
pub mod napcat;
pub mod nexus_api;
pub mod platform;
pub mod plugin_admin;
pub mod plugins;
pub mod qqbot;
pub mod singles;
pub mod slack;
pub mod telegram;
pub mod triggers;
pub mod worker_hub;
// 资源域 handlers（自本文件拆出；build_router 经此引入）
use assets::{get_asset, health};
use auth_setup::{verify_auth, auth_status, auth_snooze, auth_dismiss, inbound_channel_id};
use config_admin::{get_config, put_config, get_config_schema};
use sessions::{get_effort, put_effort, get_workspace, put_workspace, create_session, list_sessions, delete_session, get_session, list_messages, chat, get_graph, rename_session, pin_session, list_audit, stop_session, get_evidence, session_undo, session_edit, session_fork, session_archive, session_snapshots, session_tokens};
use agents_admin::{list_agents, create_agent, remove_agent_from_group, remove_agent, set_agent_model, update_agent};
use groups_admin::{list_groups, create_group, active_group, switch_group, activate_group, delete_group, list_templates};
use version::{version_info, version_check, version_apply, version_apply_status};
use persona::{get_agent, get_agent_prompt, put_agent_prompt, get_persona, put_persona, reset_persona};
use playbooks::list_playbooks;
use mcp_admin::{mcp_servers, mcp_refresh, mcp_mount};
use voice::{voice_transcribe, voice_speak};
use checkpoints::{list_checkpoints, restore_checkpoint};
use memory_admin::{list_memory, add_memory, search_memory, memory_stats, memory_reindex, memory_decay, get_memory_md, put_memory_md, memory_render, forget_memory, pin_memory};
use ws::{ws_handler, ws_auth_guard};

pub mod assets;
pub mod auth_setup;
pub mod checkpoints;
pub mod config_admin;
pub mod groups_admin;
pub mod mcp_admin;
pub mod memory_admin;
pub mod persona;
pub mod playbooks;
pub mod sessions;
pub mod agents_admin;
pub mod version;
pub mod voice;
pub mod ws;

/// 提取器拒绝（请求体 / 查询串解析失败）统一成全站错误契约：400 + {"error": 可读中文}。
///
/// axum 0.7 里 `Json`/`Query` 的拒绝在 MethodRouter 层就变成响应，绕过 handler（`HandleErrorLayer`
/// 对 Router 不生效），默认应答是**纯文本英文内部串**——"Failed to deserialize the JSON body into
/// the target type: missing field `description` at line 1 column 54"。前端一律按 `{"error"}` 读取、
/// 失败时把响应体原样塞进 Error（api.ts::req），于是表单少一个字段，用户看到的就是这串内部英文。
///
/// 不改上百个 handler 的签名，改在响应层做归一：仅当应答是提取器拒绝的特征
/// （415 / 422，或 400 + text/plain）时换壳；本层自己抛的 400 业务错误都是 JSON，不会被误伤。
async fn normalize_rejection(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let resp = next.run(req).await;
    let text_plain = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.starts_with("text/plain"))
        .unwrap_or(false);
    let status = resp.status();
    let is_rejection = status == StatusCode::UNPROCESSABLE_ENTITY
        || status == StatusCode::UNSUPPORTED_MEDIA_TYPE
        || (status == StatusCode::BAD_REQUEST && text_plain);
    if !is_rejection {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    let bytes = axum::body::to_bytes(body, 64 * 1024)
        .await
        .unwrap_or_default();
    let raw = String::from_utf8_lossy(&bytes);
    let msg = match status {
        StatusCode::UNPROCESSABLE_ENTITY => format!("请求体字段有误：{}", serde_hint(&raw)),
        StatusCode::UNSUPPORTED_MEDIA_TYPE => "需以 application/json 提交请求体".to_string(),
        _ => format!("请求体格式有误：{}", serde_hint(&raw)),
    };
    parts.status = StatusCode::BAD_REQUEST;
    parts.headers.remove(axum::http::header::CONTENT_TYPE);
    parts.headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    let payload = serde_json::to_vec(&json!({ "error": msg })).unwrap_or_default();
    axum::response::Response::from_parts(parts, axum::body::Body::from(payload))
}

/// 从 serde 的拒绝文案里摘出「缺哪个字段 / 类型不对」那段：去掉固定前缀与行号定位噪音
fn serde_hint(text: &str) -> String {
    let msg = text.split_once(": ").map(|(_, r)| r).unwrap_or(text);
    msg.split(" at line ")
        .next()
        .unwrap_or(msg)
        .trim()
        .to_string()
}


#[derive(Clone)]
pub struct AppState {
    pub core: Arc<Core>,
}

/// 后台鉴权：auth_key 非空时 /api/* 需 X-Auth-Key（或 Bearer）；/api/auth/*（verify/status/引导动作）与静态资源豁免
async fn auth_middleware(
    State(st): State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let required = !st.core.config().security.auth_key.is_empty();
    let path = req.uri().path();
    // 守 /api/* 与 MCP 挂载；静态资源（壳/登录页）与 verify 豁免。
    // /ws 不在此守卫：浏览器 WebSocket 无法携带自定义头，改由路由级 ws_auth_guard
    // 前置校验查询参数密钥（缺失/错误同样 401，安全口径不降级）。MCP HTTP 挂载（10.3）启用时纳入守卫。
    let mcp_path = {
        let c = st.core.config();
        if c.mcp_serve.enabled {
            format!("/{}", c.mcp_serve.http_path.trim_matches('/'))
        } else {
            String::new()
        }
    };
    let guarded = path.starts_with("/api/") || (!mcp_path.is_empty() && path == mcp_path);
    if !required || path.starts_with("/api/auth/") || !guarded {
        return next.run(req).await;
    }
    // 事件 webhook 豁免（7.2）：自带 HMAC 签名校验（独立密钥），不走全局访问密钥
    if path == "/api/events/webhook" && req.method() == axum::http::Method::POST {
        let ew = &st.core.config().triggers.event_webhook;
        if ew.enabled && !ew.secret.trim().is_empty() {
            return next.run(req).await;
        }
    }
    // 通道入站豁免（保守口径）：webhook 通道配置了自有凭据（secret 非空）时放行，
    // 凭据真伪由 inbound handler 自行校验（不符返回 401）；无自有凭据的通道仍被全局鉴权拦截
    if let Some(id) = inbound_channel_id(path, req.method()) {
        let has_own_secret = crate::platform::load_channels(&st.core).iter().any(|c| {
            c.id == id
                && c.enabled
                && c.secret
                    .as_deref()
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
        });
        if has_own_secret {
            return next.run(req).await;
        }
    }
    let provided = req
        .headers()
        .get("x-auth-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| {
            req.headers()
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.strip_prefix("Bearer ").map(|s| s.to_string()))
        });
    match provided {
        Some(k) if k == st.core.config().security.auth_key => {
            // 用量治理：API 维度滑动窗口限流（仅外部入口；组内派发不经 HTTP）
            let limits = st.core.config().limits.clone();
            if limits.enabled && limits.max_requests > 0 {
                let now = exm_core::types::now_ms();
                if !crate::limits::RateLimiter::check(
                    "api",
                    limits.window_secs * 1000,
                    limits.max_requests,
                    now,
                ) {
                    return (
                        StatusCode::TOO_MANY_REQUESTS,
                        Json(json!({ "error": "请求过于频繁", "retrySecs": limits.window_secs })),
                    )
                        .into_response();
                }
            }
            next.run(req).await
        }
        _ => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "需要访问密钥" })),
        )
            .into_response(),
    }
}


pub fn build_router(core: Arc<Core>) -> Router {
    let state = AppState { core: core.clone() };
    let mut router = Router::new()
        .route("/api/health", get(health))
        .route("/api/sessions", get(list_sessions).post(create_session))
        .route("/api/sessions/:id", get(get_session).delete(delete_session))
        .route("/api/sessions/:id/messages", get(list_messages))
        .route("/api/sessions/:id/chat", post(chat))
        .route("/api/sessions/:id/graph", get(get_graph))
        .route("/api/sessions/:id/evidence", get(get_evidence))
        .route(
            "/api/sessions/:id/title",
            axum::routing::put(rename_session),
        )
        .route("/api/sessions/:id/pin", axum::routing::put(pin_session))
        // 会话历史管控（组 9）：撤销 / 编辑重发 / 分支派生 / 归档与快照视图
        .route("/api/sessions/:id/undo", post(session_undo))
        .route("/api/sessions/:id/edit", post(session_edit))
        .route("/api/sessions/:id/fork", post(session_fork))
        .route("/api/sessions/:id/archive", get(session_archive))
        .route("/api/sessions/:id/snapshots", get(session_snapshots))
        .route("/api/agents", get(list_agents).post(create_agent))
        .route(
            "/api/agents/:identifier",
            get(get_agent).put(update_agent).delete(remove_agent),
        )
        .route(
            "/api/agents/:identifier/model",
            axum::routing::put(set_agent_model),
        )
        .route(
            "/api/agents/:identifier/persona",
            get(get_persona).put(put_persona).delete(reset_persona),
        )
        .route("/api/groups", get(list_groups).post(create_group))
        .route("/api/templates", get(list_templates))
        .route("/api/version", get(version_info))
        .route("/api/version/check", get(version_check))
        .route("/api/version/apply", post(version_apply))
        .route("/api/version/apply/status", get(version_apply_status))
        .route("/api/groups/active", get(active_group).put(switch_group))
        .route("/api/groups/:id", axum::routing::delete(delete_group))
        .route("/api/groups/:id/activate", post(activate_group))
        .route("/api/playbooks", get(list_playbooks))
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/config/schema", get(get_config_schema))
        .route("/api/mcp", get(mcp_servers))
        .route("/api/mcp/refresh", post(mcp_refresh))
        .route("/api/voice/transcribe", post(voice_transcribe).layer(axum::extract::DefaultBodyLimit::max(25 * 1024 * 1024)))
        .route("/api/voice/speak", post(voice_speak))
        .route("/api/sessions/:id/tokens", get(session_tokens))
        // 文件检查点（写工具落笔前的自动快照）：回看与回滚
        .route("/api/checkpoints", get(list_checkpoints))
        .route("/api/checkpoints/restore", post(restore_checkpoint))
        // 记忆系统（docs/08 §6）
        .route("/api/memory", get(list_memory).post(add_memory))
        .route("/api/memory/search", post(search_memory))
        .route("/api/memory/stats", get(memory_stats))
        .route("/api/memory/reindex", post(memory_reindex))
        .route("/api/memory/decay", post(memory_decay))
        .route("/api/memory/render", post(memory_render))
        .route("/api/memory/:id", axum::routing::delete(forget_memory))
        .route("/api/memory/:id/pin", post(pin_memory))
        .route("/api/memory/md", get(get_memory_md).put(put_memory_md))
        .route(
            "/ws",
            get(ws_handler).layer(axum::middleware::from_fn_with_state(
                state.clone(),
                ws_auth_guard,
            )),
        )
        .merge(platform::routes())
        .merge(llm_admin::routes())
        .merge(plugin_admin::routes())
        .merge(identity::routes())
        .merge(limits::routes())
        .merge(singles::routes())
        .merge(worker_hub::routes())
        .merge(triggers::routes())
        .merge(nexus_api::routes())
        .route(
            "/api/groups/:gid/agents/:identifier",
            axum::routing::delete(remove_agent_from_group),
        )
        .route(
            "/api/groups/:gid/agents/:identifier/prompt",
            get(get_agent_prompt).put(put_agent_prompt),
        )
        // 停止会话运行中的轮次（process-transparency）
        .route("/api/sessions/:id/stop", post(stop_session))
        // 工具审计聚合（ops-visibility）
        .route("/api/audit", get(list_audit))
        // 通道连通测试（integration-ux）
        .route(
            "/api/channels/:id/test",
            post(crate::channel_test::test_channel),
        )
        .route("/api/auth/verify", post(verify_auth))
        .route("/api/auth/status", get(auth_status))
        .route("/api/auth/snooze", post(auth_snooze))
        .route("/api/auth/dismiss", post(auth_dismiss))
        // 模型思考强度（reasoning effort）：进程级生效于全部 LLM 调用（指挥体/子个体/工具回路）
        .route("/api/llm/effort", get(get_effort).put(put_effort))
        // 工作目录（干活的项目）：智能体与智能体组通用，优先于组声明与全局根
        .route("/api/workspace", get(get_workspace).put(put_workspace))
        // MCP 服务端 HTTP 挂载（10.3）：配置启用时挂载（默认关闭 = 不挂载）；
        // 挂载在鉴权中间件之前，复用同一鉴权口径
        .merge(mcp_mount(core.clone()))
        // 工作区资产（截图 / 工具输出落盘）：沙箱内只读，聊天工具卡内联预览
        .route("/api/asset", get(get_asset))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        // 提取器拒绝归一为全站错误契约（放在鉴权之内：拒绝体不含数据，但口径一致优先）
        .layer(axum::middleware::from_fn(normalize_rejection))
        .layer(tower_http::cors::CorsLayer::permissive())
        // 请求体上限 2MB（审计补面：此前无限制，1MB 纯文本 chat 直接过——公网部署的 DoS 面）。
        // 注意：Option<Json<T>> 提取器会把 413 吞成 None——需要拒绝语义的端点别用 Option 包 Json。
        // 未来大载荷端点（知识库导入等）用 DefaultBodyLimit::max 在该路由显式覆盖。
        .layer(axum::extract::DefaultBodyLimit::max(2 * 1024 * 1024))
        .with_state(state);

    // 静态托管 WebUI 构建产物（存在时）；根路径回落 index.html（SPA）
    let dist = core.config().webui_dist.clone();
    if dist.exists() {
        let index = tower_http::services::ServeFile::new(dist.join("index.html"));
        router = router
            .fallback_service(tower_http::services::ServeDir::new(dist).not_found_service(index));
        // 静态资源缓存语义（SW 换版可靠性）：
        // - sw.js / index.html：no-cache（每次回源验证）——sw.js 被浏览器/旧 SW 缓存会阻塞新版接管，
        //   v1 cache-first 时代的 SW 尤其顽固；index.html 引用 hash 资产，同样必须每次回源
        // - /assets/：immutable 长缓存（内容 hash 文件名，内容变则名变）
        router = router.layer(axum::middleware::from_fn(
            |req: axum::extract::Request, next: axum::middleware::Next| async move {
                let is_static =
                    !req.uri().path().starts_with("/api") && !req.uri().path().starts_with("/ws");
                let path = req.uri().path().to_string();
                let mut resp = next.run(req).await;
                if !is_static {
                    return resp;
                }
                let cc = if path.starts_with("/assets/") {
                    "public, max-age=31536000, immutable"
                } else {
                    // sw.js / index.html / 文档 / 其余：每次回源验证
                    "no-cache"
                };
                resp.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static(cc),
                );
                resp
            },
        ));
    }
    router
}




















// ---------------------------------------------------------------- 启动

/// 启动网关并阻塞运行。
///
/// `host`：默认 `0.0.0.0`（公网可访问，方便本地开发用 `127.0.0.1` 收紧）。
///   可通过 `EXM_HOST` 环境变量覆盖；显式传 `None` 时读取环境变量，传 `Some(s)` 时强制使用 `s`。
pub async fn serve(core: Arc<Core>, port: u16, host: Option<&str>) -> anyhow::Result<()> {
    platform::spawn_cron_scheduler(core.clone());
    // 回收站周期清理（spec: trash-retention；1h 间隔 + 启动错峰，supervisor 同款模式）
    {
        let core_for_trash = core.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(120)).await; // 启动错峰
            loop {
                exm_core::trash::cleanup_once(&core_for_trash);
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            }
        });
    }
    // 过期配对码自动清理：未兑换的码过期即成死数据，周期清扫（1h 间隔；
    // TTL 默认 600s，清理粒度远小于有效期，不会出现可见的过期残留）
    {
        let core_for_codes = core.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(180)).await; // 启动错峰（与回收站错开）
            loop {
                core_for_codes.purge_expired_pairing_codes();
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            }
        });
    }
    telegram::spawn_supervisor(core.clone());
    napcat::spawn_supervisor(core.clone());
    qqbot::spawn_supervisor(core.clone());
    discord::spawn_supervisor(core.clone());
    slack::spawn_supervisor(core.clone());
    matrix::spawn_supervisor(core.clone());
    // 插件装载面：builtin 插件注册 + watcher 拉起（video 走新路径，行为等价）
    plugins::spawn_all(core.clone());
    // 视频互动审批代发执行器（video__act approval=always 批准后的 [video] 回调）
    {
        let core_for_act = core.clone();
        core.register_video_act_executor(Arc::new(move |payload| {
            let core = core_for_act.clone();
            Box::pin(
                async move { plugins::video::tools::execute_act_payload(&core, &payload).await },
            )
        }));
    }
    // 事件触发器（7.1）：文件监听（配置启用时才启动）
    triggers::spawn_file_watcher(core.clone());
    // 分布式执行：工作者池注入（有工作者在线即自动路由远程，失败回落本地）
    {
        let st = AppState { core: core.clone() };
        core.set_remote(worker_hub::hub(&st))?;
    }
    // 断点续跑：上次进程中断的会话（图非终态）自动重新调度收束。
    // 后台执行——续跑可能包含完整的多节点调度与 LLM 往返，绝不能阻塞端口绑定（否则健康检查超时、整站 502）
    {
        let core = core.clone();
        tokio::spawn(async move {
            let n = core.resume_interrupted().await;
            if n > 0 {
                println!("[gateway] 断点续跑：已恢复 {n} 个中断会话（后台）");
            }
        });
    }
    // MCP 工具清单后台刷新（懒连接，失败仅记录；首次调用会重试）
    {
        let mcp = core.mcp();
        tokio::spawn(async move {
            for (id, r) in mcp.refresh_all().await {
                match r {
                    Ok(n) if n > 0 => println!("[mcp] {id}: {n} 个工具就绪"),
                    Ok(_) => {}
                    Err(e) => eprintln!("[mcp] {id}: 清单获取失败（首次调用时重试）: {e}"),
                }
            }
        });
    }
    let router = build_router(core.clone());
    // 默认监听 0.0.0.0，公网可访问（避免「能跑起来」但远程访问不到的尴尬）。
    // 本地严格隔离可设环境变量 EXM_HOST=127.0.0.1 或显式传 Some("127.0.0.1")。
    let host_str: String = match host {
        Some(s) => s.to_string(),
        None => std::env::var("EXM_HOST").unwrap_or_else(|_| "0.0.0.0".to_string()),
    };
    let bind_addr: std::net::IpAddr = host_str
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid --host/EXM_HOST value {host_str:?}: {e}"))?;
    let addr = std::net::SocketAddr::from((bind_addr, port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("[gateway] EXMACHINA Gateway 已启动");
    println!("[gateway]   REST  http://{addr}/api");
    println!("[gateway]   WS    ws://{addr}/ws?sessionId=<id>");
    println!(
        "[gateway]   LLM   {}",
        if core.is_mock() {
            "测试替身（EXM_LLM_MOCK=1）".to_string()
        } else {
            let c = core.config();
            let configured = !c.llm.api_key.trim().is_empty() || !c.llm.api_keys.is_empty();
            if configured {
                format!(
                    "{}（{}）",
                    if c.active_profile.is_empty() {
                        "全局通道"
                    } else {
                        c.active_profile.as_str()
                    },
                    c.llm.base_url
                )
            } else {
                "未配置 —— 请在「提供商」页填写端点与 API Key".to_string()
            }
        }
    );
    println!(
        "[gateway]   个体  {}（1 指挥体 + {} 子个体）",
        core.registry.count(),
        core.registry.units().len()
    );
    let dist = core.config().webui_dist.clone();
    println!(
        "[gateway]   WebUI {}",
        if dist.exists() {
            format!("{}", dist.display())
        } else {
            "未构建（运行 npm run build:webui 后可由本网关托管）".to_string()
        }
    );
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}
