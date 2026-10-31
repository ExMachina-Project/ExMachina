//! 平台能力接入层 —— 定时任务/心跳、执行审批、通道网关、技能包（docs/10）
//!
//! 边界：协议翻译与事件转发；调度判定与执行在 exm-core（cron.rs / tools.rs / Core）。

use crate::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Json, Router};
use exm_core::types::CoreEvent;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

// ---------------------------------------------------------------- 拆分再导出
// platform.rs 已按资源域拆为子模块（纯搬移零重写）；此处 re-export 保持
// `crate::platform::X` 与六个通道适配器（telegram/qqbot/napcat/discord/slack/matrix）
// 及 lib.rs / channel_test.rs 的既有引用路径完全兼容。
// 跨子模块共享的内部项与通道面符号较多，用 glob 再导出保持
// `crate::platform::X` 与六个通道适配器（telegram/qqbot/napcat/discord/slack/matrix）
// 及 lib.rs / channel_test.rs 的既有引用路径完全兼容。
pub use channels::*;
pub use channel_runtime::*;
pub use skills::*;
pub use groups::*;
pub use events::*;
pub use cron_admin::*;
pub use cron_scheduler::*;
pub use approvals::*;
pub use workbench::*;
pub use plan::*;
pub use routes::routes;

mod skills;
mod groups;
mod events;
mod cron_admin;
mod cron_scheduler;
mod approvals;
mod workbench;
mod plan;
pub mod channels;
pub mod channel_runtime;
mod routes;

#[cfg(test)]
mod tests_identity_gate;
#[cfg(test)]
mod tests_inbound_media;
#[cfg(test)]
mod tests_heartbeat;
#[cfg(test)]
mod tests_cron_notify;
#[cfg(test)]
mod tests_plan;
#[cfg(test)]
mod tests_channel_boundary;
#[cfg(test)]
mod tests_plugin_secret_mask;
