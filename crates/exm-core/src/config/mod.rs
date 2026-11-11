//! 核心配置 —— 环境变量与 `.exmachina/config.json` 的唯一读取入口
//!
//! 可扩展性设计（避免后期重构）：
//!   1. `configVersion` 字段 + 版本迁移函数：新增字段只需追加默认值 + 迁移分支；
//!   2. `config_schema()` 输出字段描述（分组/类型/默认值/校验），
//!      CLI 安装向导与 WebUI 设置页均由该 schema 驱动渲染——新增配置项零 UI 改动；
//!   3. 安装清单 `install.json` 记录安装剖面与渠道开关，与运行配置解耦。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 配置结构版本（新增字段时 +1，并在 `migrate_file` 中补默认值）
pub const CONFIG_VERSION: u32 = 2;

// ---------------------------------------------------------------- 域分文件

mod automation;
mod exec_identity;
mod llm;
mod misc;
mod plugins;
mod security;

pub use automation::*;
pub use exec_identity::*;
pub use llm::*;
pub use misc::*;
pub use plugins::*;
pub use security::*;

#[cfg(test)]
mod tests;


fn default_true() -> bool {
    true
}

/// 单智能体循环默认步数上限（与既有硬编码 24 保持一致）
fn default_chat_max_steps() -> usize {
    24
}

/// 上下文预算安全线默认值
fn default_context_budget_ratio() -> f64 {
    0.8
}

fn default_browser_engine() -> String {
    "bing".into()
}

fn default_memory_digest_min_turns() -> usize {
    20
}

fn default_pairing_max_failures() -> u32 {
    5
}

fn default_pairing_lockout_secs() -> u64 {
    900
}

fn default_watch_interval() -> u64 {
    10
}

fn default_handshake_secs() -> u64 {
    15
}
fn default_call_timeout_secs() -> u64 {
    60
}
fn default_plugin_concurrency() -> usize {
    4
}

fn link_enabled() -> bool {
    true
}

#[derive(Debug, Clone)]
pub struct ExmConfig {
    pub config_version: u32,
    pub workspace_root: PathBuf,
    /// 数据根目录（会话/任务/证据/事件/记忆的文档存储）
    pub data_dir: PathBuf,
    pub config_path: PathBuf,
    pub memory_md_path: PathBuf,
    pub agents_dir: PathBuf,
    /// WebUI 构建产物目录（Gateway 静态托管；不存在则仅提供 API）
    pub webui_dist: PathBuf,
    pub llm: LlmConfig,
    pub max_concurrency: usize,
    /// 会话 token 预算（估算：字符/4；0 = 不限；超出后拒绝新轮次）
    pub max_session_tokens: u64,
    /// memory.md 字数上限（超限触发 AI 自主压缩，旧内容归档）
    pub memory_md_max_chars: usize,
    /// 记忆系统开关与参数
    pub memory_enabled: bool,
    pub memory_recall_limit: usize,
    pub memory_half_life_days: f64,
    /// 语义检索目标："档案ID" 或 "档案ID/模型名"；空 = 仅词项召回
    pub memory_semantic_model: String,
    /// 能力模型槽位（"档案ID" 或 "档案ID/模型名"）：
    /// 语音合成（TTS）；空 = 全局档案默认 tts-1
    pub speech_model: String,
    /// 语音识别 / 语音转述（STT）；空 = 全局档案默认 whisper-1
    pub stt_model: String,
    /// 视觉转述：生效模型未标记视觉能力时，用它把图片转成文字描述；空 = 不转述
    pub vision_relay_model: String,
    /// 测试替身通道（仅 `EXM_LLM_MOCK=1` 或测试代码置位；产品运行时不生效）
    pub use_mock: bool,
    /// 执行审批（安全闸门）
    pub security: SecurityConfig,
    /// Computer Use（截屏 + 鼠标键盘控制；默认关闭）
    pub computer: ComputerUseConfig,
    /// 自动化（心跳巡检）
    pub automation: AutomationConfig,
    /// 联网搜索后端（web_search 工具；未配置则不下发该工具）
    pub search: SearchConfig,
    /// 生命周期钩子（pre/post tool、run 收束）
    pub hooks: HooksConfig,
    /// 工具面（声明式自定义工具）
    pub tools: ToolsConfig,
    /// 沙箱执行（子进程环境净化 / bubblewrap / strict 闸门）
    pub sandbox: SandboxConfig,
    /// 浏览器自动化（browser 工具）
    pub browser: BrowserConfig,
    /// 厂商模型档案与当前生效档案（llm 字段 = 生效档案的解析结果）
    pub llm_profiles: Vec<LlmProfile>,
    pub active_profile: String,
    /// 界面与提示词语言（EXM_LANG，zh/en）：en 时优先装载 {stem}.{lang}.md 提示词变体
    pub language: String,
    /// MCP 服务器（第三方工具生态，docs/架构与设计.md 扩展点）
    pub mcp_servers: Vec<crate::mcp::McpServerConfig>,
    /// 事件触发器（文件监听 / webhook 事件源）
    pub triggers: TriggersConfig,
    /// 通道身份（配对绑定与群聊唤醒门控）
    pub identity: IdentityConfig,
    /// 用量治理（速率限制与周期配额）
    pub limits: LimitsConfig,
    /// 回收站保留契约（保留期 + 容量上限自动清理）
    pub trash: TrashConfig,
    /// MCP 服务端（工具面对外暴露；默认关闭）
    pub mcp_serve: McpServeConfig,
    /// 推送（定时任务结果通道订阅）
    pub notify: NotifyConfig,
    /// 智能连结网络（实验性）
    pub nexus: NexusConfig,
    /// 实验性功能开关（动态压缩 / 组共享上下文等；默认全关）
    pub experiments: ExperimentsConfig,
    /// 插件系统（plugins/ 目录装载；state 持久化启停覆盖）
    pub plugins: PluginsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ConfigFile {
    #[serde(default)]
    config_version: Option<u32>,
    #[serde(default)]
    llm: Option<LlmConfigFile>,
    #[serde(default)]
    max_concurrency: Option<usize>,
    #[serde(default)]
    max_session_tokens: Option<u64>,
    #[serde(default)]
    memory_md_max_chars: Option<usize>,
    #[serde(default)]
    memory: Option<MemoryConfigFile>,
    #[serde(default)]
    security: Option<SecurityFile>,
    #[serde(default)]
    computer: Option<ComputerUseConfig>,
    #[serde(default)]
    automation: Option<AutomationFile>,
    #[serde(default)]
    llm_profiles: Option<Vec<LlmProfileFile>>,
    #[serde(default)]
    active_profile: Option<String>,
    #[serde(default)]
    capabilities: Option<CapabilitiesFile>,
    #[serde(default)]
    search: Option<SearchConfig>,
    #[serde(default)]
    hooks: Option<HooksConfig>,
    #[serde(default)]
    sandbox: Option<SandboxConfig>,
    #[serde(default)]
    browser: Option<BrowserConfig>,
    #[serde(default)]
    tools: Option<ToolsConfig>,
    #[serde(default)]
    mcp_servers: Option<Vec<crate::mcp::McpServerConfig>>,
    #[serde(default)]
    triggers: Option<TriggersConfig>,
    #[serde(default)]
    identity: Option<IdentityConfig>,
    #[serde(default)]
    limits: Option<LimitsConfig>,
    #[serde(default)]
    trash: Option<TrashConfig>,
    #[serde(default)]
    mcp_serve: Option<McpServeConfig>,
    #[serde(default)]
    notify: Option<NotifyConfig>,
    #[serde(default)]
    nexus: Option<NexusConfig>,
    #[serde(default)]
    experiments: Option<ExperimentsConfig>,
    #[serde(default)]
    plugins: Option<PluginsConfig>,
}

/// 能力模型槽位（模型设置页配置）：语音合成 / 语音识别 / 视觉转述。
/// 嵌入（语义检索）沿用 memory.semanticModel，不在此重复。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CapabilitiesFile {
    #[serde(default)]
    speech: Option<String>,
    #[serde(default)]
    transcribe: Option<String>,
    #[serde(default)]
    vision_relay: Option<String>,
}

/// 档案的读取形态：兼容历史上区分「指挥体模型 / 子个体模型」的旧配置。
/// 两者合并为单一 `model`（优先取指挥体模型，其次子个体模型）；旧键解析后不再回写。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LlmProfileFile {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    api_keys: Vec<String>,
    #[serde(default)]
    api_format: String,
    /// 现役：该提供商的默认模型名
    #[serde(default)]
    model: String,
    /// 历史遗留：指挥体模型（迁移来源）
    #[serde(default)]
    orch_model: Option<String>,
    /// 历史遗留：子个体模型（迁移兜底）
    #[serde(default)]
    unit_model: Option<String>,
    #[serde(default)]
    fallback: Option<String>,
    /// 模型清单（模型名 + 视觉/语音能力开关）
    #[serde(default)]
    models: Option<Vec<ModelEntry>>,
    /// 历史遗留：档案级嵌入模型（现由「记忆」页的语义检索设置管理）
    #[serde(default)]
    embed_model: Option<String>,
}

impl LlmProfileFile {
    /// 补默认值并完成历史字段迁移
    fn into_profile(self, fallback_model: &str) -> LlmProfile {
        let model = if !self.model.trim().is_empty() {
            self.model.trim().to_string()
        } else if let Some(m) = self
            .orch_model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            m.to_string()
        } else if let Some(m) = self
            .unit_model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            m.to_string()
        } else {
            fallback_model.to_string()
        };
        LlmProfile {
            id: self.id,
            name: self.name,
            base_url: self.base_url,
            api_key: self.api_key,
            api_keys: self.api_keys,
            api_format: self.api_format,
            model,
            models: self
                .models
                .unwrap_or_default()
                .into_iter()
                .filter(|m| !m.model.trim().is_empty())
                .collect(),
            fallback: self.fallback,
        }
    }
}

/// LLM 段历史值：旧配置区分指挥体 / 子个体模型，合并为单一 `model`
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LlmConfigFile {
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    orch_model: Option<String>,
    #[serde(default)]
    unit_model: Option<String>,
}

impl LlmConfigFile {
    fn into_config(self) -> LlmConfig {
        let model = if !self.model.trim().is_empty() {
            self.model.trim().to_string()
        } else if let Some(m) = self
            .orch_model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            m.to_string()
        } else if let Some(m) = self
            .unit_model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            m.to_string()
        } else {
            String::new()
        };
        LlmConfig {
            base_url: self.base_url,
            api_key: self.api_key,
            api_keys: Vec::new(),
            api_format: String::new(),
            model,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SecurityFile {
    #[serde(default)]
    exec_approval: Option<String>,
    #[serde(default)]
    exec_allowlist: Option<Vec<String>>,
    #[serde(default)]
    auth_key: Option<String>,
    #[serde(default)]
    terminal_timeout_secs: Option<u32>,
    #[serde(default)]
    tool_output_spill_chars: Option<usize>,
    #[serde(default)]
    approval_wait_secs: Option<u64>,
    #[serde(default)]
    chat_max_steps: Option<usize>,
    #[serde(default)]
    context_budget_ratio: Option<f64>,
    #[serde(default)]
    key_setup_dismissed: Option<bool>,
    #[serde(default)]
    sanitize_rules: Option<SanitizeRuleConfig>,
    #[serde(default)]
    exec_user: Option<String>,
    #[serde(default)]
    exec_home: Option<String>,
    #[serde(default)]
    tool_call_budget: Option<ToolCallBudget>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AutomationFile {
    #[serde(default)]
    heartbeat_enabled: Option<bool>,
    #[serde(default)]
    heartbeat_interval_minutes: Option<u32>,
    #[serde(default)]
    heartbeat_prompt: Option<String>,
    #[serde(default)]
    heartbeat_agent: Option<String>,
    #[serde(default)]
    auto_adapt: Option<bool>,
    #[serde(default)]
    unit_max_steps: Option<usize>,
    #[serde(default)]
    auto_agent_interval_minutes: Option<u32>,
    #[serde(default)]
    auto_agent_max_steps: Option<usize>,
    #[serde(default)]
    auto_agent_backoff_max: Option<u32>,
    #[serde(default)]
    meta_interval_min: Option<u32>,
    #[serde(default)]
    flow_idle_days: Option<u32>,
    #[serde(default)]
    memory_digest_min_turns: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct MemoryConfigFile {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    recall_limit: Option<usize>,
    #[serde(default)]
    half_life_days: Option<f64>,
    /// 语义检索目标："档案ID" 或 "档案ID/模型名"；空 = 仅词项召回
    #[serde(default)]
    semantic_model: Option<String>,
}

/// 半衰期清洗：必须为有限正数（天），非法值（0 / 负数 / NaN / ∞）回落默认 30 天。
/// 为什么在配置层清洗而不只靠 decay() 内部兜底：设置页与 /api/config 回显的是配置值，
/// decay 内部会把非法半衰期按 1 天处理——「显示 0 天、实际按 1 天衰减」属于所见非所跑，
/// 展示与行为必须同源。
fn sanitize_half_life_days(v: Option<f64>) -> f64 {
    match v {
        Some(x) if x.is_finite() && x > 0.0 => x,
        _ => 30.0,
    }
}

/// 测试替身开关：仅 `EXM_LLM_MOCK=1|true|yes` 时启用（单测/端到端验收/无网联调流程）。
/// 产品运行时**不存在**"无密钥自动降级"这一类行为——未配置模型即明确报错并引导配置。
pub fn mock_enabled_from_env() -> bool {
    matches!(
        std::env::var("EXM_LLM_MOCK")
            .unwrap_or_default()
            .to_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

impl ExmConfig {
    /// 语言（仅供低层组件读取，避免整份配置依赖）：EXM_LANG，默认 zh
    pub fn load_language() -> String {
        std::env::var("EXM_LANG")
            .map(|v| if v.is_empty() { "zh".into() } else { v })
            .unwrap_or_else(|_| "zh".into())
    }

    pub fn state_dir(workspace_root: impl AsRef<Path>) -> PathBuf {
        workspace_root.as_ref().join(".exmachina")
    }

    pub fn install_manifest_path(&self) -> PathBuf {
        self.config_path
            .parent()
            .map(|d| d.join("install.json"))
            .unwrap_or_else(|| PathBuf::from("install.json"))
    }

    pub fn load(workspace_root: impl AsRef<Path>) -> Self {
        let root = workspace_root.as_ref().to_path_buf();
        let dir = Self::state_dir(&root);
        let _ = std::fs::create_dir_all(&dir);
        let config_path = dir.join("config.json");

        let file = read_config_file(&config_path);
        let file = migrate_file(file, &config_path);

        let file_llm = file.llm.clone().unwrap_or_default().into_config();
        let file_mem = file.memory.clone().unwrap_or_default();
        let file_sec = file.security.clone().unwrap_or_default();
        let file_auto = file.automation.clone().unwrap_or_default();
        let security = SecurityConfig {
            exec_approval: file_sec
                .exec_approval
                .clone()
                .unwrap_or_else(|| SecurityConfig::default().exec_approval),
            exec_allowlist: file_sec.exec_allowlist.clone().unwrap_or_default(),
            auth_key: std::env::var("EXM_AUTH_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| file_sec.auth_key.clone().unwrap_or_default()),
            terminal_timeout_secs: file_sec
                .terminal_timeout_secs
                .unwrap_or_else(|| SecurityConfig::default().terminal_timeout_secs),
            tool_output_spill_chars: file_sec
                .tool_output_spill_chars
                .unwrap_or_else(|| SecurityConfig::default().tool_output_spill_chars),
            approval_wait_secs: file_sec
                .approval_wait_secs
                .unwrap_or_else(|| SecurityConfig::default().approval_wait_secs),
            chat_max_steps: file_sec
                .chat_max_steps
                .filter(|v| *v > 0)
                .unwrap_or_else(|| SecurityConfig::default().chat_max_steps),
            context_budget_ratio: file_sec
                .context_budget_ratio
                .filter(|v| (0.5..=1.0).contains(v))
                .unwrap_or_else(|| SecurityConfig::default().context_budget_ratio),
            key_setup_dismissed: file_sec.key_setup_dismissed.unwrap_or(false),
            sanitize_rules: file_sec.sanitize_rules.clone().unwrap_or_default(),
            exec_user: file_sec.exec_user.clone().unwrap_or_default(),
            exec_home: file_sec.exec_home.clone().unwrap_or_default(),
            tool_call_budget: file_sec
                .tool_call_budget
                .unwrap_or_else(|| SecurityConfig::default().tool_call_budget),
        };
        let automation = AutomationConfig {
            heartbeat_enabled: file_auto
                .heartbeat_enabled
                .unwrap_or_else(|| AutomationConfig::default().heartbeat_enabled),
            heartbeat_interval_minutes: file_auto
                .heartbeat_interval_minutes
                .unwrap_or_else(|| AutomationConfig::default().heartbeat_interval_minutes),
            heartbeat_prompt: file_auto
                .heartbeat_prompt
                .clone()
                .unwrap_or_else(|| AutomationConfig::default().heartbeat_prompt),
            heartbeat_agent: file_auto
                .heartbeat_agent
                .clone()
                .filter(|s| !s.trim().is_empty()),
            auto_adapt: file_auto
                .auto_adapt
                .unwrap_or_else(|| AutomationConfig::default().auto_adapt),
            unit_max_steps: file_auto
                .unit_max_steps
                .unwrap_or_else(|| AutomationConfig::default().unit_max_steps),
            auto_agent_interval_minutes: file_auto
                .auto_agent_interval_minutes
                .unwrap_or_else(|| AutomationConfig::default().auto_agent_interval_minutes),
            auto_agent_max_steps: file_auto
                .auto_agent_max_steps
                .unwrap_or_else(|| AutomationConfig::default().auto_agent_max_steps),
            auto_agent_backoff_max: file_auto
                .auto_agent_backoff_max
                .unwrap_or_else(|| AutomationConfig::default().auto_agent_backoff_max),
            meta_interval_min: file_auto
                .meta_interval_min
                .unwrap_or_else(|| AutomationConfig::default().meta_interval_min),
            flow_idle_days: file_auto
                .flow_idle_days
                .unwrap_or_else(|| AutomationConfig::default().flow_idle_days),
            memory_digest_min_turns: file_auto
                .memory_digest_min_turns
                .unwrap_or_else(|| AutomationConfig::default().memory_digest_min_turns),
        };

        let mut llm = LlmConfig {
            base_url: env_or(
                "EXM_LLM_BASE_URL",
                &file_llm.base_url,
                "https://api.openai.com/v1",
            ),
            api_key: env_or("EXM_LLM_API_KEY", &file_llm.api_key, ""),
            api_keys: Vec::new(),
            api_format: String::new(),
            model: env_or("EXM_LLM_MODEL", &file_llm.model, ""),
        };

        // 模型档案：旧配置自动迁移为单一 default 档案；激活档案的值解析进 llm（env 仍最高优先）
        let mut llm_profiles: Vec<LlmProfile> = file
            .llm_profiles
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|p| p.into_profile(&llm.model))
            .collect();
        let active_profile = file
            .active_profile
            .clone()
            .unwrap_or_else(|| "default".into());
        if llm_profiles.is_empty() {
            llm_profiles.push(LlmProfile {
                id: "default".into(),
                name: "默认".into(),
                base_url: llm.base_url.clone(),
                api_key: llm.api_key.clone(),
                api_keys: Vec::new(),
                api_format: String::new(),
                model: llm.model.clone(),
                models: Vec::new(),
                fallback: None,
            });
        }
        if !llm_profiles.iter().any(|p| p.id == active_profile) {
            if let Some(first) = llm_profiles.first() {
                llm = LlmConfig {
                    base_url: env_or(
                        "EXM_LLM_BASE_URL",
                        &first.base_url,
                        "https://api.openai.com/v1",
                    ),
                    api_key: env_or("EXM_LLM_API_KEY", &first.api_key, ""),
                    api_keys: Vec::new(),
                    api_format: String::new(),
                    model: env_or("EXM_LLM_MODEL", &first.model, ""),
                };
            }
        } else if let Some(p) = llm_profiles.iter().find(|p| p.id == active_profile) {
            let mut keys: Vec<String> = p
                .api_keys
                .clone()
                .into_iter()
                .filter(|k| !k.trim().is_empty())
                .collect();
            if keys.is_empty() && !p.api_key.trim().is_empty() {
                keys.push(p.api_key.clone());
            }
            llm = LlmConfig {
                base_url: env_or("EXM_LLM_BASE_URL", &p.base_url, "https://api.openai.com/v1"),
                api_key: env_or("EXM_LLM_API_KEY", &p.api_key, ""),
                api_keys: keys,
                api_format: p.api_format.clone(),
                model: env_or("EXM_LLM_MODEL", &p.model, ""),
            };
        }

        let data_dir = std::env::var("EXM_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| dir.join("data"));
        let file_cap = file.capabilities.clone().unwrap_or_default();
        let agents_dir = std::env::var("EXM_AGENTS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| root.join("entities"));
        let webui_dist = std::env::var("EXM_WEBUI_DIST")
            .map(PathBuf::from)
            .unwrap_or_else(|_| root.join("webui").join("dist"));

        // 无密钥**不再**自动降级为模拟通道：未配置模型就是未配置，由界面/CLI 明确引导。
        // 测试替身仅在显式设置环境变量时启用（见 docs/架构与设计.md「测试策略」）。
        let use_mock = mock_enabled_from_env();

        ExmConfig {
            config_version: CONFIG_VERSION,
            workspace_root: root,
            data_dir,
            config_path,
            memory_md_path: dir.join("memory.md"),
            agents_dir,
            webui_dist,
            llm,
            max_concurrency: file.max_concurrency.unwrap_or(4),
            max_session_tokens: file.max_session_tokens.unwrap_or(0),
            memory_md_max_chars: file.memory_md_max_chars.unwrap_or(5000),
            memory_enabled: file_mem.enabled.unwrap_or(true),
            memory_recall_limit: file_mem.recall_limit.unwrap_or(5),
            memory_half_life_days: sanitize_half_life_days(file_mem.half_life_days),
            memory_semantic_model: file_mem.semantic_model.clone().unwrap_or_default(),
            speech_model: file_cap.speech.unwrap_or_default(),
            stt_model: file_cap.transcribe.unwrap_or_default(),
            vision_relay_model: file_cap.vision_relay.unwrap_or_default(),
            use_mock,
            security,
            computer: {
                let mut c = file.computer.clone().unwrap_or_default();
                // 环境变量直开（自动化部署）：EXM_COMPUTER_USE=1 强制启用
                if std::env::var("EXM_COMPUTER_USE")
                    .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
                    .unwrap_or(false)
                {
                    c.enabled = true;
                }
                c
            },
            automation,
            search: file.search.clone().unwrap_or_default(),
            hooks: file.hooks.clone().unwrap_or_default(),
            sandbox: file.sandbox.clone().unwrap_or_default(),
            browser: file.browser.clone().unwrap_or_default(),
            tools: {
                let mut t = file.tools.clone().unwrap_or_default();
                // 声明式工具的合法性兜底：名字不合法或 target 为空的条目直接丢弃（不承诺坏配置）
                t.custom
                    .retain(|c| c.valid() && !c.target.trim().is_empty());
                t
            },
            llm_profiles,
            active_profile,
            language: std::env::var("EXM_LANG").unwrap_or_else(|_| "zh".into()),
            mcp_servers: file.mcp_servers.unwrap_or_default(),
            triggers: file.triggers.clone().unwrap_or_default(),
            identity: file.identity.clone().unwrap_or_default(),
            limits: file.limits.clone().unwrap_or_default(),
            trash: file.trash.clone().unwrap_or_default(),
            mcp_serve: file.mcp_serve.clone().unwrap_or_default(),
            notify: file.notify.clone().unwrap_or_default(),
            nexus: file.nexus.clone().unwrap_or_default(),
            experiments: file.experiments.clone().unwrap_or_default(),
            plugins: file.plugins.clone().unwrap_or_default(),
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let file = ConfigFile {
            config_version: Some(CONFIG_VERSION),
            llm: Some(LlmConfigFile {
                base_url: self.llm.base_url.clone(),
                api_key: self.llm.api_key.clone(),
                model: self.llm.model.clone(),
                orch_model: None,
                unit_model: None,
            }),
            max_concurrency: Some(self.max_concurrency),
            max_session_tokens: Some(self.max_session_tokens),
            memory_md_max_chars: Some(self.memory_md_max_chars),
            memory: Some(MemoryConfigFile {
                enabled: Some(self.memory_enabled),
                recall_limit: Some(self.memory_recall_limit),
                half_life_days: Some(self.memory_half_life_days),
                semantic_model: Some(self.memory_semantic_model.clone()),
            }),
            llm_profiles: Some(
                self.llm_profiles
                    .iter()
                    .map(|p| LlmProfileFile {
                        id: p.id.clone(),
                        name: p.name.clone(),
                        base_url: p.base_url.clone(),
                        api_key: p.api_key.clone(),
                        api_keys: p.api_keys.clone(),
                        api_format: p.api_format.clone(),
                        model: p.model.clone(),
                        models: Some(p.models.clone()),
                        orch_model: None,
                        unit_model: None,
                        fallback: p.fallback.clone(),
                        embed_model: None,
                    })
                    .collect(),
            ),
            active_profile: Some(self.active_profile.clone()),
            capabilities: Some(CapabilitiesFile {
                speech: Some(self.speech_model.clone()),
                transcribe: Some(self.stt_model.clone()),
                vision_relay: Some(self.vision_relay_model.clone()),
            }),
            mcp_servers: Some(self.mcp_servers.clone()),
            security: Some(SecurityFile {
                exec_approval: Some(self.security.exec_approval.clone()),
                exec_allowlist: Some(self.security.exec_allowlist.clone()),
                auth_key: Some(self.security.auth_key.clone()),
                terminal_timeout_secs: Some(self.security.terminal_timeout_secs),
                tool_output_spill_chars: Some(self.security.tool_output_spill_chars),
                approval_wait_secs: Some(self.security.approval_wait_secs),
                chat_max_steps: Some(self.security.chat_max_steps),
                context_budget_ratio: Some(self.security.context_budget_ratio),
                key_setup_dismissed: Some(self.security.key_setup_dismissed),
                sanitize_rules: Some(self.security.sanitize_rules.clone()),
                exec_user: Some(self.security.exec_user.clone()),
                exec_home: Some(self.security.exec_home.clone()),
                tool_call_budget: Some(self.security.tool_call_budget),
            }),
            computer: Some(self.computer.clone()),
            automation: Some(AutomationFile {
                heartbeat_enabled: Some(self.automation.heartbeat_enabled),
                heartbeat_interval_minutes: Some(self.automation.heartbeat_interval_minutes),
                heartbeat_prompt: Some(self.automation.heartbeat_prompt.clone()),
                heartbeat_agent: self.automation.heartbeat_agent.clone(),
                auto_adapt: Some(self.automation.auto_adapt),
                unit_max_steps: Some(self.automation.unit_max_steps),
                auto_agent_interval_minutes: Some(self.automation.auto_agent_interval_minutes),
                auto_agent_max_steps: Some(self.automation.auto_agent_max_steps),
                auto_agent_backoff_max: Some(self.automation.auto_agent_backoff_max),
                meta_interval_min: Some(self.automation.meta_interval_min),
                flow_idle_days: Some(self.automation.flow_idle_days),
                memory_digest_min_turns: Some(self.automation.memory_digest_min_turns),
            }),
            search: Some(self.search.clone()),
            hooks: Some(self.hooks.clone()),
            sandbox: Some(self.sandbox.clone()),
            browser: Some(self.browser.clone()),
            tools: Some(self.tools.clone()),
            triggers: Some(self.triggers.clone()),
            identity: Some(self.identity.clone()),
            limits: Some(self.limits.clone()),
            trash: Some(self.trash.clone()),
            mcp_serve: Some(self.mcp_serve.clone()),
            notify: Some(self.notify.clone()),
            nexus: Some(self.nexus.clone()),
            experiments: Some(self.experiments.clone()),
            plugins: Some(self.plugins.clone()),
        };
        if let Some(parent) = self.config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.config_path, serde_json::to_string_pretty(&file)?)?;
        Ok(())
    }

    pub fn load_manifest(&self) -> Option<InstallManifest> {
        std::fs::read_to_string(self.install_manifest_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    pub fn save_manifest(&self, manifest: &InstallManifest) -> anyhow::Result<()> {
        let p = self.install_manifest_path();
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(p, serde_json::to_string_pretty(manifest)?)?;
        Ok(())
    }
}

fn read_config_file(path: &Path) -> ConfigFile {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 版本迁移：低版本文件补齐新字段默认值后回写
fn migrate_file(mut file: ConfigFile, path: &Path) -> ConfigFile {
    let version = file.config_version.unwrap_or(0);
    if version >= CONFIG_VERSION {
        return file;
    }
    if file.llm.is_none() {
        file.llm = Some(LlmConfigFile::default());
    }
    if file.memory.is_none() {
        file.memory = Some(MemoryConfigFile {
            enabled: Some(true),
            recall_limit: Some(5),
            half_life_days: Some(30.0),
            semantic_model: None,
        });
    }
    if file.max_concurrency.is_none() {
        file.max_concurrency = Some(4);
    }
    // v<2：补齐六簇能力配置段。
    // v0 = 全新初始化（无配置文件）：门控走 Default（默认开）；
    // v1 = 既有安装升级：群聊门控显式置关（保持升级前行为）——迁移语义与全新语义在此分流。
    if version < 2 {
        if file.triggers.is_none() {
            file.triggers = Some(TriggersConfig::default());
        }
        if file.identity.is_none() {
            let gate_on = version == 0;
            file.identity = Some(IdentityConfig {
                group_gate_default: gate_on,
                ..IdentityConfig::default()
            });
        }
        if file.limits.is_none() {
            file.limits = Some(LimitsConfig::default());
        }
        if file.mcp_serve.is_none() {
            file.mcp_serve = Some(McpServeConfig::default());
        }
        if file.notify.is_none() {
            file.notify = Some(NotifyConfig::default());
        }
    }
    file.config_version = Some(CONFIG_VERSION);
    if let Ok(s) = serde_json::to_string_pretty(&file) {
        let _ = std::fs::write(path, s);
    }
    file
}

fn env_or(key: &str, file_value: &str, default: &str) -> String {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => {
            if !file_value.trim().is_empty() {
                file_value.to_string()
            } else {
                default.to_string()
            }
        }
    }
}

// ---------------------------------------------------------------- 配置 Schema（UI/向导驱动）


// ---------------------------------------------------------------- Agent 执行身份约束（Linux）

/// execUser 解析结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecIdentity {
    pub uid: u32,
    pub gid: u32,
    /// 圈禁根目录（绝对路径）
    pub home: String,
}

