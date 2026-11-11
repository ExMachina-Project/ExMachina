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
    /// 流展示名（群名 / 对端昵称；适配器尽力提供，None = 未知）——
    /// 建流/补名用（旁听路径建流也有名可用），见 ensure_flow_named
    pub flow_name: Option<&'a str>,
    /// 本条消息自带的已落盘媒体（kind, 绝对路径, 文件名）——旁听路径的附件
    /// 回填只挂本条消息的媒体（napcat 闸门前预下载并携带）。主链注入媒体
    /// 不走此字段（各适配器自行下载暂存）。无旁听预下载的适配器传 `&[]`。
    pub media: &'a [(String, String, String)],
}

pub(crate) enum GateDecision {
    /// 放行，携带消毒后文本——消毒在身份判定之后执行（inbound-sanitization 分层要求：
    /// 已过身份判定的消息仍必经消毒），携带于返回值使调用方无法绕过消毒结果
    Allow(String),
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
    // 防爆破（inbound-sanitization）：单通道单账号连续失败 N 次（默认 5）进入锁定
    // 窗口（默认 15 分钟）——锁定期内正确码也拒绝并提示剩余时间；锁定有审计事件
    let trimmed = ctx.text.trim();
    if let Some(code) = trimmed
        .strip_prefix("/pair ")
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        let idcfg = &core.config().identity;
        let pkey = format!("pair:{}:{}", ch.id, ctx.external_id);
        let now = exm_core::types::now_ms();
        let lockout_ms = idcfg.pairing_lockout_secs * 1000;
        let already = crate::limits::RateLimiter::count(&pkey, lockout_ms, now);
        if idcfg.pairing_max_failures > 0 && already >= idcfg.pairing_max_failures as usize {
            core.emit_sanitize_finding(serde_json::json!({
                "kind": "pairing.lockout",
                "channel": ch.id,
                "externalId": ctx.external_id,
                "failures": already,
                "lockoutRemainSecs": lockout_ms / 1000,
            }));
            return GateDecision::Deny(format!(
                "【报告】尝试过于频繁：已锁定，请约 {} 分钟后再试。",
                lockout_ms / 60000
            ));
        }
        return match core.redeem_pairing(code, &ch.id, ctx.external_id, ctx.display_name, "member")
        {
            Ok(idn) => {
                // 兑换成功清空失败计数（成功后不应残留历史失败导致误锁）
                crate::limits::RateLimiter::reset(&pkey);
                GateDecision::Deny(format!(
                    "【报告】绑定成功：{}（角色 {}）。现在可以正常对话。",
                    idn.display_name, idn.role
                ))
            }
            Err(e) => {
                let allowed = crate::limits::RateLimiter::check(
                    &pkey,
                    lockout_ms,
                    idcfg.pairing_max_failures.max(1),
                    now,
                );
                if !allowed {
                    core.emit_sanitize_finding(serde_json::json!({
                        "kind": "pairing.lockout",
                        "channel": ch.id,
                        "externalId": ctx.external_id,
                        "failures": idcfg.pairing_max_failures,
                        "lockoutRemainSecs": lockout_ms / 1000,
                    }));
                    GateDecision::Deny(format!(
                        "【报告】连续失败 {} 次，已锁定约 {} 分钟：期间即使码正确也不受理。",
                        idcfg.pairing_max_failures,
                        lockout_ms / 60000
                    ))
                } else {
                    GateDecision::Deny(format!("【报告】绑定失败：{e}"))
                }
            }
        };
    }

    // ①' 聊天流（chat flow）会话管理：一个群 / 一个私聊各是一条流，流固定复用
    // 一个活跃会话；不切换就一直在同一会话里聊，跨轮上下文自然延续。
    if let Some(reply) = flow_command(core, ch, ctx).await {
        return GateDecision::Deny(reply);
    }

    // ② 群聊唤醒门控：群内仅提及 / 回复触发；私聊豁免。
    // 主动回复（实验性，通道级开关）：未 @ 但通道开了 proactive_reply 时，
    // 经三级漏斗（回复率采样 → 探活词表 → 决策模型）判断是否主动回应
    // （proactive.rs；任何一级失败都保守忽略——主动回复是增益不是义务）。
    // 被拦下的未 @ 消息不再完全丢弃：落流会话作为「旁听」上下文（imctx: 标记，
    // 不触发轮次）——「仅 @ 回复」模式下 Agent 被点名时仍能接住群聊话题。
    if ch.gate_enabled(core) && ctx.is_group && !ctx.mentioned {
        let proactive = ch.proactive_enabled()
            && super::proactive::should_proactively_reply(core, ch, ctx).await;
        if !proactive {
            record_ambient(core, ch, ctx).await;
            return GateDecision::Ignore;
        }
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
    // ⑤ 入站消毒（inbound-sanitization）：身份判定之后、放行之前——
    // 检出注入模式即中性化（Finding 仅审计不阻断）。此时流尚未绑定会话
    //（ChannelRun::begin 才发生），审计事件 payload 内嵌通道/账号定位；
    // session_id 留空走广播（WS 实时可见），不落事件库（无所属会话可挂）。
    // 规则开关随 config（security.sanitizeRules 单项可关）
    let (clean_text, findings) =
        crate::sanitize::sanitize_with(ctx.text, Some(&core.config().security.sanitize_rules));
    if !findings.is_empty() {
        crate::sanitize::note_findings(&findings);
        core.emit_sanitize_finding(serde_json::json!({
            "channel": ch.id,
            "externalId": ctx.external_id,
            "patterns": findings.iter().map(|f| f.rule).collect::<Vec<_>>(),
            "actions": findings.iter().map(|f| f.action).collect::<Vec<_>>(),
        }));
    }
    GateDecision::Allow(clean_text)
}

/// 群聊旁听落库（ambient context）：门控拦下的未 @ 群消息以 `imctx:` 标记
/// 追加到流会话——不触发轮次，仅作为「仅 @ 回复」模式下被点名时的群聊上下文。
///
/// 语义约定（docs/协议与契约.md）：
/// - 落 User 角色但标记 `imctx:发送者`，与真实输入（`im:`）区分；
/// - collect 不并入 goal（群聊旁听是背景，不是本轮任务输入）；
/// - 模型侧渲染「群聊·发送者」，展示层署名 + 「群聊」标签；
/// - 文本仍经入站消毒——旁听会进入未来轮次的模型上下文，不能成为注入面。
///
/// 媒体附件（ambient-media-attach）：适配器在闸门前已把旁听媒体下载落盘
/// （napcat 旁听窗口等），此处把附件元数据随占位消息回填——部署实测缺陷：
/// 群里未 @ 的纯图走旁听路径，图片文件落了盘（inbox 有文件）、占位文本
/// 落了库（「（用户发来附件：图片 x1）」），但附件元数据从未随消息存储，
/// 聊天流里永远看不到图。只回填**本条消息自带**的媒体（`ctx.media`）；
/// 窗口条目不 take（TTL 窗口仍供后续 @ 轮次取走注入模型）。
pub(crate) async fn record_ambient(core: &exm_core::Core, ch: &Channel, ctx: &InboundCtx<'_>) {
    let text = ctx.text.trim();
    if text.is_empty() {
        return;
    }
    let Some(sid) =
        ensure_flow_session(core, ch, ctx.chat_key, ctx.flow_name, ctx.is_group)
    else {
        return;
    };
    let (clean, findings) = crate::sanitize::sanitize_with(
        text,
        Some(&core.config().security.sanitize_rules),
    );
    if !findings.is_empty() {
        crate::sanitize::note_findings(&findings);
        core.emit_sanitize_finding(serde_json::json!({
            "channel": ch.id,
            "externalId": ctx.external_id,
            "patterns": findings.iter().map(|f| f.rule).collect::<Vec<_>>(),
            "actions": findings.iter().map(|f| f.action).collect::<Vec<_>>(),
            "ambient": true,
        }));
    }
    // 发送者标记沿用六适配器同口径（im:名(id) → imctx:名(id)）；
    // 无署名时仍保留 imctx: 前缀（前缀本身就是旁听语义，不可丢）
    let sender = crate::channel_util::sender_origin(
        ctx.is_group,
        ctx.display_name,
        ctx.external_id,
    )
    .map(|m| m.trim_start_matches("im:").to_string())
    .unwrap_or_default();
    let origin = format!("imctx:{sender}");
    let msg = core.store.add_message(
        &sid,
        exm_core::types::MessageRole::User,
        Some(origin.as_str()),
        vec![exm_core::types::Statement::new(
            exm_core::types::SpeechTag::要求,
            clean.as_str(),
        )],
    );
    // 旁听媒体随占位消息回填展示层元数据（不注入模型——旁听不触发轮次；
    // 模型注入走 @ 轮次 take 旁听媒体窗口的既有链路）。
    // 只挂本条消息自带的媒体（ctx.media，napcat 闸门前预下载携带）——
    // 曾用全窗快照回填：窗口内每条新旁听消息（含纯文本）都复读全部近期
    // 图片，聊天流里同图在每条消息上重复出现（部署实测缺陷）。
    if let Ok(m) = &msg {
        let rels: Vec<exm_core::types::InboundAttachment> = ctx
            .media
            .iter()
            .map(|a| {
                let rel = std::path::Path::new(&a.1)
                    .strip_prefix(&core.config().workspace_root)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| a.1.clone());
                exm_core::types::InboundAttachment {
                    kind: a.0.clone(),
                    path: rel,
                    name: a.2.clone(),
                }
            })
            .collect();
        if !rels.is_empty() {
            let _ = core.store.attach_inbound(&sid, &m.id, rels);
        }
    }
    // 流预览跟随最新群消息（控制台流列表展示「群里刚说了什么」）
    let _ = core.touch_flow_message(&ch.id, ctx.chat_key, &clean);
}

/// 流会话定位/建立（旁听用，与 `ChannelRun::begin` 同口径）：
/// 有效绑定优先；未绑定则按 title 认领旧会话（升级迁移）或新建并回写绑定。
/// 不切全局交互上下文——旁听只是记账，不应翻转打开中的工作目标。
fn ensure_flow_session(
    core: &exm_core::Core,
    ch: &Channel,
    chat_key: &str,
    flow_name: Option<&str>,
    is_group: bool,
) -> Option<String> {
    if let Some(sid) = core.flow_active_session(&ch.id, chat_key) {
        // 流名回填（空填不覆盖）：流存在但名为空时补（begin_named 同口径）
        if let Some(n) = flow_name.filter(|n| !n.trim().is_empty()) {
            core.ensure_flow_named(&ch.id, chat_key, n, is_group);
        }
        return Some(sid);
    }
    let target = ch
        .group
        .clone()
        .unwrap_or_else(|| format!("{SINGLE_PREFIX}{}", default_single_id(core)));
    let (single, group) = parse_target(&target);
    let scope = match (single, group) {
        (Some(id), _) => format!("{SINGLE_PREFIX}{id}"),
        (_, Some(g)) => g,
        _ => core.active_group(),
    };
    let title = format!("channel:{}:{chat_key}", ch.id);
    // 原子版（竞态 P1-3）：查绑定→建会话→回写绑定整体临界区——
    // 并发同流消息不再各建会话（孤儿+绑定互抹）
    core.ensure_flow_session_atomic(&ch.id, chat_key, &scope, &title, flow_name, is_group)
}

/// 聊天流（chat flow）固定单会话：一条流恒定复用同一个会话，不再有流内
/// 会话区分（`/new` `/sessions` `/use` 已退役——多会话切换单独治理记忆与
/// 上下文，与「聊天流 = 一个外部会话的镜像」的定位冲突；需要全新上下文
/// 时由管理者在控制台归档/重建，通道侧不提供切换入口）。
///
/// 旧指令仍拦截并回执说明（群里发了不能没反应），但不做任何状态变更。
/// 返回 Some(reply) = 指令已处理（Deny 语义回帖）；None = 非流指令，放行后续闸门。
async fn flow_command(
    core: &exm_core::Core,
    ch: &Channel,
    ctx: &InboundCtx<'_>,
) -> Option<String> {
    let text = ctx.text.trim();
    // /reset：重置聊天上下文——旧会话归档保留（控制台归档区可回看），流绑定
    // 切到新会话，后续消息从零开始。「聊天流固定单会话」退役了 /new /sessions
    // /use，但「干净重开」是真实高频诉求（上下文被长对话灌满 / 话题彻底换轨），
    // 只靠控制台手动归档对通道侧用户不可达。
    if text == "/reset" {
        return Some(reset_flow_session(core, ch, ctx).await);
    }
    let retired = [
        ("/new", "本聊天流固定使用一个会话，不再开新会话。如需干净重开请发 /reset（旧对话归档保留）。"),
        ("/sessions", "本聊天流固定使用一个会话，无流内会话列表。"),
        ("/use", "本聊天流固定使用一个会话，无需切换。"),
    ];
    for (cmd, reply) in retired {
        if text == cmd || text.starts_with(&format!("{cmd} ")) {
            // 附当前绑定会话短码，管理者可在控制台按此定位
            let cur = core
                .flow_active_session(&ch.id, ctx.chat_key)
                .map(|sid| sid.chars().take(8).collect::<String>())
                .unwrap_or_else(|| "（尚未创建）".into());
            return Some(format!("{reply}当前会话：{cur}"));
        }
    }
    None
}

/// /reset 落地：归档旧会话 → 新建会话 → 流绑定切换。
/// 会话内消息不动（归档 = archived_at 标记 + 移出默认列表，历史在归档区完整可回看）。
async fn reset_flow_session(core: &exm_core::Core, ch: &Channel, ctx: &InboundCtx<'_>) -> String {
    let old = core.flow_active_session(&ch.id, ctx.chat_key);
    // 无会话可直接开新的（首次消息前 /reset 也成立）
    let Some(old_sid) = old else {
        return match ensure_flow_session(core, ch, ctx.chat_key, ctx.flow_name, ctx.is_group) {
            Some(_) => "✅ 已重置：新建会话就绪，发消息即从零开始。".into(),
            None => "❌ 重置失败：会话创建失败，请稍后重试。".into(),
        };
    };
    // 运行中的轮次先停（旧会话可能在长任务里，不停会把回复落进归档会话）
    let stop_note = match core.stop_session(&old_sid) {
        Ok(n) => format!("（已停止运行中任务，{n} 次工具调用中止）"),
        Err(_) => String::new(),
    };
    // 归档旧会话（幂等；失败不阻断——旧会话保持原样，新会话照常开）
    let archived = core.store.set_session_archived(&old_sid, true).is_ok();
    // 新建同 scope 会话并切换绑定（ensure_flow_session_atomic 会复用绑定，
    // 这里需显式 create + bind：归档会话不再是活跃目标）
    let scope = core
        .store
        .get_session(&old_sid)
        .ok()
        .flatten()
        .map(|s| s.group_id)
        .unwrap_or_else(|| {
            ch.group
                .clone()
                .unwrap_or_else(|| format!("{SINGLE_PREFIX}{}", default_single_id(core)))
        });
    let title = format!("channel:{}:{}", ch.id, ctx.chat_key);
    match core.store.create_session(&title, &scope) {
        Ok(new_s) => {
            let bind = core.bind_flow_session_named(
                &ch.id,
                ctx.chat_key,
                &new_s.id,
                ctx.flow_name,
                Some(ctx.is_group),
            );
            let archive_note = if archived { "旧对话已归档（控制台归档区可回看）" } else { "⚠️ 旧会话归档失败（保留原样），但已切换新会话" };
            match bind {
                Ok(()) => format!(
                    "✅ 已重置{stop_note}。{archive_note}，新会话从零开始。"
                ),
                Err(e) => format!("❌ 重置失败：新会话已建但绑定切换失败（{e}）。旧会话保持归档态，可到控制台恢复。"),
            }
        }
        Err(e) => format!(
            "❌ 重置失败：新会话创建失败（{e}）。{}",
            if archived { "旧会话已归档，可到控制台恢复后重试。" } else { "旧会话未受影响。" }
        ),
    }
}

/// 运行收束事件 → 纯文本回帖（telegram / napcat / qqbot 三个适配器共用）。
/// 收束语句平铺（IM 投递形态）：正文原样逐行拼接，**不添加任何句式前缀**——
/// 是否使用【报告】等标记由模型自己决定，投递层只做拼接与截断。
///
/// 沉默约定（IM silence）：模型按旁听纪律输出 `<沉默/>`（可能独立成行或与
/// 少量文字混行）= 本轮无话可说——投递层整轮丢弃（返回空串，由调用方
/// 决定不发送 / 不记未读），群聊里不再出现「不参与群聊闲聊」这类废话。
///
/// 不在此处截断（防截断丢内容）：分段职责归 `split_reply`（按段落/代码块
/// 边界切），段数封顶归回帖循环。这里仅设单轮总预算天花板（一段上限 ×
/// 24 段）防异常超长（如工具输出裸拼）打爆通道侧。
pub(crate) fn flatten_statements(payload: &serde_json::Value, max_chars: usize) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Some(list) = payload.get("statements").and_then(|v| v.as_array()) {
        for s in list {
            let text = s.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if text.trim().is_empty() {
                continue;
            }
            lines.push(text.to_string());
        }
    }
    let mut text = lines.join("\n");
    if text.trim().is_empty() {
        text = "（本轮无收束输出）".into();
    }
    if is_silence(&text) {
        return String::new();
    }
    let budget = max_chars.saturating_mul(MAX_REPLY_SEGMENTS.max(24));
    text.chars().take(budget).collect()
}

/// 单轮回帖段数默认封顶（IM 防轰炸）：超出部分截断并注明去处。
/// 不再像旧版一刀切 3 段——分段前先经完整预算（见 flatten_statements），
/// 常规长文（数段）可完整送达，只有异常超长才触顶。
pub(crate) const MAX_REPLY_SEGMENTS: usize = 24;

/// 沉默标记判定：语句里出现 `<沉默/>`（允许全角变体与空白）即整轮沉默。
/// 保守口径——只在标记明确出现时丢轮，避免误杀正常回复。
pub(crate) fn is_silence(text: &str) -> bool {
    let t = text.trim();
    t.contains("<沉默/>")
        || t.contains("<沉默 />")
        || t.contains("＜沉默/＞")
        || t.contains("＜沉默 ／＞")
        || t.contains("〈沉默/〉")
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
    /// 本轮消息来源标记（None = 人工触发的平台消息；"auto:video" = 主动观看等系统自动内容；
    /// "im:<名字|id>" = 群聊发送者——落库进消息 agent_id，展示层据此在消息头显示发送者，
    /// 模型侧上下文渲染为「[用户·名字]」区分群聊多人发言）
    pub origin: Option<String>,
    /// 发送者平台外部 ID（approval-ergonomics 通道裁决权限判定：
    /// origin 用户本人或跨通道 admins 才能裁 ok:/no:；非 flow 入口为 None）
    pub external_id: Option<String>,
    /// 聊天流定位（begin 时快照；run 落库后回写流预览用）
    pub(crate) flow: Option<(String, String)>,
    /// 本轮执行完成标记（`run()` 返回时置位）：回帖任务的退出判定输入之一。
    /// 收束事件（run.finished/run.error）在 emit 后微秒级到达，可能先于本标记
    /// 置位——因此回帖任务用「事件 + 定时轮询」双通道观察，绝不对该标记做
    /// 单次等待（notify 单发会因观察时机错过而永久悬挂，测试实测踩过）。
    /// Clone 共享同一实例（回帖任务与执行方各持一份）。
    pub(crate) round_done: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ChannelRun {
    /// 账号绑定目标切换（单智能体或组）+ 会话键复用（`channel:<通道id>:<会话键>`）；
    /// 返回 (上下文, 运行事件订阅)
    pub(crate) async fn begin(
        core: &std::sync::Arc<exm_core::Core>,
        ch: &Channel,
        chat_key: &str,
    ) -> Option<(Self, tokio::sync::broadcast::Receiver<CoreEvent>)> {
        Self::begin_named(core, ch, chat_key, None, None).await
    }

    /// begin 带流名变体（会话管理可用性修复）：适配器把入站事件自带的
    /// 展示名（群名 / 对端昵称）传进来，建流/补名用——流列表不再显示裸
    /// chat_key（QQ 号）。名称只空填不覆盖（ensure_flow_named 语义）。
    pub(crate) async fn begin_named(
        core: &std::sync::Arc<exm_core::Core>,
        ch: &Channel,
        chat_key: &str,
        flow_name: Option<&str>,
        is_group: Option<bool>,
    ) -> Option<(Self, tokio::sync::broadcast::Receiver<CoreEvent>)> {
        let prev_group = core.active_group();
        let prev_single = core.registry().active_single();
        // 绑定目标缺省回落单智能体默认体（machina），未配置单智能体时维持当前上下文
        let target = ch
            .group
            .clone()
            .unwrap_or_else(|| format!("{SINGLE_PREFIX}{}", default_single_id(core)));
        // 切换后校验 + 有限重试（竞态 P1-2 短期缓解）：激活组/单体是进程级全局态，
        // 并发 begin（多通道同时来消息）会互相切走上下文——switch 后立即快照 scope，
        // 与目标不符说明被他人切走：重试抢占（最多 3 次，仍不符则用当前 scope 继续，
        // 消息不丢，只可能落错组——比挂死好）。中期方案：scope 做成 ChannelRun
        // 显式参数，消灭全局切换。
        let mut switched = false;
        let mut scope = core.registry().active_scope();
        for _ in 0..3 {
            switched = switch_context_to(core, &target);
            scope = core.registry().active_scope();
            let expected = expected_scope_of(core, &target);
            if expected.as_deref() == Some(scope.as_str()) {
                break;
            }
            // 被并发切换抢走：重试（下轮 switch 回自己的目标）
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // 聊天流（chat flow）会话定位：流绑定优先——不主动切换时固定复用
        // 同一会话（跨轮上下文与记忆自然延续）。旧数据（按 title 寻址的存量
        // 通道会话）自动认领迁移到流上，行为无缝升级。
        // 原子版（竞态 P1-3）：查绑定→建会话→回写绑定整体临界区——并发同流
        // 消息不再各建会话（孤儿+绑定互抹）。组过滤在外层做（换组后旧绑定
        // 会话不跨组复用，原子版内组不符时按 None 处理重建）。
        let title = format!("channel:{}:{chat_key}", ch.id);
        let session = {
            let sid = core.ensure_flow_session_atomic(
                &ch.id,
                chat_key,
                &scope,
                &title,
                flow_name,
                is_group.unwrap_or(false),
            )?;
            core.store
                .get_session(&sid)
                .ok()
                .flatten()
                .filter(|s| s.group_id == scope)
        };
        let Some(session) = session else {
            // 绑定会话跨组不符：原子版内已复用绑定，但 scope 过滤后无效——
            // 新开本组会话并改绑（保守路径：罕见，通道换组才触发）。
            // create_session 落当前激活组（上面 switch 后即 scope）。
            let s = core.create_session(&title).ok()?;
            let _ = core.bind_flow_session_named(&ch.id, chat_key, &s.id, flow_name, is_group);
            // 建失败也别把交互上下文留在别人身上
            if core.store.get_session(&s.id).ok().flatten().is_none() {
                if switched {
                    restore_context(core, &prev_group, &prev_single);
                }
                return None;
            }
            return Self::finish_begin(
                core,
                ch,
                chat_key,
                s,
                flow_name,
                is_group.unwrap_or(false),
                prev_group,
                prev_single,
                switched,
            )
            .await;
        };
        Self::finish_begin(
            core,
            ch,
            chat_key,
            session,
            flow_name,
            is_group.unwrap_or(false),
            prev_group,
            prev_single,
            switched,
        )
        .await
    }

    /// begin 尾段（会话已定位）：公共装配（流名回填/IM 登记/预算档位/记忆分区/订阅）
    #[allow(clippy::too_many_arguments)] // 装配参数天然多；收拢成结构体反增噪音
    async fn finish_begin(
        core: &std::sync::Arc<exm_core::Core>,
        ch: &Channel,
        chat_key: &str,
        session: exm_core::types::Session,
        flow_name: Option<&str>,
        is_group: bool,
        prev_group: String,
        prev_single: Option<String>,
        switched: bool,
    ) -> Option<(Self, tokio::sync::broadcast::Receiver<CoreEvent>)> {
        let scope = core.registry().active_scope();
        // 流名回填（空填不覆盖）：流已存在但名为空（更早版本建的）时补上
        if let Some(n) = flow_name.filter(|n| !n.trim().is_empty()) {
            core.ensure_flow_named(&ch.id, chat_key, n, is_group);
        }
        // IM 通道会话登记：轮次组装系统提示时注入简洁表达纪律
        // （治理「一次性回复大量内容」：提示层约束 + 通道层截断双保险）
        exm_core::orchestrator::mark_im_session(&session.id);
        // 会话作用域快照（agent-budget-audit）：工具预算档位按通道 capability
        // 联动（chat < tools < computer）;begin 时写入,轮次内不变。
        core.orchestrator()
            .tools
            .set_session_scope(&session.id, ch.tool_scope());
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
                external_id: None,
                flow: Some((ch.id.clone(), chat_key.to_string())),
                round_done: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
            rx,
        ))
    }

    /// 标记本轮消息来源（auto:xxx 系统自动内容 / im:xxx 群聊发送者；
    /// 落库进消息 agent_id，展示层与模型侧按前缀区分语义）
    pub(crate) fn with_origin(mut self, origin: impl Into<String>) -> Self {
        self.origin = Some(origin.into());
        self
    }

    /// 标记发送者平台外部 ID（approval-ergonomics：通道裁决 ok:/no: 权限判定）。
    /// 六适配器在 stamp_session_origin 同位调用（同一个人的两个标记）。
    pub(crate) fn with_external_id(mut self, external_id: impl Into<String>) -> Self {
        self.external_id = Some(external_id.into());
        self
    }

    /// 收束事件 → 回帖文本（非收束事件返回 None；已按平台上限截断）
    pub(crate) fn take_reply(&self, evt: &CoreEvent, max_chars: usize) -> Option<String> {
        match evt.kind.as_str() {
            "run.finished" => {
                let text = flatten_statements(&evt.payload, max_chars);
                // 沉默约定（IM silence）：模型宣告无话可说 → None（不投递），
                // 所有直调方（spawn_reply / rss / video 插件）统一免发送。
                if text.trim().is_empty() {
                    None
                } else {
                    Some(text)
                }
            }
            "run.error" => {
                let msg = evt
                    .payload
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("运行失败");
                // 消息自带句式前缀（【报告】/【警告】等）时不再包裹——
                // emit_run_error 的调用方文案已按语言纪律定稿，二次包裹会产生
                // 「【警告】【报告】…」双层前缀（部署实测）
                let already = msg.starts_with('【')
                    && msg.chars().skip(1).take(4).any(|c| c == '】');
                let text = if already {
                    msg.to_string()
                } else {
                    format!("【警告】{msg}")
                };
                Some(text.chars().take(max_chars).collect())
            }
            // 工具预算熔断内联提示（agent-budget-audit 3.2）：预算耗尽的首次拦截
            // 内联一条警告（用户感知「为什么停了」）；后续同轮再拦截不重复推——
            // 模型收束前可能连续多次尝试，逐条推送会刷屏。
            "tool.budget" => {
                let reason = evt
                    .payload
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("工具调用预算已耗尽");
                let key = format!("budget-notified:{}", evt.session_id);
                if self.core.consume_once_flag(&key) {
                    Some(format!("【警告】{reason}").chars().take(max_chars).collect())
                } else {
                    None
                }
            }
            // 计划待审批（spec plan-mode 跨端呈现）：文本摘要 + 快捷审批指引。
            // 各 IM 适配器把本回复推回通道，即完成「推送文本摘要 + 快捷审批回复方式」
            "plan.pending" => {                let count = evt
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
            // 审批单待裁决（approval-ergonomics 跨端呈现）：与 plan.pending 同位。
            // 推单号 + 命令摘要 + 裁决用法;单号支持前缀匹配（完整 8 位或其唯一前缀）。
            // 通道裁决只走 once 档（设计决策:档位在文本界面无法可靠表达,升档去控制台）。
            "approval.required" => {
                let id = evt
                    .payload
                    .get("approvalId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let cmd = evt
                    .payload
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let wait = self
                    .core
                    .config()
                    .security
                    .approval_wait_secs;
                let cmd_short: String = cmd.chars().take(120).collect();
                let mut lines = vec![
                    format!("⛔ 审批请求（单号 {id}）："),
                    cmd_short,
                    format!(
                        "{wait} 秒内回复 ok:{id} 批准 / no:{id} 拒绝，或到控制台裁决"
                    ),
                ];
                if cmd.len() > 120 {
                    lines.insert(2, "…（命令已截断）".into());
                }
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
        // 计划/审批快捷指令（plan-mode / approval-ergonomics）：plan:yes / plan:no /
        // ok:<单号> / no:<单号> 直达裁决，不进模型轮次。
        // 适配器已在 begin 前调用 quick_directive_reply 直发回帖；这里保留同一入口
        // 兜底（非适配器调用方 / 旧路径），行为与直发一致。
        if let Some(reply) = quick_directive_reply(
            &self.core,
            Some(&self.session_id),
            self.external_id.as_deref(),
            text,
        )
        .await
        {
            self.core.emit_run_error(&self.session_id, &reply);
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
            .chat_scoped_origin(&self.session_id, text, self.scope, self.origin.as_deref())
            .await
        {
            eprintln!("[channel:{}] 执行失败：{e}", self.session_id);
        }
        // 本轮执行完成（含失败路径）：回帖任务据此在「无收束事件」的异常路径
        //（如预算闸门 bail!）下也能退出（轮询 round_done + 排空广播，见 spawn_reply）。
        self.round_done
            .store(true, std::sync::atomic::Ordering::Release);
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

/// 审批快捷指令解析（approval-ergonomics）：`ok:<单号>` / `no:<单号>`。
/// 单号 = 8 位审批单 id（approval.required 推送文本所带）。返回 (是否批准, 单号)。
/// 大小写不敏感；单号非空即受理（不存在/已处理在裁决点报告,不在解析点挡）。
pub(crate) fn parse_approval_directive(text: &str) -> Option<(bool, String)> {
    let lower = text.trim().to_lowercase();
    let (approve, rest) = if let Some(r) = lower.strip_prefix("ok:") {
        (true, r)
    } else if let Some(r) = lower.strip_prefix("no:") {
        (false, r)
    } else {
        return None;
    };
    let id = rest.trim();
    if id.is_empty() {
        return None;
    }
    Some((approve, id.to_string()))
}

/// 是否快捷裁决指令（plan:yes / plan:no / ok:<单号> / no:<单号>）。
/// 适配器用它在建会话/回帖任务前分流，避免为裁决消息启动模型轮次。
pub(crate) fn is_quick_directive(text: &str) -> bool {
    let t = text.trim();
    t == "plan:yes" || t == "plan:no" || parse_approval_directive(t).is_some()
}

/// 快捷指令直裁（plan-mode / approval-ergonomics 跨端审批）：
/// `plan:yes` / `plan:no` / `ok:<单号>` / `no:<单号>` 直达裁决，不进模型轮次。
/// 返回 Some(回帖文本) = 已处理；None = 非指令。
///
/// 适配器应在 `ChannelRun::begin` / `spawn_reply` 之前调用：同一会话可能仍有
/// 原轮次在跑（审批等待中），再建一个回帖任务会与原任务争抢同一事件流——
/// 裁决回帖被重复投递，且原任务提前退出会吞掉原轮最终答复。
pub(crate) async fn quick_directive_reply(
    core: &std::sync::Arc<exm_core::Core>,
    session_id: Option<&str>,
    external_id: Option<&str>,
    text: &str,
) -> Option<String> {
    let trimmed = text.trim();
    // 计划快捷审批（spec plan-mode 跨端审批）：无待审清单时提示当前状态
    if trimmed == "plan:yes" || trimmed == "plan:no" {
        let approve = trimmed == "plan:yes";
        let Some(session_id) = session_id else {
            return Some("【报告】当前无待审批计划（清单为空或已处理）。".into());
        };
        if core.plan_list(session_id).is_empty() {
            return Some("【报告】当前无待审批计划（清单为空或已处理）。".into());
        }
        if approve {
            return Some(match core.plan_approve(session_id).await {
                Ok(rs) => {
                    let ok_n = rs.iter().filter(|(_, r)| r.ok).count();
                    let fail_n = rs.len() - ok_n;
                    format!("【报告】计划已批准并回放：{ok_n} 条成功，{fail_n} 条失败。")
                }
                Err(e) => format!("【警告】计划回放失败：{e}"),
            });
        }
        core.plan_reject(session_id);
        return Some("【报告】计划已拒绝（清单保留，可继续修改后再次生成）。".into());
    }
    // 审批快捷裁决（approval-ergonomics 跨端审批）：单号支持前缀匹配
    //（完整 8 位或唯一前缀）。权限口径：审批单 origin 用户本人，或任一通道
    // admins 命中（判权跟人走，跨通道）。通道裁决只走 once 档（档位在文本
    // 界面无法可靠表达，升档去控制台）。
    let (approve, id_input) = parse_approval_directive(trimmed)?;
    let pendings = core.approval_list(Some("pending"), 500).unwrap_or_default();
    let hits: Vec<exm_core::types::ApprovalRequest> = pendings
        .into_iter()
        .filter(|a| a.id.starts_with(&id_input))
        .collect();
    let ap = match hits.as_slice() {
        [one] => one.clone(),
        [] => return Some(format!("【报告】审批单 {id_input} 不存在或已处理。")),
        _ => {
            return Some(format!(
                "【报告】单号前缀 {} 命中 {} 张待审单（{}），请补全单号。",
                id_input,
                hits.len(),
                hits.iter().map(|a| a.id.as_str()).collect::<Vec<_>>().join(" / ")
            ))
        }
    };
    let id = ap.id.clone();
    let caller = external_id.unwrap_or("");
    let is_origin_user = ap
        .origin_user
        .as_deref()
        .is_some_and(|u| !u.is_empty() && caller == u);
    let is_admin = !caller.is_empty()
        && super::channels::load_channels(core)
            .iter()
            .any(|c| c.admins.iter().any(|a| a.trim() == caller));
    if !is_origin_user && !is_admin {
        return Some(format!(
            "【报告】审批裁决被拒绝：发送者无权裁决审批单 {id}（仅发起用户或管理员）。"
        ));
    }
    Some(
        match core
            .approval_decide_scoped(
                &id,
                if approve {
                    exm_core::types::ApprovalScope::Once { approve: true }
                } else {
                    exm_core::types::ApprovalScope::Once { approve: false }
                },
            )
            .await
        {
            Ok(r) => {
                let head = if approve { "已批准并执行" } else { "已拒绝" };
                let detail = r.result.as_deref().unwrap_or("");
                let note: String = detail.chars().take(300).collect();
                format!("【报告】审批单 {id} {head}。{note}")
            }
            Err(e) => format!("【警告】审批单 {id} 裁决失败：{e}"),
        },
    )
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

/// 审批推送的富消息钩子载荷（approval-ergonomics 平台交互）：文本摘要 + 审批单号。
/// 平台适配器可据此附加原生交互控件（如 QQ 官方机器人按钮）；钩子返回 false
/// 表示富消息发送失败，调用方回落普通文本（审批单不丢）。
pub(crate) struct ApprovalPrompt {
    pub approval_id: String,
    pub text: String,
}

/// 审批富消息发送钩子：返回是否已成功送达（false = 回落普通文本）。
pub(crate) type ApprovalHook = Box<
    dyn Fn(ApprovalPrompt) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>
        + Send,
>;

// ---------------------------------------------------------------- 回帖发言权（latest-wins）
//
// 重复回复根治（生产实测）：同一会话上并发挂多个回帖任务时（上一轮还在跑，
// 用户又发来新消息——每条入站消息各挂一个 spawn_reply），旧任务不会退出
// （退出只认 run.finished / run.error），旧轮的收束事件会被新旧两个任务
// 同时收到、各发一遍——用户视角「回复发两遍」。
//
// 规则：同会话只允许「最新」回帖任务发言。新任务诞生即接管发言权；被取代
// 的旧任务继续消化事件（保持 channel 容量回收）但一律不发送，直至自己
// 等到收束事件退出。这样每轮回复恰好由一个任务发出一次。
//
// 退出配对（防丢回复，生产实测）：回帖任务的「自己的收束事件」判定 =
// run() 已返回（round_done）后到达的 run.finished/run.error；此时广播里
// 可能还有在途事件（自己的收束紧随 run() 返回，微秒级乱序），故进入
// 「排空模式」把队列里剩余事件全部消化（作为发言人正常发送）再退出。
// 更早到达的收束事件属于前序轮次：代发其回复后继续等自己那轮——旧版
// 在此直接退出，导致追问轮次的回复无人发送（长轮次期间用户追问必现）。
//
// 注：不改收束事件的产生语义，也不合并轮次（排队窗口合并逻辑在 core 侧
// collect_pending_inputs，靠会话串行锁保证），只修「谁有资格发、何时退」。

/// 会话 → 当前发言任务的序号（latest-wins：新任务恒接管；序号在 spawn 前
/// 分配，接管顺序 = 创建顺序 = 轮次排队顺序，与任务体调度顺序无关）。
fn reply_ledger() -> &'static Mutex<HashMap<String, u64>> {
    static SPEAKERS: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    SPEAKERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_reply_seq() -> u64 {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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
    spawn_reply_with_approval(run, rx, max_chars, send, send_media, None)
}

/// `spawn_reply` 的审批富消息变体：审批事件优先交给 `approval_send` 钩子
/// （平台原生控件），失败再走普通文本分段。其余事件行为完全一致。
pub(crate) fn spawn_reply_with_approval<F, Fut, M, MFut>(
    run: &ChannelRun,
    rx: tokio::sync::broadcast::Receiver<CoreEvent>,
    max_chars: usize,
    send: F,
    send_media: M,
    approval_send: Option<ApprovalHook>,
) -> tokio::task::JoinHandle<()>
where
    F: Fn(String) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
    M: Fn(MediaItem) -> MFut + Send + 'static,
    MFut: std::future::Future<Output = bool> + Send,
{
    let run = run.clone();
    // 发言权接管（latest-wins）：spawn 前登记（顺序即创建顺序），
    // 本任务成为该会话当前唯一发言人；旧任务自此静默。
    let my_seq = next_reply_seq();
    reply_ledger()
        .lock()
        .insert(run.session_id.clone(), my_seq);
    tokio::spawn(async move {
        let mut rx = rx;
        let sid = run.session_id.clone();
        let is_speaker = || {
            reply_ledger().lock().get(&sid).copied() == Some(my_seq)
        };
        // round_done 兜底（见模块注释）：无收束事件的异常路径下防悬挂
        let round_done = || run.round_done.load(std::sync::atomic::Ordering::Acquire);
        // 收流容错（竞态 P1-4）：Lagged = 广播环溢出丢了一批旧事件，新事件仍在
        // 环里——必须继续收，退出会让本轮回复静默丢失。Closed = 发送端全关。
        //
        // 观察通道（无单发等待，杜绝错过通知的悬挂）：
        // - 主 select 等事件，同时等 250ms 心跳 tick；
        // - round_done 置位后（tick 观察到，或收束事件到达时已置位）：进入
        //   排空模式，try_recv 消化完在途广播后退出。
        //
        // 退出条件（三选一）：
        // ① 消化到收束事件且 round_done 已置位（进入排空，消化完退出）；
        // ② round_done 置位但无收束事件（异常路径兜底，排空后退出防悬挂）；
        // ③ 已被更新任务取代时到达的收束事件（静默让位退出）。
        let mut draining = false;
        loop {
            let evt = if draining {
                match rx.try_recv() {
                    Ok(evt) => evt,
                    Err(_) => break, // 排空完毕，无收束事件：兜底退出
                }
            } else {
                tokio::select! {
                    r = rx.recv() => match r {
                        Ok(evt) => evt,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    },
                    _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {
                        if round_done() {
                            draining = true;
                        }
                        continue;
                    }
                }
            };
            if evt.session_id != run.session_id {
                continue;
            }
            // 已被更新的回帖任务取代：静默让位——不再发送，见到收束事件即退出
            //（自己的轮次与回复由接管的发言任务负责送达）。
            if !is_speaker() {
                if matches!(evt.kind.as_str(), "run.finished" | "run.error") {
                    break;
                }
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
                // 流未读 +1：智能体在本流产出回复（spec experience-iteration 未读徽标；
                // 用户打开该会话时清零）。一次收束记一条未读（不是按分段计数）。
                if let Some((ch_id, chat_key)) = run.flow.as_ref() {
                    run.core.bump_flow_unread(ch_id, chat_key);
                }
                // 审批事件优先走平台富消息（approval-ergonomics 平台交互）：
                // 如 QQ 官方机器人按钮，点击即产生 ok:/no: 裁决指令；失败回落文本。
                if evt.kind == "approval.required" {
                    if let (Some(hook), Some(id)) = (
                        approval_send.as_ref(),
                        evt.payload.get("approvalId").and_then(|v| v.as_str()),
                    ) {
                        let prompt = ApprovalPrompt {
                            approval_id: id.to_string(),
                            text: text.clone(),
                        };
                        if hook(prompt).await {
                            continue;
                        }
                    }
                }
                // 长回复分段推送（组 6.6）：按平台上限逐段送达，顺序保持。
                // 段数封顶（IM 防轰炸）：异常超长触顶时截断并注明去处——
                // 需要完整长文的场景（文档/代码交付）应走文件产物而非刷屏。
                // 官方机器人被动回复每条入站消息限 ~5 次（群 5 次/5 分钟），
                // 审批/计划推送还共享同一额度——qqbot 单独收紧防后段静默失败。
                let segments = split_reply(&text, max_chars);
                let total = segments.len();
                let max_segments = if run.kind == "qqbot" { 4 } else { MAX_REPLY_SEGMENTS };
                if total <= max_segments {
                    for (i, seg) in segments.into_iter().enumerate() {
                        // 产物注记并入最后一段（文本主体保持整洁）
                        if i + 1 == total && !media_notes.is_empty() {
                            send(format!("{seg}\n{}", media_notes.join("\n"))).await;
                        } else {
                            send(seg).await;
                        }
                    }
                } else {
                    let capped = total - max_segments;
                    for seg in segments.into_iter().take(max_segments - 1) {
                        send(seg).await;
                    }
                    let mut note = media_notes.join("\n");
                    note.truncate(note.char_indices().nth(200).map(|(i, _)| i).unwrap_or(note.len()));
                    send(format!(
                        "（内容过长已截断，略去末尾 {capped} 段；完整回复见工作台会话记录）\n{note}"
                    ))
                    .await;
                }
                // 退出判定（见模块注释）：收束事件 + 本轮已执行完（round_done）
                // ⇒ 自己那轮收束事件已到达。广播里可能还有紧随的在途事件
                //（emit 与 store 微秒级乱序）：进入排空模式消化完再退出，
                // 防止把同批收束答复漏发。更早到达（round_done 未置位）的
                // 收束事件属于前序轮次：代发后继续等自己那轮。
                if matches!(evt.kind.as_str(), "run.finished" | "run.error") && round_done() {
                    draining = true;
                }
            }
        }
        // 退出清理发言权：仅当仍属自己（新任务已接管时不误删）
        {
            let mut l = reply_ledger().lock();
            if l.get(&sid).copied() == Some(my_seq) {
                l.remove(&sid);
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
/// 同时把附件元数据（工作区相对路径）登记进会话暂存，随本轮 user 消息落库
/// （im-inbound-attach）——WebUI 聊天流据此渲染缩略图/下载卡片。
pub(crate) fn stage_inbound_media(
    core: &exm_core::Core,
    session_id: &str,
    saved: &[(String, String, String)], // (kind, 绝对路径, 文件名)
) -> String {
    let mut notes = String::new();
    let mut rels: Vec<exm_core::types::InboundAttachment> = Vec::new();
    for (kind, path, name) in saved {
        // 绝对路径 → 工作区相对路径（展示层经 /api/asset 预览；strip 失败保留原样兜底）
        let rel = std::path::Path::new(path)
            .strip_prefix(&core.config().workspace_root)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.clone());
        rels.push(exm_core::types::InboundAttachment {
            kind: kind.clone(),
            path: rel,
            name: name.clone(),
        });
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
    if !rels.is_empty() {
        exm_core::inbox_stash::stage(session_id, rels);
    }
    notes
}
