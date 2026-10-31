//! tools/memory_tools.rs —— 自 tools.rs 按领域拆出（纯搬移，零行为变更）

use super::*;

impl ToolGateway {
    // ---------------------------------------------------------------- 记忆工具（组隔离，design D7）

    /// 调用者是否当前组主智能体（memory_read 全层视角 / memory_write 默认授权的判定口径）
    pub(crate) fn is_primary(&self, agent_id: &str) -> bool {
        self.registry
            .primary()
            .map(|p| p.identifier == agent_id)
            .unwrap_or(false)
    }

    /// 会话的记忆命名空间：通道「记忆隔离」开启时用会话分区，否则用激活组。
    /// 默认所有平台会话共享同组记忆；隔离只改变记忆键，不改变智能体组归属。
    pub(crate) fn memory_group(&self, session_id: &str) -> String {
        self.store
            .get_session(session_id)
            .ok()
            .flatten()
            .and_then(|s| s.memory_partition)
            .unwrap_or_else(|| self.registry.active_group())
    }

    /// 隔离分区的 memory.md 路径（与 orchestrator 的命名规则一致）。
    pub(crate) fn memory_md_path_for(&self, session_id: &str) -> Option<std::path::PathBuf> {
        let base = self.memory_md_path.as_ref()?;
        let partition = self
            .store
            .get_session(session_id)
            .ok()
            .flatten()
            .and_then(|s| s.memory_partition);
        Some(match partition {
            None => base.clone(),
            Some(p) => {
                let safe: String = p
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect();
                base.with_file_name(format!("memory-{safe}.md"))
            }
        })
    }

    /// memory_read：组内检索（主智能体全层视角 = 群体 + 所有个体私有；个体 = 私有 + 群体）。
    /// 组隔离在工具层强制：范围恒为「有效记忆命名空间 + 全局条目」，其他组条目不可见。
    /// 文件记忆模式（深层记忆关闭）：回落 memory.md 全文——与规划注入同源，口径一致。
    pub(crate) fn tool_memory_read(
        &self,
        agent_id: &str,
        session_id: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        let Some(memory) = &self.memory else {
            return ToolResult::err("记忆系统未接入（内部装配缺失）");
        };
        if !self.memory_deep {
            return self.file_memory_read(session_id);
        }
        let Some(query) = args
            .get("query")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return ToolResult::err("query 不能为空");
        };
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| (v as usize).min(20))
            .unwrap_or(5)
            .max(1);
        let gid = self.memory_group(session_id);
        let hits = if self.is_primary(agent_id) {
            memory.recall_all(query, limit, Some(&gid), None)
        } else {
            memory.recall_for_agent(agent_id, query, limit, Some(&gid), None)
        };
        match hits {
            Ok(list) if list.is_empty() => {
                ToolResult::ok(format!("记忆检索「{query}」：组内无命中条目。"))
            }
            Ok(list) => {
                let mut lines = vec![format!(
                    "记忆检索「{query}」：{} 条命中（按相关度排序）",
                    list.len()
                )];
                for h in list {
                    let source = match (&h.entry.agent_id, &h.entry.group_id) {
                        (Some(a), _) => format!("个体:{a}"),
                        (None, Some(g)) => format!("组:{g}"),
                        (None, None) => "全局".to_string(),
                    };
                    lines.push(format!(
                        "- [{}|{}|置信{:.2}|重要{:.2}|{}] {}：{}（id: {}）",
                        h.entry.kind.key(),
                        source,
                        h.entry.confidence,
                        h.entry.importance,
                        if h.entry.pinned { "置顶" } else { "常规" },
                        h.entry.title,
                        h.entry.body.replace('\n', " "),
                        h.entry.id
                    ));
                }
                ToolResult::ok(lines.join("\n"))
            }
            Err(e) => ToolResult::err(format!("记忆检索失败：{e}")),
        }
    }

    /// 文件记忆模式的 memory_read：返回 memory.md 内容（与规划注入同源、同一 8000 字符上限）。
    /// 为什么不查数据库：文件记忆模式下规划面只注入 memory.md——
    /// 工具看到的与规划看到的不一致，本身就是行为分裂。
    pub(crate) fn file_memory_read(&self, session_id: &str) -> ToolResult {
        let Some(path) = self.memory_md_path_for(session_id) else {
            return ToolResult::err("记忆系统未接入（内部装配缺失）");
        };
        match std::fs::read_to_string(&path) {
            Ok(md) if !md.trim().is_empty() => {
                let total = md.chars().count();
                let trimmed: String = md.chars().take(8000).collect();
                let note = if total > 8000 {
                    format!("\n…（已截断：memory.md 共 {total} 字符，仅保留前 8000）")
                } else {
                    String::new()
                };
                ToolResult::ok(format!(
                    "文件记忆（memory.md，深层记忆已关闭）：\n{trimmed}{note}"
                ))
            }
            Ok(_) => ToolResult::ok("memory.md 为空：文件记忆模式下尚无记忆内容。"),
            Err(_) => ToolResult::ok("memory.md 不存在：文件记忆模式下尚无记忆内容。"),
        }
    }

    /// memory_write：写入组内记忆（哈希去重由 MemoryStore 承担）；来源标注智能体自写（source=agent）
    pub(crate) fn tool_memory_write(
        &self,
        agent_id: &str,
        session_id: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        let Some(memory) = &self.memory else {
            return ToolResult::err("记忆系统未接入（内部装配缺失）");
        };
        if !self.memory_deep {
            return ToolResult::err(
                "深层记忆已关闭（文件记忆模式）：写进数据库的记忆不会被规划召回。请改为直接维护 memory.md（filesystem write 可达），或建议用户在设置中开启深层记忆。",
            );
        }
        let Some(kind) = args
            .get("kind")
            .and_then(|v| v.as_str())
            .and_then(crate::memory::MemoryKind::parse)
        else {
            return ToolResult::err(
                "kind 须为 fact/decision/preference/evidence/digest/lesson 之一",
            );
        };
        let Some(title) = args
            .get("title")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return ToolResult::err("title 不能为空");
        };
        let Some(body) = args
            .get("body")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return ToolResult::err("body 不能为空");
        };
        let importance = args
            .get("importance")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.5);
        let tags: Vec<String> = args
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let mut draft = crate::memory::MemoryDraft::new(kind, title, body)
            .importance(importance)
            .agent(agent_id)
            .group(&self.memory_group(session_id));
        draft.tags = tags;
        // 来源标注：智能体自写（与归纳写入可区分，管理视图可过滤）
        draft.source_ref = Some("agent".into());
        match memory.remember(&draft) {
            Ok(entry) => ToolResult::ok(format!(
                "已写入记忆：{}（id: {}，类型 {}，重要性 {:.2}）；同内容重复写入会被去重并提升既有条目",
                entry.title,
                entry.id,
                entry.kind.key(),
                entry.importance
            )),
            Err(e) => ToolResult::err(format!("记忆写入失败：{e}")),
        }
    }

    /// memory_link：关联建立（组隔离校验双端）与主题列示（条目 + 关联链）
    pub(crate) fn tool_memory_link(
        &self,
        agent_id: &str,
        session_id: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        let Some(memory) = &self.memory else {
            return ToolResult::err("记忆系统未接入（内部装配缺失）");
        };
        if !self.memory_deep {
            return ToolResult::err(
                "深层记忆已关闭（文件记忆模式）：记忆关联属于深层记忆库特性。可改为直接在 memory.md 中以文字标注关联。",
            );
        }
        let gid = self.memory_group(session_id);
        let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("link");
        match op {
            "list" => {
                let query = args
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                let limit = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|v| (v as usize).min(20))
                    .unwrap_or(5)
                    .max(1);
                let hits = if query.is_empty() {
                    // 空查询列示与检索同一套分层口径：个体只见「自己私有 + 群体」，
                    // 主智能体全层可见——recent 全量版不设防，不能在这里直接用
                    if self.is_primary(agent_id) {
                        memory.recent_in_layer(None, limit, Some(&gid))
                    } else {
                        memory.recent_in_layer(Some(agent_id), limit, Some(&gid))
                    }
                } else if self.is_primary(agent_id) {
                    memory.recall_all(query, limit, Some(&gid), None)
                } else {
                    memory.recall_for_agent(agent_id, query, limit, Some(&gid), None)
                };
                match hits {
                    Ok(list) if list.is_empty() => ToolResult::ok("主题下无记忆条目。".to_string()),
                    Ok(list) => {
                        let mut lines = vec![format!(
                            "主题「{query}」列示（{} 条，含关联链）：",
                            list.len()
                        )];
                        for h in list {
                            let links = memory.links_of(&h.entry.id).unwrap_or_default();
                            let chain = links
                                .iter()
                                .filter_map(|l| {
                                    let to = l.get("to").and_then(|v| v.as_str())?;
                                    let rel = l
                                        .get("relation")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("related");
                                    let dir = if l.get("from").and_then(|v| v.as_str())
                                        == Some(h.entry.id.as_str())
                                    {
                                        format!("{rel} → {to}")
                                    } else {
                                        let from =
                                            l.get("from").and_then(|v| v.as_str()).unwrap_or("?");
                                        format!("{from} →{rel}→ 本条")
                                    };
                                    Some(dir)
                                })
                                .collect::<Vec<_>>()
                                .join("；");
                            lines.push(format!(
                                "- [{}] {}（id: {}）{}",
                                h.entry.kind.key(),
                                h.entry.title,
                                h.entry.id,
                                if chain.is_empty() {
                                    String::new()
                                } else {
                                    format!("｜关联：{chain}")
                                }
                            ));
                        }
                        ToolResult::ok(lines.join("\n"))
                    }
                    Err(e) => ToolResult::err(format!("记忆列示失败：{e}")),
                }
            }
            _ => {
                let Some(from) = args
                    .get("from")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                else {
                    return ToolResult::err("from 不能为空（关联起始条目 id）");
                };
                let Some(to) = args
                    .get("to")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                else {
                    return ToolResult::err("to 不能为空（关联目标条目 id）");
                };
                let relation = args
                    .get("relation")
                    .and_then(|v| v.as_str())
                    .unwrap_or("related")
                    .trim();
                // 组隔离：双端条目都必须在调用者可见范围（激活组 + 全局），越组拒绝
                let visible = |id: &str| -> Option<bool> {
                    memory
                        .get_entry_public(id)
                        .ok()
                        .flatten()
                        .map(|e| e.group_id.as_deref().is_none_or(|g| g == gid))
                };
                match (visible(from), visible(to)) {
                    (Some(true), Some(true)) => match memory.link(from, to, relation) {
                        Ok(_) => ToolResult::ok(format!(
                            "已建立关联：{from} —{relation}→ {to}（后续列示与检索可见）"
                        )),
                        Err(e) => ToolResult::err(format!("关联写入失败：{e}")),
                    },
                    (Some(false), _) | (_, Some(false)) => {
                        ToolResult::err("关联被拒绝：条目不在你的组范围内（越组隔离）")
                    }
                    _ => ToolResult::err(
                        "条目不存在（检查 from/to 的条目 id，可先用 memory_link op=list 列示）",
                    ),
                }
            }
        }
    }
}
