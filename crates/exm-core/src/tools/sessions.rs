//! tools/sessions.rs —— 跨会话互通（同组范围）
//!
//! 让智能体在组内「看见另一条会话、读取它的上下文、把任务投递过去」：
//! - list / search / history 是只读面；
//! - send 经 Core 回接的投递执行器触发目标会话新一轮（默认异步，wait=true 等待完成）。
//!
//! 安全边界：组隔离在工具层强制——目标会话必须与当前会话同 group_id 且同
//! memory_partition；跨组/跨分区一律拒绝（不暴露存在性）。

use super::*;
use crate::types::Session;

impl ToolGateway {
    fn same_scope(a: &Session, b: &Session) -> bool {
        a.group_id == b.group_id && a.memory_partition == b.memory_partition
    }

    fn scope_denied() -> ToolResult {
        ToolResult::err("目标会话不在当前组的作用域内（跨组/跨记忆分区不可见）")
    }

    pub(crate) async fn tool_sessions(
        &self,
        agent_id: &str,
        session_id: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        let Some(current) = self.store.get_session(session_id).ok().flatten() else {
            return ToolResult::err("当前会话不存在");
        };
        let op = args
            .get("op")
            .and_then(|v| v.as_str())
            .unwrap_or("list")
            .trim();
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20)
            .clamp(1, 100) as usize;

        match op {
            "list" => {
                let list = match self.store.list_sessions_in_group(&current.group_id) {
                    Ok(l) => l,
                    Err(e) => return ToolResult::err(format!("会话列表读取失败: {e}")),
                };
                let items: Vec<serde_json::Value> = list
                    .iter()
                    .filter(|s| Self::same_scope(&current, s))
                    .take(limit)
                    .map(|s| {
                        serde_json::json!({
                            "sessionId": s.id,
                            "title": s.title,
                            "status": s.status,
                            "current": s.id == current.id,
                            "pinned": s.pinned,
                            "messageCount": self.store.message_count(&s.id),
                            "createdAt": s.created_at,
                            "updatedAt": s.updated_at,
                        })
                    })
                    .collect();
                ToolResult::ok(
                    serde_json::json!({
                        "groupId": current.group_id,
                        "count": items.len(),
                        "sessions": items,
                    })
                    .to_string(),
                )
            }
            "search" => {
                let query = args
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if query.is_empty() {
                    return ToolResult::err("op=search 需要 query");
                }
                let hits = match self.store.search_sessions(query, limit) {
                    Ok(h) => h,
                    Err(e) => return ToolResult::err(format!("会话检索失败: {e}")),
                };
                let mut out: Vec<serde_json::Value> = Vec::new();
                for h in hits {
                    // 组隔离：命中后再按会话归属过滤（检索接口本身跨全库）
                    let Some(target) = self.store.get_session(&h.session_id).ok().flatten() else {
                        continue;
                    };
                    if !Self::same_scope(&current, &target) {
                        continue;
                    }
                    out.push(serde_json::json!({
                        "sessionId": h.session_id,
                        "title": h.title,
                        "titleMatch": h.title_match,
                        "messageMatches": h.message_matches,
                        "lastActiveAt": h.last_active_at,
                        "snippets": h.snippets,
                    }));
                }
                ToolResult::ok(
                    serde_json::json!({
                        "query": query,
                        "count": out.len(),
                        "hits": out,
                    })
                    .to_string(),
                )
            }
            "history" => {
                let target_id = args
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if target_id.is_empty() {
                    return ToolResult::err("op=history 需要 sessionId");
                }
                let Some(target) = self.store.get_session(target_id).ok().flatten() else {
                    return Self::scope_denied();
                };
                if !Self::same_scope(&current, &target) {
                    return Self::scope_denied();
                }
                let msgs = match self.store.list_messages(target_id, limit) {
                    Ok(m) => m,
                    Err(e) => return ToolResult::err(format!("会话历史读取失败: {e}")),
                };
                let items: Vec<serde_json::Value> = msgs
                    .iter()
                    .map(|m| {
                        let text = m
                            .statements
                            .iter()
                            .map(|s| s.text.as_str())
                            .collect::<Vec<_>>()
                            .join("\n");
                        let text: String = text.chars().take(4000).collect();
                        serde_json::json!({
                            "messageId": m.id,
                            "role": m.role,
                            "agentId": m.agent_id,
                            "text": text,
                            "createdAt": m.created_at,
                            "toolCalls": m.tool_calls.iter().map(|t| {
                                serde_json::json!({ "tool": t.tool, "ok": t.ok })
                            }).collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                ToolResult::ok(
                    serde_json::json!({
                        "sessionId": target.id,
                        "title": target.title,
                        "count": items.len(),
                        "messages": items,
                    })
                    .to_string(),
                )
            }
            "send" => {
                let target_id = args
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                let text = args
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if target_id.is_empty() || text.is_empty() {
                    return ToolResult::err("op=send 需要 sessionId 与 text");
                }
                if target_id == current.id {
                    return ToolResult::err("不能向当前会话投递（会与进行中的本轮串行排队）；直接继续对话即可");
                }
                let Some(target) = self.store.get_session(target_id).ok().flatten() else {
                    return Self::scope_denied();
                };
                if !Self::same_scope(&current, &target) {
                    return Self::scope_denied();
                }
                let wait = args.get("wait").and_then(|v| v.as_bool()).unwrap_or(false);
                let Some(exec) = self.session_send_executor.read().clone() else {
                    return ToolResult::err("跨会话投递执行器未装配（运行时未就绪）");
                };
                let origin = format!("auto:agent:{agent_id}");
                let text: String = text.chars().take(8000).collect();
                match exec(target.id.clone(), text, origin, wait).await {
                    Ok(note) => ToolResult::ok(
                        serde_json::json!({
                            "sessionId": target.id,
                            "title": target.title,
                            "delivered": true,
                            "waited": wait,
                            "note": note,
                        })
                        .to_string(),
                    ),
                    Err(e) => ToolResult::err(format!("投递失败: {e}")),
                }
            }
            other => ToolResult::err(format!(
                "未知 op: {other}（支持 list / search / history / send）"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::types::{MessageRole, Statement};

    /// 组隔离是硬边界：list/search/history 不得跨组暴露会话内容
    #[tokio::test]
    async fn 跨会话_组隔离与历史读取() {
        let dir = std::env::temp_dir().join(format!("exm-sessions-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = crate::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        let core = crate::Core::with_config(cfg).expect("创建 Core 失败");
        core.registry().create_group(Some("g1".into()), "组一", "").unwrap();
        core.registry().create_group(Some("g2".into()), "组二", "").unwrap();
        let s1 = core.store.create_session("组一会话", "g1").unwrap();
        let s2 = core.store.create_session("组二会话", "g2").unwrap();
        core.store
            .add_message(
                &s1.id,
                MessageRole::User,
                None,
                vec![Statement::report("组一独有内容")],
            )
            .unwrap();
        core.store
            .add_message(
                &s2.id,
                MessageRole::User,
                None,
                vec![Statement::report("组二机密内容")],
            )
            .unwrap();

        let gw = &core.orchestrator().tools;
        // list：只看到同组会话
        let r = gw
            .tool_sessions("agent", &s1.id, &serde_json::json!({ "op": "list" }))
            .await;
        assert!(r.ok, "error={:?}", r.error);
        assert!(r.output.contains("组一会话"));
        assert!(!r.output.contains("组二会话"), "跨组会话不得列示");
        // history：跨组拒绝（不暴露存在性）
        let r = gw
            .tool_sessions(
                "agent",
                &s1.id,
                &serde_json::json!({ "op": "history", "sessionId": s2.id }),
            )
            .await;
        assert!(!r.ok);
        assert!(r.error.unwrap_or_default().contains("作用域"));
        // search：跨组命中被过滤
        let r = gw
            .tool_sessions(
                "agent",
                &s1.id,
                &serde_json::json!({ "op": "search", "query": "机密内容" }),
            )
            .await;
        assert!(r.ok, "error={:?}", r.error);
        assert!(!r.output.contains("组二会话"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
