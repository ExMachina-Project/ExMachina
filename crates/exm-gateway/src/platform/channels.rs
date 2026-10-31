//! gateway/platform/channels —— 自 platform.rs 按资源域拆出（纯搬移，零行为变更）

// ---------------------------------------------------------------- 通道网关（webhook 通道）


use super::*;
/// 支持的平台清单：
/// - `webhook` / `qq` / `wechat`：webhook 桥接语义（社区桥把消息 POST 到 inbound，回复走回调）；
/// - `telegram`：内置长轮询适配器（telegram.rs）；
/// - `qqbot`：QQ 官方机器人（q.qq.com 开放平台，WebSocket 长连接，qqbot.rs）；
/// - `napcat`：OneBot 11 实现（NapCat / Lagrange 等，正向 WebSocket，napcat.rs）；
/// - `discord`：Discord 机器人（Gateway WebSocket，discord.rs）；
/// - `slack`：Slack 应用（Socket Mode，免公网回调，slack.rs）；
/// - `matrix`：Matrix 房间（Client-Server API 长轮询 /sync，matrix.rs）；
/// - `bilibili` / `youtube`：视频平台主动观看与互动（video.rs，按配置轮询热门/关键词视频，
///   读取字幕/简介与评论 → 生成评论/回复 → 可选真发；默认 dry-run）；
/// - `rss`：RSS/Atom 订阅阅读（plugins/rss，按配置轮询新条目 → 阅读简报 → 可选推送）。
pub const CHANNEL_PLATFORMS: &[&str] = &[
    "webhook", "telegram", "qqbot", "napcat", "discord", "slack", "matrix", "qq", "wechat",
    "bilibili", "youtube", "rss",
];

/// 通道定义：外部消息源接入点。平台 = `type`（webhook / telegram / qqbot / napcat …）。
/// 多平台多账号：同类平台可并存多条（每条一个账号），每条可绑定不同智能体组。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub id: String,
    /// 平台：webhook | telegram | qqbot | napcat | qq | wechat
    #[serde(rename = "type", default = "default_platform")]
    pub kind: String,
    #[serde(default = "default_true_channel")]
    pub enabled: bool,
    /// 账号绑定组：该账号的入站消息在此组上下文执行；缺省 = 激活组
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// 会话白名单：允许交互的会话键（Telegram chat id / QQ openid / OneBot 群号或 QQ 号；空 = 不限）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_chats: Vec<String>,
    /// 管理者账号：外部用户 ID 清单（QQ 号 / Telegram user id 等），命中即视同 admin 角色——
    /// 免配对绑定即拥有管理者判权（与绑定身份的 admin 角色同权并集）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub admins: Vec<String>,
    /// 账号备注（同平台多账号时区分用途）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// webhook / qq / wechat 桥接语义：入站密钥校验
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_webhook: Option<String>,
    /// telegram 平台：BotFather 签发的 bot token
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// 平台扩展配置（新平台零结构体改动）：qqbot → appId/appSecret/sandbox；
    /// napcat → url/token。空值键不入库。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config: BTreeMap<String, String>,
    /// 群聊唤醒门控：Some(true)=群内仅提及/回复触发；None=取全局默认（config.identity.groupGateDefault）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_gate: Option<bool>,
    /// 能力边界：chat = 仅聊天；tools = 可调工具（不含 computer）；computer = 完整能力。
    /// 缺省 computer：兼容旧通道配置，不改变既有接入账号的行为。
    #[serde(default = "default_channel_capability")]
    pub capability: String,
    /// 记忆隔离：false（默认）= 同组所有平台会话共享记忆；true = 本通道独立记忆分区。
    #[serde(default)]
    pub memory_isolation: bool,
    pub created_at: String,
}

pub(crate) fn default_platform() -> String {
    "webhook".into()
}
pub(crate) fn default_true_channel() -> bool {
    true
}
pub(crate) fn default_channel_capability() -> String {
    "computer".into()
}

pub(crate) fn channels_path(core: &exm_core::Core) -> std::path::PathBuf {
    core.config().data_dir.join("channels.json")
}

pub(crate) fn load_channels(core: &exm_core::Core) -> Vec<Channel> {
    std::fs::read_to_string(channels_path(core))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub(crate) fn save_channels(core: &exm_core::Core, channels: &[Channel]) -> anyhow::Result<()> {
    let p = channels_path(core);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&p, serde_json::to_string_pretty(channels)?)?;
    Ok(())
}

impl Channel {
    /// 平台扩展配置读取（config 键，去除首尾空白）
    pub(crate) fn cfg(&self, key: &str) -> Option<String> {
        self.config
            .get(key)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    /// 群聊唤醒门控是否开启（通道级覆盖 > 全局默认）
    pub(crate) fn gate_enabled(&self, core: &exm_core::Core) -> bool {
        self.group_gate
            .unwrap_or_else(|| core.config().identity.group_gate_default)
    }

    /// 能力边界（未知值回退完整档：老配置不因新字段失效）。
    pub(crate) fn tool_scope(&self) -> exm_core::types::ToolScope {
        exm_core::types::ToolScope::parse(&self.capability).unwrap_or_default()
    }

    /// 本通道的记忆分区键：开启隔离时与同组其他平台/网页会话互不可见。
    /// 键里带通道 id，同平台多账号也各自隔离（账号 = 一条通道）。
    pub(crate) fn memory_partition(&self, group_id: &str) -> Option<String> {
        self.memory_isolation
            .then(|| format!("{}::channel:{}", group_id, self.id))
    }
}

// ---------------------------------------------------------------- 通道运行状态

/// 通道运行状态（内置适配器上报；webhook/qq/wechat 桥接无运行时，不产生状态）
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChanStatus {
    /// ok | error
    pub state: String,
    /// 人类可读明细（机器人身份 / 最近一次错误原因）
    pub detail: String,
    /// 最近上报时刻（ISO8601）
    pub at: String,
}

pub(crate) fn chan_statuses() -> &'static Mutex<HashMap<String, ChanStatus>> {
    static S: OnceLock<Mutex<HashMap<String, ChanStatus>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 适配器上报连接状态（连上 / 断开 / 出错均覆盖写，UI 只看最近一次）
pub(crate) fn report_status(id: &str, state: &str, detail: impl Into<String>) {
    chan_statuses().lock().insert(
        id.to_string(),
        ChanStatus {
            state: state.to_string(),
            detail: detail.into(),
            at: exm_core::types::now_iso(),
        },
    );
}

/// 通道删除 / 停用时清除残留状态（下线的账号不该在状态表里阴魂不散）
pub(crate) fn clear_status(id: &str) {
    chan_statuses().lock().remove(id);
}

/// 单通道运行状态读取（连通测试的「长连接平台」口径）
pub(crate) fn status_of(id: &str) -> Option<ChanStatus> {
    chan_statuses().lock().get(id).cloned()
}

/// 通道运行状态总览（通道页状态列轮询）
pub async fn channel_status() -> impl IntoResponse {
    let snapshot = chan_statuses().lock().clone();
    Json(serde_json::to_value(snapshot).unwrap_or(Value::Null)).into_response()
}

// ---------------------------------------------------------------- B 站扫码登录
// 出码与轮询都是无副作用的 passport 只读接口；登录态 Cookie 只在 poll 确认瞬间产生，
// 直接回给前端填表单，不落盘（保存通道时才随 config 入库并掩码）。

pub async fn bili_qr_start() -> impl IntoResponse {
    match crate::plugins::video::bili_qr_generate().await {
        Ok(s) => Json(json!({ "qrcodeKey": s.qrcode_key, "url": s.url })).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("B 站出码失败：{e:#}") })),
        )
            .into_response(),
    }
}

pub async fn bili_qr_poll(Path(qrcode_key): Path<String>) -> impl IntoResponse {
    if qrcode_key.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "缺少 qrcode_key" })),
        )
            .into_response();
    }
    match crate::plugins::video::bili_qr_poll(qrcode_key.trim()).await {
        // Ok(登录态) → 成功；Err(业务码) → 86101 未扫 / 86090 待确认 / 86083 过期
        Ok(Ok(login)) => Json(json!({
            "state": "confirmed",
            "sessdata": login.sessdata,
            "biliJct": login.bili_jct,
            "uid": login.dede_user_id,
            "uname": login.uname,
        }))
        .into_response(),
        Ok(Err(code)) => Json(json!({ "state": "waiting", "code": code })).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("B 站轮询失败：{e:#}") })),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------- 通道 CRUD（脱敏回显）

/// 平台 config 中的敏感键（掩码回显；更新时掩码值 = 沿用旧值）。
/// 插件通道（经注册面声明的 kind）再并入清单 sensitive 凭据键（双源并集）。
pub(crate) fn secret_config_keys(kind: &str) -> Vec<&'static str> {
    let static_keys: &[&'static str] = match kind {
        "qqbot" => &["appSecret"],
        "napcat" => &["token"],
        // Slack Socket Mode 应用级令牌（xapp-）；机器人令牌走顶层 token
        "slack" => &["appToken"],
        // B 站：登录态 Cookie（SESSDATA 鉴权 / biliJct 为 CSRF）；YouTube：API Key / OAuth
        "bilibili" => &["sessdata", "biliJct"],
        "youtube" => &["apiKey", "accessToken"],
        _ => &[],
    };
    static_keys.to_vec()
}

/// 通道脱敏判定：静态表或任一已注册插件清单的 sensitive 凭据键命中即掩码。
/// （插件凭据面由 PluginChannelDecl.credentials.sensitive 声明——通道页渲染与
/// 脱敏回显共用同一清单，两边不会漂移。）
pub(crate) fn is_sensitive_config_key(kind: &str, key: &str, core: &exm_core::Core) -> bool {
    if secret_config_keys(kind).contains(&key) {
        return true;
    }
    core.plugins()
        .enabled_providers()
        .iter()
        .filter_map(|p| p.channel_provider())
        .any(|cp| {
            cp.kinds().iter().any(|k| k == kind)
                && cp
                    .credential_fields()
                    .iter()
                    .any(|f| f.sensitive && f.key == key)
        })
}

/// 通道脱敏序列化：顶层 token/secret 与平台敏感 config 键掩码回显，任何接口都不出明文密钥
pub(crate) fn channel_json(ch: &Channel, core: &exm_core::Core) -> Value {
    let mut v = serde_json::to_value(ch).unwrap_or(Value::Null);
    let Some(obj) = v.as_object_mut() else {
        return v;
    };
    let mask = Value::String(crate::llm_admin::KEY_MASK.into());
    if ch
        .secret
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
    {
        obj.insert("secret".into(), mask.clone());
    }
    if ch
        .token
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
    {
        obj.insert("token".into(), mask.clone());
    }
    if let Some(cfg) = obj.get_mut("config").and_then(|c| c.as_object_mut()) {
        let keys: Vec<String> = cfg.keys().cloned().collect();
        for k in keys {
            if ch.cfg(&k).is_some() && is_sensitive_config_key(&ch.kind, &k, core) {
                cfg.insert(k, mask.clone());
            }
        }
    }
    v
}

pub async fn list_channels(State(st): State<AppState>) -> impl IntoResponse {
    let list: Vec<Value> = load_channels(&st.core)
        .iter()
        .map(|c| channel_json(c, &st.core))
        .collect();
    Json(list).into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelBody {
    /// 创建必填；更新走路径参数，body 可不带（缺省即空串）
    #[serde(default)]
    pub id: String,
    /// 平台：webhook（默认）| telegram | qqbot | napcat | qq | wechat
    #[serde(default)]
    pub platform: Option<String>,
    /// 账号绑定组
    #[serde(default)]
    pub group: Option<String>,
    /// 会话白名单（空 = 不限）
    #[serde(default)]
    pub allowed_chats: Option<Vec<String>>,
    /// 管理者账号（外部用户 ID 清单；判权视同 admin）
    #[serde(default)]
    pub admins: Option<Vec<String>>,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub reply_webhook: Option<String>,
    /// telegram bot token
    #[serde(default)]
    pub token: Option<String>,
    /// 平台扩展配置（qqbot: appId/appSecret/sandbox；napcat: url/token）
    #[serde(default)]
    pub config: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub enabled: Option<bool>,
    /// 群聊唤醒门控（None = 取全局默认）
    #[serde(default)]
    pub group_gate: Option<bool>,
    /// 能力边界：chat | tools | computer（None = 创建用完整档；更新不传 = 保持）
    #[serde(default)]
    pub capability: Option<String>,
    /// 记忆隔离（None = 创建用共享，更新不传 = 保持）
    #[serde(default)]
    pub memory_isolation: Option<bool>,
}

pub async fn create_channel(
    State(st): State<AppState>,
    Json(b): Json<ChannelBody>,
) -> impl IntoResponse {
    let id = b.id.trim().to_string();
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "非法通道 id" })),
        )
            .into_response();
    }
    let platform = b.platform.clone().unwrap_or_else(|| "webhook".into());
    if !CHANNEL_PLATFORMS.contains(&platform.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("不支持的平台（可选 {}）", CHANNEL_PLATFORMS.join(" / ")) })),
        )
            .into_response();
    }
    if platform == "telegram" && b.token.as_deref().is_none_or(|t| t.trim().is_empty()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "telegram 通道需要 bot token" })),
        )
            .into_response();
    }
    let config: BTreeMap<String, String> = b
        .config
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, v)| !v.trim().is_empty())
        .collect();
    // 新平台必填项：qqbot 需要 appId + appSecret；napcat 需要 WS 地址
    if platform == "qqbot"
        && (config.get("appId").is_none_or(|v| v.trim().is_empty())
            || config.get("appSecret").is_none_or(|v| v.trim().is_empty()))
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "qqbot 通道需要 config.appId 与 config.appSecret" })),
        )
            .into_response();
    }
    if platform == "napcat" && config.get("url").is_none_or(|v| v.trim().is_empty()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "napcat 通道需要 config.url（OneBot 11 正向 WebSocket 地址）" })),
        )
            .into_response();
    }
    // discord / slack / matrix 的顶层 token 均为必填（bot token 或 access token）
    if matches!(platform.as_str(), "discord" | "slack" | "matrix")
        && b.token.as_deref().is_none_or(|t| t.trim().is_empty())
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "该平台需要 token（Discord bot token / Slack bot token / Matrix access token）" })),
        )
            .into_response();
    }
    if platform == "slack" && config.get("appToken").is_none_or(|v| v.trim().is_empty()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "slack 通道需要 config.appToken（Socket Mode 应用级令牌）" })),
        )
            .into_response();
    }
    if platform == "matrix" && config.get("homeserver").is_none_or(|v| v.trim().is_empty()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "matrix 通道需要 config.homeserver（如 https://matrix.org）" })),
        )
            .into_response();
    }
    if platform == "bilibili"
        && (config.get("sessdata").is_none_or(|v| v.trim().is_empty())
            || config.get("biliJct").is_none_or(|v| v.trim().is_empty()))
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "bilibili 通道需要 config.sessdata 与 config.biliJct（登录 Cookie）" })),
        )
            .into_response();
    }
    if platform == "youtube" && config.get("apiKey").is_none_or(|v| v.trim().is_empty()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "youtube 通道需要 config.apiKey（YouTube Data API v3）" })),
        )
            .into_response();
    }
    // actRole 合法性（点播评论准入：admin/member）
    if let Some(r) = config.get("actRole") {
        let r = r.trim();
        if !r.is_empty() && r != "admin" && r != "member" {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "config.actRole 须为 admin 或 member" })),
            )
                .into_response();
        }
    }
    let capability = b
        .capability
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("computer");
    if exm_core::types::ToolScope::parse(capability).is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "capability 须为 chat / tools / computer" })),
        )
            .into_response();
    }
    // 绑定目标校验：组 id 或 single:<智能体id>（单智能体为默认形态）
    if let Some(g) = b.group.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        if !target_exists(&st.core, g) {
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    json!({ "error": "绑定的智能体或组不存在（智能体写 single:<id>，组写组 id）" }),
                ),
            )
                .into_response();
        }
    }
    let mut channels = load_channels(&st.core);
    if channels.iter().any(|c| c.id == id) {
        return (StatusCode::CONFLICT, Json(json!({ "error": "通道已存在" }))).into_response();
    }
    let ch = Channel {
        id,
        kind: platform,
        enabled: b.enabled.unwrap_or(true),
        group: b.group.clone().filter(|s| !s.trim().is_empty()),
        allowed_chats: b.allowed_chats.clone().unwrap_or_default(),
        admins: b.admins.clone().unwrap_or_default(),
        account: b.account.clone().filter(|s| !s.trim().is_empty()),
        secret: b.secret.filter(|s| !s.trim().is_empty()),
        reply_webhook: b.reply_webhook.filter(|s| !s.trim().is_empty()),
        token: b.token.filter(|s| !s.trim().is_empty()),
        config,
        group_gate: b.group_gate,
        capability: capability.to_string(),
        memory_isolation: b.memory_isolation.unwrap_or(false),
        created_at: exm_core::types::now_iso(),
    };
    channels.push(ch.clone());
    if let Err(e) = save_channels(&st.core, &channels) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response();
    }
    Json(channel_json(&ch, &st.core)).into_response()
}

/// 更新通道（启停 / 换组 / 换 token 等）
pub async fn update_channel(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<ChannelBody>,
) -> impl IntoResponse {
    let mut channels = load_channels(&st.core);
    let Some(ch) = channels.iter_mut().find(|c| c.id == id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "通道不存在" })),
        )
            .into_response();
    };
    if b.enabled == Some(false) {
        // 停用即下线：清掉运行状态，避免「已停用」的账号还挂着旧状态
        clear_status(&id);
    }
    if let Some(v) = &b.enabled {
        ch.enabled = *v;
    }
    if let Some(v) = b.group_gate {
        ch.group_gate = Some(v);
    }
    if let Some(v) = &b.capability {
        let cap = v.trim();
        if exm_core::types::ToolScope::parse(cap).is_none() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "capability 须为 chat / tools / computer" })),
            )
                .into_response();
        }
        ch.capability = cap.to_ascii_lowercase();
    }
    if let Some(v) = b.memory_isolation {
        ch.memory_isolation = v;
    }
    if let Some(v) = &b.group {
        ch.group = if v.trim().is_empty() {
            None
        } else {
            Some(v.clone())
        };
    }
    if let Some(v) = &b.allowed_chats {
        ch.allowed_chats = v.iter().filter(|s| !s.trim().is_empty()).cloned().collect();
    }
    // 管理者账号：整表替换（空 = 清空；条目为外部用户 ID）
    if let Some(v) = &b.admins {
        ch.admins = v.iter().filter(|s| !s.trim().is_empty()).cloned().collect();
    }
    if let Some(v) = &b.account {
        ch.account = if v.trim().is_empty() {
            None
        } else {
            Some(v.clone())
        };
    }
    // 敏感字段三态：掩码 = 沿用旧值；空 = 清除；其余 = 新值（与提供商页同一语义）
    if let Some(v) = &b.secret {
        let t = v.trim();
        if t == crate::llm_admin::KEY_MASK {
            // 掩码回传，保留旧值
        } else if t.is_empty() {
            ch.secret = None;
        } else {
            ch.secret = Some(t.to_string());
        }
    }
    if let Some(v) = &b.reply_webhook {
        ch.reply_webhook = if v.trim().is_empty() {
            None
        } else {
            Some(v.clone())
        };
    }
    if let Some(v) = &b.token {
        let t = v.trim();
        if t == crate::llm_admin::KEY_MASK {
            // 掩码回传，保留旧值
        } else if t.is_empty() {
            ch.token = None;
        } else {
            ch.token = Some(t.to_string());
        }
    }
    if let Some(v) = &b.config {
        // actRole 合法性前置校验（点播评论准入：admin/member）
        if let Some(r) = v.get("actRole") {
            let r = r.trim();
            if !r.is_empty() && r != "admin" && r != "member" {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "config.actRole 须为 admin 或 member" })),
                )
                    .into_response();
            }
        }
        // 整表替换：UI 编辑时按平台提交完整配置；启停等局部更新不带 config 即保持不变。
        // 敏感键掩码回传 = 从旧表取回原值；空值键不入库（用户清空敏感键即删除）。
        // 防误丢保护：敏感键在旧表有值而新表缺失时保留旧值——API 直接 PUT 局部
        // config（如仅 actRole）不应静默清掉登录凭据（部署实测踩坑）。
        let mut next: BTreeMap<String, String> = BTreeMap::new();
        for (k, val) in v {
            let t = val.trim();
            if t.is_empty() {
                continue;
            }
            if t == crate::llm_admin::KEY_MASK {
                if let Some(old) = ch.config.get(k) {
                    next.insert(k.clone(), old.clone());
                }
                continue;
            }
            next.insert(k.clone(), t.to_string());
        }
        for (k, old) in &ch.config {
            if !next.contains_key(k)
                && !v.contains_key(k)
                && is_sensitive_config_key(&ch.kind, k, &st.core)
            {
                next.insert(k.clone(), old.clone());
            }
        }
        ch.config = next;
    }
    let saved = ch.clone();
    match save_channels(&st.core, &channels) {
        Ok(_) => Json(channel_json(&saved, &st.core)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn delete_channel(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let mut channels = load_channels(&st.core);
    let before = channels.len();
    channels.retain(|c| c.id != id);
    if channels.len() == before {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "通道不存在" })),
        )
            .into_response();
    }
    match save_channels(&st.core, &channels) {
        Ok(_) => {
            clear_status(&id);
            Json(json!({ "ok": true })).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboundBody {
    #[serde(default)]
    pub secret: Option<String>,
    pub text: String,
    #[serde(default)]
    pub sender_id: Option<String>,
    /// 会话键：相同键复用同一会话；缺省按 senderId；再缺省 default
    #[serde(default)]
    pub session_key: Option<String>,
    #[serde(default)]
    pub reply_webhook: Option<String>,
}

pub async fn channel_inbound(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<InboundBody>,
) -> impl IntoResponse {
    let Some(channel) = load_channels(&st.core).into_iter().find(|c| c.id == id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "通道不存在" })),
        )
            .into_response();
    };
    if !channel.enabled {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "通道已停用" })),
        )
            .into_response();
    }
    if let Some(secret) = &channel.secret {
        if b.secret.as_deref() != Some(secret.as_str()) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "通道密钥不符" })),
            )
                .into_response();
        }
    }
    if b.text.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "text 不能为空" })),
        )
            .into_response();
    }
    // 账号绑定目标（单智能体或组）：入站消息在绑定的上下文执行（会话也归属该 scope）
    let prev_group = st.core.active_group();
    let prev_single = st.core.registry().active_single();
    let target = channel
        .group
        .clone()
        .unwrap_or_else(|| format!("{SINGLE_PREFIX}{}", default_single_id(&st.core)));
    let switched = switch_context_to(&st.core, &target);
    // 会话复用（scope 内）：channel:{id}:{sessionKey|senderId|default}
    let key = b
        .session_key
        .clone()
        .or_else(|| b.sender_id.clone())
        .unwrap_or_else(|| "default".into());
    let title = format!("channel:{}:{}", channel.id, key);
    let scope_id = st.core.registry().active_scope();
    let session = match st
        .core
        .list_sessions_in_group(&scope_id)
        .ok()
        .and_then(|list| list.into_iter().find(|s| s.title == title))
    {
        Some(s) => s,
        None => match st.core.create_session(&title) {
            Ok(s) => s,
            Err(e) => {
                if switched {
                    restore_context(&st.core, &prev_group, &prev_single);
                }
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": e.to_string() })),
                )
                    .into_response();
            }
        },
    };
    // 能力边界 + 记忆隔离：webhook 与内置适配器走同一套通道配置语义
    let tool_scope = channel.tool_scope();
    let partition = channel.memory_partition(&scope_id);
    let _ = st
        .core
        .store
        .set_session_memory_partition(&session.id, partition.as_deref());
    // 出站回调：通道级 replyWebhook 或请求级覆盖；运行结束后推送最终事件载荷
    let reply_url = b
        .reply_webhook
        .clone()
        .or_else(|| channel.reply_webhook.clone());
    if let Some(url) = reply_url {
        let core = st.core.clone();
        let sid = session.id.clone();
        let mut rx = core.subscribe();
        tokio::spawn(async move {
            while let Ok(evt) = rx.recv().await {
                if evt.session_id != sid {
                    continue;
                }
                if evt.kind == "run.finished" || evt.kind == "run.error" {
                    let _ = reqwest::Client::new()
                        .post(&url)
                        .json(&evt.payload)
                        .send()
                        .await;
                    break;
                }
            }
        });
    }
    // 202 受理，运行过程经 WS 推流；运行结束后恢复交互上下文
    let core = st.core.clone();
    let text = b.text.clone();
    let sid = session.id.clone();
    tokio::spawn(async move {
        let r = core.chat_scoped(&sid, &text, tool_scope).await;
        if switched {
            restore_context(&core, &prev_group, &prev_single);
        }
        if let Err(e) = r {
            let _ = core.events.send(CoreEvent {
                kind: "run.error".into(),
                session_id: sid,
                payload: json!({ "message": e.to_string() }),
            });
        }
    });
    (
        StatusCode::ACCEPTED,
        Json(json!({ "accepted": true, "sessionId": session.id, "group": st.core.active_group() })),
    )
        .into_response()
}
