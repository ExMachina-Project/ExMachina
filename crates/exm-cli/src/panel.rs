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
use std::sync::{Arc, Mutex};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 面板强调色（对齐 WebUI 五套主题）
#[derive(Clone, Copy, PartialEq)]
enum Theme {
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
struct PanelState {
    session_id: String,
    session_title: String,
    turns: usize,
}

enum Flow {
    Continue,
    Exit,
}

struct Panel {
    theme: Theme,
    quiet: bool,
    width: usize,
    workspace: PathBuf,
    /// 偏好落盘目录（`.exmachina`）
    prefs_root: PathBuf,
}

impl Panel {
    fn new(workspace: PathBuf, prefs_root: PathBuf, quiet: bool) -> Self {
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
        for r in banner_rows() {
            println!("  {}", self.accent(&r));
        }
        println!(
            "  {}",
            dim(format!(
                "群体协作智能体系统   v{VERSION}   ｜   /help 查看命令"
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
                rows.push(("说明", "单体服务，不经调度；回组用 /target group <id>".to_string()));
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
                format!("智能体 {name}（{id}）· 单体服务，不经调度")
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
        let model = cfg.llm.model.clone();
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

    async fn dispatch(&mut self, core: &Arc<Core>, state: &mut PanelState, line: &str) -> anyhow::Result<Flow> {
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
                    println!("{}", format!("图 {}｜状态 {:?}", g.id, g.status).bold().to_string());
                    for n in &g.nodes {
                        println!("{}", crate::render::node_line(n));
                    }
                }
                None => println!("{}", dim("该会话暂无任务图")),
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
                    println!("{}", dim("（无智能体；智能体为预置单体，不经调度直接服务）"));
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
                    "primary" => match rest.get(1) {
                        Some(id) => group_cmd::run_agent_set_primary(core, id, args.kv.get("group").map(|s| s.as_str()))?,
                        None => println!("{}", warn("用法：/agent primary <identifier>")),
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
                            )?;
                        }
                        None => println!("{}", warn("用法：/cron add <名称> --prompt <提示词> [--cron \"分 时 日 月 周\"] [--at <ISO8601>]")),
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

        println!("\n{}", self.accent_bold("// 智能体（单体服务，不经调度）"));
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
        println!("\n{}", dim("  切换：/target group <id>（集群）或 /target single <id>（单体）"));
    }

    /// 切到某个组：退出单体模式 → 设激活组 → 新会话
    fn switch_target_group(&mut self, core: &Arc<Core>, state: &mut PanelState, id: &str) -> anyhow::Result<()> {
        core.registry().set_active_single(None)?;
        group_cmd::run_switch(core, id)?;
        self.on_target_switched(core, state);
        Ok(())
    }

    /// 切到某个智能体（单体服务）
    fn switch_target_single(&mut self, core: &Arc<Core>, state: &mut PanelState, id: &str) -> anyhow::Result<()> {
        core.registry().set_active_single(Some(id))?;
        let name = core.registry().single(id).map(|d| d.name).unwrap_or_else(|| id.to_string());
        println!("{} 交互目标已切换：智能体 {}（{id}）· 单体服务，不经调度", ok("完成"), name);
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

// ---------------------------------------------------------------- 面板入口

/// 全部面板命令（用于未知命令纠错建议）
const COMMANDS: &[&str] = &[
    "help", "exit", "quit", "version", "workspace", "clear", "status", "quiet", "theme", "new",
    "resume", "sessions", "session", "del", "tasks", "stop", "undo", "fork", "target", "singles",
    "groups", "group", "agents", "agent", "persona", "model", "config", "memory", "skills", "cron",
    "approvals", "approve", "deny", "checkpoints", "restore", "doctor",
];

/// 未知命令 → 最接近的命令（前缀优先，其次编辑距离 ≤2）
fn suggest_command(input: &str) -> Option<&'static str> {
    let q = input.trim().to_lowercase();
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
fn levenshtein(a: &str, b: &str) -> usize {
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
struct Interrupt {
    busy: AtomicBool,
    closing: AtomicBool,
    session: Mutex<String>,
}

impl Interrupt {
    fn new(session: &str) -> Self {
        Interrupt {
            busy: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            session: Mutex::new(session.to_string()),
        }
    }
    fn set_session(&self, id: &str) {
        if let Ok(mut s) = self.session.lock() {
            *s = id.to_string();
        }
    }
    fn current_session(&self) -> String {
        self.session.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

/// 常驻监听 Ctrl+C：运行中 → 中止当前轮次；空闲 → 打印告别并退出；连按 → 强制退出
fn spawn_interrupt_listener(core: Arc<Core>, itr: Arc<Interrupt>) {
    tokio::spawn(async move {
        loop {
            if tokio::signal::ctrl_c().await.is_err() {
                return;
            }
            if itr.closing.load(Ordering::SeqCst) {
                std::process::exit(130);
            }
            if itr.busy.load(Ordering::SeqCst) {
                let sid = itr.current_session();
                if !sid.is_empty() {
                    let _ = core.stop_session(&sid);
                }
                println!(
                    "\n{}",
                    warn("⏹ 已请求中止当前轮次（再按一次 Ctrl+C 退出面板）")
                );
            } else {
                itr.closing.store(true, Ordering::SeqCst);
                println!(
                    "\n{}",
                    dim("已退出面板；会话已保留（再次进入后 /resume 或 /sessions 可切回）。")
                );
                std::process::exit(0);
            }
        }
    });
}

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
fn prefs_root(core: &Arc<Core>) -> PathBuf {
    core.config()
        .config_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| core.config().workspace_root.clone())
}

/// 进入终端交互面板（TTY 场景；非 TTY 由 `chat::run_interactive` 兜底）
pub async fn run_panel(core: Arc<Core>, quiet: bool) -> anyhow::Result<()> {
    let workspace = core.config().workspace_root.clone();
    let mut panel = Panel::new(workspace, prefs_root(&core), quiet);
    panel.clear();
    panel.print_banner();

    let session = core.create_session("CLI 面板会话")?;
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

// ---------------------------------------------------------------- 辅助

/// 字符画字母表：EXMACHINA（每字母 5 列 × 5 行，等宽块元素）
const BANNER_LETTERS: [[&str; 5]; 9] = [
    // E
    ["█████", "█    ", "████ ", "█    ", "█████"],
    // X
    ["█   █", " █ █ ", "  █  ", " █ █ ", "█   █"],
    // M
    ["█   █", "██ ██", "█ █ █", "█   █", "█   █"],
    // A
    [" ███ ", "█   █", "█████", "█   █", "█   █"],
    // C
    [" ████", "█    ", "█    ", "█    ", " ████"],
    // H
    ["█   █", "█   █", "█████", "█   █", "█   █"],
    // I
    ["█████", "  █  ", "  █  ", "  █  ", "█████"],
    // N
    ["█   █", "██  █", "█ █ █", "█  ██", "█   █"],
    // A
    [" ███ ", "█   █", "█████", "█   █", "█   █"],
];

/// 非 TTY（管道 / 重定向）逐行模式下复用同一套 `/` 命令实现。
///
/// 否则在管道里输入 `/help` 会被当成普通任务发给模型。返回 `true` 表示请求退出。
pub async fn handle_line(core: &Arc<Core>, line: &str, quiet: bool, session_id: &str) -> anyhow::Result<bool> {
    let mut panel = Panel::new(
        core.config().workspace_root.clone(),
        prefs_root(core),
        quiet,
    );
    // 标题与轮次都从既有会话回读，让 /session、/tasks 等落在正确的会话上
    let title = core
        .list_sessions()?
        .into_iter()
        .find(|s| s.id == session_id)
        .map(|s| s.title)
        .unwrap_or_default();
    let mut state = PanelState {
        session_id: session_id.to_string(),
        session_title: title,
        turns: core.session_usage(session_id).2,
    };
    Ok(matches!(panel.dispatch(core, &mut state, line).await?, Flow::Exit))
}

/// 由字母表拼出横幅 5 行（保证各行列宽严格一致）
fn banner_rows() -> Vec<String> {
    let mut rows = vec![String::new(); 5];
    for (i, letter) in BANNER_LETTERS.iter().enumerate() {
        for (r, line) in letter.iter().enumerate() {
            if i > 0 {
                rows[r].push(' ');
            }
            rows[r].push_str(line);
        }
    }
    rows
}

fn print_agents(core: &Core, filter: Option<&str>) -> anyhow::Result<()> {
    let defs = match filter {
        Some(cap) if !cap.trim().is_empty() => core.registry.find_by_capability(cap),
        _ => core.agents(),
    };
    let mut last_domain = String::new();
    for d in defs {
        if d.domain != last_domain {
            last_domain = d.domain.clone();
            println!("\n{}", last_domain.bold().underline().to_string());
        }
        let tier = if matches!(d.tier, Tier::Orchestrator) {
            format!(" {} ", "指挥体".on_blue().white())
        } else {
            "  ".to_string()
        };
        println!(
            "{} {} {} — {}",
            tier,
            d.identifier.cyan(),
            d.name.bold(),
            dim(d.description.chars().take(40).collect::<String>())
        );
    }
    println!(
        "\n{}",
        dim(format!(
            "共 {}（指挥体 1 + 子个体 {}）",
            core.registry.count(),
            core.registry.units().len()
        ))
    );
    Ok(())
}

fn print_checkpoints(core: &Core) {
    let rows = core.list_checkpoints();
    if rows.is_empty() {
        println!("{}", dim("尚无检查点（写/编辑文件前会自动快照到 .exmachina/checkpoints/）"));
        return;
    }
    println!("{:<12} {:<50} {:>10}", "日期", "文件", "大小");
    for (date, rel, size) in rows.iter().take(200) {
        println!("{date:<12} {rel:<50} {size:>9} B");
    }
    println!("（共 {} 份快照）", rows.len());
}

/// 会话解析：序号（1 起）或 id（精确 / 前缀）
fn resolve_session(core: &Core, token: &str) -> Option<exm_core::types::Session> {
    let list = core.list_sessions_in_scope().ok()?;
    if let Ok(idx) = token.parse::<usize>() {
        if idx >= 1 && idx <= list.len() {
            return list.get(idx - 1).cloned();
        }
        return None;
    }
    list.iter()
        .find(|s| s.id == token)
        .or_else(|| list.iter().find(|s| s.id.starts_with(token)))
        .cloned()
}

#[derive(Default)]
struct Args {
    pos: Vec<String>,
    kv: HashMap<String, String>,
    bools: HashSet<String>,
}

/// 已知的布尔开关（`--flag` 后不接值）
const BOOL_FLAGS: &[&str] = &["pin", "off", "global", "quiet", "verbose", "json", "force"];

fn parse_args(tokens: &[String]) -> Args {
    let mut a = Args::default();
    let mut i = 0;
    while i < tokens.len() {
        let t = &tokens[i];
        if let Some(rest) = t.strip_prefix("--") {
            if let Some((k, v)) = rest.split_once('=') {
                a.kv.insert(k.to_string(), v.to_string());
                i += 1;
                continue;
            }
            if BOOL_FLAGS.contains(&rest) {
                a.bools.insert(rest.to_string());
                i += 1;
                continue;
            }
            if i + 1 < tokens.len() && !tokens[i + 1].starts_with("--") {
                a.kv.insert(rest.to_string(), tokens[i + 1].clone());
                i += 2;
                continue;
            }
            a.bools.insert(rest.to_string());
            i += 1;
        } else {
            a.pos.push(t.clone());
            i += 1;
        }
    }
    a
}

/// 分词：支持双引号包裹的空格
fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    for c in s.chars() {
        match c {
            '"' => in_q = !in_q,
            c if c.is_whitespace() && !in_q => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 终端宽度（COLUMNS / EXM_PANEL_WIDTH，缺省 72，夹在 48..=100）
fn term_width() -> usize {
    let raw = std::env::var("EXM_PANEL_WIDTH")
        .or_else(|_| std::env::var("COLUMNS"))
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(72);
    raw.clamp(48, 100)
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// 贪心装填状态栏芯片：按顺序累加，装不下即停（首枚即超宽则截断保留）
fn fit_chips(chips: Vec<String>, width: usize, sep_w: usize) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    let mut total = 0usize;
    for c in chips {
        let w = disp_width(&c);
        let add = if kept.is_empty() { w } else { w + sep_w };
        if total + add <= width {
            total += add;
            kept.push(c);
        } else if kept.is_empty() {
            kept.push(fit(&c, width));
            break;
        } else {
            break;
        }
    }
    kept
}

fn shorten_path(p: &std::path::Path, max: usize) -> String {
    let mut s = p.display().to_string();
    // Windows canonicalize 会带 verbatim 前缀（\\?\ 与 \\?\UNC\），面板里去掉更易读
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        s = format!(r"\\{rest}");
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        s = rest.to_string();
    }
    if disp_width(&s) <= max {
        return s;
    }
    // 从尾部保留（项目名比盘符更有信息量），宽度按显示列计算
    let mut tail = String::new();
    let mut w = 0usize;
    for c in s.chars().rev() {
        let cw = char_width(c);
        if w + cw > max.saturating_sub(1) {
            break;
        }
        tail.insert(0, c);
        w += cw;
    }
    format!("…{tail}")
}

/// 单个字符的显示宽度（CJK / 全角 / emoji 记 2 列）
fn char_width(c: char) -> usize {
    let cp = c as u32;
    match cp {
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1FAFF
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

/// 可见显示宽度（跳过 ANSI 转义序列）
fn disp_width(s: &str) -> usize {
    let mut w = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c2 in chars.by_ref() {
                    if ('@'..='~').contains(&c2) {
                        break;
                    }
                }
            }
            continue;
        }
        w += char_width(c);
    }
    w
}

/// 截断并补齐到指定显示宽度（保留 ANSI 序列；截断处补省略号）
fn fit(s: &str, width: usize) -> String {
    let total = disp_width(s);
    if total <= width {
        let mut out = s.to_string();
        let mut w = total;
        while w < width {
            out.push(' ');
            w += 1;
        }
        return out;
    }
    // 需要截断：为省略号预留 1 列，其余按字符宽度填充
    let keep = width.saturating_sub(1);
    let mut out = String::new();
    let mut w = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            out.push(c);
            if chars.peek() == Some(&'[') {
                out.push(chars.next().unwrap());
                for c2 in chars.by_ref() {
                    out.push(c2);
                    if ('@'..='~').contains(&c2) {
                        break;
                    }
                }
            }
            continue;
        }
        let cw = char_width(c);
        if w + cw > keep {
            break;
        }
        w += cw;
        out.push(c);
    }
    out.push('…');
    let mut w = w + 1;
    while w < width {
        out.push(' ');
        w += 1;
    }
    out
}

/// 按显示宽度右侧补齐（不截断）
fn pad_disp(s: &str, width: usize) -> String {
    let w = disp_width(s);
    if w >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - w))
    }
}

/// `/help` 描述列的对齐宽度上限；组内命令列超过此值即退化为紧凑排版，避免描述被挤没。
const HELP_ALIGN_CAP: usize = 24;

/// `/help` 条目折行后描述部分的缩进列数。
const HELP_DESC_INDENT: usize = 4;

/// 帮助渲染出的一行：分组标题、或「命令部分 + 可选描述部分」（分别上色）。
#[derive(Debug)]
enum HelpRow {
    Title(String),
    Entry { head: String, tail: Option<String> },
}

/// 词是否以待取值的**旗标**开头：`--flag` 或 `[--flag`（可选项同样要取值）。
/// 折行时不让这类词落在行尾，否则 `--key <key>` 会被劈成两行、读起来像两件事。
///
/// 注意 `<占位>` 不算——它是旗标的**值**，跟着旗标一起走才对。
fn dangling_value(word: &str) -> bool {
    word.starts_with('-') || word.starts_with('[')
}

/// 单个超宽「词」（无空格的长中文串 / 长标志链）按显示列硬折，优先在标点处断开。
fn hard_wrap(word: &str, avail: usize) -> Vec<String> {
    // 允许断行的标点：断点保留在上一行末尾
    const BREAKS: &[char] = &['·', '、', '，', '。', '（', '）', '/', '|', '[', ']', ':', '：', '→'];
    let mut out: Vec<String> = Vec::new();
    let mut cur: Vec<char> = Vec::new();
    let mut cur_w = 0usize;
    let mut brk: Option<usize> = None;
    for ch in word.chars() {
        let cw = char_width(ch);
        if cur_w + cw > avail && !cur.is_empty() {
            // 断点下标可等于 cur.len()（断点字符正好是本行最后一个字符）
            let cut = brk.filter(|&b| b > 0).unwrap_or(cur.len());
            let head: String = cur[..cut].iter().collect();
            let tail: Vec<char> = cur[cut..].iter().copied().skip_while(|c| c.is_whitespace()).collect();
            out.push(head.trim_end().to_string());
            cur_w = tail.iter().map(|c| char_width(*c)).sum();
            cur = tail;
            brk = None;
        }
        cur.push(ch);
        cur_w += cw;
        if BREAKS.contains(&ch) {
            brk = Some(cur.len());
        }
    }
    out.push(cur.iter().collect::<String>().trim_end().to_string());
    out
}

/// 按**显示宽度**贪心折行（全角按 2 列计）。
///
/// 两段式：先按空格切词（不劈开英文单词），再对本身就超宽的长词按显示列硬折。
/// 断行后若行尾恰好是 `--flag` / `<占位>`，则把它后移到下一行，避免「参数与值分离」。
///
/// 必须按列宽而非字符数折行：终端按列宽换行，`chars().count()` 会把中文算成 1 列，
/// 按字符数折行会让中文行实际超宽、被终端二次硬折，缩进全乱。
fn wrap_disp(s: &str, avail: usize) -> Vec<String> {
    if avail == 0 {
        return vec![s.to_string()];
    }
    let line_w = |line: &[&str]| -> usize {
        line.iter().map(|w| disp_width(w)).sum::<usize>() + line.len().saturating_sub(1)
    };
    let mut out: Vec<String> = Vec::new();
    let mut line: Vec<&str> = Vec::new();

    for word in s.split(' ').filter(|w| !w.is_empty()) {
        if disp_width(word) > avail {
            // 单词本身超宽：结算当前行后按显示列硬折
            if !line.is_empty() {
                out.push(line.join(" "));
                line.clear();
            }
            out.extend(hard_wrap(word, avail));
            continue;
        }
        while !line.is_empty() && line_w(&line) + 1 + disp_width(word) > avail {
            // 行尾是待取值参数就先把它挪到下一行；已只剩它自己则只能直接断行
            let carry = if line.len() > 1 && dangling_value(line[line.len() - 1]) {
                line.pop()
            } else {
                None
            };
            out.push(line.join(" "));
            line.clear();
            if let Some(c) = carry {
                line.push(c);
            }
        }
        line.push(word);
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line.join(" "));
    }
    out
}

/// 把帮助表渲染成行序列（不含颜色），**保证每行不超过 `width` 显示列**。
///
/// 单行放得下就「命令 + 对齐描述」；放不下则命令独占一行、描述缩进折行。
/// 抽成纯函数是为了让单测能对真实命令表逐行校验列宽。
fn render_help(groups: &[(&str, &[(&str, &str)])], width: usize) -> Vec<HelpRow> {
    // 全局统一命令列：取全表最长（截到上限），这样跨组翻看时描述列不会忽左忽右。
    // 超过该列的命令一律走「命令独占一行」的两行式。
    let col = groups
        .iter()
        .flat_map(|(_, items)| items.iter())
        .map(|(c, _)| disp_width(c))
        .max()
        .unwrap_or(0)
        .min(HELP_ALIGN_CAP);
    let mut rows: Vec<HelpRow> = Vec::new();
    for (title, items) in groups {
        rows.push(HelpRow::Title((*title).to_string()));
        for &(cmd, desc) in items.iter() {
            let cw = disp_width(cmd);
            let dw = disp_width(desc);
            // 单行式：命令列得下、描述也能在剩余宽度内完整放下
            if cw <= col && 2 + col + 2 + dw <= width {
                rows.push(HelpRow::Entry {
                    head: format!("  {}  ", pad_disp(cmd, col)),
                    tail: Some(desc.to_string()),
                });
                continue;
            }
            // 两行式：命令独占一行（本身超宽则先折行），描述缩进折行
            for l in wrap_disp(cmd, width.saturating_sub(2)) {
                rows.push(HelpRow::Entry {
                    head: format!("  {l}"),
                    tail: None,
                });
            }
            for l in wrap_disp(desc, width.saturating_sub(HELP_DESC_INDENT)) {
                rows.push(HelpRow::Entry {
                    head: " ".repeat(HELP_DESC_INDENT),
                    tail: Some(l),
                });
            }
        }
    }
    rows
}

/// 面板 `/help` 的命令表：既是渲染源，也供单测逐行校验列宽。
const HELP_GROUPS: &[(&str, &[(&str, &str)])] = &[
    (
        "// 会话",
        &[
            ("/new [标题]", "新建会话（原会话保留，可用 /sessions 切回）"),
            ("/resume", "恢复当前目标下最近的会话"),
            ("/sessions", "列出当前交互目标的会话"),
            ("/session <序号|id>", "切换会话"),
            ("/del <序号|id>", "删除会话"),
            ("/tasks", "当前会话任务图（节点/依赖/回流）"),
            ("/stop", "终止当前运行中的轮次（等同 Ctrl+C）"),
            ("/undo <轮次>", "撤销到指定轮次（联动回滚该轮之后改动的工作区文件）"),
            ("/fork <轮次>", "派生分支会话（原会话不变）"),
            ("/clear", "清屏并重绘面板"),
            ("/status", "重绘运行状态面板"),
            ("/quiet [on|off]", "静默模式：只输出最终结果（落盘记忆）"),
            ("/theme <名>", "强调色：cyan|green|amber|violet|ice（落盘记忆）"),
        ],
    ),
    (
        "// 交互目标与编成",
        &[
            ("/target", "当前交互目标 + 全部可选目标（组 / 智能体）"),
            ("/target group <id>", "切到某组（集群协作，经调度）"),
            ("/target single <id>", "切到某智能体（单体服务，不经调度）"),
            ("/singles", "智能体清单（可直接作为交互目标）"),
            ("/groups", "智能体组清单"),
            ("/group <id>", "切换激活组（等价 /target group <id>）"),
            ("/group create <名称> [--id id] [--desc 描述]", "新建组"),
            ("/group info [id]", "组详情（主智能体 / 成员）"),
            ("/group delete <id>", "删除自定义组"),
            ("/agents [关键词]", "组内个体编成清单（可按能力过滤）"),
            ("/agent create <名称> --id <identifier> --desc <职责> [--tier orchestrator|unit]", "新建个体"),
            ("/agent remove <identifier>", "删除个体"),
            ("/agent primary <identifier>", "设为组内主智能体"),
            ("/agent optimize|adaptation|reset-adaptation <identifier>", "经验优化：教训 → 行为改进要点"),
            ("/persona get|set|reset <identifier> [文本]", "人设（说话风格）"),
        ],
    ),
    (
        "// 模型与配置",
        &[
            ("/model", "模型档案清单（生效档案标注 ●）"),
            ("/model use <id>", "切换生效档案（热生效）"),
            ("/model test [id]", "连通测试（真实发一条极短请求）"),
            ("/model add <id> --name <名称> --base-url <url> --key <key> --model <模型>", "新增/更新档案"),
            ("/model remove <id>", "删除档案"),
            ("/config", "当前配置摘要"),
            ("/config get <键>", "读单键（密码打码）"),
            ("/config set <键> <值>", "写单键（网关运行中需重启生效）"),
            ("/config schema", "全部配置键 Schema"),
        ],
    ),
    (
        "// 记忆与平台",
        &[
            ("/memory list [--kind k] [--agent a]", "记忆清单"),
            ("/memory search <查询>", "混合检索（显示得分与理由）"),
            ("/memory add --kind k <标题> <内容> [--pin] [--agent a] [--global]", "固化一条记忆"),
            ("/memory stats|pin|forget|decay|reindex|render ...", "统计 / 固定 / 遗忘 / 衰减 / 重建 / 渲染"),
            ("/skills", "技能包清单"),
            ("/cron list|add|enable|disable|remove|run|runs ...", "定时任务"),
            ("/approvals [--status pending]", "执行审批单"),
            ("/approve <id> | /deny <id>", "批准（代执行并回灌输出）/ 拒绝"),
            ("/checkpoints", "文件检查点清单（写前快照）"),
            ("/restore <路径> [--date YYYY-MM-DD]", "回滚工作区文件到检查点"),
            ("/doctor", "体检：配置 / 编成 / 记忆 / 模型通道 / WebUI"),
        ],
    ),
    (
        "// 面板",
        &[
            ("/version", "版本信息"),
            ("/workspace", "工作区路径"),
            ("/help", "本帮助"),
            ("/exit | /quit", "退出面板（会话保留）"),
        ],
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 分词_支持引号与普通词() {
        assert_eq!(tokenize("create \"研发 组\" --id dev"), vec!["create", "研发 组", "--id", "dev"]);
        assert!(tokenize("   ").is_empty());
    }

    #[test]
    fn 参数解析_键值_布尔_位置() {
        let t = tokenize("add --kind fact \"标题\" \"内容\" --pin --agent scout --importance 0.8");
        let a = parse_args(&t);
        assert_eq!(a.pos, vec!["add", "标题", "内容"]);
        assert_eq!(a.kv.get("kind").map(String::as_str), Some("fact"));
        assert_eq!(a.kv.get("agent").map(String::as_str), Some("scout"));
        assert_eq!(a.kv.get("importance").map(String::as_str), Some("0.8"));
        assert!(a.bools.contains("pin"));
    }

    #[test]
    fn 参数解析_等号写法() {
        let a = parse_args(&tokenize("--base-url=https://x/v1 --model m"));
        assert_eq!(a.kv.get("base-url").map(String::as_str), Some("https://x/v1"));
        assert_eq!(a.kv.get("model").map(String::as_str), Some("m"));
    }

    #[test]
    fn 宽度_跳过_ansi与全角() {
        let colored = "\x1b[36m你好\x1b[0m";
        assert_eq!(disp_width(colored), 4);
        assert_eq!(fit(colored, 6), format!("{colored}  "));
    }

    #[test]
    fn 宽度_超长截断补省略号() {
        let s = fit("abcdefgh", 4);
        assert_eq!(disp_width(&s), 4);
        assert!(s.starts_with("abc"));
        assert!(s.contains('…'));
    }

    #[test]
    fn 主题解析_中英别名() {
        assert!(matches!(Theme::parse("cyan"), Some(Theme::Cyan)));
        assert!(matches!(Theme::parse("紫芒"), Some(Theme::Violet)));
        assert!(matches!(Theme::parse("ICE"), Some(Theme::Ice)));
        assert!(Theme::parse("rainbow").is_none());
    }

    #[test]
    fn 横幅_各行列宽严格一致() {
        let rows = banner_rows();
        assert_eq!(rows.len(), 5);
        let w = disp_width(&rows[0]);
        assert_eq!(w, 53, "EXMACHINA = 9 字母 × 5 列 + 8 个间隔");
        for r in &rows {
            assert_eq!(disp_width(r), w, "行宽不一致：{r}");
        }
    }

    #[test]
    fn 画框_含中文与空串时仍等宽() {
        let p = Panel::new(PathBuf::from("."), PathBuf::from("."), false);
        let w = p.width;
        assert_eq!(disp_width(&p.rule_top("运行状态")), w);
        assert_eq!(disp_width(&p.rule_mid("中文内容与 ascii 混排")), w);
        assert_eq!(disp_width(&p.rule_mid("")), w);
        assert_eq!(disp_width(&p.rule_bottom()), w);
    }

    #[test]
    fn 编辑距离_基本口径() {
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    #[test]
    fn 状态栏_按宽度自适应装填() {
        let sep = " │ ";
        let sep_w = disp_width(sep);
        let chips: Vec<String> = vec![
            "TARGET 组 智能连结 · 40体".to_string(),
            "MODEL deepseek-chat".to_string(),
            "SESS e4e9c9e0 · 0轮".to_string(),
            "TOK ≈1900".to_string(),
        ];
        // 窄终端：装不下的靠后芯片被丢弃，且绝不超宽（不换行）
        let narrow = fit_chips(chips.clone(), 48, sep_w);
        assert!(narrow.len() < chips.len(), "窄终端应丢弃部分芯片");
        assert!(disp_width(&narrow.join(sep)) <= 48);
        // 宽终端：全部保留
        let wide = fit_chips(chips.clone(), 100, sep_w);
        assert_eq!(wide.len(), 4);
        assert!(disp_width(&wide.join(sep)) <= 100);
        // 单枚即超宽（组名极长）→ 截断保留而非整体消失
        let one = fit_chips(vec![format!("TARGET {}", "超".repeat(60))], 40, sep_w);
        assert_eq!(one.len(), 1);
        assert!(disp_width(&one[0]) <= 40);
    }

    #[test]
    fn 命令建议_前缀优先再编辑距离() {
        assert_eq!(suggest_command("sess"), Some("session"));
        assert_eq!(suggest_command("sessons"), Some("sessions"));
        assert_eq!(suggest_command("targt"), Some("target"));
        assert_eq!(suggest_command("zzzzzz"), None);
        assert_eq!(suggest_command(""), None);
    }

    #[test]
    fn 偏好_落盘读回与缺省() {
        let dir = std::env::temp_dir().join(format!("exm-prefs-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // 文件不存在 → 缺省
        let empty = load_prefs(&dir);
        assert_eq!(empty.theme, "");
        assert!(!empty.quiet);
        // 落盘 → 读回
        save_prefs(&dir, &PanelPrefs { theme: "violet".into(), quiet: true });
        let back = load_prefs(&dir);
        assert_eq!(back.theme, "violet");
        assert!(back.quiet);
        assert!(matches!(Theme::parse(&back.theme), Some(Theme::Violet)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 会话短id_截断到八位() {
        assert_eq!(short_id("0123456789abcdef"), "01234567");
        assert_eq!(short_id("abc"), "abc");
    }

    #[test]
    fn 路径_去verbatim前缀并按显示宽截断() {
        assert_eq!(
            shorten_path(Path::new(r"\\?\D:\Projects\ExMachina"), 40),
            r"D:\Projects\ExMachina"
        );
        assert_eq!(
            shorten_path(Path::new(r"\\?\UNC\srv\share\x"), 40),
            r"\\srv\share\x"
        );
        let long = Path::new(r"D:\a\b\c\d\e\f\g\h\i\j\k\l\m\n\o\p\q\r\s\t\u\v");
        let out = shorten_path(long, 20);
        assert!(disp_width(&out) <= 20, "截断后不应超宽：{out}");
        assert!(out.starts_with('…'));
        // 中文路径按显示列算宽
        let cjk = Path::new(r"D:\项目\智能连结\工作区");
        assert!(disp_width(&shorten_path(cjk, 12)) <= 12);
    }

    fn help_row_width(row: &HelpRow) -> usize {
        match row {
            HelpRow::Title(t) => disp_width(t),
            HelpRow::Entry { head, tail } => {
                disp_width(head) + tail.as_deref().map(disp_width).unwrap_or(0)
            }
        }
    }

    /// 真实命令表在多种终端宽度下都必须**逐行不超宽**（否则终端硬折行、缩进错位）
    #[test]
    fn 帮助渲染_每行不超面板宽度() {
        // 覆盖真实表格 + 极端宽度：窄终端下折行、宽终端下仍不超宽
        for width in [30usize, 40, 60, 72, 100, 200] {
            for row in render_help(HELP_GROUPS, width) {
                let w = help_row_width(&row);
                assert!(w <= width, "宽度 {width} 下超宽（{w}）：{row:?}");
            }
        }
    }

    /// 命令表必须自洽：命令以 `/` 开头、组标题以 `//` 开头、无重复命令
    #[test]
    fn 帮助表_结构自洽() {
        let mut seen = std::collections::HashSet::new();
        for (title, items) in HELP_GROUPS {
            assert!(title.starts_with("//"), "组标题应以 `//` 开头：{title}");
            assert!(!items.is_empty(), "组 {title} 不应为空");
            for (cmd, desc) in items.iter() {
                assert!(cmd.starts_with('/'), "命令应以 `/` 开头：{cmd}");
                assert!(!desc.trim().is_empty(), "命令 {cmd} 缺少说明");
                assert!(seen.insert(*cmd), "命令重复：{cmd}");
            }
        }
    }

    /// 折行必须按显示列算：中文占 2 列，否则中文行会实际超宽
    #[test]
    fn 折行_按显示宽度且不丢字符() {
        // 10 个汉字 = 20 列，按 10 列折行 → 每行 5 字、共 2 行
        let src = "深层记忆检索固定统计";
        let out = wrap_disp(src, 10);
        assert_eq!(out.len(), 2, "{out:?}");
        for l in &out {
            assert!(disp_width(l) <= 10, "超宽：{l}");
        }
        // 折行不丢字符（忽略空白后与原文一致：空格折行处的空格即断点）
        assert_eq!(
            out.join("").replace(' ', ""),
            src.replace(' ', "")
        );
        // 优先在空格处断，不把英文单词劈开
        assert_eq!(
            wrap_disp("read config schema now", 12),
            vec!["read config".to_string(), "schema now".to_string()]
        );
        // 放不下时也至少产出一行，不能空手而归
        assert_eq!(wrap_disp("abc", 0), vec!["abc".to_string()]);
        // 单行内放得下就原样返回
        assert_eq!(wrap_disp("short", 20), vec!["short".to_string()]);
        // 行尾不留待取值参数：`--key <key>` 这类成对被挪到下一行，不劈开
        let cmd = "/model add <id> --name <名称> --base-url <url> --key <key> --model <模型>";
        let out = wrap_disp(cmd, 60);
        assert!(out.iter().all(|l| disp_width(l) <= 60), "超宽：{out:?}");
        for l in &out[..out.len() - 1] {
            let last = l.rsplit(' ').next().unwrap_or_default();
            assert!(!dangling_value(last), "行尾是被劈开的参数：{l}");
        }
        assert_eq!(
            out.join("").replace(' ', ""),
            cmd.replace(' ', "")
        );
    }
}
