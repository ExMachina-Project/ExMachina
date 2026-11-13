//! runtime 域分文件（自 runtime.rs 按域拆出，纯搬移零行为变更）。

use super::*;

impl AgentRuntime {
    /// 执行一个节点，产出 SyncReport。
    /// `chain` = 按个体/组默认模型展开的回退候选链（含全局档案兜底）。
    /// `on_delta` 用于把流式增量推给渠道（正文 / 思维链分轨）。
    /// `trace` = 过程数据（工具记录 / 工具事件）归集键：派发运行指向子会话；
    /// 用量记账与停止判定仍按 `session_id`（保持主会话口径）。
    /// `attach_images` = 目标模型具备视觉能力：工具副产物截图以图像消息回灌（computer use 闭环）。
    // 派发执行的完整上下文逐项透传，收拢参数会造一个只用一次的中间结构
    #[allow(clippy::too_many_arguments)]
    pub async fn execute<F>(
        &self,
        def: &AgentDefinition,
        order: &DispatchOrder,
        chain: crate::provider::ModelChain,
        session_id: &str,
        trace: &str,
        attach_images: bool,
        scope: ToolScope,
        mut on_delta: F,
    ) -> anyhow::Result<SyncReport>
    where
        F: FnMut(StreamDelta),
    {
        // 多 Key 粘性键位按个体区分：每个子个体用自己的 Key（限额才在该键位上前进）
        let agent_hint = Some(def.identifier.clone());
        // 系统提示词 = 职责提示词（内置最优，已含统一智械纪律）。
        // 子个体无人格层（SOUL 仅限用户面智能体：组主智能体 / 独立智能体）。
        let mut system_prompt = self.registry.load_prompt(&def.prompt_file)?;
        let boundary = scope.system_note();
        if !boundary.is_empty() {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(boundary);
        }
        // 主动搜索纪律（proactive-search）：搜索就绪时教会模型「何时该主动搜」——
        // 工具在表里但模型想不到用，是行为层缺口而非能力层
        if scope.allows_tools() && self.tools.search_discipline_ready() {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(crate::autonomy::SEARCH_DISCIPLINE);
        }

        let max_steps = order.constraints.max_steps.max(1);
        // 派发时限（指挥体按步数分档声明，timeout_ms = 步数 × 30s）：0 = 不限（旧调用方兼容）。
        // 没有这道闸，挂死的 LLM 流会让节点无限期占住编排并发槽，整轮调度随之卡死——
        // max_steps 只约束「思考-调工具」轮数，约束不了单次 LLM 调用内部的无限等待。
        let deadline = (order.constraints.timeout_ms > 0).then(|| {
            std::time::Instant::now()
                + std::time::Duration::from_millis(order.constraints.timeout_ms)
        });
        let mut rewrites: u32 = 0;
        let mut usage_prompt: u64 = 0;
        let mut usage_completion: u64 = 0;
        let mut last_model = String::new();
        let mut budget_warned = false;
        // 原生 function calling：按白名单生成工具 schema + 该个体可见的 MCP 工具
        // （web_search / computer 仅在后端就绪时下发——不承诺不存在的能力；
        //   文件记忆模式下 memory_write/link 必然失败，同样不下发）
        let scoped_allowlist = scope.filter_allowlist(&order.tool_allowlist);
        let mut specs = crate::tools::ToolGateway::tool_specs_gated(
            &scoped_allowlist,
            self.tools.search_ready(),
            self.tools.browser_ready(),
            self.tools.computer_ready(),
            self.tools.speech_ready(),
            self.tools.image_gen_ready(),
            self.tools.chart_ready(),
            self.tools.memory_deep(),
        );
        specs = crate::tools::ToolGateway::tool_specs_experiments(
            specs,
            self.experiments.dynamic_compaction,
            self.experiments.sliding_context_window,
            self.experiments.meta_agent,
        );
        // 声明式自定义工具（按 agents 可见性）与 MCP 第三方工具一并下发；
        // 仅聊天档连 schema 都不下发——不承诺、不执行任何工具。
        if scope.allows_tools() {
            specs.extend(self.tools.custom_specs_for(&def.identifier));
            specs.extend(self.tools.mcp().tool_snapshot_for(&def.identifier));
            // 会话级注入工具（视频观看会话等按会话收窄的插件工具）
            specs.extend(self.tools.session_tool_specs(session_id));
        }
        // 工具面清单注入（防「能力幻觉」拒绝）：与 chat_execute 同口径
        if scope.allows_tools() && !specs.is_empty() {
            let names: Vec<String> = specs.iter().map(|s| s.name.clone()).collect();
            system_prompt.push_str("\n\n");
            system_prompt.push_str(&tool_surface_note(&names));
        }
        let order_json = serde_json::to_string_pretty(order)?;
        let mut messages = vec![
            ChatMessage::system(system_prompt),
            ChatMessage::user(format!(
                "【要求】以下是本节点的调度指令（DispatchOrder）。严格在边界内执行，最终只输出 SyncReport JSON。\n```json\n{order_json}\n```"
            )),
        ];
        // 步数预算预告（unit-step-budget）：步数是硬上限，用完即失败回流——
        // 模型不知道预算多大就会在外围探索里烧完步数（部署实测：interaction-agent
        // 16 步全花在「找怎么发审批」的路上的外围翻查，44 个工具调用零收束）。
        // 开局告知预算 + 提前规划；临近耗尽强提醒立即收束。
        let total_steps = max_steps;
        messages.push(ChatMessage::user(format!(
            "【步数预算】本次执行最多 {total_steps} 轮「思考-调工具」。请规划好路径尽快达成目标：探索类操作尽量合并（一次 list 看全目录而非逐层翻），避免反复试错；第 {} 轮起必须开始收束，产出 SyncReport JSON。",
            total_steps.saturating_sub(2).max(1)
        )));

        for _step in 0..max_steps {
            // 停止检查点：会话被请求停止时终止本节点执行
            if crate::round_trace::is_cancelled(session_id) {
                anyhow::bail!("本轮已被用户停止");
            }
            // 临近预算耗尽的强提醒（unit-step-budget）：还剩 2 轮时注入收束通牒——
            // 模型常把步数当「软提示」无限探索，不点名剩余量就会一路烧到底。
            // 注入一次即可（重复注入徒增上下文）。
            let remain = max_steps - _step;
            if remain == 2 && !budget_warned {
                budget_warned = true;
                messages.push(ChatMessage::user(
                    "【预算警告】剩余 2 轮。不要再发起新的探索或工具调用（除非是收束必需的最后一步验证）：立即汇总已有信息，下一轮输出 SyncReport JSON。已有的部分成果在报告中如实陈述即可，未完成项列入 blockers。".to_string(),
                ));
            }
            // 时限检查点：派发预算耗尽即快速失败（把并发槽还给调度器），而非等 LLM 挂到天荒地老
            if let Some(dl) = deadline {
                if std::time::Instant::now() >= dl {
                    self.tools.record_usage(
                        session_id,
                        usage_prompt,
                        usage_completion,
                        &last_model,
                    );
                    anyhow::bail!(
                        "个体 {} 超过派发时限 {}ms（{} 步内未产出合法 SyncReport）",
                        def.identifier,
                        order.constraints.timeout_ms,
                        _step
                    );
                }
            }
            let (text, calls, usage) = match self
                .call_llm(
                    &messages,
                    &agent_hint,
                    &chain,
                    &specs,
                    deadline,
                    &mut on_delta,
                )
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    // LLM 调用失败（含超时）：已消耗的用量照记，失败原因回流调度器
                    self.tools.record_usage(
                        session_id,
                        usage_prompt,
                        usage_completion,
                        &last_model,
                    );
                    return Err(e);
                }
            };
            usage_prompt += usage.0;
            usage_completion += usage.1;
            if let Some((_, _, m)) = chain.candidates.first() {
                last_model = m.clone();
            }

            // 1a) 原生 function calling：协议级工具调用
            //     只读工具（read/grep/glob/web_fetch/web_search）并发执行；写工具按原顺序串行
            if !calls.is_empty() {
                let mut assistant = ChatMessage::assistant(text.clone());
                assistant.tool_calls = calls.clone();
                messages.push(assistant);
                let feedbacks = self
                    .run_tool_calls(
                        &def.identifier,
                        trace,
                        &scoped_allowlist,
                        &calls,
                        attach_images,
                        scope,
                    )
                    .await;
                for (c, (feedback, images)) in calls.iter().zip(feedbacks) {
                    let mut msg = ChatMessage::tool_result(&c.id, feedback);
                    msg.name = Some(c.name.clone());
                    messages.push(msg);
                    // 视觉回灌：截图以 user 消息附带（OpenAI 兼容端点不支持 tool 消息带图）
                    if let Some((note, data_urls)) = images {
                        let mut um = ChatMessage::user(note);
                        um.images = data_urls;
                        messages.push(um);
                    }
                }
                continue;
            }

            // 1b) 文本协议兜底（Mock / 不支持原生调用的端点）；仅聊天档不解析工具调用
            if scope.allows_tools() {
                if let Some(raw) = parse::extract_json(&text) {
                    if let Some((tool, args)) = parse::as_tool_call(&raw) {
                        let result = self
                            .tools
                            .execute(&def.identifier, trace, &scoped_allowlist, tool, &args)
                            .await;
                        messages.push(ChatMessage::assistant(text));
                        let feedback = if result.ok {
                            format!("【报告】工具 {} 执行结果：\n{}", tool.key(), result.output)
                        } else {
                            format!(
                                "【警告】工具 {} 执行失败：{}",
                                tool.key(),
                                result.error.unwrap_or_default()
                            )
                        };
                        messages.push(ChatMessage::user(feedback));
                        if attach_images {
                            if let Some((note, data_urls)) =
                                self.image_note(tool.key(), &result.images)
                            {
                                let mut um = ChatMessage::user(note);
                                um.images = data_urls;
                                messages.push(um);
                            }
                        }
                        continue;
                    }
                }
            }

            // 2) 回流契约校验
            match parse::parse_sync_report(&text) {
                Ok(mut report) => {
                    // 归属强制校正
                    report.source_agent = def.identifier.clone();
                    report.task_node_id = order.task_node_id.clone();
                    self.tools.record_usage(
                        session_id,
                        usage_prompt,
                        usage_completion,
                        &last_model,
                    );
                    return Ok(report);
                }
                Err(err) => {
                    rewrites += 1;
                    if rewrites > SYNC_REWRITE_LIMIT {
                        anyhow::bail!("SyncReport 契约连续校验失败（{rewrites} 次）：{err}");
                    }
                    messages.push(ChatMessage::assistant(text));
                    messages.push(ChatMessage::user(format!(
                        "【警告】上一次输出未通过校验：{err}\n【要求】仅输出一个符合 SyncReport 契约的 JSON 代码块，不要输出其他内容。"
                    )));
                }
            }
        }

        self.tools
            .record_usage(session_id, usage_prompt, usage_completion, &last_model);
        // 步数耗尽降级回流（unit-step-budget）：不再直接判死——用已收集的
        // 过程轨迹构造 blocked SyncReport。部署实测：个体 16 步里做了 44 个
        // 工具调用的大量真实工作，只差「最终收束」一步，判死等于把成果全扔
        // 掉、指挥体只拿到一行失败串。降级报告如实陈述：已完成工作进 summary
        // 与 statements，未达成目标进 blockers，指挥体可据实裁决（补步数重派
        // / 换路径 / 放弃）。
        let tool_trace = crate::round_trace::snapshot_tool_trace(session_id);
        let tool_n = crate::round_trace::tool_count(session_id);
        let work_note = if tool_n > 0 {
            format!("已执行 {tool_n} 次工具调用（最近轨迹：{tool_trace}）")
        } else {
            "未留下工具调用记录".to_string()
        };
        Ok(SyncReport {
            source_agent: def.identifier.clone(),
            task_node_id: order.task_node_id.clone(),
            status: crate::types::SyncStatus::Blocked,
            statements: vec![
                crate::types::Statement::report(format!(
                    "步数预算（{max_steps} 轮）耗尽，未能产出最终收束；{}。",
                    work_note
                )),
                crate::types::Statement::warn(
                    "预算内完成的工作因缺少最终收束而状态未知；未完成项见 blockers。",
                ),
            ],
            summary: format!(
                "步数预算耗尽降级回流：{work_note}。目标「{}」未在预算内完成收束。",
                node_objective_of(order)
            ),
            evidence: Vec::new(),
            risks: Vec::new(),
            blockers: vec![crate::types::BlockerItem {
                reason: format!(
                    "步数预算 {max_steps} 轮耗尽时未产出合法 SyncReport；最后行为：{tool_trace}"
                ),
                unblock_condition: "追加步数预算重派，或拆分目标缩小单次执行边界".into(),
            }],
            conflicts: None,
            next_suggestion: crate::types::NextSuggestion::default(),
            confidence: 0.3,
        })
    }

    /// 截图回灌材料：读文件 → data URL；不可读时返回 None
    pub(crate) fn image_note(&self, tool: &str, images: &[String]) -> Option<(String, Vec<String>)> {
        if images.is_empty() {
            return None;
        }
        let mut data_urls = Vec::new();
        for p in images {
            if let Some(url) = self.tools.read_image_data_url(p) {
                data_urls.push(url);
            }
        }
        if data_urls.is_empty() {
            return None;
        }
        Some((
            format!(
                "【附图】以下是工具 {tool} 的执行截图（已作为图像附带）。请直接观察画面内容并决定下一步。"
            ),
            data_urls,
        ))
    }

    /// 直接对话执行(常见单 agent 应用同款):系统提示 + 会话历史 + 工具循环,
    /// 无派发概念、无 SyncReport 契约——模型产出不含工具调用的文本即为终点。
    /// 供智能体模式使用;工具面 = 个体 allowlist + 可见自定义工具 + MCP。
    /// `trace` = 过程数据归集键（单聊与用量同键）。
    // 会话执行的全部上下文逐项透传，收拢参数会造一个只用一次的中间结构
    #[allow(clippy::too_many_arguments)]
    pub async fn chat_execute<F>(
        &self,
        def: &AgentDefinition,
        system_prompt: String,
        history: Vec<ChatMessage>,
        session_id: &str,
        trace: &str,
        chain: crate::provider::ModelChain,
        attach_images: bool,
        scope: ToolScope,
        mut on_delta: F,
    ) -> anyhow::Result<String>
    where
        F: FnMut(StreamDelta),
    {
        let agent_hint = Some(def.identifier.clone());
        let mut system_prompt = system_prompt;
        // 单轮工具预算清零（agent-budget-audit）：每轮用户输入一个新预算周期
        self.tools.reset_turn_tool_budget(session_id);
        let boundary = scope.system_note();
        if !boundary.is_empty() {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(boundary);
        }
        // 主动搜索纪律（proactive-search）：搜索就绪时教会模型「何时该主动搜」——
        // 工具在表里但模型想不到用，是行为层缺口而非能力层
        if scope.allows_tools() && self.tools.search_discipline_ready() {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(crate::autonomy::SEARCH_DISCIPLINE);
        }
        // 任务清单注入（spec: agent-todo）：清单非空时每轮带上进度全景（✅/🔄/⬜）
        if let Some(todo_block) = crate::todo::render(session_id) {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(&todo_block);
        }
        // 后台子任务未回收提醒（spec: subagent-spawn）：已终结未读的终态摘要，
        // 模型不必主动查询也知道有结果待收（限 5 条）
        if let Some(spawn_note) = self.tools.spawn_pending_note(session_id) {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(&spawn_note);
        }

        // 步数上限三级覆盖：个体 maxSteps > 作用域档位偏好 > 全局 security.chatMaxSteps
        // （Chat 档无工具循环，对话轮保护取半；Tools/Computer 档用全局默认）
        // 会话级预算（自动任务「防失控」面）优先级最高：通道配置显式收紧时生效
        let max_steps = self
            .tools
            .session_step_budget(session_id)
            .map(|v| v.max(1))
            .unwrap_or_else(|| {
                resolve_max_steps(def.max_steps, scope, self.tools.chat_max_steps())
            });
        // 步数耗尽时回显已完成的工具调用清单（便于归因）
        let mut done_tools: Vec<String> = Vec::new();
        let mut usage_prompt: u64 = 0;
        let mut usage_completion: u64 = 0;
        let mut last_model = String::new();
        // 元认知反思态（每轮一次上限）
        let mut consecutive_failures: u32 = 0;
        let mut reflected = false;

        // 原生 function calling：按白名单生成工具 schema + 该个体可见的 MCP 工具
        // （后端未就绪 / 模式不支持的工具不下发——不承诺不存在的能力）
        let scoped_allowlist = scope.filter_allowlist(&def.tools);
        let mut specs = crate::tools::ToolGateway::tool_specs_gated(
            &scoped_allowlist,
            self.tools.search_ready(),
            self.tools.browser_ready(),
            self.tools.computer_ready(),
            self.tools.speech_ready(),
            self.tools.image_gen_ready(),
            self.tools.chart_ready(),
            self.tools.memory_deep(),
        );
        specs = crate::tools::ToolGateway::tool_specs_experiments(
            specs,
            self.experiments.dynamic_compaction,
            self.experiments.sliding_context_window,
            self.experiments.meta_agent,
        );
        // 仅聊天档连自定义 / MCP schema 都不下发
        if scope.allows_tools() {
            specs.extend(self.tools.custom_specs_for(&def.identifier));
            specs.extend(self.tools.mcp().tool_snapshot_for(&def.identifier));
            // 会话级注入工具（视频观看会话等按会话收窄的插件工具）
            specs.extend(self.tools.session_tool_specs(session_id));
        }
        // 工具面清单注入（防「能力幻觉」拒绝）：把本轮实际生效的工具名列进系统提示。
        // 部署实测：工具 schema 已在请求里，但模型不「看见」清单时会臆断
        // 「本机无文件系统访问权限」并拒绝执行——显式清单让能力自述有据可依。
        if scope.allows_tools() && !specs.is_empty() {
            let names: Vec<String> = specs.iter().map(|s| s.name.clone()).collect();
            system_prompt.push_str("\n\n");
            system_prompt.push_str(&tool_surface_note(&names));
        }
        let mut messages = vec![ChatMessage::system(system_prompt)];
        messages.extend(history);

        for _step in 0..max_steps {
            // 停止检查点:会话被请求停止时终止本循环
            if crate::round_trace::is_cancelled(session_id) {
                anyhow::bail!("本轮已被用户停止");
            }
            let (text, calls, usage) = self
                .call_llm(&messages, &agent_hint, &chain, &specs, None, &mut on_delta)
                .await?;
            usage_prompt += usage.0;
            usage_completion += usage.1;
            if let Some((_, _, m)) = chain.candidates.first() {
                last_model = m.clone();
            }

            // 原生 function calling:只读并发 / 写串行,结果回灌后继续
            if !calls.is_empty() {
                let mut assistant = ChatMessage::assistant(text.clone());
                assistant.tool_calls = calls.clone();
                messages.push(assistant);
                for c in &calls {
                    done_tools.push(c.name.clone());
                }
                let feedbacks = self
                    .run_tool_calls(
                        &def.identifier,
                        trace,
                        &scoped_allowlist,
                        &calls,
                        attach_images,
                        scope,
                    )
                    .await;
                for (c, (feedback, images)) in calls.iter().zip(feedbacks) {
                    let mut msg = ChatMessage::tool_result(&c.id, feedback);
                    msg.name = Some(c.name.clone());
                    messages.push(msg);
                    if let Some((note, data_urls)) = images {
                        let mut um = ChatMessage::user(note);
                        um.images = data_urls;
                        messages.push(um);
                    }
                }
                continue;
            }

            // 文本协议兜底(Mock / 不支持原生调用的端点)；仅聊天档不解析工具调用
            if scope.allows_tools() {
                if let Some(raw) = parse::extract_json(&text) {
                    if let Some((tool, args)) = parse::as_tool_call(&raw) {
                        let result = self
                            .tools
                            .execute(&def.identifier, trace, &scoped_allowlist, tool, &args)
                            .await;
                        done_tools.push(tool.key().to_string());
                        messages.push(ChatMessage::assistant(text));
                        let feedback = if result.ok {
                            format!("【报告】工具 {} 执行结果：\n{}", tool.key(), result.output)
                        } else {
                            format!(
                                "【警告】工具 {} 执行失败：{}",
                                tool.key(),
                                result.error.unwrap_or_default()
                            )
                        };
                        messages.push(ChatMessage::user(feedback));
                        if attach_images {
                            if let Some((note, data_urls)) =
                                self.image_note(tool.key(), &result.images)
                            {
                                let mut um = ChatMessage::user(note);
                                um.images = data_urls;
                                messages.push(um);
                            }
                        }
                        // 元认知反思钩子（实验性 metaAgent）：连续失败 ≥2 时触发一次
                        // 反思调用（策略 JSON 注入后续循环;失败静默降级 continue）
                        if self.experiments.meta_agent && !reflected && !result.ok {
                            consecutive_failures += 1;
                            if consecutive_failures >= 2 {
                                reflected = true;
                                if let Some(hint) = self
                                    .meta_reflect(session_id, &consecutive_failures, &done_tools)
                                    .await
                                {
                                    messages.push(ChatMessage::user(hint));
                                }
                            }
                        } else if result.ok {
                            consecutive_failures = 0;
                        }
                        continue;
                    }
                }
            }

            // 无工具调用 = 最终回复
            self.tools
                .record_usage(session_id, usage_prompt, usage_completion, &last_model);
            return Ok(text);
        }

        // 步数预算耗尽的软着陆：注入收束提示再给最后一轮纯文本机会——
        // 基于已有材料产出结论而非硬截断丢弃（视频观看轮次的观感收束依赖此路径）
        let wrap_note = ChatMessage::user(
            "【系统提示】工具调用步数预算已用完。不要再调用任何工具，直接基于已获取的材料给出最终结论。",
        );
        messages.push(wrap_note);
        let (text, _calls, usage) = self
            .call_llm(&messages, &agent_hint, &chain, &[], None, &mut on_delta)
            .await?;
        usage_prompt += usage.0;
        usage_completion += usage.1;
        self.tools
            .record_usage(session_id, usage_prompt, usage_completion, &last_model);
        if !text.trim().is_empty() {
            return Ok(text);
        }
        anyhow::bail!(
            "个体 {} 已达最大工具步数 {max_steps} 仍未产出最终回复；已完成的工具调用：{}",
            def.identifier,
            if done_tools.is_empty() {
                "（无）".to_string()
            } else {
                done_tools.join("、")
            }
        )
    }

    /// 元认知反思（实验性 metaAgent）：连续失败触发的单次非流式调用,
    /// 输出策略 JSON → 注入后续循环的系统提示。失败/解析失败 → None(静默降级)。
    pub(crate) async fn meta_reflect(
        &self,
        session_id: &str,
        failures: &u32,
        done_tools: &[String],
    ) -> Option<String> {
        let trace = crate::round_trace::snapshot_tool_trace(session_id);
        let req = crate::provider::ChatRequest::new(
            self.model.clone(),
            vec![
                crate::provider::ChatMessage::system(
                    "你是元认知反思器。基于本轮工具轨迹判断策略是否需要调整。只输出 JSON:\n{\"strategy\": \"replan\"|\"redirect\"|\"compact\"|\"continue\", \"note\": \"一句话说明\"}\nreplan=重新分解任务;redirect=换工具/换路径;compact=先压缩上下文;continue=保持现策略",
                ),
                crate::provider::ChatMessage::user(format!(
                    "连续失败 {failures} 次。已用工具: {done_tools:?}\n工具轨迹: {trace}"
                )),
            ],
        );
        let out = self.provider.chat(req).await.ok()?;
        let trimmed = out.content.trim();
        let start = trimmed.find('{')?;
        let end = trimmed.rfind('}')?;
        let v: serde_json::Value = serde_json::from_str(&trimmed[start..=end]).ok()?;
        let strategy = v.get("strategy")?.as_str()?.to_string();
        let note = v
            .get("note")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let hint = match strategy.as_str() {
            "replan" => "【元认知】连续失败提示当前分解方式可能有误:请重新审视任务分解,换一条更稳的路径。",
            "redirect" => "【元认知】连续失败提示该工具/路径不通:请更换工具或调整调用方式。",
            "compact" => "【元认知】上下文可能已混入大量失败噪声:请先理清已知有效信息,基于确定的事实继续。",
            _ => return None, // continue = 不注入
        };
        let _ = self.tools.events.send(crate::types::CoreEvent {
            kind: "meta.reflect".into(),
            session_id: session_id.to_string(),
            payload: serde_json::json!({
                "strategy": strategy, "note": note, "consecutiveFailures": failures,
            }),
        });
        Some(format!("{hint}（{note}）"))
    }

}
