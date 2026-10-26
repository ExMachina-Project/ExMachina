//! 会话 token 预算软着陆：上下文组装超模型窗口安全余量时的多级裁剪。
//!
//! 裁剪顺序（docs 设计 D7）：
//! 1. 先压缩历史（调用方已有滚动压缩，此处对注入文本做二次收窄：滚动摘要截短）
//! 2. 再按优先级裁剪注入块：记忆 → 技能 → 统计（低价值块先裁）
//! 3. 最后截断最长消息
//!
//! 估算口径：~4 字符/token（无 tokenizer 依赖，保守触发 + 多级兜底）。
//! 每次裁剪在会话事件流留痕（context.trim 事件，记录被裁剪块与量级）。

/// 估算口径：约 4 字符/token
pub const CHARS_PER_TOKEN: usize = 4;

/// 可裁剪注入块（按裁剪优先级从高到低排列——先裁低价值块）
#[derive(Debug, Clone, PartialEq)]
pub enum TrimBlock {
    /// 组记忆注入（memory.md 渲染）——首先裁剪
    Memory,
    /// 个体可靠性统计——其次
    Stats,
    /// 滚动摘要——收窄到预算内
    Summary,
}

impl TrimBlock {
    pub fn key(&self) -> &'static str {
        match self {
            TrimBlock::Memory => "memory",
            TrimBlock::Stats => "stats",
            TrimBlock::Summary => "summary",
        }
    }
}

/// 预算裁剪结果
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BudgetOutcome {
    /// 裁剪后各块新文本（None = 整块移除）
    pub memory: Option<String>,
    pub stats: Option<String>,
    pub summary: Option<String>,
    /// 最长消息截断后的字符数（None = 未截断）
    pub truncated_msg_chars: Option<usize>,
    /// 留痕条目：(块, 原字符数, 新字符数)
    pub trims: Vec<(&'static str, usize, usize)>,
}

/// 字符数 → token 估算
pub fn est_tokens(chars: usize) -> usize {
    chars / CHARS_PER_TOKEN
}

/// 预算判定与多级裁剪（纯函数，便于单测）。
///
/// - `over_chars`：当前超出预算的字符数（正数才需要裁剪）
/// - 输入三块当前文本；返回裁剪结果与留痕
///
/// 裁剪策略：按 Memory → Stats → Summary 顺序，能整块砍掉就砍；
/// 砍到最后一块还不够时截断该块；消息截断由调用方在拼装后执行。
pub fn trim_to_budget(
    over_chars: usize,
    memory: Option<&str>,
    stats: Option<&str>,
    summary: Option<&str>,
) -> BudgetOutcome {
    let mut out = BudgetOutcome {
        memory: memory.map(|s| s.to_string()),
        stats: stats.map(|s| s.to_string()),
        summary: summary.map(|s| s.to_string()),
        truncated_msg_chars: None,
        trims: Vec::new(),
    };
    if over_chars == 0 {
        return out;
    }
    let mut need = over_chars;
    // 第一级：整块裁剪（低价值在前）
    for (block, slot) in [
        (TrimBlock::Memory, &mut out.memory),
        (TrimBlock::Stats, &mut out.stats),
    ] {
        if need == 0 {
            break;
        }
        if let Some(text) = slot.take() {
            let len = text.chars().count();
            if len > 0 {
                need = need.saturating_sub(len);
                out.trims.push((block.key(), len, 0));
            }
        }
    }
    // 第二级：滚动摘要收窄到剩余预算内（至少保 200 字符，避免完全丢失上下文）
    if need > 0 {
        if let Some(text) = out.summary.take() {
            let len = text.chars().count();
            let keep = len.saturating_sub(need).max(200).min(len);
            let shortened: String = text.chars().take(keep).collect();
            out.trims.push((TrimBlock::Summary.key(), len, keep));
            out.summary = Some(shortened);
            need = need.saturating_sub(len - keep);
        }
    }
    // 第三级：仍不够 → 标记需截断最长消息（调用方执行）
    out.truncated_msg_chars = (need > 0).then_some(need);
    out
}

/// 从最旧一侧逐条截短历史消息（每条保底 200 字符），返回被截短的消息条数。
/// 时间正序输入；最新消息（尾部）保持完整——上下文连续性优先保新。
pub fn trim_oldest_messages(msgs: &mut [crate::types::ChatMessage], shorten_by: usize) -> usize {
    const FLOOR: usize = 200;
    let mut remaining = shorten_by;
    let mut trimmed = 0usize;
    for m in msgs.iter_mut() {
        if remaining == 0 {
            break;
        }
        for s in m.statements.iter_mut() {
            if remaining == 0 {
                break;
            }
            let len = s.text.chars().count();
            if len > FLOOR {
                let keep = (len - remaining).max(FLOOR).min(len);
                let cut = len - keep;
                if cut > 0 {
                    s.text = s.text.chars().take(keep).collect();
                    remaining -= cut;
                    trimmed += 1;
                }
            }
        }
    }
    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 估算_四字符一token() {
        assert_eq!(est_tokens(400), 100);
        assert_eq!(est_tokens(399), 99);
    }

    #[test]
    fn 无超出_不裁剪() {
        let o = trim_to_budget(0, Some("m"), Some("s"), Some("y"));
        assert_eq!(o.memory.as_deref(), Some("m"));
        assert!(o.trims.is_empty());
        assert!(o.truncated_msg_chars.is_none());
    }

    #[test]
    fn 超预算_先整块裁记忆再统计() {
        // 记忆 100 字符、统计 50、摘要 400：超 120 → 砍记忆(100) + 砍统计(50) 即够
        let mem = "m".repeat(100);
        let stats = "s".repeat(50);
        let summary = "y".repeat(400);
        let o = trim_to_budget(120, Some(&mem), Some(&stats), Some(&summary));
        assert!(o.memory.is_none());
        assert!(o.stats.is_none());
        assert!(o.summary.is_some());
        assert_eq!(o.trims.len(), 2);
        assert_eq!(o.trims[0], ("memory", 100, 0));
        assert_eq!(o.trims[1], ("stats", 50, 0));
        assert!(o.truncated_msg_chars.is_none());
    }

    #[test]
    fn 超预算_摘要收窄保底两百字符() {
        // 只剩摘要可裁：超 300，摘要 400 → 收窄到 max(400-300, 200)=200
        let summary = "y".repeat(400);
        let o = trim_to_budget(300, None, None, Some(&summary));
        assert_eq!(o.summary.as_deref().map(|s| s.chars().count()), Some(200));
        assert_eq!(o.trims.len(), 1);
        assert_eq!(o.trims[0], ("summary", 400, 200));
    }

    #[test]
    fn 超预算_仍不够_标记截断消息() {
        // 摘要已到保底仍有缺口 → 标记截断
        let summary = "y".repeat(300);
        let o = trim_to_budget(500, None, None, Some(&summary));
        // 摘要保底 200，消掉 100，还缺 400
        assert_eq!(o.truncated_msg_chars, Some(400));
        assert_eq!(o.summary.as_deref().map(|s| s.chars().count()), Some(200));
    }

    #[test]
    fn 旧消息截短_保最新完整() {
        use crate::types::{ChatMessage, MessageRole, SpeechTag, Statement};
        let mk = |text: &str| ChatMessage {
            id: String::new(),
            session_id: String::new(),
            role: MessageRole::User,
            agent_id: None,
            statements: vec![Statement::new(SpeechTag::报告, text)],
            created_at: String::new(),
            turn: None,
            token_usage: None,
            thinking: None,
            last_message_preview: None,
            last_active_at: None,
            tool_calls: Vec::new(),
            steps: Vec::new(),
        };
        let mut msgs = vec![mk(&"旧".repeat(500)), mk(&"新".repeat(300))];
        let n = trim_oldest_messages(&mut msgs, 250);
        assert_eq!(n, 1);
        // 最旧消息被截到保底 250（500-250），最新保持 300 完整
        assert_eq!(msgs[0].statements[0].text.chars().count(), 250);
        assert_eq!(msgs[1].statements[0].text.chars().count(), 300);
    }
}
