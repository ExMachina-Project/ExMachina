//! 计划模式工具（plan__mode）：把「先计划再执行」的节奏决定权交给智能体。
//!
//! 此前模式由 WebUI Segmented 硬开关（POST setMode）切换——用户必须手动切，
//! 与「agent 自主决定先计划再执行」的诉求相悖。现在：
//! - 用户对话中要求「先做计划」→ agent 调 plan__mode op=set plan=true →
//!   写类工具被拦截为计划条目（gateway.rs 拦截机制不变）；
//! - 计划批准后回放执行（plan_approve 原有机制），批准即自动回 execute；
//! - agent 也可自主 op=set plan=false 提前回到执行模式（如计划被拒后调整）。
//!
//! 注入口径：全会话默认注入（serve 装配），与 task__queue 同构。

use exm_core::config::CustomTool;
use exm_core::tools::{dynamic_exec, DynamicToolExec};
use exm_core::Core;
use std::sync::Arc;

/// 工具 schema（单工具两操作）
pub(crate) fn tool_spec() -> CustomTool {
    CustomTool {
        name: "plan__mode".into(),
        description: "计划模式：自主决定任务节奏——复杂/高风险/多步写入类任务先做计划再执行。\nop=set plan=true 进入计划模式：此后所有写类操作（写文件/终端变更类/发消息等）不再直接执行，而是累积为计划条目呈现给用户审批；读类操作照常执行（调查、分析不受限）。\n计划条目攒齐后停止本轮（告知用户等待审批）——不要在计划模式下反复空转。\n用户批准后系统自动回放执行并回到执行模式；被拒绝时按反馈调整后再次进入计划模式。\nop=get 查询当前模式与待审批条目数。简单问答、纯读类任务不要进计划模式。".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["set", "get"], "description": "操作：set 切换模式 / get 查询当前状态" },
                "plan": { "type": "boolean", "description": "op=set 必填：true 进入计划模式（写操作转计划条目）/ false 回到执行模式" },
                "reason": { "type": "string", "description": "可选：切换原因（呈现给用户，说明为什么先计划）" }
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

/// 工具分发
async fn dispatch(core: &Arc<Core>, session_id: &str, args: serde_json::Value) -> anyhow::Result<String> {
    let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("");
    match op {
        "set" => op_set(core, session_id, &args).await,
        "get" => Ok(op_get(session_id)),
        _ => Err(anyhow::anyhow!("未知 op：{op}（支持 set / get）")),
    }
}

/// op=set：切换模式并广播事件（前端 plan 徽标实时刷新）
async fn op_set(core: &Arc<Core>, session_id: &str, args: &serde_json::Value) -> anyhow::Result<String> {
    let plan = args
        .get("plan")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| anyhow::anyhow!("op=set 需要 plan 布尔值"))?;
    let reason = args.get("reason").and_then(|v| v.as_str()).unwrap_or("");
    core.set_plan_mode(session_id, plan);
    core.emit_session_event(
        session_id,
        "plan.mode",
        serde_json::json!({
            "mode": if plan { "plan" } else { "execute" },
            "reason": reason,
            "source": "agent",
        }),
    );
    let pending = exm_core::plan::list_items(session_id).len();
    let suffix = if reason.is_empty() { "。".to_string() } else { format!("（{reason}）。") };
    Ok(if plan {
        format!("已进入计划模式{suffix}写操作将累积为计划条目呈现给用户审批（当前待审批 {pending} 条）。读类操作照常执行；计划攒齐后停止本轮并告知用户等待审批。")
    } else {
        format!("已回到执行模式{suffix}写操作恢复直接执行（未审批计划条目仍保留 {pending} 条，可经批准回放）。")
    })
}

/// op=get：当前模式 + 待审批条目数
fn op_get(session_id: &str) -> String {
    let plan = exm_core::plan::is_plan_mode(session_id);
    let pending = exm_core::plan::list_items(session_id).len();
    format!(
        "当前模式：{}；待审批计划条目：{pending} 条",
        if plan { "计划模式（写操作转计划条目）" } else { "执行模式（写操作直接执行）" }
    )
}
