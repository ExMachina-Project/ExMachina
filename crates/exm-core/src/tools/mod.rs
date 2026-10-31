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
pub use result::ToolResult;
pub use allowlist::{is_destructive_command};
pub use terminal::{BgTask, SpawnBgState, SpawnBgTask};
pub use hooks::run_hook_command;
pub use sandbox::{ShellOpts, WinJob, is_network_command, execute_shell_command_timed, execute_shell_with, windows_job_available};
pub use dynamic::{DynamicToolOutput, DynamicToolExec, dynamic_exec, dynamic_exec_images};
pub use gateway::{SpawnExec, PluginCredResolver, ToolGateway};


// 跨子模块共享的内部项（原 tools.rs 同文件私有；拆分后经 tools 根再导出保持 sibling 可见）。
// #[allow(unused_imports)]：这些符号经 `use super::*` glob 链被各子模块使用，
// rustc 的 unused lint 不把 glob 使用归因到具体 re-export 行，会误报。
#[allow(unused_imports)]
pub(crate) use allowlist::{contains_shell_chain, chain_allowed, shell_unsafe_char, shell_quote, terminal_allowed, split_chain_segments, strip_quoted};
#[allow(unused_imports)]
pub(crate) use sandbox::{ENV_ALLOW, bwrap_available};
#[allow(unused_imports)]
pub(crate) use sandbox::attach_job;
#[allow(unused_imports)]
pub(crate) use gateway::InputAction;
#[allow(unused_imports)]
pub(crate) use custom::{urlencode, shape_response, shape_truncate, interpolate, json_path, placeholder_names};
#[allow(unused_imports)]
pub(crate) use web::strip_html;

mod result;
mod allowlist;
mod hooks;
mod sandbox;
mod sandbox_helpers;
mod dynamic;
mod gateway;
mod memory_tools;
mod custom;
mod agents;
mod files;
mod git;
mod spawn;
mod terminal;
mod todo;
mod computer;
mod approval;
mod web;
mod browser;
mod schedule;

#[cfg(test)]
mod tests;
