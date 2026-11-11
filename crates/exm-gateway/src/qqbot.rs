//! QQ 官方机器人适配器（qqbot.rs）：q.qq.com 开放平台 WebSocket 长连接。
//! 鉴权链：appId + appSecret → getAppAccessToken（7200s，本地缓存提前 5 分钟刷新）
//! → WS identify（token 格式 `QQBot {access_token}`）。
//! intents：GROUP_AND_C2C_EVENT(1<<25) 群/单聊 + PUBLIC_GUILD_MESSAGES(1<<30) 公域频道。
//! 被动回复：群 /v2/groups/{group_openid}/messages、单聊 /v2/users/{user_openid}/messages、
//! 频道 /channels/{channel_id}/messages —— 均须携带事件里的 msg_id + 递增 msg_seq
//! （被动消息 5 分钟有效；群/单聊文本限 2000 字节，收束陈述截断到 650 字符）。
//! 断线重连：优先 Resume（补发漏掉的事件），Invalid Session(op9) 回退重新 Identify；
//! config.sandbox = "true" 时走沙箱 openapi（sandbox.api.sgroup.qq.com）。
//! 平台限制：被动消息正文以文本为主（群/单聊 markdown 已开放到所有机器人，
//! 审批富消息走 markdown，其余正文仍文本）——媒体产物以链接注记降级、
//! 无 typing 指示（见 platform::caps）；被动回复每条入站消息限 ~5 次
//! （群 5 次/5 分钟），回帖段数相应收紧。
//! 入站附件按扩展名归 image/file，落 inbox 注入（官方 bot 无语音消息形态）。
//! 监督循环每 5 秒对账：新增账号拉起会话，删除/停用/凭证变更的账号回收任务。

use crate::channel_util::{builtin_command, first_seen, media_placeholder};
use crate::platform::{
    admit, report_status, spawn_reply_with_approval, ApprovalPrompt, Channel, ChannelRun,
    GateDecision, InboundCtx, MediaItem,
};
use exm_core::Core;
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{atomic::AtomicU64, atomic::Ordering, Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

const TOKEN_HOST: &str = "https://api.bot.qq.com";
const OPENAPI: &str = "https://api.bot.qq.com";
const OPENAPI_SANDBOX: &str = "https://sandbox.api.sgroup.qq.com";
const INTENTS: i64 = (1 << 25) | (1 << 30);

struct Sess {
    handle: JoinHandle<()>,
    /// 凭证指纹（appId+secret+sandbox）：变更即重开会话
    fingerprint: String,
}

fn sessions() -> &'static Mutex<HashMap<String, Sess>> {
    static SESS: OnceLock<Mutex<HashMap<String, Sess>>> = OnceLock::new();
    SESS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn fingerprint(ch: &Channel) -> String {
    format!(
        "{}|{}|{}",
        ch.cfg("appId").unwrap_or_default(),
        ch.cfg("appSecret").unwrap_or_default(),
        ch.cfg("sandbox").unwrap_or_default()
    )
}

pub fn spawn_supervisor(core: Arc<Core>) {
    tokio::spawn(async move {
        loop {
            reconcile(&core).await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
}

async fn reconcile(core: &Arc<Core>) {
    let desired: Vec<Channel> = crate::platform::load_channels(core)
        .into_iter()
        .filter(|c| {
            c.kind == "qqbot"
                && c.enabled
                && c.cfg("appId").is_some()
                && c.cfg("appSecret").is_some()
        })
        .collect();

    let mut guards = sessions().lock();
    let stale: Vec<String> = guards
        .iter()
        .filter(|(id, s)| match desired.iter().find(|d| &d.id == *id) {
            Some(d) => s.fingerprint != fingerprint(d) || s.handle.is_finished(),
            None => true,
        })
        .map(|(id, _)| id.clone())
        .collect();
    for id in &stale {
        if let Some(s) = guards.remove(id) {
            s.handle.abort();
        }
    }
    for ch in &desired {
        if guards.contains_key(&ch.id) {
            continue;
        }
        let core = core.clone();
        let ch_task = ch.clone();
        let fp = fingerprint(ch);
        let handle = tokio::spawn(async move { session_loop(core, ch_task).await });
        guards.insert(
            ch.id.clone(),
            Sess {
                handle,
                fingerprint: fp,
            },
        );
    }
}

// ---------------------------------------------------------------- 凭证

struct BotCreds {
    app_id: String,
    secret: String,
    client: reqwest::Client,
    /// (access_token, 过期时刻)：提前 5 分钟判失效
    cache: Mutex<Option<(String, Instant)>>,
}

impl BotCreds {
    async fn token(&self) -> Option<String> {
        {
            let g = self.cache.lock();
            if let Some((t, exp)) = &*g {
                if *exp > Instant::now() {
                    return Some(t.clone());
                }
            }
        }
        let resp: Value = self
            .client
            .post(format!("{TOKEN_HOST}/app/getAppAccessToken"))
            .json(&json!({ "appId": self.app_id, "clientSecret": self.secret }))
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        let token = resp.get("access_token")?.as_str()?.to_string();
        let ttl: u64 = resp
            .get("expires_in")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .unwrap_or(7200);
        let exp = Instant::now() + Duration::from_secs(ttl.saturating_sub(300));
        *self.cache.lock() = Some((token.clone(), exp));
        Some(token)
    }
}

// ---------------------------------------------------------------- 会话循环

async fn session_loop(core: Arc<Core>, ch: Channel) {
    let Some(app_id) = ch.cfg("appId") else {
        return;
    };
    let Some(secret) = ch.cfg("appSecret") else {
        return;
    };
    let sandbox = ch.cfg("sandbox").as_deref() == Some("true");
    let base = if sandbox { OPENAPI_SANDBOX } else { OPENAPI };
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
    else {
        eprintln!("[qqbot:{}] HTTP 客户端构建失败", ch.id);
        return;
    };
    let creds = Arc::new(BotCreds {
        app_id,
        secret,
        client: client.clone(),
        cache: Mutex::new(None),
    });
    // (session_id, seq)：断线优先 Resume 补发
    let mut session: Option<(String, i64)> = None;
    // 断线退避：连续失败按 3→6→12…（封顶 300s）指数退避；成功连结（READY/RESUMED）即复位。
    // 否则凭证长期失效时会每 3 秒敲一次网关，日志与对端都不好看。
    let mut fail_streak: u32 = 0;

    loop {
        let Some(token) = creds.token().await else {
            let msg = "access_token 获取失败（检查 appId/appSecret）";
            eprintln!("[qqbot:{}] {msg}，30s 后重试", ch.id);
            report_status(&ch.id, "error", msg);
            tokio::time::sleep(Duration::from_secs(30)).await;
            continue;
        };
        // 网关地址
        let gw = match client
            .get(format!("{base}/gateway"))
            .header("Authorization", format!("QQBot {token}"))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => match r.json::<Value>().await {
                Ok(v) => v.get("url").and_then(|u| u.as_str()).map(|u| u.to_string()),
                _ => None,
            },
            Ok(r) => {
                let msg = format!("网关接口 HTTP {}（token 或权限问题）", r.status());
                eprintln!("[qqbot:{}] {msg}", ch.id);
                report_status(&ch.id, "error", msg);
                tokio::time::sleep(Duration::from_secs(30)).await;
                continue;
            }
            Err(e) => {
                let msg = format!("网关接口网络错误：{e}");
                eprintln!("[qqbot:{}] {msg}", ch.id);
                report_status(&ch.id, "error", msg);
                tokio::time::sleep(Duration::from_secs(15)).await;
                continue;
            }
        };
        let Some(gw) = gw else {
            let msg = "网关响应缺少 url";
            eprintln!("[qqbot:{}] {msg}", ch.id);
            report_status(&ch.id, "error", msg);
            tokio::time::sleep(Duration::from_secs(15)).await;
            continue;
        };
        let (ws, _) = match connect_async(&gw).await {
            Ok(x) => x,
            Err(e) => {
                let msg = format!("WS 连接失败：{e}");
                eprintln!("[qqbot:{}] {msg}（{gw}）", ch.id);
                report_status(&ch.id, "error", msg);
                tokio::time::sleep(Duration::from_secs(10)).await;
                continue;
            }
        };
        let (mut write, mut read) = ws.split();

        // Hello（op10）→ 心跳周期
        let hb_ms = match read.next().await {
            Some(Ok(Message::Text(s))) => serde_json::from_str::<Value>(&s)
                .ok()
                .and_then(|v| v.pointer("/d/heartbeat_interval").and_then(|x| x.as_u64()))
                .unwrap_or(45_000),
            _ => 45_000,
        };
        let hb_ms = hb_ms.saturating_sub(3_000).max(5_000);

        // 鉴权：有 session 走 Resume 补发，否则 Identify
        let auth = match &session {
            Some((sid, seq)) => {
                json!({ "op": 6, "d": { "token": format!("QQBot {token}"), "session_id": sid, "seq": seq } })
            }
            None => {
                json!({ "op": 2, "d": { "token": format!("QQBot {token}"), "intents": INTENTS, "shard": [0, 1] } })
            }
        };
        if write.send(Message::text(auth.to_string())).await.is_err() {
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }

        let mut hb = tokio::time::interval(Duration::from_millis(hb_ms));
        hb.tick().await; // interval 首跳立即触发，消耗掉
        loop {
            tokio::select! {
                maybe = read.next() => match maybe {
                    Some(Ok(Message::Text(s))) => {
                        let Ok(v) = serde_json::from_str::<Value>(&s) else { continue };
                        match v.get("op").and_then(|x| x.as_i64()) {
                            Some(11) => {} // 心跳 ACK
                            Some(1) => { // 服务端要求立即心跳
                                let d = session.as_ref().map(|(_, s)| json!(s)).unwrap_or(Value::Null);
                                let _ = write.send(Message::text(json!({ "op": 1, "d": d }).to_string())).await;
                            }
                            Some(9) => { // Invalid Session：凭证或 intents 有误，重新 Identify
                                let msg = "鉴权失效（检查 appId/appSecret 与机器人权限）";
                                eprintln!("[qqbot:{}] {msg}，将重新 Identify", ch.id);
                                report_status(&ch.id, "error", msg);
                                session = None;
                                break;
                            }
                            Some(7) => break, // 服务端要求重连：保留 session 走 Resume
                            Some(0) => {
                                let seq = v.get("s").and_then(|x| x.as_i64());
                                if let (Some(n), Some(sess)) = (seq, session.as_mut()) {
                                    sess.1 = n;
                                }
                                let t = v.get("t").and_then(|x| x.as_str()).unwrap_or("");
                                let d = v.get("d").cloned().unwrap_or(Value::Null);
                                match t {
                                    "READY" => {
                                        let sid = d.get("session_id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                                        session = Some((sid, seq.unwrap_or(0)));
                                        let user = d.pointer("/user/username").and_then(|x| x.as_str()).unwrap_or("?");
                                        println!("[qqbot:{}] 机器人已连结：{user}", ch.id);
                                        report_status(&ch.id, "ok", format!("机器人 {user}"));
                                        fail_streak = 0;
                                    }
                                    "RESUMED" => {
                                        report_status(&ch.id, "ok", "会话已恢复");
                                        fail_streak = 0;
                                    }
                                    "GROUP_AT_MESSAGE_CREATE" | "C2C_MESSAGE_CREATE" | "AT_MESSAGE_CREATE" => {
                                        let Some((peer, msg_id, content)) = parse_message(t, &d) else { continue };
                                        // 消息去重：断线 Resume 会重放最近事件，按平台消息 id 首见放行。
                                        // 放在最前——重放期间不能重复下载附件，更不能重复执行一轮对话。
                                        if !first_seen(&format!("qqbot:{}", ch.id), &msg_id) {
                                            continue;
                                        }
                                        // 入站媒体（组 6.4）：attachments 下载
                                        let mut inbound: Vec<(String, String, String, Vec<u8>)> = Vec::new();
                                        if let Some(atts) = d.get("attachments").and_then(|x| x.as_array()) {
                                            for att in atts {
                                                let url = att.get("url").and_then(|x| x.as_str()).unwrap_or("");
                                                let name = att.get("filename").and_then(|x| x.as_str()).unwrap_or("attachment.bin");
                                                if url.is_empty() {
                                                    continue;
                                                }
                                                let full = if url.starts_with("http") { url.to_string() } else { format!("https://multimedia.qq.com{url}") };
                                                let kind = if is_image_name(name) { "image" } else { "file" };
                                                if let Some(bytes) = crate::platform::download_bytes(&full).await {
                                                    inbound.push((kind.into(), name.into(), peer.key().to_string(), bytes));
                                                }
                                            }
                                        }
                                        // 纯媒体消息（官方平台发图常无 caption）：占位正文兜底，
                                        // 曾直接空内容丢弃——用户「只发一张图」会得不到任何响应
                                        let kinds: Vec<&str> = inbound.iter().map(|(k, _, _, _)| k.as_str()).collect();
                                        let mut text = if content.trim().is_empty() { media_placeholder(&kinds) } else { content.trim().to_string() };
                                        if text.trim().is_empty() && inbound.is_empty() {
                                            continue;
                                        }
                                        if !ch.allowed_chats.is_empty()
                                            && !ch.allowed_chats.iter().any(|a| a == peer.key())
                                        {
                                            eprintln!("[qqbot:{}] {} 不在白名单，已忽略", ch.id, peer.key());
                                            continue;
                                        }
                                        // 闸门：官方 bot 群聊/频道本就 @ 驱动（mentioned 恒真）；C2C 私聊豁免门控
                                        let is_group = !matches!(peer, Peer::C2C(_));
                                        let external_id =
                                            d.pointer("/author/id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                                        // 群聊发送者标记（消息头显示 + 模型上下文区分发言人）：
                                        // qqbot 载荷只有 author.username，事件上无 display 可言
                                        let sender_mark = crate::channel_util::sender_origin(
                                            is_group,
                                            d.pointer("/author/username")
                                                .and_then(|x| x.as_str())
                                                .unwrap_or(""),
                                            &external_id,
                                        );
                                        let gate_ctx = InboundCtx {
                                            text: &text,
                                            external_id: &external_id,
                                            display_name: &external_id,
                                            is_group,
                                            mentioned: true,
                                            chat_key: peer.key(),
                                            flow_name: None,
                                        };
                                        match admit(core.as_ref(), &ch, &gate_ctx).await {
                                            GateDecision::Allow(clean) => {
                                                text = clean;
                                            }
                                            GateDecision::Ignore => continue,
                                            GateDecision::Deny(reply) => {
                                                let creds2 = creds.clone();
                                                let peer2 = peer.clone();
                                                let msg_id2 = msg_id.clone();
                                                let seq2 = Arc::new(AtomicU64::new(0));
                                                tokio::spawn(async move {
                                                    send_passive(&creds2, base, &peer2, &msg_id2, &seq2, &reply).await;
                                                });
                                                continue;
                                            }
                                        }
                                        // 内置命令（/new /status）：闸门放行后、进入会话执行前拦截
                                        // ——未绑定 / 被拒用户已被闸门挡住，这里只服务合法用户
                                        if let Some(reply) = builtin_command(&core, &ch, peer.key(), &text).await {
                                            let creds2 = creds.clone();
                                            let peer2 = peer.clone();
                                            let msg_id2 = msg_id.clone();
                                            let seq2 = Arc::new(AtomicU64::new(0));
                                            tokio::spawn(async move {
                                                send_passive(&creds2, base, &peer2, &msg_id2, &seq2, &reply).await;
                                            });
                                            continue;
                                        }
                                        // 审批/计划快捷指令直裁（approval-ergonomics）：在 begin/回帖
                                        // 任务之前处理，避免原轮次审批等待中新建回帖任务造成重复
                                        // 投递与最终答复丢失（按钮点击产生的 ok:/no: 也走这里）。
                                        // 裁决可能执行长命令：整体 spawn，不阻塞 WS 事件循环。
                                        if crate::platform::is_quick_directive(&text) {
                                            let core2 = core.clone();
                                            let ch2 = ch.clone();
                                            let peer2 = peer.clone();
                                            let external_id2 = external_id.clone();
                                            let text2 = text.clone();
                                            let creds2 = creds.clone();
                                            let msg_id2 = msg_id.clone();
                                            tokio::spawn(async move {
                                                let sid = core2.flow_active_session(&ch2.id, peer2.key());
                                                let reply = crate::platform::quick_directive_reply(
                                                    &core2,
                                                    sid.as_deref(),
                                                    Some(&external_id2),
                                                    &text2,
                                                )
                                                .await
                                                .unwrap_or_else(|| {
                                                    "【报告】当前无待审批计划或审批单。".into()
                                                });
                                                send_passive(&creds2, base, &peer2, &msg_id2, &Arc::new(AtomicU64::new(0)), &reply)
                                                    .await;
                                            });
                                            continue;
                                        }
                                        let core = core.clone();
                                        let ch2 = ch.clone();
                                        let creds2 = creds.clone();
                                        tokio::spawn(async move {
                                            let mut saved: Vec<(String, String, String)> = Vec::new();
                                            for (kind, name, _ck, bytes) in inbound {
                                                if let Some(p) = crate::platform::save_inbound_media(&core, &ch2, &name, bytes).await {
                                                    saved.push((kind, p, name));
                                                }
                                            }
                                            handle_message(&core, &ch2, &creds2, base, peer, &external_id, saved, msg_id, &text, sender_mark.as_deref()).await;
                                        });
                                    }
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        let msg = format!("WS 错误：{e}");
                        eprintln!("[qqbot:{}] {msg}", ch.id);
                        report_status(&ch.id, "error", msg);
                        break;
                    }
                    None => {
                        eprintln!("[qqbot:{}] WS 断开", ch.id);
                        report_status(&ch.id, "error", "连接断开");
                        break;
                    }
                },
                _ = hb.tick() => {
                    let d = session.as_ref().map(|(_, s)| json!(s)).unwrap_or(Value::Null);
                    if write.send(Message::text(json!({ "op": 1, "d": d }).to_string())).await.is_err() {
                        break;
                    }
                }
            }
        }
        // 断线退避：指数递增（3→6→12…封顶 300s）；连结成功时已在 READY/RESUMED 处复位
        let wait = Duration::from_secs((3u64 << fail_streak.min(7)).min(300));
        fail_streak = fail_streak.saturating_add(1);
        tokio::time::sleep(wait).await;
    }
}

/// 消息事件 → (会话端点, msg_id, 正文)。群/单聊走 openid，公域频道走 channel_id。
fn parse_message(t: &str, d: &Value) -> Option<(Peer, String, String)> {
    let msg_id = d.get("id").and_then(|x| x.as_str())?.to_string();
    let content = d.get("content").and_then(|x| x.as_str()).unwrap_or("");
    match t {
        "GROUP_AT_MESSAGE_CREATE" => {
            let g = d
                .get("group_openid")
                .or_else(|| d.get("group_id"))
                .or_else(|| d.get("groupid"))
                .and_then(|x| x.as_str())?
                .to_string();
            Some((Peer::Group(g), msg_id, strip_mentions(content)))
        }
        "C2C_MESSAGE_CREATE" => {
            let u = d
                .get("user_openid")
                .or_else(|| d.pointer("/author/user_openid"))
                .and_then(|x| x.as_str())?
                .to_string();
            Some((Peer::C2C(u), msg_id, strip_mentions(content)))
        }
        "AT_MESSAGE_CREATE" => {
            let c = d.get("channel_id").and_then(|x| x.as_str())?.to_string();
            Some((Peer::Guild(c), msg_id, strip_mentions(content)))
        }
        _ => None,
    }
}

#[derive(Debug, Clone)]
enum Peer {
    /// 群（group_openid）
    Group(String),
    /// 单聊（user_openid）
    C2C(String),
    /// 公域频道（channel_id）
    Guild(String),
}

impl Peer {
    fn key(&self) -> &str {
        match self {
            Peer::Group(s) | Peer::C2C(s) | Peer::Guild(s) => s,
        }
    }
    fn max_chars(&self) -> usize {
        match self {
            // 群/单聊文本限 2000 字节，CJK 下 650 字符稳妥
            Peer::Guild(_) => 3800,
            _ => 650,
        }
    }
}

/// 附件文件名 → 是否图片（纯函数）：按最后一个 '.' 的扩展名（小写）比对常见位图后缀。
/// 曾用 contains(".png") 判定——「a.png.txt」会误判成图片，且漏掉 gif/webp/bmp。
fn is_image_name(name: &str) -> bool {
    matches!(
        name.to_lowercase().rsplit('.').next(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp")
    )
}

/// 频道 @ 消息正文剥掉 `<@…>` 提及片段；未闭合片段保留原样（曾把前缀重复拼一遍）
fn strip_mentions(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("<@") {
        out.push_str(&rest[..i]);
        match rest[i..].find('>') {
            Some(j) => rest = &rest[i + j + 1..],
            None => {
                // 找不到收尾 > ：把标记连同后面正文原样保留，宁可带杂质也不吞正文
                out.push_str(&rest[i..]);
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// 一条用户消息：组绑定 → 会话复用 → 订阅回复 → 执行 → 被动回复
#[allow(clippy::too_many_arguments)]
// 平台适配器的消息上下文逐项透传
async fn handle_message(
    core: &Arc<Core>,
    ch: &Channel,
    creds: &Arc<BotCreds>,
    base: &str,
    peer: Peer,
    external_id: &str,
    media: Vec<(String, String, String)>,
    msg_id: String,
    text: &str,
    sender_mark: Option<&str>,
) {
    let Some((run, rx)) = ChannelRun::begin(core, ch, peer.key()).await else {
        return;
    };
    core.stamp_session_origin(
        &run.session_id,
        external_id,
        core.identity_of(&ch.id, external_id)
            .map(|i| i.id)
            .unwrap_or_else(|| format!("ch:{}:{}", ch.id, external_id))
            .as_str(),
    );
    // 入站媒体注入（组 6.4）
    let note = crate::platform::stage_inbound_media(core, &run.session_id, &media);
    let text = if note.is_empty() {
        text.to_string()
    } else {
        format!("{text}{note}")
    };
    // 回复任务：被动回复须带原消息 msg_id + 递增 msg_seq
    let seq = Arc::new(AtomicU64::new(0));
    let max_chars = peer.max_chars();
    let creds = creds.clone();
    let base = base.to_string();
    // 审批富消息（approval-ergonomics × QQ 官方机器人）：markdown + 内嵌键盘，
    // 按钮为「指令按钮」——点击等价于用户发出 ok:/no: 文本；平台不支持时逐级回落
    //（markdown 被拒 → 文本+键盘 → 纯文本），按钮尽量保住。
    let ap_creds = creds.clone();
    let ap_base = base.clone();
    let ap_peer = peer.clone();
    let ap_msg_id = msg_id.clone();
    let ap_seq = seq.clone();
    let approval_send: crate::platform::channel_runtime::ApprovalHook = Box::new(
        move |prompt: ApprovalPrompt| -> std::pin::Pin<
            Box<dyn std::future::Future<Output = bool> + Send>,
        > {
            let creds = ap_creds.clone();
            let base = ap_base.clone();
            let peer = ap_peer.clone();
            let msg_id = ap_msg_id.clone();
            let seq = ap_seq.clone();
            Box::pin(async move {
                send_passive_approval(&creds, &base, &peer, &msg_id, seq.as_ref(), &prompt).await
            })
        },
    );
    let reply = spawn_reply_with_approval(
        &run,
        rx,
        max_chars,
        move |text| {
            let creds = creds.clone();
            let base = base.clone();
            let peer = peer.clone();
            let msg_id = msg_id.clone();
            let seq = seq.clone();
            async move {
                send_passive(&creds, &base, &peer, &msg_id, seq.as_ref(), &text).await;
            }
        },
        move |_item: MediaItem| async { false },
        Some(approval_send),
    );
    // 群聊发送者标记 → 消息 agent_id（消息头显示 + 模型上下文区分发言人）
    let run = if let Some(mark) = sender_mark {
        run.with_origin(mark)
    } else {
        run
    }
    .with_external_id(external_id);
    run.run(&text).await;
    let _ = reply.await;
}

/// 被动消息 POST（返回是否成功）：普通文本与审批富消息共用。
async fn post_message(creds: &BotCreds, base: &str, path: &str, body: &Value) -> bool {
    let Some(token) = creds.token().await else {
        eprintln!("[qqbot] 回复失败：access_token 获取失败");
        return false;
    };
    match creds
        .client
        .post(format!("{base}{path}"))
        .header("Authorization", format!("QQBot {token}"))
        .json(body)
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => true,
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            eprintln!("[qqbot] 回复失败：HTTP {status} {body}");
            false
        }
        Err(e) => {
            eprintln!("[qqbot] 回复网络错误：{e}");
            false
        }
    }
}

/// 被动回复（群/单聊/频道端点各异）。群/单聊正文含 markdown 语法时优先
/// msg_type=2 原生渲染（2026-04 起开放到所有机器人，无需开关；与主流
/// 框架的默认行为一致），被平台拒收（内容审核/权限）时换新 msg_seq
/// 回落 msg_type=0 文本——失败请求未送达不占被动回复额度。
/// 频道（Guild）markdown 仍需内邀，恒走文本。
async fn send_passive(
    creds: &BotCreds,
    base: &str,
    peer: &Peer,
    msg_id: &str,
    seq: &AtomicU64,
    content: &str,
) {
    let next = || seq.fetch_add(1, Ordering::Relaxed) + 1;
    match peer {
        Peer::Group(g) => {
            let path = format!("/v2/groups/{g}/messages");
            if has_markdown_syntax(content) {
                let body = json!({
                    "msg_type": 2,
                    "markdown": { "content": content },
                    "msg_id": msg_id,
                    "msg_seq": next(),
                });
                if post_message(creds, base, &path, &body).await {
                    return;
                }
            }
            let body = json!({
                "content": content,
                "msg_type": 0,
                "msg_id": msg_id,
                "msg_seq": next(),
            });
            let _ = post_message(creds, base, &path, &body).await;
        }
        Peer::C2C(u) => {
            let path = format!("/v2/users/{u}/messages");
            if has_markdown_syntax(content) {
                let body = json!({
                    "msg_type": 2,
                    "markdown": { "content": content },
                    "msg_id": msg_id,
                    "msg_seq": next(),
                });
                if post_message(creds, base, &path, &body).await {
                    return;
                }
            }
            let body = json!({
                "content": content,
                "msg_type": 0,
                "msg_id": msg_id,
                "msg_seq": next(),
            });
            let _ = post_message(creds, base, &path, &body).await;
        }
        Peer::Guild(c) => {
            let path = format!("/channels/{c}/messages");
            let body = json!({ "content": content, "msg_id": msg_id, "msg_seq": next() });
            let _ = post_message(creds, base, &path, &body).await;
        }
    }
}

/// 正文是否含值得原生渲染的 markdown 语法（纯函数，保守判定）：
/// 只有确实出现结构标记才走 msg_type=2——普通闲聊不碰 markdown 通路
/// （规避内容审核的不确定性），误判代价只是符号照原样显示。
fn has_markdown_syntax(text: &str) -> bool {
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("```") {
            return true; // 代码块（开闭都算，出现即受益）
        }
        if t.starts_with('#')
            || t.starts_with("> ")
            || t.starts_with("- ")
            || t.starts_with("* ")
            || t.starts_with("1. ")
        {
            return true;
        }
        if line.contains("**") || (line.contains("`") && !t.starts_with('`')) {
            return true;
        }
    }
    false
}

/// 审批消息富发送（approval-ergonomics × QQ 官方机器人）——三级回落：
/// ① msg_type=2 markdown + 内嵌键盘：markdown 渲染 + 按钮（2026-04 起
///    群/单聊自定义 markdown 已开放到所有机器人，无需申请模板）；
/// ② msg_type=0 纯文本 + 键盘：markdown 被拒（内容审核 50056 / 语法
///    拒收）时按钮仍可用——键盘不依赖 markdown 权限；
/// ③ 返回 false 由调用方回落分段纯文本（键盘也被拒的保底）。
/// 按钮用「指令按钮」（type=2）而非回调按钮——无需 INTERACTION intent
/// 与 3 秒回调响应，点击等价于用户发出 ok:/no: 文本：单聊 enter=true
/// 自动发送；群聊填入输入框（带 @机器人）后由用户发送。
/// 每次尝试递增 msg_seq（失败请求也可能已消耗序号，重试必须换号）。
async fn send_passive_approval(
    creds: &BotCreds,
    base: &str,
    peer: &Peer,
    msg_id: &str,
    seq: &std::sync::atomic::AtomicU64,
    prompt: &ApprovalPrompt,
) -> bool {
    // 频道消息 API 的键盘形态与群/单聊不同，保守回落纯文本。
    let path = match peer {
        Peer::Group(g) => format!("/v2/groups/{g}/messages"),
        Peer::C2C(u) => format!("/v2/users/{u}/messages"),
        Peer::Guild(_) => return false,
    };
    // ① markdown + 键盘
    let body_md = approval_body_markdown(prompt, msg_id, seq.fetch_add(1, Ordering::Relaxed) + 1);
    if post_message(creds, base, &path, &body_md).await {
        return true;
    }
    // ② 纯文本 + 键盘：正文保持原文（无 markdown 修饰），指令指引保留
    let body_kb = approval_body_text(prompt, msg_id, seq.fetch_add(1, Ordering::Relaxed) + 1);
    post_message(creds, base, &path, &body_kb).await
    // ③ 两级都失败 → false，调用方走分段纯文本回落（不在这里重试纯文本，
    //    避免与调用方回落叠加造成同内容重复投递）。
}

/// 审批富消息请求体（纯函数，便于测试）——首选形态：markdown + 键盘。
fn approval_body_markdown(prompt: &ApprovalPrompt, msg_id: &str, msg_seq: u64) -> Value {
    json!({
        "msg_type": 2,
        "markdown": { "content": approval_markdown(&prompt.text) },
        "keyboard": approval_keyboard(&prompt.approval_id),
        "msg_id": msg_id,
        "msg_seq": msg_seq,
    })
}

/// 审批富消息请求体（纯函数，便于测试）——回落形态：纯文本 + 键盘。
/// markdown 被平台拒收（权限 50056 / 内容审核）时按钮仍可用。
fn approval_body_text(prompt: &ApprovalPrompt, msg_id: &str, msg_seq: u64) -> Value {
    json!({
        "msg_type": 0,
        "content": prompt.text,
        "keyboard": approval_keyboard(&prompt.approval_id),
        "msg_id": msg_id,
        "msg_seq": msg_seq,
    })
}

/// 审批 markdown 正文：首行升为标题，保留摘要原文，尾部补按钮操作提示。
fn approval_markdown(text: &str) -> String {
    let mut lines = text.trim().lines();
    let head = lines.next().unwrap_or("⛔ 审批请求");
    let mut out = format!("## {head}");
    for line in lines {
        out.push('\n');
        out.push_str(line);
    }
    out.push_str("\n\n点击下方按钮可直接裁决（群聊点击后发送即可）");
    out
}

/// 审批内嵌键盘（纯函数，便于测试）：批准/拒绝两个指令按钮。
/// data 即快捷裁决指令（`ok:<单号>` / `no:<单号>`），权限由通道侧
/// `parse_approval_directive` 的 origin/管理员判定兜底。
fn approval_keyboard(approval_id: &str) -> Value {
    let button = |id: &str, label: &str, visited: &str, style: i64, data: String, tip: String| {
        json!({
            "id": id,
            "render_data": {
                "label": label,
                "visited_label": visited,
                "style": style,
            },
            "action": {
                "type": 2,
                "permission": { "type": 2 },
                "data": data,
                "enter": true,
                "unsupport_tips": tip,
            },
        })
    };
    json!({
        "content": {
            "rows": [{
                "buttons": [
                    button(
                        "approve",
                        "批准",
                        "已批准",
                        1,
                        format!("ok:{approval_id}"),
                        format!("请回复 ok:{approval_id} 批准"),
                    ),
                    button(
                        "deny",
                        "拒绝",
                        "已拒绝",
                        0,
                        format!("no:{approval_id}"),
                        format!("请回复 no:{approval_id} 拒绝"),
                    ),
                ],
            }],
        },
    })
}

// ---------------------------------------------------------------- 测试（纯函数部分）

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 消息解析：群 / 单聊 / 公域频道三种事件形态各自取到正确的会话端点与正文
    #[test]
    fn 消息解析_群单聊频道三形态() {
        // 群聊：group_openid 优先
        let (peer, msg_id, content) = parse_message(
            "GROUP_AT_MESSAGE_CREATE",
            &json!({ "id": "m1", "group_openid": "G1", "content": "<@!bot>在吗" }),
        )
        .expect("群事件应可解析");
        assert_eq!(peer.key(), "G1");
        assert!(!matches!(peer, Peer::C2C(_)), "群聊不是私聊");
        assert_eq!(msg_id, "m1");
        assert_eq!(content, "在吗", "群 @ 消息应剥掉提及片段");

        // 单聊：user_openid；缺 author.user_openid 时兜底指针
        let (peer, _, content) = parse_message(
            "C2C_MESSAGE_CREATE",
            &json!({ "id": "m2", "user_openid": "U9", "content": "帮我查天气" }),
        )
        .expect("单聊事件应可解析");
        assert!(
            matches!(peer, Peer::C2C(_)),
            "C2C 应识别为私聊（门控豁免依据）"
        );
        assert_eq!(content, "帮我查天气");

        // 公域频道：channel_id
        let (peer, _, _) = parse_message(
            "AT_MESSAGE_CREATE",
            &json!({ "id": "m3", "channel_id": "CH1", "content": "hi" }),
        )
        .expect("频道事件应可解析");
        assert!(matches!(peer, Peer::Guild(_)));

        // 缺 msg_id / 缺端点 → 不可路由，直接放弃
        assert!(parse_message("GROUP_AT_MESSAGE_CREATE", &json!({ "content": "x" })).is_none());
        assert!(parse_message("GROUP_AT_MESSAGE_CREATE", &json!({ "id": "m4" })).is_none());
        // 未知事件类型不解析
        assert!(parse_message("DIRECT_MESSAGE_CREATE", &json!({ "id": "m5" })).is_none());
    }

    /// 提及剥离：完整片段剥掉、未闭合片段保持原样（宁可带杂质也不吞正文）
    #[test]
    fn 提及剥离_未闭合保持原样() {
        assert_eq!(strip_mentions("<@!abc123>在吗"), "在吗");
        assert_eq!(strip_mentions("前缀<@x>中缀<@y>后缀"), "前缀中缀后缀");
        assert_eq!(strip_mentions("没有提及"), "没有提及");
        assert_eq!(
            strip_mentions("未闭合<@abc"),
            "未闭合<@abc",
            "找不到 > 时不再吞后面正文"
        );
    }

    /// 回复上限：群/单聊文本限 2000 字节（CJK 按 650 字符收敛），公域频道走常规上限
    #[test]
    fn 回复上限_按端点收敛() {
        assert_eq!(Peer::Group("g".into()).max_chars(), 650);
        assert_eq!(Peer::C2C("u".into()).max_chars(), 650);
        assert_eq!(Peer::Guild("c".into()).max_chars(), 3800);
    }

    /// 附件图片判定：按扩展名整词比对（大小写不敏感），假扩展名与无扩展名不算
    #[test]
    fn 图片文件名判定_扩展名整词() {
        assert!(is_image_name("shot.png"));
        assert!(is_image_name("IMG_0001.JPG"), "大小写不敏感");
        assert!(is_image_name("a.jpeg") && is_image_name("b.gif") && is_image_name("c.webp"));
        assert!(
            !is_image_name("a.png.txt"),
            "中间含 .png 不算图片（曾用 contains 误判）"
        );
        assert!(!is_image_name("report.pdf"));
        assert!(!is_image_name("noext"), "无扩展名不算图片");
    }

    /// 审批键盘（approval-ergonomics × QQ 官方机器人）：两个指令按钮，
    /// data 即 ok:/no: 裁决指令；用 type=2（指令按钮）避免 INTERACTION 回调依赖。
    #[test]
    fn 审批键盘_指令按钮带裁决指令() {
        let kb = approval_keyboard("abcd1234");
        let buttons = kb
            .pointer("/content/rows/0/buttons")
            .and_then(|v| v.as_array())
            .expect("键盘应含一行按钮");
        assert_eq!(buttons.len(), 2, "批准/拒绝两个按钮");
        let approve = &buttons[0];
        let deny = &buttons[1];
        assert_eq!(approve["action"]["type"], 2, "指令按钮（非回调按钮）");
        assert_eq!(approve["action"]["data"], "ok:abcd1234");
        assert_eq!(deny["action"]["data"], "no:abcd1234");
        assert_eq!(
            approve["action"]["permission"]["type"], 2,
            "所有人可见按钮；裁决权限由通道侧兜底"
        );
        assert_eq!(approve["action"]["enter"], true, "单聊点击自动发送");
        assert!(
            approve["action"].get("reply").is_none(),
            "官方 action 无 reply 字段，非标字段会导致整条消息 400 被拒"
        );
        assert_eq!(approve["render_data"]["label"], "批准");
        assert_eq!(deny["render_data"]["label"], "拒绝");
    }

    /// markdown 语法嗅探（纯函数）：结构标记（标题/列表/引用/代码块/粗体/
    /// 行内码）出现才走 msg_type=2；普通闲聊不误入 markdown 通路。
    #[test]
    fn markdown嗅探_保守判定() {
        assert!(has_markdown_syntax("## 报告\n正文"));
        assert!(has_markdown_syntax("- 第一项\n- 第二项"));
        assert!(has_markdown_syntax("1. 步骤"));
        assert!(has_markdown_syntax("> 引用"));
        assert!(has_markdown_syntax("重点**加粗**说明"));
        assert!(has_markdown_syntax("运行 `cargo test` 即可"));
        assert!(has_markdown_syntax("```rust\nfn main() {}\n```"));
        // 普通文本：不碰 markdown 通路（含「沉默」等无结构标记的闲聊）
        assert!(!has_markdown_syntax("已把任务跑完，结果见文件。"));
        assert!(!has_markdown_syntax("好的，我看看\n稍等"));
        // 纯反引号开头（未闭合代码块）也算——渲染收益优先
        assert!(has_markdown_syntax("```\nfn main() {}"));
    }

    /// 审批富消息三级回落的请求体形态（纯函数部分）：markdown 体带
    /// msg_type=2 + markdown.content；文本体带 msg_type=0 + content + keyboard。
    #[test]
    fn 审批富消息_回落链请求体() {
        let prompt = ApprovalPrompt {
            approval_id: "abcd1234".into(),
            text: "⛔ 审批请求（单号 abcd1234）：\necho hi".into(),
        };
        // 构造两级请求体的纯函数抽出（与 send_passive_approval 内联保持一致）
        let md = approval_body_markdown(&prompt, "m1", 1);
        assert_eq!(md["msg_type"], 2, "首选 markdown 形态");
        assert!(md["markdown"]["content"].as_str().unwrap().contains("## "));
        assert!(md["keyboard"]["content"]["rows"].is_array(), "带内嵌键盘");

        let plain = approval_body_text(&prompt, "m1", 2);
        assert_eq!(plain["msg_type"], 0, "回落形态为纯文本");
        assert!(plain["content"].as_str().unwrap().starts_with("⛔"), "正文为原文");
        assert!(plain["keyboard"]["content"]["rows"].is_array(), "键盘保留");
        assert!(plain.get("markdown").is_none(), "文本体不得携带 markdown 字段");
    }

    /// 审批 markdown：首行升标题、保留摘要原文、附按钮提示。
    #[test]
    fn 审批markdown_标题与操作提示() {
        let text = "⛔ 审批请求（单号 abcd1234）：\necho hi\n120 秒内回复 ok:abcd1234 批准 / no:abcd1234 拒绝，或到控制台裁决";
        let md = approval_markdown(text);
        assert!(md.starts_with("## ⛔ 审批请求"), "首行应为标题: {md}");
        assert!(md.contains("echo hi"), "应保留命令摘要: {md}");
        assert!(md.contains("ok:abcd1234"), "应保留文本裁决指引: {md}");
        assert!(md.contains("点击下方按钮"), "应提示按钮操作: {md}");
    }
}
