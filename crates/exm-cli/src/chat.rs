//! `exm chat` —— 进程内直挂 core 的一等交互
//!
//! 单发（`exm chat "任务"`）与交互面板（`exm chat` 进 TTY 面板）共用同一轮渲染
//! `run_round`：订阅 CoreEvent → 流式呈现指挥体/子个体/思维链/工具/回流 → 等待收束。

use crate::render::*;
use exm_core::types::{CoreEvent, TaskGraph};
use exm_core::Core;
use owo_colors::OwoColorize;
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;

pub struct ChatOptions {
    pub session: Option<String>,
    pub quiet: bool,
}

/// 订阅事件流并渲染一轮对话（CLI 单发 / 交互面板共用）。
///
/// 事件渲染循环与任务执行并行；`run.finished` / `run.error` 到达即返回。
pub async fn run_round(
    core: &Arc<Core>,
    session_id: &str,
    text: &str,
    quiet: bool,
) -> anyhow::Result<()> {
    let mut rx = core.subscribe();
    let mut last_graph: Option<TaskGraph> = None;
    let mut current_unit = String::new();
    let mut orch_streaming = false;
    let mut thinking_streaming = false;
    let sid = session_id.to_string();
    let orch_id_fallback = core.orchestrator_id();
    // 思维链只计字数，不逐字直出：规划 JSON / SyncReport 都是内部契约，
    // 在终端裸奔会把「一屏回答」冲成两页协议（此前 48 字节分块还顺带切碎了中文）。
    // 轮次收尾时给一行摘要，需要原文用 /tasks 与 WebUI 的收起栏。
    let mut think_chars: u64 = 0;
    let mut think_agents: Vec<(String, u64)> = Vec::new();

    let render_task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(evt) => {
                    if evt.session_id != sid {
                        continue;
                    }
                    match evt.kind.as_str() {
                        // 指挥体收束：给一行「收束中」提示即可——正文由 run.finished
                        // 以结构化陈述一次性呈现（此前两者都出，答案被打印两遍）
                        "orchestrator.token" => {
                            if quiet {
                                continue;
                            }
                            if let Some(d) = evt.payload.get("delta").and_then(|v| v.as_str()) {
                                if !d.is_empty() && !orch_streaming {
                                    if thinking_streaming || !current_unit.is_empty() {
                                        println!();
                                        thinking_streaming = false;
                                        current_unit.clear();
                                    }
                                    println!("{}", dim("  ⏳ 收束中…"));
                                    orch_streaming = true;
                                }
                            }
                        }
                        // 子个体产出的是 SyncReport 契约 JSON（内部协议）：
                        // 进度由 graph.updated（派发/回流）与 sync.received 呈现，此处不直出
                        "unit.token" => {
                            let agent = evt
                                .payload
                                .get("agentId")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unit")
                                .to_string();
                            if agent != current_unit {
                                current_unit = agent.clone();
                                orch_streaming = false;
                            }
                        }
                        // 思维链（分轨）：累计字数，收尾给摘要
                        "orchestrator.thinking" | "unit.thinking" => {
                            if quiet {
                                continue;
                            }
                            if let Some(d) = evt.payload.get("delta").and_then(|v| v.as_str()) {
                                let n = d.chars().count() as u64;
                                think_chars += n;
                                let who = evt
                                    .payload
                                    .get("agentId")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("指挥体")
                                    .to_string();
                                match think_agents.iter_mut().find(|(a, _)| a == &who) {
                                    Some((_, c)) => *c += n,
                                    None => think_agents.push((who, n)),
                                }
                                if !thinking_streaming {
                                    if orch_streaming || !current_unit.is_empty() {
                                        println!();
                                        orch_streaming = false;
                                        current_unit.clear();
                                    }
                                    println!("{}", dim("  ⌛ 思考中…"));
                                    thinking_streaming = true;
                                }
                            }
                        }
                        // 工具执行（文件操作 / 终端 / 截屏全程可见）
                        "tool.call" => {
                            if quiet {
                                continue;
                            }
                            if orch_streaming || !current_unit.is_empty() || thinking_streaming {
                                println!();
                                orch_streaming = false;
                                thinking_streaming = false;
                                current_unit.clear();
                            }
                            let tool = evt.payload.get("tool").and_then(|v| v.as_str()).unwrap_or("?");
                            let args = evt.payload.get("args").cloned().unwrap_or_default();
                            let brief = match tool {
                                "read" | "edit" => args.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                "terminal" => args.get("command").and_then(|v| v.as_str()).unwrap_or("").chars().take(120).collect(),
                                "filesystem" => format!(
                                    "{} {}",
                                    args.get("op").and_then(|v| v.as_str()).unwrap_or(""),
                                    args.get("path").and_then(|v| v.as_str()).unwrap_or("")
                                ),
                                "browser" | "computer" => args.get("op").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                other => other.to_string(),
                            };
                            println!("{}", format!("  ⚙ {tool} {brief}").yellow());
                        }
                        "tool.result" => {
                            if quiet {
                                continue;
                            }
                            let ok = evt.payload.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                            let ms = evt.payload.get("durationMs").and_then(|v| v.as_u64()).unwrap_or(0);
                            println!(
                                "{}",
                                dim(format!("  {} ({ms}ms)", if ok { "✔" } else { "✘ 失败" }))
                            );
                        }
                        "approval.required" => {
                            if orch_streaming || !current_unit.is_empty() {
                                println!();
                                orch_streaming = false;
                                current_unit.clear();
                            }
                            let cmd = evt.payload.get("command").and_then(|v| v.as_str()).unwrap_or("");
                            let id = evt.payload.get("approvalId").and_then(|v| v.as_str()).unwrap_or("");
                            println!("{}", err(format!("  ⏸ 待人工审批 [{id}]：{cmd}")));
                            println!("{}", dim("     用 /approvals 查看、/approve <id> 批准、/deny <id> 拒绝（批准后系统代执行并把输出回灌）。"));
                        }
                        "graph.updated" => {
                            if let Ok(g) = serde_json::from_value::<TaskGraph>(evt.payload.clone()) {
                                let changed: Vec<_> = g
                                    .nodes
                                    .iter()
                                    .filter(|n| {
                                        last_graph
                                            .as_ref()
                                            .and_then(|lg| lg.nodes.iter().find(|x| x.id == n.id))
                                            .map(|x| x.status != n.status)
                                            .unwrap_or(true)
                                    })
                                    .collect();
                                if !quiet && !changed.is_empty() {
                                    if orch_streaming || !current_unit.is_empty() {
                                        println!();
                                        orch_streaming = false;
                                        current_unit.clear();
                                    }
                                    for n in changed {
                                        println!("{}", node_line(n));
                                    }
                                }
                                last_graph = Some(g);
                            }
                        }
                        "sync.received" => {
                            if !current_unit.is_empty() {
                                println!();
                                current_unit.clear();
                            }
                            let agent = evt
                                .payload
                                .pointer("/report/sourceAgent")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?");
                            let conf = evt
                                .payload
                                .pointer("/report/confidence")
                                .and_then(|v| v.as_f64())
                                .unwrap_or(0.0);
                            let summary = evt
                                .payload
                                .pointer("/report/summary")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            println!("{}", format!("  ⇦ 同步回流 [{agent}] 置信度 {conf:.2}").cyan());
                            println!("{}", dim(format!("     {}", summary.chars().take(120).collect::<String>())));
                        }
                        "memory.recall" => {
                            let n = evt
                                .payload
                                .get("hits")
                                .and_then(|v| v.as_array())
                                .map(|a| a.len())
                                .unwrap_or(0);
                            if n > 0 {
                                println!("{}", dim(format!("  ⇧ 记忆召回 {n} 条")));
                            }
                        }
                        "memory.written" => {
                            let n = evt
                                .payload
                                .get("entries")
                                .and_then(|v| v.as_array())
                                .map(|a| a.len())
                                .unwrap_or(0);
                            println!("{}", dim(format!("  ⇩ 记忆写入 {n} 条（基础记忆已更新）")));
                        }
                        "arbitration.required" => {
                            println!("{}", err("  ⚖ 冲突待裁决，已追加裁决节点"));
                        }
                        "run.finished" => {
                            let statements: Vec<exm_core::types::Statement> = evt
                                .payload
                                .get("statements")
                                .cloned()
                                .and_then(|v| serde_json::from_value(v).ok())
                                .unwrap_or_default();
                            let orch_id = evt
                                .payload
                                .get("agentId")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| orch_id_fallback.clone());
                            if orch_streaming || thinking_streaming || !current_unit.is_empty() {
                                println!();
                                orch_streaming = false;
                                thinking_streaming = false;
                                current_unit.clear();
                            }
                            // 思维链摘要（原文是内部协议，不直出）
                            if think_chars > 0 {
                                let by = think_agents
                                    .iter()
                                    .map(|(a, c)| format!("{a} {c}"))
                                    .collect::<Vec<_>>()
                                    .join(" · ");
                                println!("{}", dim(format!("  ⌛ 思维链 {think_chars} 字（{by}）")));
                            }
                            println!(
                                "{}",
                                render_role("orchestrator", Some(orch_id.as_str()), &statements)
                            );
                            return;
                        }
                        "run.error" => {
                            let msg = evt.payload.get("message").and_then(|v| v.as_str()).unwrap_or("");
                            println!("{}", err(format!("【警告】运行失败：{msg}")));
                            return;
                        }
                        _ => {}
                    }
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return,
            }
        }
    });

    core.chat(session_id, text).await?;
    // 等待渲染循环收到 run.finished 并退出
    let _ = tokio::time::timeout(std::time::Duration::from_secs(600), render_task).await;
    Ok(())
}

pub async fn run_chat(core: Arc<Core>, text: &str, opts: &ChatOptions) -> anyhow::Result<()> {
    let session = match &opts.session {
        Some(id) => core
            .store
            .get_session(id)?
            .ok_or_else(|| anyhow::anyhow!("会话不存在: {id}"))?,
        None => core.create_session(text.chars().take(30).collect::<String>().as_str())?,
    };

    let channel_label = if core.is_mock() {
        "测试替身（EXM_LLM_MOCK=1）".to_string()
    } else {
        let cfg = core.config();
        if cfg.active_profile.is_empty() {
            cfg.llm.base_url.clone()
        } else {
            format!("{}（{}）", cfg.active_profile, cfg.llm.base_url)
        }
    };
    println!("{}", dim(format!("会话 {}｜LLM 通道：{}", session.id, channel_label)));
    println!("{}", render_role("user", None, &[exm_core::types::Statement::new(exm_core::types::SpeechTag::要求, text)]));
    println!();

    run_round(&core, &session.id, text, opts.quiet).await
}

/// 交互模式（非 TTY 兜底：管道 / 重定向场景的逐行对话）
pub async fn run_interactive(core: Arc<Core>, quiet: bool) -> anyhow::Result<()> {
    use std::io::{BufRead, Write};
    let session = core.create_session("CLI 交互会话")?;
    println!("{}", dim(format!("交互模式｜会话 {}｜输入 exit 退出", session.id)));
    let stdin = std::io::stdin();
    loop {
        print!("{}", "用户> ".yellow());
        std::io::stdout().flush().ok();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break;
        }
        let t = line.trim().to_string();
        // 行模式下输入不会被终端回显，先换行让命令输出（带框面板）另起一行：
        // 否则 `用户> ` 前缀与框首行同行，框的左边框看起来缩进了 6 格
        if !t.is_empty() {
            println!();
        }
        let t = t.as_str();
        if t.is_empty() {
            continue;
        }
        if t == "exit" || t == "quit" {
            break;
        }
        // `/` 命令走面板那套实现：管道里输入 /help 才会真的出帮助，
        // 而不是被当成普通任务发给模型
        if t.starts_with('/') {
            match crate::panel::handle_line(&core, t, quiet, &session.id).await {
                Ok(true) => break,
                Ok(false) => continue,
                Err(e) => eprintln!("{} {}", "命令失败：".red(), e),
            }
            continue;
        }
        run_chat(
            core.clone(),
            t,
            &ChatOptions { session: Some(session.id.clone()), quiet },
        )
        .await?;
    }
    Ok(())
}

/// 事件流给渠道消费的统一入口（Gateway/未来桌面端复用）
pub fn spawn_event_logger(mut rx: tokio::sync::broadcast::Receiver<CoreEvent>) {
    tokio::spawn(async move {
        while let Ok(evt) = rx.recv().await {
            println!("{}", dim(format!("[event] {}", evt.kind)));
        }
    });
}
