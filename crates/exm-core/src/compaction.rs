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
        MessageRole::User => user_label(m),
        MessageRole::Orchestrator => m.agent_id.as_deref().unwrap_or("指挥体").to_string(),
        MessageRole::Unit => m.agent_id.as_deref().unwrap_or("子个体").to_string(),
        MessageRole::System => "系统".to_string(),
    };
    let body = m
        .statements
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join("；");
    format!("[{who}] {}", body.chars().take(400).collect::<String>())
}

/// user 消息的转录署名：带 im: 发送者标记（群聊多人区分）时渲染「群聊·名字」，
/// imctx: 旁听（门控拦下的未 @ 群消息）渲染「旁听·群聊·名字」——方括号背景语义
/// 与 history 渲染同口径（orchestrator.rs），明确是背景上下文而非对本体的指令，
/// 其余（WebUI / 私聊 / 自动任务）保持「用户」。
fn user_label(m: &ChatMessage) -> String {
    match m.agent_id.as_deref() {
        Some(mark) if mark.starts_with("imctx:") => format!("旁听·群聊·{}", &mark[6..]),
        Some(mark) if mark.starts_with("im:") => format!("群聊·{}", &mark[3..]),
        _ => "用户".into(),
    }
}

/// 最近窗口消息渲染为文本（按时间正序输出；每条截 300 字符）
pub fn render_recent(msgs: &[ChatMessage]) -> String {
    msgs.iter()
        .map(|m| {
            let who = match m.role {
                MessageRole::User => user_label(m),
                MessageRole::Orchestrator => m.agent_id.as_deref().unwrap_or("指挥体").to_string(),
                MessageRole::Unit => m.agent_id.as_deref().unwrap_or("子个体").to_string(),
                MessageRole::System => "系统".to_string(),
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
    /// 超过该条数才压缩（兜底判据：防估算器系统性低估）
    pub trigger: usize,
    /// 保留最近 N 条原文
    pub keep: usize,
    /// token 占比触发阈值（compaction.triggerRatio；0 = 关闭 token 判据仅条数兜底）。
    /// 与 trigger 为「或」关系：估算 token > 模型窗口 × contextBudgetRatio × triggerRatio 即压缩
    pub trigger_ratio: f32,
    /// token 预算基数（模型窗口 × contextBudgetRatio 的字符当量；None = 无 token 判据）
    pub budget_chars: Option<usize>,
    /// 滑动窗口模式（实验性，experiments.slidingContextWindow）：
    /// 不做新压缩调用——已压过的沿用旧摘要，窗口外未压的历史直接不进上下文。
    /// 模型持续持有最近 keep 条完整原文；早期细节由模型经 context_compact
    /// 自主决策回收（与 dynamicCompaction 协同）。
    pub sliding: bool,
}

impl Default for CompactParams {
    fn default() -> Self {
        Self {
            trigger: COMPACT_TRIGGER,
            keep: COMPACT_KEEP,
            trigger_ratio: 0.0,
            budget_chars: None,
            sliding: false,
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
    let msgs: Vec<ChatMessage> = store.list_messages(session_id, 500).unwrap_or_default();
    // 双判据（或关系，compaction-tuning）：token 占比先到（token-ratio）或条数先到（count-fallback）
    let (should_compact, trigger_reason) = compact_trigger(&msgs, &params);
    if !should_compact {
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
    // 滑动窗口模式（实验性）：不做新压缩调用。已压过的沿用旧摘要；
    // 窗口外未压历史不进上下文——模型持续持有最近 keep 条完整原文，
    // 何时回收早期细节交由模型（context_compact / 任务自然结束）。
    if params.sliding {
        let recent_text = render_recent(&recent.into_iter().cloned().collect::<Vec<_>>());
        return Compacted {
            summary: (!prev_summary.trim().is_empty()).then_some(prev_summary),
            recent: recent_text,
        };
    }
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
    let user =
        format!("已有摘要：\n{prev_summary}\n\n新增对话：\n{transcript}\n\n输出合并后的新摘要。");
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
        // 压缩留痕（compaction-tuning）：触发原因 + 前后估算用量
        let before_tokens = crate::budget::est_tokens(history_chars_of(&msgs));
        let after_tokens =
            crate::budget::est_tokens(summary.chars().count() + recent_text.chars().count());
        let env = crate::types::Envelope {
            id: crate::types::new_id(),
            topic: "context".into(),
            from: "system".into(),
            to: "session".into(),
            kind: "context.compact".into(),
            session_id: session_id.to_string(),
            task_node_id: None,
            payload: serde_json::json!({
                "trigger": trigger_reason,
                "beforeTokens": before_tokens,
                "afterTokens": after_tokens,
                "compactedMessages": upto,
            }),
            trace_id: crate::types::new_id(),
            created_at: crate::types::now_iso(),
        };
        let _ = store.append_event(&env);
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

/// 双判据触发判定（纯函数，单测口径）：
/// token 占比（估算字符 > budget_chars × trigger_ratio）先到 → token-ratio；
/// 条数超 trigger 兜底 → count-fallback；都不满足 → 不压缩。
fn compact_trigger(msgs: &[ChatMessage], params: &CompactParams) -> (bool, &'static str) {
    // token 判据（0 或无预算基数 = 关闭）
    if let Some(budget) = params.budget_chars {
        if params.trigger_ratio > 0.0 {
            let threshold = (budget as f32 * params.trigger_ratio) as usize;
            let total = history_chars_of(msgs);
            if total > threshold {
                return (true, "token-ratio");
            }
        }
    }
    // 条数兜底
    if msgs.len() > params.trigger {
        return (true, "count-fallback");
    }
    (false, "")
}

/// 消息正文总字符数（statements 拼接，与 budget.rs 估算口径一致）
fn history_chars_of(msgs: &[ChatMessage]) -> usize {
    msgs.iter()
        .map(|m| m.statements.iter().map(|s| s.text.chars().count()).sum::<usize>())
        .sum()
}

/// 模型主动压缩（实验性，experiments.dynamicCompaction）：context_compact 工具的执行体。
///
/// 与 `ensure_compacted` 的差异：不看条数阈值，只要窗口外还有未压缩历史就压。
/// `keep` 是保留原文的最近窗口；返回 (新摘要, 本次数纳入压缩的消息条数)。
/// 无可压缩内容 / 压缩失败时返回 None（调用方转成工具层说明，不阻断会话）。
pub async fn compact_now(
    store: &Arc<Store>,
    provider: &Arc<dyn LlmProvider>,
    model: &str,
    session_id: &str,
    keep: usize,
    note: &str,
) -> Option<(String, usize)> {
    let msgs: Vec<ChatMessage> = store.list_messages(session_id, 500).unwrap_or_default();
    // keep 至少 2 条：正在进行的轮次（用户输入 + 尚未落地的回复）不能被压走
    let keep = keep.max(2);
    if msgs.len() <= keep {
        return None;
    }
    let split = msgs.len() - keep;
    let older: Vec<&ChatMessage> = msgs.iter().take(split).collect();
    let upto = older.len();
    let session = store.get_session(session_id).ok().flatten();
    let prev_upto = session.as_ref().and_then(|s| s.summary_upto).unwrap_or(0);
    if prev_upto >= upto {
        return None; // 窗口外已全部压过：无新内容
    }
    let prev_summary = session
        .as_ref()
        .and_then(|s| s.rolling_summary.clone())
        .unwrap_or_default();
    let transcript = older
        .iter()
        .map(|m| transcript_line(m))
        .collect::<Vec<_>>()
        .join("\n");
    let user = if note.trim().is_empty() {
        format!("已有摘要：\n{prev_summary}\n\n新增对话：\n{transcript}\n\n输出合并后的新摘要。")
    } else {
        format!(
            "已有摘要：\n{prev_summary}\n\n新增对话：\n{transcript}\n\n模型压缩说明：{note}\n输出合并后的新摘要。"
        )
    };
    let req = ChatRequest::new(
        model.to_string(),
        vec![Wire::system(COMPACTER_SYSTEM), Wire::user(user)],
    );
    let content = provider.chat(req).await.ok()?.content;
    let summary = content.trim().to_string();
    if summary.is_empty() {
        return None;
    }
    store.update_compaction(session_id, &summary, upto).ok()?;
    Some((summary, upto - prev_upto))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MessageRole, SpeechTag, Statement};

    fn mk_msg(role: MessageRole, text: &str) -> ChatMessage {
        ChatMessage {
            id: String::new(),
            session_id: String::new(),
            role,
            agent_id: Some("a1".into()),
            statements: vec![Statement::new(SpeechTag::报告, text)],
            created_at: String::new(),
            turn: None,
            token_usage: None,
            thinking: None,
            last_message_preview: None,
            last_active_at: None,
            tool_calls: Vec::new(),
            steps: Vec::new(),
            attachments: Vec::new(),
        }
    }

    /// 压缩替身：固定返回摘要文本
    struct OkCompacter;
    #[async_trait::async_trait]
    impl LlmProvider for OkCompacter {
        fn name(&self) -> &'static str {
            "ok-compacter"
        }
        async fn chat(&self, _req: ChatRequest) -> anyhow::Result<crate::provider::ChatResponse> {
            Ok(crate::provider::ChatResponse {
                content: "合并摘要：目标与结论".into(),
                reasoning: String::new(),
                prompt_tokens: 0,
                completion_tokens: 0,
                tool_calls: Vec::new(),
            })
        }
        async fn stream(
            &self,
            _req: ChatRequest,
            _tx: tokio::sync::mpsc::UnboundedSender<crate::provider::StreamDelta>,
        ) -> anyhow::Result<crate::provider::ChatResponse> {
            anyhow::bail!("测试替身不支持流式")
        }
    }

    /// 压缩替身：必然失败（模拟模型超时/异常）
    struct FailCompacter;
    #[async_trait::async_trait]
    impl LlmProvider for FailCompacter {
        fn name(&self) -> &'static str {
            "fail-compacter"
        }
        async fn chat(&self, _req: ChatRequest) -> anyhow::Result<crate::provider::ChatResponse> {
            anyhow::bail!("模拟压缩失败")
        }
        async fn stream(
            &self,
            _req: ChatRequest,
            _tx: tokio::sync::mpsc::UnboundedSender<crate::provider::StreamDelta>,
        ) -> anyhow::Result<crate::provider::ChatResponse> {
            anyhow::bail!("测试替身不支持流式")
        }
    }

    fn test_store_with(n_msgs: usize) -> (Arc<Store>, String) {
        let dir = std::env::temp_dir().join(format!("exm-compact-{}", uuid::Uuid::new_v4()));
        let store = Store::open(&dir).expect("打开测试存储失败");
        let s = store.create_session("压缩测试", "g1").unwrap();
        let sid = s.id;
        for i in 0..n_msgs {
            let role = if i % 2 == 0 {
                MessageRole::User
            } else {
                MessageRole::Orchestrator
            };
            store
                .add_message(
                    &sid,
                    role,
                    None,
                    vec![Statement::new(SpeechTag::报告, format!("消息{i}"))],
                )
                .unwrap();
        }
        (Arc::new(store), sid)
    }

    #[test]
    fn render_recent_roles() {
        let msgs = vec![
            mk_msg(MessageRole::User, "你好"),
            mk_msg(MessageRole::Orchestrator, "收到"),
        ];
        let out = render_recent(&msgs);
        assert!(out.contains("[用户] 你好"));
        assert!(out.contains("[a1] 收到"));
    }

    /// 群聊旁听（imctx:）：模型侧渲染「群聊·发送者」——
    /// 与真实用户输入（用户·发送者）区分，明确是背景上下文。
    #[test]
    fn render_recent_群聊旁听署名() {
        let mut m = mk_msg(MessageRole::User, "部署方案定了吗");
        m.agent_id = Some("imctx:张三".into());
        let out = render_recent(&[m]);
        assert!(out.contains("[旁听·群聊·张三] 部署方案定了吗"), "{out}");
        // 旁听（背景）与真实输入（im:）署名必须可区分：防无关上下文回帖
        let mut real = mk_msg(MessageRole::User, "正式问一下");
        real.agent_id = Some("im:李四".into());
        let out2 = render_recent(&[real]);
        assert!(out2.contains("[群聊·李四] 正式问一下"), "{out2}");
        assert_ne!(out, out2, "旁听与真实输入渲染必须不同");
    }

    #[tokio::test]
    async fn 未超阈值_不压缩_返回全量原文() {
        let (store, sid) = test_store_with(5);
        let provider: Arc<dyn LlmProvider> = Arc::new(OkCompacter);
        let c = ensure_compacted(
            &store,
            &provider,
            "m",
            &sid,
            CompactParams {
                trigger: 10,
                sliding: false,
                keep: 5,
                ..Default::default()
            },
        )
        .await;
        assert!(c.summary.is_none());
        assert!(c.recent.contains("消息0"));
        assert!(c.recent.contains("消息4"));
    }

    #[tokio::test]
    async fn 超阈值_触发压缩_摘要持久化() {
        let (store, sid) = test_store_with(30);
        let provider: Arc<dyn LlmProvider> = Arc::new(OkCompacter);
        let c = ensure_compacted(
            &store,
            &provider,
            "m",
            &sid,
            CompactParams {
                trigger: 10,
                sliding: false,
                keep: 5,
                ..Default::default()
            },
        )
        .await;
        // 摘要来自替身固定返回
        assert_eq!(c.summary.as_deref(), Some("合并摘要：目标与结论"));
        // 最近窗口保留原文（尾部 5 条），窗口外消息不出现
        assert!(c.recent.contains("消息29"));
        assert!(!c.recent.contains("消息0"));
        // 摘要已持久化到会话
        let s = store.get_session(&sid).unwrap().unwrap();
        assert_eq!(s.rolling_summary.as_deref(), Some("合并摘要：目标与结论"));
        assert_eq!(s.summary_upto, Some(25));
    }

    #[tokio::test]
    async fn 压缩失败_沿用旧摘要_不阻断() {
        let (store, sid) = test_store_with(30);
        // 先用成功替身建立旧摘要
        let ok: Arc<dyn LlmProvider> = Arc::new(OkCompacter);
        ensure_compacted(
            &store,
            &ok,
            "m",
            &sid,
            CompactParams {
                trigger: 10,
                sliding: false,
                keep: 5,
                ..Default::default()
            },
        )
        .await;
        // 再追加消息后用失败替身压缩：应沿用旧摘要
        store
            .add_message(
                &sid,
                MessageRole::User,
                None,
                vec![Statement::new(SpeechTag::报告, "追加消息")],
            )
            .unwrap();
        let fail: Arc<dyn LlmProvider> = Arc::new(FailCompacter);
        let c = ensure_compacted(
            &store,
            &fail,
            "m",
            &sid,
            CompactParams {
                trigger: 10,
                sliding: false,
                keep: 5,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(c.summary.as_deref(), Some("合并摘要：目标与结论"));
        // 最近窗口仍包含最新消息原文
        assert!(c.recent.contains("追加消息"));
    }

    #[test]
    fn params_default_matches_consts() {
        let p = CompactParams::default();
        assert_eq!(p.trigger, COMPACT_TRIGGER);
        assert_eq!(p.keep, COMPACT_KEEP);
        assert!(!p.sliding, "默认非滑动窗口（既有行为）");
    }

    /// 滑动窗口模式（实验性，experiments.slidingContextWindow）：
    /// 超阈值也不发起压缩调用——失败 provider 不阻断即证明零 LLM 调用；
    /// 已有旧摘要沿用；窗口 = 最近 keep 条完整原文。
    #[tokio::test]
    async fn 滑动窗口_超阈值不压缩_窗口完整原文() {
        let (store, sid) = test_store_with(30);
        // 失败替身：若滑动模式仍调 LLM 压缩会返回 Err→摘要 None 且 panic 链路不可达；
        // 这里直接断言「无新摘要持久化 + 窗口外消息不进上下文」
        let provider: Arc<dyn LlmProvider> = Arc::new(FailCompacter);
        let c = ensure_compacted(
            &store,
            &provider,
            "m",
            &sid,
            CompactParams {
                trigger: 10,
                keep: 5,
                sliding: true,
                ..Default::default()
            },
        )
        .await;
        // 无旧摘要可沿用：summary=None（未压缩，不是失败）
        assert!(c.summary.is_none(), "滑动模式不产生新摘要");
        // 窗口 = 最近 5 条原文；窗口外不进上下文
        assert!(c.recent.contains("消息29"), "窗口含最新消息");
        assert!(!c.recent.contains("消息0"), "窗口外消息不进上下文");
        // 会话无摘要持久化（压缩零调用）
        let s = store.get_session(&sid).unwrap().unwrap();
        assert!(s.rolling_summary.is_none());
        assert_eq!(s.summary_upto, None);
    }

    /// 滑动窗口 + 已有旧摘要：沿用旧摘要（不追加压缩），新窗口滑过旧边界
    #[tokio::test]
    async fn 滑动窗口_已有旧摘要_沿用不追压() {
        let (store, sid) = test_store_with(30);
        // 先以常规模式建立摘要（upto=25）
        let ok: Arc<dyn LlmProvider> = Arc::new(OkCompacter);
        ensure_compacted(
            &store,
            &ok,
            "m",
            &sid,
            CompactParams {
                trigger: 10,
                keep: 5,
                sliding: false,
                ..Default::default()
            },
        )
        .await;
        // 追加消息后切滑动模式：沿用旧摘要，不再压（FailCompacter 证明零调用）
        store
            .add_message(
                &sid,
                MessageRole::User,
                None,
                vec![Statement::new(SpeechTag::报告, "追加消息")],
            )
            .unwrap();
        let fail: Arc<dyn LlmProvider> = Arc::new(FailCompacter);
        let c = ensure_compacted(
            &store,
            &fail,
            "m",
            &sid,
            CompactParams {
                trigger: 10,
                keep: 5,
                sliding: true,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(
            c.summary.as_deref(),
            Some("合并摘要：目标与结论"),
            "沿用旧摘要"
        );
        assert!(c.recent.contains("追加消息"), "窗口含最新消息");
        // summary_upto 不变（未追压）
        let s = store.get_session(&sid).unwrap().unwrap();
        assert_eq!(s.summary_upto, Some(25));
    }

    // ---------------- 双判据触发（compaction-tuning） ----------------

    /// token 占比触发：条数未到条数阈值，但估算字符已超预算占比线 → token-ratio
    #[test]
    fn 触发判定_token占比先到() {
        // 5 条短消息（条数不超 10），但预算占比线压得很低：必超
        let msgs: Vec<ChatMessage> = (0..5)
            .map(|i| mk_msg(MessageRole::User, &format!("消息{i}")))
            .collect();
        let (hit, reason) = compact_trigger(
            &msgs,
            &CompactParams {
                trigger: 10,
                trigger_ratio: 0.6,
                budget_chars: Some(4), // 占比线 = 4 × 0.6 = 2 字符，5 条必超
                ..Default::default()
            },
        );
        assert!(hit);
        assert_eq!(reason, "token-ratio");
    }

    /// 未达不触发：占比线与条数线都没到 → 不压缩（返回全量原文）
    #[test]
    fn 触发判定_未达不触发() {
        let msgs: Vec<ChatMessage> = (0..5)
            .map(|i| mk_msg(MessageRole::User, &format!("消息{i}")))
            .collect();
        let (hit, reason) = compact_trigger(
            &msgs,
            &CompactParams {
                trigger: 10,
                trigger_ratio: 0.6,
                budget_chars: Some(10_000), // 占比线 6000 字符，远未到
                ..Default::default()
            },
        );
        assert!(!hit);
        assert_eq!(reason, "");
    }

    /// 条数兜底：token 判据关闭（trigger_ratio=0 或无预算基数），
    /// 条数超阈值仍触发 → count-fallback（= 旧行为）
    #[test]
    fn 触发判定_条数兜底() {
        let msgs: Vec<ChatMessage> = (0..30)
            .map(|i| mk_msg(MessageRole::User, &format!("消息{i}")))
            .collect();
        // ratio=0：token 判据关闭，仅条数
        let (hit, reason) = compact_trigger(
            &msgs,
            &CompactParams {
                trigger: 10,
                trigger_ratio: 0.0,
                budget_chars: Some(10_000),
                ..Default::default()
            },
        );
        assert!(hit);
        assert_eq!(reason, "count-fallback");
        // 无预算基数：同样只剩条数
        let (hit2, reason2) = compact_trigger(
            &msgs,
            &CompactParams {
                trigger: 10,
                budget_chars: None,
                ..Default::default()
            },
        );
        assert!(hit2);
        assert_eq!(reason2, "count-fallback");
    }

    /// 关闭回退现状：ratio=0 + 无基数 = 既有条数判据，条数也没到 → 不压缩
    #[test]
    fn 触发判定_关闭回退现状() {
        let msgs: Vec<ChatMessage> = (0..5)
            .map(|i| mk_msg(MessageRole::User, &format!("消息{i}")))
            .collect();
        let (hit, reason) = compact_trigger(
            &msgs,
            &CompactParams {
                trigger: 10,
                trigger_ratio: 0.0,
                budget_chars: None,
                ..Default::default()
            },
        );
        assert!(!hit);
        assert_eq!(reason, "");
    }

    /// 端到端：token 占比先到 → 真压缩 + context.compact 事件落库
    /// （trigger/beforeTokens/afterTokens/compactedMessages 新载荷）
    #[tokio::test]
    async fn token占比触发_压缩留痕事件() {
        // 本地构造长消息（每条 60 字符）：保证压缩后净缩量可断言
        let dir = std::env::temp_dir().join(format!("exm-compact-{}", uuid::Uuid::new_v4()));
        let store = Store::open(&dir).expect("打开测试存储失败");
        let sid = store.create_session("压缩测试", "g1").unwrap().id;
        for i in 0..5 {
            store
                .add_message(
                    &sid,
                    MessageRole::User,
                    None,
                    vec![Statement::new(
                        SpeechTag::报告,
                        format!("消息{i}：{}", "背景细节".repeat(12)),
                    )],
                )
                .unwrap();
        }
        let store = Arc::new(store);
        let provider: Arc<dyn LlmProvider> = Arc::new(OkCompacter);
        let c = ensure_compacted(
            &store,
            &provider,
            "m",
            &sid,
            CompactParams {
                trigger: 100, // 条数判据关死，只走 token
                trigger_ratio: 0.6,
                budget_chars: Some(4),
                keep: 2,
                ..Default::default()
            },
        )
        .await;
        // 压缩发生：摘要来自替身
        assert_eq!(c.summary.as_deref(), Some("合并摘要：目标与结论"));
        // 事件落库：kind=context.compact（序列化为 "type"），含新载荷
        let events = store.list_events(&sid).unwrap();
        let ev = events
            .iter()
            .find(|e| e["type"] == "context.compact")
            .expect("context.compact 事件应落库");
        assert_eq!(ev["payload"]["trigger"], "token-ratio");
        assert_eq!(ev["payload"]["compactedMessages"], 3, "5 条压 3 留 2");
        let before = ev["payload"]["beforeTokens"].as_u64().unwrap();
        let after = ev["payload"]["afterTokens"].as_u64().unwrap();
        assert!(
            after < before,
            "压缩后估算用量应小于压缩前：before={before} after={after}"
        );
    }

    /// 条数触发的事件 trigger=count-fallback（新载荷在两条路径都留痕）
    #[tokio::test]
    async fn 条数触发_事件trigger为count_fallback() {
        let (store, sid) = test_store_with(30);
        let provider: Arc<dyn LlmProvider> = Arc::new(OkCompacter);
        let _ = ensure_compacted(
            &store,
            &provider,
            "m",
            &sid,
            CompactParams {
                trigger: 10,
                keep: 5,
                trigger_ratio: 0.0,
                budget_chars: Some(10_000),
                sliding: false,
            },
        )
        .await;
        let events = store.list_events(&sid).unwrap();
        let ev = events
            .iter()
            .find(|e| e["type"] == "context.compact")
            .expect("context.compact 事件应落库");
        assert_eq!(ev["payload"]["trigger"], "count-fallback");
    }
}
