//! OneBot 11 平台适配器（napcat.rs）：适配 NapCat / Lagrange 等_onebot11_ 实现。
//! 接法：正向 WebSocket —— 连接 OneBot 实现的 WS 服务端（config.url，可选 config.token 访问令牌），
//! 收 `post_type=message` 事件 → 绑定组内执行 → `send_group_msg` / `send_private_msg` 回复。
//! 监督循环每 5 秒对账：新增账号拉起连接，删除/停用/地址或令牌变更的账号回收任务。
//!
//! 收发解耦：入站读取不阻塞（消息处理全部 spawn），动作经 mpsc 通道交给专职写任务，
//! 避免长运行期间无法应答 WS 层 Ping 被服务端断开。

use crate::channel_util::{builtin_command, first_seen, media_placeholder};
use crate::platform::{
    admit, report_status, spawn_reply, Channel, ChannelRun, GateDecision, InboundCtx, MediaItem,
};
use crate::ambient_media;
use exm_core::Core;
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use parking_lot::Mutex;
use std::sync::{Arc, OnceLock};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

struct Conn {
    handle: JoinHandle<()>,
    /// 连接指纹（url+token）：变更即重连
    fingerprint: String,
}

fn conns() -> &'static Mutex<HashMap<String, Conn>> {
    static CONNS: OnceLock<Mutex<HashMap<String, Conn>>> = OnceLock::new();
    CONNS.get_or_init(|| Mutex::new(HashMap::new()))
}

// ---------------------------------------------------------------- API 调用通道
// OneBot 11 正向 WS 复用事件连接发 API 请求（action + echo）→ 回包按 echo 匹配。
// 用途：引用消息回查（get_msg）、群文件下载 URL（get_group_file_url）等——
// 适配器此前只发不收 API 回包（echo 非自检一律忽略），需要回包的能力全部缺位。

/// 每连接的 API 回包等待表：echo → 回包投递口。写入方是 WS 读循环
///（on_text 识别带 echo 的回包），读取方是 api_call 的调用任务。
fn api_waiters() -> &'static Mutex<HashMap<String, mpsc::UnboundedSender<serde_json::Value>>> {
    static W: OnceLock<Mutex<HashMap<String, mpsc::UnboundedSender<serde_json::Value>>>> =
        OnceLock::new();
    W.get_or_init(|| Mutex::new(HashMap::new()))
}

/// echo 序号（api_call 生成唯一 echo，避免并发调用互串）
fn api_echo_seq() -> &'static std::sync::atomic::AtomicU64 {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    &N
}

/// 向指定连接发一条 OneBot API 并等待回包（超时 None）。
/// 连接不在线 / 发送失败同样 None——调用方按「能力不可用」降级，不阻塞消息主链。
pub(crate) async fn api_call(
    ch_id: &str,
    action: &str,
    params: serde_json::Value,
) -> Option<serde_json::Value> {
    let echo = format!("exm-api-{}", api_echo_seq().fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let (tx, mut rx) = mpsc::unbounded_channel();
    api_waiters().lock().insert(echo.clone(), tx);
    let sent = {
        let guards = conns().lock();
        guards.get(ch_id).map(|_| ()).is_some()
    };
    // 动作要经该连接的写通道发出：写通道随事件任务存续，这里经 action_tx 全局表投递。
    // （tx 保存在 conns 之外：napcat.rs 用静态表收当前各连接的动作发送端。）
    let ok = if sent {
        action_tx(ch_id)
            .and_then(|tx| {
                tx.send(serde_json::json!({
                    "action": action,
                    "params": params,
                    "echo": echo,
                }))
                .ok()
            })
            .is_some()
    } else {
        false
    };
    if !ok {
        api_waiters().lock().remove(&echo);
        return None;
    }
    let resp = tokio::time::timeout(std::time::Duration::from_secs(8), rx.recv()).await;
    api_waiters().lock().remove(&echo);
    match resp {
        Ok(Some(v)) => {
            // OneBot 回包：retcode == 1 常见于「异步执行」；0 = 同步成功。其余视为失败。
            let code = v.get("retcode").and_then(|x| x.as_i64()).unwrap_or(-1);
            (code == 0).then_some(v)
        }
        _ => None,
    }
}

/// 每连接的动作发送端（api_call 投递用）：连接建立时登记，断开时清除。
fn action_tx(ch_id: &str) -> Option<mpsc::UnboundedSender<serde_json::Value>> {
    action_txs().lock().get(ch_id).cloned()
}

fn action_txs() -> &'static Mutex<HashMap<String, mpsc::UnboundedSender<serde_json::Value>>> {
    static T: OnceLock<Mutex<HashMap<String, mpsc::UnboundedSender<serde_json::Value>>>> =
        OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

// ---------------- 事件管道自检（spec: agent-terminal-tools D6） ----------------
// NapCat 侧 bug：WS 连结但事件面不挂（@ 无响应）。连接状态无法反映事件管道健康，
// 用「最近事件时间」对账：连结后长时间零事件 → degraded 告警（限频每小时一条），
// 事件恢复 → 自动回 ok。首次连结前不告警（还没有基线）。

/// 每通道最近事件时间戳（ms；连结成功时初始化，post_type=message 事件刷新）
fn last_event_at() -> &'static Mutex<HashMap<String, u64>> {
    static T: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 每通道上次 degraded 告警时间（ms；限频用，避免夜间静默刷屏）
fn last_degraded_log() -> &'static Mutex<HashMap<String, u64>> {
    static T: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 每通道上次探测回包时间（ms）：连接内存活的连接发 get_login_info 能拿到回包
/// = 协议端口真活（区别于 WS 层活着但协议端假死——容器假活）。任一 API 回包续打点。
fn last_probe_ok() -> &'static Mutex<HashMap<String, u64>> {
    static T: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 每通道最近一次登录在线确认时间（ms；get_login_info 回包带身份时刷新）。
/// 登录轮换期（扫码换号/风控下线）WS 层仍连着、事件面自然零消息——
/// 这不是「事件面未挂载」，不该建议重启（5.4：自检误报）。
fn last_login_ok() -> &'static Mutex<HashMap<String, u64>> {
    static T: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 事件到达打点（on_text 收到 message 事件时调用）
fn note_event(ch_id: &str) {
    let now = exm_core::types::now_ms();
    let mut m = last_event_at().lock();
    let was_silent = m
        .get(ch_id)
        .map(|t| now.saturating_sub(*t) > 30 * 60 * 1000)
        .unwrap_or(false);
    m.insert(ch_id.to_string(), now);
    if was_silent {
        // 事件恢复：状态回 ok（此前若因零事件标过 degraded）
        report_status(ch_id, "ok", "事件管道已恢复（收到新消息事件）");
    }
}

/// 连结成功初始化基线（此后 30min 零事件才视为可疑）
fn note_connected(ch_id: &str) {
    last_event_at()
        .lock()
        .insert(ch_id.to_string(), exm_core::types::now_ms());
    last_degraded_log().lock().remove(ch_id);
    // 登录确认打点同步清：新连接的登录态未知，等 exm-login 回包重新确认
    last_login_ok().lock().remove(ch_id);
}

/// 连接回收时清打点（通道删除/停用/重建）
fn note_conn_reset(ch_id: &str) {
    last_event_at().lock().remove(ch_id);
    last_degraded_log().lock().remove(ch_id);
    last_login_ok().lock().remove(ch_id);
    last_probe_ok().lock().remove(ch_id);
}

/// supervisor 对账环的事件面检查：连结后 30min 零事件 → degraded（限频每小时一条日志）。
/// 首次连结前（无基线）与刚连结 30min 内不告警。
/// 登录态分流（5.4 误报修复）：最近 10min 内确认过登录在线 → 零事件大概率是
/// 「登录轮换期 / 无人说话」，报等待登录（info）而非事件面未挂载（degraded + 重启建议）。
fn check_event_silence(ch_id: &str) {
    let Some(t0) = last_event_at().lock().get(ch_id).copied() else {
        return; // 未连结过：无基线不告警
    };
    let now = exm_core::types::now_ms();
    if now.saturating_sub(t0) <= 30 * 60 * 1000 {
        return; // 窗口内：正常
    }
    // 登录态先验：近期确认在线 → 等待登录轮换完成（不降级、不建议重启）
    let login_recent = last_login_ok()
        .lock()
        .get(ch_id)
        .map(|t| now.saturating_sub(*t) <= 10 * 60 * 1000)
        .unwrap_or(false);
    if login_recent {
        report_status(
            ch_id,
            "ok",
            "连接正常、账号在线；近期无消息事件（可能登录轮换或无人发言）",
        );
        return;
    }
    // 假活分流（1.4）：5min 周期探测发出后 15min 内拿到过任何 API 回包 = 协议端口真活，
    // 问题定性为「账号未登录」（容器活着、协议端活着，等扫码即可，不建议重启容器）；
    // 探测也无回包 = 协议端口假死（容器假活），指引重启 NapCat 容器。
    let probe_recent = last_probe_ok()
        .lock()
        .get(ch_id)
        .map(|t| now.saturating_sub(*t) <= 15 * 60 * 1000)
        .unwrap_or(false);
    let mut logs = last_degraded_log().lock();
    if let Some(last) = logs.get(ch_id) {
        if now.saturating_sub(*last) < 60 * 60 * 1000 {
            return; // 一小时内已告警过：限频
        }
    }
    logs.insert(ch_id.to_string(), now);
    if probe_recent {
        eprintln!(
            "[napcat:{ch_id}] 自检：协议端口正常但账号未登录（探测有回包、登录确认缺失）。\
             请在 NapCat 侧完成扫码登录，无需重启容器"
        );
        report_status(
            ch_id,
            "degraded",
            "协议端口正常但账号未登录；请在 NapCat 完成扫码登录（无需重启容器）",
        );
        return;
    }
    eprintln!(
        "[napcat:{ch_id}] 事件管道自检：连结后 30 分钟零消息事件且探测无回包（NapCat 协议端口疑似假死）。\
         建议重启 NapCat 容器：docker restart napcat-exm"
    );
    report_status(
        ch_id,
        "degraded",
        "连接与探测均无响应（协议端口疑似假死）；建议重启 NapCat 容器",
    );
}

fn fingerprint(ch: &Channel) -> String {
    format!(
        "{}|{}",
        ch.cfg("url").unwrap_or_default(),
        ch.cfg("token").unwrap_or_default()
    )
}

pub fn spawn_supervisor(core: Arc<Core>) {
    tokio::spawn(async move {
        loop {
            reconcile(&core).await;
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });
}

async fn reconcile(core: &Arc<Core>) {
    // snowluma 同走本适配器：SnowLuma 是 OneBot 11 协议端（NapCat 同类替代），
    // 正向 WS 接法与事件/动作契约完全一致——只区分 kind，运行链路零分叉。
    let desired: Vec<Channel> = crate::platform::load_channels(core)
        .into_iter()
        .filter(|c| {
            matches!(c.kind.as_str(), "napcat" | "snowluma")
                && c.enabled
                && c.cfg("url").is_some()
        })
        .collect();

    let mut guards = conns().lock();
    let stale: Vec<String> = guards
        .iter()
        .filter(|(id, c)| match desired.iter().find(|d| &d.id == *id) {
            Some(d) => c.fingerprint != fingerprint(d) || c.handle.is_finished(),
            None => true,
        })
        .map(|(id, _)| id.clone())
        .collect();
    for id in &stale {
        if let Some(c) = guards.remove(id) {
            c.handle.abort();
            // 事件自检打点随连接回收（重建时 note_connected 重新建基线）
            note_conn_reset(id);
        }
    }
    for ch in &desired {
        if guards.contains_key(&ch.id) {
            // 事件管道自检：已连结通道的对账检查（零事件超窗 → degraded 限频告警）
            check_event_silence(&ch.id);
            continue;
        }
        let core = core.clone();
        let ch_task = ch.clone();
        let fp = fingerprint(ch);
        let handle = tokio::spawn(async move { connect_loop(core, ch_task).await });
        guards.insert(
            ch.id.clone(),
            Conn {
                handle,
                fingerprint: fp,
            },
        );
    }
}

async fn connect_loop(core: Arc<Core>, ch: Channel) {
    let url = ch.cfg("url").unwrap_or_default();
    let token = ch.cfg("token").unwrap_or_default();
    // 断线退避：连续失败按 5→10→20…（封顶 300s）指数递增；连接成功即复位。
    // NapCat 停机维护时不该每 5 秒重试一次敲日志。
    let mut fail_streak: u32 = 0;
    loop {
        // 握手请求：由 String 的 IntoRequest 生成规范 WS 头（tungstenite 0.21+ 不再自动补
        // Sec-WebSocket-Key 等 5 头，手动 Request::builder 缺头即握手失败），再叠加鉴权头。
        // token 为空则不带 Authorization。
        let request = match url.clone().into_client_request() {
            Ok(mut req) => {
                if !token.is_empty() {
                    req.headers_mut()
                        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
                }
                req.headers_mut()
                    .insert("X-Client-Role", "ExMachina-Gateway".parse().unwrap());
                req
            }
            Err(e) => {
                let msg = format!("地址非法（{url}）：{e}");
                eprintln!("[napcat:{}] {msg}", ch.id);
                report_status(&ch.id, "error", msg);
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                continue;
            }
        };
        let ws = match connect_async(request).await {
            Ok((ws, _)) => {
                println!("[napcat:{}] 已连结 {url}", ch.id);
                report_status(&ch.id, "ok", format!("已连结 {url}"));
                // 事件自检基线初始化：连结成功才有「零事件」的对账起点
                note_connected(&ch.id);
                // 复位退避：连上即视为恢复（连上后立刻断开属对端不稳，5s 基线重试可接受）
                fail_streak = 0;
                ws
            }
            Err(e) => {
                let msg = format!("连接失败：{e}");
                eprintln!("[napcat:{}] {msg}", ch.id);
                report_status(&ch.id, "error", msg);
                let wait = std::time::Duration::from_secs((5u64 << fail_streak.min(7)).min(300));
                fail_streak = fail_streak.saturating_add(1);
                tokio::time::sleep(wait).await;
                continue;
            }
        };

        let (mut write, mut read) = ws.split();
        // 动作通道：事件处理任务 → 专职写任务（保持读循环常开，WS 层 Ping 及时应答）
        let (tx, mut rx) = mpsc::unbounded_channel::<serde_json::Value>();
        // api_call 投递端登记（连接存活期内有效；断开清除——悬挂调用走超时降级）
        action_txs().lock().insert(ch.id.clone(), tx.clone());
        let writer = tokio::spawn(async move {
            while let Some(v) = rx.recv().await {
                if write.send(Message::text(v.to_string())).await.is_err() {
                    break;
                }
            }
        });
        // 自检：拿机器人身份（回包在读取循环里识别）
        let _ = tx.send(serde_json::json!({ "action": "get_login_info", "echo": "exm-login" }));

        // 登录态周期探测（5.4）：每 5min 重发 get_login_info——登录打点 10min 过期，
        // 不续发则「在线但无人说话」的超窗检查拿不到在线证据，误报重启建议
        let probe_tx = tx.clone();
        let probe = tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(5 * 60));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            tick.tick().await; // 首拍立即（interval 首拍是 now）——已被上面的首发覆盖，跳过
            loop {
                tick.tick().await;
                if probe_tx
                    .send(serde_json::json!({ "action": "get_login_info", "echo": "exm-login" }))
                    .is_err()
                {
                    break; // 连接已断（写任务退出）：探测随之收场
                }
            }
        });

        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(s)) => on_text(&core, &ch, &tx, &s).await,
                Ok(Message::Close(c)) => {
                    eprintln!("[napcat:{}] 服务端关闭：{c:?}", ch.id);
                    report_status(&ch.id, "error", "服务端关闭连接");
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    let msg = format!("连接错误：{e}");
                    eprintln!("[napcat:{}] {msg}", ch.id);
                    report_status(&ch.id, "error", msg);
                    break;
                }
            }
        }
        probe.abort();
        drop(tx);
        let _ = writer.await;
        // 断开即清除本连接的 api_call 投递端与未决等待（防悬挂 8s 超时）
        action_txs().lock().remove(&ch.id);
        api_waiters().lock().retain(|_, tx| !tx.is_closed());
        // 断线退避：指数递增（连接成功时已复位）
        let wait = std::time::Duration::from_secs((5u64 << fail_streak.min(7)).min(300));
        fail_streak = fail_streak.saturating_add(1);
        tokio::time::sleep(wait).await;
    }
}

async fn on_text(
    core: &Arc<Core>,
    ch: &Channel,
    tx: &mpsc::UnboundedSender<serde_json::Value>,
    s: &str,
) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(s) else {
        return;
    };
    // API 回包（带 echo 的响应）：自检回包打日志；exm-api-* 投递给 api_call 等待方
    //（引用回查 / 群文件 URL 等）；其余忽略
    if let Some(echo) = v.get("echo").and_then(|x| x.as_str()) {
        // 协议端口活性打点（1.4 假活判定）：连接内能拿到任何 API 回包
        // = 协议端口真活（WS 层活着但协议端假死时容器假活拿不到回包）
        last_probe_ok()
            .lock()
            .insert(ch.id.clone(), exm_core::types::now_ms());
        if echo.starts_with("exm-api-") {
            if let Some(tx) = api_waiters().lock().remove(echo) {
                let _ = tx.send(v);
            }
            return;
        }
        if echo == "exm-login" {
            let nick = v
                .pointer("/data/nickname")
                .and_then(|x| x.as_str())
                .unwrap_or("?");
            let uid = v
                .pointer("/data/user_id")
                .and_then(|x| x.as_i64())
                .unwrap_or(0);
            // 登录确认打点（5.4）：拿到身份 = 账号在线。零事件窗口的告警分流依据
            //（在线 → 等待登录轮换文案，而非「事件面未挂载建议重启」）
            let confirmed = uid != 0 && !nick.is_empty() && nick != "?";
            // 日志口径：仅首连（打点表中该通道尚无在线记录）打「已连结」全量日志；
            // 5min 周期探测的回包静默续打点——否则探测每次都刷屏连接日志（生产实测噪音）
            let first = !last_login_ok().lock().contains_key(&ch.id);
            if confirmed {
                last_login_ok()
                    .lock()
                    .insert(ch.id.clone(), exm_core::types::now_ms());
            }
            if confirmed && first {
                println!("[napcat:{}] 机器人已连结：{nick}（{uid}）", ch.id);
            }
            report_status(&ch.id, "ok", format!("机器人 {nick}（{uid}）"));
        }
        return;
    }
    let post = v.get("post_type").and_then(|x| x.as_str()).unwrap_or("");
    if post != "message" {
        return; // meta_event / request / notice 忽略
    }
    // 事件管道自检打点：message 事件到达 = 事件面活着（连结后零事件会 degraded）
    note_event(&ch.id);
    let group_id = v.get("group_id").and_then(|x| x.as_i64());
    let user_id = v.get("user_id").and_then(|x| x.as_i64());
    // 机器人自己的消息不回环处理（NapCat 开启 reportSelfMessage 时会上报自己说的话）
    if user_id.is_some() && v.get("self_id").and_then(|x| x.as_i64()) == user_id {
        return;
    }
    // 消息去重：OneBot 实现断线重连可能重放最近事件，按 message_id 首见放行
    let msg_id = v
        .get("message_id")
        .map(|x| x.to_string())
        .unwrap_or_default();
    if !first_seen(&format!("napcat:{}", ch.id), &msg_id) {
        return;
    }
    let mut text = extract_text(v.get("message").unwrap_or(&serde_json::Value::Null));
    // 引用消息回查（get_msg）：reply 段的 id 换回原文——「回复图片：这是什么」
    // 若不带原文，模型只看到「这是什么」三个字，引用语义整个丢失。
    // 注记拼在正文头（【回复 某人：…】），尽力而为：回查失败不阻塞主链。
    let reply_ids = reply_para_ids(v.get("message").unwrap_or(&serde_json::Value::Null));
    let ch_id_for_reply = ch.id.clone();
    if !reply_ids.is_empty() {
        let quote = reply_quote_note(&ch_id_for_reply, &reply_ids).await;
        text = format!("{quote}{text}");
    }
    // 入站媒体段（组 6.4）：先盘类型，纯媒体消息给占位正文——
    // 曾在文本为空时直接 return，「只发一张图」被整体丢弃
    let media = collect_media(v.get("message").unwrap_or(&serde_json::Value::Null));
    if text.trim().is_empty() {
        let kinds: Vec<&str> = media.iter().map(|(k, _)| k.as_str()).collect();
        text = media_placeholder(&kinds);
    }
    if text.trim().is_empty() {
        return;
    }
    // 会话白名单：非空时仅放行清单内的群号 / QQ 号
    let peer = group_id
        .map(|g| g.to_string())
        .or(user_id.map(|u| u.to_string()));
    if !ch.allowed_chats.is_empty() {
        let ok = peer
            .as_deref()
            .map(|p| ch.allowed_chats.iter().any(|a| a == p))
            .unwrap_or(false);
        if !ok {
            eprintln!("[napcat:{}] {peer:?} 不在白名单，已忽略", ch.id);
            return;
        }
    }
    let core = core.clone();
    let ch = ch.clone();
    let tx = tx.clone();
    let raw = v.clone();
    tokio::spawn(async move {
        // 流展示名（会话管理可用性修复）：群聊取事件自带 group_name，私聊取
        // 对端昵称——流列表显示「萝莉领主二群」而非裸群号。解析不到则 None
        // （建流缺名回落 chat_key，后续消息到达再补）。
        let flow_name: Option<String> = raw
            .pointer("/group_name")
            .and_then(|x| x.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                raw.pointer("/sender/nickname")
                    .and_then(|x| x.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty() && group_id.is_none())
            });
        let Some((external_id, text, sender_mark)) =
            napcat_gate(&core, &ch, &tx, group_id, user_id, &raw, &text).await
        else {
            // 旁听媒体窗口（ambient-media）：未 @ 的群媒体也下载落盘入窗——
            // 「先发图、后 @ 问」时取走并入多模态注入。仅群聊进窗口（私聊无旁听语义）；
            // 语音不入窗（转写依赖当轮上下文，隔轮转写语义漂移）。
            let peer = group_id
                .map(|g| g.to_string())
                .or_else(|| user_id.map(|u| u.to_string()))
                .unwrap_or_default();
            if group_id.is_some() {
                let mut windowed = Vec::new();
                for (kind, src) in media {
                    if kind != "image" {
                        continue;
                    }
                    let name = format!("napcat-{}.png", exm_core::types::now_ms());
                    let bytes = if let Some(local) = src.strip_prefix("file://") {
                        std::fs::read(local).ok()
                    } else {
                        crate::platform::download_bytes(&src).await
                    };
                    if let Some(b) = bytes {
                        if let Some(p) =
                            crate::platform::save_inbound_media(&core, &ch, &name, b).await
                        {
                            windowed.push((kind, p, name));
                        }
                    }
                }
                ambient_media::push(&ch.id, &peer, windowed);
            }
            return; // 闸门拦截（忽略 / 已回复）
        };
        // 入站媒体（组 6.4）：下载或本地 file:// 复制 → 注入（段清单已在闸门前盘好）；
        // 语音段（record/voice）顺带转写为文本注记——telegram 同款能力，
        // QQ 语音是高频输入形态，只存路径等于让用户对着机器人说了一堆「已保存的噪音」
        // 引用 / @ 轮次一并取走旁听媒体窗口（先发图后 @ 问的图在这里并入注入）。
        let peer = group_id
            .map(|g| g.to_string())
            .or_else(|| user_id.map(|u| u.to_string()))
            .unwrap_or_default();
        let mut saved: Vec<(String, String, String)> = ambient_media::take(&ch.id, &peer);
        let mut transcript_note = String::new();
        for (kind, src) in media {
            let name = format!(
                "napcat-{}.{}",
                exm_core::types::now_ms(),
                if kind == "image" {
                    "png"
                } else if kind == "voice" {
                    "ogg"
                } else {
                    "bin"
                }
            );
            let bytes = if let Some(local) = src.strip_prefix("file://") {
                std::fs::read(local).ok()
            } else {
                crate::platform::download_bytes(&src).await
            };
            if let Some(b) = bytes {
                if let Some(p) =
                    crate::platform::save_inbound_media(&core, &ch, &name, b.clone()).await
                {
                    saved.push((kind.clone(), p, name.clone()));
                }
                if kind == "voice" {
                    transcript_note.push_str(
                        &crate::channel_util::voice_transcript_note(&core, b, &name).await,
                    );
                }
            }
        }
        let text = if transcript_note.is_empty() {
            text
        } else {
            format!("{text}{transcript_note}")
        };
        // 内置命令（/new /status）：闸门放行后、进入会话执行前拦截
        // 会话键与 handle_message 同口径：群号优先，私聊为用户 id
        let peer = group_id
            .map(|g| g.to_string())
            .or_else(|| user_id.map(|u| u.to_string()))
            .unwrap_or_default();
        if let Some(reply) = builtin_command(&core, &ch, &peer, &text).await {
            let action = match group_id {
                Some(g) => serde_json::json!({
                    "action": "send_group_msg",
                    "params": { "group_id": g, "message": [{ "type": "text", "data": { "text": reply } }] },
                }),
                None => serde_json::json!({
                    "action": "send_private_msg",
                    "params": { "user_id": user_id.unwrap_or(0), "message": [{ "type": "text", "data": { "text": reply } }] },
                }),
            };
            let _ = tx.send(action);
            return;
        }
        handle_message(
            &core,
            &ch,
            &tx,
            group_id,
            user_id,
            &external_id,
            saved,
            &text,
            sender_mark.as_deref(),
            flow_name.as_deref(),
        )
        .await;
    });
}

/// OneBot 消息体 → 附件段清单 (kind, 来源)：kind ∈ image | file | voice（record 归一为 voice）；
/// 来源取 data.url，缺省回退 data.file（可能是本地 file:// 路径，由调用方分支处理）。
fn collect_media(m: &serde_json::Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(arr) = m.as_array() else {
        return out; // CQ 码字符串形态不带结构化附件段（NapCat 默认数组段）
    };
    for seg in arr {
        let t = seg.get("type").and_then(|x| x.as_str()).unwrap_or("");
        let kind = match t {
            "image" => "image",
            "file" => "file",
            // 语音段（record / voice 两种写法都见过）；视频体积大且无消费方，不采
            "record" | "voice" => "voice",
            _ => continue,
        };
        let url = seg
            .pointer("/data/url")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let file = seg
            .pointer("/data/file")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let src = if !url.is_empty() {
            url.to_string()
        } else {
            file.to_string()
        };
        if src.is_empty() {
            continue;
        }
        out.push((kind.to_string(), src));
    }
    out
}

/// OneBot 消息的入站闸门（群聊唤醒 + 身份管控）：在 spawn 的任务内执行
/// 闸门放行时返回 (external_id, 消毒后文本)——消毒后文本替代原始 text 进入会话执行
async fn napcat_gate(
    core: &Core,
    ch: &Channel,
    tx: &tokio::sync::mpsc::UnboundedSender<serde_json::Value>,
    group_id: Option<i64>,
    user_id: Option<i64>,
    raw: &serde_json::Value,
    text: &str,
) -> Option<(String, String, Option<String>)> {
    let is_group = group_id.is_some();
    // 提及归一化：@ 机器人本体才算提及（群内 @ 机器人是主语义）。
    // NapCat 上报两种格式：CQ 码字符串（[CQ:at qq=xxx]）与数组段
    // （[{"type":"at","data":{"qq":"..."}}]）——后者是默认，只认 CQ 码时
    // 数组消息恒判「未提及」→ 群聊唤醒门控全灭（群里说话无反应的根因）。
    let self_id = v_self_id(raw);
    let raw_message = raw.get("message");
    let mentioned = raw_message
        .map(|m| is_mentioned(m, self_id))
        .unwrap_or(false);
    let external_id = user_id.map(|u| u.to_string()).unwrap_or_default();
    // 会话键（与 ChannelRun::begin 同口径）：群号优先，私聊为用户 id
    let peer = group_id
        .map(|g| g.to_string())
        .or_else(|| user_id.map(|u| u.to_string()))
        .unwrap_or_default();
    // 发送者真名（card 优先，回落昵称，再回落 QQ 号）——配对/身份判定用
    // 真名而非裸号（控制台身份页与流内显示可辨认）
    let sender_name = raw
        .pointer("/sender/card")
        .or_else(|| raw.pointer("/sender/nickname"))
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| external_id.clone());
    let ctx = InboundCtx {
        text,
        external_id: &external_id,
        display_name: &sender_name,
        is_group,
        mentioned,
        chat_key: &peer,
        // 流展示名：群名（事件自带）优先；私聊用发送者昵称（对端即流主体）
        flow_name: raw
            .pointer("/group_name")
            .and_then(|x| x.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                if is_group {
                    None
                } else {
                    raw.pointer("/sender/nickname")
                        .and_then(|x| x.as_str())
                        .map(|s| s.trim())
                        .filter(|s| !s.is_empty())
                }
            }),
    };
    // 群聊发送者标记（消息头显示 + 模型上下文区分发言人）：card 优先，回落昵称，再回落 QQ 号
    let sender_mark = crate::channel_util::sender_origin(
        is_group,
        raw
            .pointer("/sender/card")
            .or_else(|| raw.pointer("/sender/nickname"))
            .and_then(|x| x.as_str())
            .unwrap_or(""),
        &external_id,
    );
    match admit(core, ch, &ctx).await {
        GateDecision::Allow(clean) => Some((external_id, clean, sender_mark)),
        GateDecision::Ignore => None,
        GateDecision::Deny(reply) => {
            let action = match group_id {
                Some(g) => serde_json::json!({
                    "action": "send_group_msg",
                    "params": { "group_id": g, "message": [{ "type": "text", "data": { "text": reply } }] },
                }),
                None => serde_json::json!({
                    "action": "send_private_msg",
                    "params": { "user_id": user_id.unwrap_or(0), "message": [{ "type": "text", "data": { "text": reply } }] },
                }),
            };
            let _ = tx.send(action);
            None
        }
    }
}

/// OneBot 消息体 → 纯文本：数组段取 text；at 段转写为 `@<名字|qq>` 内联
/// （群聊「@张三 你看」若整个丢弃 at 段，@谁的信息就丢了——部署实测用户
/// 反馈带@的消息语义残缺）；CQ 码字符串剥掉非文本段（at 码同样转写）。
fn extract_text(m: &serde_json::Value) -> String {
    if let Some(arr) = m.as_array() {
        let mut out = String::new();
        for seg in arr {
            match seg.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(t) = seg.pointer("/data/text").and_then(|t| t.as_str()) {
                        out.push_str(t);
                    }
                }
                Some("at") => {
                    // 名字优先（NapCat 常带 data.name），缺省回落 qq 号
                    let name = seg
                        .pointer("/data/name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("");
                    let qq = seg.pointer("/data/qq").and_then(|x| x.as_str()).unwrap_or("");
                    let who = if name.trim().is_empty() { qq } else { name };
                    if !who.trim().is_empty() {
                        out.push_str(&format!("@{who}"));
                    }
                }
                _ => {}
            }
        }
        out
    } else if let Some(s) = m.as_str() {
        strip_cq(s)
    } else {
        String::new()
    }
}

/// 回复消息体的 id 清单（reply 段，按事件顺序）。
/// 引用语义在 IM 高频出现（「回复图片：这是什么」），id 是回查原文的唯一钥匙。
fn reply_para_ids(m: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(arr) = m.as_array() {
        for seg in arr {
            if seg.get("type").and_then(|t| t.as_str()) == Some("reply") {
                if let Some(id) = seg.pointer("/data/id").and_then(|x| x.as_str()) {
                    if !id.trim().is_empty() {
                        out.push(id.to_string());
                    }
                }
            }
        }
    } else if let Some(s) = m.as_str() {
        // CQ 码：[CQ:reply,id=123]
        for cap in s.split("[CQ:reply,").skip(1) {
            if let Some(rest) = cap.split(']').next() {
                for kv in rest.split(',') {
                    let mut it = kv.splitn(2, '=');
                    if it.next() == Some("id") {
                        let id = it.next().unwrap_or("").trim();
                        if !id.is_empty() {
                            out.push(id.to_string());
                        }
                    }
                }
            }
        }
    }
    out
}

/// 回查引用原文（get_msg）：返回 `【回复 <发送者>：<摘要>】` 注记前缀。
/// 失败/超时返回空串（引用链路是尽力而为，不阻塞消息）——模型侧拿到
/// 「【回复 某人：…】正文」即知用户在回应哪条消息。
async fn reply_quote_note(ch_id: &str, msg_ids: &[String]) -> String {
    let mut notes = Vec::new();
    for id in msg_ids.iter().take(3) {
        let Some(resp) = api_call(ch_id, "get_msg", serde_json::json!({ "message_id": id })).await
        else {
            continue;
        };
        let sender = resp
            .pointer("/data/sender/card")
            .or_else(|| resp.pointer("/data/sender/nickname"))
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                resp.pointer("/data/sender/user_id")
                    .and_then(|x| x.as_i64())
                    .map(|u| u.to_string())
            })
            .unwrap_or_else(|| "未知".into());
        // 引用原文的文本段（数组 or CQ 字符串）；图片/文件段给占位说明
        let raw = resp.get("data").cloned().unwrap_or(serde_json::Value::Null);
        let mut body = extract_text(&raw);
        let segs = raw.get("message").cloned().unwrap_or(serde_json::Value::Null);
        if body.trim().is_empty() {
            // get_msg 的 data.message 是消息体本体；文本为空说明纯媒体——盘段型
            let media = collect_media(&segs);
            let kinds = media
                .iter()
                .map(|(k, _)| k.as_str())
                .collect::<Vec<_>>();
            body = media_placeholder(&kinds);
        }
        let body = body.trim().chars().take(200).collect::<String>();
        notes.push(format!("【回复 {sender}：{body}】"));
    }
    if notes.is_empty() {
        String::new()
    } else {
        format!("{}\n", notes.join(" "))
    }
}

/// 取上报事件里的机器人自身 QQ（self_id 字段；数组段提及判定的比对基准）
fn v_self_id(raw: &serde_json::Value) -> Option<i64> {
    raw.get("self_id").and_then(|x| x.as_i64())
}

/// 消息是否 @ 了机器人本体（两种上报格式都认；群内 @ 他人不算）。
/// CQ 码：`[CQ:at qq=2511705734]`；数组段：`{"type":"at","data":{"qq":"2511705734"}}`。
fn is_mentioned(m: &serde_json::Value, self_id: Option<i64>) -> bool {
    if let Some(arr) = m.as_array() {
        arr.iter().any(|seg| {
            let qq = seg
                .pointer("/data/qq")
                .and_then(|x| x.as_str())
                .and_then(|s| s.parse::<i64>().ok());
            seg.get("type").and_then(|t| t.as_str()) == Some("at") && qq == self_id
        })
    } else if let Some(s) = m.as_str() {
        let Some(self_id) = self_id else {
            return s.contains("[CQ:at");
        };
        // 逐个 [CQ:at 出现位，检查其后参数段（到 ] 为止）是否 qq=<self_id>
        let mut rest = s;
        while let Some(i) = rest.find("[CQ:at") {
            let tail = &rest[i + 6..];
            let end = tail.find(']').unwrap_or(tail.len());
            if tail[..end].contains(&format!("qq={self_id}")) {
                return true;
            }
            rest = &rest[i + 6..];
        }
        false
    } else {
        false
    }
}

fn strip_cq(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("[CQ:") {
        out.push_str(&rest[..i]);
        let close = rest[i..].find(']');
        let Some(j) = close else {
            // 找不到收尾 ] ：原样保留标记与后面正文，曾把前缀重复拼一遍
            out.push_str(&rest[i..]);
            rest = "";
            break;
        };
        let code = &rest[i + 4..i + j]; // [CQ:xxx,key=val,...] 的 xxx,key=val,... 部分
        let kind = code.split(',').next().unwrap_or("").trim();
        if kind == "at" {
            // at 码转写为可读 @（与数组段同口径）：qq= 名字缺失时用号
            let mut who = "";
            let mut qq = "";
            for kv in code.split(',') {
                if let Some(v) = kv.strip_prefix("qq=") {
                    qq = v.trim();
                } else if let Some(v) = kv.strip_prefix("name=") {
                    who = v.trim();
                }
            }
            let who = if who.is_empty() { qq } else { who };
            if !who.is_empty() {
                out.push_str(&format!("@{who}"));
            }
        }
        rest = &rest[i + j + 1..];
    }
    out.push_str(rest);
    out
}

/// 一条用户消息：组绑定 → 会话复用 → 订阅回复 → 执行 → send_group_msg / send_private_msg
#[allow(clippy::too_many_arguments)]
// 平台适配器的消息上下文逐项透传
async fn handle_message(
    core: &Arc<Core>,
    ch: &Channel,
    tx: &mpsc::UnboundedSender<serde_json::Value>,
    group_id: Option<i64>,
    user_id: Option<i64>,
    external_id: &str,
    media: Vec<(String, String, String)>,
    text: &str,
    sender_mark: Option<&str>,
    flow_name: Option<&str>,
) {
    // 会话键：群聊优先，无群号走私聊
    let peer = group_id
        .map(|g| g.to_string())
        .or(user_id.map(|u| u.to_string()))
        .unwrap_or_default();
    let Some((run, rx)) =
        ChannelRun::begin_named(core, ch, &peer, flow_name, Some(group_id.is_some())).await
    else {
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
    // napcat 会话工具组（群文件 / 发文件）：绑定本流（群号取 chat_key）。
    // 注入 = 本轮生效；轮次收束后清理（防工具面泄漏到无关会话）。
    core.orchestrator()
        .tools
        .inject_session_tools(
            &run.session_id,
            crate::napcat_tools::build_injections(core, ch, &peer),
        );
    // 入站媒体注入（组 6.4）：图片进多模态暂存，文件附路径说明
    let note = crate::platform::stage_inbound_media(core, &run.session_id, &media);
    let text = if note.is_empty() {
        text.to_string()
    } else {
        format!("{text}{note}")
    };
    let tx = tx.clone();
    let media_tx = tx.clone();
    let reply = spawn_reply(
        &run,
        rx,
        3500,
        move |text| {
            let tx = tx.clone();
            async move {
                let action = match group_id {
                    Some(g) => serde_json::json!({
                        "action": "send_group_msg",
                        "params": { "group_id": g, "message": [{ "type": "text", "data": { "text": text } }] },
                    }),
                    None => serde_json::json!({
                        "action": "send_private_msg",
                        "params": { "user_id": user_id.unwrap_or(0), "message": [{ "type": "text", "data": { "text": text } }] },
                    }),
                };
                let _ = tx.send(action);
            }
        },
        move |item: MediaItem| {
            let tx = media_tx.clone();
            async move {
                // OneBot 11：base64:// 数据段（远程 NapCat 同样可用，免文件系统共享）
                let (seg_type, mime) = match item.kind.as_str() {
                    "voice" => ("record", "audio/ogg"),
                    "file" => ("file", "application/octet-stream"),
                    _ => ("image", "image/png"),
                };
                let Ok(bytes) = tokio::fs::read(&item.path).await else {
                    return false;
                };
                use base64::Engine as _;
                let data = format!(
                    "base64://{}",
                    base64::engine::general_purpose::STANDARD.encode(bytes)
                );
                let name = item
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "media".into());
                let seg = match seg_type {
                    "file" => {
                        serde_json::json!({ "type": "file", "data": { "file": data, "name": name } })
                    }
                    _ => serde_json::json!({ "type": seg_type, "data": { "file": data } }),
                };
                let _ = mime;
                let action = match group_id {
                    Some(g) => serde_json::json!({
                        "action": "send_group_msg",
                        "params": { "group_id": g, "message": [seg] },
                    }),
                    None => serde_json::json!({
                        "action": "send_private_msg",
                        "params": { "user_id": user_id.unwrap_or(0), "message": [seg] },
                    }),
                };
                let _ = tx.send(action);
                true
            }
        },
    );
    // 群聊发送者标记 → 消息 agent_id（消息头显示 + 模型上下文区分发言人）
    let run = if let Some(mark) = sender_mark {
        run.with_origin(mark)
    } else {
        run
    }
    .with_external_id(external_id);
    let session_id = run.session_id.clone();
    run.run(&text).await;
    let _ = reply.await;
    // 会话工具组随轮次收束清理（注入键 = 本轮数据库会话 id）
    core.orchestrator()
        .tools
        .clear_session_tools(&session_id);
}

// ---------------------------------------------------------------- 测试（纯函数部分）

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 登录态分流（5.4 误报修复）：超窗零事件但近期确认在线 → ok（等待登录轮换
    /// 文案，不降级不建议重启）；登录打点过期/从未确认 → 维持 degraded 原判。
    #[test]
    fn 事件自检_在线零事件不误报重启() {
        let id = format!("napcat-selftest-{}", exm_core::types::new_id());
        note_connected(&id);
        // 拨回 31 分钟前（超窗）
        last_event_at()
            .lock()
            .insert(id.clone(), exm_core::types::now_ms() - 31 * 60 * 1000);
        // 在线确认 5 分钟前（10min 窗口内）→ 不降级，报等待登录
        last_login_ok()
            .lock()
            .insert(id.clone(), exm_core::types::now_ms() - 5 * 60 * 1000);
        check_event_silence(&id);
        let st = crate::platform::status_of(&id);
        assert_eq!(st.as_ref().map(|s| s.state.as_str()), Some("ok"), "在线零事件不应降级");
        let detail = st.map(|s| s.detail).unwrap_or_default();
        assert!(
            detail.contains("在线") || detail.contains("轮换"),
            "文案应为等待登录/在线而非重启建议：{detail}"
        );
        assert!(!detail.contains("重启"), "不得建议重启：{detail}");

        // 登录打点 15 分钟前（10min 窗口外，视为登录态不明）→ 维持降级判
        let id2 = format!("napcat-selftest-{}", exm_core::types::new_id());
        note_connected(&id2);
        last_event_at()
            .lock()
            .insert(id2.clone(), exm_core::types::now_ms() - 31 * 60 * 1000);
        last_login_ok()
            .lock()
            .insert(id2.clone(), exm_core::types::now_ms() - 15 * 60 * 1000);
        check_event_silence(&id2);
        assert_eq!(
            crate::platform::status_of(&id2).map(|s| s.state),
            Some("degraded".into()),
            "登录态不明 + 超窗零事件应维持降级"
        );
        note_conn_reset(&id);
        note_conn_reset(&id2);
    }

    /// 假活分流（1.4）：超窗零事件 + 登录不明时——探测近期有回包 = 协议端口活，
    /// 定性「账号未登录」（指引扫码、不建议重启容器）；探测无回包 = 协议端口假死
    ///（容器假活，指引重启容器）。两类失败的修复指引必须可区分。
    #[test]
    fn 事件自检_假活与未登录分流() {
        // 分支一：探测 5 分钟前有回包（协议端口活）→ 报未登录，不提重启容器
        let id = format!("napcat-selftest-{}", exm_core::types::new_id());
        note_connected(&id);
        last_event_at()
            .lock()
            .insert(id.clone(), exm_core::types::now_ms() - 31 * 60 * 1000);
        last_probe_ok()
            .lock()
            .insert(id.clone(), exm_core::types::now_ms() - 5 * 60 * 1000);
        check_event_silence(&id);
        let st = crate::platform::status_of(&id);
        assert_eq!(
            st.as_ref().map(|s| s.state.as_str()),
            Some("degraded"),
            "登录不明应降级"
        );
        let detail = st.map(|s| s.detail).unwrap_or_default();
        assert!(
            detail.contains("未登录") && detail.contains("扫码"),
            "协议端口活应报未登录并指引扫码：{detail}"
        );
        assert!(
            !detail.contains("建议重启") && !detail.contains("重启 NapCat"),
            "端口活不得建议重启容器：{detail}"
        );
        note_conn_reset(&id);

        // 分支二：探测也无回包（协议端口假死）→ 指引重启容器
        let id2 = format!("napcat-selftest-{}", exm_core::types::new_id());
        note_connected(&id2);
        last_event_at()
            .lock()
            .insert(id2.clone(), exm_core::types::now_ms() - 31 * 60 * 1000);
        // 不写 last_probe_ok：探测从未拿到回包
        check_event_silence(&id2);
        let st2 = crate::platform::status_of(&id2);
        assert_eq!(
            st2.as_ref().map(|s| s.state.as_str()),
            Some("degraded"),
            "假活应降级"
        );
        let detail2 = st2.map(|s| s.detail).unwrap_or_default();
        assert!(
            detail2.contains("重启"),
            "协议端口假死应指引重启容器：{detail2}"
        );
        assert!(!detail2.contains("未登录"), "假活不得误报未登录：{detail2}");
        note_conn_reset(&id2);
    }

    /// 事件管道自检（D6）：连结正常 + 超窗零事件 → degraded；事件到达 → ok。
    /// 时间面用打点表直接操纵（不对表 mock 时钟）。
    #[test]
    fn 事件自检_超窗零事件降级_事件到达恢复() {
        let id = format!("napcat-selftest-{}", exm_core::types::new_id());
        // 未连结：无基线不告警
        check_event_silence(&id);
        assert_eq!(crate::platform::status_of(&id).map(|s| s.state), None);

        // 连结建基线：窗口内不告警
        note_connected(&id);
        check_event_silence(&id);
        assert_ne!(
            crate::platform::status_of(&id).map(|s| s.state),
            Some("degraded".into())
        );

        // 基线拨回 31 分钟前：超窗 → degraded
        last_event_at()
            .lock()
            .insert(id.clone(), exm_core::types::now_ms() - 31 * 60 * 1000);
        check_event_silence(&id);
        assert_eq!(
            crate::platform::status_of(&id).map(|s| s.state),
            Some("degraded".into()),
            "超窗零事件应降级"
        );

        // 限频：一小时内重复检查不再重复刷状态（detail 不变即未重报——此处只验不恐慌）
        check_event_silence(&id);

        // 事件到达 → 恢复 ok
        note_event(&id);
        assert_eq!(
            crate::platform::status_of(&id).map(|s| s.state),
            Some("ok".into()),
            "事件到达应恢复"
        );

        // 回收清打点（不残留到下个同名通道）
        note_conn_reset(&id);
        assert!(last_event_at().lock().get(&id).is_none());
    }

    /// 文本提取：数组段拼接 text；at 段转写为 `@<名字|qq>` 内联（@ 语义不丢）
    #[test]
    fn 文本提取_数组段拼接() {
        let m = json!([
            { "type": "text", "data": { "text": "帮我" } },
            { "type": "image", "data": { "url": "https://x/y.png" } },
            { "type": "text", "data": { "text": "看这张图" } },
            { "type": "at", "data": { "qq": "10086" } },
        ]);
        assert_eq!(extract_text(&m), "帮我看这张图@10086");
        // at 段带名字（NapCat 常见形态）：名字优先
        let m2 = json!([
            { "type": "at", "data": { "qq": "10086", "name": "黑星" } },
            { "type": "text", "data": { "text": " 你看这个" } },
        ]);
        assert_eq!(extract_text(&m2), "@黑星 你看这个");
        // 空白 at 段（qq/name 全缺）：不产生悬挂 @
        let m3 = json!([
            { "type": "at", "data": {} },
            { "type": "text", "data": { "text": "正文" } },
        ]);
        assert_eq!(extract_text(&m3), "正文");
        // 非数组（CQ 码字符串）走剥离路径
        assert_eq!(extract_text(&json!("看[CQ:image,id=1]这张")), "看这张");
    }

    /// CQ 码剥离：完整码剥掉；未闭合码不吞后面正文；at 码转写为 @（语义保留）
    #[test]
    fn cq码剥离_未闭合不吞正文() {
        assert_eq!(strip_cq("[CQ:at,qq=1]你好"), "@1你好");
        assert_eq!(strip_cq("[CQ:at,qq=1,name=黑星]你好"), "@黑星你好");
        assert_eq!(strip_cq("a[CQ:x]b[CQ:y]c"), "abc");
        assert_eq!(strip_cq("没有码"), "没有码");
        assert_eq!(strip_cq("未闭合[CQ:at"), "未闭合[CQ:at");
    }

    /// 附件段盘型：image / file / record(→voice) 分类，缺来源的段跳过，非数组段返回空
    #[test]
    fn 附件段盘型_语音归一与来源回退() {
        let m = json!([
            { "type": "image", "data": { "url": "https://x/a.png", "file": "a.png" } },
            { "type": "record", "data": { "file": "file:///tmp/b.mp3" } },
            { "type": "file", "data": {} },
            { "type": "at", "data": { "qq": "1" } },
        ]);
        let media = collect_media(&m);
        assert_eq!(media.len(), 2, "缺来源的 file 段与 at 段不收：{media:?}");
        assert_eq!(
            media[0],
            ("image".to_string(), "https://x/a.png".to_string()),
            "url 优先"
        );
        assert_eq!(
            media[1],
            ("voice".to_string(), "file:///tmp/b.mp3".to_string()),
            "record 归一为 voice"
        );
        // CQ 码字符串形态：无结构化段 → 空（不至于误采）
        assert!(collect_media(&json!("[CQ:image,url=x]")).is_empty());
    }

    /// 提及判定回归（群里无反应根因）：数组段是 NapCat 默认上报格式，
    /// 只认 CQ 码时数组消息恒「未提及」→ 群聊唤醒门控静默全灭。
    #[test]
    fn 提及判定_数组段与cq码() {
        let self_id = Some(2511705734i64);
        // 数组段：@ 机器人本体
        let arr_hit = json!([
            { "type": "at", "data": { "qq": "2511705734" } },
            { "type": "text", "data": { "text": " 在吗" } }
        ]);
        assert!(is_mentioned(&arr_hit, self_id), "数组段 @ 机器人应判提及");
        // 数组段：@ 他人不算
        let arr_other = json!([{ "type": "at", "data": { "qq": "10086" } }]);
        assert!(!is_mentioned(&arr_other, self_id), "@ 他人不应判提及");
        // CQ 码：qq 参数命中
        let cq_hit = json!("[CQ:at,qq=2511705734] 在吗");
        assert!(is_mentioned(&cq_hit, self_id), "CQ 码 @ 机器人应判提及");
        // CQ 码：@ 他人
        let cq_other = json!("[CQ:at,qq=10086] 帮我叫他");
        assert!(!is_mentioned(&cq_other, self_id), "CQ 码 @ 他人不应判提及");
        // 无 self_id（异常上报）：CQ 码退化为「含 at 即提及」（旧行为），数组段严格不命中
        assert!(is_mentioned(&cq_hit, None));
        assert!(!is_mentioned(&arr_hit, None));
    }
}
