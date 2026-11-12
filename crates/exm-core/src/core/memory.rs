//! core/memory.rs —— Core 记忆域（codebase-refactor 拆分）：记忆召回/写入/清单/
//! 统计/钉选/遗忘/重建索引/衰减、memory.md 渲染与压缩、个体 persona、语音转写合成。
//! 纯搬移：方法签名与实现零改动。

use super::super::Core;
use super::super::DIGEST_SYSTEM_PROMPT;
use crate::memory::{MemoryDraft, MemoryEntry, MemoryKind, RecallHit};

impl Core {
    /// 会话的记忆分区（外部通道「记忆隔离」）：None = 与同组共享。
    pub fn memory_partition_for_session(&self, session_id: &str) -> Option<String> {
        self.store
            .get_session(session_id)
            .ok()
            .flatten()
            .and_then(|s| s.memory_partition)
    }
    /// 会话的有效记忆命名空间：有分区用分区，否则用当前组。
    /// 注意：这里返回的是**记忆维度**的组键，不改变 registry 的激活组。
    pub fn memory_group_for_session(&self, session_id: &str) -> String {
        self.memory_partition_for_session(session_id)
            .unwrap_or_else(|| self.active_group())
    }
    pub fn memory_recall(
        &self,
        query: &str,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<RecallHit>> {
        let cfg = self.config();
        let gid = self.registry.active_group();
        self.memory.recall(
            query,
            limit.unwrap_or(cfg.memory_recall_limit),
            None,
            Some(&gid),
            None,
        )
    }
    /// 全量检索（管理视角：群体 + 所有个体私有；范围 = 激活组 + 全局条目）
    pub fn memory_recall_all(
        &self,
        query: &str,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<RecallHit>> {
        let gid = self.registry.active_group();
        self.memory.recall_all(
            query,
            limit.unwrap_or(self.config().memory_recall_limit),
            Some(&gid),
            None,
        )
    }
    /// 调试检索（8.4 记忆检索调试视图）：与对话内召回同口径——语义检索
    /// 启用时嵌入查询向量（词项 + 语义混合命中，分数与 reasons 可解释）；
    /// 未启用等价 memory_recall（仅词项）。嵌入失败静默回落词项（调试视图
    /// 不应因嵌入端点抖动把检索本身打挂）。
    pub async fn memory_recall_debug(
        &self,
        query: &str,
        limit: Option<usize>,
        agent: Option<&str>,
    ) -> anyhow::Result<Vec<RecallHit>> {
        let qe = self
            .orchestrator()
            .memory_query_embedding(query)
            .await;
        let gid = self.registry.active_group();
        let cfg = self.config();
        let limit = limit.unwrap_or(cfg.memory_recall_limit);
        match agent {
            Some(id) => self.memory.recall_for_agent(id, query, limit, Some(&gid), qe.as_deref()),
            None => self.memory.recall(query, limit, None, Some(&gid), qe.as_deref()),
        }
    }
    /// 个体检索：该智能体私有 + 群体共享（范围 = 激活组 + 全局条目）
    pub fn memory_recall_for_agent(
        &self,
        agent_id: &str,
        query: &str,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<RecallHit>> {
        let cfg = self.config();
        let gid = self.registry.active_group();
        self.memory.recall_for_agent(
            agent_id,
            query,
            limit.unwrap_or(cfg.memory_recall_limit),
            Some(&gid),
            None,
        )
    }
    /// 写入记忆：自动归属激活组（组内可见，其他组不可见）
    pub fn memory_remember(&self, mut draft: MemoryDraft) -> anyhow::Result<MemoryEntry> {
        if draft.group_id.is_none() {
            draft.group_id = Some(self.registry.active_group());
        }
        let entry = self.memory.remember(&draft)?;
        self.render_memory_md()?;
        Ok(entry)
    }
    /// 写入全局记忆（跨组可见）
    pub fn memory_remember_global(&self, draft: MemoryDraft) -> anyhow::Result<MemoryEntry> {
        let mut draft = draft;
        draft.group_id = None;
        let entry = self.memory.remember(&draft)?;
        self.render_memory_md()?;
        Ok(entry)
    }
    pub fn memory_list(
        &self,
        kind: Option<MemoryKind>,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.memory.list(kind, limit)
    }
    /// 按归属过滤列表：Some(id) => 该个体私有 + 群体共享；组范围 = 激活组 + 全局条目
    pub fn memory_list_filtered(
        &self,
        kind: Option<MemoryKind>,
        agent: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let gid = self.registry.active_group();
        self.memory.list_filtered(kind, agent, limit, Some(&gid))
    }
    pub fn memory_stats(&self) -> anyhow::Result<serde_json::Value> {
        self.memory.stats()
    }
    pub fn memory_pin(&self, id: &str, pinned: bool) -> anyhow::Result<()> {
        self.memory.pin(id, pinned)?;
        self.render_memory_md()?;
        Ok(())
    }
    pub fn memory_forget(&self, id: &str) -> anyhow::Result<()> {
        self.memory.forget(id)?;
        self.render_memory_md()?;
        Ok(())
    }
    /// 条目编辑（assistant-parity T3）：改后同步刷 memory.md（面板与文件记忆同源）
    pub fn memory_update(
        &self,
        id: &str,
        title: Option<&str>,
        body: Option<&str>,
        tags: Option<Vec<String>>,
        kind: Option<&str>,
    ) -> anyhow::Result<Option<crate::memory::MemoryEntry>> {
        let entry = self.memory.update(id, title, body, tags, kind)?;
        if entry.is_some() {
            self.render_memory_md()?;
        }
        Ok(entry)
    }
    pub fn memory_reindex(&self) -> anyhow::Result<usize> {
        self.memory.reindex()
    }
    /// 记忆整理：时间衰减 + 重新渲染 memory.md
    pub fn memory_decay(&self, floor: f64) -> anyhow::Result<usize> {
        let cfg = self.config();
        let changed = self.memory.decay(cfg.memory_half_life_days, floor)?;
        self.render_memory_md()?;
        Ok(changed)
    }
    /// 重新生成基础记忆文件（memory.md，全局浅层视图）；
    /// 同时刷新：每组一份 `groups/<gid>/MEMORY.md`（组浅层记忆）、每智能体一份 `entities/agents/<id>/MEMORY.md`。
    /// 深层记忆始终在 MemoryStore（数据库检索/衰减/压缩）——MEMORY.md 只是容量受限的人读快照。
    pub fn render_memory_md(&self) -> anyhow::Result<usize> {
        let cfg = self.config();
        let n = self.memory.render_memory_md(&cfg.memory_md_path)?;
        let gid = self.registry.active_group();
        for meta in self.registry.list_groups() {
            if let Some(dir) = self.registry.group_dir(&meta.id) {
                let _ = self
                    .memory
                    .render_group_memory_md(&meta.id, dir.join("MEMORY.md"));
            }
        }
        for s in self.registry.list_singles() {
            let dir = self.registry.singles_dir_pub().join(&s.identifier);
            let _ = self.memory.render_agent_memory_md(
                &s.identifier,
                Some(&gid),
                dir.join("MEMORY.md"),
            );
        }
        Ok(n)
    }
    pub fn persona(&self, identifier: &str) -> anyhow::Result<String> {
        self.registry.persona(identifier)
    }
    pub fn persona_is_custom(&self, identifier: &str) -> anyhow::Result<bool> {
        self.registry.persona_is_custom(identifier)
    }
    pub fn set_persona(&self, identifier: &str, text: &str) -> anyhow::Result<()> {
        self.registry.set_persona(identifier, text)
    }
    pub fn reset_persona(&self, identifier: &str) -> anyhow::Result<bool> {
        self.registry.reset_persona(identifier)
    }
    /// 统一对话入口：会话级串行（运行中的后续输入排队为 followup，收束后依次处理；
    /// 排队窗内到达的多条输入由 handle_user_message 的 collect 合并为一轮）
    /// 语音转写（全局生效档案，whisper 系模型；音频字节 → 文本）
    pub async fn transcribe(&self, audio: &[u8], filename: &str) -> anyhow::Result<String> {
        let orch = self.orchestrator();
        orch.transcribe_audio(audio, filename).await
    }
    /// 语音合成（全局生效档案，tts 系模型；文本 → mp3 字节）
    pub async fn speak(&self, text: &str) -> anyhow::Result<Vec<u8>> {
        let orch = self.orchestrator();
        orch.speak_audio(text).await
    }
    /// memory.md 超限自主压缩（LLM 简略 + 旧文归档）；返回 (压缩前, 压缩后) 字数
    pub async fn compact_memory_md(&self) -> anyhow::Result<(usize, usize)> {
        let max = self.config().memory_md_max_chars;
        let orch = self.orchestrator();
        orch.compact_memory_md(max).await
    }
    /// 自主记忆摘要（实验性 experiments.autoMemoryDigest，9.1）：把一个长会话
    /// 历史归纳为记忆条目。低峰期由调度器对每个达标会话调用（当日全库只跑一次）。
    /// 返回 Some(标题) = 本次产出/命中记忆；None = 会话不达标或调用失败（留事件）。
    ///
    /// 设计约束（任务 9.1）：
    /// - token 预算：转录每条截 400 字符、最多取 200 条（compaction 同口径），
    ///   单会话一次非流式调用，无工具循环；
    /// - 去重：落库走 memory.remember（内容哈希去重——重复摘要提升重要性
    ///   而非新建条目）；
    /// - 默认关：关闭态调度器不调用（零任务零 token）。
    pub async fn memory_digest_session(&self, session_id: &str) -> Option<String> {
        let cfg = self.config();
        let min_turns = cfg.automation.memory_digest_min_turns.max(5);
        let count = self.store.message_count(session_id);
        if count < min_turns {
            return None;
        }
        let msgs = self
            .store
            .list_messages(session_id, 200)
            .unwrap_or_default();
        if msgs.is_empty() {
            return None;
        }
        let transcript: String = msgs
            .iter()
            .map(|m| {
                let who = match m.role {
                    crate::types::MessageRole::User => "用户",
                    crate::types::MessageRole::Orchestrator => {
                        m.agent_id.as_deref().unwrap_or("指挥体")
                    }
                    crate::types::MessageRole::Unit => m.agent_id.as_deref().unwrap_or("子个体"),
                    crate::types::MessageRole::System => "系统",
                };
                let body = m
                    .statements
                    .iter()
                    .map(|s| s.text.as_str())
                    .collect::<Vec<_>>()
                    .join("；");
                format!("[{who}] {}", body.chars().take(400).collect::<String>())
            })
            .collect::<Vec<_>>()
            .join("\n");

        let orch = self.orchestrator();
        let req = crate::provider::ChatRequest::new(
            orch.orch_model.clone(),
            vec![
                crate::provider::ChatMessage::system(DIGEST_SYSTEM_PROMPT),
                crate::provider::ChatMessage::user(format!(
                    "## 会话转录（{count} 条消息）\n{transcript}"
                )),
            ],
        );
        let output = match orch.orch_provider.chat(req).await {
            Ok(r) => r.content,
            Err(e) => {
                orch.emit(
                    session_id,
                    "memory.digest",
                    serde_json::json!({ "ok": false, "error": e.to_string() }),
                );
                return None;
            }
        };
        // 摘要落记忆（内容哈希去重；来源指向会话）
        let title = format!("会话摘要：{}", output.chars().take(40).collect::<String>());
        let draft = crate::memory::MemoryDraft::new(
            crate::memory::MemoryKind::Digest,
            &title,
            output.chars().take(600).collect::<String>(),
        )
        .session(session_id);
        match self.memory.remember(&draft) {
            Ok(entry) => {
                orch.emit(
                    session_id,
                    "memory.digest",
                    serde_json::json!({
                        "ok": true,
                        "entryId": entry.id,
                        "deduped": entry.updated_at != entry.created_at,
                        "messageCount": count,
                    }),
                );
                Some(title)
            }
            Err(e) => {
                orch.emit(
                    session_id,
                    "memory.digest",
                    serde_json::json!({ "ok": false, "error": e.to_string() }),
                );
                None
            }
        }
    }

}
