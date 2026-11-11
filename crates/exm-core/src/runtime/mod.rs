//! 子个体运行时 —— docs/01 §2.3
//! 装配 → LLM 循环（流式：正文 + 思维链双轨）→ 工具调用约定 → SyncReport 契约校验 → 回流

use crate::parse;
use crate::provider::{ChatMessage, ChatRequest, LlmProvider, StreamDelta, ToolCall};
use crate::registry::LocalRegistry;
use crate::tools::ToolGateway;
use crate::types::{AgentDefinition, DispatchOrder, SyncReport, ToolName, ToolScope};
use std::sync::Arc;
use tokio::sync::mpsc::unbounded_channel;

pub const SYNC_REWRITE_LIMIT: u32 = 2;

/// 单个工具调用的回填材料：回填文本 + 截图回灌组（图注 + 图像 data URL 清单）
pub type ToolFeedback = (String, Option<(String, Vec<String>)>);

/// 工具结果 → 回填文本（【报告】成功 / 【警告】失败），语义与审计一致
fn tool_feedback(name: &str, r: &crate::tools::ToolResult) -> String {
    if r.ok {
        format!("【报告】工具 {name} 执行结果：\n{}", r.output)
    } else {
        format!(
            "【警告】工具 {name} 执行失败：{}",
            r.error.as_deref().unwrap_or_default()
        )
    }
}

/// 单智能体循环步数上限解析（三级覆盖）：
/// 节点目标简述（降级报告用）：objective 优先，回落 title。
fn node_objective_of(order: &DispatchOrder) -> String {
    let t: String = order.objective.chars().take(80).collect();
    t
}

/// 个体 maxSteps > 作用域档位偏好（Chat 档取全局半数作对话轮保护）> 全局默认。
/// 全局 0 值视为无效回落 24（与 ToolGateway::chat_max_steps 同口径）。
/// 工具面清单注入（防「能力幻觉」拒绝）：本轮实际生效的工具名逐个列出。
/// 部署实测教训：工具 schema 虽随请求下发，模型不「看见」清单时会凭空
/// 声明「本机无文件系统访问权限」并拒绝执行——显式清单让能力自述有据可依。
fn tool_surface_note(names: &[String]) -> String {
    if names.is_empty() {
        return String::new();
    }
    format!(
        "## 本轮可用工具（实际生效清单）\n\n以下是你在本轮**真实可以调用**的全部工具，需要什么就直接用，不要凭空声明自己没有某项能力：\n{}",
        names
            .iter()
            .map(|n| format!("- {n}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

pub fn resolve_max_steps(agent_override: Option<usize>, scope: ToolScope, global: usize) -> usize {
    let global = if global == 0 { 24 } else { global };
    let scope_pref = match scope {
        ToolScope::Chat => (global / 2).max(1),
        _ => global,
    };
    agent_override
        .filter(|v| *v > 0)
        .unwrap_or(scope_pref)
        .max(1)
}

/// 子个体执行器：持 Provider / 工具网关 / 注册表（注册表只读共享）
pub struct AgentRuntime {
    registry: Arc<LocalRegistry>,
    provider: Arc<dyn LlmProvider>,
    tools: Arc<ToolGateway>,
    model: String,
    /// 实验性开关快照（装配时写入；context_compact 下发门控用）
    experiments: crate::config::ExperimentsConfig,
}

impl AgentRuntime {
    pub fn new(
        registry: Arc<LocalRegistry>,
        provider: Arc<dyn LlmProvider>,
        tools: Arc<ToolGateway>,
        model: impl Into<String>,
    ) -> Self {
        AgentRuntime {
            registry,
            provider,
            tools,
            model: model.into(),
            experiments: crate::config::ExperimentsConfig::default(),
        }
    }

    /// 注入实验性开关（Core 装配后回接）
    pub fn with_experiments(mut self, experiments: crate::config::ExperimentsConfig) -> Self {
        self.experiments = experiments;
        self
    }

    /// Provider 共享（spawn 执行器构造回退链用）
    pub fn provider_clone(&self) -> Arc<dyn LlmProvider> {
        self.provider.clone()
    }

    /// 默认执行目标（全局生效档案）：未解析出个体/组专属模型时回退用
    pub fn default_target(&self) -> (Arc<dyn LlmProvider>, String) {
        (self.provider.clone(), self.model.clone())
    }

}

// ---------------------------------------------------------------- 域分文件

mod chat;
mod llm;
mod tools;

#[cfg(test)]
mod tests;
