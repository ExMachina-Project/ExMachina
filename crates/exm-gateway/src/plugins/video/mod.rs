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
use std::sync::{Arc, Mutex, OnceLock};
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
    let wanted: HashMap<String, Channel> = platform::load_channels(core)
        .into_iter()
        .filter(|c| c.enabled && is_video_platform(&c.kind))
        .map(|c| (c.id.clone(), c))
        .collect();

    let mut map = runners().lock().unwrap();
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

struct Settings {
    watch: bool,
    comment: bool,
    reply: bool,
    dry_run: bool,
    interval_minutes: u64,
    max_actions: usize,
    daily_limit: u32,
    max_comment_chars: usize,
    source: String,
    keywords: Vec<String>,
    /// 内容倾向：自由文本提示词，整段注入观看提示作口味参考；分词仅用于候选软排序
    interests: String,
    reply_keywords: Vec<String>,
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
            max_comment_chars: cfg_u64(ch, "maxCommentChars", 120).clamp(20, 800) as usize,
            source: ch.cfg("source").unwrap_or_else(|| "popular".into()),
            keywords: split_list(ch.cfg("keywords").as_deref()),
            interests: ch.cfg("interests").unwrap_or_default(),
            reply_keywords: split_list(ch.cfg("replyKeywords").as_deref()),
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
struct VideoState {
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
    fn load(core: &Core, channel_id: &str) -> Self {
        let p = state_path(core, channel_id);
        std::fs::read_to_string(p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, core: &Core, channel_id: &str) {
        let p = state_path(core, channel_id);
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }

    fn reset_day(&mut self) {
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

    fn mark_comment(&mut self, key: &str) {
        if !self.seen_comments.iter().any(|k| k == key) {
            self.seen_comments.push(key.to_string());
            if self.seen_comments.len() > 1000 {
                let drop = self.seen_comments.len() - 1000;
                self.seen_comments.drain(0..drop);
            }
        }
    }

    fn comment_seen(&self, key: &str) -> bool {
        self.seen_comments.iter().any(|k| k == key)
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

// ---------------------------------------------------------------- 单轮

async fn tick(core: &Arc<Core>, ch: &Channel, s: &Settings) -> anyhow::Result<String> {
    if !s.watch && !s.comment && !s.reply {
        return Ok("主动观看与互动均已关闭".into());
    }
    // B 站互动前先养设备指纹（buvid3/4），缺指纹发评论易被静默风控
    if ch.kind == "bilibili" && (s.comment || s.reply) {
        bili_prime_buvid(ch).await;
    }
    let mut state = VideoState::load(core, &ch.id);
    state.reset_day();
    if (s.comment || s.reply) && state.actions_today >= s.daily_limit {
        return Ok(format!("今日互动已达上限（{} 次）", s.daily_limit));
    }

    let mut items = fetch_candidates(ch, s).await?;
    items.retain(|v| !state.seen(&video_key(v)));
    // 内容倾向（软偏好）：命中的视频排到队首优先观看，未命中的保留在后（不硬过滤）
    let interest_terms = split_list(Some(s.interests.as_str()));
    if !interest_terms.is_empty() {
        items.sort_by_key(|v| !matches_keywords(v, &interest_terms));
    }
    if !s.keywords.is_empty() {
        let filtered: Vec<VideoItem> = items
            .iter()
            .filter(|v| matches_keywords(v, &s.keywords))
            .cloned()
            .collect();
        // 热门流没有命中关键词时，尝试一次关键词搜索；失败则回退热门流（不中断轮询）
        if filtered.is_empty() && s.source != "search" && !s.keywords.is_empty() {
            if let Ok(mut searched) = search_candidates(ch, s).await {
                searched.retain(|v| !state.seen(&video_key(v)) && matches_keywords(v, &s.keywords));
                if !searched.is_empty() {
                    items = searched;
                }
            }
        } else {
            items = filtered;
        }
    }
    if items.is_empty() {
        return Ok("本轮没有新的候选视频".into());
    }

    let mut processed = 0usize;
    let mut posted = 0u32;
    for item in items.into_iter().take(s.max_actions) {
        let key = video_key(&item);
        match process_video(core, ch, s, &mut state, item).await {
            Ok(did_post) => {
                posted += did_post;
                processed += 1;
                state.mark_video(&key);
            }
            Err(e) => {
                // 单条失败不打断整轮；标记已见，避免同一坏数据每轮重试
                state.mark_video(&key);
                state.save(core, &ch.id);
                return Err(e);
            }
        }
        state.save(core, &ch.id);
    }
    state.save(core, &ch.id);
    let mode = if s.dry_run { "dry-run" } else { "已发布" };
    Ok(format!(
        "本轮观看 {processed} 条，互动 {posted} 次（{mode}）；今日互动 {}/{}",
        state.actions_today, s.daily_limit
    ))
}

/// 返回值 = 本轮实际发布的互动条数（dry-run 恒为 0）。
async fn process_video(
    core: &Arc<Core>,
    ch: &Channel,
    s: &Settings,
    state: &mut VideoState,
    mut item: VideoItem,
) -> anyhow::Result<u32> {
    let (material, detail) = watch_material(ch, &item).await?;
    item = detail;
    let comments = list_comments(ch, &item).await.unwrap_or_default();
    let prompt = build_prompt(ch, s, &item, &material, &comments);
    let raw = ask_agent(core, ch, &prompt).await?;
    let action = parse_action(&raw).ok_or_else(|| {
        anyhow::anyhow!(
            "智能体未返回约定 JSON：{}",
            raw.chars().take(200).collect::<String>()
        )
    })?;
    let has_comment = !action.comment.trim().is_empty();
    let has_reply = action
        .reply
        .as_ref()
        .map(|r| !r.text.trim().is_empty())
        .unwrap_or(false);
    if !has_comment && !has_reply && !action.skip_reason.trim().is_empty() {
        platform::report_status(
            &ch.id,
            "ok",
            format!("跳过：{}｜{}", item.title, action.skip_reason.trim()),
        );
    }

    let mut count = 0u32;
    let summary = clean_text(&action.summary, 400);
    if s.dry_run {
        platform::report_status(
            &ch.id,
            "ok",
            format!(
                "[dry-run] {}｜摘要：{}｜评论：{}｜回复：{}",
                item.title,
                summary,
                if action.comment.trim().is_empty() {
                    "—"
                } else {
                    action.comment.trim()
                },
                action
                    .reply
                    .as_ref()
                    .map(|r| r.text.trim())
                    .filter(|t| !t.is_empty())
                    .unwrap_or("—")
            ),
        );
        return Ok(0);
    }

    if s.comment {
        let text = clean_comment(&action.comment, s.max_comment_chars);
        if !text.is_empty() {
            post_comment(ch, &item, &text, None).await?;
            count += 1;
            state.actions_today += 1;
            platform::report_status(&ch.id, "ok", format!("已评论：{}｜{}", item.title, text));
        }
    }

    if s.reply {
        if let Some(reply) = action.reply.as_ref() {
            let to = reply_id(&reply.to);
            let text = clean_comment(&reply.text, s.max_comment_chars);
            let target = comments.iter().find(|c| c.id == to);
            if let Some(target) = target {
                if !text.is_empty()
                    && !state.comment_seen(&format!("{}:{}", item.id, target.id))
                    && (s.reply_keywords.is_empty()
                        || s.reply_keywords.iter().any(|k| target.text.contains(k)))
                {
                    post_comment(ch, &item, &text, Some(target.id.as_str())).await?;
                    state.mark_comment(&format!("{}:{}", item.id, target.id));
                    state.actions_today += 1;
                    count += 1;
                    platform::report_status(
                        &ch.id,
                        "ok",
                        format!("已回复 @{}：{}", target.author, text),
                    );
                }
            }
        }
    }

    Ok(count)
}

// ---------------------------------------------------------------- 智能体调用

async fn ask_agent(core: &Arc<Core>, ch: &Channel, prompt: &str) -> anyhow::Result<String> {
    let (run, mut rx) = ChannelRun::begin(core, ch, "video")
        .await
        .ok_or_else(|| anyhow::anyhow!("视频通道会话建立失败"))?;
    // 系统自动任务标记：落库消息带 auto:video 来源，展示层与真人消息区分
    let run = run.with_origin("auto:video");
    let sid = run.session_id.clone();
    let reply_run = run.clone();
    let prompt = prompt.to_string();
    let task = tokio::spawn(async move { run.run(&prompt).await });

    let mut out = String::new();
    let wait = async {
        loop {
            match rx.recv().await {
                Ok(evt) if evt.session_id == sid => {
                    if let Some(text) = reply_run.take_reply(&evt, 20_000) {
                        out = text;
                        break;
                    }
                }
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    };
    match tokio::time::timeout(Duration::from_secs(300), wait).await {
        Ok(_) => {}
        Err(_) => {
            // 超时中止：ChannelRun::run 内部的组还原 guard 会随 Drop 执行
            task.abort();
            anyhow::bail!("视频观看推理超时（300s）");
        }
    }
    let _ = task.await;
    if out.trim().is_empty() {
        anyhow::bail!("智能体没有产出收束文本");
    }
    Ok(out)
}

fn build_prompt(
    _ch: &Channel,
    s: &Settings,
    item: &VideoItem,
    material: &str,
    comments: &[CommentItem],
) -> String {
    let mut c = String::new();
    for x in comments.iter().take(12) {
        c.push_str(&format!(
            "- id={}｜{}｜赞{}｜{}\n",
            x.id,
            x.author,
            x.likes,
            x.text.replace('\n', " ")
        ));
    }
    if c.is_empty() {
        c.push_str("（暂无可见评论）\n");
    }
    let comment_rule = if s.comment {
        format!(
            "写一条具体、有信息量的中文评论，不超过 {} 字；不复读标题、不空泛夸赞、不营销腔，不提自己是 AI。语气保持你自身的说话风格——观众感来自内容具体，不靠刻意模仿人类口吻。",
            s.max_comment_chars
        )
    } else {
        "不要写公开评论（comment 字段留空）。".into()
    };
    let reply_rule = if s.reply {
        format!(
            "从评论区挑选一条最值得回应的评论（排除自己）写回复，不超过 {} 字；reply.to 必须使用上面列出的 id，不能编造。没有合适目标时 reply 设为 null。",
            s.max_comment_chars
        )
    } else {
        "不要回复评论（reply 设为 null）。".into()
    };
    let style = if s.comment_style.is_empty() {
        String::new()
    } else {
        format!("【风格要求】{}（与本账号人设冲突时，以人设为准）\n", s.comment_style)
    };
    // 内容倾向：观看与互动的口味参考（软偏好，不排斥其他内容）
    let taste = if s.interests.trim().is_empty() {
        String::new()
    } else {
        format!("【内容倾向】{}\n这是账号主理人给的内容口味提示——命中倾向的视频可以多聊、多深挖，未命中的正常观看即可，不必硬凑，也不要在评论里复述这段要求。\n", s.interests.trim())
    };
    let published = if item.published_at > 0 {
        chrono::DateTime::from_timestamp(item.published_at, 0)
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "未知".into())
    } else {
        "未知".into()
    };
    format!(
        "你是 EXMACHINA 在视频平台的观看与互动代理。请先认真“看”完下面的视频材料，再决定如何互动。\n\n\
【视频资料】\n平台：{}\n标题：{}\n作者：{}\n链接：{}\n播放量：{}\n发布时间：{}\n\n{}\n\n\
【评论区摘录（按热度）】\n{}\n\
【任务】\n\
1. 以观众视角概括视频内容、亮点或问题（summary，80 字以内）。summary 用你自己的语言，保持你一贯的说话方式。\n\
2. {} \n\
3. {} \n\
{}{}\
【硬性要求】\n\
- 你的人格、语气、口头禅全部沿用你自身的设定（SOUL/人设）；本任务不要求你扮演别人，只用你自己的方式谈论视频内容。\n\
- 只输出一个 JSON 对象，不要 Markdown 代码块、不要解释、不要使用任何工具。\n\
- 格式：{{\"summary\":\"...\",\"comment\":\"...\",\"reply\":{{\"to\":\"评论id\",\"text\":\"...\"}}}}；不回复时 reply 为 null。\n\
- 不要捏造视频里没有的事实；看不懂或材料不足时在 summary 说明，评论/回复宁可留空。\n",
        item.platform_label(),
        item.title,
        item.author,
        item.url,
        item.views,
        published,
        material,
        c,
        comment_rule,
        reply_rule,
        style,
        taste
    )
}

#[derive(Debug, Default, Deserialize)]
struct AgentAction {
    #[serde(default)]
    summary: String,
    #[serde(default)]
    comment: String,
    #[serde(default)]
    reply: Option<AgentReply>,
    #[serde(default, alias = "skipReason")]
    skip_reason: String,
}

#[derive(Debug, Deserialize)]
struct AgentReply {
    #[serde(default)]
    to: Value,
    #[serde(default)]
    text: String,
}

fn parse_action(raw: &str) -> Option<AgentAction> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<AgentAction>(&raw[start..=end]).ok()
}

fn reply_id(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn clean_text(raw: &str, max: usize) -> String {
    let s = raw
        .trim()
        .trim_matches('`')
        .trim()
        .trim_matches('"')
        .trim()
        .replace("\r\n", "\n")
        .replace('\n', " ");
    let mut out = String::new();
    let mut last_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(ch);
            last_space = false;
        }
    }
    let out = out.trim().to_string();
    if out.chars().count() > max {
        out.chars().take(max).collect()
    } else {
        out
    }
}

/// 评论清洗：在 clean_text 基础上剥离 Markdown/装饰符号。
/// B 站评论不支持 Markdown，`**`、`#`、`>` 等会原样上屏且是审核拦截的高发因素；
/// 智能体产出常带人设口癖装饰（▸、⚠ 等），同样易触发风控，一并去除。
fn clean_comment(raw: &str, max: usize) -> String {
    let s = clean_text(raw, max * 2);
    let mut out = String::new();
    for ch in s.chars() {
        match ch {
            '*' | '#' | '`' | '>' | '[' | ']' | '~' | '_' | '|' | '▸' | '◀' | '▶' | '«' | '»'
            | '⚠' => {} // 剥离 Markdown 与人设装饰符号（【】为中文常用标点，保留）
            _ => out.push(ch),
        }
    }
    // 折叠剥离后产生的连续空格
    let mut collapsed = String::new();
    let mut last_space = false;
    for ch in out.trim().chars() {
        if ch == ' ' {
            if !last_space {
                collapsed.push(' ');
            }
            last_space = true;
        } else {
            collapsed.push(ch);
            last_space = false;
        }
    }
    let collapsed = collapsed.trim().to_string();
    if collapsed.chars().count() > max {
        collapsed.chars().take(max).collect()
    } else {
        collapsed
    }
}

// ---------------------------------------------------------------- 平台动作分发

#[derive(Clone, Debug)]
struct VideoItem {
    platform: String,
    id: String,
    aid: Option<u64>,
    title: String,
    description: String,
    author: String,
    views: u64,
    url: String,
    published_at: i64,
}

impl VideoItem {
    fn platform_label(&self) -> &str {
        match self.platform.as_str() {
            "bilibili" => "B站",
            "youtube" => "YouTube",
            other => other,
        }
    }
}

#[derive(Clone, Debug)]
struct CommentItem {
    id: String,
    author: String,
    text: String,
    likes: u64,
}

fn video_key(v: &VideoItem) -> String {
    format!("{}:{}", v.platform, v.id)
}

fn matches_keywords(v: &VideoItem, keywords: &[String]) -> bool {
    let hay = format!("{} {} {}", v.title, v.description, v.author).to_lowercase();
    keywords.iter().any(|k| hay.contains(&k.to_lowercase()))
}

async fn fetch_candidates(ch: &Channel, s: &Settings) -> anyhow::Result<Vec<VideoItem>> {
    match ch.kind.as_str() {
        "bilibili" => {
            // 内容倾向先行：popular 模式且配置了倾向时，用倾向词主动搜索（权重高于热门榜单），
            // 命中倾向的搜索结果排前、热门流补位——而不是只对 20 条热门做事后排序
            if s.source == "search" && !s.keywords.is_empty() {
                if let Ok(list) = search_candidates(ch, s).await {
                    if !list.is_empty() {
                        return Ok(list);
                    }
                }
            }
            let mut items = bili_popular(ch, 20).await?;
            // 倾向「选择」候选：从自由文本倾向提取核心搜索词，主动搜索替代热门流——
            // 让倾向在选视频之前起效，而不是对热门榜做事后排序
            let interest_terms = split_list(Some(s.interests.as_str()));
            let search_terms = interest_search_terms(&s.interests);
            if !search_terms.is_empty() {
                // 游标轮换：每轮用不同的倾向词搜索，避免反复搜同一个词刷到同一批
                let term = search_terms[interest_cursor(ch, &search_terms)].clone();
                if let Ok(mut hits) = bili_search(ch, &term, s).await {
                    hits.retain(|v| matches_keywords(v, &interest_terms) || matches_keywords(v, &search_terms));
                    if !hits.is_empty() {
                        items = hits; // 倾向搜索命中优先，热门流丢弃（本轮不混排，倾向即选择）
                    }
                }
            }
            Ok(items)
        }
        "youtube" => {
            if s.source == "search" && !s.keywords.is_empty() {
                if let Ok(list) = search_candidates(ch, s).await {
                    if !list.is_empty() {
                        return Ok(list);
                    }
                }
            }
            yt_popular(ch, s).await
        }
        other => anyhow::bail!("不支持的视频平台：{other}"),
    }
}

/// 倾向词轮换游标：按通道记忆轮次取模，跨轮次推进避免反复搜同一个词
fn interest_cursor(ch: &Channel, terms: &[String]) -> usize {
    static CURSORS: OnceLock<std::sync::Mutex<std::collections::HashMap<String, usize>>> =
        OnceLock::new();
    let map = CURSORS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut guard = map.lock().expect("interest cursor lock");
    let n = guard.entry(ch.id.clone()).or_insert(0);
    let cur = *n;
    *n = n.wrapping_add(1);
    cur % terms.len().max(1)
}

/// 从自由文本倾向里提取可用的搜索词：倾向是「只查看AI相关内容，优先二创其次是技术类。」
/// 这类长句，整句直接搜索噪音大，匹配也难命中。按引导虚词二次切分，取剩下的核心词。
/// 例：「优先二创其次是技术类」→ ["二创", "技术类"]；「只查看AI相关内容」→ ["AI"]。
fn interest_search_terms(interests: &str) -> Vec<String> {
    const PREFIX_FILLERS: &[&str] = &[
        "只查看", "只看", "优先看", "优先", "偏向", "偏爱", "喜欢看", "喜欢", "偏好", "多看",
        "少看", "不要看", "不看", "避免看", "避免", "查看",
    ];
    const JOINERS: &[&str] = &[
        "相关", "其次是", "其次", "类型", "类的", "类", "为主", "以及", "还有", "都行", "的",
    ];
    let mut out = Vec::new();
    for seg in split_list(Some(interests)) {
        // 剥离句首引导虚词，得到核心片段
        let mut cur = seg.as_str();
        loop {
            let mut changed = false;
            for f in PREFIX_FILLERS {
                if let Some(rest) = cur.strip_prefix(f) {
                    cur = rest;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let cur = cur.trim_matches(['。', '！', '？', '，', '；', ' ']);
        if cur.is_empty() {
            continue;
        }
        // 句中连接虚词切分：核心词在连接词之前（「AI相关」→ AI；「二创其次技术类」→ 二创、技术类）
        let mut parts = vec![cur.to_string()];
        for j in JOINERS {
            let mut next = Vec::new();
            for p in parts {
                for seg in p.split(j) {
                    next.push(seg.to_string());
                }
            }
            parts = next;
        }
        for part in parts {
            let core = part.trim();
            // 泛词过滤：「内容」「视频」这类词做搜索词噪音太大
            const GENERIC: &[&str] = &["内容", "视频", "东西", "玩意"];
            if core.chars().count() > 1
                && !GENERIC.contains(&core)
                && !out.iter().any(|o: &String| o.contains(core) || core.contains(o.as_str()))
            {
                out.push(core.to_string());
            }
        }
    }
    out
}

async fn search_candidates(ch: &Channel, s: &Settings) -> anyhow::Result<Vec<VideoItem>> {
    match ch.kind.as_str() {
        "bilibili" => {
            let q = s.keywords.first().cloned().unwrap_or_default();
            if q.is_empty() {
                return Ok(Vec::new());
            }
            bili_search(ch, &q, s).await
        }
        "youtube" => {
            let q = s.keywords.join(" ");
            if q.trim().is_empty() {
                return Ok(Vec::new());
            }
            yt_search(ch, &q, s).await
        }
        _ => Ok(Vec::new()),
    }
}

async fn watch_material(ch: &Channel, item: &VideoItem) -> anyhow::Result<(String, VideoItem)> {
    match ch.kind.as_str() {
        "bilibili" => bili_watch_material(ch, item).await,
        "youtube" => {
            let mut material = format!(
                "简介：\n{}\n\n（YouTube 未提供字幕下载时，以简介与评论区作为观看材料；不伪造播放行为。）",
                item.description
            );
            if let Ok(comments) = yt_comments(ch, item, 8).await {
                if !comments.is_empty() {
                    material.push_str("\n\n观众反应摘录：\n");
                    for c in comments {
                        material.push_str(&format!(
                            "- {}：{}\n",
                            c.author,
                            c.text.replace('\n', " ")
                        ));
                    }
                }
            }
            Ok((material, item.clone()))
        }
        other => anyhow::bail!("不支持的视频平台：{other}"),
    }
}

async fn list_comments(ch: &Channel, item: &VideoItem) -> anyhow::Result<Vec<CommentItem>> {
    match ch.kind.as_str() {
        "bilibili" => bili_comments(ch, item).await,
        "youtube" => yt_comments(ch, item, 15).await,
        _ => Ok(Vec::new()),
    }
}

async fn post_comment(
    ch: &Channel,
    item: &VideoItem,
    text: &str,
    reply_to: Option<&str>,
) -> anyhow::Result<()> {
    match ch.kind.as_str() {
        "bilibili" => {
            let aid = item
                .aid
                .ok_or_else(|| anyhow::anyhow!("B 站视频缺少 aid，无法发评论"))?;
            bili_post_comment(ch, aid, text, reply_to).await
        }
        "youtube" => yt_post_comment(ch, item, text, reply_to).await,
        other => anyhow::bail!("不支持的视频平台：{other}"),
    }
}

// ---------------------------------------------------------------- B 站

fn http_client() -> reqwest::Client {
    // 共享单例：reqwest::Client 内部自带连接池，每请求新建会反复拆建 TCP+TLS
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .user_agent(UA)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new())
        })
        .clone()
}

fn bili_cookie(ch: &Channel) -> String {
    let mut parts = Vec::new();
    if let Some(v) = ch.cfg("sessdata") {
        parts.push(format!("SESSDATA={v}"));
    }
    if let Some(v) = ch.cfg("biliJct") {
        parts.push(format!("bili_jct={v}"));
    }
    if let Some(v) = ch.cfg("uid") {
        parts.push(format!("DedeUserID={v}"));
    }
    // 设备指纹：缺 buvid3 时发评论常被静默风控（code=0 但评论进自查不上屏）。
    // 优先用匿名访问 B 站时服务端种下的 buvid3/buvid4（内存缓存跨请求复用）。
    if let Some(b) = buvid_cached(ch) {
        parts.push(format!("buvid3={}", b.0));
        if let Some(b4) = b.1 {
            parts.push(format!("buvid4={b4}"));
        }
    }
    parts.join("; ")
}

/// 通道级 buvid 缓存（共享单例）：(buvid3, Option<buvid4>)
fn buvid_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, (String, Option<String>)>> {
    static CACHE: OnceLock<std::sync::Mutex<std::collections::HashMap<String, (String, Option<String>)>>> =
        OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// 读取缓存的 buvid（无则 None——发评论前应先 bili_prime_buvid）
fn buvid_cached(ch: &Channel) -> Option<(String, Option<String>)> {
    buvid_cache().lock().expect("buvid cache lock").get(&ch.id).cloned()
}

/// 异步获取并缓存 buvid（观看轮次开始时调用一次；匿名访问首页即下发）
async fn bili_prime_buvid(ch: &Channel) {
    let need = !buvid_cache().lock().expect("buvid cache lock").contains_key(&ch.id);
    if !need {
        return;
    }
    let resp = match http_client()
        .get("https://www.bilibili.com/")
        .header(reqwest::header::USER_AGENT, UA)
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return,
    };
    let mut buvid3 = None;
    let mut buvid4 = None;
    for cv in resp.headers().get_all(reqwest::header::SET_COOKIE) {
        if let Ok(s) = cv.to_str() {
            for pair in s.split(';') {
                let pair = pair.trim();
                if let Some(v) = pair.strip_prefix("buvid3=") {
                    buvid3 = Some(v.to_string());
                } else if let Some(v) = pair.strip_prefix("buvid4=") {
                    buvid4 = Some(v.to_string());
                }
            }
        }
    }
    if let Some(b3) = buvid3 {
        buvid_cache()
            .lock()
            .expect("buvid cache lock")
            .insert(ch.id.clone(), (b3, buvid4));
    }
}

fn bili_request(ch: &Channel, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    bili_request_with_referer(ch, BILI_REFERER, rb)
}

fn bili_request_with_referer(
    ch: &Channel,
    referer: &str,
    rb: reqwest::RequestBuilder,
) -> reqwest::RequestBuilder {
    let rb = rb
        .header(reqwest::header::USER_AGENT, UA)
        .header(reqwest::header::REFERER, referer);
    let cookie = bili_cookie(ch);
    if cookie.is_empty() {
        rb
    } else {
        rb.header(reqwest::header::COOKIE, cookie)
    }
}

async fn bili_get(ch: &Channel, url: &str) -> anyhow::Result<Value> {
    let resp = bili_request(ch, http_client().get(url))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("B 站请求失败：{e}"))?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("B 站响应不是 JSON（HTTP {status}）：{e}"))?;
    if !status.is_success() {
        anyhow::bail!("B 站 HTTP {status}：{}", api_message(&body));
    }
    Ok(body)
}

fn api_message(v: &Value) -> String {
    v.get("message")
        .and_then(|m| m.as_str())
        .or_else(|| v.get("error").and_then(|m| m.as_str()))
        .unwrap_or("未知平台错误")
        .to_string()
}

fn api_ok(v: &Value) -> anyhow::Result<()> {
    let code = v.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
    if code == 0 {
        Ok(())
    } else {
        anyhow::bail!("平台返回 code={code}：{}", api_message(v))
    }
}

async fn bili_popular(ch: &Channel, limit: usize) -> anyhow::Result<Vec<VideoItem>> {
    let url = format!("https://api.bilibili.com/x/web-interface/popular?ps={limit}&pn=1");
    let body = bili_get(ch, &url).await?;
    api_ok(&body)?;
    let list = body
        .pointer("/data/list")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(list
        .iter()
        .filter_map(|x| {
            let bvid = x.get("bvid")?.as_str()?.to_string();
            Some(VideoItem {
                platform: "bilibili".into(),
                id: bvid.clone(),
                aid: x.get("aid").and_then(as_u64),
                title: strip_html(x.get("title").and_then(|v| v.as_str()).unwrap_or("")),
                description: x
                    .get("desc")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                author: x
                    .pointer("/owner/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                views: x.pointer("/stat/view").and_then(as_u64).unwrap_or(0),
                url: format!("https://www.bilibili.com/video/{bvid}"),
                published_at: x.get("pubdate").and_then(as_i64).unwrap_or(0),
            })
        })
        .collect())
}

async fn bili_search(ch: &Channel, keyword: &str, _s: &Settings) -> anyhow::Result<Vec<VideoItem>> {
    // 搜索接口风控比普通接口严：无 buvid 指纹或 Referer 不符会返回 HTML 拦截页（实测）。
    // Referer 用搜索页；buvid 由 tick 开头的 bili_prime_buvid 预热进 cookie。
    let resp = bili_request_with_referer(
        ch,
        "https://search.bilibili.com/",
        http_client()
            .get("https://api.bilibili.com/x/web-interface/search/type")
            .query(&[
                ("search_type", "video"),
                ("keyword", keyword),
                ("page", "1"),
            ]),
    )
    .send()
    .await
    .map_err(|e| anyhow::anyhow!("B 站搜索失败：{e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| anyhow::anyhow!("B 站搜索响应读取失败：{e}"))?;
    // 风控拦截页返回 HTML 而非 JSON：按搜索失败处理（调用方回退热门流），不当硬错误
    if !text.trim_start().starts_with('{') {
        anyhow::bail!("B 站搜索被风控拦截（返回非 JSON）");
    }
    let body: Value = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("B 站搜索响应解析失败（HTTP {status}）：{e}"))?;
    api_ok(&body)?;
    let list = body
        .pointer("/data/result")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(list
        .iter()
        .filter_map(|x| {
            let bvid = x.get("bvid")?.as_str()?.to_string();
            Some(VideoItem {
                platform: "bilibili".into(),
                id: bvid.clone(),
                aid: x.get("aid").and_then(as_u64),
                title: strip_html(x.get("title").and_then(|v| v.as_str()).unwrap_or("")),
                description: strip_html(
                    x.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                ),
                author: strip_html(x.get("author").and_then(|v| v.as_str()).unwrap_or("")),
                views: x.get("play").and_then(as_u64).unwrap_or(0),
                url: format!("https://www.bilibili.com/video/{bvid}"),
                published_at: x.get("pubdate").and_then(as_i64).unwrap_or(0),
            })
        })
        .collect())
}

async fn bili_watch_material(
    ch: &Channel,
    item: &VideoItem,
) -> anyhow::Result<(String, VideoItem)> {
    let body = bili_get(
        ch,
        &format!(
            "https://api.bilibili.com/x/web-interface/view?bvid={}",
            item.id
        ),
    )
    .await?;
    api_ok(&body)?;
    let data = body.get("data").cloned().unwrap_or(Value::Null);
    let mut out = item.clone();
    out.aid = data.get("aid").and_then(as_u64).or(out.aid);
    out.title = data
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or(out.title);
    out.description = data
        .get("desc")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    out.author = data
        .pointer("/owner/name")
        .and_then(|v| v.as_str())
        .unwrap_or(&out.author)
        .to_string();
    out.views = data
        .pointer("/stat/view")
        .and_then(as_u64)
        .unwrap_or(out.views);
    let cid = data
        .pointer("/pages/0/cid")
        .and_then(as_u64)
        .or_else(|| data.get("cid").and_then(as_u64))
        .unwrap_or(0);

    let mut material = format!("简介：\n{}\n", out.description);
    if cid > 0 {
        if let Ok(v) = bili_get(
            ch,
            &format!(
                "https://api.bilibili.com/x/player/v2?bvid={}&cid={}",
                out.id, cid
            ),
        )
        .await
        {
            if let Some(subs) = v
                .pointer("/data/subtitle/subtitles")
                .and_then(|s| s.as_array())
            {
                if let Some(url) = subs
                    .iter()
                    .find(|s| {
                        s.get("lan")
                            .and_then(|l| l.as_str())
                            .map(|l| l.contains("zh"))
                            .unwrap_or(false)
                    })
                    .or_else(|| subs.first())
                    .and_then(|s| s.get("subtitle_url"))
                    .and_then(|u| u.as_str())
                {
                    let url = if url.starts_with("//") {
                        format!("https:{url}")
                    } else {
                        url.to_string()
                    };
                    if let Ok(sub) = bili_get(ch, &url).await {
                        let text = sub
                            .pointer("/body")
                            .and_then(|b| b.as_array())
                            .map(|lines| {
                                lines
                                    .iter()
                                    .filter_map(|l| l.get("content").and_then(|c| c.as_str()))
                                    .collect::<Vec<_>>()
                                    .join(" ")
                            })
                            .unwrap_or_default();
                        if !text.trim().is_empty() {
                            material.push_str("\n字幕（观看材料）：\n");
                            material.push_str(&text.chars().take(8_000).collect::<String>());
                        }
                    }
                }
            }
        }
    }
    if material.trim().len() < 20 {
        material.push_str("\n（该视频没有可用字幕，以上简介即为观看材料。）");
    }
    Ok((material, out))
}

async fn bili_comments(ch: &Channel, item: &VideoItem) -> anyhow::Result<Vec<CommentItem>> {
    let aid = item.aid.ok_or_else(|| anyhow::anyhow!("缺少 aid"))?;
    let body = bili_get(
        ch,
        &format!("https://api.bilibili.com/x/v2/reply?type=1&oid={aid}&pn=1&ps=20&sort=2"),
    )
    .await?;
    api_ok(&body)?;
    let owner_mid = ch.cfg("uid");
    let replies = body
        .pointer("/data/replies")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for r in replies {
        let id = r
            .get("rpid")
            .and_then(as_u64)
            .map(|v| v.to_string())
            .unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        let mid = r.pointer("/member/mid").and_then(as_u64);
        if let (Some(owner), Some(mid)) = (owner_mid.as_deref(), mid) {
            if owner == mid.to_string() {
                continue;
            }
        }
        out.push(CommentItem {
            id,
            author: r
                .pointer("/member/uname")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            text: r
                .pointer("/content/message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            likes: r.get("like").and_then(as_u64).unwrap_or(0),
        });
        if out.len() >= 15 {
            break;
        }
    }
    Ok(out)
}

async fn bili_post_comment(
    ch: &Channel,
    aid: u64,
    text: &str,
    reply_to: Option<&str>,
) -> anyhow::Result<()> {
    let csrf = ch
        .cfg("biliJct")
        .ok_or_else(|| anyhow::anyhow!("bilibili 缺少 biliJct（CSRF），无法发评论"))?;
    // serde_urlencoded 只接受 map/struct；Vec<(k,v)> 会被序列化成数组而失败
    let mut form: std::collections::BTreeMap<&str, String> = std::collections::BTreeMap::new();
    form.insert("oid", aid.to_string());
    form.insert("type", "1".to_string());
    form.insert("message", text.to_string());
    form.insert("csrf", csrf);
    form.insert("plat", "1".to_string());
    if let Some(root) = reply_to {
        form.insert("root", root.to_string());
        form.insert("parent", root.to_string());
    }
    let resp = bili_request(
        ch,
        http_client().post("https://api.bilibili.com/x/v2/reply/add"),
    )
    .form(&form)
    .send()
    .await
    .map_err(|e| anyhow::anyhow!("B 站评论请求失败：{e}"))?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("B 站评论响应不是 JSON（HTTP {status}）：{e}"))?;
    if !status.is_success() {
        anyhow::bail!("B 站评论 HTTP {status}：{}", api_message(&body));
    }
    api_ok(&body)?;
    // 发后验证：B 站可能 code=0 但评论进自查/风控不上屏（静默拦截）。
    // 用「按时间倒序」接口验证——自己的新评论必然在第一页前排（热度排序会漏）。
    // 双次确认（4s + 6s）：容忍审核短暂延迟；未上屏才如实报错，
    // 避免 actions_today 虚增、状态条谎报「已评论」。
    let uid = ch.cfg("uid").unwrap_or_default();
    if !uid.is_empty() {
        let mut visible = false;
        for wait in [4u64, 6] {
            tokio::time::sleep(Duration::from_secs(wait)).await;
            match bili_comment_visible(ch, aid, &uid).await {
                Ok(true) => {
                    visible = true;
                    break;
                }
                Ok(false) => continue,
                Err(e) => {
                    // 验证本身失败不判定发布结果（评论可能仍在审核中），只记日志
                    eprintln!("[video:bilibili] 评论上屏验证失败（不判定发布结果）：{e}");
                    visible = true;
                    break;
                }
            }
        }
        if !visible {
            anyhow::bail!(
                "评论提交返回成功但未在评论区上屏——疑似被 B 站审核/风控拦截（建议降低频次或检查账号状态）"
            );
        }
    }
    Ok(())
}

/// 检查指定 uid 的评论是否出现在该视频评论区（按时间倒序第一页，新评论必在前排）
async fn bili_comment_visible(ch: &Channel, aid: u64, uid: &str) -> anyhow::Result<bool> {
    let body = bili_get(
        ch,
        &format!(
            "https://api.bilibili.com/x/v2/reply?pn=1&ps=20&oid={aid}&type=1&sort=2"
        ),
    )
    .await?;
    let replies = body
        .pointer("/data/replies")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for r in replies {
        let mid = r
            .pointer("/member/mid")
            .and_then(as_u64)
            .map(|v| v.to_string())
            .unwrap_or_default();
        if mid == uid {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) enum ProbeError {
    Missing(String),
    Auth(String),
    Network(String),
}

// ---------------------------------------------------------------- B 站扫码登录
// passport 开放接口：generate 出码 → 客户端扫 → poll 轮询；确认后 Set-Cookie 下发登录态。

pub(crate) struct BiliQrStart {
    pub qrcode_key: String,
    pub url: String,
}

pub(crate) struct BiliQrLogin {
    pub sessdata: String,
    pub bili_jct: String,
    pub dede_user_id: String,
    pub uname: String,
}

/// 生成扫码登录二维码（B 站 passport web 协议）
pub(crate) async fn bili_qr_generate() -> anyhow::Result<BiliQrStart> {
    let resp = http_client()
        .get("https://passport.bilibili.com/x/passport-login/web/qrcode/generate")
        .header(reqwest::header::USER_AGENT, UA)
        .header(reqwest::header::REFERER, BILI_REFERER)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("B 站请求失败：{e}"))?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("B 站响应不是 JSON（HTTP {status}）：{e}"))?;
    if body.get("code").and_then(|c| c.as_i64()) != Some(0) {
        anyhow::bail!("出码失败：{}", api_message(&body));
    }
    let qrcode_key = body
        .pointer("/data/qrcode_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("响应缺 qrcode_key"))?
        .to_string();
    let url = body
        .pointer("/data/url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("响应缺 url"))?
        .to_string();
    Ok(BiliQrStart { qrcode_key, url })
}

/// 轮询扫码状态：code 0 = 已确认（Set-Cookie 带登录态）；86101 未扫；86090 已扫待确认；86083 过期
pub(crate) async fn bili_qr_poll(qrcode_key: &str) -> anyhow::Result<Result<BiliQrLogin, i64>> {
    let resp = http_client()
        .get("https://passport.bilibili.com/x/passport-login/web/qrcode/poll")
        .query(&[("qrcode_key", qrcode_key)])
        .header(reqwest::header::USER_AGENT, UA)
        .header(reqwest::header::REFERER, BILI_REFERER)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("B 站请求失败：{e}"))?;
    let status = resp.status();
    // 先取 Set-Cookie 再消费 body
    let cookies: Vec<String> = resp
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok().map(|s| s.to_string()))
        .collect();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("B 站响应不是 JSON（HTTP {status}）：{e}"))?;
    // 外层 code 是 HTTP 层结果；扫码状态在 data.code：0 已确认 / 86101 未扫 / 86090 待确认 / 86083 过期
    let code = body
        .pointer("/data/code")
        .and_then(|c| c.as_i64())
        .unwrap_or(-1);
    if code != 0 {
        // 未扫/待确认/过期：把业务码交调用方决定轮询节奏
        return Ok(Err(code));
    }
    let pick = |name: &str| -> Option<String> {
        cookies.iter().find_map(|c| {
            let c = c.trim();
            let kv = c.split(';').next()?.trim();
            let (k, v) = kv.split_once('=')?;
            (k.trim() == name && !v.is_empty()).then(|| v.to_string())
        })
    };
    let sessdata = pick("SESSDATA").ok_or_else(|| anyhow::anyhow!("登录成功但响应缺 SESSDATA Cookie"))?;
    let bili_jct = pick("bili_jct").ok_or_else(|| anyhow::anyhow!("登录成功但响应缺 bili_jct Cookie"))?;
    let dede_user_id = pick("DedeUserID").unwrap_or_default();
    // 用新 Cookie 查一次 nav 拿用户名，顺带校验登录态真的可用
    let mut cookie_parts = vec![format!("SESSDATA={sessdata}"), format!("bili_jct={bili_jct}")];
    if !dede_user_id.is_empty() {
        cookie_parts.push(format!("DedeUserID={dede_user_id}"));
    }
    let nav: Value = http_client()
        .get("https://api.bilibili.com/x/web-interface/nav")
        .header(reqwest::header::USER_AGENT, UA)
        .header(reqwest::header::REFERER, BILI_REFERER)
        .header(reqwest::header::COOKIE, cookie_parts.join("; "))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("校验登录态失败：{e}"))?
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("nav 响应不是 JSON：{e}"))?;
    if nav.get("code").and_then(|c| c.as_i64()) != Some(0)
        || !nav.pointer("/data/isLogin").and_then(|v| v.as_bool()).unwrap_or(false)
    {
        anyhow::bail!("扫码成功但登录态校验未通过：{}", api_message(&nav));
    }
    let uname = nav
        .pointer("/data/uname")
        .and_then(|v| v.as_str())
        .unwrap_or("bilibili")
        .to_string();
    Ok(Ok(BiliQrLogin { sessdata, bili_jct, dede_user_id, uname }))
}

pub(crate) async fn probe_bilibili(ch: &Channel) -> Result<String, ProbeError> {
    if ch.cfg("sessdata").is_none() || ch.cfg("biliJct").is_none() {
        return Err(ProbeError::Missing("SESSDATA / biliJct 登录 Cookie".into()));
    }
    let body = bili_get(ch, "https://api.bilibili.com/x/web-interface/nav")
        .await
        .map_err(|e| ProbeError::Network(e.to_string()))?;
    let code = body.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
    let login = body
        .pointer("/data/isLogin")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if code == 0 && login {
        let name = body
            .pointer("/data/uname")
            .and_then(|v| v.as_str())
            .unwrap_or("bilibili");
        Ok(name.to_string())
    } else {
        Err(ProbeError::Auth(format!(
            "code={code} {}",
            api_message(&body)
        )))
    }
}

// ---------------------------------------------------------------- YouTube

fn yt_key(ch: &Channel) -> anyhow::Result<String> {
    ch.cfg("apiKey")
        .ok_or_else(|| anyhow::anyhow!("youtube 缺少 config.apiKey"))
}

async fn yt_get(path: &str, params: &[(&str, String)]) -> anyhow::Result<Value> {
    let resp = http_client()
        .get(format!("https://www.googleapis.com/youtube/v3/{path}"))
        .query(params)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("YouTube 请求失败：{e}"))?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("YouTube 响应不是 JSON（HTTP {status}）：{e}"))?;
    if !status.is_success() {
        anyhow::bail!("YouTube HTTP {status}：{}", api_message(&body));
    }
    Ok(body)
}

async fn yt_popular(ch: &Channel, s: &Settings) -> anyhow::Result<Vec<VideoItem>> {
    let key = yt_key(ch)?;
    let body = yt_get(
        "videos",
        &[
            ("part", "snippet,statistics".into()),
            ("chart", "mostPopular".into()),
            ("maxResults", "20".into()),
            ("regionCode", s.region.clone()),
            ("key", key),
        ],
    )
    .await?;
    Ok(body
        .get("items")
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(yt_item).collect())
        .unwrap_or_default())
}

async fn yt_search(ch: &Channel, q: &str, _s: &Settings) -> anyhow::Result<Vec<VideoItem>> {
    let key = yt_key(ch)?;
    let body = yt_get(
        "search",
        &[
            ("part", "snippet".into()),
            ("type", "video".into()),
            ("order", "date".into()),
            ("maxResults", "20".into()),
            ("q", q.to_string()),
            ("key", key),
        ],
    )
    .await?;
    Ok(body
        .get("items")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|x| {
                    let id = x.pointer("/id/videoId")?.as_str()?.to_string();
                    let sn = x.get("snippet")?;
                    Some(VideoItem {
                        platform: "youtube".into(),
                        id: id.clone(),
                        aid: None,
                        title: sn
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        description: sn
                            .get("description")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        author: sn
                            .get("channelTitle")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        views: 0,
                        url: format!("https://www.youtube.com/watch?v={id}"),
                        published_at: 0,
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

fn yt_item(x: &Value) -> Option<VideoItem> {
    let id = x.get("id")?.as_str()?.to_string();
    let sn = x.get("snippet")?;
    Some(VideoItem {
        platform: "youtube".into(),
        id: id.clone(),
        aid: None,
        title: sn
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        description: sn
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        author: sn
            .get("channelTitle")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        views: x
            .pointer("/statistics/viewCount")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
        url: format!("https://www.youtube.com/watch?v={id}"),
        published_at: 0,
    })
}

async fn yt_comments(
    ch: &Channel,
    item: &VideoItem,
    limit: usize,
) -> anyhow::Result<Vec<CommentItem>> {
    let key = yt_key(ch)?;
    let body = yt_get(
        "commentThreads",
        &[
            ("part", "snippet".into()),
            ("videoId", item.id.clone()),
            ("maxResults", limit.to_string()),
            ("order", "relevance".into()),
            ("textFormat", "plainText".into()),
            ("key", key),
        ],
    )
    .await?;
    let mut out = Vec::new();
    for x in body
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
    {
        let sn = x.pointer("/snippet/topLevelComment");
        let Some(sn) = sn else { continue };
        let id = sn.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if id.is_empty() {
            continue;
        }
        out.push(CommentItem {
            id: id.to_string(),
            author: sn
                .pointer("/snippet/authorDisplayName")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            text: sn
                .pointer("/snippet/textDisplay")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            likes: sn
                .pointer("/snippet/likeCount")
                .and_then(as_u64)
                .unwrap_or(0),
        });
    }
    Ok(out)
}

async fn yt_post_comment(
    ch: &Channel,
    item: &VideoItem,
    text: &str,
    reply_to: Option<&str>,
) -> anyhow::Result<()> {
    // 写操作只用 OAuth Bearer：带 API Key 可能因 Key 限制被拒
    let _ = yt_key(ch)?;
    let token = ch
        .cfg("accessToken")
        .ok_or_else(|| anyhow::anyhow!("youtube 缺少 config.accessToken（OAuth），无法发评论"))?;
    let (path, body) = match reply_to {
        Some(parent) => (
            "comments",
            serde_json::json!({
                "snippet": { "parentId": parent, "textOriginal": text }
            }),
        ),
        None => (
            "commentThreads",
            serde_json::json!({
                "snippet": {
                    "videoId": item.id,
                    "topLevelComment": { "snippet": { "textOriginal": text } }
                }
            }),
        ),
    };
    let resp = http_client()
        .post(format!(
            "https://www.googleapis.com/youtube/v3/{path}?part=snippet"
        ))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("YouTube 评论请求失败：{e}"))?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("YouTube 评论响应不是 JSON（HTTP {status}）：{e}"))?;
    if !status.is_success() {
        anyhow::bail!("YouTube 评论 HTTP {status}：{}", api_message(&body));
    }
    Ok(())
}

pub(crate) async fn probe_youtube(ch: &Channel) -> Result<String, ProbeError> {
    let key = ch
        .cfg("apiKey")
        .ok_or_else(|| ProbeError::Missing("apiKey".into()))?;
    let body = yt_get(
        "videos",
        &[
            ("part", "snippet".into()),
            ("chart", "mostPopular".into()),
            ("maxResults", "1".into()),
            ("regionCode", "US".into()),
            ("key", key),
        ],
    )
    .await
    .map_err(|e| ProbeError::Network(e.to_string()))?;
    if body.get("items").and_then(|v| v.as_array()).is_some() {
        if let Some(token) = ch.cfg("accessToken") {
            // 有 OAuth 时顺带验证身份；失败仍以 API Key 可用为准（只读能力已通过）
            if let Ok(who) = yt_whoami(&token).await {
                return Ok(who);
            }
        }
        Ok("YouTube Data API".into())
    } else {
        Err(ProbeError::Auth("响应缺少 items".into()))
    }
}

async fn yt_whoami(token: &str) -> anyhow::Result<String> {
    let resp = http_client()
        .get("https://www.googleapis.com/youtube/v3/channels")
        .query(&[("part", "snippet"), ("mine", "true")])
        .bearer_auth(token)
        .send()
        .await?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        anyhow::bail!("HTTP {status}");
    }
    Ok(body
        .pointer("/items/0/snippet/title")
        .and_then(|v| v.as_str())
        .unwrap_or("YouTube OAuth")
        .to_string())
}

// ---------------------------------------------------------------- 小工具

fn as_u64(v: &Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .or_else(|| v.as_f64().map(|f| f as u64))
}

fn as_i64(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .or_else(|| v.as_u64().map(|u| u as i64))
}

/// B 站搜索标题/简介里带 `<em class="keyword">` 高亮标签，展示前去掉标签保留文字。
fn strip_html(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .trim()
        .to_string()
}

// ---------------------------------------------------------------- 测试（无网络）

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 配置解析_默认安全档() {
        let ch = Channel {
            id: "bili".into(),
            kind: "bilibili".into(),
            enabled: true,
            group: None,
            allowed_chats: vec![],
            account: None,
            secret: None,
            reply_webhook: None,
            token: None,
            config: Default::default(),
            group_gate: None,
            capability: "chat".into(),
            memory_isolation: true,
            created_at: String::new(),
        };
        let s = Settings::from_channel(&ch);
        assert!(s.watch);
        assert!(!s.comment);
        assert!(!s.reply);
        assert!(s.dry_run, "默认必须 dry-run");
        assert_eq!(s.max_actions, 1);
        assert_eq!(ch.memory_partition("g"), Some("g::channel:bili".into()));
        assert_eq!(ch.tool_scope(), exm_core::types::ToolScope::Chat);
    }

    #[test]
    fn 动作解析_容忍代码块与数字id() {
        let raw = "```json\n{\"summary\":\"讲了一个技巧\",\"comment\":\"学到了\",\"reply\":{\"to\":123,\"text\":\"同意\"}}\n```";
        let a = parse_action(raw).expect("应解析");
        assert_eq!(a.comment, "学到了");
        let r = a.reply.expect("reply");
        assert_eq!(reply_id(&r.to), "123");
        assert_eq!(r.text, "同意");
    }

    #[test]
    fn 标题去标签与文本清洗() {
        assert_eq!(
            strip_html("A <em class=\"keyword\">B站</em> 视频"),
            "A B站 视频"
        );
        assert_eq!(clean_text("  hello\n\nworld  ", 20), "hello world");
        assert_eq!(clean_text("abcdef", 3), "abc");
    }

    #[test]
    fn 关键词大小写不敏感() {
        let v = VideoItem {
            platform: "bilibili".into(),
            id: "BV1".into(),
            aid: Some(1),
            title: "Rust 异步运行时".into(),
            description: String::new(),
            author: "up".into(),
            views: 0,
            url: String::new(),
            published_at: 0,
        };
        assert!(matches_keywords(&v, &["rust".into()]));
        assert!(!matches_keywords(&v, &["python".into()]));
    }
}

#[cfg(test)]
mod interest_tests {
    use super::*;

    #[test]
    fn test_interest_search_terms() {
        let terms = interest_search_terms("只查看AI相关内容，优先二创其次是技术类。");
        println!("提取结果: {terms:?}");
        assert!(!terms.is_empty());
        assert!(terms.iter().any(|t| t.contains("AI")));
    }
}
