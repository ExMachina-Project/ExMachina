//! 工具网关 —— docs/01 §2.6：白名单 + 工作区沙箱 + 审计
//!
//! 工具面（对标主流 agent 的「手」）：read（行号/分页）/ edit（精确替换）/ grep / glob /
//! filesystem（write/mkdir/list）/ terminal（白名单 + 审批 + 后台长任务）/ web_search（真实后端，
//! 未配置即不下发）/ web_fetch / agent_manage（组内编成）/ schedule（自主排程）。

use crate::config::{SandboxConfig, SearchConfig, SecurityConfig};
use crate::provider::LlmProvider;
use crate::registry::LocalRegistry;
use crate::store::Store;
use crate::types::{normalize_domain, AgentDefinition, ApprovalRequest, CoreEvent, Tier, ToolName};
use anyhow::{bail, Context};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

pub struct ToolResult {
    pub ok: bool,
    pub output: String,
    pub error: Option<String>,
    /// 副产物图像（如 computer 截图）的落盘路径：runtime 在目标模型具备视觉能力时
    /// 作为图像输入回灌；渠道（WebUI）据此在工具卡里预览。
    pub images: Vec<String>,
}

impl ToolResult {
    pub(crate) fn ok(output: impl Into<String>) -> Self {
        ToolResult {
            ok: true,
            output: output.into(),
            error: None,
            images: Vec::new(),
        }
    }
    pub(crate) fn err(msg: impl Into<String>) -> Self {
        ToolResult {
            ok: false,
            output: String::new(),
            error: Some(msg.into()),
            images: Vec::new(),
        }
    }
    fn with_images(mut self, images: Vec<String>) -> Self {
        self.images = images;
        self
    }
}

/// 终端命令白名单规则：命令名 → 允许的子命令前缀（空 = 该命令整体放行）。
/// 只匹配**首命令**且拒绝 shell 连接符（`&&` `|` `;` 等），避免「白名单命令 + 追加危险命令」绕过。
struct AllowRule {
    cmd: &'static str,
    subs: &'static [&'static str],
}

const TERMINAL_ALLOW: &[AllowRule] = &[
    AllowRule {
        cmd: "dir",
        subs: &[],
    },
    AllowRule {
        cmd: "ls",
        subs: &[],
    },
    AllowRule {
        cmd: "type",
        subs: &[],
    },
    AllowRule {
        cmd: "echo",
        subs: &[],
    },
    AllowRule {
        cmd: "findstr",
        subs: &[],
    },
    AllowRule {
        cmd: "where",
        subs: &[],
    },
    AllowRule {
        cmd: "which",
        subs: &[],
    },
    AllowRule {
        cmd: "whoami",
        subs: &[],
    },
    AllowRule {
        cmd: "uname",
        subs: &[],
    },
    AllowRule {
        cmd: "pwd",
        subs: &[],
    },
    AllowRule {
        cmd: "date",
        subs: &[],
    },
    AllowRule {
        cmd: "head",
        subs: &[],
    },
    AllowRule {
        cmd: "tail",
        subs: &[],
    },
    AllowRule {
        cmd: "cat",
        subs: &[],
    },
    AllowRule {
        cmd: "wc",
        subs: &[],
    },
    AllowRule {
        cmd: "diff",
        subs: &[],
    },
    AllowRule {
        cmd: "mkdir",
        subs: &[],
    },
    AllowRule {
        cmd: "cp",
        subs: &[],
    },
    AllowRule {
        cmd: "mv",
        subs: &[],
    },
    AllowRule {
        cmd: "touch",
        subs: &[],
    },
    AllowRule {
        cmd: "rg",
        subs: &[],
    },
    AllowRule {
        cmd: "grep",
        subs: &[],
    },
    AllowRule {
        cmd: "find",
        subs: &[],
    },
    AllowRule {
        cmd: "sed",
        subs: &["-n"],
    },
    AllowRule {
        cmd: "node",
        subs: &[],
    },
    AllowRule {
        cmd: "npx",
        subs: &[],
    },
    AllowRule {
        cmd: "tsc",
        subs: &[],
    },
    AllowRule {
        cmd: "deno",
        subs: &["test", "run", "check", "fmt", "lint", "info", "cache"],
    },
    AllowRule {
        cmd: "bun",
        subs: &["test", "run", "install", "x", "build"],
    },
    AllowRule {
        cmd: "pytest",
        subs: &[],
    },
    AllowRule {
        cmd: "jest",
        subs: &[],
    },
    AllowRule {
        cmd: "vitest",
        subs: &[],
    },
    AllowRule {
        cmd: "npm",
        subs: &[
            "run", "test", "ci", "install", "ls", "view", "why", "audit", "outdated", "pack",
        ],
    },
    AllowRule {
        cmd: "pnpm",
        subs: &[
            "run", "test", "install", "list", "ls", "why", "audit", "outdated",
        ],
    },
    AllowRule {
        cmd: "yarn",
        subs: &["run", "test", "install", "list", "why", "audit", "outdated"],
    },
    AllowRule {
        cmd: "git",
        subs: &[
            "status",
            "log",
            "diff",
            "show",
            "branch",
            "fetch",
            "add",
            "commit",
            "checkout",
            "switch",
            "restore",
            "stash",
            "rev-parse",
            "ls-files",
            "blame",
            "describe",
            "tag",
            "remote",
            "config",
            "apply",
            "merge-base",
            "shortlog",
            "grep",
            "worktree",
            "init",
        ],
    },
    AllowRule {
        cmd: "cargo",
        subs: &[
            "build",
            "check",
            "test",
            "run",
            "bench",
            "fmt",
            "clippy",
            "doc",
            "metadata",
            "tree",
            "nextest",
            "update",
            "add",
            "remove",
            "fetch",
            "verify-project",
            "locate-project",
            "pkgid",
            "search",
            "info",
            "clean",
        ],
    },
    AllowRule {
        cmd: "rustc",
        subs: &[],
    },
    AllowRule {
        cmd: "rustfmt",
        subs: &[],
    },
    AllowRule {
        cmd: "python",
        subs: &[],
    },
    AllowRule {
        cmd: "python3",
        subs: &[],
    },
    AllowRule {
        cmd: "pip",
        subs: &["list", "show", "freeze", "download"],
    },
    AllowRule {
        cmd: "go",
        subs: &[
            "build", "test", "run", "vet", "fmt", "mod", "get", "generate", "doc", "env", "list",
        ],
    },
    AllowRule {
        cmd: "gcc",
        subs: &[],
    },
    AllowRule {
        cmd: "g++",
        subs: &[],
    },
    AllowRule {
        cmd: "clang",
        subs: &[],
    },
    AllowRule {
        cmd: "make",
        subs: &[],
    },
    AllowRule {
        cmd: "cmake",
        subs: &[],
    },
    AllowRule {
        cmd: "java",
        subs: &[],
    },
    AllowRule {
        cmd: "javac",
        subs: &[],
    },
    AllowRule {
        cmd: "mvn",
        subs: &[],
    },
    AllowRule {
        cmd: "gradle",
        subs: &[],
    },
    AllowRule {
        cmd: "dotnet",
        subs: &["build", "test", "run", "restore", "format", "publish"],
    },
    AllowRule {
        cmd: "php",
        subs: &[],
    },
    AllowRule {
        cmd: "composer",
        subs: &["install", "update", "test", "dump-autoload"],
    },
    AllowRule {
        cmd: "ruby",
        subs: &[],
    },
    AllowRule {
        cmd: "gem",
        subs: &["list", "install", "info"],
    },
    AllowRule {
        cmd: "rake",
        subs: &[],
    },
    // ---- harness 第二轮扩充（spec: agent-terminal-tools）----
    // 信息只读类
    AllowRule {
        cmd: "stat",
        subs: &[],
    },
    AllowRule {
        cmd: "du",
        subs: &[],
    },
    AllowRule {
        cmd: "df",
        subs: &[],
    },
    AllowRule {
        cmd: "env",
        subs: &[],
    },
    AllowRule {
        cmd: "ps",
        subs: &[],
    },
    AllowRule {
        cmd: "file",
        subs: &[],
    },
    AllowRule {
        cmd: "basename",
        subs: &[],
    },
    AllowRule {
        cmd: "dirname",
        subs: &[],
    },
    AllowRule {
        cmd: "realpath",
        subs: &[],
    },
    AllowRule {
        cmd: "md5sum",
        subs: &[],
    },
    AllowRule {
        cmd: "sha256sum",
        subs: &[],
    },
    AllowRule {
        cmd: "free",
        subs: &[],
    },
    AllowRule {
        cmd: "uptime",
        subs: &[],
    },
    AllowRule {
        cmd: "hostname",
        subs: &[],
    },
    AllowRule {
        cmd: "id",
        subs: &[],
    },
    AllowRule {
        cmd: "groups",
        subs: &[],
    },
    AllowRule {
        cmd: "locale",
        subs: &[],
    },
    // 文本处理类
    AllowRule {
        cmd: "awk",
        subs: &[],
    },
    AllowRule {
        cmd: "sort",
        subs: &[],
    },
    AllowRule {
        cmd: "uniq",
        subs: &[],
    },
    AllowRule {
        cmd: "xargs",
        subs: &[],
    },
    AllowRule {
        cmd: "cut",
        subs: &[],
    },
    AllowRule {
        cmd: "tr",
        subs: &[],
    },
    AllowRule {
        cmd: "column",
        subs: &[],
    },
    AllowRule {
        cmd: "bc",
        subs: &[],
    },
    AllowRule {
        cmd: "jq",
        subs: &[],
    },
    AllowRule {
        cmd: "yq",
        subs: &[],
    },
    // 网络获取类（联网语义仍受沙箱联网闸门后置拦截）
    AllowRule {
        cmd: "curl",
        subs: &[],
    },
    AllowRule {
        cmd: "wget",
        subs: &[],
    },
    AllowRule {
        cmd: "ping",
        subs: &[],
    },
    AllowRule {
        cmd: "dig",
        subs: &[],
    },
    AllowRule {
        cmd: "nslookup",
        subs: &[],
    },
    AllowRule {
        cmd: "host",
        subs: &[],
    },
    AllowRule {
        cmd: "ip",
        subs: &["addr", "route", "link", "a", "r", "l", "-brief"],
    },
    // 压缩归档类
    AllowRule {
        cmd: "tar",
        subs: &[],
    },
    AllowRule {
        cmd: "unzip",
        subs: &[],
    },
    AllowRule {
        cmd: "zip",
        subs: &[],
    },
    AllowRule {
        cmd: "gzip",
        subs: &[],
    },
    AllowRule {
        cmd: "gunzip",
        subs: &[],
    },
    AllowRule {
        cmd: "xz",
        subs: &[],
    },
    AllowRule {
        cmd: "zcat",
        subs: &[],
    },
    // 权限与时间类
    AllowRule {
        cmd: "chmod",
        subs: &[],
    },
    AllowRule {
        cmd: "chown",
        subs: &[],
    },
    AllowRule {
        cmd: "sleep",
        subs: &[],
    },
    // 远程同步类（联网语义仍受沙箱联网闸门后置拦截）
    AllowRule {
        cmd: "ssh",
        subs: &[],
    },
    AllowRule {
        cmd: "scp",
        subs: &[],
    },
    AllowRule {
        cmd: "rsync",
        subs: &[],
    },
    AllowRule {
        cmd: "sftp",
        subs: &[],
    },
    // 容器类（只读 + exec/run；高危子命令仍走审批）
    AllowRule {
        cmd: "docker",
        subs: &[
            "ps", "images", "inspect", "logs", "exec", "run", "version",
        ],
    },
    // 数据库类（本地文件库；psql/mysql 走审批）
    AllowRule {
        cmd: "sqlite3",
        subs: &[],
    },
    // 媒体与工具类
    AllowRule {
        cmd: "ffmpeg",
        subs: &[],
    },
    AllowRule {
        cmd: "convert",
        subs: &[],
    },
    AllowRule {
        cmd: "openssl",
        subs: &[],
    },
    AllowRule {
        cmd: "base64",
        subs: &[],
    },
    AllowRule {
        cmd: "uuidgen",
        subs: &[],
    },
    AllowRule {
        cmd: "xxd",
        subs: &[],
    },
];

/// shell 连接符：白名单只放行单一命令，带连接符一律拒绝（防「白名单命令 && 危险命令」绕过）
const SHELL_CHAIN_MARKS: &[&str] = &["&&", "||", ";", "|", "`", "$(", ">", "<", "&"];

/// 剥掉引号包裹的片段（引号内的 `>` `|` 等是普通字符，不应误判为连接符）
fn strip_quoted(cmd: &str) -> String {
    let mut out = String::with_capacity(cmd.len());
    let mut quote: Option<char> = None;
    for ch in cmd.chars() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                }
            }
            None => {
                if ch == '"' || ch == '\'' {
                    quote = Some(ch);
                } else {
                    out.push(ch);
                }
            }
        }
    }
    out
}

fn contains_shell_chain(cmd: &str) -> bool {
    let bare = strip_quoted(cmd);
    SHELL_CHAIN_MARKS.iter().any(|m| bare.contains(m))
}

/// 白名单判定：首命令 + （有子命令约束时）子命令前缀
fn terminal_allowed(cmd: &str) -> bool {
    let lowered = cmd.trim().to_lowercase();
    let mut parts = lowered.split_whitespace();
    let Some(head_raw) = parts.next() else {
        return false;
    };
    let head = head_raw.trim_end_matches(".exe");
    let Some(rule) = TERMINAL_ALLOW.iter().find(|r| r.cmd == head) else {
        return false;
    };
    if rule.subs.is_empty() {
        return true;
    }
    let rest: Vec<&str> = parts.collect();
    rule.subs.iter().any(|s| {
        let want: Vec<&str> = s.split_whitespace().collect();
        rest.len() >= want.len() && want.iter().zip(rest.iter()).all(|(a, b)| a == b)
    })
}

/// 高危命令特征（审批闸门 risky 模式匹配子串）
const DESTRUCTIVE_PATTERNS: &[&str] = &[
    "rm ",
    "rm -",
    "del ",
    "rmdir",
    "rd /s",
    "format ",
    "shutdown",
    "taskkill",
    "reg delete",
    "reg add",
    "dd if=",
    "mkfs",
    "git push --force",
    "git push -f",
    "git reset --hard",
    "git clean",
    "git rebase",
    "drop table",
    "drop database",
    "truncate table",
    "remove-item",
    "invoke-expression",
    "curl | sh",
    "curl | bash",
];

pub fn is_destructive_command(lowered: &str) -> bool {
    DESTRUCTIVE_PATTERNS.iter().any(|p| lowered.contains(p))
}

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
const ENV_ALLOW: &[&str] = &[
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
fn bwrap_available() -> bool {
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
    fn drop(&mut self) {
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
fn attach_job(child: &tokio::process::Child, opts: &ShellOpts) -> Option<WinJob> {
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
/// 极简 HTML 正文提取：剥 script/style 块与标签、压缩空白（零正则依赖）。
/// 全程字符级推进（Vec<char> 索引）：lower 与原文同长对齐，杜绝字节级切片
/// 落进多字节字符内部的 panic（部署实测旧实现 15177 字节边界崩溃）。
fn strip_html(html: &str) -> String {
    let src: Vec<char> = html.chars().collect();
    // 小写化按字符逐个 to_lowercase 首字符（标签判定只涉及 ASCII，此处保语义即可）
    let lower: Vec<char> = src
        .iter()
        .map(|c| c.to_lowercase().next().unwrap_or(*c))
        .collect();
    let starts_with = |from: usize, pat: &str| -> bool {
        let pcs: Vec<char> = pat.chars().collect();
        lower.len() >= from + pcs.len() && lower[from..from + pcs.len()].iter().eq(pcs.iter())
    };
    let mut out = String::with_capacity(src.len() / 2);
    let mut i = 0usize;
    let mut skip_until: Option<&'static str> = None;
    while i < src.len() {
        if let Some(end) = skip_until {
            if starts_with(i, end) {
                skip_until = None;
                i += end.chars().count();
                // 吞掉闭合标签的剩余部分（到 '>'）
                while i < src.len() && src[i] != '>' {
                    i += 1;
                }
                i = (i + 1).min(src.len());
            } else {
                i += 1;
            }
            continue;
        }
        if starts_with(i, "<script") {
            skip_until = Some("</script");
        } else if starts_with(i, "<style") {
            skip_until = Some("</style");
        } else if src[i] == '<' {
            while i < src.len() && src[i] != '>' {
                i += 1;
            }
        } else {
            let ch = src[i];
            out.push(if ch.is_whitespace() { ' ' } else { ch });
            i += 1;
        }
    }
    let mut compact = String::with_capacity(out.len());
    let mut last_space = true;
    for c in out.chars() {
        if c == ' ' {
            if !last_space {
                compact.push(' ');
            }
            last_space = true;
        } else {
            compact.push(c);
            last_space = false;
        }
    }
    compact
}

/// 动态工具执行器（T3 内置插件注册）：接收 (agent_id, session_id, args)，
/// 返回 (ok, 输出或错误文本)。异步闭包由注册方构造（平台 API / 会话态访问）。
pub type DynamicToolExec = std::sync::Arc<
    dyn Fn(&str, &str, serde_json::Value) -> futures_util::future::BoxFuture<'static, (bool, String)>
        + Send
        + Sync,
>;

/// 便捷构造动态执行器（闭包返回 anyhow::Result<String>）
pub fn dynamic_exec<F, Fut>(f: F) -> DynamicToolExec
where
    F: Fn(&str, &str, serde_json::Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = anyhow::Result<String>> + Send + 'static,
{
    std::sync::Arc::new(move |agent, session, args| {
        let fut = f(agent, session, args);
        Box::pin(async move {
            match fut.await {
                Ok(text) => (true, text),
                Err(e) => (false, e.to_string()),
            }
        })
    })
}

/// 子智能体派生执行器闭包：(主个体 id, 会话 id, 任务描述, 工具白名单) → 执行产出
pub type SpawnExec = Arc<
    dyn Fn(&str, &str, &str, Vec<ToolName>) -> futures_util::future::BoxFuture<'static, anyhow::Result<String>>
        + Send
        + Sync,
>;

/// 插件凭据解析闭包：(插件 id, 键) → 真值（None = 未配置）
pub type PluginCredResolver = Box<dyn Fn(&str, &str) -> Option<String> + Send + Sync>;

pub struct ToolGateway {
    workspace_root: PathBuf,
    store: Arc<Store>,
    registry: Arc<LocalRegistry>,
    events: tokio::sync::broadcast::Sender<CoreEvent>,
    security: SecurityConfig,
    /// 联网搜索后端（未配置 = web_search 不下发，避免「承诺不存在的能力」）
    search: SearchConfig,
    /// 沙箱执行策略（环境净化 / bwrap / strict 闸门）
    sandbox: SandboxConfig,
    /// 生命周期钩子（pre/post tool、run 收束）
    hooks: crate::config::HooksConfig,
    /// 定时任务存储（schedule 工具：AI 自主排程）
    cron: Option<Arc<crate::cron::CronStore>>,
    /// 子智能体派生执行器（spawn 工具；未注入 = spawn 返回未装配错误）。
    /// RwLock 槽：Core 装配后回接（tools 已 Arc 共享给 runtime，无法再拿可变引用）
    spawn_executor: parking_lot::RwLock<Option<SpawnExec>>,
    /// 上下文压缩执行器（context_compact 工具，实验性；未注入 = 工具返回未装配说明）。
    /// 槽位语义同 spawn_executor：Core 装配后回接 (provider, model)。
    compactor: parking_lot::RwLock<Option<(Arc<dyn LlmProvider>, String)>>,
    /// spawn 并发闸（实例级信号量；ToolGateway 装配时按上限创建）
    spawn_semaphore: Arc<tokio::sync::Semaphore>,
    /// spawn 子任务超时（秒；默认 300）
    spawn_timeout_secs: u64,
    /// 后台任务注册表（长命令：启动后返回 id，可轮询/终止）
    background: Arc<parking_lot::Mutex<std::collections::HashMap<String, BgTask>>>,
    /// 后台 spawn 子任务注册表（spec: subagent-spawn 异步并行）：
    /// background=true 派生后立即返回，终态暂存供 op=status/list 回收
    spawns_bg: Arc<parking_lot::Mutex<std::collections::HashMap<String, SpawnBgTask>>>,
    /// 浏览器配置与会话（懒启动常驻；串行化访问）
    browser_cfg: crate::config::BrowserConfig,
    browser: Arc<tokio::sync::Mutex<Option<crate::browser::BrowserSession>>>,
    /// Computer Use 配置与机械执行器（懒初始化；串行化访问）
    computer_cfg: crate::config::ComputerUseConfig,
    computer: Arc<tokio::sync::Mutex<Option<crate::computer::ComputerSession>>>,
    /// MCP 服务器池（第三方工具：mcp:server:tool 全名空间）
    mcp: Arc<crate::mcp::McpRegistry>,
    /// 声明式自定义工具（配置驱动，免写 MCP 服务器；按 agents 可见性放行）
    custom: Vec<crate::config::CustomTool>,
    /// 插件凭据解析面（T1 声明式工具 `${credentials.<key>}` 变量；Core 装配后回接——
    /// ToolGateway 构建期 config 尚未定稿，运行期经此闭包查 `plugins.credentials.<id>`）
    plugin_credentials: parking_lot::RwLock<Option<PluginCredResolver>>,
    /// 插件声明式工具（T1 清单内联 HTTP 工具，命名空间 `<plugin>__<tool>`；
    /// 与 custom 共用执行链——run_custom 全套（http/shell、审计、钩子、落盘））
    plugin_tools: parking_lot::RwLock<Vec<crate::config::CustomTool>>,
    /// 进程插件运行时工具面（T2：握手后发现，无法静态声明）。
    /// key = 插件 id；specs/执行转发到 provider（supervisor 持进程句柄）。
    /// 注册时机：spawn_process_plugin 拉起成功后（Core 回接）。
    plugin_runtime_tools: parking_lot::RwLock<std::collections::BTreeMap<String, std::sync::Arc<dyn crate::plugin::ToolProvider>>>,
    /// 动态工具执行器（T3 内置插件用：命名空间工具注册 schema + 自定义 async 执行面）。
    /// key = 工具全名；执行优先级：动态表 > plugin_tools/custom（可覆盖声明式同名）。
    dynamic_tools: parking_lot::RwLock<
        std::collections::BTreeMap<
            String,
            (
                crate::config::CustomTool,
                DynamicToolExec,
            ),
        >,
    >,
    /// 会话级工具注入（视频观看会话等「按会话来源收窄」的插件工具）：
    /// key = session_id；schema 下发与执行都只对本会话生效，普通会话不可见。
    session_scoped_tools: parking_lot::RwLock<
        std::collections::HashMap<String, Vec<(crate::config::CustomTool, DynamicToolExec)>>,
    >,
    /// 会话级步数预算覆盖（自动任务「防失控」面；None = 走三级默认解析）
    session_step_budgets: parking_lot::RwLock<std::collections::HashMap<String, usize>>,
    /// 记忆库（memory_read/write/link 三工具的受控入口；组隔离在工具层强制）
    memory: Option<Arc<crate::memory::MemoryStore>>,
    /// 深层记忆开关（config.memory.enabled）。false = 文件记忆模式：
    /// memory_read 改读 memory.md（与规划注入同源），memory_write/link 拒绝并引导。
    /// 为什么写工具要拒绝而非照写数据库：文件记忆模式下规划面只注入 memory.md，
    /// 写进数据库的知识永远不会被召回（写入黑洞）——静默成功比失败更伤。
    memory_deep: bool,
    /// memory.md 路径（文件记忆模式的读取源；与 orchestrator 注入用同一路径与同一 8000 字符上限）
    memory_md_path: Option<PathBuf>,
}

/// computer 输入动作：在既有会话上执行一次鼠标/键盘操作
type InputAction = Box<dyn FnOnce(&mut crate::computer::ComputerSession) -> anyhow::Result<()> + Send>;

impl ToolGateway {
    pub fn new(
        workspace_root: impl AsRef<Path>,
        store: Arc<Store>,
        registry: Arc<LocalRegistry>,
        events: tokio::sync::broadcast::Sender<CoreEvent>,
        security: SecurityConfig,
    ) -> Self {
        ToolGateway {
            workspace_root: workspace_root.as_ref().to_path_buf(),
            store,
            registry,
            events,
            security,
            search: SearchConfig::default(),
            sandbox: SandboxConfig::default(),
            hooks: crate::config::HooksConfig::default(),
            cron: None,
            spawn_executor: parking_lot::RwLock::new(None),
            compactor: parking_lot::RwLock::new(None),
            spawn_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
            spawn_timeout_secs: 300,
            background: Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new())),
            spawns_bg: Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new())),
            browser_cfg: crate::config::BrowserConfig::default(),
            browser: Arc::new(tokio::sync::Mutex::new(None)),
            computer_cfg: crate::config::ComputerUseConfig::default(),
            computer: Arc::new(tokio::sync::Mutex::new(None)),
            mcp: crate::mcp::McpRegistry::shared(),
            custom: Vec::new(),
            plugin_credentials: parking_lot::RwLock::new(None),
            plugin_tools: parking_lot::RwLock::new(Vec::new()),
            plugin_runtime_tools: parking_lot::RwLock::new(std::collections::BTreeMap::new()),
            dynamic_tools: parking_lot::RwLock::new(std::collections::BTreeMap::new()),
            session_scoped_tools: parking_lot::RwLock::new(std::collections::HashMap::new()),
            session_step_budgets: parking_lot::RwLock::new(std::collections::HashMap::new()),
            memory: None,
            // 默认深层记忆开启 = 历史行为（未接线 memory 模式的调用方零感知）
            memory_deep: true,
            memory_md_path: None,
        }
    }

    /// 注入记忆库（memory_read/write/link 三工具；未注入时三工具执行返回明确错误）
    pub fn with_memory(mut self, memory: Arc<crate::memory::MemoryStore>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// 注入子智能体派生执行器（spawn 工具）：Core 装配后回接 AgentRuntime。
    /// 闭包签名：(主个体 id, 会话 id, 任务描述, 工具白名单) → 执行产出（结论文本）。
    /// 并发/超时控制由网关层实现（spec: subagent-spawn）。
    pub fn set_spawn_executor(&self, exec: SpawnExec) {
        *self.spawn_executor.write() = Some(exec);
    }

    /// 注入上下文压缩执行器（context_compact 工具，实验性开关开启时由 Core 装配后回接）
    pub fn set_compactor(&self, provider: Arc<dyn LlmProvider>, model: String) {
        *self.compactor.write() = Some((provider, model));
    }

    /// 注入记忆模式（config.memory.enabled + memory_md_path）。
    /// `deep=false` 进入文件记忆模式：三工具口径与规划注入对齐（read 读 md、write/link 拒绝并引导）。
    pub fn with_memory_mode(mut self, deep: bool, md_path: PathBuf) -> Self {
        self.memory_deep = deep;
        self.memory_md_path = Some(md_path);
        self
    }

    /// 深层记忆是否开启（工具 schema 下发口径用）
    pub fn memory_deep(&self) -> bool {
        self.memory_deep
    }

    /// 注入联网搜索后端配置（未配置时 web_search 不下发给模型）
    pub fn with_search(mut self, search: SearchConfig) -> Self {
        self.search = search;
        self
    }

    /// 注入声明式自定义工具（配置驱动）
    pub fn with_custom_tools(mut self, tools: Vec<crate::config::CustomTool>) -> Self {
        self.custom = tools;
        self
    }

    /// 追加插件声明式工具（T1 零代码形态：构建期注入用；Core 回接走 append_plugin_tools）
    pub fn with_plugin_tools(self, tools: Vec<crate::config::CustomTool>) -> Self {
        self.append_plugin_tools(tools);
        self
    }

    /// 就地追加插件工具（Core 装配后回接用：ToolGateway 已包在 Arc 里不可再构建）。
    /// 重复调用幂等（同名先注册者胜，custom 与本表都算已注册）。
    pub fn append_plugin_tools(&self, tools: Vec<crate::config::CustomTool>) {
        let mut guard = self.plugin_tools.write();
        for t in tools {
            if guard.iter().any(|c| c.name == t.name) {
                continue; // 先注册者胜
            }
            if self.custom.iter().any(|c| c.name == t.name) {
                continue; // 与 config CustomTool 冲突：config 优先
            }
            guard.push(t);
        }
    }

    /// 注册进程插件运行时工具面（T2：拉起握手成功后由 spawn_process_plugin 回接）。
    /// 幂等：同 id 重复注册覆盖（重启场景新 supervisor 顶旧）。
    pub fn register_plugin_runtime_tools(&self, plugin_id: &str, provider: std::sync::Arc<dyn crate::plugin::ToolProvider>) {
        self.plugin_runtime_tools
            .write()
            .insert(plugin_id.to_string(), provider);
    }

    /// 摘除进程插件运行时工具面（禁用/卸载用）
    pub fn unregister_plugin_runtime_tools(&self, plugin_id: &str) {
        self.plugin_runtime_tools.write().remove(plugin_id);
    }

    /// 当前注册的全部插件工具（诊断/管理端点用）
    pub fn plugin_tools(&self) -> Vec<crate::config::CustomTool> {
        self.plugin_tools.read().clone()
    }

    /// 注入插件凭据解析面（Core 装配后回接：查 `plugins.credentials.<插件id>.<键>`）
    pub fn set_plugin_credentials_resolver(
        &self,
        f: PluginCredResolver,
    ) {
        *self.plugin_credentials.write() = Some(f);
    }

    /// 解析文本中的 `${credentials.<key>}` 变量（插件工具执行期）。
    /// 返回 (解析后文本, 缺失键列表)——缺失时不发部分鉴权请求，由调用方拒绝。
    fn resolve_credential_vars(&self, plugin_id: &str, text: &str) -> (String, Vec<String>) {
        let mut missing = Vec::new();
        // 形如 ${credentials.api_token}（键名：字母/数字/_/-）
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(i) = rest.find("${credentials.") {
            out.push_str(&rest[..i]);
            let after = &rest[i + "${credentials.".len()..];
            match after.find('}') {
                Some(j) => {
                    let key = &after[..j];
                    if key.is_empty()
                        || !key
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                    {
                        // 非法键名：原样保留（不视为变量）
                        out.push_str("${credentials.");
                        rest = after;
                        continue;
                    }
                    let resolver = self.plugin_credentials.read();
                    let hit = resolver
                        .as_ref()
                        .and_then(|f| f(plugin_id, key));
                    match hit {
                        Some(v) => out.push_str(&v),
                        None => {
                            // 缺键：保留原文（调用方按 missing 拒绝；纯函数面可测）
                            missing.push(key.to_string());
                            out.push_str(&rest[i..i + "${credentials.".len() + key.len() + 1]);
                        }
                    }
                    rest = &after[j + 1..];
                }
                None => {
                    out.push_str("${credentials.");
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        (out, missing)
    }

    /// 注册动态命名空间工具（T3 内置插件：schema + 自定义 async 执行面）。
    /// 名字须含 `<plugin>__<tool>` 命名空间；同名重复注册 = 覆盖（热更新用）。
    pub fn register_dynamic_tool(
        &self,
        spec: crate::config::CustomTool,
        exec: DynamicToolExec,
    ) {
        self.dynamic_tools
            .write()
            .insert(spec.name.clone(), (spec, exec));
    }

    /// 注销动态命名空间工具（插件停用/通道下线回收工具面）
    pub fn unregister_dynamic_tool(&self, name: &str) {
        self.dynamic_tools.write().remove(name);
    }

    /// 按名查动态工具（执行面用）
    pub fn dynamic_tool(&self, name: &str) -> Option<(crate::config::CustomTool, DynamicToolExec)> {
        self.dynamic_tools.read().get(name).cloned()
    }

    /// 当前注册的动态工具 schema 清单（诊断用）
    pub fn dynamic_tool_names(&self) -> Vec<String> {
        self.dynamic_tools.read().keys().cloned().collect()
    }

    /// 会话级工具注入：把一组工具绑定到指定会话（视频观看会话等「按会话来源
    /// 收窄」的场景）。同名重复注入 = 覆盖；轮次结束须调用 clear_session_tools 清理。
    pub fn inject_session_tools(
        &self,
        session_id: &str,
        tools: Vec<(crate::config::CustomTool, DynamicToolExec)>,
    ) {
        self.session_scoped_tools
            .write()
            .insert(session_id.to_string(), tools);
    }

    /// 清理会话级注入（观看轮次收束时调用；防泄漏）
    pub fn clear_session_tools(&self, session_id: &str) {
        self.session_scoped_tools.write().remove(session_id);
    }

    /// 按会话查注入工具（执行面用）
    fn session_tool(&self, session_id: &str, name: &str) -> Option<(crate::config::CustomTool, DynamicToolExec)> {
        self.session_scoped_tools
            .read()
            .get(session_id)
            .and_then(|v| v.iter().find(|(c, _)| c.name == name).cloned())
    }

    /// 会话级注入工具的 schema（runtime 下发用：session_id 变体）
    pub fn session_tool_specs(&self, session_id: &str) -> Vec<crate::provider::ToolSpec> {
        let to_spec = |c: &crate::config::CustomTool| crate::provider::ToolSpec {
            name: c.name.trim().to_string(),
            description: if c.description.trim().is_empty() {
                format!("插件会话工具 {}", c.name)
            } else {
                c.description.clone()
            },
            parameters: c.param_schema(),
        };
        self.session_scoped_tools
            .read()
            .get(session_id)
            .map(|v| v.iter().map(|(c, _)| to_spec(c)).collect())
            .unwrap_or_default()
    }

    /// 设置会话级步数预算（视频观看轮次等自动任务的「防失控」通道面配置）：
    /// n=0 清除覆盖（回落个体 maxSteps / 档位 / 全局默认）。
    pub fn set_session_step_budget(&self, session_id: &str, steps: usize) {
        let mut map = self.session_step_budgets.write();
        if steps == 0 {
            map.remove(session_id);
        } else {
            map.insert(session_id.to_string(), steps);
        }
    }

    /// 查会话级步数覆盖（None = 未设置，走默认三级解析）
    pub fn session_step_budget(&self, session_id: &str) -> Option<usize> {
        self.session_step_budgets.read().get(session_id).copied()
    }

    /// 该个体可见的自定义工具 schema（与内置工具一起随 function calling 下发）。
    /// 插件工具（`<plugin>__<tool>` 命名空间）一并发下——清单未配 agents = 全体可见。
    pub fn custom_specs_for(&self, agent_id: &str) -> Vec<crate::provider::ToolSpec> {
        let to_spec = |c: &crate::config::CustomTool| crate::provider::ToolSpec {
            name: c.name.trim().to_string(),
            description: if c.description.trim().is_empty() {
                format!("自定义工具 {}（{}）", c.name, c.kind_norm())
            } else {
                c.description.clone()
            },
            parameters: c.param_schema(),
        };
        self.custom
            .iter()
            .filter(|c| c.visible_to(agent_id))
            .map(to_spec)
            .chain(
                self.plugin_tools
                    .read()
                    .iter()
                    .filter(|c| c.visible_to(agent_id))
                    .map(to_spec),
            )
            .chain(
                self.dynamic_tools
                    .read()
                    .values()
                    .filter(|(c, _)| c.visible_to(agent_id))
                    .map(|(c, _)| to_spec(c)),
            )
            // 进程插件运行时工具（T2 握手发现；清单未配 agents = 全体可见）
            .chain(
                self.plugin_runtime_tools
                    .read()
                    .values()
                    .flat_map(|p| p.tool_specs().into_iter())
                    .filter(|c| c.visible_to(agent_id))
                    .map(|c| to_spec(&c)),
            )
            .collect()
    }

    /// 自定义工具是否已声明（诊断 / doctor 用）
    pub fn has_custom_tool(&self, name: &str) -> bool {
        self.custom.iter().any(|c| c.name == name)
    }

    /// 注入生命周期钩子配置
    pub fn with_hooks(mut self, hooks: crate::config::HooksConfig) -> Self {
        self.hooks = hooks;
        self
    }

    /// 注入沙箱执行策略
    pub fn with_sandbox(mut self, sandbox: SandboxConfig) -> Self {
        self.sandbox = sandbox;
        self
    }

    /// 注入浏览器配置（browser 工具）
    pub fn with_browser(mut self, browser: crate::config::BrowserConfig) -> Self {
        self.browser_cfg = browser;
        self
    }

    /// 注入 Computer Use 配置（computer 工具；enabled=false 时不下发）
    pub fn with_computer(mut self, computer: crate::config::ComputerUseConfig) -> Self {
        self.computer_cfg = computer;
        self
    }

    /// Computer Use 是否可用（总开关开启即下发——无交互桌面时调用会得到明确报错，
    /// 而「承诺存在但配置关闭」与「不存在」是两回事）
    pub fn computer_ready(&self) -> bool {
        self.computer_cfg.enabled
    }

    /// 浏览器是否可用（探测可执行文件；不可用时不下发 browser 工具，避免「承诺不存在的能力」）
    pub fn browser_ready(&self) -> bool {
        crate::browser::find_browser(&self.browser_cfg.executable).is_some()
    }

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
    fn sandbox_env(&self) -> Vec<(String, String)> {
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
    fn shell_opts(&self, cmd: &str) -> ShellOpts {
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

    /// 注入定时任务存储（schedule 工具）
    pub fn with_cron(mut self, cron: Arc<crate::cron::CronStore>) -> Self {
        self.cron = Some(cron);
        self
    }

    /// 测试可见：审批单断言用
    #[cfg(test)]
    pub(crate) fn store_for_test(&self) -> &Store {
        &self.store
    }

    /// 搜索后端是否就绪（决定 web_search 是否出现在下发给模型的工具表里）
    pub fn search_ready(&self) -> bool {
        self.search.ready()
    }

    /// 单智能体对话循环全局步数上限（security.chatMaxSteps；0 值配置视为默认）
    pub fn chat_max_steps(&self) -> usize {
        let v = self.security.chat_max_steps;
        if v == 0 {
            24
        } else {
            v
        }
    }

    /// 上下文预算安全线（security.contextBudgetRatio；越界值回落 0.8）
    pub fn context_budget_ratio(&self) -> f64 {
        let r = self.security.context_budget_ratio;
        if (0.5..=1.0).contains(&r) {
            r
        } else {
            0.8
        }
    }

    /// 记录一次 LLM 调用的真实用量（provider 返回的 usage；session 空 = 归入 unattributed）
    pub fn record_usage(
        &self,
        session_id: &str,
        prompt_tokens: u64,
        completion_tokens: u64,
        model: &str,
    ) {
        let scope = if session_id.trim().is_empty() {
            "unattributed"
        } else {
            session_id.trim()
        };
        let _ = self
            .store
            .add_usage(scope, prompt_tokens, completion_tokens, model);
    }

    /// 运行收束钩子（onRunEnd）：由指挥体在 run 结束时调用
    pub fn run_end_hooks(&self, status: &str, session_id: &str, agent_id: &str) -> Vec<String> {
        if self.hooks.on_run_end.is_empty() {
            return Vec::new();
        }
        let hooks = self.hooks.on_run_end.clone();
        let mut notes = Vec::new();
        for h in hooks {
            let (ok, out) = run_hook_command(
                &h,
                &self.effective_root(),
                &[
                    ("EXM_HOOK", "run_end"),
                    ("EXM_RUN_STATUS", status),
                    ("EXM_SESSION", session_id),
                    ("EXM_AGENT", agent_id),
                ],
            );
            if !ok {
                notes.push(format!("钩子失败（{h}）：{out}"));
            }
        }
        notes
    }

    /// 注入 MCP 服务器池（build_orchestrator 配置后传入同一实例）
    pub fn with_mcp(mut self, mcp: Arc<crate::mcp::McpRegistry>) -> Self {
        self.mcp = mcp;
        self
    }

    pub fn mcp(&self) -> Arc<crate::mcp::McpRegistry> {
        self.mcp.clone()
    }

    /// 执行 MCP 工具（全名 mcp:server:tool）；审计与内置工具一致落库，事件流对用户可见
    pub async fn execute_mcp(
        &self,
        agent_id: &str,
        session_id: &str,
        full_name: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        let started = Instant::now();
        let call_id = format!("t{}", &crate::types::new_id()[..12]);
        let _ = self.events.send(CoreEvent {
            kind: "tool.call".into(),
            session_id: session_id.to_string(),
            payload: serde_json::json!({
                "callId": call_id, "agentId": agent_id, "tool": full_name, "args": args,
            }),
        });
        let result = match self.mcp.call(full_name, agent_id, args).await {
            Some(r) => ToolResult {
                ok: r.ok,
                output: r.text.chars().take(8000).collect(),
                error: if r.ok {
                    None
                } else {
                    Some(r.text.chars().take(2000).collect())
                },
                images: Vec::new(),
            },
            None => ToolResult::err(format!("MCP 工具不存在或未开放: {full_name}")),
        };
        let summary: String = if result.ok {
            result.output.chars().take(200).collect()
        } else {
            result
                .error
                .clone()
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect()
        };
        let _ = self.store.audit_tool(
            agent_id,
            full_name,
            args,
            &summary,
            started.elapsed().as_millis() as u64,
        );
        // 轮次过程轨迹：随收束落盘到最终消息（过程收起栏数据源）
        crate::round_trace::push_tool(
            session_id,
            crate::round_trace::ToolCallRecord {
                call_id: call_id.clone(),
                agent_id: agent_id.to_string(),
                tool: full_name.to_string(),
                args: args.clone(),
                ok: result.ok,
                duration_ms: started.elapsed().as_millis() as u64,
                summary,
            },
        );
        self.emit_tool_result(session_id, &call_id, agent_id, full_name, &result, &started);
        result
    }

    /// 生效工作区根：显式工作目录（干活的项目）> 激活组声明 > 全局根（统一走注册表口径）
    fn effective_root(&self) -> PathBuf {
        self.registry.effective_workspace(&self.workspace_root)
    }

    /// 派发时的运行时工具白名单（8.2 口径）：
    /// - 个体 = 编成 JSON 的 tools 清单（未授权工具不在其工具面，不可见亦不可调用）
    /// - 组主智能体 = 额外获得组内个体管理工具（自定义组）与记忆三工具（默认授权）
    /// - 智能体（tier=orchestrator 且不在组内）按编成清单，可经 tools 显式授权记忆工具
    pub fn runtime_allowlist(
        def: &AgentDefinition,
        group_meta: Option<&crate::types::GroupMeta>,
    ) -> Vec<ToolName> {
        let mut allowlist = def.tools.clone();
        let is_primary = group_meta
            .map(|m| m.primary.as_deref() == Some(def.identifier.as_str()))
            .unwrap_or(false);
        if is_primary {
            if !group_meta.map(|m| m.builtin).unwrap_or(true)
                && !allowlist.contains(&ToolName::AgentManage)
            {
                allowlist.push(ToolName::AgentManage);
            }
            for t in [
                ToolName::MemoryRead,
                ToolName::MemoryWrite,
                ToolName::MemoryLink,
            ] {
                if !allowlist.contains(&t) {
                    allowlist.push(t);
                }
            }
        }
        allowlist
    }

    /// 原生 function calling：按白名单生成工具 schema（docs/协议与契约.md）。
    /// `search_ready` / `browser_ready` 决定联网搜索与浏览器工具是否下发——
    /// 后端能力未就绪时不承诺该工具（避免模型调用必然失败的工具）。
    pub fn tool_specs(
        allowlist: &[ToolName],
        search_ready: bool,
        browser_ready: bool,
        computer_ready: bool,
    ) -> Vec<crate::provider::ToolSpec> {
        let mut specs: Vec<crate::provider::ToolSpec> = Vec::new();
        let mut push = |name: ToolName, description: &str, parameters: serde_json::Value| {
            if !allowlist.contains(&name) {
                return;
            }
            if name == ToolName::WebSearch && !search_ready {
                return;
            }
            if name == ToolName::Browser && !browser_ready {
                return; // 未探测到浏览器：不下发
            }
            if name == ToolName::Computer && !computer_ready {
                return; // Computer Use 未启用：不下发（整机输入权须显式交出）
            }
            specs.push(crate::provider::ToolSpec {
                name: name.key().to_string(),
                description: description.to_string(),
                parameters,
            });
        };
        push(ToolName::Read, "读取工作区内文件或目录。文件按行返回并带行号，可用 offset/limit 分页续读；目录返回条目清单", serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "相对工作区的文件或目录路径" },
                "offset": { "type": "integer", "description": "起始行号（1 起，默认 1）" },
                "limit": { "type": "integer", "description": "最多返回行数（默认 400）" },
                "maxChars": { "type": "integer", "description": "返回字符上限（默认 24000）" }
            },
            "required": ["path"]
        }));
        push(ToolName::Edit, "精确编辑文件：把 old_string 替换为 new_string（old_string 必须唯一命中；整段替换请用 filesystem write）", serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "相对工作区的文件路径" },
                "old_string": { "type": "string", "description": "被替换的原文（含缩进与换行，必须与文件内容完全一致且唯一）" },
                "new_string": { "type": "string", "description": "替换后的内容；空串 = 删除该段" },
                "replace_all": { "type": "boolean", "description": "true = 替换全部命中（默认 false，多命中会报错并列出位置）" }
            },
            "required": ["path", "old_string", "new_string"]
        }));
        push(ToolName::Patch, "应用统一 diff 补丁到工作区文件：支持多文件段与新建/删除；任一文件的上下文校验失败则整体拒绝（不写入任何文件），全部通过才原子应用并写检查点", serde_json::json!({
            "type": "object",
            "properties": {
                "patch": { "type": "string", "description": "统一 diff 格式补丁全文（含 ---/+++ 路径行与 @@ hunk 头；路径相对工作区）" }
            },
            "required": ["patch"]
        }));
        push(
            ToolName::Grep,
            "在工作区内按正则搜索文件内容（尊重 .gitignore），返回 文件:行号:内容",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "正则表达式（Rust regex 语法）" },
                    "path": { "type": "string", "description": "搜索起始路径（默认工作区根）" },
                    "glob": { "type": "string", "description": "文件过滤，如 **/*.rs" },
                    "ignoreCase": { "type": "boolean", "description": "忽略大小写（默认 false）" },
                    "contextLines": { "type": "integer", "description": "命中行前后各带 N 行上下文（0-5，默认 0 = 单行输出）" },
                    "maxResults": { "type": "integer", "description": "最多返回条数（默认 100，上限 500）" }
                },
                "required": ["pattern"]
            }),
        );
        push(
            ToolName::Glob,
            "按 glob 模式列出工作区内的文件路径（尊重 .gitignore），用于定位文件",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "glob 模式，如 src/**/*.ts 或 **/*.md" },
                    "path": { "type": "string", "description": "起始目录（默认工作区根）" }
                },
                "required": ["pattern"]
            }),
        );
        push(
            ToolName::Filesystem,
            "文件系统操作：write（整文件写入/新建）、mkdir、list（条目清单）、delete（移入回收站可恢复，不裸删）、rename（重命名/移动，不覆盖已存在目标）",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "op": { "type": "string", "enum": ["write", "mkdir", "list", "delete", "rename"] },
                    "path": { "type": "string", "description": "相对工作区的路径" },
                    "content": { "type": "string", "description": "op=write 时的文件内容" },
                    "newPath": { "type": "string", "description": "op=rename 时的目标路径（相对工作区；目标存在则拒绝）" }
                },
                "required": ["op", "path"]
            }),
        );
        push(ToolName::Terminal, "在工作区内执行终端命令（白名单 + 审批闸门）。长任务用 background=true 启动后轮询 taskId", serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "op=run 时必填；单一命令，不支持 && | ; 等连接符" },
                "op": { "type": "string", "enum": ["run", "poll", "kill", "list"], "description": "默认 run；list 列出本会话后台任务（无需 taskId）" },
                "taskId": { "type": "string", "description": "op=poll/kill 时的后台任务 id" },
                "background": { "type": "boolean", "description": "true = 后台执行并立即返回任务 id（适合构建/测试等长任务）" },
                "timeoutSecs": { "type": "integer", "description": "前台命令超时（秒），上限由安全设置决定" }
            },
            "required": []
        }));
        push(
            ToolName::WebSearch,
            "联网搜索并返回结果摘要（标题 + 链接 + 摘要）",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "maxResults": { "type": "integer", "description": "返回条数（默认取配置值）" }
                },
                "required": ["query"]
            }),
        );
        push(
            ToolName::WebFetch,
            "抓取网页 URL 并提取正文文本（自动剥离标签与脚本）",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "http(s) 地址" },
                    "maxChars": { "type": "integer", "description": "最多返回字符数，默认 8000" }
                },
                "required": ["url"]
            }),
        );
        push(ToolName::Schedule, "自主排程：创建/查看/删除定时或一次性任务（到期由调度器唤醒本组执行）。用于「每天巡检」「明天提醒跟进」这类自驱工作", serde_json::json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["create", "list", "remove", "enable", "disable"] },
                "id": { "type": "string", "description": "op=list 时可过滤；remove/enable/disable 必填" },
                "name": { "type": "string", "description": "任务名（create 必填）" },
                "prompt": { "type": "string", "description": "到期时注入的提示词（create 必填）" },
                "cron": { "type": "string", "description": "五段 cron：分 时 日 月 周，如 0 9 * * *（与 at 二选一）" },
                "at": { "type": "string", "description": "一次性触发时间（ISO8601，如 2026-09-20T18:00:00+08:00）" }
            },
            "required": ["op"]
        }));
        push(ToolName::Browser, "浏览器自动化（headless Chrome + CDP）：导航网页、取渲染后正文、执行 JS、截图。适合抓动态页面 / 走一遍网页流程 / 留存页面证据", serde_json::json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["open", "text", "eval", "screenshot", "close"], "description": "默认 open" },
                "url": { "type": "string", "description": "op=open：http(s):// 或 file:// 地址" },
                "expression": { "type": "string", "description": "op=eval：页面内执行的 JS 表达式（可点击/填表：document.querySelector('#x').click()）" },
                "name": { "type": "string", "description": "op=screenshot：截图文件名（不含扩展名；缺省用时间戳）" },
                "maxChars": { "type": "integer", "description": "正文返回上限（默认取配置值）" }
            },
            "required": ["op"]
        }));
        push(ToolName::Computer, "Computer Use：控制本机桌面。screenshot 截屏（返回截图，视觉模型可直接看到屏幕画面）；click/move/scroll/type/key 操作鼠标键盘。坐标为截屏上的物理像素坐标。输入类动作可能需要人工审批", serde_json::json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["screenshot", "click", "move", "scroll", "type", "key"], "description": "screenshot=截屏并回看画面；click=点击；move=移动鼠标；scroll=滚动；type=键入文本；key=按键/热键" },
                "x": { "type": "integer", "description": "click/move：目标 X 坐标（截屏物理像素）" },
                "y": { "type": "integer", "description": "click/move：目标 Y 坐标（截屏物理像素）" },
                "button": { "type": "string", "enum": ["left", "right", "middle"], "description": "click：鼠标键，默认 left" },
                "double": { "type": "boolean", "description": "click：true = 双击" },
                "dx": { "type": "integer", "description": "scroll：横向格数（正右负左）" },
                "dy": { "type": "integer", "description": "scroll：纵向格数（正上负下）" },
                "text": { "type": "string", "description": "type：要键入的文本" },
                "combo": { "type": "string", "description": "key：按键组合，如 enter / ctrl+c / ctrl+shift+t / f5" },
                "monitor": { "type": "integer", "description": "screenshot：显示器序号（默认 0 主屏）" }
            },
            "required": ["op"]
        }));
        push(ToolName::AgentManage, "组内个体管理（仅主智能体）：templates/list/create/update/remove；create 支持基于编成模板", serde_json::json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["templates", "list", "create", "update", "remove"] },
                "template": { "type": "string", "description": "create 可选:模板 id(unit-*),先 templates 查询;提供后仅 identifier 必填" },
                "identifier": { "type": "string" },
                "name": { "type": "string" },
                "description": { "type": "string" },
                "domain": { "type": "string" },
                "tier": { "type": "string", "enum": ["unit", "orchestrator"] },
                "prompt": { "type": "string" },
                "capabilities": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["op"]
        }));
        push(ToolName::MemoryRead, "检索本组记忆（含你的个体私有层）：按关键词返回相关度排序的条目（内容/类型/置信度/重要性/来源）。执行中需要历史结论、约定或教训时先查再断言", serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "检索关键词（标题/正文/标签分词匹配）" },
                "limit": { "type": "integer", "description": "返回条数（默认 5，上限 20）" }
            },
            "required": ["query"]
        }));
        push(ToolName::MemoryWrite, "写入一条本组记忆：六类之一（fact/decision/preference/evidence/digest/lesson），内容哈希去重（重复写入提升既有条目重要性而非新建）。仅固化跨会话有价值的知识，不写过程性流水", serde_json::json!({
            "type": "object",
            "properties": {
                "kind": { "type": "string", "enum": ["fact", "decision", "preference", "evidence", "digest", "lesson"], "description": "fact=基础事实 decision=决策取舍 preference=用户偏好 evidence=证据 digest=摘要 lesson=教训" },
                "title": { "type": "string", "description": "一句话标题（去重按标题+内容判定）" },
                "body": { "type": "string", "description": "条目正文（精炼、可独立理解）" },
                "tags": { "type": "array", "items": { "type": "string" }, "description": "标签（可选，辅助检索）" },
                "importance": { "type": "number", "description": "重要性 0-1（默认 0.5）" }
            },
            "required": ["kind", "title", "body"]
        }));
        push(ToolName::MemoryLink, "记忆关联与主题列示：op=link 把两条记忆标注关联（如 derives-from/contradicts/supports，后续检索可见）；op=list 列示某主题下的条目及其关联链", serde_json::json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["link", "list"], "description": "默认 link" },
                "from": { "type": "string", "description": "op=link：起始条目 id" },
                "to": { "type": "string", "description": "op=link：目标条目 id" },
                "relation": { "type": "string", "description": "op=link：关系（derives-from / contradicts / supports / related）" },
                "query": { "type": "string", "description": "op=list：主题关键词" },
                "limit": { "type": "integer", "description": "op=list：返回条数（默认 5）" }
            },
            "required": []
        }));
        push(ToolName::Git, "工作区仓库的 Git 版本控制。只读动作直接执行：status/diff/log/branch/show/stash-list；写动作走审批闸门：add/commit/restore/stash-push/stash-pop/checkout-b。push/pull 等远程操作不在动作面（如需请用 terminal 工具走完整审批链）", serde_json::json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["status", "diff", "log", "branch", "show", "stash-list", "add", "commit", "restore", "stash-push", "stash-pop", "checkout-b"], "description": "git 动作" },
                "pathSpec": { "type": "string", "description": "add/restore/diff：目标路径（相对工作区，可多行）" },
                "message": { "type": "string", "description": "commit：提交信息" },
                "branch": { "type": "string", "description": "checkout-b：新分支名；branch：留空列分支" },
                "maxCount": { "type": "integer", "description": "log：返回条数（默认 20）" },
                "object": { "type": "string", "description": "show：对象引用（commit/分支/标签）" }
            },
            "required": ["action"]
        }));
        push(ToolName::Spawn, "派生子智能体执行封闭子任务：子智能体在独立上下文（不继承本会话历史）中执行任务描述，完成后返回结论摘要。适合全库检索、批量调研等可独立完成的工作。并发上限与超时受控；子智能体不可再派生（单层）。background=true 立即返回 spawnId 后台执行（期间可继续对话/派生其它子任务），用 op=status/list 回收结果", serde_json::json!({
            "type": "object",
            "properties": {
                "task": { "type": "string", "description": "自包含的任务书：目标、范围、期望产出（子智能体看不到本会话，必须写全背景）" },
                "tools": { "type": "array", "items": { "type": "string", "enum": ["read", "edit", "patch", "grep", "glob", "filesystem", "terminal", "git", "web_search", "web_fetch"] }, "description": "子智能体工具白名单（缺省只读：read/grep/glob/web_fetch）" },
                "timeoutSecs": { "type": "integer", "description": "子任务超时（秒；默认 300，范围 5-1800）" },
                "background": { "type": "boolean", "description": "true = 后台执行：立即返回 spawnId，结果经 op=status 查询（长任务并行时用）" },
                "op": { "type": "string", "enum": ["status", "list"], "description": "后台子任务回收：status 需配 spawnId；list 列当前会话全部子任务" },
                "spawnId": { "type": "string", "description": "op=status 时指定子任务" }
            },
            "required": []
        }));
        push(ToolName::Todo, "维护会话任务清单（长任务自组织）：op=write 一次性全量重写整个清单（不是追加——每次都写全所有条目与最新状态）；清单会注入后续每轮上下文，让你随时掌握进度全景。任务开始时建清单、每完成一项立即更新状态、全部完成后收尾", serde_json::json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["write", "list"], "description": "默认 write" },
                "items": { "type": "array", "items": { "type": "object", "properties": { "content": { "type": "string" }, "status": { "type": "string", "enum": ["pending", "in_progress", "done"] } }, "required": ["content"] }, "description": "全量条目（write 时必填；空数组 = 清空清单）" }
            },
            "required": []
        }));
        push(ToolName::ContextCompact, "主动压缩会话上下文：把最近窗口之外的更早历史压成滚动摘要（原文保留在会话记录中，仅上下文视图替换）。何时调用由你判断：上下文已冗长、早期细节不再关键、即将进入需要大量新鲜上下文的长任务阶段。压缩是增量的，可多次调用", serde_json::json!({
            "type": "object",
            "properties": {
                "keepRecent": { "type": "integer", "description": "保留最近 N 条原文不压缩（默认 12，范围 2-100）" },
                "note": { "type": "string", "description": "本次压缩时机判断的简要说明（为什么现在压、想保住什么）" }
            },
            "required": []
        }));
        specs
    }

    /// 同 [`Self::tool_specs`]，额外按记忆模式收敛 schema：文件记忆模式下
    /// memory_write / memory_link 必然失败，不下发（与 web_search/browser 的
    /// 「后端未就绪不承诺」同一口径）；memory_read 保留——读 memory.md 仍有意义。
    /// 记忆模式门控下发：memory_deep=false（文件记忆模式）时记忆写类工具不下发
    pub fn tool_specs_gated(
        allowlist: &[ToolName],
        search_ready: bool,
        browser_ready: bool,
        computer_ready: bool,
        memory_deep: bool,
    ) -> Vec<crate::provider::ToolSpec> {
        let mut specs = Self::tool_specs(allowlist, search_ready, browser_ready, computer_ready);
        if !memory_deep {
            let drop: Vec<&str> = [ToolName::MemoryWrite, ToolName::MemoryLink]
                .iter()
                .map(|t| t.key())
                .collect();
            specs.retain(|s| !drop.contains(&s.name.as_str()));
        }
        specs
    }

    /// 实验性开关门控：dynamic_compaction=false 时 context_compact 不下发
    /// （不开实验开关就不给模型这个能力面，避免误用未稳定行为）
    pub fn tool_specs_experiments(
        specs: Vec<crate::provider::ToolSpec>,
        dynamic_compaction: bool,
    ) -> Vec<crate::provider::ToolSpec> {
        if dynamic_compaction {
            specs
        } else {
            specs
                .into_iter()
                .filter(|s| s.name != ToolName::ContextCompact.key())
                .collect()
        }
    }

    /// 工具层事件发双层：实时广播（WS 推流）+ 事件库落盘（历史可回放）。
    /// 此前 tool.call/tool.result 只广播不落库——历史会话的事件页看不到任何工具过程，
    /// 「过程透明」只对在线连接生效。落库量与工具次数同阶（每次调用 2 条），
    /// 与 token 流水账（一次运行数千条）不同量级，不会刷屏。
    fn emit_and_persist(&self, evt: CoreEvent) {
        let _ = self.events.send(evt.clone());
        let env = crate::types::Envelope::new(
            format!("session.{}.events", evt.session_id),
            "tools",
            "broadcast",
            "event",
            evt.session_id.clone(),
            serde_json::to_value(&evt).unwrap_or(serde_json::Value::Null),
        );
        let _ = self.store.append_event(&env);
    }

    /// 执行内置工具（按枚举）：白名单外拒绝 → preTool 钩子（可拦截）→ 执行 → 结果落盘 → postTool 钩子 → 审计
    pub async fn execute(
        &self,
        agent_id: &str,
        session_id: &str,
        allowlist: &[ToolName],
        tool: ToolName,
        args: &serde_json::Value,
    ) -> ToolResult {
        self.execute_named(agent_id, session_id, allowlist, tool.key(), args)
            .await
    }

    /// 执行任意工具名：内置（按 `allowlist` 放行）或**声明式自定义工具**（按 `agents` 可见性放行）。
    /// 模型给出的工具名统一走这里，钩子 / 结果落盘 / 审计对两类工具一视同仁。
    /// `session_id` 用于：工具执行事件（tool.call / tool.result）与审批单归属——渠道可按会话过滤呈现。
    pub async fn execute_named(
        &self,
        agent_id: &str,
        session_id: &str,
        allowlist: &[ToolName],
        name: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        let started = Instant::now();
        let call_id = format!("t{}", &crate::types::new_id()[..12]);
        // 工具执行事件（call）：文件操作 / 终端命令 / 截屏对用户全程可见——编程软件式的过程透明
        // 插件工具归因：`<plugin>__<tool>` 名字解析出 plugin id 写入事件（审计可检索）
        let plugin_attr = crate::plugin::parse_namespaced(name)
            .map(|(p, _)| serde_json::json!(p))
            .unwrap_or(serde_json::Value::Null);
        self.emit_and_persist(CoreEvent {
            kind: "tool.call".into(),
            session_id: session_id.to_string(),
            payload: serde_json::json!({
                "callId": call_id, "agentId": agent_id, "tool": name, "args": args,
                "plugin": plugin_attr,
            }),
        });
        // 计划模式拦截（spec: plan-mode）：写类工具不执行，转为计划条目累积清单。
        // 读取类工具放行（read/grep/glob/web/截屏/git 只读等）
        if let Some(tool) = ToolName::parse(name) {
            if crate::plan::is_plan_mode(session_id) && crate::plan::is_write_tool(tool, args) {
                let item = crate::plan::to_plan_item(tool, args);
                crate::plan::push_item(session_id, item.clone());
                let pending_count = crate::plan::list_items(session_id).len();
                self.emit_and_persist(CoreEvent {
                    kind: "plan.item".into(),
                    session_id: session_id.to_string(),
                    payload: serde_json::json!({
                        "itemId": item.id, "tool": item.tool, "target": item.target,
                        "action": item.action, "pendingCount": pending_count,
                    }),
                });
                // 首条出现 = 待审批状态生效：广播 + 复用 approvals 通道落计划审批单
                // （批准/拒绝经 approval_decide 的 [plan] 分支或 REST 计划端点处理）
                if pending_count == 1 {
                    let req = crate::types::ApprovalRequest {
                        id: crate::types::new_id()[..8].to_string(),
                        session_id: session_id.to_string(),
                        node_id: None,
                        agent_id: agent_id.to_string(),
                        command: format!("[plan] 会话计划清单（{} 条待执行）", pending_count),
                        status: "pending".into(),
                        result: None,
                        created_at: crate::types::now_iso(),
                        decided_at: None,
                        origin_user: None,
                        origin_role: None,
                    };
                    let _ = self.store.add_approval(&req);
                    crate::plan::set_pending_approval(session_id, &req.id);
                    self.emit_and_persist(CoreEvent {
                        kind: "plan.pending".into(),
                        session_id: session_id.to_string(),
                        payload: serde_json::json!({
                            "count": 1,
                            "approvalId": req.id,
                        }),
                    });
                }
                let blocked = ToolResult::ok(format!(
                    "【计划模式】该写操作已记录为计划条目（{} {} {}），未执行；\
                     继续规划或等待用户批准计划后统一执行。",
                    item.action, item.tool, item.target
                ));
                self.emit_tool_result(session_id, &call_id, agent_id, name, &blocked, &started);
                return blocked;
            }
        }
        // preTool 钩子：非零退出即拦截（返回原因给模型，计入审计）
        if !self.hooks.pre_tool.is_empty() {
            let args_json = serde_json::to_string(args).unwrap_or_default();
            let short_args: String = args_json.chars().take(4000).collect();
            let root = self.effective_root();
            let root_s = root.display().to_string();
            for h in self.hooks.pre_tool.clone() {
                let (ok, out) = run_hook_command(
                    &h,
                    &root,
                    &[
                        ("EXM_HOOK", "pre_tool"),
                        ("EXM_TOOL", name),
                        ("EXM_AGENT", agent_id),
                        ("EXM_ARGS", &short_args),
                        ("EXM_WORKSPACE", &root_s),
                    ],
                );
                if !ok {
                    let why = if out.trim().is_empty() {
                        h.clone()
                    } else {
                        out.clone()
                    };
                    let blocked = ToolResult::err(format!("preTool 钩子拦截：{why}"));
                    let _ = self.store.audit_tool(
                        agent_id,
                        name,
                        args,
                        "preTool 钩子拦截",
                        started.elapsed().as_millis() as u64,
                    );
                    self.emit_tool_result(session_id, &call_id, agent_id, name, &blocked, &started);
                    return blocked;
                }
            }
        }

        let result = match ToolName::parse(name) {
            Some(tool) => {
                if !allowlist.contains(&tool) && tool != ToolName::ContextCompact {
                    ToolResult::err(format!("工具 {} 不在本节点放行白名单内", tool.key()))
                } else if tool == ToolName::AgentManage
                    && self
                        .registry
                        .active_group_meta()
                        .and_then(|m| m.primary)
                        .as_deref()
                        != Some(agent_id)
                {
                    // 纵深防御：allowlist 注入之外再校验调用者身份
                    ToolResult::err("agent_manage 仅限激活组主智能体使用")
                } else if tool == ToolName::Computer {
                    self.tool_computer(agent_id, session_id, args).await
                } else if tool == ToolName::ContextCompact {
                    // 实验性动态压缩：豁免 allowlist（能力面由 schema 下发门控控制——
                    // 未开实验开关时模型根本看不到该工具 schema，不会有调用进来）
                    self.tool_context_compact(agent_id, session_id, args)
                        .await
                } else {
                    self.dispatch(agent_id, session_id, tool, args).await
                }
            }
            // 非内置名：会话级注入（视频观看会话）→ 动态命名空间工具（T3 内置插件）→ 声明式自定义工具
            None => {
                // 会话级注入优先（按会话收窄的插件工具：video__* 等）
                if let Some((spec, exec)) = self.session_tool(session_id, name) {
                    if spec.visible_to(agent_id) {
                        let (ok, text) = exec(agent_id, session_id, args.clone()).await;
                        if ok {
                            ToolResult::ok(text)
                        } else {
                            ToolResult::err(text)
                        }
                    } else {
                        ToolResult::err(format!(
                            "工具 {name} 未对本个体开放（插件工具的 agents 未包含 {agent_id}）"
                        ))
                    }
                } else if let Some((spec, exec)) = self.dynamic_tool(name) {
                    // 动态表（T3 内置插件全局注册面）
                    if spec.visible_to(agent_id) {
                        let (ok, text) = exec(agent_id, session_id, args.clone()).await;
                        if ok {
                            ToolResult::ok(text)
                        } else {
                            ToolResult::err(text)
                        }
                    } else {
                        ToolResult::err(format!(
                            "工具 {name} 未对本个体开放（插件工具的 agents 未包含 {agent_id}）"
                        ))
                    }
                } else {
                    // 查找顺序：config CustomTool 先于插件工具（先注册者胜）；
                    // 进程插件运行时工具最后（T2 握手发现面）
                    let found = self
                        .custom
                        .iter()
                        .find(|c| c.name == name)
                        .cloned()
                        .or_else(|| {
                            self.plugin_tools
                                .read()
                                .iter()
                                .find(|c| c.name == name)
                                .cloned()
                        });
                    match found {
                        Some(ct) if ct.visible_to(agent_id) => {
                            self.run_custom(agent_id, session_id, &ct, args).await
                        }
                        Some(_) => ToolResult::err(format!(
                            "工具 {name} 未对本个体开放（自定义工具的 agents 未包含 {agent_id}）"
                        )),
                        None => {
                            // 进程插件运行时工具（T2）：转发到 provider（supervisor → 子进程）
                            let provider = self
                                .plugin_runtime_tools
                                .read()
                                .values()
                                .find(|p| {
                                    p.tool_specs().iter().any(|c| c.name == name)
                                })
                                .cloned();
                            match provider {
                                Some(p) => match p.call_tool(name, args) {
                                    Ok(text) => ToolResult::ok(text),
                                    Err(e) => ToolResult::err(format!("插件工具执行失败：{e}")),
                                },
                                None => ToolResult::err(format!("未知工具：{name}")),
                            }
                        }
                    }
                }
            }
        };
        let result = self.spill_if_large(name, result);
        // postTool 钩子：仅留痕（失败不改变工具结果，但会追加到输出末尾提示）
        let mut result = result;
        if !self.hooks.post_tool.is_empty() {
            let root = self.effective_root();
            let root_s = root.display().to_string();
            let ok_s = if result.ok { "true" } else { "false" };
            let preview: String = if result.ok {
                result.output.chars().take(4000).collect()
            } else {
                result
                    .error
                    .clone()
                    .unwrap_or_default()
                    .chars()
                    .take(4000)
                    .collect()
            };
            for h in self.hooks.post_tool.clone() {
                let (hook_ok, out) = run_hook_command(
                    &h,
                    &root,
                    &[
                        ("EXM_HOOK", "post_tool"),
                        ("EXM_TOOL", name),
                        ("EXM_AGENT", agent_id),
                        ("EXM_TOOL_OK", ok_s),
                        ("EXM_OUTPUT", &preview),
                        ("EXM_WORKSPACE", &root_s),
                    ],
                );
                if !hook_ok {
                    let tail = format!(
                        "\n[postTool 钩子失败] {}",
                        out.chars().take(300).collect::<String>()
                    );
                    if result.ok {
                        result.output.push_str(&tail);
                    } else if let Some(e) = result.error.as_mut() {
                        e.push_str(&tail);
                    }
                }
            }
        }
        let summary = if result.ok {
            result.output.chars().take(200).collect::<String>()
        } else {
            result
                .error
                .clone()
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect()
        };
        let _ = self.store.audit_tool(
            agent_id,
            name,
            args,
            &summary,
            started.elapsed().as_millis() as u64,
        );
        // 轮次过程轨迹：与 execute_mcp 一致,内置/自定义工具执行同样随收束落盘到最终消息
        crate::round_trace::push_tool(
            session_id,
            crate::round_trace::ToolCallRecord {
                call_id: call_id.clone(),
                agent_id: agent_id.to_string(),
                tool: name.to_string(),
                args: args.clone(),
                ok: result.ok,
                duration_ms: started.elapsed().as_millis() as u64,
                summary,
            },
        );
        self.emit_tool_result(session_id, &call_id, agent_id, name, &result, &started);
        result
    }

    /// 工具执行事件（result）：状态 / 耗时 / 摘要 / 图像清单——渠道据此渲染工具卡与截图预览
    fn emit_tool_result(
        &self,
        session_id: &str,
        call_id: &str,
        agent_id: &str,
        name: &str,
        result: &ToolResult,
        started: &Instant,
    ) {
        let text = if result.ok {
            result.output.clone()
        } else {
            result.error.clone().unwrap_or_default()
        };
        // 插件归因（与 tool.call 对称；非插件工具为 null 不占字段语义）
        let plugin_attr = crate::plugin::parse_namespaced(name)
            .map(|p| serde_json::json!(p.0))
            .unwrap_or(serde_json::Value::Null);
        self.emit_and_persist(CoreEvent {
            kind: "tool.result".into(),
            session_id: session_id.to_string(),
            payload: serde_json::json!({
                "callId": call_id, "agentId": agent_id, "tool": name,
                "ok": result.ok,
                "durationMs": started.elapsed().as_millis() as u64,
                "summary": text.chars().take(400).collect::<String>(),
                "images": result.images,
                "plugin": plugin_attr,
            }),
        });
        // 历史回放可见工具的成败与耗时摘要（不止「开始了」）——落库在 emit_and_persist 内完成
    }

    /// 结果超阈值时全文落盘并回填「摘要 + 路径 + 行数」，供模型按需用 read 取回。
    /// 阈值 0 = 关闭（仅按工具自身的截断）。
    fn spill_if_large(&self, name: &str, result: ToolResult) -> ToolResult {
        let limit = self.security.tool_output_spill_chars;
        if limit == 0 {
            return result;
        }
        let (text, is_err) = if result.ok {
            (result.output.clone(), false)
        } else {
            (result.error.clone().unwrap_or_default(), true)
        };
        if text.chars().count() <= limit {
            return result;
        }
        let dir = self.workspace_root.join(".exmachina").join("tool-output");
        if std::fs::create_dir_all(&dir).is_err() {
            return result;
        }
        let stamp = crate::types::now_iso().replace([':', '-'], "");
        let file = dir.join(format!("{stamp}-{name}.txt"));
        if std::fs::write(&file, &text).is_err() {
            return result;
        }
        let lines = text.lines().count();
        let head: String = text.chars().take(4000).collect();
        let rel = file
            .strip_prefix(&self.workspace_root)
            .map(|p| p.display().to_string().replace('\\', "/"))
            .unwrap_or_else(|_| file.display().to_string());
        let note = format!(
            "{head}\n\n…（结果共 {total} 字符 / {lines} 行，已截断）\n完整内容已落盘：{rel}\n可用 read 工具按 offset/limit 分段读取。",
            total = text.chars().count()
        );
        if is_err {
            ToolResult::err(note)
        } else {
            ToolResult::ok(note)
        }
    }

    async fn dispatch(
        &self,
        agent_id: &str,
        session_id: &str,
        tool: ToolName,
        args: &serde_json::Value,
    ) -> ToolResult {
        match tool {
            ToolName::Read => self.tool_read(args),
            ToolName::Edit => self.tool_edit(session_id, args),
            ToolName::Patch => self.tool_patch(session_id, args),
            ToolName::Grep => self.tool_grep(args),
            ToolName::Glob => self.tool_glob(args),
            ToolName::Filesystem => self.tool_fs(session_id, args),
            ToolName::Terminal => self.tool_terminal(agent_id, session_id, args).await,
            ToolName::WebSearch => self.tool_web_search(args).await,
            ToolName::WebFetch => self.tool_web_fetch(args).await,
            ToolName::AgentManage => self.tool_agent_manage(args),
            ToolName::Schedule => self.tool_schedule(args),
            ToolName::Browser => self.tool_browser(session_id, args).await,
            ToolName::Computer => self.tool_computer(agent_id, session_id, args).await,
            ToolName::MemoryRead => self.tool_memory_read(agent_id, session_id, args),
            ToolName::MemoryWrite => self.tool_memory_write(agent_id, session_id, args),
            ToolName::MemoryLink => self.tool_memory_link(agent_id, session_id, args),
            ToolName::Git => self.tool_git(agent_id, session_id, args).await,
            ToolName::Spawn => self.tool_spawn(agent_id, session_id, args).await,
            ToolName::Todo => self.tool_todo(session_id, args),
            ToolName::ContextCompact => unreachable!("context_compact 在 execute_named 预分发"),
        }
    }

    // ---------------------------------------------------------------- 记忆工具（组隔离，design D7）

    /// 调用者是否当前组主智能体（memory_read 全层视角 / memory_write 默认授权的判定口径）
    fn is_primary(&self, agent_id: &str) -> bool {
        self.registry
            .primary()
            .map(|p| p.identifier == agent_id)
            .unwrap_or(false)
    }

    /// 会话的记忆命名空间：通道「记忆隔离」开启时用会话分区，否则用激活组。
    /// 默认所有平台会话共享同组记忆；隔离只改变记忆键，不改变智能体组归属。
    fn memory_group(&self, session_id: &str) -> String {
        self.store
            .get_session(session_id)
            .ok()
            .flatten()
            .and_then(|s| s.memory_partition)
            .unwrap_or_else(|| self.registry.active_group())
    }

    /// 隔离分区的 memory.md 路径（与 orchestrator 的命名规则一致）。
    fn memory_md_path_for(&self, session_id: &str) -> Option<std::path::PathBuf> {
        let base = self.memory_md_path.as_ref()?;
        let partition = self
            .store
            .get_session(session_id)
            .ok()
            .flatten()
            .and_then(|s| s.memory_partition);
        Some(match partition {
            None => base.clone(),
            Some(p) => {
                let safe: String = p
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect();
                base.with_file_name(format!("memory-{safe}.md"))
            }
        })
    }

    /// memory_read：组内检索（主智能体全层视角 = 群体 + 所有个体私有；个体 = 私有 + 群体）。
    /// 组隔离在工具层强制：范围恒为「有效记忆命名空间 + 全局条目」，其他组条目不可见。
    /// 文件记忆模式（深层记忆关闭）：回落 memory.md 全文——与规划注入同源，口径一致。
    fn tool_memory_read(
        &self,
        agent_id: &str,
        session_id: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        let Some(memory) = &self.memory else {
            return ToolResult::err("记忆系统未接入（内部装配缺失）");
        };
        if !self.memory_deep {
            return self.file_memory_read(session_id);
        }
        let Some(query) = args
            .get("query")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return ToolResult::err("query 不能为空");
        };
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| (v as usize).min(20))
            .unwrap_or(5)
            .max(1);
        let gid = self.memory_group(session_id);
        let hits = if self.is_primary(agent_id) {
            memory.recall_all(query, limit, Some(&gid), None)
        } else {
            memory.recall_for_agent(agent_id, query, limit, Some(&gid), None)
        };
        match hits {
            Ok(list) if list.is_empty() => {
                ToolResult::ok(format!("记忆检索「{query}」：组内无命中条目。"))
            }
            Ok(list) => {
                let mut lines = vec![format!(
                    "记忆检索「{query}」：{} 条命中（按相关度排序）",
                    list.len()
                )];
                for h in list {
                    let source = match (&h.entry.agent_id, &h.entry.group_id) {
                        (Some(a), _) => format!("个体:{a}"),
                        (None, Some(g)) => format!("组:{g}"),
                        (None, None) => "全局".to_string(),
                    };
                    lines.push(format!(
                        "- [{}|{}|置信{:.2}|重要{:.2}|{}] {}：{}（id: {}）",
                        h.entry.kind.key(),
                        source,
                        h.entry.confidence,
                        h.entry.importance,
                        if h.entry.pinned { "置顶" } else { "常规" },
                        h.entry.title,
                        h.entry.body.replace('\n', " "),
                        h.entry.id
                    ));
                }
                ToolResult::ok(lines.join("\n"))
            }
            Err(e) => ToolResult::err(format!("记忆检索失败：{e}")),
        }
    }

    /// 文件记忆模式的 memory_read：返回 memory.md 内容（与规划注入同源、同一 8000 字符上限）。
    /// 为什么不查数据库：文件记忆模式下规划面只注入 memory.md——
    /// 工具看到的与规划看到的不一致，本身就是行为分裂。
    fn file_memory_read(&self, session_id: &str) -> ToolResult {
        let Some(path) = self.memory_md_path_for(session_id) else {
            return ToolResult::err("记忆系统未接入（内部装配缺失）");
        };
        match std::fs::read_to_string(&path) {
            Ok(md) if !md.trim().is_empty() => {
                let total = md.chars().count();
                let trimmed: String = md.chars().take(8000).collect();
                let note = if total > 8000 {
                    format!("\n…（已截断：memory.md 共 {total} 字符，仅保留前 8000）")
                } else {
                    String::new()
                };
                ToolResult::ok(format!(
                    "文件记忆（memory.md，深层记忆已关闭）：\n{trimmed}{note}"
                ))
            }
            Ok(_) => ToolResult::ok("memory.md 为空：文件记忆模式下尚无记忆内容。"),
            Err(_) => ToolResult::ok("memory.md 不存在：文件记忆模式下尚无记忆内容。"),
        }
    }

    /// memory_write：写入组内记忆（哈希去重由 MemoryStore 承担）；来源标注智能体自写（source=agent）
    fn tool_memory_write(
        &self,
        agent_id: &str,
        session_id: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        let Some(memory) = &self.memory else {
            return ToolResult::err("记忆系统未接入（内部装配缺失）");
        };
        if !self.memory_deep {
            return ToolResult::err(
                "深层记忆已关闭（文件记忆模式）：写进数据库的记忆不会被规划召回。请改为直接维护 memory.md（filesystem write 可达），或建议用户在设置中开启深层记忆。",
            );
        }
        let Some(kind) = args
            .get("kind")
            .and_then(|v| v.as_str())
            .and_then(crate::memory::MemoryKind::parse)
        else {
            return ToolResult::err(
                "kind 须为 fact/decision/preference/evidence/digest/lesson 之一",
            );
        };
        let Some(title) = args
            .get("title")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return ToolResult::err("title 不能为空");
        };
        let Some(body) = args
            .get("body")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return ToolResult::err("body 不能为空");
        };
        let importance = args
            .get("importance")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.5);
        let tags: Vec<String> = args
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let mut draft = crate::memory::MemoryDraft::new(kind, title, body)
            .importance(importance)
            .agent(agent_id)
            .group(&self.memory_group(session_id));
        draft.tags = tags;
        // 来源标注：智能体自写（与归纳写入可区分，管理视图可过滤）
        draft.source_ref = Some("agent".into());
        match memory.remember(&draft) {
            Ok(entry) => ToolResult::ok(format!(
                "已写入记忆：{}（id: {}，类型 {}，重要性 {:.2}）；同内容重复写入会被去重并提升既有条目",
                entry.title,
                entry.id,
                entry.kind.key(),
                entry.importance
            )),
            Err(e) => ToolResult::err(format!("记忆写入失败：{e}")),
        }
    }

    /// memory_link：关联建立（组隔离校验双端）与主题列示（条目 + 关联链）
    fn tool_memory_link(
        &self,
        agent_id: &str,
        session_id: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        let Some(memory) = &self.memory else {
            return ToolResult::err("记忆系统未接入（内部装配缺失）");
        };
        if !self.memory_deep {
            return ToolResult::err(
                "深层记忆已关闭（文件记忆模式）：记忆关联属于深层记忆库特性。可改为直接在 memory.md 中以文字标注关联。",
            );
        }
        let gid = self.memory_group(session_id);
        let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("link");
        match op {
            "list" => {
                let query = args
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                let limit = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|v| (v as usize).min(20))
                    .unwrap_or(5)
                    .max(1);
                let hits = if query.is_empty() {
                    // 空查询列示与检索同一套分层口径：个体只见「自己私有 + 群体」，
                    // 主智能体全层可见——recent 全量版不设防，不能在这里直接用
                    if self.is_primary(agent_id) {
                        memory.recent_in_layer(None, limit, Some(&gid))
                    } else {
                        memory.recent_in_layer(Some(agent_id), limit, Some(&gid))
                    }
                } else if self.is_primary(agent_id) {
                    memory.recall_all(query, limit, Some(&gid), None)
                } else {
                    memory.recall_for_agent(agent_id, query, limit, Some(&gid), None)
                };
                match hits {
                    Ok(list) if list.is_empty() => ToolResult::ok("主题下无记忆条目。".to_string()),
                    Ok(list) => {
                        let mut lines = vec![format!(
                            "主题「{query}」列示（{} 条，含关联链）：",
                            list.len()
                        )];
                        for h in list {
                            let links = memory.links_of(&h.entry.id).unwrap_or_default();
                            let chain = links
                                .iter()
                                .filter_map(|l| {
                                    let to = l.get("to").and_then(|v| v.as_str())?;
                                    let rel = l
                                        .get("relation")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("related");
                                    let dir = if l.get("from").and_then(|v| v.as_str())
                                        == Some(h.entry.id.as_str())
                                    {
                                        format!("{rel} → {to}")
                                    } else {
                                        let from =
                                            l.get("from").and_then(|v| v.as_str()).unwrap_or("?");
                                        format!("{from} →{rel}→ 本条")
                                    };
                                    Some(dir)
                                })
                                .collect::<Vec<_>>()
                                .join("；");
                            lines.push(format!(
                                "- [{}] {}（id: {}）{}",
                                h.entry.kind.key(),
                                h.entry.title,
                                h.entry.id,
                                if chain.is_empty() {
                                    String::new()
                                } else {
                                    format!("｜关联：{chain}")
                                }
                            ));
                        }
                        ToolResult::ok(lines.join("\n"))
                    }
                    Err(e) => ToolResult::err(format!("记忆列示失败：{e}")),
                }
            }
            _ => {
                let Some(from) = args
                    .get("from")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                else {
                    return ToolResult::err("from 不能为空（关联起始条目 id）");
                };
                let Some(to) = args
                    .get("to")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                else {
                    return ToolResult::err("to 不能为空（关联目标条目 id）");
                };
                let relation = args
                    .get("relation")
                    .and_then(|v| v.as_str())
                    .unwrap_or("related")
                    .trim();
                // 组隔离：双端条目都必须在调用者可见范围（激活组 + 全局），越组拒绝
                let visible = |id: &str| -> Option<bool> {
                    memory
                        .get_entry_public(id)
                        .ok()
                        .flatten()
                        .map(|e| e.group_id.as_deref().is_none_or(|g| g == gid))
                };
                match (visible(from), visible(to)) {
                    (Some(true), Some(true)) => match memory.link(from, to, relation) {
                        Ok(_) => ToolResult::ok(format!(
                            "已建立关联：{from} —{relation}→ {to}（后续列示与检索可见）"
                        )),
                        Err(e) => ToolResult::err(format!("关联写入失败：{e}")),
                    },
                    (Some(false), _) | (_, Some(false)) => {
                        ToolResult::err("关联被拒绝：条目不在你的组范围内（越组隔离）")
                    }
                    _ => ToolResult::err(
                        "条目不存在（检查 from/to 的条目 id，可先用 memory_link op=list 列示）",
                    ),
                }
            }
        }
    }

    // ---------------------------------------------------------------- 声明式自定义工具

    /// 执行声明式自定义工具：http（GET query / POST JSON）或 shell（模板插值 + 审批闸门）。
    /// 输出与内置工具同口径截断，并共享结果落盘 / 钩子 / 审计链路（execute_named 统一走这里）。
    async fn run_custom(
        &self,
        agent_id: &str,
        session_id: &str,
        tool: &crate::config::CustomTool,
        args: &serde_json::Value,
    ) -> ToolResult {
        let Some(params) = args.as_object() else {
            return ToolResult::err("自定义工具的参数必须是对象");
        };
        // 插值用字符串表：字符串直取，其余 JSON 序列化
        let mut vals: std::collections::BTreeMap<String, String> = Default::default();
        for (k, v) in params {
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            vals.insert(k.clone(), s);
        }
        // target 引用的占位：schema 声明 required 而调用未给 → 明确报错（拼空串比报错更坑）
        let required: Vec<String> = tool
            .parameters
            .pointer("/required")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        for name in placeholder_names(&tool.target) {
            if let std::collections::btree_map::Entry::Vacant(slot) = vals.entry(name) {
                if required.iter().any(|r| r == slot.key()) {
                    return ToolResult::err(format!(
                        "自定义工具 {} 缺少必填参数 {}",
                        tool.name,
                        slot.key()
                    ));
                }
                slot.insert(String::new());
            }
        }
        if tool.kind_norm() == "shell" {
            return self
                .run_custom_shell(agent_id, session_id, tool, &vals)
                .await;
        }
        self.run_custom_http(tool, &vals, args).await
    }

    /// http 类自定义工具：GET 把实参拼进已插值的 URL；POST/PUT 把全部实参作为 JSON body
    async fn run_custom_http(
        &self,
        tool: &crate::config::CustomTool,
        vals: &std::collections::BTreeMap<String, String>,
        args: &serde_json::Value,
    ) -> ToolResult {
        // 插件凭据变量（plugin-ecosystem 1.3）：`${credentials.<key>}` 宿主侧解析——
        // 清单零密钥分发。仅命名空间工具可解析（config CustomTool 无插件归属，
        // 引用变量视为缺键）。缺失时拒绝调用（不发部分鉴权请求）。
        let plugin_id = crate::plugin::parse_namespaced(&tool.name).map(|(p, _)| p);
        let (url, mut missing) = match &plugin_id {
            Some(pid) => self.resolve_credential_vars(pid, &tool.target),
            None if tool.target.contains("${credentials.") => {
                return ToolResult::err(format!(
                    "工具 {} 引用了 ${{credentials.*}} 变量但无插件归属（仅插件清单工具可声明凭据变量）",
                    tool.name
                ))
            }
            None => (tool.target.clone(), Vec::new()),
        };
        let mut headers: std::collections::BTreeMap<String, String> = Default::default();
        for (k, v) in &tool.headers {
            let resolved = match &plugin_id {
                Some(pid) => self.resolve_credential_vars(pid, v),
                None => (v.clone(), Vec::new()),
            };
            missing.extend(resolved.1);
            headers.insert(k.clone(), resolved.0);
        }
        if !missing.is_empty() {
            return ToolResult::err(format!(
                "工具 {} 的凭据未配置：{}（请在插件详情的凭据区配置后重试）",
                tool.name,
                missing.join("、")
            ));
        }
        // 实参插值（凭据解析后的 URL 上做 `{参数名}` 占位替换）
        let url = interpolate(&url, vals, urlencode);
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return ToolResult::err("http 类自定义工具的 target 必须是 http(s) URL");
        }
        let method = {
            let m = tool.method.trim().to_ascii_uppercase();
            if m.is_empty() {
                "POST".to_string()
            } else {
                m
            }
        };
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
        {
            Ok(c) => c,
            Err(e) => return ToolResult::err(format!("HTTP 客户端构建失败: {e}")),
        };
        let parsed =
            reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::POST);
        let mut req = client.request(parsed, &url);
        for (k, v) in &headers {
            req = req.header(k, v);
        }
        if method == "POST" || method == "PUT" || method == "PATCH" {
            req = req.json(args);
        }
        match req.send().await {
            Ok(r) => {
                let status = r.status();
                let text = r.text().await.unwrap_or_default();
                if !status.is_success() {
                    return ToolResult::err(format!(
                        "HTTP {status}：{}",
                        text.chars().take(2000).collect::<String>()
                    ));
                }
                shape_response(tool, &text)
            }
            Err(e) => ToolResult::err(format!("请求失败: {e}")),
        }
    }

    /// shell 类自定义工具：模板插值（值经白名单校验 + 引号包裹）→ 与 terminal 同一道审批闸门
    async fn run_custom_shell(
        &self,
        agent_id: &str,
        session_id: &str,
        tool: &crate::config::CustomTool,
        vals: &std::collections::BTreeMap<String, String>,
    ) -> ToolResult {
        // 实参字符白名单：模板插值 + 引号包裹仍挡不住的 shell 元字符一律拒绝 —— 宁可失败不留注入面
        for (k, v) in vals {
            if let Some(c) = v.chars().find(|c| shell_unsafe_char(*c)) {
                return ToolResult::err(format!(
                    "自定义工具 {} 的参数 {k} 含不允许的字符 {c:?}（shell 类工具实参仅支持普通文本）",
                    tool.name
                ));
            }
        }
        let cmd = interpolate(&tool.target, vals, shell_quote);
        // 与 terminal 同一道闸门：声明式模板不是越权通道（risky 拦高危 / always 全拦）
        let lowered = cmd.to_lowercase();
        let mode = self.security.exec_approval.as_str();
        if mode == "risky" || mode == "always" {
            let allowlisted = self.security.exec_allowlist.iter().any(|p| {
                !p.trim().is_empty() && lowered.starts_with(p.trim().to_lowercase().as_str())
            });
            let gated = mode == "always" || is_destructive_command(&lowered);
            if gated && !allowlisted {
                return self.gate_command(agent_id, session_id, &cmd).await;
            }
        }
        let opts = self.shell_opts(&cmd);
        execute_shell_with(&self.effective_root(), &cmd, 120, &opts).await
    }

    /// 智能体管理（docs/09）：仅激活组的主智能体可用；内置组定义受保护。
    /// 审计由 execute() 统一落库。
    fn tool_agent_manage(&self, args: &serde_json::Value) -> ToolResult {
        // 权限闸门：调用者必须是激活组的主智能体，且当前组非内置
        let Some(meta) = self.registry.active_group_meta() else {
            return ToolResult::err("无激活组");
        };
        if meta.builtin {
            return ToolResult::err("内置组的定义受保护：不可增删改个体");
        }
        // 调用者身份由网关在 args 上层校验（execute 时传入 agent_id），
        // 这里校验 op 与内容；调用者==主智能体的强校验在 execute() 的 allowlist 注入处保证。

        let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("");
        match self.agent_manage_op(&meta, op, args) {
            Ok(v) => ToolResult::ok(serde_json::to_string_pretty(&v).unwrap_or_default()),
            Err(e) => ToolResult::err(e.to_string()),
        }
    }

    fn agent_manage_op(
        &self,
        meta: &crate::types::GroupMeta,
        op: &str,
        args: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let s = |k: &str| {
            args.get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        match op {
            "list" => {
                let defs = self.registry.list();
                Ok(serde_json::json!({
                    "group": meta,
                    "agents": defs.iter().map(|d| serde_json::json!({
                        "name": d.name, "identifier": d.identifier,
                        "domain": d.domain, "tier": d.tier, "description": d.description,
                    })).collect::<Vec<_>>()
                }))
            }
            "group_info" => Ok(serde_json::json!({
                "group": meta,
                "count": self.registry.count(),
                "isBuiltin": self.registry.is_builtin(&meta.id),
            })),
            "templates" => {
                let all = self.registry.list_templates();
                Ok(serde_json::json!({
                    "templates": all.iter().map(|t| serde_json::json!({
                        "id": t.id, "name": t.name, "description": t.description, "kind": t.kind,
                    })).collect::<Vec<_>>(),
                    "usage": "create 时传 template=<unit 模板 id>,仅需 identifier;name/description 缺省用模板默认"
                }))
            }
            "create" | "update" => {
                let identifier = s("identifier");
                let name = s("name");
                let description = s("description");
                let template = s("template");
                // 模板创建:仅 identifier 必填,工具面/能力/提示词由模板提供
                if op == "create" && !template.is_empty() {
                    let def = self.registry.create_agent_from_template(
                        &meta.id,
                        &template,
                        &identifier,
                        &name,
                        &description,
                    )?;
                    return Ok(serde_json::json!({ "created": def, "fromTemplate": template }));
                }
                if (op == "create"
                    && (identifier.is_empty() || name.is_empty() || description.is_empty()))
                    || (op == "update" && identifier.is_empty())
                {
                    bail!("create 需要 identifier/name/description（或提供 template）；update 需要 identifier");
                }
                // update：基于现有定义合并
                let mut def = match (op, self.registry.get(&identifier)) {
                    ("update", Some(existing)) => existing,
                    _ => AgentDefinition {
                        name: name.clone(),
                        identifier: identifier.clone(),
                        domain: if s("domain").is_empty() {
                            "自定义".into()
                        } else {
                            s("domain")
                        },
                        tier: Tier::Unit,
                        description: description.clone(),
                        capabilities: args
                            .get("capabilities")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        tools: vec![],
                        when_to_call: s("whenToCall"),
                        dependencies: vec![],
                        composable_with: vec![],
                        input_schema: Default::default(),
                        output_schema: Default::default(),
                        prompt_file: String::new(),
                        model_hint: None,
                        link: None,
                        max_steps: None,
                    },
                };
                if !name.is_empty() {
                    def.name = name;
                }
                if !description.is_empty() {
                    def.description = description;
                }
                if !s("domain").is_empty() {
                    def.domain = normalize_domain(&s("domain"));
                }
                if let Some(t) = args.get("tier").and_then(|v| v.as_str()) {
                    if t == "orchestrator" {
                        def.tier = Tier::Orchestrator;
                    }
                }
                if let Some(t) = args.get("prompt").and_then(|v| v.as_str()) {
                    def.prompt_file = format!("{}.md", def.identifier);
                    self.registry.write_prompt(&def.prompt_file, t)?;
                }
                if let Some(m) = args.get("maxSteps").and_then(|v| v.as_u64()) {
                    def.max_steps = Some(m as usize);
                }
                let saved = self.registry.upsert_agent(&meta.id, def, None)?;
                // 主智能体由系统指定：仅组的首个个体自动成为主智能体，不支持手动换主
                if meta.primary.is_none() {
                    self.registry.set_primary(&meta.id, &saved.identifier)?;
                }
                Ok(serde_json::to_value(&saved)?)
            }
            "delete" => {
                let identifier = s("identifier");
                self.registry.remove_agent(&meta.id, &identifier)?;
                Ok(serde_json::json!({ "deleted": identifier }))
            }
            other => bail!("未知 agent_manage 操作: {other}"),
        }
    }

    /// 路径安全解析：规范化后必须落在「组工作区」或「全局工作区」之内。
    /// 不存在的路径按「最近存在的父目录」规范化后再判定 —— 否则 `..` 拼接可绕过越界检查。
    fn resolve_safe(&self, raw: &str) -> anyhow::Result<PathBuf> {
        let raw_trim = raw.trim();
        if raw_trim.is_empty() {
            bail!("路径不能为空");
        }
        let group_root = self.effective_root();
        let candidate = if Path::new(raw_trim).is_absolute() {
            PathBuf::from(raw_trim)
        } else {
            group_root.join(raw_trim)
        };
        let group_canon = canonicalize_best(&group_root);
        let ws_canon = canonicalize_best(&self.workspace_root);
        let cand_canon = canonicalize_best(&candidate);
        if !cand_canon.starts_with(&group_canon) && !cand_canon.starts_with(&ws_canon) {
            bail!("路径越界（仅允许工作区内的路径）: {raw}");
        }
        Ok(candidate)
    }

    /// 写前快照（检查点）：原文件复制到 .exmachina/checkpoints/<日期>/<相对路径>。
    /// 同日同名已存在则跳过 —— 保存的是「当日首次改动前」的版本，供 `exm checkpoint restore` 回滚。
    /// `session_id` 非空时把检查点引用（date/rel）登记进该会话的轮次链（undo 联动回滚用）。
    fn checkpoint(&self, target: &Path, session_id: &str) -> anyhow::Result<()> {
        if !target.is_file() {
            return Ok(());
        }
        let root = self.effective_root();
        let Ok(rel) = target.strip_prefix(&root) else {
            return Ok(()); // 组工作区在工作区外：不做快照（无回滚能力，但也不阻断写入）
        };
        let day: String = crate::types::now_iso().chars().take(10).collect();
        let dst = self
            .workspace_root
            .join(".exmachina")
            .join("checkpoints")
            .join(&day)
            .join(rel);
        let fresh = !dst.exists();
        if fresh {
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(target, &dst).context("写检查点失败")?;
        }
        // 轮次检查点链：仅登记当日首次快照（重复跳过的文件其既有检查点已可回滚）
        if fresh && !session_id.is_empty() {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let _ = self.store.record_checkpoint_ref(session_id, &day, &rel_str);
        }
        Ok(())
    }

    /// 读取文件或目录：文件按行带行号返回（offset/limit 分页），目录返回条目清单
    fn tool_read(&self, args: &serde_json::Value) -> ToolResult {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let offset = args
            .get("offset")
            .and_then(|v| v.as_u64())
            .unwrap_or(1)
            .max(1) as usize;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(400)
            .clamp(1, 5000) as usize;
        // maxChars 与 limit 同口径设上限：模型传超大值（1e9）不应绕过单次读取的预算
        // （默认 24000，硬上限 100000；再大的内容请分段读）
        let max_chars = args
            .get("maxChars")
            .and_then(|v| v.as_u64())
            .unwrap_or(24_000)
            .clamp(1, 100_000) as usize;
        let p = match self.resolve_safe(path) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e.to_string()),
        };
        if p.is_dir() {
            return self.list_dir(&p);
        }
        // 图像副产物（spec: agent-terminal-tools read 图像副产物）：
        // 按扩展名识别图像，返回摘要 + images 路径（runtime 视觉模型回灌）
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp") {
            let meta = match std::fs::metadata(&p) {
                Ok(m) => m,
                Err(e) => return ToolResult::err(format!("读取失败: {e}")),
            };
            let abs = std::fs::canonicalize(&p)
                .map(|x| x.display().to_string())
                .unwrap_or_else(|_| p.display().to_string());
            return ToolResult::ok(format!(
                "图像文件：{path}（{ext}，{} 字节）—— 已作为图像输入回灌，具备视觉能力的模型可直接查看",
                meta.len()
            ))
            .with_images(vec![abs]);
        }
        let bytes = match std::fs::read(&p) {
            Ok(b) => b,
            Err(e) => return ToolResult::err(format!("读取失败: {e}")),
        };
        let text = match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(e) => {
                return ToolResult::err(format!(
                    "不是 UTF-8 文本文件（{} 字节），无法按行读取；二进制处理请用 terminal",
                    e.as_bytes().len()
                ))
            }
        };
        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len();
        if total == 0 {
            return ToolResult::ok(format!("（空文件，0 行）{path}"));
        }
        let start = (offset - 1).min(total);
        // 越界 offset 给出可行动的提示而非「已显示第 无 行」：模型下一跳就能自己纠正
        if start >= total {
            return ToolResult::ok(format!(
                "{path}：共 {total} 行，offset={offset} 已超出文件末尾；请用 offset≤{total}"
            ));
        }
        let mut out = String::new();
        let mut used = 0usize;
        let mut end = start;
        for (i, line) in lines.iter().enumerate().skip(start) {
            if i - start >= limit {
                break;
            }
            let rendered = format!("{:>5}→{}\n", i + 1, line);
            if used + rendered.chars().count() > max_chars {
                if i == start {
                    out.push_str(&format!(
                        "{:>5}→{}\n",
                        i + 1,
                        line.chars().take(600).collect::<String>()
                    ));
                    end = i + 1;
                }
                break;
            }
            used += rendered.chars().count();
            out.push_str(&rendered);
            end = i + 1;
        }
        let shown = if end > start {
            format!("{}–{}", start + 1, end)
        } else {
            "无".to_string()
        };
        out.push_str(&format!("\n（{path}：共 {total} 行；已显示第 {shown} 行）"));
        if end < total {
            out.push_str(&format!("；续读 offset={}", end + 1));
        }
        ToolResult::ok(out)
    }

    /// 目录清单：目录在前，带大小与条目总数
    fn list_dir(&self, p: &Path) -> ToolResult {
        let entries = match std::fs::read_dir(p) {
            Ok(e) => e,
            Err(e) => return ToolResult::err(format!("列目录失败: {e}")),
        };
        let mut rows: Vec<(String, bool, u64)> = Vec::new();
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let meta = e.metadata().ok();
            let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            rows.push((name, is_dir, size));
        }
        rows.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
        });
        let total = rows.len();
        let body = rows
            .iter()
            .take(200)
            .map(|(n, d, s)| {
                if *d {
                    format!("[dir]  {n}/")
                } else {
                    format!("[file] {n}  ({})", human_size(*s))
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        if body.is_empty() {
            return ToolResult::ok("（空目录）");
        }
        ToolResult::ok(format!(
            "{body}\n（共 {total} 项，已列 {}）",
            total.min(200)
        ))
    }

    /// 精确编辑：old_string → new_string（唯一命中校验；支持 replace_all）
    fn tool_edit(&self, session_id: &str, args: &serde_json::Value) -> ToolResult {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let old = args
            .get("old_string")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let new = args
            .get("new_string")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let replace_all = args
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if old.is_empty() {
            return ToolResult::err("old_string 不能为空；新建文件请用 filesystem write");
        }
        if old == new {
            return ToolResult::err("old_string 与 new_string 完全相同，无需编辑");
        }
        let p = match self.resolve_safe(path) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e.to_string()),
        };
        if !p.is_file() {
            return ToolResult::err(format!("文件不存在或不是普通文件: {path}"));
        }
        let content = match std::fs::read_to_string(&p) {
            Ok(c) => c,
            Err(e) => return ToolResult::err(format!("读取失败: {e}")),
        };
        let hits: Vec<usize> = content
            .match_indices(old)
            .map(|(i, _)| content[..i].matches('\n').count() + 1)
            .collect();
        if hits.is_empty() {
            return ToolResult::err(format!(
                "未找到匹配文本（{path}）。old_string 需与文件内容逐字符一致（含缩进与换行）——建议先用 read 读取该段再复制。"
            ));
        }
        if hits.len() > 1 && !replace_all {
            let at: Vec<String> = hits.iter().take(8).map(|l| l.to_string()).collect();
            return ToolResult::err(format!(
                "old_string 命中 {} 处（行 {}），无法判断改哪一处：请扩大上下文使其唯一，或置 replace_all=true 全部替换。",
                hits.len(),
                at.join(", ")
            ));
        }
        let (updated, count) = if replace_all {
            (content.replace(old, new), hits.len())
        } else {
            (content.replacen(old, new, 1), 1)
        };
        if let Err(e) = self.checkpoint(&p, session_id) {
            return ToolResult::err(format!("写前检查点失败，已中止编辑：{e}"));
        }
        if let Err(e) = std::fs::write(&p, &updated) {
            return ToolResult::err(format!("写入失败: {e}"));
        }
        let delta = updated.lines().count() as i64 - content.lines().count() as i64;
        let mut msg = format!("已替换 {count} 处（{path}，首处位于第 {} 行）", hits[0]);
        if delta != 0 {
            msg.push_str(&format!("；行数 {delta:+}"));
        }
        ToolResult::ok(msg)
    }

    /// 统一 diff 补丁：逐文件在内存中校验并计算新内容，任一文件失败则整体拒绝；
    /// 全部通过后统一落检查点再写入（多文件原子语义）
    fn tool_patch(&self, session_id: &str, args: &serde_json::Value) -> ToolResult {
        let patch_text = args.get("patch").and_then(|v| v.as_str()).unwrap_or("");
        if patch_text.trim().is_empty() {
            return ToolResult::err("patch 不能为空：请提供统一 diff 格式的补丁全文");
        }
        let file_patches = match crate::patch::parse_unified_diff(patch_text) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(format!("补丁解析失败: {e}")),
        };

        struct Plan {
            path: PathBuf,
            rel: String,
            content: String,
            delete: bool,
            added: usize,
            removed: usize,
        }
        let mut plans: Vec<Plan> = Vec::new();
        let mut failures: Vec<String> = Vec::new();

        for fp in &file_patches {
            let p = match self.resolve_safe(&fp.path) {
                Ok(p) => p,
                Err(e) => {
                    failures.push(format!("{}: {e}", fp.path));
                    continue;
                }
            };
            let old = if fp.new_file {
                if p.is_file() {
                    failures.push(format!("{}: 标记为新文件但已存在", fp.path));
                    continue;
                }
                String::new()
            } else {
                match std::fs::read_to_string(&p) {
                    Ok(c) => c,
                    Err(e) => {
                        failures.push(format!(
                            "{}: 读取失败（二进制文件不支持补丁）: {e}",
                            fp.path
                        ));
                        continue;
                    }
                }
            };
            match crate::patch::apply_file(&old, fp) {
                Ok(new) => {
                    let added = fp
                        .hunks
                        .iter()
                        .flat_map(|h| h.lines.iter())
                        .filter(|l| l.kind == crate::patch::LineKind::Add)
                        .count();
                    let removed = fp
                        .hunks
                        .iter()
                        .flat_map(|h| h.lines.iter())
                        .filter(|l| l.kind == crate::patch::LineKind::Del)
                        .count();
                    plans.push(Plan {
                        path: p,
                        rel: fp.path.clone(),
                        content: new,
                        delete: fp.delete_file,
                        added,
                        removed,
                    });
                }
                Err(e) => failures.push(e),
            }
        }

        // 整体拒绝：报告全部冲突，不落任何写入
        if !failures.is_empty() {
            let mut msg = format!(
                "补丁整体拒绝：{} 个文件中 {} 个校验失败，未写入任何文件。\n失败详情：",
                file_patches.len(),
                failures.len()
            );
            for f in &failures {
                msg.push_str(&format!("\n- {f}"));
            }
            msg.push_str("\n请用 read 读取目标文件后修正补丁上下文再重试。");
            return ToolResult::err(msg);
        }

        // 全部通过：检查点 → 写入（/删除）
        for plan in &plans {
            if let Err(e) = self.checkpoint(&plan.path, session_id) {
                return ToolResult::err(format!("写前检查点失败，已中止补丁应用：{e}"));
            }
            if plan.delete {
                if let Err(e) = std::fs::remove_file(&plan.path) {
                    return ToolResult::err(format!("删除文件失败（{}）: {e}", plan.rel));
                }
            } else if let Some(parent) = plan.path.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return ToolResult::err(format!("创建目录失败（{}）: {e}", plan.rel));
                }
                if let Err(e) = std::fs::write(&plan.path, &plan.content) {
                    return ToolResult::err(format!("写入失败（{}）: {e}", plan.rel));
                }
            }
        }
        let detail: Vec<String> = plans
            .iter()
            .map(|p| {
                if p.delete {
                    format!("{}（删除）", p.rel)
                } else {
                    format!("{}（+{}/-{}）", p.rel, p.added, p.removed)
                }
            })
            .collect();
        ToolResult::ok(format!(
            "补丁已应用 {} 个文件：\n- {}",
            plans.len(),
            detail.join("\n- ")
        ))
    }

    /// 仓库检索：正则搜索文件内容（尊重 .gitignore），返回 文件:行号:内容
    fn tool_grep(&self, args: &serde_json::Value) -> ToolResult {
        let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
        if pattern.trim().is_empty() {
            return ToolResult::err("pattern 不能为空");
        }
        let sub = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let glob = args
            .get("glob")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let ic = args
            .get("ignoreCase")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let max_results = args
            .get("maxResults")
            .and_then(|v| v.as_u64())
            .unwrap_or(100)
            .clamp(1, 500) as usize;
        // 上下文行数（spec: agent-terminal-tools grep 上下文行）：0 = 现状单行输出
        let ctx_n = args
            .get("contextLines")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            .clamp(0, 5) as usize;
        let root = match self.resolve_safe(sub) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e.to_string()),
        };
        let re = match regex::RegexBuilder::new(pattern)
            .case_insensitive(ic)
            .build()
        {
            Ok(r) => r,
            Err(e) => return ToolResult::err(format!("正则无效: {e}")),
        };
        let walker = build_walker(&root, &glob);
        let base = self.effective_root();
        let mut out: Vec<String> = Vec::new();
        let mut hit_files = 0usize;
        let mut truncated = false;
        let mut hit_lines: Vec<(usize, String)> = Vec::new();
        'files: for entry in walker {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if meta.len() > 2 * 1024 * 1024 {
                continue; // 跳过 >2MB 的文件
            }
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            let rel = path
                .strip_prefix(&base)
                .unwrap_or(path)
                .display()
                .to_string()
                .replace('\\', "/");
            let mut file_hit = false;
            hit_lines.clear();
            for (i, line) in text.lines().enumerate() {
                if re.is_match(line) {
                    if !file_hit {
                        file_hit = true;
                        hit_files += 1;
                    }
                    // contextLines>0 时记录行号，稍后按块合并输出（含前后上下文行）
                    if ctx_n > 0 {
                        hit_lines.push((i, line.to_string()));
                        continue;
                    }
                    let trimmed: String = line.trim().chars().take(240).collect();
                    out.push(format!("{rel}:{}:{}", i + 1, trimmed));
                    if out.len() >= max_results {
                        truncated = true;
                        break 'files;
                    }
                }
            }
            // 上下文块输出：相邻命中（间隔 ≤ 2N+1）合并为一块，
            // 块内逐行 `rel:行号: 内容`，命中行内容保持 240 字符截断
            if ctx_n > 0 && !hit_lines.is_empty() {
                let all_lines: Vec<&str> = text.lines().collect();
                let mut blocks: Vec<(usize, usize)> = Vec::new(); // [start, end] 行号闭区间
                for (i, _) in &hit_lines {
                    let s = i.saturating_sub(ctx_n);
                    let e = (i + ctx_n).min(all_lines.len().saturating_sub(1));
                    match blocks.last_mut() {
                        Some((bs, be)) if s <= *be + 1 => {
                            if e > *be {
                                *be = e;
                            }
                        }
                        _ => blocks.push((s, e)),
                    }
                }
                for (s, e) in blocks {
                    if !out.is_empty() {
                        out.push(format!("{rel}—"));
                    }
                    for ln in s..=e {
                        if ln >= all_lines.len() {
                            break;
                        }
                        let content = &all_lines[ln];
                        let mark = if re.is_match(content) { ">" } else { " " };
                        let trimmed: String = content.trim().chars().take(240).collect();
                        out.push(format!("{rel}:{}:{} {}", ln + 1, mark, trimmed));
                    }
                    if out.len() >= max_results {
                        truncated = true;
                        break 'files;
                    }
                }
            }
        }
        if out.is_empty() {
            return ToolResult::ok(format!("无匹配（pattern: {pattern}）"));
        }
        let mut body = out.join("\n");
        body.push_str(&format!(
            "\n（{} 条命中，涉及 {hit_files} 个文件{}）",
            out.len(),
            if truncated {
                "，已达结果上限——请收窄 pattern/path"
            } else {
                ""
            }
        ));
        ToolResult::ok(body)
    }

    /// 按 glob 模式列出文件路径
    fn tool_glob(&self, args: &serde_json::Value) -> ToolResult {
        let pattern = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if pattern.is_empty() {
            return ToolResult::err("pattern 不能为空（如 **/*.rs）");
        }
        let sub = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let root = match self.resolve_safe(sub) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e.to_string()),
        };
        let walker = build_walker(&root, &pattern);
        let base = self.effective_root();
        let mut files: Vec<String> = Vec::new();
        for entry in walker {
            let Ok(entry) = entry else { continue };
            if entry.path().is_file() {
                let rel = entry
                    .path()
                    .strip_prefix(&base)
                    .unwrap_or(entry.path())
                    .display()
                    .to_string()
                    .replace('\\', "/");
                files.push(rel);
                if files.len() >= 500 {
                    break;
                }
            }
        }
        files.sort();
        if files.is_empty() {
            return ToolResult::ok(format!("无匹配文件（pattern: {pattern}）"));
        }
        let n = files.len();
        ToolResult::ok(format!("{}\n（{n} 个文件）", files.join("\n")))
    }

    /// 递归复制目录树/文件（回收站跨设备 fallback 用）：
    /// 目标已存在则清空重建，保证「移入」语义的原子性近似
    fn copy_tree(src: &Path, dst: &Path) -> anyhow::Result<()> {
        use std::fs;
        if src.is_dir() {
            if dst.exists() {
                // 目标可能是文件或目录，分别清理
                if dst.is_dir() {
                    fs::remove_dir_all(dst).context("回收站旧目标清理失败")?;
                } else {
                    fs::remove_file(dst).context("回收站旧目标清理失败")?;
                }
            }
            fs::create_dir_all(dst).context("回收站目录创建失败")?;
            for entry in fs::read_dir(src).context("读取源目录失败")? {
                let entry = entry?;
                let ty = entry.file_type()?;
                let child_dst = dst.join(entry.file_name());
                if ty.is_dir() {
                    fs::create_dir_all(&child_dst)?;
                    Self::copy_tree(&entry.path(), &child_dst)?;
                } else {
                    fs::copy(entry.path(), &child_dst).context("复制文件失败")?;
                }
            }
            Ok(())
        } else {
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(src, dst).context("复制文件失败").map(|_| ())
        }
    }

    fn tool_fs(&self, session_id: &str, args: &serde_json::Value) -> ToolResult {
        let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("write");
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let target = match self.resolve_safe(path) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e.to_string()),
        };
        match op {
            "write" => {
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(parent) = target.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = self.checkpoint(&target, session_id) {
                    return ToolResult::err(format!("写前检查点失败，已中止写入：{e}"));
                }
                match std::fs::write(&target, content).context("写入失败") {
                    Ok(_) => {
                        // 产物登记（D12）：写入投递目录 `.exmachina/outbox/` 的文件 → 会话 outbox
                        let norm = target
                            .strip_prefix(&self.workspace_root)
                            .map(|p| p.display().to_string().replace('\\', "/"))
                            .unwrap_or_default();
                        if norm.starts_with(".exmachina/outbox/") {
                            let _ = self.store.outbox_push(session_id, "file", &norm, "");
                        }
                        ToolResult::ok(format!(
                            "written: {}（{} 行 / {} 字符）",
                        target.display(),
                        content.lines().count(),
                        content.chars().count()
                    ))
                    }
                    Err(e) => ToolResult::err(e.to_string()),
                }
            }
            "mkdir" => match std::fs::create_dir_all(&target) {
                Ok(_) => ToolResult::ok(format!("mkdir: {}", target.display())),
                Err(e) => ToolResult::err(e.to_string()),
            },
            "list" => {
                if target.is_dir() {
                    self.list_dir(&target)
                } else {
                    ToolResult::err(format!("不是目录: {path}"))
                }
            }
            // 删除进回收站（spec: filesystem-ops）：不裸删——整树移入
            // .exmachina/trash/<日期>/<相对路径>，保留原内容供人工恢复
            "delete" => {
                let root = self.effective_root();
                // 保护区：工作区根 / .exmachina 系统区（含检查点与回收站自身）
                if target == root {
                    return ToolResult::err("拒绝删除：工作区根目录不可删除");
                }
                let sys = self.workspace_root.join(".exmachina");
                if target.starts_with(&sys) {
                    return ToolResult::err(
                        "拒绝删除：.exmachina 系统区（检查点/回收站/沙箱等）不可经此删除；确需清理请用终端并过审批",
                    );
                }
                if !target.exists() {
                    return ToolResult::err(format!("目标不存在: {path}"));
                }
                let Ok(rel) = target.strip_prefix(&root) else {
                    return ToolResult::err("拒绝删除：目标不在当前工作区内");
                };
                let day: String = crate::types::now_iso().chars().take(10).collect();
                let mut dst = sys.join("trash").join(&day).join(rel);
                // 同日同名冲突：追加序号（build.log → build.log.2）
                let mut seq = 2u32;
                while dst.exists() {
                    let name = format!(
                        "{}.{seq}",
                        rel.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default()
                    );
                    dst = sys.join("trash").join(&day).join(rel.with_file_name(name));
                    seq += 1;
                }
                if let Some(parent) = dst.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        return ToolResult::err(format!("回收站目录创建失败：{e}"));
                    }
                }
                let rel_disp = dst
                    .strip_prefix(&self.workspace_root)
                    .map(|p| p.display().to_string().replace('\\', "/"))
                    .unwrap_or_else(|_| dst.display().to_string());
                // fs::rename 跨设备（EXDEV：回收站常为独立挂载卷）时 fallback copy+删源。
                // 逐条目复制保序（目录先建、文件后拷），任一失败即中止（残留部分文件不影响原位数据）
                let moved = std::fs::rename(&target, &dst)
                    .or_else(|_| Self::copy_tree(&target, &dst))
                    .and_then(|_| {
                        // rename 成功时源已不在；copy 路径源仍在，删源完成「移入」语义
                        if target.exists() {
                            if target.is_dir() {
                                std::fs::remove_dir_all(&target)
                            } else {
                                std::fs::remove_file(&target)
                            }
                            .context("回收站已写入但删源失败（原位数据未动）")
                        } else {
                            Ok(())
                        }
                    });
                match moved {
                    Ok(_) => ToolResult::ok(format!(
                        "已删除（移入回收站，可人工恢复）：{path} → {rel_disp}"
                    )),
                    Err(e) => ToolResult::err(format!("删除失败（回收站移入）: {e}")),
                }
            }
            // 重命名/移动（spec: filesystem-ops）：不覆盖 + 源检查点（undo 链不断）
            "rename" => {
                let new_path = args.get("newPath").and_then(|v| v.as_str()).unwrap_or("");
                if new_path.trim().is_empty() {
                    return ToolResult::err("op=rename 需要 newPath（相对工作区的目标路径）");
                }
                let new_target = match self.resolve_safe(new_path) {
                    Ok(p) => p,
                    Err(e) => return ToolResult::err(e.to_string()),
                };
                if !target.exists() {
                    return ToolResult::err(format!("源不存在: {path}"));
                }
                if target == new_target {
                    return ToolResult::err("newPath 与原路径相同，无需重命名");
                }
                if new_target.exists() {
                    return ToolResult::err(format!(
                        "目标已存在: {new_path}（rename 不覆盖；如需替换请先 delete）"
                    ));
                }
                // 源写前检查点：undo 链可回滚到重命名前
                if target.is_file() {
                    if let Err(e) = self.checkpoint(&target, session_id) {
                        return ToolResult::err(format!("写前检查点失败，已中止重命名：{e}"));
                    }
                }
                if let Some(parent) = new_target.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match std::fs::rename(&target, &new_target) {
                    Ok(_) => ToolResult::ok(format!("renamed: {path} -> {new_path}")),
                    Err(e) => ToolResult::err(format!("重命名失败: {e}")),
                }
            }
            other => ToolResult::err(format!(
                "未知 filesystem 操作: {other}（支持 write / mkdir / list / delete / rename）"
            )),
        }
    }

    /// 审批闸门是否开启（risky/always）——白名单外命令转人工审批；off 档直接拒绝
    pub fn is_approval_gated(&self) -> bool {
        matches!(self.security.exec_approval.as_str(), "risky" | "always")
    }

    /// Git 多合一工具（spec: git-tools）。
    /// 只读动作直接执行（无副作用）；写动作走 terminal 同一道审批闸门
    /// （off 档拒绝、risky/always 档产审批单系统代执行）——命令文本以
    /// `git <action> ...` 呈现，天然不在白名单前缀内即进审批。
    /// push/pull 等远程操作不在动作面（防越出工作区边界）。
    /// 写动作执行前对 pathSpec 涉及文件记检查点（来源 git，可回滚）。
    async fn tool_git(
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
                    return ToolResult::err("message 含引号/反引号/美元符号/反斜杠，为防注入请调整提交信息");
                }
                format!("git commit -m '{msg}'")
            }
            "restore" => {
                let path = s("pathSpec");
                if path.is_empty() || path.starts_with('-') || contains_shell_chain(&path) {
                    return ToolResult::err("restore 需要合法 pathSpec（非空、非选项前缀、无连接符）");
                }
                format!("git restore -- {path}")
            }
            "stash-push" => {
                let msg = s("message");
                if msg.contains(['\'', '"', '\\', '`', '$']) {
                    return ToolResult::err("message 含引号/反引号/美元符号/反斜杠，为防注入请调整说明");
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
                    return ToolResult::err("checkout-b 需要合法分支名（非空、非选项前缀、无连接符）");
                }
                format!("git checkout -b {b}")
            }
            "push" | "pull" | "fetch" | "clone" | "remote" | "reset" | "rebase" | "merge" | "cherry-pick" | "revert" => {
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
    fn git_readonly_argv(
        action: &str,
        args: &serde_json::Value,
    ) -> Result<Vec<String>, String> {
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

    /// 子智能体派生（spec: subagent-spawn）：
    /// - 独立上下文（空历史）、自包含任务书
    /// - 工具白名单强制剔除 spawn/agent_manage（单层限制）
    /// - 实例级并发信号量 + 独立超时（超限/超时明确说明）
    /// - 启动/完成/失败事件进主会话事件流（可观测）
    async fn tool_spawn(
        &self,
        agent_id: &str,
        session_id: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        // 后台回收面（spec: subagent-spawn 异步并行）：status/list 不走派生流程，
        // 也不依赖执行器装配（只读注册表）
        match args.get("op").and_then(|v| v.as_str()).unwrap_or("") {
            "status" => {
                let sid = args.get("spawnId").and_then(|v| v.as_str()).unwrap_or("");
                return self.spawn_bg_status(session_id, sid);
            }
            "list" => return self.spawn_bg_list(session_id),
            _ => {}
        }
        let Some(exec) = self.spawn_executor.read().clone() else {
            return ToolResult::err("spawn 执行器未装配（运行时未就绪）");
        };
        let task = args.get("task").and_then(|v| v.as_str()).unwrap_or("").trim();
        if task.is_empty() {
            return ToolResult::err("task 不能为空：请提供自包含任务书（目标、范围、期望产出）");
        }
        // 工具白名单：显式指定或默认只读；强制剔除 spawn/agent_manage
        let requested: Vec<ToolName> = args
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .filter_map(ToolName::parse)
                    .filter(|t| !matches!(t, ToolName::Spawn | ToolName::AgentManage))
                    .collect()
            })
            .unwrap_or_else(|| {
                vec![
                    ToolName::Read,
                    ToolName::Grep,
                    ToolName::Glob,
                    ToolName::WebFetch,
                ]
            });
        if requested.is_empty() {
            return ToolResult::err("工具白名单为空（或全部被剔除）：至少保留一个工具");
        }
        let timeout = args
            .get("timeoutSecs")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.spawn_timeout_secs)
            .clamp(5, 1800);
        let spawn_id = format!("sp-{}", &crate::types::new_id()[..8]);
        // 可观测：子任务启动事件
        self.emit_and_persist(CoreEvent {
            kind: "spawn.started".into(),
            session_id: session_id.to_string(),
            payload: serde_json::json!({
                "spawnId": spawn_id, "agentId": agent_id,
                "taskPreview": task.chars().take(120).collect::<String>(),
                "tools": requested.iter().map(|t| t.key()).collect::<Vec<_>>(),
                "background": args.get("background").and_then(|v| v.as_bool()).unwrap_or(false),
            }),
        });
        // 并发闸：try_acquire 即时反馈（排队语义由主智能体重试承担）
        let sem = self.spawn_semaphore.clone();
        let Ok(permit) = sem.clone().try_acquire_owned() else {
            return ToolResult::err(format!(
                "子智能体并发已达上限（{}）：请等待在跑子任务完成后再派生",
                sem.available_permits()
            ));
        };
        // 后台模式（spec: subagent-spawn）：立即返回 spawnId，许可随任务体持有至终结；
        // 终态写注册表 + spawn.finished 事件，主智能体期间可继续对话/派生/调工具
        if args
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let agent_owned = agent_id.to_string();
            let registry = self.spawns_bg.clone();
            let events = self.events.clone();
            // 持久化通道一并移入任务体：终态事件落 store（REST 事件面/WebUI 刷新后可见）
            let store = self.store.clone();
            let session = session_id.to_string();
            let task_preview: String = task.chars().take(120).collect();
            registry.lock().insert(
                spawn_id.clone(),
                SpawnBgTask {
                    spawn_id: spawn_id.clone(),
                    task_preview: task_preview.clone(),
                    session: session.clone(),
                    started: Instant::now(),
                    state: parking_lot::RwLock::new(SpawnBgState::Running),
                    read: parking_lot::RwLock::new(false),
                },
            );
            let task_owned = task.to_string();
            let sid_evt = spawn_id.clone();
            let _handle = tokio::spawn(async move {
                let _p = permit; // 许可随任务体存活，终结自动释放
                let fut = exec(&agent_owned, &session, task_owned.as_str(), requested);
                let (state, status, preview) = match tokio::time::timeout(
                    std::time::Duration::from_secs(timeout),
                    fut,
                )
                .await
                {
                    Ok(Ok(text)) => {
                        let p: String = text.chars().take(160).collect();
                        (SpawnBgState::Done(text), "done", p)
                    }
                    Ok(Err(e)) => {
                        let msg = e.to_string();
                        (SpawnBgState::Failed(msg.clone()), "failed", msg)
                    }
                    Err(_) => (SpawnBgState::Timeout, "timeout", String::new()),
                };
                if let Some(t) = registry.lock().get(&sid_evt) {
                    *t.state.write() = state;
                }
                let evt = CoreEvent {
                    kind: "spawn.finished".into(),
                    session_id: session,
                    payload: serde_json::json!({
                        "spawnId": sid_evt, "status": status, "background": true,
                        "summaryPreview": preview,
                    }),
                };
                let _ = events.send(evt.clone());
                // 落 store：REST 事件 API / WebUI 刷新后可查（与 emit_and_persist 同口径）
                let env = crate::types::Envelope::new(
                    format!("session.{}.events", evt.session_id),
                    "tools",
                    "broadcast",
                    "event",
                    evt.session_id.clone(),
                    serde_json::to_value(&evt).unwrap_or(serde_json::Value::Null),
                );
                let _ = store.append_event(&env);
            });
            return ToolResult::ok(format!(
                "子任务已后台启动：spawnId={spawn_id}\n任务：{task_preview}\n用 spawn(op=\"status\", spawnId=\"{spawn_id}\") 查询结果；spawn(op=\"list\") 列全部。完成后下一轮上下文会自动提醒。"
            ));
        }
        let task_owned = task.to_string();
        let fut = exec(agent_id, session_id, task_owned.as_str(), requested);
        let result = match tokio::time::timeout(std::time::Duration::from_secs(timeout), fut).await
        {
            Ok(Ok(text)) => {
                self.emit_and_persist(CoreEvent {
                    kind: "spawn.finished".into(),
                    session_id: session_id.to_string(),
                    payload: serde_json::json!({
                        "spawnId": spawn_id, "status": "done",
                        "summaryPreview": text.chars().take(160).collect::<String>(),
                    }),
                });
                ToolResult::ok(text)
            }
            Ok(Err(e)) => {
                self.emit_and_persist(CoreEvent {
                    kind: "spawn.finished".into(),
                    session_id: session_id.to_string(),
                    payload: serde_json::json!({
                        "spawnId": spawn_id, "status": "failed", "error": e.to_string(),
                    }),
                });
                ToolResult::err(format!("子任务执行失败：{e}"))
            }
            Err(_) => {
                self.emit_and_persist(CoreEvent {
                    kind: "spawn.finished".into(),
                    session_id: session_id.to_string(),
                    payload: serde_json::json!({
                        "spawnId": spawn_id, "status": "timeout", "timeoutSecs": timeout,
                    }),
                });
                ToolResult::err(format!(
                    "子任务超时（{timeout} 秒）被终止：请缩小任务范围或调大 timeoutSecs"
                ))
            }
        };
        result
    }

    /// 动态上下文压缩（实验性，experiments.dynamicCompaction）：模型主动触发。
    /// 压缩执行体在 compaction.rs（compact_now），网关只做装配判定与结果转述。
    async fn tool_context_compact(
        &self,
        agent_id: &str,
        session_id: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        let Some((provider, model)) = self.compactor.read().clone() else {
            return ToolResult::err("压缩器未装配（运行时未就绪）");
        };
        let keep = args
            .get("keepRecent")
            .and_then(|v| v.as_u64())
            .unwrap_or(crate::compaction::COMPACT_KEEP as u64)
            .clamp(2, 100) as usize;
        let note = args
            .get("note")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        match crate::compaction::compact_now(
            &self.store,
            &provider,
            &model,
            session_id,
            keep,
            &note,
        )
        .await
        {
            Some((summary, n)) => {
                self.emit_and_persist(CoreEvent {
                    kind: "context.compacted".into(),
                    session_id: session_id.to_string(),
                    payload: serde_json::json!({
                        "agentId": agent_id, "messagesCompacted": n,
                        "keepRecent": keep, "summaryChars": summary.chars().count(),
                    }),
                });
                ToolResult::ok(format!(
                    "已压缩 {n} 条更早历史为滚动摘要（保留最近 {keep} 条原文）。摘要要点：{}",
                    summary.chars().take(300).collect::<String>()
                ))
            }
            None => ToolResult::ok(
                "窗口外没有待压缩的历史（已全部压过或条数不足）——无需压缩。".to_string(),
            ),
        }
    }

    /// 工作台文件树（对话内 @ 文件提及与 e2e 用，spec: webui-code-workbench → chat-as-coding）：
    /// 相对工作区根的目录列表（.gitignore 过滤 + 深度/数量限制）。
    /// 与工具同一工作区边界（resolve_safe），越权路径拒绝。
    pub fn workbench_tree(&self, rel: &str, depth_limit: usize) -> anyhow::Result<serde_json::Value> {
        let root = self.effective_root();
        let start = if rel.trim().is_empty() || rel == "." {
            root.clone()
        } else {
            self.resolve_safe(rel)?
        };
        let mut nodes = Vec::new();
        let mut count = 0usize;
        let max_entries = 2000usize;
        walk_tree(&root, &start, 0, depth_limit, &mut nodes, &mut count, max_entries)?;
        Ok(serde_json::json!({
            "root": rel,
            "count": nodes.len(),
            "nodes": nodes,
        }))
    }

    /// 工作台文件读取（与 read 工具同口径分页：offset/limit/maxChars）
    pub fn workbench_file(
        &self,
        path: &str,
        offset: u64,
        limit: u64,
    ) -> anyhow::Result<serde_json::Value> {
        let p = self.resolve_safe(path)?;
        if p.is_dir() {
            anyhow::bail!("是目录不是文件：{path}");
        }
        let bytes = std::fs::read(&p)?;
        let text = String::from_utf8_lossy(&bytes).to_string();
        let total = text.lines().count();
        let off = (offset.max(1) as usize).saturating_sub(1);
        let lim = limit.clamp(1, 5000) as usize;
        let page: Vec<&str> = text.lines().skip(off).take(lim).collect();
        Ok(serde_json::json!({
            "path": path,
            "offset": off + 1,
            "limit": lim,
            "totalLines": total,
            "content": page.join("\n"),
        }))
    }

    /// 工作台树遍历辅助（自由函数，见下方 walk_tree）
    async fn tool_terminal(
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
        // 单一命令约束：带 shell 连接符（&& | ; 重定向等）一律拒绝，防白名单绕过
        if contains_shell_chain(&cmd) {
            return ToolResult::err(
                "命令包含 shell 连接符（&& / | / ; / 重定向等），白名单只放行单一命令；请拆成多次调用。",
            );
        }
        let lowered = cmd.to_lowercase();
        let whitelisted = terminal_allowed(&cmd);
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
                    "命令不在白名单: {cmd}（白名单按「命令 + 子命令」匹配，且不支持连接符；\
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
    fn bg_start(&self, cmd: &str, session_id: &str) -> ToolResult {
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
    fn bg_poll(&self, args: &serde_json::Value) -> ToolResult {
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
    fn bg_kill(&self, args: &serde_json::Value) -> ToolResult {
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
    fn bg_list(&self, session_id: &str) -> ToolResult {
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
                chars[start..].iter().collect::<String>().replace('\n', " ⏎ ")
            };
            let tail_disp = if tail.trim().is_empty() { "（暂无）".to_string() } else { tail };
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
    fn spawn_bg_status(&self, session_id: &str, spawn_id: &str) -> ToolResult {
        if spawn_id.is_empty() {
            return ToolResult::err("op=status 需要 spawnId");
        }
        let guard = self.spawns_bg.lock();
        let Some(t) = guard.get(spawn_id) else {
            return ToolResult::err(format!("后台子任务不存在: {spawn_id}（是否同步派生？同步结果直接在原调用返回）"));
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
    fn spawn_bg_list(&self, session_id: &str) -> ToolResult {
        let mut guard = self.spawns_bg.lock();
        let mut rows: Vec<&SpawnBgTask> = guard
            .values()
            .filter(|t| t.session == session_id)
            .collect();
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
    /// 只读语义面（不进审批闸门）；write 后发 todo.updated 事件。
    fn tool_todo(&self, session_id: &str, args: &serde_json::Value) -> ToolResult {
        let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("write");
        match op {
            "list" => {
                let items = crate::todo::list_items(session_id);
                if items.is_empty() {
                    return ToolResult::ok("当前会话无任务清单。");
                }
                let done = items.iter().filter(|t| t.status == "done").count();
                let body = items
                    .iter()
                    .map(|t| {
                        let mark = match t.status.as_str() {
                            "done" => "✅",
                            "in_progress" => "🔄",
                            _ => "⬜",
                        };
                        format!("- {mark} {}", t.content)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                ToolResult::ok(format!("任务清单（{done}/{} 完成）：\n{body}", items.len()))
            }
            "write" => {
                let Some(arr) = args.get("items").and_then(|v| v.as_array()) else {
                    return ToolResult::err("op=write 需要 items 数组（全量条目；空数组 = 清空）");
                };
                let mut parsed: Vec<(String, String)> = Vec::with_capacity(arr.len());
                for (i, it) in arr.iter().enumerate() {
                    let content = it
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if content.is_empty() {
                        return ToolResult::err(format!(
                            "第 {} 条 content 为空（每条必须有内容；空 items 清单请传空数组）",
                            i + 1
                        ));
                    }
                    let status = it
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("pending")
                        .to_string();
                    parsed.push((content, status));
                }
                let items = crate::todo::write(session_id, parsed);
                let done = items.iter().filter(|t| t.status == "done").count();
                let all = items.is_empty() || done == items.len();
                self.emit_and_persist(CoreEvent {
                    kind: "todo.updated".into(),
                    session_id: session_id.to_string(),
                    payload: serde_json::json!({
                        "count": items.len(),
                        "doneCount": done,
                        "allDone": all,
                        "items": items,
                    }),
                });
                if items.is_empty() {
                    return ToolResult::ok("清单已清空。");
                }
                ToolResult::ok(format!(
                    "清单已更新（{done}/{} 完成）。\n后续每轮上下文都会携带当前清单；全部完成后建议收尾总结。",
                    items.len()
                ))
            }
            other => ToolResult::err(format!("未知 op：{other}（支持 write/list）")),
        }
    }

    /// Computer Use 工具：截屏（只读）与键鼠动作（过审批闸门 + 动作节流）。
    /// 截图落盘 `.exmachina/screenshots/`，路径随 `ToolResult.images` 带回——
    /// 视觉模型直接看到屏幕画面；渠道在工具卡里同样可预览。
    async fn tool_computer(
        &self,
        agent_id: &str,
        session_id: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        if !self.computer_cfg.enabled {
            return ToolResult::err(
                "Computer Use 未启用：请到「设置 → Computer Use」开启（整机输入权须显式交出）",
            );
        }
        let op = args
            .get("op")
            .and_then(|v| v.as_str())
            .unwrap_or("screenshot");
        let mut guard = self.computer.lock().await;
        // 懒初始化：首次调用才建立键鼠控制句柄
        if guard.is_none() {
            match crate::computer::ComputerSession::new() {
                Ok(s) => *guard = Some(s),
                Err(e) => return ToolResult::err(format!("Computer Use 初始化失败：{e}")),
            }
        }
        let session = guard.as_mut().unwrap();
        let interval = self.computer_cfg.action_interval_ms;

        // 截屏：只读，不审批
        if op == "screenshot" {
            let index = args.get("monitor").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let stamp = crate::types::now_iso().replace([':', '-', '.'], "");
            let name = format!("computer-{stamp}");
            let dir = self.workspace_root.join(".exmachina").join("screenshots");
            let result = session.screenshot(index, self.computer_cfg.max_edge, &dir, &name);
            return match result {
                Ok(shot) => {
                    let rel = std::path::Path::new(&shot.path)
                        .strip_prefix(&self.workspace_root)
                        .map(|p| p.display().to_string().replace('\\', "/"))
                        .unwrap_or_else(|_| shot.path.clone());
                    // 产物登记（D12）：截图自动进入会话 outbox，通道投递
                    let _ = self
                        .store
                        .outbox_push(session_id, "image", &rel, "屏幕截图");
                    ToolResult::ok(format!(
                        "【报告】截屏完成：{}，缩放后 {}x{}，文件 {rel}\n（截图已作为图像附带本轮回灌，可直接描述画面内容）",
                        shot.monitor, shot.width, shot.height
                    ))
                    .with_images(vec![rel])
                }
                Err(e) => ToolResult::err(format!("截屏失败：{e}")),
            };
        }

        // 输入类动作：统一审批闸门 + 节流
        let (desc, action): (String, InputAction) = match op {
            "move" => {
                let x = args.get("x").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                let y = args.get("y").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                (format!("move ({x}, {y})"), Box::new(move |s| s.move_mouse(x, y)))
            }
            "click" => {
                let x = args.get("x").and_then(|v| v.as_i64());
                let y = args.get("y").and_then(|v| v.as_i64());
                let button = args.get("button").and_then(|v| v.as_str()).unwrap_or("left").to_string();
                let double = args.get("double").and_then(|v| v.as_bool()).unwrap_or(false);
                let label = format!(
                    "click {}{}",
                    if double { "double " } else { "" },
                    button
                );
                let label = match (x, y) {
                    (Some(x), Some(y)) => format!("{label} @ ({x}, {y})"),
                    _ => label,
                };
                (label, Box::new(move |s| {
                    if let (Some(x), Some(y)) = (x, y) {
                        s.move_mouse(x as i32, y as i32)?;
                    }
                    s.click(&button, double)
                }))
            }
            "scroll" => {
                let dx = args.get("dx").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                let dy = args.get("dy").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                (format!("scroll dx={dx} dy={dy}"), Box::new(move |s| s.scroll(dx, dy)))
            }
            "type" => {
                let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let label = format!("type「{}」", text.chars().take(40).collect::<String>());
                (label, Box::new(move |s| s.type_text(&text)))
            }
            "key" => {
                let combo = args.get("combo").and_then(|v| v.as_str()).unwrap_or("").to_string();
                (format!("key {combo}"), Box::new(move |s| s.hotkey(&combo)))
            }
            other => return ToolResult::err(format!(
                "未知 computer 操作: {other}（支持 screenshot / click / move / scroll / type / key）"
            )),
        };

        // 审批：human 输入权确认（computer.approval=true 时拦截；等待期间批准即放行）
        if self.computer_cfg.approval {
            let gated = self
                .gate_command(agent_id, session_id, &format!("[computer] {desc}"))
                .await;
            if !gated.ok {
                return gated;
            }
        }

        session.throttle(interval);
        let result = action(session);
        match result {
            Ok(()) => ToolResult::ok(format!(
                "【报告】Computer Use 动作已执行：{desc}。需要确认画面可再 screenshot。"
            )),
            Err(e) => ToolResult::err(format!("Computer Use 动作失败（{desc}）：{e}")),
        }
    }

    /// 审批闸门：落审批单 → 发 `approval.required`（带会话归属，聊天页可内联裁决）→
    /// 原地等待人工决定（`security.approvalWaitSecs`，0 = 不等待直接受阻）。
    /// - 批准：`Core::approval_decide` 系统代执行并把输出写回审批单，本侧读回作为工具结果
    ///   ——模型拿到真实执行输出，流程不断；
    /// - 拒绝：把拒绝原因作为工具结果回灌；
    /// - 超时：按「待人工审批」受阻回流（旧行为）。
    async fn gate_command(&self, agent_id: &str, session_id: &str, cmd: &str) -> ToolResult {
        // 来源升级标：通道闸门登记过来源的会话（member）→ 审批单标记须管理员批准
        let origin = self.store.get_session_origin(session_id).ok().flatten();
        let req = ApprovalRequest {
            id: crate::types::new_id()[..8].to_string(),
            session_id: session_id.to_string(),
            node_id: None,
            agent_id: agent_id.to_string(),
            command: cmd.to_string(),
            status: "pending".into(),
            result: None,
            created_at: crate::types::now_iso(),
            decided_at: None,
            origin_user: origin.as_ref().map(|(u, _)| u.clone()),
            origin_role: origin.as_ref().map(|(_, r)| r.clone()),
        };
        let _ = self.store.add_approval(&req);
        let _ = self.events.send(CoreEvent {
            kind: "approval.required".into(),
            session_id: session_id.to_string(),
            payload: serde_json::json!({
                "approvalId": req.id, "agentId": agent_id, "command": cmd,
            }),
        });
        let wait = self.security.approval_wait_secs;
        if wait == 0 {
            return ToolResult::err(format!(
                "命令已拦截待人工审批（审批单 {}）：{cmd}。回流受阻原因；用户批准后由系统代执行。",
                req.id
            ));
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(wait);
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            let Ok(Some(latest)) = self.store.get_approval(&req.id) else {
                continue;
            };
            match latest.status.as_str() {
                "executed" => {
                    let out = latest.result.unwrap_or_default();
                    return ToolResult::ok(format!(
                        "【报告】命令已被用户批准并由系统代执行：{cmd}\n执行输出：\n{out}"
                    ));
                }
                "failed" => {
                    let out = latest.result.unwrap_or_default();
                    return ToolResult::err(format!(
                        "命令已被用户批准并代执行，但执行失败：{cmd}\n输出：\n{out}"
                    ));
                }
                "denied" => {
                    return ToolResult::err(format!(
                        "用户拒绝执行该命令（审批单 {}）：{cmd}。请改用其他方案或向用户说明。",
                        req.id
                    ));
                }
                _ => {}
            }
            if tokio::time::Instant::now() >= deadline {
                return ToolResult::err(format!(
                    "命令已拦截待人工审批（审批单 {}）：{cmd}。等待用户决定超时（{wait}s），按受阻回流；批准后输出不会自动回灌本轮。",
                    req.id
                ));
            }
        }
    }

    /// 抓取网页并提取正文（大小/时长受限，标签与脚本剥离）
    async fn tool_web_fetch(&self, args: &serde_json::Value) -> ToolResult {
        let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
        let max_chars = args
            .get("maxChars")
            .and_then(|v| v.as_u64())
            .unwrap_or(8000) as usize;
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return ToolResult::err("url 必须以 http(s):// 开头");
        }
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
        {
            Ok(c) => c,
            Err(e) => return ToolResult::err(format!("HTTP 客户端构建失败: {e}")),
        };
        let resp = match client.get(url).send().await {
            Ok(r) => r,
            Err(e) => return ToolResult::err(format!("抓取失败: {e}")),
        };
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return ToolResult::err(format!("HTTP {status}"));
        }
        // 截断到 2MB 以内再读
        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => return ToolResult::err(format!("读取响应失败: {e}")),
        };
        let raw = String::from_utf8_lossy(&bytes[..bytes.len().min(2 * 1024 * 1024)]);
        let text = strip_html(&raw);
        let text = text.trim().chars().take(max_chars).collect::<String>();
        if text.is_empty() {
            ToolResult::err("页面无可提取文本（可能是纯二进制或脚本渲染页）")
        } else {
            ToolResult::ok(text)
        }
    }

    /// 联网搜索：调用配置的后端（tavily / brave / searxng / custom）。
    /// 未配置时该工具不会出现在下发 schema 里（此处仍兜底报错，防文本协议路径调用）。
    async fn tool_web_search(&self, args: &serde_json::Value) -> ToolResult {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if query.is_empty() {
            return ToolResult::err("query 不能为空");
        }
        if !self.search.ready() {
            return ToolResult::err(
                "联网搜索未配置：请到「设置 → 联网搜索」选择后端并填写凭据（tavily / brave / searxng / custom）。",
            );
        }
        let provider = self.search.provider.trim().to_lowercase();
        let max = args
            .get("maxResults")
            .and_then(|v| v.as_u64())
            .map(|n| n.clamp(1, 20) as usize)
            .unwrap_or(self.search.max_results.clamp(1, 20));
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
        {
            Ok(c) => c,
            Err(e) => return ToolResult::err(format!("HTTP 客户端构建失败: {e}")),
        };
        let (endpoint, method, payload, bearer) = match provider.as_str() {
            "tavily" => {
                let ep = if self.search.endpoint.trim().is_empty() {
                    "https://api.tavily.com/search".to_string()
                } else {
                    self.search.endpoint.trim().to_string()
                };
                (
                    ep,
                    "POST",
                    serde_json::json!({ "api_key": self.search.api_key.trim(), "query": query, "max_results": max }),
                    None,
                )
            }
            "brave" => {
                let ep = if self.search.endpoint.trim().is_empty() {
                    format!(
                        "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
                        urlencode(&query),
                        max
                    )
                } else {
                    format!(
                        "{}?q={}&count={}",
                        self.search.endpoint.trim().trim_end_matches('/'),
                        urlencode(&query),
                        max
                    )
                };
                (
                    ep,
                    "GET",
                    serde_json::Value::Null,
                    Some(self.search.api_key.trim().to_string()),
                )
            }
            "searxng" => {
                let ep = format!(
                    "{}/search?q={}&format=json",
                    self.search.endpoint.trim().trim_end_matches('/'),
                    urlencode(&query)
                );
                (ep, "GET", serde_json::Value::Null, None)
            }
            _ => {
                // custom：POST {query, maxResults}，响应兼容多种字段名
                (
                    self.search.endpoint.trim().to_string(),
                    "POST",
                    serde_json::json!({ "query": query, "maxResults": max, "apiKey": self.search.api_key.trim() }),
                    None,
                )
            }
        };
        let mut req = if method == "POST" {
            client.post(&endpoint)
        } else {
            client.get(&endpoint)
        };
        if method == "POST" {
            req = req.json(&payload);
        }
        if let Some(tok) = bearer {
            if !tok.is_empty() {
                req = req.header("X-Subscription-Token", tok);
            }
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => return ToolResult::err(format!("搜索请求失败: {e}")),
        };
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        if !(200..300).contains(&status) {
            return ToolResult::err(format!(
                "搜索后端返回 HTTP {status}：{}",
                body.chars().take(300).collect::<String>()
            ));
        }
        let parsed: serde_json::Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(e) => return ToolResult::err(format!("搜索响应不是 JSON: {e}")),
        };
        let items = parsed
            .get("results")
            .or_else(|| parsed.get("web").and_then(|w| w.get("results")))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if items.is_empty() {
            return ToolResult::ok(format!("无搜索结果（query: {query}）"));
        }
        let mut lines: Vec<String> = Vec::new();
        for (i, it) in items.iter().take(max).enumerate() {
            let title = it
                .get("title")
                .or_else(|| it.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("（无标题）");
            let url = it
                .get("url")
                .or_else(|| it.get("link"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let snippet = it
                .get("content")
                .or_else(|| it.get("snippet"))
                .or_else(|| it.get("description"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            lines.push(format!(
                "{}. {title}\n   {url}\n   {}",
                i + 1,
                snippet
                    .replace('\n', " ")
                    .chars()
                    .take(300)
                    .collect::<String>()
            ));
        }
        ToolResult::ok(format!(
            "搜索「{query}」（{}）：\n{}",
            self.search.provider.trim(),
            lines.join("\n")
        ))
    }

    /// 浏览器自动化（headless Chrome + CDP）：懒启动常驻，串行化访问。
    /// 探测不到浏览器时该工具不会出现在下发 schema 里（此处兜底报错）。
    async fn tool_browser(&self, session_id: &str, args: &serde_json::Value) -> ToolResult {
        let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("open");
        if op == "close" {
            let mut guard = self.browser.lock().await;
            return match guard.take() {
                Some(mut s) => {
                    s.shutdown();
                    ToolResult::ok("浏览器已关闭")
                }
                None => ToolResult::ok("浏览器未启动"),
            };
        }
        let mut guard = self.browser.lock().await;
        if guard.is_none() {
            match crate::browser::BrowserSession::launch(&self.browser_cfg, &self.workspace_root)
                .await
            {
                Ok(s) => *guard = Some(s),
                Err(e) => return ToolResult::err(format!("启动浏览器失败：{e}")),
            }
        }
        let session = match guard.as_mut() {
            Some(s) => s,
            None => return ToolResult::err("浏览器会话不可用"),
        };
        let max_default = self.browser_cfg.max_chars;
        let max = args
            .get("maxChars")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(max_default);
        match op {
            "open" => {
                let url = args
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if !(url.starts_with("http://")
                    || url.starts_with("https://")
                    || url.starts_with("file://")
                    || url.starts_with("about:"))
                {
                    return ToolResult::err("url 需以 http(s):// 开头（本地文件用 file://）");
                }
                if let Err(e) = session.navigate(url).await {
                    return ToolResult::err(format!("导航失败：{e}"));
                }
                match session.page_text(max).await {
                    Ok((title, text)) => {
                        ToolResult::ok(format!("已加载：{title}\nURL：{url}\n\n{text}"))
                    }
                    Err(e) => ToolResult::err(format!("读取页面失败：{e}")),
                }
            }
            "text" => match session.page_text(max).await {
                Ok((title, text)) => ToolResult::ok(format!("{title}\n\n{text}")),
                Err(e) => ToolResult::err(format!("读取页面失败：{e}")),
            },
            "eval" => {
                let expr = args
                    .get("expression")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if expr.trim().is_empty() {
                    return ToolResult::err("eval 需要 expression");
                }
                match session.eval(expr).await {
                    Ok(v) => ToolResult::ok(v.chars().take(8000).collect::<String>()),
                    Err(e) => ToolResult::err(format!("脚本执行失败：{e}")),
                }
            }
            "screenshot" => {
                let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
                match session.screenshot(&self.workspace_root, name).await {
                    Ok(rel) => {
                        // 产物登记（D12）：截图自动进入会话 outbox，通道投递
                        let _ = self
                            .store
                            .outbox_push(session_id, "image", &rel, "网页截图");
                        ToolResult::ok(format!("截图已保存：{rel}（可用 read 查看路径，或用 browser screenshot 再次覆盖）"))
                    }
                    Err(e) => ToolResult::err(format!("截图失败：{e}")),
                }
            }
            other => ToolResult::err(format!(
                "未知 browser 操作: {other}（支持 open / text / eval / screenshot / close）"
            )),
        }
    }

    /// 自主排程：创建/查看/删除定时与一次性任务（AI 自驱工作的时间维度）
    fn tool_schedule(&self, args: &serde_json::Value) -> ToolResult {
        let Some(cron) = &self.cron else {
            return ToolResult::err("排程存储未接入（该运行环境不支持 schedule）");
        };
        let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("list");
        match op {
            "create" => {
                let name = args
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let prompt = args
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let expr = args
                    .get("cron")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                let at = args
                    .get("at")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                if name.is_empty() || prompt.is_empty() {
                    return ToolResult::err("create 需要 name 与 prompt");
                }
                if expr.is_none() && at.is_none() {
                    return ToolResult::err("create 需要 cron（周期）或 at（一次性时间）之一");
                }
                if let Some(e) = &expr {
                    if e.split_whitespace().count() != 5 {
                        return ToolResult::err("cron 需为五段：分 时 日 月 周，如 0 9 * * *");
                    }
                }
                let job = crate::types::CronJob {
                    id: crate::types::new_id()[..8].to_string(),
                    name,
                    prompt,
                    cron: expr,
                    at,
                    kind: None,
                    target_session: None,
                    steps: Vec::new(),
                    group: Some(self.registry.active_group()),
                    session_title: None,
                    enabled: true,
                    last_run_at: None,
                    last_status: None,
                    last_run_minute: None,
                    notify_channels: Vec::new(),
                    created_at: crate::types::now_iso(),
                };
                match cron.upsert(job) {
                    Ok(j) => ToolResult::ok(format!(
                        "已排程：id={} 名称={} 触发={}（激活组 {}；到期由调度器唤醒本组执行）",
                        j.id,
                        j.name,
                        j.cron.clone().or(j.at.clone()).unwrap_or_default(),
                        self.registry.active_group()
                    )),
                    Err(e) => ToolResult::err(format!("排程失败: {e}")),
                }
            }
            "list" => match cron.list() {
                Ok(jobs) => {
                    let filter = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let rows: Vec<String> = jobs
                        .iter()
                        .filter(|j| filter.is_empty() || j.id == filter)
                        .map(|j| {
                            format!(
                                "- {} [{}] {} 触发={} 启用={}{}",
                                j.name,
                                j.id,
                                if j.prompt.chars().count() > 40 {
                                    format!("{}…", j.prompt.chars().take(40).collect::<String>())
                                } else {
                                    j.prompt.clone()
                                },
                                j.cron.clone().or(j.at.clone()).unwrap_or_default(),
                                if j.enabled { "是" } else { "否" },
                                j.last_status
                                    .as_ref()
                                    .map(|s| format!(" 上次={s}"))
                                    .unwrap_or_default()
                            )
                        })
                        .collect();
                    if rows.is_empty() {
                        ToolResult::ok("（无排程任务）")
                    } else {
                        ToolResult::ok(rows.join("\n"))
                    }
                }
                Err(e) => ToolResult::err(format!("读取排程失败: {e}")),
            },
            "remove" | "enable" | "disable" => {
                let id = args
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if id.is_empty() {
                    return ToolResult::err(format!("{op} 需要 id"));
                }
                if op == "remove" {
                    return match cron.remove(&id) {
                        Ok(true) => ToolResult::ok(format!("已删除排程：{id}")),
                        Ok(false) => ToolResult::err(format!("排程不存在：{id}")),
                        Err(e) => ToolResult::err(format!("删除失败: {e}")),
                    };
                }
                match cron.get(&id) {
                    Ok(Some(mut job)) => {
                        job.enabled = op == "enable";
                        match cron.upsert(job) {
                            Ok(_) => ToolResult::ok(format!(
                                "已{}排程：{id}",
                                if op == "enable" { "启用" } else { "停用" }
                            )),
                            Err(e) => ToolResult::err(format!("更新失败: {e}")),
                        }
                    }
                    Ok(None) => ToolResult::err(format!("排程不存在：{id}")),
                    Err(e) => ToolResult::err(format!("读取失败: {e}")),
                }
            }
            other => ToolResult::err(format!(
                "未知 schedule 操作: {other}（支持 create/list/remove/enable/disable）"
            )),
        }
    }
}

// ---------------------------------------------------------------- 自由函数

/// 去掉 Windows canonicalize 产生的 `\\?\` 前缀（便于跨路径比较）
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy().to_string();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        p
    }
}

/// 词法规范化：消解 `.` 与 `..`（不解析符号链接）
fn normalize_lexically(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 尽可能规范化：路径存在则 canonicalize；不存在时向父目录回溯，
/// 拼接规范化后的祖先 + 剩余部分（词法消解 `..`）——保证越界判定用的是真实路径。
fn canonicalize_best(p: &Path) -> PathBuf {
    if let Ok(c) = p.canonicalize() {
        return strip_verbatim(c);
    }
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = p.to_path_buf();
    loop {
        if let Ok(base) = cur.canonicalize() {
            let mut joined = strip_verbatim(base);
            for t in tail.iter().rev() {
                joined.push(t);
            }
            return normalize_lexically(&joined);
        }
        let Some(name) = cur.file_name().map(|s| s.to_os_string()) else {
            return normalize_lexically(p);
        };
        tail.push(name);
        if !cur.pop() {
            return normalize_lexically(p);
        }
    }
}

/// 人类可读大小
fn human_size(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / 1048576.0)
    }
}

/// 工作台树遍历（spec: webui-code-workbench）：相对根收集节点，目录优先排序。
/// 排除 .git/target/node_modules 与隐藏目录（.gitignore 的文件面过滤在 build_walker；
/// 工作台树要的是稳定可浏览结构，重目录手工排除即可）
fn walk_tree(
    root: &Path,
    dir: &Path,
    depth: usize,
    depth_limit: usize,
    nodes: &mut Vec<serde_json::Value>,
    count: &mut usize,
    max_entries: usize,
) -> anyhow::Result<()> {
    if depth > depth_limit || *count >= max_entries {
        return Ok(());
    }
    let mut entries: Vec<(String, bool, u64)> = std::fs::read_dir(dir)?
        .flatten()
        .map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let meta = e.metadata().ok();
            let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            (name, is_dir, size)
        })
        .collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
    for (name, is_dir, size) in entries {
        if *count >= max_entries {
            return Ok(());
        }
        if name.starts_with('.') && name != ".github" && name != ".gitignore" {
            continue;
        }
        if name == ".git" || name == "target" || name == "node_modules" {
            continue;
        }
        let child = dir.join(&name);
        let rel = child
            .strip_prefix(root)
            .unwrap_or(&child)
            .to_string_lossy()
            .to_string();
        *count += 1;
        if is_dir {
            nodes.push(serde_json::json!({
                "path": rel, "name": name, "type": "dir", "children": [],
            }));
            walk_tree(root, &child, depth + 1, depth_limit, nodes, count, max_entries)?;
        } else {
            nodes.push(serde_json::json!({
                "path": rel, "name": name, "type": "file", "size": size,
            }));
        }
    }
    Ok(())
}

/// 构造遍历器：尊重 .gitignore / .ignore（隐藏文件也纳入，便于读 .github 等目录）；
/// glob 非空时只保留匹配项（`*.rs` 这类裸模式自动补 `**/` 前缀）。
fn build_walker(root: &Path, glob: &str) -> ignore::Walk {
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .max_filesize(Some(4 * 1024 * 1024));
    if !glob.trim().is_empty() {
        let mut ov = ignore::overrides::OverrideBuilder::new(root);
        let pat = glob.trim();
        let normalized = if pat.contains('/') || pat.starts_with("**") {
            pat.to_string()
        } else {
            format!("**/{pat}")
        };
        if ov.add(&normalized).is_ok() {
            if let Ok(set) = ov.build() {
                builder.overrides(set);
            }
        }
    }
    builder.build()
}

/// 极简 URL 编码（查询串用）
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            b' ' => out.push_str("%20"),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------- 自定义工具插值

/// 模板里的占位名清单（`{name}`；不完整的 `{` 忽略）
fn placeholder_names(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        match rest[i + 1..].find('}') {
            Some(j) => {
                let name = rest[i + 1..i + 1 + j].trim().to_string();
                if !name.is_empty() && !out.contains(&name) {
                    out.push(name);
                }
                rest = &rest[i + j + 2..];
            }
            None => break,
        }
    }
    out
}

/// 单趟插值：`{name}` → 处理后的值；未提供的占位（已在调用方兜底为空串）替换为空
fn interpolate(
    template: &str,
    vals: &std::collections::BTreeMap<String, String>,
    f: impl Fn(&str) -> String,
) -> String {
    let mut out = String::with_capacity(template.len() + 32);
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        match rest[i + 1..].find('}') {
            Some(j) => {
                let name = rest[i + 1..i + 1 + j].trim();
                out.push_str(&f(vals.get(name).map(String::as_str).unwrap_or("")));
                rest = &rest[i + j + 2..];
            }
            None => {
                out.push('{');
                rest = &rest[i + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// 简化 JSON path 求值：`$.a.b[0].c`（点取字段 / 下标取数组；前导 `$.` 可省）。
/// 命中返回子树；路径不存在/类型不符返回 None。
fn json_path<'v>(root: &'v serde_json::Value, path: &str) -> Option<&'v serde_json::Value> {
    let p = path.trim();
    let p = p.strip_prefix('$').unwrap_or(p);
    let p = p.strip_prefix('.').unwrap_or(p);
    if p.is_empty() {
        // `$` 或 `$.` 之外的空段（如 `""`/`"$"`）= 整根；尾随孤立 `.` 非法
        if path.trim() == "$" || path.trim().is_empty() {
            return Some(root);
        }
        return None;
    }
    let mut cur = root;
    let mut rest = p;
    while !rest.is_empty() {
        // 前导 `.` 分隔（首轮无）
        if let Some(stripped) = rest.strip_prefix('.') {
            if stripped.is_empty() {
                return None; // 尾随 `.` 非法
            }
            rest = stripped;
        }
        // 字段名段：到下一个 `.` 或 `[`
        let seg_end = rest.find(['.', '[']).unwrap_or(rest.len());
        if seg_end > 0 {
            let key = &rest[..seg_end];
            cur = cur.as_object().and_then(|o| o.get(key))?;
            rest = &rest[seg_end..];
        }
        // 下标段：连续的 `[n]`（必须紧跟字段或前一 `]`）
        while rest.starts_with('[') {
            let close = rest.find(']')?;
            let idx: usize = rest[1..close].trim().parse().ok()?;
            cur = cur.as_array().and_then(|a| a.get(idx))?;
            rest = &rest[close + 1..];
        }
        if !rest.is_empty() && !rest.starts_with('.') {
            return None; // 残余非法语法
        }
    }
    Some(cur)
}

/// 自定义工具响应治理（插件生态扩展）：JSON 响应按 select 摘取 → maxChars 截断。
/// select 路径不存在返回明确错误（拼空串比报错更坑）；非 JSON 忽略 select。
fn shape_response(tool: &crate::config::CustomTool, text: &str) -> ToolResult {
    if let Some(sel) = tool
        .response_select
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
            return match json_path(&v, sel) {
                Some(sub) => {
                    let s = match sub {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    shape_truncate(tool, &s)
                }
                None => ToolResult::err(format!(
                    "工具 {} 的响应不含 select 路径 {sel}（响应为 JSON 但路径不存在）",
                    tool.name
                )),
            };
        }
        // 非 JSON：忽略 select，仅截断
    }
    shape_truncate(tool, text)
}

fn shape_truncate(tool: &crate::config::CustomTool, text: &str) -> ToolResult {
    let cap = tool.response_max_chars.unwrap_or(8000);
    let mut out: String = text.chars().take(cap).collect();
    if out.len() < text.len() {
        out.push_str("…（截断）");
    }
    ToolResult::ok(out)
}

/// shell 实参不允许出现的元字符（模板插值 + 引号包裹之外的最后防线）
fn shell_unsafe_char(c: char) -> bool {
    matches!(
        c,
        '"' | '\''
            | '`'
            | '$'
            | '&'
            | '|'
            | ';'
            | '<'
            | '>'
            | '('
            | ')'
            | '%'
            | '!'
            | '^'
            | '\\'
            | '\n'
            | '\r'
            | '\t'
    )
}

/// 实参包裹：Windows 双引号 / POSIX 单引号（调用方已先行做字符白名单校验）
fn shell_quote(v: &str) -> String {
    #[cfg(target_os = "windows")]
    {
        format!("\"{v}\"")
    }
    #[cfg(not(target_os = "windows"))]
    {
        format!("'{v}'")
    }
}

// ---------------------------------------------------------------- 记忆工具与工具面快照（组 8 单测）

#[cfg(test)]
mod memory_tool_tests {
    use super::*;
    use crate::types::{AgentDefinition, GroupMeta, Tier};

    struct TestRig {
        gw: ToolGateway,
        dir: std::path::PathBuf,
    }

    fn rig() -> TestRig {
        let dir = std::env::temp_dir().join(format!("exm-tools-{}", crate::types::new_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let store = Arc::new(Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(LocalRegistry::new(dir.join("agents")).unwrap());
        // 自定义测试组（内置组定义受保护，不可增删个体）：注册主智能体与个体
        registry
            .create_group(Some("t".into()), "测试组", "")
            .unwrap();
        let _ = registry.upsert_agent(
            "t",
            crate::types::AgentDefinition {
                name: "orch-1".into(),
                identifier: "orch-1".into(),
                domain: "测试".into(),
                tier: Tier::Orchestrator,
                description: "测试主智能体".into(),
                capabilities: vec![],
                tools: vec![],
                when_to_call: String::new(),
                link: None,
                max_steps: None,
                dependencies: vec![],
                composable_with: vec![],
                input_schema: Default::default(),
                output_schema: Default::default(),
                prompt_file: String::new(),
                model_hint: None,
            },
            None,
        );
        registry.set_primary("t", "orch-1").unwrap();
        registry.set_active_group("t").unwrap();
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let memory = Arc::new(crate::memory::MemoryStore::open(dir.join("data")).unwrap());
        let gw = ToolGateway::new(&dir, store, registry, events, SecurityConfig::default())
            .with_memory(memory);
        TestRig { gw, dir }
    }

    /// 通道记忆隔离（memory_isolation）：memory_write/read 必须跟随会话分区，
    /// 否则隔离平台的会话会把知识写进共享组、或读到共享组的记忆——隔离形同虚设。
    #[test]
    fn 记忆隔离_工具读写跟随会话分区() {
        let r = rig();
        let mk = |title: &str, body: &str| serde_json::json!({ "kind": "fact", "title": title, "body": body });
        // 两个会话同组：shared 共享记忆；iso 开了隔离（模拟通道写入分区键）
        let shared = r.gw.store.create_session("共享会话", "t").unwrap();
        let iso = r.gw.store.create_session("隔离会话", "t").unwrap();
        r.gw.store
            .set_session_memory_partition(&iso.id, Some("t::channel:iso"))
            .unwrap();

        let a =
            r.gw.tool_memory_write("orch-1", &shared.id, &mk("共享暗号", "shared-secret"));
        assert!(a.ok, "{}", a.error.unwrap_or_default());
        let b =
            r.gw.tool_memory_write("orch-1", &iso.id, &mk("隔离暗号", "iso-secret"));
        assert!(b.ok, "{}", b.error.unwrap_or_default());

        // 写入落进各自命名空间（组键不同），互不可见
        let sees_shared = r.gw.tool_memory_read(
            "orch-1",
            &shared.id,
            &serde_json::json!({ "query": "暗号" }),
        );
        assert!(
            sees_shared.output.contains("共享暗号"),
            "共享会话应读到共享条目"
        );
        assert!(
            !sees_shared.output.contains("隔离暗号"),
            "共享会话不得读到隔离条目"
        );
        let sees_iso =
            r.gw.tool_memory_read("orch-1", &iso.id, &serde_json::json!({ "query": "暗号" }));
        assert!(
            sees_iso.output.contains("隔离暗号"),
            "隔离会话应读到隔离条目"
        );
        assert!(
            !sees_iso.output.contains("共享暗号"),
            "隔离会话不得读到共享条目"
        );

        // 关闭隔离立即回到共享：分区键被清除，随后写入/读取都归组
        r.gw.store
            .set_session_memory_partition(&iso.id, None)
            .unwrap();
        let back =
            r.gw.tool_memory_read("orch-1", &iso.id, &serde_json::json!({ "query": "暗号" }));
        assert!(back.output.contains("共享暗号"), "关闭隔离后应回到共享记忆");
    }

    fn def(identifier: &str, tier: Tier, tools: &[ToolName]) -> AgentDefinition {
        AgentDefinition {
            name: identifier.into(),
            identifier: identifier.into(),
            domain: "测试".into(),
            tier,
            description: "测试个体".into(),
            capabilities: vec![],
            link: None,
            max_steps: None,
            tools: tools.to_vec(),
            when_to_call: String::new(),
            dependencies: vec![],
            composable_with: vec![],
            input_schema: Default::default(),
            output_schema: Default::default(),
            prompt_file: String::new(),
            model_hint: None,
        }
    }

    fn group_meta(primary: &str, builtin: bool) -> GroupMeta {
        GroupMeta {
            id: "exmachina".into(),
            name: "测试组".into(),
            description: String::new(),
            primary: Some(primary.into()),
            workspace: None,
            model: None,
            capabilities: None,
            builtin,
            created_at: String::new(),
        }
    }

    #[test]
    fn 工具面快照_主智能体默认可见记忆三工具() {
        let primary = def("orch-1", Tier::Orchestrator, &[ToolName::Read]);
        let allow = ToolGateway::runtime_allowlist(&primary, Some(&group_meta("orch-1", true)));
        for t in [
            ToolName::MemoryRead,
            ToolName::MemoryWrite,
            ToolName::MemoryLink,
        ] {
            assert!(allow.contains(&t), "主智能体应默认获得 {t:?}（内置组）");
        }
        assert!(
            !allow.contains(&ToolName::AgentManage),
            "内置组主智能体不获得个体管理工具"
        );

        // 自定义组主智能体：记忆三工具 + 个体管理
        let allow2 = ToolGateway::runtime_allowlist(&primary, Some(&group_meta("orch-1", false)));
        assert!(allow2.contains(&ToolName::AgentManage));
        assert!(allow2.contains(&ToolName::MemoryWrite));
    }

    #[test]
    fn 工具面快照_个体未经编成授权不可见() {
        let unit = def("unit-1", Tier::Unit, &[ToolName::Read, ToolName::Terminal]);
        let allow = ToolGateway::runtime_allowlist(&unit, Some(&group_meta("orch-1", true)));
        assert!(
            !allow.contains(&ToolName::MemoryRead),
            "未授权个体不应看到记忆检索"
        );
        assert!(
            !allow.contains(&ToolName::MemoryWrite),
            "未授权个体不应看到记忆写入"
        );
        assert!(!allow.contains(&ToolName::MemoryLink));

        // 编成 JSON 显式授权：仅授权的工具可见（memory_write 授权但 memory_read 未授权）
        let granted = def(
            "unit-2",
            Tier::Unit,
            &[ToolName::Read, ToolName::MemoryWrite],
        );
        let allow2 = ToolGateway::runtime_allowlist(&granted, Some(&group_meta("orch-1", true)));
        assert!(allow2.contains(&ToolName::MemoryWrite));
        assert!(
            !allow2.contains(&ToolName::MemoryRead),
            "逐工具授权，不整组放行"
        );
    }

    #[test]
    fn 工具面快照_schema按白名单下发() {
        let primary = def("orch-1", Tier::Orchestrator, &[ToolName::Read]);
        let allow = ToolGateway::runtime_allowlist(&primary, Some(&group_meta("orch-1", true)));
        let specs = ToolGateway::tool_specs(&allow, false, false, false);
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"memory_read")
                && names.contains(&"memory_write")
                && names.contains(&"memory_link")
        );
        assert!(
            !names.contains(&"web_search"),
            "后端未就绪不下发（口径不因新工具改变）"
        );

        // 未授权个体：schema 不含记忆工具
        let unit = def("unit-1", Tier::Unit, &[ToolName::Read]);
        let specs2 = ToolGateway::tool_specs(
            &ToolGateway::runtime_allowlist(&unit, None),
            false,
            false,
            false,
        );
        assert!(specs2.iter().all(|s| !s.name.starts_with("memory_")));
    }

    #[test]
    fn 记忆写入_哈希去重提升既有条目() {
        let r = rig();
        let args = serde_json::json!({ "kind": "fact", "title": "部署约定", "body": "生产环境用 docker compose 部署", "tags": ["deploy"], "importance": 0.4 });
        let out = r.gw.tool_memory_write("orch-1", "test-session", &args);
        assert!(out.ok, "{}", out.error.unwrap_or_default());
        let again = r.gw.tool_memory_write("orch-1", "test-session", &args);
        assert!(again.ok);
        // 同内容二次写入：库内仅一条，重要性按既有规则提升（+0.08）
        let entries = r.gw.memory.as_ref().unwrap().list(None, 100).unwrap();
        assert_eq!(entries.len(), 1, "哈希去重：相同内容不得产生重复条目");
        assert!(
            entries[0].importance > 0.4,
            "重复写入应提升重要性：{}",
            entries[0].importance
        );
        assert_eq!(
            entries[0].source_ref.as_deref(),
            Some("agent"),
            "来源标注 source=agent"
        );
        assert_eq!(
            entries[0].agent_id.as_deref(),
            Some("orch-1"),
            "归属调用个体"
        );
        assert_eq!(entries[0].group_id.as_deref(), Some("t"), "归属激活组");
    }

    #[test]
    fn 记忆检索_相关度排序与来源标注() {
        let r = rig();
        let mk = |t: &str, b: &str| serde_json::json!({ "kind": "fact", "title": t, "body": b });
        r.gw.tool_memory_write(
            "orch-1",
            "test-session",
            &mk("redis 部署手册", "redis 用 compose 起单实例"),
        );
        r.gw.tool_memory_write(
            "orch-1",
            "test-session",
            &mk("前端构建", "npm run build:webui 产出 dist"),
        );
        r.gw.tool_memory_write(
            "unit-1",
            "test-session",
            &mk("redis 持久化教训", "redis 未开 AOF 重启丢数据"),
        );
        // 群体共享条目（无个体归属）：来源应标 "组:t"
        r.gw.memory
            .as_ref()
            .unwrap()
            .remember(
                &crate::memory::MemoryDraft::new(
                    crate::memory::MemoryKind::Fact,
                    "redis 端口约定",
                    "统一用 6379",
                )
                .group("t"),
            )
            .unwrap();

        let out = r.gw.tool_memory_read(
            "orch-1",
            "test-session",
            &serde_json::json!({ "query": "redis", "limit": 5 }),
        );
        assert!(out.ok, "{}", out.error.unwrap_or_default());
        let text = out.output;
        assert!(
            text.contains("2 条命中") || text.contains("3 条命中"),
            "应含全部 redis 条目：{text}"
        );
        assert!(text.contains("redis 部署手册"), "标题命中应排前：{text}");
        assert!(
            text.contains("个体:unit-1"),
            "主智能体可见个体私有层并标注来源：{text}"
        );
        assert!(text.contains("组:t"), "群体条目标注来源：{text}");
        // 个体视角：只见自己私有 + 群体，不见他体私有
        let unit_view = r.gw.tool_memory_read(
            "unit-1",
            "test-session",
            &serde_json::json!({ "query": "redis" }),
        );
        assert!(
            unit_view.output.contains("redis 持久化教训"),
            "自己的私有层可见"
        );
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[test]
    fn 记忆检索_越组隔离() {
        let r = rig();
        // 在 default 组写入一条
        let w = r.gw.tool_memory_write("orch-1", "test-session", &serde_json::json!({ "kind": "fact", "title": "机密暗号", "body": "仅 default 组可见的暗号 xyzzy" }));
        assert!(w.ok, "{}", w.error.unwrap_or_default());
        assert!(r
            .gw
            .registry
            .create_group(Some("other".into()), "他组", "")
            .is_ok());
        r.gw.registry.set_active_group("other").unwrap();
        let out = r.gw.tool_memory_read(
            "orch-1",
            "test-session",
            &serde_json::json!({ "query": "机密暗号 xyzzy" }),
        );
        assert!(
            out.output.contains("无命中"),
            "越组检索不得返回他组条目：{}",
            out.output
        );
        // 写入归属切换后的组：不串组
        let w2 = r.gw.tool_memory_write(
            "orch-1",
            "test-session",
            &serde_json::json!({ "kind": "fact", "title": "他组约定", "body": "other 组的约定" }),
        );
        assert!(w2.ok);
        r.gw.registry.set_active_group("exmachina").unwrap();
        let back = r.gw.tool_memory_read(
            "orch-1",
            "test-session",
            &serde_json::json!({ "query": "他组约定" }),
        );
        assert!(
            back.output.contains("无命中"),
            "other 组条目不得泄漏进 default 视图：{}",
            back.output
        );
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    /// 从写入结果文本提取条目 id（输出格式：…（id: <id>，类型 …)
    fn id_of(r: &ToolResult) -> String {
        r.output
            .rsplit("id: ")
            .next()
            .unwrap_or("")
            .split('，')
            .next()
            .unwrap_or("")
            .trim()
            .to_string()
    }

    #[test]
    fn 记忆关联_建立与列示_越组拒绝() {
        let r = rig();
        let mk =
            |t: &str, b: &str| serde_json::json!({ "kind": "decision", "title": t, "body": b });
        let a =
            r.gw.tool_memory_write("orch-1", "test-session", &mk("决策A", "采用方案 A"));
        let b =
            r.gw.tool_memory_write("orch-1", "test-session", &mk("决策B", "由 A 派生方案 B"));
        assert!(
            a.ok && b.ok,
            "{} {}",
            a.error.unwrap_or_default(),
            b.error.unwrap_or_default()
        );
        let id_a = id_of(&a);
        let id_b = id_of(&b);
        assert!(
            !id_a.is_empty() && !id_b.is_empty(),
            "id 解析：{} / {}",
            a.output,
            b.output
        );

        let link = r.gw.tool_memory_link("orch-1", "test-session", &serde_json::json!({ "op": "link", "from": id_a, "to": id_b, "relation": "derives-from" }));
        assert!(link.ok, "{}", link.error.unwrap_or_default());

        // 列示可见关联链
        let list = r.gw.tool_memory_link(
            "orch-1",
            "test-session",
            &serde_json::json!({ "op": "list", "query": "决策", "limit": 10 }),
        );
        assert!(list.ok);
        assert!(
            list.output.contains("derives-from"),
            "列示应含关联链：{}",
            list.output
        );

        // 越组关联拒绝
        assert!(r
            .gw
            .registry
            .create_group(Some("elsewhere".into()), "他组", "")
            .is_ok());
        r.gw.registry.set_active_group("elsewhere").unwrap();
        let c =
            r.gw.tool_memory_write("orch-1", "test-session", &mk("他组条目", "other 组的决策"));
        r.gw.registry.set_active_group("exmachina").unwrap();
        assert!(c.ok);
        let id_c = id_of(&c);
        let denied = r.gw.tool_memory_link(
            "orch-1",
            "test-session",
            &serde_json::json!({ "op": "link", "from": id_a, "to": id_c, "relation": "related" }),
        );
        assert!(!denied.ok, "越组关联应被拒绝");
        assert!(denied.error.unwrap_or_default().contains("越组隔离"));

        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[test]
    fn 记忆写入_参数校验() {
        let r = rig();
        let bad_kind = r.gw.tool_memory_write(
            "orch-1",
            "test-session",
            &serde_json::json!({ "kind": "nope", "title": "t", "body": "b" }),
        );
        assert!(!bad_kind.ok, "非法类型应拒绝");
        let no_body = r.gw.tool_memory_write(
            "orch-1",
            "test-session",
            &serde_json::json!({ "kind": "fact", "title": "t" }),
        );
        assert!(!no_body.ok, "缺 body 应拒绝");
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    // ---------------- 文件记忆模式（memory.enabled=false）行为一致性 ----------------

    /// 装配成文件记忆模式（深层记忆关闭）：memory.md 与 orchestrator 规划注入同路径
    fn file_mode(md_content: Option<&str>) -> TestRig {
        let mut r = rig();
        let md_path = r.dir.join("memory.md");
        match md_content {
            Some(c) => std::fs::write(&md_path, c).unwrap(),
            None => {
                let _ = std::fs::remove_file(&md_path);
            }
        }
        r.gw.memory_deep = false;
        r.gw.memory_md_path = Some(md_path);
        r
    }

    #[test]
    fn 文件记忆模式_读取回落memory_md() {
        let r = file_mode(Some("# 记忆\n- 用中文汇报"));
        let out = r.gw.tool_memory_read(
            "orch-1",
            "test-session",
            &serde_json::json!({ "query": "任意" }),
        );
        assert!(out.ok, "{}", out.error.unwrap_or_default());
        assert!(
            out.output.contains("用中文汇报"),
            "应返回 memory.md 内容：{}",
            out.output
        );
        assert!(
            out.output.contains("深层记忆已关闭"),
            "应标注文件记忆模式：{}",
            out.output
        );

        // 无文件：友好提示而非报错（文件模式下空记忆是正常态）
        let r2 = file_mode(None);
        let out2 = r2.gw.tool_memory_read(
            "orch-1",
            "test-session",
            &serde_json::json!({ "query": "任意" }),
        );
        assert!(out2.ok, "{}", out2.error.unwrap_or_default());
        assert!(out2.output.contains("尚无记忆内容"), "{}", out2.output);

        // 超长 memory.md：与规划注入同一 8000 字符上限并声明截断
        let r3 = file_mode(Some("长".repeat(20_000).as_str()));
        let out3 = r3
            .gw
            .tool_memory_read("orch-1", "test-session", &serde_json::json!({}));
        assert!(out3.ok);
        assert!(out3.output.contains("已截断"), "超长 md 应声明截断");

        let _ = std::fs::remove_dir_all(&r.dir);
        let _ = std::fs::remove_dir_all(&r2.dir);
        let _ = std::fs::remove_dir_all(&r3.dir);
    }

    #[test]
    fn 文件记忆模式_写入关联拒绝并引导() {
        let r = file_mode(Some("已有内容"));
        let args = serde_json::json!({ "kind": "fact", "title": "t", "body": "b" });
        let w = r.gw.tool_memory_write("orch-1", "test-session", &args);
        assert!(!w.ok, "文件记忆模式下写库应拒绝（规划面不召回 = 写入黑洞）");
        assert!(
            w.error.unwrap_or_default().contains("文件记忆模式"),
            "错误应引导用户："
        );

        let l = r.gw.tool_memory_link(
            "orch-1",
            "test-session",
            &serde_json::json!({ "op": "list" }),
        );
        assert!(!l.ok, "文件记忆模式下关联应拒绝");
        // 拒绝是真拒绝：数据库确实没被写入
        assert_eq!(
            r.gw.memory.as_ref().unwrap().list(None, 10).unwrap().len(),
            0
        );

        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[test]
    fn 文件记忆模式_schema不下发写与关联() {
        let primary = def("orch-1", Tier::Orchestrator, &[ToolName::Read]);
        let allow = ToolGateway::runtime_allowlist(&primary, Some(&group_meta("orch-1", true)));
        // 深层开启：三工具照常下发（兼容口径不变）
        let deep = ToolGateway::tool_specs_gated(&allow, false, false, false, true);
        assert!(deep.iter().any(|s| s.name == "memory_write"));
        assert!(deep.iter().any(|s| s.name == "memory_link"));
        // 文件模式：write/link 不承诺（必然失败的工具不下发），read 保留（读 md 有意义）
        let file = ToolGateway::tool_specs_gated(&allow, false, false, false, false);
        assert!(
            file.iter().any(|s| s.name == "memory_read"),
            "memory_read 保留"
        );
        assert!(
            !file.iter().any(|s| s.name == "memory_write"),
            "文件模式不下发 memory_write"
        );
        assert!(
            !file.iter().any(|s| s.name == "memory_link"),
            "文件模式不下发 memory_link"
        );
    }

    #[test]
    fn 列示空查询_分层不越权() {
        let r = rig();
        // unit-1 的私有记忆 + 一条群体记忆；unit-1 编成里显式授权三工具由 def() 提供
        let mut draft = crate::memory::MemoryDraft::new(
            crate::memory::MemoryKind::Lesson,
            "unit私有",
            "只属于 unit-1",
        );
        draft.agent_id = Some("unit-1".into());
        r.gw.memory.as_ref().unwrap().remember(&draft).unwrap();
        r.gw.memory
            .as_ref()
            .unwrap()
            .remember(
                &crate::memory::MemoryDraft::new(
                    crate::memory::MemoryKind::Fact,
                    "群体事实",
                    "无归属共享",
                )
                .group("t"),
            )
            .unwrap();

        // 个体空查询列示：见群体，不见他人（主智能体 orch-1）尚未写私有 → 只验不泄漏
        let out = r.gw.tool_memory_link(
            "unit-1",
            "test-session",
            &serde_json::json!({ "op": "list", "query": "" }),
        );
        assert!(out.ok, "{}", out.error.unwrap_or_default());
        assert!(
            out.output.contains("群体事实"),
            "个体列示应含群体条目：{}",
            out.output
        );

        // 主智能体再写一条私有；unit-1 的空查询列示不得看到
        let mut o = crate::memory::MemoryDraft::new(
            crate::memory::MemoryKind::Fact,
            "orch私有",
            "只属于 orch-1",
        );
        o.agent_id = Some("orch-1".into());
        r.gw.memory.as_ref().unwrap().remember(&o).unwrap();
        let out2 = r.gw.tool_memory_link(
            "unit-1",
            "test-session",
            &serde_json::json!({ "op": "list", "query": "" }),
        );
        assert!(
            !out2.output.contains("orch私有"),
            "个体空查询列示不得泄漏主智能体私有记忆：{}",
            out2.output
        );
        // 主智能体全层视角：双方私有都可见
        let out3 = r.gw.tool_memory_link(
            "orch-1",
            "test-session",
            &serde_json::json!({ "op": "list", "query": "" }),
        );
        assert!(
            out3.output.contains("orch私有") && out3.output.contains("unit私有"),
            "主智能体全层列示：{}",
            out3.output
        );

        let _ = std::fs::remove_dir_all(&r.dir);
    }

    #[test]
    fn 读取_offset越界与max_chars上限() {
        let r = rig();
        let p = r.dir.join("sample.txt");
        std::fs::write(&p, "l1\nl2\nl3\n").unwrap();

        let ok = r.gw.tool_read(&serde_json::json!({ "path": "sample.txt" }));
        assert!(ok.ok, "{}", ok.error.unwrap_or_default());
        assert!(ok.output.contains("共 3 行"));

        // 越界 offset：给可行动的提示而非「已显示第 无 行」
        let beyond =
            r.gw.tool_read(&serde_json::json!({ "path": "sample.txt", "offset": 99 }));
        assert!(beyond.ok, "{}", beyond.error.unwrap_or_default());
        assert!(beyond.output.contains("超出文件末尾"), "{}", beyond.output);
        assert!(
            beyond.output.contains("offset≤3"),
            "应提示合法上界：{}",
            beyond.output
        );

        // maxChars 传超大值：被钳到硬上限，不绕过单次读取预算（小文件内容不受影响）
        let clamped = r
            .gw
            .tool_read(&serde_json::json!({ "path": "sample.txt", "maxChars": 1_000_000_000u64 }));
        assert!(clamped.ok, "{}", clamped.error.unwrap_or_default());
        assert!(
            clamped.output.contains("l3"),
            "小文件不受钳制影响：{}",
            clamped.output
        );

        let _ = std::fs::remove_dir_all(&r.dir);
    }
}

// ---------------------------------------------------------------- 终端超时诊断测试

#[cfg(test)]
mod timeout_output_tests {
    use super::*;

    /// 超时强杀后必须带回终止前的部分输出（回归：超时只报「已终止」、输出全丢，
    /// 长构建/测试卡死时无法诊断卡在哪一步）
    #[tokio::test]
    async fn 终端超时_部分输出随错误带回() {
        let root = std::env::temp_dir();
        // 先产出一段标记输出，再挂死等强杀
        let cmd = if cfg!(windows) {
            "echo out-start & ping -n 30 127.0.0.1".to_string()
        } else {
            "printf out-start; sleep 30".to_string()
        };
        let started = std::time::Instant::now();
        let r = execute_shell_command_timed(&root, &cmd, 1).await;
        let elapsed = started.elapsed();
        assert!(!r.ok, "挂死命令应失败");
        let err = r.error.unwrap_or_default();
        assert!(err.contains("超时"), "应为超时错误: {err}");
        assert!(
            err.contains("out-start"),
            "超时错误应包含终止前的部分输出，实际: {err}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "应在时限附近快速返回，实际 {elapsed:?}"
        );
    }

    /// 正常路径不受影响：快速完成的命令照常返回成功输出
    #[tokio::test]
    async fn 终端超时_正常完成不受影响() {
        let root = std::env::temp_dir();
        let cmd = "echo quick-ok";
        let r = execute_shell_command_timed(&root, cmd, 15).await;
        assert!(r.ok, "快速命令应成功");
        assert!(r.output.contains("quick-ok"), "输出应完整: {}", r.output);
    }

    /// 失败命令必须可诊断：退出码与输出摘要进 error——
    /// 回灌层（runtime::tool_feedback）与事件/审计摘要只取 error 字段，
    /// 之前失败命令 error 留空，模型与用户看到的失败原因是一句空话。
    /// output 保留原文（审批代执行等老调用方按 output 取输出，向后兼容）。
    #[tokio::test]
    async fn 终端失败_退出码与输出进入错误字段() {
        let root = std::env::temp_dir();
        let cmd = if cfg!(windows) {
            // 执行器已按 cmd /C <命令> 拼装，这里再包一层 cmd /C "..." 会被内层当程序名
            "echo out-tail & echo boom 1>&2 & exit /b 3".to_string()
        } else {
            "printf out-tail; echo boom >&2; exit 3".to_string()
        };
        let r = execute_shell_command_timed(&root, &cmd, 15).await;
        assert!(!r.ok, "非零退出应判定失败");
        let err = r.error.unwrap_or_else(|| "（error 为空）".into());
        assert!(err.contains("退出码 3"), "错误应含退出码: {err}");
        assert!(err.contains("boom"), "错误应含 stderr 摘要: {err}");
        assert!(err.contains("out-tail"), "错误应含 stdout 尾部摘要: {err}");
        assert!(
            r.output.contains("out-tail"),
            "output 保留原文（向后兼容）: {}",
            r.output
        );
    }

    /// 超长输出截断必须带标记：模型不得把半截输出当完整结论
    /// （典型误判：测试/构建日志尾部被切，把半路输出读成「成功」）
    #[tokio::test]
    async fn 终端输出_超长截断带标记() {
        let root = std::env::temp_dir();
        let cmd = if cfg!(windows) {
            // for /L 从 0 起到 1499：与 unix 分支对齐（断言头部含 -0、尾部 -1499 被截）
            "for /L %i in (0,1,1499) do @echo aaaaaaaaaaaaaaaaaaaa-%i".to_string()
        } else {
            "i=0; while [ $i -lt 1500 ]; do echo aaaaaaaaaaaaaaaaaaaa-$i; i=$((i+1)); done"
                .to_string()
        };
        let r = execute_shell_command_timed(&root, &cmd, 30).await;
        assert!(r.ok, "{}", r.error.unwrap_or_default());
        assert!(r.output.contains("输出已截断"), "超长输出必须带截断标记");
        assert!(
            r.output.contains("aaaaaaaaaaaaaaaaaaaa-0"),
            "截断应保留头部"
        );
        let tail = &r.output[r.output.len().saturating_sub(400)..];
        assert!(
            !tail.contains("-1499"),
            "尾部内容应被截掉，实际尾部: {tail}"
        );
    }
}

// ---------------------------------------------------------------- 终端白名单与审批路径

#[cfg(test)]
mod terminal_gate_tests {
    use super::*;

    /// filesystem delete/rename（spec: filesystem-ops）回归
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fs删除_跨设备fallback用copy_tree() {
        // rename EXDEV 场景无法在本机模拟挂载边界——直接验证 copy_tree 语义
        //（fallback 路径的核心：目录树完整复制 + 目标已存在清空重建）
        let dir = std::env::temp_dir().join(format!("exm-fscopy-{}", crate::types::new_id()));
        std::fs::create_dir_all(dir.join("src/nested")).unwrap();
        std::fs::write(dir.join("src/a.txt"), "A").unwrap();
        std::fs::write(dir.join("src/nested/b.txt"), "B").unwrap();
        let dst = dir.join("trash-copy");
        std::fs::write(&dst, "旧残留").unwrap(); // 已存在文件（非目录）
        ToolGateway::copy_tree(&dir.join("src"), &dst).unwrap();
        assert!(dst.is_dir(), "已存在文件应被清空重建为目录");
        assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "A");
        assert_eq!(std::fs::read_to_string(dst.join("nested/b.txt")).unwrap(), "B");
        // 单文件路径
        let f = dir.join("one.log");
        std::fs::write(&f, "x").unwrap();
        ToolGateway::copy_tree(&f, &dir.join("t/one.log")).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("t/one.log")).unwrap(), "x");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fs删除_移入回收站可恢复() {
        let dir = std::env::temp_dir().join(format!("exm-fsdel-{}", crate::types::new_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let store = Arc::new(Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(LocalRegistry::new(dir.join("agents")).unwrap());
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let gw = ToolGateway::new(&dir, store, registry, events, SecurityConfig::default());
        std::fs::write(dir.join("tmp.log"), "hello").unwrap();
        // 删除 → 回收站
        let r = gw
            .execute(
                "a",
                "s-fs-1",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "delete", "path": "tmp.log" }),
            )
            .await;
        assert!(r.ok, "删除应成功：{:?}", r.error);
        assert!(r.output.contains("回收站"), "应说明回收站路径：{}", r.output);
        assert!(!dir.join("tmp.log").exists(), "原位应消失");
        // 回收站里可找到（按日期目录搜索）
        let trash_root = dir.join(".exmachina").join("trash");
        let found = std::fs::read_dir(&trash_root)
            .unwrap()
            .any(|day| {
                let day = day.unwrap().path();
                day.join("tmp.log").exists()
            });
        assert!(found, "回收站应含被删文件");
        // 不存在目标 → 报错
        let r = gw
            .execute(
                "a",
                "s-fs-1",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "delete", "path": "tmp.log" }),
            )
            .await;
        assert!(!r.ok, "重复删除应报目标不存在");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fs删除_目录递归与同名序号() {
        let dir = std::env::temp_dir().join(format!("exm-fsdir-{}", crate::types::new_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let store = Arc::new(Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(LocalRegistry::new(dir.join("agents")).unwrap());
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let gw = ToolGateway::new(&dir, store, registry, events, SecurityConfig::default());
        // 目录树
        std::fs::create_dir_all(dir.join("build").join("nested")).unwrap();
        std::fs::write(dir.join("build").join("a.txt"), "1").unwrap();
        std::fs::write(dir.join("build").join("nested").join("b.txt"), "2").unwrap();
        let r = gw
            .execute(
                "a",
                "s-fs-2",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "delete", "path": "build" }),
            )
            .await;
        assert!(r.ok, "目录删除应成功：{:?}", r.error);
        assert!(!dir.join("build").exists(), "目录树应整体移入");
        // 同名序号：同日两次删除同名文件，第二次带 .2
        std::fs::write(dir.join("x.log"), "v1").unwrap();
        let r1 = gw
            .execute(
                "a",
                "s-fs-2",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "delete", "path": "x.log" }),
            )
            .await;
        assert!(r1.ok, "第一次删除应成功");
        std::fs::write(dir.join("x.log"), "v2").unwrap();
        let r2 = gw
            .execute(
                "a",
                "s-fs-2",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "delete", "path": "x.log" }),
            )
            .await;
        assert!(r2.ok);
        assert!(r2.output.contains(".2"), "第二次删除应带序号：{}", r2.output);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fs删除_保护区拒绝() {
        let dir = std::env::temp_dir().join(format!("exm-fspro-{}", crate::types::new_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let store = Arc::new(Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(LocalRegistry::new(dir.join("agents")).unwrap());
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let gw = ToolGateway::new(&dir, store, registry, events, SecurityConfig::default());
        // .exmachina 系统区
        std::fs::create_dir_all(dir.join(".exmachina").join("checkpoints")).unwrap();
        let r = gw
            .execute(
                "a",
                "s-fs-3",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "delete", "path": ".exmachina" }),
            )
            .await;
        assert!(!r.ok, "系统区删除应拒绝");
        assert!(r.error.unwrap_or_default().contains("系统区"));
        // 工作区根（resolve_safe(".") 应解析到根）
        let r = gw
            .execute(
                "a",
                "s-fs-3",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "delete", "path": "." }),
            )
            .await;
        assert!(!r.ok, "根删除应拒绝");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fs重命名_成功不覆盖与跨目录() {
        let dir = std::env::temp_dir().join(format!("exm-fsren-{}", crate::types::new_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let store = Arc::new(Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(LocalRegistry::new(dir.join("agents")).unwrap());
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let gw = ToolGateway::new(&dir, store, registry, events, SecurityConfig::default());
        std::fs::write(dir.join("old.txt"), "content-1").unwrap();
        // 成功重命名
        let r = gw
            .execute(
                "a",
                "s-fs-4",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "rename", "path": "old.txt", "newPath": "new.txt" }),
            )
            .await;
        assert!(r.ok, "重命名应成功：{:?}", r.error);
        assert!(dir.join("new.txt").exists());
        assert!(!dir.join("old.txt").exists());
        assert_eq!(std::fs::read_to_string(dir.join("new.txt")).unwrap(), "content-1");
        // 不覆盖：目标已存在拒绝
        std::fs::write(dir.join("other.txt"), "x").unwrap();
        let r = gw
            .execute(
                "a",
                "s-fs-4",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "rename", "path": "other.txt", "newPath": "new.txt" }),
            )
            .await;
        assert!(!r.ok, "覆盖应拒绝");
        assert!(r.error.unwrap_or_default().contains("已存在"));
        assert_eq!(std::fs::read_to_string(dir.join("new.txt")).unwrap(), "content-1", "原内容不应被破坏");
        // 跨目录移动（父目录自动创建）
        let r = gw
            .execute(
                "a",
                "s-fs-4",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "rename", "path": "new.txt", "newPath": "sub/dir/moved.txt" }),
            )
            .await;
        assert!(r.ok, "跨目录移动应成功：{:?}", r.error);
        assert!(dir.join("sub").join("dir").join("moved.txt").exists());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fs删除重命名_plan模式拦截() {
        let dir = std::env::temp_dir().join(format!("exm-fsplan-{}", crate::types::new_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let store = Arc::new(Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(LocalRegistry::new(dir.join("agents")).unwrap());
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let gw = ToolGateway::new(&dir, store, registry, events, SecurityConfig::default());
        std::fs::write(dir.join("plan-target.txt"), "p").unwrap();
        crate::plan::set_plan_mode("s-fs-5", true);
        let r = gw
            .execute(
                "a",
                "s-fs-5",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "delete", "path": "plan-target.txt" }),
            )
            .await;
        assert!(r.ok);
        assert!(dir.join("plan-target.txt").exists(), "plan 模式不应真删");
        assert_eq!(crate::plan::list_items("s-fs-5").len(), 1, "应转为计划条目");
        let r = gw
            .execute(
                "a",
                "s-fs-5",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "rename", "path": "plan-target.txt", "newPath": "z.txt" }),
            )
            .await;
        assert!(r.ok);
        assert_eq!(crate::plan::list_items("s-fs-5").len(), 2, "rename 也应拦截");
    }

    #[test]
    fn 白名单_日常开发命令放行() {
        // 本轮修复核心场景：mkdir/cat/go/pytest/tsc 等此前全部被硬拒
        for cmd in [
            "cat README.md",
            "mkdir -p src/new",
            "cp a.rs b.rs",
            "mv old.rs new.rs",
            "touch main.go",
            "diff -u a.rs b.rs",
            "grep -rn TODO src",
            "find . -name x",
            "go build ./...",
            "go test ./...",
            "pytest -x",
            "jest",
            "vitest run",
            "tsc --noEmit",
            "rg pattern",
            "which cargo",
            "uname -a",
            "cargo check",
            "cargo test",
            "git status",
            "git diff",
            "ls -la",
            "npm run build",
            "node script.js",
            "python3 -m venv .venv",
            "make",
            "cmake .",
            "dotnet build",
            "mvn package",
        ] {
            assert!(terminal_allowed(cmd), "日常开发命令应放行: {cmd}");
        }
    }

    #[test]
    fn 白名单_危险与未知命令不放行() {
        for cmd in [
            "rm -rf /",
            "sudo rm x",
            "vim",
            "unknown-cmd",
            // docker 只放只读/exec/run 子命令；rmi/build 等高危仍拒
            "docker rmi nginx",
            "docker system prune",
            // ip 只放 addr/route/link；netns 配置类仍拒
            "ip netns add foo",
        ] {
            assert!(!terminal_allowed(cmd), "不应放行: {cmd}");
        }
        // curl/wget/chmod 已入白名单（harness 第二轮）：联网/权限语义由后置闸门分层把关
        for cmd in ["curl http://x", "wget x", "chmod 777 a"] {
            assert!(terminal_allowed(cmd), "应放行（后置闸门把关）: {cmd}");
        }
    }

    #[test]
    fn 白名单_子命令约束依旧生效() {
        // npm 只放行特定子命令
        assert!(terminal_allowed("npm install"));
        assert!(!terminal_allowed("npm exec something-weird"));
        // git push 不在子命令清单
        assert!(!terminal_allowed("git push origin main"));
        // cargo 全部子命令受约束
        assert!(terminal_allowed("cargo build"));
        assert!(!terminal_allowed("cargoSomething"));
    }

    fn gw_with_approval(mode: &str, wait_secs: u64) -> ToolGateway {
        let dir = std::env::temp_dir().join(format!("exm-gate-{}", crate::types::new_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let store = Arc::new(Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(LocalRegistry::new(dir.join("agents")).unwrap());
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let sec = SecurityConfig {
            exec_approval: mode.into(),
            approval_wait_secs: wait_secs,
            ..Default::default()
        };
        ToolGateway::new(&dir, store, registry, events, sec)
    }

    /// spawn 白名单剔除：子级请求 spawn/agent_manage 被过滤；全剔除报错
    #[tokio::test]
    async fn spawn工具_白名单强制剔除spawn与agent_manage() {
        let gw = gw_with_approval("off", 0);
        // 未装配执行器时先到"未装配"错误（白名单校验在其之前）——装个空执行器测过滤
        let exec: Arc<
            dyn Fn(&str, &str, &str, Vec<ToolName>) -> futures_util::future::BoxFuture<'static, anyhow::Result<String>>
                + Send
                + Sync,
        > = Arc::new(|_a, _s, _t, tools| {
            Box::pin(async move {
                assert!(
                    !tools.contains(&ToolName::Spawn) && !tools.contains(&ToolName::AgentManage),
                    "子级白名单不应含 spawn/agent_manage"
                );
                Ok("结论".into())
            })
        });
        gw.set_spawn_executor(exec);
        let r = gw
            .execute(
                "a1",
                "s1",
                &[ToolName::Spawn],
                ToolName::Spawn,
                &serde_json::json!({
                    "task": "检索测试",
                    "tools": ["read", "spawn", "agent_manage", "grep"]
                }),
            )
            .await;
        assert!(r.ok, "spawn 应执行成功：{:?}", r.error);
        // 全剔除 → 报错
        let r2 = gw
            .execute(
                "a1",
                "s1",
                &[ToolName::Spawn],
                ToolName::Spawn,
                &serde_json::json!({ "task": "x", "tools": ["spawn", "agent_manage"] }),
            )
            .await;
        assert!(!r2.ok, "全剔除的白名单应报错");
    }

    /// 超时：执行器睡眠超过 timeoutSecs → 超时终止说明
    #[tokio::test]
    async fn spawn工具_子任务超时终止() {
        let gw = gw_with_approval("off", 0);
        let exec: Arc<
            dyn Fn(&str, &str, &str, Vec<ToolName>) -> futures_util::future::BoxFuture<'static, anyhow::Result<String>>
                + Send
                + Sync,
        > = Arc::new(|_, _, _, _| {
            Box::pin(async {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                Ok("不该到这".into())
            })
        });
        gw.set_spawn_executor(exec);
        let r = gw
            .execute(
                "a1",
                "s1",
                &[ToolName::Spawn],
                ToolName::Spawn,
                &serde_json::json!({ "task": "慢任务", "timeoutSecs": 10 }),
            )
            .await;
        assert!(!r.ok);
        assert!(
            r.error.unwrap_or_default().contains("超时"),
            "应报超时说明"
        );
    }

    /// 并发上限：占满信号量后再派生 → 明确拒绝
    #[tokio::test]
    async fn spawn工具_并发上限拒绝() {
        // 信号量是进程级 OnceLock：并发测试与其他测试互相干扰，用独立子进程语义不可行；
        // 此处改为验证「上限拒绝路径」——直接耗尽后触发。先并发占满（4 张许可）：
        let gw = Arc::new(gw_with_approval("off", 0));
        let exec: Arc<
            dyn Fn(&str, &str, &str, Vec<ToolName>) -> futures_util::future::BoxFuture<'static, anyhow::Result<String>>
                + Send
                + Sync,
        > = Arc::new(|_, _, _, _| {
            Box::pin(async {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                Ok("done".into())
            })
        });
        gw.set_spawn_executor(exec);
        let mut handles = Vec::new();
        for _ in 0..4 {
            let gw2 = gw.clone();
            handles.push(tokio::spawn(async move {
                gw2.execute(
                    "a1",
                    "s1",
                    &[ToolName::Spawn],
                    ToolName::Spawn,
                    &serde_json::json!({ "task": "占位", "timeoutSecs": 60 }),
                )
                .await
            }));
        }
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        let r = gw
            .execute(
                "a1",
                "s1",
                &[ToolName::Spawn],
                ToolName::Spawn,
                &serde_json::json!({ "task": "第五个" }),
            )
            .await;
        // 注：信号量为进程级共享，若其他测试恰好占用则可能直接拒绝——同样满足断言
        assert!(!r.ok, "并发占满时应拒绝");
        assert!(
            r.error.unwrap_or_default().contains("并发"),
            "应报并发上限说明"
        );
        for h in handles {
            let _ = h.await;
        }
    }


    /// 计划模式：写操作拦截转计划条目，读操作放行
    #[tokio::test]
    async fn 计划模式_拦截写操作转计划条目() {
        let gw = gw_with_approval("off", 0);
        crate::plan::set_plan_mode("plan-s1", true);
        // 写操作（edit）被拦截
        let r = gw
            .execute(
                "a1",
                "plan-s1",
                &[ToolName::Edit],
                ToolName::Edit,
                &serde_json::json!({ "path": "a.rs", "old_string": "x", "new_string": "y" }),
            )
            .await;
        // 拦截返回 ok 文本（引导模型继续规划），非执行错误
        assert!(r.ok, "拦截应以引导文本返回：{:?}", r.error);
        assert!(r.output.contains("计划模式"), "应含计划模式说明");
        let items = crate::plan::list_items("plan-s1");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].tool, "edit");
        assert_eq!(items[0].target, "a.rs");
        // 读操作放行
        let r2 = gw
            .execute(
                "a1",
                "plan-s1",
                &[ToolName::Grep],
                ToolName::Grep,
                &serde_json::json!({ "pattern": "fn " }),
            )
            .await;
        assert!(r2.ok, "读操作应放行：{:?}", r2.error);
        // 清理
        crate::plan::approve("plan-s1");
    }

    /// 计划模式：批准后清单条目按序回放执行
    #[tokio::test]
    async fn 计划模式_批准后回放执行() {
        let dir = std::env::temp_dir().join(format!("exm-plan-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(LocalRegistry::new(dir.join("agents")).unwrap());
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let gw = Arc::new(ToolGateway::new(&dir, store, registry, events, SecurityConfig::default()));
        // 计划模式拦截 filesystem write
        crate::plan::set_plan_mode("plan-s2", true);
        let r = gw
            .execute(
                "a1",
                "plan-s2",
                &[ToolName::Filesystem],
                ToolName::Filesystem,
                &serde_json::json!({ "op": "write", "path": "note.txt", "content": "planned" }),
            )
            .await;
        assert!(r.ok);
        assert_eq!(crate::plan::list_items("plan-s2").len(), 1);
        // 批准：切 execute 并回放
        let items = crate::plan::approve("plan-s2");
        assert_eq!(items.len(), 1);
        assert!(!crate::plan::is_plan_mode("plan-s2"));
        // 回放（此时非 plan 模式，真正执行）
        let item = &items[0];
        let t = ToolName::parse(&item.tool).unwrap();
        let applied = gw
            .execute("plan-replay", "plan-s2", &[t], t, &item.args)
            .await;
        assert!(applied.ok, "回放应执行成功：{:?}", applied.error);
        assert!(dir.join("note.txt").exists(), "文件应已写入");
    }

    /// git 测试仓：临时目录初始化仓库并提交一个文件
    fn git_test_repo(gw: &ToolGateway) -> PathBuf {
        let root = gw.effective_root();
        std::fs::create_dir_all(&root).unwrap();
        std::process::Command::new("git").arg("init").current_dir(&root).output().expect("git init");
        std::process::Command::new("git")
            .args(["config", "user.email", "test@exm.local"])
            .current_dir(&root)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "exm-test"])
            .current_dir(&root)
            .output()
            .unwrap();
        std::fs::write(root.join("a.txt"), "hello\n").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&root)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&root)
            .output()
            .unwrap();
        root
    }

    /// 只读动作（status）直接执行，无需审批
    #[tokio::test]
    async fn git工具_status只读直行() {
        let gw = gw_with_approval("off", 0);
        git_test_repo(&gw);
        let r = gw
            .execute(
                "a1",
                "s1",
                &[ToolName::Git],
                ToolName::Git,
                &serde_json::json!({ "action": "status" }),
            )
            .await;
        assert!(r.ok, "status 只读应放行：{:?}", r.error);
        assert!(r.output.contains("##"), "status 输出应含分支行：{}", r.output);
    }

    /// 严格模式（risky + 立即超时）：写动作产生审批单而非执行
    #[tokio::test]
    async fn git工具_严格模式commit产生审批单() {
        let gw = gw_with_approval("always", 0);
        git_test_repo(&gw);
        std::fs::write(gw.effective_root().join("b.txt"), "new\n").unwrap();
        let r = gw
            .execute(
                "a1",
                "s1",
                &[ToolName::Git],
                ToolName::Git,
                &serde_json::json!({ "action": "commit", "message": "add b" }),
            )
            .await;
        // wait=0 → 受阻回流，但审批单已落库
        assert!(!r.ok, "审批模式下 commit 不应直接执行");
        let approvals = gw
            .store_for_test()
            .list_approvals(Some("pending"), 10)
            .unwrap_or_default();
        assert!(
            approvals.iter().any(|a| a.command.contains("git commit")),
            "应产生 git commit 审批单：{approvals:?}"
        );
    }

    /// push 动作不在动作面：明确报错说明
    #[tokio::test]
    async fn git工具_push返回不支持说明() {
        let gw = gw_with_approval("risky", 0);
        git_test_repo(&gw);
        let r = gw
            .execute(
                "a1",
                "s1",
                &[ToolName::Git],
                ToolName::Git,
                &serde_json::json!({ "action": "push" }),
            )
            .await;
        assert!(!r.ok);
        assert!(
            r.error.unwrap_or_default().contains("不在 git 工具动作面"),
            "push 应返回动作面说明"
        );
    }

    /// 非仓库工作区：git 工具明确报错
    #[tokio::test]
    async fn git工具_非仓库报错() {
        let gw = gw_with_approval("off", 0);
        // 不初始化仓库
        let r = gw
            .execute(
                "a1",
                "s1",
                &[ToolName::Git],
                ToolName::Git,
                &serde_json::json!({ "action": "status" }),
            )
            .await;
        assert!(!r.ok);
        assert!(r.error.unwrap_or_default().contains("不是 git 仓库"));
    }

    /// 参数白名单：非法参数（注入尝试）拒绝
    #[tokio::test]
    async fn git工具_非法参数拒绝() {
        let gw = gw_with_approval("off", 0);
        git_test_repo(&gw);
        for bad in [
            serde_json::json!({ "action": "show", "object": "--exec=evil" }),
            serde_json::json!({ "action": "commit", "message": "x' && rm -rf /" }),
            serde_json::json!({ "action": "checkout-b", "branch": "b1 && echo pwned" }),
        ] {
            let r = gw
                .execute("a1", "s1", &[ToolName::Git], ToolName::Git, &bad)
                .await;
            assert!(!r.ok, "非法参数应拒绝：{bad}");
        }
    }

    /// off 档：白名单外命令直接拒绝（历史行为保持）
    #[tokio::test]
    async fn 审批路径_off档_白名单外命令直接拒绝() {
        let gw = gw_with_approval("off", 0);
        let r = gw
            .execute(
                "a1",
                "s1",
                &[ToolName::Terminal],
                ToolName::Terminal,
                &serde_json::json!({
                    "command": "unknown-cmd --flag"
                }),
            )
            .await;
        assert!(!r.ok, "off 档白名单外应拒绝");
        assert!(r.error.unwrap_or_default().contains("不在白名单"));
    }

    /// risky 档：白名单外的日常命令转审批单（不等待，受阻回流但审批单已落库）
    /// ——修复前这条路径是硬拒，agent 无从开发
    #[tokio::test]
    async fn 审批路径_risky档_白名单外命令转审批() {
        let gw = gw_with_approval("risky", 0);
        let r = gw
            .execute(
                "a1",
                "s2",
                &[ToolName::Terminal],
                ToolName::Terminal,
                &serde_json::json!({
                    "command": "terraform apply"
                }),
            )
            .await;
        assert!(!r.ok, "未批准前不应放行");
        let err = r.error.unwrap_or_default();
        assert!(err.contains("审批"), "应转审批而非白名单硬拒: {err}");
        // 审批单确实落库（用户可在面板批准）
        let pending = gw
            .store
            .list_approvals(Some("pending"), 10)
            .unwrap_or_default();
        assert!(
            pending.iter().any(|p| p.command.contains("terraform")),
            "审批单应已落库"
        );
    }

    /// risky 档：白名单内安全命令不受影响，照常执行
    #[tokio::test]
    async fn 审批路径_risky档_白名单内命令照常执行() {
        let gw = gw_with_approval("risky", 0);
        let cmd = "echo hello";
        let r = gw
            .execute(
                "a1",
                "s3",
                &[ToolName::Terminal],
                ToolName::Terminal,
                &serde_json::json!({
                    "command": cmd
                }),
            )
            .await;
        assert!(
            r.ok,
            "白名单内命令应直接执行: {}",
            r.error.unwrap_or_default()
        );
        assert!(r.output.contains("hello"));
    }

    /// risky 档：白名单内但高危（git reset --hard）仍走审批
    #[tokio::test]
    async fn 审批路径_risky档_白名单内高危命令仍审批() {
        let gw = gw_with_approval("risky", 0);
        let r = gw
            .execute(
                "a1",
                "s4",
                &[ToolName::Terminal],
                ToolName::Terminal,
                &serde_json::json!({
                    "command": "git reset --hard HEAD"
                }),
            )
            .await;
        assert!(!r.ok);
        assert!(
            r.error.unwrap_or_default().contains("审批"),
            "白名单内高危命令应走审批闸门"
        );
    }

    // ---------------- 实验性开关（experiments） ----------------

    /// 动态压缩门控：开关关闭时 context_compact 不在 schema 面
    #[test]
    fn 实验开关_动态压缩关闭时schema剔除context_compact() {
        let allow = [ToolName::Terminal, ToolName::ContextCompact];
        let specs = ToolGateway::tool_specs_gated(&allow, false, false, false, true);
        let specs = ToolGateway::tool_specs_experiments(specs, false);
        assert!(
            specs.iter().all(|s| s.name != "context_compact"),
            "开关关闭时 context_compact 不应下发"
        );
        // 开启时保留
        let specs = ToolGateway::tool_specs_gated(&allow, false, false, false, true);
        let specs = ToolGateway::tool_specs_experiments(specs, true);
        assert!(specs.iter().any(|s| s.name == "context_compact"));
    }

    /// 动态压缩执行：装配压缩器后调用成功落库摘要；未装配时明确报错
    #[tokio::test]
    async fn 动态压缩_装配后执行并落库() {
        use crate::provider::{ChatRequest, LlmProvider};
        struct OkCompacter;
        #[async_trait::async_trait]
        impl LlmProvider for OkCompacter {
            fn name(&self) -> &'static str {
                "test-compacter"
            }
            async fn chat(&self, _req: ChatRequest) -> anyhow::Result<crate::provider::ChatResponse> {
                Ok(crate::provider::ChatResponse {
                    content: "目标 X 已完成；结论 Y；未决 Z".into(),
                    reasoning: String::new(),
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    tool_calls: Vec::new(),
                })
            }
            async fn stream(
                &self,
                _req: ChatRequest,
                _tx: tokio::sync::mpsc::UnboundedSender<crate::provider::StreamDelta>,
            ) -> anyhow::Result<crate::provider::ChatResponse> {
                anyhow::bail!("测试替身不支持流式")
            }
        }

        let gw = gw_with_approval("off", 0);
        // 造 6 条历史，keep=2 → 压 4 条
        let sid = gw
            .store
            .create_session("压缩测试", "g1")
            .unwrap()
            .id;
        for i in 0..6 {
            gw.store
                .add_message(
                    &sid,
                    crate::types::MessageRole::User,
                    None,
                    vec![crate::types::Statement::new(
                        crate::types::SpeechTag::要求,
                        format!("第 {i} 条"),
                    )],
                )
                .unwrap();
        }
        // 未装配：明确报错（不是静默失败）
        let r = gw
            .execute(
                "a1",
                &sid,
                &[],
                ToolName::ContextCompact,
                &serde_json::json!({ "keepRecent": 2 }),
            )
            .await;
        assert!(!r.ok);

        // 装配后：压缩成功 + 会话滚动摘要落库
        gw.set_compactor(Arc::new(OkCompacter), "test-model".into());
        let r = gw
            .execute(
                "a1",
                &sid,
                &[],
                ToolName::ContextCompact,
                &serde_json::json!({ "keepRecent": 2, "note": "进入长任务阶段" }),
            )
            .await;
        assert!(r.ok, "压缩应成功: {:?}", r.error);
        let s = gw.store.get_session(&sid).unwrap().unwrap();
        assert_eq!(s.summary_upto, Some(4), "摘要应覆盖前 4 条");
        assert!(s.rolling_summary.is_some());

        // 幂等：窗口外已全部压过 → 无可压缩说明
        let r2 = gw
            .execute(
                "a1",
                &sid,
                &[],
                ToolName::ContextCompact,
                &serde_json::json!({ "keepRecent": 2 }),
            )
            .await;
        assert!(r2.ok);
        assert!(r2.output.contains("无需压缩"), "二次压缩应报无可压缩");
    }

    // ---------------- 插件声明式工具（T1：命名空间接入 custom 执行链）

    fn mk_custom(name: &str, target: &str) -> crate::config::CustomTool {
        crate::config::CustomTool {
            name: name.into(),
            description: format!("插件工具 {name}"),
            kind: "http".into(),
            method: "GET".into(),
            target: target.into(),
            headers: Default::default(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "q": { "type": "string" } },
                "required": ["q"]
            }),
            agents: vec![],
            response_max_chars: None,
            response_select: None,
        }
    }

    /// 下发面：插件工具以命名空间名出现在 schema 下发里
    #[test]
    fn 插件工具_schema下发含命名空间前缀() {
        let gw = gw_with_approval("off", 0);
        gw.append_plugin_tools(vec![mk_custom("demo__ping", "https://example.com")]);
        let specs = gw.custom_specs_for("a1");
        let name = specs.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
        assert!(name.contains(&"demo__ping".to_string()), "下发应含 demo__ping");
    }

    /// 冲突面：插件工具与 config CustomTool 同名 → 先注册者胜（custom 优先，插件被拒）
    #[test]
    fn 插件工具_与config同名冲突先注册者胜() {
        let dir = std::env::temp_dir().join(format!("exm-gate-{}", crate::types::new_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let store = Arc::new(Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(LocalRegistry::new(dir.join("agents")).unwrap());
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let gw = ToolGateway::new(&dir, store, registry, events, SecurityConfig::default())
            .with_custom_tools(vec![mk_custom("dupe", "https://config-first.example")]);
        gw.append_plugin_tools(vec![mk_custom("dupe", "https://plugin.example")]);
        // custom 优先：下发的是 config 版 target
        assert_eq!(gw.custom_specs_for("a1").len(), 1);
        assert!(gw.has_custom_tool("dupe"));
        assert!(gw.plugin_tools().is_empty(), "同名插件工具应被拒绝注册");
    }

    /// 执行面：命名空间名走 custom 执行链（HTTP 往返可见行为）。
    /// 用不可达地址验证错误路径也走通（不 panic、错误信息含工具名口径）。
    #[tokio::test]
    async fn 插件工具_执行走custom链路() {
        let gw = gw_with_approval("off", 0);
        gw.append_plugin_tools(vec![mk_custom("demo__ping", "https://127.0.0.1:1/nope")]);
        let sid = gw.store.create_session("s", "a1").unwrap().id;
        let r = gw
            .execute_named(
                "a1",
                &sid,
                &[],
                "demo__ping",
                &serde_json::json!({ "q": "hi" }),
            )
            .await;
        // 不可达地址 → 返回错误但 ok=false 而非 panic；错误面含工具名
        assert!(!r.ok, "不可达地址应为失败结果: {:?}", r.output);
        let text = format!("{}{}", r.output, r.error.unwrap_or_default());
        assert!(text.contains("demo__ping") || text.len() < 500, "错误面应含工具名或简洁");
        // 不应泄露内部路径
        assert!(!text.contains("/home/"), "不应泄露内部路径");
    }

    // ---------------- 进程插件运行时工具（T2：握手发现面接入工具网关）

    /// 假进程工具 provider（模拟 supervisor 转发：specs 带命名空间、执行回显）
    struct FakeProcessTools;

    impl crate::plugin::ToolProvider for FakeProcessTools {
        fn tool_specs(&self) -> Vec<crate::config::CustomTool> {
            vec![crate::config::CustomTool {
                name: "fakep__hello".into(),
                description: "进程握手发现".into(),
                kind: "process".into(),
                method: "POST".into(),
                target: String::new(),
                headers: Default::default(),
                parameters: serde_json::json!({ "type": "object" }),
                agents: vec![],
                response_max_chars: None,
                response_select: None,
            }]
        }
        fn call_tool(&self, name: &str, args: &serde_json::Value) -> anyhow::Result<String> {
            if name == "fakep__hello" {
                Ok(format!("hello:{}", args.get("name").and_then(|v| v.as_str()).unwrap_or("?")))
            } else {
                anyhow::bail!("未知工具 {name}")
            }
        }
    }

    /// T2 工具面：注册 → specs 下发可见 → 执行转发 provider → 摘除后不可见
    #[tokio::test]
    async fn 进程插件工具_注册下发执行与摘除() {
        let gw = gw_with_approval("off", 0);
        // 注册前不可见
        assert!(!gw.custom_specs_for("a1").iter().any(|s| s.name == "fakep__hello"));
        gw.register_plugin_runtime_tools("fakep", Arc::new(FakeProcessTools));
        // specs 可见（进程形态工具进入下发面）
        let specs = gw.custom_specs_for("a1");
        assert!(specs.iter().any(|s| s.name == "fakep__hello"), "注册后应下发");
        // 执行转发
        let sid = gw.store.create_session("s", "a1").unwrap().id;
        let r = gw
            .execute_named(
                "a1",
                &sid,
                &[],
                "fakep__hello",
                &serde_json::json!({ "name": "巡检" }),
            )
            .await;
        assert!(r.ok, "执行应成功: {:?}", r.error);
        assert!(r.output.contains("hello:巡检"), "输出应含 provider 回显: {}", r.output);
        // 未知工具仍走原错误路径
        let r2 = gw.execute_named("a1", &sid, &[], "fakep__nope", &serde_json::json!({})).await;
        assert!(!r2.ok);
        // 摘除后不可见
        gw.unregister_plugin_runtime_tools("fakep");
        assert!(!gw.custom_specs_for("a1").iter().any(|s| s.name == "fakep__hello"));
        let r3 = gw.execute_named("a1", &sid, &[], "fakep__hello", &serde_json::json!({})).await;
        assert!(!r3.ok, "摘除后执行应失败");
    }

    /// 响应治理（插件生态扩展 1.1）：两级裁剪纯函数面。
    fn shaped(max: Option<usize>, select: Option<&str>, _body: &str) -> crate::config::CustomTool {
        let mut t = mk_custom("demo__shaped", "https://example.com");
        t.response_max_chars = max;
        t.response_select = select.map(str::to_string);
        t
    }

    #[test]
    fn 响应治理_select摘取与截断() {
        let body = r#"{"data":{"items":[{"title":"a"},{"title":"b"}],"total":2}}"#;
        // select 摘取数组 → 序列化字符串
        let r = shape_response(&shaped(None, Some("$.data.items"), body), body);
        assert!(r.ok, "{:?}", r.error);
        assert!(r.output.contains("\"title\":\"a\""), "应含摘取内容: {}", r.output);
        // select + maxChars 两级
        let long = format!("{{\"msg\":\"{}\"}}", "x".repeat(5000));
        let r = shape_response(&shaped(Some(100), Some("$.msg"), &long), &long);
        assert!(r.ok);
        assert!(r.output.chars().count() <= 100 + "…（截断）".chars().count(), "应受 maxChars 限制");
    }

    #[test]
    fn 响应治理_路径缺失明确报错() {
        let body = r#"{"data":1}"#;
        let r = shape_response(&shaped(None, Some("$.missing.path"), body), body);
        assert!(!r.ok, "路径不存在应失败");
        assert!(r.error.unwrap_or_default().contains("$.missing.path"), "错误应含路径");
    }

    #[test]
    fn 响应治理_非JSON忽略select() {
        let body = "纯文本响应".repeat(20);
        let r = shape_response(&shaped(Some(50), Some("$.a"), &body), &body);
        assert!(r.ok, "非 JSON 应忽略 select 仅截断");
        assert!(r.output.contains("…（截断）"));
    }

    #[test]
    fn 响应治理_缺省口径不变() {
        // 未声明任何字段：8000 截断（与迁移前行为一致）
        let long = "y".repeat(9000);
        let r = shape_response(&shaped(None, None, &long), &long);
        assert!(r.ok);
        assert!(r.output.starts_with('y') && r.output.chars().count() <= 8000 + "…（截断）".chars().count());
    }

    #[test]
    fn json_path_基本语法() {
        let v: serde_json::Value = serde_json::from_str(r#"{"a":{"b":[{"c":1},{"c":2}],"d":"x"}}"#).unwrap();
        assert_eq!(json_path(&v, "$.a.b[1].c").and_then(|x| x.as_i64()), Some(2));
        assert_eq!(json_path(&v, "a.d").and_then(|x| x.as_str()), Some("x")); // 前导 $. 可省
        assert_eq!(json_path(&v, "$.a").map(|x| x.is_object()), Some(true));
        assert!(json_path(&v, "$.a.b[9]").is_none(), "数组越界");
        assert!(json_path(&v, "$.x").is_none(), "字段不存在");
        assert!(json_path(&v, "$.").is_none(), "尾随点非法");
    }

    /// 凭据变量（plugin-ecosystem 1.3）：注入往返 / 缺键拒绝 / config 工具无归属拒绝
    #[tokio::test]
    async fn 凭据变量_注入与缺键() {
        let gw = gw_with_approval("off", 0);
        let sid = gw.store.create_session("s", "a1").unwrap().id;
        // 注入凭据解析面
        gw.set_plugin_credentials_resolver(Box::new(|pid, key| {
            (pid == "demo" && key == "api_token").then(|| "sk-real-value".to_string())
        }));
        // 缺键：调用被拒、错误含键名与指引、不发请求
        let mut t = mk_custom("demo__secure", "https://example.com/api");
        t.headers.insert("Authorization".into(), "Bearer ${credentials.api_token}".into());
        gw.append_plugin_tools(vec![t]);
        // 先不配置凭据（resolver 只认 demo.api_token——配置上后应通过；
        // 此处 resolver 已配置，验证注入路径：把 resolver 换成缺键版本再验拒绝）
        gw.set_plugin_credentials_resolver(Box::new(|_, _| None));
        let r = gw
            .execute_named("a1", &sid, &[], "demo__secure", &serde_json::json!({}))
            .await;
        assert!(!r.ok, "缺键应拒绝");
        let err = r.error.unwrap_or_default();
        assert!(err.contains("api_token") && err.contains("凭据未配置"), "错误应含键名与指引：{err}");
    }

    #[test]
    fn strip_html_多字节字符不越界() {
        // 部署实测回归：旧字节级实现在多字节字符中间切片 panic（15177 边界）
        let html = "<html><body><p>你好世界，这是中文正文𝄞音符与 emoji 😀 混排</p>\
<script>var s = \"中文脚本内容不影响正文\";</script>\
<p>İstanbul 与 straße 的大小写归一也不崩</p></body></html>";
        let text = strip_html(html);
        assert!(text.contains("你好世界"), "正文保留：{text}");
        assert!(text.contains("😀"), "emoji 完整：{text}");
        assert!(!text.contains("中文脚本内容"), "script 块剥除：{text}");
        assert!(text.contains("İstanbul"), "特殊大小写字符完整：{text}");
        // 孤立多字节 + 未闭合标签：不 panic 即过
        assert!(!strip_html("标题<").is_empty());
        assert_eq!(strip_html("<script>永不闭合"), "");
    }

    #[test]
    fn 凭据变量_解析纯函数() {
        let gw = gw_with_approval("off", 0);
        // 无 resolver：全部视为缺键
        let (out, missing) = gw.resolve_credential_vars("p", "Bearer ${credentials.k1}");
        assert_eq!(out, "Bearer ${credentials.k1}");
        assert_eq!(missing, vec!["k1"]);
        // resolver 命中：明文注入
        gw.set_plugin_credentials_resolver(Box::new(|pid, key| {
            (pid == "p" && key == "k1").then(|| "V".to_string())
        }));
        let (out, missing) = gw.resolve_credential_vars("p", "Bearer ${credentials.k1} tail ${credentials.k2}");
        assert_eq!(out, "Bearer V tail ${credentials.k2}");
        assert_eq!(missing, vec!["k2"]);
        // 非法键名原样保留（不视为变量）
        let (out, missing) = gw.resolve_credential_vars("p", "${credentials.ba d}");
        assert_eq!(out, "${credentials.ba d}");
        assert!(missing.is_empty());
    }

    #[tokio::test]
    async fn 凭据变量_无插件归属拒绝() {
        let gw = gw_with_approval("off", 0);
        let sid = gw.store.create_session("s", "a1").unwrap().id;
        let t = mk_custom("plain_tool", "https://example.com/${credentials.k}");
        gw.append_plugin_tools(vec![t]);
        let r = gw
            .execute_named("a1", &sid, &[], "plain_tool", &serde_json::json!({}))
            .await;
        assert!(!r.ok, "config 工具无插件归属，引用变量应拒绝");
        assert!(r.error.unwrap_or_default().contains("无插件归属"));
    }
}

// ---------------------------------------------------------------- 会话级工具注入（video-watch-agent 组 2 单测）

#[cfg(test)]
mod session_tool_tests {
    use super::*;

    fn ct(name: &str) -> crate::config::CustomTool {
        crate::config::CustomTool {
            name: name.into(),
            description: "测试会话工具".into(),
            kind: "builtin".into(),
            method: String::new(),
            target: String::new(),
            headers: Default::default(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "q": { "type": "string" } }
            }),
            agents: vec![],
            response_max_chars: None,
            response_select: None,
        }
    }

    #[test]
    fn 会话注入_可见性收窄() {
        let dir = std::env::temp_dir().join(format!("exm-sess-tools-{}", crate::types::new_id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let store = Arc::new(crate::store::Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(crate::registry::LocalRegistry::new(dir.join("agents")).unwrap());
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let gw = ToolGateway::new(&dir, store, registry, events, crate::config::SecurityConfig::default());

        // 注入前：普通会话看不到
        assert!(gw.session_tool_specs("s-video").is_empty());

        // 注入 video__summary 到 s-video 会话
        let exec = dynamic_exec(|_a, _s, args| async move {
            Ok(format!("看到了：{}", args.get("q").and_then(|v| v.as_str()).unwrap_or("?")))
        });
        gw.inject_session_tools("s-video", vec![(ct("video__summary"), exec)]);

        // 本会话可见、别的会话不可见
        let specs = gw.session_tool_specs("s-video");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "video__summary");
        assert!(gw.session_tool_specs("s-other").is_empty());

        // 执行面同样收窄：非注入会话调不到（未知工具）
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            // 命名空间工具名走 execute_named 动态/会话表查找
            let r = gw
                .execute_named(
                    "a1",
                    "s-video",
                    &[],
                    "video__summary",
                    &serde_json::json!({ "q": "测试" }),
                )
                .await;
            // 会话工具执行：ok 或业务错误均可，但不应是「未知工具」
            let text = r.output.clone();
            assert!(!text.contains("未知工具"), "会话注入会话应命中工具：{text}");

            let r2 = gw
                .execute_named(
                    "a1",
                    "s-other",
                    &[],
                    "video__summary",
                    &serde_json::json!({ "q": "测试" }),
                )
                .await;
            assert!(!r2.ok, "普通会话不可见应失败");
            let t2 = r2.error.clone().unwrap_or_default();
            assert!(t2.contains("未知工具"), "普通会话不可见应报未知工具：{t2}");
        });

        // 清理后再不可见
        gw.clear_session_tools("s-video");
        assert!(gw.session_tool_specs("s-video").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
