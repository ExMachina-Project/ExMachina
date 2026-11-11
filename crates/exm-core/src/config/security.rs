//! config 域分文件：安全配置（SecurityConfig/SanitizeRule/ToolCallBudget/ComputerUse/Sandbox）。

use super::*;
use serde::{Deserialize, Serialize};

/// 安全配置：终端命令审批闸门（docs/10）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecurityConfig {
    /// off | risky | always
    pub exec_approval: String,
    /// 命令前缀白名单（命中前缀即跳过审批闸门）
    pub exec_allowlist: Vec<String>,
    /// 后台访问密钥：非空时 /api/* 全部要求鉴权（env EXM_AUTH_KEY 最高优先）
    pub auth_key: String,
    /// 终端命令超时（秒）；后台任务不受此限
    pub terminal_timeout_secs: u32,
    /// 工具结果超过该字符数时落盘并回填路径（0 = 不落盘，仅截断）
    pub tool_output_spill_chars: usize,
    /// 审批等待（秒）：拦截后运行时在原地等待人工决定的最大时长，
    /// 期间批准 → 执行结果直接回灌模型继续干活；超时 → 按「受阻回流」交还上层。0 = 不等待（旧行为）
    #[serde(default)]
    pub approval_wait_secs: u64,
    /// 单智能体对话循环（chat_execute）默认最大工具步数；
    /// 个体 maxSteps / 作用域档位可覆盖此默认
    #[serde(default = "default_chat_max_steps")]
    pub chat_max_steps: usize,
    /// 上下文预算安全线：估算 token 超模型窗口该比例时触发多级裁剪
    /// （0 = 用默认 0.8；合法范围 0.5~1.0，越界回落默认）
    #[serde(default = "default_context_budget_ratio")]
    pub context_budget_ratio: f64,
    /// 首启密钥设置引导：无密钥部署的首次启动会引导设置访问密钥；
    /// 选「不再提醒」后为 true（「下次再说」只记进程内，网关重启即恢复提醒）
    #[serde(default)]
    pub key_setup_dismissed: bool,
    /// 入站消毒规则开关（inbound-sanitization）：false = 关闭该规则（检出也不改写）。
    /// 规则族：fake-system-prompt（伪装系统指令）/ hijack-directive（指令劫持短语）/
    /// fake-tool-result（工具结果伪装）/ hidden-unicode（隐藏控制字符）
    #[serde(default)]
    pub sanitize_rules: SanitizeRuleConfig,
    /// Agent 执行身份约束（Linux）：终端子进程降权到此用户（用户名或数字 uid；
    /// 空 = 不约束，默认）。开启后 root 执行需经提权审批（[root] 前缀审批单）
    #[serde(default)]
    pub exec_user: String,
    /// 圈禁根目录（绝对路径；空 = 自动推导 /home/<execUser> 或 getpwuid 查询）
    #[serde(default)]
    pub exec_home: String,
    /// 工具调用预算三值（agent-budget-audit）：默认仅单轮 48（覆盖既有硬上限
    /// 语义,行为等价收紧）；0 = 单项关闭,全零 = 预算整体关闭
    #[serde(default)]
    pub tool_call_budget: ToolCallBudget,
}

/// 消毒规则开关（全默认开；单项可关）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SanitizeRuleConfig {
    #[serde(default = "default_true")]
    pub fake_system_prompt: bool,
    #[serde(default = "default_true")]
    pub hijack_directive: bool,
    #[serde(default = "default_true")]
    pub fake_tool_result: bool,
    /// 隐藏 Unicode 剥离建议常开（关闭后零宽/双向覆写字符可穿透）
    #[serde(default = "default_true")]
    pub hidden_unicode: bool,
}

impl Default for SanitizeRuleConfig {
    fn default() -> Self {
        SanitizeRuleConfig {
            fake_system_prompt: true,
            hijack_directive: true,
            fake_tool_result: true,
            hidden_unicode: true,
        }
    }
}

/// 工具调用预算三值（agent-budget-audit）：0 = 不限（单项关闭）。
/// 档位表在 ToolGateway（chat < tools < computer），此处为 computer 档全量值,
/// 低档位按比例收严。config 显式值直接生效（覆盖档位表）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallBudget {
    /// 单轮最大工具调用数（一轮用户输入内的工具循环上限）
    #[serde(default)]
    pub per_turn: usize,
    /// 单会话累计上限（进程生命周期内；会话删除不重置——防长会话慢性消耗）
    #[serde(default)]
    pub per_session: usize,
    /// 时间窗内最大调用数（滑动窗口秒数 windowSecs）
    #[serde(default)]
    pub per_window: usize,
    /// 时间窗长度（秒）；0 = 时间窗预算关闭
    #[serde(default)]
    pub window_secs: u64,
}

impl Default for ToolCallBudget {
    fn default() -> Self {
        ToolCallBudget {
            per_turn: 48,
            per_session: 0,
            per_window: 0,
            window_secs: 60,
        }
    }
}

impl ToolCallBudget {
    /// 全零 = 预算完全关闭（兼容旧行为）
    pub fn disabled(&self) -> bool {
        self.per_turn == 0 && self.per_session == 0 && self.per_window == 0
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        SecurityConfig {
            exec_approval: "off".into(),
            exec_allowlist: vec![],
            auth_key: String::new(),
            terminal_timeout_secs: 120,
            tool_output_spill_chars: 12000,
            approval_wait_secs: 120,
            chat_max_steps: default_chat_max_steps(),
            context_budget_ratio: default_context_budget_ratio(),
            key_setup_dismissed: false,
            sanitize_rules: SanitizeRuleConfig::default(),
            exec_user: String::new(),
            exec_home: String::new(),
            tool_call_budget: ToolCallBudget::default(),
        }
    }
}

/// Computer Use 配置（computer 工具：截屏 + 鼠标 / 键盘控制本机桌面）。
/// **默认关闭**——把整台机器的输入权交给模型必须是显式决定。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerUseConfig {
    /// 总开关：false = computer 工具不下发给模型
    pub enabled: bool,
    /// 输入类动作（点击 / 键入 / 滚动 / 拖拽 / 热键）是否走审批闸门（截屏始终只读放行）
    pub approval: bool,
    /// 截图最长边（像素，超出等比缩小后回灌视觉模型；0 = 不缩放）
    pub max_edge: u32,
    /// 单次动作间隔下限（毫秒）：防失控连点
    pub action_interval_ms: u64,
}

impl Default for ComputerUseConfig {
    fn default() -> Self {
        ComputerUseConfig {
            enabled: false,
            approval: true,
            max_edge: 1568,
            action_interval_ms: 120,
        }
    }
}

/// 沙箱执行配置：让 AI 在不扩大爆炸半径的前提下自行跑命令。
/// - off：仅白名单 + 审批（历史行为）
/// - workspace：**环境净化**（白名单环境变量 + HOME/TMP 重定向到 .exmachina/sandbox）
/// - strict：净化 + 联网类命令一律走审批 + 更短超时；Linux/macOS 检测到 bubblewrap 时自动做系统级隔离
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxConfig {
    /// off | workspace | strict
    pub mode: String,
    /// strict 模式下是否允许安装/下载类联网命令（false = 需审批）
    pub allow_network: bool,
    /// Linux/macOS：检测到 bwrap 时用它做文件系统隔离（工作区可写、根只读、默认断网）
    pub use_bwrap: bool,
    /// 子进程内存上限（MB；Linux 经 ulimit、Windows 经作业对象生效，0 = 不限）
    pub memory_mb: usize,
    /// 子进程数上限（Windows 作业对象生效，0 = 不限）
    #[serde(default)]
    pub max_processes: usize,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        SandboxConfig {
            mode: "workspace".into(),
            allow_network: false,
            use_bwrap: true,
            memory_mb: 0,
            max_processes: 0,
        }
    }
}

impl SandboxConfig {
    pub fn enabled(&self) -> bool {
        !self.mode.trim().eq_ignore_ascii_case("off")
    }
    pub fn strict(&self) -> bool {
        self.mode.trim().eq_ignore_ascii_case("strict")
    }
}

