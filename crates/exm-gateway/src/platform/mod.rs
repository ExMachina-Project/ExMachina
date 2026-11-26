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
pub use approvals::*;
pub use channel::*;
pub use channels::*;
pub use cron_admin::*;
pub use cron_scheduler::*;
pub use events::*;
pub use groups::*;
pub use plan::*;
pub use routes::routes;
pub use skills::*;
pub use workbench::*;

mod approvals;
pub mod channel;
pub mod channels;
mod cron_admin;
mod cron_scheduler;
mod events;
mod groups;
mod plan;
mod proactive;
mod routes;
mod skills;
mod workbench;

/// 测试临时目录守卫：`Drop` 时整棵移除（断言 panic 也会执行）。
///
/// 夹具都往 `std::env::temp_dir()` 建随机目录；各测试自觉清理容易漏，一轮全量
/// 测试就留下数百个目录，`/tmp` 是 tmpfs 时累积会填满并把存储当内存吃
/// （实测一轮 450 个）。持有本守卫的测试把目录交给它，离开作用域即清理。
#[cfg(test)]
pub(crate) struct TestDirs(pub Vec<std::path::PathBuf>);

#[cfg(test)]
impl Drop for TestDirs {
    fn drop(&mut self) {
        for dir in &self.0 {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[cfg(test)]
mod tests_channel_approval;
#[cfg(test)]
mod tests_channel_boundary;
#[cfg(test)]
mod tests_channel_plan;
#[cfg(test)]
mod tests_channel_steering;
#[cfg(test)]
mod tests_channel_workflow;
mod tests_context_sources;
#[cfg(test)]
mod tests_cron_notify;
#[cfg(test)]
mod tests_heartbeat;
#[cfg(test)]
mod tests_identity_gate;
#[cfg(test)]
mod tests_inbound_media;
#[cfg(test)]
mod tests_plan;
#[cfg(test)]
mod tests_plugin_secret_mask;
