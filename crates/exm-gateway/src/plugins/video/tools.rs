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
use exm_core::tools::{dynamic_exec_images, DynamicToolExec};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::Arc;

/// 单个观看会话的运行期状态（工具间共享：已见评论/视频/步数/配额引用）
#[derive(Default)]
pub(crate) struct WatchSession {
    /// videoId 归一缓存（原始输入 → 归一 id；同会话重复输入零重复解析）
    pub resolved_ids: std::collections::HashMap<String, String>,
    /// 已校验的 B 站字幕快照（按通道 + 视频隔离，最近使用的放末尾）。分页不重新选轨。
    subtitle_snapshots: Vec<Arc<BiliSubtitleSnapshot>>,
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

impl WatchSession {
    fn cached_subtitle(&mut self, channel: &str, video: &str) -> Option<Arc<BiliSubtitleSnapshot>> {
        let i = self
            .subtitle_snapshots
            .iter()
            .position(|s| s.channel_id == channel && matches_bvid(Some(&s.video_id), video))?;
        let snapshot = self.subtitle_snapshots.remove(i);
        self.subtitle_snapshots.push(snapshot.clone());
        Some(snapshot)
    }

    fn pin_subtitle(&mut self, snapshot: BiliSubtitleSnapshot) -> Arc<BiliSubtitleSnapshot> {
        // 同轮并发请求可能各自下载一次；只采用先完成的快照，后续页保持同一轨道/总页数。
        if let Some(existing) = self.cached_subtitle(&snapshot.channel_id, &snapshot.video_id) {
            return existing;
        }
        // 普通点播会话可长时间存活，避免字幕全文缓存随观看数量无限增长。
        if self.subtitle_snapshots.len() >= 8 {
            self.subtitle_snapshots.remove(0);
        }
        let snapshot = Arc::new(snapshot);
        self.subtitle_snapshots.push(snapshot.clone());
        snapshot
    }
}

/// 全局会话表（session_id → 观看状态；进程生命周期）
fn watch_sessions() -> &'static Mutex<std::collections::HashMap<String, Arc<Mutex<WatchSession>>>> {
    static S: std::sync::OnceLock<
        Mutex<std::collections::HashMap<String, Arc<Mutex<WatchSession>>>>,
    > = std::sync::OnceLock::new();
    S.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// 取（或创建）会话的观看状态
pub(crate) fn session_state(session_id: &str) -> Arc<Mutex<WatchSession>> {
    let mut map = watch_sessions().lock();
    map.entry(session_id.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(WatchSession::default())))
        .clone()
}

/// 会话结束清理（轮次收束后调用；防内存缓慢增长）
pub(crate) fn drop_session(session_id: &str) {
    watch_sessions().lock().remove(session_id);
}

// ---------------------------------------------------------------- videoId 归一解析

/// videoId 归一：裸号直通 / 长链正则抽取 / b23.tv 短链一层重定向展开。
/// 解析失败原样返回（下游平台接口报「视频不存在」）。
/// 同步部分纯字符串处理；短链展开是唯一的异步分支（调用方 await）。
pub(crate) fn normalize_video_id(input: &str) -> String {
    let raw = input.trim();
    if raw.is_empty() {
        return String::new();
    }
    // 已是裸号形态（BV… / av… / YouTube 11 位 id）直接采用
    if is_bare_id(raw) {
        return raw.to_string();
    }
    // B 站长链：bilibili.com/video/BV…（剔除 ?p= 分 P 等查询参数）
    if let Some(id) = extract_bili_from_url(raw) {
        return id;
    }
    // YouTube 三形态：watch?v= / youtu.be/ / shorts/
    if let Some(id) = extract_yt_from_url(raw) {
        return id;
    }
    // 兜底：去掉空白后原样透传（可能是裸号的变体写法）
    raw.to_string()
}

/// 是否裸视频号形态（不含 URL 结构）
fn is_bare_id(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("bv") && s.len() >= 8 {
        return true; // BV1xxxx…
    }
    if let Some(num) = lower.strip_prefix("av") {
        return !num.is_empty() && num.chars().all(|c| c.is_ascii_digit());
    }
    // YouTube videoId：恰 11 位、字母数字-_、非 URL
    s.len() == 11
        && !s.contains('/')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 从 B 站 URL 抽取 BV 号（bilibili.com/video/BV…；容忍查询串/锚点）
fn extract_bili_from_url(url: &str) -> Option<String> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"(?:bilibili\.com|b23\.tv)/video/(BV[0-9A-Za-z]{8,})").unwrap()
    });
    re.captures(url)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// 从 YouTube URL 抽取 videoId（watch?v= / youtu.be/ / shorts/ / live/）
fn extract_yt_from_url(url: &str) -> Option<String> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(
            r#"(?:youtube\.com/(?:watch\?v=|shorts/|live/)|youtu\.be/)([0-9A-Za-z_-]{11})"#,
        )
        .unwrap()
    });
    re.captures(url)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// b23.tv 短链一层重定向展开（GET 跟随一次 Location；失败返回 None 原样透传）
pub(crate) async fn expand_b23(input: &str) -> Option<String> {
    let short = input.trim();
    if !short.contains("b23.tv") {
        return None;
    }
    // 归一短链形态（无协议头补 https://）
    let url = b23_url(short);
    let resp = super::head_no_redirect(&url).await.ok()?;
    let loc = resp.headers().get("location")?.to_str().ok()?.to_string();
    extract_bili_from_url(&loc)
}

fn b23_url(short: &str) -> String {
    if let Some(rest) = short.strip_prefix("http://") {
        format!("https://{rest}")
    } else if short.starts_with("https://") {
        short.to_string()
    } else {
        format!("https://{short}")
    }
}

/// 分发入口的归一收口：同步归一 + 短链展开 + 会话缓存
async fn resolve_video_id(st: &Arc<Mutex<WatchSession>>, input: &str) -> String {
    let raw = input.trim().to_string();
    if raw.is_empty() {
        return raw;
    }
    // 缓存命中
    if let Some(hit) = st.lock().resolved_ids.get(&raw) {
        return hit.clone();
    }
    let mut id = normalize_video_id(&raw);
    if id == raw && raw.contains("b23.tv") {
        if let Some(expanded) = expand_b23(&raw).await {
            id = expanded;
        }
    }
    st.lock().resolved_ids.insert(raw, id.clone());
    id
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
        response_max_chars: None,
        response_select: None,
    }
}

fn video_id_param(desc: &str) -> Value {
    json!({
        "type": "object",
        "properties": { "videoId": { "type": "string", "description": format!("{desc}；可直接传完整链接或 b23.tv 短链（系统自动归一）") } },
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
            "字幕全文（带时间戳，超长分页）；先取第 1 页建立固定字幕快照，续页沿用同一 videoId；无字幕或来源校验失败时明确告知",
            json!({
                "type": "object",
                "properties": {
                    "videoId": { "type": "string", "description": "视频标识" },
                    "page": { "type": "integer", "description": "页码（从 1 起；缺省第 1 页；续页须已取同一 videoId 的第 1 页）" }
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
        // 画面观看（视觉模态）：B站官方 videoshot 雪碧图抽帧——真正「看到」画面
        out.push(spec(
            "video__shots",
            "视频画面抽帧（视觉直看）：按时间轴抽取关键帧图像直接供视觉观察——画面构图/字幕画面/UI 演示等文本工具拿不到的信息。建议与 summary/subtitle 配合使用",
            json!({
                "type": "object",
                "properties": {
                    "videoId": { "type": "string", "description": "视频标识（bvid）；可直接传完整链接或 b23.tv 短链（系统自动归一）" },
                    "startSec": { "type": "integer", "description": "起始时间（秒；缺省 = 开头）" },
                    "count": { "type": "integer", "description": "抽帧数 1-8（缺省 4；每帧间隔约 10-60 秒，覆盖视频不同阶段）" }
                },
                "required": ["videoId"]
            }),
        ));
    }
    out
}

/// 互动工具 schema（video__act；评论开关与闸门由执行面判定）
pub(crate) fn act_tool_spec() -> CustomTool {
    spec(
        "video__act",
        "互动动作：发表评论或回复（回复目标必须是本会话已见评论）。评论须像真人的随手短评：一句一个具体点，无套话无排比无总结升华，延续你自己的说话方式；受每日配额/字数清洗/dry-run/审批闸门约束",
        json!({
            "type": "object",
            "properties": {
                "videoId": { "type": "string", "description": "视频标识" },
                "comment": { "type": "string", "description": "评论文本（对视频直接评论；与 replyText 二选一）。写法：短、具体、口语化——说一个真实的点（画面/台词/细节），不要面面俱到，不要「总的来说」类套话，不要排比与感叹号轰炸，纯文本无 Markdown" },
                "replyText": { "type": "string", "description": "回复文本（与 comment 二选一，需同时给 replyTo）。写法同 comment：短、具体、像聊天" },
                "replyTo": { "type": "string", "description": "回复目标评论 id（须为本会话已见评论的 id）" }
            },
            "required": ["videoId"]
        }),
    )
}

// ---------------------------------------------------------------- 全局点播注册

/// 全局点播注册表指纹（上次注册时的通道绑定快照；变更即重建）。
/// Vec<(平台, 通道 id + fingerprint)>：任一平台通道增删/启停/凭据变化即重建工具闭包。
type OndemandFp = Vec<(String, String)>;
type OndemandRegistration = (std::sync::Weak<exm_core::tools::ToolGateway>, OndemandFp);

fn ondemand_fp() -> &'static Mutex<Option<OndemandRegistration>> {
    static F: std::sync::OnceLock<Mutex<Option<OndemandRegistration>>> = OnceLock::new();
    F.get_or_init(|| Mutex::new(None))
}

/// 维护全局点播工具面：存在启用的视频通道 → 注册 video__* 为全局动态工具
/// （全体个体可见）；全部停用/删除 → 注销。多平台时按平台各绑首个启用通道。
/// 与 reconcile() 同周期调用（10s 内收敛）。
pub(crate) fn reconcile_ondemand(core: &Arc<Core>) {
    // 各平台取首个启用通道（与 push_channel_text 取 first 同口径）
    let mut by_kind: std::collections::BTreeMap<String, Channel> =
        std::collections::BTreeMap::new();
    if super::plugin_enabled(core) {
        for ch in platform::load_channels(core) {
            if ch.enabled && super::is_video_platform(&ch.kind) && !by_kind.contains_key(&ch.kind) {
                by_kind.insert(ch.kind.clone(), ch);
            }
        }
    }
    // 注册指纹：平台 → 通道 fingerprint（凭据/配置变化即重建闭包）
    let fp: Vec<(String, String)> = by_kind
        .iter()
        .map(|(k, ch)| (k.clone(), format!("{}:{}", ch.id, super::fingerprint(ch))))
        .collect();
    let gateway = core.orchestrator().tools.clone();
    {
        let last = ondemand_fp().lock();
        // apply_config/编成切换会重建 ToolGateway；通道没变也必须向新网关重挂。
        if last.as_ref().is_some_and(|(registered, previous)| {
            registered.ptr_eq(&Arc::downgrade(&gateway)) && previous == &fp
        }) {
            return; // 无变化
        }
    }
    // 先全部注销（通道全停时清面；平台消失时回收该平台工具）
    for name in [
        "video__summary",
        "video__subtitle",
        "video__danmaku",
        "video__comments",
        "video__uploader",
        "video__related",
        "video__search",
        "video__shots",
        "video__act",
    ] {
        gateway.unregister_dynamic_tool(name);
    }
    for ch in by_kind.values() {
        for (mut spec, exec) in build_injections(core, ch) {
            // 点播纪律段（spec.md「点播评论真实发送边界」）：随工具面注册注入，
            // 与通道启停同生命周期——评论必须经 video__act 真实发送或如实报告失败，
            // 禁止把评论文本写进最终回复冒充已发布；dry-run 结果转述时必须标注未发送。
            spec.description = format!(
                "{desc}\n\n[使用纪律] 评论必须经 video__act 提交才算完成评论任务；\
                 video__* 工具结果里的拟发文案若标注 [dry-run] 即未实际发送，转述时必须\
                 告知用户「未实际发送」；本工具组不可用时如实报告无法评论，不得以文本交差。",
                desc = spec.description
            );
            gateway.register_dynamic_tool(spec, exec);
        }
    }
    *ondemand_fp().lock() = Some((Arc::downgrade(&gateway), fp));
}

// ---------------------------------------------------------------- 执行面注册

/// 构建观看会话的完整注入集（观看工具组 + act 互动工具）。
/// 每工具绑定通道快照（kind/凭据），执行闭包路由到 dispatch_tool。
/// 观看轮次经 inject_session_tools 注入专属会话；全局点播经 register_dynamic_tool
/// 注册全体可见——执行面同一闭包，会话内注入优先于全局注册。
pub(crate) fn build_injections(
    core: &Arc<Core>,
    ch: &Channel,
) -> Vec<(CustomTool, DynamicToolExec)> {
    build_injections_for(core, ch, WatchTrigger::OnDemand)
}

pub(crate) fn build_watch_injections(
    core: &Arc<Core>,
    ch: &Channel,
) -> Vec<(CustomTool, DynamicToolExec)> {
    build_injections_for(core, ch, WatchTrigger::Automatic)
}

fn build_injections_for(
    core: &Arc<Core>,
    ch: &Channel,
    trigger: WatchTrigger,
) -> Vec<(CustomTool, DynamicToolExec)> {
    let mut out: Vec<(CustomTool, DynamicToolExec)> = watch_tool_specs(&ch.kind)
        .into_iter()
        .chain(std::iter::once(act_tool_spec()))
        .map(|s| {
            let exec = build_exec(core.clone(), ch.clone(), s.name.clone(), trigger);
            (s, exec)
        })
        .collect();
    out.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    out
}

fn build_exec(
    core: Arc<Core>,
    ch: Channel,
    name: String,
    trigger: WatchTrigger,
) -> DynamicToolExec {
    // 执行闭包：路由到具体工具实现；错误统一为结构化文本（工具结果 ok=false）
    dynamic_exec_images(move |_agent, session, args| {
        let core = core.clone();
        let ch = ch.clone();
        let name = name.clone();
        let session = session.to_string();
        async move {
            with_live_guard(&core, &ch, trigger, async {
                let st = session_state(&session);
                dispatch_tool(&core, &name, &ch, &session, &st, trigger, args).await
            })
            .await
        }
    })
}

/// 工具分发（单点：新增工具在此路由）
/// 分发返回：(文本, 图像路径列表)。图像非空时经 DynamicToolOutput.images 视觉回灌。
async fn dispatch_tool(
    core: &Arc<Core>,
    name: &str,
    ch: &Channel,
    session: &str,
    st: &Arc<Mutex<WatchSession>>,
    trigger: WatchTrigger,
    args: Value,
) -> anyhow::Result<(String, Vec<String>)> {
    let ch = live_channel(core, ch, trigger)?;
    let ch = &ch;
    // videoId 归一收口：所有吃 videoId 的工具统一过 resolve（裸号/长链/短链 → 归一 id）
    let vid =
        |k: &str| -> Option<String> { args.get(k).and_then(|v| v.as_str()).map(str::to_string) };
    match name {
        "video__summary" => {
            let id = resolve_video_id(st, &vid("videoId").unwrap_or_default()).await;
            tool_summary(ch, id).await.map(|s| (s, vec![]))
        }
        "video__subtitle" => {
            let id = resolve_video_id(st, &vid("videoId").unwrap_or_default()).await;
            tool_subtitle(
                ch,
                st,
                id,
                args.get("page").and_then(|p| p.as_u64()).unwrap_or(1),
            )
            .await
            .map(|s| (s, vec![]))
        }
        "video__danmaku" => {
            if ch.kind != "bilibili" {
                return Ok((
                    "该平台无弹幕数据（YouTube 不提供弹幕；请用评论/字幕了解观众反应）".into(),
                    vec![],
                ));
            }
            let id = resolve_video_id(st, &vid("videoId").unwrap_or_default()).await;
            tool_danmaku(
                ch,
                id,
                args.get("segment")
                    .and_then(|s| s.as_u64())
                    .map(|s| s as i64),
            )
            .await
            .map(|s| (s, vec![]))
        }
        "video__comments" => {
            let sort = args
                .get("sort")
                .and_then(|s| s.as_str())
                .unwrap_or("hot")
                .to_string();
            let id = resolve_video_id(st, &vid("videoId").unwrap_or_default()).await;
            tool_comments(
                ch,
                st,
                id,
                args.get("page").and_then(|p| p.as_u64()).unwrap_or(1),
                &sort,
            )
            .await
            .map(|s| (s, vec![]))
        }
        "video__uploader" => tool_uploader(ch, vid("uploaderId").unwrap_or_default())
            .await
            .map(|s| (s, vec![])),
        "video__related" => {
            if ch.kind != "bilibili" {
                return Ok((
                    "该平台工具暂不提供（YouTube 相关推荐请用 video__search）".into(),
                    vec![],
                ));
            }
            let id = resolve_video_id(st, &vid("videoId").unwrap_or_default()).await;
            tool_related(ch, id).await.map(|s| (s, vec![]))
        }
        "video__search" => tool_search(ch, vid("query").unwrap_or_default())
            .await
            .map(|s| (s, vec![])),
        "video__shots" => {
            let id = resolve_video_id(st, &vid("videoId").unwrap_or_default()).await;
            let start = args.get("startSec").and_then(|v| v.as_u64());
            let count = args
                .get("count")
                .and_then(|v| v.as_u64())
                .unwrap_or(4)
                .clamp(1, 8);
            tool_shots(core, ch, &id, start, count as usize).await
        }
        "video__act" => {
            let id = resolve_video_id(st, &vid("videoId").unwrap_or_default()).await;
            let mut a = args.clone();
            if let Some(obj) = a.as_object_mut() {
                obj.insert("videoId".into(), serde_json::Value::String(id));
            }
            tool_act(core, ch, st, session, trigger, a)
                .await
                .map(|s| (s, vec![]))
        }
        other => anyhow::bail!("未知视频工具：{other}"),
    }
}

// ---------------------------------------------------------------- 工具实现

fn placeholder(v: &str) -> String {
    if v.trim().is_empty() {
        "（未知）".into()
    } else {
        v.to_string()
    }
}

/// video__summary：双平台元信息
async fn tool_summary(ch: &Channel, video_id: String) -> anyhow::Result<String> {
    if video_id.trim().is_empty() {
        anyhow::bail!("缺少 videoId");
    }
    match ch.kind.as_str() {
        "bilibili" => {
            let body = bili_get(
                ch,
                &format!("https://api.bilibili.com/x/web-interface/view?bvid={video_id}"),
            )
            .await?;
            api_ok(&body)?;
            let d = body.get("data").cloned().unwrap_or(Value::Null);
            let dur = d.get("duration").and_then(as_u64).unwrap_or(0);
            // 外部内容隔离包装（inbound-sanitization）：简介是典型外部注入载体
            let desc = exm_core::tools::wrap_external_for(
                "视频简介",
                &format!("bilibili {video_id}"),
                d.get("desc")
                    .and_then(|v| v.as_str())
                    .unwrap_or("（无简介）"),
                4000,
            );
            let out = format!(
                "标题：{}\nUP主：{}\n时长：{}分{}秒\n播放：{}\n弹幕：{}\n评论：{}\n点赞：{}\n分区：{}\n发布：{}\n\n{desc}",
                placeholder(d.get("title").and_then(|v| v.as_str()).unwrap_or("")),
                placeholder(d.pointer("/owner/name").and_then(|v| v.as_str()).unwrap_or("")),
                dur / 60, dur % 60,
                d.pointer("/stat/view").and_then(as_u64).unwrap_or(0),
                d.pointer("/stat/danmaku").and_then(as_u64).unwrap_or(0),
                d.pointer("/stat/reply").and_then(as_u64).unwrap_or(0),
                d.pointer("/stat/like").and_then(as_u64).unwrap_or(0),
                placeholder(d.pointer("/tid_name").unwrap_or(&Value::Null).as_str().unwrap_or("")),
                placeholder(d.get("pubdate").and_then(|p| p.as_i64()).map(|t| chrono::DateTime::from_timestamp(t, 0).map(|dt| dt.format("%Y-%m-%d").to_string()).unwrap_or_default()).as_deref().unwrap_or("")),
            );
            Ok(out)
        }
        "youtube" => {
            let key = yt_key(ch)?;
            let body = yt_get(
                "videos",
                &[
                    ("part", "snippet,statistics,contentDetails".into()),
                    ("id", video_id.clone()),
                    ("key", key),
                ],
            )
            .await?;
            let item = body.pointer("/items/0").cloned().unwrap_or(Value::Null);
            if item.is_null() {
                anyhow::bail!("YouTube 未找到视频：{video_id}");
            }
            // 外部内容隔离包装（inbound-sanitization）：简介是典型外部注入载体
            let desc = exm_core::tools::wrap_external_for(
                "视频简介",
                &format!("youtube {video_id}"),
                item.pointer("/snippet/description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("（无简介）"),
                4000,
            );
            let out = format!(
                "标题：{}\n频道：{}\n时长：{}\n播放：{}\n评论：{}\n点赞：{}\n发布：{}\n\n{desc}",
                placeholder(
                    item.pointer("/snippet/title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                ),
                placeholder(
                    item.pointer("/snippet/channelTitle")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                ),
                placeholder(
                    item.pointer("/contentDetails/duration")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                ),
                item.pointer("/statistics/viewCount")
                    .and_then(as_u64)
                    .unwrap_or(0),
                item.pointer("/statistics/commentCount")
                    .and_then(as_u64)
                    .unwrap_or(0),
                item.pointer("/statistics/likeCount")
                    .and_then(as_u64)
                    .unwrap_or(0),
                placeholder(
                    item.pointer("/snippet/publishedAt")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                ),
            );
            Ok(out)
        }
        other => anyhow::bail!("不支持的平台：{other}"),
    }
}

const SUBTITLE_PAGE_LINES: usize = 120;

struct BiliSubtitleSnapshot {
    channel_id: String,
    video_id: String,
    cid: u64,
    duration: u64,
    track_id: String,
    language: String,
    lines: Vec<Value>,
}

impl BiliSubtitleSnapshot {
    fn page(&self, page: u64) -> anyhow::Result<String> {
        Ok(format!(
            "字幕来源：bilibili {}；cid={}；字幕轨={}（{}）；视频时长={}。本会话分页使用同一份已校验字幕快照。\n{}",
            self.video_id, self.cid, self.track_id, self.language, fmt_mmss(self.duration),
            format_subtitle_pages(&self.lines, usize::try_from(page)?)?,
        ))
    }
}

fn matches_bvid(actual: Option<&str>, expected: &str) -> bool {
    let Some(actual) = actual else { return false };
    match (
        actual
            .strip_prefix("BV")
            .or_else(|| actual.strip_prefix("bv")),
        expected
            .strip_prefix("BV")
            .or_else(|| expected.strip_prefix("bv")),
    ) {
        (Some(a), Some(b)) => a == b,
        _ => actual == expected,
    }
}

fn bili_video_identity(view: &Value, video_id: &str) -> anyhow::Result<(u64, u64)> {
    api_ok(view)?;
    anyhow::ensure!(
        matches_bvid(view.pointer("/data/bvid").and_then(Value::as_str), video_id),
        "来源视频不匹配（请求 {video_id}），已拒绝将响应作为观看依据"
    );
    let cid = view
        .pointer("/data/pages/0/cid")
        .and_then(as_u64)
        .or_else(|| view.pointer("/data/cid").and_then(as_u64))
        .filter(|cid| *cid > 0)
        .ok_or_else(|| anyhow::anyhow!("无法取得视频 cid"))?;
    let duration = view
        .pointer("/data/pages/0/duration")
        .and_then(as_u64)
        .or_else(|| view.pointer("/data/duration").and_then(as_u64))
        .filter(|duration| *duration > 0)
        .ok_or_else(|| anyhow::anyhow!("无法取得视频时长，不能校验观看材料来源"))?;
    Ok((cid, duration))
}

fn subtitle_track<'a>(
    player: &'a Value,
    video_id: &str,
    cid: u64,
) -> anyhow::Result<Option<&'a Value>> {
    // 风控/鉴权错误不能被吞成「无字幕」，否则模型会把取材失败当成正常无轨。
    api_ok(player)?;
    anyhow::ensure!(
        matches_bvid(
            player.pointer("/data/bvid").and_then(Value::as_str),
            video_id
        ) && player.pointer("/data/cid").and_then(as_u64) == Some(cid),
        "字幕播放器响应的视频/cid 不匹配（请求 {video_id}，cid={cid}），已拒绝作为观看依据"
    );
    let subs = player
        .pointer("/data/subtitle/subtitles")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("平台响应缺少字幕轨列表，无法确认是否有字幕"))?;
    Ok(subs
        .iter()
        .find(|s| {
            s.get("lan")
                .and_then(Value::as_str)
                .is_some_and(|lan| lan.contains("zh"))
        })
        .or_else(|| subs.first()))
}

fn validate_subtitle_lines(lines: &[Value], duration: u64) -> anyhow::Result<()> {
    anyhow::ensure!(
        lines.iter().any(|line| line
            .get("content")
            .and_then(Value::as_str)
            .is_some_and(|content| !content.trim().is_empty())),
        "字幕正文没有可用文本，不能作为观看依据"
    );
    let mut previous = 0.0;
    for (i, line) in lines.iter().enumerate() {
        let from = line.get("from").and_then(Value::as_f64);
        let to = line.get("to").and_then(Value::as_f64);
        let valid = from.zip(to).is_some_and(|(from, to)| {
            from.is_finite() && to.is_finite() && from >= previous && to >= from
                // 字幕尾句可能略超视频取整时长；容忍 2 秒，而非接受另一视频的长轨道。
                && to <= duration as f64 + 2.0
        });
        anyhow::ensure!(valid,
            "字幕时间范围与视频时长不匹配或顺序异常（第 {} 行，视频 {} 秒），疑似字幕来源错误；已拒绝作为观看依据，请改用 video__shots 核实画面",
            i + 1, duration
        );
        previous = from.unwrap_or_default();
    }
    Ok(())
}

#[cfg(test)]
mod manual_watch_tests {
    use super::*;

    const VIDEO: &str = "BV1qDHF69EiZ";
    const CID: u64 = 43_085_202_957;

    fn channel() -> Channel {
        serde_json::from_value(json!({
            "id": "manual-bili", "type": "bilibili", "enabled": true,
            "createdAt": "", "config": { "watch": "false", "comment": "false", "reply": "false" }
        }))
        .unwrap()
    }

    fn core() -> Arc<Core> {
        let dir =
            std::env::temp_dir().join(format!("exm-manual-video-{}", exm_core::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        Arc::new(Core::with_config(cfg).unwrap())
    }

    fn snapshot(video: &str, track: &str, line_count: usize) -> BiliSubtitleSnapshot {
        BiliSubtitleSnapshot {
            channel_id: "manual-bili".into(),
            video_id: video.into(),
            cid: CID,
            duration: 335,
            track_id: track.into(),
            language: "zh-CN".into(),
            lines: (0..line_count)
                .map(|i| {
                    json!({
                        "from": i as f64 / 2.0, "to": (i + 1) as f64 / 2.0,
                        "content": format!("{track} 第{i}句"),
                    })
                })
                .collect(),
        }
    }

    #[test]
    fn manual_b23_url_协议头不重复且裸短链补齐() {
        for input in ["b23.tv/abc", "http://b23.tv/abc", "https://b23.tv/abc"] {
            assert_eq!(b23_url(input), "https://b23.tv/abc");
        }
    }

    #[test]
    fn manual_subtitle_校验视频与cid并保留平台错误() {
        let view = json!({"code": 0, "data": {
            "bvid": VIDEO, "cid": CID, "duration": 335,
            "pages": [{"cid": CID, "duration": 335}]
        }});
        assert_eq!(bili_video_identity(&view, VIDEO).unwrap(), (CID, 335));
        assert_eq!(
            bili_video_identity(&view, "bv1qDHF69EiZ").unwrap(),
            (CID, 335)
        );
        assert!(
            bili_video_identity(&view, "BV1qdHF69EiZ").is_err(),
            "BV 号主体大小写不能混同"
        );
        assert!(bili_video_identity(&view, "BV1xx411c7mD").is_err());
        let player = json!({"code": 0, "data": {
            "bvid": VIDEO, "cid": CID,
            "subtitle": {"subtitles": [
                {"id_str": "en", "lan": "en"}, {"id_str": "zh", "lan": "zh-CN"}
            ]}
        }});
        assert_eq!(
            subtitle_track(&player, VIDEO, CID).unwrap().unwrap()["id_str"],
            "zh"
        );
        assert!(subtitle_track(&player, VIDEO, CID + 1).is_err());
        assert!(subtitle_track(&player, "BV1xx411c7mD", CID).is_err());
        let error = subtitle_track(&json!({"code": -352, "message": "风控"}), VIDEO, CID)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("code=-352"),
            "平台失败不能伪报无字幕：{error}"
        );
        let no_tracks = json!({"code": 0, "data": {
            "bvid": VIDEO, "cid": CID, "subtitle": {"subtitles": []}
        }});
        assert!(subtitle_track(&no_tracks, VIDEO, CID).unwrap().is_none());
    }

    #[test]
    fn manual_subtitle_拒绝超过335秒的视频串轨材料() {
        let valid = vec![
            json!({"from": 0.0, "to": 2.0, "content": "开场"}),
            json!({"from": 333.0, "to": 335.8, "content": "结束"}),
        ];
        assert!(validate_subtitle_lines(&valid, 335).is_ok());
        let mismatch = vec![json!({"from": 393.0, "to": 438.0, "content": "其他视频的解说"})];
        let error = validate_subtitle_lines(&mismatch, 335)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("视频 335 秒") && error.contains("已拒绝作为观看依据"),
            "{error}"
        );
        for line in [
            json!({"from": -1.0, "to": 2.0, "content": "异常"}),
            json!({"from": 2.0, "to": 1.0, "content": "异常"}),
            json!({"from": 0.0, "content": "缺结束时间"}),
            json!({"from": 0.0, "to": 2.0}),
        ] {
            assert!(validate_subtitle_lines(&[line], 335).is_err());
        }
    }

    #[test]
    fn manual_subtitle_分页固定快照且视频通道隔离() {
        let mut st = WatchSession::default();
        let first = st.pin_subtitle(snapshot(VIDEO, "original", 400));
        assert!(first.page(1).unwrap().contains("第 1/4 页"));
        let changed = st.pin_subtitle(snapshot(VIDEO, "changed", 300));
        assert!(Arc::ptr_eq(&first, &changed), "不能随翻页换轨道和总页数");
        let page = changed.page(2).unwrap();
        assert!(
            page.contains("第 2/4 页") && page.contains("original 第120句"),
            "{page}"
        );
        assert!(!page.contains("changed"));
        assert!(
            page.contains(VIDEO) && page.contains(&format!("cid={CID}")),
            "应带来源标识"
        );
        assert!(st.cached_subtitle("manual-bili", "bv1qDHF69EiZ").is_some());
        assert!(st.cached_subtitle("manual-bili", "BV1qdHF69EiZ").is_none());
        assert!(st.cached_subtitle("manual-bili", "BV1xx411c7mD").is_none());
        assert!(st.cached_subtitle("other-channel", VIDEO).is_none());
        assert!(first.page(0).is_err());
        assert!(first.page(u64::MAX).unwrap().contains("没有更多字幕"));
        for i in 0..10 {
            st.pin_subtitle(snapshot(&format!("other-{i}"), "other", 1));
        }
        assert_eq!(st.subtitle_snapshots.len(), 8, "长会话全文缓存须有上限");
    }

    #[test]
    fn manual_tools_配置重建恢复注册且停用回收shots() {
        let core = core();
        let ch = channel();
        platform::save_channels(&core, &[ch.clone()]).unwrap();
        reconcile_ondemand(&core);
        let old = core.orchestrator().tools.clone();
        assert!(old.dynamic_tool("video__shots").is_some());
        core.apply_config((*core.config()).clone()).unwrap();
        let new = core.orchestrator().tools.clone();
        assert!(!Arc::ptr_eq(&old, &new));
        assert!(new.dynamic_tool("video__summary").is_none());
        reconcile_ondemand(&core);
        for name in ["video__summary", "video__subtitle", "video__shots"] {
            assert!(new.dynamic_tool(name).is_some(), "新网关需要恢复 {name}");
        }
        let mut disabled = ch;
        disabled.enabled = false;
        platform::save_channels(&core, &[disabled]).unwrap();
        reconcile_ondemand(&core);
        assert!(new
            .dynamic_tool_names()
            .iter()
            .all(|name| !name.starts_with("video__")));
    }

    #[tokio::test]
    async fn manual_tools_watch关闭仍可离线点播字幕分页() {
        let core = core();
        let ch = channel();
        platform::save_channels(&core, &[ch]).unwrap();
        reconcile_ondemand(&core);
        let sid = format!("manual-subtitle-{}", exm_core::types::new_id());
        session_state(&sid)
            .lock()
            .pin_subtitle(snapshot(VIDEO, "manual", 400));
        let (_, exec) = core
            .orchestrator()
            .tools
            .dynamic_tool("video__subtitle")
            .unwrap();
        for page in [1, 2] {
            let output = exec("machina", &sid, json!({"videoId": VIDEO, "page": page})).await;
            assert!(output.ok, "watch=false 不能禁用手动观看：{}", output.text);
            assert!(
                output.text.contains(&format!("第 {page}/4 页")),
                "{}",
                output.text
            );
            assert!(output.images.is_empty());
        }
        let different = exec(
            "machina",
            &sid,
            json!({"videoId": "BV1xx411c7mD", "page": 2}),
        )
        .await;
        assert!(
            !different.ok && different.text.contains("同一个 videoId"),
            "不能把别的视频当成目标视频的第 2 页：{}",
            different.text
        );
        let mut replacement = channel();
        replacement.id = "manual-bili-replacement".into();
        let mut replaced_snapshot = snapshot(VIDEO, "replacement", 400);
        replaced_snapshot.channel_id = replacement.id.clone();
        session_state(&sid).lock().pin_subtitle(replaced_snapshot);
        platform::save_channels(&core, &[replacement]).unwrap();
        reconcile_ondemand(&core);
        let (_, new_exec) = core
            .orchestrator()
            .tools
            .dynamic_tool("video__subtitle")
            .unwrap();
        let output = new_exec("machina", &sid, json!({"videoId": VIDEO})).await;
        assert!(
            output.ok && output.text.contains("replacement 第0句"),
            "配置相同但通道 id 更换也必须重绑：{}",
            output.text
        );
        drop_session(&sid);
    }

    #[tokio::test]
    async fn manual_shots_项目目录保存的图像可被运行时读取() {
        let core = core();
        let project = core.config().workspace_root.join("manual-project");
        std::fs::create_dir_all(&project).unwrap();
        exm_core::workspace::scope(Some(project.clone()), async {
            let dir = shots_directory(&core);
            assert_eq!(dir, project.join(".exmachina/video_shots"));
            std::fs::create_dir_all(&dir).unwrap();
            image::RgbImage::from_pixel(1, 1, image::Rgb([1, 2, 3]))
                .save(dir.join("manual-test.png"))
                .unwrap();
            let data = core
                .orchestrator()
                .tools
                .read_image_data_url(".exmachina/video_shots/manual-test.png")
                .unwrap();
            assert!(data.starts_with("data:image/png;base64,"));
        })
        .await;
    }
}

/// video__subtitle：B站 CC/AI 字幕（分页）；YouTube captions 轨
async fn tool_subtitle(
    ch: &Channel,
    st: &Arc<Mutex<WatchSession>>,
    video_id: String,
    page: u64,
) -> anyhow::Result<String> {
    if video_id.trim().is_empty() {
        anyhow::bail!("缺少 videoId");
    }
    if page == 0 {
        anyhow::bail!("page 从 1 起");
    }
    match ch.kind.as_str() {
        "bilibili" => {
            if let Some(snapshot) = st.lock().cached_subtitle(&ch.id, &video_id) {
                return snapshot.page(page);
            }
            anyhow::ensure!(page == 1,
                "本会话没有 {video_id} 的固定字幕快照（尚未获取或已过期）；请先对同一个 videoId 调用 page=1，不能跨视频续页"
            );
            let view = bili_get(
                ch,
                &format!("https://api.bilibili.com/x/web-interface/view?bvid={video_id}"),
            )
            .await?;
            let (cid, duration) = bili_video_identity(&view, &video_id)?;
            let player = bili_get(
                ch,
                &format!("https://api.bilibili.com/x/player/v2?bvid={video_id}&cid={cid}"),
            )
            .await?;
            let Some(track) = subtitle_track(&player, &video_id, cid)? else {
                return Ok(format!("未获取字幕正文（{video_id}，cid={cid}；当前登录态未返回可用字幕轨）。可用 video__shots 查看抽样画面；简介/弹幕/评论只是辅助资料，不能据此声称已听取对白或完整观看视频。"));
            };
            let mut url = track
                .get("subtitle_url")
                .and_then(Value::as_str)
                .filter(|url| !url.is_empty())
                .ok_or_else(|| anyhow::anyhow!("字幕轨缺少正文地址，未获取可核验字幕"))?
                .to_string();
            if url.starts_with("//") {
                url = format!("https:{url}");
            }
            let doc = http_client()
                .get(&url)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("字幕下载失败：{}", e.without_url()))?
                .error_for_status()
                .map_err(|e| anyhow::anyhow!("字幕下载失败：{}", e.without_url()))?
                .json::<Value>()
                .await
                .map_err(|e| anyhow::anyhow!("字幕解析失败：{}", e.without_url()))?;
            let lines = doc
                .get("body")
                .and_then(|b| b.as_array())
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("字幕正文缺少 body 数组，未获取可核验字幕"))?;
            if lines.is_empty() {
                return Ok(format!(
                    "未获取字幕正文（{video_id}，cid={cid}；字幕轨为空），不能作为已观看依据"
                ));
            }
            validate_subtitle_lines(&lines, duration)?;
            let snapshot = st.lock().pin_subtitle(BiliSubtitleSnapshot {
                channel_id: ch.id.clone(),
                video_id,
                cid,
                duration,
                track_id: track
                    .get("id_str")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| track.get("id").and_then(as_u64).map(|id| id.to_string()))
                    .unwrap_or_else(|| "未知".into()),
                language: track
                    .get("lan")
                    .and_then(Value::as_str)
                    .unwrap_or("未知")
                    .to_string(),
                lines,
            });
            snapshot.page(page)
        }
        "youtube" => {
            // captions.list 需 OAuth；用 API Key 能列轨，正文多数需权限——失败时明确降级说明
            let key = yt_key(ch)?;
            let body = yt_get(
                "captions",
                &[
                    ("part", "snippet".into()),
                    ("videoId", video_id.clone()),
                    ("key", key),
                ],
            )
            .await?;
            let items = body
                .get("items")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if items.is_empty() {
                return Ok("未获取字幕正文（YouTube 未返回 caption 轨）；简介与评论只是辅助资料，不能据此声称已听取对白或完整观看视频".into());
            }
            let track = items
                .iter()
                .find(|t| {
                    t.pointer("/snippet/language")
                        .and_then(|l| l.as_str())
                        .map(|l| l.starts_with("zh"))
                        .unwrap_or(false)
                })
                .or_else(|| items.first());
            let Some(track) = track else {
                return Ok("该视频无字幕".into());
            };
            let lang = track
                .pointer("/snippet/language")
                .and_then(|l| l.as_str())
                .unwrap_or("?");
            let name = track
                .pointer("/snippet/name")
                .and_then(|l| l.as_str())
                .unwrap_or("");
            Ok(format!(
                "该视频有字幕轨（{lang} {name}），但正文下载需视频所有者授权（OAuth caption download）。\n尚未获取字幕正文；简介与评论只是辅助资料，不能据此声称已听取对白或完整观看视频。当前工具没有视频下载/语音转写回退。"
            ))
        }
        other => anyhow::bail!("不支持的平台：{other}"),
    }
}

fn format_subtitle_pages(lines: &[Value], page: usize) -> anyhow::Result<String> {
    anyhow::ensure!(page > 0, "page 从 1 起");
    let total_pages = lines.len().div_ceil(SUBTITLE_PAGE_LINES);
    if page > total_pages {
        return Ok(format!(
            "没有更多字幕（共 {total_pages} 页，当前请求第 {page} 页）"
        ));
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
        format!(
            "\n（第 {page}/{total_pages} 页；下一页传 page={}）",
            page + 1
        )
    } else {
        format!("\n（第 {page}/{total_pages} 页；已到末页）")
    };
    // 外部内容隔离包装（inbound-sanitization）：字幕是外部数据
    Ok(exm_core::tools::wrap_external_for(
        "字幕",
        &format!("第 {page}/{total_pages} 页"),
        &format!("{out}{nav}"),
        12000,
    ))
}

/// video__danmaku：B站弹幕 protobuf 接口（分段抽样）
async fn tool_danmaku(
    ch: &Channel,
    video_id: String,
    segment: Option<i64>,
) -> anyhow::Result<String> {
    let view = bili_get(
        ch,
        &format!("https://api.bilibili.com/x/web-interface/view?bvid={video_id}"),
    )
    .await?;
    api_ok(&view)?;
    let d = view.get("data").cloned().unwrap_or(Value::Null);
    let aid = d.get("aid").and_then(as_u64).unwrap_or(0);
    let cid = d
        .pointer("/pages/0/cid")
        .and_then(as_u64)
        .or_else(|| d.get("cid").and_then(as_u64))
        .unwrap_or(0);
    if aid == 0 || cid == 0 {
        anyhow::bail!("无法定位弹幕轨（aid/cid 缺失）");
    }
    let duration = d.get("duration").and_then(as_u64).unwrap_or(0);
    let total_segs = std::cmp::max(1, duration.div_ceil(360) as i64); // 每段 6 分钟
                                                                      // 请求段：指定段或全部前 6 段（全片概览）
    let segs: Vec<i64> = match segment {
        Some(s) if (0..total_segs).contains(&s) => vec![s],
        Some(s) => {
            return Ok(format!(
                "分段序号越界（请求段 {s}；该视频共 {total_segs} 段，有效 0..{}）",
                total_segs - 1
            ))
        }
        None => (0..std::cmp::min(6, total_segs)).collect(),
    };
    let mut out = String::new();
    for seg in segs {
        // protobuf 弹幕：SegSoC 接口（非官方稳定 API；失败降级为结构化错误）
        let url = format!(
            "https://api.bilibili.com/x/v2/dm/web/seg.so?type=1&oid={aid}&segment_index={}",
            seg + 1
        );
        let resp = bili_request(ch, http_client().get(&url))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("弹幕请求失败（段 {seg}）：{e}"))?;
        if !resp.status().is_success() {
            anyhow::bail!(
                "弹幕接口 HTTP {}（段 {seg}）；可改用评论/字幕继续",
                resp.status()
            );
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| anyhow::anyhow!("弹幕读取失败：{e}"))?;
        let dans = parse_danmaku(&bytes);
        if dans.is_empty() {
            if segment.is_some() {
                out.push_str(&format!("（段 {seg}：无弹幕）\n"));
            }
            continue;
        }
        let t0 = seg * 360;
        out.push_str(&format!(
            "—— 时间段 {}:{} 至 {}:{} ——\n",
            t0 / 60,
            t0 % 60,
            (t0 + 360) / 60,
            (t0 + 360) % 60
        ));
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
        out.push_str(&format!(
            "\n（全片概览：前 {} 段抽样；指定段传 segment 0..{} 深入）",
            std::cmp::min(6, total_segs),
            total_segs - 1
        ));
    }
    // 外部内容隔离包装（inbound-sanitization）：弹幕是外部数据
    Ok(exm_core::tools::wrap_external_for(
        "弹幕",
        &format!("aid={aid}"),
        &out,
        8000,
    ))
}

/// protobuf 弹幕粗解析（字段 6 = 内容文本；字段 2 附近为时间偏移 ms）。
/// 不引 prost 全量结构：只按 varint tag 流抽字段（够抽样展示，非完整还原）。
fn parse_danmaku(bytes: &[u8]) -> Vec<(i64, String)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        // 每条弹幕 = 长度前缀（varint）+ DanmakuElem 消息体
        let (len, adv) = match read_varint(&bytes[i..]) {
            Some(v) => v,
            None => break,
        };
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
            0 => {
                let (v, a) = read_varint(&msg[i..])?;
                i += a;
                if field == 2 {
                    ts = v as i64;
                }
            }
            2 => {
                let (l, a) = read_varint(&msg[i..])?;
                i += a;
                let end = std::cmp::min(i + l as usize, msg.len());
                if field == 6 || field == 7 || field == 9 {
                    if let Ok(s) = std::str::from_utf8(&msg[i..end]) {
                        if field == 6 && text.is_empty() {
                            text = s.to_string();
                        }
                    }
                }
                i = end;
            }
            5 => i += 4,
            1 => i += 8,
            _ => return None,
        }
    }
    if text.is_empty() {
        None
    } else {
        Some((ts, text))
    }
}

fn read_varint(b: &[u8]) -> Option<(u64, usize)> {
    let mut v = 0u64;
    let mut shift = 0u32;
    for (idx, byte) in b.iter().enumerate() {
        v |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some((v, idx + 1));
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
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
    if page == 0 {
        anyhow::bail!("page 从 1 起");
    }
    match ch.kind.as_str() {
        "bilibili" => {
            let view = bili_get(
                ch,
                &format!("https://api.bilibili.com/x/web-interface/view?bvid={video_id}"),
            )
            .await?;
            api_ok(&view)?;
            let aid = view.pointer("/data/aid").and_then(as_u64).unwrap_or(0);
            if aid == 0 {
                anyhow::bail!("无法定位评论区（aid 缺失）");
            }
            let sort_code = if sort == "time" { 0 } else { 2 };
            let body = bili_get(ch, &format!(
                "https://api.bilibili.com/x/v2/reply?type=1&oid={aid}&pn={page}&ps={COMMENTS_PAGE}&sort={sort_code}"
            )).await?;
            api_ok(&body)?;
            let page_info = body.pointer("/data/page").cloned().unwrap_or(Value::Null);
            let total = page_info.get("count").and_then(as_u64).unwrap_or(0);
            let replies = body
                .pointer("/data/replies")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if replies.is_empty() {
                return Ok(format!("没有更多评论（第 {page} 页为空；共 {total} 条）"));
            }
            let mut st_lock = st.lock();
            let mut body = format!("评论区（第 {page} 页，热度排序）共 {total} 条：\n");
            for r in &replies {
                let id = r
                    .get("rpid")
                    .and_then(as_u64)
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                let author = r
                    .pointer("/member/uname")
                    .and_then(|v| v.as_str())
                    .unwrap_or("匿名");
                let text = r
                    .pointer("/content/message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let likes = r.get("like").and_then(as_u64).unwrap_or(0);
                let rcount = r.get("rcount").and_then(as_u64).unwrap_or(0);
                st_lock.seen_comments.insert(id.clone());
                body.push_str(&format!(
                    "- [{likes}赞 {rcount}回复] {author}（id={id}）：{}\n",
                    text.replace('\n', " ")
                ));
            }
            body.push_str(&format!(
                "\n（翻下一页传 page={}；回复某条用 video__act 的 reply.commentId）",
                page + 1
            ));
            // 外部内容隔离包装（inbound-sanitization）：评论文本是典型外部注入载体
            Ok(exm_core::tools::wrap_external_for(
                "评论",
                &format!("bilibili {video_id}"),
                &body,
                8000,
            ))
        }
        "youtube" => {
            let key = yt_key(ch)?;
            let order = if sort == "time" {
                "time".to_string()
            } else {
                "relevance".to_string()
            };
            // 翻页经 pageToken 链：为免维护 token 链复杂度，用 maxResults 拉大窗口 + 内存分页
            let want = (page * COMMENTS_PAGE as u64).to_string();
            let body = yt_get(
                "commentThreads",
                &[
                    ("part", "snippet".into()),
                    ("videoId", video_id.clone()),
                    ("maxResults", want),
                    ("order", order),
                    ("textFormat", "plainText".into()),
                    ("key", key),
                ],
            )
            .await?;
            let items = body
                .get("items")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let start = ((page - 1) * COMMENTS_PAGE as u64) as usize;
            if start >= items.len() {
                return Ok(format!("没有更多评论（第 {page} 页为空）"));
            }
            let window: Vec<Value> = items.into_iter().skip(start).take(COMMENTS_PAGE).collect();
            let mut st_lock = st.lock();
            let mut body = format!("评论区（第 {page} 页）：\n");
            for x in &window {
                let Some(sn) = x.pointer("/snippet/topLevelComment") else {
                    continue;
                };
                let id = sn.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let author = sn
                    .pointer("/snippet/authorDisplayName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("匿名");
                let text = sn
                    .pointer("/snippet/textDisplay")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let likes = sn
                    .pointer("/snippet/likeCount")
                    .and_then(as_u64)
                    .unwrap_or(0);
                let rcount = x
                    .pointer("/snippet/totalReplyCount")
                    .and_then(as_u64)
                    .unwrap_or(0);
                st_lock.seen_comments.insert(id.to_string());
                body.push_str(&format!(
                    "- [{likes}赞 {rcount}回复] {author}（id={id}）：{}\n",
                    text.replace('\n', " ")
                ));
            }
            body.push_str(&format!("\n（翻下一页传 page={}）", page + 1));
            // 外部内容隔离包装（inbound-sanitization）：评论文本是典型外部注入载体
            Ok(exm_core::tools::wrap_external_for(
                "评论",
                &format!("youtube {video_id}"),
                &body,
                8000,
            ))
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
            let card = bili_get(
                ch,
                &format!("https://api.bilibili.com/x/web-interface/card?mid={uploader_id}"),
            )
            .await?;
            api_ok(&card)?;
            let d = card.get("data").cloned().unwrap_or(Value::Null);
            let name = d
                .pointer("/card/name")
                .and_then(|v| v.as_str())
                .unwrap_or("（未知）");
            let sign = d
                .pointer("/card/sign")
                .and_then(|v| v.as_str())
                .unwrap_or("（无签名）");
            let fans = d.pointer("/follower").and_then(as_u64).unwrap_or(0);
            let videos = d
                .pointer("/archive")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let mut out = format!(
                "UP主：{name}\n粉丝：{fans}\n签名：{}\n\n近期投稿（{}）：\n",
                exm_core::tools::wrap_external_for("UP主签名", name, sign, 2000),
                videos.len()
            );
            for v in videos.iter().take(8) {
                let t = v
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("（无题）");
                let play = v.get("play").and_then(as_u64).unwrap_or(0);
                let created = v.get("created").and_then(|c| c.as_i64()).unwrap_or(0);
                let dt = chrono::DateTime::from_timestamp(created, 0)
                    .map(|x| x.format("%Y-%m-%d").to_string())
                    .unwrap_or_default();
                out.push_str(&format!("- {t}（{play}播放 {dt}）\n"));
            }
            Ok(out)
        }
        "youtube" => {
            let key = yt_key(ch)?;
            let ch_body = yt_get(
                "channels",
                &[
                    ("part", "snippet,statistics".into()),
                    ("id", uploader_id.clone()),
                    ("key", key.clone()),
                ],
            )
            .await?;
            let c = ch_body.pointer("/items/0").cloned().unwrap_or(Value::Null);
            if c.is_null() {
                anyhow::bail!("未找到频道：{uploader_id}");
            }
            let name = c
                .pointer("/snippet/title")
                .and_then(|v| v.as_str())
                .unwrap_or("（未知）");
            let desc = c
                .pointer("/snippet/description")
                .and_then(|v| v.as_str())
                .unwrap_or("（无简介）");
            let subs = c
                .pointer("/statistics/subscriberCount")
                .and_then(as_u64)
                .unwrap_or(0);
            // 近期投稿：search by channelId（date 序）
            let uploads = yt_get(
                "search",
                &[
                    ("part", "snippet".into()),
                    ("channelId", uploader_id.clone()),
                    ("order", "date".into()),
                    ("type", "video".into()),
                    ("maxResults", "8".into()),
                    ("key", key),
                ],
            )
            .await?;
            let items = uploads
                .get("items")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let mut out = format!(
                "频道：{name}\n订阅：{subs}\n简介：{}\n\n近期投稿：\n",
                exm_core::tools::wrap_external_for("频道简介", name, desc, 2000)
            );
            for x in items.iter().take(8) {
                let t = x
                    .pointer("/snippet/title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("（无题）");
                let dt = x
                    .pointer("/snippet/publishedAt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let vid = x
                    .pointer("/id/videoId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                out.push_str(&format!("- {t}（{dt} id={vid}）\n"));
            }
            Ok(out)
        }
        other => anyhow::bail!("不支持的平台：{other}"),
    }
}

/// video__related：B站相关视频
async fn tool_related(ch: &Channel, video_id: String) -> anyhow::Result<String> {
    let view = bili_get(
        ch,
        &format!("https://api.bilibili.com/x/web-interface/view?bvid={video_id}"),
    )
    .await?;
    api_ok(&view)?;
    let aid = view.pointer("/data/aid").and_then(as_u64).unwrap_or(0);
    if aid == 0 {
        anyhow::bail!("无法定位相关视频（aid 缺失）");
    }
    let rel = bili_get(
        ch,
        &format!("https://api.bilibili.com/x/web-interface/archive/related?aid={aid}"),
    )
    .await?;
    api_ok(&rel)?;
    let list = rel
        .get("data")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if list.is_empty() {
        return Ok("无相关视频推荐".into());
    }
    let mut out = format!("相关/推荐视频（{}）：\n", list.len().min(10));
    for x in list.iter().take(10) {
        let bv = x.get("bvid").and_then(|v| v.as_str()).unwrap_or("?");
        let t = x
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("（无题）");
        let owner = x
            .pointer("/owner/name")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
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
        return Ok(format!(
            "搜索「{query}」无结果（可能被风控或无匹配；可换词重试）"
        ));
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

fn shots_directory(core: &Core) -> std::path::PathBuf {
    // 与 ToolGateway::read_image_data_url 使用同一生效根，否则项目/组工作区会静默丢图。
    core.registry
        .effective_workspace(&core.config().workspace_root)
        .join(".exmachina")
        .join("video_shots")
}

/// video__shots：B 站官方 videoshot 雪碧图抽帧——视觉模态直看画面。
/// 雪碧图 = 网格 JPG（img_x_len × img_y_len 帧/图，多图各 100 帧按序铺满全片）。
/// 帧时间表在 index 数组（画面变化自适应间隔，非匀速）——按目标时间查表定位帧。
/// 流程：videoshot?index=1 拿索引 → 均匀采样目标时间 → 查表映射帧号 →
/// 下载所在雪碧图 → 裁出该帧 → 存 .exmachina/video_shots/（png）→ 路径随 images 回灌。
async fn tool_shots(
    core: &Arc<Core>,
    ch: &Channel,
    video_id: &str,
    start_sec: Option<u64>,
    count: usize,
) -> anyhow::Result<(String, Vec<String>)> {
    use image::GenericImageView;
    if ch.kind != "bilibili" {
        return Ok((
            "该平台无画面抽帧（YouTube 无雪碧图 API；请用 summary/subtitle/comments 了解内容）"
                .into(),
            vec![],
        ));
    }
    if video_id.trim().is_empty() {
        anyhow::bail!("缺少 videoId");
    }
    // 视频时长（帧采样铺满全片需要）
    let view = bili_get(
        ch,
        &format!("https://api.bilibili.com/x/web-interface/view?bvid={video_id}"),
    )
    .await?;
    let (_, duration) = bili_video_identity(&view, video_id)?;
    // 雪碧图索引（index=1 带帧时间表：index[n] = 第 n 帧截取秒，自适应间隔）
    let vs = bili_get(
        ch,
        &format!("https://api.bilibili.com/x/player/videoshot?bvid={video_id}&index=1"),
    )
    .await?;
    api_ok(&vs)?;
    let data = vs.get("data").cloned().unwrap_or(Value::Null);
    let images: Vec<String> = data
        .get("image")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if images.is_empty() {
        anyhow::bail!("该视频无画面抽帧数据（{video_id}，可能未生成预览）；未获得可观察图像，不能作为已观看依据");
    }
    let x_len = data.get("img_x_len").and_then(as_u64).unwrap_or(10).max(1) as u32; // 每行帧数
    let fw = data.get("img_x_size").and_then(as_u64).unwrap_or(480) as u32; // 单帧宽
    let fh = data.get("img_y_size").and_then(as_u64).unwrap_or(270) as u32; // 单帧高
    let per_sheet =
        (x_len * data.get("img_y_len").and_then(as_u64).unwrap_or(10).max(1) as u32).max(1);
    // 帧时间表（秒，升序；index[0]=0 恒置，有效帧从 1 起）
    let index: Vec<u64> = data
        .get("index")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_u64()).collect())
        .unwrap_or_default();

    // 时间 → 帧号（0-based：max{i : index[i] <= t}，即截取时刻不超过 t 的最后一帧）。
    // index 缺失（极少数视频）回退匀速假设：帧数 = 雪碧图容量 × 总时长估。
    let time_to_shot = |t: u64| -> usize {
        if index.len() >= 2 {
            let mut f = 0usize;
            for (i, &sec) in index.iter().enumerate().skip(1) {
                if sec <= t {
                    f = i;
                } else {
                    break;
                }
            }
            f.min(index.len().saturating_sub(1))
        } else {
            // 匀速回退：帧率按 index 缺省（每 10s 一帧是常见值，容错估计）
            (t / 10) as usize
        }
    };

    // 采样：startSec（缺省 0）到片尾均匀取 count 个时间点
    let start = start_sec.unwrap_or(0).min(duration.saturating_sub(1));
    let span = duration.saturating_sub(start).max(1);
    let step = ((span as f64 / count as f64).ceil() as u64).max(1);
    let times: Vec<u64> = (0..count as u64)
        .map(|i| (start + i * step).min(duration.saturating_sub(1)))
        .collect();

    let dir = shots_directory(core);
    std::fs::create_dir_all(&dir)?;
    // 雪碧图缓存（同图多帧只下载一次）
    let mut sheet_cache: std::collections::HashMap<usize, Option<image::DynamicImage>> =
        std::collections::HashMap::new();
    let mut saved: Vec<(u64, String)> = Vec::new();
    let mut saved_frames: Vec<usize> = Vec::new();
    for &t in &times {
        let shot = time_to_shot(t); // 0-based 全局帧号
                                    // 部分视频（官方 PV 等）预览只覆盖前段：目标时间超出时钳到末帧。
                                    // 钳制会让多个采样点映射同一帧——同帧只存一张，避免重复图回灌。
        let sheet_idx = shot / per_sheet as usize;
        let in_sheet = (shot % per_sheet as usize) as u32;
        let Some(url) = images.get(sheet_idx) else {
            continue;
        };
        let sheet = match sheet_cache.get(&sheet_idx) {
            Some(Some(img)) => img.clone(),
            _ => {
                let u = if url.starts_with("//") {
                    format!("https:{url}")
                } else {
                    url.clone()
                };
                let bytes = http_client()
                    .get(&u)
                    .header(reqwest::header::USER_AGENT, UA)
                    .header(reqwest::header::REFERER, BILI_REFERER)
                    .send()
                    .await
                    .map_err(|e| anyhow::anyhow!("雪碧图下载失败：{e}"))?
                    .bytes()
                    .await
                    .map_err(|e| anyhow::anyhow!("雪碧图读取失败：{e}"))?;
                let img = image::load_from_memory(&bytes).ok();
                sheet_cache.insert(sheet_idx, img.clone());
                match img {
                    Some(i) => i,
                    None => continue,
                }
            }
        };
        // 裁出目标帧（网格内行列：col = in_sheet % x_len，row = in_sheet / x_len）
        let col = in_sheet % x_len;
        let row = in_sheet / x_len;
        let (sw, sh) = sheet.dimensions();
        let x = (col * fw).min(sw.saturating_sub(fw));
        let y = (row * fh).min(sh.saturating_sub(fh));
        let frame = sheet.crop_imm(x, y, fw.min(sw), fh.min(sh));
        // 同帧去重（截断预览的钳制重复）：跳过已保存的帧号
        if saved_frames.contains(&shot) {
            continue;
        }
        let name = format!("shot-{}-{}s.png", video_id, t);
        let path = dir.join(&name);
        if frame.save(&path).is_err() {
            continue;
        }
        saved_frames.push(shot);
        saved.push((t, format!(".exmachina/video_shots/{name}")));
    }
    if saved.is_empty() {
        anyhow::bail!("画面抽帧失败（雪碧图下载或裁切均未产出；{video_id}）；未获得可观察图像，不能作为已观看依据");
    }
    let list = saved
        .iter()
        .map(|(t, p)| format!("采样请求 {} → {}", fmt_mmss(*t), p))
        .collect::<Vec<_>>()
        .join("\n");
    let note = format!(
        "已生成 {} 张平台预览抽样帧（采样请求区间 {}–{}；实际时刻以平台预览为准，非完整播放，未提取音频）。仅在本轮实际收到并观察附图后描述画面；若未收到图像请如实报告，不能把生成文件当作已观看：\n{list}",
        saved.len(),
        fmt_mmss(saved[0].0),
        fmt_mmss(saved[saved.len() - 1].0),
    );
    Ok((note, saved.into_iter().map(|(_, p)| p).collect()))
}

/// 秒 → mm:ss
fn fmt_mmss(sec: u64) -> String {
    format!("{}:{:02}", sec / 60, sec % 60)
}

pub(crate) fn register_act_gate(session_id: &str, gate: ActGate) {
    act_gates()
        .lock()
        .insert(session_id.to_string(), Arc::new(gate));
}

pub(crate) fn drop_act_gate(session_id: &str) {
    act_gates().lock().remove(session_id);
}

/// act 后回取当日计数（runner 同步回 VideoState）
pub(crate) fn act_gate_count(session_id: &str) -> u32 {
    act_gates()
        .lock()
        .get(session_id)
        .map(|g| g.actions_today.load(std::sync::atomic::Ordering::SeqCst))
        .unwrap_or(0)
}

/// video__act：评论/回复（闸门链：开关→目标核验→清洗→配额→审批/dry-run→buvid→执行）
/// 点播判权结果：通过（带一次性闸门）/ 拒绝（带指引文案）
enum GateOutcome {
    Granted(Arc<ActGate>),
    Denied(String),
}

/// 点播会话（非轮次）的即时判权：按会话来源角色对照通道 actRole 口径。
/// - 无外部来源（WebUI 控制台会话）→ 全权
/// - 未绑定身份（origin 是 ch:… 原始标注）→ 拒绝 + 指引 /pair
/// - 绑定身份但角色低于 actRole → 拒绝 + 提示提权
///
/// 通过后按通道 Settings 构建一次性 ActGate（开关/配额/清洗/dry-run/审批全量复用）。
async fn build_ondemand_gate(
    core: &Arc<Core>,
    ch: &Channel,
    session: &str,
) -> anyhow::Result<GateOutcome> {
    let s = super::Settings::from_channel(ch);
    // actRole 口径：admin（默认）= 仅管理者；member = 所有绑定成员
    let act_role = s.act_role.clone();
    // 点播闸门不落 act_gates 注册表（一次性；配额计数即时回写 VideoState）
    let gate = || {
        let mut state_probe = super::VideoState::load(core, &ch.id);
        state_probe.reset_day();
        Arc::new(ActGate::from_settings(&s, state_probe.actions_today))
    };
    // 会话来源：无 origin = 控制台会话（全权）；有 origin 按角色判
    let Some((origin_user, origin_role)) = core.store.get_session_origin(session).ok().flatten()
    else {
        return Ok(GateOutcome::Granted(gate()));
    };
    // 管理者账号清单：按会话来源的外部用户 ID 跨通道匹配——
    // 用户在**任一**通道的管理者清单命中即视同 admin（免配对绑定）。
    // 点播请求常跨通道发起（如在 QQ 里点播 B 站评论），判权跟「人」走不跟执行通道走；
    // 与绑定身份的 admin 角色同权并集。
    if !origin_user.trim().is_empty() {
        let uid = origin_user.trim().to_string();
        let hit = crate::platform::load_channels(core)
            .iter()
            .any(|c| c.admins.iter().any(|a| a.trim() == uid));
        if hit {
            return Ok(GateOutcome::Granted(gate()));
        }
    }
    // origin_role 形态：绑定身份 = identity id（"ch:xxx" 前缀的 UserIdentity.id）
    // 未绑定的原始标注 = "ch:<channel>:<external_id>"——区分靠查询身份表
    let _ = origin_user;
    // 兼容旧标注：原始形态以 ch: 开头且不是绑定身份 id 的都视为未绑定
    let is_bound = core.identity_of_id(&origin_role).is_some();
    if !is_bound {
        return Ok(GateOutcome::Denied(
            "已拒绝：当前用户未绑定通道身份，不能让智能体代发评论。请在控制台「通道身份」页签发配对码，然后发送 /pair <配对码> 完成绑定（观看视频不受影响）".into(),
        ));
    }
    let role = core
        .identity_of_id(&origin_role)
        .map(|i| i.role)
        .unwrap_or_else(|| "member".into());
    let allowed = match act_role.as_str() {
        "member" => role == "member" || role == "admin",
        _ => role == "admin", // admin 口径：仅管理者
    };
    if !allowed {
        return Ok(GateOutcome::Denied(format!(
            "已拒绝：点播评论需「{act_role}」及以上角色，当前用户角色为 {role}。请联系管理员在控制台「通道身份」页调整角色（观看视频不受影响）"
        )));
    }
    Ok(GateOutcome::Granted(gate()))
}

/// 点播会话配额即时回写：点播无 runner 收束，计数须当场落 VideoState。
/// 轮次会话（act_gates 有注册）由 ask_agent 收束时统一回写，此处跳过。
fn sync_ondemand_quota(core: &Arc<Core>, ch: &Channel, session: &str, gate: &ActGate) {
    if act_gates().lock().contains_key(session) {
        return; // 轮次会话：收束时统一回写
    }
    let mut state = super::VideoState::load(core, &ch.id);
    state.reset_day();
    let today = gate.actions_today.load(std::sync::atomic::Ordering::SeqCst);
    state.actions_today = state.actions_today.max(today);
    state.save(core, &ch.id);
}

async fn tool_act(
    core: &Arc<Core>,
    ch: &Channel,
    st: &Arc<Mutex<WatchSession>>,
    session: &str,
    trigger: WatchTrigger,
    args: Value,
) -> anyhow::Result<String> {
    let video_id = args
        .get("videoId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if video_id.is_empty() {
        anyhow::bail!("缺少 videoId");
    }
    // 闸门获取：轮次会话有注册闸门；点播会话（非轮次）按通道配置 + 会话来源角色
    // 即时构建一次性闸门——判权失败直接拒绝并给绑定/提权指引。
    let registered = act_gates().lock().get(session).cloned();
    let gate = match registered {
        Some(g) => g, // 轮次闸门（ask_agent 注册；随轮次收束清理）
        None => match build_ondemand_gate(core, ch, session).await? {
            GateOutcome::Granted(g) => g,
            GateOutcome::Denied(reason) => return Ok(reason),
        },
    };
    let comment_text = args
        .get("comment")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let reply_text = args
        .get("replyText")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let reply_target = args
        .get("replyTo")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    // 构造动作（comment 或 reply 二选一）
    let (kind, raw_text, target): (&str, String, Option<String>) = if !reply_text.is_empty() {
        if !gate.reply_on {
            return Ok("已拒绝：本通道未开启回复（config.reply=false）".into());
        }
        // 闸门 1：回复目标必须来自本会话已见评论（防编造 id）
        if !st.lock().seen_comments.contains(&reply_target) {
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
    let ch = live_act_channel(core, ch, trigger, target.is_some())?;
    let ch = &ch;

    // 闸门 2：内容清洗（字数上限 + markdown/指令痕迹去除）。
    // 超长/AI 味套话不静默截断——回给模型明确反馈，下一轮自我修正
    let text = clean_comment(&raw_text, gate.max_comment_chars);
    if text.trim().is_empty() {
        return Ok(format!(
            "已拒绝：清洗后文本为空（原文 {} 字）",
            raw_text.chars().count()
        ));
    }
    if raw_text.chars().count() > gate.max_comment_chars {
        return Ok(format!(
            "已拒绝：评论 {} 字，超过 {} 字上限。不是截断是重写：挑一个最想说的点，\
             一两句话说完（像随手打的短评），重新调用",
            raw_text.chars().count(),
            gate.max_comment_chars
        ));
    }
    if let Some(cliche) = detect_cliche(&text) {
        return Ok(format!(
            "已拒绝：评论含套话「{cliche}」，AI 味太重。删掉套话与总结腔，\
             直接说那个具体的点（画面/台词/细节），重新调用"
        ));
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
        let view = bili_get(
            ch,
            &format!("https://api.bilibili.com/x/web-interface/view?bvid={video_id}"),
        )
        .await?;
        api_ok(&view)?;
        item.aid = view.pointer("/data/aid").and_then(as_u64);
        item.title = view
            .pointer("/data/title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
    }
    let ch = live_act_channel(core, ch, trigger, target.is_some())?;
    let ch = &ch;
    let current = Settings::from_channel(ch);

    // 闸门 4：dry-run 分流（不落平台）
    if gate.dry_run || current.dry_run {
        gate.actions_today
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        sync_ondemand_quota(core, ch, session, &gate);
        st.lock().acted += 1;
        return Ok(format!(
            "[dry-run] 拟{}：{}{}｜目标视频 {}（{}）\n（dry-run 模式开启，未发送至平台；正式发送请在通道配置关闭 dryRun）",
            kind, text,
            target.map(|t| format!("（回复 id={t}）")).unwrap_or_default(),
            if item.title.trim().is_empty() { &video_id } else { &item.title },
            video_id,
        ));
    }

    // 闸门 5：审批模式（always = 产审批单，批准代发/拒绝跳过；off = 直接发）
    if gate.approval == "always" || current.approval == "always" {
        return create_act_approval(
            core,
            ch,
            session,
            st,
            &gate,
            kind,
            &text,
            target.as_ref(),
            &item,
            &video_id,
            trigger,
        )
        .await;
    }

    // 闸门 6：B站 buvid 指纹养护（发前自动补齐）
    if ch.kind == "bilibili" {
        bili_prime_buvid(ch).await;
    }

    // 执行
    let latest = live_act_channel(core, ch, trigger, target.is_some())?;
    let current = Settings::from_channel(&latest);
    anyhow::ensure!(
        !current.dry_run && current.approval != "always",
        "互动安全配置已变化，已取消本次发送"
    );
    post_comment(&latest, &item, &text, target.as_deref()).await?;
    gate.actions_today
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    sync_ondemand_quota(core, ch, session, &gate);
    {
        let mut s = st.lock();
        s.acted += 1;
        s.acted_notes.push(format!(
            "{kind}：{}",
            text.chars().take(60).collect::<String>()
        ));
    }
    Ok(format!(
        "{kind}已发送：{}（视频 {}）",
        text.chars().take(60).collect::<String>(),
        video_id
    ))
}

fn live_act_channel(
    core: &Core,
    ch: &Channel,
    trigger: WatchTrigger,
    reply: bool,
) -> anyhow::Result<Channel> {
    let ch = live_channel(core, ch, trigger)?;
    let s = Settings::from_channel(&ch);
    anyhow::ensure!(
        if reply { s.reply } else { s.comment },
        "已拒绝：本通道未开启{}",
        if reply { "回复" } else { "评论" }
    );
    Ok(ch)
}

/// 审批分流的代发执行入口（Core::approval_decide 的 [video] 回调）：
/// 载荷 = { channelId, videoId, text, replyTo }；buvid 养护后直接发。
pub(crate) async fn execute_act_payload(core: &Arc<Core>, payload: &str) -> anyhow::Result<String> {
    let v: Value =
        serde_json::from_str(payload).map_err(|e| anyhow::anyhow!("视频审批载荷解析失败：{e}"))?;
    let channels = platform::load_channels(core);
    let channel_id = v
        .get("channelId")
        .and_then(|x| x.as_str())
        .unwrap_or_default();
    let Some(ch) = channels.iter().find(|c| c.id == channel_id) else {
        anyhow::bail!("通道不存在：{channel_id}");
    };
    let video_id = v
        .get("videoId")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    let text = v
        .get("text")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    let reply_to = v
        .get("replyTo")
        .and_then(|x| x.as_str())
        .map(str::to_string);
    let trigger = approval_trigger(core, ch, payload, &v)?;
    let ch = live_act_channel(core, ch, trigger, reply_to.is_some())?;
    anyhow::ensure!(
        !Settings::from_channel(&ch).dry_run,
        "演练模式已开启，已取消审批代发"
    );
    let ch = &ch;
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
        let view = bili_get(
            ch,
            &format!("https://api.bilibili.com/x/web-interface/view?bvid={video_id}"),
        )
        .await?;
        api_ok(&view)?;
        item.aid = view.pointer("/data/aid").and_then(as_u64);
        live_act_channel(core, ch, trigger, reply_to.is_some())?;
        bili_prime_buvid(ch).await;
    }
    let latest = live_act_channel(core, ch, trigger, reply_to.is_some())?;
    anyhow::ensure!(
        !Settings::from_channel(&latest).dry_run,
        "演练模式已开启，已取消审批代发"
    );
    post_comment(&latest, &item, &text, reply_to.as_deref()).await?;
    Ok(format!(
        "已代发（审批通过）：{}",
        text.chars().take(60).collect::<String>()
    ))
}

fn approval_trigger(
    core: &Core,
    ch: &Channel,
    payload: &str,
    value: &Value,
) -> anyhow::Result<WatchTrigger> {
    if let Some(automatic) = value.get("automatic").and_then(Value::as_bool) {
        return Ok(if automatic {
            WatchTrigger::Automatic
        } else {
            WatchTrigger::OnDemand
        });
    }
    // 升级前的审批载荷没有来源字段；从原审批单会话识别，不能把滞留的自动动作
    // 默认当成点播绕过 watch。无法确认来源则保守沿用自动闸门，不重放未知任务。
    let watch_title = format!("channel:{}:video", ch.id);
    let mut manual = false;
    for request in core.approval_list(Some("pending"), usize::MAX)? {
        if request.command.strip_prefix("[video]").map(str::trim) == Some(payload.trim()) {
            if let Some(session) = core.store.get_session(&request.session_id)? {
                if session.title == watch_title {
                    return Ok(WatchTrigger::Automatic);
                }
                manual = true;
            }
        }
    }
    Ok(if manual {
        WatchTrigger::OnDemand
    } else {
        WatchTrigger::Automatic
    })
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
    trigger: WatchTrigger,
) -> anyhow::Result<String> {
    live_act_channel(core, ch, trigger, target.is_some())?;
    let payload = serde_json::json!({
        "channelId": ch.id,
        "videoId": video_id,
        "text": text,
        "replyTo": target,
        "automatic": trigger == WatchTrigger::Automatic,
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
        meta_json: None,
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
                gate.actions_today
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                st.lock().acted += 1;
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
    fn video_id_归一_裸号直通() {
        assert_eq!(normalize_video_id("BV1xx411c7mD"), "BV1xx411c7mD");
        assert_eq!(normalize_video_id("bv1xx411c7mD"), "bv1xx411c7mD");
        assert_eq!(normalize_video_id("av170001"), "av170001");
        assert_eq!(normalize_video_id("dQw4w9WgXcQ"), "dQw4w9WgXcQ"); // YouTube 11 位
        assert_eq!(normalize_video_id("  BV1xx411c7mD  "), "BV1xx411c7mD");
    }

    #[test]
    fn video_id_归一_长链抽取() {
        assert_eq!(
            normalize_video_id("https://www.bilibili.com/video/BV1xx411c7mD?p=5&spm_id_from=333"),
            "BV1xx411c7mD"
        );
        assert_eq!(
            normalize_video_id("https://m.bilibili.com/video/BV1AB2345678cd"),
            "BV1AB2345678cd"
        );
        assert_eq!(
            normalize_video_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
            "dQw4w9WgXcQ"
        );
        assert_eq!(
            normalize_video_id("https://youtu.be/dQw4w9WgXcQ"),
            "dQw4w9WgXcQ"
        );
        assert_eq!(
            normalize_video_id("https://www.youtube.com/shorts/abcdefghijk"),
            "abcdefghijk"
        );
    }

    #[test]
    fn video_id_归一_解析失败原样透传() {
        // 无 BV 的杂讯：原样返回（下游 API 报「视频不存在」）
        assert_eq!(normalize_video_id("随便看看"), "随便看看");
    }

    #[tokio::test]
    async fn video_id_归一_会话缓存() {
        let st = session_state("test-resolve-cache");
        let a = resolve_video_id(&st, "BV1xx411c7mD").await;
        let b = resolve_video_id(&st, "BV1xx411c7mD").await;
        assert_eq!(a, b);
        assert_eq!(st.lock().resolved_ids.len(), 1, "同输入只解析一次");
        drop_session("test-resolve-cache");
    }

    #[test]
    fn 点播判权_矩阵() {
        // 纯函数面：act_role 解析口径（Settings::from_channel 的 actRole 读取）
        let mk = |cfg: Option<&str>| {
            let mut ch = Channel {
                id: "bili-test".into(),
                kind: "bilibili".into(),
                enabled: true,
                group: None,
                allowed_chats: vec![],
                admins: vec![],
                account: None,
                secret: None,
                reply_webhook: None,
                token: None,
                config: std::collections::BTreeMap::new(),
                group_gate: None,
                proactive_reply: None,
                proactive_rate: None,
                mention_reply: None,
                capability: "computer".into(),
                memory_isolation: false,
                created_at: String::new(),
            };
            if let Some(v) = cfg {
                ch.config.insert("actRole".into(), v.into());
            }
            super::super::Settings::from_channel(&ch).act_role
        };
        assert_eq!(mk(None), "admin", "缺省 admin");
        assert_eq!(mk(Some("member")), "member");
        assert_eq!(mk(Some("ADMIN")), "admin", "大小写归一");
        assert_eq!(mk(Some("hacker")), "admin", "非法值回落默认");
    }

    #[test]
    fn 字幕分页_窗口与导航() {
        let lines: Vec<Value> = (0..300)
            .map(|i| json!({ "from": i as f64 * 3.0, "content": format!("第{i}句") }))
            .collect();
        let p1 = format_subtitle_pages(&lines, 1).unwrap();
        assert!(p1.contains("第0句") && p1.contains("第119句"));
        assert!(p1.contains("page=2"), "应提示下一页：{p1}");
        let p3 = format_subtitle_pages(&lines, 3).unwrap();
        assert!(
            p3.contains("已到末页"),
            "第 3 页（300/120=2.5→3 页）末页：{p3}"
        );
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
                if v == 0 {
                    msg.push(b);
                    break;
                } else {
                    msg.push(b | 0x80);
                }
            }
            let tb = text.as_bytes();
            let mut l = tb.len() as u64;
            let mut len_bytes = Vec::new();
            loop {
                let b = (l & 0x7f) as u8;
                l >>= 7;
                if l == 0 {
                    len_bytes.push(b);
                    break;
                } else {
                    len_bytes.push(b | 0x80);
                }
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
                if n == 0 {
                    out.push(b);
                    break;
                } else {
                    out.push(b | 0x80);
                }
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
        assert_eq!(bili.len(), 8, "B站 8 工具（含 shots 画面抽帧）：{bili:?}");
        assert!(bili.iter().any(|t| t.name == "video__danmaku"));
        assert!(bili.iter().any(|t| t.name == "video__related"));
        assert!(
            bili.iter().any(|t| t.name == "video__shots"),
            "B站应有画面抽帧（视觉直看）"
        );

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
            admins: vec![],
            account: None,
            secret: None,
            token: None,
            reply_webhook: None,
            group_gate: None,
            proactive_reply: None,
            proactive_rate: None,
            mention_reply: None,
            capability: "computer".into(),
            memory_isolation: false,
            created_at: String::new(),
            config: [
                ("comment".to_string(), "true".to_string()),
                ("reply".to_string(), "true".to_string()),
                ("dryRun".to_string(), "true".to_string()),
                ("apiKey".to_string(), "test".to_string()),
            ]
            .into_iter()
            .collect(),
        };
        let settings = Settings::from_channel(&ch);
        assert!(settings.comment && settings.dry_run);
        platform::save_channels(&core, &[ch.clone()]).unwrap();
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
            WatchTrigger::Automatic,
            json!({ "videoId": "vid1", "replyText": "你好", "replyTo": "ghost-id" }),
        )
        .await
        .unwrap()
        .0;
        assert!(r.contains("目标评论未见"), "{r}");

        // 正常评论（dry-run）→ 回执不落平台
        let r = dispatch_tool(
            &core,
            "video__act",
            &ch,
            "s1",
            &st,
            WatchTrigger::Automatic,
            json!({ "videoId": "vid1", "comment": "测试评论内容" }),
        )
        .await
        .unwrap()
        .0;
        assert!(r.contains("[dry-run]"), "{r}");
        assert_eq!(act_gate_count("s1"), 1);

        // 配额耗尽（limit=1 已用）→ 拒绝并告知
        let r = dispatch_tool(
            &core,
            "video__act",
            &ch,
            "s1",
            &st,
            WatchTrigger::Automatic,
            json!({ "videoId": "vid1", "comment": "再来一条" }),
        )
        .await
        .unwrap()
        .0;
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
            admins: vec![],
            account: None,
            secret: None,
            token: None,
            reply_webhook: None,
            group_gate: None,
            proactive_reply: None,
            proactive_rate: None,
            mention_reply: None,
            capability: "computer".into(),
            memory_isolation: false,
            created_at: String::new(),
            config: [
                ("comment".to_string(), "false".to_string()),
                ("dryRun".to_string(), "true".to_string()),
            ]
            .into_iter()
            .collect(),
        };
        let settings = Settings::from_channel(&ch);
        register_act_gate("s2", ActGate::from_settings(&settings, 0));
        let st = session_state("s2");
        let core = test_core("off");
        platform::save_channels(&core, &[ch.clone()]).unwrap();
        let r = dispatch_tool(
            &core,
            "video__act",
            &ch,
            "s2",
            &st,
            WatchTrigger::Automatic,
            json!({ "videoId": "v", "comment": "hi" }),
        )
        .await
        .unwrap()
        .0;
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
        cfg.security.approval_wait_secs = 0;
        Arc::new(Core::with_config(cfg).expect("Core 创建失败"))
    }

    fn yt_channel() -> Channel {
        Channel {
            id: "yt-apr".into(),
            kind: "youtube".into(),
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
                ("dryRun".to_string(), "false".to_string()),
                ("approval".to_string(), "always".to_string()),
            ]
            .into_iter()
            .collect(),
            group_gate: None,
            proactive_reply: None,
            proactive_rate: None,
            mention_reply: None,
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
        platform::save_channels(&core, &[ch.clone()]).unwrap();
        register_act_gate("s-apr", ActGate::from_settings(&s, 0));
        let st = session_state("s-apr");
        let r = dispatch_tool(
            &core,
            "video__act",
            &ch,
            "s-apr",
            &st,
            WatchTrigger::Automatic,
            json!({ "videoId": "vid1", "comment": "审批模式下的拟发评论" }),
        )
        .await
        .unwrap()
        .0;
        // wait 语义：等待窗口内即时回执「已进入人工审批」；超时回执「等待超时本轮跳过」。
        // 两者都证明未直接落平台（审批分流生效），断言取并集。
        assert!(
            r.contains("人工审批") || r.contains("等待超时"),
            "应走审批分流而非直接发送：{r}"
        );
        // 审批单已落库且 pending
        let pending = core.approval_list(Some("pending"), 10).unwrap();
        assert_eq!(pending.len(), 1, "应有一张待审批单");
        assert!(
            pending[0].command.starts_with("[video]"),
            "审批单应带 [video] 前缀"
        );
        assert!(
            pending[0].command.contains("审批模式下的拟发评论"),
            "审批单含拟发文本"
        );
        drop_act_gate("s-apr");
        drop_session("s-apr");
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }
}
