//! EXMACHINA CLI —— `exm` 与 `exmachina` 双命令入口（同一实现）
//!
//! 命令规范见 docs/07 §2：两个名字互为别名，参数、行为、退出码完全一致。

mod backend_cmd;
pub mod chat;
mod command_cmd;
pub mod config_cmd;
pub mod doctor;
pub mod group_cmd;
pub mod headless;
pub mod install;
pub mod knowledge_cmd;
pub mod learn_cmd;
pub mod memory_cmd;
pub mod panel;
pub mod persona_cmd;
pub mod platform_cmd;
pub mod plugin_cmd;
pub mod render;
pub mod term_caps;
pub mod tui;

use clap::{CommandFactory, Parser, Subcommand};
use exm_core::Core;
use owo_colors::OwoColorize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Parser)]
#[command(
    name = "exm",
    bin_name = "exm | exmachina",
    disable_help_subcommand = true,
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
    /// 执行后端的能力清单与只读依赖自检（不启动容器、不回退宿主）
    Backend {
        #[command(subcommand)]
        action: backend_cmd::BackendAction,
    },
    /// 查询规范命令目录生成的帮助；标准 `--help` 仍提供参数级用法
    Help {
        /// 可选规范名、别名或命令前缀（例如 `spec.export`、`model`）
        command: Option<String>,
    },
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
    Tasks { session_id: String },
    /// 查看会话运行、目标、规格、计划和后续队列状态
    Status { session_id: String },
    /// 明确管理后续任务队列；普通 chat 不会隐式入队
    Queue {
        #[command(subcommand)]
        action: QueueAction,
    },
    /// 会话新建、列表与恢复
    Sessions {
        #[command(subcommand)]
        action: SessionAction,
    },
    /// 停止指定会话当前运行轮次
    Stop { session_id: String },
    /// 向当前运行提交非中断引导
    Steer {
        session_id: String,
        instruction: String,
    },
    /// 只读计划模式与审批回放
    Plan {
        #[command(subcommand)]
        action: PlanAction,
    },
    /// 会话内编程规格（默认不写工作区/OpenSpec）
    Spec {
        #[command(subcommand)]
        action: SpecAction,
    },
    /// 会话持久目标生命周期
    Goal {
        #[command(subcommand)]
        action: GoalAction,
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
    /// 学习候选：任务后沉淀经验的清单查看与人工裁决（批准转正 / 拒绝）
    Learn {
        #[command(subcommand)]
        action: LearnAction,
    },
    /// 技能包：派发时按触发词自动携带的指令包（数据化技能）
    #[command(alias = "skills")]
    Skill {
        #[command(subcommand)]
        action: SkillAction,
    },
    /// 会话上下文来源：文件/记忆/检索/技能的清单、详情、刷新与自动注入开关
    Context {
        /// 会话 id
        session_id: String,
        #[command(subcommand)]
        action: ContextAction,
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
    /// 查询或运行当前会话的动态技能/插件命令
    Command {
        #[command(subcommand)]
        action: ExtensionCommandAction,
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
enum ExtensionCommandAction {
    /// 列出动态命令；可加顶层 --session 查看当前会话可执行命令
    List,
    /// 输出由版本化规范目录生成的入口矩阵（text/json/markdown）
    Catalog {
        #[arg(long)]
        markdown: bool,
    },
    /// 运行命名空间命令（plugin.<id>.<tool> 或 skill.<id>）
    Run {
        name: String,
        #[arg(long, default_value_t = exm_core::command::COMMAND_DESCRIPTOR_VERSION)]
        command_version: u16,
        /// 命令参数（JSON 对象）
        #[arg(long, default_value = "{}")]
        args: String,
    },
}

#[derive(Subcommand)]
enum QueueAction {
    /// 查询队列
    List { session_id: String },
    /// 明确加入队列
    Add {
        session_id: String,
        instruction: String,
    },
    /// 编辑排队中的任务文本（仅 queued 状态；带 revision 乐观锁）
    Edit {
        session_id: String,
        /// 队列条目 id（tq- 开头）
        queue_id: String,
        new_instruction: String,
        /// 期望 revision（缺省跳过乐观锁检查）
        #[arg(long)]
        revision: Option<u64>,
    },
    /// 移除排队中的任务（不执行其文本）
    Remove {
        session_id: String,
        queue_id: String,
    },
    /// 移动排队中的任务到指定序位（1 起）
    Move {
        session_id: String,
        queue_id: String,
        position: usize,
    },
    /// 清空指定会话队列
    Clear { session_id: String },
}

#[derive(Subcommand)]
enum SessionAction {
    New { title: Option<String> },
    List,
    Resume { session_id: String },
}

#[derive(Subcommand)]
enum PlanAction {
    Status {
        session_id: String,
    },
    Start {
        session_id: String,
        task: Option<String>,
    },
    Approve {
        session_id: String,
    },
    Reject {
        session_id: String,
    },
    Execute {
        session_id: String,
    },
}

#[derive(Subcommand)]
enum SpecAction {
    Show {
        session_id: String,
    },
    Draft {
        session_id: String,
        objective: String,
    },
    Edit {
        session_id: String,
        objective: String,
        #[arg(long)]
        revision: Option<u64>,
    },
    /// 显式导出到当前项目内的相对路径；不会写入 openspec/。
    Export {
        session_id: String,
        path: String,
    },
    Clear {
        session_id: String,
        #[arg(long)]
        revision: Option<u64>,
    },
}

#[derive(Subcommand)]
enum GoalAction {
    Status {
        session_id: String,
    },
    Start {
        session_id: String,
        objective: String,
    },
    Edit {
        session_id: String,
        objective: String,
        #[arg(long)]
        revision: Option<u64>,
    },
    Pause {
        session_id: String,
        #[arg(long)]
        revision: Option<u64>,
    },
    Resume {
        session_id: String,
        #[arg(long)]
        revision: Option<u64>,
    },
    Complete {
        session_id: String,
        verification: String,
        #[arg(long)]
        revision: Option<u64>,
    },
    Block {
        session_id: String,
        note: String,
        #[arg(long)]
        revision: Option<u64>,
    },
    Clear {
        session_id: String,
        #[arg(long)]
        revision: Option<u64>,
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
    /// 版本历史（capability-parity 6.5：受控写入的变更记录面）
    Versions { id: String },
    /// 回滚到历史版本（以旧内容写新版本行，审计链不断）
    Rollback {
        id: String,
        /// 目标版本号
        #[arg(long)]
        to: u64,
    },
}

/// `exm context <session>` 的子命令（capability-parity 6.3）。
/// 会话 id 是位置参数——与 `exm plan status <session_id>` 同形。
#[derive(Subcommand)]
enum ContextAction {
    /// 列出上下文来源（文件/记忆/检索/技能）及状态、禁用位与缺失原因
    List,
    /// 单个来源的详情
    Detail {
        /// 来源键：file:AGENTS.md / file:.exmachina/context.md / memory / retrieval / skills
        key: String,
    },
    /// 重新读取单个来源（失败只影响该来源；其余来源不变）
    Refresh {
        /// 来源键；缺省重读整表
        key: Option<String>,
    },
    /// 停用来源的自动注入（模型主动召回与用户显式 /skill 不受影响）
    Disable {
        /// 来源键
        key: String,
    },
    /// 恢复来源的自动注入
    Enable {
        /// 来源键
        key: String,
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
enum LearnAction {
    /// 列出学习候选（默认待裁决的；--status 筛选）
    List {
        /// 状态筛选：pending | approved | rejected
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// 批准候选并转正（写入技能/记忆版本链；回执带落点与版本号）
    Approve { id: String },
    /// 拒绝候选（建议带原因）
    Reject { id: String, reason: Option<String> },
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
    Info {
        #[arg(long)]
        id: Option<String>,
    },
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
    /// 遗忘指定记忆（可撤销：exm memory restore <id>；版本历史保留）
    Forget { id: String },
    /// 版本历史（旧 → 新，含遗忘标记）
    Versions { id: String },
    /// 回滚到历史版本（历史内容经同一校验成为新版本行）
    Rollback {
        id: String,
        /// 目标版本号
        #[arg(long)]
        to: u64,
    },
    /// 撤销遗忘（以被遗忘时的内容重建条目）
    Restore { id: String },
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

/// 顶层 Clap 命令与规范目录的兼容映射。Clap 仍负责参数解析和既有领域 handler，
/// 规范目录负责跨入口名称/别名事实源；映射漂移在进入任何副作用前失败。
fn validate_cli_command_catalog() -> anyhow::Result<()> {
    let registry = cli_command_registry()?;
    let mappings = [
        ("help", "help"),
        ("model", "model.list"),
        ("config", "config.show"),
        ("memory", "memory.list"),
        ("skill", "skills.list"),
        ("cron", "cron.list"),
        ("approval", "approval.list"),
        ("checkpoint", "checkpoint.list"),
        ("sessions", "sessions.list"),
        ("stop", "stop"),
        ("status", "status"),
        ("queue", "queue.add"),
        ("steer", "steer"),
        ("plan", "plan.status"),
        ("spec", "spec.show"),
        ("goal", "goal.status"),
        ("context", "context.list"),
    ];
    for (entry_name, canonical) in mappings {
        if Cli::command().find_subcommand(entry_name).is_none()
            || registry.resolve(entry_name).is_none()
            || registry.resolve(canonical).is_none()
            || registry.resolve(&format!("cli.{entry_name}")).is_none()
        {
            anyhow::bail!("CLI 命令目录漂移：顶层 `{entry_name}` 未映射到规范命令 `{canonical}`");
        }
    }
    Ok(())
}

fn core_session(workspace: &Path, session_id: &str) -> anyhow::Result<exm_core::types::Session> {
    let core = build_core(workspace)?;
    core.store
        .get_session(session_id)?
        .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))
}

/// 把 clap 实际声明的顶层子命令注册到统一目录；它们作为 `cli.<name>` 入口记录，
/// 原有 Clap 分派仍是 handler owner。CLI-only 条目显式携带 handler-ready capability
/// 占位，因此不会被面板或其他入口错误地展示为可执行命令。
pub(crate) fn cli_command_registry() -> anyhow::Result<exm_core::command::CommandRegistry> {
    use exm_core::command::{
        CommandDescriptor, CommandEntry, CommandErrorCategory, CommandOutputFormat,
        CommandPermission, CommandRegistry, CommandScope, CommandSideEffect,
        COMMAND_DESCRIPTOR_VERSION, COMMAND_PROTOCOL_VERSION,
    };
    let mut registry = CommandRegistry::with_builtins()
        .map_err(|diagnostic| anyhow::anyhow!("命令目录初始化失败：{}", diagnostic.code))?;
    for command in Cli::command().get_subcommands() {
        let name = command.get_name().to_string();
        let canonical = format!("cli.{name}");
        let summary = command
            .get_about()
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("现有 CLI 子命令 `{name}` 的兼容入口"));
        let descriptor = CommandDescriptor {
            protocol_version: COMMAND_PROTOCOL_VERSION,
            name: canonical,
            aliases: Vec::new(),
            version: COMMAND_DESCRIPTOR_VERSION,
            summary,
            usage: format!("exm {name} …"),
            argument_schema: serde_json::json!({
                "type": "object",
                "description": "由 Clap 子命令定义解析；本目录项不替代原参数 parser",
                "additionalProperties": true
            }),
            scope: CommandScope::Global,
            permissions: vec![CommandPermission::Read],
            side_effect: CommandSideEffect::None,
            entries: vec![CommandEntry::Cli],
            output_formats: vec![CommandOutputFormat::Human, CommandOutputFormat::Json],
            error_categories: vec![
                CommandErrorCategory::Forbidden,
                CommandErrorCategory::Unsupported,
                CommandErrorCategory::InvalidArgs,
                CommandErrorCategory::Unavailable,
            ],
            required_capabilities: vec![format!("cli.handler.{name}")],
            owner: "cli".into(),
            deprecated: false,
            replacement: None,
        };
        registry
            .register(descriptor)
            .map_err(|diagnostic| anyhow::anyhow!("CLI 命令目录注册失败：{}", diagnostic.code))?;
    }
    Ok(registry)
}

pub(crate) fn cli_command_capabilities() -> Vec<exm_core::command::CapabilityItem> {
    Cli::command()
        .get_subcommands()
        .map(|command| exm_core::command::CapabilityItem {
            name: format!("cli.handler.{}", command.get_name()),
            status: exm_core::command::CapabilityStatus::Available,
            reason: None,
        })
        .collect()
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
                eprintln!(
                    "错误：-p 需要文本参数或管道输入（例：exm -p \"任务\" 或 echo 任务 | exm -p）"
                );
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
    let session = match resolve_resume_session(
        &core,
        cli.session.as_deref(),
        cli.resume.as_deref(),
        cli.continue_last,
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("错误：{e}");
            return 2;
        }
    };
    let opts = headless::HeadlessOptions {
        text,
        session,
        format,
    };
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
                s.title.to_lowercase().contains(&needle) || s.id.to_lowercase().contains(&needle)
            })
            .collect();
        return match hits.len() {
            0 => Err(anyhow::anyhow!("-r 无匹配会话：{pattern}")),
            1 => Ok(Some(hits[0].id.clone())),
            _ => {
                let mut msg = format!(
                    "-r 命中 {} 个会话，请改用更精确匹配或 --session <id>：",
                    hits.len()
                );
                for s in hits.iter().take(10) {
                    let short = &s.id[..s.id.len().min(8)];
                    msg.push_str(&format!(
                        "\n  {short}  {}（{}）",
                        s.title,
                        relative_time(&s.updated_at)
                    ));
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
    let year: i64 = s
        .get(0..4)
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("年"))?;
    let month: i64 = s
        .get(5..7)
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("月"))?;
    let day: i64 = s
        .get(8..10)
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("日"))?;
    let hour: i64 = s
        .get(11..13)
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("时"))?;
    let min: i64 = s
        .get(14..16)
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("分"))?;
    let sec: i64 = s
        .get(17..19)
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("秒"))?;
    // 简化儒略日算法（days_from_civil）：UTC 视角
    let (y, m) = if month <= 2 {
        (year - 1, month + 12)
    } else {
        (year, month)
    };
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
    let args: Vec<String> = std::env::args().collect();
    let json_requested = requested_json_output(&args);
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            if json_requested {
                println!("{}", cli_error_result("invalid_args", &error.to_string()));
                std::process::exit(2);
            }
            error.exit();
        }
    };
    let output_format = cli.output_format.clone();
    let rt = tokio::runtime::Runtime::new()?;
    match rt.block_on(async { dispatch(cli).await }) {
        Ok(()) => Ok(()),
        Err(error) if error.is::<command_cmd::ReportedCommandFailure>() => std::process::exit(1),
        Err(error) if output_format == "json" => {
            println!(
                "{}",
                cli_error_result(classify_cli_error(&error.to_string()), &error.to_string())
            );
            std::process::exit(1);
        }
        Err(error) => Err(error),
    }
}

fn requested_json_output(args: &[String]) -> bool {
    args.iter().enumerate().any(|(index, arg)| {
        arg == "--output-format=json"
            || arg == "--output-format" && args.get(index + 1).is_some_and(|value| value == "json")
    })
}

fn cli_error_result(category: &str, message: &str) -> String {
    serde_json::json!({
        "ok": false,
        "status": if category == "invalid_args" { "rejected" } else { "failed" },
        "error": {
            "category": category,
            "message": exm_core::tools::redact_str(message)
        },
        "requestId": exm_core::types::new_id()
    })
    .to_string()
}

fn classify_cli_error(message: &str) -> &'static str {
    let lower = message.to_ascii_lowercase();
    if lower.contains("permission") || message.contains("权限") || message.contains("禁止") {
        "forbidden"
    } else if lower.contains("unsupported") || message.contains("不支持") {
        "unsupported"
    } else if message.contains("不存在") || lower.contains("not found") {
        "not_found"
    } else if message.contains("冲突") || lower.contains("conflict") {
        "conflict"
    } else if message.contains("模型") && (message.contains("不可用") || message.contains("未配置"))
    {
        "unavailable"
    } else {
        "internal"
    }
}

async fn dispatch(cli: Cli) -> anyhow::Result<()> {
    validate_cli_command_catalog()?;
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
    let has_session_args = cli.session.is_some() || cli.resume.is_some() || cli.continue_last;
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
        Commands::Backend { action } => {
            let core = build_core(&workspace)?;
            backend_cmd::run(&core, action, cli.output_format == "json").await
        }
        Commands::Help { command } => {
            let core = build_core(&workspace)?;
            let session = initial_session.as_ref().map(|session| session.id.as_str());
            let directory = command_cmd::command_directory(
                &core,
                exm_core::command::CommandEntry::Cli,
                session,
            )?;
            if cli.output_format == "json" {
                if let Some(name) = command.as_deref() {
                    let value = directory
                        .entries
                        .iter()
                        .find(|entry| {
                            entry.descriptor.name == name
                                || entry.descriptor.aliases.iter().any(|alias| alias == name)
                        })
                        .map(|entry| {
                            serde_json::json!({
                                "protocolVersion": directory.protocol_version,
                                "entry": entry,
                            })
                        })
                        .unwrap_or_else(|| {
                            serde_json::json!({
                                "ok": false,
                                "error": {"category":"unsupported", "message":"命令不在目录中"}
                            })
                        });
                    println!("{}", serde_json::to_string_pretty(&value)?);
                } else {
                    println!("{}", serde_json::to_string_pretty(&directory)?);
                }
            } else if let Some(name) = command.as_deref() {
                let Some(entry) = directory.entries.iter().find(|entry| {
                    entry.descriptor.name == name
                        || entry.descriptor.aliases.iter().any(|alias| alias == name)
                }) else {
                    anyhow::bail!("命令不在目录中：{name}");
                };
                println!(
                    "{}\n  用法：exm {}\n  说明：{}\n  状态：{}",
                    entry.descriptor.name,
                    entry.descriptor.usage,
                    entry.descriptor.summary,
                    if entry.executable {
                        "目录允许"
                    } else {
                        "目录拒绝"
                    }
                );
            } else {
                println!("{}", command_cmd::render_directory_text(&directory));
                for diagnostic in directory.diagnostics {
                    eprintln!("目录提示：{}", diagnostic.message);
                }
            }
            Ok(())
        }
        Commands::Chat {
            text,
            session,
            quiet,
            preview,
        } => {
            let core = build_core(&workspace)?;
            if preview {
                return panel::run_preview(&core, quiet);
            }
            match text {
                Some(t) => chat::run_chat(core, &t, &chat::ChatOptions { session, quiet }).await,
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
                Some(cap) => core
                    .registry
                    .find_by_capability(&cap)
                    .into_iter()
                    .collect::<Vec<_>>(),
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
                render::dim(format!(
                    "共 {}（指挥体 1 + 子个体 {}）",
                    core.registry.count(),
                    core.registry.units().len()
                ))
            );
            Ok(())
        }
        Commands::Trajectory {
            session_id,
            out,
            jsonl,
            include_thinking,
            max_tool_chars,
        } => {
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
        Commands::Status { session_id } => {
            let core = build_core(&workspace)?;
            let session = core
                .store
                .get_session(&session_id)?
                .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))?;
            let relation = core.workflow_relation(&session_id)?;
            let output = serde_json::json!({
                "sessionId": session.id,
                "title": session.title,
                "goal": relation.goal,
                "spec": relation.spec,
                "plan": {
                    "mode": if relation.plan_mode { "plan" } else { "execute" },
                    "items": core.plan_list(&session_id),
                    "count": relation.plan_items,
                },
                "todoPresent": relation.todo_present,
                "queue": exm_core::task_queue::list(&session_id),
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
            Ok(())
        }
        Commands::Queue { action } => {
            match action {
                QueueAction::List { session_id } => {
                    let session = core_session(&workspace, &session_id)?;
                    let output = serde_json::json!({
                        "sessionId": session.id,
                        "items": exm_core::task_queue::list(&session_id),
                    });
                    println!("{}", serde_json::to_string_pretty(&output)?);
                }
                QueueAction::Add {
                    session_id,
                    instruction,
                } => {
                    core_session(&workspace, &session_id)?;
                    let id = exm_core::task_queue::enqueue_scoped(
                        &session_id,
                        &instruction,
                        "queued",
                        "",
                        "cli",
                        None,
                    )
                    .map_err(anyhow::Error::msg)?;
                    let snapshot = exm_core::task_queue::list(&session_id);
                    let seq = snapshot
                        .iter()
                        .find(|t| t.id == id)
                        .map(|t| t.seq)
                        .unwrap_or_default();
                    println!(
                        "{}",
                        serde_json::json!({"ok":true,"status":"queued","queueId":id,"seq":seq,"pending":true})
                    );
                }
                QueueAction::Edit {
                    session_id,
                    queue_id,
                    new_instruction,
                    revision,
                } => {
                    core_session(&workspace, &session_id)?;
                    let item = exm_core::task_queue::edit(
                        &session_id,
                        &queue_id,
                        &new_instruction,
                        revision,
                    )
                    .map_err(anyhow::Error::msg)?;
                    println!("{}", serde_json::to_string_pretty(&item)?);
                }
                QueueAction::Remove {
                    session_id,
                    queue_id,
                } => {
                    core_session(&workspace, &session_id)?;
                    exm_core::task_queue::remove(&session_id, &queue_id)
                        .map_err(anyhow::Error::msg)?;
                    println!(
                        "{}",
                        serde_json::json!({"ok":true,"status":"removed","queueId":queue_id})
                    );
                }
                QueueAction::Move {
                    session_id,
                    queue_id,
                    position,
                } => {
                    core_session(&workspace, &session_id)?;
                    let items = exm_core::task_queue::move_to(&session_id, &queue_id, position)
                        .map_err(anyhow::Error::msg)?;
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({ "items": items }))?
                    );
                }
                QueueAction::Clear { session_id } => {
                    core_session(&workspace, &session_id)?;
                    exm_core::task_queue::clear(&session_id);
                    println!(
                        "{}",
                        serde_json::json!({"ok":true,"status":"succeeded","sessionId":session_id})
                    );
                }
            }
            Ok(())
        }
        Commands::Sessions { action } => {
            let core = build_core(&workspace)?;
            match action {
                SessionAction::New { title } => {
                    let session = core.create_session(title.as_deref().unwrap_or("CLI 会话"))?;
                    println!(
                        "{}",
                        serde_json::json!({
                            "ok": true,
                            "status": "succeeded",
                            "session": session,
                        })
                    );
                }
                SessionAction::List => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&core.list_sessions_in_scope()?)?
                    );
                }
                SessionAction::Resume { session_id } => {
                    let session = core
                        .store
                        .get_session(&session_id)?
                        .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))?;
                    use std::io::IsTerminal;
                    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
                        return tui::run_tui_with_session(core, Some(session), false).await;
                    }
                    return chat::run_interactive_with_session(core, session, false).await;
                }
            }
            Ok(())
        }
        Commands::Stop { session_id } => {
            let core = build_core(&workspace)?;
            let stopped_tools = core.stop_session(&session_id)?;
            println!(
                "{}",
                serde_json::json!({
                    "ok": true,
                    "status": "accepted",
                    "sessionId": session_id,
                    "stoppedTools": stopped_tools,
                })
            );
            Ok(())
        }
        Commands::Steer {
            session_id,
            instruction,
        } => {
            let core = build_core(&workspace)?;
            core.store
                .get_session(&session_id)?
                .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))?;
            if instruction.trim().is_empty() {
                anyhow::bail!("steer 指令不能为空");
            }
            match core.admit_steering(&session_id, &instruction, Some("cli"))? {
                exm_core::SteeringAdmission::Running => println!(
                    "{}",
                    serde_json::json!({
                        "ok": true,
                        "status": "steering_accepted",
                        "running": true,
                        "interrupted": false,
                        "sessionId": session_id,
                    })
                ),
                exm_core::SteeringAdmission::Idle => {
                    core.chat(&session_id, &instruction).await?;
                    println!(
                        "{}",
                        serde_json::json!({
                            "ok": true,
                            "status": "accepted",
                            "mode": "chat",
                            "running": false,
                            "interrupted": false,
                            "sessionId": session_id,
                        })
                    );
                }
            }
            Ok(())
        }
        Commands::Plan { action } => {
            let core = build_core(&workspace)?;
            match action {
                PlanAction::Status { session_id } => {
                    core.store
                        .get_session(&session_id)?
                        .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))?;
                    let output = serde_json::json!({
                        "sessionId": session_id,
                        "mode": if exm_core::plan::is_plan_mode(&session_id) { "plan" } else { "execute" },
                        "items": core.plan_list(&session_id),
                    });
                    println!("{}", serde_json::to_string_pretty(&output)?);
                }
                PlanAction::Start { session_id, task } => {
                    core.store
                        .get_session(&session_id)?
                        .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))?;
                    core.set_plan_mode(&session_id, true);
                    if let Some(task) = task {
                        let message = chat::run_chat(
                            core.clone(),
                            &task,
                            &chat::ChatOptions {
                                session: Some(session_id.clone()),
                                quiet: false,
                            },
                        )
                        .await;
                        message?;
                    }
                    // capability-parity 4.6：json 输出契约——plan 面输出必须
                    // 是机器可解析的单值（human 提示只走 text 模式）
                    if cli.output_format == "json" {
                        println!(
                            "{}",
                            serde_json::json!({"ok":true,"status":"succeeded","mode":"plan"})
                        );
                    } else {
                        println!("计划模式已启用；写工具调用会被转换为计划条目。你可用 `exm plan status {session_id}` 查看。 ");
                    }
                }
                PlanAction::Approve { session_id } => {
                    let applied = core.plan_approve(&session_id).await?;
                    println!(
                        "{}",
                        serde_json::json!({"ok":true,"status":"succeeded","applied":applied.len()})
                    );
                }
                PlanAction::Reject { session_id } => {
                    let retained = core.plan_reject(&session_id);
                    println!(
                        "{}",
                        serde_json::json!({"ok":true,"status":"succeeded","retained":retained.len()})
                    );
                }
                PlanAction::Execute { session_id } => {
                    core.set_plan_mode(&session_id, false);
                    println!(
                        "{}",
                        serde_json::json!({"ok":true,"status":"succeeded","mode":"execute"})
                    );
                }
            }
            Ok(())
        }
        Commands::Spec { action } => {
            let core = build_core(&workspace)?;
            match action {
                SpecAction::Show { session_id } => {
                    core.store
                        .get_session(&session_id)?
                        .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))?;
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&core.spec_get(&session_id)?)?
                    );
                }
                SpecAction::Draft {
                    session_id,
                    objective,
                } => {
                    core.store
                        .get_session(&session_id)?
                        .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))?;
                    let current = core.spec_get(&session_id)?;
                    let spec = core.spec_update(
                        &session_id,
                        &objective,
                        current
                            .as_ref()
                            .map(|spec| spec.scope.clone())
                            .unwrap_or_default(),
                        current
                            .as_ref()
                            .map(|spec| spec.constraints.clone())
                            .unwrap_or_default(),
                        current
                            .as_ref()
                            .map(|spec| spec.acceptance.clone())
                            .unwrap_or_default(),
                        current
                            .as_ref()
                            .map(|spec| spec.verification.clone())
                            .unwrap_or_default(),
                        current.as_ref().map(|spec| spec.revision),
                    )?;
                    println!(
                        "{}",
                        serde_json::json!({"ok":true,"status":"succeeded","spec":spec,"writtenToProject":false})
                    );
                }
                SpecAction::Edit {
                    session_id,
                    objective,
                    revision,
                } => {
                    core.store
                        .get_session(&session_id)?
                        .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))?;
                    let current = core
                        .spec_get(&session_id)?
                        .ok_or_else(|| anyhow::anyhow!("当前会话没有 spec；请先使用 spec draft"))?;
                    let spec = core.spec_update(
                        &session_id,
                        &objective,
                        current.scope,
                        current.constraints,
                        current.acceptance,
                        current.verification,
                        revision.or(Some(current.revision)),
                    )?;
                    println!(
                        "{}",
                        serde_json::json!({"ok":true,"status":"succeeded","spec":spec,"writtenToProject":false})
                    );
                }
                SpecAction::Export { session_id, path } => {
                    core.store
                        .get_session(&session_id)?
                        .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))?;
                    let receipt = core.spec_export(&session_id, &path)?;
                    println!(
                        "{}",
                        serde_json::json!({"ok":true,"status":"succeeded","export":receipt})
                    );
                }
                SpecAction::Clear {
                    session_id,
                    revision,
                } => {
                    core.store
                        .get_session(&session_id)?
                        .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))?;
                    core.spec_clear(
                        &session_id,
                        revision.or(core.spec_get(&session_id)?.map(|spec| spec.revision)),
                    )?;
                    println!(
                        "{}",
                        serde_json::json!({"ok":true,"status":"succeeded","cleared":true})
                    );
                }
            }
            Ok(())
        }
        Commands::Goal { action } => {
            let core = build_core(&workspace)?;
            let (session_id, operation, text, revision) = match action {
                GoalAction::Status { session_id } => {
                    core.store
                        .get_session(&session_id)?
                        .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))?;
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&core.goal_get(&session_id)?)?
                    );
                    return Ok(());
                }
                GoalAction::Start {
                    session_id,
                    objective,
                } => (session_id, "start", Some(objective), None),
                GoalAction::Edit {
                    session_id,
                    objective,
                    revision,
                } => (session_id, "edit", Some(objective), revision),
                GoalAction::Pause {
                    session_id,
                    revision,
                } => (session_id, "pause", None, revision),
                GoalAction::Resume {
                    session_id,
                    revision,
                } => (session_id, "resume", None, revision),
                GoalAction::Complete {
                    session_id,
                    verification,
                    revision,
                } => (session_id, "complete", Some(verification), revision),
                GoalAction::Block {
                    session_id,
                    note,
                    revision,
                } => (session_id, "block", Some(note), revision),
                GoalAction::Clear {
                    session_id,
                    revision,
                } => (session_id, "clear", None, revision),
            };
            core.store
                .get_session(&session_id)?
                .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))?;
            let goal = core.goal_update(
                &session_id,
                operation,
                text.as_deref(),
                if operation == "block" {
                    text.as_deref()
                } else {
                    None
                },
                if operation == "complete" {
                    text.as_deref()
                } else {
                    None
                },
                revision.or_else(|| {
                    core.goal_get(&session_id)
                        .ok()
                        .flatten()
                        .map(|goal| goal.revision)
                }),
            )?;
            println!(
                "{}",
                serde_json::json!({"ok":true,"status":"succeeded","goal":goal})
            );
            Ok(())
        }
        Commands::Gateway { port, host } => {
            let core = build_core(&workspace)?;
            let host_ref = if host.is_empty() {
                None
            } else {
                Some(host.as_str())
            };
            exm_gateway::serve(core, port, host_ref).await
        }
        Commands::Plugin { action } => plugin_cmd::run_plugin_action(&workspace, action).await,
        Commands::Command { action } => {
            use exm_core::command::{CommandEntry, CommandOutputFormat};
            let core = build_core(&workspace)?;
            let session = initial_session.as_ref().map(|session| session.id.as_str());
            let format = if cli.output_format == "json" {
                CommandOutputFormat::Json
            } else {
                CommandOutputFormat::Human
            };
            match action {
                ExtensionCommandAction::List => {
                    let directory = command_cmd::directory(&core, CommandEntry::Cli, session)?;
                    if format == CommandOutputFormat::Json {
                        println!("{}", serde_json::to_string(&directory)?);
                    } else {
                        for entry in directory.entries {
                            let descriptor = entry.descriptor;
                            if entry.executable {
                                println!(
                                    "{} (v{}) — {}",
                                    descriptor.name, descriptor.version, descriptor.summary
                                );
                            } else {
                                let reason = serde_json::to_value(entry.unavailable_reason)?;
                                println!(
                                    "{}（不可用：{}）",
                                    descriptor.name,
                                    reason.as_str().unwrap_or("unavailable")
                                );
                            }
                        }
                    }
                }
                ExtensionCommandAction::Catalog { markdown } => {
                    let directory =
                        command_cmd::command_directory(&core, CommandEntry::Cli, session)?;
                    let rendered = if markdown {
                        command_cmd::render_directory_markdown(&directory)
                    } else if format == CommandOutputFormat::Json {
                        serde_json::to_string_pretty(&directory)?
                    } else {
                        command_cmd::render_directory_text(&directory)
                    };
                    println!("{rendered}");
                }
                ExtensionCommandAction::Run {
                    name,
                    command_version,
                    args,
                } => {
                    let result = command_cmd::execute(
                        &core,
                        CommandEntry::Cli,
                        session,
                        &name,
                        command_version,
                        &args,
                        format,
                    )
                    .await;
                    println!("{}", command_cmd::render(&result, format));
                    if !result.ok {
                        return Err(command_cmd::ReportedCommandFailure.into());
                    }
                }
            }
            Ok(())
        }
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
                                if let Some(params) =
                                    req.get_mut("params").and_then(|p| p.as_object_mut())
                                {
                                    params.insert(
                                        "arguments".into(),
                                        serde_json::json!({ "session": default_sid }),
                                    );
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
                println!(
                    "{}",
                    crate::render::ok(format!(
                        "OpenAPI 契约已写入 {out}（{} 端点）",
                        doc["paths"].as_object().map(|m| m.len()).unwrap_or(0)
                    ))
                );
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
            eprintln!(
                "[worker] 本地就绪（模型：{}），接入 {url}",
                if core.is_mock() { "Mock" } else { "已配置" }
            );
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
                MemoryAction::Search {
                    query,
                    agent,
                    limit,
                } => memory_cmd::run_memory_search(&core, &query, agent.as_deref(), limit),
                MemoryAction::Add {
                    kind,
                    title,
                    body,
                    pin,
                    importance,
                    agent,
                    tags,
                    global,
                } => memory_cmd::run_memory_add(
                    &core,
                    &kind,
                    &title,
                    &body,
                    pin,
                    agent.as_deref(),
                    tags,
                    importance,
                    global,
                ),
                MemoryAction::Pin { id, off } => memory_cmd::run_memory_pin(&core, &id, !off),
                MemoryAction::Edit {
                    id,
                    title,
                    body,
                    tags,
                    kind,
                } => memory_cmd::run_memory_edit(&core, &id, title, body, tags, kind),
                MemoryAction::Forget { id } => memory_cmd::run_memory_forget(&core, &id),
                MemoryAction::Versions { id } => memory_cmd::run_memory_versions(&core, &id),
                MemoryAction::Rollback { id, to } => {
                    memory_cmd::run_memory_rollback(&core, &id, to)
                }
                MemoryAction::Restore { id } => memory_cmd::run_memory_restore(&core, &id),
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
        Commands::Learn { action } => {
            let core = build_core(&workspace)?;
            match action {
                LearnAction::List { status, limit } => {
                    learn_cmd::run_learn_list(&core, status.as_deref(), limit)
                }
                LearnAction::Approve { id } => learn_cmd::run_learn_decide(&core, &id, true, None),
                LearnAction::Reject { id, reason } => {
                    learn_cmd::run_learn_decide(&core, &id, false, reason.as_deref())
                }
            }
        }
        Commands::Skill { action } => {
            let core = build_core(&workspace)?;
            match action {
                SkillAction::List => platform_cmd::run_skill_list(&core),
                SkillAction::Add {
                    id,
                    name,
                    instructions,
                    description,
                    trigger,
                    agent,
                } => platform_cmd::run_skill_add(
                    &core,
                    &id,
                    &name,
                    &instructions,
                    &description,
                    trigger,
                    agent,
                ),
                SkillAction::Remove { id } => platform_cmd::run_skill_remove(&core, &id),
                SkillAction::Import { file } => platform_cmd::run_skill_import(&core, &file),
                SkillAction::Export { id, out } => platform_cmd::run_skill_export(&core, &id, &out),
                SkillAction::Versions { id } => platform_cmd::run_skill_versions(&core, &id),
                SkillAction::Rollback { id, to } => {
                    platform_cmd::run_skill_rollback(&core, &id, to)
                }
            }
        }
        Commands::Context { session_id, action } => {
            let core = build_core(&workspace)?;
            let sid = session_id.as_str();
            match action {
                ContextAction::List => platform_cmd::run_context_list(&core, sid),
                ContextAction::Detail { key } => platform_cmd::run_context_detail(&core, sid, &key),
                ContextAction::Refresh { key } => {
                    platform_cmd::run_context_refresh(&core, sid, key.as_deref())
                }
                ContextAction::Disable { key } => {
                    platform_cmd::run_context_set(&core, sid, &key, true)
                }
                ContextAction::Enable { key } => {
                    platform_cmd::run_context_set(&core, sid, &key, false)
                }
            }
        }
        Commands::Cron { action } => {
            let core = build_core(&workspace)?;
            match action {
                CronAction::List => platform_cmd::run_cron_list(&core),
                CronAction::Add {
                    name,
                    prompt,
                    cron,
                    at,
                    kind,
                    target_session,
                    steps,
                    group,
                    session_title,
                } => platform_cmd::run_cron_add(
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
                ),
                CronAction::Run { id } => platform_cmd::run_cron_now(&core, &id).await,
                CronAction::Enable { id } => platform_cmd::run_cron_enable(&core, &id, true),
                CronAction::Disable { id } => platform_cmd::run_cron_enable(&core, &id, false),
                CronAction::Remove { id } => platform_cmd::run_cron_remove(&core, &id),
                CronAction::Runs { job, limit } => {
                    platform_cmd::run_cron_runs(&core, job.as_deref(), limit)
                }
            }
        }
        Commands::Approval { action } => {
            let core = build_core(&workspace)?;
            match action {
                ApprovalAction::List { status, limit } => {
                    platform_cmd::run_approval_list(&core, status.as_deref(), limit)
                }
                ApprovalAction::Approve { id } => {
                    platform_cmd::run_approval_decide(&core, &id, true).await
                }
                ApprovalAction::Deny { id } => {
                    platform_cmd::run_approval_decide(&core, &id, false).await
                }
            }
        }
        Commands::Model { action } => {
            let core = build_core(&workspace)?;
            match action {
                ModelAction::List => platform_cmd::run_model_list(&core),
                ModelAction::Add {
                    id,
                    name,
                    base_url,
                    api_key,
                    model,
                } => platform_cmd::run_model_add(&core, &id, &name, &base_url, &api_key, &model),
                ModelAction::Use { id } => platform_cmd::run_model_use(&core, &id),
                ModelAction::Remove { id } => platform_cmd::run_model_remove(&core, &id),
                ModelAction::Test { id } => {
                    platform_cmd::run_model_test(&core, id.as_deref()).await
                }
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
                GroupAction::Create {
                    name,
                    id,
                    description,
                } => group_cmd::run_create(&core, &name, id, &description),
                GroupAction::Switch { id } => group_cmd::run_switch(&core, &id),
                GroupAction::Info { id } => group_cmd::run_info(&core, id),
                GroupAction::Delete { id } => group_cmd::run_delete(&core, &id),
                GroupAction::Export { id, out } => group_cmd::run_export(&core, &id, &out),
                GroupAction::Import { file, id } => {
                    group_cmd::run_import(&core, &file, id.as_deref())
                }
            }
        }
        Commands::Agent { action } => {
            let core = build_core(&workspace)?;
            match action {
                AgentAction::Create {
                    name,
                    identifier,
                    description,
                    domain,
                    tier,
                    group,
                    prompt,
                } => group_cmd::run_agent_create(
                    &core,
                    &name,
                    &identifier,
                    &description,
                    &domain,
                    &tier,
                    group.as_deref(),
                    prompt,
                ),
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
                PersonaAction::Set {
                    identifier,
                    text,
                    file,
                } => persona_cmd::run_set(&core, &identifier, text, file),
                PersonaAction::Reset { identifier } => persona_cmd::run_reset(&core, &identifier),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// capability-parity 6.6：CLI 技能版本子命令在 clap 目录中可用
    /// （跨入口验收的 CLI 面；与 REST/聊天共用 registry 事实源）。
    #[test]
    fn skill_versions_and_rollback_subcommands_declared() {
        let cmd = Cli::command();
        let skill = cmd
            .find_subcommand("skill")
            .expect("skill 子命令在场")
            .clone();
        assert!(skill.find_subcommand("versions").is_some(), "versions 在场");
        let rollback = skill
            .find_subcommand("rollback")
            .expect("rollback 在场")
            .clone();
        // --to 必填（目标版本号）
        let missing = Cli::try_parse_from(["exm", "skill", "rollback", "some-id"]);
        assert!(missing.is_err(), "缺 --to 被拒");
        let ok = Cli::try_parse_from(["exm", "skill", "rollback", "some-id", "--to", "2"]);
        assert!(ok.is_ok());
        let _ = rollback;
    }

    /// capability-parity 6.5：记忆侧 CLI 与技能侧同一形态（versions/rollback）+ 撤销遗忘。
    #[test]
    fn memory_versions_rollback_restore_subcommands_declared() {
        let cmd = Cli::command();
        let memory = cmd
            .find_subcommand("memory")
            .expect("memory 子命令在场")
            .clone();
        assert!(
            memory.find_subcommand("versions").is_some(),
            "versions 在场"
        );
        let rollback = memory
            .find_subcommand("rollback")
            .expect("rollback 在场")
            .clone();
        // --to 必填（目标版本号）
        let missing = Cli::try_parse_from(["exm", "memory", "rollback", "some-id"]);
        assert!(missing.is_err(), "缺 --to 被拒");
        let ok = Cli::try_parse_from(["exm", "memory", "rollback", "some-id", "--to", "2"]);
        assert!(ok.is_ok());
        let _ = rollback;
        let restore = memory
            .find_subcommand("restore")
            .expect("restore 在场")
            .clone();
        // restore 只要 id，没有额外必填参数
        let ok = Cli::try_parse_from(["exm", "memory", "restore", "some-id"]);
        assert!(ok.is_ok());
        let _ = restore;
    }

    /// capability-parity 6.6：CLI 学习候选子命令在 clap 目录中可用
    /// （与 REST /api/learning/candidates、通道 /learn 共用同一 Core 事实源）。
    #[test]
    fn learn_list_approve_reject_subcommands_declared() {
        let cmd = Cli::command();
        let learn = cmd
            .find_subcommand("learn")
            .expect("learn 子命令在场")
            .clone();
        let list = learn.find_subcommand("list").expect("list 在场").clone();
        // --status 只接受三个状态值在运行时由 Core 校验；这里验参数可解析
        let ok = Cli::try_parse_from(["exm", "learn", "list", "--status", "pending"]);
        assert!(ok.is_ok());
        let _ = list;
        let approve = learn
            .find_subcommand("approve")
            .expect("approve 在场")
            .clone();
        let missing = Cli::try_parse_from(["exm", "learn", "approve"]);
        assert!(missing.is_err(), "approve 缺 id 被拒");
        let ok = Cli::try_parse_from(["exm", "learn", "approve", "some-id"]);
        assert!(ok.is_ok());
        let _ = approve;
        let reject = learn
            .find_subcommand("reject")
            .expect("reject 在场")
            .clone();
        let ok = Cli::try_parse_from(["exm", "learn", "reject", "some-id", "原因说明"]);
        assert!(ok.is_ok());
        let _ = reject;
    }

    /// capability-parity 6.6 功能面：CLI 学习候选走同一 Core 事实源——
    /// 真实证据候选提交 → CLI list 可见 → CLI approve 转正进技能版本链 →
    /// 版本史可审计（与 REST / 通道同一账本与同一版本链，无第二存储）。
    #[test]
    fn learn_cli_走同一事实源_裁决转正进技能版本链() {
        // 夹具自建工作区（resume_core 的目录不清理，累积会填满 tmpfs）；结束后整棵移除
        let home = std::env::temp_dir().join(format!("exm-learn-cli-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let cfg = exm_core::config::ExmConfig::load(&home);
        let core = Arc::new(Core::with_config(cfg).expect("创建 Core 失败"));
        let out = learn_cli_same_fact_source(&core, &home);
        let _ = std::fs::remove_dir_all(&home);
        out.unwrap();
    }

    /// 函数体：所有断言都在这里，返回 Result 而不是直接 panic——
    /// 这样断言失败时外层仍会清理临时目录。
    fn learn_cli_same_fact_source(core: &Core, home: &std::path::Path) -> Result<(), String> {
        use exm_core::learning_candidates::{CandidateDraft, CandidateEvidenceKind};
        fn e(what: &str, err: impl std::fmt::Display) -> String {
            format!("{what}：{err}")
        }
        // 落点技能预置版本链 v1 → v2：CLI 批准后的转正应续接为 v3，
        // 回执（账本与打印文本）都必须如实报 v3——写死或错位成 v1 在这里才区分得出来。
        let mut pre = exm_core::types::SkillDef {
            id: "learn-cli-skill".into(),
            name: "发布流程".into(),
            instructions: "初版：发布前手工确认".into(),
            ..Default::default()
        };
        core.skill_write_versioned(&pre)
            .map_err(|x| e("预置技能 v1", x))?;
        pre.instructions = "第二版：发布前跑一遍回归".into();
        core.skill_write_versioned(&pre)
            .map_err(|x| e("预置技能 v2", x))?;
        // 真实证据夹具：会话工作区 + 本会话成功写调用（与网关测试同款口径）
        let ws = home.join("ws");
        std::fs::create_dir_all(&ws).map_err(|x| e("建工作区", x))?;
        std::fs::write(ws.join("ARTIFACT.md"), "artifact").map_err(|x| e("写产物", x))?;
        let session = core
            .store
            .create_session_in(
                "learn-cli",
                "exmachina",
                Some(ws.to_string_lossy().into_owned()),
            )
            .map_err(|x| e("建会话", x))?;
        exm_core::round_trace::begin(&session.id);
        exm_core::round_trace::push_tool(
            &session.id,
            exm_core::round_trace::ToolCallRecord {
                call_id: "cli-seed-write".into(),
                agent_id: "exmachina".into(),
                tool: "filesystem".into(),
                args: serde_json::json!({
                    "op": "write", "path": "ARTIFACT.md", "content": "artifact"
                }),
                ok: true,
                duration_ms: 1,
                summary: String::new(),
            },
        );
        let triggers = vec!["发布".to_string()];
        let draft = CandidateDraft {
            target: "skill",
            target_id: Some("learn-cli-skill"),
            title: "CLI 转正链路验证",
            content: "真实产物支撑的经验",
            scope: "global",
            triggers: &triggers,
            source_session: &session.id,
            source_goal: "g",
            evidence: &[(CandidateEvidenceKind::ArtifactFile, "ARTIFACT.md".into())],
        };
        // 提交在 round_trace 之内；之后的每一步都先 end 再判断（不留悬挂的 trace）
        let submitted = core.learning_submit(&draft);
        exm_core::round_trace::end(&session.id);
        let cand = submitted.map_err(|x| e("真实证据候选过审", x))?;

        // CLI list 面可见（同一账本）
        let listed = core
            .learning_candidates(Some("pending"))
            .map_err(|x| e("候选清单", x))?;
        if !listed.iter().any(|c| c.id == cand.id) {
            return Err("CLI 用的 learning_candidates 与提交账本不是同一份".into());
        }
        learn_cmd::run_learn_list(core, Some("pending"), 20).map_err(|x| e("CLI list 渲染", x))?;

        // CLI approve → 转正进技能版本链（同一版本事实源，无第二存储）
        learn_cmd::run_learn_decide(core, &cand.id, true, None).map_err(|x| e("CLI approve", x))?;
        let hist = core
            .skill_history_detailed("learn-cli-skill")
            .map_err(|x| e("技能版本史", x))?;
        let vers: Vec<u64> = hist.iter().map(|r| r.version).collect();
        if vers != vec![1, 2, 3] {
            return Err(format!("转正应续接既有技能链为 v1、v2、v3，实际 {vers:?}"));
        }
        if (hist[2].version, hist[2].name.as_str()) != (3, "CLI 转正链路验证") {
            return Err(format!(
                "当前版本（v3）的名称不符：v{} {}",
                hist[2].version, hist[2].name
            ));
        }
        if hist[1].instructions != "第二版：发布前跑一遍回归" {
            return Err("被转正覆盖的 v2 应留在版本史里可回滚".into());
        }
        // 决策记录在候选账本（同一账本的 decided 状态）：list 里找已批准且带转正回执
        let approved = core
            .learning_candidates(Some("approved"))
            .map_err(|x| e("已批准清单", x))?;
        let decided = approved
            .iter()
            .find(|c| c.id == cand.id)
            .ok_or_else(|| "裁决后的候选不在账本".to_string())?;
        let promo = decided
            .promotion
            .as_ref()
            .ok_or_else(|| "转正回执未记录".to_string())?;
        let got = (
            decided.status.as_str(),
            promo.ok,
            promo.ref_id.as_str(),
            promo.version,
        );
        let want = ("approved", true, "learn-cli-skill", Some(3));
        if got != want {
            return Err(format!("裁决/转正回执不符：{got:?}，应为 {want:?}"));
        }
        // CLI 打印的回执文本与账本回执同源：落点 id + 实际生效版本号
        let line = learn_cmd::promoted_line(promo);
        if line != "已批准并转正 → learn-cli-skill v3" {
            return Err(format!("CLI 回执文本不符：{line:?}"));
        }

        // 批准 ≠ 生效：内容在批准前被改坏（直接改账本模拟）→ 转正失败。
        // CLI 必须以错误形式如实上报（裁决保留在账本），不得打印「已批准并转正」。
        // 第二条候选要有自己的会话 + 本会话写回执：上面那条会话的回执已随 round_trace 结算，
        // 再引用同一产物会被证据闸拒掉（闸在正常工作）。
        let ws2 = home.join("ws2");
        std::fs::create_dir_all(&ws2).map_err(|x| e("建第二个工作区", x))?;
        std::fs::write(ws2.join("LEAK.md"), "artifact").map_err(|x| e("写产物", x))?;
        let session2 = core
            .store
            .create_session_in(
                "learn-cli-leaky",
                "exmachina",
                Some(ws2.to_string_lossy().into_owned()),
            )
            .map_err(|x| e("建第二个会话", x))?;
        exm_core::round_trace::begin(&session2.id);
        exm_core::round_trace::push_tool(
            &session2.id,
            exm_core::round_trace::ToolCallRecord {
                call_id: "cli-seed-write-2".into(),
                agent_id: "exmachina".into(),
                tool: "filesystem".into(),
                args: serde_json::json!({
                    "op": "write", "path": "LEAK.md", "content": "artifact"
                }),
                ok: true,
                duration_ms: 1,
                summary: String::new(),
            },
        );
        let triggers = vec!["泄露".to_string()];
        let draft = CandidateDraft {
            target: "skill",
            target_id: Some("learn-cli-leaky"),
            title: "CLI 转正失败回执验证",
            content: "看起来正常的内容",
            scope: "global",
            triggers: &triggers,
            source_session: &session2.id,
            source_goal: "g",
            evidence: &[(CandidateEvidenceKind::ArtifactFile, "LEAK.md".into())],
        };
        let submitted = core.learning_submit(&draft);
        exm_core::round_trace::end(&session2.id);
        let leaky = submitted.map_err(|x| e("第二条候选过审", x))?;
        let mut all = core.store.list_learning_candidates();
        let row = all
            .iter_mut()
            .find(|c| c.id == leaky.id)
            .ok_or_else(|| "第二条候选不在账本".to_string())?;
        row.content = "部署时用 API_KEY=sk-abcdefghijklmnopqr 直连".into();
        core.store
            .update_learning_candidate(row)
            .map_err(|x| e("改写候选账本", x))?;
        // 不用 expect_err：helper 内 panic 会绕过外层的临时目录清理
        let msg = match learn_cmd::run_learn_decide(core, &leaky.id, true, None) {
            Ok(()) => return Err("转正失败时 CLI 必须报错，不能报成功".into()),
            Err(err) => err.to_string(),
        };
        if !msg.contains("已批准") || !msg.contains("转正失败") {
            return Err(format!(
                "CLI 失败回执应同时说明「已批准」与「转正失败」：{msg}"
            ));
        }
        if !msg.contains("敏感") {
            return Err(format!("CLI 失败回执应带失败原因：{msg}"));
        }
        if !msg.contains("裁决保留") {
            return Err(format!("CLI 失败回执应说明裁决保留（不静默撤销）：{msg}"));
        }
        // 账本事实：裁决保留为 approved，promotion.ok=false；落点技能不存在
        let decided = core
            .learning_candidates(Some("approved"))
            .map_err(|x| e("已批准清单", x))?
            .into_iter()
            .find(|c| c.id == leaky.id)
            .ok_or_else(|| "失败的转正不得回滚用户裁决".to_string())?;
        let promo = decided
            .promotion
            .as_ref()
            .ok_or_else(|| "转正回执未记录".to_string())?;
        if promo.ok {
            return Err("敏感内容不得转正成功".into());
        }
        if core
            .skill_history_detailed("learn-cli-leaky")
            .map(|h| !h.is_empty())
            .unwrap_or(false)
        {
            return Err("失败的转正不得留下半成品技能".into());
        }
        Ok(())
    }

    #[test]
    fn json_error_output_has_stable_category_and_redacts_inline_credentials() {
        let output = cli_error_result(
            "invalid_args",
            "missing option with token Bearer superSecretCredential123456",
        );
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["status"], "rejected");
        assert_eq!(value["error"]["category"], "invalid_args");
        assert!(value["requestId"].as_str().is_some_and(|id| !id.is_empty()));
        assert!(!output.contains("superSecretCredential123456"));
    }

    #[test]
    fn json_output_flag_detection_handles_equals_and_separate_forms() {
        assert!(requested_json_output(&[
            "exm".into(),
            "--output-format=json".into(),
            "status".into(),
        ]));
        assert!(requested_json_output(&[
            "exm".into(),
            "--output-format".into(),
            "json".into(),
            "status".into(),
        ]));
        assert!(!requested_json_output(&["exm".into(), "status".into()]));
    }

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
        assert_eq!(
            got.as_deref(),
            Some(b.id.as_str()),
            "显式 --session 必须最优先"
        );
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
        assert!(
            msg.contains("调试前端问题") && msg.contains("调试后端问题"),
            "候选清单：{msg}"
        );
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

    #[test]
    fn cli顶层命令已登记到统一目录且基础命令仍可解析() {
        validate_cli_command_catalog().expect("CLI 与统一目录不应漂移");
        let registry = cli_command_registry().expect("构造 CLI 目录");
        for name in ["model", "config", "memory", "skill", "cron", "approval"] {
            assert!(registry.resolve(name).is_some(), "基础命令缺失：{name}");
        }
        for name in ["cli.tui", "cli.chat", "cli.gateway", "cli.openapi"] {
            assert!(
                registry.resolve(name).is_some(),
                "Clap 顶层命令缺失：{name}"
            );
        }
    }

    /// 隔离根下的 Core + 一个绑定了临时项目的会话（有项目层文件来源）。
    fn context_fixture(
        agents_md: Option<&str>,
    ) -> (Arc<Core>, exm_core::types::Session, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("exm-cli-ctx-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let core = Arc::new(Core::create(&root).unwrap());
        let ws = root.join("project");
        std::fs::create_dir_all(&ws).unwrap();
        if let Some(text) = agents_md {
            std::fs::write(ws.join("AGENTS.md"), text).unwrap();
        }
        let ws_str = ws.canonicalize().unwrap().to_string_lossy().into_owned();
        let session = core
            .store
            .create_session_in("ctx-cli", &core.active_group(), Some(ws_str))
            .unwrap();
        (core, session, root)
    }

    #[test]
    fn context子命令_已声明且目录已登记() {
        // clap 声明：位置参数 <session_id> + 五个子命令
        for args in [
            vec!["exm", "context", "sid-1", "list"],
            vec!["exm", "context", "sid-1", "detail", "file:AGENTS.md"],
            vec!["exm", "context", "sid-1", "refresh"],
            vec!["exm", "context", "sid-1", "refresh", "memory"],
            vec!["exm", "context", "sid-1", "disable", "skills"],
            vec!["exm", "context", "sid-1", "enable", "skills"],
        ] {
            assert!(
                Cli::try_parse_from(args.clone()).is_ok(),
                "应可解析：{args:?}"
            );
        }
        // 缺会话 id / 缺子命令是用法错误
        assert!(Cli::try_parse_from(["exm", "context", "list"]).is_err());
        assert!(Cli::try_parse_from(["exm", "context"]).is_err());
        // 目录：顶层 `context` 映射到规范命令 context.list（漂移即失败）
        validate_cli_command_catalog().expect("CLI 与统一目录不应漂移");
        let registry = cli_command_registry().unwrap();
        assert!(
            registry.resolve("cli.context").is_some(),
            "cli.context 入口缺失"
        );
        // 规范目录里五个 context.* 与通道 /context 子命令同名同义
        let builtin = exm_core::command::CommandRegistry::with_builtins().unwrap();
        for name in [
            "context.list",
            "context.detail",
            "context.refresh",
            "context.disable",
            "context.enable",
        ] {
            assert!(builtin.resolve(name).is_some(), "规范命令缺失：{name}");
        }
        assert_eq!(builtin.resolve("context").unwrap().name, "context.list");
        // 刷新是只读（无副作用）；开关是会话状态写入
        use exm_core::command::{CommandPermission, CommandSideEffect};
        assert_eq!(
            builtin.resolve("context.refresh").unwrap().side_effect,
            CommandSideEffect::None
        );
        assert_eq!(
            builtin.resolve("context.refresh").unwrap().permissions,
            vec![CommandPermission::Read]
        );
        assert_eq!(
            builtin.resolve("context.disable").unwrap().side_effect,
            CommandSideEffect::SessionState
        );
    }

    #[test]
    fn context_cli_清单详情与缺失原因() {
        let (core, session, root) = context_fixture(Some("# 规则\nCLI 清单测试"));
        let sid = &session.id;
        // list 成功
        platform_cmd::run_context_list(&core, sid).unwrap();
        let sources = core.context_sources(sid).unwrap();
        let agents = sources
            .iter()
            .find(|s| s["key"] == "file:AGENTS.md")
            .unwrap();
        assert_eq!(agents["status"], "loaded");
        // 运行时来源带真实缺失原因（Core::create 默认配置下如实汇报，不硬编码是哪一种）
        for key in ["memory", "retrieval", "skills"] {
            let s = sources.iter().find(|s| s["key"] == key).unwrap();
            if s["status"] != "ready" {
                assert!(
                    s["reason"].as_str().is_some_and(|r| !r.is_empty()),
                    "{key} 未就绪必须带原因：{s}"
                );
            }
        }
        // detail：已知键成功；未知键报错并列出可用键
        platform_cmd::run_context_detail(&core, sid, "file:AGENTS.md").unwrap();
        let err = platform_cmd::run_context_detail(&core, sid, "file:nope.md")
            .unwrap_err()
            .to_string();
        assert!(err.contains("没有上下文来源 file:nope.md"), "{err}");
        assert!(
            err.contains("file:AGENTS.md"),
            "可用键里应含已知来源：{err}"
        );
        // 会话不存在：如实报错（不 panic、不伪造清单）
        let err = platform_cmd::run_context_list(&core, "no-such-session")
            .unwrap_err()
            .to_string();
        assert!(err.contains("会话不存在"), "{err}");
        drop(core);
        let _ = std::fs::remove_dir_all(root);
    }

    /// 整层缺席的文件来源要有原因（未绑定项目）：CLI 的说明行由 Core 给出的
    /// 原因决定——呈现逻辑 `context_gap_line` 是纯函数，直接对 Core 的真实返回断言。
    #[test]
    fn context_cli_整层缺席要说明原因() {
        let root = std::env::temp_dir().join(format!("exm-cli-ctx-gap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let core = Arc::new(Core::create(&root).unwrap());
        // 未绑定项目的会话（CLI/WebUI 里 `exm` 直接建的就是这种）
        let session = core
            .store
            .create_session("未绑定", &core.active_group())
            .unwrap();
        let sid = &session.id;

        let gaps = core.context_source_gaps(sid).unwrap();
        let project = gaps
            .iter()
            .find(|g| g["layer"] == "project")
            .expect("未绑定会话缺项目层");
        let line = platform_cmd::context_gap_line(project);
        assert!(line.starts_with("[项目 层]"), "标明是哪一层：{line}");
        assert!(line.contains("未绑定项目"), "原因要说出来：{line}");
        assert!(
            line.contains("AGENTS.md") && line.contains(".exmachina/context.md"),
            "列出没加载的文件：{line}"
        );
        // 原因原样来自 Core，格式化不改写它
        assert!(line.contains(project["reason"].as_str().unwrap()), "{line}");

        // 清单里确实没有项目层来源（说明与清单一致），list 仍成功返回
        assert!(core
            .context_sources(sid)
            .unwrap()
            .iter()
            .all(|s| s["layer"] != "project"));
        platform_cmd::run_context_list(&core, sid).unwrap();

        // 绑定了项目：没有项目层的缺席说明
        let ws = root.join("project");
        std::fs::create_dir_all(&ws).unwrap();
        let bound = core
            .store
            .create_session_in(
                "已绑定",
                &core.active_group(),
                Some(ws.canonicalize().unwrap().to_string_lossy().into_owned()),
            )
            .unwrap();
        let gaps2 = core.context_source_gaps(&bound.id).unwrap();
        assert!(gaps2.iter().all(|g| g["layer"] != "project"), "{gaps2:?}");
        drop(core);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn context_cli_刷新_单来源失败不影响其余且非零退出() {
        let (core, session, root) = context_fixture(Some("# v1"));
        let sid = &session.id;
        std::fs::create_dir_all(root.join("project").join(".exmachina")).unwrap();
        std::fs::write(
            root.join("project").join(".exmachina").join("context.md"),
            "# 补充",
        )
        .unwrap();
        let before_sources = core.context_sources(sid).unwrap();
        assert!(!before_sources.is_empty());
        // 先停用一个别的来源：刷新不得改动禁用清单
        platform_cmd::run_context_set(&core, sid, "skills", true).unwrap();
        // 快照在「停用之后、刷新之前」拍：后面比对的是「刷新有没有改变任何东西」，
        // 不能把测试自己的停用操作算到刷新头上
        let before = core.context_sources(sid).unwrap();
        assert_eq!(
            before.iter().find(|s| s["key"] == "skills").unwrap()["status"],
            "disabled",
            "夹具前提：skills 已停用"
        );
        // 单来源刷新成功
        platform_cmd::run_context_refresh(&core, sid, Some("file:AGENTS.md")).unwrap();
        // 让 AGENTS.md 读不出来（路径上是目录）
        std::fs::remove_file(root.join("project").join("AGENTS.md")).unwrap();
        std::fs::create_dir_all(root.join("project").join("AGENTS.md")).unwrap();
        // 单来源刷新失败：返回 Err（CLI 非零退出），其余来源与禁用清单不变
        let err = platform_cmd::run_context_refresh(&core, sid, Some("file:AGENTS.md"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("刷新失败"), "{err}");
        let after = core.context_sources(sid).unwrap();
        for b in &before {
            if b["key"] == "file:AGENTS.md" {
                continue;
            }
            let a = after.iter().find(|x| x["key"] == b["key"]).unwrap();
            assert_eq!(
                (a["status"].clone(), a["disabled"].clone()),
                (b["status"].clone(), b["disabled"].clone()),
                "{} 不受刷新失败影响",
                b["key"]
            );
        }
        assert_eq!(
            core.store.disabled_context_sources(sid),
            vec!["skills".to_string()]
        );
        // 整表刷新：一个失败不中断其余——其余来源照样被刷新（事件逐个留痕），
        // 整体返回 Err（非零退出），汇总里点名失败的那一个
        let refreshed = |core: &Arc<Core>| -> Vec<(String, bool)> {
            core.store
                .list_events(sid)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|e| {
                    let p = &e["payload"];
                    (p.get("type").and_then(|t| t.as_str()) == Some("context.source.refreshed"))
                        .then(|| {
                            let inner = &p["payload"];
                            (
                                inner
                                    .get("key")
                                    .and_then(|k| k.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                inner.get("ok").and_then(|k| k.as_bool()).unwrap_or(false),
                            )
                        })
                })
                .collect()
        };
        let refreshed_before = refreshed(&core).len();
        let err = platform_cmd::run_context_refresh(&core, sid, None)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("1 个来源刷新失败") && err.contains("file:AGENTS.md"),
            "汇总点名失败来源：{err}"
        );
        let batch: Vec<(String, bool)> = refreshed(&core)
            .into_iter()
            .skip(refreshed_before)
            .collect();
        // 每个来源都刷新过（逐个留痕，含失败的那个），不因一个失败而中断
        assert_eq!(
            batch.len(),
            before.len(),
            "整表刷新应逐个来源留痕：{batch:?}"
        );
        assert_eq!(
            batch.iter().filter(|(_, ok)| !ok).count(),
            1,
            "恰好失败一个：{batch:?}"
        );
        assert!(
            batch.iter().any(|(k, ok)| k == "file:AGENTS.md" && !ok),
            "失败的正是读不出来的那个：{batch:?}"
        );
        assert!(
            batch
                .iter()
                .any(|(k, ok)| k == "file:.exmachina/context.md" && *ok),
            "另一个文件来源照常刷新成功：{batch:?}"
        );
        // 未知键是用法错误，不是刷新失败
        let err = platform_cmd::run_context_refresh(&core, sid, Some("file:nope.md"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("没有上下文来源 file:nope.md"), "{err}");
        assert!(!err.contains("刷新失败"), "未知键不该被说成刷新失败：{err}");
        drop(core);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn context_cli_开关_独立且可恢复_未知键拒绝() {
        let (core, session, root) = context_fixture(Some("# 规则"));
        let sid = &session.id;
        platform_cmd::run_context_set(&core, sid, "file:AGENTS.md", true).unwrap();
        let sources = core.context_sources(sid).unwrap();
        let agents = sources
            .iter()
            .find(|s| s["key"] == "file:AGENTS.md")
            .unwrap();
        assert_eq!(
            (agents["disabled"].clone(), agents["baseStatus"].clone()),
            (true.into(), "loaded".into())
        );
        // 其他来源未被连带禁用
        assert!(sources.iter().filter(|s| s["disabled"] == true).count() == 1);
        // 未知键：拒绝、不留痕、说明原因
        let err = platform_cmd::run_context_set(&core, sid, "file:nope.md", true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("未知上下文来源"), "{err}");
        assert!(err.contains("可用："), "{err}");
        assert_eq!(
            core.store.disabled_context_sources(sid),
            vec!["file:AGENTS.md".to_string()]
        );
        // enable 恢复
        platform_cmd::run_context_set(&core, sid, "file:AGENTS.md", false).unwrap();
        assert!(core.store.disabled_context_sources(sid).is_empty());
        let sources = core.context_sources(sid).unwrap();
        assert_eq!(
            sources
                .iter()
                .find(|s| s["key"] == "file:AGENTS.md")
                .unwrap()["status"],
            "loaded"
        );
        drop(core);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn spec_export_has_explicit_session_and_project_path_arguments() {
        let parsed =
            Cli::try_parse_from(["exm", "spec", "export", "session-123", "docs/spec.md"]).unwrap();
        assert!(matches!(
            parsed.command,
            Some(Commands::Spec {
                action: SpecAction::Export { session_id, path }
            }) if session_id == "session-123" && path == "docs/spec.md"
        ));
    }

    #[test]
    fn command_catalog_markdown_is_explicit_and_names_are_checked_against_the_directory() {
        let parsed = Cli::try_parse_from(["exm", "command", "catalog", "--markdown"]).unwrap();
        assert!(matches!(
            parsed.command,
            Some(Commands::Command {
                action: ExtensionCommandAction::Catalog { markdown: true }
            })
        ));

        let matrix = include_str!("../../../docs/command-entry-matrix.md");
        for descriptor in exm_core::command::builtin_command_descriptors() {
            assert!(
                matrix.contains(&format!(
                    "| `{}` | `{}` |",
                    descriptor.name, descriptor.usage
                )),
                "entry matrix omitted canonical command {}",
                descriptor.name
            );
        }
        let root =
            std::env::temp_dir().join(format!("exm-command-matrix-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let core = Arc::new(Core::create(&root).unwrap());
        let snapshot =
            command_cmd::command_directory(&core, exm_core::command::CommandEntry::Cli, None)
                .unwrap();
        assert_eq!(command_cmd::render_directory_markdown(&snapshot), matrix);
        drop(core);
        let _ = std::fs::remove_dir_all(root);

        for args in [
            vec!["exm", "help", "spec.export"],
            vec!["exm", "sessions", "list"],
            vec!["exm", "stop", "session-123"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok());
        }
    }
}
