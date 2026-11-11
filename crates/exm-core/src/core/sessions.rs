//! core/sessions.rs —— Core 会话域（codebase-refactor 拆分）：会话 CRUD、
//! undo/edit/fork、轮次快照回看、检查点、审批会话授权的归属与查询、会话起源标记。
//! 纯搬移：方法签名与实现零改动；chat 系跨域粘合留 lib.rs 组合根。

use super::super::Core;
use crate::registry::LocalRegistry;
use crate::types::*;

/// 会话归属键 → 轮次作用域元组：`"single:<id>"` → (单智能体原组回落, Some(id))；
/// 组名 → (组名, None)。归属目标已被删除时返回 None（轮次回落全局激活态，
/// 与旧 align_to_session_group 的「目标不存在保持现状」语义一致）。
pub(crate) fn parse_session_scope(
    registry: &LocalRegistry,
    group_id: &str,
) -> Option<(String, Option<String>)> {
    if let Some(id) = group_id.strip_prefix("single:") {
        if registry.single_exists(id) {
            return Some((registry.active_group_global(), Some(id.to_string())));
        }
        return None;
    }
    let g = group_id.trim();
    if !g.is_empty() && registry.group_exists(g) {
        return Some((g.to_string(), None));
    }
    None
}

impl Core {
    /// 会话 token 用量：优先真实用量（provider 上报），无记录时退回字符估算（全部消息陈述字符 / 4）
    pub fn session_tokens_estimate(&self, session_id: &str) -> u64 {
        let (prompt, completion, calls) = self.store.usage_total(session_id);
        if calls > 0 {
            return prompt + completion;
        }
        self.store
            .list_messages(session_id, 500)
            .unwrap_or_default()
            .iter()
            .map(|m| {
                m.statements
                    .iter()
                    .map(|s| s.text.chars().count() as u64)
                    .sum::<u64>()
                    / 4
            })
            .sum()
    }
    /// 会话用量明细：(prompt, completion, 调用次数)
    pub fn session_usage(&self, session_id: &str) -> (u64, u64, usize) {
        self.store.usage_total(session_id)
    }
    fn checkpoints_dir(&self) -> std::path::PathBuf {
        self.config()
            .workspace_root
            .join(".exmachina")
            .join("checkpoints")
    }
    /// 检查点清单：(日期, 相对路径, 字节数)，按日期倒序
    pub fn list_checkpoints(&self) -> Vec<(String, String, u64)> {
        let root = self.checkpoints_dir();
        let mut out: Vec<(String, String, u64)> = Vec::new();
        let Ok(days) = std::fs::read_dir(&root) else {
            return out;
        };
        for day in days.flatten() {
            if !day.path().is_dir() {
                continue;
            }
            let date = day.file_name().to_string_lossy().to_string();
            let mut stack = vec![day.path()];
            while let Some(dir) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for e in entries.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else if let Ok(rel) = p.strip_prefix(&root).map(|r| r.to_path_buf()) {
                        // 去掉前缀的 <日期>/ 段
                        let rel_str = rel
                            .components()
                            .skip(1)
                            .map(|c| c.as_os_str().to_string_lossy().to_string())
                            .collect::<Vec<_>>()
                            .join("/");
                        let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                        out.push((date.clone(), rel_str, size));
                    }
                }
            }
        }
        out.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        out
    }
    /// 回滚检查点：`date` 为空取最近一份；返回被覆盖的文件相对路径
    pub fn restore_checkpoint(&self, date: Option<&str>, rel: &str) -> anyhow::Result<String> {
        let target = self
            .config()
            .workspace_root
            .join(".exmachina")
            .join("checkpoints");
        let day = match date {
            Some(d) if !d.trim().is_empty() => d.trim().to_string(),
            _ => self
                .list_checkpoints()
                .first()
                .map(|(d, _, _)| d.clone())
                .ok_or_else(|| anyhow::anyhow!("没有可用的检查点"))?,
        };
        let src = target.join(&day).join(rel);
        if !src.is_file() {
            anyhow::bail!("检查点不存在：{day}/{rel}");
        }
        let dst = self.config().workspace_root.join(rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&src, &dst)?;
        Ok(format!(
            "已回滚 {} → {rel}（来自检查点 {day}）",
            dst.display()
        ))
    }
    /// 消息撤销（9.2）：空闲强校验 → 游标回退（截断部分归档可查）→ 上下文重建
    /// → 可选检查点联动回滚 → 审计事件。返回 {archived, restored, cursor}。
    pub async fn undo_session(
        &self,
        session_id: &str,
        upto_turn: usize,
        restore_files: bool,
    ) -> anyhow::Result<serde_json::Value> {
        let lock = self.run_lock(session_id);
        // 空闲校验：处理中拒绝（spec 硬性要求）
        let _guard = lock
            .try_lock()
            .map_err(|_| anyhow::anyhow!("会话正在处理中，请等待当前任务完成后再撤销"))?;
        self.store
            .get_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))?;
        let cursor = self.cursor_at_turn(session_id, upto_turn)?;

        // 游标回退 + 归档（spec：被截断内容保留归档视图）
        let archived = self.store.truncate_messages(session_id, cursor)?;

        // 上下文重建：截断后滚动摘要失效（摘要覆盖越过游标即重置，按截断后历史重建）
        // P2-8：上一行存在性校验与此处重查之间会话可被并发删除——不再 unwrap，
        // 查不到按「无摘要」处理（截断已成功，摘要重建跳过不致命）
        if let Some(session) = self.store.get_session(session_id)? {
            if session.summary_upto.map(|u| u > cursor).unwrap_or(false) {
                let _ = self.store.update_compaction(session_id, "", 0);
            }
        }

        // 检查点联动回滚：该轮之后各快照登记的文件变更，按轮次倒序恢复
        let mut restored: Vec<String> = Vec::new();
        if restore_files {
            let mut later: Vec<TurnSnapshot> = self
                .store
                .list_turn_snapshots(session_id)?
                .into_iter()
                .filter(|s| s.turn > upto_turn && !s.checkpoint_ids.is_empty())
                .collect();
            later.sort_by_key(|s| std::cmp::Reverse(s.turn)); // 后轮先回滚（后者覆盖前者的场景）
            for snap in later {
                for cp in snap.checkpoint_ids.iter().rev() {
                    let Some((date, rel)) = cp.split_once('/') else {
                        continue;
                    };
                    match self.restore_checkpoint(Some(date), rel) {
                        Ok(_) => restored.push(cp.clone()),
                        Err(e) => eprintln!("[undo] 检查点恢复失败 {cp}: {e}"),
                    }
                }
            }
        }

        // 孤儿快照清理 + 审计 + 事件
        let _ = self.store.prune_turn_snapshots_after(session_id, upto_turn);
        self.store.audit_tool_best_effort(
            "webui",
            "session_undo",
            &serde_json::json!({ "session": session_id, "uptoTurn": upto_turn, "restoreFiles": restore_files }),
            &format!("撤销至第 {upto_turn} 轮：归档 {archived} 条，联动回滚 {} 个文件", restored.len()),
            0,
        );
        let payload = serde_json::json!({
            "session": session_id, "uptoTurn": upto_turn, "cursor": cursor,
            "archived": archived, "restored": restored,
        });
        let _ = self.events.send(CoreEvent {
            kind: "session.undo".into(),
            session_id: session_id.to_string(),
            payload: payload.clone(),
        });
        Ok(payload)
    }
    /// 编辑重发（9.3）：撤销至该轮之前（原内容归档可查）后以新内容触发新一轮处理。
    /// `turn` 为被编辑的用户消息所在轮次。
    pub async fn edit_resend(
        &self,
        session_id: &str,
        turn: usize,
        new_text: &str,
    ) -> anyhow::Result<serde_json::Value> {
        if new_text.trim().is_empty() {
            anyhow::bail!("新内容不能为空");
        }
        let upto = turn.saturating_sub(1);
        let info = self.undo_session(session_id, upto, true).await?;
        // 以新内容重新执行（原内容已在归档中可查）
        self.chat(session_id, new_text).await?;
        Ok(serde_json::json!({ "undone": info, "resent": true, "turn": turn }))
    }
    /// 会话分支派生（9.4）：新会话携带截至该轮的完整历史副本与派生来源标注；
    /// 组归属（= 权限与成员上下文）继承，原会话不变。返回新会话。
    pub fn fork_session(&self, session_id: &str, upto_turn: usize) -> anyhow::Result<Session> {
        let src = self
            .store
            .get_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))?;
        let cursor = self.cursor_at_turn(session_id, upto_turn)?;
        let now = now_iso();
        let fork = Session {
            id: new_id(),
            title: format!("{}（分支·至第 {upto_turn} 轮）", src.title),
            status: "active".into(),
            origin: None,
            origin_label: None,
            group_id: src.group_id.clone(), // 权限与组继承
            memory_partition: src.memory_partition.clone(), // 记忆分区随会话继承
            rolling_summary: None,          // 摘要按副本历史重建
            summary_upto: None,
            parent_session: Some(src.id.clone()),
            parent_upto: Some(upto_turn),
            last_message_preview: None,
            last_active_at: None,
            pinned: false, // 置顶不继承分支
            archived_at: None, // 归档态不继承分支（分支从活跃态开始）
            message_count: None,
            ledger: src.ledger.clone(),
            created_at: now.clone(),
            updated_at: now,
        };
        self.store.insert_session(&fork)?;
        let copied = self
            .store
            .copy_messages_prefix(session_id, &fork.id, cursor)?;
        self.store.audit_tool_best_effort(
            "webui",
            "session_fork",
            &serde_json::json!({ "source": session_id, "uptoTurn": upto_turn, "fork": fork.id }),
            &format!("派生分支会话 {}（复制 {copied} 条消息）", fork.id),
            0,
        );
        let _ = self.events.send(CoreEvent {
            kind: "session.fork".into(),
            session_id: fork.id.clone(),
            payload: serde_json::json!({ "source": session_id, "uptoTurn": upto_turn, "fork": fork.id, "copied": copied }),
        });
        Ok(fork)
    }
    /// 停止会话运行中的轮次：置位取消令牌（工具循环检查点终止），返回已执行的工具次数。
    /// 空闲会话（无运行中轮次）返回错误。
    pub fn stop_session(&self, session_id: &str) -> anyhow::Result<usize> {
        if !crate::round_trace::is_running(session_id) {
            anyhow::bail!("该会话当前没有运行中的任务");
        }
        crate::round_trace::request_cancel(session_id);
        let n = crate::round_trace::tool_count(session_id);
        self.store.audit_tool_best_effort(
            "system",
            "session_stop",
            &serde_json::json!({ "session": session_id }),
            &format!("用户请求停止（当时已执行 {n} 次工具调用）"),
            0,
        );
        Ok(n)
    }
    pub fn list_sessions_in_group(&self, gid: &str) -> anyhow::Result<Vec<Session>> {
        self.store.list_sessions_in_group(gid)
    }
    /// 当前交互范围（组或智能体）的会话列表
    pub fn list_sessions_in_scope(&self) -> anyhow::Result<Vec<Session>> {
        self.store
            .list_sessions_in_group(&self.registry.active_scope())
    }
    pub fn delete_session(&self, id: &str) -> anyhow::Result<()> {
        crate::orchestrator::unmark_im_session(id);
        // 会话删除连带清理审批授权与工具预算计数（生命周期 = 会话生命周期）
        self.session_grants.lock().remove(id);
        self.orchestrator().tools.clear_tool_budget(id);
        self.store.delete_session(id)
    }
    /// 会话置顶切换（列表置顶分组数据源）
    pub fn toggle_session_pin(&self, id: &str, pinned: bool) -> anyhow::Result<()> {
        self.store.toggle_session_pin(id, pinned)
    }
    /// 会话归档 / 恢复（可逆状态：归档移出主列表，恢复回原分组）
    pub fn set_session_archived(&self, id: &str, archived: bool) -> anyhow::Result<()> {
        self.store.set_session_archived(id, archived)
    }
    /// 归档区列表（只含归档态，按归档时间倒序；预览口径与主列表一致）
    pub fn list_archived_sessions(&self) -> anyhow::Result<Vec<Session>> {
        let mut list = self.store.list_archived_sessions()?;
        for s in list.iter_mut() {
            s.last_message_preview = self.store.last_message_preview(&s.id, 80);
            s.last_active_at = Some(s.updated_at.clone());
            s.message_count = Some(self.store.message_count(&s.id));
        }
        self.attach_session_origins(&mut list);
        Ok(list)
    }
    /// 管理视角全量会话（含消息数计数；主列表口径 = 未归档）
    pub fn list_sessions_admin(&self) -> anyhow::Result<Vec<Session>> {
        let mut list = self.list_sessions()?;
        for s in list.iter_mut() {
            s.message_count = Some(self.store.message_count(&s.id));
        }
        self.attach_session_origins(&mut list);
        Ok(list)
    }
    pub fn create_session(&self, title: &str) -> anyhow::Result<Session> {
        let gid = self.registry.active_scope();
        self.store.create_session(title, &gid)
    }
    pub fn list_sessions(&self) -> anyhow::Result<Vec<Session>> {
        let mut list = self.store.list_sessions()?;
        // 末条消息预览（chat-ergonomics）：会话量级 ≤200，末行微读成本可控
        for s in list.iter_mut() {
            s.last_message_preview = self.store.last_message_preview(&s.id, 80);
            s.last_active_at = Some(s.updated_at.clone());
        }
        Ok(list)
    }
    /// 会话内容检索（标题 + 最近 200 条消息）：REST / 速查共用，
    /// 范围与消息读取窗口一致（命中片段可被聊天页加载并定位）。
    pub fn search_sessions(&self, query: &str, limit: usize) -> anyhow::Result<Vec<SessionSearchHit>> {
        self.store.search_sessions(query, limit)
    }
    /// 登记会话的外部来源（闸门放行时由适配器调用；审批单创建时据此打升级标）
    pub fn stamp_session_origin(&self, session_id: &str, external_id: &str, role: &str) {
        let _ = self.store.put_session_origin(session_id, external_id, role);
    }
    pub fn session_origin(&self, session_id: &str) -> Option<(String, String)> {
        self.store.get_session_origin(session_id).ok().flatten()
    }

}
