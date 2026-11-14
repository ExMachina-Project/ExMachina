//! orchestrator/plan.rs —— 任务分解域（codebase-refactor 5.3 拆分）：
//! 规划提示组装、规划 LLM 调用（plan）、流式调用封装（call_orch_stream）。
//! 纯搬移：方法签名与实现零改动。

use super::*;

impl Orchestrator {
    pub(super) async fn plan(
        self: &Arc<Self>,
        session_id: &str,
        text: &str,
    ) -> anyhow::Result<OrchestratorPlan> {
        // 组感知：规划提示统一取组内主智能体的职责提示词；内置组即指挥体
        let primary = self.registry.primary().ok_or_else(|| {
            anyhow::anyhow!("当前组未设置主智能体（ exm group info 查看；用 agent_manage 或 exm agent create 创建）")
        })?;
        let mut system_prompt = self
            .registry
            .load_prompt(&primary.prompt_file)
            .unwrap_or_else(|_| {
                format!(
                    "# {}\n\n你是本组的主智能体，直接对接用户并调度组内个体。",
                    primary.name
                )
            });
        // 主智能体人格（用户面 SOUL）：智能体模式读 entities/agents/<id>/SOUL.md，组模式读组根 SOUL.md（热读取）
        let soul = if self.registry.single_mode() {
            self.registry
                .active_single()
                .and_then(|id| self.registry.single_persona(&id).ok())
        } else {
            self.registry.persona(&primary.identifier).ok()
        }
        .unwrap_or_else(|| LocalRegistry::DEFAULT_PERSONA.to_string());
        system_prompt.push_str(&format!("\n\n## SOUL（人格）\n{soul}"));
        if !self.registry.single_mode() {
            // 组模式：规划契约由系统统一追加，组提示词只负责人格与工作方式
            // （简洁人格提示词 + 该契约 = 指挥体能力；自定义组同口径）
            system_prompt.push_str(PLANNING_CONTRACT);
            // IM 会话路由纪律（im-fast-path）：聊天输入绝大多数是闲聊/短问句，
            // 不值得拆解派发。部署实测反面案例：「@id 摸摸」被规划成 46 步
            // 工具探索轮（67s 延迟）——闲聊进了任务流水线纯属浪费。
            if is_im_session(session_id) {
                system_prompt.push_str("\n\n## IM 路由纪律（强制）\n本会话是即时通讯聊天：绝大多数输入是闲聊、短问句或一句话指令——一律 L0 直答（finalAnswer 即回复），几句话说完。\n只有用户明确要求「做某件事」（查资料/写代码/分析文件/多步任务等）且单次回答无法完成时才拆解 nodes 派发；派发是例外不是默认。\n不使用任何工具探索上下文：会话历史里有的信息直接用，没有就按常识答或说明缺什么。\n\n## IM 表达纪律（强制）\nfinalAnswer 就是送达聊天的消息本体，按聊天消息写：默认 1~5 句话直接回答；结论先行，不绕铺垫。\n禁止：状态播报前缀（▸/⚠/⛔）、多级标题、编号清单、「依据：1. 2. 3.」式分条罗列、置信度标注（P=0.x）、证据等级标注——聊天场景这些全是噪音。\n用户明确要求详细展开或生成长文时才可长回复，此时直接输出内容主体。");
            }
        }
        // Super Agent（实验性）：组模式同样受深度委托纪律约束——规划轮决定拆解与
        // 派发策略（并行拆独立子任务）/ L0 直答决定回复口径，两处都按「静默推进、
        // 按影响停、只在停机条件产出非空回复」执行
        if self.config_snapshot.read().experiments.super_agent {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(crate::autonomy::SUPER_AGENT_DISCIPLINE);
        }
        let registry_brief = self
            .registry
            .units()
            .iter()
            .map(|d| format!("- {}（{}）：{}", d.identifier, d.name, d.description))
            .collect::<Vec<_>>()
            .join("\n");
        // 清单为空时不渲染该段（智能体模式不宣传派发能力，也不否认——交由模型自主判断）
        let registry_section = if registry_brief.is_empty() {
            String::new()
        } else {
            format!("## 当前可调度子个体\n{registry_brief}\n\n")
        };

        // 技能包清单（数据驱动）：命中触发词的技能会随派发自动携带
        let skill_brief = match self.registry.load_skills() {
            Ok(sk) if !sk.is_empty() => {
                let lines = sk
                    .iter()
                    .map(|s| {
                        let agents = if s.agents.is_empty() {
                            "全体".to_string()
                        } else {
                            s.agents.join("、")
                        };
                        format!(
                            "- {}（{}）：触发 [{}]；适用 {}",
                            s.id,
                            s.name,
                            if s.triggers.is_empty() {
                                "—".into()
                            } else {
                                s.triggers.join("、")
                            },
                            agents
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(
                        "
",
                    );
                format!(
                    "## 可用技能包
{lines}

技能维护：完成任务后若形成可复用流程，用 skill_manage 沉淀或修正（触发词要具体、指令可执行）。
"
                )
            }
            _ => "## 技能沉淀\n完成任务后若形成可复用流程，用 skill_manage 创建技能（id 稳定、触发词具体、指令可执行）。\n".to_string(),
        };

        // 链路模板清单（数据驱动）：与注册表一样注入规划提示，编成变化无需改代码
        let playbook_brief = match self.registry.load_playbooks() {
            Ok(pbs) if !pbs.is_empty() => {
                let lines = pbs
                    .iter()
                    .map(|p| {
                        format!(
                            "- {} {}：{}（触发：{}）",
                            p.id,
                            p.name,
                            p.description,
                            if p.trigger_signals.is_empty() {
                                "—".to_string()
                            } else {
                                p.trigger_signals.join("、")
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("## 可用链路模板\n{lines}\n")
            }
            _ => String::new(),
        };

        // 协议层（数据驱动，protocol/README.md）：常驻目录 + 命中触发词的协议全文按需注入
        let protocol_block = match self.registry.load_protocols() {
            Ok(ps) => protocol_plan_block(&ps, text),
            Err(_) => String::new(),
        };

        // ---- 记忆召回（docs/08 §5）：注入历史记忆与个体可靠性统计 ----
        let mut memory_block = String::new();
        // 记忆命名空间：默认同组共享；通道开启隔离时会话带独立分区键
        let memory_group = self.memory_group_for_session(session_id);
        if self.memory_enabled {
            if let Ok(hits) = self.memory.recall(
                text,
                self.memory_recall_limit,
                None,
                Some(&memory_group),
                self.embed_query(text).await.as_deref(),
            ) {
                memory_block = MemoryStore::render_prompt_block(&hits);
                self.emit(
                    session_id,
                    "memory.recall",
                    serde_json::json!({
                        "query": text,
                        "hits": hits.iter().map(|h| serde_json::json!({
                            "id": h.entry.id,
                            "kind": h.entry.kind.key(),
                            "title": h.entry.title,
                            "score": h.score,
                            "reasons": h.reasons,
                        })).collect::<Vec<_>>()
                    }),
                );
            }
        } else {
            // 深层记忆关闭：回落 memory.md 文件记忆（文件记忆模式）；隔离档读独立文件
            let md_path = self.memory_md_path_for(session_id);
            if let Ok(md) = std::fs::read_to_string(&md_path) {
                let trimmed: String = md.chars().take(8000).collect();
                if !trimmed.trim().is_empty() {
                    memory_block = format!("## Memory (memory.md)\n{trimmed}\n\n");
                }
            }
        }
        let stats_block = match self.memory.agent_stats(12) {
            Ok(stats) if !stats.is_empty() => {
                let lines = stats
                    .iter()
                    .map(|s| {
                        format!(
                            "- {}：执行 {} 次，完成 {}，受阻 {}，失败 {}，平均置信度 {:.2}",
                            s.agent_id, s.runs, s.done, s.blocked, s.failed, s.avg_confidence
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("## 个体可靠性统计（历史反馈，用于选路参考）\n{lines}\n")
            }
            _ => String::new(),
        };

        // 会话上下文（多轮记忆）：滚动摘要（压缩后的更早历史）+ 最近窗口原文
        let (summary, recent) = self.ensure_compacted(session_id).await;
        let context_block = match (&summary, recent.is_empty()) {
            (Some(s), false) if !s.trim().is_empty() => {
                format!("## 会话上下文（更早对话摘要）\n{s}\n\n## 最近对话\n{recent}\n\n")
            }
            (Some(s), _) if !s.trim().is_empty() => {
                format!("## 会话上下文（更早对话摘要）\n{s}\n\n")
            }
            (_, false) => format!("## 最近对话\n{recent}\n\n"),
            _ => String::new(),
        };

        let mut user_msg = ChatMessage::user(format!(
            "{context_block}用户任务输入：{text}\n\n【要求】按 OrchestratorPlan 契约输出 JSON。nodes 中的 agentIdentifier 必须来自上方可调度清单。"
        ));
        // 多模态输入：本轮附带的图片（data URL）。生效模型未标记视觉能力时，
        // 走「视觉转述」模型把图片转成文字描述注入；未配置转述且明确无视觉 → 丢弃并说明。
        let images = crate::image_stash::take(session_id);
        match self.relay_images(&images).await {
            Some(desc) => {
                let note = format!(
                    "\n\n## 图片内容（由视觉转述模型转写，原始图片未直接下发）\n{desc}\n【注入说明：上段是给你看的背景资料，回复时直接谈图片内容本身，不要复述转述文本、不要提「转述/转写/识图/图片描述」这类字眼，也不要逐条罗列图片细节——像亲眼看过图一样自然回应。】"
                );
                user_msg.content.push_str(&note);
            }
            None => {
                let vision = self
                    .orch_candidates()
                    .first()
                    .and_then(|(pid, _, model)| self.model_pool.capability(pid, model, true));
                if vision == Some(false) && !images.is_empty() {
                    user_msg.content.push_str(
                        "\n\n（用户随本轮附带了图片，但当前模型不支持视觉且未配置视觉转述模型，图片已忽略。）",
                    );
                } else {
                    user_msg.images = images;
                }
            }
        }
        let mut messages = vec![
            ChatMessage::system(format!(
                "{system_prompt}\n\n{registry_section}{skill_brief}{playbook_brief}{protocol_block}{stats_block}{memory_block}"
            )),
            user_msg,
        ];

        // 轮次模式（思考模式）：direct = 强制 L0 直答；full = 强制拆解派发。提示注入 + 校验双保险
        let forced_mode = take_session_mode(session_id);
        if let Some(mode) = &forced_mode {
            let line = match mode.as_str() {
                "direct" => "【本轮模式：直答】routeLevel 必须为 \"L0\"，nodes 必须为空数组，答案完整写入 finalAnswer。",
                _ => "【本轮模式：集群】除非智能体模式，routeLevel 不得为 \"L0\"：必须拆解为可派发的 nodes（至少一个节点）。",
            };
            if let Some(last) = messages.last_mut() {
                last.content.push_str(&format!("\n\n{line}"));
            }
        }

        let mut last_err = String::new();
        for _attempt in 0..3 {
            // 规划阶段：输出为内部协议 JSON，不进可见输出区（改道思维轨收起栏）
            let output = self.call_orch_stream(session_id, &messages, false).await?;
            match parse::parse_plan(&output) {
                Ok(plan) => {
                    let unknown: Vec<String> = plan
                        .nodes
                        .iter()
                        .filter(|n| self.registry.get(&n.agent_identifier).is_none())
                        .map(|n| n.agent_identifier.clone())
                        .collect();
                    let mode_violation = match forced_mode.as_deref() {
                        Some("direct") => {
                            !(matches!(plan.route_level, RouteLevel::L0) || plan.nodes.is_empty())
                        }
                        Some("full") => {
                            matches!(plan.route_level, RouteLevel::L0)
                                && plan.nodes.is_empty()
                                && !self.registry.single_mode()
                        }
                        _ => false,
                    };
                    if mode_violation {
                        last_err = "计划违反本轮模式约束（direct = 仅 L0 直答 / full = 必须派发），请重新规划".into();
                    } else if unknown.is_empty() {
                        return Ok(plan);
                    } else {
                        last_err =
                            format!("以下 agentIdentifier 不在注册表：{}", unknown.join("、"));
                    }
                }
                Err(e) => last_err = e,
            }
            messages.push(ChatMessage::assistant(output));
            messages.push(ChatMessage::user(format!(
                "【警告】计划未通过校验：{last_err}\n【要求】修正后仅输出 OrchestratorPlan JSON。"
            )));
        }
        anyhow::bail!("指挥体计划连续失败：{last_err}")
    }
    pub(super) async fn call_orch_stream(
        &self,
        session_id: &str,
        messages: &[ChatMessage],
        visible_tokens: bool,
    ) -> anyhow::Result<String> {
        // 回退链逐档尝试：失败且未发出任何 token 时切换下一档案并冷却失败者
        let candidates = self.orch_candidates();
        let mut last_err: Option<anyhow::Error> = None;
        for (pid, provider, model) in candidates {
            let mut req = ChatRequest::new(model.clone(), messages.to_vec());
            req.key_hint = Some("__orch__".into());
            let (tx, mut rx) = unbounded_channel::<StreamDelta>();
            let req_clone = req.clone();
            let stream_provider = provider.clone();
            let handle = tokio::spawn(async move { stream_provider.stream(req_clone, tx).await });

            let mut acc = String::new();
            // 收流 idle deadline（审计 错误处理 P1-1）：SSE 端点建连后不发帧不关流时
            // rx.recv() 永久阻塞 → run 永不收束 → run_lock 永久持有。帧间超过 180s
            // 无任何增量即判定僵死（正常端点两帧间隔远小于此；总时长上限由请求级
            // 600s 超时兜底）。超时后中断收流走回退链，流任务由 abort 收尾。
            let idle = std::time::Duration::from_secs(180);
            loop {
                match tokio::time::timeout(idle, rx.recv()).await {
                    Ok(Some(delta)) => match delta {
                    // 思维链单独成轨：渠道折叠呈现「思考」，不与回答混流
                    StreamDelta::Thinking(t) => {
                        // 思维增量随轮次留存（过程收起栏/落盘数据源）
                        crate::round_trace::push_thinking(session_id, &t);
                        self.emit(
                            session_id,
                            "orchestrator.thinking",
                            serde_json::json!({ "delta": t }),
                        );
                    }
                    StreamDelta::Text(t) => {
                        acc.push_str(&t);
                        // 规划阶段输出的是内部协议（OrchestratorPlan JSON），不是用户面文本：
                        // 改道思维轨（收起栏），可见输出区只保留收束阶段的用户面文本
                        let kind = if visible_tokens {
                            "orchestrator.token"
                        } else {
                            "orchestrator.thinking"
                        };
                        self.emit(session_id, kind, serde_json::json!({ "delta": t }));
                    }
                    },
                    // 流正常收尾（发送端 drop）：退出收流，等待任务终态
                    Ok(None) => break,
                    // 帧间超时：判定端点僵死——中断收流走回退链（任务由下方 abort 收尾）
                    Err(_) => {
                        handle.abort();
                        self.emit(
                            session_id,
                            "model.failover",
                            serde_json::json!({ "from": pid, "reason": "stream idle timeout" }),
                        );
                        self.failover
                            .cool(&pid, crate::provider::FAILOVER_COOLDOWN_SECS);
                        // 下一轮循环若候选耗尽，last_err 供末次非流式回退的报错语境
                        #[allow(unused_assignments)]
                        {
                            last_err = Some(anyhow::anyhow!(
                                "流式响应 {pid} 帧间超时（180s 无增量），判定端点僵死"
                            ));
                        }
                        continue;
                    }
                }
            }
            match handle.await {
                Ok(Ok(resp)) => {
                    self.failover.clear(&pid);
                    self.tools.record_usage(
                        session_id,
                        resp.prompt_tokens,
                        resp.completion_tokens,
                        &model,
                    );
                    if acc.is_empty() {
                        let fallback = provider.chat(req).await?;
                        self.tools.record_usage(
                            session_id,
                            fallback.prompt_tokens,
                            fallback.completion_tokens,
                            &model,
                        );
                        if !fallback.reasoning.is_empty() {
                            crate::round_trace::push_thinking(session_id, &fallback.reasoning);
                            self.emit(
                                session_id,
                                "orchestrator.thinking",
                                serde_json::json!({ "delta": fallback.reasoning }),
                            );
                        }
                        // 非流式回退：内联 <think> 与协议正文按阶段分轨（不裸奔到可见输出）
                        for (is_think, part) in crate::provider::inline_think::route(
                            &mut (if resp.reasoning.is_empty()
                                && fallback.content.starts_with("<think>")
                            {
                                0u8
                            } else {
                                2u8
                            }),
                            &mut String::new(),
                            &fallback.content,
                        ) {
                            let kind = if is_think || !visible_tokens {
                                "orchestrator.thinking"
                            } else {
                                "orchestrator.token"
                            };
                            self.emit(session_id, kind, serde_json::json!({ "delta": part }));
                        }
                        return Ok(fallback.content);
                    }
                    return Ok(if resp.content.is_empty() {
                        acc
                    } else {
                        resp.content
                    });
                }
                Ok(Err(e)) => {
                    if !acc.is_empty() || e.to_string().contains("未配置 API Key") {
                        return Err(e);
                    }
                    self.failover
                        .cool(&pid, crate::provider::FAILOVER_COOLDOWN_SECS);
                    self.emit(
                        session_id,
                        "model.failover",
                        serde_json::json!({ "profile": pid, "error": e.to_string() }),
                    );
                    last_err = Some(e);
                }
                Err(e) => return Err(anyhow::anyhow!("流式任务失败: {e}")),
            }
        }
        // 候选链耗尽：用首候选做最后一次非流式尝试，仍失败才报错
        if let Some(last) = last_err {
            let (_, provider, model) =
                self.orch_candidates()
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| {
                        (
                            String::new(),
                            self.orch_provider.clone(),
                            self.orch_model.clone(),
                        )
                    });
            let mut req = ChatRequest::new(model, messages.to_vec());
            req.key_hint = Some("__orch__".into());
            if let Ok(resp) = provider.chat(req).await {
                if !resp.reasoning.is_empty() {
                    self.emit(
                        session_id,
                        "orchestrator.thinking",
                        serde_json::json!({ "delta": resp.reasoning }),
                    );
                }
                self.emit(
                    session_id,
                    "orchestrator.token",
                    serde_json::json!({ "delta": resp.content }),
                );
                return Ok(resp.content);
            }
            return Err(last);
        }
        // 无候选链（池空）：全局档案直答
        let mut req = ChatRequest::new(self.orch_model.clone(), messages.to_vec());
        req.key_hint = Some("__orch__".into());
        let resp = self.orch_provider.chat(req).await?;
        self.emit(
            session_id,
            "orchestrator.token",
            serde_json::json!({ "delta": resp.content }),
        );
        Ok(resp.content)
    }
}
