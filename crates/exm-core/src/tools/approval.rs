//! Approval flows for command, terminal, and computer actions.

use super::*;

impl ToolGateway {
    /// 审批闸门：落审批单 → 发 `approval.required`（带会话归属，聊天页可内联裁决）→
    /// 原地等待人工决定（`security.approvalWaitSecs`，0 = 不等待直接受阻）。
    /// - 批准：`Core::approval_decide` 系统代执行并把输出写回审批单，本侧读回作为工具结果
    ///   ——模型拿到真实执行输出，流程不断；
    /// - 拒绝：把拒绝原因作为工具结果回灌；
    /// - 超时：按「待人工审批」受阻回流（旧行为）。
    pub(crate) async fn gate_command(
        &self,
        agent_id: &str,
        session_id: &str,
        cmd: &str,
    ) -> ToolResult {
        self.gate_command_inner(agent_id, session_id, cmd, None)
            .await
    }

    pub(crate) async fn gate_terminal(
        &self,
        agent_id: &str,
        session_id: &str,
        request: super::terminal_jobs::TerminalApprovalRequest,
    ) -> ToolResult {
        let display = format!("[terminal] {}", request.approval_command);
        self.gate_command_inner(agent_id, session_id, &display, Some(request))
            .await
    }

    /// Computer 输入使用独立的类型化审批路径。审批卡中的 description 仅供人阅读，
    /// 永远不会进入 shell，也不会在等待审批期间初始化宿主桌面的机械驱动。
    pub(super) async fn gate_computer(
        &self,
        agent_id: &str,
        session_id: &str,
        action: super::computer::ComputerApprovalAction,
    ) -> ToolResult {
        let wait = self.security.approval_wait_secs;
        let origin = self.store.get_session_origin(session_id).ok().flatten();
        let req = ApprovalRequest {
            id: crate::types::new_id()[..8].to_string(),
            session_id: session_id.to_string(),
            node_id: None,
            agent_id: agent_id.to_string(),
            command: format!("[computer] {}", action.description),
            status: "pending".into(),
            result: None,
            created_at: crate::types::now_iso(),
            decided_at: None,
            origin_user: origin.as_ref().map(|(user, _)| user.clone()),
            origin_role: origin.as_ref().map(|(_, role)| role.clone()),
            meta_json: Some(
                serde_json::json!({
                    "computerAction": action,
                })
                .to_string(),
            ),
        };
        let _ = self.store.add_approval(&req);
        let _ = self.events.send(CoreEvent {
            kind: "approval.required".into(),
            session_id: session_id.to_string(),
            payload: serde_json::json!({
                "approvalId": req.id,
                "agentId": agent_id,
                "tool": "computer",
                "command": req.command.clone(),
                "action": serde_json::from_str::<serde_json::Value>(
                    req.meta_json.as_deref().unwrap_or("{}")
                )
                .ok()
                .and_then(|meta| meta.get("computerAction").cloned()),
            }),
        });
        if wait == 0 {
            return ToolResult::err(format!(
                "Computer Use 输入已拦截待人工审批（审批单 {}）：{}。批准后由专用机械执行器执行。",
                req.id, req.command
            ));
        }

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(wait);
        loop {
            if crate::round_trace::is_cancelled(session_id) {
                // Core 的取消入口在 approval_locks 下收束待处理单；这里不直接
                // 写回状态，避免与已经开始的批准回放发生无锁覆盖。
                return ToolResult::err(format!(
                    "本轮已被用户停止，Computer Use 动作未执行（审批单 {}）",
                    req.id
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let Ok(Some(latest)) = self.store.get_approval(&req.id) else {
                continue;
            };
            match latest.status.as_str() {
                "executed" => {
                    return ToolResult::ok(format!(
                        "【报告】Computer Use 动作已批准并执行：{}",
                        latest.result.unwrap_or_default()
                    ));
                }
                "failed" => {
                    return ToolResult::err(format!(
                        "Computer Use 动作已批准但执行失败：{}",
                        latest.result.unwrap_or_default()
                    ));
                }
                "denied" | "cancelled" | "expired" => {
                    return ToolResult::err(format!(
                        "Computer Use 动作未执行（审批单 {}）：{}",
                        req.id,
                        latest.result.unwrap_or_else(|| "用户未批准".into())
                    ));
                }
                _ => {}
            }
            if tokio::time::Instant::now() >= deadline {
                // 不在 ToolGateway 中抢写审批状态：Core 的裁决互斥锁才是
                // exactly-once 所有者。动作自身的 expiresAtMs 阻止超时后迟到批准。
                return ToolResult::err(format!(
                    "Computer Use 输入审批超时（审批单 {}），动作未执行",
                    req.id
                ));
            }
        }
    }

    async fn gate_command_inner(
        &self,
        agent_id: &str,
        session_id: &str,
        cmd: &str,
        terminal: Option<super::terminal_jobs::TerminalApprovalRequest>,
    ) -> ToolResult {
        // 会话授权短路（approval-ergonomics）：「本会话允许」档已放行的命令前缀
        // 不再落审批单,直通执行（同一加固路径——execUser 降权/沙箱净化照常生效,
        // 审批放行的是命令内容不是特权）。[root]/破坏性命令在写入侧就进不了
        // 授权表,此处无需重复校验。guard 作用域收窄在 await 前（读守卫非 Send）。
        let granted = {
            let probe = self.session_grant_probe.read().clone();
            let grant_command = terminal
                .as_ref()
                .map(|request| request.approval_command.as_str())
                .unwrap_or(cmd);
            probe.is_some_and(|p| p(session_id, grant_command))
        };
        if granted {
            if let Some(request) = terminal.as_ref() {
                let workspace = match self.workspace_for_session(session_id).canonicalize() {
                    Ok(workspace) => workspace,
                    Err(error) => return ToolResult::err(format!("审批执行项目不可用: {error}")),
                };
                return self
                    .execute_terminal_request(request, session_id, workspace)
                    .await;
            }
            // 先算 opts 再 await：shell_opts 内部的身份解析含非 Send 中间态,
            // 其结果（ShellOpts 本身 Send）跨 await 持有即可
            let opts = self.shell_opts(cmd);
            let workspace = self.workspace_for_session(session_id);
            let timeout = self.security.terminal_timeout_secs.max(1) as u64;
            let out = crate::tools::execute_shell_for_session(
                &workspace, cmd, timeout, &opts, session_id,
            )
            .await;
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
        let wait = self.security.approval_wait_secs;
        let (approval_command, meta_json) = if let Some(request) = terminal.as_ref() {
            let workspace = match self.workspace_for_session(session_id).canonicalize() {
                Ok(workspace) => workspace,
                Err(error) => {
                    return ToolResult::err(format!("无法保存审批项目边界: {error}"));
                }
            };
            let action = super::terminal_jobs::TerminalApprovalAction::from_request(
                request, &workspace, wait,
            );
            let approval_command = format!("[terminal] {}", request.approval_command);
            let meta = serde_json::json!({
                "projectWorkspace": workspace,
                "terminalAction": action,
            });
            (approval_command, Some(meta.to_string()))
        } else {
            (
                cmd.to_string(),
                Some(
                    serde_json::json!({
                        "projectWorkspace": self.workspace_for_session(session_id)
                    })
                    .to_string(),
                ),
            )
        };
        let req = ApprovalRequest {
            id: crate::types::new_id()[..8].to_string(),
            session_id: session_id.to_string(),
            node_id: None,
            agent_id: agent_id.to_string(),
            command: approval_command,
            status: "pending".into(),
            result: None,
            created_at: crate::types::now_iso(),
            decided_at: None,
            origin_user: origin.as_ref().map(|(u, _)| u.clone()),
            origin_role: origin.as_ref().map(|(_, r)| r.clone()),
            meta_json,
        };
        let _ = self.store.add_approval(&req);
        let _ = self.events.send(CoreEvent {
            kind: "approval.required".into(),
            session_id: session_id.to_string(),
            payload: serde_json::json!({
                "approvalId": req.id, "agentId": agent_id, "command": cmd,
            }),
        });
        if wait == 0 {
            return ToolResult::err(format!(
                "命令已拦截待人工审批（审批单 {}）：{cmd}。回流受阻原因；用户批准后由系统代执行。",
                req.id
            ));
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(wait);
        loop {
            if crate::round_trace::is_cancelled(session_id) {
                return ToolResult::err(format!(
                    "本轮已被用户停止，待审批命令未执行（审批单 {}）",
                    req.id
                ));
            }
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
                "cancelled" => {
                    return ToolResult::err(format!(
                        "本轮已被用户停止，待审批命令未执行（审批单 {}）：{cmd}",
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
