//! 渲染：句式前缀着色 + 角色标签（CLI 视图）

use exm_core::types::{SpeechTag, Statement, TaskStatus};
use owo_colors::OwoColorize;

pub fn tag_color(tag: SpeechTag) -> String {
    match tag {
        SpeechTag::肯定 => "肯定".green().to_string(),
        SpeechTag::否定 => "否定".red().to_string(),
        SpeechTag::疑问 => "疑问".yellow().to_string(),
        SpeechTag::报告 => "报告".cyan().to_string(),
        SpeechTag::提案 => "提案".magenta().to_string(),
        SpeechTag::警告 => "警告".bright_red().to_string(),
        SpeechTag::要求 => "要求".blue().to_string(),
        SpeechTag::观测 => "观测".bright_black().to_string(),
    }
}

pub fn render_statement(s: &Statement) -> String {
    let level = s
        .evidence_level
        .map(|l| format!(" {}", format!("(证据{})", l.label()).bright_black()))
        .unwrap_or_default();
    format!("【{}】{}{}", tag_color(s.tag), s.text, level)
}

/// 连续同标签的陈述只标一次标签，其余缩进对齐：
/// 一屏回答里出现 19 行「【报告】」时，标签噪声压过内容本身。
pub fn render_statements(statements: &[Statement]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut prev: Option<SpeechTag> = None;
    for s in statements {
        if prev == Some(s.tag) {
            let level = s
                .evidence_level
                .map(|l| format!(" {}", format!("(证据{})", l.label()).bright_black()))
                .unwrap_or_default();
            out.push(format!("  {}{}", s.text, level));
        } else {
            out.push(render_statement(s));
            prev = Some(s.tag);
        }
    }
    out.join("\n")
}

pub fn render_role(role: &str, agent: Option<&str>, statements: &[Statement]) -> String {
    let head = match role {
        "orchestrator" => format!(" {} ", "指挥体".on_blue().white()),
        "unit" => format!(" {} ", agent.unwrap_or("unit").on_green().black()),
        "user" => format!(" {} ", "用户".on_yellow().black()),
        _ => format!(" {} ", "系统".on_bright_black().white()),
    };
    let body = render_statements(statements);
    format!("{head}\n{body}")
}

pub fn status_text(status: TaskStatus) -> String {
    let s = status.label();
    match status {
        TaskStatus::Done => s.green().to_string(),
        TaskStatus::Running | TaskStatus::Dispatched | TaskStatus::Syncing => s.yellow().to_string(),
        TaskStatus::Failed => s.bright_red().to_string(),
        TaskStatus::Blocked => s.red().to_string(),
        TaskStatus::Ready => s.blue().to_string(),
        _ => s.bright_black().to_string(),
    }
}

pub fn node_line(n: &exm_core::types::TaskNode) -> String {
    format!(
        "  ● {} {} {} {}",
        n.id,
        n.title,
        format!("({})", n.agent_identifier).bright_black(),
        status_text(n.status)
    )
}

pub fn dim(s: impl AsRef<str>) -> String {
    s.as_ref().bright_black().to_string()
}

pub fn ok(s: impl AsRef<str>) -> String {
    s.as_ref().green().to_string()
}

pub fn warn(s: impl AsRef<str>) -> String {
    s.as_ref().yellow().to_string()
}

pub fn err(s: impl AsRef<str>) -> String {
    s.as_ref().bright_red().to_string()
}
