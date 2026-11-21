//! 工具网关 —— docs/01 §2.6：白名单 + 工作区沙箱 + 审计
//!
//! 工具面（对标主流 agent 的「手」）：read（行号/分页）/ edit（精确替换）/ grep / glob /
//! filesystem（write/mkdir/list）/ terminal（白名单 + 审批 + 后台长任务）/ web_search（真实后端，
//! 未配置即不下发）/ web_fetch / agent_manage（组内编成）/ schedule（自主排程）。

use crate::config::{SandboxConfig, SearchConfig, SecurityConfig};
use crate::provider::LlmProvider;
use crate::registry::LocalRegistry;
use crate::store::Store;
use crate::types::{normalize_domain, AgentDefinition, ApprovalRequest, CoreEvent, Tier, ToolName};
use anyhow::{bail, Context};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

// ---------------------------------------------------------------- 拆分再导出
// tools.rs 已按领域拆为子模块（纯搬移零重写）；此处 re-export 保持
// `exm_core::tools::X` 路径与 crate 外全部既有引用兼容。
pub use allowlist::{command_head, is_destructive_command};
pub use dynamic::{dynamic_exec, dynamic_exec_images, DynamicToolExec, DynamicToolOutput};
/// 外部内容隔离包装（inbound-sanitization）：供网关侧工具（RSS digest 等）复用
pub use external_wrap::wrap_external as wrap_external_for;
pub use gateway::{
    scope_budget, PluginCredResolver, SessionGrantProbe, SessionSendExec, SpawnExec, ToolGateway,
};
pub(crate) use gateway::PluginCommandTarget;
pub use hooks::run_hook_command;
pub use redact::{redact_json, redact_str};
pub use result::ToolResult;
pub use sandbox::{
    execute_shell_command_timed, execute_shell_with, is_network_command, windows_job_available,
    ShellOpts, WinJob,
};
pub use terminal::{SpawnBgState, SpawnBgTask};
pub use terminal_jobs::BackgroundLimits;
pub(crate) use terminal_jobs::{
    terminal_action_from_approval, TerminalApprovalAction, TerminalApprovalRequest,
};

// 跨子模块共享的内部项（原 tools.rs 同文件私有；拆分后经 tools 根再导出保持 sibling 可见）。
// #[allow(unused_imports)]：这些符号经 `use super::*` glob 链被各子模块使用，
// rustc 的 unused lint 不把 glob 使用归因到具体 re-export 行，会误报。
#[allow(unused_imports)]
pub(crate) use allowlist::{
    chain_allowed, contains_shell_chain, is_elevation_command, shell_quote, shell_unsafe_char,
    split_chain_segments, strip_quoted, strip_redundant_elevation, terminal_allowed,
};
pub(crate) use computer::computer_action_from_approval;
pub use computer::{ComputerActionExecutor, ComputerApprovalAction};
#[allow(unused_imports)]
pub(crate) use custom::{
    interpolate, json_path, placeholder_names, shape_response, shape_truncate, urlencode,
};
#[allow(unused_imports)]
pub(crate) use sandbox::execute_shell_for_session;
#[allow(unused_imports)]
pub(crate) use sandbox::{build_shell_command, resolve_execution_cwd};
#[allow(unused_imports)]
pub(crate) use sandbox::{bwrap_available, ENV_ALLOW};
#[allow(unused_imports)]
pub(crate) use web::strip_html;

mod agents;
mod allowlist;
mod custom;
mod dynamic;
mod files;
mod gateway;
mod git;
mod hooks;
mod managed_process;
mod memory_tools;
mod meta;
mod result;
mod sandbox;
mod sandbox_helpers;
mod schema;
mod sessions;
mod skill_manage;
mod spawn;
mod terminal;
mod terminal_jobs;
mod terminal_output;
mod todo;
pub(crate) use meta::MetaProposal;
mod approval;
mod browser;
mod chart;
mod computer;
mod external_wrap;
mod image_gen;
mod knowledge;
mod redact;
mod research;
mod schedule;
mod speech;
mod web;

#[cfg(test)]
mod tests;
