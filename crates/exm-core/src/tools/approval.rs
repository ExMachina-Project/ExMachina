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
        // 会话授权短路（approval-ergonomics）：「本会话允许」档已放行的命令前缀
        // 不再落审批单,直通执行（同一加固路径——execUser 降权/沙箱净化照常生效,
        // 审批放行的是命令内容不是特权）。[root]/破坏性命令在写入侧就进不了
        // 授权表,此处无需重复校验。guard 作用域收窄在 await 前（读守卫非 Send）。
        let granted = {
            let probe = self.session_grant_probe.read().clone();
            probe.is_some_and(|p| p(session_id, cmd))
        };
        if granted {
            // 先算 opts 再 await：shell_opts 内部的身份解析含非 Send 中间态,
            // 其结果（ShellOpts 本身 Send）跨 await 持有即可
            let opts = self.shell_opts(cmd);
            let workspace = self.workspace_root.clone();
            let timeout = self.security.terminal_timeout_secs.max(1) as u64;
            let out = crate::tools::execute_shell_with(&workspace, cmd, timeout, &opts).await;
            return if out.ok {
                ToolResult::ok(out.output)
            } else {
                ToolResult::err(format!(
                    "命令执行失败（会话授权直通）：{}\n{}",
                    cmd,
                    out.error.unwrap_or_default()
                ))
            };
        }
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
            meta_json: None,
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
