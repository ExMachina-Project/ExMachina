//! orchestrator/finalize.rs —— 收束域（codebase-refactor 5.3 拆分）：
//! 收束回填（converge：SyncStatus 解析/回流/降级）。纯搬移：签名与实现零改动。

use super::*;

impl Orchestrator {
    pub async fn converge(
        &self,
        session_id: &str,
        plan: &OrchestratorPlan,
        reports: &Reports,
        graph: &TaskGraphModel,
    ) -> anyhow::Result<String> {
        // 组感知：收束提示取组内主智能体的职责提示词
        let primary = self
            .registry
            .primary()
            .ok_or_else(|| anyhow::anyhow!("当前组未设置主智能体，无法收束"))?;
        let mut system_prompt = self
            .registry
            .load_prompt(&primary.prompt_file)
            .unwrap_or_else(|_| format!("# {}\n\n你是本组的主智能体。", primary.name));
        // 主智能体人格（用户面 SOUL）：智能体模式读 entities/agents/<id>/SOUL.md，组模式读组根 SOUL.md
        let soul = if self.registry.single_mode() {
            self.registry
                .active_single()
                .and_then(|id| self.registry.single_persona(&id).ok())
        } else {
            self.registry.persona(&primary.identifier).ok()
        }
        .unwrap_or_else(|| LocalRegistry::DEFAULT_PERSONA.to_string());
        system_prompt.push_str(&format!("\n\n## SOUL（人格）\n{soul}"));
        // IM 通道会话：注入简洁表达纪律（收束是用户可见的最终回复）
        if is_im_session(session_id) {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(IM_DISCIPLINE);
        }
        // 自主记忆模式（实验性）：收束轮可自主决定是否写入记忆
        if self.config_snapshot.read().experiments.llm_driven_memory {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(LLM_MEMORY_DISCIPLINE);
        }
        // Super Agent（实验性）：收束轮同样受深度委托纪律约束
        if self.config_snapshot.read().experiments.super_agent {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(crate::autonomy::SUPER_AGENT_DISCIPLINE);
        }
        let digest = graph
            .list()
            .iter()
            .map(|n| {
                let summary = reports
                    .get(&n.id)
                    .map(|r| r.summary.clone())
                    .unwrap_or_else(|| "无回流".to_string());
                format!(
                    "- {} {}({}) [{}]: {}",
                    n.id,
                    n.title,
                    n.agent_identifier,
                    n.status.label(),
                    summary
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        let messages = vec![
            ChatMessage::system(system_prompt),
            ChatMessage::user(format!(
                "【收束请求】\n目标：{}\n验收：{}\n\n各节点回流摘要：\n{digest}\n\n【要求】综合各节点结果，直接向用户交付最终回复。表达形态完全由上方系统提示决定（IM 会话 = 聊天消息体：结论先行、几句话说清、禁止标题/清单/分节；控制台 = 正常陈述文）。任务有缺口就说明缺什么、卡在哪；完成就交付结果本身。禁止输出「任务边界/已启用链路/关键结论/冲突裁决/残余未知/最终交付」这类审计分节结构——那是内部过程，不是用户要看的回复。",
                plan.goal,
                plan.acceptance.join("；")
            )),
        ];
        // 收束阶段：输出为用户面最终结果，流式可见
        match self.call_orch_stream(session_id, &messages, true).await {
            Ok(text) => Ok(text),
            Err(e) => {
                // 降级路径：候选链耗尽 / 网络断等导致收束 LLM 不可用时，不把本轮直接判死——
                // 各节点已回流的真实成果（摘要 / 阻塞项 / 状态）改用确定性聚合收口，
                // 失败原因显式注入残余未知段，用户仍能拿到可执行的结构化结论。
                eprintln!("[orchestrator] 收束 LLM 调用失败，降级为结构化回流聚合: {e}");
                self.emit(
                    session_id,
                    "converge.fallback",
                    serde_json::json!({ "reason": e.to_string() }),
                );
                Ok(fallback_converge_text(plan, reports, graph, &e.to_string()))
            }
        }
    }
}
