//! 视频平台主动观看与互动（B 站 / YouTube）。
//!
//! 与 IM 适配器的区别：视频平台没有「入站消息」驱动，按配置的间隔主动轮询热门/关键词视频，
//! 把标题、简介、字幕（B 站可用时）与评论区摘录交给通道绑定的智能体，由智能体产出
//! 「观看摘要 + 评论 / 回复」，再按配置决定 dry-run 还是真正发到平台。
//!
//! 安全与边界：
//! - 默认 `dryRun=true`：不实际发评论，只记录状态；用户显式关闭 dry-run 才落笔。
//! - 评论 / 回复各自独立开关，默认关闭；每日动作上限 + 每轮视频上限双重限流。
//! - 通道能力边界（chat/tools/computer）与记忆隔离沿用 `platform::ChannelRun`，
//!   与 IM 平台完全同口径——视频平台不是绕过边界的后门。
//! - 「观看」= 读取平台公开内容（简介 / 字幕 / 热评）作为观看材料，不伪造播放量、
//!   不发送心跳骗播放；需要更高权限时由用户自己提供登录 Cookie / OAuth。

use crate::platform::{self, Channel, ChannelRun};
use exm_core::Core;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use parking_lot::Mutex;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

const UA: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";
const BILI_REFERER: &str = "https://www.bilibili.com/";

// ---------------------------------------------------------------- 监督循环

struct Runner {
    handle: tokio::task::JoinHandle<()>,
    fingerprint: String,
}

fn runners() -> &'static Mutex<HashMap<String, Runner>> {
    static RUNNERS: OnceLock<Mutex<HashMap<String, Runner>>> = OnceLock::new();
    RUNNERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn is_video_platform(kind: &str) -> bool {
    matches!(kind, "bilibili" | "youtube")
}

fn fingerprint(ch: &Channel) -> String {
    let cfg = serde_json::to_string(&ch.config).unwrap_or_default();
    format!(
        "{:?}|{:?}|{}|{}|{}",
        ch.enabled, ch.group, ch.capability, ch.memory_isolation, cfg
    )
}

/// 每 10 秒对账一次：新增/配置变化的视频通道拉起独立轮询任务，停用/删除则回收。
pub fn spawn_supervisor(core: Arc<Core>) {
    tokio::spawn(async move {
        loop {
            reconcile(&core);
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    });
}

fn reconcile(core: &Arc<Core>) {
    // 全局点播工具面同步（先于 runner 治理：通道启停立即反映到工具注册表）
    tools::reconcile_ondemand(core);
    let wanted: HashMap<String, Channel> = platform::load_channels(core)
        .into_iter()
        .filter(|c| c.enabled && is_video_platform(&c.kind))
        .map(|c| (c.id.clone(), c))
        .collect();

    let mut map = runners().lock();
    let stale: Vec<String> = map
        .iter()
        .filter(|(id, r)| {
            wanted
                .get(*id)
                .map(|ch| fingerprint(ch) != r.fingerprint)
                .unwrap_or(true)
        })
        .map(|(id, _)| id.clone())
        .collect();
    for id in stale {
        if let Some(r) = map.remove(&id) {
            r.handle.abort();
            platform::clear_status(&id);
        }
    }
    for (id, ch) in wanted {
        if map.contains_key(&id) {
            continue;
        }
        let fp = fingerprint(&ch);
        let core = core.clone();
        let handle = tokio::spawn(async move { run_loop(core, ch).await });
        map.insert(
            id,
            Runner {
                handle,
                fingerprint: fp,
            },
        );
    }
}

async fn run_loop(core: Arc<Core>, ch: Channel) {
    // 启动错峰：避免进程重启时多条视频通道同时打平台接口
    tokio::time::sleep(Duration::from_secs(8)).await;
    loop {
        let settings = Settings::from_channel(&ch);
        match tick(&core, &ch, &settings).await {
            Ok(note) => platform::report_status(&ch.id, "ok", note),
            Err(e) => platform::report_status(&ch.id, "error", format!("主动观看失败：{e}")),
        }
        tokio::time::sleep(Duration::from_secs(settings.interval_minutes * 60)).await;
    }
}

// ---------------------------------------------------------------- 配置

pub(crate) struct Settings {
    watch: bool,
    comment: bool,
    reply: bool,
    dry_run: bool,
    interval_minutes: u64,
    max_actions: usize,
    daily_limit: u32,
    /// 每轮观看的工具调用步数上限（防失控深挖；默认 8，可配 1-32）
    max_watch_steps: usize,
    /// 互动审批模式：off = 直接发；always = 评论发出前需人工审批（批准代发）
    approval: String,
    /// 点播评论准入口径：admin（默认，仅管理者）/ member（所有绑定成员）；
    /// 仅作用于非轮次会话的 video__act 判权，轮次闸门不受影响
    act_role: String,
    max_comment_chars: usize,
    source: String,
    keywords: Vec<String>,
    /// 内容倾向：自由文本提示词，整段注入观看提示作口味参考；分词仅用于候选软排序
    interests: String,
    /// 评论风格提示（软约束，注入观看提示）
    comment_style: String,
    region: String,
}

impl Settings {
    fn from_channel(ch: &Channel) -> Self {
        let comment = cfg_bool(ch, "comment", false);
        let reply = cfg_bool(ch, "reply", false);
        let watch = cfg_bool(ch, "watch", true) || comment || reply;
        Settings {
            watch,
            comment,
            reply,
            dry_run: cfg_bool(ch, "dryRun", true),
            interval_minutes: cfg_u64(ch, "intervalMinutes", 10).clamp(5, 24 * 60),
            max_actions: cfg_u64(ch, "maxActionsPerRun", 1).clamp(1, 5) as usize,
            daily_limit: cfg_u64(ch, "dailyLimit", 6).clamp(1, 60) as u32,
            max_watch_steps: cfg_u64(ch, "maxWatchSteps", 8).clamp(1, 32) as usize,
            approval: ch
                .cfg("approval")
                .unwrap_or_else(|| "off".into())
                .trim()
                .to_lowercase(),
            act_role: {
                let v = ch
                    .cfg("actRole")
                    .unwrap_or_else(|| "admin".into())
                    .trim()
                    .to_lowercase();
                if v == "member" {
                    "member".into()
                } else {
                    "admin".into()
                }
            },
            max_comment_chars: cfg_u64(ch, "maxCommentChars", 120).clamp(20, 800) as usize,
            source: ch.cfg("source").unwrap_or_else(|| "popular".into()),
            keywords: split_list(ch.cfg("keywords").as_deref()),
            interests: ch.cfg("interests").unwrap_or_default(),
            comment_style: ch.cfg("commentStyle").unwrap_or_default(),
            region: ch.cfg("region").unwrap_or_else(|| "US".into()),
        }
    }
}

fn cfg_bool(ch: &Channel, key: &str, default: bool) -> bool {
    ch.cfg(key)
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "yes" | "on"))
        .unwrap_or(default)
}

fn cfg_u64(ch: &Channel, key: &str, default: u64) -> u64 {
    ch.cfg(key)
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

fn split_list(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or("")
        .split([',', '，', '\n', ';', '；'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

// ---------------------------------------------------------------- 状态

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct VideoState {
    #[serde(default)]
    seen_videos: Vec<String>,
    #[serde(default)]
    seen_comments: Vec<String>,
    #[serde(default)]
    day: String,
    #[serde(default)]
    actions_today: u32,
}

impl VideoState {
    pub(crate) fn load(core: &Core, channel_id: &str) -> Self {
        let p = state_path(core, channel_id);
        std::fs::read_to_string(p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub(crate) fn save(&self, core: &Core, channel_id: &str) {
        let p = state_path(core, channel_id);
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }

    pub(crate) fn reset_day(&mut self) {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        if self.day != today {
            self.day = today;
            self.actions_today = 0;
        }
    }

    fn seen(&self, key: &str) -> bool {
        self.seen_videos.iter().any(|k| k == key)
    }

    fn mark_video(&mut self, key: &str) {
        if !self.seen(key) {
            self.seen_videos.push(key.to_string());
            if self.seen_videos.len() > 500 {
                let drop = self.seen_videos.len() - 500;
                self.seen_videos.drain(0..drop);
            }
        }
    }
}

fn state_path(core: &Core, channel_id: &str) -> std::path::PathBuf {
    let safe: String = channel_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    core.config()
        .data_dir
        .join("video_state")
        .join(format!("{safe}.json"))
}


// ---------------------------------------------------------------- 域分文件

mod platform_api;
mod watch;

pub(crate) use platform_api::*;
pub(crate) use watch::*;

pub(crate) mod tools;

#[cfg(test)]
mod tests;
