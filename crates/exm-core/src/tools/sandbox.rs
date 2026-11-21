//! tools/sandbox.rs —— 自 tools.rs 按领域拆出（纯搬移，零行为变更）

use super::managed_process::{capture_prefix, JobEnd, ManagedProcess, Stream};
use super::*;

/// 沙箱执行参数：程序 + 参数 + 环境（净化后）+ 身份（execUser 约束）
pub struct ShellOpts {
    pub program: String,
    pub args: Vec<String>,
    /// true = 清空继承环境后只注入 envs
    pub env_clear: bool,
    pub envs: Vec<(String, String)>,
    /// 内存上限 MB（0 = 不限；Linux 经 ulimit、Windows 经作业对象生效）
    pub memory_mb: usize,
    /// 进程数上限（0 = 不限；仅 Windows 作业对象支持）
    pub max_processes: u32,
    /// 覆盖实际执行的命令文本（如加 ulimit 前缀）；None = 用调用方传入的 cmd
    pub cmd_override: Option<String>,
    /// 执行身份（Linux execUser 约束）：spawn 侧 uid()/gid() 降权 + cwd 圈禁。
    /// None = 不约束（旧行为）。bwrap 路径不设此项（身份经 bwrap --uid/--gid 传入）
    #[cfg(unix)]
    pub uid_gid: Option<(u32, u32)>,
    /// 执行身份 HOME 与 cwd 边界：cwd 保留已绑定项目，但必须位于此目录内。
    pub confine_home: Option<String>,
    /// 执行身份或沙箱配置无效时在 spawn 前失败，不通过 shell 命令回报错误。
    pub configuration_error: Option<String>,
}

/// Resolve the actual cwd for a command while preserving the approved project.
///
/// `confine_home` is a boundary, not a replacement cwd: an Agent may work in
/// `/home/exm/projects/demo` while its HOME remains `/home/exm`.  Both paths
/// are canonicalized before the containment check so a symlink cannot move a
/// project outside the configured execution home.
pub(crate) fn resolve_execution_cwd(
    workspace_root: &Path,
    opts: &ShellOpts,
) -> Result<std::path::PathBuf, String> {
    let cwd = workspace_root
        .canonicalize()
        .map_err(|e| format!("项目工作目录不可用: {} ({e})", workspace_root.display()))?;
    if !cwd.is_dir() {
        return Err(format!("项目工作目录不是目录: {}", cwd.display()));
    }
    if let Some(home) = &opts.confine_home {
        let home_path = std::path::Path::new(home)
            .canonicalize()
            .map_err(|e| format!("执行身份 HOME 不可用: {home} ({e})"))?;
        if !cwd.starts_with(&home_path) {
            return Err(format!(
                "项目目录越过执行身份边界: cwd={}，execHome={}",
                cwd.display(),
                home_path.display()
            ));
        }
    }
    Ok(cwd)
}

/// Build the shell command shared by foreground execution, background jobs and
/// approval replay.  Keeping argv, environment, uid/gid and cwd in one helper
/// prevents the background path from silently bypassing the foreground policy.
pub(crate) fn build_shell_command(
    workspace_root: &Path,
    cmd: &str,
    opts: &ShellOpts,
) -> Result<tokio::process::Command, String> {
    if let Some(error) = &opts.configuration_error {
        return Err(error.clone());
    }
    let mut process = tokio::process::Command::new(&opts.program);
    let effective = opts.cmd_override.clone().unwrap_or_else(|| cmd.to_string());
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
    for (key, value) in &opts.envs {
        process.env(key, value);
    }
    if opts.memory_mb > 0 {
        process.env("EXM_MEM_LIMIT_MB", opts.memory_mb.to_string());
    }
    #[cfg(target_os = "linux")]
    if let Some((uid, gid)) = opts.uid_gid {
        process.uid(uid).gid(gid);
    }
    let cwd = resolve_execution_cwd(workspace_root, opts)?;
    process.current_dir(cwd);
    Ok(process)
}

impl ShellOpts {
    /// 不加固（历史行为：直接 shell 执行，继承环境）
    pub fn plain() -> Self {
        #[cfg(target_os = "windows")]
        {
            ShellOpts {
                program: "cmd".into(),
                args: vec![],
                env_clear: false,
                envs: vec![],
                memory_mb: 0,
                max_processes: 0,
                cmd_override: None,
                confine_home: None,
                configuration_error: None,
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            ShellOpts {
                program: "sh".into(),
                args: vec![],
                env_clear: false,
                envs: vec![],
                memory_mb: 0,
                max_processes: 0,
                cmd_override: None,
                uid_gid: None,
                confine_home: None,
                configuration_error: None,
            }
        }
    }
}

/// 联网类命令特征（strict 模式下需要审批）
const NETWORK_COMMAND_MARKS: &[&str] = &[
    "curl",
    "wget",
    "pip install",
    "pip3 install",
    "npm install",
    "npm ci",
    "pnpm install",
    "yarn add",
    "cargo install",
    "cargo add",
    "apt ",
    "apt-get",
    "dnf ",
    "yum ",
    "brew ",
    "git clone",
    "git fetch",
    "git pull",
    "git push",
    "docker ",
    "docker compose pull",
    "ssh ",
    "scp ",
    "rsync ",
    "sftp ",
    "invoke-webrequest",
];

pub fn is_network_command(lowered: &str) -> bool {
    NETWORK_COMMAND_MARKS.iter().any(|m| lowered.contains(m))
}

/// 子进程环境白名单：只透传运行必需项，敏感变量（各类 KEY/TOKEN/SECRET）一律不外泄
pub(crate) const ENV_ALLOW: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SystemRoot",
    "SystemDrive",
    "COMSPEC",
    "WINDIR",
    "LANG",
    "LC_ALL",
    "TERM",
    "OS",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "USER",
    "USERNAME",
    "SHELL",
];

#[cfg(not(target_os = "windows"))]
pub(crate) fn bwrap_available() -> bool {
    std::process::Command::new("bwrap")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Windows：无 bubblewrap（沙箱走作业对象/环境净化），恒不可用。
/// 保留函数形状使跨平台调用方（tools/mod.rs re-export、sandbox_helpers）
/// 无需逐处 cfg——「能力探测统一返回 false」比条件编译分散更不易漏。
#[cfg(target_os = "windows")]
pub(crate) fn bwrap_available() -> bool {
    false
}

// ---------------------------------------------------------------- Windows 作业对象

/// Windows 作业对象（Job Object）：把子进程收进作业，由内核强制
/// **内存上限 / 活动进程数上限**，并一律带 `KILL_ON_JOB_CLOSE` ——
/// 句柄一关（含任务被 abort 时的 Drop），作业内整棵进程树随之终止。
/// 这是 `taskkill` 之外的最后一道保险：agent 跑飞的长命令不会再拖着机器。
///
/// 非 Windows 平台是空类型（Unix 生命周期由独立进程组管理）。
pub struct WinJob {
    #[cfg(target_os = "windows")]
    handle: *mut core::ffi::c_void,
}

// 内核句柄本质是不透明整数；CloseHandle 可从任意线程调用。
// 不声明 Send 的话，任何跨 await 持有作业句柄的 future 都会变成 !Send（编译期报错）。
unsafe impl Send for WinJob {}
unsafe impl Sync for WinJob {}

#[cfg(target_os = "windows")]
impl WinJob {
    /// 建作业 → 设限 → 把子进程收进去；任一步失败返回 None（调用方继续按无作业执行）
    ///
    /// # Safety
    /// `process` 必须是存活的 Win32 进程句柄——本函数仅将其透传给 Win32 API，不解引用。
    pub unsafe fn attach(
        process: *mut core::ffi::c_void,
        memory_mb: usize,
        max_processes: u32,
    ) -> Option<WinJob> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_JOB_MEMORY,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
        };
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            let mut flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if memory_mb > 0 {
                let bytes = memory_mb * 1024 * 1024;
                flags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_JOB_MEMORY;
                info.ProcessMemoryLimit = bytes;
                info.JobMemoryLimit = bytes;
            }
            if max_processes > 0 {
                flags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
                info.BasicLimitInformation.ActiveProcessLimit = max_processes;
            }
            info.BasicLimitInformation.LimitFlags = flags;
            let sized = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
            let set = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                sized,
            );
            if set == 0 {
                CloseHandle(job);
                return None;
            }
            // 子进程可能已在别的作业里（嵌套作业需 Windows 8+ 且外层允许）——失败即降级
            if AssignProcessToJobObject(job, process) == 0 {
                CloseHandle(job);
                return None;
            }
            Some(WinJob { handle: job })
        }
    }
}

#[cfg(not(target_os = "windows"))]
impl WinJob {
    /// 非 Windows 恒不可用（签名与 Windows 侧保持一致，见其 Safety 约定）
    ///
    /// # Safety
    ///
    /// 非 Windows 桩实现恒返回 Err，无实际 unsafe 语义；签名保持与 Windows 侧一致。
    pub unsafe fn attach(
        _process: *mut core::ffi::c_void,
        _memory_mb: usize,
        _max_processes: u32,
    ) -> Option<WinJob> {
        None
    }
}

#[cfg(target_os = "windows")]
impl Drop for WinJob {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        if !self.handle.is_null() {
            // KILL_ON_JOB_CLOSE：关句柄即终止作业内所有进程
            unsafe { CloseHandle(self.handle) };
        }
    }
}

/// 把子进程收进带资源上限的作业（Windows）；句柄不可得时使用备用终止路径。
#[cfg(target_os = "windows")]
pub(crate) fn attach_job(child: &tokio::process::Child, opts: &ShellOpts) -> Option<WinJob> {
    let handle = child.raw_handle()?;
    // SAFETY: 原生句柄来自当前持有的 Windows 子进程。
    unsafe { WinJob::attach(handle, opts.memory_mb, opts.max_processes) }
}

/// 作业对象是否可用（沙箱加固自检 / 测试用）：建一个空作业即关闭
pub fn windows_job_available() -> bool {
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        let mut c = Command::new("cmd");
        c.args(["/C", "exit"]);
        let mut child = match c
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return false,
        };
        let handle = {
            use std::os::windows::io::AsRawHandle;
            child.as_raw_handle()
        };
        // SAFETY: std::process::Child 存活期间的句柄，attach 内部仅透传给 Win32 API
        let ok = !handle.is_null() && unsafe { WinJob::attach(handle, 256, 0) }.is_some();
        let _ = child.kill();
        let _ = child.wait();
        ok
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

/// 跨平台 shell 执行（工具闸门与审批代执行共用）。
/// 内部走 [`execute_shell_with`]（无加固）——需要沙箱时由调用方给出 [`ShellOpts`]。
pub async fn execute_shell_command_timed(
    workspace_root: &Path,
    cmd: &str,
    timeout_secs: u64,
) -> ToolResult {
    execute_shell_with(workspace_root, cmd, timeout_secs, &ShellOpts::plain()).await
}

/// 按 ShellOpts 执行：可换程序（bwrap）、清环境、限内存
pub async fn execute_shell_with(
    workspace_root: &Path,
    cmd: &str,
    timeout_secs: u64,
    opts: &ShellOpts,
) -> ToolResult {
    execute_shell_managed(workspace_root, cmd, timeout_secs, opts, None).await
}

/// 终端直通及审批代执行绑定调用时的轮次取消，不依赖轮询全局会话表。
pub(crate) async fn execute_shell_for_session(
    workspace_root: &Path,
    cmd: &str,
    timeout_secs: u64,
    opts: &ShellOpts,
    session_id: &str,
) -> ToolResult {
    let cancellation = crate::round_trace::subscribe_cancel(session_id);
    execute_shell_managed(workspace_root, cmd, timeout_secs, opts, cancellation).await
}

async fn execute_shell_managed(
    workspace_root: &Path,
    cmd: &str,
    timeout_secs: u64,
    opts: &ShellOpts,
    cancellation: Option<tokio::sync::watch::Receiver<bool>>,
) -> ToolResult {
    if cancellation
        .as_ref()
        .is_some_and(|receiver| *receiver.borrow())
    {
        return ToolResult::err("命令已取消，未启动");
    }
    let process = match ManagedProcess::spawn(workspace_root, cmd, opts) {
        Ok(process) => process,
        Err(error) => return ToolResult::err(format!("命令执行失败: {error}")),
    };
    const OUTPUT_BUF_CAP: usize = 1024 * 1024;
    let out_buf = parking_lot::Mutex::new(Vec::new());
    let err_buf = parking_lot::Mutex::new(Vec::new());
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs.max(1));
    let outcome = process
        .run(
            Some(deadline),
            crate::round_trace::wait_for_cancel(cancellation),
            |stream, bytes| {
                let buf = match stream {
                    Stream::Stdout => &out_buf,
                    Stream::Stderr => &err_buf,
                };
                capture_prefix(&mut buf.lock(), bytes, OUTPUT_BUF_CAP);
            },
        )
        .await;
    let out_buf = out_buf.into_inner();
    let err_buf = err_buf.into_inner();
    let raw_out = String::from_utf8_lossy(&out_buf);
    let raw_err = String::from_utf8_lossy(&err_buf);
    let cleanup = outcome
        .cleanup_error
        .map(|note| format!("\n清理未确认：{note}"))
        .unwrap_or_default();
    let timed_out = matches!(&outcome.end, JobEnd::TimedOut);
    match outcome.end {
        JobEnd::TimedOut | JobEnd::Cancelled => {
            // 截断必须显式声明：半截输出若无标记，模型会把不完整结果当完整结论
            const PARTIAL_CAP: usize = 2000;
            const PARTIAL_ERR_CAP: usize = 1000;
            let reason = if timed_out {
                format!("命令超时（{timeout_secs}s）")
            } else {
                "命令已取消".into()
            };
            let partial_total = raw_out.chars().count();
            let mut msg = format!(
                "{reason}{}；终止前输出：\n{}",
                if cleanup.is_empty() {
                    "，已终止"
                } else {
                    ""
                },
                raw_out.chars().take(PARTIAL_CAP).collect::<String>()
            );
            if partial_total > PARTIAL_CAP {
                msg.push_str(&format!(
                    "\n…（已截断：终止前输出共 {partial_total} 字符，仅保留前 {PARTIAL_CAP}）"
                ));
            }
            if !raw_err.trim().is_empty() {
                let err_total = raw_err.chars().count();
                msg.push_str(&format!(
                    "\n[stderr] {}",
                    raw_err.chars().take(PARTIAL_ERR_CAP).collect::<String>()
                ));
                if err_total > PARTIAL_ERR_CAP {
                    msg.push_str(&format!("\n…（stderr 已截断：共 {err_total} 字符）"));
                }
            }
            msg.push_str(&cleanup);
            ToolResult::err(msg)
        }
        JobEnd::Failed(error) => ToolResult::err(format!("命令执行失败: {error}{cleanup}")),
        JobEnd::Exited(status) => {
            // 截断必须显式声明：半截输出若无标记，模型会把不完整结果当完整结论
            // （典型误判：测试/构建日志尾部被切，把半路输出读成「成功」）
            const OUT_CAP: usize = 8000;
            const ERR_CAP: usize = 2000;
            let out_total = raw_out.chars().count();
            let mut text: String = raw_out.chars().take(OUT_CAP).collect();
            if out_total > OUT_CAP {
                text.push_str(&format!("\n…（输出已截断：仅保留前 {OUT_CAP} 字符，共 {out_total} 字符；完整输出可用重定向落盘后 read）"));
            }
            if !raw_err.trim().is_empty() {
                let err_total = raw_err.chars().count();
                text.push_str("\n[stderr] ");
                text.push_str(&raw_err.chars().take(ERR_CAP).collect::<String>());
                if err_total > ERR_CAP {
                    text.push_str(&format!("\n…（stderr 已截断：共 {err_total} 字符）"));
                }
            }
            if status.success() && cleanup.is_empty() {
                ToolResult {
                    ok: true,
                    output: text,
                    error: None,
                    images: Vec::new(),
                }
            } else {
                // 失败必须可诊断：退出码与输出摘要必须进 error——
                // 回灌层（runtime::tool_feedback）与事件/审计摘要只取 error 字段，
                // 只置 ok=false 而 error 留空的话，模型与用户看到的失败原因就是一句空话。
                // output 保留全文：审批代执行等老调用方仍按 output 取输出（向后兼容）。
                let code = status.code().unwrap_or(-1);
                let out_tail: String = {
                    let chars: Vec<char> = raw_out.chars().collect();
                    let start = chars.len().saturating_sub(2000);
                    chars[start..].iter().collect()
                };
                let mut reason = if status.success() {
                    "命令结束但子进程树清理未确认".into()
                } else {
                    format!("命令失败（退出码 {code}）")
                };
                if out_tail.trim().is_empty() && raw_err.trim().is_empty() {
                    reason.push_str("；无任何输出");
                } else {
                    reason.push_str("；输出摘要：\n");
                    if !out_tail.trim().is_empty() {
                        reason.push_str(&out_tail);
                    }
                    if !raw_err.trim().is_empty() {
                        let err_tail: String = {
                            let chars: Vec<char> = raw_err.chars().collect();
                            let start = chars.len().saturating_sub(1000);
                            chars[start..].iter().collect()
                        };
                        reason.push_str(&format!("\n[stderr 尾部] {err_tail}"));
                    }
                }
                reason.push_str(&cleanup);
                ToolResult {
                    ok: false,
                    output: text,
                    error: Some(reason),
                    images: Vec::new(),
                }
            }
        }
    }
}

#[cfg(test)]
mod execution_config_tests {
    use super::*;

    #[test]
    fn 执行身份配置错误在spawn前失败() {
        let mut opts = ShellOpts::plain();
        opts.configuration_error = Some("bad identity; do not interpret".into());
        let result = build_shell_command(Path::new("/"), "echo should-not-run", &opts);
        assert!(matches!(
            result,
            Err(error) if error == "bad identity; do not interpret"
        ));
    }
}
