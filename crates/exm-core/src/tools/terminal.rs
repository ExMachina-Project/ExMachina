//! tools/terminal.rs —— 自 tools.rs 按领域拆出（纯搬移，零行为变更）

use super::*;

/// 后台任务（长命令）：输出累积缓冲 + 终态码 + 中止句柄
pub struct BgTask {
    pub cmd: String,
    /// 启动任务时会话（op=list 按会话过滤，跨会话不互见）
    pub session: String,
    pub started: Instant,
    pub buf: Arc<parking_lot::Mutex<String>>,
    /// None = 运行中；Some(code) = 已退出
    pub state: Arc<parking_lot::Mutex<Option<i32>>>,
    pub abort: tokio::task::AbortHandle,
}

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
            "poll" => return self.bg_poll(args),
            "kill" => return self.bg_kill(args),
            "list" => return self.bg_list(session_id),
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
        // 审批闸门（docs/10）：risky 拦高危命令，always 全拦；白名单前缀放行。
        // 拦截后运行时原地等待人工决定（security.approvalWaitSecs）：
        // 批准 → 系统代执行，输出直接回灌本工具结果，模型继续干活；拒绝/超时 → 受阻回流。
        let mode = self.security.exec_approval.as_str();
        if !whitelisted {
            // 白名单外命令：off 档直接拒绝（历史行为）；开启审批闸门时不再硬拒——
            // 转入人工审批（用户批准即由系统代执行）。此前一律硬拒导致 mkdir/go/pytest
            // 等大量日常开发命令无路可走，agent 无法完成编码任务。
            if mode == "off" {
                return ToolResult::err(format!(
                    "命令不在白名单: {cmd}（白名单按「命令 + 子命令」逐段匹配——链式命令每段都须在白名单；\
                     可将 security.execApproval 设为 risky/always 走人工审批放行）"
                ));
            }
            return self.gate_command(agent_id, session_id, &cmd).await;
        }
        if mode == "risky" || mode == "always" {
            let allowlisted = self.security.exec_allowlist.iter().any(|p| {
                !p.trim().is_empty() && lowered.starts_with(p.trim().to_lowercase().as_str())
            });
            let gated = mode == "always" || is_destructive_command(&lowered);
            if gated && !allowlisted {
                return self.gate_command(agent_id, session_id, &cmd).await;
            }
        }
        // 沙箱 strict：联网类命令（安装/下载/克隆）默认需审批——把「外联」与「本地干活」分开
        if self.sandbox.strict() && !self.sandbox.allow_network && is_network_command(&lowered) {
            return self.gate_command(agent_id, session_id, &cmd).await;
        }
        // 后台模式：长任务（构建/测试/服务）不阻塞本轮，返回任务 id 供轮询
        if args
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return self.bg_start(&cmd, session_id);
        }
        let timeout = args
            .get("timeoutSecs")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.security.terminal_timeout_secs as u64)
            .clamp(5, self.security.terminal_timeout_secs.max(5) as u64);
        // strict 模式收紧前台命令时长：长活请显式用后台模式
        let timeout = if self.sandbox.strict() {
            timeout.min(60)
        } else {
            timeout
        };
        execute_shell_with(
            &self.effective_root(),
            &cmd,
            timeout,
            &self.shell_opts(&cmd),
        )
        .await
    }

    /// 启动后台任务：输出累积到共享缓冲，任务终结记录退出码
    pub(crate) fn bg_start(&self, cmd: &str, session_id: &str) -> ToolResult {
        const BG_OUTPUT_CAP: usize = 200_000;
        let id = format!("bg-{}", &crate::types::new_id()[..8]);
        let buf = Arc::new(parking_lot::Mutex::new(String::new()));
        let state = Arc::new(parking_lot::Mutex::new(None::<i32>));
        let cwd = self.effective_root();

        let opts = self.shell_opts(cmd);
        let mut process = tokio::process::Command::new(&opts.program);
        let effective: String = opts.cmd_override.clone().unwrap_or_else(|| cmd.to_string());
        if opts.args.is_empty() {
            #[cfg(target_os = "windows")]
            process.args(["/C", effective.as_str()]);
            #[cfg(not(target_os = "windows"))]
            process.args(["-c", effective.as_str()]);
        } else {
            process.args(&opts.args);
        }
        if opts.env_clear {
            process.env_clear();
        }
        for (k, v) in &opts.envs {
            process.env(k, v);
        }
        let spawned = process
            .current_dir(&cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => return ToolResult::err(format!("后台任务启动失败: {e}")),
        };
        // Windows：作业对象随任务存活；任务被 abort 时 Drop → 关句柄 → 整树终止
        let job = attach_job(&child, &opts);
        let buf2 = buf.clone();
        let state2 = state.clone();
        let handle = tokio::spawn(async move {
            let _job = job;
            let mut out = child.stdout.take();
            let mut err = child.stderr.take();
            let b3 = buf2.clone();
            let t_out = out.take().map(|mut s| {
                let b = b3.clone();
                tokio::spawn(async move {
                    let mut chunk = [0u8; 8192];
                    loop {
                        match tokio::io::AsyncReadExt::read(&mut s, &mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                let mut b = b.lock();
                                if b.len() < BG_OUTPUT_CAP {
                                    b.push_str(&String::from_utf8_lossy(&chunk[..n]));
                                }
                            }
                        }
                    }
                })
            });
            let b4 = buf2.clone();
            let t_err = err.take().map(|mut s| {
                let b = b4.clone();
                tokio::spawn(async move {
                    let mut chunk = [0u8; 4096];
                    loop {
                        match tokio::io::AsyncReadExt::read(&mut s, &mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                let mut b = b.lock();
                                if b.len() < BG_OUTPUT_CAP {
                                    b.push_str(&String::from_utf8_lossy(&chunk[..n]));
                                }
                            }
                        }
                    }
                })
            });
            let status = child.wait().await;
            if let Some(t) = t_out {
                let _ = t.await;
            }
            if let Some(t) = t_err {
                let _ = t.await;
            }
            *state2.lock() = Some(status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1));
        });
        let task = BgTask {
            cmd: cmd.to_string(),
            session: session_id.to_string(),
            started: Instant::now(),
            buf,
            state,
            abort: handle.abort_handle(),
        };
        self.background.lock().insert(id.clone(), task);
        ToolResult::ok(format!(
            "后台任务已启动：taskId={id}\n命令：{cmd}\n用 terminal(op=\"poll\", taskId=\"{id}\") 查看输出；op=\"kill\" 终止。"
        ))
    }

    /// 轮询后台任务：返回状态 + 输出尾部
    pub(crate) fn bg_poll(&self, args: &serde_json::Value) -> ToolResult {
        let id = args.get("taskId").and_then(|v| v.as_str()).unwrap_or("");
        let guard = self.background.lock();
        let Some(t) = guard.get(id) else {
            return ToolResult::err(format!("后台任务不存在: {id}"));
        };
        let state = *t.state.lock();
        let buf = t.buf.lock().clone();
        let elapsed = t.started.elapsed().as_secs();
        let tail: String = {
            let chars: Vec<char> = buf.chars().collect();
            let start = chars.len().saturating_sub(6000);
            chars[start..].iter().collect()
        };
        drop(guard);
        let status_line = match state {
            None => format!("运行中（已 {elapsed}s）"),
            Some(0) => format!("已完成（exit=0，耗时 {elapsed}s）"),
            Some(c) => format!("已结束（exit={c}，耗时 {elapsed}s）"),
        };
        ToolResult::ok(format!(
            "{status_line}\n—— 输出（累计 {} 字符）——\n{tail}",
            buf.chars().count()
        ))
    }

    /// 终止后台任务（中止读取任务 → kill_on_drop 连带结束子进程）
    pub(crate) fn bg_kill(&self, args: &serde_json::Value) -> ToolResult {
        let id = args.get("taskId").and_then(|v| v.as_str()).unwrap_or("");
        let mut guard = self.background.lock();
        match guard.remove(id) {
            Some(t) => {
                t.abort.abort();
                ToolResult::ok(format!("后台任务已终止：{id}（{cmd}）", cmd = t.cmd))
            }
            None => ToolResult::err(format!("后台任务不存在: {id}")),
        }
    }

    /// 后台任务清单（spec: agent-terminal-tools 后台任务可寻址）：
    /// 按会话过滤返回 id/命令/状态/输出尾摘要，跨轮次可寻址。
    /// 只读内部状态，不经过白名单与审批。
    pub(crate) fn bg_list(&self, session_id: &str) -> ToolResult {
        let guard = self.background.lock();
        let mut rows: Vec<(&String, &BgTask)> = guard
            .iter()
            .filter(|(_, t)| t.session == session_id)
            .collect();
        if rows.is_empty() {
            return ToolResult::ok("当前会话无后台任务。");
        }
        rows.sort_by_key(|(_, t)| std::cmp::Reverse(t.started));
        let mut out = format!("后台任务（{} 个，最新在前）：\n", rows.len());
        for (i, (id, t)) in rows.iter().enumerate() {
            let state = *t.state.lock();
            let elapsed = t.started.elapsed().as_secs();
            let status = match state {
                None => format!("运行中（已 {elapsed}s）"),
                Some(0) => format!("已完成 exit=0（耗时 {elapsed}s）"),
                Some(c) => format!("已结束 exit={c}（耗时 {elapsed}s）"),
            };
            let tail: String = {
                let buf = t.buf.lock();
                let chars: Vec<char> = buf.chars().collect();
                let start = chars.len().saturating_sub(200);
                chars[start..]
                    .iter()
                    .collect::<String>()
                    .replace('\n', " ⏎ ")
            };
            let tail_disp = if tail.trim().is_empty() {
                "（暂无）".to_string()
            } else {
                tail
            };
            // taskId 必须在清单里——丢失 id 的模型靠它重新寻址 poll/kill
            out.push_str(&format!(
                "{}. [taskId={id}] {} [{}]\n   输出尾：{}\n",
                i + 1,
                t.cmd,
                status,
                tail_disp
            ));
        }
        drop(guard);
        ToolResult::ok(out)
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
