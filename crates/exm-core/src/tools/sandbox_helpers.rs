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
        // 执行身份约束(Linux execUser):独立于 sandbox 开关——sandbox off 时身份仍须生效。
        // 解析失败(用户不存在/路径非法)不静默降级:返回错误注入到 cmd_override,
        // 执行时 shell 报出并失败——假安全比不可用更危险。
        #[cfg(unix)]
        let identity = match crate::config::resolve_exec_identity(
            &self.security.exec_user,
            &self.security.exec_home,
        ) {
            Ok(id) => id,
            Err(e) => {
                let mut bad = ShellOpts::plain();
                bad.cmd_override = Some(format!("echo '执行身份约束配置错误: {e}' >&2; exit 125"));
                return bad;
            }
        };
        #[cfg(unix)]
        let confine = identity.as_ref().map(|id| id.home.clone());
        // Windows：无执行身份约束（唯一入口 execUser 仅 Linux 生效）
        #[cfg(not(unix))]
        let confine: Option<String> = None;
        #[cfg(unix)]
        let uid_gid = identity.as_ref().map(|id| (id.uid, id.gid));
        #[cfg(unix)]
        if !self.sandbox.enabled() {
            // sandbox off:身份约束仍生效(唯一加固维度)
            #[cfg(unix)]
            {
                let mut o = ShellOpts::plain();
                if let Some(id) = &identity {
                    let home = id.home.clone();
                    o.confine_home = Some(home.clone());
                    o.uid_gid = Some((id.uid, id.gid));
                    o.envs = vec![
                        ("HOME".into(), home.clone()),
                        ("USERPROFILE".into(), home),
                    ];
                    if let Some(id) = &identity {
                        o.envs.push(("USER".into(), self.security.exec_user.clone()));
                        let _ = &id;
                    }
                }
                return o;
            }
            #[cfg(not(unix))]
            return ShellOpts::plain();
        }
        let root = self.effective_root();
        let mut envs = self.sandbox_env();
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
                    "--die-with-parent".into(),
                ];
                // /tmp 若是独立挂载(tmpfs 等),ro-bind / 不会带上它——namespace 内
                // /tmp 是根文件系统的空目录,工作区落在 /tmp 下的场景(测试/临时部署)
                // 会整体不可见。显式 bind 宿主 /tmp(读写:tmp 本就是暂存语义)。
                let tmp_separate = {
                    use std::os::unix::fs::MetadataExt;
                    let tmp_dev = std::fs::metadata("/tmp").map(|m| m.dev()).ok();
                    let root_dev = std::fs::metadata("/").map(|m| m.dev()).ok();
                    match (tmp_dev, root_dev) {
                        (Some(t), Some(r)) => t != r,
                        // 元数据拿不到时保守按「独立挂载」处理(bind 宿主 /tmp 最不意外)
                        _ => true,
                    }
                };
                if tmp_separate {
                    args.push("--bind".into());
                    args.push("/tmp".into());
                    args.push("/tmp".into());
                } else {
                    args.push("--tmpfs".into());
                    args.push("/tmp".into());
                }
                if !self.sandbox.allow_network {
                    args.push("--unshare-net".into());
                }
                // 身份约束与 bwrap 叠加:bwrap 原生 --uid/--gid(bwrap 需以 root 跑 mount,
                // 目标进程身份在 namespace 内切换)——spawn 侧不再设 uid_gid,避免双路径冲突
                #[cfg(unix)]
                if let Some((uid, gid)) = uid_gid {
                    args.push("--uid".into());
                    args.push(uid.to_string());
                    args.push("--gid".into());
                    args.push(gid.to_string());
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
                // 身份约束开启时 HOME 让位 execHome(身份是更强的边界,workspace 的
                // HOME 重定向被覆盖)
                #[cfg(unix)]
                if let Some(home) = &confine {
                    envs.retain(|(k, _)| k != "HOME" && k != "USERPROFILE");
                    envs.push(("HOME".into(), home.clone()));
                    envs.push(("USERPROFILE".into(), home.clone()));
                    if !self.security.exec_user.trim().is_empty() {
                        envs.push(("USER".into(), self.security.exec_user.clone()));
                    }
                }
                return ShellOpts {
                    program: "bwrap".into(),
                    args,
                    env_clear: true,
                    envs,
                    memory_mb: memory,
                    max_processes: max_procs,
                    cmd_override: None,
                    #[cfg(unix)]
                    uid_gid: None, // 身份已由 bwrap --uid/--gid 承担
                    confine_home: confine.clone(),
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
        // 身份约束开启时 HOME/USER 覆盖为 execHome(优先于 workspace 沙箱 HOME)
        #[cfg(unix)]
        if let Some(home) = &confine {
            envs.retain(|(k, _)| k != "HOME" && k != "USERPROFILE" && k != "USER");
            envs.push(("HOME".into(), home.clone()));
            envs.push(("USERPROFILE".into(), home.clone()));
            if !self.security.exec_user.trim().is_empty() {
                envs.push(("USER".into(), self.security.exec_user.clone()));
            }
        }
        ShellOpts {
            program: program.into(),
            args: Vec::new(),
            env_clear: true,
            envs,
            memory_mb: memory,
            max_processes: max_procs,
            cmd_override,
            #[cfg(unix)]
            uid_gid,
            confine_home: confine,
        }
    }
}
