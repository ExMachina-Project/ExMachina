//! 渲染：正文直排 + 证据等级标注（CLI 视图）

use exm_core::types::{Statement, TaskStatus};
use owo_colors::OwoColorize;

pub fn render_statement(s: &Statement) -> String {
    let level = s
        .evidence_level
        .map(|l| format!(" {}", format!("(证据{})", l.label()).bright_black()))
        .unwrap_or_default();
    format!("{}{}", s.text, level)
}

/// 正文按行直排，不添加任何句式前缀（是否写【报告】等由模型自己决定）。
pub fn render_statements(statements: &[Statement]) -> String {
    statements
        .iter()
        .map(render_statement)
        .collect::<Vec<_>>()
        .join("\n")
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
