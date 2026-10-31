//! tools/todo.rs —— 自 tools.rs 按领域拆出（纯搬移，零行为变更）

use super::*;

impl ToolGateway {
    /// 只读语义面（不进审批闸门）；write 后发 todo.updated 事件。
    pub(crate) fn tool_todo(&self, session_id: &str, args: &serde_json::Value) -> ToolResult {
        let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("write");
        match op {
            "list" => {
                let items = crate::todo::list_items(session_id);
                if items.is_empty() {
                    return ToolResult::ok("当前会话无任务清单。");
                }
                let done = items.iter().filter(|t| t.status == "done").count();
                let body = items
                    .iter()
                    .map(|t| {
                        let mark = match t.status.as_str() {
                            "done" => "✅",
                            "in_progress" => "🔄",
                            _ => "⬜",
                        };
                        format!("- {mark} {}", t.content)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                ToolResult::ok(format!("任务清单（{done}/{} 完成）：\n{body}", items.len()))
            }
            "write" => {
                let Some(arr) = args.get("items").and_then(|v| v.as_array()) else {
                    return ToolResult::err("op=write 需要 items 数组（全量条目；空数组 = 清空）");
                };
                let mut parsed: Vec<(String, String)> = Vec::with_capacity(arr.len());
                for (i, it) in arr.iter().enumerate() {
                    let content = it
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if content.is_empty() {
                        return ToolResult::err(format!(
                            "第 {} 条 content 为空（每条必须有内容；空 items 清单请传空数组）",
                            i + 1
                        ));
                    }
                    let status = it
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("pending")
                        .to_string();
                    parsed.push((content, status));
                }
                let items = crate::todo::write(session_id, parsed);
                let done = items.iter().filter(|t| t.status == "done").count();
                let all = items.is_empty() || done == items.len();
                self.emit_and_persist(CoreEvent {
                    kind: "todo.updated".into(),
                    session_id: session_id.to_string(),
                    payload: serde_json::json!({
                        "count": items.len(),
                        "doneCount": done,
                        "allDone": all,
                        "items": items,
                    }),
                });
                if items.is_empty() {
                    return ToolResult::ok("清单已清空。");
                }
                ToolResult::ok(format!(
                    "清单已更新（{done}/{} 完成）。\n后续每轮上下文都会携带当前清单；全部完成后建议收尾总结。",
                    items.len()
                ))
            }
            other => ToolResult::err(format!("未知 op：{other}（支持 write/list）")),
        }
    }
}
