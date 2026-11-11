//! config 域分文件：杂项配置（CustomTool/Tools/Browser/Identity/Limits/Trash/Experiments/Link/Nexus/Notify/MCP/Install）。

use super::*;
use serde::{Deserialize, Serialize};

/// 浏览器自动化配置（browser 工具：headless Chrome/Chromium + CDP）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserConfig {
    /// 浏览器可执行文件路径；空 = 自动探测常见安装位置
    pub executable: String,
    /// 是否 headless（默认 true）
    pub headless: bool,
    /// 单次操作超时（秒）
    pub timeout_secs: u64,
    /// 单页正文提取上限（字符）
    pub max_chars: usize,
    /// 浏览器搜索的搜索引擎（browser-search）：bing | duckduckgo | baidu。
    /// 无需 API Key——直接导航搜索页取渲染结果，与 search.* 后端互补：
    /// API 后端给结构化结果，浏览器搜索给「真实网页所见即所得」。
    #[serde(default = "default_browser_engine")]
    pub search_engine: String,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        BrowserConfig {
            executable: String::new(),
            headless: true,
            timeout_secs: 30,
            max_chars: 8000,
            search_engine: default_browser_engine(),
        }
    }
}

/// 声明式自定义工具：把**外部 HTTP 接口**或**本机命令模板**包装成模型可调用的工具，
/// 免去为每个集成单写一个 MCP 服务器。名字与内置工具同命名空间（同名时内置工具优先）。
///
/// 例（HTTP GET，参数以 `{参数名}` 占位）：
/// `{ "name": "weather", "description": "查天气", "kind": "http", "method": "GET",
///    "target": "https://api.example.com/now?city={city}",
///    "parameters": { "type": "object", "properties": { "city": { "type": "string" } },
///                    "required": ["city"] },
///    "headers": { "Authorization": "Bearer …" }, "agents": [] }`
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CustomTool {
    /// 工具名（模型可见；字母开头，仅字母/数字/下划线/连字符）
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// 参数 JSON Schema（对象）；缺省 = 无参数
    #[serde(default)]
    pub parameters: serde_json::Value,
    /// http（默认）| shell
    #[serde(default)]
    pub kind: String,
    /// http：URL（`{参数名}` 占位 → URL 编码值）；shell：命令模板（占位 → 安全转义值）
    #[serde(default)]
    pub target: String,
    /// http：GET | POST（默认 POST，全部实参作为 JSON body）
    #[serde(default)]
    pub method: String,
    /// http：附加请求头（BTreeMap 保证序列化顺序稳定）
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
    /// 可见个体（identifier 列表）；空 = 全部个体可用
    #[serde(default)]
    pub agents: Vec<String>,
    /// http：响应文本上限（缺省 8000 维持既有截断口径；插件生态扩展）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_max_chars: Option<usize>,
    /// http：JSON path 摘取（`$.a.b[0].c`；JSON 响应按表达式摘取片段，非 JSON 忽略）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_select: Option<String>,
}

impl CustomTool {
    /// 名字合法（与内置工具命名风格一致，避免与 MCP 的 `mcp:` 前缀冲突）
    pub fn valid(&self) -> bool {
        let n = self.name.trim();
        !n.is_empty()
            && n.len() <= 48
            && !n.contains(':')
            && n.starts_with(|c: char| c.is_ascii_alphabetic())
            && n.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }

    /// 归一化类型：http（默认）| shell
    pub fn kind_norm(&self) -> String {
        let k = self.kind.trim().to_ascii_lowercase();
        if k == "shell" {
            "shell".into()
        } else {
            "http".into()
        }
    }

    /// 该个体是否可见（agents 为空 = 全部可见）
    pub fn visible_to(&self, agent_id: &str) -> bool {
        self.agents.is_empty() || self.agents.iter().any(|a| a == agent_id)
    }

    /// 参数 schema（缺省给空对象 schema，满足 function calling 契约）
    pub fn param_schema(&self) -> serde_json::Value {
        if self.parameters.is_null() {
            serde_json::json!({ "type": "object", "properties": {} })
        } else {
            self.parameters.clone()
        }
    }
}

/// 工具面配置：目前只有声明式自定义工具列表
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ToolsConfig {
    #[serde(default)]
    pub custom: Vec<CustomTool>,
}

/// 通道身份（配对绑定与群聊唤醒门控）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityConfig {
    /// 配对码有效期（秒）
    pub pairing_ttl_secs: u64,
    /// 通道身份管控总开关：true = 白名单内用户也须配对方可执行；false = 维持白名单即全权
    pub identity_required: bool,
    /// 群聊唤醒门控的通道默认值（开 = 群内仅提及/回复触发；新装默认开，升级迁移显式置关）
    pub group_gate_default: bool,
    /// 配对码防爆破：窗口内连续失败达该次数即锁定（0 = 关闭防爆破）
    #[serde(default = "default_pairing_max_failures")]
    pub pairing_max_failures: u32,
    /// 配对码防爆破：锁定窗口时长（秒）
    #[serde(default = "default_pairing_lockout_secs")]
    pub pairing_lockout_secs: u64,
}

impl Default for IdentityConfig {
    fn default() -> Self {
        IdentityConfig {
            pairing_ttl_secs: 600,
            identity_required: false,
            group_gate_default: true,
            pairing_max_failures: 5,
            pairing_lockout_secs: 900,
        }
    }
}

/// 用量治理（速率限制与周期配额；维度：通道账号 / 绑定用户 / API key）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitsConfig {
    pub enabled: bool,
    /// 限流窗口（秒）
    pub window_secs: u64,
    /// 窗口内最大请求数（0 = 不限）
    pub max_requests: u32,
    /// 配额周期：daily | weekly | monthly
    pub quota_period: String,
    /// 周期 token 配额（0 = 不限；口径复用 token 台账）
    pub quota_tokens: u64,
    /// 周期请求数配额（0 = 不限）
    pub quota_requests: u32,
    /// 配额耗尽时进行中任务策略：finish = 完成当前；interrupt = 立即中断
    pub quota_running_policy: String,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        LimitsConfig {
            enabled: false,
            window_secs: 60,
            max_requests: 0,
            quota_period: "daily".into(),
            quota_tokens: 0,
            quota_requests: 0,
            quota_running_policy: "finish".into(),
        }
    }
}

/// 回收站保留契约（filesystem delete 的 .exmachina/trash/）：按保留期与容量
/// 上限自动清理，防无限膨胀拖垮磁盘。双约束并存，先到先清。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashConfig {
    /// 保留期（天）：超期日期目录在周期巡检时删除
    pub retention_days: u64,
    /// 容量上限（字节）：超限按最旧日期目录优先淘汰，直至回到上限内
    pub max_bytes: u64,
}

impl Default for TrashConfig {
    fn default() -> Self {
        TrashConfig {
            retention_days: 7,
            max_bytes: 512 * 1024 * 1024, // 512MB
        }
    }
}

/// 实验性功能开关（未稳定的行为面；默认全关，开启即自担体验波动）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(Default)]
pub struct ExperimentsConfig {
    /// 动态上下文压缩：模型获得 context_compact 工具，自行判断压缩时机
    /// （判断依据：上下文变得冗长 / 早期细节已不再关键 / 即将进入长任务阶段），
    /// 主动把指定范围历史压成滚动摘要。关闭时仍用条数阈值的自动压缩。
    #[serde(default)]
    pub dynamic_compaction: bool,
    /// 智能体组共享上下文：派发子个体时携带组主会话的滚动摘要与最近对话窗口，
    /// 子个体不再只有 objective 孤岛视角。会放大每轮派发的 token 开销。
    #[serde(default)]
    pub group_shared_context: bool,
    /// 自主记忆摘要：关闭程序每轮自动写入的会话摘要/决策/证据/教训，
    /// 改由模型经 memory_write 工具自主决定哪些内容值得记忆
    /// （治理「经常把不需要记忆的内容也保存」）。工具失败/教训统计仍照常。
    #[serde(default)]
    pub llm_driven_memory: bool,
    /// 滑动上下文窗口：关闭条数阈值触发的定时 LLM 压缩，只保留最近 N 条原文
    /// 窗口（窗口外已压过的沿用旧摘要，不再新增压缩调用）——模型持续持有
    /// 最近的完整上下文。与 dynamicCompaction 互斥：开启时 context_compact
    /// 不装配（同为压缩时机决策，避免双重压缩语义）。
    #[serde(default)]
    pub sliding_context_window: bool,
    /// Super Agent（深度委托，实验性）：会话进入「少占注意力」模式——静默长链路
    /// 推进（工具循环 + spawn 并行子代理），只在需要决策/交付结果/无法继续时
    /// 产出一条消息。工作法：尽可能少占用人注意力，尽可能多交付价值。
    #[serde(default)]
    pub super_agent: bool,
    /// Auto Agent（完全自主，实验性）：周期 Eval 门控 + 自主开工会话执行。
    /// 无人值守：只在「有人必须行动」时经通知通道说话；参数见 automation 段。
    #[serde(default)]
    pub auto_agent: bool,
    /// Meta Agent（元智能体，实验性）：运行数据聚合 → 改进提议（提示词/经验要点/
    /// 成员编成）→ [meta] 审批应用;构建新个体;轮内元认知反思。参数见 automation.metaIntervalMin
    #[serde(default)]
    pub meta_agent: bool,
    /// 自主记忆摘要（实验性）：低峰期（每日 04:30）把长会话历史归纳为记忆条目
    /// （LLM 单次摘要调用 + 内容哈希去重 + token 上限钳制）。参数见
    /// automation.memoryDigestMinTurns。默认关——关闭态零任务零 token 消耗。
    #[serde(default)]
    pub auto_memory_digest: bool,
}

// ---------------------------------------------------------------- 插件系统（plugins/ 目录）

/// MCP 服务端（把内置工具面按 ACL 暴露给外部客户端；与客户端接入段 mcpServers 解耦）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServeConfig {
    pub enabled: bool,
    /// HTTP 挂载路径（网关侧；stdio 模式不受此限）
    pub http_path: String,
    /// 允许的组清单（空 = 全部组）
    pub allowed_groups: Vec<String>,
    /// 允许的会话清单（会话亲和范围；空 = 全部会话）
    #[serde(default)]
    pub allowed_sessions: Vec<String>,
    /// 允许的工具清单（空 = 全部启用工具）
    pub allowed_tools: Vec<String>,
}

impl Default for McpServeConfig {
    fn default() -> Self {
        McpServeConfig {
            enabled: false,
            http_path: "/mcp".into(),
            allowed_groups: Vec::new(),
            allowed_sessions: Vec::new(),
            allowed_tools: Vec::new(),
        }
    }
}

/// 智能连结网络（实验性）：外部连结体清单。
/// 连结体 = 可被当作「子个体代理」调用的外部智能体：
/// - exmachina：另一台机器/进程的 EXMACHINA 网关（REST 调用）
/// - opencode / codex / claude：本机 CLI 智能体（命令行调用，其子代理即子个体）
/// - custom：完全自定义命令（提示词作为最后一个参数追加）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkTarget {
    pub id: String,
    pub name: String,
    /// exmachina | opencode | codex | claude | custom
    pub kind: String,
    /// exmachina：网关基址（http://host:4173）；custom：完整命令行
    #[serde(default)]
    pub endpoint: String,
    /// exmachina：对端 authKey
    #[serde(default)]
    pub api_key: String,
    /// CLI 覆盖：完整命令行（提示词追加为末参数）
    #[serde(default)]
    pub command: String,
    #[serde(default = "link_enabled")]
    pub enabled: bool,
}

impl Default for LinkTarget {
    fn default() -> Self {
        LinkTarget {
            id: String::new(),
            name: String::new(),
            kind: "exmachina".into(),
            endpoint: String::new(),
            api_key: String::new(),
            command: String::new(),
            enabled: true,
        }
    }
}

/// 智能连结网络配置段
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct NexusConfig {
    #[serde(default)]
    pub links: Vec<LinkTarget>,
}

/// 推送（定时任务结果通道订阅的全局口径）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyConfig {
    /// 推送总开关（逐任务仍须配置订阅；关 = 全部不推）
    pub enabled: bool,
    /// 推送摘要长度上限（字符）
    pub max_chars: usize,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        NotifyConfig {
            enabled: true,
            max_chars: 500,
        }
    }
}

/// 安装清单：记录本次安装的剖面与渠道开关（与运行配置解耦）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallManifest {
    pub installed_at: String,
    pub app_version: String,
    pub config_version: u32,
    /// quick | custom
    pub profile: String,
    pub channels: InstallChannels,
    pub features: InstallFeatures,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallChannels {
    pub cli: bool,
    pub gateway: bool,
    pub gateway_port: u16,
    pub webui: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallFeatures {
    pub memory: bool,
    pub tools: bool,
}

impl Default for InstallManifest {
    fn default() -> Self {
        InstallManifest {
            installed_at: crate::types::now_iso(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            config_version: CONFIG_VERSION,
            profile: "quick".into(),
            channels: InstallChannels {
                cli: true,
                gateway: true,
                gateway_port: 4173,
                webui: true,
            },
            features: InstallFeatures {
                memory: true,
                tools: true,
            },
            notes: vec![],
        }
    }
}

