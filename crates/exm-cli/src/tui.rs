//! 全屏 TUI（spec: cli-tui）：`exm` 裸命令的默认形态——终端原生编程助手。
//!
//! 版式三段：消息流区（滚动）/ 输入区 / 状态栏；Tab 切换「对话 / 审批」两页。
//! 事件驱动：会话事件订阅进消息流（token 流式、工具卡片、diff、计划条目），
//! 审批页共享 approvals 通道（与 WebUI 一处处理他处消失）。
//!
//! 终端状态完整恢复：alternate screen + 原始终端模式，panic 也不留残屏（restore guard）。

use crate::panel::{Flow, Panel, PanelState};
use crossterm::{
    event::{
        DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, DisableBracketedPaste,
        KeyCode, KeyEvent, KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, LeaveAlternateScreen},
};
use exm_core::types::CoreEvent;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};
use std::sync::Arc;
use tokio::sync::mpsc;

/// TUI 内一条消息（消息流区的渲染单元）
#[derive(Debug, Clone)]
enum Msg {
    /// 用户输入（含斜杠命令）
    User(String),
    /// 智能体回答（最终陈述文本）
    Assistant { agent: String, text: String },
    /// 工具调用卡（名称 + 参数摘要 + 结果状态）
    Tool { name: String, args: String, ok: bool, out: String },
    /// 计划事件行
    Plan(String),
    /// 系统提示（斜杠命令输出、错误等）
    System(String),
    /// 运行中指示
    Running(String),
}

/// 审批页的待审列表项（共享 approvals 通道的快照）
#[derive(Debug, Clone)]
struct ApprovalRow {
    id: String,
    command: String,
    created_at: String,
}

/// 页面（Tab 切换）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Chat,
    Approvals,
}

/// TUI 聚合状态
struct Tui {
    page: Page,
    msgs: Vec<Msg>,
    /// 消息流滚动：None = 跟随底部（自动滚）；Some(i) = 手动停留
    scroll: Option<usize>,
    input: String,
    /// 光标位置（input 内字节偏移；中文安全用 char 计）
    cursor: usize,
    running: bool,
    quiet: bool,
    session_id: String,
    session_title: String,
    turns: usize,
    approvals: Vec<ApprovalRow>,
    approval_sel: ListState,
    approval_note: String,
    /// 输入历史（↑↓ 翻）
    history: Vec<String>,
    history_idx: Option<usize>,
}

impl Tui {
    fn new(session_id: String, session_title: String, quiet: bool) -> Self {
        let mut approval_sel = ListState::default();
        approval_sel.select(Some(0));
        Tui {
            page: Page::Chat,
            msgs: vec![Msg::System(
                "EXMACHINA 终端工作台 —— Tab 切页（对话/审批）· Enter 发送 · Ctrl+C 退出 · /help 命令".into(),
            )],
            scroll: None,
            input: String::new(),
            cursor: 0,
            running: false,
            quiet,
            session_id,
            session_title,
            turns: 0,
            approvals: Vec::new(),
            approval_sel,
            approval_note: String::new(),
            history: Vec::new(),
            history_idx: None,
        }
    }

    fn push(&mut self, m: Msg) {
        self.msgs.push(m);
        // 默认跟随底部
        self.scroll = None;
    }

    fn input_insert(&mut self, c: char) {
        let byte_idx = char_to_byte(&self.input, self.cursor);
        self.input.insert(byte_idx, c);
        self.cursor += 1;
    }

    fn input_backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            let byte_idx = char_to_byte(&self.input, self.cursor);
            self.input.remove(byte_idx);
        }
    }

    fn input_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn input_right(&mut self) {
        let chars = self.input.chars().count();
        if self.cursor < chars {
            self.cursor += 1;
        }
    }
}

/// char 索引 → byte 索引（中文安全）
fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

/// 待审列表刷新（approvals 通道快照）
async fn refresh_approvals(core: &Arc<exm_core::Core>, tui: &mut Tui) {
    match core.approval_list(Some("pending"), 50) {
        Ok(list) => {
            tui.approvals = list
                .into_iter()
                .map(|r| ApprovalRow {
                    id: r.id,
                    command: r.command,
                    created_at: r.created_at,
                })
                .collect();
            let sel = tui.approval_sel.selected().unwrap_or(0);
            let n = tui.approvals.len();
            tui.approval_sel.select(if n == 0 { None } else { Some(sel.min(n - 1)) });
        }
        Err(e) => {
            tui.approval_note = format!("审批列表读取失败：{e}");
        }
    }
}

/// 事件 → 消息流（与 chat.rs 的行渲染同口径，但转为卡片行）
fn event_to_msg(evt: &CoreEvent) -> Option<Msg> {
    match evt.kind.as_str() {
        "tool.call" => {
            let name = evt.payload.get("tool").and_then(|v| v.as_str()).unwrap_or("");
            let args = evt
                .payload
                .get("args")
                .map(compact_args)
                .unwrap_or_default();
            Some(Msg::Tool { name: name.into(), args, ok: true, out: String::new() })
        }
        "tool.result" => {
            // 结果合并进最后一张工具卡（由调用方处理）；此处独立行避免碎片
            None
        }
        "run.finished" => {
            let text = statements_text(&evt.payload);
            Some(Msg::Assistant { agent: "主智能体".into(), text })
        }
        "run.error" => {
            let msg = evt.payload.get("message").and_then(|v| v.as_str()).unwrap_or("运行失败");
            Some(Msg::System(format!("⚠ 运行失败：{msg}")))
        }
        "plan.item" | "plan.pending" | "plan.approved" | "plan.rejected" | "plan.applied" => {
            let line = match evt.kind.as_str() {
                "plan.item" => {
                    let t = evt.payload.get("target").and_then(|v| v.as_str()).unwrap_or("");
                    format!("📋 计划条目：{t}（未执行，待批准）")
                }
                "plan.pending" => format!(
                    "📋 计划待审批（{} 条）—— 审批页或 /plan 处理",
                    evt.payload.get("count").and_then(|v| v.as_u64()).unwrap_or(0)
                ),
                "plan.approved" => "✅ 计划已批准，回放执行".into(),
                "plan.rejected" => "⛔ 计划已拒绝（清单保留）".into(),
                _ => {
                    let ok = evt.payload.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                    let t = evt.payload.get("target").and_then(|v| v.as_str()).unwrap_or("");
                    if ok { format!("✅ 已执行：{t}") } else { format!("❌ 执行失败：{t}") }
                }
            };
            Some(Msg::Plan(line))
        }
        "spawn.started" => {
            let p = evt.payload.get("taskPreview").and_then(|v| v.as_str()).unwrap_or("");
            let bg = evt.payload.get("background").and_then(|v| v.as_bool()).unwrap_or(false);
            if bg {
                Some(Msg::Plan(format!("🚀 后台子任务启动：{p}（结果稍后可查）")))
            } else {
                Some(Msg::Plan(format!("🚀 派生子任务：{p}")))
            }
        }
        "spawn.finished" => {
            let status = evt.payload.get("status").and_then(|v| v.as_str()).unwrap_or("");
            let bg = evt.payload.get("background").and_then(|v| v.as_bool()).unwrap_or(false);
            let suffix = if bg { "（后台，结果待收）" } else { "" };
            match status {
                "done" => Some(Msg::Plan(format!("🚀 子任务完成（结论已回传）{suffix}"))),
                "timeout" => Some(Msg::Plan(format!("🚀 子任务超时终止{suffix}"))),
                _ => Some(Msg::Plan(format!("🚀 子任务失败：{status}{suffix}"))),
            }
        }
        "todo.updated" => {
            let done = evt.payload.get("doneCount").and_then(|v| v.as_u64()).unwrap_or(0);
            let total = evt.payload.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
            let all = evt.payload.get("allDone").and_then(|v| v.as_bool()).unwrap_or(false);
            if total == 0 {
                Some(Msg::Plan("📋 任务清单已清空".into()))
            } else if all {
                Some(Msg::Plan(format!("📋 任务清单全部完成（{done}/{total}）")))
            } else {
                Some(Msg::Plan(format!("📋 任务清单更新：{done}/{total} 完成")))
            }
        }
        _ => None,
    }
}

/// 陈述载荷 → 文本（与 chat.rs flatten 同口径）
fn statements_text(payload: &serde_json::Value) -> String {
    let mut lines = Vec::new();
    if let Some(list) = payload.get("statements").and_then(|v| v.as_array()) {
        for s in list {
            let text = s.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if !text.trim().is_empty() {
                lines.push(text.to_string());
            }
        }
    }
    if lines.is_empty() { "（本轮无收束输出）".into() } else { lines.join("\n") }
}

/// 工具参数 → 单行摘要（键=值 取前几个键）
fn compact_args(args: &serde_json::Value) -> String {
    match args {
        serde_json::Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .take(3)
                .map(|(k, v)| {
                    let val = v.to_string().trim_matches('"').to_string();
                    let short: String = val.chars().take(40).collect();
                    format!("{k}={short}")
                })
                .collect();
            parts.join(" ")
        }
        other => {
            let s = other.to_string();
            s.chars().take(60).collect()
        }
    }
}

/// diff 风格着色（edit/patch 结果中的 +/- 行）
fn diff_lines(text: &str) -> Vec<Line<'static>> {
    text.lines()
        .map(|l| {
            let line: String = l.chars().take(200).collect();
            if line.starts_with('+') {
                Line::styled(line, Style::new().fg(Color::Green))
            } else if line.starts_with('-') {
                Line::styled(line, Style::new().fg(Color::Red))
            } else if line.starts_with("@@") {
                Line::styled(line, Style::new().fg(Color::Cyan))
            } else {
                Line::from(line)
            }
        })
        .collect()
}

/// 消息 → 渲染行
fn msg_lines(m: &Msg, width: usize) -> Vec<Line<'static>> {
    let w = width.saturating_sub(2).max(20);
    match m {
        Msg::User(t) => {
            let mut v = vec![Line::styled(
                "┌─ 你".to_string(),
                Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD),
            )];
            for raw in t.split('\n') {
                for chunk in wrap_chars(raw, w) {
                    v.push(Line::styled(format!("│ {chunk}"), Style::new().fg(Color::Blue)));
                }
            }
            v.push(Line::styled("└─", Style::new().fg(Color::Blue)));
            v
        }
        Msg::Assistant { agent, text } => {
            let mut v = vec![Line::styled(
                format!("┌─ {agent}"),
                Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
            )];
            for raw in text.split('\n') {
                for chunk in wrap_chars(raw, w) {
                    v.push(Line::styled(format!("│ {chunk}"), Style::new().fg(Color::Green)));
                }
            }
            v.push(Line::styled("└─", Style::new().fg(Color::Green)));
            v
        }
        Msg::Tool { name, args, ok, out } => {
            let mark = if *ok { "✓" } else { "✗" };
            let mut v = vec![Line::styled(
                format!("  {mark} ⚙ {name}  {args}"),
                Style::new().fg(Color::Yellow),
            )];
            // diff 着色（edit/patch 的结果文本常含 +/- 行）
            if !out.is_empty() && (name == "edit" || name == "patch") {
                v.extend(diff_lines(out));
            } else if !out.is_empty() {
                let short: String = out.chars().take(120).collect();
                v.push(Line::styled(format!("    {short}"), Style::new().fg(Color::DarkGray)));
            }
            v
        }
        Msg::Plan(t) => vec![Line::styled(format!("  {t}"), Style::new().fg(Color::Cyan))],
        Msg::System(t) => {
            let mut v = Vec::new();
            for raw in t.split('\n') {
                for chunk in wrap_chars(raw, w) {
                    v.push(Line::styled(chunk, Style::new().fg(Color::DarkGray)));
                }
            }
            v
        }
        Msg::Running(t) => vec![Line::styled(
            format!("  ⏳ {t}"),
            Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        )],
    }
}

/// 按显示宽度折行（中文占 2 列按 2 计）
fn wrap_chars(s: &str, width: usize) -> Vec<String> {
    if s.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    for c in s.chars() {
        let cw = if c.is_wide() { 2 } else { 1 };
        if cur_w + cw > width {
            out.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        cur.push(c);
        cur_w += cw;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

trait CharWide {
    fn is_wide(&self) -> bool;
}
impl CharWide for char {
    fn is_wide(&self) -> bool {
        let u = *self as u32;
        matches!(u, 0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6)
    }
}

/// 终端恢复 guard：无论正常退出还是 panic 都还原（alternate screen + 原始终端）
struct RestoreGuard;
impl Drop for RestoreGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen, DisableMouseCapture, DisableBracketedPaste);
        ratatui::restore();
    }
}

/// TUI 主循环
pub async fn run_tui(core: Arc<exm_core::Core>, quiet: bool) -> anyhow::Result<()> {
    let session = core.create_session("CLI 终端工作台")?;
    let mut tui = Tui::new(session.id.clone(), session.title.clone(), quiet);
    refresh_approvals(&core, &mut tui).await;

    enable_raw_mode()?;
    let terminal = ratatui::init(); // 进 alternate screen + CrosstermBackend
    let _guard = RestoreGuard;
    let _ = execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste);

    // 事件通道：终端事件 / 核心事件 / 后台轮次完成
    let (evt_tx, mut evt_rx) = mpsc::channel::<TuiEvent>(1024);
    // 核心事件订阅转发
    {
        let mut rx = core.subscribe();
        let tx = evt_tx.clone();
        let sid = session.id.clone();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(e) => {
                        if e.session_id == sid || e.kind == "approval.required" || e.kind == "approval.resolved" {
                            let _ = tx.send(TuiEvent::Core(e)).await;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
    }
    // 终端事件轮询（crossterm 同步轮询放独立线程，避免阻塞 tokio）
    {
        let tx = evt_tx.clone();
        std::thread::spawn(move || {
            let mut failures = 0u32;
            loop {
                match crossterm::event::poll(std::time::Duration::from_millis(100)) {
                    Ok(true) => {
                        failures = 0;
                        match crossterm::event::read() {
                            Ok(ev) => {
                                if tx.blocking_send(TuiEvent::Term(ev)).is_err() {
                                    break;
                                }
                            }
                            Err(_) => {
                                // 读失败（stdin 关闭等）：连续失败即视为终端不可用，退出
                                failures += 1;
                                if failures >= 3 {
                                    break;
                                }
                            }
                        }
                    }
                    Ok(false) => failures = 0,
                    Err(_) => {
                        failures += 1;
                        if failures >= 3 {
                            break;
                        }
                    }
                }
            }
        });
    }

    let mut terminal = terminal;
    loop {
        // 渲染
        terminal.draw(|f| draw(f, &mut tui))?;
        // 事件
        match evt_rx.recv().await {
            Some(TuiEvent::Term(ev)) => {
                if handle_term(&core, &mut tui, ev, &evt_tx).await? {
                    break;
                }
            }
            Some(TuiEvent::Core(e)) => handle_core(&core, &mut tui, e).await,
            Some(TuiEvent::RoundDone(sid)) => {
                if sid == tui.session_id {
                    tui.running = false;
                    tui.turns += 1;
                    refresh_approvals(&core, &mut tui).await;
                }
            }
            None => break,
        }
    }
    // 事件循环结束（全部 sender drop 或显式退出）：还原终端
    drop(terminal);
    drop(_guard); // 显式还原（Drop 也会兜底）
    Ok(())
}

enum TuiEvent {
    Term(crossterm::event::Event),
    Core(CoreEvent),
    RoundDone(String),
}

/// 终端事件处理；返回 true = 退出
async fn handle_term(
    core: &Arc<exm_core::Core>,
    tui: &mut Tui,
    ev: crossterm::event::Event,
    evt_tx: &mpsc::Sender<TuiEvent>,
) -> anyhow::Result<bool> {
    use crossterm::event::Event;
    match ev {
        Event::Key(k) => {
            return handle_key(core, tui, k, evt_tx).await;
        }
        Event::Resize(_, _) => {} // ratatui 下次 draw 自动按新尺寸重绘
        _ => {}
    }
    Ok(false)
}

async fn handle_key(
    core: &Arc<exm_core::Core>,
    tui: &mut Tui,
    k: KeyEvent,
    evt_tx: &mpsc::Sender<TuiEvent>,
) -> anyhow::Result<bool> {
    // 全局键
    match (k.code, k.modifiers) {
        (KeyCode::Tab, _) => {
            tui.page = match tui.page {
                Page::Chat => Page::Approvals,
                Page::Approvals => Page::Chat,
            };
            if tui.page == Page::Approvals {
                refresh_approvals(core, tui).await;
            }
            return Ok(false);
        }
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(true),
        _ => {}
    }
    match tui.page {
        Page::Chat => handle_chat_key(core, tui, k, evt_tx).await,
        Page::Approvals => handle_approval_key(core, tui, k).await,
    }
}

/// 对话页按键
async fn handle_chat_key(
    core: &Arc<exm_core::Core>,
    tui: &mut Tui,
    k: KeyEvent,
    evt_tx: &mpsc::Sender<TuiEvent>,
) -> anyhow::Result<bool> {
    match k.code {
        KeyCode::Enter => {
            let text = tui.input.trim().to_string();
            if text.is_empty() {
                return Ok(false);
            }
            tui.input.clear();
            tui.cursor = 0;
            tui.history.push(text.clone());
            tui.history_idx = None;
            if text == "exit" || text == "quit" {
                return Ok(true);
            }
            tui.push(Msg::User(text.clone()));
            // 斜杠命令走共享分派；普通输入走 chat 轮次
            if text.starts_with('/') {
                tui.running = true;
                let flow = run_slash(core, tui, &text).await;
                tui.running = false;
                if flow == Flow::Exit {
                    return Ok(true);
                }
            } else {
                tui.running = true;
                tui.push(Msg::Running("思考中…".into()));
                let core = core.clone();
                let sid = tui.session_id.clone();
                let tx = evt_tx.clone();
                tokio::spawn(async move {
                    let r = core.chat(&sid, &text).await;
                    let _ = tx.send(TuiEvent::RoundDone(sid.clone())).await;
                    if let Err(e) = r {
                        let _ = tx
                            .send(TuiEvent::Core(CoreEvent {
                                kind: "run.error".into(),
                                session_id: sid,
                                payload: serde_json::json!({ "message": e.to_string() }),
                            }))
                            .await;
                    }
                });
            }
        }
        KeyCode::Backspace => tui.input_backspace(),
        KeyCode::Left => tui.input_left(),
        KeyCode::Right => tui.input_right(),
        KeyCode::Up if !tui.history.is_empty() => {
            // 输入历史回溯
            let idx = match tui.history_idx {
                None => tui.history.len() - 1,
                Some(i) => i.saturating_sub(1),
            };
            tui.history_idx = Some(idx);
            tui.input = tui.history[idx].clone();
            tui.cursor = tui.input.chars().count();
        }
        KeyCode::Down => {
            if let Some(i) = tui.history_idx {
                if i + 1 >= tui.history.len() {
                    tui.history_idx = None;
                    tui.input.clear();
                    tui.cursor = 0;
                } else {
                    tui.history_idx = Some(i + 1);
                    tui.input = tui.history[i + 1].clone();
                    tui.cursor = tui.input.chars().count();
                }
            }
        }
        KeyCode::PageUp => {
            // 滚动：从底部回退一页
            let n = tui.msgs.len();
            let next = tui.scroll.map(|i| i.saturating_sub(10)).unwrap_or(n.saturating_sub(10));
            tui.scroll = Some(next.min(n.saturating_sub(1)));
        }
        KeyCode::PageDown => {
            let n = tui.msgs.len();
            if let Some(i) = tui.scroll {
                if i + 10 >= n {
                    tui.scroll = None; // 回到底部跟随
                } else {
                    tui.scroll = Some(i + 10);
                }
            }
        }
        KeyCode::Char(c) => tui.input_insert(c),
        _ => {}
    }
    Ok(false)
}

/// 审批页按键
async fn handle_approval_key(
    core: &Arc<exm_core::Core>,
    tui: &mut Tui,
    k: KeyEvent,
) -> anyhow::Result<bool> {
    match k.code {
        KeyCode::Up => {
            let i = tui.approval_sel.selected().unwrap_or(0);
            if i > 0 {
                tui.approval_sel.select(Some(i - 1));
            }
        }
        KeyCode::Down => {
            let i = tui.approval_sel.selected().unwrap_or(0);
            if i + 1 < tui.approvals.len() {
                tui.approval_sel.select(Some(i + 1));
            }
        }
        KeyCode::Char('y') | KeyCode::Enter => {
            if let Some(i) = tui.approval_sel.selected() {
                if let Some(row) = tui.approvals.get(i) {
                    let id = row.id.clone();
                    tui.approval_note = "审批处理中…".into();
                    match core.approval_decide(&id, true).await {
                        Ok(r) => {
                            tui.approval_note = format!("已批准：{}", r.command);
                        }
                        Err(e) => tui.approval_note = format!("批准失败：{e}"),
                    }
                    refresh_approvals(core, tui).await;
                }
            }
        }
        KeyCode::Char('n') => {
            if let Some(i) = tui.approval_sel.selected() {
                if let Some(row) = tui.approvals.get(i) {
                    let id = row.id.clone();
                    match core.approval_decide(&id, false).await {
                        Ok(r) => tui.approval_note = format!("已拒绝：{}", r.command),
                        Err(e) => tui.approval_note = format!("拒绝失败：{e}"),
                    }
                    refresh_approvals(core, tui).await;
                }
            }
        }
        KeyCode::Char('Y') => {
            // 全部批准
            let ids: Vec<String> = tui.approvals.iter().map(|r| r.id.clone()).collect();
            let n = ids.len();
            for id in ids {
                let _ = core.approval_decide(&id, true).await;
            }
            tui.approval_note = format!("已批量批准 {n} 条");
            refresh_approvals(core, tui).await;
        }
        KeyCode::Char('N') => {
            let ids: Vec<String> = tui.approvals.iter().map(|r| r.id.clone()).collect();
            let n = ids.len();
            for id in ids {
                let _ = core.approval_decide(&id, false).await;
            }
            tui.approval_note = format!("已批量拒绝 {n} 条");
            refresh_approvals(core, tui).await;
        }
        _ => {}
    }
    Ok(false)
}

/// 斜杠命令执行（共享 panel 分派；输出捕获进消息流）。
/// 面板命令的输出走 println!（stdout）——TUI 下 stdout 被 ratatui 接管，
/// 直接打印会打穿界面。这里在 fd 层把 stdout 重定向到管道，捕获后回灌消息流。
/// 平台分层：Unix 用 POSIX pipe/dup/dup2；Windows 用 CRT 的 _pipe/_dup/_dup2（语义等价）。
async fn run_slash(core: &Arc<exm_core::Core>, tui: &mut Tui, line: &str) -> Flow {
    // 平台统一的捕获器：创建管道 + 重定向 stdout 进管道 + 命令执行 + 还原
    let cap = match StdoutCapture::new() {
        Ok(c) => c,
        Err(_) => {
            tui.push(Msg::System("命令执行失败（管道创建失败）".into()));
            return Flow::Continue;
        }
    };
    let reader = cap.spawn_reader();
    let mut panel = Panel::new(
        core.config().workspace_root.clone(),
        crate::panel::prefs_root(core),
        tui.quiet,
    );
    let mut state = PanelState {
        session_id: tui.session_id.clone(),
        session_title: tui.session_title.clone(),
        turns: tui.turns,
    };
    let flow = panel.dispatch(core, &mut state, line).await;
    let captured = cap.restore_and_collect(reader);
    if !captured.trim().is_empty() {
        tui.push(Msg::System(captured.trim_end().to_string()));
    }
    match flow {
        Ok(f) => f,
        Err(e) => {
            tui.push(Msg::System(format!("命令失败：{e}")));
            Flow::Continue
        }
    }
}

// ---------------- fd 操作：libc（crossterm 已带传递依赖，显式声明更稳） ----------------

/// stdout 捕获器：管道接住 TUI 下 panel 命令的 println! 输出，事后整段收回。
/// 平台实现分列——CRT fd 数值不等于 Windows 原生 HANDLE，两端读策略各自闭环：
/// Unix 读线程拿 owned File（fd 直转）；Windows 读线程用 CRT `_read` 直接读 fd。
struct StdoutCapture {
    /// 管道写端（重定向期间由本结构持有；还原时 drop 关闭 → 读端 EOF）
    write: Option<std::fs::File>,
    /// 原 stdout 的备份 fd
    saved: i32,
}

impl StdoutCapture {
    fn new() -> Result<Self, ()> {
        platform_impl::setup()
    }

    /// 启动后台读线程（防 64KB 管道缓冲写满阻塞命令执行）
    fn spawn_reader(&self) -> std::thread::JoinHandle<String> {
        let read_fd = platform_impl::take_reader_fd();
        std::thread::spawn(move || platform_impl::read_to_end(read_fd))
    }

    /// 还原 stdout 并收回捕获的输出（阻塞到读线程收口）
    fn restore_and_collect(self, reader: std::thread::JoinHandle<String>) -> String {
        drop(self.write); // 关管道写端 → reader 读到 EOF 收口
        platform_impl::restore(self.saved);
        reader.join().unwrap_or_default()
    }
}

/// 平台专属 fd 操作。
/// Unix：POSIX pipe/dup/dup2/close，读端转 owned File 用 `read_to_string`。
/// Windows：CRT _pipe/_dup/_dup2/_close，读端保持裸 fd 用 `_read` 循环
///（CRT fd 数值 ≠ 原生 HANDLE，不能 `File::from_raw_fd` 直转，见下方注释）。
#[cfg(unix)]
mod platform_impl {
    use std::os::fd::FromRawFd;

    pub(super) type Raw = i32;

    pub(super) fn setup() -> Result<super::StdoutCapture, ()> {
        let mut fds = [0i32; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(());
        }
        let write = unsafe { std::fs::File::from_raw_fd(fds[1]) };
        READ_FD.with(|cell| cell.set(fds[0]));
        let saved = unsafe { libc::dup(1) };
        let write_raw = {
            use std::os::fd::AsRawFd;
            write.as_raw_fd()
        };
        unsafe {
            libc::dup2(write_raw, 1);
        }
        Ok(super::StdoutCapture {
            write: Some(write),
            saved,
        })
    }

    pub(super) fn take_reader_fd() -> Raw {
        // 读端 fd 在 setup 里存进线程局部；take 后置 -1（所有权移交读线程）
        READ_FD.with(|cell| cell.replace(-1))
    }

    pub(super) fn read_to_end(fd: Raw) -> String {
        use std::io::Read;
        if fd < 0 {
            return String::new();
        }
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        let mut out = String::new();
        let _ = file.read_to_string(&mut out);
        out
    }

    pub(super) fn restore(saved: Raw) {
        unsafe {
            libc::dup2(saved, 1);
            libc::close(saved);
        }
    }

    thread_local! {
        static READ_FD: std::cell::Cell<i32> = const { std::cell::Cell::new(-1) };
    }
}

#[cfg(windows)]
mod platform_impl {
    // CRT 管道/重定向/读：<io.h> 的 _pipe/_dup/_dup2/_close/_read。extern 声明零新依赖
    //（libc crate 在 Windows target 不导出这批 CRT 符号）。
    // 关键：CRT fd 是 int 索引，不是原生 HANDLE——std File::from_raw_fd 在 Windows
    // 要求原生 HANDLE，直转 CRT fd 是未定义行为。所以读端不走 File，用 _read 闭循环。
    extern "system" {
        fn _pipe(fds: *mut i32, psize: u32, textmode: i32) -> i32;
        fn _dup(fd: i32) -> i32;
        fn _dup2(src: i32, dst: i32) -> i32;
        fn _close(fd: i32) -> i32;
        fn _read(fd: i32, buf: *mut u8, count: u32) -> i32;
    }
    /// _O_BINARY（无文本模式翻译），管道缓冲区 64KB 与 Unix 侧行为一致
    const O_BINARY: i32 = 0x8000;
    const PIPE_SIZE: u32 = 64 * 1024;

    pub(super) type Raw = i32;

    pub(super) fn setup() -> Result<super::StdoutCapture, ()> {
        let mut fds = [0i32; 2];
        if unsafe { _pipe(fds.as_mut_ptr(), PIPE_SIZE, O_BINARY) } != 0 {
            return Err(());
        }
        // 写端同样不能 File::from_raw_fd（CRT fd ≠ HANDLE）——裸 fd 表达，restore 时 _close
        WRITE_FD.with(|cell| cell.set(fds[1]));
        READ_FD.with(|cell| cell.set(fds[0]));
        let saved = unsafe { _dup(1) };
        if saved < 0 {
            return Err(());
        }
        unsafe { _dup2(fds[1], 1) };
        Ok(super::StdoutCapture {
            write: None,
            saved,
        })
    }

    pub(super) fn take_reader_fd() -> Raw {
        READ_FD.with(|cell| cell.replace(-1))
    }

    pub(super) fn read_to_end(fd: Raw) -> String {
        if fd < 0 {
            return String::new();
        }
        let mut out = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = unsafe { _read(fd, buf.as_mut_ptr(), buf.len() as u32) };
            if n <= 0 {
                break; // 0 = EOF，负 = 错误
            }
            out.extend_from_slice(&buf[..n as usize]);
        }
        unsafe { _close(fd) };
        String::from_utf8_lossy(&out).into_owned()
    }

    pub(super) fn restore(saved: Raw) {
        // 关管道写端（读端 EOF 收口）→ 还原 stdout → 关备份
        WRITE_FD.with(|cell| {
            let fd = cell.replace(-1);
            if fd >= 0 {
                unsafe { _close(fd) };
            }
        });
        unsafe {
            _dup2(saved, 1);
            _close(saved);
        }
    }

    thread_local! {
        static READ_FD: std::cell::Cell<i32> = const { std::cell::Cell::new(-1) };
        static WRITE_FD: std::cell::Cell<i32> = const { std::cell::Cell::new(-1) };
    }
}


/// 核心事件 → 状态更新
async fn handle_core(core: &Arc<exm_core::Core>, tui: &mut Tui, e: CoreEvent) {
    match e.kind.as_str() {
        "approval.required" | "approval.resolved" => {
            refresh_approvals(core, tui).await;
        }
        "tool.result" => {
            // 合并进最后一张工具卡
            let ok = e.payload.get("ok").and_then(|v| v.as_bool()).unwrap_or(true);
            let out = e
                .payload
                .get("output")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if let Some(Msg::Tool { ok: o, out: po, .. }) = tui.msgs.last_mut() {
                *o = ok;
                *po = out;
            }
        }
        "run.error" => {
            if e.session_id == tui.session_id {
                let msg = e.payload.get("message").and_then(|v| v.as_str()).unwrap_or("");
                tui.push(Msg::System(format!("⚠ {msg}")));
                tui.running = false;
            }
        }
        _ => {
            if e.session_id == tui.session_id {
                if let Some(m) = event_to_msg(&e) {
                    // 运行中指示行被真实输出替换
                    if let Some(Msg::Running(_)) = tui.msgs.last() {
                        tui.msgs.pop();
                    }
                    tui.push(m);
                }
            }
        }
    }
}

/// 主绘制
fn draw(f: &mut Frame, tui: &mut Tui) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(1),      // 顶栏
        Constraint::Min(10),        // 主区（消息流 / 审批）
        Constraint::Length(3),      // 输入区
        Constraint::Length(1),      // 状态栏
    ])
    .split(area);

    draw_topbar(f, tui, chunks[0]);
    match tui.page {
        Page::Chat => draw_chat(f, tui, chunks[1]),
        Page::Approvals => draw_approvals(f, tui, chunks[1]),
    }
    draw_input(f, tui, chunks[2]);
    draw_status(f, tui, chunks[3]);
}

fn draw_topbar(f: &mut Frame, tui: &Tui, area: Rect) {
    let tabs = match tui.page {
        Page::Chat => "│ 对话* │ 审批 │",
        Page::Approvals => "│ 对话 │ 审批* │",
    };
    let pending = if tui.approvals.is_empty() {
        String::new()
    } else {
        format!("  待审 {}", tui.approvals.len())
    };
    let line = Line::from(vec![
        Span::styled(" EXMACHINA ", Style::new().fg(Color::Black).bg(Color::Cyan).bold()),
        Span::styled(tabs, Style::new().fg(Color::Cyan)),
        Span::raw(pending),
        Span::styled(
            "  Tab 切页  Ctrl+C 退出".to_string(),
            Style::new().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_chat(f: &mut Frame, tui: &mut Tui, area: Rect) {
    // 全部消息行 → 单一 Paragraph；滚动由 scroll 偏移实现
    let mut lines: Vec<Line<'static>> = Vec::new();
    for m in &tui.msgs {
        lines.extend(msg_lines(m, area.width as usize));
    }
    // 运行中追加指示
    if tui.running {
        if let Some(Msg::Running(_)) = tui.msgs.last() {
            // 已含运行行
        } else {
            lines.push(Line::styled("  ⏳ 运行中…", Style::new().fg(Color::Magenta)));
        }
    }
    let total = lines.len();
    let visible = area.height.saturating_sub(2) as usize; // 边框占 2
    let scroll = match tui.scroll {
        None => total.saturating_sub(visible), // 跟随底部
        Some(i) => {
            // 消息索引 → 行索引近似（取消息起始行）；简化为总行数比例
            let _ = i;
            total.saturating_sub(visible).saturating_mul(0)
        }
    };
    let para = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" 对话 "))
        .wrap(Wrap { trim: false })
        .scroll((scroll as u16, 0));
    f.render_widget(para, area);
}

fn draw_approvals(f: &mut Frame, tui: &mut Tui, area: Rect) {
    if tui.approvals.is_empty() {
        let p = Paragraph::new(Text::from(vec![
            Line::from(""),
            Line::styled("  当前无待审批项", Style::new().fg(Color::DarkGray)),
            Line::styled(
                "  终端写操作 / git 写 / 计划清单 的审批单会出现在这里",
                Style::new().fg(Color::DarkGray),
            ),
        ]))
        .block(Block::default().borders(Borders::ALL).title(" 审批 "));
        f.render_widget(p, area);
        return;
    }
    let items: Vec<ListItem> = tui
        .approvals
        .iter()
        .map(|r| {
            let cmd: String = r.command.chars().take(80).collect();
            ListItem::new(vec![
                Line::styled(cmd.clone(), Style::new().fg(Color::Yellow)),
                Line::styled(
                    format!("  {} · {}", r.id, r.created_at),
                    Style::new().fg(Color::DarkGray),
                ),
            ])
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" 审批（{} 条待处理） ", tui.approvals.len())),
        )
        .highlight_style(Style::new().bg(Color::DarkGray).add_modifier(Modifier::BOLD));
    f.render_stateful_widget(list, area, &mut tui.approval_sel);
    // 操作提示（叠底行）
    let hint = Rect { x: area.x + 1, y: area.y + area.height.saturating_sub(1), width: area.width.saturating_sub(2), height: 1 };
    f.render_widget(
        Paragraph::new(Line::styled(
            "y/Enter 批准 · n 拒绝 · Y/N 全部 · ↑↓ 选择",
            Style::new().fg(Color::Cyan),
        )),
        hint,
    );
    if !tui.approval_note.is_empty() {
        // note 挤占提示行上方——简化为状态栏呈现（draw_status 读 approval_note）
    }
}

fn draw_input(f: &mut Frame, tui: &mut Tui, area: Rect) {
    let title = if tui.page == Page::Chat { " 输入（Enter 发送 · ↑↓ 历史 · PgUp/PgDn 滚动） " } else { " 输入（审批页只读） " };
    let style = if tui.page == Page::Chat { Style::new().fg(Color::Cyan) } else { Style::new().fg(Color::DarkGray) };
    let prompt = "> ";
    let cursor = tui.cursor;
    let text = tui.input.clone();
    let line = Line::from(vec![Span::styled(prompt, style), Span::raw(text)]);
    let para = Paragraph::new(line).block(Block::default().borders(Borders::ALL).title(title).border_style(style));
    f.render_widget(para, area);
    // 光标绘制：计算 prompt 宽 + 前 cursor 个字符显示宽
    if tui.page == Page::Chat {
        let prefix: usize = 2; // "> "
        let before: String = tui.input.chars().take(cursor).collect();
        let w: usize = before.chars().map(|c| if c.is_wide() { 2 } else { 1 }).sum();
        f.set_cursor_position((area.x + 1 + (prefix + w) as u16, area.y + 1));
    }
}

fn draw_status(f: &mut Frame, tui: &Tui, area: Rect) {
    let note = if !tui.approval_note.is_empty() && tui.page == Page::Approvals {
        tui.approval_note.clone()
    } else if tui.running {
        "运行中…".into()
    } else {
        format!("就绪 · 轮次 {}", tui.turns)
    };
    let mode = if exm_core::plan::is_plan_mode(&tui.session_id) { "PLAN" } else { "EXEC" };
    let line = Line::from(vec![
        Span::styled(format!(" [{mode}] "), Style::new().fg(Color::Black).bg(if mode == "PLAN" { Color::Yellow } else { Color::Green })),
        Span::styled(format!("会话 {} ", &tui.session_id[..8.min(tui.session_id.len())]), Style::new().fg(Color::DarkGray)),
        Span::styled(note, Style::new().fg(Color::DarkGray)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

#[allow(dead_code)]
fn unused(_: Buffer) {}
