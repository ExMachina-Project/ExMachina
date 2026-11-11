//! video 域分文件（自 mod.rs 按域拆出，纯搬移零行为变更）。
//! 平台域：B 站 API（含扫码登录）/ YouTube API / 连通探针 / 通用小工具。

use super::*;

// ---------------------------------------------------------------- 平台动作分发

#[derive(Clone, Debug)]
pub(crate) struct VideoItem {
    pub(crate) platform: String,
    pub(crate) id: String,
    pub(crate) aid: Option<u64>,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) author: String,
    pub(crate) views: u64,
}

pub(crate) fn video_key(v: &VideoItem) -> String {
    format!("{}:{}", v.platform, v.id)
}

pub(crate) fn matches_keywords(v: &VideoItem, keywords: &[String]) -> bool {
    let hay = format!("{} {} {}", v.title, v.description, v.author).to_lowercase();
    keywords.iter().any(|k| hay.contains(&k.to_lowercase()))
}

pub(crate) async fn post_comment(
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

pub(crate) fn http_client() -> reqwest::Client {
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

pub(crate) fn bili_cookie(ch: &Channel) -> String {
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
    Mutex<std::collections::HashMap<String, (String, Option<String>, bool)>>;
pub(crate) fn buvid_cache() -> &'static BuvidCache {
    static CACHE: OnceLock<BuvidCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// 读取缓存的 buvid（无则 None——发评论前应先 bili_prime_buvid）
pub(crate) fn buvid_cached(ch: &Channel) -> Option<(String, Option<String>)> {
    buvid_cache()
        .lock()
        .get(&ch.id)
        .map(|(b3, b4, _)| (b3.clone(), b4.clone()))
}

/// 异步获取并缓存 buvid（观看轮次开始时调用一次；匿名访问首页即下发）。
/// 2024+ 风控：buvid3 必须「激活」后才有发评论资格（未激活 = code:0 但评论进
/// 自查不上屏，或直接 -352 风控）。激活 = 带 buvid3 cookie POST 指纹 payload
/// 到 gaia-gateway/ExClimbWuzhi。缓存值三元组：(buvid3, buvid4, activated)。
pub(crate) async fn bili_prime_buvid(ch: &Channel) {
    let cache = buvid_cache();
    // 未激活的缓存条目要重试（激活失败常见于网络抖动；指纹一日内有效，成功后不再重跑）
    let need = match cache.lock().get(&ch.id) {
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
        .insert(ch.id.clone(), (b3, b4, activated));
}

/// 桌面 Chrome 指纹常量（ExClimbWuzhi payload 内层；社区长期维护的默认值）
pub(crate) const FINGERPRINT_JSON: &str = r##"{
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
pub(crate) async fn activate_buvid(buvid3: &str) -> bool {
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

pub(crate) fn bili_request(ch: &Channel, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    bili_request_with_referer(ch, BILI_REFERER, rb)
}

pub(crate) fn bili_request_with_referer(
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

pub(crate) async fn bili_get(ch: &Channel, url: &str) -> anyhow::Result<Value> {
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

pub(crate) fn api_message(v: &Value) -> String {
    v.get("message")
        .and_then(|m| m.as_str())
        .or_else(|| v.get("error").and_then(|m| m.as_str()))
        .unwrap_or("未知平台错误")
        .to_string()
}

pub(crate) fn api_ok(v: &Value) -> anyhow::Result<()> {
    let code = v.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
    if code == 0 {
        Ok(())
    } else {
        anyhow::bail!("平台返回 code={code}：{}", api_message(v))
    }
}

pub(crate) async fn bili_popular(ch: &Channel, limit: usize) -> anyhow::Result<Vec<VideoItem>> {
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

pub(crate) async fn bili_search(ch: &Channel, keyword: &str, _s: &Settings) -> anyhow::Result<Vec<VideoItem>> {
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

pub(crate) async fn bili_post_comment(
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
pub(crate) async fn bili_comment_visible(ch: &Channel, aid: u64, uid: &str) -> anyhow::Result<bool> {
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

pub(crate) fn yt_key(ch: &Channel) -> anyhow::Result<String> {
    ch.cfg("apiKey")
        .ok_or_else(|| anyhow::anyhow!("youtube 缺少 config.apiKey"))
}

pub(crate) async fn yt_get(path: &str, params: &[(&str, String)]) -> anyhow::Result<Value> {
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

pub(crate) async fn yt_popular(ch: &Channel, s: &Settings) -> anyhow::Result<Vec<VideoItem>> {
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

pub(crate) async fn yt_search(ch: &Channel, q: &str, _s: &Settings) -> anyhow::Result<Vec<VideoItem>> {
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

pub(crate) fn yt_item(x: &Value) -> Option<VideoItem> {
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

pub(crate) async fn yt_post_comment(
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

pub(crate) async fn yt_whoami(token: &str) -> anyhow::Result<String> {
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

pub(crate) fn as_u64(v: &Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .or_else(|| v.as_f64().map(|f| f as u64))
}

/// B 站搜索标题/简介里带 `<em class="keyword">` 高亮标签，展示前去掉标签保留文字。
pub(crate) fn strip_html(s: &str) -> String {
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

