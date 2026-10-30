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

pub(crate) mod tools;

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
    // 全局点播工具面同步（先于 runner 治理：通道启停立即反映到工具注册表）
    tools::reconcile_ondemand(core);
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

    // 候选限量投喂：一次观看轮次给智能体一份候选清单（自主选看/搜索/跳过），
    // 不再由系统逐个拼材料。限量 = 每轮动作数上限的 2 倍（有挑选余地但不至于刷屏）。
    let feed: Vec<VideoItem> = items.into_iter().take(s.max_actions * 2).collect();
    let keys: Vec<String> = feed.iter().map(video_key).collect();
    let mut processed = 0usize;
    let mut posted = 0u32;
    match process_video(core, ch, s, &mut state, &feed).await {
        Ok(did_post) => {
            posted += did_post;
            processed += feed.len();
            for k in &keys {
                state.mark_video(k);
            }
        }
        Err(e) => {
            // 轮次失败不打断轮询；候选全部标记已见，避免同批坏数据每轮重试
            for k in &keys {
                state.mark_video(k);
            }
            state.save(core, &ch.id);
            return Err(e);
        }
    }
    state.save(core, &ch.id);
    let mode = if s.dry_run { "dry-run" } else { "已发布" };
    Ok(format!(
        "本轮投喂候选 {processed} 条，互动 {posted} 次（{mode}）；今日互动 {}/{}",
        state.actions_today, s.daily_limit
    ))
}

// ---------------------------------------------------------------- 智能体调用

async fn process_video(
    core: &Arc<Core>,
    ch: &Channel,
    s: &Settings,
    state: &mut VideoState,
    candidates: &[VideoItem],
) -> anyhow::Result<u32> {
    // 自主观看轮次：系统只投喂候选清单，观看过程由智能体经 video__* 工具完成。
    // 会话注入与 act 闸门随轮次建立，收束后清理（凭据不外泄到普通会话）。
    let prompt = build_prompt(s, candidates);
    let WatchOutcome {
        text: raw,
        acted,
        skipped,
        acted_notes,
    } = ask_agent(core, ch, &prompt).await?;

    // 轮次摘要：观感（智能体收束文本）+ 动作/跳过记录 → 状态面板
    let summary = clean_text(&raw, 400);
    let mut status = format!("观感：{summary}");
    if !acted_notes.is_empty() {
        status.push_str(&format!("｜动作：{}", acted_notes.join("；")));
    }
    if !skipped.is_empty() {
        status.push_str(&format!("｜跳过：{}", skipped.join("；")));
    }
    platform::report_status(&ch.id, "ok", status);

    state.actions_today = state.actions_today.max(acted);
    Ok(acted.min(1))
}

/// 观看轮次产出（会话收束后从工具面收集）
struct WatchOutcome {
    text: String,
    acted: u32,
    skipped: Vec<String>,
    acted_notes: Vec<String>,
}

async fn ask_agent(core: &Arc<Core>, ch: &Channel, prompt: &str) -> anyhow::Result<WatchOutcome> {
    let (run, mut rx) = ChannelRun::begin(core, ch, "video")
        .await
        .ok_or_else(|| anyhow::anyhow!("视频通道会话建立失败"))?;
    // 视频观看会话：注入 video__* 工具组 + act 闸门（绑定本通道凭据与配额）。
    // 注入键 = ChannelRun 的数据库会话 id（工具面与事件订阅同键）。
    let sid = run.session_id.clone();
    {
        let s = Settings::from_channel(ch);
        let mut state_probe = VideoState::load(core, &ch.id);
        state_probe.reset_day();
        tools::register_act_gate(
            &sid,
            tools::ActGate::from_settings(&s, state_probe.actions_today),
        );
    }
    let injections = tools::build_injections(core, ch);
    core.orchestrator()
        .tools
        .inject_session_tools(&sid, injections);
    // 步数预算（防失控深挖）：通道面 maxWatchSteps 覆盖会话（默认 8）
    core.orchestrator()
        .tools
        .set_session_step_budget(&sid, Settings::from_channel(ch).max_watch_steps);

    // 系统自动任务标记：落库消息带 auto:video 来源，展示层与真人消息区分
    let run = run.with_origin("auto:video");
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
    // 自主探索多轮工具调用需更长收束窗口：600s（原单轮投喂 300s）
    let timed_out = tokio::time::timeout(Duration::from_secs(600), wait)
        .await
        .is_err();
    if timed_out {
        // 超时中止：ChannelRun::run 内部的组还原 guard 会随 Drop 执行
        task.abort();
        let _ = task.await;
        tools::drop_act_gate(&sid);
        tools::drop_session(&sid);
        core.orchestrator().tools.clear_session_tools(&sid);
        core.orchestrator().tools.set_session_step_budget(&sid, 0);
        anyhow::bail!("视频观看推理超时（600s）");
    }
    let _ = task.await;
    // 收束：收集轮次产出（动作计数/跳过理由），清理注入与闸门
    let acted = tools::act_gate_count(&sid);
    let st = tools::session_state(&sid);
    let (skipped, acted_notes) = {
        let mut g = st.lock().unwrap();
        (
            std::mem::take(&mut g.skipped),
            std::mem::take(&mut g.acted_notes),
        )
    };
    tools::drop_act_gate(&sid);
    tools::drop_session(&sid);
    core.orchestrator().tools.clear_session_tools(&sid);
    core.orchestrator().tools.set_session_step_budget(&sid, 0);
    if out.trim().is_empty() {
        anyhow::bail!("智能体没有产出收束文本");
    }
    Ok(WatchOutcome {
        text: out,
        acted,
        skipped,
        acted_notes,
    })
}

/// 观看轮次任务框架（新 build_prompt）：只给候选清单 + 工具用法 + 产出要求，
/// 不预拼材料、不限定探索顺序——观看过程完全由智能体自主完成。
fn build_prompt(s: &Settings, candidates: &[VideoItem]) -> String {
    let mut list = String::new();
    for (i, v) in candidates.iter().take(10).enumerate() {
        list.push_str(&format!(
            "{}. {}｜{}｜{}播放（id={}）\n",
            i + 1,
            v.title,
            v.author,
            v.views,
            v.id
        ));
    }
    let platform_label = match candidates.first().map(|v| v.platform.as_str()) {
        Some("bilibili") => "B站",
        Some("youtube") => "YouTube",
        _ => "视频平台",
    };
    let interact = if s.comment || s.reply {
        let mut parts: Vec<String> = vec![
            format!(
                "可以互动：经 video__act 发表评论或回复（回复目标必须是本会话已见的评论 id）。\
                 评论写作规范（硬要求，违反会被系统拒绝）：\
                 \n- 长度：{} 字以内——像真人随手打的短评，不是小作文。宁可一句话点到，不要面面俱到\
                 \n- 语气：用你自己的说话方式（人设优先）；像跟朋友聊天，不像在写读后感\
                 \n- 禁止 AI 腔：不写「总的来说/不得不说/真的太…了」这类套话，不排比，不总结升华，\
                 不堆表情符号，不用感叹号轰炸。一条评论只说一个具体的点：某个画面、某句台词、某个细节反应\
                 \n- B 站评论不支持 Markdown，纯文本即可\
                 \n- 写不出真诚的评论就只看不说——强行硬夸比沉默更假\n",
                s.max_comment_chars
            ),
        ];
        if s.dry_run {
            parts.push("注意：通道处于 dry-run 模式，act 只记录拟发内容，不会真正发出。".into());
        }
        parts.join("")
    } else {
        "本轮不互动（通道未开启评论/回复）：只产出观感，不要调用 video__act。".into()
    };
    let interest = if s.interests.trim().is_empty() {
        String::new()
    } else {
        format!(
            "\n内容倾向参考（软偏好，不是硬约束）：{}\n",
            s.interests.trim()
        )
    };
    let style = if s.comment_style.trim().is_empty() {
        String::new()
    } else {
        format!(
            "\n评论风格提示（与人设冲突时以人设为准）：{}\n",
            s.comment_style.trim()
        )
    };
    let budget = format!(
        "\n预算提示：本轮工具调用步数上限 {} 步，达限后系统会要求你直接收束——材料够用就好，不必穷尽。\n",
        s.max_watch_steps
    );
    format!(
        "你是{platform_label}观众代理。本轮候选视频：\n{list}\n\
用 video__* 工具自主观看：自己选一个（或搜索新视频），自己决定看哪些侧面（简介/字幕/弹幕/评论/UP主）、看多深。\
材料足够时就收束，不必穷尽工具。\n{interest}{style}{budget}\n\
产出要求：\n\
- 观感：用你自己的话讲这个视频讲了什么、你的看法（不要复述元信息）\n\
- 互动：{interact}\n\
- 不感兴趣可以都跳过——直接说明理由即可，不强制观看\n\
你是谁就怎么说话：评论、观感全部延续你的人设与口癖（它们优先于本提示的通用措辞）。"
    )
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

/// AI 味套话检测：命中返回首处套话（用于拒绝反馈，让模型重写而非静默修剪）。
/// 清单为高频「总结升华 / 强调轰炸 / 感叹套话」模式——真人短评几乎不用这些起手。
fn detect_cliche(text: &str) -> Option<&str> {
    const CLICHES: &[&str] = &[
        "总的来说",
        "总而言之",
        "综上",
        "不得不说",
        "真的是太",
        "真的太",
        "简直了",
        "太震撼",
        "太感动",
        "泪目",
        "破防了",
        "yyds",
        "绝绝子",
        "强烈推荐",
        "值得一看",
        "受益匪浅",
        "收获满满",
        "干货满满",
        "希望大家都",
        "让我们",
        "期待下一",
        "下期再见",
        "首先",
        "其次",
        "最后",
        "一方面",
        "另一方面",
    ];
    CLICHES.iter().find(|c| text.contains(**c)).copied()
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
                    hits.retain(|v| {
                        matches_keywords(v, &interest_terms) || matches_keywords(v, &search_terms)
                    });
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
        "只查看",
        "只看",
        "优先看",
        "优先",
        "偏向",
        "偏爱",
        "喜欢看",
        "喜欢",
        "偏好",
        "多看",
        "少看",
        "不要看",
        "不看",
        "避免看",
        "避免",
        "查看",
    ];
    const JOINERS: &[&str] = &[
        "相关",
        "其次是",
        "其次",
        "类型",
        "类的",
        "类",
        "为主",
        "以及",
        "还有",
        "都行",
        "的",
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
                && !out
                    .iter()
                    .any(|o: &String| o.contains(core) || core.contains(o.as_str()))
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

/// b23.tv 短链重定向解析（不跟随重定向，读一层 Location；video 工具归一用）。
/// 放行至 video 工具模块使用（tools.rs 的 expand_b23）。
pub(crate) async fn head_no_redirect(url: &str) -> anyhow::Result<reqwest::Response> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent(UA)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let resp = client.get(url).send().await?;
    Ok(resp)
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
type BuvidCache =
    std::sync::Mutex<std::collections::HashMap<String, (String, Option<String>, bool)>>;
fn buvid_cache() -> &'static BuvidCache {
    static CACHE: OnceLock<BuvidCache> = OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// 读取缓存的 buvid（无则 None——发评论前应先 bili_prime_buvid）
fn buvid_cached(ch: &Channel) -> Option<(String, Option<String>)> {
    buvid_cache()
        .lock()
        .expect("buvid cache lock")
        .get(&ch.id)
        .map(|(b3, b4, _)| (b3.clone(), b4.clone()))
}

/// 异步获取并缓存 buvid（观看轮次开始时调用一次；匿名访问首页即下发）。
/// 2024+ 风控：buvid3 必须「激活」后才有发评论资格（未激活 = code:0 但评论进
/// 自查不上屏，或直接 -352 风控）。激活 = 带 buvid3 cookie POST 指纹 payload
/// 到 gaia-gateway/ExClimbWuzhi。缓存值三元组：(buvid3, buvid4, activated)。
async fn bili_prime_buvid(ch: &Channel) {
    let cache = buvid_cache();
    // 未激活的缓存条目要重试（激活失败常见于网络抖动；指纹一日内有效，成功后不再重跑）
    let need = match cache.lock().expect("buvid cache lock").get(&ch.id) {
        None => true,
        Some((_, _, activated)) => !activated,
    };
    if !need {
        return;
    }
    let cached = buvid_cached(ch);
    // 有缓存 buvid 时无需再抓首页（只补激活）；无缓存才匿名访问首页领新指纹
    let (b3, b4) = match cached {
        Some(c) => c,
        None => {
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
            match buvid3 {
                Some(b3) => (b3, buvid4),
                None => return,
            }
        }
    };
    // 指纹激活（失败不阻断：激活态随缓存记录，下次 prime 重试）
    let activated = activate_buvid(&b3).await;
    cache
        .lock()
        .expect("buvid cache lock")
        .insert(ch.id.clone(), (b3, b4, activated));
}

/// 桌面 Chrome 指纹常量（ExClimbWuzhi payload 内层；社区长期维护的默认值）
const FINGERPRINT_JSON: &str = r##"{
    "3064": 1,
    "5062": "1707365865753",
    "03bf": "https%3A%2F%2Fwww.bilibili.com%2F",
    "39c8": "333.1007.fp.risk",
    "34f1": "",
    "d402": "",
    "654a": "",
    "6e7c": "1920x1080",
    "3c43": {
        "2673": 0, "5766": 24, "6527": 0, "7003": 1, "807e": 1,
        "b8ce": "zh-CN", "16f5": true, "65d8": 8, "3b21": 1, "11cc": 5,
        "7c98": "not available", "7c9e": "not available", "7cac": "not available",
        "80c3": [1920, 1080], "d61f": [1920, 1040], "cf1a": 0, "fbb7": 3,
        "51c3": 3, "0bd0": 4, "9ac1": "Win32", "6414": 1, "17da": 5, "5b52": 0,
        "60ba": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
        "3c74": "124.0.0.0", "65f2": 1, "4aa6": 8, "4aa9": [120, 115], "7abd": true
    },
    "39c9": null,
    "6feb": "AsUSROmqLmTcZ0ltZXRhYm9nb3V0ZQ==",
    "6f9f": 1, "6fa1": "unknown", "6fa2": 0,
    "697f": "Standard,QWERTY,EN",
    "6ff8": 0, "6ffa": 0, "6fff": 0, "7000": 3,
    "7001": "1.13.2", "7002": "1.13.2", "7003": 0, "7004": 0, "7005": 8,
    "6cf2": "881B8DFEB2D896C4A0E3F3F6BF3D2D6BE1D3C88E7E3B0C15B6B84D7E5D8AED74",
    "776d": "490a5fd6a5e8b2b8",
    "8b4c": "", "a2b0": "", "a2b1": "", "8b6e": "", "8b6f": "",
    "f7a8": 1, "f7a9": 0, "f7aa": 0, "d03e": "", "d03f": "",
    "6e78": "4bd3bf45c0beb6f4c28a9b0ad9e6f7b56f85c47b6df4c15c615d9e914b4db2a",
    "6e7d": "126f95fe5b4bd97db5f2985be136d075",
    "6e7e": "not available",
    "89c4": 0, "8a63": 0, "8a66": 0, "ce90": "", "ceac": "", "cead": "",
    "e4dd": 9, "e4e0": "1.8.2", "e4e1": 0, "e4e2": 0, "e4e3": 3,
    "f551": 4, "f552": 2, "f553": 1, "34a1": null,
    "34a5": 0, "34ac": 0, "34e1": 0, "34e2": 1, "34e3": 0
}"##;

/// buvid3 指纹激活（ExClimbWuzhi）：Web 端登录前必经的设备指纹上报。
/// payload = 指纹对象的 JSON **字符串**（双重编码：外层 {"payload": "<json str>"}），
/// 字段集取社区长期维护的默认值（桌面 Chrome 形态，与请求 UA 同源）。
async fn activate_buvid(buvid3: &str) -> bool {
    // 指纹常量：从 JSON 文本解析（json! 宏嵌套深度超限；内容同社区默认值）
    let fingerprint: Value = serde_json::from_str(FINGERPRINT_JSON).expect("指纹常量 JSON 解析");
    // 双重编码：内层指纹对象 → JSON 字符串 → 外层 {"payload": "<str>"}
    let body = serde_json::json!({ "payload": fingerprint.to_string() });
    let resp = http_client()
        .post("https://api.bilibili.com/x/internal/gaia-gateway/ExClimbWuzhi")
        .header(reqwest::header::USER_AGENT, UA)
        .header(reqwest::header::REFERER, BILI_REFERER)
        .header(reqwest::header::COOKIE, format!("buvid3={buvid3}"))
        .json(&body)
        .send()
        .await;
    match resp {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
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
            })
        })
        .collect())
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
        &format!("https://api.bilibili.com/x/v2/reply?pn=1&ps=20&oid={aid}&type=1&sort=2"),
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
    let sessdata =
        pick("SESSDATA").ok_or_else(|| anyhow::anyhow!("登录成功但响应缺 SESSDATA Cookie"))?;
    let bili_jct =
        pick("bili_jct").ok_or_else(|| anyhow::anyhow!("登录成功但响应缺 bili_jct Cookie"))?;
    let dede_user_id = pick("DedeUserID").unwrap_or_default();
    // 用新 Cookie 查一次 nav 拿用户名，顺带校验登录态真的可用
    let mut cookie_parts = vec![
        format!("SESSDATA={sessdata}"),
        format!("bili_jct={bili_jct}"),
    ];
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
        || !nav
            .pointer("/data/isLogin")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    {
        anyhow::bail!("扫码成功但登录态校验未通过：{}", api_message(&nav));
    }
    let uname = nav
        .pointer("/data/uname")
        .and_then(|v| v.as_str())
        .unwrap_or("bilibili")
        .to_string();
    Ok(Ok(BiliQrLogin {
        sessdata,
        bili_jct,
        dede_user_id,
        uname,
    }))
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
    })
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
    fn 全局点播注册_随通道启停同步() {
        let dir = std::env::temp_dir().join(format!("exm-video-od-{}", exm_core::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        let core = Arc::new(exm_core::Core::with_config(cfg).expect("创建 Core 失败"));

        let mk = |enabled: bool| Channel {
            id: "bili-od".into(),
            kind: "bilibili".into(),
            enabled,
            group: None,
            allowed_chats: vec![],
            admins: vec![],
            account: None,
            secret: None,
            reply_webhook: None,
            token: None,
            config: {
                let mut m = std::collections::BTreeMap::new();
                m.insert("sessdata".into(), "x".into());
                m.insert("biliJct".into(), "y".into());
                m
            },
            group_gate: None,
            capability: "computer".into(),
            memory_isolation: false,
            created_at: String::new(),
        };

        // 初始：无通道 → 无全局工具
        tools::reconcile_ondemand(&core);
        assert!(core
            .orchestrator()
            .tools
            .dynamic_tool("video__summary")
            .is_none());

        // 启用 → 注册（含 act）
        crate::platform::save_channels(&core, &[mk(true)]).unwrap();
        tools::reconcile_ondemand(&core);
        for n in ["video__summary", "video__comments", "video__act"] {
            assert!(
                core.orchestrator().tools.dynamic_tool(n).is_some(),
                "{n} 应已注册"
            );
        }
        // 全体可见（agents 空列表）
        let specs = core.orchestrator().tools.custom_specs_for("machina");
        assert!(specs.iter().any(|s| s.name == "video__summary"));

        // 停用 → 注销回收
        crate::platform::save_channels(&core, &[mk(false)]).unwrap();
        tools::reconcile_ondemand(&core);
        assert!(core
            .orchestrator()
            .tools
            .dynamic_tool("video__summary")
            .is_none());
    }

    #[test]
    fn 配对码_预设角色兑换() {
        let dir = std::env::temp_dir().join(format!("exm-pair-role-{}", exm_core::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        let core = exm_core::Core::with_config(cfg).expect("创建 Core 失败");

        // 管理者配对码：兑换即落 admin
        let admin_code = core
            .issue_pairing_code_role("管理员", None, 600, "admin")
            .unwrap();
        let idn = core
            .redeem_pairing(&admin_code.code, "tg-main", "u1", "张三", "member")
            .unwrap();
        assert_eq!(idn.role, "admin", "预设角色应覆盖默认");

        // 普通码（旧签名/无角色）：落 default_role
        let member_code = core.issue_pairing_code("成员", None, 600).unwrap();
        assert_eq!(member_code.role, "member");
        let idn2 = core
            .redeem_pairing(&member_code.code, "tg-main", "u2", "李四", "member")
            .unwrap();
        assert_eq!(idn2.role, "member");

        // 存量码无角色字段（手工造码）：回落 default_role
        let legacy = exm_core::types::PairingCode {
            code: "legacy001".into(),
            note: String::new(),
            channel_platform: None,
            created_by: "console".into(),
            created_at: String::new(),
            expires_at_ms: exm_core::types::now_ms() + 600_000,
            role: String::new(),
        };
        core.save_pairing_code(&legacy).unwrap();
        let idn3 = core
            .redeem_pairing("legacy001", "tg-main", "u3", "王五", "member")
            .unwrap();
        assert_eq!(idn3.role, "member");
    }

    #[test]
    fn 配置解析_默认安全档() {
        let ch = Channel {
            id: "bili".into(),
            kind: "bilibili".into(),
            enabled: true,
            group: None,
            allowed_chats: vec![],
            admins: vec![],
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
    fn 任务框架prompt_候选清单与产出要求() {
        let ch = Channel {
            id: "bili".into(),
            kind: "bilibili".into(),
            enabled: true,
            group: None,
            allowed_chats: vec![],
            admins: vec![],
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
        let cands = vec![VideoItem {
            platform: "bilibili".into(),
            id: "BV1xx".into(),
            aid: Some(1),
            title: "Rust 教程".into(),
            description: String::new(),
            author: "up主".into(),
            views: 1234,
        }];
        let p = build_prompt(&s, &cands);
        // 候选清单带标识（智能体可据此调工具）
        assert!(p.contains("BV1xx"), "候选 id 应入 prompt：{p}");
        assert!(p.contains("Rust 教程"));
        // 任务框架要素：自主观看 + 产出要求 + 跳过自由
        assert!(p.contains("video__"), "应说明工具用法：{p}");
        assert!(p.contains("观感"));
        assert!(p.contains("跳过"));
        // 未开启互动时不引导 act
        assert!(!s.comment && !s.reply);
        assert!(p.contains("不要调用 video__act"));
        // 不再预拼材料（旧管道特征文本不出现）
        assert!(!p.contains("【视频资料】"));
        assert!(!p.contains("评论区摘录"));
    }

    #[test]
    fn 任务框架prompt_互动开启与dry_run标注() {
        let ch = Channel {
            id: "bili".into(),
            kind: "bilibili".into(),
            enabled: true,
            group: None,
            allowed_chats: vec![],
            admins: vec![],
            account: None,
            secret: None,
            reply_webhook: None,
            token: None,
            config: [
                ("comment".to_string(), "true".to_string()),
                ("dryRun".to_string(), "true".to_string()),
                ("commentStyle".to_string(), "口语化、不用书面语".to_string()),
                ("interests".to_string(), "技术类视频".to_string()),
            ]
            .into_iter()
            .collect(),
            group_gate: None,
            capability: "chat".into(),
            memory_isolation: true,
            created_at: String::new(),
        };
        let s = Settings::from_channel(&ch);
        assert!(s.comment && s.dry_run);
        let p = build_prompt(&s, &[]);
        assert!(p.contains("video__act"));
        assert!(p.contains("dry-run"), "应提示 dry-run 模式：{p}");
        assert!(p.contains("技术类视频"), "内容倾向应注入：{p}");
        assert!(p.contains("口语化、不用书面语"), "评论风格应注入：{p}");
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

#[cfg(test)]
mod comment_quality_tests {
    use super::*;

    /// AI 味套话检测：命中清单返回首处，干净短评不误报
    #[test]
    fn 套话检测_命中与误报() {
        assert_eq!(detect_cliche("总的来说这个视频不错"), Some("总的来说"));
        assert_eq!(detect_cliche("不得不说 UP 主真的用心了"), Some("不得不说"));
        assert_eq!(detect_cliche("首先…其次…最后…"), Some("首先"));
        assert_eq!(detect_cliche("太感动了，泪目"), Some("太感动"));
        // 真人短评形态：具体、口语、无套话
        assert_eq!(detect_cliche("3 分 20 秒那个转场直接给我看愣了"), None);
        assert_eq!(detect_cliche("这就是我不敢养猫的原因"), None);
        assert_eq!(
            detect_cliche("最后"),
            Some("最后"),
            "「最后」单独出现也命中（总结腔起手）"
        );
    }

    /// 观看提示注入评论规范：字数预算 + 反 AI 味负面清单 + 人设优先
    #[test]
    fn 观看提示_评论规范注入() {
        let s = Settings {
            comment: true,
            reply: false,
            max_comment_chars: 80,
            ..Settings::from_channel(&Channel {
                id: "bili-q".into(),
                kind: "bilibili".into(),
                enabled: true,
                group: None,
                allowed_chats: vec![],
                admins: vec![],
                account: None,
                secret: None,
                reply_webhook: None,
                token: None,
                config: Default::default(),
                group_gate: None,
                capability: "computer".into(),
                memory_isolation: false,
                created_at: String::new(),
            })
        };
        let item = VideoItem {
            platform: "bilibili".into(),
            id: "BV1".into(),
            aid: None,
            title: "测试视频".into(),
            description: String::new(),
            author: "up".into(),
            views: 0,
        };
        let prompt = build_prompt(&s, &[item]);
        assert!(prompt.contains("80 字以内"), "字数预算应注入：{prompt}");
        assert!(prompt.contains("禁止 AI 腔"), "反 AI 味清单应注入");
        assert!(prompt.contains("人设优先"), "人设优先应注入");
        assert!(prompt.contains("你是谁就怎么说话"), "人设延续声明应注入");
    }
}

#[cfg(test)]
mod watch_loop_tests {
    use super::*;

    fn item(id: &str) -> VideoItem {
        VideoItem {
            platform: "bilibili".into(),
            id: id.into(),
            aid: None,
            title: format!("视频{id}"),
            description: String::new(),
            author: "up".into(),
            views: 0,
        }
    }

    /// 已看去重沿用 VideoState：seen/mark_video 环路 + 上限滚动
    #[test]
    fn 候选去重_VideoState环路() {
        let mut st = VideoState::default();
        assert!(!st.seen(&video_key(&item("BV1"))));
        st.mark_video(&video_key(&item("BV1")));
        assert!(st.seen(&video_key(&item("BV1"))), "标记后应已看");
        // 上限滚动：超过 500 条自动淘汰最旧
        for i in 0..510 {
            st.mark_video(&format!("bilibili:BV{i}"));
        }
        assert!(st.seen("bilibili:BV509"));
        assert!(!st.seen("bilibili:BV1"), "超限后最旧应被淘汰");
    }

    /// 跳过记录：收束文本承载跳过理由（状态面板呈现），WatchSession.skipped
    /// 记录系统面跳过（act 拒绝等）
    #[test]
    fn 跳过理由_会话状态与收束文本() {
        let sid = "skip-test";
        let st = tools::session_state(sid);
        st.lock()
            .unwrap()
            .skipped
            .push("候选均不感兴趣：题材重复".into());
        let skipped = st.lock().unwrap().skipped.clone();
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].contains("题材重复"));
        tools::drop_session(sid);
    }
}

#[cfg(test)]
mod tick_smoke_tests {
    use super::*;

    fn core(tag: &str) -> Arc<Core> {
        let dir = std::env::temp_dir().join(format!("exm-tick-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        Arc::new(Core::with_config(cfg).expect("Core 创建失败"))
    }

    /// 无凭据轮次：tick 返回结构化错误（不 panic）；错误说明含凭据指引
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 冒烟_无凭据结构化降级() {
        let core = core("nocred");
        let ch = Channel {
            id: "yt-smoke".into(),
            kind: "youtube".into(),
            enabled: true,
            group: None,
            allowed_chats: vec![],
            admins: vec![],
            account: None,
            secret: None,
            reply_webhook: None,
            token: None,
            config: [("watch".to_string(), "true".to_string())]
                .into_iter()
                .collect(),
            group_gate: None,
            capability: "computer".into(),
            memory_isolation: false,
            created_at: String::new(),
        };
        let s = Settings::from_channel(&ch);
        let r = tick(&core, &ch, &s).await;
        assert!(r.is_err(), "无 apiKey 应失败：{r:?}");
        let msg = r.unwrap_err().to_string();
        assert!(msg.contains("apiKey"), "错误应指向凭据缺失：{msg}");
    }

    /// 互动全关：tick 直接短路返回（不触网）
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 冒烟_全关短路() {
        let core = core("off");
        let ch = Channel {
            id: "bili-off".into(),
            kind: "bilibili".into(),
            enabled: true,
            group: None,
            allowed_chats: vec![],
            admins: vec![],
            account: None,
            secret: None,
            reply_webhook: None,
            token: None,
            config: [
                ("watch".to_string(), "false".to_string()),
                ("comment".to_string(), "false".to_string()),
                ("reply".to_string(), "false".to_string()),
            ]
            .into_iter()
            .collect(),
            group_gate: None,
            capability: "computer".into(),
            memory_isolation: false,
            created_at: String::new(),
        };
        let s = Settings::from_channel(&ch);
        let r = tick(&core, &ch, &s).await.unwrap();
        assert!(r.contains("均已关闭"), "{r}");
    }
}
