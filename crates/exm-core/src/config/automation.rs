//! config 域分文件：自动化与触发（Automation/Search/Hooks/FileWatch/EventWebhook/Triggers）。

use super::*;
use serde::{Deserialize, Serialize};

/// 联网搜索单后端声明（search-aggregation：多后端并发聚合）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchBackend {
    /// tavily | brave | searxng | custom
    pub provider: String,
    /// 端点（brave/custom 必填；tavily 留空用官方端点）
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub api_key: String,
    /// 合排权重（≥0；同 URL 多源时来源并列、位置取权重高者）
    #[serde(default = "default_backend_weight")]
    pub weight: f32,
}

fn default_backend_weight() -> f32 {
    1.0
}

impl SearchBackend {
    /// 是否已具备可用配置（provider 与所需凭据齐全）
    pub fn ready(&self) -> bool {
        match self.provider.trim().to_lowercase().as_str() {
            "tavily" => !self.api_key.trim().is_empty(),
            "brave" => !self.api_key.trim().is_empty(),
            "searxng" => !self.endpoint.trim().is_empty(),
            "custom" => !self.endpoint.trim().is_empty(),
            _ => false,
        }
    }
}

/// 联网搜索配置（web_search 工具的真实后端）。
/// **未配置时不向模型下发该工具** —— 不承诺不存在的能力。
///
/// 多后端（search-aggregation）：`backends[]` 数组形态；旧单后端平铺字段
/// （provider/apiKey/endpoint）保留 serde 兼容——反序列化时 backends 为空且
/// 旧字段非空则自动包装为单元素数组（内存态归一，落盘走新形态）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchConfig {
    /// 后端清单（多后端并发聚合；单后端 = 旧形态等价）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backends: Vec<SearchBackend>,
    /// —— 旧单后端平铺字段（迁移源；读取兼容，写入走 backends）——
    /// 后端：tavily | brave | searxng | custom；空 = 未配置
    #[serde(default = "default_legacy_provider")]
    pub provider: String,
    /// 端点（brave/custom 必填；tavily 留空用官方端点）
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub api_key: String,
    /// 返回条数上限
    #[serde(default = "default_max_results")]
    pub max_results: usize,
    /// includeContent 抓取正文的结果条数上限（前 N 条并发抓；上限 5）
    #[serde(default = "default_content_fetch_n")]
    pub content_fetch_n: usize,
    /// 主动搜索纪律（proactive-search）：true = 系统提示注入「何时该主动搜」
    /// 行为准则（时效性问题/事实核验/未知概念先搜再答）。仅提示层——
    /// 工具下发面由 ready() 决定，与本开关无关。
    #[serde(default = "default_true")]
    pub proactive: bool,
}

fn default_legacy_provider() -> String {
    String::new()
}

fn default_max_results() -> usize {
    5
}

fn default_content_fetch_n() -> usize {
    3
}

impl Default for SearchConfig {
    fn default() -> Self {
        SearchConfig {
            backends: Vec::new(),
            provider: String::new(),
            endpoint: String::new(),
            api_key: String::new(),
            max_results: 5,
            content_fetch_n: 3,
            proactive: true,
        }
    }
}

impl SearchConfig {
    /// 归一化的后端清单（迁移语义：backends 空 + 旧平铺字段有效 → 单元素数组）
    pub fn effective_backends(&self) -> Vec<SearchBackend> {
        if !self.backends.is_empty() {
            return self.backends.clone();
        }
        let legacy = SearchBackend {
            provider: self.provider.clone(),
            endpoint: self.endpoint.clone(),
            api_key: self.api_key.clone(),
            weight: 1.0,
        };
        if !legacy.provider.trim().is_empty() {
            vec![legacy]
        } else {
            Vec::new()
        }
    }

    /// 是否已具备可用配置（任一后端凭据齐全即下发工具）
    pub fn ready(&self) -> bool {
        self.effective_backends().iter().any(|b| b.ready())
    }
}

/// 生命周期钩子：可执行命令列表；占位符/上下文经环境变量注入。
/// preTool 非零退出 = **拦截**该次工具调用（返回拦截原因给模型）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HooksConfig {
    #[serde(default)]
    pub pre_tool: Vec<String>,
    #[serde(default)]
    pub post_tool: Vec<String>,
    /// 一轮 run 收束（或失败）时触发
    #[serde(default)]
    pub on_run_end: Vec<String>,
}

impl HooksConfig {
    pub fn is_empty(&self) -> bool {
        self.pre_tool.is_empty() && self.post_tool.is_empty() && self.on_run_end.is_empty()
    }
}

/// 自动化配置：心跳巡检（docs/10）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationConfig {
    pub heartbeat_enabled: bool,
    pub heartbeat_interval_minutes: u32,
    pub heartbeat_prompt: String,
    /// 心跳目标个体（per-agent 粒度）：以其所属组的身份巡检，产出进其组记忆体系；
    /// 空/个体不存在 = 维持全局巡检（激活组 + 全局心跳会话）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat_agent: Option<String>,
    /// 新教训达阈值时自动优化相关子个体（经验改进要点）
    pub auto_adapt: bool,
    /// 子个体单次派发的最大工具步数（主流的 20-50 步为长任务口径，取中档）
    pub unit_max_steps: usize,
    /// Auto Agent 周期（分钟，experiments.autoAgent 开启才生效）：
    /// 每次 tick 由 agent 自评「有没有值得做的事」——不是每次都执行任务
    pub auto_agent_interval_minutes: u32,
    /// Auto Agent 单次自主行动的工具步数预算（防失控；0 = 用 unit_max_steps）
    pub auto_agent_max_steps: usize,
    /// Auto Agent 连续「无事可做」的冷却倍数：连续 N 次无任务后间隔 × N（防空转），
    /// 上限 8 倍；有任务即复位
    pub auto_agent_backoff_max: u32,
    /// Meta Agent 巡检周期（分钟，experiments.metaAgent 开启才生效;0 = 仅会话召唤）
    pub meta_interval_min: u32,
    /// 通道流会话闲置归档天数（session-management）：闲置超过该天数的流会话
    /// 自动归档（活跃绑定会话豁免）。0 = 关闭（默认，零行为变更）。
    #[serde(default)]
    pub flow_idle_days: u32,
    /// 自主记忆摘要触发轮数下限（experiments.autoMemoryDigest 开启才生效）：
    /// 会话消息数 ≥ 该值才进入低峰归纳。默认 20。
    #[serde(default = "default_memory_digest_min_turns")]
    pub memory_digest_min_turns: usize,
}

impl Default for AutomationConfig {
    fn default() -> Self {
        AutomationConfig {
            heartbeat_enabled: false,
            heartbeat_interval_minutes: 30,
            heartbeat_prompt:
                "系统心跳巡检：检查未决任务、受阻节点与风险账，无事项则简短报告正常。".into(),
            heartbeat_agent: None,
            auto_adapt: true,
            unit_max_steps: 16,
            auto_agent_interval_minutes: 30,
            auto_agent_max_steps: 24,
            auto_agent_backoff_max: 8,
            meta_interval_min: 60,
            flow_idle_days: 0,
            memory_digest_min_turns: default_memory_digest_min_turns(),
        }
    }
}

/// 文件监听触发器（事件驱动主动行为）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileWatchConfig {
    pub enabled: bool,
    /// 去抖窗口（毫秒）：窗口内同路径连续变更合并为一次事件注入
    pub debounce_ms: u64,
    /// 排除的路径前缀（相对工作区；命中即不监听）
    pub exclude: Vec<String>,
}

impl Default for FileWatchConfig {
    fn default() -> Self {
        FileWatchConfig {
            enabled: false,
            debounce_ms: 1000,
            exclude: vec![
                ".exmachina/".into(),
                "target/".into(),
                "node_modules/".into(),
            ],
        }
    }
}

/// 事件 webhook 源（通用事件入口；独立于通道 webhook）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EventWebhookConfig {
    pub enabled: bool,
    /// HMAC 签名密钥；空 = 未配置（入口关闭）
    pub secret: String,
}

/// 触发器总段（事件驱动主动行为）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TriggersConfig {
    pub file_watch: FileWatchConfig,
    pub event_webhook: EventWebhookConfig,
}

