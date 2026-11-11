//! core/approvals.rs —— Core 审批域（codebase-refactor 拆分）：审批单清单/查询/
//! 决定执行（decide 含会话内授权档）、会话级命令前缀授权（「本会话允许」）、
//! WebUI git 破坏性操作审批单、计划模式裁决（set_plan_mode/plan_list/
//! plan_reject/plan_approve）。纯搬移：方法签名与实现零改动。

use super::super::Core;
use crate::types::*;
use std::sync::Arc;

impl Core {
    /// 会话审批授权写入（approval-ergonomics「本会话允许」档）：
    /// 前缀口径见 allowlist::command_head。红线校验在 approval_decide 调用侧——
    /// [root] 提权单与破坏性命令不会走到这里。
    pub fn grant_session(&self, session_id: &str, prefix: &str) {
        if prefix.is_empty() {
            return;
        }
        self.session_grants
            .lock()
            .entry(session_id.to_string())
            .or_default()
            .insert(prefix.to_string(), ());
    }
    /// 会话授权命中判定：命令前缀在授权表内即视为已批准。
    /// 返回命中的前缀（调试/审计可见），未命中返回 None。
    pub fn session_granted(&self, session_id: &str, cmd: &str) -> Option<String> {
        let head = crate::tools::command_head(cmd);
        if head.is_empty() {
            return None;
        }
        self.session_grants
            .lock()
            .get(session_id)?
            .contains_key(&head)
            .then_some(head)
    }
    pub fn approval_list(
        &self,
        status: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<ApprovalRequest>> {
        self.store.list_approvals(status, limit)
    }
    pub fn approval_get(&self, id: &str) -> anyhow::Result<Option<ApprovalRequest>> {
        self.store.get_approval(id)
    }
    /// 审批决定：批准即由系统代执行并记录输出；拒绝即关闭审批单
    pub async fn approval_decide(
        &self,
        id: &str,
        approve: bool,
    ) -> anyhow::Result<ApprovalRequest> {
        self.approval_decide_scoped(id, ApprovalScope::Once { approve }).await
    }
    /// 审批决定（approval-ergonomics 三档粒度）：
    /// - Once：本次执行（旧行为，缺省档）
    /// - Session：本次执行 + 同命令前缀本会话后续自动放行（内存态）
    /// - Allowlist：本次执行 + 前缀写入 security.execAllowlist（持久）
    ///
    /// 红线：[root] 等特殊前缀单与破坏性命令不支持 Session/Allowlist 档——
    /// 越权档位自动降级 Once。
    pub async fn approval_decide_scoped(
        &self,
        id: &str,
        decision: ApprovalScope,
    ) -> anyhow::Result<ApprovalRequest> {
        // 决定互斥（竞态 P1-1）：双击批准/WebUI+通道并发时，同一审批单只有一个
        // 决定流能走下去——否则双执行命令（执行窗口可达数分钟）。占不到锁 =
        // 另一个决定在途：等它收束后返回最新单据（语义：「你的点击已生效，
        // 由先到者代执行」而非报错）。
        let lock = {
            let mut locks = self.approval_locks.lock();
            locks
                .entry(id.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let Ok(_guard) = lock.try_lock() else {
            // 另一个决定在途：轮询等最终态（上限 60s，防永挂）
            for _ in 0..120 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if let Some(req) = self.store.get_approval(id)? {
                    if req.status != "pending" {
                        return Ok(req);
                    }
                }
            }
            anyhow::bail!("审批决定超时：另一决定长时间未收束");
        };
        let mut req = self
            .store
            .get_approval(id)?
            .ok_or_else(|| anyhow::anyhow!("审批单不存在: {id}"))?;
        if req.status != "pending" {
            anyhow::bail!("审批单已处理: {}", req.status);
        }
        // 红线校验：特殊前缀单与破坏性命令不允许升档（拒绝类决定不受限）
        let effective = if decision.is_approve() {
            let special = req.command.starts_with('[');
            let destructive =
                crate::tools::is_destructive_command(&req.command.to_lowercase());
            if matches!(
                decision,
                ApprovalScope::Session | ApprovalScope::Allowlist
            ) && (special || destructive)
            {
                ApprovalScope::Once { approve: true }
            } else {
                decision
            }
        } else {
            ApprovalScope::Once { approve: false }
        };
        let approve = effective.is_approve();
        // 视频动作审批单（[video] 前缀）：批准 = 回调代发执行器，拒绝 = 记录跳过
        if req.command.starts_with("[video]") {
            let payload = req.command.trim_start_matches("[video]").trim().to_string();
            req.decided_at = Some(now_iso());
            if !approve {
                req.status = "denied".into();
                req.result = Some("视频互动被拒绝（未发送）".into());
                self.store.update_approval(&req)?;
                self.emit_approval_resolved(&req);
                return Ok(req);
            }
            match self.execute_video_act_approval(&payload).await {
                Ok(out) => {
                    req.status = "executed".into();
                    req.result = Some(out.chars().take(4000).collect());
                }
                Err(e) => {
                    req.status = "failed".into();
                    req.result = Some(e.to_string());
                }
            }
            self.store.update_approval(&req)?;
            self.emit_approval_resolved(&req);
            return Ok(req);
        }
        // 计划审批单（[plan] 前缀）：转发计划通道——批准 = 回放清单，拒绝 = 保留清单
        if req.command.starts_with("[plan]") {
            let sid = req.session_id.clone();
            if approve {
                let results = self.plan_approve(&sid).await;
                req.status = match results {
                    Ok(rs) if rs.iter().all(|(_, r)| r.ok) => "executed".into(),
                    Ok(_) => "failed".into(),
                    Err(e) => {
                        req.status = "failed".into();
                        req.result = Some(e.to_string());
                        self.store.update_approval(&req)?;
                        self.emit_approval_resolved(&req);
                        return Ok(req);
                    }
                };
                req.result = Some("计划已回放执行".into());
            } else {
                self.plan_reject(&sid);
                req.status = "denied".into();
                req.result = Some("计划被拒绝（清单保留可继续修改）".into());
            }
            req.decided_at = Some(now_iso());
            self.store.update_approval(&req)?;
            self.emit_approval_resolved(&req);
            return Ok(req);
        }
        // 元智能体提议单（[meta] 前缀，meta-agent）：批准 = 按 kind 代为应用
        // （prompt 补段/adaptation 版本化/roster 两阶段/build 注册）;拒绝 = 受阻回流。
        // 内置组保护与主智能体校验沿 agent_manage 同路径,提议不绕过任何闸门。
        if req.command.starts_with("[meta]") {
            req.decided_at = Some(now_iso());
            if !approve {
                req.status = "denied".into();
                req.result = Some("元智能体提议被拒绝".into());
                self.store.update_approval(&req)?;
                self.emit_approval_resolved(&req);
                return Ok(req);
            }
            let outcome = self.apply_meta_proposal(&req).await;
            let (status, text) = match outcome {
                Ok(msg) => ("executed".to_string(), msg),
                Err(e) => ("failed".to_string(), e.to_string()),
            };
            req.status = status;
            req.result = Some(text.chars().take(4000).collect());
            self.store.update_approval(&req)?;
            self.emit_approval_resolved(&req);
            let _ = self.events.send(CoreEvent {
                kind: "meta.applied".into(),
                session_id: req.session_id.clone(),
                payload: serde_json::json!({
                    "approvalId": req.id,
                    "ok": req.status == "executed",
                    "command": req.command,
                }),
            });
            return Ok(req);
        }
        // root 提权审批单（[root] 前缀，agent-exec-confinement）：批准 = 以 root 身份
        // 代执行（剥冗余 sudo 壳、保留 su 切换语义、不走 execUser 降权）；拒绝 = 受阻回流
        if req.command.starts_with("[root]") {
            let raw_cmd = req.command.trim_start_matches("[root]").trim().to_string();
            req.decided_at = Some(now_iso());
            if !approve {
                req.status = "denied".into();
                req.result = Some("root 提权被拒绝".into());
                self.store.update_approval(&req)?;
                self.emit_approval_resolved(&req);
                return Ok(req);
            }
            // 剥离 sudo/doas/run0 冗余壳（root 下多余）；su/sudo -u/runuser 保留语义
            let eff_cmd = crate::tools::strip_redundant_elevation(&raw_cmd);
            let workspace = self.config().workspace_root.clone();
            // root 代执行：ShellOpts::plain（不降权——审批的就是提权本身）
            let opts = crate::tools::ShellOpts::plain();
            let out = crate::tools::execute_shell_with(&workspace, &eff_cmd, 300, &opts).await;
            req.status = if out.ok { "executed".into() } else { "failed".into() };
            let text = if out.ok { out.output } else { out.error.unwrap_or_default() };
            // 红线降级注明（approval-ergonomics）：[root] 提权单不允许升档,
            // 越权档位自动降级 once——审批结果注明让裁决者知情
            let downgrade_note = if matches!(
                decision,
                ApprovalScope::Session | ApprovalScope::Allowlist
            ) {
                "\n[红线] 提权单不支持会话授权/白名单档，已按本次执行"
            } else {
                ""
            };
            req.result = Some(
                format!("[root 执行] {eff_cmd}\n{}{downgrade_note}", text)
                    .chars()
                    .take(4000)
                    .collect(),
            );
            self.store.update_approval(&req)?;
            self.emit_approval_resolved(&req);
            return Ok(req);
        }
        req.decided_at = Some(now_iso());
        if !approve {
            req.status = "denied".into();
            self.store.update_approval(&req)?;
            self.emit_approval_resolved(&req);
            return Ok(req);
        }
        // 普通审批单代执行收口（agent-exec-confinement）：与直通执行同一加固路径——
        // 审批放行的是命令内容,不是特权;execUser 降权/沙箱净化照常生效。
        // [红线] 前缀（agent-budget-audit 3.3 分档闸门标记）不是 shell 语法，
        // 代执行前剥掉——审批的是命令本身，标记只用于档位溯源
        let eff_cmd = req
            .command
            .strip_prefix("[红线]")
            .map(|c| c.trim().to_string())
            .unwrap_or_else(|| req.command.clone());
        let workspace = self.config().workspace_root.clone();
        let opts = self.orchestrator().tools.shell_opts(&eff_cmd);
        let out = crate::tools::execute_shell_with(&workspace, &eff_cmd, 300, &opts).await;
        req.status = if out.ok {
            "executed".into()
        } else {
            "failed".into()
        };
        let text = if out.ok {
            out.output
        } else {
            out.error.unwrap_or_default()
        };
        req.result = Some(text.chars().take(4000).collect());
        // 档位副作用（仅普通命令单走到这里;特殊前缀单已在上方分流 return）。
        // 执行失败也建授权/白名单?不——失败说明命令本身有问题,放行同类无意义,
        // 且用户批的是「这类命令可以跑」的意图以成功执行为前提更符合直觉。
        if out.ok {
            match effective {
                ApprovalScope::Session => {
                    let prefix = crate::tools::command_head(&req.command);
                    self.grant_session(&req.session_id, &prefix);
                    req.result = Some(format!(
                        "{}\n[会话授权] 同前缀命令「{prefix}」本会话内自动放行",
                        req.result.as_deref().unwrap_or("")
                    ));
                }
                ApprovalScope::Allowlist => {
                    let prefix = crate::tools::command_head(&req.command);
                    if !prefix.is_empty() {
                        let mut cfg = self.config().as_ref().clone();
                        if !cfg.security.exec_allowlist.iter().any(|p| p == &prefix) {
                            cfg.security.exec_allowlist.push(prefix.clone());
                            self.apply_config(cfg)?;
                        }
                        req.result = Some(format!(
                            "{}\n[白名单] 前缀「{prefix}」已写入全局白名单",
                            req.result.as_deref().unwrap_or("")
                        ));
                    }
                }
                _ => {}
            }
        }
        self.store.update_approval(&req)?;
        self.emit_approval_resolved(&req);
        Ok(req)
    }

    /// 创建 WebUI 发起的 git 破坏性操作审批单：command 为精确 git 命令，
    /// 批准后由 approval_decide 代执行（与工具审批同一执行与审计路径）
    pub async fn request_git_approval(&self, command: &str) -> anyhow::Result<ApprovalRequest> {
        let req = ApprovalRequest {
            id: crate::types::new_id(),
            session_id: String::new(),
            node_id: None,
            agent_id: "webui".into(),
            command: command.to_string(),
            status: "pending".into(),
            result: None,
            created_at: now_iso(),
            decided_at: None,
            origin_user: None,
            origin_role: None,

            meta_json: None,
        };
        self.store.add_approval(&req)?;
        let _ = self.events.send(CoreEvent {
            kind: "approval.required".into(),
            session_id: String::new(),
            payload: serde_json::json!({ "approvalId": req.id, "agentId": "webui", "command": command }),
        });
        Ok(req)
    }

    /// 设置/清除会话计划模式（true = 只读规划，写操作转计划条目）
    pub fn set_plan_mode(&self, session_id: &str, plan: bool) {
        crate::plan::set_plan_mode(session_id, plan);
        self.orchestrator.read().emit(
            session_id,
            if plan {
                "plan.enabled"
            } else {
                "plan.disabled"
            },
            serde_json::json!({}),
        );
    }
    /// 会话计划清单快照
    pub fn plan_list(&self, session_id: &str) -> Vec<crate::plan::PlanItem> {
        crate::plan::list_items(session_id)
    }
    /// 拒绝计划：保留清单（可继续修改），不切模式，广播事件；
    /// 若 approvals 通道有关联待审批单则一并了结
    pub fn plan_reject(&self, session_id: &str) -> Vec<crate::plan::PlanItem> {
        let items = crate::plan::reject(session_id);
        if let Some(ap_id) = crate::plan::take_pending_approval(session_id) {
            if let Ok(Some(mut ap)) = self.store.get_approval(&ap_id) {
                if ap.status == "pending" {
                    ap.status = "denied".into();
                    ap.decided_at = Some(now_iso());
                    ap.result = Some("计划被拒绝（清单保留可继续修改）".into());
                    // P2-7：审批了结落库失败必须可见——静默会造内存态/落库态分叉
                    if let Err(e) = self.store.update_approval(&ap) {
                        eprintln!("[approval] 计划拒绝落库失败（{}）：{e}", ap.id);
                    }
                    self.emit_approval_resolved(&ap);
                }
            }
        }
        self.orchestrator.read().emit(
            session_id,
            "plan.rejected",
            serde_json::json!({ "count": items.len() }),
        );
        items
    }
    /// 批准计划：切 execute 并逐条回放执行（terminal 走 shell 执行器，
    /// 其余工具走网关 execute——此时已非 plan 模式，不再拦截），
    /// 每条执行结果广播 plan.applied 事件
    pub async fn plan_approve(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<(crate::plan::PlanItem, crate::tools::ToolResult)>> {
        let items = crate::plan::approve(session_id);
        if items.is_empty() {
            anyhow::bail!("计划清单为空（无可批准条目）");
        }
        // approvals 通道关联单了结（executed）
        if let Some(ap_id) = crate::plan::take_pending_approval(session_id) {
            if let Ok(Some(mut ap)) = self.store.get_approval(&ap_id) {
                if ap.status == "pending" {
                    ap.status = "executed".into();
                    ap.decided_at = Some(now_iso());
                    ap.result = Some("计划已批准回放".into());
                    // P2-7：同上——落库失败留日志防状态分叉
                    if let Err(e) = self.store.update_approval(&ap) {
                        eprintln!("[approval] 计划执行落库失败（{}）：{e}", ap.id);
                    }
                    self.emit_approval_resolved(&ap);
                }
            }
        }
        self.orchestrator.read().emit(
            session_id,
            "plan.approved",
            serde_json::json!({ "count": items.len() }),
        );
        let orch = self.orchestrator.read().clone();
        let mut out = Vec::new();
        for item in items {
            let tool = crate::types::ToolName::parse(&item.tool);
            let result = match tool {
                Some(t) => {
                    orch.tools
                        .execute("plan-replay", session_id, &[t], t, &item.args)
                        .await
                }
                None => crate::tools::ToolResult::err(format!("未知工具：{}", item.tool)),
            };
            self.orchestrator.read().emit(
                session_id,
                "plan.applied",
                serde_json::json!({
                    "itemId": item.id, "tool": item.tool, "target": item.target,
                    "ok": result.ok,
                }),
            );
            out.push((item, result));
        }
        Ok(out)
    }
}
