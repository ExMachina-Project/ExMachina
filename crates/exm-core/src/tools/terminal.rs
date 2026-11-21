//! 终端工具与有会话、项目归属的后台受管作业。

use super::*;

/// 后台 spawn 子任务终态（spec: subagent-spawn 异步并行）
#[derive(Debug, Clone)]
pub enum SpawnBgState {
    Running,
    Done(String),
    Failed(String),
    Timeout,
}

/// 后台 spawn 注册条目：终态暂存 + 已读标记（未回收提醒注入用）
pub struct SpawnBgTask {
    pub spawn_id: String,
    pub task_preview: String,
    pub session: String,
    pub started: Instant,
    pub state: parking_lot::RwLock<SpawnBgState>,
    /// 终态是否已被 status/list 读取（未读的终态进系统提示提醒）
    pub read: parking_lot::RwLock<bool>,
}
impl ToolGateway {
    /// 工作台树遍历辅助（自由函数，见下方 walk_tree）
    pub(crate) async fn tool_terminal(
        &self,
        agent_id: &str,
        session_id: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("run");
        // 后台任务轮询 / 终止 / 清单（不经过白名单与审批：对象是已批准过的任务）
        match op {
            "poll" => {
                let id = args.get("taskId").and_then(|v| v.as_str()).unwrap_or("");
                return match super::terminal_output::PageRequest::parse(args) {
                    Ok(None) => self.poll_background(session_id, id),
                    Ok(Some(page)) => self.poll_background_page(session_id, id, page).await,
                    Err(error) => ToolResult::err(error.to_string()),
                };
            }
            "kill" => {
                return self
                    .kill_background(
                        session_id,
                        args.get("taskId").and_then(|v| v.as_str()).unwrap_or(""),
                    )
                    .await
            }
            "list" => return self.list_background(session_id),
            _ => {}
        }
        let cmd = args
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if cmd.is_empty() {
            return ToolResult::err("command 不能为空");
        }
        let background = args
            .get("background")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let timeout_secs = args.get("timeoutSecs").and_then(|value| value.as_u64());
        let interactive = match args.get("interactive") {
            None => false,
            Some(value) => match value.as_bool() {
                Some(value) => value,
                None => return ToolResult::err("interactive 必须是布尔值"),
            },
        };
        if interactive {
            return ToolResult::err("当前终端 runner 不支持交互式终端");
        }
        let approval_request = |approval_command: String, elevated: bool| {
            super::terminal_jobs::TerminalApprovalRequest {
                command: cmd.clone(),
                approval_command,
                background,
                timeout_secs,
                interactive,
                elevated,
                exec_user: self.security.exec_user.clone(),
                exec_home: self.security.exec_home.clone(),
            }
        };
        // 逃逸/写入语义（` $( > < &）仍一律拒绝：命令替换绕过解析、重定向写文件、
        // 后台执行，白名单管不住。链式分隔（| && || ;）改走逐段白名单
        // （chain_allowed：每段首命令都过白名单才放行，攻击面与单命令一致）。
        if contains_shell_chain(&cmd) {
            return ToolResult::err(
                "命令包含命令替换 / 重定向 / 后台符（` $( > < &），白名单不放行；请去掉后重试或拆成多次调用。",
            );
        }
        let lowered = cmd.to_lowercase();
        let whitelisted = chain_allowed(&cmd);
        // root 提权闸门（agent-exec-confinement）：提权命令无视白名单直达审批——
        // 白名单匹配的是命令内容（apt/git/...），提权前缀（sudo/su/...）本身越权。
        // execUser 约束开启时才拦截（关 = 旧行为，sudo 按普通命令走既有闸门）。
        if !self.security.exec_user.trim().is_empty()
            && super::allowlist::is_elevation_command(&cmd)
        {
            return self
                .gate_terminal(
                    agent_id,
                    session_id,
                    approval_request(format!("[root] {cmd}"), true),
                )
                .await;
        }
        // 审批闸门（docs/10）：risky 拦高危命令，always 全拦；白名单前缀放行。
        // 拦截后运行时原地等待人工决定（security.approvalWaitSecs）：
        // 批准 → 系统代执行，输出直接回灌本工具结果，模型继续干活；拒绝/超时 → 受阻回流。
        let mode = self.security.exec_approval.as_str();
        // 命令分档闸门（agent-budget-audit 3.3）：tier_gate 联动判定
        // （capability × 审批档 × 命令分档）。Denied 档（rm/format/dd…）无论
        // 白名单命中与否、审批档位为何，一律走审批——off 档对白名单受控命令宽松，
        // 对破坏性命令永远不放行。能力边界（仅聊天档）：本工具在 Chat 档不可达
        // （runtime 执行层全拒），tier_gate 的边界文案兜底防御纵深。
        let scope = self.session_scope_of(session_id);
        let tier = super::allowlist::tier_of_command(&cmd);
        let (tier_pass, tier_deny_msg) = super::allowlist::tier_gate(&cmd, scope, mode);
        if let Some(deny_msg) = tier_deny_msg.filter(|_| !tier_pass) {
            return ToolResult::err(deny_msg);
        }
        if !tier_pass {
            return self
                .gate_terminal(
                    agent_id,
                    session_id,
                    approval_request(format!("[红线] {cmd}"), false),
                )
                .await;
        }
        if !whitelisted {
            // 白名单外命令（审批经济学）：**无害即直通**——非 Denied 档且无
            // 高危特征（is_destructive_command）时不进审批。此前 risky/always
            // 档下白名单外一律进审批，「跑个 mkdir/go/pytest 都要人工介入」。
            // off 档保持白名单即全部边界的历史语义（无害也不例外——off 的
            // 语义就是零信任白名单模式）。
            let harmless =
                tier != super::allowlist::CommandTier::Denied && !is_destructive_command(&lowered);
            if mode == "off" {
                return ToolResult::err(format!(
                    "命令不在白名单: {cmd}（白名单按「命令 + 子命令」逐段匹配——链式命令每段都须在白名单；\
                     可将 security.execApproval 设为 risky/always 走人工审批放行）"
                ));
            }
            if !harmless {
                return self
                    .gate_terminal(agent_id, session_id, approval_request(cmd.clone(), false))
                    .await;
            }
        }
        if mode == "risky" || mode == "always" {
            let allowlisted = self.security.exec_allowlist.iter().any(|p| {
                !p.trim().is_empty() && lowered.starts_with(p.trim().to_lowercase().as_str())
            });
            // 高危判定不再叠加审批档：risky 拦高危（既有语义），always 档
            // 高危也拦但**白名单外无害命令已放行**（上一分支）——「全拦」的
            // 旧语义降级为「高危必拦」，审批只花在真危险的东西上。
            let gated = is_destructive_command(&lowered);
            if gated && !allowlisted {
                return self
                    .gate_terminal(agent_id, session_id, approval_request(cmd.clone(), false))
                    .await;
            }
        }
        // 沙箱 strict：联网类命令（安装/下载/克隆）默认需审批——把「外联」与「本地干活」分开
        if self.sandbox.strict() && !self.sandbox.allow_network && is_network_command(&lowered) {
            return self
                .gate_terminal(agent_id, session_id, approval_request(cmd.clone(), false))
                .await;
        }
        let workspace = match self.workspace_for_session(session_id).canonicalize() {
            Ok(workspace) => workspace,
            Err(error) => return ToolResult::err(format!("项目工作目录不可用: {error}")),
        };
        self.execute_terminal_request(&approval_request(cmd.clone(), false), session_id, workspace)
            .await
    }

    /// 后台 spawn 单查（spec: subagent-spawn）：终态全文返回并置已读（提醒不再注入）
    pub(crate) fn spawn_bg_status(&self, session_id: &str, spawn_id: &str) -> ToolResult {
        if spawn_id.is_empty() {
            return ToolResult::err("op=status 需要 spawnId");
        }
        let guard = self.spawns_bg.lock();
        let Some(t) = guard.get(spawn_id) else {
            return ToolResult::err(format!(
                "后台子任务不存在: {spawn_id}（是否同步派生？同步结果直接在原调用返回）"
            ));
        };
        if t.session != session_id {
            return ToolResult::err(format!("后台子任务不属于当前会话: {spawn_id}"));
        }
        let elapsed = t.started.elapsed().as_secs();
        let out = match &*t.state.read() {
            SpawnBgState::Running => format!(
                "子任务 {spawn_id} 运行中（已 {elapsed}s）\n任务：{}\n请稍后再查。",
                t.task_preview
            ),
            SpawnBgState::Done(text) => {
                *t.read.write() = true;
                format!("子任务 {spawn_id} 已完成（耗时 {elapsed}s）：\n{text}")
            }
            SpawnBgState::Failed(msg) => {
                *t.read.write() = true;
                format!("子任务 {spawn_id} 失败（耗时 {elapsed}s）：{msg}")
            }
            SpawnBgState::Timeout => {
                *t.read.write() = true;
                format!("子任务 {spawn_id} 超时被终止（{elapsed}s）")
            }
        };
        drop(guard);
        ToolResult::ok(out)
    }

    /// 后台 spawn 清单（spec: subagent-spawn）：本会话全部子任务，最新在前；
    /// 终态条目顺带置已读（list 全量浏览即视为收讫）
    pub(crate) fn spawn_bg_list(&self, session_id: &str) -> ToolResult {
        let mut guard = self.spawns_bg.lock();
        let mut rows: Vec<&SpawnBgTask> =
            guard.values().filter(|t| t.session == session_id).collect();
        if rows.is_empty() {
            return ToolResult::ok("当前会话无后台子任务。");
        }
        rows.sort_by_key(|t| std::cmp::Reverse(t.started));
        let mut out = format!("后台子任务（{} 个，最新在前）：\n", rows.len());
        for (i, t) in rows.iter().enumerate() {
            let elapsed = t.started.elapsed().as_secs();
            let status = match &*t.state.read() {
                SpawnBgState::Running => format!("运行中（已 {elapsed}s）"),
                SpawnBgState::Done(_) => {
                    *t.read.write() = true;
                    format!("已完成（耗时 {elapsed}s）——用 op=status 取结果")
                }
                SpawnBgState::Failed(_) => {
                    *t.read.write() = true;
                    format!("失败（耗时 {elapsed}s）——用 op=status 取详情")
                }
                SpawnBgState::Timeout => {
                    *t.read.write() = true;
                    format!("超时终止（{elapsed}s）")
                }
            };
            out.push_str(&format!(
                "{}. [spawnId={}] {} [{}]\n",
                i + 1,
                t.spawn_id,
                t.task_preview,
                status
            ));
        }
        // 容量上界：每会话最近 50 条，超出丢最旧（防注册表无限膨胀）
        let mut by_session: Vec<(String, Instant)> = guard
            .values()
            .filter(|t| t.session == session_id)
            .map(|t| (t.spawn_id.clone(), t.started))
            .collect();
        if by_session.len() > 50 {
            by_session.sort_by_key(|(_, s)| *s);
            for (id, _) in by_session.drain(..by_session.len() - 50) {
                guard.remove(&id);
            }
        }
        drop(guard);
        ToolResult::ok(out)
    }

    /// 未回收提醒（spec: subagent-spawn）：本会话已终结未被 status/list 读取的
    /// 后台子任务终态摘要，注入下一轮系统提示（限 5 条，最新在前）
    pub fn spawn_pending_note(&self, session_id: &str) -> Option<String> {
        let guard = self.spawns_bg.lock();
        // 拥有式收集：preview 转成 String，避免注册表锁内借用外泄
        let mut unread: Vec<(std::time::Instant, String, String, usize)> = Vec::new();
        for t in guard.values() {
            if t.session != session_id || *t.read.read() {
                continue;
            }
            let elapsed = t.started.elapsed().as_secs() as usize;
            match &*t.state.read() {
                SpawnBgState::Running => continue,
                SpawnBgState::Done(text) => {
                    let preview: String = text.chars().take(200).collect();
                    unread.push((t.started, t.spawn_id.clone(), preview, elapsed));
                }
                SpawnBgState::Failed(msg) => {
                    let preview: String = msg.chars().take(200).collect();
                    unread.push((t.started, t.spawn_id.clone(), preview, elapsed));
                }
                SpawnBgState::Timeout => {
                    unread.push((t.started, t.spawn_id.clone(), "超时被终止".into(), elapsed));
                }
            }
        }
        drop(guard);
        if unread.is_empty() {
            return None;
        }
        unread.sort_by_key(|(s, _, _, _)| std::cmp::Reverse(*s));
        let mut out = String::from("## 后台子任务结果待收\n");
        for (_, id, preview, elapsed) in unread.iter().take(5) {
            out.push_str(&format!("- [{id}]（{elapsed}s）：{preview}\n"));
        }
        if unread.len() > 5 {
            out.push_str(&format!("- …（另有 {} 个未列出）\n", unread.len() - 5));
        }
        out.push_str("用 spawn(op=\"status\", spawnId=...) 收取全文。");
        Some(out)
    }
}
