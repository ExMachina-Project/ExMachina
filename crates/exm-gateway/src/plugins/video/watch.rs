//! video 域分文件（自 mod.rs 按域拆出，纯搬移零行为变更）。
//! 观看域：单轮 tick / 智能体调用 / 提示组装 / 文本清洗 / 候选获取。

use super::*;

// ---------------------------------------------------------------- 单轮

pub(crate) async fn tick(core: &Arc<Core>, ch: &Channel, s: &Settings) -> anyhow::Result<String> {
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

pub(crate) async fn process_video(
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
pub(crate) struct WatchOutcome {
    text: String,
    acted: u32,
    skipped: Vec<String>,
    acted_notes: Vec<String>,
}

pub(crate) async fn ask_agent(core: &Arc<Core>, ch: &Channel, prompt: &str) -> anyhow::Result<WatchOutcome> {
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
        let mut g = st.lock();
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
pub(crate) fn build_prompt(s: &Settings, candidates: &[VideoItem]) -> String {
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
            parts.push(
                "注意：通道处于 dry-run 模式，act 只记录拟发内容，不会真正发出。\
                 最终观感里若提及该评论，必须标注「（dry-run，未实际发送）」。"
                    .into(),
            );
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

pub(crate) fn clean_text(raw: &str, max: usize) -> String {
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
pub(crate) fn detect_cliche(text: &str) -> Option<&str> {
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
pub(crate) fn clean_comment(raw: &str, max: usize) -> String {
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

pub(crate) async fn fetch_candidates(ch: &Channel, s: &Settings) -> anyhow::Result<Vec<VideoItem>> {
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
pub(crate) fn interest_cursor(ch: &Channel, terms: &[String]) -> usize {
    static CURSORS: OnceLock<Mutex<std::collections::HashMap<String, usize>>> =
        OnceLock::new();
    let map = CURSORS.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let mut guard = map.lock();
    let n = guard.entry(ch.id.clone()).or_insert(0);
    let cur = *n;
    *n = n.wrapping_add(1);
    cur % terms.len().max(1)
}

/// 从自由文本倾向里提取可用的搜索词：倾向是「只查看AI相关内容，优先二创其次是技术类。」
/// 这类长句，整句直接搜索噪音大，匹配也难命中。按引导虚词二次切分，取剩下的核心词。
/// 例：「优先二创其次是技术类」→ ["二创", "技术类"]；「只查看AI相关内容」→ ["AI"]。
pub(crate) fn interest_search_terms(interests: &str) -> Vec<String> {
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

pub(crate) async fn search_candidates(ch: &Channel, s: &Settings) -> anyhow::Result<Vec<VideoItem>> {
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

