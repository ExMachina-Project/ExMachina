//! 计划模式（spec: plan-mode）：只读规划 → 计划清单 → 审批切换执行。
//!
//! 会话模式位 `plan | execute`（默认 execute）为进程级注册表（与既有
//! session_modes 同口径——模式是轮次级行为，不落库）。
//!
//! plan 模式下工具网关拦截写类工具调用（filesystem.write / edit / patch /
//! terminal / computer 输入 / git 写），转为 PlanItem 追加会话计划清单；
//! 读取类工具放行。批准 = 清单整体回放并切 execute；拒绝 = 保留清单。

use crate::types::ToolName;
use std::collections::HashMap;
use std::sync::OnceLock;

/// 会话计划项：被拦截的写操作转换而来
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanItem {
    pub id: String,
    /// 拦截的工具（edit/patch/filesystem/terminal/computer/git）
    pub tool: String,
    /// 目标（文件路径 / 命令 / 动作描述）
    pub target: String,
    /// 动作类型（write/edit/patch/exec/input/git）
    pub action: String,
    /// 理由（模型发起调用时的上下文说明；工具参数原文携带）
    pub args: serde_json::Value,
    /// 生成时间
    pub created_at: String,
}

/// 判定工具调用在 plan 模式下是否拦截（写类）
pub fn is_write_tool(tool: ToolName, args: &serde_json::Value) -> bool {
    match tool {
        ToolName::Edit | ToolName::Patch => true,
        ToolName::Filesystem => {
            args.get("op").and_then(|v| v.as_str()).unwrap_or("write") != "list"
        }
        ToolName::Terminal => {
            // poll/kill 是对既有任务的操作；run 才是新执行
            args.get("op").and_then(|v| v.as_str()).unwrap_or("run") == "run"
        }
        ToolName::Computer => {
            // 截屏只读放行；输入类拦截
            let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("screenshot");
            op != "screenshot"
        }
        ToolName::Git => {
            matches!(
                args.get("action").and_then(|v| v.as_str()).unwrap_or(""),
                "add" | "commit" | "restore" | "stash-push" | "stash-pop" | "checkout-b"
            )
        }
        ToolName::Spawn => true, // 派生子智能体也属执行动作，规划期拦截
        _ => false,
    }
}

/// 工具调用 → 计划条目
pub fn to_plan_item(tool: ToolName, args: &serde_json::Value) -> PlanItem {
    let target = match tool {
        ToolName::Edit | ToolName::Patch => args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        ToolName::Filesystem => args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        ToolName::Terminal => args
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        ToolName::Computer => args
            .get("op")
            .and_then(|v| v.as_str())
            .unwrap_or("input")
            .to_string(),
        ToolName::Git => format!(
            "git {}",
            args.get("action").and_then(|v| v.as_str()).unwrap_or("")
        ),
        ToolName::Spawn => args
            .get("task")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .chars()
            .take(80)
            .collect(),
        _ => String::new(),
    };
    let action = match tool {
        ToolName::Edit => "edit",
        ToolName::Patch => "patch",
        ToolName::Filesystem => "write",
        ToolName::Terminal => "exec",
        ToolName::Computer => "input",
        ToolName::Git => "git",
        ToolName::Spawn => "spawn",
        _ => "other",
    }
    .to_string();
    PlanItem {
        id: format!("pi-{}", &crate::types::new_id()[..8]),
        tool: tool.key().to_string(),
        target,
        action,
        args: args.clone(),
        created_at: crate::types::now_iso(),
    }
}

// ---------------- 会话模式位与计划清单 ----------------

fn plan_modes() -> &'static parking_lot::Mutex<HashMap<String, bool>> {
    static MODES: OnceLock<parking_lot::Mutex<HashMap<String, bool>>> = OnceLock::new();
    MODES.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

/// 计划清单（会话 → 条目；进程级——审批前有效）
fn plan_items() -> &'static parking_lot::Mutex<HashMap<String, Vec<PlanItem>>> {
    static ITEMS: OnceLock<parking_lot::Mutex<HashMap<String, Vec<PlanItem>>>> = OnceLock::new();
    ITEMS.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

/// 会话 → 关联审批单 id（复用 approvals 通道；批准/拒绝时了结）
fn pending_approvals() -> &'static parking_lot::Mutex<HashMap<String, String>> {
    static APS: OnceLock<parking_lot::Mutex<HashMap<String, String>>> = OnceLock::new();
    APS.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

/// 登记会话计划清单的关联审批单（清单出现首条时调用）
pub fn set_pending_approval(session_id: &str, approval_id: &str) {
    pending_approvals()
        .lock()
        .insert(session_id.to_string(), approval_id.to_string());
}

/// 取出并清除关联审批单（批准/拒绝时由调用方了结记录）
pub fn take_pending_approval(session_id: &str) -> Option<String> {
    pending_approvals().lock().remove(session_id)
}

/// 设置会话计划模式（true = plan；false/清除 = execute）
pub fn set_plan_mode(session_id: &str, plan: bool) {
    if plan {
        plan_modes().lock().insert(session_id.to_string(), true);
    } else {
        plan_modes().lock().remove(session_id);
    }
}

/// 查询会话是否处于计划模式
pub fn is_plan_mode(session_id: &str) -> bool {
    plan_modes()
        .lock()
        .get(session_id)
        .copied()
        .unwrap_or(false)
}

/// 追加计划条目（拦截发生时调用）
pub fn push_item(session_id: &str, item: PlanItem) {
    plan_items()
        .lock()
        .entry(session_id.to_string())
        .or_default()
        .push(item);
}

/// 取会话计划清单快照
pub fn list_items(session_id: &str) -> Vec<PlanItem> {
    plan_items()
        .lock()
        .get(session_id)
        .cloned()
        .unwrap_or_default()
}

/// 批准：取出全部条目（调用方负责回放执行）并切 execute 模式
pub fn approve(session_id: &str) -> Vec<PlanItem> {
    set_plan_mode(session_id, false);
    plan_items().lock().remove(session_id).unwrap_or_default()
}

/// 拒绝：保留清单（可继续修改），不切模式
pub fn reject(session_id: &str) -> Vec<PlanItem> {
    list_items(session_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 写类工具判定() {
        assert!(is_write_tool(ToolName::Edit, &serde_json::json!({})));
        assert!(is_write_tool(ToolName::Patch, &serde_json::json!({})));
        assert!(is_write_tool(
            ToolName::Filesystem,
            &serde_json::json!({ "op": "write" })
        ));
        assert!(!is_write_tool(
            ToolName::Filesystem,
            &serde_json::json!({ "op": "list" })
        ));
        assert!(is_write_tool(
            ToolName::Terminal,
            &serde_json::json!({ "command": "ls" })
        ));
        assert!(!is_write_tool(
            ToolName::Terminal,
            &serde_json::json!({ "op": "poll", "taskId": "t1" })
        ));
        assert!(!is_write_tool(
            ToolName::Computer,
            &serde_json::json!({ "op": "screenshot" })
        ));
        assert!(is_write_tool(
            ToolName::Computer,
            &serde_json::json!({ "op": "click", "x": 1, "y": 2 })
        ));
        assert!(is_write_tool(
            ToolName::Git,
            &serde_json::json!({ "action": "commit", "message": "x" })
        ));
        assert!(!is_write_tool(
            ToolName::Git,
            &serde_json::json!({ "action": "status" })
        ));
        assert!(!is_write_tool(ToolName::Read, &serde_json::json!({})));
        assert!(!is_write_tool(ToolName::Grep, &serde_json::json!({})));
    }

    #[test]
    fn 计划条目转换() {
        let item = to_plan_item(
            ToolName::Edit,
            &serde_json::json!({ "path": "src/a.rs", "old_string": "x", "new_string": "y" }),
        );
        assert_eq!(item.tool, "edit");
        assert_eq!(item.target, "src/a.rs");
        assert_eq!(item.action, "edit");
    }

    #[test]
    fn 模式位与清单生命周期() {
        let sid = "plan-test-1";
        assert!(!is_plan_mode(sid));
        set_plan_mode(sid, true);
        assert!(is_plan_mode(sid));
        push_item(
            sid,
            to_plan_item(ToolName::Terminal, &serde_json::json!({ "command": "ls" })),
        );
        assert_eq!(list_items(sid).len(), 1);
        // 拒绝：保留
        let kept = reject(sid);
        assert_eq!(kept.len(), 1);
        assert_eq!(list_items(sid).len(), 1);
        // 批准：取出并切 execute
        let taken = approve(sid);
        assert_eq!(taken.len(), 1);
        assert!(!is_plan_mode(sid));
        assert!(list_items(sid).is_empty());
    }
}
