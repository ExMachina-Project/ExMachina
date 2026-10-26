//! 会话上下文压缩（共享层）：历史超过阈值时，旧消息压成滚动摘要，保留最近窗口原文。
//!
//! 口径与 docs/架构与设计.md 的组调度压缩一致；抽取为共享模块供组调度
//! （orchestrator）与单智能体循环（runtime::chat_execute）共同复用，
//! 避免两套实现漂移。
//!
//! 压缩为增量式：已有摘要 + 新纳入的旧消息 → 新摘要（LLM 非流式）。
//! 压缩失败沿用旧摘要，不阻断会话。

use crate::provider::{ChatRequest, LlmProvider};
use crate::store::Store;
use crate::types::{ChatMessage, MessageRole};
use std::sync::Arc;

/// LLM 协议消息（provider 层）：压缩调用使用
type Wire = crate::provider::ChatMessage;

/// 触发压缩的历史条数阈值（超过才压缩）
pub const COMPACT_TRIGGER: usize = 20;
/// 保留最近 N 条原文窗口
pub const COMPACT_KEEP: usize = 12;

/// 压缩器系统提示词（组调度与单智能体共用同一口径）
pub const COMPACTER_SYSTEM: &str = "你是会话压缩器【会话压缩】。把历史对话压成不超过 400 字的连续上下文摘要：保留目标、关键结论、证据与未决事项；只输出摘要正文，不要任何前后缀。";

/// 消息渲染为对话转录行（每条截 400 字符）
fn transcript_line(m: &ChatMessage) -> String {
    let who = match m.role {
        MessageRole::User => "用户",
        MessageRole::Orchestrator => m.agent_id.as_deref().unwrap_or("指挥体"),
        MessageRole::Unit => m.agent_id.as_deref().unwrap_or("子个体"),
        MessageRole::System => "系统",
    };
    let body = m
        .statements
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join("；");
    format!("[{who}] {}", body.chars().take(400).collect::<String>())
}

/// 最近窗口消息渲染为文本（按时间正序输出；每条截 300 字符）
pub fn render_recent(msgs: &[ChatMessage]) -> String {
    msgs.iter()
        .map(|m| {
            let who = match m.role {
                MessageRole::User => "用户",
                MessageRole::Orchestrator => m.agent_id.as_deref().unwrap_or("指挥体"),
                MessageRole::Unit => m.agent_id.as_deref().unwrap_or("子个体"),
                MessageRole::System => "系统",
            };
            let body = m
                .statements
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join("；");
            format!("[{who}] {}", body.chars().take(300).collect::<String>())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 上下文压缩结果：滚动摘要（更早历史）+ 最近窗口原文
#[derive(Debug, Clone, PartialEq)]
pub struct Compacted {
    /// 更早历史的滚动摘要（未触发压缩或无摘要时为 None）
    pub summary: Option<String>,
    /// 最近窗口消息原文文本（时间正序）
    pub recent: String,
}

/// 压缩参数（阈值/窗口可由调用方覆盖；默认取模块常量）
#[derive(Debug, Clone, Copy)]
pub struct CompactParams {
    /// 超过该条数才压缩
    pub trigger: usize,
    /// 保留最近 N 条原文
    pub keep: usize,
}

impl Default for CompactParams {
    fn default() -> Self {
        Self {
            trigger: COMPACT_TRIGGER,
            keep: COMPACT_KEEP,
        }
    }
}

/// 会话滚动压缩：读会话消息 → 阈值判定 → 增量压缩 → 持久化摘要。
///
/// - 历史未超阈值：返回 `summary=None` + 全量原文
/// - 已压缩到最新（`summary_upto` 追平）：直接返回既有摘要 + 最近窗口
/// - 压缩失败：沿用旧摘要（旧摘要为空则返回 None），不返回 Err
pub async fn ensure_compacted(
    store: &Arc<Store>,
    provider: &Arc<dyn LlmProvider>,
    model: &str,
    session_id: &str,
    params: CompactParams,
) -> Compacted {
    let msgs: Vec<ChatMessage> = store
        .list_messages(session_id, 500)
        .unwrap_or_default();
    if msgs.len() <= params.trigger {
        return Compacted {
            summary: None,
            recent: render_recent(&msgs),
        };
    }
    // list_messages 为时间正序：older = 前段（压入摘要）；recent = 尾部 keep 条原文窗口
    let split = msgs.len().saturating_sub(params.keep);
    let older: Vec<&ChatMessage> = msgs.iter().take(split).collect();
    let recent: Vec<&ChatMessage> = msgs.iter().skip(split).collect();
    let session = store.get_session(session_id).ok().flatten();
    let upto = older.len();
    let prev_upto = session.as_ref().and_then(|s| s.summary_upto).unwrap_or(0);
    let prev_summary = session
        .as_ref()
        .and_then(|s| s.rolling_summary.clone())
        .unwrap_or_default();
    if prev_upto == upto {
        let recent_text = render_recent(&recent.into_iter().cloned().collect::<Vec<_>>());
        return Compacted {
            summary: (!prev_summary.trim().is_empty()).then_some(prev_summary),
            recent: recent_text,
        };
    }
    // 增量压缩：已有摘要 + 新纳入的旧消息 → 新摘要
    let transcript = older
        .iter()
        .map(|m| transcript_line(m))
        .collect::<Vec<_>>()
        .join("\n");
    let user = format!(
        "已有摘要：\n{prev_summary}\n\n新增对话：\n{transcript}\n\n输出合并后的新摘要。"
    );
    let req = ChatRequest::new(
        model.to_string(),
        vec![Wire::system(COMPACTER_SYSTEM), Wire::user(user)],
    );
    let summary = provider
        .chat(req)
        .await
        .map(|r| r.content)
        .unwrap_or_else(|e| {
            eprintln!("[compaction] 会话压缩失败（沿用旧摘要）：{e}");
            if prev_summary.is_empty() {
                String::new()
            } else {
                prev_summary.clone()
            }
        });
    let recent_text = render_recent(&recent.into_iter().cloned().collect::<Vec<_>>());
    if !summary.trim().is_empty() {
        let _ = store.update_compaction(session_id, &summary, upto);
        Compacted {
            summary: Some(summary),
            recent: recent_text,
        }
    } else {
        Compacted {
            summary: None,
            recent: recent_text,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_recent_roles() {
        let mk = |role: MessageRole, text: &str| ChatMessage {
            id: String::new(),
            session_id: String::new(),
            role,
            agent_id: Some("a1".into()),
            statements: vec![crate::types::Statement::new(
                crate::types::SpeechTag::报告,
                text,
            )],
            created_at: String::new(),
            token_usage: None,
            thinking: None,
            last_message_preview: None,
            last_active_at: None,
            tool_calls: Vec::new(),
            steps: Vec::new(),
        };
        let msgs = vec![
            mk(MessageRole::User, "你好"),
            mk(MessageRole::Orchestrator, "收到"),
        ];
        let out = render_recent(&msgs);
        assert!(out.contains("[用户] 你好"));
        assert!(out.contains("[a1] 收到"));
    }

    #[test]
    fn params_default_matches_consts() {
        let p = CompactParams::default();
        assert_eq!(p.trigger, COMPACT_TRIGGER);
        assert_eq!(p.keep, COMPACT_KEEP);
    }
}
