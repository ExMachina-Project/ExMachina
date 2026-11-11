//! `exm`（裸命令）/ `exm chat` 交互面板 —— 终端最简形态（区别于桌面端与 WebUI）
//!
//! 版式三段：**字符画横幅 → 输入框 → 输入框下一行小字摘要**；完整运行状态不常驻，
//! 由 `/status` 按需调出。本形态刻意保持零图形、零额外依赖：所有设置与运维动作都通过
//! `/` 命令完成（会话 / 交互目标 / 编成 / 模型 / 配置 / 记忆 / 技能 / 定时 / 审批 /
//! 检查点 / 体检）。
//!
//! 命令实现直接复用既有 CLI 子命令函数（`group_cmd` / `platform_cmd` / `memory_cmd` /
//! `persona_cmd` / `config_cmd` / `doctor`），保证面板与命令行口径完全一致。

use crate::render::{dim, err, ok, warn};
use crate::{chat, config_cmd, doctor, group_cmd, memory_cmd, persona_cmd, platform_cmd};
use exm_core::types::Tier;
use exm_core::Core;
use owo_colors::OwoColorize;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use parking_lot::Mutex;
use std::sync::{Arc};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 面板强调色（对齐 WebUI 五套主题）
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Theme {
    Cyan,
    Green,
    Amber,
    Violet,
    Ice,
}

impl Theme {
    fn parse(s: &str) -> Option<Theme> {
        match s.trim().to_lowercase().as_str() {
            "cyan" | "青" | "荧光青" => Some(Theme::Cyan),
            "green" | "绿" | "矩阵绿" => Some(Theme::Green),
            "amber" | "琥珀" => Some(Theme::Amber),
            "violet" | "purple" | "紫" | "紫芒" => Some(Theme::Violet),
            "ice" | "冰蓝" | "blue" => Some(Theme::Ice),
            _ => None,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Theme::Cyan => "cyan（荧光青）",
            Theme::Green => "green（矩阵绿）",
            Theme::Amber => "amber（琥珀）",
            Theme::Violet => "violet（紫芒）",
            Theme::Ice => "ice（冰蓝）",
        }
    }
    /// 落盘用的稳定标识（与 `parse` 互逆）
    fn name(self) -> &'static str {
        match self {
            Theme::Cyan => "cyan",
            Theme::Green => "green",
            Theme::Amber => "amber",
            Theme::Violet => "violet",
            Theme::Ice => "ice",
        }
    }
}

/// 面板偏好：落盘 `.exmachina/cli_prefs.json`，重启后保持
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct PanelPrefs {
    #[serde(default)]
    theme: String,
    #[serde(default)]
    quiet: bool,
}

fn prefs_path(root: &Path) -> PathBuf {
    root.join("cli_prefs.json")
}

fn load_prefs(root: &Path) -> PanelPrefs {
    std::fs::read_to_string(prefs_path(root))
        .ok()
        .and_then(|s| serde_json::from_str::<PanelPrefs>(&s).ok())
        .unwrap_or_default()
}

fn save_prefs(root: &Path, p: &PanelPrefs) {
    if let Ok(s) = serde_json::to_string_pretty(p) {
        let _ = std::fs::create_dir_all(root);
        let _ = std::fs::write(prefs_path(root), s);
    }
}

/// 面板会话状态（当前会话 + 轮次计数）
pub struct PanelState {
    pub session_id: String,
    pub session_title: String,
    pub turns: usize,
}

/// 面板命令流转（TUI 与 readline 面板共享语义）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Exit,
}

pub struct Panel {
    pub(crate) theme: Theme,
    pub(crate) quiet: bool,
    pub(crate) width: usize,
    pub(crate) workspace: PathBuf,
    /// 偏好落盘目录（`.exmachina`）
    pub(crate) prefs_root: PathBuf,
}

impl Panel {
    pub fn new(workspace: PathBuf, prefs_root: PathBuf, quiet: bool) -> Self {
        let width = term_width();
        let prefs = load_prefs(&prefs_root);
        let theme = Theme::parse(&prefs.theme).unwrap_or(Theme::Cyan);
        Panel { theme, quiet: quiet || prefs.quiet, width, workspace, prefs_root }
    }

    /// 主题 / 静默变更后落盘
    fn persist(&self) {
        save_prefs(
            &self.prefs_root,
            &PanelPrefs { theme: self.theme.name().to_string(), quiet: self.quiet },
        );
    }

    // ---------------------------------------------------------------- 颜色

    fn accent(&self, s: &str) -> String {
        match self.theme {
            Theme::Cyan => s.bright_cyan().to_string(),
            Theme::Green => s.bright_green().to_string(),
            Theme::Amber => s.yellow().to_string(),
            Theme::Violet => s.purple().to_string(),
            Theme::Ice => s.bright_blue().to_string(),
        }
    }

    fn accent_bold(&self, s: &str) -> String {
        match self.theme {
            Theme::Cyan => s.bright_cyan().bold().to_string(),
            Theme::Green => s.bright_green().bold().to_string(),
            Theme::Amber => s.yellow().bold().to_string(),
            Theme::Violet => s.purple().bold().to_string(),
            Theme::Ice => s.bright_blue().bold().to_string(),
        }
    }

    // ---------------------------------------------------------------- 画框

    fn rule_top(&self, title: &str) -> String {
        let used = 5 + disp_width(title);
        let fill = self.width.saturating_sub(used);
        format!(
            "{} {} {}",
            self.accent("╭─"),
            self.accent(title),
            self.accent(&format!("{}╮", "─".repeat(fill)))
        )
    }

    fn rule_mid(&self, content: &str) -> String {
        format!("{} {} {}", self.accent("│"), fit(content, self.width.saturating_sub(4)), self.accent("│"))
    }

    fn rule_bottom(&self) -> String {
        self.accent(&format!("╰{}╯", "─".repeat(self.width.saturating_sub(2))))
    }

    // ---------------------------------------------------------------- 横幅

    fn clear(&self) {
        if std::env::var("EXM_NO_CLEAR").is_ok() {
            return;
        }
        print!("\x1b[2J\x1b[H");
        let _ = std::io::stdout().flush();
    }

    fn print_banner(&self) {
        println!();
        let rows = banner_rows();
        let banner_w = disp_width(&rows[0]) + 2; // 含左侧两格缩进
        if self.width >= banner_w {
            for r in &rows {
                println!("  {}", self.accent(r));
            }
        } else {
            // 窄终端（<64 列）放不下整幅字标：折行会把版式撑破，
            // 此时只留文字标识，不画字标
            println!(
                "  {}",
                self.accent(&format!("EX·MACHINA{}", " ".repeat(banner_w.saturating_sub(12))))
            );
        }
        println!(
            "  {}",
            dim(format!(
                "理性至上主义的智能体   v{VERSION}   ｜   /help 查看命令"
            ))
        );
        println!();
    }

    // ---------------------------------------------------------------- 信息面板

    fn print_status(&self, core: &Arc<Core>, state: &PanelState) {
        let cfg = core.config();
        let reg = core.registry();
        let mut rows: Vec<(&str, String)> = Vec::new();

        rows.push(("工作区", shorten_path(&self.workspace, self.width.saturating_sub(16))));

        match reg.active_single() {
            Some(id) => {
                let name = reg.single(&id).map(|d| d.name).unwrap_or_else(|| id.clone());
                rows.push(("交互目标", format!("智能体 {name}（{id}）")));
                rows.push(("说明", "不经调度，直接服务；回组用 /target group <id>".to_string()));
            }
            None => {
                let gid = core.active_group();
                let meta = core.group_meta(&gid);
                let gname = meta.as_ref().map(|m| m.name.clone()).unwrap_or_else(|| gid.clone());
                let builtin = meta.as_ref().map(|m| m.builtin).unwrap_or(false);
                let primary = meta.as_ref().and_then(|m| m.primary.clone()).unwrap_or_else(|| core.orchestrator_id());
                rows.push((
                    "交互目标",
                    format!("组 {gname}（{gid}）· {} 个体", reg.group_agent_count(&gid)),
                ));
                rows.push((
                    "指挥体",
                    format!("{primary}{}", if builtin { "（内置组）" } else { "" }),
                ));
            }
        }

        let channel = if core.is_mock() {
            "测试替身（EXM_LLM_MOCK=1）".to_string()
        } else {
            let configured = !cfg.llm.api_key.trim().is_empty()
                || !cfg.llm.api_keys.is_empty()
                || cfg
                    .llm_profiles
                    .iter()
                    .any(|p| !p.api_key.trim().is_empty() || !p.api_keys.is_empty());
            if !configured {
                "未配置 —— 用 /model 配置提供商".to_string()
            } else {
                let profile = if cfg.active_profile.is_empty() { "（全局通道）".to_string() } else { cfg.active_profile.clone() };
                format!("{profile} / {} · 真实推理", cfg.llm.model)
            }
        };
        rows.push(("模型", channel));

        rows.push((
            "会话",
            format!("{} · {} 轮 · {}", short_id(&state.session_id), state.turns, state.session_title),
        ));

        let mem = if cfg.memory_enabled {
            format!("深层记忆（召回 {} 条）", cfg.memory_recall_limit)
        } else {
            "文件记忆（memory.md 全文注入）".to_string()
        };
        rows.push((
            "策略",
            format!("{}｜审批 {}｜并发 {}", mem, cfg.security.exec_approval, cfg.max_concurrency),
        ));

        rows.push((
            "命令",
            format!(
                "/help 命令 · /target 目标（{} 组 · {} 智能体）· /exit 退出",
                core.list_groups().len(),
                reg.list_singles().len()
            ),
        ));

        println!("{}", self.rule_top("运行状态"));
        for (label, value) in rows {
            println!("{}", self.rule_mid(&self.kv(label, &value)));
        }
        println!("{}", self.rule_bottom());
    }

    fn kv(&self, label: &str, value: &str) -> String {
        format!("{}  {}", pad_disp(label, 8), value)
    }

    /// 当前交互目标的一句话描述（组 / 智能体）
    fn target_label(&self, core: &Arc<Core>) -> String {
        let reg = core.registry();
        match reg.active_single() {
            Some(id) => {
                let name = reg.single(&id).map(|d| d.name).unwrap_or_else(|| id.clone());
                format!("智能体 {name}（{id}）· 不经调度直接服务")
            }
            None => {
                let gid = core.active_group();
                let meta = core.group_meta(&gid);
                let gname = meta.as_ref().map(|m| m.name.clone()).unwrap_or_else(|| gid.clone());
                let builtin = meta.as_ref().map(|m| m.builtin).unwrap_or(false);
                let primary = meta.as_ref().and_then(|m| m.primary.clone()).unwrap_or_else(|| core.orchestrator_id());
                format!(
                    "组 {gname}（{gid}）· {} 个体{}｜指挥体 {primary}",
                    reg.group_agent_count(&gid),
                    if builtin { "· 内置" } else { "" }
                )
            }
        }
    }

    /// 输入框下方的小字摘要行：常驻的唯一信息面，按面板宽度自适应装填芯片。
    ///
    /// 完整运行状态不常驻（那会占掉大半个屏），要看用 `/status`。
    fn print_status_bar(&self, core: &Arc<Core>, state: &PanelState) {
        let cfg = core.config();
        let reg = core.registry();
        let target = match reg.active_single() {
            Some(id) => {
                let name = reg.single(&id).map(|d| d.name).unwrap_or_else(|| id.clone());
                format!("智能体 {name}（{id}）")
            }
            None => {
                let gid = core.active_group();
                let gname = core.group_meta(&gid).map(|m| m.name).unwrap_or_else(|| gid.clone());
                format!("组 {gname} · {}体", reg.group_agent_count(&gid))
            }
        };
        // 状态栏只放模型名（档案前缀在信息面板里给全，这里省列宽）
        // 未配置模型时给出显式占位，避免出现「MODEL  │」这种标签悬空的空芯片
        let model = if cfg.llm.model.trim().is_empty() {
            if exm_core::config::mock_enabled_from_env() {
                "mock（测试替身）".to_string()
            } else {
                "未配置".to_string()
            }
        } else {
            cfg.llm.model.clone()
        };
        let tokens = core.session_tokens_estimate(&state.session_id);

        let candidates = [
            self.chip("TARGET", &target),
            self.chip("MODEL", &model),
            self.chip("SESS", &format!("{} · {}轮", short_id(&state.session_id), state.turns)),
            self.chip("TOK", &format!("≈{tokens}")),
        ];

        let sep = dim(" │ ");
        let kept = fit_chips(candidates.to_vec(), self.width, disp_width(&sep));
        if !kept.is_empty() {
            // 整行压暗：这是脚注性质的补充信息，不该抢输入框的视觉重心
            println!("  {}", dim(kept.join(&sep)));
        }
    }

    /// 终端口径的键值芯片：暗色大写键 + 常规值
    fn chip(&self, key: &str, value: &str) -> String {
        format!("{} {}", dim(key), value)
    }

    // ---------------------------------------------------------------- 输入框

    fn print_input_top(&self) {
        println!("{}", self.rule_top("输入"));
    }

    fn prompt(&self) -> String {
        format!("{} {} ", self.accent("│"), self.accent_bold("❯"))
    }

    fn print_input_bottom(&self) {
        println!("{}", self.rule_bottom());
    }

    fn print_round_footer(&self, core: &Arc<Core>, state: &PanelState, ms: u64) {
        let (prompt, completion, calls) = core.session_usage(&state.session_id);
        let total = core.session_tokens_estimate(&state.session_id);
        let mut parts = vec![format!("⏱ {:.1}s", ms as f64 / 1000.0)];
        if calls > 0 {
            parts.push(format!("↑{prompt} ↓{completion} tokens"));
        }
        parts.push(format!("会话累计 ≈{total} tokens"));
        parts.push(format!("第 {} 轮", state.turns));
        println!("{}", dim(format!("  {}", parts.join(" · "))));
        println!();
    }

    // ---------------------------------------------------------------- 帮助

    fn print_help(&self) {
        println!("{}", self.accent_bold("面板命令（所有设置都走 `/` 命令）"));
        let groups: &[(&str, &[(&str, &str)])] = HELP_GROUPS;
        for row in render_help(groups, self.width) {
            match row {
                HelpRow::Title(t) => println!("\n{}", self.accent_bold(&t)),
                HelpRow::Entry { head, tail } => match tail {
                    Some(t) => println!("{}{}", head, dim(&t)),
                    None => println!("{}", head),
                },
            }
        }
        println!("\n{}", dim("  提示：Ctrl+C 中止当前轮次；空闲时再按一次退出面板。"));
        println!();
    }

    // ---------------------------------------------------------------- 分发

    /// 斜杠命令分派（TUI 复用同一命令面；输出走 println，TUI 侧捕获后入消息流）
    pub async fn dispatch(&mut self, core: &Arc<Core>, state: &mut PanelState, line: &str) -> anyhow::Result<Flow> {
        let body = line.trim().trim_start_matches('/');
        let tokens = tokenize(body);
        if tokens.is_empty() {
            self.print_help();
            return Ok(Flow::Continue);
        }
        let cmd = tokens[0].to_lowercase();
        let rest = &tokens[1..];
        let args = parse_args(rest);

        match cmd.as_str() {
            // ---------------- 面板 ----------------
            "help" | "h" | "?" => self.print_help(),
            "exit" | "quit" | "q" => return Ok(Flow::Exit),
            "version" => {
                println!("EXMACHINA v{VERSION}（exm-cli 终端面板）");
                println!("{}", dim(format!("工作区 {}", self.workspace.display())));
            }
            "workspace" | "pwd" => println!("{}", self.workspace.display()),
            "clear" | "cls" => {
                // 只重绘横幅：常驻版式里没有状态面板，别把它又请回来
                self.clear();
                self.print_banner();
            }
            "status" => self.print_status(core, state),
            "quiet" => {
                match rest.first().map(|s| s.as_str()) {
                    Some("on") | Some("1") | Some("true") => self.quiet = true,
                    Some("off") | Some("0") | Some("false") => self.quiet = false,
                    _ => self.quiet = !self.quiet,
                }
                self.persist();
                println!(
                    "{} 静默模式：{}（已记忆，下次进入面板保持）",
                    ok("完成"),
                    if self.quiet { "开（只输出最终结果）" } else { "关（完整过程）" }
                );
            }
            "theme" => match rest.first().and_then(|s| Theme::parse(s)) {
                Some(t) => {
                    self.theme = t;
                    self.persist();
                    println!("{} 强调色已切换：{}（已记忆）", ok("完成"), self.theme.label());
                }
                None => println!(
                    "{}",
                    warn("用法：/theme cyan|green|amber|violet|ice")
                ),
            },
            // 终端能力探测（spec cli-tui 7.4）：显示当前终端的色彩/宽字符/剪贴板
            // 探测结果与降级档位——受限终端（如 TERM=vt100）下的降级可见可诊断
            "caps" => {
                println!("{}", self.accent_bold("终端能力探测"));
                println!("  {}", crate::term_caps::caps_summary());
                let c = crate::term_caps::TermCaps::detect();
                match c.clipboard {
                    crate::term_caps::ClipboardKind::Unavailable => println!(
                        "{}",
                        dim("  剪贴板不可用：/copy 将降级为打印文本（手动选择复制）")
                    ),
                    _ => println!("{}", dim("  /copy <文本> 走探测到的通道复制")),
                }
            }
            // 复制到剪贴板（探测通道：本地工具 → OSC52 → 降级打印）
            "copy" => {
                let text = rest.join(" ");
                if text.is_empty() {
                    println!("{}", warn("用法：/copy <文本>（通道探测见 /caps）"));
                    return Ok(Flow::Continue);
                }
                match crate::term_caps::copy_to_clipboard(&text) {
                    Ok(()) => println!("{} 已复制到剪贴板", ok("完成")),
                    Err(_) => {
                        // 降级回退：打印待复制文本（终端下用户用鼠标选择复制）
                        println!("{}", warn("剪贴板不可用，请手动复制以下文本："));
                        println!("{text}");
                    }
                }
            }

            // ---------------- 会话 ----------------
            "new" => {
                let title = if rest.is_empty() { "CLI 面板会话".to_string() } else { rest.join(" ") };
                let s = core.create_session(&title)?;
                state.session_id = s.id.clone();
                state.session_title = s.title.clone();
                state.turns = 0;
                println!("{} 已新建会话 {}（{}）", ok("完成"), short_id(&s.id), s.title);
            }
            "resume" => {
                let list = core.list_sessions_in_scope()?;
                match list.first() {
                    Some(s) => {
                        state.session_id = s.id.clone();
                        state.session_title = s.title.clone();
                        state.turns = 0;
                        println!(
                            "{} 已恢复最近会话 {}（{}）",
                            ok("完成"),
                            short_id(&s.id),
                            s.title
                        );
                        self.print_status_bar(core, state);
                    }
                    None => println!("{}", dim("当前交互目标下暂无会话；直接输入任务即新建。")),
                }
            }
            "sessions" => {
                let list = core.list_sessions_in_scope()?;
                if list.is_empty() {
                    println!("{}", dim("（当前交互目标下暂无会话）"));
                }
                for (i, s) in list.iter().enumerate() {
                    let mark = if s.id == state.session_id { "●".green().to_string() } else { " ".to_string() };
                    let preview = s.last_message_preview.clone().unwrap_or_default();
                    println!(
                        "{} {:>2}. {} {} {}",
                        mark,
                        i + 1,
                        short_id(&s.id),
                        s.title.bold(),
                        dim(preview.chars().take(48).collect::<String>())
                    );
                }
            }
            "session" => {
                let Some(token) = rest.first() else {
                    println!("{}", warn("用法：/session <序号|id>（先 /sessions 查看）"));
                    return Ok(Flow::Continue);
                };
                match resolve_session(core, token) {
                    Some(s) => {
                        state.session_id = s.id.clone();
                        state.session_title = s.title.clone();
                        state.turns = 0;
                        println!("{} 已切换会话 {}（{}）", ok("完成"), short_id(&s.id), s.title);
                        self.print_status_bar(core, state);
                    }
                    None => println!("{}", err(format!("未找到会话：{token}"))),
                }
            }
            "del" | "delete-session" => {
                let Some(token) = rest.first() else {
                    println!("{}", warn("用法：/del <序号|id>"));
                    return Ok(Flow::Continue);
                };
                match resolve_session(core, token) {
                    Some(s) => {
                        if s.id == state.session_id {
                            println!("{}", err("不能删除当前会话；请先 /session 切到别的会话"));
                        } else {
                            core.delete_session(&s.id)?;
                            println!("{} 已删除会话 {}（{}）", ok("完成"), short_id(&s.id), s.title);
                        }
                    }
                    None => println!("{}", err(format!("未找到会话：{token}"))),
                }
            }
            "tasks" => match core.store.latest_graph(&state.session_id)? {
                Some(g) => {
                    println!("{}", format!("图 {}｜状态 {:?}", g.id, g.status).bold());
                    for n in &g.nodes {
                        println!("{}", crate::render::node_line(n));
                    }
                }
                None => println!("{}", dim("该会话暂无编排图")),
            },
            "stop" => match core.stop_session(&state.session_id) {
                Ok(n) => println!("{} 已请求停止（当时已执行 {n} 次工具调用）", ok("完成")),
                Err(e) => println!("{}", err(format!("{e}"))),
            },
            "undo" => {
                let Some(t) = rest.first().and_then(|s| s.parse::<usize>().ok()) else {
                    println!("{}", warn("用法：/undo <轮次>（0 = 回到会话起点）"));
                    return Ok(Flow::Continue);
                };
                match core.undo_session(&state.session_id, t, true).await {
                    Ok(info) => {
                        let archived = info["archived"].as_u64().unwrap_or(0);
                        let restored = info["restored"].as_array().map(|a| a.len()).unwrap_or(0);
                        state.turns = t;
                        println!(
                            "{} 已撤销至第 {t} 轮（归档 {archived} 条，联动回滚 {restored} 个文件）",
                            ok("完成")
                        );
                    }
                    Err(e) => println!("{}", err(format!("{e}"))),
                }
            }
            "fork" => {
                let Some(t) = rest.first().and_then(|s| s.parse::<usize>().ok()) else {
                    println!("{}", warn("用法：/fork <轮次>"));
                    return Ok(Flow::Continue);
                };
                match core.fork_session(&state.session_id, t) {
                    Ok(f) => {
                        println!("{} 已派生分支会话 {}（截至第 {t} 轮）", ok("完成"), short_id(&f.id));
                        println!("{}", dim(format!("  /session {} 切到分支继续；原会话保持不变。", f.id)));
                    }
                    Err(e) => println!("{}", err(format!("{e}"))),
                }
            }

            // ---------------- 外部聊天流（spec cli-tui：/flows 视图） ----------------
            // 流状态与 WebUI/通道侧同源（core 流文档 + 未读计数内存态）；
            // 打开 = 清未读 + 切到绑定会话；/flows use 切换流内会话（与 REST
            // /channels/flows/:id/active 同一绑定状态，一处切换处处反映）
            "flows" => match rest.first().map(|s| s.as_str()) {
                None | Some("list") => {
                    let flows = sorted_flows(core);
                    if flows.is_empty() {
                        println!("{}", dim("（暂无外部聊天流；通道消息进入后自动建流）"));
                        return Ok(Flow::Continue);
                    }
                    println!("{}", self.accent_bold("外部聊天流（通道 · 对端 · 预览 · 绑定/未读）"));
                    for (i, f) in flows.iter().enumerate() {
                        let unread = core.flow_unread_count(&f.channel_id, &f.chat_key);
                        let badge = if unread > 0 {
                            format!("未读 {unread}").yellow().bold().to_string()
                        } else {
                            dim("已读")
                        };
                        let bind = match &f.active_session {
                            Some(sid) => format!("绑定 {}", short_id(sid)),
                            None => dim("未绑定").to_string(),
                        };
                        let preview = f.last_message.chars().take(36).collect::<String>();
                        println!(
                            "  {:>2}. [{}] {} {} {} {}",
                            i + 1,
                            f.channel_id,
                            if f.display_name.is_empty() { f.chat_key.as_str() } else { &f.display_name }.bold(),
                            dim(format!("「{preview}」")),
                            bind,
                            badge,
                        );
                    }
                    println!("{}", dim("  /flows open <序号> 打开流（进入绑定会话并清未读）；/flows sessions <序号> 看流内会话"));
                }
                Some("open") => {
                    let Some(idx) = rest.get(1).and_then(|s| s.parse::<usize>().ok()) else {
                        println!("{}", warn("用法：/flows open <序号>（/flows 查看列表）"));
                        return Ok(Flow::Continue);
                    };
                    let flows = sorted_flows(core);
                    let Some(f) = flows.get(idx.saturating_sub(1)) else {
                        println!("{}", err(format!("序号越界：{idx}（共 {} 条流）", flows.len())));
                        return Ok(Flow::Continue);
                    };
                    // 绑定会话不存在时按通道语义开新会话并回写绑定
                    // （与 REST/通道入站同一原子路径：查绑定→建会话→回写）
                    let scope = flow_scope(core, &f.channel_id);
                    let title = format!("channel:{}:{}", f.channel_id, f.chat_key);
                    let name = if f.display_name.is_empty() {
                        None
                    } else {
                        Some(f.display_name.as_str())
                    };
                    let sid = f
                        .active_session
                        .clone()
                        .filter(|sid| core.store.get_session(sid).ok().flatten().is_some())
                        .or_else(|| {
                            core.ensure_flow_session_atomic(
                                &f.channel_id,
                                &f.chat_key,
                                &scope,
                                &title,
                                name,
                                f.is_group,
                            )
                        })
                        .unwrap_or_default();
                    if sid.is_empty() {
                        println!("{}", err("打开失败：无法建立流会话"));
                        return Ok(Flow::Continue);
                    }
                    core.clear_flow_unread(&f.channel_id, &f.chat_key);
                    match core.store.get_session(&sid).ok().flatten() {
                        Some(s) => {
                            state.session_id = s.id.clone();
                            state.session_title = s.title.clone();
                            state.turns = 0;
                            println!(
                                "{} 已打开流会话 {}（{}）——与 WebUI 同源生效",
                                ok("完成"),
                                short_id(&s.id),
                                s.title
                            );
                            self.print_status_bar(core, state);
                        }
                        None => println!("{}", err(format!("会话不存在：{sid}"))),
                    }
                }
                Some("sessions") => {
                    let Some(idx) = rest.get(1).and_then(|s| s.parse::<usize>().ok()) else {
                        println!("{}", warn("用法：/flows sessions <序号>（/flows 查看列表）"));
                        return Ok(Flow::Continue);
                    };
                    let flows = sorted_flows(core);
                    let Some(f) = flows.get(idx.saturating_sub(1)) else {
                        println!("{}", err(format!("序号越界：{idx}（共 {} 条流）", flows.len())));
                        return Ok(Flow::Continue);
                    };
                    // 流内会话：与 REST /channels/flows/:id/sessions 同口径
                    // （标题前缀归属 + 活跃倒序 + 活跃绑定标记）
                    let title_prefix = format!("channel:{}:{}:", f.channel_id, f.chat_key);
                    let bare_title = format!("channel:{}:{}", f.channel_id, f.chat_key);
                    let scope = flow_scope(core, &f.channel_id);
                    let mut mine: Vec<_> = core
                        .list_sessions_in_group(&scope)
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|s| s.title.starts_with(&title_prefix) || s.title == bare_title)
                        .collect();
                    mine.sort_by(|a, b| {
                        b.updated_at
                            .cmp(&a.updated_at)
                            .then_with(|| b.created_at.cmp(&a.created_at))
                            .then_with(|| a.id.cmp(&b.id))
                    });
                    if mine.is_empty() {
                        println!("{}", dim("（该流暂无会话）"));
                        return Ok(Flow::Continue);
                    }
                    println!("{}", self.accent_bold(&format!("流内会话（{} 条）", mine.len())));
                    for (k, s) in mine.iter().enumerate() {
                        let mark = if f.active_session.as_deref() == Some(s.id.as_str()) {
                            "●".green().to_string()
                        } else {
                            " ".to_string()
                        };
                        let label = s
                            .title
                            .strip_prefix(&title_prefix)
                            .filter(|t| !t.is_empty())
                            .map(|t| t.to_string())
                            .unwrap_or_else(|| s.created_at.chars().take(10).collect::<String>());
                        println!("{} {:>2}. {} {}", mark, k + 1, short_id(&s.id), label.bold());
                    }
                    println!("{}", dim("  /flows use <流序号> <会话序号> 切换绑定"));
                }
                Some("use") => {
                    let (Some(fi), Some(si)) = (
                        rest.get(1).and_then(|s| s.parse::<usize>().ok()),
                        rest.get(2).and_then(|s| s.parse::<usize>().ok()),
                    ) else {
                        println!("{}", warn("用法：/flows use <流序号> <会话序号>"));
                        return Ok(Flow::Continue);
                    };
                    let flows = sorted_flows(core);
                    let Some(f) = flows.get(fi.saturating_sub(1)) else {
                        println!("{}", err(format!("流序号越界：{fi}")));
                        return Ok(Flow::Continue);
                    };
                    let title_prefix = format!("channel:{}:{}:", f.channel_id, f.chat_key);
                    let bare_title = format!("channel:{}:{}", f.channel_id, f.chat_key);
                    let scope = flow_scope(core, &f.channel_id);
                    let mut mine: Vec<_> = core
                        .list_sessions_in_group(&scope)
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|s| s.title.starts_with(&title_prefix) || s.title == bare_title)
                        .collect();
                    mine.sort_by(|a, b| {
                        b.updated_at
                            .cmp(&a.updated_at)
                            .then_with(|| b.created_at.cmp(&a.created_at))
                            .then_with(|| a.id.cmp(&b.id))
                    });
                    let Some(s) = mine.get(si.saturating_sub(1)) else {
                        println!("{}", err(format!("会话序号越界：{si}（共 {} 条）", mine.len())));
                        return Ok(Flow::Continue);
                    };
                    match core.bind_flow_session(&f.channel_id, &f.chat_key, &s.id) {
                        Ok(()) => {
                            state.session_id = s.id.clone();
                            state.session_title = s.title.clone();
                            state.turns = 0;
                            core.clear_flow_unread(&f.channel_id, &f.chat_key);
                            println!(
                                "{} 流已切到会话 {}（{}）——通道侧与 WebUI 同步生效",
                                ok("完成"),
                                short_id(&s.id),
                                s.title
                            );
                            self.print_status_bar(core, state);
                        }
                        Err(e) => println!("{}", err(format!("改绑失败：{e}"))),
                    }
                }
                Some(other) => println!("{}", warn(format!("未知子命令：{other}（open / sessions / use）"))),
            },

            // ---------------- 交互目标与编成 ----------------
            "target" | "t" => match rest.first().map(|s| s.as_str()) {
                None => self.print_targets(core),
                Some("group") | Some("g") => match rest.get(1) {
                    Some(id) => self.switch_target_group(core, state, id)?,
                    None => println!("{}", warn("用法：/target group <组id>（/groups 查看）")),
                },
                Some("single") | Some("s") | Some("agent") => match rest.get(1) {
                    Some(id) => self.switch_target_single(core, state, id)?,
                    None => println!("{}", warn("用法：/target single <智能体id>（/singles 查看）")),
                },
                // 简写 /target <id>：组优先，其次智能体
                Some(id) => {
                    if core.group_meta(id).is_some() {
                        self.switch_target_group(core, state, id)?;
                    } else if core.registry().single(id).is_some() {
                        self.switch_target_single(core, state, id)?;
                    } else {
                        println!("{}", err(format!("未找到组或智能体：{id}（/target 查看全部）")));
                    }
                }
            },
            "singles" => {
                let singles = core.registry().list_singles();
                if singles.is_empty() {
                    println!("{}", dim("（无智能体；智能体为预置，不经调度直接服务）"));
                }
                let active = core.registry().active_single();
                for s in singles {
                    let mark = if active.as_deref() == Some(s.identifier.as_str()) { "●".green().to_string() } else { " ".to_string() };
                    println!(
                        "{} {} {} — {}",
                        mark,
                        s.identifier.cyan(),
                        s.name.bold(),
                        dim(s.description.chars().take(40).collect::<String>())
                    );
                }
                println!("{}", dim("  /target single <id> 切为交互目标"));
            }
            "groups" => group_cmd::run_list(core)?,
            "group" => {
                let sub = rest.first().map(|s| s.as_str()).unwrap_or("list");
                match sub {
                    "list" => group_cmd::run_list(core)?,
                    "switch" | "use" => match rest.get(1) {
                        Some(id) => self.switch_target_group(core, state, id)?,
                        None => println!("{}", warn("用法：/group switch <id>")),
                    },
                    "create" => match rest.get(1) {
                        Some(name) => {
                            let desc = args.kv.get("desc").cloned().unwrap_or_default();
                            group_cmd::run_create(core, name, args.kv.get("id").cloned(), &desc)?;
                        }
                        None => println!("{}", warn("用法：/group create <名称> [--id id] [--desc 描述]")),
                    },
                    "info" => group_cmd::run_info(core, rest.get(1).cloned())?,
                    "delete" | "remove" => match rest.get(1) {
                        Some(id) => group_cmd::run_delete(core, id)?,
                        None => println!("{}", warn("用法：/group delete <id>")),
                    },
                    // 简写：/group <id> 直接切换
                    other => self.switch_target_group(core, state, other)?,
                }
            }
            "agents" => print_agents(core, rest.first().map(|s| s.as_str()))?,
            "agent" => {
                let sub = rest.first().map(|s| s.as_str()).unwrap_or("");
                match sub {
                    "create" | "add" => match rest.get(1) {
                        Some(name) => {
                            let identifier = args
                                .kv
                                .get("id")
                                .or_else(|| args.kv.get("identifier"))
                                .cloned()
                                .ok_or_else(|| anyhow::anyhow!("缺少 --id <identifier>"))?;
                            let desc = args.kv.get("desc").or_else(|| args.kv.get("description")).cloned().unwrap_or_default();
                            let domain = args.kv.get("domain").cloned().unwrap_or_else(|| "自定义".to_string());
                            let tier = args.kv.get("tier").cloned().unwrap_or_else(|| "unit".to_string());
                            group_cmd::run_agent_create(
                                core,
                                name,
                                &identifier,
                                &desc,
                                &domain,
                                &tier,
                                args.kv.get("group").map(|s| s.as_str()),
                                args.kv.get("prompt").cloned(),
                            )?;
                        }
                        None => println!(
                            "{}",
                            warn("用法：/agent create <名称> --id <identifier> --desc <职责> [--tier orchestrator|unit]")
                        ),
                    },
                    "remove" | "rm" => match rest.get(1) {
                        Some(id) => group_cmd::run_agent_remove(core, id, args.kv.get("group").map(|s| s.as_str()))?,
                        None => println!("{}", warn("用法：/agent remove <identifier>")),
                    },
                    "optimize" => match rest.get(1) {
                        Some(id) => platform_cmd::run_agent_optimize(core, id).await?,
                        None => println!("{}", warn("用法：/agent optimize <identifier>")),
                    },
                    "adaptation" => match rest.get(1) {
                        Some(id) => platform_cmd::run_agent_adaptation(core, id)?,
                        None => println!("{}", warn("用法：/agent adaptation <identifier>")),
                    },
                    "reset-adaptation" => match rest.get(1) {
                        Some(id) => platform_cmd::run_agent_reset_adaptation(core, id)?,
                        None => println!("{}", warn("用法：/agent reset-adaptation <identifier>")),
                    },
                    _ => println!("{}", warn("用法：/agent create|remove|primary|optimize|adaptation|reset-adaptation …")),
                }
            }
            "persona" => {
                let sub = rest.first().map(|s| s.as_str()).unwrap_or("");
                match sub {
                    "get" => match rest.get(1) {
                        Some(id) => persona_cmd::run_get(core, id)?,
                        None => println!("{}", warn("用法：/persona get <identifier>")),
                    },
                    "set" => match rest.get(1) {
                        Some(id) => {
                            let text = if rest.len() > 2 { Some(rest[2..].join(" ")) } else { args.kv.get("text").cloned() };
                            persona_cmd::run_set(core, id, text, args.kv.get("file").map(PathBuf::from))?;
                        }
                        None => println!("{}", warn("用法：/persona set <identifier> <人设文本>")),
                    },
                    "reset" => match rest.get(1) {
                        Some(id) => persona_cmd::run_reset(core, id)?,
                        None => println!("{}", warn("用法：/persona reset <identifier>")),
                    },
                    _ => println!("{}", warn("用法：/persona get|set|reset <identifier> [文本]")),
                }
            }

            // ---------------- 模型与配置 ----------------
            "model" | "models" => {
                let sub = rest.first().map(|s| s.as_str()).unwrap_or("list");
                match sub {
                    "list" => platform_cmd::run_model_list(core)?,
                    "use" => match rest.get(1) {
                        Some(id) => {
                            platform_cmd::run_model_use(core, id)?;
                            self.print_status_bar(core, state);
                        }
                        None => println!("{}", warn("用法：/model use <id>")),
                    },
                    "test" => platform_cmd::run_model_test(core, rest.get(1).map(|s| s.as_str())).await?,
                    "add" => match rest.get(1) {
                        Some(id) => {
                            let need = |k: &str| -> anyhow::Result<String> {
                                args.kv.get(k).cloned().ok_or_else(|| anyhow::anyhow!("缺少 --{k}"))
                            };
                            let name = args.kv.get("name").cloned().unwrap_or_else(|| id.clone());
                            let base_url = need("base-url")?;
                            let key = args.kv.get("key").cloned().unwrap_or_default();
                            let model = need("model")?;
                            platform_cmd::run_model_add(core, id, &name, &base_url, &key, &model)?;
                            self.print_status_bar(core, state);
                        }
                        None => println!(
                            "{}",
                            warn("用法：/model add <id> --name <名称> --base-url <url> --key <key> --model <模型>")
                        ),
                    },
                    "remove" | "rm" => match rest.get(1) {
                        Some(id) => platform_cmd::run_model_remove(core, id)?,
                        None => println!("{}", warn("用法：/model remove <id>")),
                    },
                    _ => platform_cmd::run_model_list(core)?,
                }
            }
            "config" => {
                let sub = rest.first().map(|s| s.as_str()).unwrap_or("list");
                match sub {
                    "list" => config_cmd::run_config_list(&self.workspace)?,
                    "get" => match rest.get(1) {
                        Some(k) => config_cmd::run_config_get(&self.workspace, k)?,
                        None => println!("{}", warn("用法：/config get <键>")),
                    },
                    "set" => match (rest.get(1), rest.get(2)) {
                        (Some(k), Some(v)) => {
                            config_cmd::run_config_set(&self.workspace, k, v)?;
                            self.print_status_bar(core, state);
                        }
                        _ => println!("{}", warn("用法：/config set <键> <值>")),
                    },
                    "schema" => config_cmd::run_config_schema()?,
                    _ => config_cmd::run_config_list(&self.workspace)?,
                }
            }

            // ---------------- 记忆 ----------------
            "memory" | "mem" => {
                let sub = rest.first().map(|s| s.as_str()).unwrap_or("list");
                match sub {
                    "list" => {
                        memory_cmd::run_memory_list(core, args.kv.get("kind").cloned(), args.kv.get("agent").map(|s| s.as_str()), 20)?
                    }
                    "search" => {
                        let query = args.pos.iter().skip(1).cloned().collect::<Vec<_>>().join(" ");
                        if query.trim().is_empty() {
                            println!("{}", warn("用法：/memory search <查询>"));
                        } else {
                            memory_cmd::run_memory_search(core, &query, args.kv.get("agent").map(|s| s.as_str()), 5)?
                        }
                    }
                    "add" => {
                        let kind = args.kv.get("kind").cloned().unwrap_or_else(|| "fact".to_string());
                        match (args.pos.get(1), args.pos.get(2)) {
                            (Some(title), Some(body)) => {
                                let tags = args
                                    .kv
                                    .get("tags")
                                    .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
                                    .unwrap_or_default();
                                let importance = args.kv.get("importance").and_then(|s| s.parse().ok()).unwrap_or(0.6);
                                memory_cmd::run_memory_add(
                                    core,
                                    &kind,
                                    title,
                                    body,
                                    args.bools.contains("pin"),
                                    args.kv.get("agent").map(|s| s.as_str()),
                                    tags,
                                    importance,
                                    args.bools.contains("global"),
                                )?;
                            }
                            _ => println!("{}", warn("用法：/memory add --kind fact <标题> <内容> [--pin] [--agent a] [--global]")),
                        }
                    }
                    "pin" => match rest.get(1) {
                        Some(id) => memory_cmd::run_memory_pin(core, id, !args.bools.contains("off"))?,
                        None => println!("{}", warn("用法：/memory pin <id> [--off]")),
                    },
                    "forget" => match rest.get(1) {
                        Some(id) => memory_cmd::run_memory_forget(core, id)?,
                        None => println!("{}", warn("用法：/memory forget <id>")),
                    },
                    "decay" => memory_cmd::run_memory_decay(core, 0.05)?,
                    "reindex" => memory_cmd::run_memory_reindex(core)?,
                    "render" => memory_cmd::run_memory_render(core)?,
                    "stats" => memory_cmd::run_memory_stats(core)?,
                    _ => memory_cmd::run_memory_list(core, None, None, 20)?,
                }
            }

            // ---------------- 平台 ----------------
            "skills" => platform_cmd::run_skill_list(core)?,
            "cron" => {
                let sub = rest.first().map(|s| s.as_str()).unwrap_or("list");
                match sub {
                    "list" => platform_cmd::run_cron_list(core)?,
                    "add" => match rest.get(1) {
                        Some(name) => {
                            let prompt = args
                                .kv
                                .get("prompt")
                                .cloned()
                                .ok_or_else(|| anyhow::anyhow!("缺少 --prompt <提示词>"))?;
                            platform_cmd::run_cron_add(
                                core,
                                name,
                                &prompt,
                                args.kv.get("cron").cloned(),
                                args.kv.get("at").cloned(),
                                args.kv.get("group").cloned(),
                                args.kv.get("session-title").cloned(),
                                args.kv.get("kind").cloned(),
                                args.kv.get("target-session").cloned(),
                                args.kv
                                    .get("steps")
                                    .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
                                    .unwrap_or_default(),
                            )?;
                        }
                        None => println!(
                            "{}",
                            warn("用法：/cron add <名称> --prompt <提示词> [--cron \"分 时 日 月 周\"] [--at <ISO8601>]\
                                  \n       连续任务：--kind queue --steps id1,id2")
                        ),
                    },
                    "run" => match rest.get(1) {
                        Some(id) => platform_cmd::run_cron_now(core, id).await?,
                        None => println!("{}", warn("用法：/cron run <id>")),
                    },
                    "enable" => match rest.get(1) {
                        Some(id) => platform_cmd::run_cron_enable(core, id, true)?,
                        None => println!("{}", warn("用法：/cron enable <id>")),
                    },
                    "disable" => match rest.get(1) {
                        Some(id) => platform_cmd::run_cron_enable(core, id, false)?,
                        None => println!("{}", warn("用法：/cron disable <id>")),
                    },
                    "remove" | "rm" => match rest.get(1) {
                        Some(id) => platform_cmd::run_cron_remove(core, id)?,
                        None => println!("{}", warn("用法：/cron remove <id>")),
                    },
                    "runs" => platform_cmd::run_cron_runs(core, args.kv.get("job").map(|s| s.as_str()), 20)?,
                    _ => platform_cmd::run_cron_list(core)?,
                }
            }
            "approvals" | "approval" => {
                platform_cmd::run_approval_list(core, args.kv.get("status").map(|s| s.as_str()), 50)?
            }
            "approve" => match rest.first() {
                Some(id) => platform_cmd::run_approval_decide(core, id, true).await?,
                None => println!("{}", warn("用法：/approve <id>")),
            },
            "deny" => match rest.first() {
                Some(id) => platform_cmd::run_approval_decide(core, id, false).await?,
                None => println!("{}", warn("用法：/deny <id>")),
            },
            "checkpoints" | "checkpoint" => print_checkpoints(core),
            "restore" => match rest.first() {
                Some(path) => {
                    let msg = core.restore_checkpoint(args.kv.get("date").map(|s| s.as_str()), path)?;
                    println!("{}", ok(msg));
                }
                None => println!("{}", warn("用法：/restore <工作区相对路径> [--date YYYY-MM-DD]")),
            },
            "doctor" => match doctor::run_doctor(&self.workspace) {
                Ok(()) => {}
                Err(e) => println!("{}", warn(format!("{e}"))),
            },

            other => {
                println!("{}", err(format!("未知命令：/{other}")));
                match suggest_command(other) {
                    Some(s) => println!("{}", dim(format!("  是否想用 /{s}？输入 /help 查看全部命令。"))),
                    None => println!("{}", dim("  输入 /help 查看全部命令。")),
                }
            }
        }
        Ok(Flow::Continue)
    }

    /// 列出全部可选交互目标（组 / 智能体）
    fn print_targets(&self, core: &Arc<Core>) {
        println!("{}", self.accent_bold("当前交互目标"));
        println!("  {}", self.target_label(core));

        println!("\n{}", self.accent_bold("// 智能体组（集群协作，经调度）"));
        let single_mode = core.registry().single_mode();
        for g in core.list_groups() {
            let mark = if !single_mode && g.id == core.active_group() { "●".green().to_string() } else { " ".to_string() };
            println!(
                "{} {} {} {} {}",
                mark,
                g.id.cyan(),
                g.name.bold(),
                dim(format!("主智能体 {}", g.primary.clone().unwrap_or_else(|| "未设置".into()))),
                if g.builtin { dim("内置") } else { String::new() }
            );
        }

        println!("\n{}", self.accent_bold("// 智能体（不经调度直接服务）"));
        let singles = core.registry().list_singles();
        if singles.is_empty() {
            println!("  {}", dim("（无）"));
        }
        let active = core.registry().active_single();
        for s in singles {
            let mark = if active.as_deref() == Some(s.identifier.as_str()) { "●".green().to_string() } else { " ".to_string() };
            println!(
                "{} {} {} — {}",
                mark,
                s.identifier.cyan(),
                s.name.bold(),
                dim(s.description.chars().take(40).collect::<String>())
            );
        }
        println!("\n{}", dim("  切换：/target group <id>（集群）或 /target single <id>（智能体）"));
    }

    /// 切到某个组：退出智能体模式 → 设激活组 → 新会话
    fn switch_target_group(&mut self, core: &Arc<Core>, state: &mut PanelState, id: &str) -> anyhow::Result<()> {
        core.registry().set_active_single(None)?;
        group_cmd::run_switch(core, id)?;
        self.on_target_switched(core, state);
        Ok(())
    }

    /// 切到某个智能体（不经调度直接服务）
    fn switch_target_single(&mut self, core: &Arc<Core>, state: &mut PanelState, id: &str) -> anyhow::Result<()> {
        core.registry().set_active_single(Some(id))?;
        let name = core.registry().single(id).map(|d| d.name).unwrap_or_else(|| id.to_string());
        println!("{} 交互目标已切换：智能体 {}（{id}）· 不经调度直接服务", ok("完成"), name);
        self.on_target_switched(core, state);
        Ok(())
    }

    /// 交互目标切换后：会话按范围隔离 → 自动新建会话
    fn on_target_switched(&mut self, core: &Arc<Core>, state: &mut PanelState) {
        match core.create_session("CLI 面板会话") {
            Ok(s) => {
                state.session_id = s.id.clone();
                state.session_title = s.title.clone();
                state.turns = 0;
                println!("{}", dim(format!("已随交互目标切换新建会话 {}", short_id(&s.id))));
            }
            Err(e) => println!("{}", err(format!("新建会话失败：{e}"))),
        }
        self.print_status(core, state);
    }

    fn print_farewell(&self, state: &PanelState) {
        println!(
            "{}",
            dim(format!(
                "会话 {} 已保留；重新进入面板后 /sessions 可切回（或 exm chat --session {} \"…\"）。",
                short_id(&state.session_id),
                state.session_id
            ))
        );
    }
}


// ---------------------------------------------------------------- 域分文件

mod commands;
mod events;
mod render;

pub(crate) use commands::*;
pub(crate) use events::*;
pub(crate) use render::*;

#[cfg(test)]
mod tests;
