//! panel 域分文件：命令表 + 流排序 + 命令建议 + run_preview/prefs_root/run_panel_with_session 入口。

use super::*;

// ---------------------------------------------------------------- 面板入口

/// 全部面板命令（用于未知命令纠错建议）
const COMMANDS: &[&str] = &[
    "help", "exit", "quit", "version", "workspace", "clear", "status", "quiet", "theme", "caps",
    "copy", "new",
    "resume", "sessions", "session", "del", "tasks", "stop", "undo", "fork", "flows", "target", "singles",
    "groups", "group", "agents", "agent", "persona", "model", "config", "memory", "skills", "cron",
    "approvals", "approve", "deny", "checkpoints", "restore", "doctor",
];

/// 流列表统一排序（最后活跃倒序；/flows 与子命令序号寻址一致性由同一函数保证）
pub(crate) fn sorted_flows(core: &exm_core::Core) -> Vec<exm_core::types::ChannelFlow> {
    let mut flows = core.channel_flows(None);
    flows.sort_by(|a, b| b.last_active_at.cmp(&a.last_active_at));
    flows
}

/// 流归属 scope（与 REST /channels/flows/:id/sessions 同口径）：通道绑定组 →
/// 缺省回落默认单智能体（machina 优先），再缺省激活组
pub(crate) fn flow_scope(core: &exm_core::Core, channel_id: &str) -> String {
    let channels: Option<Vec<serde_json::Value>> = std::fs::read_to_string(
        core.config().data_dir.join("channels.json"),
    )
    .ok()
    .and_then(|s| serde_json::from_str(&s).ok());
    if let Some(group) = channels
        .unwrap_or_default()
        .iter()
        .find(|c| c["id"].as_str() == Some(channel_id))
        .and_then(|c| c["group"].as_str().map(|s| s.to_string()))
    {
        return group;
    }
    if core.registry().single("machina").is_some() {
        return "single:machina".into();
    }
    core.registry()
        .list_singles()
        .first()
        .map(|d| format!("single:{}", d.identifier))
        .unwrap_or_else(|| core.registry().active_scope())
}

/// 未知命令 → 最接近的命令（前缀优先，其次编辑距离 ≤2）
pub(crate) fn suggest_command(input: &str) -> Option<&'static str> {    let q = input.trim().to_lowercase();
    if q.is_empty() {
        return None;
    }
    let prefixed: Vec<&'static str> = COMMANDS
        .iter()
        .copied()
        .filter(|c| c.starts_with(&q) || q.starts_with(*c))
        .collect();
    if let Some(best) = prefixed.iter().min_by_key(|c| c.len()) {
        return Some(best);
    }
    COMMANDS
        .iter()
        .copied()
        .map(|c| (levenshtein(&q, c), c))
        .filter(|(d, _)| *d <= 2)
        .min_by_key(|(d, c)| (*d, c.len()))
        .map(|(_, c)| c)
}

/// 经典动态规划编辑距离（命令名都很短，成本可忽略）
pub(crate) fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// 跨线程共享的中断状态：Ctrl+C 语义 = 运行中中止轮次 / 空闲时退出
/// 非交互版式预览：只渲染横幅 + 输入框 + 小字摘要，用于确认终端渲染与 ANSI 支持。
/// 完整运行状态面板刻意不在此渲染——它由 `/status` 按需调出。
pub fn run_preview(core: &Arc<Core>, quiet: bool) -> anyhow::Result<()> {
    let workspace = core.config().workspace_root.clone();
    let panel = Panel::new(workspace, prefs_root(core), quiet);
    panel.print_banner();
    let session = core.create_session("CLI 面板会话")?;
    let state = PanelState {
        session_id: session.id.clone(),
        session_title: session.title.clone(),
        turns: 0,
    };
    panel.print_input_top();
    println!("{} {}", panel.prompt(), dim("（此处输入任务，Enter 发送）"));
    panel.print_input_bottom();
    panel.print_status_bar(core, &state);
    println!();
    println!(
        "{}",
        dim("—— 版式预览结束（未进入交互）")
    );
    println!(
        "{}",
        dim("  `exm` / `exmachina` 裸命令即进面板；完整运行状态用 /status 调出。")
    );
    // 预览用会话立即清理，不留垃圾
    let _ = core.delete_session(&session.id);
    Ok(())
}

/// 偏好落盘目录 = 配置文件所在目录（`.exmachina`）
pub fn prefs_root(core: &Arc<Core>) -> PathBuf {
    core.config()
        .config_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| core.config().workspace_root.clone())
}

/// 进入终端交互面板（TTY 场景；非 TTY 由 `chat::run_interactive` 兜底）
/// 带初始会话启动（-c/-r/--session 定位结果；None = 新建「CLI 面板会话」）
pub async fn run_panel_with_session(
    core: Arc<Core>,
    initial: Option<exm_core::types::Session>,
    quiet: bool,
) -> anyhow::Result<()> {
    let workspace = core.config().workspace_root.clone();
    let mut panel = Panel::new(workspace, prefs_root(&core), quiet);
    panel.clear();
    panel.print_banner();

    let session = match initial {
        Some(s) => s,
        None => core.create_session("CLI 面板会话")?,
    };
    let mut state = PanelState {
        session_id: session.id.clone(),
        session_title: session.title.clone(),
        turns: 0,
    };
    // 版式刻意保持最简：字符画横幅 → 输入框 → 输入框下一行小字摘要。
    // 完整运行状态不常驻，要看得用 /status（与 /clear 重绘同理）。

    let itr = Arc::new(Interrupt::new(&state.session_id));
    spawn_interrupt_listener(core.clone(), itr.clone());

    let stdin = std::io::stdin();
    loop {
        panel.print_input_top();
        print!("{}", panel.prompt());
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            // Ctrl+D / EOF：收口输入框与信息行
            panel.print_input_bottom();
            panel.print_status_bar(&core, &state);
            println!();
            break;
        }
        // 输入内容已由终端回显在框内，无需再 echo 一遍
        panel.print_input_bottom();
        panel.print_status_bar(&core, &state);
        let text = line.trim().to_string();
        if text.is_empty() {
            println!();
            continue;
        }
        if text.starts_with('/') {
            match panel.dispatch(&core, &mut state, &text).await {
                Ok(Flow::Exit) => break,
                Ok(Flow::Continue) => {
                    itr.set_session(&state.session_id);
                }
                Err(e) => println!("{} {}", err("命令失败："), e),
            }
            println!();
            continue;
        }
        let started = std::time::Instant::now();
        itr.busy.store(true, Ordering::SeqCst);
        let result = chat::run_round(&core, &state.session_id, &text, panel.quiet).await;
        itr.busy.store(false, Ordering::SeqCst);
        if let Err(e) = result {
            println!("{}", err(format!("【错误】{e}")));
        }
        let ms = started.elapsed().as_millis() as u64;
        state.turns += 1;
        panel.print_round_footer(&core, &state, ms);
    }
    panel.print_farewell(&state);
    Ok(())
}

