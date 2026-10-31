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
            other => ToolResult::err(format!(
                "未知 browser 操作: {other}（支持 open / text / eval / screenshot / close）"
            )),
        }
    }
}
