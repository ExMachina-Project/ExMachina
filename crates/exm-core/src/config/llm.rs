//! config 域分文件：LLM 配置（ModelEntry/LlmProfile/LlmConfig）。

use serde::{Deserialize, Serialize};

/// 提供商下的模型条目：能力开关让平台知道该模型支持哪些多模态输入。
/// 模型未列入清单（或档案无清单）视为能力未知，保持直通行为。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelEntry {
    /// 模型名（API 的 model 参数）
    pub model: String,
    /// 支持视觉（图片输入）
    #[serde(default)]
    pub vision: bool,
    /// 支持音频（语音输入）
    #[serde(default)]
    pub audio: bool,
    /// 上下文窗口（token 数；0/缺省 = 未知，预算软着陆跳过判定）
    #[serde(default)]
    pub context_window: u64,
}

/// 厂商模型档案：一条 OpenAI 兼容端点（OpenAI/DeepSeek/通义/Kimi/智谱/Ollama/vLLM …）。
/// 多档案可并存，`active_profile` 决定当前生效者；切换即热生效（apply_config 重建运行时）。
/// 档案描述「端点 + 密钥 + 模型清单」；指挥体 / 子个体不在此区分——编成本身已按角色分配模型。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LlmProfile {
    pub id: String,
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    /// 该提供商的多把 API Key：按发起方粘性负载均衡，仅限额时切换（docs/11 §1）
    #[serde(default)]
    pub api_keys: Vec<String>,
    /// API 协议：openai（默认）| anthropic
    #[serde(default)]
    pub api_format: String,
    /// 该提供商的默认模型名（模型清单里勾「默认」的那一个）
    #[serde(default)]
    pub model: String,
    /// 模型清单（含视觉/语音能力开关）；空 = 未标记（能力未知，输入直通）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ModelEntry>,
    /// 失败回退：下一个档案 id（请求失败且未发出内容时切换；成环自动截断）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LlmConfig {
    pub base_url: String,
    pub api_key: String,
    /// 生效 Key 池（粘性负载均衡）；空 = 仅用 api_key 单键
    #[serde(default)]
    pub api_keys: Vec<String>,
    /// 生效协议：openai（默认）| anthropic
    #[serde(default)]
    pub api_format: String,
    pub model: String,
}

