//! gateway/ws —— 自 lib.rs 按资源域拆出（纯搬移，零行为变更）

use super::*;

// ---------------------------------------------------------------- WebSocket

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WsQuery {
    #[serde(default)]
    session_id: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct WsAuthQuery {
    #[serde(default)]
    key: Option<String>,
}

/// /ws 鉴权守卫：与 ws_handler 内联校验同一口径，但**先于** handler 提取器执行。
///
/// 提取器（WebSocketUpgrade）在 handler 体之前运行，未携带升级头的裸 GET /ws 会在鉴权前
/// 就被挡成 400 —— 于是未授权探测拿到的是 400 而非 401（虽不泄露数据，但与「无密钥一律 401」
/// 的对外口径不符）。这里用路由级前置守卫把口径统一，handler 内的校验保留为纵深防御。
pub(crate) async fn ws_auth_guard(
    State(st): State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let cfg = st.core.config();
    if cfg.security.auth_key.is_empty() {
        return next.run(req).await;
    }
    let provided = req.uri().query().and_then(|q| {
        q.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k == "key").then(|| percent_decode(v))
        })
    });
    if provided.as_deref() != Some(cfg.security.auth_key.as_str()) {
        return (StatusCode::UNAUTHORIZED, "需要访问密钥").into_response();
    }
    next.run(req).await
}

/// 查询串百分号解码（守卫里只需 key 一项，取最简实现：%XX → 字节，其余原样）
pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(q): Query<WsQuery>,
    Query(a): Query<WsAuthQuery>,
    State(st): State<AppState>,
) -> impl IntoResponse {
    let required = !st.core.config().security.auth_key.is_empty();
    if required && a.key.as_deref() != Some(st.core.config().security.auth_key.as_str()) {
        return (StatusCode::UNAUTHORIZED, "需要访问密钥").into_response();
    }
    ws.on_upgrade(move |socket| ws_loop(socket, st.core, q.session_id))
}

pub(crate) async fn ws_loop(mut socket: WebSocket, core: Arc<Core>, session_filter: Option<String>) {
    let mut rx = core.subscribe();
    // 心跳保活：周期性协议层 Ping（浏览器自动 Pong）。空闲连接经 NAT/代理常被静默丢弃，
    // 且无事件流时客户端无从判活——Ping 既保活又让断连可感知。
    let mut ping = tokio::time::interval(std::time::Duration::from_secs(25));
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ping.tick().await; // 首跳立即返回，跳过
    loop {
        tokio::select! {
            _ = ping.tick() => {
                if socket.send(Message::Ping(Vec::new())).await.is_err() {
                    break;
                }
            }
            evt = rx.recv() => {
                match evt {
                    Ok(e) => {
                        // 审批事件绕过会话过滤直推：审批常发生在 spawn 子会话，用户停在
                        // 主会话时若按过滤器丢弃，内联审批卡永不出现（只能去独立页面裁决，
                        // 阻塞等待窗内错过即工具受阻）。审批是全局关注事件，须跨会话可见。
                        let approval_evt = e.kind == "approval.required" || e.kind == "approval.resolved";
                        if let Some(sid) = &session_filter {
                            if &e.session_id != sid && !approval_evt {
                                continue;
                            }
                        }
                        let text = serde_json::to_string(&e).unwrap_or_default();
                        if socket.send(Message::Text(text)).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
}
