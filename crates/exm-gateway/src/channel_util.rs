//! 通道适配器共用小件（channel_util.rs）。
//!
//! 两块内容，都是六个适配器（telegram / qqbot / napcat / discord / slack / matrix）
//! 需要同一套语义的地方——各写一遍必然漂移（命令文案、去重窗口、归档命名不一致）：
//!
//! ① 内置命令：`/new` 开新会话、`/status` 查状态。`/stop` 已在 `platform::admit`
//!    最前面统一处理，这里补齐其余两条。调用时机固定为「入站闸门 Allow 之后」：
//!    未绑定 / 被拒用户不可用命令（/new 会建会话，属资源操作，不能绕过身份管控）。
//! ② 消息去重：Discord / QQ 机器人断线 Resume 会重放最近事件；Slack 对同一消息
//!    可能同时投 `message` 与 `app_mention` 双事件。按平台消息 id 做首见放行。

use crate::platform::{status_of, Channel};
use exm_core::Core;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::OnceLock;

// ---------------------------------------------------------------- 内置命令

/// 内置命令识别结果（纯数据，便于单测）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuiltinCmd {
    /// /new：开新会话
    New,
    /// /status：查通道与会话状态
    Status,
}

/// 文本 → 内置命令（纯函数）：只认整词命令（前后可有空白），不认带参数变体——
/// IM 输入法容易在命令后带空格/全角空格，trim 后整词比对最不容易误触发。
pub(crate) fn parse_command(text: &str) -> Option<BuiltinCmd> {
    match text.trim() {
        "/new" => Some(BuiltinCmd::New),
        "/status" => Some(BuiltinCmd::Status),
        _ => None,
    }
}

/// 尝试执行内置命令：命中返回回复文本（由调用方按平台发回），未命中返回 None 走正常消息流。
/// `/new` `/sessions` `/use` 已升级为聊天流（chat flow）指令，统一在 `platform::admit`
/// 的 `flow_command` 处理（流绑定语义，旧会话保留可切回）；这里只保留 `/status`。
pub(crate) async fn builtin_command(
    core: &Arc<Core>,
    ch: &Channel,
    chat_key: &str,
    text: &str,
) -> Option<String> {
    match parse_command(text) {
        Some(BuiltinCmd::Status) => Some(cmd_status(core, ch, chat_key)),
        Some(BuiltinCmd::New) | None => None,
    }
}

/// /status：通道身份、连接状态（适配器上报）、当前会话、白名单口径。
fn cmd_status(core: &Arc<Core>, ch: &Channel, chat_key: &str) -> String {
    let gid = ch.group.clone().unwrap_or_else(|| core.active_group());
    let conn = match status_of(&ch.id) {
        Some(s) => {
            let mark = if s.state == "ok" { "✅" } else { "❌" };
            format!(
                "{mark} {}（{}）",
                if s.state == "ok" {
                    "已连接"
                } else {
                    "异常"
                },
                s.detail
            )
        }
        // webhook/qq/wechat 桥接类没有运行时，无状态上报属正常
        None => "⚪ 无长连接（桥接类通道属正常）".to_string(),
    };
    let sess = core
        .flow_active_session(&ch.id, chat_key)
        .and_then(|sid| core.store.get_session(&sid).ok().flatten());
    let sess_line = match sess {
        Some(s) => format!(
            "· 会话：{}（状态 {}）",
            &s.id[..s.id.len().min(12)],
            s.status
        ),
        None => "· 会话：尚未创建（发消息即建）".to_string(),
    };
    let wl = if ch.allowed_chats.is_empty() {
        "不限（空 = 所有会话）".to_string()
    } else {
        format!("{} 个会话", ch.allowed_chats.len())
    };
    format!(
        "📡 通道状态\n· 通道：{}（{}{}）\n· 绑定组：{}\n· 连接：{}\n{}\n· 白名单：{}",
        ch.id,
        ch.kind,
        ch.account
            .as_deref()
            .map(|a| format!(" · {a}"))
            .unwrap_or_default(),
        gid,
        conn,
        sess_line,
        wl,
    )
}

// ---------------------------------------------------------------- 消息去重

/// 去重表：平台 → (集合, 环形队列)。环形容量 1024：
/// 正常流量下是几分钟的窗口，足够覆盖 Resume 重放（重放只发生在断线瞬间附近），
/// 又不会无限吃内存。
type DedupState = Mutex<HashMap<String, (HashSet<String>, VecDeque<String>)>>;

fn seen() -> &'static DedupState {
    static SEEN: OnceLock<DedupState> = OnceLock::new();
    SEEN.get_or_init(|| Mutex::new(HashMap::new()))
}

const DEDUP_CAP: usize = 1024;

/// 首见返回 true（放行）；重复返回 false（丢弃）。key 为空视为不可去重（放行）。
pub(crate) fn first_seen(scope: &str, key: &str) -> bool {
    let key = key.trim();
    if key.is_empty() {
        return true;
    }
    let mut m = seen().lock();
    let (set, q) = m.entry(scope.to_string()).or_default();
    if !set.insert(key.to_string()) {
        return false;
    }
    q.push_back(key.to_string());
    if q.len() > DEDUP_CAP {
        if let Some(old) = q.pop_front() {
            set.remove(&old);
        }
    }
    true
}

// ---------------------------------------------------------------- 媒体占位正文

/// 纯媒体消息的占位正文：只发图 / 只发文件的消息没有文字，若直接丢弃，
/// 用户会觉得「发了图没人理」（telegram 通道曾因此丢图，修复时总结成共用小件）。
/// 给一句最小说明让消息继续走完闸门与会话；附件的路径 / 多模态注入细节由
/// `platform::stage_inbound_media` 在执行前追加，这里只负责「消息不凭空消失」。
pub(crate) fn media_placeholder(kinds: &[&str]) -> String {
    let imgs = kinds.iter().filter(|k| **k == "image").count();
    let voices = kinds.iter().filter(|k| **k == "voice").count();
    let files = kinds.iter().filter(|k| **k == "file").count();
    let mut parts: Vec<String> = Vec::new();
    if imgs > 0 {
        parts.push(format!("图片 x{imgs}"));
    }
    if voices > 0 {
        parts.push(format!("语音 x{voices}"));
    }
    if files > 0 {
        parts.push(format!("文件 x{files}"));
    }
    if parts.is_empty() {
        return String::new();
    }
    format!("（用户发来附件：{}，附件细节见消息注入）", parts.join("、"))
}

// ---------------------------------------------------------------- 附件类型归类

/// 附件 mimetype → 媒体类型（纯函数）：image | voice | file。
/// 音频归 voice 以便占位正文计为「语音」并触发转写（telegram 同款能力）；
/// 未识别的 mimetype 保守归 file——宁可当附件带路径，也不丢弃。
/// 为什么在共用层：discord（content_type）与 slack（mimetype）用同一段语义，
/// 各写一遍必然漂移（某个平台漏了 audio 归类，占位与转写就静默失效）。
pub(crate) fn classify_attachment(mimetype: &str) -> &'static str {
    if mimetype.starts_with("image/") {
        "image"
    } else if mimetype.starts_with("audio/") {
        "voice"
    } else {
        "file"
    }
}

// ---------------------------------------------------------------- 语音转写注记

/// 语音附件转写注记（telegram 同款能力的共用化）：转写成功返回
/// `\n（语音转写：…）` 注记文本，失败/空结果返回空串（调用方直接 append 到正文）。
///
/// 为什么做成共用小件：入站语音在 napcat（record 段）/ slack（audio/* 文件）/
/// discord（audio/* 附件）/ matrix（m.audio）都会出现，「下载成功 → core.transcribe
/// → 失败静默降级」这段逻辑各写一遍必然漂移（有的平台会漏、报错口径会不一致）。
/// 转写属尽力而为：失败不拦截消息，语音文件本体仍以附件注记进入会话，模型还能按路径自行处理。
pub(crate) async fn voice_transcript_note(
    core: &exm_core::Core,
    bytes: Vec<u8>,
    name: &str,
) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    match core.transcribe(&bytes, name).await {
        Ok(t) if !t.trim().is_empty() => format!("\n（语音转写：{}）", t.trim()),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 命令解析_整词识别与误触防护() {
        assert_eq!(parse_command("/new"), Some(BuiltinCmd::New));
        assert_eq!(parse_command("  /new  "), Some(BuiltinCmd::New));
        assert_eq!(parse_command("/status"), Some(BuiltinCmd::Status));
        assert_eq!(parse_command("/status 后面跟话"), None, "带后缀不算命令");
        assert_eq!(
            parse_command("/NEW"),
            None,
            "命令区分大小写（不误触普通聊天）"
        );
        assert_eq!(parse_command("你好 /new"), None, "混在句中不算命令");
        assert_eq!(parse_command("/stop"), None, "/stop 归 admit 管，这里不认");
        assert_eq!(parse_command(""), None);
    }

    #[test]
    fn 媒体占位_类型计数与空清单() {
        assert_eq!(
            media_placeholder(&[]),
            "",
            "无附件不给占位（调用方应直接丢弃）"
        );
        assert_eq!(
            media_placeholder(&["image"]),
            "（用户发来附件：图片 x1，附件细节见消息注入）"
        );
        let multi = media_placeholder(&["image", "image", "file"]);
        assert!(
            multi.contains("图片 x2") && multi.contains("文件 x1"),
            "{multi}"
        );
        let voice = media_placeholder(&["voice"]);
        assert!(voice.contains("语音 x1"), "{voice}");
        assert_eq!(
            media_placeholder(&["unknown"]),
            "",
            "未知类型不计入占位（避免误导模型）"
        );
    }

    #[test]
    fn 去重_首见放行重复丢弃() {
        assert!(first_seen("ut", "m1"), "首见放行");
        assert!(!first_seen("ut", "m1"), "重复丢弃");
        assert!(first_seen("ut", "m2"));
        // 不同平台作用域互不影响
        assert!(first_seen("ut2", "m1"));
        // 空 key 放行（平台没给 id 时不去重）
        assert!(first_seen("ut", ""));
        assert!(first_seen("ut", "  "));
    }

    /// 附件 mimetype 归类（discord/slack 共用）：音频归 voice 触发转写，未知保守归 file 不丢
    #[test]
    fn 附件归类_音频归语音未知归文件() {
        assert_eq!(classify_attachment("image/png"), "image");
        assert_eq!(classify_attachment("image/jpeg"), "image");
        assert_eq!(
            classify_attachment("audio/ogg"),
            "voice",
            "音频归 voice 以触发转写"
        );
        assert_eq!(classify_attachment("audio/mpeg"), "voice");
        assert_eq!(classify_attachment("application/pdf"), "file");
        assert_eq!(
            classify_attachment("video/mp4"),
            "file",
            "视频无消费方，保守归文件"
        );
        assert_eq!(classify_attachment(""), "file", "缺 mimetype 不丢弃");
    }

    #[test]
    fn 去重_环形容量淘汰最旧() {
        for i in 0..(DEDUP_CAP + 8) {
            first_seen("ut-cap", &format!("k{i}"));
        }
        // 最早的 k0 已被挤出环：再次出现视为首见
        assert!(first_seen("ut-cap", "k0"));
        // 环内的 k(DEDUP_CAP+4) 仍是重复
        assert!(!first_seen("ut-cap", &format!("k{}", DEDUP_CAP + 4)));
    }

    fn test_channel() -> Channel {
        Channel {
            id: "t1".into(),
            kind: "telegram".into(),
            enabled: true,
            group: None,
            allowed_chats: vec![],
            admins: vec![],
            account: None,
            secret: None,
            reply_webhook: None,
            token: None,
            config: Default::default(),
            group_gate: None,
            capability: "computer".into(),
            memory_isolation: false,
            created_at: String::new(),
        }
    }

    /// /new 已升级为聊天流指令（platform::admit::flow_command）：builtin_command
    /// 不再处理 New（返回 None 放行到闸门）；此处验证转发口径不回归。
    #[tokio::test]
    async fn 命令_new转发流指令() {
        let dir = std::env::temp_dir().join(format!("exm-chutil-{}", exm_core::types::new_id()));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        let core = Arc::new({
            let mut cfg = exm_core::config::ExmConfig::load(&dir);
            cfg.use_mock = true;
            exm_core::Core::with_config(cfg).expect("创建 Core 失败")
        });
        let ch = test_channel();
        // /new 不再由 builtin_command 处理（流指令在 admit 内统一收口）
        assert!(builtin_command(&core, &ch, "100", "/new").await.is_none());
        // /status 仍在此处理
        assert!(builtin_command(&core, &ch, "100", "/status").await.is_some());
    }

    /// /status：能报出通道与连接口径（无运行时上报 = 桥接类属正常）
    #[tokio::test]
    async fn 命令_状态回执可读() {
        let dir = std::env::temp_dir().join(format!("exm-chutil-s-{}", exm_core::types::new_id()));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        let core = Arc::new({
            let mut cfg = exm_core::config::ExmConfig::load(&dir);
            cfg.use_mock = true;
            exm_core::Core::with_config(cfg).expect("创建 Core 失败")
        });
        let ch = test_channel();
        let r = builtin_command(&core, &ch, "100", "/status").await.unwrap();
        assert!(r.contains("t1"), "含通道 id：{r}");
        assert!(r.contains("telegram"), "含平台：{r}");
        assert!(r.contains("尚未创建"), "未发过消息时会话未建：{r}");
        assert!(r.contains("白名单"), "{r}");
    }

    /// 非命令文本不拦截
    #[tokio::test]
    async fn 非命令文本放行() {
        let dir = std::env::temp_dir().join(format!("exm-chutil-n-{}", exm_core::types::new_id()));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        let core = Arc::new({
            let mut cfg = exm_core::config::ExmConfig::load(&dir);
            cfg.use_mock = true;
            exm_core::Core::with_config(cfg).expect("创建 Core 失败")
        });
        let ch = test_channel();
        assert!(builtin_command(&core, &ch, "1", "帮我查一下天气")
            .await
            .is_none());
    }
}
