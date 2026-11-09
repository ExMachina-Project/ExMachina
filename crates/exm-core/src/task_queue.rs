//! 会话任务队列（task-queue）：智能体在执行当前任务时收到新任务，可自主决定
//! 「并行做」（spawn 子任务立即跑）或「排队」（等当前任务完成后串行执行）。
//!
//! 设计口径（与 todo/image_stash 同族）：
//! - 进程内注册表：任务是「轮次级运行时关注」，重启清零合理（spawn 后台注册表同口径）
//! - parallel 模式 = 复用 spawn 执行器（独立上下文/白名单/并发闸/超时，全部现成）
//! - queued 模式 = 队列暂存；轮次收束钩子（orchestrator）出队下一条继续跑
//! - 事件面：task_queue.updated 广播（WebUI 对话页实时渲染队列状态）
//!
//! 并行/排队的决策权在智能体：工具 schema 说明里给出决策依据
//! （新任务与当前任务是否共享写入路径/是否依赖当前产出）。

use std::collections::HashMap;
use std::sync::OnceLock;

/// 单条队列任务
#[derive(Debug, Clone, serde::Serialize)]
pub struct QueuedTask {
    /// tq-<8位id>
    pub id: String,
    /// 自包含任务书（子任务看不到会话历史，同 spawn 纪律）
    pub task: String,
    /// parallel（已并行派生）| queued（排队中）
    #[serde(rename = "runMode")]
    pub run_mode: String,
    /// queued | running | done | failed | cancelled
    pub status: String,
    /// parallel 模式关联的后台 spawn id（回收结果用）
    #[serde(rename = "spawnId", skip_serializing_if = "String::is_empty")]
    pub spawn_id: String,
    /// 入队时间（ISO）
    pub created_at: String,
    /// 终态时间（ISO；非终态跳过）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// 终态摘要（done=结论预览 / failed|cancelled=原因）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn queues() -> &'static parking_lot::Mutex<HashMap<String, Vec<QueuedTask>>> {
    static Q: OnceLock<parking_lot::Mutex<HashMap<String, Vec<QueuedTask>>>> = OnceLock::new();
    Q.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

/// 单会话队列容量上限（防失控：智能体狂塞任务时明确拒绝而非静默堆积）
const QUEUE_CAP: usize = 20;

/// 入队（追加到队尾）。队列满返回 Err（工具层转结构化错误给模型）。
pub fn enqueue(session_id: &str, task: &str, run_mode: &str, spawn_id: &str) -> Result<String, String> {
    let task = task.trim();
    if task.is_empty() {
        return Err("task 不能为空：请提供自包含任务书".into());
    }
    let mut q = queues().lock();
    let list = q.entry(session_id.to_string()).or_default();
    if list.len() >= QUEUE_CAP {
        return Err(format!("任务队列已满（{QUEUE_CAP}）：请先完成或取消现有任务"));
    }
    let id = format!("tq-{}", &crate::types::new_id()[..8]);
    list.push(QueuedTask {
        id: id.clone(),
        task: task.to_string(),
        run_mode: run_mode.to_string(),
        status: "queued".into(),
        spawn_id: spawn_id.to_string(),
        created_at: crate::types::now_iso(),
        finished_at: None,
        note: None,
    });
    Ok(id)
}

/// 取会话全队列（时间正序；快照副本）
pub fn list(session_id: &str) -> Vec<QueuedTask> {
    queues().lock().get(session_id).cloned().unwrap_or_default()
}

/// 取「排队中」任务（FIFO 出队口径：取最早入队的一条，标记 running）
pub fn dequeue_next(session_id: &str) -> Option<QueuedTask> {
    let mut q = queues().lock();
    let list = q.get_mut(session_id)?;
    let slot = list.iter_mut().find(|t| t.status == "queued")?;
    slot.status = "running".into();
    Some(slot.clone())
}

/// 更新任务状态（工具层回填终态用；不存在静默忽略——终态清理竞态容忍）
pub fn set_status(session_id: &str, task_id: &str, status: &str, note: Option<String>) {
    let mut q = queues().lock();
    if let Some(list) = q.get_mut(session_id) {
        if let Some(t) = list.iter_mut().find(|t| t.id == task_id) {
            t.status = status.to_string();
            t.note = note;
            if matches!(status, "done" | "failed" | "cancelled") {
                t.finished_at = Some(crate::types::now_iso());
            }
        }
    }
}

/// 按 spawn id 反查任务（spawn 完成回调关联用）
pub fn task_by_spawn(session_id: &str, spawn_id: &str) -> Option<String> {
    queues()
        .lock()
        .get(session_id)?
        .iter()
        .find(|t| t.spawn_id == spawn_id)
        .map(|t| t.id.clone())
}

/// 会话队列清空（会话删除时联动清理）
pub fn clear(session_id: &str) {
    queues().lock().remove(session_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 入队出队_fifo() {
        let sid = "tq-ses-a";
        clear(sid);
        let id1 = enqueue(sid, "任务一", "queued", "").unwrap();
        let _ = enqueue(sid, "任务二", "queued", "").unwrap();
        let next = dequeue_next(sid).unwrap();
        assert_eq!(next.id, id1, "先入先出");
        assert_eq!(next.status, "running");
        // 再出队是任务二
        let next2 = dequeue_next(sid).unwrap();
        assert_eq!(next2.task, "任务二");
        assert!(dequeue_next(sid).is_none(), "空队列返回 None");
        clear(sid);
    }

    #[test]
    fn 空任务与容量上限() {
        let sid = "tq-ses-b";
        clear(sid);
        assert!(enqueue(sid, "  ", "queued", "").is_err());
        for i in 0..QUEUE_CAP {
            enqueue(sid, &format!("任务{i}"), "queued", "").unwrap();
        }
        assert!(enqueue(sid, "超额", "queued", "").is_err(), "超容量应拒绝");
        clear(sid);
    }

    #[test]
    fn 状态回填与spawn反查() {
        let sid = "tq-ses-c";
        clear(sid);
        let id = enqueue(sid, "并行任务", "parallel", "sp-abc").unwrap();
        assert!(task_by_spawn(sid, "sp-abc").is_some());
        set_status(sid, &id, "done", Some("结论".into()));
        let listed = list(sid);
        assert_eq!(listed[0].status, "done");
        assert_eq!(listed[0].note.as_deref(), Some("结论"));
        assert!(listed[0].finished_at.is_some());
        // running 状态不会被 dequeue_next 取走
        assert!(dequeue_next(sid).is_none());
        clear(sid);
    }
}
