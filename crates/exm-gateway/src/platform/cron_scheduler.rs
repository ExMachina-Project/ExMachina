//! gateway/platform/cron_scheduler —— 自 platform.rs 按资源域拆出（纯搬移，零行为变更）


use super::*;
/// 闲置归档当日去重标记行 id（enabled=false 的哨兵行，只借 last_run_minute 记日期）
const IDLE_ARCHIVE_MARKER: &str = "__idle_archive__";

/// 调度循环：每 20s 扫描到期任务（cron 命中 / 一次性 at 到点）；心跳由配置驱动。
/// 调度体外套 catch_unwind：tick 内任何 panic（未来代码缺陷）只损失当轮扫描，
/// 不杀死调度任务——否则所有定时任务（含心跳巡检）静默全停（审计 P1-3）。
pub fn spawn_cron_scheduler(core: std::sync::Arc<exm_core::Core>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(20));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let core2 = core.clone();
            // JoinError = tick 内 panic：记日志吞掉，下轮继续（调度任务不死）
            if let Err(join_err) = tokio::task::spawn(async move { tick_jobs(&core2).await }).await {
                eprintln!("[cron-scheduler] tick panic：{join_err}（已吞掉，下轮继续）");
            }
        }
    });
}

pub(crate) async fn tick_jobs(core: &exm_core::Core) -> anyhow::Result<()> {
    let now = chrono::Utc::now();
    // cron 时区（automation.timezone）：五段表达式按配置时区的墙上时钟解释——
    // 「每天 8 点」在配置时区 8 点触发；空/非法名 = UTC（既有行为）。at 不受影响。
    let tz = core.config().automation.timezone.clone();
    let jobs = core.cron.list()?;
    let mut changed = false;
    for mut job in jobs {
        if !exm_core::cron::job_due_in(&job, now, &tz) {
            continue;
        }
        let _ = core.events.send(CoreEvent {
            kind: "cron.fired".into(),
            session_id: String::new(),
            payload: json!({ "jobId": job.id, "jobName": job.name, "prompt": job.prompt }),
        });
        job.last_run_at = Some(exm_core::types::now_iso());
        job.last_run_minute = Some(exm_core::cron::minute_key(now));
        let run = core.run_cron_job(&job).await;
        job.last_status = Some(run.status.clone());
        if job.at.is_some() {
            // 一次性任务：触发后停用
            job.enabled = false;
        }
        core.cron.upsert(job.clone())?;
        changed = true;
        // 结果推送（7.3）：订阅了通道的任务完成后出站摘要（未订阅 = 维持现状不推）
        notify_cron_result(core, &job, &run).await;
    }

    // 心跳巡检：配置驱动的内置周期任务（会话固定「心跳巡检」；per-agent 粒度见 build_heartbeat_job）
    let cfg = core.config();
    if cfg.automation.heartbeat_enabled {
        let interval = cfg.automation.heartbeat_interval_minutes.max(1) as i64;
        let minute = now.timestamp() / 60;
        if minute % interval == 0 {
            let minute_key = exm_core::cron::minute_key(now);
            let already = core
                .cron
                .get("__heartbeat__")?
                .map(|j| j.last_run_minute.as_deref() == Some(minute_key.as_str()))
                .unwrap_or(false);
            if !already {
                let hb = build_heartbeat_job(core, &minute_key);
                let run = core.run_cron_job(&hb).await;
                core.cron.upsert(exm_core::types::CronJob {
                    last_run_at: Some(exm_core::types::now_iso()),
                    last_status: Some(run.status),
                    ..hb
                })?;
                changed = true;
            }
        }
    }

    // Auto Agent（实验性，experiments.autoAgent）：周期 Eval 自评 + 自主行动。
    // 内部按分钟去重与退避（连续无任务拉长间隔）；开关关闭时 tick 直接返回。
    if core.config().experiments.auto_agent {
        let minute = now.timestamp() / 60;
        if core.auto_agent_tick(minute).await.is_some() {
            changed = true;
        }
    }

    // Meta Agent（实验性，experiments.metaAgent）：周期巡检（数据聚合 → Eval →
    // 自主会话落提议单）。独立 meta_state 周期判定,与 Auto Agent 互不干扰。
    if core.config().experiments.meta_agent {
        let minute = now.timestamp() / 60;
        if core.meta_agent_tick(minute).await.is_some() {
            changed = true;
        }
    }

    // 闲置流会话归档（session-management）：每日一次（UTC 4 点低谷窗口）。
    // flowIdleDays=0（默认）不进扫描——关闭态零行为变更。
    // 当日去重：复用 cron 表存标记行（enabled=false，不会被 job_due 触发）。
    if core.config().automation.flow_idle_days > 0 {
        use chrono::Timelike;
        if (now.hour(), now.minute()) == (4, 0) {
            let minute_key = exm_core::cron::minute_key(now);
            let last = core
                .cron
                .get(IDLE_ARCHIVE_MARKER)?
                .and_then(|j| j.last_run_minute)
                .unwrap_or_default();
            if last != minute_key {
                let idle_days = core.config().automation.flow_idle_days;
                if let Ok((archived, skipped)) = core.archive_idle_flow_sessions(idle_days, now)
                {
                    if archived > 0 {
                        let _ = core.events.send(CoreEvent {
                            kind: "cron.updated".into(),
                            session_id: String::new(),
                            payload: json!({
                                "idleArchive": { "archived": archived, "skipped": skipped }
                            }),
                        });
                    }
                }
                let marker = exm_core::types::CronJob {
                    id: IDLE_ARCHIVE_MARKER.into(),
                    name: "闲置流会话归档标记".into(),
                    prompt: String::new(),
                    kind: None,
                    cron: Some("0 4 * * *".into()),
                    at: None,
                    target_session: None,
                    steps: vec![],
                    group: None,
                    session_title: None,
                    enabled: false,
                    last_run_at: Some(exm_core::types::now_iso()),
                    last_status: None,
                    last_run_minute: Some(minute_key),
                    notify_channels: vec![],
                    created_at: exm_core::types::now_iso(),
                };
                let _ = core.cron.upsert(marker);
            }
        }
    }

    // 自主记忆摘要（实验性 experiments.autoMemoryDigest，9.1）：每日一次低峰期
    // （UTC 4:30，避开 4:00 归档窗口）。关闭态不进扫描（零任务零 token）。
    // 当日去重：同 IDLE_ARCHIVE_MARKER 模式复用 cron 表标记行。
    if core.config().experiments.auto_memory_digest {
        use chrono::Timelike;
        if (now.hour(), now.minute()) == (4, 30) {
            const DIGEST_MARKER: &str = "__memory_digest_marker__";
            let minute_key = exm_core::cron::minute_key(now);
            let last = core
                .cron
                .get(DIGEST_MARKER)?
                .and_then(|j| j.last_run_minute)
                .unwrap_or_default();
            if last != minute_key {
                let sessions = core.list_sessions_admin().unwrap_or_default();
                let mut digested = 0usize;
                for s in &sessions {
                    if core.memory_digest_session(&s.id).await.is_some() {
                        digested += 1;
                    }
                }
                if digested > 0 {
                    eprintln!("[memory-digest] 低峰归纳完成：{digested} 个会话产出记忆条目");
                }
                let marker = exm_core::types::CronJob {
                    id: DIGEST_MARKER.into(),
                    name: "自主记忆摘要标记".into(),
                    prompt: String::new(),
                    kind: None,
                    cron: Some("30 4 * * *".into()),
                    at: None,
                    target_session: None,
                    steps: vec![],
                    group: None,
                    session_title: None,
                    enabled: false,
                    last_run_at: Some(exm_core::types::now_iso()),
                    last_status: None,
                    last_run_minute: Some(minute_key),
                    notify_channels: vec![],
                    created_at: exm_core::types::now_iso(),
                };
                let _ = core.cron.upsert(marker);
            }
        }
    }

    // 每日自动衰减（memory-lifecycle 6.2）：memory.autoDecay 开启时每日一次
    // 低峰窗口（UTC 4:40，错开 4:30 记忆摘要）全量衰减。关闭态不进扫描
    // （零任务零开销）。当日去重同 DIGEST_MARKER 模式（cron 表标记行）。
    // 衰减只调 importance 数值（钳 floor），绝不删除条目；手动端点保留可并存。
    if core.config().memory_auto_decay {
        use chrono::Timelike;
        if (now.hour(), now.minute()) == (4, 40) {
            const DECAY_MARKER: &str = "__memory_decay_marker__";
            let minute_key = exm_core::cron::minute_key(now);
            let last = core
                .cron
                .get(DECAY_MARKER)?
                .and_then(|j| j.last_run_minute)
                .unwrap_or_default();
            if last != minute_key {
                let changed = core.memory_decay(0.05).unwrap_or(0);
                // 可观测：执行条数落全局事件流（memory.decay.ran）
                let _ = core.events.send(exm_core::types::CoreEvent {
                    kind: "memory.decay.ran".into(),
                    session_id: String::new(),
                    payload: json!({ "changed": changed, "floor": 0.05 }),
                });
                eprintln!("[memory-decay] 低峰衰减完成：{changed} 条 importance 更新");
                let marker = exm_core::types::CronJob {
                    id: DECAY_MARKER.into(),
                    name: "记忆衰减标记".into(),
                    prompt: String::new(),
                    kind: None,
                    cron: Some("40 4 * * *".into()),
                    at: None,
                    target_session: None,
                    steps: vec![],
                    group: None,
                    session_title: None,
                    enabled: false,
                    last_run_at: Some(exm_core::types::now_iso()),
                    last_status: None,
                    last_run_minute: Some(minute_key),
                    notify_channels: vec![],
                    created_at: exm_core::types::now_iso(),
                };
                let _ = core.cron.upsert(marker);
            }
        }
    }

    if changed {
        let _ = core.events.send(CoreEvent {
            kind: "cron.updated".into(),
            session_id: String::new(),
            payload: json!({}),
        });
    }
    Ok(())
}

/// 个体所属组（心跳 per-agent 定位用）：逐组扫描成员清单
pub(crate) fn group_of_agent(core: &exm_core::Core, identifier: &str) -> Option<String> {
    core.registry
        .list_groups()
        .into_iter()
        .find(|g| {
            core.registry
                .agents_in_group(&g.id)
                .iter()
                .any(|a| a.identifier == identifier)
        })
        .map(|g| g.id)
}

/// 构造本次心跳任务（7.4 per-agent 粒度）：
/// 配置了目标个体且其存在于某组 → 以该组为执行上下文（产出经 write_memories 进该组记忆体系，
/// 会话独立「心跳巡检-<个体>」）；未配置或个体不存在 → 维持全局巡检（激活组 + 会话「心跳巡检」）。
pub(crate) fn build_heartbeat_job(core: &exm_core::Core, minute_key: &str) -> exm_core::types::CronJob {
    let cfg = core.config();
    let target = cfg
        .automation
        .heartbeat_agent
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let agent_group = target.and_then(|id| group_of_agent(core, id));
    let (name, session_title, group) = match (target, agent_group) {
        (Some(id), Some(g)) => (format!("心跳巡检·{id}"), format!("心跳巡检-{id}"), Some(g)),
        _ => ("心跳巡检".to_string(), "心跳巡检".to_string(), None),
    };
    exm_core::types::CronJob {
        id: "__heartbeat__".into(),
        name,
        prompt: cfg.automation.heartbeat_prompt.clone(),
        cron: None,
        at: None,
        kind: None,
        target_session: None,
        steps: Vec::new(),
        group,
        session_title: Some(session_title),
        enabled: true,
        last_run_at: None,
        last_status: None,
        last_run_minute: Some(minute_key.to_string()),
        notify_channels: Vec::new(),
        created_at: String::new(),
    }
}

// ---------------------------------------------------------------- 定时任务结果推送（7.3）

/// 任务完成后的订阅通道摘要推送：全局开关 + 任务订阅清单双闸门；
/// 未订阅 = 不产生任何通道消息（维持现状）。受限流约束的出站复用同一文本通路。
pub(crate) async fn notify_cron_result(
    core: &exm_core::Core,
    job: &exm_core::types::CronJob,
    run: &exm_core::types::CronRun,
) {
    let cfg = core.config();
    if !cfg.notify.enabled || job.notify_channels.is_empty() {
        return;
    }
    let max_chars = cfg.notify.max_chars.max(50);
    let summary: String = run.summary.chars().take(max_chars).collect();
    let text = format!(
        "⏰ 定时任务「{}」{}\n{}",
        job.name,
        if run.status == "done" {
            "已完成"
        } else {
            "失败"
        },
        summary
    );
    for ch in load_channels(core) {
        if !ch.enabled || !job.notify_channels.contains(&ch.id) {
            continue;
        }
        // 投递契约（update-pipeline 规格「webhook 投递超时与重试」）：
        // 单次 15s 超时（push_channel_text 内控）+ 退避重试 2 次（共 3 次尝试），
        // 最终失败 MUST 落审计事件（含目标与原因摘要），MUST NOT 静默吞掉。
        let mut ok = push_channel_text(&ch, &text).await;
        let mut attempts = 1u32;
        while !ok && attempts < 3 {
            tokio::time::sleep(std::time::Duration::from_millis(
                500u64 * (1 << (attempts - 1)), // 0.5s → 1s 退避
            ))
            .await;
            ok = push_channel_text(&ch, &text).await;
            attempts += 1;
        }
        if !ok {
            eprintln!("[cron-notify] 通道 {} 推送失败（重试 {} 次后放弃）", ch.id, attempts);
            core.orchestrator()
                .emit(
                    "",
                    "notify.delivery_failed",
                    serde_json::json!({
                        "channel": ch.id,
                        "job": job.id,
                        "jobName": job.name,
                        "attempts": attempts,
                        "reason": "投递重试耗尽（端点超时或非 2xx 响应）",
                    }),
                );
        }
    }
}

/// 每日自动衰减（memory-lifecycle 6.2）测试：开关两态与当日不重复
#[cfg(test)]
mod auto_decay_tests {
    use super::*;

    fn test_core() -> exm_core::Core {
        let dir = std::env::temp_dir().join(format!("exm-decay-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        exm_core::Core::create(&dir).expect("创建 Core 失败")
    }

    /// 关闭态：不注册标记行（零任务零开销）
    #[tokio::test]
    async fn 关闭态_不注册标记() {
        let core = test_core();
        let mut cfg = (*core.config()).clone();
        cfg.memory_auto_decay = false;
        core.apply_config(cfg).unwrap();
        assert!(!core.config().memory_auto_decay, "热更关闭生效");
        // 关闭态不进扫描分支：标记行不存在（若注册过即行为错误）
        assert!(core.cron.get("__memory_decay_marker__").unwrap().is_none());
    }

    /// 衰减语义锚定：只调 importance（钳 floor），绝不删除条目
    #[tokio::test]
    async fn 开启态_衰减不删除条目() {
        let core = test_core();
        assert!(core.config().memory_auto_decay, "缺省开启");
        let draft = exm_core::memory::MemoryDraft::new(
            exm_core::memory::MemoryKind::Fact,
            "老记忆",
            "测试记忆",
        );
        core.memory_remember_global(draft).unwrap();
        // 连续执行两次衰减（幂等 + 不删除）
        let _ = core.memory_decay(0.05).unwrap();
        let _ = core.memory_decay(0.05).unwrap();
        let after = core.memory_list(None, 100).unwrap();
        assert_eq!(after.len(), 1, "衰减绝不删除条目");
        assert!(
            (0.05..=1.0).contains(&after[0].importance),
            "importance 钳在 [floor, 初始] 区间: {}",
            after[0].importance
        );
    }

    /// 标记行当日去重基础：同分钟同 minute_key
    #[test]
    fn 标记行_minute_key去重语义() {
        let now = chrono::Utc::now();
        let k1 = exm_core::cron::minute_key(now);
        let k2 = exm_core::cron::minute_key(now);
        assert_eq!(k1, k2, "同分钟同 key（当日同窗口去重的基础）");
    }
}
