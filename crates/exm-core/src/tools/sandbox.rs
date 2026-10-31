//! tools/sandbox.rs —— 自 tools.rs 按领域拆出（纯搬移，零行为变更）

use super::*;

/// 沙箱执行参数：程序 + 参数 + 环境（净化后）
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

// ---------------------------------------------------------------- Windows 作业对象

/// Windows 作业对象（Job Object）：把子进程收进作业，由内核强制
/// **内存上限 / 活动进程数上限**，并一律带 `KILL_ON_JOB_CLOSE` ——
/// 句柄一关（含任务被 abort 时的 Drop），作业内整棵进程树随之终止。
/// 这是 `taskkill` 之外的最后一道保险：agent 跑飞的长命令不会再拖着机器。
///
/// 非 Windows 平台是空类型（Linux 侧由 ulimit + bwrap 负责）。
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
    pub(crate) fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        if !self.handle.is_null() {
            // KILL_ON_JOB_CLOSE：关句柄即终止作业内所有进程
            unsafe { CloseHandle(self.handle) };
        }
    }
}

/// 取子进程的原生句柄（Windows 传作业对象用；其他平台恒 None）
fn child_raw_handle(child: &tokio::process::Child) -> *mut core::ffi::c_void {
    #[cfg(target_os = "windows")]
    {
        // tokio::process::Child::raw_handle 是固有方法（无需引入 AsRawHandle）
        child.raw_handle().unwrap_or(std::ptr::null_mut())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = child;
        std::ptr::null_mut()
    }
}

/// 把子进程收进带资源上限的作业（Windows）；其他平台 / 句柄不可得时静默跳过
pub(crate) fn attach_job(child: &tokio::process::Child, opts: &ShellOpts) -> Option<WinJob> {
    let handle = child_raw_handle(child);
    if handle.is_null() {
        return None;
    }
    // SAFETY: handle 来自 child_raw_handle 的存活子进程，attach 内部仅透传给 Win32 API
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
    let mut process = tokio::process::Command::new(&opts.program);
    let effective: String = opts.cmd_override.clone().unwrap_or_else(|| cmd.to_string());
    if opts.args.is_empty() {
        // 默认：走 shell -c（bwrap 时 args 已含完整命令行）
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
    if opts.memory_mb > 0 {
        let _ = process.env("EXM_MEM_LIMIT_MB", opts.memory_mb.to_string());
    }
    let spawned = process
        .current_dir(workspace_root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => return ToolResult::err(format!("命令执行失败: {e}")),
    };
    // `pid` 只在 Windows 路径下作为 taskkill 的 /PID 入参使用；
    // POSIX 上 tokio 已通过 spawn 时的 kill_on_drop(true) 自动 SIGKILL 子进程。
    #[cfg_attr(not(target_os = "windows"), allow(unused_variables))]
    let pid = child.id();
    // Windows：收进作业对象（内存/进程数上限 + 句柄关闭即整树终止）。
    // 句柄活到函数末尾，Drop 即关 —— 命令结束后不留游离子进程。
    let _job = attach_job(&child, opts);
    // 管道读取放入独立任务：增量写入共享缓冲（而非 read_to_end 等闭流）。
    // 为什么：超时强杀后可能残留孙进程占着管道写端（如 sh -c "x; sleep 30" 杀不掉 sleep），
    // 等 EOF 会永远等不到——增量缓冲让超时路径能立刻快照「已产出」的部分输出。
    // 1MB 软上限防病态输出撑爆内存（末尾文本截断策略不变）。
    const OUTPUT_BUF_CAP: usize = 1024 * 1024;
    let out_buf: Arc<parking_lot::Mutex<Vec<u8>>> = Arc::default();
    let err_buf: Arc<parking_lot::Mutex<Vec<u8>>> = Arc::default();
    let out_task = child.stdout.take().map(|mut s| {
        let buf = out_buf.clone();
        tokio::spawn(async move {
            let mut chunk = [0u8; 8192];
            loop {
                match tokio::io::AsyncReadExt::read(&mut s, &mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut b = buf.lock();
                        if b.len() < OUTPUT_BUF_CAP {
                            b.extend_from_slice(&chunk[..n]);
                        }
                    }
                }
            }
        })
    });
    let err_task = child.stderr.take().map(|mut s| {
        let buf = err_buf.clone();
        tokio::spawn(async move {
            let mut chunk = [0u8; 4096];
            loop {
                match tokio::io::AsyncReadExt::read(&mut s, &mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut b = buf.lock();
                        if b.len() < OUTPUT_BUF_CAP {
                            b.extend_from_slice(&chunk[..n]);
                        }
                    }
                }
            }
        })
    });

    let budget = std::time::Duration::from_secs(timeout_secs.max(1));
    match tokio::time::timeout(budget, child.wait()).await {
        Err(_) => {
            // 进程树强杀：Windows taskkill /T /F（cmd 的子进程一并终止）
            #[cfg(target_os = "windows")]
            {
                if let Some(pid) = pid {
                    let _ = tokio::process::Command::new("taskkill")
                        .args(["/PID", &pid.to_string(), "/T", "/F"])
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status()
                        .await;
                }
            }
            // 有界回收：先强杀（POSIX 立即 SIGKILL；Windows 上方 taskkill 已收进程树），
            // 再等被杀进程退出最长 3s——超时路径绝不无限阻塞。
            // 为什么必须显式杀：kill_on_drop 只在 Child 被 drop 时才生效。
            let _ = child.start_kill();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(3), child.wait()).await;
            // 快照已读输出：增量读者持续写入共享缓冲，即使残留孙进程占着管道写端
            // （EOF 永不到来），超时前已产出的部分照样可诊断——长构建/测试卡在哪一步不再黑箱。
            if let Some(t) = out_task {
                t.abort();
            }
            if let Some(t) = err_task {
                t.abort();
            }
            let out_lock = out_buf.lock();
            let err_lock = err_buf.lock();
            let partial = String::from_utf8_lossy(&out_lock);
            let partial_err = String::from_utf8_lossy(&err_lock);
            // 截断必须显式声明：半截输出若无标记，模型会把不完整结果当完整结论
            const PARTIAL_CAP: usize = 2000;
            const PARTIAL_ERR_CAP: usize = 1000;
            let partial_total = partial.chars().count();
            let mut msg = format!(
                "命令超时（{timeout_secs}s）已终止；终止前输出：\n{}",
                partial.chars().take(PARTIAL_CAP).collect::<String>()
            );
            if partial_total > PARTIAL_CAP {
                msg.push_str(&format!(
                    "\n…（已截断：终止前输出共 {partial_total} 字符，仅保留前 {PARTIAL_CAP}）"
                ));
            }
            if !partial_err.trim().is_empty() {
                let err_total = partial_err.chars().count();
                msg.push_str(&format!(
                    "\n[stderr] {}",
                    partial_err
                        .chars()
                        .take(PARTIAL_ERR_CAP)
                        .collect::<String>()
                ));
                if err_total > PARTIAL_ERR_CAP {
                    msg.push_str(&format!("\n…（stderr 已截断：共 {err_total} 字符）"));
                }
            }
            ToolResult::err(msg)
        }
        Ok(Err(e)) => ToolResult::err(format!("命令执行失败: {e}")),
        Ok(Ok(status)) => {
            // 正常退出：管道已闭（EOF），读者任务自然收尾后取全量输出
            if let Some(t) = out_task {
                let _ = t.await;
            }
            if let Some(t) = err_task {
                let _ = t.await;
            }
            let raw_out = String::from_utf8_lossy(&out_buf.lock()).to_string();
            let raw_err = String::from_utf8_lossy(&err_buf.lock()).to_string();
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
            if status.success() {
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
                let mut reason = format!("命令失败（退出码 {code}）");
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
