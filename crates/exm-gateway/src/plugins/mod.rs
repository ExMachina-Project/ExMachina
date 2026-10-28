//! 插件装载面（gateway 侧）：把 builtin Rust 插件接进 Core 的 PluginManager。
//!
//! video 是第一个 builtin 插件：manifest 声明 channel（bilibili/youtube 凭据面）
//! + watch（自动观看轮询），实现经 plugins/video.rs 的既有 supervisor 生命周期
//! （spawn_supervisor / reconcile / run_loop 全套保持不变——迁移验收线 = 行为等价）。
//!
//! 其余内置通道（telegram/napcat/qqbot/discord/slack/matrix）保留静态注册
//! （lib.rs 启动序列直呼 spawn_supervisor），不强制走插件面——统一边界渐进收敛。

pub mod video;
pub mod rss;

use exm_core::config::{PluginChannelDecl, PluginCredentialField, PluginManifest};
use exm_core::plugin::{ChannelProvider, WatchProvider};
use exm_core::Core;
use std::sync::Arc;

/// video 插件的 manifest（builtin 静态声明；plugins/video/plugin.json 可覆盖）
pub fn video_manifest() -> PluginManifest {
    PluginManifest {
        id: "video".into(),
        name: "视频观看".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        description: "B站/YouTube 自动观看与互动（通道适配 + 轮询 watcher）".into(),
        exm_min: String::new(),
        tools: vec![],
        channel: Some(PluginChannelDecl {
            kinds: vec!["bilibili".into(), "youtube".into()],
            credentials: vec![
                PluginCredentialField {
                    key: "sessdata".into(),
                    label: "B站 SESSDATA 凭据".into(),
                    sensitive: true,
                },
                PluginCredentialField {
                    key: "biliJct".into(),
                    label: "B站 bili_jct CSRF 令牌".into(),
                    sensitive: true,
                },
                PluginCredentialField {
                    key: "uid".into(),
                    label: "B站用户 uid".into(),
                    sensitive: false,
                },
                PluginCredentialField {
                    key: "apiKey".into(),
                    label: "YouTube Data API 密钥".into(),
                    sensitive: true,
                },
            ],
        }),
        watch: Some(exm_core::config::PluginWatchDecl {
            interval_minutes: 10,
        }),
        resources: vec![],
        process: None,
    }
}

/// video 的通道能力声明（ChannelProvider：平台 kind 与凭据面）
struct VideoChannel;

impl ChannelProvider for VideoChannel {
    fn kinds(&self) -> Vec<String> {
        vec!["bilibili".into(), "youtube".into()]
    }
    fn credential_fields(&self) -> Vec<PluginCredentialField> {
        video_manifest()
            .channel
            .map(|c| c.credentials)
            .unwrap_or_default()
    }
}

/// video 的 watcher 能力声明（WatchProvider：宿主调度启停）
struct VideoWatch;

impl WatchProvider for VideoWatch {
    /// 启动轮询循环：复用既有 supervisor（reconcile 10s 节拍 + run_loop 全套）
    fn spawn(self: Arc<Self>, core: Arc<Core>) {
        video::spawn_supervisor(core);
    }
}

/// video 插件 provider（ChannelProvider + WatchProvider）
pub struct VideoPlugin {
    manifest: PluginManifest,
}

impl VideoPlugin {
    pub fn new() -> Self {
        VideoPlugin {
            manifest: video_manifest(),
        }
    }
}

impl Default for VideoPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl exm_core::plugin::PluginProvider for VideoPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }
    fn channel_provider(&self) -> Option<Arc<dyn ChannelProvider>> {
        Some(Arc::new(VideoChannel))
    }
    fn watch_provider(&self) -> Option<Arc<dyn WatchProvider>> {
        Some(Arc::new(VideoWatch))
    }
}

/// 网关启动时装配：注册全部 builtin 插件并拉起已启用插件的 watcher。
/// 取代 lib.rs 直呼 `video::spawn_supervisor`（其余通道的静态序列保留）。
pub fn spawn_all(core: Arc<Core>) {
    // builtin 插件注册（PluginManager 持注册面；watcher 启动在此驱动）
    let pm = core.plugins();
    {
        // register_builtin 需要 &mut——PluginManager 装配期在 Core::with_config，
        // 网关侧补注册走独立通道：直接登记 manifest + provider（load_all 已跑过，
        // 此处 set_provider + 伪 load 一次以进入注册表）。
        // 简化口径：Core 装配时 plugins/ 目录可能还没被扫（builtin 在 gateway 侧），
        // 这里补一次 register——PluginManager 的 builtin 表设计为编译期注册。
        pm.register_builtin_late(Box::new(VideoPlugin::new()));
        pm.register_builtin_late(Box::new(rss::RssPlugin::new()));
    }
    // process 插件拉起（plugin-ecosystem 组 3）：握手协商 + tools 快照 + 凭据下发。
    // 失败不阻塞网关启动——entry.error 已记原因（管理页可见），健康检查可重试。
    {
        let pm2 = pm.clone();
        let core2 = core.clone();
        tokio::spawn(async move {
            for e in pm2.list() {
                if e.form != "process" || !e.enabled || e.error.is_some() {
                    continue;
                }
                let Some(dir) = e.dir.clone() else { continue };
                let creds = core2
                    .config()
                    .plugins
                    .credentials
                    .get(&e.manifest.id)
                    .cloned();
                let r = exm_core::plugin::bridge::spawn_process_plugin(
                    &pm2,
                    &e.manifest,
                    &dir,
                    creds,
                )
                .await;
                match r {
                    Ok(_) => eprintln!("[plugins] 进程插件 {} 已拉起（握手协商通过）", e.manifest.id),
                    Err(err) => {
                        pm2.set_tools_snapshot(&e.manifest.id, vec![]);
                        pm2.record_error(&e.manifest.id, Some(format!("拉起失败：{err}")));
                        eprintln!("[plugins] 进程插件 {} 拉起失败：{err}", e.manifest.id);
                    }
                }
            }
        });
    }
    // watcher：已启用插件的 WatchProvider 逐个拉起
    for p in pm.enabled_providers() {
        if let Some(w) = p.watch_provider() {
            w.spawn(core.clone());
        }
    }
}
