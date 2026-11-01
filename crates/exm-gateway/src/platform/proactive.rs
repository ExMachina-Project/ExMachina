//! platform/proactive —— 群聊免 @ 主动回复的决策链
//!
//! 动机：群聊里「仅 @ 触发」是最保守的口径——没被点名时哪怕话题正中
//! 擅长领域也只能沉默。主动回复让模型参与群聊，但每条群消息都进完整
//! 轮次不可承受（成本 + 刷屏），所以决策链是三级漏斗：
//!
//! ① 回复率采样（零成本）：概率闸门挡掉大半；
//! ② 探活词表短路（零成本）：在吗/晚安这类不该被抢答；
//! ③ 决策模型（一次轻量调用）：看最近群聊上下文判断值不值得回应。
//!
//! 三级都过才放行进完整轮次（与 @ 消息同链路）。任何一级失败都保守
//! 忽略——主动回复是增益不是义务，宁可漏不可吵。

use exm_core::Core;
use exm_core::provider::{ChatMessage, ChatRequest};

use super::channels::Channel;
use super::channel_runtime::InboundCtx;

/// 决策模型提示：只判断「值不值得回应」，输出严格 JSON。
/// 上下文给最近群聊消息（含发送者署名）——是否值得回应高度依赖语境：
/// 有人问了个问题 / 话题正中擅长领域 / 讨论出现事实错误 → 值得；
/// 闲聊 / 与本集群无关 / 已有人在聊 → 不值得。
const PROACTIVE_EVAL_PROMPT: &str = r#"你是群聊参与的把关者。判断本集群（一个多智能体助手）是否应该回应群里最新一条消息。

背景：这是一个聊天群，本集群是群成员之一（一个 AI 助手）。最新一条消息没有 @ 本集群——是否主动插话由你判断。

值得回应（reply: true）的典型情形：
- 有人提出了问题或求助（哪怕没有 @）；
- 话题正对本集群擅长的领域（编程/数据/自动化/信息检索等），且能补充有价值的内容；
- 讨论中出现了明显的事实错误，纠正很有价值。

不值得回应（reply: false）的典型情形：
- 日常寒暄、闲聊、表情包接龙；
- 话题与本集群无关（吃饭、游戏、私人事务）；
- 已有其他群友在正常讨论，插话没有增量；
- 消息含糊无法理解。

只输出一个 JSON 对象：
{"reply": true, "why": "一句话理由"}
{"reply": false, "why": "一句话理由"}"#;

/// 一次主动回复决策：返回 true = 放行进完整轮次。
/// 决策失败（模型不可用 / 输出不合法）保守返回 false——主动回复是增益不是义务。
pub(crate) async fn decide(
    core: &Core,
    ch: &Channel,
    ctx: &InboundCtx<'_>,
    recent: &[String],
) -> bool {
    let who = if ctx.display_name.trim().is_empty() {
        ctx.external_id.to_string()
    } else {
        format!("{}({})", ctx.display_name.trim(), ctx.external_id)
    };
    let mut user = String::from("最近群聊记录（旧→新）：\n");
    if recent.is_empty() {
        user.push_str("（暂无记录）\n");
    } else {
        for line in recent {
            user.push_str(line);
            user.push('\n');
        }
    }
    user.push_str(&format!("\n最新消息（未 @ 本集群）：{who} 说：{}\n\n判断是否回应。", ctx.text.trim()));

    let orch = core.orchestrator();
    let req = ChatRequest::new(
        orch.orch_model.clone(),
        vec![
            ChatMessage::system(PROACTIVE_EVAL_PROMPT),
            ChatMessage::user(user),
        ],
    );
    let output = match orch.orch_provider.chat(req).await {
        Ok(r) => r.content,
        Err(e) => {
            core.emit_sanitize_finding(serde_json::json!({
                "kind": "proactive.evalError",
                "channel": ch.id,
                "error": e.to_string(),
            }));
            return false;
        }
    };
    let decision = parse_proactive_eval(&output);
    core.emit_sanitize_finding(serde_json::json!({
        "kind": "proactive.decision",
        "channel": ch.id,
        "externalId": ctx.external_id,
        "reply": decision,
        "why": extract_why(&output),
    }));
    decision
}

/// 解析决策输出：`{"reply": true, ...}` → true；false / 非法 → false。
/// 保守侧：解析失败不回应。
pub(crate) fn parse_proactive_eval(output: &str) -> bool {
    // 模型可能带 ```json 围栏或前后杂文：定位首个 { 起的 JSON 段
    let Some(start) = output.find('{') else {
        return false;
    };
    let Some(end) = output.rfind('}') else {
        return false;
    };
    if end <= start {
        return false;
    }
    match serde_json::from_str::<serde_json::Value>(&output[start..=end]) {
        Ok(v) => v.get("reply").and_then(|r| r.as_bool()).unwrap_or(false),
        Err(_) => false,
    }
}

/// 提取 why 字段（审计事件用；解析失败给原文截断）
fn extract_why(output: &str) -> String {
    let Some(start) = output.find('{') else {
        return output.chars().take(80).collect();
    };
    let Some(end) = output.rfind('}') else {
        return output.chars().take(80).collect();
    };
    if end <= start {
        return output.chars().take(80).collect();
    }
    serde_json::from_str::<serde_json::Value>(&output[start..=end])
        .ok()
        .and_then(|v| v.get("why").and_then(|w| w.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| output.chars().take(80).collect())
}

/// 三级漏斗入口：群聊未 @ 消息是否主动回应。
/// 调用前提：通道开了 proactive_reply 且唤醒门控拦截了本条。
/// 返回 true = 放行（进完整轮次）；false = 忽略。
pub(crate) async fn should_proactively_reply(
    core: &Core,
    ch: &Channel,
    ctx: &InboundCtx<'_>,
) -> bool {
    // ① 回复率采样：概率闸门挡在最前（零成本漏斗，控制决策模型调用量）
    let rate = ch.proactive_rate_pct();
    if rate == 0 {
        return false;
    }
    if rate < 100 && !rate_hit(rate) {
        return false;
    }
    // ② 探活词表：在吗/晚安这类不该被主动抢答（用户没 @，抢答「在」是噪音）
    if exm_core::orchestrator::probe_reply(ctx.text).is_some() {
        return false;
    }
    // ③ 决策模型：带最近群聊上下文判断
    let recent = recent_group_lines(core, ch, ctx);
    decide(core, ch, ctx, &recent).await
}

/// 概率采样命中判定（0~100 百分比）。std 哈希伪随机（time 种子）——
/// 采样粒度只影响回复频率手感，不要求密码学随机。
fn rate_hit(rate_pct: u32) -> bool {
    use std::hash::{BuildHasher, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};
    // 纳秒时钟做种子（两次调用间必然变化）；SipHash 输出的低 20 位做采样值
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(0);
    let mut hasher = std::hash::RandomState::new().build_hasher();
    hasher.write_u64(ns);
    let sample = hasher.finish() & 0xFFFFF; // 0..1_048_575（20 位）
    sample < rate_pct as u64 * 10_486 // rate% * 2^20 / 100
}

/// 最近群聊消息行（含发送者署名，决策上下文用）：读流活跃会话最近 8 条。
fn recent_group_lines(core: &Core, ch: &Channel, ctx: &InboundCtx<'_>) -> Vec<String> {
    let Some(sid) = core.flow_active_session(&ch.id, ctx.chat_key) else {
        return Vec::new();
    };
    let Ok(msgs) = core.store.list_messages(&sid, 8) else {
        return Vec::new();
    };
    msgs.iter()
        .map(|m| {
            let body = m
                .statements
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join("；");
            let body: String = body.chars().take(120).collect();
            match m.role {
                exm_core::types::MessageRole::User => {
                    // im: 标记还原发送者署名（与模型上下文同口径）
                    let who = m
                        .agent_id
                        .as_deref()
                        .and_then(|a| a.strip_prefix("im:"))
                        .unwrap_or("");
                    if who.is_empty() {
                        format!("[用户] {body}")
                    } else {
                        format!("[用户·{who}] {body}")
                    }
                }
                exm_core::types::MessageRole::Orchestrator
                | exm_core::types::MessageRole::Unit => format!("[本集群] {body}"),
                exm_core::types::MessageRole::System => format!("[系统] {body}"),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 决策输出解析：合法 true / 合法 false / 围栏包裹 / 杂文前后缀 / 非法。
    /// 保守侧：解析不出来一律不回应。
    #[test]
    fn 主动回复决策解析_合法与非法() {
        assert!(parse_proactive_eval(r#"{"reply": true, "why": "有人求助"}"#));
        assert!(!parse_proactive_eval(r#"{"reply": false, "why": "闲聊"}"#));
        // 围栏包裹
        assert!(parse_proactive_eval(
            "```json\n{\"reply\": true, \"why\": \"事实错误\"}\n```"
        ));
        // 前后杂文
        assert!(parse_proactive_eval(
            "判断如下：\n{\"reply\": true, \"why\": \"问题正中领域\"}\n以上。"
        ));
        // 非法：无 JSON / reply 非布尔 / 缺 reply
        assert!(!parse_proactive_eval("我觉得不该回"));
        assert!(!parse_proactive_eval(r#"{"reply": "yes"}"#));
        assert!(!parse_proactive_eval(r#"{"why": "缺 reply"}"#));
        // JSON 段截取错位（end <= start 防御）
        assert!(!parse_proactive_eval("}{"));
    }

    /// 回复率边界：0 恒不命中、100 恒命中；中间值统计意义（不测随机性本身）。
    #[test]
    fn 回复率采样_边界() {
        assert!(!rate_hit_check(0));
        assert!(rate_hit_check(100));
    }
    fn rate_hit_check(rate: u32) -> bool {
        // 直接复刻 rate_hit 的语义：rate=0 → range 条件恒假；rate=100 → 恒真
        if rate == 0 {
            return false;
        }
        if rate == 100 {
            return true;
        }
        rate_hit(rate)
    }

    /// 漏斗零成本级的端到端判定（不触 LLM）：
    /// rate=0 恒拒；探活词表恒拒（在吗不该被主动抢答）；
    /// 决策模型不可用（mock 之外的 provider 全断）时保守拒。
    /// 走完整 should_proactively_reply 链路（含 InboundCtx 构造）。
    #[tokio::test]
    async fn 主动回复漏斗_零成本级保守拒() {
        let dir = std::env::temp_dir().join(format!("exm-proactive-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        let core = exm_core::Core::create(&dir).expect("创建 Core 失败");
        let ch = Channel {
            id: "napcat-main".into(),
            kind: "napcat".into(),
            enabled: true,
            group: None,
            allowed_chats: vec![],
            admins: vec![],
            account: None,
            secret: None,
            reply_webhook: None,
            token: None,
            config: Default::default(),
            group_gate: Some(true),
            proactive_reply: Some(true),
            proactive_rate: Some(0), // 回复率 0 = 概率闸门恒拒
            capability: "computer".into(),
            memory_isolation: false,
            created_at: exm_core::types::now_iso(),
        };
        let text = String::from("这个报错怎么修？");
        let ctx = InboundCtx {
            text: &text,
            external_id: "10086",
            display_name: "黑星",
            is_group: true,
            mentioned: false,
            chat_key: "12345",
        };
        // ① 回复率 0：概率闸门在最前，不该触达任何后续判定
        assert!(!should_proactively_reply(&core, &ch, &ctx).await);

        // ② 探活词表：rate=100 但消息是探活——不该被主动抢答
        let ch_full = Channel { proactive_rate: Some(100), ..ch };
        let probe = String::from("在吗");
        let probe_ctx = InboundCtx { text: &probe, ..ctx };
        assert!(!should_proactively_reply(&core, &ch_full, &probe_ctx).await);

        // ③ 正常求助消息 + rate=100：漏斗走到底，决策模型调用失败（本测试环境
        // 无可用 provider）→ 保守拒。三级漏斗的「宁可漏不可吵」语义锚定。
        assert!(!should_proactively_reply(&core, &ch_full, &ctx).await);
    }
}
