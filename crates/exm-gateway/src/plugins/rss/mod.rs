//! RSS 阅读内置插件（plugin-ecosystem 组 4）：第二个 builtin 三件套范例。
//!
//! 形态对齐 video：通道（kinds=["rss"]）+ watcher（间隔轮询）+ 工具（rss__digest）。
//! 轮询发现新条目 → 拼材料唤醒通道绑定个体会话（video ask_agent 模式）产出简报 →
//! 简报进会话流；订阅了本通道的会话可收到推送（复用自动化推送文本通路）。
//! 状态（已见链接）持久化 `.exmachina/rss_state/<通道id>.json`（video_state 同模式）。

pub(crate) mod feed;

use crate::platform::{self, Channel, ChannelRun};
use exm_core::config::{
    CustomTool, PluginChannelDecl, PluginCredentialField, PluginManifest, PluginWatchDecl,
};
use exm_core::Core;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

// ---------------------------------------------------------------- 监督循环（video 同模式）

struct Runner {
    handle: tokio::task::JoinHandle<()>,
    fingerprint: String,
}

fn runners() -> &'static Mutex<HashMap<String, Runner>> {
    static RUNNERS: OnceLock<Mutex<HashMap<String, Runner>>> = OnceLock::new();
    RUNNERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// RSS 通道判定（通道 kind = rss；与 manifest channel.kinds 一致）
fn is_rss_channel(kind: &str) -> bool {
    kind == "rss"
}

fn fingerprint(ch: &Channel) -> String {
    let cfg = serde_json::to_string(&ch.config).unwrap_or_default();
    format!(
        "{:?}|{:?}|{:?}|{}|{:?}|{}",
        ch.enabled, ch.group, ch.capability, ch.memory_isolation, ch.allowed_chats, cfg
    )
}

/// 每 10 秒对账一次：新增/配置变化的 RSS 通道拉起轮询任务，停用/删除回收。
pub fn spawn_supervisor(core: Arc<Core>) {
    tokio::spawn(async move {
        loop {
            reconcile(&core);
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    });
}

fn reconcile(core: &Arc<Core>) {
    let wanted: HashMap<String, Channel> = platform::load_channels(core)
        .into_iter()
        .filter(|c| c.enabled && is_rss_channel(&c.kind))
        .map(|c| (c.id.clone(), c))
        .collect();
    let mut runners = runners().lock().unwrap();
    // 回收：停用/删除/kind 变化的
    runners.retain(|id, r| {
        let keep = wanted
            .get(id)
            .map(|ch| fingerprint(ch) == r.fingerprint)
            .unwrap_or(false);
        if !keep {
            r.handle.abort();
        }
        keep
    });
    // 新拉：新增/配置变化
    for (id, ch) in wanted {
        if runners.contains_key(&id) {
            continue;
        }
        let core2 = core.clone();
        let ch2 = ch.clone();
        let handle = tokio::spawn(async move { run_loop(core2, ch2).await });
        runners.insert(
            id,
            Runner {
                handle,
                fingerprint: fingerprint(&ch),
            },
        );
    }
}

async fn run_loop(core: Arc<Core>, ch: Channel) {
    // 启动错峰（多通道不同时打源站）
    tokio::time::sleep(Duration::from_secs(5)).await;
    loop {
        let s = Settings::from_channel(&ch);
        match tick(&core, &ch, &s).await {
            Ok(note) => platform::report_status(&ch.id, "ok", note),
            Err(e) => platform::report_status(&ch.id, "error", format!("RSS 轮询失败：{e}")),
        }
        tokio::time::sleep(Duration::from_secs(s.interval_minutes * 60)).await;
    }
}

// ---------------------------------------------------------------- 配置

pub(crate) struct Settings {
    /// 订阅源列表（「名称=URL」或裸 URL；一行一个，# 注释）
    feeds: Vec<(String, String)>,
    interval_minutes: u64,
    /// 简报推送通道 id 订阅（复用自动化推送文本通路；空 = 不推送）
    push: Vec<String>,
    /// 每轮阅读条目上限（超出顺延下轮）
    max_per_round: usize,
}

fn cfg_u64(ch: &Channel, key: &str, default: u64) -> u64 {
    ch.cfg(key)
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

impl Settings {
    fn from_channel(ch: &Channel) -> Self {
        // feeds：一行一个「名称=URL」或裸 URL（缺省名取 URL 末段）
        let feeds = ch
            .cfg("feeds")
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| match l.split_once('=') {
                Some((name, url)) if url.contains("://") => {
                    (name.trim().to_string(), url.trim().to_string())
                }
                _ => (
                    l.rsplit('/').next().unwrap_or(l).to_string(),
                    l.to_string(),
                ),
            })
            .collect();
        Settings {
            feeds,
            interval_minutes: cfg_u64(ch, "intervalMinutes", 30).clamp(5, 24 * 60),
            push: ch
                .cfg("push")
                .map(|v| {
                    v.split([',', '，', ';', '；', '\n'])
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            max_per_round: cfg_u64(ch, "maxPerRound", 10).clamp(1, 50) as usize,
        }
    }
}

// ---------------------------------------------------------------- 状态（video_state 同模式）

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RssState {
    /// 已见条目链接（去重键；上限 500，FIFO 淘汰）
    seen_links: Vec<String>,
    /// 顺延待读条目（超出 maxPerRound 的部分；下轮优先）
    deferred: Vec<feed::FeedItem>,
}

impl RssState {
    fn seen(&self, link: &str) -> bool {
        self.seen_links.iter().any(|l| l == link)
    }

    fn mark(&mut self, link: &str) {
        if !self.seen(link) {
            self.seen_links.push(link.to_string());
            if self.seen_links.len() > 500 {
                let drop = self.seen_links.len() - 500;
                self.seen_links.drain(0..drop);
            }
        }
    }

    fn load(core: &Core, channel_id: &str) -> Self {
        let p = state_path(core, channel_id);
        std::fs::read_to_string(p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, core: &Core, channel_id: &str) {
        let p = state_path(core, channel_id);
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(p, text);
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
        .join("rss_state")
        .join(format!("{safe}.json"))
}

// ---------------------------------------------------------------- 单轮

/// 一轮轮询：逐源拉取 → 去重 → （顺延优先）截 maxPerRound → 拼材料唤醒阅读会话 → 简报推送。
/// 逐源失败不阻塞（记状态面板）；无新条目 = 空轮。
async fn tick(core: &Arc<Core>, ch: &Channel, s: &Settings) -> anyhow::Result<String> {
    let mut state = RssState::load(core, &ch.id);
    // 顺延优先：上轮超出的条目先读
    let mut fresh: Vec<(String, feed::FeedItem)> = std::mem::take(&mut state.deferred)
        .into_iter()
        .map(|it| ("（顺延）".to_string(), it))
        .collect();
    let mut errors: Vec<String> = Vec::new();
    for (name, url) in &s.feeds {
        match fetch_feed(url).await {
            Ok(items) => {
                for it in items {
                    if !state.seen(&it.link) {
                        fresh.push((name.clone(), it));
                    }
                }
            }
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    if fresh.is_empty() {
        let note = if errors.is_empty() {
            "无新条目".to_string()
        } else {
            format!("无新条目｜源失败：{}", errors.join("；"))
        };
        state.save(core, &ch.id); // 空轮也落盘（清空已消费的顺延队列）
        return Ok(note);
    }
    // 上限截断：超出部分顺延（不标已读——下轮仍算新）
    let read: Vec<_> = if fresh.len() > s.max_per_round {
        let rest = fresh.split_off(s.max_per_round);
        state.deferred = rest.into_iter().map(|(_, it)| it).collect();
        fresh
    } else {
        fresh
    };
    // 阅读会话：材料唤醒通道绑定个体，产出简报
    let digest = read_session(core, ch, s, &read).await;
    // 标已读 + 持久化（重启不重推）
    for (_, it) in &read {
        state.mark(&it.link);
    }
    state.save(core, &ch.id);
    // 推送（订阅通道；复用自动化推送文本通路；失败不阻塞）
    if !s.push.is_empty() && !digest.is_empty() {
        push_digest(core, &s.push, &digest).await;
    }
    let mut note = format!("阅读 {} 条新条目", read.len());
    if !state.deferred.is_empty() {
        note.push_str(&format!("（顺延 {} 条）", state.deferred.len()));
    }
    if !errors.is_empty() {
        note.push_str(&format!("｜源失败：{}", errors.join("；")));
    }
    Ok(note)
}

/// 拉取单个源（10s 超时；UA 注明用途）
async fn fetch_feed(url: &str) -> anyhow::Result<Vec<feed::FeedItem>> {
    let text = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?
        .get(url)
        .header("User-Agent", "ExMachina-RSS-Reader/0.5")
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    feed::parse_feed(&text)
}

/// 阅读会话：材料唤醒通道绑定个体（video ask_agent 模式）→ 简报文本。
/// 会话注入 rss__digest（rss 会话可见；普通会话不可见）；收束后清理。
async fn read_session(
    core: &Arc<Core>,
    ch: &Channel,
    _s: &Settings,
    items: &[(String, feed::FeedItem)],
) -> String {
    let mut material = String::from(
        "以下是本轮 RSS 新条目。请逐条给出一句话要点（格式：标题｜要点），最后给一行整体摘要：\n",
    );
    for (i, (src, it)) in items.iter().enumerate() {
        material.push_str(&format!(
            "\n{}. [{}] {}（{}）\n   {}",
            i + 1,
            src,
            it.title,
            it.link,
            if it.summary.is_empty() { "（无摘要）" } else { &it.summary }
        ));
    }
    let (run, mut rx) = match ChannelRun::begin(core, ch, "rss").await {
        Some(r) => r,
        None => {
            platform::report_status(&ch.id, "error", "RSS 阅读会话建立失败");
            return String::new();
        }
    };
    let sid = run.session_id.clone();
    // rss__digest 注入本会话（工具面与事件订阅同键）
    let injections = digest_injection(core, ch);
    core.orchestrator().tools.inject_session_tools(&sid, injections);
    // 系统自动任务标记：落库消息带 auto:rss 来源
    let run = run.with_origin("auto:rss");
    let reply_run = run.clone();
    let prompt = material.clone();
    let task = tokio::spawn(async move { run.run(&prompt).await });
    let mut digest = String::new();
    let wait = async {
        loop {
            match rx.recv().await {
                Ok(evt) if evt.session_id == sid => {
                    if let Some(text) = reply_run.take_reply(&evt, 4_000) {
                        digest = text;
                        break;
                    }
                }
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    };
    // 阅读多轮工具调用收束窗口：600s（video 同口径）
    let timed_out = tokio::time::timeout(Duration::from_secs(600), wait).await.is_err();
    if timed_out {
        task.abort();
        let _ = task.await;
    } else {
        let _ = task.await;
    }
    core.orchestrator().tools.clear_session_tools(&sid);
    digest
}

/// 简报推送：订阅通道文本直发（push_channel_text；失败不阻塞）
async fn push_digest(core: &Arc<Core>, targets: &[String], digest: &str) {
    let text = format!("📬 RSS 简报\n{}", digest);
    for ch in platform::load_channels(core) {
        if !ch.enabled || !targets.contains(&ch.id) {
            continue;
        }
        if !platform::push_channel_text(&ch, &text).await {
            eprintln!("[rss] 通道 {} 简报推送失败", ch.id);
        }
    }
}

// ---------------------------------------------------------------- rss__digest 工具

/// digest 工具 schema（会话注入面：rss 会话可见）
fn digest_tool_spec() -> CustomTool {
    CustomTool {
        name: "rss__digest".into(),
        description: "查看 RSS 订阅源当前条目（缺省全源每源最新 5 条；source 指定 feed 名或 URL）"
            .into(),
        kind: "builtin".into(),
        method: String::new(),
        target: String::new(),
        headers: Default::default(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "source": { "type": "string", "description": "feed 名称或 URL（缺省全源）" }
            }
        }),
        agents: vec![],
        response_max_chars: None,
        response_select: None,
    }
}

/// digest 执行器：拉源 → 每源最新 5 条摘要（只读视图，不消费轮询状态）
fn digest_exec(core: Arc<Core>, ch: Channel) -> exm_core::tools::DynamicToolExec {
    exm_core::tools::dynamic_exec(move |_agent, _session, args| {
        let core = core.clone();
        let ch = ch.clone();
        async move {
            let s = Settings::from_channel(&ch);
            let want = args
                .get("source")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty());
            // 源筛选：名称或 URL 匹配
            let selected: Vec<_> = s
                .feeds
                .iter()
                .filter(|(name, url)| match want {
                    Some(w) => name == w || url == w || url.starts_with(w),
                    None => true,
                })
                .collect();
            if selected.is_empty() {
                anyhow::bail!(
                    "未匹配到订阅源：{}（可用：{}）",
                    want.unwrap_or("?"),
                    s.feeds.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>().join("、")
                );
            }
            let mut out = String::new();
            for (name, url) in selected {
                match fetch_feed(url).await {
                    Ok(items) => {
                        out.push_str(&format!("[{name}]\n"));
                        for it in items.iter().take(5) {
                            out.push_str(&format!(
                                "- {}（{}）{}\n",
                                it.title,
                                it.published,
                                if it.summary.is_empty() {
                                    String::new()
                                } else {
                                    format!("：{}", it.summary)
                                }
                            ));
                        }
                    }
                    Err(e) => out.push_str(&format!("[{name}] 拉取失败：{e}\n")),
                }
            }
            let _ = core; // 保留句柄（未来接状态视图）
            Ok(out.trim().to_string())
        }
    })
}

/// 会话注入面：rss 阅读会话的 digest 工具组
pub(crate) fn digest_injection(
    core: &Arc<Core>,
    ch: &Channel,
) -> Vec<(CustomTool, exm_core::tools::DynamicToolExec)> {
    vec![(digest_tool_spec(), digest_exec(core.clone(), ch.clone()))]
}

// ---------------------------------------------------------------- builtin 三件套（manifest + provider）

/// RSS 插件清单（与 plugins/rss/plugin.json 同源；磁盘清单可覆盖描述/凭据面）
pub fn rss_manifest() -> PluginManifest {
    PluginManifest {
        id: "rss".into(),
        name: "RSS 阅读".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        description: "订阅 RSS/Atom 源：定时轮询新条目唤醒阅读简报，rss__digest 随手查阅".into(),
        exm_min: "0.5.3".into(),
        tools: vec![],
        channel: Some(PluginChannelDecl {
            kinds: vec!["rss".into()],
            credentials: vec![
                PluginCredentialField {
                    key: "feeds".into(),
                    label: "订阅源（每行「名称=URL」或裸 URL）".into(),
                    sensitive: false,
                },
                PluginCredentialField {
                    key: "intervalMinutes".into(),
                    label: "轮询间隔（分钟，5-1440）".into(),
                    sensitive: false,
                },
                PluginCredentialField {
                    key: "maxPerRound".into(),
                    label: "每轮阅读上限（1-50，超出顺延）".into(),
                    sensitive: false,
                },
                PluginCredentialField {
                    key: "push".into(),
                    label: "简报推送通道 id（逗号分隔，空=不推送）".into(),
                    sensitive: false,
                },
            ],
        }),
        watch: Some(PluginWatchDecl {
            interval_minutes: 30,
        }),
        resources: vec![],
        process: None,
    }
}

/// RSS 通道能力声明
struct RssChannel;

impl exm_core::plugin::ChannelProvider for RssChannel {
    fn kinds(&self) -> Vec<String> {
        vec!["rss".into()]
    }
    fn credential_fields(&self) -> Vec<PluginCredentialField> {
        rss_manifest()
            .channel
            .map(|c| c.credentials)
            .unwrap_or_default()
    }
}

/// RSS watcher 能力声明
struct RssWatch;

impl exm_core::plugin::WatchProvider for RssWatch {
    fn spawn(self: Arc<Self>, core: Arc<Core>) {
        spawn_supervisor(core);
    }
}

/// RSS 插件 provider（ChannelProvider + WatchProvider）
pub struct RssPlugin {
    manifest: PluginManifest,
}

impl RssPlugin {
    pub fn new() -> Self {
        RssPlugin {
            manifest: rss_manifest(),
        }
    }
}

impl Default for RssPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl exm_core::plugin::PluginProvider for RssPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }
    fn channel_provider(&self) -> Option<Arc<dyn exm_core::plugin::ChannelProvider>> {
        Some(Arc::new(RssChannel))
    }
    fn watch_provider(&self) -> Option<Arc<dyn exm_core::plugin::WatchProvider>> {
        Some(Arc::new(RssWatch))
    }
}

// ---------------------------------------------------------------- 单测

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_channel(feeds: &str, push: &str, max: u64) -> Channel {
        Channel {
            id: "rss-main".into(),
            kind: "rss".into(),
            enabled: true,
            group: None,
            allowed_chats: vec![],
            account: None,
            secret: None,
            reply_webhook: None,
            token: None,
            config: BTreeMap::from([
                ("feeds".to_string(), feeds.to_string()),
                ("push".to_string(), push.to_string()),
                ("maxPerRound".to_string(), max.to_string()),
                ("intervalMinutes".to_string(), "30".to_string()),
            ]),
            group_gate: None,
            capability: "chat".into(),
            memory_isolation: true,
            created_at: String::new(),
        }
    }

    #[test]
    fn 配置_订阅源解析() {
        let s = Settings::from_channel(&mock_channel(
            "Hacker News=https://hnrss.org/newest\nhttps://example.com/feed.xml",
            "ch-1,ch-2",
            10,
        ));
        assert_eq!(s.feeds.len(), 2);
        assert_eq!(s.feeds[0].0, "Hacker News");
        assert_eq!(s.feeds[1].0, "feed.xml", "裸 URL 缺省名取末段");
        assert_eq!(s.push, vec!["ch-1".to_string(), "ch-2".to_string()]);
        assert_eq!(s.max_per_round, 10);
        assert_eq!(s.interval_minutes, 30);
    }

    #[test]
    fn 状态_去重与持久化重启不重推() {
        let dir = std::env::temp_dir().join(format!("exm-rss-st-{}", exm_core::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        let core = Core::with_config(cfg).unwrap();
        let mut st = RssState::default();
        st.mark("https://x/1");
        st.mark("https://x/1"); // 幂等
        st.save(&core, "ch-a");
        // 重启（新实例读同一状态文件）
        let st2 = RssState::load(&core, "ch-a");
        assert!(st2.seen("https://x/1"), "重启后已见链接不重推");
        assert!(!st2.seen("https://x/2"));
        assert_eq!(st.seen_links.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 状态_上限淘汰() {
        let mut st = RssState::default();
        for i in 0..600 {
            st.mark(&format!("https://x/{i}"));
        }
        assert_eq!(st.seen_links.len(), 500, "上限 500 FIFO 淘汰");
        assert!(!st.seen("https://x/0"), "最旧应淘汰");
        assert!(st.seen("https://x/599"));
    }

    #[test]
    fn 工具_清单与注入面() {
        let m = rss_manifest();
        assert_eq!(m.id, "rss");
        assert_eq!(
            m.channel.as_ref().unwrap().kinds,
            vec!["rss".to_string()]
        );
        assert_eq!(m.channel.as_ref().unwrap().credentials.len(), 4);
        assert!(m.watch.is_some() && m.process.is_none());
        // 注入面：digest 工具名带命名空间
        let dir = std::env::temp_dir().join(format!("exm-rss-inj-{}", exm_core::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        let core = Arc::new(Core::with_config(cfg).unwrap());
        let inj = digest_injection(&core, &mock_channel("https://example.com/feed.xml", "", 10));
        assert_eq!(inj.len(), 1);
        assert_eq!(inj[0].0.name, "rss__digest");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 材料拼装：标题/链接/摘要齐全，无摘要条目降级占位
    #[test]
    fn 单轮_材料拼装() {
        let items = vec![
            (
                "源A".to_string(),
                feed::FeedItem {
                    title: "t1".into(),
                    link: "https://x/1".into(),
                    published: "Thu, 29 Oct 2026".into(),
                    summary: "s1".into(),
                },
            ),
            (
                "源B".to_string(),
                feed::FeedItem {
                    title: "t2".into(),
                    link: "https://x/2".into(),
                    published: String::new(),
                    summary: String::new(),
                },
            ),
        ];
        // 与 read_session 相同的拼装口径（抽出来便于断言）
        let mut material = String::from("以下是本轮 RSS 新条目。请逐条给出一句话要点（格式：标题｜要点），最后给一行整体摘要：\n");
        for (i, (src, it)) in items.iter().enumerate() {
            material.push_str(&format!(
                "\n{}. [{}] {}（{}）\n   {}",
                i + 1,
                src,
                it.title,
                it.link,
                if it.summary.is_empty() { "（无摘要）" } else { &it.summary }
            ));
        }
        assert!(material.contains("[源A] t1（https://x/1）"));
        assert!(material.contains("[源B] t2（https://x/2）"));
        assert!(material.contains("（无摘要）"));
    }

    /// 上限顺延：fresh 超 maxPerRound → 读 max 条、余量进 deferred
    #[test]
    fn 单轮_上限顺延切分() {
        let mut state = RssState::default();
        let mut fresh: Vec<(String, feed::FeedItem)> = (0..15)
            .map(|i| {
                (
                    "src".to_string(),
                    feed::FeedItem {
                        title: format!("t{i}"),
                        link: format!("https://x/{i}"),
                        published: String::new(),
                        summary: String::new(),
                    },
                )
            })
            .collect();
        let max = 10usize;
        let read: Vec<_> = if fresh.len() > max {
            let rest = fresh.split_off(max);
            state.deferred = rest.into_iter().map(|(_, it)| it).collect();
            fresh
        } else {
            fresh
        };
        assert_eq!(read.len(), 10);
        assert_eq!(state.deferred.len(), 5);
        assert_eq!(state.deferred[0].link, "https://x/10");
    }

    /// digest 源筛选与错误口径：不匹配报错（含可用源清单）；匹配规则 名称/URL/前缀
    #[tokio::test]
    async fn 工具_digest源筛选与错误() {
        let ch = mock_channel("HN=https://hnrss.org/newest\nhttps://example.com/feed.xml", "", 10);
        let s = Settings::from_channel(&ch);
        // 复刻 digest_exec 的筛选逻辑（执行面会拉网，这里只测匹配规则）
        let select = |want: Option<&str>| -> Vec<(String, String)> {
            s.feeds
                .iter()
                .filter(|(name, url)| match want {
                    Some(w) => name == w || url == w || url.starts_with(w),
                    None => true,
                })
                .cloned()
                .collect()
        };
        assert_eq!(select(None).len(), 2, "缺省全源");
        assert_eq!(select(Some("HN")).len(), 1, "名称精确匹配");
        assert_eq!(select(Some("https://example.com/feed.xml")).len(), 1, "URL 精确匹配");
        assert_eq!(select(Some("https://hnrss")).len(), 1, "URL 前缀匹配");
        assert!(select(Some("不存在")).is_empty(), "不匹配 → 执行面报「未匹配到订阅源」");
    }
}
