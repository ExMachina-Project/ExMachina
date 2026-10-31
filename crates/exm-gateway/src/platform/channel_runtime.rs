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
        // napcat/qqbot：长连接适配器自有会话内出站，推送语义暂不支持（诚实返回 false）
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

/// 能力矩阵：telegram/discord/slack/napcat/matrix 直发媒体（或部分），qqbot 官方 API 受限全降级。
/// 与各适配器实际发送实现保持同步——新增实现即开位。
/// 平台不支持项的原因（给维护者的口径，别随手"优化"回去）：
/// · discord voice：无原生语音条目形态，为避免与「discord 不支持语音」的既有测试口径冲突暂不开位；
/// · slack / napcat typing：Slack 机器人无输入中指示 API；OneBot 11 无标准 typing 动作；
/// · qqbot 全降级：官方机器人被动消息仅支持文本，富媒体需另走 richmedia 上报链路。
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
        "napcat" => ChannelCaps {
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
            // 硬切：按行聚合，超过预算开新片，带续接标注
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
            let total = hard_parts.len();
            for (i, p) in hard_parts.iter().enumerate() {
                if total > 1 {
                    out.push(format!("{p}\n（续 {}/{}）", i + 1, total));
                } else {
                    out.push(p.clone());
                }
            }
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
    fn 超长单元硬切带续接序号() {
        let block = format!("```text\n{}\n```", "x".repeat(200));
        let out = split_reply(&block, 60);
        assert!(out.len() >= 3, "硬切应多片: {}", out.len());
        assert!(out.first().unwrap().contains("（续 1/"));
        assert!(out.last().unwrap().contains("）") && out.last().unwrap().contains("（续 "));
        // 每片（含标注）接近但不远超预算
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
        // qqbot 全降级
        assert_eq!(media_fallback("qqbot", "image"), MediaFallback::LinkNote);
        assert_eq!(media_fallback("qqbot", "file"), MediaFallback::LinkNote);
        // 未知平台全降级
        assert_eq!(media_fallback("webhook", "image"), MediaFallback::LinkNote);
    }
}

// ---------------------------------------------------------------- 通道入站闸门（身份 + 群聊唤醒）

/// 入站消息上下文：由各适配器从平台消息归一化提取
pub(crate) struct InboundCtx<'a> {
    pub text: &'a str,
    pub external_id: &'a str,
    pub display_name: &'a str,
    /// 群聊 / 频道上下文（私聊为 false）
    pub is_group: bool,
    /// 平台归一化信号：被提及（@）或回复智能体消息
    pub mentioned: bool,
    /// 通道会话键（与会话标题 `channel:<id>:<key>` 一致；/stop 定位会话用）
    pub chat_key: &'a str,
}

pub(crate) enum GateDecision {
    Allow,
    /// 群聊未唤醒：静默忽略
    Ignore,
    /// 拒绝执行并回复提示（含 /pair 结果与配对指引）
    Deny(String),
}

/// 入站统一闸门：① `/pair` 兑换拦截（未绑定也可用）→ ② 群聊唤醒门控 → ③ 身份管控裁决。
/// 位置前置于一切执行（ChannelRun::begin 之前由各适配器调用）。
pub(crate) async fn admit(
    core: &exm_core::Core,
    ch: &Channel,
    ctx: &InboundCtx<'_>,
) -> GateDecision {
    let identity_cfg = &core.config().identity;

    // ⓪ /stop 停止指令（先于一切校验）：直接终止该通道会话运行中的轮次并回执
    if ctx.text.trim() == "/stop" {
        let gid = ch.group.clone().unwrap_or_else(|| core.active_group());
        let title = format!("channel:{}:{}", ch.id, ctx.chat_key);
        let sid = core
            .flow_active_session(&ch.id, ctx.chat_key)
            .or_else(|| {
                // 流无绑定（含升级前的存量数据）：回落旧 title 寻址
                core.list_sessions_in_group(&gid)
                    .ok()
                    .and_then(|list| list.into_iter().find(|s| s.title == title))
                    .map(|s| s.id)
            });
        let receipt = match sid.map(|id| core.stop_session(&id)) {
            Some(Ok(n)) => format!("⏹ 已停止本轮（已执行 {n} 次工具调用）。"),
            Some(Err(_)) | None => "当前没有运行中的任务。".to_string(),
        };
        return GateDecision::Deny(receipt);
    }

    // ① 配对码兑换（控制台签发短码 → 通道内 /pair <码> 绑定；单次有效、限期）
    let trimmed = ctx.text.trim();
    if let Some(code) = trimmed
        .strip_prefix("/pair ")
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        return match core.redeem_pairing(code, &ch.id, ctx.external_id, ctx.display_name, "member")
        {
            Ok(idn) => GateDecision::Deny(format!(
                "【报告】绑定成功：{}（角色 {}）。现在可以正常对话。",
                idn.display_name, idn.role
            )),
            Err(e) => GateDecision::Deny(format!("【报告】绑定失败：{e}")),
        };
    }

    // ①' 聊天流（chat flow）会话管理：一个群 / 一个私聊各是一条流，流固定复用
    // 一个活跃会话；不切换就一直在同一会话里聊，跨轮上下文自然延续。
    if let Some(reply) = flow_command(core, ch, ctx).await {
        return GateDecision::Deny(reply);
    }

    // ② 群聊唤醒门控：群内仅提及 / 回复触发；私聊豁免
    if ch.gate_enabled(core) && ctx.is_group && !ctx.mentioned {
        return GateDecision::Ignore;
    }

    // ③ 身份管控：开启后未绑定用户拒绝执行并给配对指引。管理者账号清单命中豁免——
    // 按外部用户 ID 跨通道匹配（人在任一通道的管理者清单即凭证；判权跟人走，
    // 用户可能在此通道说话、经别的通道执行动作，故不限于本通道清单）
    if identity_cfg.identity_required
        && core.identity_of(&ch.id, ctx.external_id).is_none()
        && !load_channels(core)
            .iter()
            .any(|c| c.admins.iter().any(|a| a.trim() == ctx.external_id))
    {
        return GateDecision::Deny(
            "【报告】未绑定身份：请在控制台「通道身份」页签发配对码，然后在此发送 /pair <配对码> 完成绑定。".into(),
        );
    }

    // ④ 用量治理：通道主体滑动窗口限流 + 周期请求数配额（仅外部入口；组内派发不经此处）
    let limits = &core.config().limits;
    if limits.enabled {
        let subject = match core.identity_of(&ch.id, ctx.external_id) {
            Some(i) => format!("user:{}", i.id),
            None => format!("ch:{}:{}", ch.id, ctx.external_id),
        };
        let now = exm_core::types::now_ms();
        if limits.max_requests > 0
            && !crate::limits::RateLimiter::check(
                &format!("ch:{subject}"),
                limits.window_secs * 1000,
                limits.max_requests,
                now,
            )
        {
            return GateDecision::Deny(format!(
                "【报告】请求过于频繁：受用量限制，请在 {} 秒后重试。",
                limits.window_secs
            ));
        }
        if limits.quota_requests > 0 {
            let (qkey, qwin) = crate::limits::quota_key(&subject, &limits.quota_period, now);
            if !crate::limits::RateLimiter::check(&qkey, qwin, limits.quota_requests, now) {
                return GateDecision::Deny(
                    "【报告】周期请求数配额已耗尽，请等待周期刷新或联系管理员调整配额。".into(),
                );
            }
        }
    }
    GateDecision::Allow
}

/// 聊天流（chat flow）会话管理指令：
/// - `/new [标题]`：开新会话并绑到流（旧会话保留可切回）
/// - `/sessions`：列出流内全部会话（含当前绑定标记）
/// - `/use <序号|标题片段>`：切换流的活跃会话
///
/// 返回 Some(reply) = 指令已处理（Deny 语义回帖）；None = 非流指令，放行后续闸门。
async fn flow_command(
    core: &exm_core::Core,
    ch: &Channel,
    ctx: &InboundCtx<'_>,
) -> Option<String> {
    let text = ctx.text.trim();
    let gid = ch.group.clone().unwrap_or_else(|| core.active_group());
    let title_prefix = format!("channel:{}:{}:", ch.id, ctx.chat_key);

    if text == "/new" || text.starts_with("/new ") {
        let custom = text.strip_prefix("/new").unwrap_or_default().trim();
        let title = if custom.is_empty() {
            format!("channel:{}:{}", ch.id, ctx.chat_key)
        } else {
            format!("{title_prefix}{custom}")
        };
        return match core.create_session(&title) {
            Ok(s) => match core.bind_flow_session(&ch.id, ctx.chat_key, &s.id) {
                Ok(()) => Some(format!("已开新会话（{}）。", short_id(&s.id))),
                Err(e) => Some(format!("会话已建但绑定失败：{e}")),
            },
            Err(e) => Some(format!("开新会话失败：{e}")),
        };
    }

    if text == "/sessions" {
        // 流内会话 = 同流前缀标题的全部会话（新旧口径都覆盖）
        let all = core.list_sessions_in_group(&gid).unwrap_or_default();
        let mut mine: Vec<_> = all
            .iter()
            .filter(|s| s.title.starts_with(&title_prefix) || {
                s.title == format!("channel:{}:{}", ch.id, ctx.chat_key)
            })
            .cloned()
            .collect();
        mine.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        if mine.is_empty() {
            return Some("本聊天流还没有会话。发送 /new 开一个。".into());
        }
        let active = core.flow_active_session(&ch.id, ctx.chat_key);
        let lines: Vec<String> = mine
            .iter()
            .take(10)
            .enumerate()
            .map(|(i, s)| {
                let mark = if active.as_deref() == Some(s.id.as_str()) {
                    "●"
                } else {
                    "○"
                };
                let label = s
                    .title
                    .strip_prefix(&title_prefix)
                    .filter(|t| !t.is_empty())
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| default_flow_session_label(&s.created_at));
                format!("{mark} {i}. {label}（{}）", short_id(&s.id))
            })
            .collect();
        return Some(format!(
            "本聊天流的会话：\n{}\n/use <序号> 切换；/new 开新的。",
            lines.join("\n")
        ));
    }

    if let Some(rest) = text.strip_prefix("/use ") {
        let rest = rest.trim();
        let all = core.list_sessions_in_group(&gid).unwrap_or_default();
        let mut mine: Vec<_> = all
            .iter()
            .filter(|s| s.title.starts_with(&title_prefix) || {
                s.title == format!("channel:{}:{}", ch.id, ctx.chat_key)
            })
            .cloned()
            .collect();
        mine.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        // 序号优先；否则按标题片段模糊匹配
        let hit = if let Ok(idx) = rest.parse::<usize>() {
            mine.get(idx).cloned().map(|s| s.id).map(|id| (id, rest.to_string()))
        } else {
            mine.iter()
                .find(|s| {
                    s.title
                        .strip_prefix(&title_prefix)
                        .map(|t| t.contains(rest))
                        .unwrap_or(false)
                })
                .map(|s| (s.id.clone(), rest.to_string()))
        };
        return match hit {
            Some((sid, label)) => match core.bind_flow_session(&ch.id, ctx.chat_key, &sid) {
                Ok(()) => Some(format!("已切换到会话 {label}（{}）。", short_id(&sid))),
                Err(e) => Some(format!("切换失败：{e}")),
            },
            None => Some(format!("没找到会话「{rest}」。/sessions 看全部。")),
        };
    }

    None
}

/// 流内会话的默认展示名（无自定义标题时用创建时间区分）
fn default_flow_session_label(created_at: &str) -> String {
    // ISO 时间取到分位（2026-11-01T15:30:00 → 11-01 15:30）
    let chars: Vec<char> = created_at.chars().collect();
    if chars.len() >= 16 {
        chars[5..16].iter().collect()
    } else {
        created_at.to_string()
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// 运行收束事件 → 纯文本回帖（telegram / napcat / qqbot 三个适配器共用）。
pub(crate) fn flatten_statements(payload: &serde_json::Value, max_chars: usize) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Some(list) = payload.get("statements").and_then(|v| v.as_array()) {
        for s in list {
            let tag = s.get("tag").and_then(|v| v.as_str()).unwrap_or("报告");
            let text = s.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if text.trim().is_empty() {
                continue;
            }
            lines.push(format!("【{tag}】{text}"));
        }
    }
    let mut text = lines.join("\n");
    if text.trim().is_empty() {
        text = "（本轮无收束输出）".into();
    }
    text.chars().take(max_chars).collect()
}

// ---------------------------------------------------------------- 通道消息统一执行

/// 通道入站消息的执行上下文：**组绑定 → 会话复用 → 事件订阅 → 执行 → 组还原**。
/// 六个内置适配器（telegram / qqbot / napcat / discord / slack / matrix）共用这一段，
/// 避免「切组」与「会话键复用」两处语义在各自的适配器里各写一遍而慢慢漂移。
///
/// 用法：`begin` 建立上下文并拿到事件订阅（**先订阅再执行**，确保不漏收束事件）
/// → `spawn_reply` 挂回帖 → `run` 执行本轮 → await 回帖任务。
///
/// 注意：上下文本身可 Clone（回帖任务与执行方各持一份），
/// 但 `broadcast::Receiver` 不可 Clone，故它由 `begin` 单独交出。
#[derive(Clone)]
pub(crate) struct ChannelRun {
    pub(crate) core: std::sync::Arc<exm_core::Core>,
    pub session_id: String,
    pub(crate) prev_group: String,
    pub(crate) prev_single: Option<String>,
    pub(crate) switched: bool,
    /// 平台 kind（能力矩阵判定用）
    pub kind: String,
    /// 本轮能力边界（通道配置在 begin 时快照，执行期不变）
    pub(crate) scope: exm_core::types::ToolScope,
    /// 自动任务来源标记（None = 人工触发的平台消息；"auto:video" = 主动观看等系统自动内容）
    pub origin: Option<&'static str>,
    /// 聊天流定位（begin 时快照；run 落库后回写流预览用）
    pub(crate) flow: Option<(String, String)>,
}

impl ChannelRun {
    /// 账号绑定目标切换（单智能体或组）+ 会话键复用（`channel:<通道id>:<会话键>`）；
    /// 返回 (上下文, 运行事件订阅)
    pub(crate) async fn begin(
        core: &std::sync::Arc<exm_core::Core>,
        ch: &Channel,
        chat_key: &str,
    ) -> Option<(Self, tokio::sync::broadcast::Receiver<CoreEvent>)> {
        let prev_group = core.active_group();
        let prev_single = core.registry().active_single();
        // 绑定目标缺省回落单智能体默认体（machina），未配置单智能体时维持当前上下文
        let target = ch
            .group
            .clone()
            .unwrap_or_else(|| format!("{SINGLE_PREFIX}{}", default_single_id(core)));
        let switched = switch_context_to(core, &target);
        let scope = core.registry().active_scope();
        // 聊天流（chat flow）会话定位：流绑定优先——不主动切换时固定复用
        // 同一会话（跨轮上下文与记忆自然延续）。旧数据（按 title 寻址的存量
        // 通道会话）自动认领迁移到流上，行为无缝升级。
        let title = format!("channel:{}:{chat_key}", ch.id);
        let session = match core.flow_active_session(&ch.id, chat_key) {
            // 绑定有效且仍在当前交互范围内（通道换组后旧绑定会话不跨组复用）
            Some(sid) => core
                .store
                .get_session(&sid)
                .ok()
                .flatten()
                .filter(|s| s.group_id == scope),
            None => None,
        };
        let session = match session {
            Some(s) => Some(s),
            None => {
                // 流无有效绑定：认领旧 title 会话（升级迁移），否则开新会话
                let legacy = core
                    .list_sessions_in_group(&scope)
                    .ok()
                    .and_then(|list| list.into_iter().find(|s| s.title == title));
                let s = legacy.or_else(|| core.create_session(&title).ok())?;
                let _ = core.bind_flow_session(&ch.id, chat_key, &s.id);
                Some(s)
            }
        };
        let Some(session) = session else {
            // 会话建立失败也别把交互上下文留在别人身上
            if switched {
                restore_context(core, &prev_group, &prev_single);
            }
            return None;
        };
        // 记忆隔离即时生效：通道每次执行前把分区键同步到会话；
        // 关闭隔离时清空分区，立即回到同组共享记忆。
        let partition = ch.memory_partition(&scope);
        let _ = core
            .store
            .set_session_memory_partition(&session.id, partition.as_deref());
        let rx = core.subscribe();
        Some((
            ChannelRun {
                core: core.clone(),
                session_id: session.id,
                prev_group,
                prev_single,
                switched,
                kind: ch.kind.clone(),
                scope: ch.tool_scope(),
                origin: None,
                flow: Some((ch.id.clone(), chat_key.to_string())),
            },
            rx,
        ))
    }

    /// 标记本轮为系统自动任务（落库消息带来源，展示层区分于真人消息）
    pub(crate) fn with_origin(mut self, origin: &'static str) -> Self {
        self.origin = Some(origin);
        self
    }

    /// 收束事件 → 回帖文本（非收束事件返回 None；已按平台上限截断）
    pub(crate) fn take_reply(&self, evt: &CoreEvent, max_chars: usize) -> Option<String> {
        match evt.kind.as_str() {
            "run.finished" => Some(flatten_statements(&evt.payload, max_chars)),
            "run.error" => {
                let msg = evt
                    .payload
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("运行失败");
                Some(format!("【警告】{msg}").chars().take(max_chars).collect())
            }
            // 计划待审批（spec plan-mode 跨端呈现）：文本摘要 + 快捷审批指引。
            // 各 IM 适配器把本回复推回通道，即完成「推送文本摘要 + 快捷审批回复方式」
            "plan.pending" => {
                let count = evt
                    .payload
                    .get("count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let items = self.core.plan_list(&self.session_id);
                let mut lines = vec![format!("📋 计划待审批（{count} 条）：")];
                for (i, it) in items.iter().take(8).enumerate() {
                    let target: String = it.target.chars().take(60).collect();
                    lines.push(format!("{}. [{}] {} {}", i + 1, it.action, it.tool, target));
                }
                if items.len() > 8 {
                    lines.push(format!("… 共 {} 条", items.len()));
                }
                lines.push("回复「plan:yes」批准执行，「plan:no」拒绝保留".into());
                Some(lines.join("\n").chars().take(max_chars).collect())
            }
            "plan.applied" => {
                let ok = evt
                    .payload
                    .get("ok")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let target = evt
                    .payload
                    .get("target")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                Some(format!(
                    "{} 计划条目已执行：{target}",
                    if ok { "✅" } else { "❌" }
                ))
            }
            _ => None,
        }
    }

    /// 执行本轮；返回后交互上下文已还原（Drop guard 覆盖提前返回 / 任务中止）
    pub(crate) async fn run(self, text: &str) {
        // 聊天流预览回写（列表展示最新一条；失败不影响执行）
        if let Some((ch_id, chat_key)) = &self.flow {
            let _ = self
                .core
                .touch_flow_message(ch_id, chat_key, text.trim());
        }
        let _restore = GroupRestore {
            core: self.core.clone(),
            prev_group: self.prev_group.clone(),
            prev_single: self.prev_single.clone(),
            switched: self.switched,
        };
        // 计划快捷审批（spec plan-mode 跨端审批）：plan:yes / plan:no 直达裁决，
        // 不进模型轮次；无待审清单时提示当前状态
        let trimmed = text.trim();
        if trimmed == "plan:yes" || trimmed == "plan:no" {
            let approve = trimmed == "plan:yes";
            let items = self.core.plan_list(&self.session_id);
            if items.is_empty() {
                self.core.emit_run_error(
                    &self.session_id,
                    "【报告】当前无待审批计划（清单为空或已处理）。",
                );
                return;
            }
            if approve {
                match self.core.plan_approve(&self.session_id).await {
                    Ok(rs) => {
                        let ok_n = rs.iter().filter(|(_, r)| r.ok).count();
                        let fail_n = rs.len() - ok_n;
                        self.core.emit_run_error(
                            &self.session_id,
                            &format!("【报告】计划已批准并回放：{ok_n} 条成功，{fail_n} 条失败。"),
                        );
                    }
                    Err(e) => {
                        self.core.emit_run_error(
                            &self.session_id,
                            &format!("【警告】计划回放失败：{e}"),
                        );
                    }
                }
            } else {
                self.core.plan_reject(&self.session_id);
                self.core.emit_run_error(
                    &self.session_id,
                    "【报告】计划已拒绝（清单保留，可继续修改后再次生成）。",
                );
            }
            return;
        }
        // 周期 token 配额（外部来源会话）：耗尽即拒绝新轮次并经 run.error 回帖提示；
        // 组内派发会话无来源登记，天然不受限（设计 D10）
        let limits = self.core.config().limits.clone();
        if limits.enabled && limits.quota_tokens > 0 {
            if let Some((_subject, _role)) = self.core.session_origin(&self.session_id) {
                let used = self.core.session_tokens_estimate(&self.session_id);
                if used >= limits.quota_tokens {
                    self.core.emit_run_error(
                        &self.session_id,
                        &format!(
                            "【报告】周期 token 配额已耗尽（已用 {used} / 上限 {}）：本轮拒绝执行，请联系管理员调整配额。",
                            limits.quota_tokens
                        ),
                    );
                    return;
                }
            }
        }
        let _ = limits;
        if let Err(e) = self
            .core
            .chat_scoped_origin(&self.session_id, text, self.scope, self.origin)
            .await
        {
            eprintln!("[channel:{}] 执行失败：{e}", self.session_id);
        }
    }
}

/// 交互上下文兜底还原：任务正常结束、提前返回、甚至被 abort 时都执行 Drop，
/// 避免视频轮询等后台任务超时中止后把全局激活组/单智能体留在通道绑定目标上。
struct GroupRestore {
    core: std::sync::Arc<exm_core::Core>,
    prev_group: String,
    prev_single: Option<String>,
    switched: bool,
}

impl Drop for GroupRestore {
    fn drop(&mut self) {
        if self.switched {
            restore_context(&self.core, &self.prev_group, &self.prev_single);
        }
    }
}

/// 还原到之前的交互上下文（组 + 单智能体快照）
pub(crate) fn restore_context(
    core: &exm_core::Core,
    prev_group: &str,
    prev_single: &Option<String>,
) {
    if let Some(id) = prev_single {
        let _ = core.registry().set_active_single(Some(id));
    } else {
        if core.registry().single_mode() {
            let _ = core.registry().set_active_single(None);
        }
        let _ = core.registry().set_active_group(prev_group);
    }
}

/// 通道缺省绑定目标：默认单智能体 machina；不存在时取任意首个单智能体；都无则回落组模式
pub(crate) fn default_single_id(core: &exm_core::Core) -> String {
    if core.registry().single("machina").is_some() {
        return "machina".into();
    }
    core.registry()
        .list_singles()
        .first()
        .map(|d| d.identifier.clone())
        .unwrap_or_default()
}

/// 挂本轮回帖任务：订阅运行事件，收束即用 `send` 把文本发回原会话（发完或流断即退出）。
/// 各适配器只需给出发送闭包（平台 API 差异全部封在这里面）。
/// 媒体产物条目（run.finished artifacts → 平台投递）
#[derive(Debug, Clone)]
pub(crate) struct MediaItem {
    /// image | file | voice
    pub kind: String,
    /// 绝对路径（spawn_reply 已从工作区相对路径解析）
    pub path: std::path::PathBuf,
    pub caption: String,
}

pub(crate) fn spawn_reply<F, Fut, M, MFut>(
    run: &ChannelRun,
    rx: tokio::sync::broadcast::Receiver<CoreEvent>,
    max_chars: usize,
    send: F,
    send_media: M,
) -> tokio::task::JoinHandle<()>
where
    F: Fn(String) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
    M: Fn(MediaItem) -> MFut + Send + 'static,
    MFut: std::future::Future<Output = bool> + Send,
{
    let run = run.clone();
    tokio::spawn(async move {
        let mut rx = rx;
        while let Ok(evt) = rx.recv().await {
            if evt.session_id != run.session_id {
                continue;
            }
            // 产物投递（组 6.2/6.3）：能力矩阵判定直发或降级并注
            let mut media_notes: Vec<String> = Vec::new();
            if let Some(list) = evt.payload.get("artifacts").and_then(|v| v.as_array()) {
                for a in list {
                    let kind = a
                        .get("kind")
                        .and_then(|x| x.as_str())
                        .unwrap_or("file")
                        .to_string();
                    let rel = a
                        .get("path")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    if rel.is_empty() {
                        continue;
                    }
                    let caption = a
                        .get("caption")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let abs = run.core.config().workspace_root.join(&rel);
                    let item = MediaItem {
                        kind: kind.clone(),
                        path: abs,
                        caption,
                    };
                    match media_fallback(&run.kind, &kind) {
                        MediaFallback::Native => {
                            if !send_media(item.clone()).await {
                                media_notes.push(format!(
                                    "📎 {}：{rel}（直发失败，可在工作区查看）",
                                    item.caption
                                ));
                            }
                        }
                        MediaFallback::LinkNote => {
                            media_notes.push(format!("📎 {}：{rel}（工作区内）", item.caption));
                        }
                    }
                }
            }
            if let Some(text) = run.take_reply(&evt, max_chars) {
                // 长回复分段推送（组 6.6）：按平台上限逐段送达，顺序保持
                let segments = split_reply(&text, max_chars);
                let total = segments.len();
                for (i, seg) in segments.into_iter().enumerate() {
                    // 产物注记并入最后一段（文本主体保持整洁）
                    if i + 1 == total && !media_notes.is_empty() {
                        send(format!("{seg}\n{}", media_notes.join("\n"))).await;
                    } else {
                        send(seg).await;
                    }
                }
                break;
            }
        }
    })
}

/// 入站媒体：下载保存到 `.exmachina/inbox/{通道id}/`，返回（绝对路径, 文件名）
pub(crate) async fn save_inbound_media(
    core: &exm_core::Core,
    ch: &Channel,
    name: &str,
    bytes: Vec<u8>,
) -> Option<String> {
    let dir = core
        .config()
        .workspace_root
        .join(".exmachina")
        .join("inbox")
        .join(&ch.id);
    tokio::fs::create_dir_all(&dir).await.ok()?;
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let file = dir.join(format!("{}-{safe}", exm_core::types::now_ms()));
    tokio::fs::write(&file, &bytes).await.ok()?;
    Some(file.display().to_string())
}

/// 通用下载（入站媒体用）：带超时；失败 None
pub(crate) async fn download_bytes(url: &str) -> Option<Vec<u8>> {
    reqwest::Client::new()
        .get(url)
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await
        .ok()?
        .bytes()
        .await
        .ok()
        .map(|b| b.to_vec())
}

/// 带鉴权下载（slack url_private 等）：Bearer token
pub(crate) async fn download_bytes_auth(url: &str, token: &str) -> Option<Vec<u8>> {
    reqwest::Client::new()
        .get(url)
        .bearer_auth(token)
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await
        .ok()?
        .bytes()
        .await
        .ok()
        .map(|b| b.to_vec())
}

/// 入站媒体注入会话：图片走多模态暂存（data URL），文件/语音注入路径说明。
/// 追加到消息文本后由模型一并感知。返回注入说明文本。
pub(crate) fn stage_inbound_media(
    core: &exm_core::Core,
    session_id: &str,
    saved: &[(String, String, String)], // (kind, 绝对路径, 文件名)
) -> String {
    let mut notes = String::new();
    for (kind, path, name) in saved {
        match kind.as_str() {
            "image" => {
                if let Ok(bytes) = std::fs::read(path) {
                    use base64::Engine as _;
                    let data = format!(
                        "data:image/png;base64,{}",
                        base64::engine::general_purpose::STANDARD.encode(bytes)
                    );
                    exm_core::image_stash::stage(session_id, vec![data]);
                    notes.push_str(&format!("\n（图片附件 {name} 已注入，可直接描述画面内容）"));
                } else {
                    notes.push_str(&format!("\n（图片附件 {name} 读取失败）"));
                }
            }
            "voice" => {
                notes.push_str(&format!("\n（语音附件 {name} 已保存：{path}，可按需处理）"));
            }
            _ => {
                notes.push_str(&format!("\n（文件附件 {name} 已保存：{path}）"));
            }
        }
    }
    let _ = core;
    notes
}
