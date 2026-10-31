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
        } else if let Some(hint) = self.video_platform_hint(url) {
            ToolResult::ok(format!("{hint}\n\n{text}"))
        } else {
            ToolResult::ok(text)
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

    /// 联网搜索：调用配置的后端（tavily / brave / searxng / custom）。
    /// 未配置时该工具不会出现在下发 schema 里（此处仍兜底报错，防文本协议路径调用）。
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
        if !self.search.ready() {
            return ToolResult::err(
                "联网搜索未配置：请到「设置 → 联网搜索」选择后端并填写凭据（tavily / brave / searxng / custom）。",
            );
        }
        let provider = self.search.provider.trim().to_lowercase();
        let max = args
            .get("maxResults")
            .and_then(|v| v.as_u64())
            .map(|n| n.clamp(1, 20) as usize)
            .unwrap_or(self.search.max_results.clamp(1, 20));
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
        {
            Ok(c) => c,
            Err(e) => return ToolResult::err(format!("HTTP 客户端构建失败: {e}")),
        };
        let (endpoint, method, payload, bearer) = match provider.as_str() {
            "tavily" => {
                let ep = if self.search.endpoint.trim().is_empty() {
                    "https://api.tavily.com/search".to_string()
                } else {
                    self.search.endpoint.trim().to_string()
                };
                (
                    ep,
                    "POST",
                    serde_json::json!({ "api_key": self.search.api_key.trim(), "query": query, "max_results": max }),
                    None,
                )
            }
            "brave" => {
                let ep = if self.search.endpoint.trim().is_empty() {
                    format!(
                        "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
                        urlencode(&query),
                        max
                    )
                } else {
                    format!(
                        "{}?q={}&count={}",
                        self.search.endpoint.trim().trim_end_matches('/'),
                        urlencode(&query),
                        max
                    )
                };
                (
                    ep,
                    "GET",
                    serde_json::Value::Null,
                    Some(self.search.api_key.trim().to_string()),
                )
            }
            "searxng" => {
                let ep = format!(
                    "{}/search?q={}&format=json",
                    self.search.endpoint.trim().trim_end_matches('/'),
                    urlencode(&query)
                );
                (ep, "GET", serde_json::Value::Null, None)
            }
            _ => {
                // custom：POST {query, maxResults}，响应兼容多种字段名
                (
                    self.search.endpoint.trim().to_string(),
                    "POST",
                    serde_json::json!({ "query": query, "maxResults": max, "apiKey": self.search.api_key.trim() }),
                    None,
                )
            }
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
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => return ToolResult::err(format!("搜索请求失败: {e}")),
        };
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        if !(200..300).contains(&status) {
            return ToolResult::err(format!(
                "搜索后端返回 HTTP {status}：{}",
                body.chars().take(300).collect::<String>()
            ));
        }
        let parsed: serde_json::Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(e) => return ToolResult::err(format!("搜索响应不是 JSON: {e}")),
        };
        let items = parsed
            .get("results")
            .or_else(|| parsed.get("web").and_then(|w| w.get("results")))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if items.is_empty() {
            return ToolResult::ok(format!("无搜索结果（query: {query}）"));
        }
        let mut lines: Vec<String> = Vec::new();
        for (i, it) in items.iter().take(max).enumerate() {
            let title = it
                .get("title")
                .or_else(|| it.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("（无标题）");
            let url = it
                .get("url")
                .or_else(|| it.get("link"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let snippet = it
                .get("content")
                .or_else(|| it.get("snippet"))
                .or_else(|| it.get("description"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            lines.push(format!(
                "{}. {title}\n   {url}\n   {}",
                i + 1,
                snippet
                    .replace('\n', " ")
                    .chars()
                    .take(300)
                    .collect::<String>()
            ));
        }
        ToolResult::ok(format!(
            "搜索「{query}」（{}）：\n{}",
            self.search.provider.trim(),
            lines.join("\n")
        ))
    }
}
