//! tools/git.rs —— 自 tools.rs 按领域拆出（纯搬移，零行为变更）

use super::*;

impl ToolGateway {
    /// Git 多合一工具（spec: git-tools）。
    /// 只读动作直接执行（无副作用）；写动作走 terminal 同一道审批闸门
    /// （off 档拒绝、risky/always 档产审批单系统代执行）——命令文本以
    /// `git <action> ...` 呈现，天然不在白名单前缀内即进审批。
    /// push/pull 等远程操作不在动作面（防越出工作区边界）。
    /// 写动作执行前对 pathSpec 涉及文件记检查点（来源 git，可回滚）。
    pub(crate) async fn tool_git(
        &self,
        agent_id: &str,
        session_id: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let root = self.effective_root();
        // 工作区仓库检测：.git 目录或文件（worktree）存在才可用
        if !root.join(".git").exists() {
            return ToolResult::err(format!(
                "当前工作区不是 git 仓库（{} 下无 .git）；git 工具仅作用于工作区仓库。",
                root.display()
            ));
        }
        let s = |k: &str| {
            args.get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string()
        };
        // 动作 → (是否写动作, 参数校验, 命令构造)
        let is_write = matches!(
            action,
            "add" | "commit" | "restore" | "stash-push" | "stash-pop" | "checkout-b"
        );
        let cmd = match action {
            "status" => "git status --short --branch".to_string(),
            "diff" => {
                let path = s("pathSpec");
                if contains_shell_chain(&path) || path.starts_with('-') {
                    return ToolResult::err("pathSpec 含非法字符（shell 连接符 / 选项前缀）");
                }
                if path.is_empty() {
                    "git diff --stat".to_string()
                } else {
                    format!("git diff -- {path}")
                }
            }
            "log" => {
                let n = args
                    .get("maxCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(20)
                    .clamp(1, 200);
                format!("git log --oneline -n {n}")
            }
            "branch" => "git branch -a".to_string(),
            "show" => {
                let obj = s("object");
                if obj.is_empty() || obj.starts_with('-') || contains_shell_chain(&obj) {
                    return ToolResult::err("show 需要合法对象引用（非空、非选项前缀、无连接符）");
                }
                format!("git show --stat {obj}")
            }
            "stash-list" => "git stash list".to_string(),
            "add" => {
                let path = s("pathSpec");
                if path.is_empty() || path.starts_with('-') || contains_shell_chain(&path) {
                    return ToolResult::err("add 需要合法 pathSpec（非空、非选项前缀、无连接符）");
                }
                format!("git add -- {path}")
            }
            "commit" => {
                let msg = s("message");
                if msg.is_empty() {
                    return ToolResult::err("commit 需要 message");
                }
                // 单引号转义防注入（message 内容含引号即拒绝，简化白名单口径）
                if msg.contains(['\'', '"', '\\', '`', '$']) {
                    return ToolResult::err(
                        "message 含引号/反引号/美元符号/反斜杠，为防注入请调整提交信息",
                    );
                }
                format!("git commit -m '{msg}'")
            }
            "restore" => {
                let path = s("pathSpec");
                if path.is_empty() || path.starts_with('-') || contains_shell_chain(&path) {
                    return ToolResult::err(
                        "restore 需要合法 pathSpec（非空、非选项前缀、无连接符）",
                    );
                }
                format!("git restore -- {path}")
            }
            "stash-push" => {
                let msg = s("message");
                if msg.contains(['\'', '"', '\\', '`', '$']) {
                    return ToolResult::err(
                        "message 含引号/反引号/美元符号/反斜杠，为防注入请调整说明",
                    );
                }
                if msg.is_empty() {
                    "git stash push".to_string()
                } else {
                    format!("git stash push -m '{msg}'")
                }
            }
            "stash-pop" => "git stash pop".to_string(),
            "checkout-b" => {
                let b = s("branch");
                if b.is_empty() || b.starts_with('-') || contains_shell_chain(&b) {
                    return ToolResult::err(
                        "checkout-b 需要合法分支名（非空、非选项前缀、无连接符）",
                    );
                }
                format!("git checkout -b {b}")
            }
            "push" | "pull" | "fetch" | "clone" | "remote" | "reset" | "rebase" | "merge"
            | "cherry-pick" | "revert" => {
                return ToolResult::err(format!(
                    "动作 {action} 不在 git 工具动作面（远程/重写类操作请用 terminal 工具走完整审批链）"
                ));
            }
            _ => {
                return ToolResult::err(format!(
                    "未知 action：{action}（支持 status/diff/log/branch/show/stash-list/add/commit/restore/stash-push/stash-pop/checkout-b）"
                ));
            }
        };
        // 只读/写动作统一经审批闸门后的执行：直接调 git 二进制（参数数组化，
        // 不经 shell——无注入面；不受 bwrap --tmpfs /tmp 影响——沙箱内 /tmp 被清空
        // 会让工作区在 /tmp 下的 git 仓库不可见）
        let run_git = |argv: &[String]| -> ToolResult {
            let out = std::process::Command::new("git")
                .args(argv)
                .current_dir(&root)
                .output();
            match out {
                Ok(o) if o.status.success() => {
                    let mut text = String::from_utf8_lossy(&o.stdout).to_string();
                    let err_tail = String::from_utf8_lossy(&o.stderr);
                    if !err_tail.trim().is_empty() {
                        text.push_str("\n[stderr] ");
                        text.push_str(err_tail.trim_end());
                    }
                    let text = text.trim().to_string();
                    if text.is_empty() {
                        ToolResult::ok("（无输出）")
                    } else {
                        ToolResult::ok(text)
                    }
                }
                Ok(o) => {
                    let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
                    ToolResult::err(format!(
                        "git {} 失败（退出码 {}）：{}",
                        action,
                        o.status.code().unwrap_or(-1),
                        if err.is_empty() {
                            String::from_utf8_lossy(&o.stdout).trim().to_string()
                        } else {
                            err
                        }
                    ))
                }
                Err(e) => ToolResult::err(format!("系统 git 不可用：{e}")),
            }
        };
        // 写动作：off 档直接拒绝；risky/always 档进审批（gate_command 原地等待批准后代执行）
        if is_write {
            let mode = self.security.exec_approval.as_str();
            if mode == "off" {
                return ToolResult::err(format!(
                    "git 写动作（{action}）需审批放行：security.execApproval 当前为 off，\
                     请改为 risky/always 后重试（或经用户在终端自行执行）"
                ));
            }
            // 检查点：pathSpec 涉及文件动作前快照（可回滚）；commit/stash 全仓性质跳过逐文件快照
            if matches!(action, "add" | "restore") {
                for rel in s("pathSpec").lines() {
                    let rel = rel.trim();
                    if rel.is_empty() {
                        continue;
                    }
                    let target = root.join(rel);
                    if target.is_file() {
                        if let Err(e) = self.checkpoint(&target, session_id) {
                            return ToolResult::err(format!("git 动作前检查点失败：{e}"));
                        }
                    }
                }
            }
            return self.gate_command(agent_id, session_id, &cmd).await;
        }
        // 只读动作：直接执行（argv 数组化；动态参数拼装后统一校验）
        match Self::git_readonly_argv(action, args) {
            Ok(a) => run_git(&a),
            Err(e) => ToolResult::err(e),
        }
    }

    /// git 只读动作 → argv 数组（与 tool_git 内的命令文本构造一一对应；
    /// 数组化不经 shell，天然无注入面）。动态参数校验后混入。
    pub(crate) fn git_readonly_argv(action: &str, args: &serde_json::Value) -> Result<Vec<String>, String> {
        let s = |k: &str| {
            args.get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let mut argv: Vec<String> = match action {
            "status" => vec!["status", "--short", "--branch"],
            "diff" => vec!["diff", "--stat"],
            "log" => vec!["log", "--oneline"],
            "branch" => vec!["branch", "-a"],
            "stash-list" => vec!["stash", "list"],
            "show" => vec!["show", "--stat"],
            _ => return Err(format!("未知只读 action：{action}")),
        }
        .into_iter()
        .map(String::from)
        .collect();
        match action {
            "log" => {
                let n = args
                    .get("maxCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(20)
                    .clamp(1, 200);
                argv.push(format!("-n{n}"));
            }
            "show" => {
                let obj = s("object");
                if obj.is_empty() || obj.starts_with('-') {
                    return Err("show 需要合法对象引用（非空、非选项前缀）".into());
                }
                argv.push(obj);
            }
            "diff" => {
                let path = s("pathSpec");
                if !path.is_empty() {
                    if path.starts_with('-') {
                        return Err("pathSpec 不能是选项前缀".into());
                    }
                    argv.push("--".into());
                    argv.push(path);
                }
            }
            _ => {}
        }
        Ok(argv)
    }
}
