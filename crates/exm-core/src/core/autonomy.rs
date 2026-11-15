//! core/autonomy.rs —— Core 实验体调度域（codebase-refactor 拆分）：
//! Auto Agent 空转巡检（auto_agent_tick）、Meta Agent 周期巡检（meta_agent_tick）、
//! 定时任务执行（run_cron_job）、事件提示触发（run_event_prompt）。
//! 纯搬移：方法签名与实现零改动（memory_digest_session 随记忆域在 core/memory.rs）。

use super::super::Core;
use crate::types::*;

impl Core {
    /// 注入一条事件提示词到目标组会话（事件触发器共用通路，design D6：
    /// 组切换 + 会话复用 + prompt 注入；与 cron 同路但独立记账，不进 cron 运行日志）。
    /// 返回执行的会话 id。标题缺省 = `event-<group>`（同组事件复用同一会话）。
    pub async fn run_event_prompt(
        &self,
        title: &str,
        group: Option<&str>,
        prompt: &str,
    ) -> anyhow::Result<String> {
        let prev_group = self.registry.active_group();
        let mut switched = false;
        if let Some(g) = group {
            if *g != prev_group && self.group_meta(g).is_some() {
                switched = self.registry.set_active_group(g).is_ok();
                // 抢占校验（竞态 P1-2）：切换后立即回读——被并发通道抢走则抢回一次
                if switched && self.registry.active_group() != *g {
                    let _ = self.registry.set_active_group(g);
                }
            }
        }
        let gid = self.registry.active_group();
        let title = if title.trim().is_empty() {
            format!("event-{gid}")
        } else {
            title.to_string()
        };
        let result = async {
            let existing = self
                .store
                .list_sessions_in_group(&gid)
                .ok()
                .and_then(|list| list.into_iter().find(|s| s.title == title));
            let session = match existing {
                Some(s) => s,
                None => self.create_session(&title)?,
            };
            self.chat(&session.id, prompt).await?;
            Ok::<String, anyhow::Error>(session.id)
        }
        .await;
        if switched {
            let _ = self.registry.set_active_group(&prev_group);
        }
        result
    }
    /// 执行一条定时任务：复用同名会话（缺省 job-<id>），注入提示词并等待收束
    pub async fn run_cron_job(&self, job: &CronJob) -> CronRun {
        let started = now_iso();
        let title = job
            .session_title
            .clone()
            .unwrap_or_else(|| format!("job-{}", job.id));
        // 组切换先行：任务声明的组决定会话归属与执行上下文（执行完恢复）
        let prev_group = self.registry.active_group();
        let mut switched = false;
        if let Some(g) = &job.group {
            if *g != prev_group && self.group_meta(g).is_some() {
                switched = self.registry.set_active_group(g).is_ok();
                // 抢占校验（竞态 P1-2）：切换后立即回读——被并发通道抢走则抢回一次
                if switched && self.registry.active_group() != *g {
                    let _ = self.registry.set_active_group(g);
                }
            }
        }
        // 会话复用（限定本组）：按标题匹配已有会话，避免每次运行新建。
        // 创建失败（磁盘满/库忙）不 panic——落 failed 记录 + 事件，调度循环继续（审计 P1-3）
        let gid = self.registry.active_group();
        let existing = self
            .store
            .list_sessions_in_group(&gid)
            .ok()
            .and_then(|list| list.into_iter().find(|s| s.title == title));
        let session = match existing {
            Some(s) => s,
            None => match self.create_session(&title) {
                Ok(s) => s,
                Err(e) => {
                    if switched {
                        let _ = self.registry.set_active_group(&prev_group);
                    }
                    let err = format!("会话创建失败：{e}");
                    let _ = self.events.send(CoreEvent {
                        kind: "cron.finished".into(),
                        session_id: String::new(),
                        payload: serde_json::json!({
                            "jobId": job.id,
                            "jobName": job.name,
                            "status": "failed",
                            "error": err,
                        }),
                    });
                    return CronRun {
                        id: new_id()[..8].to_string(),
                        job_id: job.id.clone(),
                        job_name: job.name.clone(),
                        session_id: String::new(),
                        started_at: started,
                        finished_at: now_iso(),
                        status: "failed".into(),
                        summary: err,
                    };
                }
            },
        };
        let result = self.chat(&session.id, &job.prompt).await;
        if switched {
            let _ = self.registry.set_active_group(&prev_group);
        }
        let status = if result.is_ok() { "done" } else { "failed" };
        let summary = match result {
            Ok(_) => "任务执行完成（详见会话与编排图）".to_string(),
            Err(e) => format!("执行失败：{e}"),
        };
        let run = CronRun {
            id: new_id()[..8].to_string(),
            job_id: job.id.clone(),
            job_name: job.name.clone(),
            session_id: session.id.clone(),
            started_at: started,
            finished_at: now_iso(),
            status: status.into(),
            summary,
        };
        let _ = self.cron.record_run(&run);
        let _ = self.events.send(CoreEvent {
            kind: "cron.finished".into(),
            session_id: session.id,
            payload: serde_json::json!({
                "jobId": run.job_id, "jobName": run.job_name,
                "status": run.status, "summary": run.summary,
            }),
        });
        run
    }
    /// Auto Agent 一次 tick：周期/退避判定 → Eval 自评 →（通过）自主行动。
    /// 返回 Some(任务) 表示本轮触发了行动；None = 未到周期 / Eval 无任务。
    /// 调度器每 20s 调用；内部按分钟粒度去重（同一分钟只评一次）。
    pub async fn auto_agent_tick(&self, now_minute: i64) -> Option<(String, String)> {
        let cfg = self.config();
        if !cfg.experiments.auto_agent {
            return None;
        }
        let (idle_streak, last_tick) = self.auto_agent_state();
        // 周期判定：距上次 tick 的分钟数 ≥ 退避间隔才评（首 tick last=0 直评）
        let due = crate::autonomy::auto_backoff_minutes(&cfg, idle_streak) as i64;
        if last_tick != 0 && now_minute - last_tick < due {
            return None;
        }
        self.auto_state.write().last_tick_minute = now_minute;
        // Eval：非流式一次调用（工具不可用——Eval 只读判断，行动轮才有工具）
        let orch = self.orchestrator();
        let req = crate::provider::ChatRequest::new(
            orch.orch_model.clone(),
            vec![
                crate::provider::ChatMessage::system(crate::autonomy::AUTO_EVAL_PROMPT),
                crate::provider::ChatMessage::user("执行一次自评。"),
            ],
        );
        let output = match orch.orch_provider.chat(req).await {
            Ok(r) => r.content,
            Err(e) => {
                orch.emit(
                    "",
                    "auto.eval",
                    serde_json::json!({ "ok": false, "error": e.to_string() }),
                );
                return None;
            }
        };
        let Some((task, why)) = crate::autonomy::parse_auto_eval(&output) else {
            // 无任务：空转计数 +1（退避拉长），发审计事件（广播 + 落库，事件页可查）
            self.auto_state.write().idle_streak += 1;
            orch.emit("", "auto.eval", serde_json::json!({ "ok": true, "act": false }));
            return None;
        };
        // Eval 通过：空传统计复位，开自主行动会话执行
        self.auto_state.write().idle_streak = 0;
        orch.emit(
            "",
            "auto.eval",
            serde_json::json!({ "ok": true, "act": true, "task": task, "why": why }),
        );
        let title = format!("auto-agent:{}", task.chars().take(30).collect::<String>());
        let session = self.create_session(&title).ok()?;
        let prompt = format!(
            "{}\n\n## 任务输入\n目标：{task}\n选定理由：{why}\n步数预算：{}",
            crate::autonomy::AUTO_ACT_DISCIPLINE,
            crate::autonomy::auto_act_max_steps(&cfg),
        );
        let result = self.chat(&session.id, &prompt).await;
        orch.emit(
            &session.id,
            "auto.act",
            serde_json::json!({
                "task": task,
                "status": if result.is_ok() { "done" } else { "failed" },
                "error": result.err().map(|e| e.to_string()),
            }),
        );
        Some((task, why))
    }
    /// Meta Agent 一次巡检 tick（实验性 experiments.metaAgent）：周期判定 →
    /// 数据快照注入的 Eval 自评 →（通过）开自主会话（装 meta 工具组）分析落提议。
    /// 调度器每 20s 调;内部分钟去重。0 = Some((focus, why)) 表示触发了巡检行动。
    pub async fn meta_agent_tick(&self, now_minute: i64) -> Option<(String, String)> {
        let cfg = self.config();
        if !cfg.experiments.meta_agent {
            return None;
        }
        let interval = cfg.automation.meta_interval_min.max(1) as i64;
        if interval == 0 {
            return None; // 0 = 仅会话召唤(被 max(1) 归一后由显式判断拦截——实际不可达,防御式)
        }
        let last = self.meta_state.read().last_tick_minute;
        if last != 0 && now_minute - last < interval {
            return None;
        }
        self.meta_state.write().last_tick_minute = now_minute;
        // 数据快照（Eval 输入）
        let orch = self.orchestrator();
        let obs = crate::meta::system_observation(&self.store, &orch.registry);
        let snapshot = serde_json::to_string_pretty(&obs).unwrap_or_default();
        // Eval 自评（非流式无工具）
        let req = crate::provider::ChatRequest::new(
            orch.orch_model.clone(),
            vec![
                crate::provider::ChatMessage::system(crate::meta::META_EVAL_PROMPT),
                crate::provider::ChatMessage::user(format!(
                    "运行数据快照（7 天窗口）:\n{snapshot}\n\n执行一次巡评。"
                )),
            ],
        );
        let output = match orch.orch_provider.chat(req).await {
            Ok(r) => r.content,
            Err(e) => {
                let _ = self.events.send(CoreEvent {
                    kind: "meta.eval".into(),
                    session_id: String::new(),
                    payload: serde_json::json!({ "ok": false, "error": e.to_string() }),
                });
                return None;
            }
        };
        let Some((focus, why)) = crate::meta::parse_meta_eval(&output) else {
            let _ = self.events.send(CoreEvent {
                kind: "meta.eval".into(),
                session_id: String::new(),
                payload: serde_json::json!({ "ok": true, "act": false }),
            });
            return None;
        };
        let _ = self.events.send(CoreEvent {
            kind: "meta.eval".into(),
            session_id: String::new(),
            payload: serde_json::json!({ "ok": true, "act": true, "focus": focus, "why": why }),
        });
        // 巡检行动会话:主智能体 + meta 工具组（runtime_allowlist 追加 meta 三件）
        let title = format!("meta-agent:{}", focus.chars().take(30).collect::<String>());
        let session = self.create_session(&title).ok()?;
        let prompt = format!(
            "{}\n\n## 巡检输入\n关注点：{focus}\n依据：{why}\n数据快照已可经 meta_observe 获取。",
            crate::meta::META_ACT_PROMPT,
        );
        let result = self.chat(&session.id, &prompt).await;
        let _ = self.events.send(CoreEvent {
            kind: "meta.act".into(),
            session_id: session.id.clone(),
            payload: serde_json::json!({
                "focus": focus,
                "status": if result.is_ok() { "done" } else { "failed" },
                "error": result.err().map(|e| e.to_string()),
            }),
        });
        Some((focus, why))
    }
}
