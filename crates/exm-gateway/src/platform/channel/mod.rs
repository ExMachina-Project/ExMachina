//! gateway/platform/channel_runtime —— 自 platform.rs 按资源域拆出（纯搬移，零行为变更）

use super::*;

/// 通道文本出站（推送语义，不经历会话）：按平台直发到首个白名单会话或回调地址。
/// 返回是否成功；无可用目标的平台（未配置会话键/回调）返回 false。
pub(crate) async fn push_channel_text(ch: &Channel, text: &str) -> bool {
    let chat = ch.allowed_chats.first().cloned();
    match ch.kind.as_str() {
        // webhook 桥接：POST 回调地址（无回调则无从推送）
        "webhook" | "qq" | "wechat" => {
            let Some(url) = ch.reply_webhook.clone().filter(|u| !u.trim().is_empty()) else {
                return false;
            };
            reqwest::Client::new()
                .post(&url)
                .json(&serde_json::json!({ "text": text }))
                .timeout(std::time::Duration::from_secs(15))
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false)
        }
        // telegram：Bot API sendMessage 到首个白名单会话
        "telegram" => {
            let (Some(token), Some(chat)) =
                (ch.token.clone().filter(|t| !t.trim().is_empty()), chat)
            else {
                return false;
            };
            reqwest::Client::new()
                .post(format!("https://api.telegram.org/bot{token}/sendMessage"))
                .json(&serde_json::json!({ "chat_id": chat, "text": text }))
                .timeout(std::time::Duration::from_secs(15))
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false)
        }
        // discord：Bot REST 发消息（config.channelId 优先于白名单）
        "discord" => {
            let Some(token) = ch.token.clone().filter(|t| !t.trim().is_empty()) else {
                return false;
            };
            let Some(target) = ch.cfg("channelId").or(chat) else {
                return false;
            };
            reqwest::Client::new()
                .post(format!(
                    "https://discord.com/api/v10/channels/{target}/messages"
                ))
                .header("Authorization", format!("Bot {token}"))
                .json(&serde_json::json!({ "content": text }))
                .timeout(std::time::Duration::from_secs(15))
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false)
        }
        // slack：chat.postMessage（token + 频道）
        "slack" => {
            let Some(token) = ch.token.clone().filter(|t| !t.trim().is_empty()) else {
                return false;
            };
            let Some(target) = ch.cfg("channelId").or(chat) else {
                return false;
            };
            reqwest::Client::new()
                .post("https://slack.com/api/chat.postMessage")
                .bearer_auth(token)
                .json(&serde_json::json!({ "channel": target, "text": text }))
                .timeout(std::time::Duration::from_secs(15))
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false)
        }
        // matrix：房间消息事件（token + roomId；与适配器同口径 Bearer 鉴权）
        "matrix" => {
            let Some(homeserver) = ch.cfg("homeserver").filter(|h| !h.trim().is_empty()) else {
                return false;
            };
            let Some(token) = ch.token.clone().filter(|t| !t.trim().is_empty()) else {
                return false;
            };
            let Some(room) = ch.cfg("channelId").or(chat) else {
                return false;
            };
            let url = format!(
                "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
                homeserver.trim_end_matches('/'),
                crate::matrix::url_encode_pub(&room),
                exm_core::types::new_id()
            );
            reqwest::Client::new()
                .put(url)
                .bearer_auth(&token)
                .json(&serde_json::json!({ "msgtype": "m.text", "body": text }))
                .timeout(std::time::Duration::from_secs(15))
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false)
        }
        // napcat / snowluma / qqbot：长连接适配器自有会话内出站，推送语义暂不支持（诚实返回 false）
        _ => false,
    }
}

// ---------------------------------------------------------------- 绑定目标（单智能体 / 组） ----

/// 通道绑定目标的统一口径：`group` 字段既可存组 id（历史形态），也可存 `single:<智能体id>`。
/// 单智能体是通道的一等目标（默认形态），组为次要形态。
pub(crate) const SINGLE_PREFIX: &str = "single:";

/// 解析绑定目标是否为单智能体；返回 (single:<id> 前缀剥出的 id, 组 id)
pub(crate) fn parse_target(raw: &str) -> (Option<String>, Option<String>) {
    let s = raw.trim();
    if let Some(id) = s.strip_prefix(SINGLE_PREFIX) {
        let id = id.trim();
        if !id.is_empty() {
            return (Some(id.to_string()), None);
        }
    }
    if s.is_empty() {
        (None, None)
    } else {
        (None, Some(s.to_string()))
    }
}

/// 目标对应的期望 scope（竞态 P1-2 校验用）：单智能体 = `single:id`，组 = 组 id，
/// 空目标/目标不存在 = None（维持现状，无需校验）
pub(crate) fn expected_scope_of(core: &exm_core::Core, target: &str) -> Option<String> {
    let (single, group) = parse_target(target);
    if let Some(id) = single {
        return if core.registry().single(&id).is_some() {
            Some(format!("single:{id}"))
        } else {
            None
        };
    }
    group
        .filter(|g| core.group_meta(g).is_some())
}

/// 切换全局交互上下文到通道绑定目标（单智能体或组），返回是否发生了切换。
/// 目标不存在/未配置时不动当前上下文（回落调用方各自的默认）。
pub(crate) fn switch_context_to(core: &exm_core::Core, target: &str) -> bool {
    let (single, group) = parse_target(target);
    if let Some(id) = single {
        if core.registry().active_single().as_deref() != Some(id.as_str()) {
            return core.registry().set_active_single(Some(&id)).is_ok();
        }
        return false;
    }
    if let Some(g) = group {
        // 进入组模式前清掉单智能体上下文（set_active_group 不清，会残留 single 模式）
        if core.registry().single_mode() {
            let _ = core.registry().set_active_single(None);
        }
        if g != core.active_group() && core.group_meta(&g).is_some() {
            return core.registry().set_active_group(&g).is_ok();
        }
    }
    false
}

/// 通道绑定目标是否存在（校验用）：单智能体查 singles 表，组查组表
pub(crate) fn target_exists(core: &exm_core::Core, target: &str) -> bool {
    let (single, group) = parse_target(target);
    if let Some(id) = single {
        return core.registry().single(&id).is_some();
    }
    group
        .as_deref()
        .map(|g| core.group_meta(g).is_some())
        .unwrap_or(true)
}

// ---------------------------------------------------------------- 出站能力矩阵与回复整形（组 6）

/// 平台出站能力矩阵（按 kind 静态声明）：媒体直发与 typing 支持
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChannelCaps {
    pub image: bool,
    pub file: bool,
    pub voice: bool,
    pub typing: bool,
}

/// 能力矩阵：telegram/discord/slack/napcat/matrix 直发媒体（或部分），
/// qqbot 图片直发（files 上行 file_data），其余类型降级。
/// 与各适配器实际发送实现保持同步——新增实现即开位。
/// 平台不支持项的原因（给维护者的口径，别随手"优化"回去）：
/// · discord voice：无原生语音条目形态，为避免与「discord 不支持语音」的既有测试口径冲突暂不开位；
/// · slack / napcat typing：Slack 机器人无输入中指示 API；OneBot 11 无标准 typing 动作；
/// · qqbot file/voice：官方 files 上行 file_data 仅图片（png/jpg）支持充分——
///   语音须 silk 转码、视频/文件的上行用例不稳；且 file_info 有 TTL，超时即失效。
pub(crate) fn caps(kind: &str) -> ChannelCaps {
    match kind {
        "telegram" => ChannelCaps {
            image: true,
            file: true,
            voice: true,
            typing: true,
        },
        "discord" => ChannelCaps {
            image: true,
            file: true,
            voice: false,
            typing: true,
        },
        "slack" => ChannelCaps {
            image: true,
            file: true,
            voice: false,
            typing: false,
        },
        // napcat / snowluma：OneBot 11 正向 WS（图片/文件/语音出站齐备；无标准 typing 动作）
        "napcat" | "snowluma" => ChannelCaps {
            image: true,
            file: true,
            voice: true,
            typing: false,
        },
        // matrix：m.image / m.file / m.audio 出站已实现（mx_send_media：upload → content_uri → 事件），
        // typing 有官方端点（PUT /rooms/{id}/typing/{uid}）——曾因这里全 false 把已实现能力封成死代码。
        "matrix" => ChannelCaps {
            image: true,
            file: true,
            voice: true,
            typing: true,
        },
        // qqbot：图片经 files 上行（file_data base64 → file_info → msg_type=7）
        // 原生直发；file/voice 降级（file_data 官方仅图片支持充分）
        "qqbot" => ChannelCaps {
            image: true,
            file: false,
            voice: false,
            typing: false,
        },
        _ => ChannelCaps {
            image: false,
            file: false,
            voice: false,
            typing: false,
        },
    }
}

/// 媒体降级决策：平台支持则原生直发；否则按类型降级（spec: 图片→链接/文本描述、文件→链接、语音→文本摘要）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaFallback {
    /// 平台原生直发
    Native,
    /// 降级：文本描述 + 可访问链接（由调用方拼装）
    LinkNote,
}

pub(crate) fn media_fallback(kind: &str, kind_want: &str) -> MediaFallback {
    let c = caps(kind);
    let supported = match kind_want {
        "image" => c.image,
        "file" => c.file,
        "voice" => c.voice,
        _ => false,
    };
    if supported {
        MediaFallback::Native
    } else {
        MediaFallback::LinkNote
    }
}

/// 长回复分段（spec 6.6）：段落边界优先；代码块与结构化陈述前缀保持完整；
/// 单块超限才硬切并标注续接序号。max_chars 为平台上限。
pub(crate) fn split_reply(text: &str, max_chars: usize) -> Vec<String> {
    let text = text.trim_end();
    if text.chars().count() <= max_chars {
        return vec![text.to_string()];
    }
    // 切分单元：段落（\n\n），代码块视为不可分单元（与前后段落独立）
    let mut units: Vec<(String, bool)> = Vec::new(); // (内容, 是否代码块)
    let mut para = String::new();
    let mut in_code = false;
    for line in text.split('\n') {
        let is_fence = line.trim_start().starts_with("```");
        if is_fence {
            in_code = !in_code;
            para.push_str(line);
            para.push('\n');
            if !in_code {
                units.push((std::mem::take(&mut para), true));
            }
            continue;
        }
        if in_code {
            para.push_str(line);
            para.push('\n');
            continue;
        }
        if line.is_empty() && !para.is_empty() {
            units.push((std::mem::take(&mut para), false));
            continue;
        }
        para.push_str(line);
        para.push('\n');
    }
    if !para.trim().is_empty() {
        units.push((para, false));
    }

    // 聚合：按字符预算把相邻单元拼进同一消息（超预算即开新消息）
    let mut out: Vec<String> = Vec::new();
    let mut buf = String::new();
    let mut hard_parts: Vec<String> = Vec::new();
    let flush_hard = |buf: &mut String, out: &mut Vec<String>| {
        if !buf.trim().is_empty() {
            out.push(buf.trim_end().to_string());
        }
        buf.clear();
    };
    for (unit, _is_code) in &units {
        let unit = unit.trim_end().to_string();
        if unit.chars().count() > max_chars {
            flush_hard(&mut buf, &mut out);
            // 硬切：按行聚合，超过预算开新片。不加「（续 n/m）」标注——
            // IM 聊天里机器腔的分段序号是噪音（部署实测用户反馈），分段
            // 本身已按顺序投递，序号没有信息量。
            hard_parts.clear();
            let mut piece = String::new();
            for line in unit.split_inclusive('\n') {
                if piece.chars().count() + line.chars().count() > max_chars && !piece.is_empty() {
                    hard_parts.push(piece.trim_end().to_string());
                    piece.clear();
                }
                // 超长单行：按字符预算行内硬切
                if line.chars().count() > max_chars {
                    let chars: Vec<char> = line.chars().collect();
                    let mut idx = 0usize;
                    while idx < chars.len() {
                        let end = (idx + max_chars).min(chars.len());
                        let seg: String = chars[idx..end].iter().collect();
                        if end < chars.len() {
                            hard_parts.push(seg);
                        } else {
                            piece.push_str(&seg);
                        }
                        idx = end;
                    }
                    continue;
                }
                piece.push_str(line);
            }
            if !piece.trim().is_empty() {
                hard_parts.push(piece.trim_end().to_string());
            }
            out.extend(hard_parts.iter().cloned());
            continue;
        }
        if buf.chars().count() + unit.chars().count() + 2 > max_chars && !buf.is_empty() {
            flush_hard(&mut buf, &mut out);
        }
        if !buf.is_empty() {
            buf.push('\n');
        }
        buf.push_str(&unit);
        buf.push('\n');
    }
    flush_hard(&mut buf, &mut out);
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

#[cfg(test)]
mod reply_split_tests {
    use super::*;

    #[test]
    fn 短文本不分段() {
        assert_eq!(split_reply("hello", 100), vec!["hello".to_string()]);
    }

    #[test]
    fn 段落边界优先切分() {
        let text = "第一段。\n\n第二段比较长一些的内容。\n\n第三段。";
        let out = split_reply(text, 20);
        assert!(out.len() >= 2, "应切成多条: {out:?}");
        // 每条不超限
        assert!(out.iter().all(|s| s.chars().count() <= 20), "{out:?}");
        // 顺序保持
        let joined = out.join("\n");
        assert!(joined.contains("第一段") && joined.contains("第三段"));
    }

    #[test]
    fn 代码块不被切断() {
        let code = "```rust\nfn a() {\n    println!(\"x\");\n}\n```";
        let text = format!("说明：\n\n{code}\n\n结尾。");
        // max 60：代码块（约 46 字符）可完整容纳，聚合时不得跨消息断裂
        let out = split_reply(&text, 60);
        for s in &out {
            let fences = s.matches("```").count();
            assert_eq!(fences % 2, 0, "代码块围栏应成对出现: {s:?}");
        }
        assert!(
            out.iter().any(|s| s.contains("println!")),
            "代码块应完整在某条里: {out:?}"
        );
    }

    #[test]
    fn 超长单元硬切多片() {
        let block = format!("```text\n{}\n```", "x".repeat(200));
        let out = split_reply(&block, 60);
        assert!(out.len() >= 3, "硬切应多片: {}", out.len());
        // 不加机器腔续接标注（部署实测用户反馈：聊天里「（续 1/2）」是噪音）
        assert!(
            out.iter().all(|s| !s.contains("（续 ")),
            "硬切不应带续接序号: {out:?}"
        );
        // 每片接近但不远超预算
        for s in &out {
            assert!(s.chars().count() <= 60 + 20, "{}", s.chars().count());
        }
    }

    #[test]
    fn 降级决策_矩阵覆盖() {
        // telegram 全支持
        assert_eq!(media_fallback("telegram", "image"), MediaFallback::Native);
        assert_eq!(media_fallback("telegram", "voice"), MediaFallback::Native);
        // discord 不支持语音
        assert_eq!(media_fallback("discord", "voice"), MediaFallback::LinkNote);
        assert_eq!(media_fallback("discord", "image"), MediaFallback::Native);
        // qqbot：图片直发（files 上行），file/voice 降级
        assert_eq!(media_fallback("qqbot", "image"), MediaFallback::Native);
        assert_eq!(media_fallback("qqbot", "file"), MediaFallback::LinkNote);
        // 未知平台全降级
        assert_eq!(media_fallback("webhook", "image"), MediaFallback::LinkNote);
    }
}


// ---------------------------------------------------------------- 域分文件

mod exec;
mod gate;
mod reply;

pub(crate) use exec::*;
pub(crate) use gate::*;
pub(crate) use reply::*;

// push_channel_text/绑定目标/能力矩阵/回复整形 留在本文件
