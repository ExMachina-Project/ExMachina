//! EXMACHINA CLI —— `exm` 与 `exmachina` 双命令入口（同一实现）
//!
//! 命令规范见 docs/07 §2：两个名字互为别名，参数、行为、退出码完全一致。

pub mod chat;
pub mod config_cmd;
pub mod doctor;
pub mod headless;
pub mod group_cmd;
pub mod install;
pub mod memory_cmd;
pub mod knowledge_cmd;
pub mod panel;
pub mod tui;
pub mod term_caps;
pub mod persona_cmd;
pub mod platform_cmd;
pub mod plugin_cmd;
pub mod render;

use clap::{Parser, Subcommand};
use exm_core::Core;
use owo_colors::OwoColorize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Parser)]
#[command(
    name = "exm",
    bin_name = "exm | exmachina",
    version,
    about = "EXMACHINA 多智能体系统 CLI（`exm` 与 `exmachina` 为同一命令的两个入口名）",
    long_about = "EXMACHINA CLI\n\n用法场景：\n  · `exm`        日常简洁调用（脚本、Pipeline、快速对话）\n  · `exmachina`  同一实现的完整名称（文档、CI、教学场景更自解释）\n\n两者行为、参数、退出码完全一致；差异仅体现在帮助信息的命令名回显。"
)]
struct Cli {
    /// 工作区根目录（默认当前目录）
    #[arg(long, global = true, default_value = ".")]
    workspace: PathBuf,

    /// 无头单轮调用：执行完即退出（脚本 / CI 入口）；缺文本参数时读 stdin 管道。
    /// 仅裸命令位置生效（`exm -p "任务"`），不下渗子命令（避让 gateway -p 等既有短旗标）
    #[arg(short = 'p', long = "print")]
    print: Option<Option<String>>,

    /// 无头输出格式：text（缺省纯正文）| json（单行结构化）| stream-json（JSONL 过程事件）
    #[arg(long, default_value = "text")]
    output_format: String,

    /// 指定会话 id（无头路径用；与 -c/-r 同时给出时优先级最高）
    #[arg(long)]
    session: Option<String>,

    /// 接续最近活跃会话（session-resume；无历史会话即新建）
    #[arg(short = 'c', long = "continue")]
    continue_last: bool,

    /// 按名称模糊匹配接续会话（session-resume；多命中取最近）
    #[arg(short = 'r', long = "resume")]
    resume: Option<String>,

    /// 子命令可省略：`exm` / `exmachina` 裸命令即进入终端交互面板
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// 全屏 TUI 终端工作台（裸 `exm` 默认形态）：消息流/输入/状态栏 + 审批页
    Tui,
    /// 首启安装向导：环境检查 → 模型接入 → 落盘 → 自检
    Install {
        /// 快速模式：仅确认关键项（模型接入仍会询问）
        #[arg(long)]
        quick: bool,
    },
    /// 体检：配置 / 编成 / 记忆 / 通道 / WebUI
    Doctor,
    /// 与指挥体对话（进程内直挂 core）；省略子命令时裸命令等价于 `exm chat`
    Chat {
        /// 任务输入；缺省进入终端交互面板（TTY：横幅 + 对话流 + 输入框 + `/` 命令）
        text: Option<String>,
        /// 继续既有会话
        #[arg(short, long)]
        session: Option<String>,
        /// 仅输出最终结果
        #[arg(short, long)]
        quiet: bool,
        /// 只渲染面板版式（横幅 / 信息面板 / 输入框）后退出，不进交互
        #[arg(long)]
        preview: bool,
    },
    /// 列出指挥体与全部子个体
    Agents {
        /// 按能力检索
        #[arg(long)]
        capability: Option<String>,
    },
    /// 查看会话最新编排图（任务依赖 DAG）
    Tasks {
        session_id: String,
    },
    /// 导出会话训练轨迹（按轮次切分；JSON 或 JSONL）
    Trajectory {
        session_id: String,
        /// 输出文件（缺省打印到 stdout）
        #[arg(long, default_value = "")]
        out: String,
        /// 每轮一行 JSONL（批处理/微调友好）
        #[arg(long)]
        jsonl: bool,
        /// 保留思维链（默认剥离）
        #[arg(long)]
        include_thinking: bool,
        /// 工具参数/结果摘要截断字符数（默认 2000）
        #[arg(long, default_value_t = 2000)]
        max_tool_chars: usize,
    },
    /// 起网关（HTTP REST + WebSocket + WebUI 托管），供浏览器 / 第三方渠道接入
    #[command(alias = "serve")]
    Gateway {
        #[arg(short, long, default_value_t = 4173)]
        port: u16,
        /// 监听地址，默认 `0.0.0.0`（公网可访问）；本地严格隔离用 `127.0.0.1`
        /// 也可通过 `EXM_HOST` 环境变量设置
        #[arg(short = 'H', long, default_value = "")]
        host: String,
    },
    /// 分布式执行节点：接入网关承接子个体派发（旧设备算力入池）
    Worker {
        /// 网关工作者端点，如 ws://192.168.1.10:4173/worker
        #[arg(short, long)]
        url: String,
        /// 工作者密钥（= 网关 authKey；未配置鉴权可省略）
        #[arg(short, long, default_value = "")]
        token: String,
        /// 本节点标识（缺省 = 主机名）
        #[arg(short, long, default_value = "")]
        id: String,
    },
    /// 智能体组：列出 / 创建 / 切换 / 删除（组是隔离与切换的基本单位）
    Group {
        #[command(subcommand)]
        action: GroupAction,
    },
    /// 组内个体管理：创建 / 删除（默认组定义受保护）
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },
    /// 人设（说话风格）：查看 / 修改 / 重置
    Persona {
        #[command(subcommand)]
        action: PersonaAction,
    },
    /// 配置管理（get/set/list/schema）
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// 记忆系统：查看 / 检索 / 固化 / 维护
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
    /// 共享知识库：实例级通用知识的查看 / 检索 / 沉淀 / 治理（全智能体共享）
    Knowledge {
        #[command(subcommand)]
        action: KnowledgeAction,
    },
    /// 技能包：派发时按触发词自动携带的指令包（数据化技能）
    Skill {
        #[command(subcommand)]
        action: SkillAction,
    },
    /// 定时任务：cron / 一次性任务，由网关调度器自动执行
    Cron {
        #[command(subcommand)]
        action: CronAction,
    },
    /// 执行审批：高危命令的拦截与批准/拒绝
    Approval {
        #[command(subcommand)]
        action: ApprovalAction,
    },
    /// 模型档案：多厂商端点管理与切换（OpenAI 兼容）
    Model {
        #[command(subcommand)]
        action: ModelAction,
    },
    /// 文件检查点：写工具落笔前的自动快照，可回看与回滚
    Checkpoint {
        #[command(subcommand)]
        action: CheckpointAction,
    },
    /// 插件管理：list / install / update / uninstall（目录装载 + 索引安装）
    Plugin {
        #[command(subcommand)]
        action: plugin_cmd::PluginAction,
    },
    /// MCP 服务端：把启用的内置工具按 ACL 暴露给外部 MCP 客户端（默认关闭，配置 mcpServe.enabled 开启）
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
    /// 输出 OpenAPI 契约文档（离线生成：直接读网关路由清单，不依赖网关进程）
    Openapi {
        /// 输出文件（缺省打印到 stdout）
        #[arg(long, default_value = "")]
        out: String,
    },
}

#[derive(Subcommand)]
enum McpAction {
    /// stdio 模式：本进程以 MCP 协议（JSON-RPC 2.0，逐行）与客户端通信
    /// 会话亲和：调用在 arguments.session 给出会话 id；--session 提供缺省绑定
    Serve {
        /// 缺省会话 id（调用未携带 session 时使用；无效会话仍会被拒绝）
        #[arg(long)]
        session: Option<String>,
        /// 客户端身份标注（审计可归因；缺省 stdio）
        #[arg(long, default_value = "stdio")]
        client: String,
    },
}

#[derive(Subcommand)]
enum CheckpointAction {
    /// 列出检查点（日期 / 文件 / 大小）
    List,
    /// 回滚某文件到检查点版本
    Restore {
        /// 工作区相对路径，如 src/main.rs
        path: String,
        /// 检查点日期（YYYY-MM-DD）；缺省 = 最近一份
        #[arg(long)]
        date: Option<String>,
    },
}

#[derive(Subcommand)]
enum ModelAction {
    /// 列出模型档案与当前生效者
    List,
    /// 新增/更新档案
    Add {
        id: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        base_url: String,
        #[arg(long, default_value = "")]
        api_key: String,
        #[arg(long)]
        model: String,
    },
    /// 切换生效档案（热生效）
    Use { id: String },
    /// 删除档案
    Remove { id: String },
    /// 连通测试（缺省测当前生效档案）
    Test { id: Option<String> },
}

#[derive(Subcommand)]
enum SkillAction {
    /// 列出激活组技能包
    List,
    /// 创建技能包（任务目标命中触发词即随派发携带）
    Add {
        id: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        instructions: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long, value_delimiter = ',')]
        trigger: Vec<String>,
        /// 限定适用的个体 identifier；缺省全体适用
        #[arg(long, value_delimiter = ',')]
        agent: Vec<String>,
    },
    /// 删除技能包
    Remove { id: String },
    /// 从 SKILL.md 文件导入技能（同 id 覆盖）
    Import {
        #[arg(long)]
        file: String,
    },
    /// 导出技能为 SKILL.md（缺省打印到 stdout）
    Export {
        id: String,
        /// 输出文件（缺省 stdout）
        #[arg(long, default_value = "")]
        out: String,
    },
}

#[derive(Subcommand)]
enum CronAction {
    /// 列出定时任务
    List,
    /// 创建定时任务（--kind cron|at|queue；缺省按 --cron/--at 推断）
    Add {
        name: String,
        #[arg(long)]
        prompt: String,
        #[arg(long)]
        cron: Option<String>,
        #[arg(long)]
        at: Option<String>,
        /// 任务类型：cron（周期）/ at（一次性）/ queue（连续，按步骤 id 顺序执行）
        #[arg(long)]
        kind: Option<String>,
        /// 一次性任务的投递会话 id（留空 = 按 session_title 新建或复用）
        #[arg(long)]
        target_session: Option<String>,
        /// 连续任务的步骤任务 id（逗号分隔；--kind queue 时必填）
        #[arg(long, value_delimiter = ',')]
        steps: Vec<String>,
        /// 执行组；缺省为运行时激活组
        #[arg(long)]
        group: Option<String>,
        /// 会话标题（复用同名会话）
        #[arg(long)]
        session_title: Option<String>,
    },
    /// 手动执行一次
    Run { id: String },
    /// 启用 / 停用
    Enable { id: String },
    /// 启用 / 停用
    Disable { id: String },
    /// 删除
    Remove { id: String },
    /// 运行记录（可按任务过滤）
    Runs {
        #[arg(long)]
        job: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

#[derive(Subcommand)]
enum ApprovalAction {
    /// 列出审批单（--status pending 筛选）
    List {
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// 批准并代执行
    Approve { id: String },
    /// 拒绝
    Deny { id: String },
}

#[derive(Subcommand)]
enum GroupAction {
    /// 列出全部智能体组（标注激活组）
    List,
    /// 创建新组（可只建组；首个个体创建时自动成为主智能体）
    Create {
        name: String,
        #[arg(long)]
        id: Option<String>,
        #[arg(long, default_value = "")]
        description: String,
    },
    /// 切换激活组
    Switch { id: String },
    /// 组详情
    Info { #[arg(long)] id: Option<String> },
    /// 删除自定义组
    Delete { id: String },
    /// 导出组为可分享的 bundle（组元信息 + 个体定义 + 提示词）
    Export {
        id: String,
        #[arg(long, default_value = "")]
        out: String,
    },
    /// 从 bundle 导入为新组
    Import {
        #[arg(long)]
        file: String,
        #[arg(long)]
        id: Option<String>,
    },
}

#[derive(Subcommand)]
enum AgentAction {
    /// 在组内创建个体（首个个体自动成为主智能体；默认写入激活组，可用 --group 指定）
    Create {
        name: String,
        #[arg(long)]
        identifier: String,
        #[arg(long)]
        description: String,
        #[arg(long, default_value = "自定义")]
        domain: String,
        /// orchestrator（主智能体）或 unit
        #[arg(long, default_value = "unit")]
        tier: String,
        #[arg(long)]
        group: Option<String>,
        #[arg(long)]
        prompt: Option<String>,
    },
    /// 删除组内个体（默认激活组，可用 --group 指定）
    Remove {
        identifier: String,
        #[arg(long)]
        group: Option<String>,
    },
    /// 基于历史教训优化个体：合成经验改进要点（只调优，不创建）
    Optimize { identifier: String },
    /// 查看个体经验改进要点
    Adaptation { identifier: String },
    /// 重置个体的经验改进要点
    ResetAdaptation { identifier: String },
}

#[derive(Subcommand)]
enum PersonaAction {
    /// 查看个体人设（标注是否自定义）
    Get { identifier: String },
    /// 修改个体人设（--text 直接给内容，或 --file 从文件读取）
    Set {
        identifier: String,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// 重置为默认智械体风格
    Reset { identifier: String },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// 列出当前配置
    List,
    /// 读取单个配置键
    Get { key: String },
    /// 写入单个配置键
    Set { key: String, value: String },
    /// 输出配置 Schema（设置页/向导驱动源）
    Schema,
}

#[derive(Subcommand)]
enum MemoryAction {
    /// 列出记忆条目（--agent 查看该个体可见的：私有 + 群体）
    List {
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// 检索记忆（综合得分排序；--agent 限定个体可见范围）
    Search {
        query: String,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long, default_value_t = 5)]
        limit: usize,
    },
    /// 写入一条记忆（--agent 归属个体则为私有记忆；--global 为跨组全局记忆；缺省归属激活组）
    Add {
        #[arg(long, default_value = "fact")]
        kind: String,
        title: String,
        body: String,
        #[arg(long)]
        pin: bool,
        #[arg(long, default_value_t = 0.6)]
        importance: f64,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
        #[arg(long)]
        global: bool,
    },
    /// 固定/取消固定（固定项进入基础记忆 memory.md）
    Pin {
        id: String,
        #[arg(long)]
        off: bool,
    },
    /// 编辑条目（--title/--body/--tags/--kind 缺省字段不改动）
    Edit {
        id: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        body: Option<String>,
        #[arg(long, value_delimiter = ',')]
        tags: Option<Vec<String>>,
        #[arg(long)]
        kind: Option<String>,
    },
    /// 遗忘指定记忆
    Forget { id: String },
    /// 重建倒排索引
    Reindex,
    /// 记忆衰减整理
    Decay {
        #[arg(long, default_value_t = 0.05)]
        floor: f64,
    },
    /// 重渲染基础记忆 memory.md
    Render,
    /// 记忆库统计与个体可靠性
    Stats,
    /// 全量导出为 JSONL（备份/迁移；--agent 过滤；缺省打印 stdout，传 file 落盘）
    Export {
        file: Option<String>,
        #[arg(long)]
        agent: Option<String>,
    },
}

#[derive(Subcommand)]
enum KnowledgeAction {
    /// 列出最近知识条目（createdAt 倒序）
    List {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// 检索知识库（词项命中 + 热度合排）
    Search {
        query: String,
        #[arg(long, default_value_t = 5)]
        limit: usize,
    },
    /// 写入一条知识（内容哈希去重；重复写入提升既有条目）
    Add {
        title: String,
        body: String,
        #[arg(long, value_delimiter = ',')]
        tags: Vec<String>,
    },
    /// 删除指定知识条目
    Delete { id: String },
}

/// 规范化工作区根目录：`canonicalize` 后去掉 Windows verbatim 前缀。
///
/// Windows 上 `canonicalize` 会返回 `\\?\D:\...`（UNC 则为 `\\?\UNC\srv\share`），
/// 这个前缀一旦进了 config.json 或 `doctor` / 面板的输出，跨端（桌面端、WebUI）比对
/// 就会处处对不上，可读性也差。故在 CLI 入口统一剥掉。
fn normalize_root(ws: &Path) -> PathBuf {
    let root = ws.canonicalize().unwrap_or_else(|_| ws.to_path_buf());
    let s = root.to_string_lossy().to_string();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        root
    }
}

fn build_core(workspace: &Path) -> anyhow::Result<Arc<Core>> {
    let core = Arc::new(Core::create(normalize_root(workspace))?);
    core.attach_session_send_executor();
    Ok(core)
}

/// 无头分派：参数校验 → 文本获取（参数/stdin/TTY 防护）→ 会话定位 → run_headless。
/// 返回值直接作为进程退出码（0/1/2）；使用错误信息走 stderr（text 模式契约）。
async fn run_headless_dispatch(cli: &Cli, workspace: &Path) -> i32 {
    let format = match headless::OutputFormat::parse(&cli.output_format) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    // 文本来源：显式参数 > stdin 管道；TTY 无参数 = 误用（不能挂起等输入）
    let text = match cli.print.clone().flatten() {
        Some(t) => t.trim().to_string(),
        None => {
            use std::io::IsTerminal;
            if std::io::stdin().is_terminal() {
                eprintln!("错误：-p 需要文本参数或管道输入（例：exm -p \"任务\" 或 echo 任务 | exm -p）");
                return 2;
            }
            let mut buf = String::new();
            match std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf) {
                Ok(_) => buf.trim().to_string(),
                Err(e) => {
                    eprintln!("错误：读取 stdin 失败：{e}");
                    return 2;
                }
            }
        }
    };
    if text.is_empty() {
        eprintln!("错误：空输入——-p 需要非空文本（参数或 stdin）");
        return 2;
    }

    let core = match build_core(workspace) {
        Ok(c) => c,
        Err(e) => {
            // 工作区/配置不可用归使用侧错误（未 init 的目录跑 -p 是调用方问题）
            eprintln!("错误：{e}");
            return 2;
        }
    };

    // 会话定位：--session 显式 > -r 匹配 > -c 最近 > 新建（spec 优先序）
    let session = match resolve_resume_session(&core, cli.session.as_deref(), cli.resume.as_deref(), cli.continue_last) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("错误：{e}");
            return 2;
        }
    };
    let opts = headless::HeadlessOptions { text, session, format };
    let outcome = headless::run_headless(&core, opts).await;
    if let Some(err) = outcome.error {
        // text 模式的执行失败信息走 stderr；json/stream-json 已并入终态 stdout 行
        eprintln!("错误：{err}");
    }
    outcome.code
}

/// 会话定位（无头 / 交互续接共用）：--session > -r 模糊 > -c 最近 > None（新建）。
/// -r 匹配：标题/ID 前缀子串、大小写不敏感、排除归档（list_sessions 已过滤）；
/// 唯一命中返回；多命中报错并列候选（≤10 条：标题 + ID 前 8 位 + 相对时间）；零命中报错。
/// 定位失败返回 Err（消息），调用方按使用错误（退出码 2）处理。
fn resolve_resume_session(
    core: &Arc<Core>,
    explicit: Option<&str>,
    resume_match: Option<&str>,
    continue_last: bool,
) -> anyhow::Result<Option<String>> {
    if let Some(id) = explicit {
        return Ok(Some(id.to_string()));
    }
    // 匹配范围：当前交互目标（组）内未归档会话，按活跃时间倒序（与 /resume 同口径）。
    // 子会话（parent_session 有值 = 派发运行台账）不是用户交互对象，排除
    let scoped: Vec<exm_core::types::Session> = core
        .list_sessions_in_scope()?
        .into_iter()
        .filter(|s| s.parent_session.is_none())
        .collect();
    if let Some(pattern) = resume_match {
        let sessions = &scoped;
        let needle = pattern.to_lowercase();
        let hits: Vec<&exm_core::types::Session> = sessions
            .iter()
            .filter(|s| {
                s.title.to_lowercase().contains(&needle)
                    || s.id.to_lowercase().contains(&needle)
            })
            .collect();
        return match hits.len() {
            0 => Err(anyhow::anyhow!("-r 无匹配会话：{pattern}")),
            1 => Ok(Some(hits[0].id.clone())),
            _ => {
                let mut msg = format!("-r 命中 {} 个会话，请改用更精确匹配或 --session <id>：", hits.len());
                for s in hits.iter().take(10) {
                    let short = &s.id[..s.id.len().min(8)];
                    msg.push_str(&format!("\n  {short}  {}（{}）", s.title, relative_time(&s.updated_at)));
                }
                Err(anyhow::anyhow!(msg))
            }
        };
    }
    if continue_last {
        if let Some(first) = scoped.first() {
            return Ok(Some(first.id.clone()));
        }
        // 无历史会话：与不带 -c 一致（新建），由调用方提示
    }
    Ok(None)
}

/// ISO 时间 → 相对时间描述（候选清单用；解析失败原样返回）
fn relative_time(iso: &str) -> String {
    let Ok(ts) = chrono_time_iso(iso) else {
        return iso.to_string();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let diff = now - ts;
    if diff < 0 {
        return "刚刚".to_string();
    }
    let mins = diff / 60;
    match mins {
        0 => "刚刚".to_string(),
        1..=59 => format!("{mins} 分钟前"),
        60..=1439 => format!("{} 小时前", mins / 60),
        1440..=43199 => format!("{} 天前", mins / 1440),
        _ => format!("{} 个月前", mins / 43200),
    }
}

/// ISO 8601（含毫秒/Z 形态）→ Unix 秒（无 chrono 依赖的手工解析）
fn chrono_time_iso(iso: &str) -> anyhow::Result<i64> {
    // 形如 2026-11-12T02:47:57.590Z / 2026-11-12T02:47:57Z
    let bytes = iso.as_bytes();
    if bytes.len() < 19 {
        anyhow::bail!("过短");
    }
    let s = iso;
    let year: i64 = s.get(0..4).and_then(|v| v.parse().ok()).ok_or_else(|| anyhow::anyhow!("年"))?;
    let month: i64 = s.get(5..7).and_then(|v| v.parse().ok()).ok_or_else(|| anyhow::anyhow!("月"))?;
    let day: i64 = s.get(8..10).and_then(|v| v.parse().ok()).ok_or_else(|| anyhow::anyhow!("日"))?;
    let hour: i64 = s.get(11..13).and_then(|v| v.parse().ok()).ok_or_else(|| anyhow::anyhow!("时"))?;
    let min: i64 = s.get(14..16).and_then(|v| v.parse().ok()).ok_or_else(|| anyhow::anyhow!("分"))?;
    let sec: i64 = s.get(17..19).and_then(|v| v.parse().ok()).ok_or_else(|| anyhow::anyhow!("秒"))?;
    // 简化儒略日算法（days_from_civil）：UTC 视角
    let (y, m) = if month <= 2 { (year - 1, month + 12) } else { (year, month) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m - 3) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Ok(days * 86400 + hour * 3600 + min * 60 + sec)
}

pub fn run() -> anyhow::Result<()> {
    // --version 由 clap 自动生成（CARGO_PKG_VERSION，随发布 bump）——审计补面：
    // 此前 exm-gateway/exm 无版本参数，--version 会直接拉起实例
    let cli = Cli::parse();
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async { dispatch(cli).await })
}

async fn dispatch(cli: Cli) -> anyhow::Result<()> {
    let workspace = cli.workspace.clone();
    // 无头单轮（-p）：顶层前置分派——不进任何交互 UI，执行完按退出码契约退出。
    // 使用错误（2）不能走 anyhow::Err（那是 1 的语义），直接 process::exit 落码
    if cli.print.is_some() {
        let code = run_headless_dispatch(&cli, &workspace).await;
        std::process::exit(code);
    }
    // 裸命令 `exm` / `exmachina`：进全屏 TUI（消息流/输入/状态栏三段版式）。
    // `exm chat` 保留 readline 面板（ BREAKING：裸命令旧版进 readline 面板）
    // 会话参数（-c/-r/--session）在交互形态同样生效：定位到会话作为初始会话载入。
    // 会话定位放在 match 之前（TUI/panel/chat 三分支共用），command 只 move 一次
    let has_session_args =
        cli.session.is_some() || cli.resume.is_some() || cli.continue_last;
    let initial_session: Option<exm_core::types::Session> = if has_session_args {
        let core = build_core(&workspace)?;
        let located = resolve_resume_session(
            &core,
            cli.session.as_deref(),
            cli.resume.as_deref(),
            cli.continue_last,
        )?;
        match located {
            Some(id) => match core.store.get_session(&id)? {
                Some(s) => Some(s),
                None => anyhow::bail!("会话不存在: {id}"),
            },
            // -c 无历史：回落新建（spec：与不带 -c 一致）
            None => None,
        }
    } else {
        None
    };
    let command = cli.command.unwrap_or(Commands::Tui);
    match command {
        Commands::Tui => {
            let core = build_core(&workspace)?;
            // TTY 校验：非终端（管道/重定向）退回行交互，避免控制序列污染
            use std::io::IsTerminal;
            if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
                tui::run_tui_with_session(core, initial_session, false).await
            } else {
                chat::run_interactive(core, false).await
            }
        }
        Commands::Install { quick } => {
            let root = normalize_root(&workspace);
            install::run_install(&root, quick)
        }
        Commands::Doctor => {
            let root = normalize_root(&workspace);
            doctor::run_doctor(&root)
        }
        Commands::Chat { text, session, quiet, preview } => {
            let core = build_core(&workspace)?;
            if preview {
                return panel::run_preview(&core, quiet);
            }
            match text {
                Some(t) => {
                    chat::run_chat(core, &t, &chat::ChatOptions { session, quiet }).await
                }
                None => {
                    // TTY：进入终端交互面板（字符画横幅 + 对话流 + 输入框 + `/` 命令）
                    // 非 TTY（管道 / 重定向）：退回逐行交互，避免控制字符污染日志
                    use std::io::IsTerminal;
                    if std::io::stdin().is_terminal() {
                        panel::run_panel_with_session(core, initial_session, quiet).await
                    } else {
                        chat::run_interactive(core, quiet).await
                    }
                }
            }
        }
        Commands::Agents { capability } => {
            let core = build_core(&workspace)?;
            let defs = match capability {
                Some(cap) => core.registry.find_by_capability(&cap).into_iter().collect::<Vec<_>>(),
                None => core.agents(),
            };
            let mut last_domain = String::new();
            for d in defs {
                let label = d.domain.clone();
                if label != last_domain {
                    last_domain = label.clone();
                    println!("\n{}", label.bold().underline());
                }
                let tier = if matches!(d.tier, exm_core::types::Tier::Orchestrator) {
                    format!(" {} ", "指挥体".on_blue().white())
                } else {
                    "  ".to_string()
                };
                println!(
                    "{} {} {} — {}",
                    tier,
                    d.identifier.cyan(),
                    d.name.bold(),
                    render::dim(d.description.chars().take(40).collect::<String>())
                );
            }
            println!(
                "\n{}",
                render::dim(format!("共 {}（指挥体 1 + 子个体 {}）", core.registry.count(), core.registry.units().len()))
            );
            Ok(())
        }
        Commands::Trajectory { session_id, out, jsonl, include_thinking, max_tool_chars } => {
            let core = build_core(&workspace)?;
            platform_cmd::run_trajectory(
                &core,
                &session_id,
                &out,
                jsonl,
                include_thinking,
                max_tool_chars,
            )
        }
        Commands::Tasks { session_id } => {
            let core = build_core(&workspace)?;
            match core.store.latest_graph(&session_id)? {
                Some(g) => {
                    println!("{}", format!("图 {}｜状态 {:?}", g.id, g.status).bold());
                    for n in &g.nodes {
                        println!("{}", render::node_line(n));
                    }
                }
                None => println!("{}", render::dim("该会话暂无编排图")),
            }
            Ok(())
        }
        Commands::Gateway { port, host } => {
            let core = build_core(&workspace)?;
            let host_ref = if host.is_empty() { None } else { Some(host.as_str()) };
            exm_gateway::serve(core, port, host_ref).await
        }
        Commands::Plugin { action } => plugin_cmd::run_plugin_action(&workspace, action).await,
        Commands::Mcp { action } => {
            let core = build_core(&workspace)?;
            let McpAction::Serve { session, client } = action;
            // ACL 默认关闭：未启用时给出明确指引（不静默空转）
            if !core.config().mcp_serve.enabled {
                anyhow::bail!("MCP 服务端未启用：请在 .exmachina/config.json 的 mcpServe 段置 enabled=true 后重试");
            }
            use std::io::{BufRead, Write};
            let srv = exm_core::mcp_server::McpServer::new(core, client);
            let stdin = std::io::stdin();
            let mut stdout = std::io::stdout();
            eprintln!(
                "[mcp] stdio 服务端已就绪（会话亲和缺省: {}）",
                session.as_deref().unwrap_or("<未绑定>")
            );
            for line in stdin.lock().lines() {
                let Ok(line) = line else { break };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let mut req: serde_json::Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = serde_json::to_writer(
                            &mut stdout,
                            &serde_json::json!({
                                "jsonrpc": "2.0", "id": null,
                                "error": { "code": -32700, "message": format!("解析失败: {e}") }
                            }),
                        );
                        let _ = writeln!(stdout);
                        continue;
                    }
                };
                // 缺省会话绑定：调用未携带 arguments.session 时注入（无效会话仍由服务端拒绝）
                if let Some(default_sid) = session.as_deref() {
                    if req["method"] == "tools/call" {
                        let args = req.pointer_mut("/params/arguments");
                        match args {
                            Some(a) if a.get("session").is_none() => {
                                if let Some(obj) = a.as_object_mut() {
                                    obj.insert("session".into(), serde_json::json!(default_sid));
                                }
                            }
                            None => {
                                if let Some(params) = req.get_mut("params").and_then(|p| p.as_object_mut()) {
                                    params.insert("arguments".into(), serde_json::json!({ "session": default_sid }));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if let Some(resp) = srv.handle_request(&req).await {
                    serde_json::to_writer(&mut stdout, &resp)?;
                    writeln!(stdout)?;
                }
            }
            Ok(())
        }
        Commands::Openapi { out } => {
            // 离线生成：不 build_core（契约与工作区状态无关），直接读网关清单
            let doc = exm_gateway::openapi::generate();
            let pretty = serde_json::to_string_pretty(&doc)?;
            if out.is_empty() {
                println!("{pretty}");
            } else {
                std::fs::write(&out, pretty)?;
                println!("{}", crate::render::ok(format!("OpenAPI 契约已写入 {out}（{} 端点）", doc["paths"].as_object().map(|m| m.len()).unwrap_or(0))));
            }
            Ok(())
        }
        Commands::Worker { url, token, id } => {
            let core = build_core(&workspace)?;
            let worker_id = if id.is_empty() {
                std::env::var("COMPUTERNAME")
                    .or_else(|_| std::env::var("HOSTNAME"))
                    .unwrap_or_else(|_| "worker".into())
            } else {
                id
            };
            eprintln!("[worker] 本地就绪（模型：{}），接入 {url}", if core.is_mock() { "Mock" } else { "已配置" });
            exm_core::remote::WorkerSession::run(&url, &token, &worker_id, core).await
        }
        Commands::Config { action } => {
            let root = normalize_root(&workspace);
            match action {
                ConfigAction::List => config_cmd::run_config_list(&root),
                ConfigAction::Get { key } => config_cmd::run_config_get(&root, &key),
                ConfigAction::Set { key, value } => config_cmd::run_config_set(&root, &key, &value),
                ConfigAction::Schema => config_cmd::run_config_schema(),
            }
        }
        Commands::Memory { action } => {
            let core = build_core(&workspace)?;
            match action {
                MemoryAction::List { kind, agent, limit } => {
                    memory_cmd::run_memory_list(&core, kind, agent.as_deref(), limit)
                }
                MemoryAction::Search { query, agent, limit } => {
                    memory_cmd::run_memory_search(&core, &query, agent.as_deref(), limit)
                }
                MemoryAction::Add { kind, title, body, pin, importance, agent, tags, global } => {
                    memory_cmd::run_memory_add(&core, &kind, &title, &body, pin, agent.as_deref(), tags, importance, global)
                }
                MemoryAction::Pin { id, off } => memory_cmd::run_memory_pin(&core, &id, !off),
                MemoryAction::Edit { id, title, body, tags, kind } => {
                    memory_cmd::run_memory_edit(&core, &id, title, body, tags, kind)
                }
                MemoryAction::Forget { id } => memory_cmd::run_memory_forget(&core, &id),
                MemoryAction::Reindex => memory_cmd::run_memory_reindex(&core),
                MemoryAction::Decay { floor } => memory_cmd::run_memory_decay(&core, floor),
                MemoryAction::Render => memory_cmd::run_memory_render(&core),
                MemoryAction::Stats => memory_cmd::run_memory_stats(&core),
                MemoryAction::Export { file, agent } => {
                    memory_cmd::run_memory_export(&core, file.as_deref(), agent.as_deref())
                }
            }
        }
        Commands::Knowledge { action } => {
            let core = build_core(&workspace)?;
            match action {
                KnowledgeAction::List { limit } => knowledge_cmd::run_knowledge_list(&core, limit),
                KnowledgeAction::Search { query, limit } => {
                    knowledge_cmd::run_knowledge_search(&core, &query, limit)
                }
                KnowledgeAction::Add { title, body, tags } => {
                    knowledge_cmd::run_knowledge_add(&core, &title, &body, tags)
                }
                KnowledgeAction::Delete { id } => knowledge_cmd::run_knowledge_delete(&core, &id),
            }
        }
        Commands::Skill { action } => {
            let core = build_core(&workspace)?;
            match action {
                SkillAction::List => platform_cmd::run_skill_list(&core),
                SkillAction::Add { id, name, instructions, description, trigger, agent } => {
                    platform_cmd::run_skill_add(&core, &id, &name, &instructions, &description, trigger, agent)
                }
                SkillAction::Remove { id } => platform_cmd::run_skill_remove(&core, &id),
                SkillAction::Import { file } => platform_cmd::run_skill_import(&core, &file),
                SkillAction::Export { id, out } => platform_cmd::run_skill_export(&core, &id, &out),
            }
        }
        Commands::Cron { action } => {
            let core = build_core(&workspace)?;
            match action {
                CronAction::List => platform_cmd::run_cron_list(&core),
                CronAction::Add { name, prompt, cron, at, kind, target_session, steps, group, session_title } => {
                    platform_cmd::run_cron_add(
                        &core,
                        &name,
                        &prompt,
                        cron,
                        at,
                        group,
                        session_title,
                        kind,
                        target_session,
                        steps,
                    )
                }
                CronAction::Run { id } => platform_cmd::run_cron_now(&core, &id).await,
                CronAction::Enable { id } => platform_cmd::run_cron_enable(&core, &id, true),
                CronAction::Disable { id } => platform_cmd::run_cron_enable(&core, &id, false),
                CronAction::Remove { id } => platform_cmd::run_cron_remove(&core, &id),
                CronAction::Runs { job, limit } => platform_cmd::run_cron_runs(&core, job.as_deref(), limit),
            }
        }
        Commands::Approval { action } => {
            let core = build_core(&workspace)?;
            match action {
                ApprovalAction::List { status, limit } => {
                    platform_cmd::run_approval_list(&core, status.as_deref(), limit)
                }
                ApprovalAction::Approve { id } => platform_cmd::run_approval_decide(&core, &id, true).await,
                ApprovalAction::Deny { id } => platform_cmd::run_approval_decide(&core, &id, false).await,
            }
        }
        Commands::Model { action } => {
            let core = build_core(&workspace)?;
            match action {
                ModelAction::List => platform_cmd::run_model_list(&core),
                ModelAction::Add { id, name, base_url, api_key, model } => {
                    platform_cmd::run_model_add(&core, &id, &name, &base_url, &api_key, &model)
                }
                ModelAction::Use { id } => platform_cmd::run_model_use(&core, &id),
                ModelAction::Remove { id } => platform_cmd::run_model_remove(&core, &id),
                ModelAction::Test { id } => platform_cmd::run_model_test(&core, id.as_deref()).await,
            }
        }
        Commands::Checkpoint { action } => {
            let core = build_core(&workspace)?;
            match action {
                CheckpointAction::List => {
                    let rows = core.list_checkpoints();
                    if rows.is_empty() {
                        println!("尚无检查点（写/编辑文件前会自动快照到 .exmachina/checkpoints/）");
                    } else {
                        println!("{:<12} {:<50} {:>10}", "日期", "文件", "大小");
                        for (date, rel, size) in rows.iter().take(200) {
                            println!("{date:<12} {rel:<50} {size:>9} B");
                        }
                        println!("（共 {} 份快照）", rows.len());
                    }
                    Ok(())
                }
                CheckpointAction::Restore { path, date } => {
                    let msg = core.restore_checkpoint(date.as_deref(), &path)?;
                    println!("{msg}");
                    Ok(())
                }
            }
        }
        Commands::Group { action } => {
            let core = build_core(&workspace)?;
            match action {
                GroupAction::List => group_cmd::run_list(&core),
                GroupAction::Create { name, id, description } => {
                    group_cmd::run_create(&core, &name, id, &description)
                }
                GroupAction::Switch { id } => group_cmd::run_switch(&core, &id),
                GroupAction::Info { id } => group_cmd::run_info(&core, id),
                GroupAction::Delete { id } => group_cmd::run_delete(&core, &id),
                GroupAction::Export { id, out } => group_cmd::run_export(&core, &id, &out),
                GroupAction::Import { file, id } => group_cmd::run_import(&core, &file, id.as_deref()),
            }
        }
        Commands::Agent { action } => {
            let core = build_core(&workspace)?;
            match action {
                AgentAction::Create { name, identifier, description, domain, tier, group, prompt } => {
                    group_cmd::run_agent_create(
                        &core, &name, &identifier, &description, &domain, &tier, group.as_deref(), prompt,
                    )
                }
                AgentAction::Remove { identifier, group } => {
                    group_cmd::run_agent_remove(&core, &identifier, group.as_deref())
                }
                AgentAction::Optimize { identifier } => {
                    platform_cmd::run_agent_optimize(&core, &identifier).await
                }
                AgentAction::Adaptation { identifier } => {
                    platform_cmd::run_agent_adaptation(&core, &identifier)
                }
                AgentAction::ResetAdaptation { identifier } => {
                    platform_cmd::run_agent_reset_adaptation(&core, &identifier)
                }
            }
        }
        Commands::Persona { action } => {
            let core = build_core(&workspace)?;
            match action {
                PersonaAction::Get { identifier } => persona_cmd::run_get(&core, &identifier),
                PersonaAction::Set { identifier, text, file } => {
                    persona_cmd::run_set(&core, &identifier, text, file)
                }
                PersonaAction::Reset { identifier } => persona_cmd::run_reset(&core, &identifier),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------- 会话定位（session-resume：-c/-r/--session 优先序与匹配三态）

    fn resume_core() -> Arc<Core> {
        let dir = std::env::temp_dir().join(format!("exm-resume-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = exm_core::config::ExmConfig::load(&dir);
        Arc::new(Core::with_config(cfg).expect("创建 Core 失败"))
    }

    /// 优先序：--session 显式 > -r 匹配 > -c 最近
    #[test]
    fn 会话定位_优先序() {
        let core = resume_core();
        let a = core.create_session("调试会话A").unwrap();
        let b = core.create_session("普通会话B").unwrap();
        // 同毫秒创建的会话 updated_at 相同会让「最近」排序不稳定：b 先 touch 定序
        core.store
            .set_session_active_at(&b.id, "2999-01-01T00:00:00.000Z")
            .unwrap();
        // --session 最高（即使 -r/-c 同时给出）
        let got = resolve_resume_session(&core, Some(&b.id), Some("调试"), false).unwrap();
        assert_eq!(got.as_deref(), Some(b.id.as_str()), "显式 --session 必须最优先");
        // -r 次之（-c 被忽略）
        let got = resolve_resume_session(&core, None, Some("调试"), true).unwrap();
        assert_eq!(got.as_deref(), Some(a.id.as_str()), "-r 优先于 -c");
        // -c 兜底：最近活跃的（b 已 touch 为最晚）
        let got = resolve_resume_session(&core, None, None, true).unwrap();
        assert_eq!(got.as_deref(), Some(b.id.as_str()), "-c 取最近活跃");
    }

    /// -r 匹配三态：唯一命中 / 多命中列候选报错 / 零命中报错
    #[test]
    fn 会话定位_匹配三态() {
        let core = resume_core();
        core.create_session("调试前端问题").unwrap();
        core.create_session("调试后端问题").unwrap();
        core.create_session("无关会话").unwrap();
        // 唯一命中
        let got = resolve_resume_session(&core, None, Some("前端"), false).unwrap();
        assert!(got.is_some(), "唯一命中返回会话 id");
        // 多命中：报错并列候选（错误信息含两个标题）
        let err = resolve_resume_session(&core, None, Some("调试"), false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("2 个会话"), "多命中应报数量：{msg}");
        assert!(msg.contains("调试前端问题") && msg.contains("调试后端问题"), "候选清单：{msg}");
        // 零命中
        let err = resolve_resume_session(&core, None, Some("不存在的名字"), false).unwrap_err();
        assert!(err.to_string().contains("无匹配"), "零命中报错");
    }

    /// -r 大小写不敏感（ID 十六进制大小写 / 标题 ASCII 大小写）
    #[test]
    fn 会话定位_大小写不敏感() {
        let core = resume_core();
        let s = core.create_session("Debug Session").unwrap();
        let got = resolve_resume_session(&core, None, Some("debug session"), false).unwrap();
        assert_eq!(got.as_deref(), Some(s.id.as_str()), "小写应命中大写标题");
        // ID 大写也应命中
        let upper = s.id.to_uppercase();
        let got = resolve_resume_session(&core, None, Some(&upper), false).unwrap();
        assert!(got.is_some(), "ID 大写形态应命中");
    }

    /// 无历史会话：-c 回落 None（= 新建），不报错
    #[test]
    fn 会话定位_无历史回落() {
        let core = resume_core();
        let got = resolve_resume_session(&core, None, None, true).unwrap();
        assert!(got.is_none(), "无历史会话 -c 应回落新建（None）");
    }

    /// 相对时间：ISO 解析与分档描述
    #[test]
    fn 相对时间_分档() {
        // 非法/过短输入原样返回
        assert_eq!(relative_time("bad"), "bad");
        // 合法 ISO：2000-01-01 距今多年
        let ts = chrono_time_iso("2000-01-01T00:00:00Z").unwrap();
        assert_eq!(ts, 946684800, "Unix 秒换算");
    }

    /// Windows canonicalize 会带 `\\?\` 前缀，若漏剥会一路渗进 config.json 与 doctor 输出
    #[test]
    fn 工作区根目录_剥离_win_verbatim_前缀() {
        let ws = PathBuf::from(r"\\?\D:\Projects\ExMachina");
        let out = normalize_root(&ws);
        assert!(
            !out.to_string_lossy().contains(r"\\?\"),
            "普通盘符前缀未剥除：{}",
            out.display()
        );
        assert_eq!(out, PathBuf::from(r"D:\Projects\ExMachina"));
    }

    /// UNC 共享盘的 verbatim 前缀剥除后要还原成 `\\srv\share` 形态
    #[test]
    fn 工作区根目录_剥离_unc_verbatim_前缀() {
        let ws = PathBuf::from(r"\\?\UNC\srv\share\ExMachina");
        let out = normalize_root(&ws);
        assert_eq!(out, PathBuf::from(r"\\srv\share\ExMachina"));
        assert!(!out.to_string_lossy().contains("UNC"));
    }

    /// 非 Windows 原生路径原样返回，不能误伤
    #[test]
    fn 工作区根目录_不动普通路径() {
        let ws = PathBuf::from("/home/dev/ExMachina");
        assert_eq!(normalize_root(&ws), ws);
    }
}
