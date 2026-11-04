//! tools/browser.rs —— 自 tools.rs 按领域拆出（纯搬移，零行为变更）

use super::*;

impl ToolGateway {
    /// 浏览器自动化（headless Chrome + CDP）：懒启动常驻，串行化访问。
    /// 探测不到浏览器时该工具不会出现在下发 schema 里（此处兜底报错）。
    pub(crate) async fn tool_browser(&self, session_id: &str, args: &serde_json::Value) -> ToolResult {
        let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("open");
        if op == "close" {
            let mut guard = self.browser.lock().await;
            return match guard.take() {
                Some(mut s) => {
                    s.shutdown();
                    ToolResult::ok("浏览器已关闭")
                }
                None => ToolResult::ok("浏览器未启动"),
            };
        }
        let mut guard = self.browser.lock().await;
        if guard.is_none() {
            match crate::browser::BrowserSession::launch(&self.browser_cfg, &self.workspace_root)
                .await
            {
                Ok(s) => *guard = Some(s),
                Err(e) => return ToolResult::err(format!("启动浏览器失败：{e}")),
            }
        }
        let session = match guard.as_mut() {
            Some(s) => s,
            None => return ToolResult::err("浏览器会话不可用"),
        };
        let max_default = self.browser_cfg.max_chars;
        let max = args
            .get("maxChars")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(max_default);
        match op {
            "open" => {
                let url = args
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if !(url.starts_with("http://")
                    || url.starts_with("https://")
                    || url.starts_with("file://")
                    || url.starts_with("about:"))
                {
                    return ToolResult::err("url 需以 http(s):// 开头（本地文件用 file://）");
                }
                if let Err(e) = session.navigate(url).await {
                    return ToolResult::err(format!("导航失败：{e}"));
                }
                match session.page_text(max).await {
                    Ok((title, text)) => {
                        ToolResult::ok(format!("已加载：{title}\nURL：{url}\n\n{text}"))
                    }
                    Err(e) => ToolResult::err(format!("读取页面失败：{e}")),
                }
            }
            "text" => match session.page_text(max).await {
                Ok((title, text)) => ToolResult::ok(format!("{title}\n\n{text}")),
                Err(e) => ToolResult::err(format!("读取页面失败：{e}")),
            },
            "eval" => {
                let expr = args
                    .get("expression")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if expr.trim().is_empty() {
                    return ToolResult::err("eval 需要 expression");
                }
                match session.eval(expr).await {
                    Ok(v) => ToolResult::ok(v.chars().take(8000).collect::<String>()),
                    Err(e) => ToolResult::err(format!("脚本执行失败：{e}")),
                }
            }
            "screenshot" => {
                let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
                match session.screenshot(&self.workspace_root, name).await {
                    Ok(rel) => {
                        // 产物登记（D12）：截图自动进入会话 outbox，通道投递
                        let _ = self
                            .store
                            .outbox_push(session_id, "image", &rel, "网页截图");
                        ToolResult::ok(format!("截图已保存：{rel}（可用 read 查看路径，或用 browser screenshot 再次覆盖）"))
                    }
                    Err(e) => ToolResult::err(format!("截图失败：{e}")),
                }
            }
            // 浏览器搜索（browser-search）：导航搜索引擎取渲染后结果——
            // 无需 API Key，与 search.* 后端互补（API 给结构化结果，这里给
            // 「真实网页所见」）。结果提取用引擎无关的启发式 JS（h2/h3 结果标题 +
            // 最近 a[href]），够用且不依赖引擎 DOM 结构细节。
            "search" => {
                let query = args
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if query.is_empty() {
                    return ToolResult::err("search 需要 query");
                }
                let engine = self.browser_cfg.search_engine.trim().to_lowercase();
                let url = match engine.as_str() {
                    "duckduckgo" | "ddg" => {
                        format!("https://duckduckgo.com/?q={}", urlencode(query))
                    }
                    "baidu" => format!("https://www.baidu.com/s?wd={}", urlencode(query)),
                    // 缺省 bing（国内可达、结果页无需登录）
                    _ => format!("https://www.bing.com/search?q={}", urlencode(query)),
                };
                if let Err(e) = session.navigate(&url).await {
                    return ToolResult::err(format!("搜索导航失败：{e}"));
                }
                // 结果渲染等待：导航完成 ≠ 结果就绪（Bing/DDG 结果由 JS 异步注入）。
                // 轮询「结果容器的标题」出现（页头导航 h2 不算），最多 ~5s；
                // 超时则尽力提取（启发式兜底会接住）。
                for _ in 0..10 {
                    let ready = session
                        .eval(
                            "document.querySelectorAll('#b_results h2, #b_results h3, \
                             #links .result__title, #content_left h3').length",
                        )
                        .await
                        .ok()
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if ready >= 1 {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                // 结果提取：引擎无关启发式——主内容区里的 h2/h3 标题 + 链接 + 摘要。
                // 注意 headless 下 innerText 可能为空（布局未渲染），用 textContent。
                let extract = r#"(function(){
                    var out = [];
                    var seen = {};
                    // Bing/DDG 结果标题多为 h2，百度为 h3；取容器块再找链接与摘要
                    var heads = document.querySelectorAll('#b_results h2, #b_results h3, #links .result__title, #content_left .result h3, #content_left .c-container h3');
                    for (var i = 0; i < heads.length && out.length < 12; i++) {
                        var h = heads[i];
                        var a = h.querySelector('a[href]') || (h.closest('li, .result, .c-container') || document).querySelector('a[href]');
                        if (!a) continue;
                        var href = a.href || '';
                        var title = ((a.textContent || h.textContent || '') + '').trim();
                        if (!href || !title || seen[href + title]) continue;
                        seen[href + title] = 1;
                        // 摘要：标题容器所在块的后续文本
                        var block = h.closest('li, .result, .c-container') || h.parentElement;
                        var text = block ? ((block.textContent || '') + '') : '';
                        // 压掉连续空白与空行
                        text = text.replace(title, '').replace(/\s+\n/g, '\n').replace(/\n{2,}/g, '\n').trim();
                        out.push({ title: title, url: href, snippet: text.slice(0, 200) });
                    }
                    return JSON.stringify(out);
                })()"#;
                let raw = match session.eval(extract).await {
                    Ok(v) => v,
                    Err(e) => return ToolResult::err(format!("结果提取失败：{e}")),
                };
                let parsed: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap_or_default();
                if parsed.is_empty() {
                    // 兜底：启发式没抓到（引擎改版/页面结构变化）——回整页正文让模型自己读
                    let (title, text) = session.page_text(max).await.unwrap_or_default();
                    return ToolResult::ok(format!(
                        "搜索已完成（结构化提取未命中，回退整页正文）\n页面：{title}\n\n{text}"
                    ));
                }
                let mut lines = String::new();
                for (i, it) in parsed.iter().take(10).enumerate() {
                    lines.push_str(&format!(
                        "{}. {}\n   {}\n   {}\n",
                        i + 1,
                        it.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                        it.get("url").and_then(|v| v.as_str()).unwrap_or(""),
                        it.get("snippet").and_then(|v| v.as_str()).unwrap_or("")
                    ));
                }
                ToolResult::ok(format!(
                    "搜索「{query}」（{engine}）结果：\n\n{lines}\n提示：需要某条的完整内容用 browser open 打开其 URL。"
                ))
            }
            other => ToolResult::err(format!(
                "未知 browser 操作: {other}（支持 open / search / text / eval / screenshot / close）"
            )),
        }
    }
}
