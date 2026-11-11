//! channel 域分文件：入站闸门（身份+群聊唤醒）+ 旁听落库（ambient）+ 流会话定位。

use super::*;

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

