//! tools/approval.rs —— 自 tools.rs 按领域拆出（纯搬移，零行为变更）

use super::*;

impl ToolGateway {
    /// 审批闸门：落审批单 → 发 `approval.required`（带会话归属，聊天页可内联裁决）→
    /// 原地等待人工决定（`security.approvalWaitSecs`，0 = 不等待直接受阻）。
    /// - 批准：`Core::approval_decide` 系统代执行并把输出写回审批单，本侧读回作为工具结果
    ///   ——模型拿到真实执行输出，流程不断；
    /// - 拒绝：把拒绝原因作为工具结果回灌；
    /// - 超时：按「待人工审批」受阻回流（旧行为）。
    pub(crate) async fn gate_command(&self, agent_id: &str, session_id: &str, cmd: &str) -> ToolResult {
        // 来源升级标：通道闸门登记过来源的会话（member）→ 审批单标记须管理员批准
        let origin = self.store.get_session_origin(session_id).ok().flatten();
        let req = ApprovalRequest {
            id: crate::types::new_id()[..8].to_string(),
            session_id: session_id.to_string(),
            node_id: None,
            agent_id: agent_id.to_string(),
            command: cmd.to_string(),
            status: "pending".into(),
            result: None,
            created_at: crate::types::now_iso(),
            decided_at: None,
            origin_user: origin.as_ref().map(|(u, _)| u.clone()),
            origin_role: origin.as_ref().map(|(_, r)| r.clone()),
        };
        let _ = self.store.add_approval(&req);
        let _ = self.events.send(CoreEvent {
            kind: "approval.required".into(),
            session_id: session_id.to_string(),
            payload: serde_json::json!({
                "approvalId": req.id, "agentId": agent_id, "command": cmd,
            }),
        });
        let wait = self.security.approval_wait_secs;
        if wait == 0 {
            return ToolResult::err(format!(
                "命令已拦截待人工审批（审批单 {}）：{cmd}。回流受阻原因；用户批准后由系统代执行。",
                req.id
            ));
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(wait);
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            let Ok(Some(latest)) = self.store.get_approval(&req.id) else {
                continue;
            };
            match latest.status.as_str() {
                "executed" => {
                    let out = latest.result.unwrap_or_default();
                    return ToolResult::ok(format!(
                        "【报告】命令已被用户批准并由系统代执行：{cmd}\n执行输出：\n{out}"
                    ));
                }
                "failed" => {
                    let out = latest.result.unwrap_or_default();
                    return ToolResult::err(format!(
                        "命令已被用户批准并代执行，但执行失败：{cmd}\n输出：\n{out}"
                    ));
                }
                "denied" => {
                    return ToolResult::err(format!(
                        "用户拒绝执行该命令（审批单 {}）：{cmd}。请改用其他方案或向用户说明。",
                        req.id
                    ));
                }
                _ => {}
            }
            if tokio::time::Instant::now() >= deadline {
                return ToolResult::err(format!(
                    "命令已拦截待人工审批（审批单 {}）：{cmd}。等待用户决定超时（{wait}s），按受阻回流；批准后输出不会自动回灌本轮。",
                    req.id
                ));
            }
        }
    }
}
