//! 前台、后台和审批终端共用的子进程与输出通道生命周期。

#[cfg(windows)]
use super::sandbox::{attach_job, WinJob};
use super::sandbox::{build_shell_command, ShellOpts};
use std::future::{pending, Future};
use std::io;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Child;
use tokio::time::{self, Instant};

const REAP_TIMEOUT: Duration = Duration::from_secs(3);
const PIPE_TIMEOUT: Duration = Duration::from_millis(500);
#[cfg(windows)]
const TASKKILL_TIMEOUT: Duration = Duration::from_secs(2);
/// 包含回收、管道收尾、Windows 备用终止和少量调度余量。
pub(super) const STOP_TIMEOUT: Duration = Duration::from_secs(7);

#[derive(Clone, Debug)]
pub(super) enum JobEnd {
    Exited(ExitStatus),
    Cancelled,
    TimedOut,
    Failed(String),
}

#[derive(Clone, Debug)]
pub(super) struct JobOutcome {
    pub end: JobEnd,
    pub cleanup_error: Option<String>,
}

impl JobOutcome {
    fn note_cleanup_error(&mut self, error: String) {
        if let Some(previous) = &mut self.cleanup_error {
            previous.push(',');
            previous.push_str(&error);
        } else {
            self.cleanup_error = Some(error);
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Stream {
    Stdout,
    Stderr,
}

pub(super) struct ManagedProcess {
    child: Child,
    #[cfg(unix)]
    pgid: Option<libc::pid_t>,
    #[cfg(windows)]
    job: Option<WinJob>,
}

impl ManagedProcess {
    pub fn spawn(root: &Path, cmd: &str, opts: &ShellOpts) -> Result<Self, String> {
        let mut command = build_shell_command(root, cmd, opts)?;
        #[cfg(unix)]
        command.process_group(0);
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Unix 上由本类型先终止进程组，Child 的 Drop 只负责安排收尸。
            .kill_on_drop(!cfg!(unix))
            .spawn()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            #[cfg(unix)]
            pgid: Some(child.id().expect("new child has a PID") as libc::pid_t),
            #[cfg(windows)]
            job: attach_job(&child, opts),
            child,
        })
    }

    pub async fn run(
        mut self,
        deadline: Option<Instant>,
        cancel: impl Future<Output = ()>,
        capture: impl Fn(Stream, &[u8]) + Sync,
    ) -> JobOutcome {
        let stdout = self.child.stdout.take();
        let stderr = self.child.stderr.take();
        let read_pipes = async {
            let (out, err) = tokio::join!(
                drain_pipe(stdout, |bytes| capture(Stream::Stdout, bytes)),
                drain_pipe(stderr, |bytes| capture(Stream::Stderr, bytes)),
            );
            out.and(err)
        };
        let finish = self.wait_and_reap(deadline, cancel);
        tokio::pin!(read_pipes, finish);
        // 读管道与等待进程同属一个 future，不创建可能失去所有者的读取任务。
        let (mut outcome, pipes) = tokio::select! {
            result = &mut finish => (result, time::timeout(PIPE_TIMEOUT, &mut read_pipes).await),
            pipes = &mut read_pipes => (finish.await, Ok(pipes)),
        };
        match pipes {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                outcome.note_cleanup_error(format!("输出读取失败，输出可能不完整: {error}"))
            }
            Err(_) => outcome.note_cleanup_error("输出管道未在时限内关闭，输出可能不完整".into()),
        }
        outcome
    }

    async fn wait_and_reap(
        &mut self,
        deadline: Option<Instant>,
        cancel: impl Future<Output = ()>,
    ) -> JobOutcome {
        let observed = tokio::select! {
            biased;
            _ = cancel => Err(JobEnd::Cancelled),
            result = self.observe_exit() => result.map_err(|error| JobEnd::Failed(error.to_string())),
            _ = async { match deadline {
                Some(deadline) => time::sleep_until(deadline).await,
                None => pending::<()>().await,
            }} => Err(JobEnd::TimedOut),
        };
        let group_error = self.stop_tree().await.err();
        let status = match observed {
            Ok(Some(status)) => Ok(status),
            _ => match time::timeout(REAP_TIMEOUT, self.child.wait()).await {
                Ok(Ok(status)) => Ok(status),
                Ok(Err(error)) => Err(format!("无法回收子进程: {error}")),
                Err(_) => Err("子进程未在回收时限内退出".into()),
            },
        };
        let end = match (observed, &status) {
            (Err(reason), _) => reason,
            (Ok(_), Ok(status)) => JobEnd::Exited(*status),
            (Ok(_), Err(error)) => JobEnd::Failed(error.clone()),
        };
        let mut result = JobOutcome {
            end,
            cleanup_error: None,
        };
        if let Some(error) = group_error {
            result.note_cleanup_error(format!("无法确认进程树已终止: {error}"));
        }
        if let Err(error) = status {
            if !matches!(result.end, JobEnd::Failed(_)) {
                result.note_cleanup_error(error);
            }
        }
        result
    }

    #[cfg(unix)]
    async fn observe_exit(&mut self) -> io::Result<Option<ExitStatus>> {
        use tokio::signal::unix::{signal, SignalKind};
        // 先订阅再探测，SIGCHLD 合并或进程提前退出都不会丢失通知。
        let mut changed = signal(SignalKind::child())?;
        let pid = self.pgid.expect("group alive until its leader is reaped");
        while !leader_exited(pid)? {
            changed.recv().await;
        }
        // POSIX 尚未收尸：保留组 ID，先清理留在组内的子孙进程。
        Ok(None)
    }

    #[cfg(not(unix))]
    async fn observe_exit(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.wait().await.map(Some)
    }

    #[cfg(unix)]
    fn stop_group(&mut self) -> io::Result<()> {
        let Some(pgid) = self.pgid.take() else {
            return Ok(());
        };
        // 独占 Child，且未 wait/try_wait，因此组长尚未收尸，PID 不会被重用。
        // 若进程已被其他等待者回收（ECHILD），绝不再按这个数字发信号。
        let exited = leader_exited(pgid)?;
        // SAFETY: pgid 是本次 spawn 得到的正 PID，只对为本次任务创建的组发信号。
        let signalled = match unsafe { libc::kill(-pgid, libc::SIGKILL) } {
            0 => Ok(()),
            _ => {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        };
        // 此后可能进入 wait 并释放 PID；无论信号是否成功，都不得在 Drop 中
        // 使用这个数字重试，以免误杀后来复用该 PID 的无关子进程。
        if !exited {
            let leader = self.child.start_kill();
            signalled.and(leader)
        } else {
            signalled
        }
    }

    async fn stop_tree(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        return self.stop_group();
        #[cfg(windows)]
        {
            if let Some(job) = self.job.take() {
                drop(job); // KILL_ON_JOB_CLOSE 终止整棵树，不依赖退出码或 cmd 的 PID。
                return Ok(());
            }
            // 没有 Job Object 时，保留既有 taskkill 兜底，但限制其自身等待时间。
            let Some(pid) = self.child.id() else {
                return Ok(());
            };
            let killed = time::timeout(
                TASKKILL_TIMEOUT,
                tokio::process::Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .kill_on_drop(true)
                    .status(),
            )
            .await;
            match killed {
                Ok(Ok(status)) if status.success() => Ok(()),
                other => {
                    let _ = self.child.start_kill();
                    Err(io::Error::other(format!("taskkill 未完成: {other:?}")))
                }
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            if self.child.id().is_some() {
                self.child.start_kill()
            } else {
                Ok(())
            }
        }
    }
}

#[cfg(unix)]
impl Drop for ManagedProcess {
    fn drop(&mut self) {
        // 调用者丢弃 future 时也终止组。正常路径的 wait 已回收 Child 并解除组信号。
        if let Err(error) = self.stop_group() {
            eprintln!("终端进程组回收失败: {error}");
        }
    }
}

#[cfg(unix)]
fn leader_exited(pid: libc::pid_t) -> io::Result<bool> {
    if pid <= 0 {
        return Err(io::Error::other("无效的终端子进程 ID"));
    }
    // SAFETY: siginfo 是 waitid 的输出参数；非阻塞无事件时其 si_pid 保持为 0。
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    loop {
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            return Ok(unsafe { info.si_pid() } == pid);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

async fn drain_pipe(
    pipe: Option<impl AsyncRead + Unpin>,
    capture: impl Fn(&[u8]),
) -> io::Result<()> {
    if let Some(mut pipe) = pipe {
        // 不让每层工具/审批 future 内嵌两块 8KB 缓冲，避免深层编排放大调用栈。
        let mut chunk = vec![0; 8192];
        loop {
            let n = pipe.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            capture(&chunk[..n]);
        }
    }
    Ok(())
}

pub(super) fn capture_prefix(buf: &mut Vec<u8>, chunk: &[u8], limit: usize) {
    let count = chunk.len().min(limit.saturating_sub(buf.len()));
    buf.extend_from_slice(&chunk[..count]);
}
