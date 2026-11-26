//! Core approval operations: request management, execution decisions,
//! 决定执行（decide 含会话内授权档）、会话级命令前缀授权（「本会话允许」）、
//! WebUI git 破坏性操作审批单、计划模式裁决（set_plan_mode/plan_list/
//! plan_reject/plan_approve）。

use super::super::Core;
use crate::types::*;
use std::sync::Arc;

#[derive(serde::Serialize, serde::Deserialize)]
struct EditorGitAction {
    root: std::path::PathBuf,
    args: Vec<String>,
}

fn editor_git_args_valid(args: &[String]) -> bool {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["push"] | ["reset", "--hard"] => true,
        ["restore", "--", path] => {
            !path.is_empty()
                && !path.starts_with(['/', '\\'])
                && !path.contains(':')
                && !path.replace('\\', "/").split('/').any(|s| s == "..")
        }
        _ => false,
    }
}

async fn execute_editor_git(req: &ApprovalRequest) -> anyhow::Result<String> {
    let action: EditorGitAction = serde_json::from_str(
        req.meta_json
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Git 审批缺少工作区信息"))?,
    )?;
    anyhow::ensure!(editor_git_args_valid(&action.args), "Git 审批参数无效");
    let root = crate::workspace::pinned(&action.root.to_string_lossy())?;
    anyhow::ensure!(
        action.root.join(".git").exists(),
        "审批来源工作区已不是 Git 仓库"
    );
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        tokio::process::Command::new("git")
            .arg("--literal-pathspecs")
            .args(&action.args)
            .current_dir(root)
            .env("GIT_TERMINAL_PROMPT", "0")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Git 操作超时"))??;
    anyhow::ensure!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(String::from_utf8_lossy(&out.stdout)
        .chars()
        .take(4000)
        .collect())
}

impl Core {
    /// Cancel pending approvals owned by a stopped session. The per-approval
    /// lock is taken before changing the row so a late click cannot race an
    /// approval that is already being executed.
    pub(crate) fn cancel_pending_approvals(&self, session_id: &str) -> usize {
        let pending = match self.store.list_approvals(Some("pending"), 200) {
            Ok(rows) => rows,
            Err(_) => return 0,
        };
        let mut cancelled = 0;
        for mut req in pending {
            if req.session_id != session_id {
                continue;
            }
            let lock = {
                let mut locks = self.approval_locks.lock();
                locks
                    .entry(req.id.clone())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                    .clone()
            };
            let Ok(_guard) = lock.try_lock() else {
                continue;
            };
            let Ok(Some(latest)) = self.store.get_approval(&req.id) else {
                continue;
            };
            if latest.status != "pending" {
                continue;
            }
            req = latest;
            req.status = "cancelled".into();
            req.decided_at = Some(now_iso());
            req.result = Some("本轮已被用户停止，审批动作已取消；迟到批准不得执行".into());
            if self.store.update_approval(&req).is_ok() {
                self.emit_approval_resolved(&req);
                cancelled += 1;
            }
        }
        cancelled
    }

    pub fn approval_workspace(&self, req: &ApprovalRequest) -> anyhow::Result<Option<String>> {
        if req.agent_id == "desktop" && req.command.starts_with("[git]") {
            let meta: EditorGitAction =
                serde_json::from_str(req.meta_json.as_deref().unwrap_or(""))?;
            return Ok(Some(meta.root.to_string_lossy().into_owned()));
        }
        if let Some(workspace) = req
            .meta_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .and_then(|meta| {
                meta.get("projectWorkspace")
                    .and_then(|path| path.as_str())
                    .map(str::to_owned)
            })
        {
            return Ok(Some(workspace));
        }
        if req.session_id.is_empty() {
            return Ok(None);
        }
        Ok(self
            .store
            .get_session(&req.session_id)?
            .and_then(|s| s.workspace))
    }

    fn approved_command_root(&self, req: &ApprovalRequest) -> anyhow::Result<std::path::PathBuf> {
        let root = req
            .meta_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .and_then(|meta| {
                meta.get("projectWorkspace")
                    .or_else(|| meta.get("root"))?
                    .as_str()
                    .map(str::to_string)
            });
        if let Some(root) = root {
            return crate::workspace::pinned(&root);
        }
        let fallback = self.orchestrator().tools.effective_root();
        self.approval_workspace(req)?
            .as_deref()
            .map(crate::workspace::pinned)
            .transpose()
            .map(|root| root.unwrap_or(fallback))
    }
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
        self.approval_decide_scoped(id, ApprovalScope::Once { approve })
            .await
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
            let terminal_command = req
                .meta_json
                .as_deref()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                .and_then(|meta| {
                    meta.get("terminalAction")
                        .and_then(|action| action.get("command"))
                        .and_then(|command| command.as_str())
                        .map(str::to_owned)
                });
            let command_for_policy = terminal_command.as_deref().unwrap_or(req.command.as_str());
            let special = req
                .command
                .strip_prefix("[terminal] ")
                .map(|label| label.starts_with('['))
                .unwrap_or_else(|| req.command.starts_with('['));
            let destructive =
                crate::tools::is_destructive_command(&command_for_policy.to_lowercase());
            if matches!(decision, ApprovalScope::Session | ApprovalScope::Allowlist)
                && (special || destructive)
            {
                ApprovalScope::Once { approve: true }
            } else {
                decision
            }
        } else {
            ApprovalScope::Once { approve: false }
        };
        let approve = effective.is_approve();
        // Computer Use 输入审批必须在普通终端审批分支之前收口。旧版若只有
        // `[computer]` 文本而没有结构化动作，宁可失败也不能把描述交给 shell。
        let has_computer_action = req.command.starts_with("[computer]")
            || req.meta_json.as_deref().is_some_and(|raw| {
                serde_json::from_str::<serde_json::Value>(raw)
                    .ok()
                    .and_then(|value| value.get("computerAction").cloned())
                    .is_some()
            });
        let computer_action = match crate::tools::computer_action_from_approval(&req) {
            Ok(action) => action,
            Err(error) if has_computer_action => {
                req.decided_at = Some(now_iso());
                req.status = "failed".into();
                req.result = Some(format!("Computer Use 审批参数无效，未执行：{error}"));
                self.store.update_approval(&req)?;
                self.emit_approval_resolved(&req);
                return Ok(req);
            }
            Err(_) => None,
        };
        if req.command.starts_with("[computer]") && computer_action.is_none() {
            req.decided_at = Some(now_iso());
            req.status = "failed".into();
            req.result = Some("Computer Use 旧审批缺少类型化动作，未执行".into());
            self.store.update_approval(&req)?;
            self.emit_approval_resolved(&req);
            return Ok(req);
        }
        if let Some(action) = computer_action {
            req.decided_at = Some(now_iso());
            if !approve {
                req.status = "denied".into();
                req.result = Some("Computer Use 动作被拒绝（未执行）".into());
            } else if req.command != format!("[computer] {}", action.description) {
                req.status = "failed".into();
                req.result = Some("Computer Use 审批描述与结构化动作不一致，未执行".into());
            } else {
                match self
                    .orchestrator()
                    .tools
                    .execute_computer_approval(&action)
                    .await
                {
                    Ok(result) => {
                        req.status = "executed".into();
                        req.result = Some(result);
                    }
                    Err(error) => {
                        req.status = "failed".into();
                        req.result = Some(format!("Computer Use 审批未执行：{error}"));
                    }
                }
            }
            self.store.update_approval(&req)?;
            self.emit_approval_resolved(&req);
            return Ok(req);
        }
        // 新终端审批单保留结构化调用语义。必须在所有旧的字符串分支前收口，
        // 否则后台请求会被错误地当成前台 shell 回放，或因元数据损坏而降级执行。
        let has_terminal_action = req.command.starts_with("[terminal] ")
            || req.meta_json.as_deref().is_some_and(|raw| {
                serde_json::from_str::<serde_json::Value>(raw)
                    .ok()
                    .is_some_and(|value| value.get("terminalAction").is_some())
            });
        let terminal_action = match crate::tools::terminal_action_from_approval(&req) {
            Ok(action) => action,
            Err(error) if has_terminal_action => {
                req.decided_at = Some(now_iso());
                req.status = "failed".into();
                req.result = Some(format!("终端审批参数无效，未执行：{error}"));
                self.store.update_approval(&req)?;
                self.emit_approval_resolved(&req);
                return Ok(req);
            }
            // 旧审批可能没有结构化元数据；保留其原有普通前台回放语义。
            Err(_) => None,
        };
        if req.command.starts_with("[terminal] ") && terminal_action.is_none() {
            req.decided_at = Some(now_iso());
            req.status = "failed".into();
            req.result = Some("终端审批缺少有效的类型化动作，未执行".into());
            self.store.update_approval(&req)?;
            self.emit_approval_resolved(&req);
            return Ok(req);
        }
        if let Some(action) = terminal_action {
            req.decided_at = Some(now_iso());
            if !approve {
                req.status = "denied".into();
                req.result = Some("终端命令被拒绝（未执行）".into());
            } else if req.command != format!("[terminal] {}", action.approval_command) {
                req.status = "failed".into();
                req.result = Some("终端审批卡与结构化参数不一致，未执行".into());
            } else if req
                .meta_json
                .as_deref()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                .and_then(|meta| {
                    meta.get("projectWorkspace")
                        .and_then(|path| path.as_str())
                        .map(str::to_owned)
                })
                .as_deref()
                != Some(action.workspace.as_str())
            {
                req.status = "failed".into();
                req.result = Some("终端审批项目元数据不一致，未执行".into());
            } else {
                match self
                    .orchestrator()
                    .tools
                    .execute_terminal_approval(&req.session_id, &action)
                    .await
                {
                    Ok(out) if out.ok => {
                        req.status = "executed".into();
                        req.result = Some(out.output.chars().take(4000).collect());
                    }
                    Ok(out) => {
                        req.status = "failed".into();
                        req.result = Some(
                            out.error
                                .or_else(|| (!out.output.is_empty()).then_some(out.output))
                                .unwrap_or_else(|| "终端审批代执行失败".into())
                                .chars()
                                .take(4000)
                                .collect(),
                        );
                    }
                    Err(error) => {
                        req.status = "failed".into();
                        req.result = Some(
                            format!("终端审批未执行：{error}")
                                .chars()
                                .take(4000)
                                .collect(),
                        );
                    }
                }
            }
            if req.status == "executed" {
                let prefix = crate::tools::command_head(&action.command);
                match effective {
                    ApprovalScope::Session if !prefix.is_empty() => {
                        self.grant_session(&req.session_id, &prefix);
                        if let Some(result) = req.result.as_mut() {
                            result.push_str(&format!(
                                "\n[会话授权] 同前缀命令「{prefix}」本会话内自动放行"
                            ));
                        }
                    }
                    ApprovalScope::Allowlist if !prefix.is_empty() => {
                        let mut config = self.config().as_ref().clone();
                        if !config
                            .security
                            .exec_allowlist
                            .iter()
                            .any(|old| old == &prefix)
                        {
                            config.security.exec_allowlist.push(prefix.clone());
                            match self.apply_config(config) {
                                Ok(()) => {
                                    if let Some(result) = req.result.as_mut() {
                                        result.push_str(&format!(
                                            "\n[白名单] 前缀「{prefix}」已写入全局白名单"
                                        ));
                                    }
                                }
                                Err(error) => {
                                    if let Some(result) = req.result.as_mut() {
                                        result.push_str(&format!(
                                            "\n[白名单] 写入失败，执行已完成但未授权后续命令：{error}"
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            self.store.update_approval(&req)?;
            self.emit_approval_resolved(&req);
            return Ok(req);
        }
        if req.command.starts_with("[git] ") {
            req.decided_at = Some(now_iso());
            if !approve {
                req.status = "denied".into();
            } else {
                let result = execute_editor_git(&req).await;
                req.status = if result.is_ok() { "executed" } else { "failed" }.into();
                req.result = Some(match result {
                    Ok(text) => text,
                    Err(e) => e.to_string(),
                });
                self.store.audit_tool_best_effort(
                    "desktop",
                    "git",
                    &serde_json::json!({
                        "approvalId": req.id, "command": req.command,
                    }),
                    req.result.as_deref().unwrap_or(""),
                    0,
                );
            }
            self.store.update_approval(&req)?;
            self.emit_approval_resolved(&req);
            return Ok(req);
        }
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
            // 提议载荷明细（kind/target）随应用结果事件透出——与
            // meta.proposal(created) 对称，事件页可串起提议→应用轨迹
            let (p_kind, p_target) = req
                .meta_json
                .as_deref()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                .map(|v| {
                    (
                        v.get("kind")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                        v.get("target")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string(),
                    )
                })
                .unwrap_or_default();
            let _ = self.events.send(CoreEvent {
                kind: "meta.applied".into(),
                session_id: req.session_id.clone(),
                payload: serde_json::json!({
                    "approvalId": req.id,
                    "ok": req.status == "executed",
                    "command": req.command,
                    "kind": p_kind,
                    "target": p_target,
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
            let workspace = self.approved_command_root(&req)?;
            // root 代执行：ShellOpts::plain（不降权——审批的就是提权本身）
            let opts = crate::tools::ShellOpts::plain();
            let out = crate::tools::execute_shell_for_session(
                &workspace,
                &eff_cmd,
                300,
                &opts,
                &req.session_id,
            )
            .await;
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
            // 红线降级注明（approval-ergonomics）：[root] 提权单不允许升档,
            // 越权档位自动降级 once——审批结果注明让裁决者知情
            let downgrade_note =
                if matches!(decision, ApprovalScope::Session | ApprovalScope::Allowlist) {
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
        let workspace = self.approved_command_root(&req)?;
        let opts = self.orchestrator().tools.shell_opts(&eff_cmd);
        let out = crate::tools::execute_shell_for_session(
            &workspace,
            &eff_cmd,
            300,
            &opts,
            &req.session_id,
        )
        .await;
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

    /// The exact project and argv survive a workspace switch while awaiting approval.
    pub async fn request_editor_git_approval(
        &self,
        root: &std::path::Path,
        args: Vec<String>,
    ) -> anyhow::Result<ApprovalRequest> {
        anyhow::ensure!(editor_git_args_valid(&args), "不支持的 Git 审批动作");
        let root = root.canonicalize()?;
        let command = format!(
            "[git] {} · git {}",
            root.display(),
            args.iter()
                .map(|a| format!("{a:?}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let req = ApprovalRequest {
            id: crate::types::new_id(),
            session_id: String::new(),
            node_id: None,
            agent_id: "desktop".into(),
            command,
            status: "pending".into(),
            result: None,
            created_at: now_iso(),
            decided_at: None,
            origin_user: None,
            origin_role: None,
            meta_json: Some(serde_json::to_string(&EditorGitAction { root, args })?),
        };
        self.store.add_approval(&req)?;
        let _ = self.events.send(CoreEvent { kind: "approval.required".into(), session_id: String::new(),
            payload: serde_json::json!({ "approvalId": req.id, "agentId": req.agent_id, "command": req.command }) });
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
