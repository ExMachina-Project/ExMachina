//! tools/sandbox_helpers.rs —— 自 tools.rs 按领域拆出（纯搬移，零行为变更）

use super::*;

impl ToolGateway {
    /// 读取工作区内图片文件 → data URL（computer 截图回灌视觉模型用）。
    /// 路径沙箱校验；仅支持常见图像格式，8MB 上限防误读。
    pub fn read_image_data_url(&self, rel_path: &str) -> Option<String> {
        let p = self.resolve_safe(rel_path).ok()?;
        let ext = p.extension()?.to_str()?.to_ascii_lowercase();
        let mime = match ext.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "webp" => "image/webp",
            "gif" => "image/gif",
            "bmp" => "image/bmp",
            _ => return None,
        };
        let data = std::fs::read(&p).ok()?;
        if data.len() > 8 * 1024 * 1024 {
            return None;
        }
        Some(format!(
            "data:{mime};base64,{}",
            crate::computer::b64_encode(&data)
        ))
    }

    /// 子进程环境（净化后）：白名单透传 + HOME/TMP 重定向到工作区内沙箱目录。
    /// 敏感变量（各类 KEY / TOKEN / SECRET）一律不外泄给工具子进程。
    pub(crate) fn sandbox_env(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for k in ENV_ALLOW {
            if let Ok(v) = std::env::var(k) {
                out.push((k.to_string(), v));
            }
        }
        let base = self.workspace_root.join(".exmachina").join("sandbox");
        let home = base.join("home");
        let tmp = base.join("tmp");
        let _ = std::fs::create_dir_all(&home);
        let _ = std::fs::create_dir_all(&tmp);
        out.push(("HOME".into(), home.display().to_string()));
        out.push(("USERPROFILE".into(), home.display().to_string()));
        out.push(("TMP".into(), tmp.display().to_string()));
        out.push(("TEMP".into(), tmp.display().to_string()));
        out.push(("EXM_SANDBOX".into(), self.sandbox.mode.clone()));
        out
    }

    /// 构造终端执行计划：按沙箱级别决定是否清环境、是否用 bubblewrap 做系统级隔离
    #[cfg_attr(target_os = "windows", allow(unused_variables))]
    pub(crate) fn shell_opts(&self, cmd: &str) -> ShellOpts {
        if !self.sandbox.enabled() {
            return ShellOpts::plain();
        }
        let root = self.effective_root();
        let envs = self.sandbox_env();
        let memory = self.sandbox.memory_mb;
        let max_procs = self.sandbox.max_processes as u32;
        #[cfg(not(target_os = "windows"))]
        {
            if self.sandbox.use_bwrap && bwrap_available() {
                let root_s = root.display().to_string();
                let mut args: Vec<String> = vec![
                    "--ro-bind".into(),
                    "/".into(),
                    "/".into(),
                    "--bind".into(),
                    root_s.clone(),
                    root_s.clone(),
                    "--dev".into(),
                    "/dev".into(),
                    "--proc".into(),
                    "/proc".into(),
                    "--tmpfs".into(),
                    "/tmp".into(),
                    "--die-with-parent".into(),
                ];
                if !self.sandbox.allow_network {
                    args.push("--unshare-net".into());
                }
                args.push("--".into());
                args.push("sh".into());
                args.push("-c".into());
                let lim = if memory > 0 {
                    format!("ulimit -v {} 2>/dev/null; ", memory * 1024)
                } else {
                    String::new()
                };
                args.push(format!("{lim}{cmd}"));
                return ShellOpts {
                    program: "bwrap".into(),
                    args,
                    env_clear: true,
                    envs,
                    memory_mb: memory,
                    max_processes: max_procs,
                    cmd_override: None,
                };
            }
        }
        // 无 bwrap（或 Windows）：净化环境 + （Linux 下）ulimit 限内存
        #[cfg(target_os = "windows")]
        let program = "cmd";
        #[cfg(not(target_os = "windows"))]
        let program = "sh";
        #[cfg(not(target_os = "windows"))]
        let cmd_override = if memory > 0 {
            Some(format!("ulimit -v {} 2>/dev/null; {cmd}", memory * 1024))
        } else {
            None
        };
        #[cfg(target_os = "windows")]
        let cmd_override = None;
        ShellOpts {
            program: program.into(),
            args: Vec::new(),
            env_clear: true,
            envs,
            memory_mb: memory,
            max_processes: max_procs,
            cmd_override,
        }
    }
}
