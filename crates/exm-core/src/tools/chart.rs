//! tools/chart.rs —— 图表渲染工具（chart-rendering 任务 3.2）
//!
//! 链路：mermaid 源码 → POST {chart.endpoint}/mermaid/svg（Kroki 兼容，
//! 15s 超时）→ SVG 落盘 `.exmachina/outbox/` → outbox 登记 file 产物
//! （WebUI 可预览 SVG；IM 通道按注记/文件能力投递）。
//!
//! 门控：chart.endpoint 空串 = 关闭（工具不下发）；语法错误把服务端
//! 错误摘要带回模型（可修正后重试），不登记产物。

use super::*;

/// mermaid 源码护栏：图表源是检索单元不是文档——超长拒绝并引导精简
const MERMAID_CAP_CHARS: usize = 20_000;

impl ToolGateway {
    pub(crate) async fn tool_render_chart(&self, session_id: &str, args: &serde_json::Value) -> ToolResult {
        let mermaid = args
            .get("mermaid")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if mermaid.is_empty() {
            return ToolResult::err("mermaid 不能为空（图表源码，如 flowchart TD 开头）");
        }
        if mermaid.chars().count() > MERMAID_CAP_CHARS {
            return ToolResult::err(format!(
                "mermaid 源码过长（{} 字符 > {MERMAID_CAP_CHARS} 上限）：请精简图表结构",
                mermaid.chars().count()
            ));
        }
        let endpoint = self.chart_endpoint.read().clone();
        if endpoint.trim().is_empty() {
            return ToolResult::err("图表渲染未启用（chart.endpoint 未配置或为空）");
        }
        let url = format!("{}/mermaid/svg", endpoint.trim_end_matches('/'));
        let client = reqwest::Client::new();
        let resp = match tokio::time::timeout(
            std::time::Duration::from_secs(15),
            client.post(&url).body(mermaid.clone()).send(),
        )
        .await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return ToolResult::err(format!("渲染服务请求失败：{e}")),
            Err(_) => {
                return ToolResult::err("渲染服务超时（15 秒）：端点不可达或过载，可稍后重试")
            }
        };
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            // 400 = mermaid 语法错误：带回服务端摘要（模型可修正重试）
            let body = resp.text().await.unwrap_or_default();
            return ToolResult::err(format!(
                "渲染失败 {status}: {}",
                body.chars().take(300).collect::<String>()
            ));
        }
        let svg = match resp.bytes().await {
            Ok(b) => b.to_vec(),
            Err(e) => return ToolResult::err(format!("读取 SVG 失败：{e}")),
        };
        if svg.is_empty() {
            return ToolResult::err("渲染服务返回空内容");
        }
        // 轻校验：SVG 开头（防端点回 HTML 错误页）
        let head = String::from_utf8_lossy(&svg[..svg.len().min(256)]).to_lowercase();
        if !head.contains("<svg") && !head.contains("<?xml") {
            return ToolResult::err("渲染服务返回内容不是 SVG（可能配置了错误的 endpoint）");
        }
        // 落盘 + outbox 登记（kind=file；SVG 在 WebUI 可直接预览）
        let dir = self.workspace_root.join(".exmachina/outbox");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return ToolResult::err(format!("创建发件箱目录失败：{e}"));
        }
        let fname = format!("chart-{}.svg", &crate::types::new_id()[..8]);
        let abs = dir.join(&fname);
        if let Err(e) = std::fs::write(&abs, &svg) {
            return ToolResult::err(format!("SVG 落盘失败：{e}"));
        }
        let rel = format!(".exmachina/outbox/{fname}");
        let caption = format!("图表：{kib:.0} KB SVG（Kroki 渲染）", kib = svg.len() as f64 / 1024.0);
        if let Err(e) = self.store.outbox_push(session_id, "file", &rel, &caption) {
            return ToolResult::err(format!("发件箱登记失败：{e}"));
        }
        ToolResult::ok(format!(
            "图表已渲染并登记投递（{kib:.0} KB SVG，收束时随回复送达）：{fname}",
            kib = svg.len() as f64 / 1024.0
        ))
    }
}
