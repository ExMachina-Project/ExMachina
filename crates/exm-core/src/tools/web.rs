//! tools/web.rs —— 自 tools.rs 按领域拆出（纯搬移，零行为变更）

use super::*;

/// 极简 HTML 正文提取：剥 script/style 块与标签、压缩空白（零正则依赖）。
/// 全程字符级推进（Vec<char> 索引）：lower 与原文同长对齐，杜绝字节级切片
/// 落进多字节字符内部的 panic（部署实测旧实现 15177 字节边界崩溃）。
pub(crate) fn strip_html(html: &str) -> String {
    let src: Vec<char> = html.chars().collect();
    // 小写化按字符逐个 to_lowercase 首字符（标签判定只涉及 ASCII，此处保语义即可）
    let lower: Vec<char> = src
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    let starts_with = |from: usize, pat: &str| -> bool {
        let pcs: Vec<char> = pat.chars().collect();
        lower.len() >= from + pcs.len() && lower[from..from + pcs.len()].iter().eq(pcs.iter())
    };
    let mut out = String::with_capacity(src.len() / 2);
    let mut i = 0usize;
    let mut skip_until: Option<&'static str> = None;
    while i < src.len() {
        if let Some(end) = skip_until {
            if starts_with(i, end) {
                skip_until = None;
                i += end.chars().count();
                // 吞掉闭合标签的剩余部分（到 '>'）
                while i < src.len() && src[i] != '>' {
                    i += 1;
                }
                i = (i + 1).min(src.len());
            } else {
                i += 1;
            }
            continue;
        }
        if starts_with(i, "<script") {
            skip_until = Some("</script");
        } else if starts_with(i, "<style") {
            skip_until = Some("</style");
        } else if src[i] == '<' {
            while i < src.len() && src[i] != '>' {
                i += 1;
            }
        } else {
            let ch = src[i];
            out.push(if ch.is_whitespace() { ' ' } else { ch });
            i += 1;
        }
    }
    let mut compact = String::with_capacity(out.len());
    let mut last_space = true;
    for c in out.chars() {
        if c == ' ' {
            if !last_space {
                compact.push(' ');
            }
            last_space = true;
        } else {
            compact.push(c);
            last_space = false;
        }
    }
    compact
}
impl ToolGateway {
    /// 抓取网页并提取正文（大小/时长受限，标签与脚本剥离）
    pub(crate) async fn tool_web_fetch(&self, args: &serde_json::Value) -> ToolResult {
        let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
        let max_chars = args
            .get("maxChars")
            .and_then(|v| v.as_u64())
            .unwrap_or(8000) as usize;
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return ToolResult::err("url 必须以 http(s):// 开头");
        }
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
        {
            Ok(c) => c,
            Err(e) => return ToolResult::err(format!("HTTP 客户端构建失败: {e}")),
        };
        let resp = match client.get(url).send().await {
            Ok(r) => r,
            Err(e) => return ToolResult::err(format!("抓取失败: {e}")),
        };
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return ToolResult::err(format!("HTTP {status}"));
        }
        // 截断到 2MB 以内再读
        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => return ToolResult::err(format!("读取响应失败: {e}")),
        };
        let raw = String::from_utf8_lossy(&bytes[..bytes.len().min(2 * 1024 * 1024)]);
        let text = strip_html(&raw);
        let text = text.trim().chars().take(max_chars).collect::<String>();
        if text.is_empty() {
            ToolResult::err("页面无可提取文本（可能是纯二进制或脚本渲染页）")
        } else {
            // 外部内容隔离包装（inbound-sanitization）：数据而非指令边界 + 截断标注
            let wrapped = external_wrap::wrap_external("网页", url, &text, max_chars);
            if let Some(hint) = self.video_platform_hint(url) {
                ToolResult::ok(format!("{hint}\n\n{wrapped}"))
            } else {
                ToolResult::ok(wrapped)
            }
        }
    }

    /// 视频平台 URL 判定（web_fetch 引导提示用）
    pub(crate) fn video_platform_of_url(url: &str) -> Option<&'static str> {
        if url.contains("bilibili.com") || url.contains("b23.tv") {
            Some("bilibili")
        } else if url.contains("youtube.com") || url.contains("youtu.be") {
            Some("youtube")
        } else {
            None
        }
    }

    /// web_fetch 抓到视频平台 URL 时的引导提示：
    /// · 有 video__act（视频通道已启用且工具已注册）→ 指路专用工具（看视频/发评论的正道）
    /// · 没有 → 明确告知当前无法实际评论，禁止把评论文本写进回复冒充已发
    pub(crate) fn video_platform_hint(&self, url: &str) -> Option<String> {
        let platform = Self::video_platform_of_url(url)?;
        let has_act = self.dynamic_tool_names().iter().any(|n| n == "video__act");
        if has_act {
            Some(format!(
                "[提示] 这是{platform}视频页：web_fetch 只能取元信息。看视频/发评论请用 \
                 video__* 工具组（video__summary/字幕/弹幕/评论/UP主，video__act 发评论），\
                 走登录态与风控养护，评论才会真实上屏。"
            ))
        } else {
            Some(format!(
                "[提示] 这是{platform}视频页，且当前系统未启用{platform}通道——\
                 没有可用的 video__* 工具。你可以继续用 web_fetch 了解视频信息，但\
                 **无法实际发表评论**：请如实告知用户「当前无法评论（需在控制台启用 \
                 {platform} 通道并配置凭据）」，不要把评论内容写进回复里冒充已发布。"
            ))
        }
    }

    /// 联网搜索（search-aggregation）：多后端并发聚合。
    /// 单后端 = 旧行为等价；多后端并发查询（10s/后端超时护栏），URL 规范化去重，
    /// 按「后端权重 × 排名倒数」合排，截 maxResults；includeContent=true 时
    /// 前 N 条并发抓正文（8s/页）。未配置时该工具不下发（此处兜底报错防文本协议调用）。
    pub(crate) async fn tool_web_search(&self, args: &serde_json::Value) -> ToolResult {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if query.is_empty() {
            return ToolResult::err("query 不能为空");
        }
        let backends = self.search.effective_backends();
        let active: Vec<_> = backends.iter().filter(|b| b.ready()).collect();
        if active.is_empty() {
            return ToolResult::err(
                "联网搜索未配置：请到「设置 → 联网搜索」配置后端凭据（tavily / brave / searxng / custom）。",
            );
        }
        let max = args
            .get("maxResults")
            .and_then(|v| v.as_u64())
            .map(|n| n.clamp(1, 20) as usize)
            .unwrap_or(self.search.max_results.clamp(1, 20));
        let include_content = args
            .get("includeContent")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
        {
            Ok(c) => c,
            Err(e) => return ToolResult::err(format!("HTTP 客户端构建失败: {e}")),
        };
        // 并发查询全部后端（每后端 10s 超时护栏：单后端悬挂不阻塞整体）
        let query_ref = &query;
        let queries = active.iter().map(|b| {
            let client = &client;
            async move {
                let r = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    search_one_backend(client, b, query_ref, max),
                )
                .await;
                match r {
                    Ok(Ok(items)) => Ok((b, items)),
                    Ok(Err(e)) => Err((b.weight, e)),
                    Err(_) => Err((b.weight, anyhow::anyhow!("后端超时（>10s）"))),
                }
            }
        });
        let results = futures_util::future::join_all(queries).await;
        let (ok_parts, failed): (Vec<_>, Vec<_>) =
            results.into_iter().partition(|r| r.is_ok());
        if ok_parts.is_empty() {
            let msg = failed
                .first()
                .and_then(|r| r.as_ref().err())
                .map(|(_, e)| e.to_string())
                .unwrap_or_else(|| "未知错误".to_string());
            return ToolResult::err(format!("搜索请求失败（全部后端不可用）：{msg}"));
        }
        let mut agg = AggregateResults::new();
        let mut ok_names: Vec<&str> = Vec::new();
        for part in ok_parts {
            let (backend, items) = part.expect("已过滤 Err");
            ok_names.push(backend.provider.trim());
            for (rank, item) in items.into_iter().enumerate() {
                agg.push(item, backend, rank);
            }
        }
        let ranked = agg.ranked(max);
        if ranked.is_empty() {
            return ToolResult::ok(format!("无搜索结果（query: {query}）"));
        }
        // includeContent：前 N 条并发抓正文（8s/页；失败留空不影响整体）
        let mut contents: Vec<Option<String>> = vec![None; ranked.len()];
        if include_content {
            let n = self.search.content_fetch_n.clamp(0, 5).min(ranked.len());
            let fetches = ranked.iter().take(n).map(|it| async {
                fetch_content_window(&client, &it.url, &query).await.ok()
            });
            let got = futures_util::future::join_all(fetches).await;
            for (i, c) in got.into_iter().enumerate() {
                contents[i] = c;
            }
        }
        // 渲染（保持旧格式兼容：编号 + 标题 + URL + 摘要；追加 content/aggregatedFrom 可选段）
        let mut lines: Vec<String> = Vec::new();
        for (i, item) in ranked.iter().enumerate() {
            let mut line = format!(
                "{}. {}\n   {}\n   {}",
                i + 1,
                item.title,
                item.url,
                item.snippet.replace('\n', " ").chars().take(300).collect::<String>()
            );
            if item.sources.len() > 1 {
                line.push_str(&format!("\n   〔聚合命中：{}〕", item.sources.join(" + ")));
            }
            if let Some(Some(c)) = contents.get(i) {
                line.push_str(&format!("\n   〔正文〕{c}"));
            }
            lines.push(line);
        }
        let source_note = if active.len() > 1 {
            let fail_note = if failed.is_empty() {
                String::new()
            } else {
                format!("（{} 个后端失败/超时已丢弃）", failed.len())
            };
            format!("{} 个后端聚合：{}{fail_note}", ok_names.len(), ok_names.join(" + "))
        } else {
            active[0].provider.trim().to_string()
        };
        ToolResult::ok(format!("搜索「{query}」（{source_note}）：\n{}", lines.join("\n")))
    }
}

// ---------------------------------------------------------------- 搜索聚合内部实现

/// 单条聚合结果（多源同 URL 合并：sources 并列，位置取高分）
struct AggItem {
    title: String,
    url: String,
    snippet: String,
    sources: Vec<String>,
    score: f32,
}

/// 后端原始结果条目（统一字段名后的中间形态）
struct RawItem {
    title: String,
    url: String,
    snippet: String,
}

/// 单后端查询（统一各后端协议 → RawItem 列表）
async fn search_one_backend(
    client: &reqwest::Client,
    b: &crate::config::SearchBackend,
    query: &str,
    max: usize,
) -> anyhow::Result<Vec<RawItem>> {
    let provider = b.provider.trim().to_lowercase();
    let (endpoint, method, payload, bearer) = match provider.as_str() {
        "tavily" => {
            let ep = if b.endpoint.trim().is_empty() {
                "https://api.tavily.com/search".to_string()
            } else {
                b.endpoint.trim().to_string()
            };
            (
                ep,
                "POST",
                serde_json::json!({ "api_key": b.api_key.trim(), "query": query, "max_results": max }),
                None,
            )
        }
        "brave" => {
            let ep = if b.endpoint.trim().is_empty() {
                format!(
                    "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
                    urlencode(query),
                    max
                )
            } else {
                format!(
                    "{}?q={}&count={}",
                    b.endpoint.trim().trim_end_matches('/'),
                    urlencode(query),
                    max
                )
            };
            (ep, "GET", serde_json::Value::Null, Some(b.api_key.trim().to_string()))
        }
        "searxng" => (
            format!(
                "{}/search?q={}&format=json",
                b.endpoint.trim().trim_end_matches('/'),
                urlencode(query)
            ),
            "GET",
            serde_json::Value::Null,
            None,
        ),
        _ => (
            b.endpoint.trim().to_string(),
            "POST",
            serde_json::json!({ "query": query, "maxResults": max, "apiKey": b.api_key.trim() }),
            None,
        ),
    };
    let mut req = if method == "POST" {
        client.post(&endpoint)
    } else {
        client.get(&endpoint)
    };
    if method == "POST" {
        req = req.json(&payload);
    }
    if let Some(tok) = bearer {
        if !tok.is_empty() {
            req = req.header("X-Subscription-Token", tok);
        }
    }
    let resp = req
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("搜索请求失败: {e}"))?;
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        anyhow::bail!(
            "搜索后端返回 HTTP {status}：{}",
            body.chars().take(300).collect::<String>()
        );
    }
    let parsed: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| anyhow::anyhow!("搜索响应不是 JSON: {e}"))?;
    let items = parsed
        .get("results")
        .or_else(|| parsed.get("web").and_then(|w| w.get("results")))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(items
        .into_iter()
        .take(max)
        .map(|it| RawItem {
            title: it
                .get("title")
                .or_else(|| it.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("（无标题）")
                .to_string(),
            url: it
                .get("url")
                .or_else(|| it.get("link"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            snippet: it
                .get("content")
                .or_else(|| it.get("snippet"))
                .or_else(|| it.get("description"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        })
        .collect())
}

/// URL 规范化（去重键）：去 utm_*/fbclid 等追踪参数与 fragment、host 小写、去尾斜杠
fn normalize_url(url: &str) -> String {
    let (scheme_rest, rest) = match url.split_once("://") {
        Some((s, r)) => (s.to_lowercase(), r),
        None => return url.trim().to_lowercase(),
    };
    let (host_port, path_query) = match rest.split_once('/') {
        Some((h, p)) => (h.to_lowercase(), format!("/{p}")),
        None => (rest.to_lowercase(), "/".to_string()),
    };
    let (path, query) = match path_query.split_once('?') {
        Some((p, q)) => (p.to_string(), Some(q.to_string())),
        None => (path_query, None),
    };
    // 去 fragment
    let path = path.split('#').next().unwrap_or(&path).to_string();
    let tracking = ["utm_", "fbclid", "gclid", "msclkid", "ref_", "spm", "share_source", "vd_source"];
    let kept: Vec<String> = match query {
        None => Vec::new(),
        Some(q) => q
            .split('&')
            .filter(|kv| {
                let key = kv.split('=').next().unwrap_or("");
                let kl = key.to_lowercase();
                !tracking.iter().any(|t| kl.starts_with(t))
            })
            .map(|s| s.to_string())
            .collect(),
    };
    let mut norm = format!("{scheme_rest}://{host_port}");
    let path = path.trim_end_matches('/');
    norm.push_str(if path.is_empty() { "/" } else { path });
    if !kept.is_empty() {
        norm.push('?');
        norm.push_str(&kept.join("&"));
    }
    norm
}

/// 聚合器：URL 规范化去重 + 权重合排（权重 × 排名倒数）
struct AggregateResults {
    items: Vec<AggItem>,
    index: std::collections::HashMap<String, usize>,
}

impl AggregateResults {
    fn new() -> Self {
        AggregateResults { items: Vec::new(), index: std::collections::HashMap::new() }
    }

    fn push(&mut self, raw: RawItem, backend: &crate::config::SearchBackend, rank: usize) {
        if raw.url.is_empty() {
            return;
        }
        let key = normalize_url(&raw.url);
        let score = backend.weight * (1.0 / (rank as f32 + 1.0));
        // 来源指纹：provider + endpoint（同 provider 多实例各自计源；同实例同 URL 只计一次）
        let source = if backend.endpoint.trim().is_empty() {
            backend.provider.trim().to_string()
        } else {
            format!("{}@{}", backend.provider.trim(), normalize_url(backend.endpoint.trim()))
        };
        match self.index.get(&key) {
            Some(&i) => {
                // 同 URL 合并：来源并列、摘要取首见、分数取 max 后加成（多源印证）
                let it = &mut self.items[i];
                if !it.sources.contains(&source) {
                    it.sources.push(source.clone());
                    it.score += score * 0.5; // 多源印证加成
                }
                it.score = it.score.max(score);
            }
            None => {
                self.index.insert(key, self.items.len());
                self.items.push(AggItem {
                    title: raw.title,
                    url: raw.url,
                    snippet: raw.snippet,
                    sources: vec![source],
                    score,
                });
            }
        }
    }

    fn ranked(mut self, max: usize) -> Vec<AggItem> {
        self.items.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        self.items.truncate(max);
        self.items
    }
}

/// 正文抓取（includeContent）：取回页面 → strip_html → 查询词定位窗口（±200 字符）→ 截 1000
/// 复用 external_wrap 的来源标注边界（数据而非指令）
async fn fetch_content_window(
    client: &reqwest::Client,
    url: &str,
    query: &str,
) -> anyhow::Result<String> {
    let resp = tokio::time::timeout(std::time::Duration::from_secs(8), client.get(url).send())
        .await
        .map_err(|_| anyhow::anyhow!("抓取超时"))?
        .map_err(|e| anyhow::anyhow!("抓取失败: {e}"))?;
    if !resp.status().is_success() {
        anyhow::bail!("HTTP {}", resp.status());
    }
    let bytes = resp.bytes().await.map_err(|e| anyhow::anyhow!("读取失败: {e}"))?;
    let raw = String::from_utf8_lossy(&bytes[..bytes.len().min(2 * 1024 * 1024)]);
    let text = strip_html(&raw);
    // 查询词窗口：任一词命中即取其前后 ±200 字符（未命中取开头 400）
    let chars: Vec<char> = text.chars().collect();
    let terms: Vec<String> = query
        .split_whitespace()
        .filter(|t| t.chars().count() >= 2)
        .map(|t| t.to_lowercase())
        .collect();
    let lower: Vec<char> = text.to_lowercase().chars().collect();
    let mut hit: Option<usize> = None;
    for t in &terms {
        let tc: Vec<char> = t.chars().collect();
        'outer: for start in 0..lower.len().saturating_sub(tc.len()) {
            if lower[start..start + tc.len()] == tc[..] {
                hit = Some(start);
                break 'outer;
            }
        }
        if hit.is_some() {
            break;
        }
    }
    let (from, len) = match hit {
        Some(h) => (h.saturating_sub(200), 1000),
        None => (0, 400),
    };
    let window: String = chars.iter().skip(from).take(len).collect();
    if window.trim().is_empty() {
        anyhow::bail!("无可提取文本");
    }
    let total = chars.len();
    let trunc = if from + window.chars().count() < total {
        format!("…〔截断：正文 {total} 字符，取关键段〕")
    } else {
        String::new()
    };
    Ok(format!("{window}{trunc}"))
}

// ---------------------------------------------------------------- 单测（search-aggregation）

#[cfg(test)]
mod search_agg_tests {
    use super::*;
    use crate::config::{SearchBackend, SearchConfig};

    fn backend(provider: &str, weight: f32) -> SearchBackend {
        SearchBackend {
            provider: provider.into(),
            endpoint: String::new(),
            api_key: "k".into(),
            weight,
        }
    }

    fn raw(url: &str, title: &str) -> RawItem {
        RawItem {
            title: title.into(),
            url: url.into(),
            snippet: format!("{title} 摘要"),
        }
    }

    /// URL 规范化：追踪参数剥离、fragment 剥离、host 小写、尾斜杠归一
    #[test]
    fn url_规范化_追踪参数与形态() {
        assert_eq!(
            normalize_url("https://Example.com/a/?utm_source=x&fbclid=yyy"),
            "https://example.com/a"
        );
        assert_eq!(
            normalize_url("https://example.com/a#section"),
            "https://example.com/a"
        );
        // 有效查询参数保留
        assert_eq!(
            normalize_url("https://example.com/search?q=rust&utm_medium=social"),
            "https://example.com/search?q=rust"
        );
        // 同 URL 不同追踪形态归一到同键
        assert_eq!(
            normalize_url("https://example.com/a?utm_source=1"),
            normalize_url("https://example.com/a/")
        );
    }

    /// 聚合去重：同 URL（规范化后）合并来源、多源并列标注
    #[test]
    fn 聚合_同url多源合并() {
        let mut agg = AggregateResults::new();
        agg.push(raw("https://a.com/x?utm_source=tt", "A"), &backend("tavily", 1.0), 0);
        agg.push(raw("https://a.com/x", "A"), &backend("brave", 1.0), 1);
        agg.push(raw("https://b.com/y", "B"), &backend("tavily", 1.0), 1);
        let ranked = agg.ranked(10);
        assert_eq!(ranked.len(), 2, "同 URL 应去重为一条");
        let a = ranked.iter().find(|i| i.url.contains("a.com")).unwrap();
        assert_eq!(a.sources, vec!["tavily".to_string(), "brave".to_string()], "来源并列");
    }

    /// 权重合排：高权重后端的同排名结果排前
    #[test]
    fn 聚合_权重合排() {
        let mut agg = AggregateResults::new();
        agg.push(raw("https://low.com/", "低权重第一"), &backend("searxng", 0.5), 0);
        agg.push(raw("https://high.com/", "高权重第二"), &backend("tavily", 2.0), 1);
        let ranked = agg.ranked(10);
        assert_eq!(ranked[0].url, "https://high.com/", "高权重的 rank1 应超过低权重 rank0：0.5 vs 1.0");
    }

    /// 条数上限截断
    #[test]
    fn 聚合_截断上限() {
        let mut agg = AggregateResults::new();
        for i in 0..8 {
            agg.push(raw(&format!("https://x.com/{i}"), &format!("T{i}")), &backend("tavily", 1.0), i);
        }
        assert_eq!(agg.ranked(5).len(), 5);
    }

    /// 旧配置迁移：平铺字段 → effective_backends 单元素数组；新数组优先
    #[test]
    fn 配置_旧形态迁移() {
        let mut cfg = SearchConfig::default();
        assert!(!cfg.ready(), "零配置不下发");
        assert!(cfg.effective_backends().is_empty(), "零配置无后端");
        // 旧平铺形态
        cfg.provider = "tavily".into();
        cfg.api_key = "k".into();
        let eff = cfg.effective_backends();
        assert_eq!(eff.len(), 1, "旧形态迁移为单元素数组");
        assert_eq!(eff[0].provider, "tavily");
        assert!(cfg.ready(), "旧形态 ready 语义保持");
        // 新数组优先于旧平铺
        cfg.backends = vec![backend("brave", 1.0)];
        assert_eq!(cfg.effective_backends()[0].provider, "brave", "backends 非空时优先");
    }

    /// serde 兼容：旧 JSON（无 backends 字段）可反序列化
    #[test]
    fn 配置_旧json反序列化() {
        let legacy = serde_json::json!({
            "provider": "brave",
            "apiKey": "secret",
            "maxResults": 8,
            "proactive": true
        });
        let cfg: SearchConfig = serde_json::from_value(legacy).expect("旧 JSON 必须兼容");
        assert_eq!(cfg.provider, "brave");
        assert_eq!(cfg.max_results, 8);
        assert!(cfg.backends.is_empty());
        assert_eq!(cfg.effective_backends().len(), 1);
        // 新形态
        let modern = serde_json::json!({
            "provider": "",
            "backends": [
                { "provider": "tavily", "apiKey": "a", "weight": 1.5 },
                { "provider": "brave", "apiKey": "b" }
            ],
            "maxResults": 5,
            "contentFetchN": 2
        });
        let cfg: SearchConfig = serde_json::from_value(modern).expect("新 JSON 解析");
        assert_eq!(cfg.effective_backends().len(), 2);
        assert!((cfg.effective_backends()[0].weight - 1.5).abs() < 1e-6);
        assert_eq!(cfg.content_fetch_n, 2);
        assert!(cfg.ready(), "双后端任一就绪即下发");
    }
}

#[cfg(test)]
mod search_agg_e2e_tests {
    use super::*;
    use crate::config::{SearchBackend, SearchConfig};
    use std::sync::Arc;

    /// 起 mock 搜索后端（axum）：/search 返回固定结果；/slow 挂 15s（超时护栏用）
    async fn spawn_backend(mode: &'static str) -> (String, tokio::task::JoinHandle<()>) {
        let app = axum::Router::new().route(
            "/search",
            axum::routing::get(move || async move {
                match mode {
                    "a" => {
                        // 后端 A：两条结果（与后端 B 一条同 URL —— 聚合去重样例）
                        axum::Json(serde_json::json!({
                            "results": [
                                { "title": "共同结果", "url": "https://shared.com/doc", "content": "A 与 B 都返回的页面" },
                                { "title": "A 独有", "url": "https://a-only.com/x", "content": "只有 A 有" }
                            ]
                        }))
                    }
                    "b" => axum::Json(serde_json::json!({
                        "results": [
                            { "title": "共同结果", "url": "https://shared.com/doc", "content": "同 URL 不同摘要" },
                            { "title": "B 独有", "url": "https://b-only.com/y", "content": "只有 B 有" }
                        ]
                    })),
                    "slow" => {
                        tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                        axum::Json(serde_json::json!({ "results": [] }))
                    }
                    _ => axum::Json(serde_json::json!({ "results": [] })),
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), handle)
    }

    fn gw_with(backends: Vec<SearchBackend>) -> ToolGateway {
        let dir = std::env::temp_dir().join(format!("exm-sa-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(crate::store::Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(crate::registry::LocalRegistry::new(dir.join("agents")).unwrap());
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let search = SearchConfig {
            backends,
            ..Default::default()
        };
        ToolGateway::new(&dir, store, registry, events, Default::default()).with_search(search)
    }

    /// 双后端聚合：并发查询、同 URL 去重（来源并列）、独有结果都保留
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 双后端_聚合并去重() {
        let (ep_a, _ha) = spawn_backend("a").await;
        let (ep_b, _hb) = spawn_backend("b").await;
        let gw = gw_with(vec![
            SearchBackend { provider: "searxng".into(), endpoint: ep_a, api_key: String::new(), weight: 1.0 },
            SearchBackend { provider: "searxng".into(), endpoint: ep_b.clone(), api_key: String::new(), weight: 1.0 },
        ]);
        let out = gw
            .tool_web_search(&serde_json::json!({ "query": "聚合测试", "maxResults": 10 }))
            .await;
        let text = out.output.clone();
        assert!(out.ok, "聚合应成功：{text}");
        // 同 URL 只出现一次（链接在文本中出现 1 次而非 2 次）
        assert_eq!(text.matches("shared.com/doc").count(), 1, "同 URL 去重：{text}");
        assert!(text.contains("A 独有") && text.contains("B 独有"), "两后端独有结果都在：{text}");
        assert!(text.contains("聚合命中"), "多源并列标注：{text}");
        assert!(text.contains("2 个后端聚合"), "来源说明：{text}");
    }

    /// 超时护栏：慢后端（15s）不阻塞整体，快后端结果正常返回
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 超时护栏_慢后端不拖累() {
        let (ep_slow, _hs) = spawn_backend("slow").await;
        let (ep_b, _hb) = spawn_backend("b").await;
        let gw = gw_with(vec![
            SearchBackend { provider: "searxng".into(), endpoint: ep_slow, api_key: String::new(), weight: 1.0 },
            SearchBackend { provider: "searxng".into(), endpoint: ep_b.clone(), api_key: String::new(), weight: 1.0 },
        ]);
        let started = std::time::Instant::now();
        let out = gw
            .tool_web_search(&serde_json::json!({ "query": "护栏测试" }))
            .await;
        assert!(out.ok, "慢后端不影响整体：{}", out.output);
        assert!(started.elapsed() < std::time::Duration::from_secs(12), "10s 护栏内返回");
        assert!(out.output.contains("已丢弃"), "失败后端标注：{}", out.output);
        assert!(out.output.contains("B 独有"), "快后端结果完整");
    }

    /// 单后端降级：仅一个后端时行为与旧版一致（无聚合标注）
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 单后端_旧行为等价() {
        let (ep_a, _ha) = spawn_backend("a").await;
        let gw = gw_with(vec![SearchBackend {
            provider: "searxng".into(),
            endpoint: ep_a,
            api_key: String::new(),
            weight: 1.0,
        }]);
        let out = gw
            .tool_web_search(&serde_json::json!({ "query": "单后端" }))
            .await;
        assert!(out.ok);
        assert!(out.output.contains("A 独有"), "结果完整");
        assert!(!out.output.contains("后端聚合"), "单后端无聚合标注（旧格式）：{}", out.output);
    }

    /// 全部后端不可用 → 工具报错（不是空结果）
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 全后端失败_报错() {
        // 端口 1（保留地址）：连接立即失败
        let gw = gw_with(vec![SearchBackend {
            provider: "custom".into(),
            endpoint: "http://127.0.0.1:1/nope".into(),
            api_key: String::new(),
            weight: 1.0,
        }]);
        let out = gw
            .tool_web_search(&serde_json::json!({ "query": "失败路径" }))
            .await;
        assert!(!out.ok, "全后端失败应报错");
        let err = out.error.clone().unwrap_or_default();
        assert!(err.contains("全部后端不可用"), "错误口径：{err}");
    }
}
