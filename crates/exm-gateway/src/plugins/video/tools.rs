//! 视频观看工具组（video__*）：智能体在观看会话内自主调用的工具面。
//!
//! - 观看工具（B站 7 + YouTube 5 同构）：summary/subtitle/danmaku/comments/uploader/related/search
//! - 互动工具：video__act（评论/回复，系统硬闸门链：目标核验/清洗/配额/buvid/dry-run/审批）
//! - 注册面：动态命名空间工具（ToolGateway::register_dynamic_tool），schema 随
//!   function calling 下发；仅视频通道的观看会话注入（普通会话不注册）。
//!
//! 平台差异：同一工具名双平台同构，凭 Channel.kind 路由；YouTube 无弹幕，
//! danmaku 返回「该平台无弹幕数据」。接口失败返回结构化错误（不中断轮次）。

use super::*;
use exm_core::config::CustomTool;
use exm_core::tools::{dynamic_exec, DynamicToolExec};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

/// 单个观看会话的运行期状态（工具间共享：已见评论/视频/步数/配额引用）
#[derive(Default)]
pub(crate) struct WatchSession {
    /// 本会话已见评论 id 集合（act 回复目标的核验面——防编造 id）
    pub seen_comments: BTreeSet<String>,
    /// 本会话已发动作数（与全局每日配额联动由调用方在 act 内检查）
    pub acted: u32,
    /// 跳过理由（候选不感兴趣时记录，状态面板呈现）
    pub skipped: Vec<String>,
    /// 观感摘要（收束时智能体产出——由 prompt 引导写在最终回复，工具不设此项；
    /// 此字段留给 act/报告填写动作面）
    pub acted_notes: Vec<String>,
}

/// 全局会话表（session_id → 观看状态；进程生命周期）
fn watch_sessions() -> &'static Mutex<std::collections::HashMap<String, Arc<Mutex<WatchSession>>>> {
    static S: std::sync::OnceLock<Mutex<std::collections::HashMap<String, Arc<Mutex<WatchSession>>>>> =
        std::sync::OnceLock::new();
    S.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// 取（或创建）会话的观看状态
pub(crate) fn session_state(session_id: &str) -> Arc<Mutex<WatchSession>> {
    let mut map = watch_sessions().lock().unwrap();
    map.entry(session_id.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(WatchSession::default())))
        .clone()
}

/// 会话结束清理（轮次收束后调用；防内存缓慢增长）
pub(crate) fn drop_session(session_id: &str) {
    watch_sessions().lock().unwrap().remove(session_id);
}

// ---------------------------------------------------------------- 工具 schema

fn spec(name: &str, desc: &str, params: Value) -> CustomTool {
    CustomTool {
        name: name.into(),
        description: desc.into(),
        kind: "builtin".into(), // 动态执行面（不走 http/shell）
        method: String::new(),
        target: String::new(),
        headers: Default::default(),
        parameters: params,
        agents: vec![],
    }
}

fn video_id_param(desc: &str) -> Value {
    json!({
        "type": "object",
        "properties": { "videoId": { "type": "string", "description": desc } },
        "required": ["videoId"]
    })
}

/// 观看工具组 schema（B站全量 / YouTube 子集由注册时按平台过滤 danmaku/related）
pub(crate) fn watch_tool_specs(kind: &str) -> Vec<CustomTool> {
    let mut out = vec![
        spec(
            "video__summary",
            "视频元信息：标题、UP主、时长、播放/弹幕/评论数、简介、分区、发布时间（字段缺失以占位标注）",
            video_id_param("视频标识（B站 bvid / YouTube videoId）"),
        ),
        spec(
            "video__subtitle",
            "字幕全文（带时间戳，超长分页）；无字幕时明确告知",
            json!({
                "type": "object",
                "properties": {
                    "videoId": { "type": "string", "description": "视频标识" },
                    "page": { "type": "integer", "description": "页码（从 1 起；缺省第 1 页）" }
                },
                "required": ["videoId"]
            }),
        ),
        spec(
            "video__comments",
            "评论区页（热度/时间排序可选）：作者/赞数/文本/回复数；返回的评论 id 可用于 act 回复目标",
            json!({
                "type": "object",
                "properties": {
                    "videoId": { "type": "string", "description": "视频标识" },
                    "page": { "type": "integer", "description": "页码（从 1 起）" },
                    "sort": { "type": "string", "enum": ["hot", "time"], "description": "排序（缺省 hot）" }
                },
                "required": ["videoId"]
            }),
        ),
        spec(
            "video__uploader",
            "UP主/频道公开信息与近期投稿（限量）",
            json!({
                "type": "object",
                "properties": { "uploaderId": { "type": "string", "description": "UP主 mid / 频道 channelId" } },
                "required": ["uploaderId"]
            }),
        ),
        spec(
            "video__search",
            "平台内搜索视频（结果结构同候选清单：标识/标题/作者/数据）",
            json!({
                "type": "object",
                "properties": { "query": { "type": "string", "description": "搜索词（可用兴趣词）" } },
                "required": ["query"]
            }),
        ),
    ];
    if kind == "bilibili" {
        out.push(spec(
            "video__danmaku",
            "弹幕抽样（按时间分片，含热度权重）；可指定段（segment 0-9）或看全片概览",
            json!({
                "type": "object",
                "properties": {
                    "videoId": { "type": "string", "description": "视频标识（bvid）" },
                    "segment": { "type": "integer", "description": "时间分段序号（0 起，每段约 6 分钟；缺省 = 全片概览）" }
                },
                "required": ["videoId"]
            }),
        ));
        out.push(spec(
            "video__related",
            "相关/推荐视频（限量，结构同候选清单）",
            video_id_param("视频标识（bvid）"),
        ));
    }
    out
}

/// 互动工具 schema（video__act；评论开关与闸门由执行面判定）
pub(crate) fn act_tool_spec() -> CustomTool {
    spec(
        "video__act",
        "互动动作：发表评论或回复（回复目标必须是本会话已见评论）。受每日配额/字数清洗/dry-run/审批闸门约束",
        json!({
            "type": "object",
            "properties": {
                "videoId": { "type": "string", "description": "视频标识" },
                "comment": { "type": "string", "description": "评论文本（对视频直接评论；与 replyText 二选一）" },
                "replyText": { "type": "string", "description": "回复文本（与 comment 二选一，需同时给 replyTo）" },
                "replyTo": { "type": "string", "description": "回复目标评论 id（须为本会话已见评论的 id）" }
            },
            "required": ["videoId"]
        }),
    )
}

// ---------------------------------------------------------------- 执行面注册

/// 构建观看会话的完整注入集（观看工具组 + act 互动工具）。
/// 每工具绑定通道快照（kind/凭据），执行闭包路由到 dispatch_tool。
pub(crate) fn build_injections(
    core: &Arc<Core>,
    ch: &Channel,
) -> Vec<(CustomTool, DynamicToolExec)> {
    let mut out: Vec<(CustomTool, DynamicToolExec)> = watch_tool_specs(&ch.kind)
        .into_iter()
        .chain(std::iter::once(act_tool_spec()))
        .map(|s| {
            let exec = build_exec(core.clone(), ch.clone(), s.name.clone());
            (s, exec)
        })
        .collect();
    out.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    out
}

fn build_exec(core: Arc<Core>, ch: Channel, name: String) -> DynamicToolExec {
    // 执行闭包：路由到具体工具实现；错误统一为结构化文本（工具结果 ok=false）
    dynamic_exec(move |_agent, session, args| {
        let core = core.clone();
        let ch = ch.clone();
        let name = name.clone();
        let session = session.to_string();
        async move {
            let st = session_state(&session);
            dispatch_tool(&core, &name, &ch, &session, &st, args).await
        }
    })
}

/// 工具分发（单点：新增工具在此路由）
async fn dispatch_tool(
    core: &Arc<Core>,
    name: &str,
    ch: &Channel,
    session: &str,
    st: &Arc<Mutex<WatchSession>>,
    args: Value,
) -> anyhow::Result<String> {
    let vid = |k: &str| -> Option<String> { args.get(k).and_then(|v| v.as_str()).map(str::to_string) };
    match name {
        "video__summary" => tool_summary(ch, vid("videoId").unwrap_or_default()).await,
        "video__subtitle" => {
            tool_subtitle(ch, vid("videoId").unwrap_or_default(), args.get("page").and_then(|p| p.as_u64()).unwrap_or(1)).await
        }
        "video__danmaku" => {
            if ch.kind != "bilibili" {
                return Ok("该平台无弹幕数据（YouTube 不提供弹幕；请用评论/字幕了解观众反应）".into());
            }
            tool_danmaku(ch, vid("videoId").unwrap_or_default(), args.get("segment").and_then(|s| s.as_u64()).map(|s| s as i64)).await
        }
        "video__comments" => {
            let sort = args.get("sort").and_then(|s| s.as_str()).unwrap_or("hot").to_string();
            tool_comments(ch, st, vid("videoId").unwrap_or_default(), args.get("page").and_then(|p| p.as_u64()).unwrap_or(1), &sort).await
        }
        "video__uploader" => tool_uploader(ch, vid("uploaderId").unwrap_or_default()).await,
        "video__related" => {
            if ch.kind != "bilibili" {
                return Ok("该平台工具暂不提供（YouTube 相关推荐请用 video__search）".into());
            }
            tool_related(ch, vid("videoId").unwrap_or_default()).await
        }
        "video__search" => tool_search(ch, vid("query").unwrap_or_default()).await,
        "video__act" => tool_act(core, ch, st, session, args).await,
        other => anyhow::bail!("未知视频工具：{other}"),
    }
}

// ---------------------------------------------------------------- 工具实现

fn placeholder(v: &str) -> String {
    if v.trim().is_empty() { "（未知）".into() } else { v.to_string() }
}

/// video__summary：双平台元信息
async fn tool_summary(ch: &Channel, video_id: String) -> anyhow::Result<String> {
    if video_id.trim().is_empty() {
        anyhow::bail!("缺少 videoId");
    }
    match ch.kind.as_str() {
        "bilibili" => {
            let body = bili_get(ch, &format!("https://api.bilibili.com/x/web-interface/view?bvid={video_id}")).await?;
            api_ok(&body)?;
            let d = body.get("data").cloned().unwrap_or(Value::Null);
            let dur = d.get("duration").and_then(as_u64).unwrap_or(0);
            let out = format!(
                "标题：{}\nUP主：{}\n时长：{}分{}秒\n播放：{}\n弹幕：{}\n评论：{}\n点赞：{}\n分区：{}\n发布：{}\n\n简介：\n{}",
                placeholder(d.get("title").and_then(|v| v.as_str()).unwrap_or("")),
                placeholder(d.pointer("/owner/name").and_then(|v| v.as_str()).unwrap_or("")),
                dur / 60, dur % 60,
                d.pointer("/stat/view").and_then(as_u64).unwrap_or(0),
                d.pointer("/stat/danmaku").and_then(as_u64).unwrap_or(0),
                d.pointer("/stat/reply").and_then(as_u64).unwrap_or(0),
                d.pointer("/stat/like").and_then(as_u64).unwrap_or(0),
                placeholder(d.pointer("/tid_name").unwrap_or(&Value::Null).as_str().unwrap_or("")),
                placeholder(d.get("pubdate").and_then(|p| p.as_i64()).map(|t| chrono::DateTime::from_timestamp(t, 0).map(|dt| dt.format("%Y-%m-%d").to_string()).unwrap_or_default()).as_deref().unwrap_or("")),
                d.get("desc").and_then(|v| v.as_str()).unwrap_or("（无简介）"),
            );
            Ok(out)
        }
        "youtube" => {
            let key = yt_key(ch)?;
            let body = yt_get("videos", &[
                ("part", "snippet,statistics,contentDetails".into()),
                ("id", video_id.clone()),
                ("key", key),
            ]).await?;
            let item = body.pointer("/items/0").cloned().unwrap_or(Value::Null);
            if item.is_null() {
                anyhow::bail!("YouTube 未找到视频：{video_id}");
            }
            let out = format!(
                "标题：{}\n频道：{}\n时长：{}\n播放：{}\n评论：{}\n点赞：{}\n发布：{}\n\n简介：\n{}",
                placeholder(item.pointer("/snippet/title").and_then(|v| v.as_str()).unwrap_or("")),
                placeholder(item.pointer("/snippet/channelTitle").and_then(|v| v.as_str()).unwrap_or("")),
                placeholder(item.pointer("/contentDetails/duration").and_then(|v| v.as_str()).unwrap_or("")),
                item.pointer("/statistics/viewCount").and_then(as_u64).unwrap_or(0),
                item.pointer("/statistics/commentCount").and_then(as_u64).unwrap_or(0),
                item.pointer("/statistics/likeCount").and_then(as_u64).unwrap_or(0),
                placeholder(item.pointer("/snippet/publishedAt").and_then(|v| v.as_str()).unwrap_or("")),
                item.pointer("/snippet/description").and_then(|v| v.as_str()).unwrap_or("（无简介）"),
            );
            Ok(out)
        }
        other => anyhow::bail!("不支持的平台：{other}"),
    }
}

const SUBTITLE_PAGE_LINES: usize = 120;

/// video__subtitle：B站 CC/AI 字幕（分页）；YouTube captions 轨
async fn tool_subtitle(ch: &Channel, video_id: String, page: u64) -> anyhow::Result<String> {
    if video_id.trim().is_empty() {
        anyhow::bail!("缺少 videoId");
    }
    if page == 0 {
        anyhow::bail!("page 从 1 起");
    }
    match ch.kind.as_str() {
        "bilibili" => {
            let view = bili_get(ch, &format!("https://api.bilibili.com/x/web-interface/view?bvid={video_id}")).await?;
            api_ok(&view)?;
            let d = view.get("data").cloned().unwrap_or(Value::Null);
            let cid = d.pointer("/pages/0/cid").and_then(as_u64).or_else(|| d.get("cid").and_then(as_u64)).unwrap_or(0);
            if cid == 0 {
                anyhow::bail!("无法取得字幕轨 cid");
            }
            let player = bili_get(ch, &format!("https://api.bilibili.com/x/player/v2?bvid={video_id}&cid={cid}")).await?;
            let subs = player.pointer("/data/subtitle/subtitles").and_then(|s| s.as_array()).cloned().unwrap_or_default();
            let url = subs.iter()
                .find(|s| s.get("lan").and_then(|l| l.as_str()).map(|l| l.contains("zh")).unwrap_or(false))
                .or_else(|| subs.first())
                .and_then(|s| s.get("subtitle_url").and_then(|u| u.as_str()));
            let Some(mut url) = url.map(str::to_string) else {
                return Ok("该视频无字幕（B站无 CC 字幕且无 AI 字幕）。请用 video__danmaku / video__comments 了解内容与观众反应".into());
            };
            if url.starts_with("//") { url = format!("https:{url}"); }
            let doc = http_client().get(&url).send().await
                .map_err(|e| anyhow::anyhow!("字幕下载失败：{e}"))?
                .json::<Value>().await
                .map_err(|e| anyhow::anyhow!("字幕解析失败：{e}"))?;
            let lines = doc.get("body").and_then(|b| b.as_array()).cloned().unwrap_or_default();
            if lines.is_empty() {
                return Ok("该视频无字幕（字幕轨为空）".into());
            }
            format_subtitle_pages(&lines, page as usize)
        }
        "youtube" => {
            // captions.list 需 OAuth；用 API Key 能列轨，正文多数需权限——失败时明确降级说明
            let key = yt_key(ch)?;
            let body = yt_get("captions", &[("part", "snippet".into()), ("videoId", video_id.clone()), ("key", key)]).await?;
            let items = body.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            if items.is_empty() {
                return Ok("该视频无字幕（YouTube 无 caption 轨）。请用 video__summary 的简介与 video__comments 了解内容".into());
            }
            let track = items.iter()
                .find(|t| t.pointer("/snippet/language").and_then(|l| l.as_str()).map(|l| l.starts_with("zh")).unwrap_or(false))
                .or_else(|| items.first());
            let Some(track) = track else { return Ok("该视频无字幕".into()); };
            let lang = track.pointer("/snippet/language").and_then(|l| l.as_str()).unwrap_or("?");
            let name = track.pointer("/snippet/name").and_then(|l| l.as_str()).unwrap_or("");
            Ok(format!(
                "该视频有字幕轨（{lang} {name}），但正文下载需视频所有者授权（OAuth caption download）。\n建议：简介 + 评论区（video__comments）已足够形成观感；如需逐句内容可结合弹幕（若为B站）"
            ))
        }
        other => anyhow::bail!("不支持的平台：{other}"),
    }
}

fn format_subtitle_pages(lines: &[Value], page: usize) -> anyhow::Result<String> {
    let total_pages = lines.len().div_ceil(SUBTITLE_PAGE_LINES);
    if page > total_pages {
        return Ok(format!("没有更多字幕（共 {total_pages} 页，当前请求第 {page} 页）"));
    }
    let start = (page - 1) * SUBTITLE_PAGE_LINES;
    let end = std::cmp::min(start + SUBTITLE_PAGE_LINES, lines.len());
    let mut out = String::new();
    for l in &lines[start..end] {
        let from = l.get("from").and_then(|f| f.as_f64()).unwrap_or(0.0);
        let text = l.get("content").and_then(|c| c.as_str()).unwrap_or("");
        let mm = (from as i64) / 60;
        let ss = (from as i64) % 60;
        out.push_str(&format!("[{mm:02}:{ss:02}] {text}\n"));
    }
    let nav = if page < total_pages {
        format!("\n（第 {page}/{total_pages} 页；下一页传 page={}）", page + 1)
    } else {
        format!("\n（第 {page}/{total_pages} 页；已到末页）")
    };
    Ok(format!("{out}{nav}"))
}

/// video__danmaku：B站弹幕 protobuf 接口（分段抽样）
async fn tool_danmaku(ch: &Channel, video_id: String, segment: Option<i64>) -> anyhow::Result<String> {
    let view = bili_get(ch, &format!("https://api.bilibili.com/x/web-interface/view?bvid={video_id}")).await?;
    api_ok(&view)?;
    let d = view.get("data").cloned().unwrap_or(Value::Null);
    let aid = d.get("aid").and_then(as_u64).unwrap_or(0);
    let cid = d.pointer("/pages/0/cid").and_then(as_u64).or_else(|| d.get("cid").and_then(as_u64)).unwrap_or(0);
    if aid == 0 || cid == 0 {
        anyhow::bail!("无法定位弹幕轨（aid/cid 缺失）");
    }
    let duration = d.get("duration").and_then(as_u64).unwrap_or(0);
    let total_segs = std::cmp::max(1, duration.div_ceil(360) as i64); // 每段 6 分钟
    // 请求段：指定段或全部前 6 段（全片概览）
    let segs: Vec<i64> = match segment {
        Some(s) if (0..total_segs).contains(&s) => vec![s],
        Some(s) => {
            return Ok(format!("分段序号越界（请求段 {s}；该视频共 {total_segs} 段，有效 0..{}）", total_segs - 1))
        }
        None => (0..std::cmp::min(6, total_segs)).collect(),
    };
    let mut out = String::new();
    for seg in segs {
        // protobuf 弹幕：SegSoC 接口（非官方稳定 API；失败降级为结构化错误）
        let url = format!("https://api.bilibili.com/x/v2/dm/web/seg.so?type=1&oid={aid}&segment_index={}", seg + 1);
        let resp = bili_request(ch, http_client().get(&url)).send().await
            .map_err(|e| anyhow::anyhow!("弹幕请求失败（段 {seg}）：{e}"))?;
        if !resp.status().is_success() {
            anyhow::bail!("弹幕接口 HTTP {}（段 {seg}）；可改用评论/字幕继续", resp.status());
        }
        let bytes = resp.bytes().await.map_err(|e| anyhow::anyhow!("弹幕读取失败：{e}"))?;
        let dans = parse_danmaku(&bytes);
        if dans.is_empty() {
            if segment.is_some() {
                out.push_str(&format!("（段 {seg}：无弹幕）\n"));
            }
            continue;
        }
        let t0 = seg * 360;
        out.push_str(&format!("—— 时间段 {}:{} 至 {}:{} ——\n", t0 / 60, t0 % 60, (t0 + 360) / 60, (t0 + 360) % 60));
        // 抽样：按密度取前 25 条（弹幕流内高密度时刻天然靠前出现），限量防刷屏
        for (ts, text) in dans.iter().take(25) {
            let mm = (t0 + ts / 1000) / 60;
            let ss = (t0 + ts / 1000) % 60;
            out.push_str(&format!("[{mm:02}:{ss:02}] {text}\n"));
        }
    }
    if out.trim().is_empty() {
        return Ok("该视频（所请求分段）无弹幕数据".into());
    }
    if segment.is_none() {
        out.push_str(&format!("\n（全片概览：前 {} 段抽样；指定段传 segment 0..{} 深入）", std::cmp::min(6, total_segs), total_segs - 1));
    }
    Ok(out)
}

/// protobuf 弹幕粗解析（字段 6 = 内容文本；字段 2 附近为时间偏移 ms）。
/// 不引 prost 全量结构：只按 varint tag 流抽字段（够抽样展示，非完整还原）。
fn parse_danmaku(bytes: &[u8]) -> Vec<(i64, String)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        // 每条弹幕 = 长度前缀（varint）+ DanmakuElem 消息体
        let (len, adv) = match read_varint(&bytes[i..]) { Some(v) => v, None => break };
        i += adv;
        let end = std::cmp::min(i + len as usize, bytes.len());
        if let Some((ts, text)) = parse_danmaku_elem(&bytes[i..end]) {
            if !text.trim().is_empty() {
                out.push((ts, text));
            }
        }
        i = end;
    }
    out
}

fn parse_danmaku_elem(msg: &[u8]) -> Option<(i64, String)> {
    let mut i = 0usize;
    let mut ts = 0i64;
    let mut text = String::new();
    while i < msg.len() {
        let (tag, adv) = read_varint(&msg[i..])?;
        i += adv;
        let field = (tag >> 3) as u8;
        let wire = (tag & 0x7) as u8;
        match wire {
            0 => { let (v, a) = read_varint(&msg[i..])?; i += a; if field == 2 { ts = v as i64; } }
            2 => {
                let (l, a) = read_varint(&msg[i..])?; i += a;
                let end = std::cmp::min(i + l as usize, msg.len());
                if field == 6 || field == 7 || field == 9 {
                    if let Ok(s) = std::str::from_utf8(&msg[i..end]) {
                        if field == 6 && text.is_empty() { text = s.to_string(); }
                    }
                }
                i = end;
            }
            5 => i += 4,
            1 => i += 8,
            _ => return None,
        }
    }
    if text.is_empty() { None } else { Some((ts, text)) }
}

fn read_varint(b: &[u8]) -> Option<(u64, usize)> {
    let mut v = 0u64;
    let mut shift = 0u32;
    for (idx, byte) in b.iter().enumerate() {
        v |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 { return Some((v, idx + 1)); }
        shift += 7;
        if shift > 63 { return None; }
    }
    None
}

const COMMENTS_PAGE: usize = 10;

/// video__comments：评论区分页（B站支持回复展开提示；YouTube 翻页）
async fn tool_comments(
    ch: &Channel,
    st: &Arc<Mutex<WatchSession>>,
    video_id: String,
    page: u64,
    sort: &str,
) -> anyhow::Result<String> {
    if video_id.trim().is_empty() {
        anyhow::bail!("缺少 videoId");
    }
    if page == 0 { anyhow::bail!("page 从 1 起"); }
    match ch.kind.as_str() {
        "bilibili" => {
            let view = bili_get(ch, &format!("https://api.bilibili.com/x/web-interface/view?bvid={video_id}")).await?;
            api_ok(&view)?;
            let aid = view.pointer("/data/aid").and_then(as_u64).unwrap_or(0);
            if aid == 0 { anyhow::bail!("无法定位评论区（aid 缺失）"); }
            let sort_code = if sort == "time" { 0 } else { 2 };
            let body = bili_get(ch, &format!(
                "https://api.bilibili.com/x/v2/reply?type=1&oid={aid}&pn={page}&ps={COMMENTS_PAGE}&sort={sort_code}"
            )).await?;
            api_ok(&body)?;
            let page_info = body.pointer("/data/page").cloned().unwrap_or(Value::Null);
            let total = page_info.get("count").and_then(as_u64).unwrap_or(0);
            let replies = body.pointer("/data/replies").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            if replies.is_empty() {
                return Ok(format!("没有更多评论（第 {page} 页为空；共 {total} 条）"));
            }
            let mut st_lock = st.lock().unwrap();
            let mut out = format!("评论区（第 {page} 页，热度排序）共 {total} 条：\n");
            for r in &replies {
                let id = r.get("rpid").and_then(as_u64).map(|v| v.to_string()).unwrap_or_default();
                let author = r.pointer("/member/uname").and_then(|v| v.as_str()).unwrap_or("匿名");
                let text = r.pointer("/content/message").and_then(|v| v.as_str()).unwrap_or("");
                let likes = r.get("like").and_then(as_u64).unwrap_or(0);
                let rcount = r.get("rcount").and_then(as_u64).unwrap_or(0);
                st_lock.seen_comments.insert(id.clone());
                out.push_str(&format!("- [{likes}赞 {rcount}回复] {author}（id={id}）：{}\n", text.replace('\n', " ")));
            }
            out.push_str(&format!("\n（翻下一页传 page={}；回复某条用 video__act 的 reply.commentId）", page + 1));
            Ok(out)
        }
        "youtube" => {
            let key = yt_key(ch)?;
            let order = if sort == "time" { "time".to_string() } else { "relevance".to_string() };
            // 翻页经 pageToken 链：为免维护 token 链复杂度，用 maxResults 拉大窗口 + 内存分页
            let want = (page * COMMENTS_PAGE as u64).to_string();
            let body = yt_get("commentThreads", &[
                ("part", "snippet".into()),
                ("videoId", video_id.clone()),
                ("maxResults", want),
                ("order", order),
                ("textFormat", "plainText".into()),
                ("key", key),
            ]).await?;
            let items = body.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let start = ((page - 1) * COMMENTS_PAGE as u64) as usize;
            if start >= items.len() {
                return Ok(format!("没有更多评论（第 {page} 页为空）"));
            }
            let window: Vec<Value> = items.into_iter().skip(start).take(COMMENTS_PAGE).collect();
            let mut st_lock = st.lock().unwrap();
            let mut out = format!("评论区（第 {page} 页）：\n");
            for x in &window {
                let Some(sn) = x.pointer("/snippet/topLevelComment") else { continue };
                let id = sn.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let author = sn.pointer("/snippet/authorDisplayName").and_then(|v| v.as_str()).unwrap_or("匿名");
                let text = sn.pointer("/snippet/textDisplay").and_then(|v| v.as_str()).unwrap_or("");
                let likes = sn.pointer("/snippet/likeCount").and_then(as_u64).unwrap_or(0);
                let rcount = x.pointer("/snippet/totalReplyCount").and_then(as_u64).unwrap_or(0);
                st_lock.seen_comments.insert(id.to_string());
                out.push_str(&format!("- [{likes}赞 {rcount}回复] {author}（id={id}）：{}\n", text.replace('\n', " ")));
            }
            out.push_str(&format!("\n（翻下一页传 page={}）", page + 1));
            Ok(out)
        }
        other => anyhow::bail!("不支持的平台：{other}"),
    }
}

/// video__uploader：UP主信息 + 近期投稿
async fn tool_uploader(ch: &Channel, uploader_id: String) -> anyhow::Result<String> {
    if uploader_id.trim().is_empty() {
        anyhow::bail!("缺少 uploaderId");
    }
    match ch.kind.as_str() {
        "bilibili" => {
            let card = bili_get(ch, &format!("https://api.bilibili.com/x/web-interface/card?mid={uploader_id}")).await?;
            api_ok(&card)?;
            let d = card.get("data").cloned().unwrap_or(Value::Null);
            let name = d.pointer("/card/name").and_then(|v| v.as_str()).unwrap_or("（未知）");
            let sign = d.pointer("/card/sign").and_then(|v| v.as_str()).unwrap_or("（无签名）");
            let fans = d.pointer("/follower").and_then(as_u64).unwrap_or(0);
            let videos = d.pointer("/archive").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let mut out = format!("UP主：{name}\n粉丝：{fans}\n签名：{sign}\n\n近期投稿（{}）：\n", videos.len());
            for v in videos.iter().take(8) {
                let t = v.get("title").and_then(|v| v.as_str()).unwrap_or("（无题）");
                let play = v.get("play").and_then(as_u64).unwrap_or(0);
                let created = v.get("created").and_then(|c| c.as_i64()).unwrap_or(0);
                let dt = chrono::DateTime::from_timestamp(created, 0).map(|x| x.format("%Y-%m-%d").to_string()).unwrap_or_default();
                out.push_str(&format!("- {t}（{play}播放 {dt}）\n"));
            }
            Ok(out)
        }
        "youtube" => {
            let key = yt_key(ch)?;
            let ch_body = yt_get("channels", &[("part", "snippet,statistics".into()), ("id", uploader_id.clone()), ("key", key.clone())]).await?;
            let c = ch_body.pointer("/items/0").cloned().unwrap_or(Value::Null);
            if c.is_null() { anyhow::bail!("未找到频道：{uploader_id}"); }
            let name = c.pointer("/snippet/title").and_then(|v| v.as_str()).unwrap_or("（未知）");
            let desc = c.pointer("/snippet/description").and_then(|v| v.as_str()).unwrap_or("（无简介）");
            let subs = c.pointer("/statistics/subscriberCount").and_then(as_u64).unwrap_or(0);
            // 近期投稿：search by channelId（date 序）
            let uploads = yt_get("search", &[
                ("part", "snippet".into()),
                ("channelId", uploader_id.clone()),
                ("order", "date".into()),
                ("type", "video".into()),
                ("maxResults", "8".into()),
                ("key", key),
            ]).await?;
            let items = uploads.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let mut out = format!("频道：{name}\n订阅：{subs}\n简介：{desc}\n\n近期投稿：\n");
            for x in items.iter().take(8) {
                let t = x.pointer("/snippet/title").and_then(|v| v.as_str()).unwrap_or("（无题）");
                let dt = x.pointer("/snippet/publishedAt").and_then(|v| v.as_str()).unwrap_or("");
                let vid = x.pointer("/id/videoId").and_then(|v| v.as_str()).unwrap_or("");
                out.push_str(&format!("- {t}（{dt} id={vid}）\n"));
            }
            Ok(out)
        }
        other => anyhow::bail!("不支持的平台：{other}"),
    }
}

/// video__related：B站相关视频
async fn tool_related(ch: &Channel, video_id: String) -> anyhow::Result<String> {
    let view = bili_get(ch, &format!("https://api.bilibili.com/x/web-interface/view?bvid={video_id}")).await?;
    api_ok(&view)?;
    let aid = view.pointer("/data/aid").and_then(as_u64).unwrap_or(0);
    if aid == 0 { anyhow::bail!("无法定位相关视频（aid 缺失）"); }
    let rel = bili_get(ch, &format!("https://api.bilibili.com/x/web-interface/archive/related?aid={aid}")).await?;
    api_ok(&rel)?;
    let list = rel.get("data").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if list.is_empty() { return Ok("无相关视频推荐".into()); }
    let mut out = format!("相关/推荐视频（{}）：\n", list.len().min(10));
    for x in list.iter().take(10) {
        let bv = x.get("bvid").and_then(|v| v.as_str()).unwrap_or("?");
        let t = x.get("title").and_then(|v| v.as_str()).unwrap_or("（无题）");
        let owner = x.pointer("/owner/name").and_then(|v| v.as_str()).unwrap_or("?");
        let play = x.pointer("/stat/view").and_then(as_u64).unwrap_or(0);
        out.push_str(&format!("- {t}｜{owner}｜{play}播放（id={bv}）\n"));
    }
    Ok(out)
}

/// video__search：双平台搜索（结构同候选清单）
async fn tool_search(ch: &Channel, query: String) -> anyhow::Result<String> {
    if query.trim().is_empty() {
        anyhow::bail!("缺少 query");
    }
    let s = Settings::from_channel(ch);
    let items = match ch.kind.as_str() {
        "bilibili" => bili_search(ch, &query, &s).await?,
        "youtube" => yt_search(ch, &query, &s).await?,
        other => anyhow::bail!("不支持的平台：{other}"),
    };
    if items.is_empty() {
        return Ok(format!("搜索「{query}」无结果（可能被风控或无匹配；可换词重试）"));
    }
    let mut out = format!("搜索「{query}」结果（{}）：\n", items.len().min(10));
    for v in items.iter().take(10) {
        out.push_str(&format!(
            "- {}｜{}｜{}播放（id={}）\n",
            v.title, v.author, v.views, v.id
        ));
    }
    Ok(out)
}

// ---------------------------------------------------------------- video__act（互动闸门链）

/// act 闸门上下文（每轮观看开始时构建：配额/配置快照）
pub(crate) struct ActGate {
    pub comment_on: bool,
    pub reply_on: bool,
    pub dry_run: bool,
    /// 审批模式：off = 直接发；always = 产审批单（批准代发/拒绝跳过）
    pub approval: String,
    pub daily_limit: u32,
    pub max_comment_chars: usize,
    /// 每日已发计数（由 runner 在 VideoState 维护，act 内检查+递增）
    pub actions_today: std::sync::atomic::AtomicU32,
}

impl ActGate {
    pub fn from_settings(s: &Settings, actions_today: u32) -> Self {
        ActGate {
            comment_on: s.comment,
            reply_on: s.reply,
            dry_run: s.dry_run,
            approval: s.approval.clone(),
            daily_limit: s.daily_limit,
            max_comment_chars: s.max_comment_chars,
            actions_today: std::sync::atomic::AtomicU32::new(actions_today),
        }
    }
}

/// 全局 act 闸门表（session_id → gate；每轮观看开始注册，结束清理）
fn act_gates() -> &'static Mutex<std::collections::HashMap<String, Arc<ActGate>>> {
    static G: std::sync::OnceLock<Mutex<std::collections::HashMap<String, Arc<ActGate>>>> =
        std::sync::OnceLock::new();
    G.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

pub(crate) fn register_act_gate(session_id: &str, gate: ActGate) {
    act_gates().lock().unwrap().insert(session_id.to_string(), Arc::new(gate));
}

pub(crate) fn drop_act_gate(session_id: &str) {
    act_gates().lock().unwrap().remove(session_id);
}

/// act 后回取当日计数（runner 同步回 VideoState）
pub(crate) fn act_gate_count(session_id: &str) -> u32 {
    act_gates()
        .lock()
        .unwrap()
        .get(session_id)
        .map(|g| g.actions_today.load(std::sync::atomic::Ordering::SeqCst))
        .unwrap_or(0)
}

/// video__act：评论/回复（闸门链：开关→目标核验→清洗→配额→审批/dry-run→buvid→执行）
async fn tool_act(
    core: &Arc<Core>,
    ch: &Channel,
    st: &Arc<Mutex<WatchSession>>,
    session: &str,
    args: Value,
) -> anyhow::Result<String> {
    let video_id = args.get("videoId").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if video_id.is_empty() {
        anyhow::bail!("缺少 videoId");
    }
    let Some(gate) = act_gates().lock().unwrap().get(session).cloned() else {
        anyhow::bail!("本会话无互动闸门（非观看轮次或已收束）；仅可观看不可互动");
    };
    let comment_text = args.get("comment").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let reply_text = args.get("replyText").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let reply_target = args.get("replyTo").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();

    // 构造动作（comment 或 reply 二选一）
    let (kind, raw_text, target): (&str, String, Option<String>) = if !reply_text.is_empty() {
        if !gate.reply_on {
            return Ok("已拒绝：本通道未开启回复（config.reply=false）".into());
        }
        // 闸门 1：回复目标必须来自本会话已见评论（防编造 id）
        if !st.lock().unwrap().seen_comments.contains(&reply_target) {
            return Ok(format!(
                "已拒绝：目标评论未见（id={reply_target} 不在本会话已获取的评论集合内）。请先用 video__comments 获取评论再回复"
            ));
        }
        ("回复", reply_text, Some(reply_target))
    } else if !comment_text.is_empty() {
        if !gate.comment_on {
            return Ok("已拒绝：本通道未开启评论（config.comment=false）".into());
        }
        ("评论", comment_text, None)
    } else {
        anyhow::bail!("需提供 comment（评论）或 replyText+replyTo（回复），不能都为空");
    };

    // 闸门 2：内容清洗（字数上限 + markdown/指令痕迹去除）
    let text = clean_comment(&raw_text, gate.max_comment_chars);
    if text.trim().is_empty() {
        return Ok(format!("已拒绝：清洗后文本为空（原文 {} 字）", raw_text.chars().count()));
    }

    // 闸门 3：每日配额
    let today = gate.actions_today.load(std::sync::atomic::Ordering::SeqCst);
    if today >= gate.daily_limit {
        return Ok(format!(
            "已拒绝：今日互动已达上限（{today}/{} 次）。可继续观看并在观感中表述，明日再互动",
            gate.daily_limit
        ));
    }

    // 定位视频（B站需 aid）
    let mut item = VideoItem {
        platform: ch.kind.clone(),
        id: video_id.clone(),
        aid: None,
        title: String::new(),
        description: String::new(),
        author: String::new(),
        views: 0,
    };
    if ch.kind == "bilibili" {
        let view = bili_get(ch, &format!("https://api.bilibili.com/x/web-interface/view?bvid={video_id}")).await?;
        api_ok(&view)?;
        item.aid = view.pointer("/data/aid").and_then(as_u64);
        item.title = view.pointer("/data/title").and_then(|v| v.as_str()).unwrap_or("").to_string();
    }

    // 闸门 4：dry-run 分流（不落平台）
    if gate.dry_run {
        gate.actions_today.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        st.lock().unwrap().acted += 1;
        return Ok(format!(
            "[dry-run] 拟{}：{}{}｜目标视频 {}（{}）\n（dry-run 模式开启，未发送至平台；正式发送请在通道配置关闭 dryRun）",
            kind, text,
            target.map(|t| format!("（回复 id={t}）")).unwrap_or_default(),
            if item.title.trim().is_empty() { &video_id } else { &item.title },
            video_id,
        ));
    }

    // 闸门 5：审批模式（always = 产审批单，批准代发/拒绝跳过；off = 直接发）
    if gate.approval == "always" {
        return create_act_approval(core, ch, session, st, &gate, &kind, &text, target.as_ref(), &item, &video_id)
            .await;
    }

    // 闸门 6：B站 buvid 指纹养护（发前自动补齐）
    if ch.kind == "bilibili" {
        bili_prime_buvid(ch).await;
    }

    // 执行
    post_comment(ch, &item, &text, target.as_deref()).await?;
    gate.actions_today.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    {
        let mut s = st.lock().unwrap();
        s.acted += 1;
        s.acted_notes.push(format!("{kind}：{}", text.chars().take(60).collect::<String>()));
    }
    Ok(format!("{kind}已发送：{}（视频 {}）", text.chars().take(60).collect::<String>(), video_id))
}

/// 审批分流的代发执行入口（Core::approval_decide 的 [video] 回调）：
/// 载荷 = { channelId, videoId, text, replyTo }；buvid 养护后直接发。
pub(crate) async fn execute_act_payload(core: &Arc<Core>, payload: &str) -> anyhow::Result<String> {
    let v: Value = serde_json::from_str(payload)
        .map_err(|e| anyhow::anyhow!("视频审批载荷解析失败：{e}"))?;
    let channels = platform::load_channels(core);
    let channel_id = v.get("channelId").and_then(|x| x.as_str()).unwrap_or_default();
    let Some(ch) = channels.iter().find(|c| c.id == channel_id) else {
        anyhow::bail!("通道不存在：{channel_id}");
    };
    let video_id = v.get("videoId").and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let text = v.get("text").and_then(|x| x.as_str()).unwrap_or_default().to_string();
    let reply_to = v.get("replyTo").and_then(|x| x.as_str()).map(str::to_string);
    let mut item = VideoItem {
        platform: ch.kind.clone(),
        id: video_id.clone(),
        aid: None,
        title: String::new(),
        description: String::new(),
        author: String::new(),
        views: 0,
    };
    if ch.kind == "bilibili" {
        let view = bili_get(ch, &format!("https://api.bilibili.com/x/web-interface/view?bvid={video_id}")).await?;
        api_ok(&view)?;
        item.aid = view.pointer("/data/aid").and_then(as_u64);
        bili_prime_buvid(ch).await;
    }
    post_comment(ch, &item, &text, reply_to.as_deref()).await?;
    Ok(format!("已代发（审批通过）：{}", text.chars().take(60).collect::<String>()))
}

/// 产审批单并等待裁决（approval=always 分流）：
/// 载荷带通道/视频/文本/回复目标；批准 → 系统代发；拒绝/超时 → 跳过。
#[allow(clippy::too_many_arguments)]
async fn create_act_approval(
    core: &Arc<Core>,
    ch: &Channel,
    session: &str,
    st: &Arc<Mutex<WatchSession>>,
    gate: &Arc<ActGate>,
    kind: &str,
    text: &str,
    target: Option<&String>,
    item: &VideoItem,
    video_id: &str,
) -> anyhow::Result<String> {
    let payload = serde_json::json!({
        "channelId": ch.id,
        "videoId": video_id,
        "text": text,
        "replyTo": target,
    });
    let req = exm_core::types::ApprovalRequest {
        id: format!("apr{}", &exm_core::types::new_id()[..14]),
        session_id: session.to_string(),
        node_id: None,
        agent_id: "video".into(),
        command: format!("[video] {}", payload),
        status: "pending".into(),
        result: None,
        created_at: exm_core::types::now_iso(),
        decided_at: None,
        origin_user: None,
        origin_role: None,
    };
    core.store.add_approval(&req)?;
    let _ = core.events.send(exm_core::types::CoreEvent {
        kind: "approval.required".into(),
        session_id: session.to_string(),
        payload: serde_json::json!({
            "approvalId": req.id,
            "agentId": "video",
            "command": format!("[video] 拟{kind}《{}》：{}", item.title, text),
        }),
    });
    // 轮询裁决（与命令审批闸门同口径：等待窗口 = 全局 approval_wait_secs）
    let wait = core.config().security.approval_wait_secs;
    if wait == 0 {
        return Ok(format!(
            "{kind}已进入人工审批（审批单 {}；拟发：{}）。继续你的观看，无需等待裁决",
            req.id, text
        ));
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(wait);
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        let Ok(Some(latest)) = core.store.get_approval(&req.id) else {
            continue;
        };
        match latest.status.as_str() {
            "executed" => {
                gate.actions_today.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                st.lock().unwrap().acted += 1;
                return Ok(format!(
                    "{kind}已被用户批准并代发：{}",
                    latest.result.unwrap_or_default()
                ));
            }
            "failed" => {
                return Ok(format!(
                    "{kind}审批通过但代发失败：{}",
                    latest.result.unwrap_or_default()
                ));
            }
            "denied" => {
                st.lock()
                    .unwrap()
                    .skipped
                    .push(format!("{kind}被用户拒绝（审批单 {}）", req.id));
                return Ok(format!("{kind}被用户拒绝（审批单 {}），本轮跳过", req.id));
            }
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(format!(
                "{kind}审批等待超时（审批单 {} 未裁决）；本轮跳过，可继续观看",
                req.id
            ));
        }
    }
}

// ---------------------------------------------------------------- 单测

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 字幕分页_窗口与导航() {
        let lines: Vec<Value> = (0..300)
            .map(|i| json!({ "from": i as f64 * 3.0, "content": format!("第{i}句") }))
            .collect();
        let p1 = format_subtitle_pages(&lines, 1).unwrap();
        assert!(p1.contains("第0句") && p1.contains("第119句"));
        assert!(p1.contains("page=2"), "应提示下一页：{p1}");
        let p3 = format_subtitle_pages(&lines, 3).unwrap();
        assert!(p3.contains("已到末页"), "第 3 页（300/120=2.5→3 页）末页：{p3}");
        let over = format_subtitle_pages(&lines, 4).unwrap();
        assert!(over.contains("没有更多"));
    }

    #[test]
    fn 弹幕解析_varint流() {
        // 构造一条最小弹幕记录：field2(varint 时间) + field6(文本)
        fn elem(ts: u64, text: &str) -> Vec<u8> {
            let mut msg = vec![0x10]; // field 2, wire 0
            let mut v = ts;
            loop {
                let b = (v & 0x7f) as u8;
                v >>= 7;
                if v == 0 { msg.push(b); break; } else { msg.push(b | 0x80); }
            }
            let tb = text.as_bytes();
            let mut l = tb.len() as u64;
            let mut len_bytes = Vec::new();
            loop {
                let b = (l & 0x7f) as u8;
                l >>= 7;
                if l == 0 { len_bytes.push(b); break; } else { len_bytes.push(b | 0x80); }
            }
            msg.push(0x32); // field 6, wire 2
            msg.extend_from_slice(&len_bytes);
            msg.extend_from_slice(tb);
            // 外层长度前缀
            let mut out = Vec::new();
            let mut n = msg.len() as u64;
            loop {
                let b = (n & 0x7f) as u8;
                n >>= 7;
                if n == 0 { out.push(b); break; } else { out.push(b | 0x80); }
            }
            out.extend_from_slice(&msg);
            out
        }
        let mut buf = elem(1500, "前方高能");
        buf.extend_from_slice(&elem(61000, "哈哈哈哈"));
        let dans = parse_danmaku(&buf);
        assert_eq!(dans.len(), 2);
        assert_eq!(dans[0].1, "前方高能");
        assert_eq!(dans[0].0, 1500);
        assert_eq!(dans[1].1, "哈哈哈哈");
    }

    #[test]
    fn 工具schema_平台差异() {
        let bili = watch_tool_specs("bilibili");
        assert_eq!(bili.len(), 7, "B站 7 工具：{bili:?}");
        assert!(bili.iter().any(|t| t.name == "video__danmaku"));
        assert!(bili.iter().any(|t| t.name == "video__related"));

        let yt = watch_tool_specs("youtube");
        assert_eq!(yt.len(), 5, "YouTube 5 工具（无 danmaku/related）：{yt:?}");
        assert!(!yt.iter().any(|t| t.name == "video__danmaku"));

        // act 独立注册
        let act = act_tool_spec();
        assert_eq!(act.name, "video__act");
        assert!(act.parameters["properties"]["comment"].is_object());
        assert!(act.parameters["properties"]["replyTo"].is_object());
    }

    fn test_core(tag: &str) -> Arc<Core> {
        let dir = std::env::temp_dir().join(format!("exm-act-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        Arc::new(Core::with_config(cfg).expect("Core 创建失败"))
    }

    #[tokio::test]
    async fn act闸门_目标未见与配额拒绝() {
        let core = test_core("gate");
        // YouTube 通道（无需 B站 cookie；dry-run 路径不触网）
        let ch = Channel {
            id: "yt-test".into(),
            kind: "youtube".into(),
            enabled: true,
            group: None,
            allowed_chats: vec![],
            account: None,
            secret: None,
            token: None,
            reply_webhook: None,
            group_gate: None,
            capability: "computer".into(),
            memory_isolation: false,
            created_at: String::new(),
            config: [
                ("comment".to_string(), "true".to_string()),
                ("reply".to_string(), "true".to_string()),
                ("dryRun".to_string(), "true".to_string()),
                ("apiKey".to_string(), "test".to_string()),
            ].into_iter().collect(),
        };
        let settings = Settings::from_channel(&ch);
        assert!(settings.comment && settings.dry_run);
        // 配额测试需要 limit=1：显式注入（默认 6 不够严格）
        let mut gate = ActGate::from_settings(&settings, 0);
        gate.daily_limit = 1;
        register_act_gate("s1", gate);
        let st = session_state("s1");

        // 回复目标未见 → 拒绝
        let r = dispatch_tool(
            &core,
            "video__act",
            &ch,
            "s1",
            &st,
            json!({ "videoId": "vid1", "replyText": "你好", "replyTo": "ghost-id" }),
        )
        .await
        .unwrap();
        assert!(r.contains("目标评论未见"), "{r}");

        // 正常评论（dry-run）→ 回执不落平台
        let r = dispatch_tool(
            &core,
            "video__act",
            &ch,
            "s1",
            &st,
            json!({ "videoId": "vid1", "comment": "测试评论内容" }),
        )
        .await
        .unwrap();
        assert!(r.contains("[dry-run]"), "{r}");
        assert_eq!(act_gate_count("s1"), 1);

        // 配额耗尽（limit=1 已用）→ 拒绝并告知
        let r = dispatch_tool(
            &core,
            "video__act",
            &ch,
            "s1",
            &st,
            json!({ "videoId": "vid1", "comment": "再来一条" }),
        )
        .await
        .unwrap();
        assert!(r.contains("已达上限"), "{r}");

        drop_act_gate("s1");
        drop_session("s1");
    }

    #[tokio::test]
    async fn act闸门_评论开关关闭拒绝() {
        let ch = Channel {
            id: "yt-off".into(),
            kind: "youtube".into(),
            enabled: true,
            group: None,
            allowed_chats: vec![],
            account: None,
            secret: None,
            token: None,
            reply_webhook: None,
            group_gate: None,
            capability: "computer".into(),
            memory_isolation: false,
            created_at: String::new(),
            config: [
                ("comment".to_string(), "false".to_string()),
                ("dryRun".to_string(), "true".to_string()),
            ].into_iter().collect(),
        };
        let settings = Settings::from_channel(&ch);
        register_act_gate("s2", ActGate::from_settings(&settings, 0));
        let st = session_state("s2");
        let r = dispatch_tool(
            &test_core("off"),
            "video__act",
            &ch,
            "s2",
            &st,
            json!({ "videoId": "v", "comment": "hi" }),
        )
        .await
        .unwrap();
        assert!(r.contains("未开启评论"), "{r}");
        drop_act_gate("s2");
        drop_session("s2");
    }
}

#[cfg(test)]
mod act_approval_tests {
    use super::*;
    use exm_core::{config::ExmConfig, Core};

    fn test_core() -> Arc<Core> {
        let dir = std::env::temp_dir().join(format!("exm-act-apr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        Arc::new(Core::with_config(cfg).expect("Core 创建失败"))
    }

    fn yt_channel() -> Channel {
        Channel {
            id: "yt-apr".into(),
            kind: "youtube".into(),
            enabled: true,
            group: None,
            allowed_chats: vec![],
            account: None,
            secret: None,
            reply_webhook: None,
            token: None,
            config: [
                ("comment".to_string(), "true".to_string()),
                ("dryRun".to_string(), "false".to_string()),
                ("approval".to_string(), "always".to_string()),
            ]
            .into_iter()
            .collect(),
            group_gate: None,
            capability: "computer".into(),
            memory_isolation: false,
            created_at: String::new(),
        }
    }

    /// approval=always：act 落审批单（wait=0 立即返回不阻塞），不直接发平台
    #[tokio::test]
    async fn act审批_产审批单不落平台() {
        let core = test_core();
        let ch = yt_channel();
        let s = Settings::from_channel(&ch);
        assert_eq!(s.approval, "always");
        assert!(!s.dry_run);
        register_act_gate("s-apr", ActGate::from_settings(&s, 0));
        let st = session_state("s-apr");
        let r = dispatch_tool(
            &core,
            "video__act",
            &ch,
            "s-apr",
            &st,
            json!({ "videoId": "vid1", "comment": "审批模式下的拟发评论" }),
        )
        .await
        .unwrap();
        // wait 语义：等待窗口内即时回执「已进入人工审批」；超时回执「等待超时本轮跳过」。
        // 两者都证明未直接落平台（审批分流生效），断言取并集。
        assert!(
            r.contains("人工审批") || r.contains("等待超时"),
            "应走审批分流而非直接发送：{r}"
        );
        // 审批单已落库且 pending
        let pending = core.approval_list(Some("pending"), 10).unwrap();
        assert_eq!(pending.len(), 1, "应有一张待审批单");
        assert!(pending[0].command.starts_with("[video]"), "审批单应带 [video] 前缀");
        assert!(pending[0].command.contains("审批模式下的拟发评论"), "审批单含拟发文本");
        drop_act_gate("s-apr");
        drop_session("s-apr");
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }
}
