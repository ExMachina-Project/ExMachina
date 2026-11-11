//! orchestrator/optimize.rs —— 经验优化域（codebase-refactor 5.3 拆分，
//! docs/10 §5）：记忆写入（write_memories）、个体经验提炼（optimize_agent）、
//! 批量自适应（auto_adapt）。纯搬移：签名与实现零改动。

use super::*;

impl Orchestrator {
    /// 记忆写入策略（docs/08 §4）：摘要必有、决策必存、A/B 级证据必存、失败必记教训
    pub(super) fn write_memories(
        &self,
        session_id: &str,
        user_text: &str,
        plan: &OrchestratorPlan,
        graph: &TaskGraphModel,
        reports: &Reports,
        statements: &[Statement],
    ) -> Vec<String> {
        let mut written: Vec<serde_json::Value> = Vec::new();
        let entry_ids: Vec<String> = Vec::new();
        // 记忆命名空间：默认归属激活组（同组跨平台共享）；通道开启记忆隔离时使用会话分区。
        let (gid, partition) = self.memory_scope_for(session_id);

        // 1) 会话摘要（薄记）
        let digest = format!(
            "目标：{}\n路由：{:?}{}\n节点：{}\n收束要点：{}",
            plan.goal,
            plan.route_level,
            plan.playbook
                .as_ref()
                .map(|p| format!("（链路 {p}）"))
                .unwrap_or_default(),
            graph
                .list()
                .iter()
                .map(|n| format!("{}({}):{}", n.id, n.agent_identifier, n.status.label()))
                .collect::<Vec<_>>()
                .join("、"),
            statements
                .iter()
                .filter(|s| matches!(s.tag, SpeechTag::肯定 | SpeechTag::报告))
                .map(|s| s.text.clone())
                .take(3)
                .collect::<Vec<_>>()
                .join("；")
        );
        if let Ok(e) = self.memory.remember(
            &MemoryDraft::new(
                MemoryKind::Digest,
                format!(
                    "会话摘要：{}",
                    plan.goal.chars().take(40).collect::<String>()
                ),
                digest,
            )
            .importance(0.45)
            .session(session_id)
            .tags(&["session", "digest"])
            .group(&gid),
        ) {
            written.push(serde_json::to_value(&e).unwrap_or(serde_json::Value::Null));
        }

        // 2) 决策（边界与禁区 = 本次任务的裁决口径）
        if let Ok(e) = self.memory.remember(
            &MemoryDraft::new(
                MemoryKind::Decision,
                format!(
                    "任务边界裁决：{}",
                    plan.goal.chars().take(40).collect::<String>()
                ),
                format!(
                    "范围内：{}\n禁区：{}\n验收：{}",
                    plan.boundary.in_scope.join("；"),
                    plan.boundary.forbidden.join("；"),
                    plan.acceptance.join("；")
                ),
            )
            .importance(0.7)
            .session(session_id)
            .tags(&["decision", "boundary"])
            .group(&gid),
        ) {
            written.push(serde_json::to_value(&e).unwrap_or(serde_json::Value::Null));
        }

        // 3) 证据（仅 A/B 级）与教训（失败/受阻），并按个体落统计
        for node in graph.list() {
            let Some(report) = reports.get(&node.id) else {
                continue;
            };
            let status = match report.status {
                SyncStatus::Done => "done",
                SyncStatus::Blocked => "blocked",
                SyncStatus::NeedArbitration => "blocked",
                SyncStatus::InProgress => "blocked",
            };
            let _ =
                self.memory
                    .record_agent_outcome(&node.agent_identifier, status, report.confidence);

            let strong: Vec<String> = report
                .evidence
                .iter()
                .filter(|e| matches!(e.level, EvidenceLevel::A | EvidenceLevel::B))
                .map(|e| {
                    format!(
                        "[{}|{}] {} — {}",
                        e.level.label(),
                        e.kind.label(),
                        e.reference,
                        e.note
                    )
                })
                .collect();
            if !strong.is_empty() {
                if let Ok(e) = self.memory.remember(
                    &MemoryDraft::new(
                        MemoryKind::Evidence,
                        format!("证据：{} / {}", node.agent_identifier, node.title),
                        strong.join("\n"),
                    )
                    .importance(0.6)
                    .session(session_id)
                    .agent(&node.agent_identifier)
                    .tags(&["evidence", &node.agent_identifier])
                    .group(&gid),
                ) {
                    written.push(serde_json::to_value(&e).unwrap_or(serde_json::Value::Null));
                }
            }

            if !matches!(report.status, SyncStatus::Done) || !report.blockers.is_empty() {
                let mut body = report.summary.clone();
                for b in &report.blockers {
                    body.push_str(&format!(
                        "\n阻塞：{}（解除条件：{}）",
                        b.reason, b.unblock_condition
                    ));
                }
                if let Ok(e) = self.memory.remember(
                    &MemoryDraft::new(
                        MemoryKind::Lesson,
                        format!("教训：{} 在「{}」受阻", node.agent_identifier, node.title),
                        body,
                    )
                    .importance(0.75)
                    .session(session_id)
                    .agent(&node.agent_identifier)
                    .tags(&["lesson", "failure", &node.agent_identifier])
                    .group(&gid),
                ) {
                    written.push(serde_json::to_value(&e).unwrap_or(serde_json::Value::Null));
                }
            }
        }

        // 4) 用户偏好：输入含"记住/以后/每次"等指令词时固化
        let lower = user_text.to_lowercase();
        if ["记住", "以后", "每次", "默认", "偏好"]
            .iter()
            .any(|k| lower.contains(k))
        {
            if let Ok(e) = self.memory.remember(
                &MemoryDraft::new(
                    MemoryKind::Preference,
                    "用户偏好指令",
                    user_text.chars().take(400).collect::<String>(),
                )
                .importance(0.9)
                .session(session_id)
                .tags(&["preference", "user"])
                .group(&gid),
            ) {
                written.push(serde_json::to_value(&e).unwrap_or(serde_json::Value::Null));
            }
        }

        // 5) 重渲染基础记忆文件（memory.md）：隔离档只渲染本分区可见条目
        let md_path = self.memory_md_path_for(session_id);
        let pinned = if partition.is_some() {
            self.memory
                .render_memory_md_scoped(&md_path, Some(&gid))
                .unwrap_or(0)
        } else {
            self.memory.render_memory_md(&md_path).unwrap_or(0)
        };
        self.emit(
            session_id,
            "memory.written",
            serde_json::json!({ "entries": written, "pinned": pinned, "path": md_path.display().to_string(), "partition": partition }),
        );
        entry_ids
    }
    /// 合成个体的经验改进要点：教训 + 可靠性统计 → LLM 提炼 → 净化 → 版本化落盘
    pub async fn optimize_agent(&self, identifier: &str) -> anyhow::Result<AgentAdaptation> {
        let Some(def) = self.registry.get(identifier) else {
            anyhow::bail!("个体不存在（激活组内）: {identifier}");
        };
        let gid = self.registry.active_group();
        let stats = self
            .memory
            .agent_stats(1000)?
            .into_iter()
            .find(|s| s.agent_id == identifier);
        let lessons = self.memory.list_filtered(
            Some(MemoryKind::Lesson),
            Some(identifier),
            20,
            Some(&gid),
        )?;

        let stat_line = stats
            .as_ref()
            .map(|s| {
                format!(
                    "执行 {} 次，完成 {}，受阻 {}，失败 {}，平均置信度 {:.2}",
                    s.runs, s.done, s.blocked, s.failed, s.avg_confidence
                )
            })
            .unwrap_or_else(|| "暂无执行统计".into());
        let lessons_block = if lessons.is_empty() {
            "（暂无历史教训）".to_string()
        } else {
            lessons
                .iter()
                .map(|e| {
                    format!(
                        "- {}：{}",
                        e.title,
                        e.body
                            .replace('\n', " ")
                            .chars()
                            .take(120)
                            .collect::<String>()
                    )
                })
                .collect::<Vec<_>>()
                .join(
                    "
",
                )
        };
        let basis = stats.as_ref().map(|s| {
            serde_json::json!({
                "runs": s.runs, "done": s.done, "blocked": s.blocked, "failed": s.failed,
                "avgConfidence": format!("{:.2}", s.avg_confidence),
            })
        });

        let system = "你是 EXMACHINA 的个体优化器：基于子个体的历史教训与执行统计，提炼可复用的行为改进要点。
要求：最多 6 条；每条以 \"- \" 开头、不超过 100 字；只输出规则列表，不要输出其他内容；
规则必须是可执行的行为约束（提炼共性规律，不复述个案细节）；缺乏可靠依据时仅输出：- 暂无改进要点。【经验改进要点合成】".to_string();
        let user = format!(
            "个体：{}（{}）
职责：{}
统计：{}

历史教训：
{}",
            def.name, def.identifier, def.description, stat_line, lessons_block
        );
        let (provider, model) = self.resolve_orch_model();
        let req = ChatRequest::new(
            model,
            vec![ChatMessage::system(system), ChatMessage::user(user)],
        );
        let resp = provider.chat(req).await?;
        let content = sanitize_adaptation(&resp.content);
        anyhow::ensure!(!content.is_empty(), "未能提炼出有效的改进要点");

        let mut adapt = self
            .registry
            .load_adaptation(identifier)?
            .unwrap_or_else(|| AgentAdaptation {
                identifier: identifier.to_string(),
                content: String::new(),
                revision: 0,
                lessons_seen: 0,
                updated_at: String::new(),
                basis: None,
                previous: vec![],
            });
        adapt.content = content;
        adapt.revision += 1;
        adapt.lessons_seen = lessons.len() as u32;
        adapt.basis = basis.clone();
        let saved = self.registry.save_adaptation(adapt)?;
        self.emit(
            "",
            "agent.adapted",
            serde_json::json!({
                "agentId": identifier, "revision": saved.revision,
                "basis": basis, "lessonsSeen": saved.lessons_seen,
            }),
        );
        Ok(saved)
    }
    /// 自动触发：某个体新增教训达 3 条时后台合成
    pub(super) async fn auto_adapt(self: &Arc<Self>, agent_ids: &[String]) {
        for id in agent_ids {
            let lessons = self
                .memory
                .list_filtered(
                    Some(MemoryKind::Lesson),
                    Some(id),
                    50,
                    Some(&self.registry.active_group()),
                )
                .unwrap_or_default()
                .len() as u32;
            let seen = self
                .registry
                .load_adaptation(id)
                .ok()
                .flatten()
                .map(|a| a.lessons_seen)
                .unwrap_or(0);
            if lessons.saturating_sub(seen) < 3 {
                continue;
            }
            if let Err(e) = self.optimize_agent(id).await {
                eprintln!("[orchestrator] 自动经验优化失败（{id}）：{e}");
            }
        }
    }
}
