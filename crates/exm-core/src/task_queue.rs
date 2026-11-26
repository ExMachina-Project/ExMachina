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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QueuedTask {
    /// tq-<8位id>
    pub id: String,
    /// 会话内单调序号（1 起，按接收顺序；展示与排序口径，不重复）
    #[serde(rename = "seq")]
    pub seq: u64,
    /// 自包含任务书（子任务看不到会话历史，同 spawn 纪律）
    pub task: String,
    /// parallel（已并行派生）| queued（排队中）
    #[serde(rename = "runMode")]
    pub run_mode: String,
    /// queued | running | done | failed | cancelled | blocked | pending_recovery
    pub status: String,
    /// parallel 模式关联的后台 spawn id（回收结果用）
    #[serde(rename = "spawnId", default, skip_serializing_if = "String::is_empty")]
    pub spawn_id: String,
    /// 来源入口（chat / http / cli / tui / webui……审计与回执区分普通 steering 与显式入队）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// 项目归属（工作区根路径；空 = 非项目会话）。跨项目查询/编辑/删除按此拒绝。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// 乐观并发版本：每次编辑/状态迁移 +1；调用方带旧 revision 的写入被拒绝
    #[serde(default = "default_revision")]
    pub revision: u64,
    /// 入队时间（ISO）
    pub created_at: String,
    /// 终态时间（ISO；非终态跳过）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// 终态摘要（done=结论预览 / failed|cancelled=原因）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// 审计轨迹：编辑/移动/状态迁移的简要记录（旧文本不保留正文，防重放）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audit: Vec<String>,
}

fn default_revision() -> u64 {
    1
}

fn queues() -> &'static parking_lot::Mutex<HashMap<String, QueueState>> {
    static Q: OnceLock<parking_lot::Mutex<HashMap<String, QueueState>>> = OnceLock::new();
    Q.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

/// 会话队列状态：条目 + 已分配的单调序号水位（会话内不重复）
#[derive(Default)]
struct QueueState {
    items: Vec<QueuedTask>,
    seq_watermark: u64,
    /// 已移除条目的 id 墓碑（防止 remove 后 edit/move 以旧 id 复活旧文本）
    removed: Vec<String>,
}

/// 单会话队列容量上限（防失控：智能体狂塞任务时明确拒绝而非静默堆积）
const QUEUE_CAP: usize = 20;

/// 入队（追加到队尾）。队列满返回 Err（工具层转结构化错误给模型）。
#[allow(clippy::too_many_arguments)]
pub fn enqueue(
    session_id: &str,
    task: &str,
    run_mode: &str,
    spawn_id: &str,
) -> Result<String, String> {
    enqueue_scoped(session_id, task, run_mode, spawn_id, "", None)
}

/// 带来源/项目归属的入队（显式 `/queue` 入口用；普通消息不走这里）。
/// 空 task / 超容量拒绝；返回完整 queue id。
pub fn enqueue_scoped(
    session_id: &str,
    task: &str,
    run_mode: &str,
    spawn_id: &str,
    source: &str,
    workspace: Option<&str>,
) -> Result<String, String> {
    let task = task.trim();
    if task.is_empty() {
        return Err("task 不能为空：请提供自包含任务书".into());
    }
    let mut q = queues().lock();
    let state = q.entry(session_id.to_string()).or_default();
    if state.items.len() >= QUEUE_CAP {
        return Err(format!(
            "任务队列已满（{QUEUE_CAP}）：请先完成或取消现有任务"
        ));
    }
    let id = format!("tq-{}", &crate::types::new_id()[..8]);
    state.seq_watermark += 1;
    let seq = state.seq_watermark;
    state.items.push(QueuedTask {
        id: id.clone(),
        seq,
        task: task.to_string(),
        run_mode: run_mode.to_string(),
        status: "queued".into(),
        spawn_id: spawn_id.to_string(),
        source: source.to_string(),
        workspace: workspace.map(str::to_string),
        revision: default_revision(),
        created_at: crate::types::now_iso(),
        finished_at: None,
        note: None,
        audit: Vec::new(),
    });
    Ok(id)
}

/// 取会话全队列（时间正序；快照副本）。旧会话无队列 = 空列表。
pub fn list(session_id: &str) -> Vec<QueuedTask> {
    queues()
        .lock()
        .get(session_id)
        .map(|s| s.items.clone())
        .unwrap_or_default()
}

/// 按 id 查单条（同会话作用域：跨会话 id 查不到 = 拒绝，不泄漏正文）。
pub fn get(session_id: &str, task_id: &str) -> Option<QueuedTask> {
    queues()
        .lock()
        .get(session_id)?
        .items
        .iter()
        .find(|t| t.id == task_id)
        .cloned()
}

/// 编辑排队中的任务文本（steering 生效前用户改主意）。只有 `queued` 状态可编辑；
/// 带旧 revision 的并发写入被拒绝（乐观锁）。历史文本只留审计摘要，不重复执行。
pub fn edit(
    session_id: &str,
    task_id: &str,
    new_task: &str,
    expected_revision: Option<u64>,
) -> Result<QueuedTask, String> {
    let new_task = new_task.trim();
    if new_task.is_empty() {
        return Err("task 不能为空：请提供自包含任务书".into());
    }
    let mut q = queues().lock();
    let state = q.get_mut(session_id).ok_or("队列不存在或已清空")?;
    if state.removed.iter().any(|id| id == task_id) {
        return Err("队列条目已被移除".into());
    }
    let t = state
        .items
        .iter_mut()
        .find(|t| t.id == task_id)
        .ok_or("队列条目不存在")?;
    if t.status != "queued" {
        return Err(format!("仅排队中条目可编辑（当前 {}）", t.status));
    }
    if let Some(expected) = expected_revision {
        if t.revision != expected {
            return Err(format!(
                "revision 不匹配（期望 {expected}，当前 {}）：请刷新后重试",
                t.revision
            ));
        }
    }
    t.revision += 1;
    t.audit
        .push(format!("{} 编辑（旧文本已废弃）", crate::types::now_iso()));
    t.task = new_task.to_string();
    Ok(t.clone())
}

/// 移除排队中的任务（不执行其文本）。
pub fn remove(session_id: &str, task_id: &str) -> Result<(), String> {
    let mut q = queues().lock();
    let state = q.get_mut(session_id).ok_or("队列不存在或已清空")?;
    let idx = state
        .items
        .iter()
        .position(|t| t.id == task_id)
        .ok_or("队列条目不存在")?;
    if state.items[idx].status != "queued" {
        return Err(format!(
            "仅排队中条目可移除（当前 {}）",
            state.items[idx].status
        ));
    }
    state.items.remove(idx);
    state.removed.push(task_id.to_string());
    Ok(())
}

/// 移动排队中任务到指定序位（1 起；越界钳到队首/队尾）。出队仍按 FIFO
///（list 顺序即处理顺序），move 只改变列表位置。
pub fn move_to(
    session_id: &str,
    task_id: &str,
    position: usize,
) -> Result<Vec<QueuedTask>, String> {
    let mut q = queues().lock();
    let state = q.get_mut(session_id).ok_or("队列不存在或已清空")?;
    let idx = state
        .items
        .iter()
        .position(|t| t.id == task_id)
        .ok_or("队列条目不存在")?;
    if state.items[idx].status != "queued" {
        return Err(format!(
            "仅排队中条目可移动（当前 {}）",
            state.items[idx].status
        ));
    }
    let target = position.max(1).min(state.items.len()) - 1;
    let item = state.items.remove(idx);
    state.items.insert(target, item);
    Ok(state.items.clone())
}

/// 取「排队中」任务（FIFO 出队口径：取最早入队的一条，标记 running）
pub fn dequeue_next(session_id: &str) -> Option<QueuedTask> {
    let mut q = queues().lock();
    let state = q.get_mut(session_id)?;
    let slot = state.items.iter_mut().find(|t| t.status == "queued")?;
    slot.status = "running".into();
    slot.revision += 1;
    Some(slot.clone())
}

/// 更新任务状态（工具层回填终态用；不存在静默忽略——终态清理竞态容忍）
pub fn set_status(session_id: &str, task_id: &str, status: &str, note: Option<String>) {
    let mut q = queues().lock();
    if let Some(state) = q.get_mut(session_id) {
        if let Some(t) = state.items.iter_mut().find(|t| t.id == task_id) {
            t.status = status.to_string();
            t.note = note;
            t.revision += 1;
            if matches!(
                status,
                "done" | "failed" | "cancelled" | "blocked" | "pending_recovery"
            ) {
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
        .items
        .iter()
        .find(|t| t.spawn_id == spawn_id)
        .map(|t| t.id.clone())
}

/// 会话队列清空（会话删除时联动清理）
pub fn clear(session_id: &str) {
    queues().lock().remove(session_id);
}

/// 服务重启后的队列再物化（capability-parity 3.6「服务重启」）：队列注册表是
/// 进程内存态（重启清零），但落库的 `task_queue.updated` 快照事件会让刷新
/// 回放出**永远不会执行的虚假 queued/running 条目**。从事件尾取最后快照，
/// 把未终态项（queued/running）以 `pending_recovery` 重新装回内存注册表
/// （id/seq/来源/审计保留，run_mode 归一 queued）——不自动执行，等用户确认。
/// 返回受影响的会话数。
pub fn rematerialize_from_events(session_id: &str, store: &crate::store::Store) -> usize {
    let evts = match store.list_events(session_id) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    // 事件尾向前找最近的全量快照（与 webui 回放同口径：无 items 的旧格式跳过）
    let snapshot = evts.iter().rev().find_map(|env| {
        let evt = env.get("payload")?.as_object()?;
        if evt.get("type").and_then(|v| v.as_str()) != Some("task_queue.updated") {
            return None;
        }
        let d = evt.get("payload")?;
        let items = d.get("items")?.as_array()?;
        Some(items.clone())
    });
    let Some(items) = snapshot else {
        return 0;
    };
    let mut recovered = Vec::new();
    for raw in items {
        let Ok(mut t) = serde_json::from_value::<QueuedTask>(raw.clone()) else {
            continue;
        };
        if matches!(t.status.as_str(), "queued" | "running") {
            t.status = "pending_recovery".into();
            t.run_mode = "queued".into();
            t.spawn_id.clear();
            t.note = Some("服务重启：队列项转入待恢复，确认后可重新排队执行".into());
            t.revision += 1;
            recovered.push(t);
        }
    }
    if recovered.is_empty() {
        return 0;
    }
    let mut q = queues().lock();
    let state = q.entry(session_id.to_string()).or_default();
    let watermark = state.seq_watermark;
    for t in recovered {
        state.seq_watermark = state.seq_watermark.max(t.seq);
        state.items.push(t);
    }
    // 保留既有水位（防重启后 seq 回退重复）
    state.seq_watermark = state.seq_watermark.max(watermark);
    1
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

    // ---- capability-parity 3.1：归属/序号/revision/来源/审计/跨会话拒绝 ----

    #[test]
    fn 旧会话缺字段按空队列读取() {
        let sid = "tq-ses-legacy";
        clear(sid);
        assert!(list(sid).is_empty(), "从未入队的会话读空队列");
        assert!(get(sid, "tq-anyone").is_none());
        assert!(dequeue_next(sid).is_none());
        clear(sid);
    }

    #[test]
    fn 序号单调且按接收顺序() {
        let sid = "tq-ses-seq";
        clear(sid);
        enqueue(sid, "一", "queued", "").unwrap();
        enqueue(sid, "二", "queued", "").unwrap();
        let items = list(sid);
        assert_eq!(items[0].seq, 1);
        assert_eq!(items[1].seq, 2, "会话内单调递增");
        let third = enqueue(sid, "三", "queued", "").unwrap();
        assert_eq!(get(sid, &third).unwrap().seq, 3);
        clear(sid);
    }

    #[test]
    fn 跨会话查询编辑删除被拒绝() {
        let sid_a = "tq-ses-x";
        let sid_b = "tq-ses-y";
        clear(sid_a);
        clear(sid_b);
        let id_a = enqueue(sid_a, "A 的任务", "queued", "").unwrap();
        // B 会话持 A 的 queue id：查询/编辑/删除全部拒绝，不泄漏正文
        assert!(get(sid_b, &id_a).is_none(), "跨会话查询拒绝");
        assert!(edit(sid_b, &id_a, "篡改", None).is_err(), "跨会话编辑拒绝");
        assert!(remove(sid_b, &id_a).is_err(), "跨会话删除拒绝");
        assert!(
            get(sid_a, &id_a).unwrap().task == "A 的任务",
            "原条目不受影响"
        );
        clear(sid_a);
        clear(sid_b);
    }

    #[test]
    fn 编辑带乐观锁与状态门() {
        let sid = "tq-ses-edit";
        clear(sid);
        let id = enqueue(sid, "原任务", "queued", "").unwrap();
        let rev1 = get(sid, &id).unwrap().revision;
        // 正确 revision 的编辑成功，revision +1，审计有痕
        let edited = edit(sid, &id, "新任务", Some(rev1)).unwrap();
        assert_eq!(edited.task, "新任务");
        assert_eq!(edited.revision, rev1 + 1);
        assert!(!edited.audit.is_empty(), "编辑留审计记录");
        // 旧 revision 再编辑被拒绝（并发冲突）
        assert!(edit(sid, &id, "并发写", Some(rev1)).is_err());
        // running 条目不可编辑
        dequeue_next(sid);
        assert!(edit(sid, &id, "运行中改", None).is_err());
        clear(sid);
    }

    #[test]
    fn 移除后旧id不可复活() {
        let sid = "tq-ses-rm";
        clear(sid);
        let id = enqueue(sid, "待移除", "queued", "").unwrap();
        remove(sid, &id).unwrap();
        assert!(get(sid, &id).is_none());
        // remove 后 edit/move 不能以旧 id 复活旧文本
        assert!(edit(sid, &id, "复活", None).is_err());
        assert!(move_to(sid, &id, 1).is_err());
        // 二次 remove 同 id 报不存在
        assert!(remove(sid, &id).is_err());
        clear(sid);
    }

    #[test]
    fn 移动钳制与出队顺序() {
        let sid = "tq-ses-mv";
        clear(sid);
        let _a = enqueue(sid, "甲", "queued", "").unwrap();
        let b = enqueue(sid, "乙", "queued", "").unwrap();
        let _c = enqueue(sid, "丙", "queued", "").unwrap();
        // 乙移到队首（position 1）
        let items = move_to(sid, &b, 1).unwrap();
        assert_eq!(items[0].task, "乙");
        // 越界钳制到队尾
        let items = move_to(sid, &b, 99).unwrap();
        assert_eq!(items.last().unwrap().task, "乙");
        // FIFO 出队按移动后的列表顺序
        let next = dequeue_next(sid).unwrap();
        assert_eq!(next.task, "甲", "未被移动的条目仍按原相对顺序");
        clear(sid);
    }

    #[test]
    fn 来源与项目归属字段() {
        let sid = "tq-ses-scope";
        clear(sid);
        let id = enqueue_scoped(sid, "/queue 指令", "queued", "", "chat", Some("/proj/x")).unwrap();
        let t = get(sid, &id).unwrap();
        assert_eq!(t.source, "chat");
        assert_eq!(t.workspace.as_deref(), Some("/proj/x"));
        // 旧入口 enqueue() 无来源：source 空、workspace None
        let id2 = enqueue(sid, "工具入队", "queued", "").unwrap();
        let t2 = get(sid, &id2).unwrap();
        assert_eq!(t2.source, "");
        assert!(t2.workspace.is_none());
        clear(sid);
    }

    /// capability-parity 3.6「服务重启」：队列注册表是内存态（重启清零），
    /// 落库快照事件会把虚假的 queued/running 回放给用户。rematerialize 从
    /// 事件尾取最后全量快照，未终态项装回为 pending_recovery（不自动执行、
    /// 不虚报完成），终态项不复活，旧格式（无 items）事件不炸。
    #[test]
    fn 重启后未终态项从事件快照再物化为待恢复() {
        let tmp = std::env::temp_dir().join(format!("exm-tq-restart-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let store = crate::store::Store::open(&tmp).unwrap();
        let sid = "tq-restart-ses";
        clear(sid);

        // 落一条全量快照事件（模拟 3.5 的 emit_session_event 落库链路：
        // envelope.payload = CoreEvent{type, payload}）
        let q1 = enqueue_scoped(sid, "重启前排队的任务", "queued", "", "webui", None).unwrap();
        let items = list(sid);
        let evt = serde_json::json!({
            "type": "task_queue.updated",
            "sessionId": sid,
            "payload": { "action": "add", "items": items, "running": 0, "queued": 1 },
        });
        let env = crate::types::Envelope::new(
            format!("session.{sid}.events"),
            "core",
            "broadcast",
            "event",
            sid.to_string(),
            evt,
        );
        store.append_event(&env).unwrap();

        // 模拟重启：内存注册表清零（进程退出即如此）
        clear(sid);
        assert!(list(sid).is_empty(), "重启后内存队列为空");

        // 再物化：未终态项回来，状态 pending_recovery
        let n = rematerialize_from_events(sid, &store);
        assert_eq!(n, 1, "应恢复 1 个会话的队列");
        let items = list(sid);
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].status, "pending_recovery",
            "不虚报 queued：{items:?}"
        );
        assert_eq!(items[0].task, "重启前排队的任务");
        assert_eq!(items[0].source, "webui", "来源保留");
        assert_eq!(items[0].id, q1, "id 稳定（编辑/移除仍可对准）");

        // 终态项不复活：快照里 done 的条目再物化后不出现
        set_status(sid, &q1, "done", None);
        let snapshot_done = serde_json::json!({
            "type": "task_queue.updated",
            "payload": { "action": "finish", "items": list(sid), "running": 0, "queued": 0 },
        });
        let env2 = crate::types::Envelope::new(
            format!("session.{sid}.events"),
            "core",
            "broadcast",
            "event",
            sid.to_string(),
            snapshot_done,
        );
        store.append_event(&env2).unwrap();
        clear(sid);
        let n2 = rematerialize_from_events(sid, &store);
        assert_eq!(n2, 0, "全终态快照不再物化");
        assert!(list(sid).is_empty());

        // 旧格式（无 items）事件：跳过不炸、不产生虚假空队列
        let env3 = crate::types::Envelope::new(
            format!("session.{sid}.events"),
            "core",
            "broadcast",
            "event",
            sid.to_string(),
            serde_json::json!({
                "type": "task_queue.updated",
                "payload": { "action": "add", "queueId": "legacy-1", "seq": 1 },
            }),
        );
        store.append_event(&env3).unwrap();
        clear(sid);
        assert_eq!(rematerialize_from_events(sid, &store), 0);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
