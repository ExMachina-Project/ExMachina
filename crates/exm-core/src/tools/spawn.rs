//! tools/spawn.rs —— 自 tools.rs 按领域拆出（纯搬移，零行为变更）

use super::*;

impl ToolGateway {
    /// 子智能体派生（spec: subagent-spawn）：
    /// - 独立上下文（空历史）、自包含任务书
    /// - 工具白名单强制剔除 spawn/agent_manage（单层限制）
    /// - 实例级并发信号量 + 独立超时（超限/超时明确说明）
    /// - 启动/完成/失败事件进主会话事件流（可观测）
    pub(crate) async fn tool_spawn(
        &self,
        agent_id: &str,
        session_id: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        // 后台回收面（spec: subagent-spawn 异步并行）：status/list 不走派生流程，
        // 也不依赖执行器装配（只读注册表）
        match args.get("op").and_then(|v| v.as_str()).unwrap_or("") {
            "status" => {
                let sid = args.get("spawnId").and_then(|v| v.as_str()).unwrap_or("");
                return self.spawn_bg_status(session_id, sid);
            }
            "list" => return self.spawn_bg_list(session_id),
            _ => {}
        }
        let Some(exec) = self.spawn_executor.read().clone() else {
            return ToolResult::err("spawn 执行器未装配（运行时未就绪）");
        };
        let task = args
            .get("task")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if task.is_empty() {
            return ToolResult::err("task 不能为空：请提供自包含任务书（目标、范围、期望产出）");
        }
        // 工具白名单：显式指定或默认只读；强制剔除 spawn/agent_manage
        let requested: Vec<ToolName> = args
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .filter_map(ToolName::parse)
                    .filter(|t| !matches!(t, ToolName::Spawn | ToolName::AgentManage))
                    .collect()
            })
            .unwrap_or_else(|| {
                vec![
                    ToolName::Read,
                    ToolName::Grep,
                    ToolName::Glob,
                    ToolName::WebFetch,
                ]
            });
        if requested.is_empty() {
            return ToolResult::err("工具白名单为空（或全部被剔除）：至少保留一个工具");
        }
        let timeout = args
            .get("timeoutSecs")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.spawn_timeout_secs)
            .clamp(5, 1800);
        let spawn_id = format!("sp-{}", &crate::types::new_id()[..8]);
        // 可观测：子任务启动事件
        self.emit_and_persist(CoreEvent {
            kind: "spawn.started".into(),
            session_id: session_id.to_string(),
            payload: serde_json::json!({
                "spawnId": spawn_id, "agentId": agent_id,
                "taskPreview": task.chars().take(120).collect::<String>(),
                "tools": requested.iter().map(|t| t.key()).collect::<Vec<_>>(),
                "background": args.get("background").and_then(|v| v.as_bool()).unwrap_or(false),
            }),
        });
        // 并发闸：try_acquire 即时反馈（排队语义由主智能体重试承担）
        let sem = self.spawn_semaphore.clone();
        let Ok(permit) = sem.clone().try_acquire_owned() else {
            return ToolResult::err(format!(
                "子智能体并发已达上限（{}）：请等待在跑子任务完成后再派生",
                sem.available_permits()
            ));
        };
        // 后台模式（spec: subagent-spawn）：立即返回 spawnId，许可随任务体持有至终结；
        // 终态写注册表 + spawn.finished 事件，主智能体期间可继续对话/派生/调工具
        if args
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let agent_owned = agent_id.to_string();
            let registry = self.spawns_bg.clone();
            let events = self.events.clone();
            // 持久化通道一并移入任务体：终态事件落 store（REST 事件面/WebUI 刷新后可见）
            let store = self.store.clone();
            let session = session_id.to_string();
            let task_preview: String = task.chars().take(120).collect();
            registry.lock().insert(
                spawn_id.clone(),
                SpawnBgTask {
                    spawn_id: spawn_id.clone(),
                    task_preview: task_preview.clone(),
                    session: session.clone(),
                    started: Instant::now(),
                    state: parking_lot::RwLock::new(SpawnBgState::Running),
                    read: parking_lot::RwLock::new(false),
                },
            );
            let task_owned = task.to_string();
            let sid_evt = spawn_id.clone();
            let _handle =
                tokio::spawn(async move {
                    let _p = permit; // 许可随任务体存活，终结自动释放
                    let fut = exec(&agent_owned, &session, task_owned.as_str(), requested);
                    let (state, status, preview) =
                        match tokio::time::timeout(std::time::Duration::from_secs(timeout), fut)
                            .await
                        {
                            Ok(Ok(text)) => {
                                let p: String = text.chars().take(160).collect();
                                (SpawnBgState::Done(text), "done", p)
                            }
                            Ok(Err(e)) => {
                                let msg = e.to_string();
                                (SpawnBgState::Failed(msg.clone()), "failed", msg)
                            }
                            Err(_) => (SpawnBgState::Timeout, "timeout", String::new()),
                        };
                    if let Some(t) = registry.lock().get(&sid_evt) {
                        *t.state.write() = state;
                    }
                    let evt = CoreEvent {
                        kind: "spawn.finished".into(),
                        session_id: session,
                        payload: serde_json::json!({
                            "spawnId": sid_evt, "status": status, "background": true,
                            "summaryPreview": preview,
                        }),
                    };
                    let _ = events.send(evt.clone());
                    // 落 store：REST 事件 API / WebUI 刷新后可查（与 emit_and_persist 同口径）
                    let env = crate::types::Envelope::new(
                        format!("session.{}.events", evt.session_id),
                        "tools",
                        "broadcast",
                        "event",
                        evt.session_id.clone(),
                        serde_json::to_value(&evt).unwrap_or(serde_json::Value::Null),
                    );
                    let _ = store.append_event(&env);
                });
            return ToolResult::ok(format!(
                "子任务已后台启动：spawnId={spawn_id}\n任务：{task_preview}\n用 spawn(op=\"status\", spawnId=\"{spawn_id}\") 查询结果；spawn(op=\"list\") 列全部。完成后下一轮上下文会自动提醒。"
            ));
        }
        let task_owned = task.to_string();
        let fut = exec(agent_id, session_id, task_owned.as_str(), requested);
        let result = match tokio::time::timeout(std::time::Duration::from_secs(timeout), fut).await
        {
            Ok(Ok(text)) => {
                self.emit_and_persist(CoreEvent {
                    kind: "spawn.finished".into(),
                    session_id: session_id.to_string(),
                    payload: serde_json::json!({
                        "spawnId": spawn_id, "status": "done",
                        "summaryPreview": text.chars().take(160).collect::<String>(),
                    }),
                });
                ToolResult::ok(text)
            }
            Ok(Err(e)) => {
                self.emit_and_persist(CoreEvent {
                    kind: "spawn.finished".into(),
                    session_id: session_id.to_string(),
                    payload: serde_json::json!({
                        "spawnId": spawn_id, "status": "failed", "error": e.to_string(),
                    }),
                });
                ToolResult::err(format!("子任务执行失败：{e}"))
            }
            Err(_) => {
                self.emit_and_persist(CoreEvent {
                    kind: "spawn.finished".into(),
                    session_id: session_id.to_string(),
                    payload: serde_json::json!({
                        "spawnId": spawn_id, "status": "timeout", "timeoutSecs": timeout,
                    }),
                });
                ToolResult::err(format!(
                    "子任务超时（{timeout} 秒）被终止：请缩小任务范围或调大 timeoutSecs"
                ))
            }
        };
        result
    }

    /// 动态上下文压缩（实验性，experiments.dynamicCompaction）：模型主动触发。
    /// 压缩执行体在 compaction.rs（compact_now），网关只做装配判定与结果转述。
    pub(crate) async fn tool_context_compact(
        &self,
        agent_id: &str,
        session_id: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        let Some((provider, model)) = self.compactor.read().clone() else {
            return ToolResult::err("压缩器未装配（运行时未就绪）");
        };
        let keep = args
            .get("keepRecent")
            .and_then(|v| v.as_u64())
            .unwrap_or(crate::compaction::COMPACT_KEEP as u64)
            .clamp(2, 100) as usize;
        let note = args
            .get("note")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        match crate::compaction::compact_now(
            &self.store,
            &provider,
            &model,
            session_id,
            keep,
            &note,
        )
        .await
        {
            Some((summary, n)) => {
                self.emit_and_persist(CoreEvent {
                    kind: "context.compacted".into(),
                    session_id: session_id.to_string(),
                    payload: serde_json::json!({
                        "agentId": agent_id, "messagesCompacted": n,
                        "keepRecent": keep, "summaryChars": summary.chars().count(),
                    }),
                });
                ToolResult::ok(format!(
                    "已压缩 {n} 条更早历史为滚动摘要（保留最近 {keep} 条原文）。摘要要点：{}",
                    summary.chars().take(300).collect::<String>()
                ))
            }
            None => ToolResult::ok(
                "窗口外没有待压缩的历史（已全部压过或条数不足）——无需压缩。".to_string(),
            ),
        }
    }
}
