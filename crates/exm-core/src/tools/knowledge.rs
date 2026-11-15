//! tools/knowledge.rs —— 知识工具面（knowledge-base 任务 1.2）
//!
//! knowledge_write / knowledge_read 两工具。与记忆工具的关键差异：
//! 知识全局共享不分区——没有组隔离/个体私有层/记忆模式分支，装配即全量语义；
//! 恒定全智能体下发（不受记忆分区排除逻辑影响，spec: knowledge-base）。

use super::*;

impl ToolGateway {
    /// 知识库引用（未装配返回明确错误——与 memory 三工具同口径）
    fn knowledge(&self) -> Result<&Arc<crate::knowledge::KnowledgeStore>, ToolResult> {
        match &self.knowledge {
            Some(k) => Ok(k),
            None => Err(ToolResult::err(
                "知识库未装配（knowledge_store 未注入工具网关）",
            )),
        }
    }

    pub(crate) async fn tool_knowledge_write(
        &self,
        agent_id: &str,
        session_id: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        let k = match self.knowledge() {
            Ok(k) => k,
            Err(e) => return e,
        };
        let title = args
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let body = args
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if title.is_empty() {
            return ToolResult::err("title 不能为空（一句话概括这条知识）");
        }
        if body.is_empty() {
            return ToolResult::err("body 不能为空（知识正文）");
        }
        // 沉淀纪律在 schema 描述里，但硬护栏同样保留：超长直接拒绝并引导拆分
        // （写入侧钳制是兜底，工具面先给模型可纠正的失败）
        if body.chars().count() > crate::knowledge::KNOWLEDGE_BODY_CAP_CHARS {
            return ToolResult::err(format!(
                "body 过长（{} 字符 > {} 上限）：知识条目是检索单元而非文档仓库，请精炼要点或拆分为多条聚焦知识",
                body.chars().count(),
                crate::knowledge::KNOWLEDGE_BODY_CAP_CHARS
            ));
        }
        let tags: Vec<String> = args
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let draft = crate::knowledge::KnowledgeDraft {
            title,
            body,
            tags,
            source_agent: agent_id.to_string(),
            source_session: Some(session_id.to_string()),
        };
        let outcome = match k.write(&draft) {
            Ok(o) => o,
            Err(e) => return ToolResult::err(format!("知识写入失败：{e:#}")),
        };
        let e = outcome.entry();
        let status = if outcome.deduped() {
            "去重提升（既有条目热度 +1，标签并集补充）"
        } else {
            "新建"
        };
        ToolResult::ok(format!(
            "已写入知识库（{status}）\nid: {}\ntitle: {}\ntags: {}\n来源: {agent_id}",
            e.id,
            e.title,
            if e.tags.is_empty() {
                "-".to_string()
            } else {
                e.tags.join(", ")
            }
        ))
    }

    pub(crate) async fn tool_knowledge_read(
        &self,
        _agent_id: &str,
        _session_id: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        let k = match self.knowledge() {
            Ok(k) => k,
            Err(e) => return e,
        };
        let op = args
            .get("op")
            .and_then(|v| v.as_str())
            .unwrap_or("search")
            .trim()
            .to_string();
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n.clamp(1, 20) as usize)
            .unwrap_or(5);
        match op.as_str() {
            "search" => {
                let query = args
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if query.is_empty() {
                    return ToolResult::err("op=search 需要 query（检索关键词）");
                }
                let hits = match k.search(&query, limit) {
                    Ok(h) => h,
                    Err(e) => return ToolResult::err(format!("知识检索失败：{e:#}")),
                };
                if hits.is_empty() {
                    return ToolResult::ok(format!("知识库检索「{query}」无命中"));
                }
                let mut out = format!("知识库检索「{query}」命中 {} 条：", hits.len());
                for (i, e) in hits.iter().enumerate() {
                    out.push_str(&format!(
                        "\n{}. [{}] {}（来源: {}，hits: {}）\n   {}",
                        i + 1,
                        e.id,
                        e.title,
                        e.source_agent,
                        e.hits,
                        cap_line(&e.body, 200)
                    ));
                }
                ToolResult::ok(out)
            }
            "get" => {
                let id = args
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if id.is_empty() {
                    return ToolResult::err("op=get 需要 id（条目 id）");
                }
                match k.get(&id) {
                    Ok(Some(e)) => ToolResult::ok(format!(
                        "id: {}\ntitle: {}\ntags: {}\n来源: {}（会话: {}）\nhits: {}\n创建: {}\n更新: {}\n\n{}",
                        e.id,
                        e.title,
                        if e.tags.is_empty() { "-".into() } else { e.tags.join(", ") },
                        e.source_agent,
                        e.source_session.as_deref().unwrap_or("-"),
                        e.hits,
                        e.created_at,
                        e.updated_at,
                        e.body
                    )),
                    Ok(None) => ToolResult::err(format!("知识条目 {id} 不存在")),
                    Err(e) => ToolResult::err(format!("读取失败：{e:#}")),
                }
            }
            "list" => {
                let all = k.recent(limit);
                if all.is_empty() {
                    return ToolResult::ok("知识库为空");
                }
                let mut out = format!("知识库最近 {} 条（createdAt 倒序）：", all.len());
                for (i, e) in all.iter().enumerate() {
                    out.push_str(&format!(
                        "\n{}. [{}] {}（来源: {}，hits: {}）",
                        i + 1,
                        e.id,
                        e.title,
                        e.source_agent,
                        e.hits
                    ));
                }
                out.push_str("\n（op=get 按 id 取全文；hits 高的热门条目可优先参考）");
                ToolResult::ok(out)
            }
            other => ToolResult::err(format!(
                "未知 op「{other}」（支持 search / get / list）"
            )),
        }
    }
}

/// 单行摘要钳制（检索回显用）
fn cap_line(s: &str, cap: usize) -> String {
    let mut line: String = s.chars().take(cap).collect();
    if s.chars().count() > cap {
        line.push('…');
    }
    line.replace('\n', " ")
}
