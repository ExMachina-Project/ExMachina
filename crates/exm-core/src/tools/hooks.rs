//! tools/hooks.rs —— 自 tools.rs 按领域拆出（纯搬移，零行为变更）

use super::*;

/// 执行一条钩子命令（同步、30s 上界）；返回 (是否成功, 输出摘要)。
/// 钩子在工作区内执行，环境变量注入上下文（工具名/参数/agent/工作区）。
pub fn run_hook_command(hook: &str, cwd: &Path, envs: &[(&str, &str)]) -> (bool, String) {
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", hook]);
        c
    };
    #[cfg(not(target_os = "windows"))]
    let mut cmd = {
        let mut c = std::process::Command::new("sh");
        c.args(["-c", hook]);
        c
    };
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.current_dir(cwd).stdin(std::process::Stdio::null());
    let out = cmd.output();
    match out {
        Ok(o) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            let trimmed: String = text.trim().chars().take(600).collect();
            (o.status.success(), trimmed)
        }
        Err(e) => (false, format!("钩子执行失败: {e}")),
    }
}
