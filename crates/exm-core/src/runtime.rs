//! 子个体运行时 —— docs/01 §2.3
//! 装配 → LLM 循环（流式：正文 + 思维链双轨）→ 工具调用约定 → SyncReport 契约校验 → 回流

use crate::parse;
use crate::provider::{ChatMessage, ChatRequest, LlmProvider, StreamDelta, ToolCall};
use crate::registry::LocalRegistry;
use crate::tools::ToolGateway;
use crate::types::{AgentDefinition, DispatchOrder, SyncReport, ToolName, ToolScope};
use std::sync::Arc;
use tokio::sync::mpsc::unbounded_channel;

pub const SYNC_REWRITE_LIMIT: u32 = 2;

/// 单个工具调用的回填材料：回填文本 + 截图回灌组（图注 + 图像 data URL 清单）
pub type ToolFeedback = (String, Option<(String, Vec<String>)>);

/// 工具结果 → 回填文本（【报告】成功 / 【警告】失败），语义与审计一致
fn tool_feedback(name: &str, r: &crate::tools::ToolResult) -> String {
    if r.ok {
        format!("【报告】工具 {name} 执行结果：\n{}", r.output)
    } else {
        format!(
            "【警告】工具 {name} 执行失败：{}",
            r.error.as_deref().unwrap_or_default()
        )
    }
}

/// 单智能体循环步数上限解析（三级覆盖）：
/// 个体 maxSteps > 作用域档位偏好（Chat 档取全局半数作对话轮保护）> 全局默认。
/// 全局 0 值视为无效回落 24（与 ToolGateway::chat_max_steps 同口径）。
pub fn resolve_max_steps(agent_override: Option<usize>, scope: ToolScope, global: usize) -> usize {
    let global = if global == 0 { 24 } else { global };
    let scope_pref = match scope {
        ToolScope::Chat => (global / 2).max(1),
        _ => global,
    };
    agent_override.filter(|v| *v > 0).unwrap_or(scope_pref).max(1)
}

/// 子个体执行器：持 Provider / 工具网关 / 注册表（注册表只读共享）
pub struct AgentRuntime {
    registry: Arc<LocalRegistry>,
    provider: Arc<dyn LlmProvider>,
    tools: Arc<ToolGateway>,
    model: String,
    /// 实验性开关快照（装配时写入；context_compact 下发门控用）
    experiments: crate::config::ExperimentsConfig,
}

impl AgentRuntime {
    pub fn new(
        registry: Arc<LocalRegistry>,
        provider: Arc<dyn LlmProvider>,
        tools: Arc<ToolGateway>,
        model: impl Into<String>,
    ) -> Self {
        AgentRuntime {
            registry,
            provider,
            tools,
            model: model.into(),
            experiments: crate::config::ExperimentsConfig::default(),
        }
    }

    /// 注入实验性开关（Core 装配后回接）
    pub fn with_experiments(mut self, experiments: crate::config::ExperimentsConfig) -> Self {
        self.experiments = experiments;
        self
    }

    /// Provider 共享（spawn 执行器构造回退链用）
    pub fn provider_clone(&self) -> Arc<dyn LlmProvider> {
        self.provider.clone()
    }

    /// 默认执行目标（全局生效档案）：未解析出个体/组专属模型时回退用
    pub fn default_target(&self) -> (Arc<dyn LlmProvider>, String) {
        (self.provider.clone(), self.model.clone())
    }

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
        let order_json = serde_json::to_string_pretty(order)?;
        let mut messages = vec![
            ChatMessage::system(system_prompt),
            ChatMessage::user(format!(
                "【要求】以下是本节点的调度指令（DispatchOrder）。严格在边界内执行，最终只输出 SyncReport JSON。\n```json\n{order_json}\n```"
            )),
        ];

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
        // 原生 function calling：按白名单生成工具 schema + 该个体可见的 MCP 工具
        // （web_search / computer 仅在后端就绪时下发——不承诺不存在的能力；
        //   文件记忆模式下 memory_write/link 必然失败，同样不下发）
        let scoped_allowlist = scope.filter_allowlist(&order.tool_allowlist);
        let mut specs = crate::tools::ToolGateway::tool_specs_gated(
            &scoped_allowlist,
            self.tools.search_ready(),
            self.tools.browser_ready(),
            self.tools.computer_ready(),
            self.tools.memory_deep(),
        );
        specs = crate::tools::ToolGateway::tool_specs_experiments(
            specs,
            self.experiments.dynamic_compaction,
        );
        // 声明式自定义工具（按 agents 可见性）与 MCP 第三方工具一并下发；
        // 仅聊天档连 schema 都不下发——不承诺、不执行任何工具。
        if scope.allows_tools() {
            specs.extend(self.tools.custom_specs_for(&def.identifier));
            specs.extend(self.tools.mcp().tool_snapshot_for(&def.identifier));
            // 会话级注入工具（视频观看会话等按会话收窄的插件工具）
            specs.extend(self.tools.session_tool_specs(session_id));
        }

        for _step in 0..max_steps {
            // 停止检查点：会话被请求停止时终止本节点执行
            if crate::round_trace::is_cancelled(session_id) {
                anyhow::bail!("本轮已被用户停止");
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
        anyhow::bail!(
            "个体 {} 超过最大步数 {max_steps} 仍未产出合法 SyncReport",
            def.identifier
        )
    }

    /// 截图回灌材料：读文件 → data URL；不可读时返回 None
    fn image_note(&self, tool: &str, images: &[String]) -> Option<(String, Vec<String>)> {
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
    pub async fn chat_execute<F>(        &self,
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
        let boundary = scope.system_note();
        if !boundary.is_empty() {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(boundary);
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
        let mut messages = vec![ChatMessage::system(system_prompt)];
        messages.extend(history);

        // 步数上限三级覆盖：个体 maxSteps > 作用域档位偏好 > 全局 security.chatMaxSteps
        // （Chat 档无工具循环，对话轮保护取半；Tools/Computer 档用全局默认）
        // 会话级预算（自动任务「防失控」面）优先级最高：通道配置显式收紧时生效
        let max_steps = self
            .tools
            .session_step_budget(session_id)
            .map(|v| v.max(1))
            .unwrap_or_else(|| resolve_max_steps(def.max_steps, scope, self.tools.chat_max_steps()));
        // 步数耗尽时回显已完成的工具调用清单（便于归因）
        let mut done_tools: Vec<String> = Vec::new();
        let mut usage_prompt: u64 = 0;
        let mut usage_completion: u64 = 0;
        let mut last_model = String::new();

        // 原生 function calling：按白名单生成工具 schema + 该个体可见的 MCP 工具
        // （后端未就绪 / 模式不支持的工具不下发——不承诺不存在的能力）
        let scoped_allowlist = scope.filter_allowlist(&def.tools);
        let mut specs = crate::tools::ToolGateway::tool_specs_gated(
            &scoped_allowlist,
            self.tools.search_ready(),
            self.tools.browser_ready(),
            self.tools.computer_ready(),
            self.tools.memory_deep(),
        );
        specs = crate::tools::ToolGateway::tool_specs_experiments(
            specs,
            self.experiments.dynamic_compaction,
        );
        // 仅聊天档连自定义 / MCP schema 都不下发
        if scope.allows_tools() {
            specs.extend(self.tools.custom_specs_for(&def.identifier));
            specs.extend(self.tools.mcp().tool_snapshot_for(&def.identifier));
            // 会话级注入工具（视频观看会话等按会话收窄的插件工具）
            specs.extend(self.tools.session_tool_specs(session_id));
        }

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

    /// 同轮工具调用：只读工具并发（join_all），写工具与 MCP 按原顺序串行。
    /// 返回与 `calls` 同序的 `ToolFeedback`——保证与 tool_call_id 配对。
    /// `trace` = 工具记录与工具事件的归集键（派发运行指向子会话）。
    async fn run_tool_calls(
        &self,
        agent_id: &str,
        trace: &str,
        allowlist: &[ToolName],
        calls: &[ToolCall],
        attach_images: bool,
        scope: ToolScope,
    ) -> Vec<ToolFeedback> {
        // 仅聊天档：即使模型绕过 schema 硬造 tool call，也在执行层全部拒绝。
        if !scope.allows_tools() {
            return calls
                .iter()
                .map(|c| {
                    (
                        format!(
                            "【警告】当前通道能力边界为「仅聊天」，工具 {} 被拒绝执行。",
                            c.name
                        ),
                        None,
                    )
                })
                .collect();
        }
        let mut out: Vec<Option<ToolFeedback>> = vec![None; calls.len()];
        let mut readonly: Vec<usize> = Vec::new();
        let mut serial: Vec<usize> = Vec::new();
        for (i, c) in calls.iter().enumerate() {
            let ro = !c.name.starts_with("mcp:")
                && ToolName::parse(&c.name)
                    .map(|t| t.is_readonly())
                    .unwrap_or(false);
            if ro {
                readonly.push(i);
            } else {
                serial.push(i);
            }
        }
        // 只读并发
        if !readonly.is_empty() {
            let futs = readonly.iter().map(|i| {
                let c = &calls[*i];
                async move {
                    let tool = ToolName::parse(&c.name).unwrap_or(ToolName::Read);
                    let r = self
                        .tools
                        .execute(agent_id, trace, allowlist, tool, &c.arguments)
                        .await;
                    (tool_feedback(&c.name, &r), None)
                }
            });
            let results = futures_util::future::join_all(futs).await;
            for (i, pair) in readonly.iter().zip(results) {
                out[*i] = Some(pair);
            }
        }
        // 写 / MCP / 自定义 / 未知：串行
        for i in serial {
            let c = &calls[i];
            let (r, name) = if ToolName::parse(&c.name) == Some(ToolName::Computer)
                && !scope.allows_computer()
            {
                // 工具档硬闸门：即使 allowlist 意外混入 computer，也不执行桌面操作
                (
                    crate::tools::ToolResult {
                        ok: false,
                        output: String::new(),
                        error: Some("当前通道未授权电脑操控（computer）".into()),
                        images: Vec::new(),
                    },
                    c.name.clone(),
                )
            } else if c.name.starts_with("mcp:") {
                (
                    self.tools
                        .execute_mcp(agent_id, trace, &c.name, &c.arguments)
                        .await,
                    c.name.clone(),
                )
            } else {
                // 统一入口：内置按白名单放行，声明式自定义按 agents 可见性放行
                (
                    self.tools
                        .execute_named(agent_id, trace, allowlist, &c.name, &c.arguments)
                        .await,
                    c.name.clone(),
                )
            };
            let images = if attach_images {
                self.image_note(&name, &r.images)
            } else {
                None
            };
            out[i] = Some((tool_feedback(&name, &r), images));
        }
        out.into_iter().map(|x| x.unwrap_or_default()).collect()
    }

    /// `deadline` = 派发时限（Some = execute 路径按 DispatchOrder 预算收口；None = 不限时）。
    /// 挂死的 SSE 连接在剩余预算内等不到收尾时：abort 流任务并报可诊断错误，节点不被无限拖住。
    async fn call_llm<F>(
        &self,
        messages: &[ChatMessage],
        hint: &Option<String>,
        chain: &crate::provider::ModelChain,
        tools: &[crate::provider::ToolSpec],
        deadline: Option<std::time::Instant>,
        on_delta: &mut F,
    ) -> anyhow::Result<(String, Vec<ToolCall>, (u64, u64))>
    where
        F: FnMut(StreamDelta),
    {
        // 候选链为空 → 运行时默认（全局生效档案）
        let mut candidates: Vec<(String, Arc<dyn LlmProvider>, String)> = chain.candidates.clone();
        if candidates.is_empty() {
            let (p, m) = self.default_target();
            candidates.push((String::new(), p, m));
        }
        let mut last_err: Option<anyhow::Error> = None;
        for (pid, provider, model) in candidates {
            let mut req = ChatRequest::new(model.clone(), messages.to_vec());
            req.key_hint = hint.clone();
            if !tools.is_empty() {
                req.tools = tools.to_vec();
            }
            let (tx, mut rx) = unbounded_channel::<StreamDelta>();
            let req_clone = req.clone();
            let stream_provider = provider.clone();

            // 注意：tx 必须被 move 进任务（不能保留原 sender），否则 rx 永不关闭 → 自锁
            let handle = tokio::spawn(async move { stream_provider.stream(req_clone, tx).await });

            let mut emitted = false;
            // 有界收增量：到点（deadline）或流自然关闭即出循环；超时路径 abort 流任务，
            // 连接随任务取消而释放（不残留读端拖住运行时收尾）
            let drain = async {
                while let Some(delta) = rx.recv().await {
                    emitted = true;
                    on_delta(delta);
                }
            };
            match deadline {
                Some(dl) => {
                    if tokio::time::timeout_at(tokio::time::Instant::from_std(dl), drain)
                        .await
                        .is_err()
                    {
                        handle.abort();
                        return Err(anyhow::anyhow!(
                            "LLM 调用超时（候选 {pid}/{model}）：时限内未完成流式响应，已中止"
                        ));
                    }
                }
                None => drain.await,
            }
            match handle.await {
                Ok(Ok(resp)) => {
                    if !pid.is_empty() {
                        chain.failover.clear(&pid);
                    }
                    if resp.content.trim().is_empty() && resp.tool_calls.is_empty() {
                        // 流式空响应退化非流式
                        let fallback = provider.chat(req).await?;
                        if !fallback.reasoning.is_empty() {
                            on_delta(StreamDelta::Thinking(fallback.reasoning.clone()));
                        }
                        on_delta(StreamDelta::Text(fallback.content.clone()));
                        let usage = (fallback.prompt_tokens, fallback.completion_tokens);
                        return Ok((fallback.content, fallback.tool_calls, usage));
                    }
                    let usage = (resp.prompt_tokens, resp.completion_tokens);
                    return Ok((resp.content, resp.tool_calls, usage));
                }
                Ok(Err(e)) => {
                    // 已发出 token 的流不再重试（避免重复输出）；Mock 守卫错误直抛
                    if emitted || e.to_string().contains("未配置 API Key") {
                        return Err(e);
                    }
                    if !pid.is_empty() {
                        chain
                            .failover
                            .cool(&pid, crate::provider::FAILOVER_COOLDOWN_SECS);
                    }
                    last_err = Some(e);
                }
                Err(e) => return Err(anyhow::anyhow!("流式任务失败: {e}")),
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("无可用模型候选")))
    }
}

// ---------------------------------------------------------------- 派发时限测试

#[cfg(test)]
mod timeout_tests {
    use super::*;
    use crate::config::SecurityConfig;
    use crate::provider::{ChatResponse, FailoverState};
    use crate::types::{DispatchBoundary, DispatchConstraints, DispatchInput, DispatchOrder};
    use std::time::Duration;

    /// 挂死流替身：永不产出增量、长眠不醒（模拟端点挂死 / 网络黑洞）
    struct HangingStreamProvider;

    #[async_trait::async_trait]
    impl LlmProvider for HangingStreamProvider {
        fn name(&self) -> &'static str {
            "hanging"
        }
        async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok(ChatResponse::default())
        }
        async fn stream(
            &self,
            _req: ChatRequest,
            _tx: tokio::sync::mpsc::UnboundedSender<StreamDelta>,
        ) -> anyhow::Result<ChatResponse> {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok(ChatResponse::default())
        }
    }

    fn runtime_with(provider: Arc<dyn LlmProvider>) -> AgentRuntime {
        let base = std::env::temp_dir().join(format!("exm-rt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let reg_dir = base.join("entities");
        std::fs::create_dir_all(reg_dir.join("prompts")).unwrap();
        let meta = crate::types::GroupMeta {
            id: "exmachina".into(),
            name: "测试组".into(),
            description: String::new(),
            primary: None,
            workspace: None,
            model: None,
            capabilities: None,
            builtin: true,
            created_at: crate::types::now_iso(),
        };
        std::fs::write(
            reg_dir.join("group.json"),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();
        // 提示词按组布局解析：groups/<gid>/prompts/<file>.md
        let prompts_dir = reg_dir.join("groups").join("exmachina").join("prompts");
        std::fs::create_dir_all(&prompts_dir).unwrap();
        std::fs::write(
            prompts_dir.join("timeout-agent.md"),
            "# 测试个体\n你是测试个体。",
        )
        .unwrap();
        let registry = Arc::new(LocalRegistry::new(&reg_dir).expect("临时注册表创建失败"));
        let store =
            Arc::new(crate::store::Store::open(base.join("data")).expect("临时存储创建失败"));
        let (events, _rx) = tokio::sync::broadcast::channel::<crate::types::CoreEvent>(16);
        let tools = Arc::new(ToolGateway::new(
            &base,
            store,
            registry.clone(),
            events,
            SecurityConfig::default(),
        ));
        AgentRuntime::new(registry, provider, tools, "test-model")
    }

    fn order_with(timeout_ms: u64) -> DispatchOrder {
        DispatchOrder {
            task_node_id: "T1".into(),
            objective: "时限演练".into(),
            acceptance: vec!["产出 SyncReport".into()],
            boundary: DispatchBoundary {
                in_scope: vec![],
                forbidden: vec![],
            },
            inputs: vec![DispatchInput {
                ref_id: "user".into(),
                kind: "userInput".into(),
                summary: "时限演练".into(),
            }],
            tool_allowlist: vec![],
            constraints: DispatchConstraints {
                max_steps: 3,
                timeout_ms,
            },
            report_format: "SyncReport".into(),
        }
    }

    fn def_with() -> AgentDefinition {
        serde_json::from_value(serde_json::json!({
            "name": "时限演练体", "identifier": "timeout-agent", "domain": "公共",
            "tier": "unit", "description": "派发时限演练", "capabilities": ["测试"],
            "promptFile": "timeout-agent.md"
        }))
        .unwrap()
    }

    /// 挂死的 LLM 流必须在派发时限内被掐断并报可诊断错误（回归：timeout_ms 声明无人执行，
    /// 节点无限期占住编排并发槽）
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 派发时限_挂死流被掐断并报错() {
        let rt = runtime_with(Arc::new(HangingStreamProvider));
        let def = def_with();
        let order = order_with(400); // 400ms 时限
        let chain = crate::provider::ModelChain {
            candidates: vec![(String::new(), Arc::new(HangingStreamProvider), "m".into())],
            failover: Arc::new(FailoverState::default()),
        };
        let started = std::time::Instant::now();
        let r = rt
            .execute(
                &def,
                &order,
                chain,
                "ses-timeout",
                "ses-timeout",
                false,
                ToolScope::Computer,
                |_| {},
            )
            .await;
        let elapsed = started.elapsed();
        let err = r.expect_err("挂死流应在时限内失败");
        assert!(
            err.to_string().contains("超时"),
            "错误应可诊断为超时: {err}"
        );
        assert!(
            elapsed >= Duration::from_millis(400) && elapsed < Duration::from_secs(10),
            "应约在时限处失败而非挂满 60s，实际 {elapsed:?}"
        );
    }

    /// 快速正常的 LLM 不应被时限误伤（时限内正常产出 SyncReport → 成功回流）
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 派发时限_正常流不受影响() {
        let rt = runtime_with(Arc::new(crate::provider::MockLlmProvider));
        let def = def_with();
        let order = order_with(10_000);
        let chain = crate::provider::ModelChain {
            candidates: vec![(
                String::new(),
                Arc::new(crate::provider::MockLlmProvider),
                "m".into(),
            )],
            failover: Arc::new(FailoverState::default()),
        };
        let report = rt
            .execute(
                &def,
                &order,
                chain,
                "ses-ok",
                "ses-ok",
                false,
                ToolScope::Computer,
                |_| {},
            )
            .await
            .expect("时限充裕时应正常执行");
        assert_eq!(report.task_node_id, "T1");
        assert_eq!(report.status, crate::types::SyncStatus::Done);
    }
}

// ---------------------------------------------------------------- 原生工具循环回归

/// 复现「agent 无法正常开发」：原生 function calling 的完整多轮工具循环。
/// 覆盖：同轮只读并发（read+grep）→ 写工具（edit）→ terminal → 工具结果回灌 → SyncReport。
/// Mock 通道只走文本协议，覆盖不到这条路径——真实端点（OpenAI 兼容）全走这里。
#[cfg(test)]
mod tool_loop_tests {
    use super::*;
    use crate::config::SecurityConfig;
    use crate::provider::{ChatResponse, FailoverState, ToolCall};
    use crate::types::{DispatchBoundary, DispatchConstraints, DispatchInput, SyncStatus};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 脚本化 Provider：按调用次序回放固定的工具调用序列（模拟真实编码模型的作业轨迹）
    struct ScriptedProvider {
        step: AtomicUsize,
        /// 每步收到的消息列表快照（role 序列），供回灌断言
        seen_roles: std::sync::Mutex<Vec<Vec<String>>>,
    }

    impl ScriptedProvider {
        fn new() -> Self {
            ScriptedProvider {
                step: AtomicUsize::new(0),
                seen_roles: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    fn resp(content: &str, calls: Vec<ToolCall>) -> ChatResponse {
        ChatResponse {
            content: content.to_string(),
            reasoning: String::new(),
            prompt_tokens: 10,
            completion_tokens: 10,
            tool_calls: calls,
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for ScriptedProvider {
        fn name(&self) -> &'static str {
            "scripted"
        }
        async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
            anyhow::bail!("脚本化通道只走流式")
        }
        async fn stream(
            &self,
            req: ChatRequest,
            _tx: tokio::sync::mpsc::UnboundedSender<StreamDelta>,
        ) -> anyhow::Result<ChatResponse> {
            self.seen_roles
                .lock()
                .unwrap()
                .push(req.messages.iter().map(|m| m.role.clone()).collect());
            let step = self.step.fetch_add(1, Ordering::SeqCst);
            let s = match step {
                0 => resp(
                    "",
                    vec![
                        ToolCall {
                            id: "c1".into(),
                            name: "read".into(),
                            arguments: serde_json::json!({"path": "src/app.rs"}),
                        },
                        ToolCall {
                            id: "c2".into(),
                            name: "grep".into(),
                            arguments: serde_json::json!({"pattern": "fn main"}),
                        },
                    ],
                ),
                1 => resp(
                    "",
                    vec![ToolCall {
                        id: "c3".into(),
                        name: "edit".into(),
                        arguments: serde_json::json!({
                            "path": "src/app.rs",
                            "old_string": "fn main() {}",
                            "new_string": "fn main() { println!(\"exm-ok\"); }"
                        }),
                    }],
                ),
                2 => resp(
                    "",
                    vec![ToolCall {
                        id: "c4".into(),
                        name: "terminal".into(),
                        arguments: serde_json::json!({"command": "ls"}),
                    }],
                ),
                _ => {
                    let report = serde_json::json!({
                        "sourceAgent": "coding-agent",
                        "taskNodeId": "T1",
                        "status": "done",
                        "statements": [{"tag": "报告", "text": "已完成", "evidenceLevel": "B"}],
                        "summary": "完成",
                        "evidence": [{"level": "B", "kind": "code", "ref": "src/app.rs"}],
                        "risks": [],
                        "confidence": 0.8
                    });
                    return Ok(resp(&format!("```json\n{report}\n```"), vec![]));
                }
            };
            Ok(s)
        }
    }

    fn runtime_with_files(provider: Arc<dyn LlmProvider>) -> (AgentRuntime, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("exm-loop-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(base.join("src")).unwrap();
        std::fs::write(base.join("src/app.rs"), "fn main() {}\n").unwrap();
        let reg_dir = base.join("entities");
        std::fs::create_dir_all(reg_dir.join("groups/exmachina/prompts")).unwrap();
        let meta = crate::types::GroupMeta {
            id: "exmachina".into(),
            name: "测试组".into(),
            description: String::new(),
            primary: None,
            workspace: None,
            model: None,
            capabilities: None,
            builtin: true,
            created_at: crate::types::now_iso(),
        };
        std::fs::write(
            reg_dir.join("group.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();
        std::fs::write(
            reg_dir.join("groups/exmachina/prompts/coding-agent.md"),
            "# 测试个体\n你是测试个体。",
        )
        .unwrap();
        let registry = Arc::new(LocalRegistry::new(&reg_dir).expect("临时注册表创建失败"));
        let store =
            Arc::new(crate::store::Store::open(base.join("data")).expect("临时存储创建失败"));
        let (events, _rx) = tokio::sync::broadcast::channel::<crate::types::CoreEvent>(16);
        let tools = Arc::new(ToolGateway::new(
            &base,
            store,
            registry.clone(),
            events,
            SecurityConfig::default(),
        ));
        (
            AgentRuntime::new(registry, provider, tools, "test-model"),
            base,
        )
    }

    fn coding_def() -> crate::types::AgentDefinition {
        serde_json::from_value(serde_json::json!({
            "name": "编码体", "identifier": "coding-agent", "domain": "实作域",
            "tier": "unit", "description": "编码", "capabilities": ["编码"],
            "tools": ["read", "grep", "glob", "edit", "patch", "filesystem", "terminal"],
            "promptFile": "coding-agent.md"
        }))
        .unwrap()
    }

    fn coding_order(allowlist: Vec<crate::types::ToolName>) -> DispatchOrder {
        DispatchOrder {
            task_node_id: "T1".into(),
            objective: "在 src/app.rs 加输出".into(),
            acceptance: vec!["文件被修改".into()],
            boundary: DispatchBoundary {
                in_scope: vec![],
                forbidden: vec![],
            },
            inputs: vec![DispatchInput {
                ref_id: "user".into(),
                kind: "userInput".into(),
                summary: "改文件".into(),
            }],
            tool_allowlist: allowlist,
            constraints: DispatchConstraints {
                max_steps: 8,
                timeout_ms: 0,
            },
            report_format: "SyncReport".into(),
        }
    }

    /// 完整编码作业轨迹：只读并发 → edit 落盘 → terminal → 工具结果回灌 → SyncReport 校验通过
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 原生工具循环_编码作业全链路() {
        let provider = Arc::new(ScriptedProvider::new());
        let (rt, base) = runtime_with_files(provider.clone());
        let def = coding_def();
        let order = coding_order(def.tools.clone());
        let chain = crate::provider::ModelChain {
            candidates: vec![(String::new(), provider.clone(), "m".into())],
            failover: Arc::new(FailoverState::default()),
        };
        let report = rt
            .execute(
                &def,
                &order,
                chain,
                "sess-loop",
                "trace-loop",
                false,
                ToolScope::Computer,
                |_| {},
            )
            .await
            .expect("原生工具循环应完整走通");
        assert_eq!(report.status, SyncStatus::Done);

        // edit 真的落盘了
        let content = std::fs::read_to_string(base.join("src/app.rs")).unwrap();
        assert!(
            content.contains("exm-ok"),
            "edit 应真实落盘，实际: {content}"
        );

        // 工具结果以 role=tool 回灌，assistant 的 tool_calls 有被记录
        let seen = provider.seen_roles.lock().unwrap();
        assert!(seen.len() >= 4, "应有多轮交互，实际 {} 轮", seen.len());
        let last = seen.last().unwrap();
        assert!(
            last.iter().any(|r| r == "tool"),
            "最终轮消息里应有工具结果回灌，实际 roles: {last:?}"
        );
        assert!(
            last.iter().any(|r| r == "assistant"),
            "应保留 assistant 工具调用记录"
        );
    }

    /// 白名单外的工具调用必须被拒且不炸循环：模型拿到错误回灌后仍能继续收敛
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 原生工具循环_白名单外工具被拒_循环不中断() {
        struct RogueProvider {
            step: AtomicUsize,
        }
        #[async_trait::async_trait]
        impl LlmProvider for RogueProvider {
            fn name(&self) -> &'static str {
                "rogue"
            }
            async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
                anyhow::bail!("只走流式")
            }
            async fn stream(
                &self,
                _req: ChatRequest,
                _tx: tokio::sync::mpsc::UnboundedSender<StreamDelta>,
            ) -> anyhow::Result<ChatResponse> {
                let step = self.step.fetch_add(1, Ordering::SeqCst);
                if step == 0 {
                    Ok(resp(
                        "",
                        vec![ToolCall {
                            id: "r1".into(),
                            name: "web_search".into(), // 不在 coding-agent 白名单
                            arguments: serde_json::json!({"query": "rust"}),
                        }],
                    ))
                } else {
                    let report = serde_json::json!({
                        "sourceAgent": "coding-agent", "taskNodeId": "T1", "status": "done",
                        "statements": [{"tag": "报告", "text": "收到拒绝后收敛", "evidenceLevel": "C"}],
                        "summary": "ok", "evidence": [], "risks": [], "confidence": 0.7
                    });
                    Ok(resp(&format!("```json\n{report}\n```"), vec![]))
                }
            }
        }
        let provider = Arc::new(RogueProvider {
            step: AtomicUsize::new(0),
        });
        let (rt, _base) = runtime_with_files(provider.clone());
        let def = coding_def();
        let order = coding_order(def.tools.clone());
        let chain = crate::provider::ModelChain {
            candidates: vec![(String::new(), provider.clone(), "m".into())],
            failover: Arc::new(FailoverState::default()),
        };
        let report = rt
            .execute(
                &def,
                &order,
                chain,
                "sess-rogue",
                "trace-rogue",
                false,
                ToolScope::Computer,
                |_| {},
            )
            .await
            .expect("白名单拒绝不应中断循环");
        assert_eq!(report.status, SyncStatus::Done);
    }
}

#[cfg(test)]
mod max_steps_tests {
    use super::resolve_max_steps;
    use crate::types::ToolScope;

    #[test]
    fn 步数覆盖优先级_个体最高() {
        // 个体显式 48 覆盖一切
        assert_eq!(resolve_max_steps(Some(48), ToolScope::Tools, 24), 48);
        assert_eq!(resolve_max_steps(Some(48), ToolScope::Chat, 24), 48);
    }

    #[test]
    fn 步数覆盖优先级_作用域档位次之() {
        // 无个体覆盖：Chat 档取半，Tools/Computer 用全局
        assert_eq!(resolve_max_steps(None, ToolScope::Chat, 24), 12);
        assert_eq!(resolve_max_steps(None, ToolScope::Tools, 24), 24);
        assert_eq!(resolve_max_steps(None, ToolScope::Computer, 24), 24);
    }

    #[test]
    fn 步数无效值兜底() {
        // 个体 0 / 全局 0 均视为未配置：回落默认
        assert_eq!(resolve_max_steps(Some(0), ToolScope::Tools, 24), 24);
        assert_eq!(resolve_max_steps(None, ToolScope::Tools, 0), 24);
        assert_eq!(resolve_max_steps(None, ToolScope::Chat, 0), 12);
        // Chat 档取半后至少 1
        assert_eq!(resolve_max_steps(None, ToolScope::Chat, 1), 1);
    }
}

#[cfg(test)]
mod soft_landing_tests {
    use super::*;
    use crate::config::SecurityConfig;
    use crate::provider::{ChatRequest, ChatResponse, ToolCall};
    use crate::types::AgentDefinition;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 永远想调工具的替身：直到看见收束提示才吐最终文本
    struct ToolLoopProvider {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl LlmProvider for ToolLoopProvider {
        fn name(&self) -> &'static str {
            "tool-loop"
        }
        async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
            let wrapped = req
                .messages
                .iter()
                .any(|m| m.content.contains("步数预算已用完"));
            if wrapped {
                return Ok(ChatResponse {
                    content: "最终结论：基于已看材料，视频值得一览。".into(),
                    reasoning: String::new(),
                    prompt_tokens: 10,
                    completion_tokens: 10,
                    tool_calls: Vec::new(),
                });
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ChatResponse {
                content: String::new(),
                reasoning: String::new(),
                prompt_tokens: 10,
                completion_tokens: 10,
                tool_calls: vec![ToolCall {
                    id: format!("c{}", self.calls.load(Ordering::SeqCst)),
                    name: "read".into(),
                    arguments: serde_json::json!({ "path": "/tmp/x" }),
                }],
            })
        }
        async fn embed(&self, _model: &str, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![0.0f32; 8]).collect())
        }
        async fn stream(
            &self,
            _req: ChatRequest,
            _tx: tokio::sync::mpsc::UnboundedSender<StreamDelta>,
        ) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse::default())
        }
    }

    fn base_rig(provider: Arc<dyn LlmProvider>) -> (AgentRuntime, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("exm-rt-sl-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let reg_dir = base.join("entities");
        std::fs::create_dir_all(reg_dir.join("prompts")).unwrap();
        let meta = crate::types::GroupMeta {
            id: "exmachina".into(),
            name: "测试组".into(),
            description: String::new(),
            primary: None,
            workspace: None,
            model: None,
            capabilities: None,
            builtin: true,
            created_at: crate::types::now_iso(),
        };
        std::fs::write(
            reg_dir.join("group.json"),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();
        let prompts_dir = reg_dir.join("groups").join("exmachina").join("prompts");
        std::fs::create_dir_all(&prompts_dir).unwrap();
        std::fs::write(prompts_dir.join("watch-agent.md"), "# 观众代理\n你是观众代理。").unwrap();
        let registry = Arc::new(LocalRegistry::new(&reg_dir).expect("临时注册表创建失败"));
        let store = Arc::new(crate::store::Store::open(base.join("data")).expect("临时存储创建失败"));
        let (events, _rx) = tokio::sync::broadcast::channel::<crate::types::CoreEvent>(16);
        let tools = Arc::new(ToolGateway::new(&base, store, registry.clone(), events, SecurityConfig::default()));
        (AgentRuntime::new(registry, provider, tools, "test-model"), base)
    }

    fn watch_def() -> AgentDefinition {
        serde_json::from_value(serde_json::json!({
            "name": "观众代理", "identifier": "watch-agent", "domain": "公共",
            "tier": "unit", "description": "观看演练", "capabilities": ["测试"],
            "promptFile": "watch-agent.md"
        }))
        .unwrap()
    }

    /// 步数耗尽软着陆：注入收束提示后智能体产出结论（不硬截断）
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 步数耗尽_软着陆产出结论() {
        let provider = Arc::new(ToolLoopProvider { calls: AtomicUsize::new(0) });
        let (rt, base) = base_rig(provider.clone());
        let def = watch_def();
        // 会话步数预算 = 2（模拟通道面 maxWatchSteps）
        rt.tools.set_session_step_budget("s-sl", 2);
        let out = rt
            .chat_execute(
                &def,
                "系统提示".into(),
                vec![],
                "s-sl",
                "test",
                crate::provider::ModelChain {
                    candidates: vec![("loop".into(), provider.clone(), "test-model".into())],
                    failover: std::sync::Arc::new(crate::provider::FailoverState::default()),
                },
                false,
                ToolScope::Tools,
                |_d| {},
            )
            .await
            .expect("软着陆应收束成功");
        assert!(out.contains("最终结论"), "应收束提示产出结论：{out}");
        assert!(provider.calls.load(Ordering::SeqCst) >= 2, "工具调用至少走了预算步数");
        let _ = std::fs::remove_dir_all(&base);
    }
}
