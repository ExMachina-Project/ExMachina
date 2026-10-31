//! gateway/platform/cron_scheduler —— 自 platform.rs 按资源域拆出（纯搬移，零行为变更）


use super::*;
/// 调度循环：每 20s 扫描到期任务（cron 命中 / 一次性 at 到点）；心跳由配置驱动
pub fn spawn_cron_scheduler(core: std::sync::Arc<exm_core::Core>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(20));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let _ = tick_jobs(&core).await;
        }
    });
}

pub(crate) async fn tick_jobs(core: &exm_core::Core) -> anyhow::Result<()> {
    let now = chrono::Utc::now();
    let jobs = core.cron.list()?;
    let mut changed = false;
    for mut job in jobs {
        if !exm_core::cron::job_due(&job, now) {
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
        let ok = push_channel_text(&ch, &text).await;
        if !ok {
            eprintln!("[cron-notify] 通道 {} 推送失败", ch.id);
        }
    }
}
