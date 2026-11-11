//! panel 域分文件：命令分发 handle_line + 打印辅助 + 显示宽度/换行族 + 帮助渲染 + 字符画。

use super::*;

// ---------------------------------------------------------------- 辅助

/// 字符画字母表：EXMACHINA（每字母 6 列 × 7 行，双线笔画）
///
/// 字形口径对齐 `docs/wordmark.svg`（终端横幅与字标同一套视觉语汇）：
/// 笔画取两格宽，与字标的粗板式笔画同量级；单线细字模在终端里过于轻飘。
const BANNER_LETTERS: [[&str; 7]; 9] = [
    // E
    ["██████", "██    ", "██    ", "██████", "██    ", "██    ", "██████"],
    // X
    ["██  ██", "██  ██", " ████ ", "  ██  ", " ████ ", "██  ██", "██  ██"],
    // M
    ["██████", "██  ██", "██  ██", "██ █ █", "██  ██", "██  ██", "██  ██"],
    // A
    [" ████ ", "██  ██", "██  ██", "██████", "██  ██", "██  ██", "██  ██"],
    // C
    [" ████ ", "██  ██", "██    ", "██    ", "██    ", "██  ██", " ████ "],
    // H
    ["██  ██", "██  ██", "██  ██", "██████", "██  ██", "██  ██", "██  ██"],
    // I
    ["██████", "  ██  ", "  ██  ", "  ██  ", "  ██  ", "  ██  ", "██████"],
    // N
    ["██  ██", "███ ██", "████ █", "██ ███", "██  ██", "██  ██", "██  ██"],
    // A
    [" ████ ", "██  ██", "██  ██", "██████", "██  ██", "██  ██", "██  ██"],
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

/// 由字母表拼出横幅 7 行（保证各行列宽严格一致）
pub(crate) fn banner_rows() -> Vec<String> {
    let mut rows = vec![String::new(); BANNER_LETTERS[0].len()];
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

pub(crate) fn print_agents(core: &Core, filter: Option<&str>) -> anyhow::Result<()> {
    let defs = match filter {
        Some(cap) if !cap.trim().is_empty() => core.registry.find_by_capability(cap),
        _ => core.agents(),
    };
    let mut last_domain = String::new();
    for d in defs {
        if d.domain != last_domain {
            last_domain = d.domain.clone();
            println!("\n{}", last_domain.bold().underline());
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

pub(crate) fn print_checkpoints(core: &Core) {
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
pub(crate) fn resolve_session(core: &Core, token: &str) -> Option<exm_core::types::Session> {
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
pub(crate) struct Args {
    pub(crate) pos: Vec<String>,
    pub(crate) kv: HashMap<String, String>,
    pub(crate) bools: HashSet<String>,
}

/// 已知的布尔开关（`--flag` 后不接值）
const BOOL_FLAGS: &[&str] = &["pin", "off", "global", "quiet", "verbose", "json", "force"];

pub(crate) fn parse_args(tokens: &[String]) -> Args {
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
pub(crate) fn tokenize(s: &str) -> Vec<String> {
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
pub(crate) fn term_width() -> usize {
    let raw = std::env::var("EXM_PANEL_WIDTH")
        .or_else(|_| std::env::var("COLUMNS"))
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(72);
    raw.clamp(48, 100)
}

pub(crate) fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// 贪心装填状态栏芯片：按顺序累加，装不下即停（首枚即超宽则截断保留）
pub(crate) fn fit_chips(chips: Vec<String>, width: usize, sep_w: usize) -> Vec<String> {
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

pub(crate) fn shorten_path(p: &std::path::Path, max: usize) -> String {
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
pub(crate) fn disp_width(s: &str) -> usize {
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
pub(crate) fn fit(s: &str, width: usize) -> String {
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
pub(crate) fn pad_disp(s: &str, width: usize) -> String {
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
pub(crate) enum HelpRow {
    Title(String),
    Entry { head: String, tail: Option<String> },
}

/// 词是否以待取值的**旗标**开头：`--flag` 或 `[--flag`（可选项同样要取值）。
/// 折行时不让这类词落在行尾，否则 `--key <key>` 会被劈成两行、读起来像两件事。
///
/// 注意 `<占位>` 不算——它是旗标的**值**，跟着旗标一起走才对。
pub(crate) fn dangling_value(word: &str) -> bool {
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
pub(crate) fn wrap_disp(s: &str, avail: usize) -> Vec<String> {
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
pub(crate) fn render_help(groups: &[(&str, &[(&str, &str)])], width: usize) -> Vec<HelpRow> {
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
pub(crate) const HELP_GROUPS: &[(&str, &[(&str, &str)])] = &[
    (
        "// 会话",
        &[
            ("/new [标题]", "新建会话（原会话保留，可用 /sessions 切回）"),
            ("/resume", "恢复当前目标下最近的会话"),
            ("/sessions", "列出当前交互目标的会话"),
            ("/session <序号|id>", "切换会话"),
            ("/del <序号|id>", "删除会话"),
            ("/tasks", "当前会话编排图（节点/依赖/回流）"),
            ("/stop", "终止当前运行中的轮次（等同 Ctrl+C）"),
            ("/undo <轮次>", "撤销到指定轮次（联动回滚该轮之后改动的工作区文件）"),
            ("/fork <轮次>", "派生分支会话（原会话不变）"),
            ("/flows", "外部聊天流列表（通道 · 对端 · 预览 · 绑定/未读）"),
            ("/flows open <序号>", "打开流（进入绑定会话并清未读；与 WebUI 同源）"),
            ("/flows sessions <序号>", "流内会话列表（活跃倒序，绑定带 ●）"),
            ("/flows use <流序号> <会话序号>", "切换流的绑定会话（通道侧同步生效）"),
            ("/clear", "清屏并重绘面板"),
            ("/status", "重绘运行状态面板"),
            ("/quiet [on|off]", "静默模式：只输出最终结果（落盘记忆）"),
            ("/theme <名>", "强调色：cyan|green|amber|violet|ice（落盘记忆）"),
            ("/caps", "终端能力探测（色彩档位/宽字符/剪贴板通道）"),
            ("/copy <文本>", "复制到剪贴板（不可用时降级打印）"),
        ],
    ),
    (
        "// 交互目标与编成",
        &[
            ("/target", "当前交互目标 + 全部可选目标（组 / 智能体）"),
            ("/target group <id>", "切到某组（集群协作，经调度）"),
            ("/target single <id>", "切到某智能体（不经调度直接服务）"),
            ("/singles", "智能体清单（可直接作为交互目标）"),
            ("/groups", "智能体组清单"),
            ("/group <id>", "切换激活组（等价 /target group <id>）"),
            ("/group create <名称> [--id id] [--desc 描述]", "新建组"),
            ("/group info [id]", "组详情（主智能体 / 成员）"),
            ("/group delete <id>", "删除自定义组"),
            ("/agents [关键词]", "组内个体编成清单（可按能力过滤）"),
            ("/agent create <名称> --id <identifier> --desc <职责> [--tier orchestrator|unit]", "新建个体"),
            ("/agent remove <identifier>", "删除个体"),
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

