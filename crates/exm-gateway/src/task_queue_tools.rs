//! 会话任务队列工具（task__queue）：智能体在执行当前任务时收到新任务，
//! 自主决定「并行做」（spawn 子任务立即跑，不阻塞当前任务）或「排队」
//! （等当前任务完成后串行执行）。
//!
//! - task__queue op=add：runMode=parallel → 立即经 spawn 执行器派生后台子任务；
//!   runMode=queued → 进队，轮次收束钩子自动出队执行
//! - task__queue op=list：队列快照（含并行任务关联的 spawnId 可回收结果）
//! - task__queue op=cancel：取消排队中任务 / 标记运行中任务待终止说明
//!
//! 注入口径：全会话默认注入（build_injections 由 Core 装配时统一挂载），
//! 与 napcat__*（按通道收窄）不同——任务队列是通用执行面能力。

use exm_core::config::CustomTool;
use exm_core::task_queue;
use exm_core::tools::{dynamic_exec, DynamicToolExec};
use exm_core::Core;
use std::sync::Arc;

/// 工具 schema（单工具三操作，减少工具面碎片）
pub(crate) fn tool_spec() -> CustomTool {
    CustomTool {
        name: "task__queue".into(),
        description: "任务队列：当前任务执行中收到新任务时，自主决定处置方式。\nrunMode=parallel：新任务与当前任务无写入冲突、不依赖当前产出 → 立即派生子智能体并行执行（不阻塞本轮，立即返回 spawnId）。\nrunMode=queued：新任务依赖当前任务产出、或共享写入路径 → 排队，当前任务收束后自动依次执行。\n任务书必须自包含（执行者看不到会话历史）。用 op=list 查看队列，op=cancel 取消。".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["add", "list", "cancel"], "description": "操作：add 入队 / list 查看 / cancel 取消" },
                "task": { "type": "string", "description": "op=add 必填：自包含任务书（目标、范围、期望产出）" },
                "runMode": { "type": "string", "enum": ["parallel", "queued"], "description": "op=add 必填：parallel 立即并行派生 / queued 排队等当前任务完成" },
                "taskId": { "type": "string", "description": "op=cancel 必填：任务 id（tq-…）" },
                "reason": { "type": "string", "description": "op=cancel 可选：取消原因（记录用）" }
            },
            "required": ["op"]
        }),
        ..Default::default()
    }
}

/// 执行闭包（dynamic_exec：Result<String> → DynamicToolOutput）
pub(crate) fn build_exec(core: Arc<Core>) -> DynamicToolExec {
    dynamic_exec(move |_agent, session, args| {
        let core = core.clone();
        let session = session.to_string();
        async move { dispatch(&core, &session, args).await }
    })
}

/// 注入清单（单条；napcat_tools::build_injections 同构）。
/// 当前经 register_dynamic_tool 全局注册（serve 装配），此入口保留给
/// 需要按会话收窄的场景（如未来按工具面档位下发）。
#[allow(dead_code)]
pub(crate) fn build_injections(core: &Arc<Core>) -> Vec<(CustomTool, DynamicToolExec)> {
    vec![(tool_spec(), build_exec(core.clone()))]
}

/// 工具分发
async fn dispatch(core: &Arc<Core>, session_id: &str, args: serde_json::Value) -> anyhow::Result<String> {
    let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("");
    match op {
        "add" => op_add(core, session_id, &args).await,
        "list" => Ok(op_list(session_id)),
        "cancel" => Ok(op_cancel(core, session_id, &args)),
        _ => Ok("op 必须是 add / list / cancel".into()),
    }
}

/// op=add：parallel 立即派生 / queued 进队
async fn op_add(core: &Arc<Core>, session_id: &str, args: &serde_json::Value) -> anyhow::Result<String> {
    let task = args.get("task").and_then(|v| v.as_str()).unwrap_or("").trim();
    if task.is_empty() {
        return Ok("task 不能为空：请提供自包含任务书（目标、范围、期望产出）。".into());
    }
    let mode = args
        .get("runMode")
        .and_then(|v| v.as_str())
        .unwrap_or("queued");
    match mode {
        // 并行：直接走 spawn 工具（独立上下文/白名单/并发闸/后台注册表全套现成），
        // 任务登记进队列（关联 spawnId）后转发 spawn 结果文本
        "parallel" => {
            let gw = core.orchestrator().tools.clone();
            let spawn_args = serde_json::json!({
                "task": task,
                "background": true,
            });
            // spawn 走内置面（execute_named 白名单传空即可——Spawn 在内置枚举内，
            // allowlist 空表 = 不经白名单过滤的内置直派发路径，与工具循环内调用同权）
            let agent = core.orchestrator().orch_id();
            let result = gw
                .execute_named(&agent, session_id, &[], "spawn", &spawn_args)
                .await;
            // spawn 拒绝（并发满/执行器未装配）→ 回落排队（任务不丢，等收束后跑）
            let spawn_ok = result.ok;
            let spawn_id = extract_spawn_id(&result.output);
            let tid = task_queue::enqueue(session_id, task, "parallel", &spawn_id)
                .map_err(|e| anyhow::anyhow!(e))?;
            emit_updated(core, session_id);
            if spawn_ok {
                Ok(format!("已并行派生（taskId={tid}, {spawn_id}）：子智能体后台执行中，本轮继续当前任务；用 task__queue op=list 查看进度，spawn op=status spawnId={spawn_id} 回收结果。"))
            } else {
                Ok(format!("并行派生暂不可用（{}），已转为排队（taskId={tid}）：当前任务收束后自动执行，不丢失。", result.error.unwrap_or_else(|| "原因未知".into())))
            }
        }
        // 排队：轮次收束钩子出队执行
        _ => {
            let tid = task_queue::enqueue(session_id, task, "queued", "")
                .map_err(|e| anyhow::anyhow!(e))?;
            emit_updated(core, session_id);
            Ok(format!("已排队（taskId={tid}）：当前任务收束后自动执行。队列中排在它之前还有 {} 项。", queued_count(session_id, &tid)))
        }
    }
}

/// op=list：队列快照
fn op_list(session_id: &str) -> String {
    let items = task_queue::list(session_id);
    if items.is_empty() {
        return "任务队列为空。".into();
    }
    let mut out = format!("任务队列（{} 项）：\n", items.len());
    for t in &items {
        let preview: String = t.task.chars().take(60).collect();
        let spawn_tag = if t.spawn_id.is_empty() {
            String::new()
        } else {
            format!("（spawnId={}）", t.spawn_id)
        };
        out.push_str(&format!(
            "- {} [{}] {}：{}…{}\n",
            t.id, t.status, t.run_mode, preview, spawn_tag
        ));
    }
    out.push_str("\nparallel 项用 spawn op=status spawnId=… 回收结果；queued 项等当前任务收束后自动执行。");
    out
}

/// op=cancel：取消任务（排队中直接取消；运行中 parallel 提示走 spawn 停止语义）
fn op_cancel(core: &Arc<Core>, session_id: &str, args: &serde_json::Value) -> String {
    let tid = args.get("taskId").and_then(|v| v.as_str()).unwrap_or("");
    if tid.is_empty() {
        return "op=cancel 需要 taskId（op=list 查看）".into();
    }
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("智能体请求取消")
        .to_string();
    let items = task_queue::list(session_id);
    let Some(t) = items.iter().find(|t| t.id == tid) else {
        return format!("任务不存在: {tid}");
    };
    match t.status.as_str() {
        "queued" => {
            task_queue::set_status(session_id, tid, "cancelled", Some(reason.clone()));
            emit_updated(core, session_id);
            format!("已取消 {tid}：{reason}")
        }
        "running" => {
            if t.spawn_id.is_empty() {
                format!("{tid} 正在收束钩子中执行，无法取消；如需中断当前会话轮次请请求用户停止。")
            } else {
                format!("{tid} 是并行子任务（{0}）：子任务超时由网关强制控制；如需立即终止请请求用户停止会话。", t.spawn_id)
            }
        }
        s => format!("{tid} 已是终态（{s}），无需取消。"),
    }
}

// ---------------------------------------------------------------- 辅助

/// 从 spawn 工具输出中提取 spawnId（sp-xxxx）：取首个 `sp-` 后跟 id 字符的片段
fn extract_spawn_id(output: &str) -> String {
    let bytes = output.as_bytes();
    let id_chars = |c: u8| c.is_ascii_alphanumeric() || c == b'-';
    let mut start = 0;
    while let Some(pos) = output[start..].find("sp-") {
        let abs = start + pos;
        let end = bytes[abs..].iter().take_while(|&&c| id_chars(c)).count();
        if end > 3 {
            // sp- 后还有 id 字符才算有效 id（排除「sp-」悬空后缀）
            return output[abs..abs + end].to_string();
        }
        start = abs + 3;
    }
    String::new()
}

/// 排队中计数（排除刚入队的这条——入队提示「前面还有几项」用）
fn queued_count(session_id: &str, exclude_id: &str) -> usize {
    task_queue::list(session_id)
        .iter()
        .filter(|t| t.status == "queued" && t.id != exclude_id)
        .count()
}

/// 广播队列更新事件（WebUI 实时渲染）
fn emit_updated(core: &Core, session_id: &str) {
    let items = task_queue::list(session_id);
    let running = items.iter().filter(|t| t.status == "running").count();
    let queued = items.iter().filter(|t| t.status == "queued").count();
    core.emit_session_event(
        session_id,
        "task_queue.updated",
        serde_json::json!({ "items": items, "running": running, "queued": queued }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_id提取() {
        assert_eq!(extract_spawn_id("已派生 sp-ab12cd34 后台执行"), "sp-ab12cd34");
        assert_eq!(extract_spawn_id("拒绝：并发满"), "");
        assert_eq!(extract_spawn_id("id=sp-xyz），请查收"), "sp-xyz");
    }

    #[tokio::test]
    async fn list空队列与入队后快照() {
        let core = test_core();
        let sid = "tq-tool-ses";
        task_queue::clear(sid);
        let out = dispatch(&core, sid, serde_json::json!({"op": "list"})).await.unwrap();
        assert!(out.contains("为空"), "{out}");
        task_queue::enqueue(sid, "测试任务：检索全库", "queued", "").unwrap();
        let out = dispatch(&core, sid, serde_json::json!({"op": "list"})).await.unwrap();
        assert!(out.contains("1 项"), "{out}");
        assert!(out.contains("检索全库"), "{out}");
        task_queue::clear(sid);
    }

    #[tokio::test]
    async fn cancel排队中任务() {
        let core = test_core();
        let sid = "tq-tool-ses2";
        task_queue::clear(sid);
        let tid = task_queue::enqueue(sid, "要取消的任务", "queued", "").unwrap();
        let out = dispatch(&core, sid, serde_json::json!({"op": "cancel", "taskId": tid, "reason": "不需要了"})).await.unwrap();
        assert!(out.contains("已取消"), "{out}");
        let listed = task_queue::list(sid);
        assert_eq!(listed[0].status, "cancelled");
        // 再取消一次：已是终态
        let out = dispatch(&core, sid, serde_json::json!({"op": "cancel", "taskId": tid})).await.unwrap();
        assert!(out.contains("终态"), "{out}");
        task_queue::clear(sid);
    }

    fn test_core() -> Arc<Core> {
        let dir = std::env::temp_dir().join(format!("exm-tq-{}", exm_core::types::new_id()));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        Arc::new(Core::create(&dir).expect("创建 Core 失败"))
    }
}
