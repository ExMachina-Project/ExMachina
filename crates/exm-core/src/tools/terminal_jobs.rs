//! 后台终端准入、受限日志与保守恢复；持久记录永不用于重放命令或恢复 PID。

use super::managed_process::{JobEnd, ManagedProcess, Stream, STOP_TIMEOUT};
use super::terminal_output::{OutputLog, OutputTail, PageRequest};
use super::{ToolGateway, ToolResult};
use crate::store::{Store, TerminalJobRecord, MAX_TERMINAL_LOG_BYTES};
use crate::types::ApprovalRequest;
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

/// Public, bounded metadata for a console; never exposes process control handles.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalJobSnapshot {
    pub task_id: String,
    pub command: String,
    pub status: String,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub output_preview: String,
    pub output_bytes: usize,
    pub output_truncated: bool,
    pub error: Option<String>,
}

impl From<&TerminalJobRecord> for TerminalJobSnapshot {
    fn from(record: &TerminalJobRecord) -> Self {
        Self {
            task_id: record.id.clone(),
            command: super::redact_str(&record.command)
                .chars()
                .take(2000)
                .collect(),
            status: record.status.clone(),
            started_at_ms: record.started_at_ms,
            finished_at_ms: record.finished_at_ms,
            exit_code: record.exit_code,
            output_preview: super::redact_str(&record.output_preview)
                .chars()
                .take(2000)
                .collect(),
            output_bytes: record
                .output_end_byte
                .unwrap_or(record.output_bytes as u64)
                .min(usize::MAX as u64) as usize,
            output_truncated: record.output_truncated,
            error: record
                .error
                .as_deref()
                .map(|error| super::redact_str(error).chars().take(500).collect()),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BackgroundLimits {
    pub max_running: usize,
    pub max_retained: usize,
    pub timeout_secs: u64,
    /// 内存输出与每项 JSONL 日志各自的硬上限，包含日志序列化开销。
    pub max_output_bytes: usize,
}

impl Default for BackgroundLimits {
    fn default() -> Self {
        Self {
            max_running: 4,
            max_retained: 128,
            timeout_secs: 900,
            max_output_bytes: 1024 * 1024,
        }
    }
}

impl BackgroundLimits {
    fn validate(self) -> anyhow::Result<()> {
        anyhow::ensure!(
            (1..=32).contains(&self.max_running),
            "后台并发上限必须为 1..32"
        );
        anyhow::ensure!(
            (self.max_running..=256).contains(&self.max_retained),
            "后台记录上限不能低于并发且不能超过 256"
        );
        anyhow::ensure!(
            (1..=3600).contains(&self.timeout_secs),
            "后台总时长上限必须为 1..3600 秒"
        );
        anyhow::ensure!(
            (1024..=MAX_TERMINAL_LOG_BYTES).contains(&self.max_output_bytes),
            "后台输出上限必须为 1KB..4MB"
        );
        Ok(())
    }
}

/// 终端进入审批时保存的原始调用语义；不把后台意图降级成一条 shell 字符串。
#[derive(Debug, Clone)]
pub(crate) struct TerminalApprovalRequest {
    pub command: String,
    /// 审批卡上使用的既有前缀（如 `[红线]`/`[root]`）；不得作为 shell 命令执行。
    pub approval_command: String,
    pub background: bool,
    pub timeout_secs: Option<u64>,
    pub interactive: bool,
    pub elevated: bool,
    pub exec_user: String,
    pub exec_home: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TerminalApprovalAction {
    pub version: u8,
    pub tool: String,
    pub op: String,
    pub command: String,
    pub approval_command: String,
    pub background: bool,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    pub interactive: bool,
    pub elevated: bool,
    #[serde(default)]
    pub exec_user: String,
    #[serde(default)]
    pub exec_home: String,
    pub workspace: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
}

impl TerminalApprovalAction {
    pub(crate) fn from_request(
        request: &TerminalApprovalRequest,
        workspace: &Path,
        approval_wait_secs: u64,
    ) -> Self {
        let created_at_ms = now_ms();
        let ttl_secs = approval_wait_secs
            .saturating_add(60)
            .clamp(300, 24 * 60 * 60);
        Self {
            version: 1,
            tool: "terminal".into(),
            op: "run".into(),
            command: request.command.clone(),
            approval_command: request.approval_command.clone(),
            background: request.background,
            timeout_secs: request.timeout_secs,
            interactive: request.interactive,
            elevated: request.elevated,
            exec_user: request.exec_user.clone(),
            exec_home: request.exec_home.clone(),
            workspace: workspace.to_string_lossy().into_owned(),
            created_at_ms,
            expires_at_ms: created_at_ms.saturating_add(ttl_secs.saturating_mul(1000)),
        }
    }
}

impl ToolGateway {
    fn validate_terminal_action(
        &self,
        session_id: &str,
        action: &TerminalApprovalAction,
    ) -> anyhow::Result<PathBuf> {
        anyhow::ensure!(action.version == 1, "终端审批版本不受支持");
        anyhow::ensure!(
            action.tool == "terminal" && action.op == "run",
            "终端审批动作类型无效"
        );
        anyhow::ensure!(!action.command.trim().is_empty(), "终端审批命令为空");
        anyhow::ensure!(
            action.command.len() <= 32 * 1024,
            "终端审批命令超过 32KB 上限"
        );
        anyhow::ensure!(
            !super::contains_shell_chain(&action.command),
            "终端审批命令包含被禁止的 shell 元字符"
        );
        anyhow::ensure!(!action.interactive, "当前终端 runner 不支持交互式审批回放");
        anyhow::ensure!(
            action.exec_user == self.security.exec_user,
            "终端执行身份配置已变化，未执行"
        );
        anyhow::ensure!(
            action.exec_home == self.security.exec_home,
            "终端执行 HOME 配置已变化，未执行"
        );
        anyhow::ensure!(
            !action.elevated || !action.exec_user.trim().is_empty(),
            "提权终端审批不再适用于当前执行身份，未执行"
        );
        let max_ttl_ms = 24 * 60 * 60 * 1000;
        anyhow::ensure!(
            action.created_at_ms <= now_ms().saturating_add(60_000)
                && action.expires_at_ms > action.created_at_ms
                && action.expires_at_ms - action.created_at_ms <= max_ttl_ms,
            "终端审批有效期无效"
        );
        anyhow::ensure!(now_ms() < action.expires_at_ms, "终端审批已过期，未执行");
        let plain_label = action.approval_command == action.command;
        let redline_label = action.approval_command == format!("[红线] {}", action.command);
        let root_label = action.approval_command == format!("[root] {}", action.command);
        anyhow::ensure!(
            if action.elevated {
                root_label && super::is_elevation_command(&action.command)
            } else {
                !root_label && (plain_label || redline_label)
            },
            "终端审批命令标记无效"
        );

        let workspace = crate::workspace::pinned(&action.workspace)?;
        let session_workspace = self
            .store
            .get_session(session_id)?
            .and_then(|session| session.workspace)
            .map(|raw| crate::workspace::pinned(&raw))
            .transpose()?;
        if let Some(bound) = session_workspace {
            anyhow::ensure!(bound == workspace, "终端审批项目已变化，未执行");
        } else {
            anyhow::ensure!(
                self.effective_root().canonicalize()? == workspace,
                "终端审批项目不在当前项目边界内，未执行"
            );
        }
        Ok(workspace)
    }

    pub(crate) async fn execute_terminal_request(
        &self,
        request: &TerminalApprovalRequest,
        session_id: &str,
        workspace: PathBuf,
    ) -> ToolResult {
        if request.interactive {
            return ToolResult::err("当前终端 runner 不支持交互式终端");
        }
        let command = if request.elevated {
            super::strip_redundant_elevation(&request.command)
        } else {
            request.command.clone()
        };
        if request.background {
            return self.start_background_at(
                &command,
                session_id,
                request.timeout_secs,
                workspace,
                request.elevated,
            );
        }
        let opts = if request.elevated {
            super::ShellOpts::plain()
        } else {
            self.shell_opts(&command)
        };
        let timeout = request
            .timeout_secs
            .unwrap_or(self.security.terminal_timeout_secs as u64)
            .clamp(5, self.security.terminal_timeout_secs.max(5) as u64);
        let timeout = if self.sandbox.strict() {
            timeout.min(60)
        } else {
            timeout
        };
        super::execute_shell_for_session(&workspace, &command, timeout, &opts, session_id).await
    }

    /// 仅供 Core 审批决定调用：校验持久化动作后复用前台/后台同一 runner。
    pub(crate) async fn execute_terminal_approval(
        &self,
        session_id: &str,
        action: &TerminalApprovalAction,
    ) -> anyhow::Result<ToolResult> {
        let workspace = self.validate_terminal_action(session_id, action)?;
        let request = TerminalApprovalRequest {
            command: action.command.clone(),
            approval_command: action.approval_command.clone(),
            background: action.background,
            timeout_secs: action.timeout_secs,
            interactive: action.interactive,
            elevated: action.elevated,
            exec_user: action.exec_user.clone(),
            exec_home: action.exec_home.clone(),
        };
        Ok(self
            .execute_terminal_request(&request, session_id, workspace)
            .await)
    }
}

pub(crate) fn terminal_action_from_approval(
    request: &ApprovalRequest,
) -> anyhow::Result<Option<TerminalApprovalAction>> {
    let Some(raw) = request.meta_json.as_deref() else {
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_str(raw)?;
    let Some(action) = value.get("terminalAction") else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_value(action.clone())?))
}

#[derive(Clone)]
struct LiveJob {
    stop: watch::Sender<bool>,
    worker: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    record: Arc<Mutex<TerminalJobRecord>>,
    output: Arc<Mutex<OutputTail>>,
    persist: Arc<Mutex<bool>>,
    owner: Weak<AtomicBool>,
    progress: watch::Sender<()>,
}

type JobKey = (PathBuf, String);
fn live_jobs() -> &'static Mutex<HashMap<JobKey, LiveJob>> {
    static JOBS: OnceLock<Mutex<HashMap<JobKey, LiveJob>>> = OnceLock::new();
    JOBS.get_or_init(Mutex::default)
}

type Admissions = HashMap<PathBuf, Vec<Weak<AtomicBool>>>;
fn admissions() -> &'static Mutex<Admissions> {
    static ADMISSIONS: OnceLock<Mutex<Admissions>> = OnceLock::new();
    ADMISSIONS.get_or_init(Mutex::default)
}
fn shutdown_roots() -> &'static Mutex<HashSet<PathBuf>> {
    static ROOTS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    ROOTS.get_or_init(Mutex::default)
}

pub(super) fn background_admission(store: &Store) -> Arc<AtomicBool> {
    let Ok(root) = root(store) else {
        return Arc::new(AtomicBool::new(false));
    };
    let active = live_jobs().lock();
    let mut registered = admissions().lock();
    registered.retain(|_, values| {
        values.retain(|value| value.strong_count() != 0);
        !values.is_empty()
    });
    let mut closed = shutdown_roots().lock();
    closed.retain(|root| {
        registered.contains_key(root) || active.keys().any(|(other, _)| other == root)
    });
    let accepting = !closed.contains(&root);
    let values = registered.entry(root).or_default();
    let admission = Arc::new(AtomicBool::new(accepting));
    values.push(Arc::downgrade(&admission));
    admission
}

pub(super) fn recover_records(store: &Store) -> anyhow::Result<()> {
    let root = root(store)?;
    let active = live_jobs().lock();
    for mut record in store.list_terminal_jobs()? {
        if record.status == "running" && !active.contains_key(&(root.clone(), record.id.clone())) {
            recover_interrupted(store, &mut record)?;
        }
    }
    Ok(())
}

struct Registration(JobKey);
impl Drop for Registration {
    fn drop(&mut self) {
        live_jobs().lock().remove(&self.0);
    }
}

fn root(store: &Store) -> anyhow::Result<PathBuf> {
    Ok(PathBuf::from(store.root()).canonicalize()?)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn recover_interrupted(store: &Store, record: &mut TerminalJobRecord) -> anyhow::Result<()> {
    let log_error = OutputLog::reconcile(store, record).err();
    record.status = "interrupted".into();
    record.exit_code = None;
    record.finished_at_ms = Some(now_ms());
    let mut message =
        "运行句柄已丢失，可能因重启或执行器异常中断；未恢复旧 PID，也未重放命令".to_string();
    if let Some(error) = log_error {
        message.push_str(&format!("；保留输出无法确认：{error}"));
    }
    record.error = Some(message);
    store.put_terminal_job(record)
}

struct Capture {
    record: Arc<Mutex<TerminalJobRecord>>,
    output: Arc<Mutex<OutputTail>>,
    progress: watch::Sender<()>,
    store: Arc<Store>,
    stop: watch::Sender<bool>,
    limit: usize,
    persist: Arc<Mutex<bool>>,
    events: tokio::sync::broadcast::Sender<crate::types::CoreEvent>,
    published_at: Mutex<Option<std::time::Instant>>,
}

impl Capture {
    fn publish(&self, kind: &str, record: &TerminalJobRecord) {
        let event = crate::types::CoreEvent {
            kind: kind.to_string(),
            session_id: record.session_id.clone(),
            payload: serde_json::to_value(TerminalJobSnapshot::from(record)).unwrap_or_default(),
        };
        crate::round_trace::push_activity_event(event.clone());
        let _ = self.events.send(event.clone());
        if kind != "terminal.updated" {
            let envelope = crate::types::Envelope::new(
                format!("session.{}.events", record.session_id),
                "terminal",
                "broadcast",
                "event",
                &record.session_id,
                serde_json::to_value(event).unwrap_or_default(),
            );
            let _ = self.store.append_event(&envelope);
        }
    }

    fn push(&self, stream: Stream, bytes: &[u8], eof: bool) {
        let mut output = self.output.lock();
        let text = output.decode(stream, bytes, eof);
        if text.is_empty() {
            return;
        }
        let text = super::redact_str(&text);
        let mut record = self.record.lock();
        let persistent = self.persist.lock();
        if !*persistent || record.error.is_some() {
            return;
        }
        if let Err(error) = output.append(&self.store, &mut record, stream, &text, self.limit) {
            record.error = Some(format!("后台输出落盘失败: {error}"));
            self.stop.send_replace(true);
            self.progress.send_replace(());
            return;
        }
        // 活动作业从内存读状态；崩溃后从带偏移的完整日志行重建，不重复写元数据。
        self.progress.send_replace(());
        let now = std::time::Instant::now();
        let mut published = self.published_at.lock();
        if published.is_none_or(|last| now.duration_since(last) >= Duration::from_millis(250)) {
            *published = Some(now);
            self.publish("terminal.updated", &record);
        }
    }
}

impl ToolGateway {
    pub fn with_background_limits(mut self, limits: BackgroundLimits) -> anyhow::Result<Self> {
        limits.validate()?;
        self.background_limits = limits;
        Ok(self)
    }

    fn start_background_at(
        &self,
        cmd: &str,
        session: &str,
        requested_timeout: Option<u64>,
        cwd: PathBuf,
        elevated: bool,
    ) -> ToolResult {
        let started = || -> anyhow::Result<String> {
            anyhow::ensure!(
                self.background_accepting.load(Ordering::Acquire),
                "后台执行器已关闭"
            );
            anyhow::ensure!(cmd.len() <= 32 * 1024, "后台命令超过 32KB 上限");
            let limits = self.background_limits;
            let root = root(&self.store)?;
            let mut active = live_jobs().lock();
            anyhow::ensure!(
                self.background_accepting.load(Ordering::Acquire),
                "后台执行器已关闭"
            );
            let running = active.keys().filter(|(other, _)| *other == root).count();
            anyhow::ensure!(
                running < limits.max_running,
                "后台并发已达上限（{}），命令未启动",
                limits.max_running
            );
            let mut records = self.store.list_terminal_jobs()?;
            for record in &mut records {
                if record.status == "running"
                    && !active.contains_key(&(root.clone(), record.id.clone()))
                {
                    recover_interrupted(&self.store, record)?;
                }
            }
            records.sort_by_key(|job| job.finished_at_ms.unwrap_or(job.started_at_ms));
            while records.len() >= limits.max_retained {
                let at = records
                    .iter()
                    .position(|job| {
                        job.status != "running"
                            && !active.contains_key(&(root.clone(), job.id.clone()))
                    })
                    .ok_or_else(|| anyhow::anyhow!("后台记录已达上限，命令未启动"))?;
                self.store.delete_terminal_job(&records.remove(at).id)?;
            }
            let cancellation = crate::round_trace::subscribe_cancel(session);
            anyhow::ensure!(
                !cancellation.as_ref().is_some_and(|rx| *rx.borrow()),
                "本轮已被用户停止，后台命令未启动"
            );
            tokio::runtime::Handle::try_current()?;
            let id = format!("bg-{}", crate::types::new_id());
            let seconds = requested_timeout
                .unwrap_or(limits.timeout_secs)
                .clamp(1, limits.timeout_secs);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
            let started_at_ms = now_ms();
            let mut record = TerminalJobRecord {
                id: id.clone(),
                session_id: session.into(),
                workspace: cwd.to_string_lossy().into_owned(),
                command: super::redact_str(cmd),
                started_at_ms,
                deadline_at_ms: started_at_ms + seconds * 1000,
                finished_at_ms: None,
                status: "running".into(),
                exit_code: None,
                output_bytes: 0,
                log_bytes: 0,
                output_chunks: 0,
                output_truncated: false,
                output_end_byte: Some(0),
                output_preview: String::new(),
                error: None,
            };
            self.store.put_terminal_job(&record)?;
            let opts = if elevated {
                super::ShellOpts::plain()
            } else {
                self.shell_opts(cmd)
            };
            let process = match ManagedProcess::spawn(&cwd, cmd, &opts) {
                Ok(process) => process,
                Err(error) => {
                    record.status = "failed".into();
                    record.error = Some(error.clone());
                    record.finished_at_ms = Some(now_ms());
                    self.store.put_terminal_job(&record)?;
                    anyhow::bail!("后台任务启动失败: {error}");
                }
            };
            let record = Arc::new(Mutex::new(record));
            let (stop, mut stop_rx) = watch::channel(false);
            let worker = Arc::new(tokio::sync::Mutex::new(None));
            let registration = Registration((root.clone(), id.clone()));
            let live = LiveJob {
                stop: stop.clone(),
                worker: worker.clone(),
                record: record.clone(),
                output: Arc::new(Mutex::default()),
                persist: Arc::new(Mutex::new(true)),
                owner: Arc::downgrade(&self.background_accepting),
                progress: watch::channel(()).0,
            };
            let persist = live.persist.clone();
            let capture = Capture {
                record,
                output: live.output.clone(),
                progress: live.progress.clone(),
                store: self.store.clone(),
                stop: stop.clone(),
                limit: limits.max_output_bytes,
                persist: persist.clone(),
                events: self.events.clone(),
                published_at: Mutex::new(None),
            };
            // Publish only after the live registry owns this job. Otherwise a fast
            // reconnect can observe the persisted `running` row without its live
            // handle and conservatively mark it interrupted.
            active.insert((root, id.clone()), live);
            capture.publish("terminal.started", &capture.record.lock());
            let handle = tokio::spawn(async move {
                let _registration = registration;
                let outcome = process
                    .run(
                        Some(deadline),
                        async {
                            tokio::select! {
                                _ = crate::round_trace::wait_for_cancel(cancellation) => {},
                                _ = stop_rx.wait_for(|stop| *stop) => {},
                            }
                        },
                        |stream, bytes| capture.push(stream, bytes, false),
                    )
                    .await;
                for stream in [Stream::Stdout, Stream::Stderr] {
                    capture.push(stream, &[], true);
                }
                let mut record = capture.record.lock();
                record.finished_at_ms = Some(now_ms());
                let status = match outcome.end {
                    JobEnd::Exited(exit) => {
                        record.exit_code = exit.code();
                        "exited"
                    }
                    JobEnd::Cancelled => "cancelled",
                    JobEnd::TimedOut => "timedOut",
                    JobEnd::Failed(error) => {
                        record.error = Some(error);
                        "failed"
                    }
                };
                if let Some(error) = outcome.cleanup_error {
                    record.error = Some(error);
                }
                if record.error.is_some() {
                    record.status = "failed".into();
                } else {
                    record.status = status.into();
                }
                let persistent = persist.lock();
                if *persistent {
                    if let Err(error) = capture.store.put_terminal_job(&record) {
                        eprintln!("[terminal] 后台任务终态持久化失败 {}: {error}", record.id);
                        record.status = "failed".into();
                        record.error = Some(format!("终态持久化失败: {error}"));
                    }
                    capture.publish("terminal.finished", &record);
                }
                capture.progress.send_replace(());
            });
            *worker.try_lock().expect("new worker slot") = Some(handle);
            Ok(format!("后台任务已启动：taskId={id}\n命令：{cmd}\n截止：{seconds}s；用 terminal(op=\"poll\", taskId=\"{id}\") 查看输出；op=\"kill\" 终止。"))
        };
        match started() {
            Ok(output) => ToolResult::ok(output),
            Err(error) => ToolResult::err(error.to_string()),
        }
    }

    fn visible_background_records(&self, session: &str) -> anyhow::Result<Vec<TerminalJobRecord>> {
        let root = root(&self.store)?;
        let workspace = self.workspace_for_session(session).canonicalize()?;
        let active = live_jobs().lock();
        let mut records = self.store.list_terminal_jobs()?;
        records.retain(|record| {
            record.session_id == session
                && std::path::Path::new(&record.workspace) == workspace.as_path()
        });
        for record in &mut records {
            if let Some(live) = active.get(&(root.clone(), record.id.clone())) {
                *record = live.record.lock().clone();
            } else if record.status == "running" {
                recover_interrupted(&self.store, record)?;
            }
        }
        records.sort_by_key(|record| std::cmp::Reverse(record.started_at_ms));
        Ok(records)
    }

    pub fn terminal_job_snapshots(
        &self,
        session: &str,
    ) -> anyhow::Result<Vec<TerminalJobSnapshot>> {
        Ok(self
            .visible_background_records(session)?
            .iter()
            .take(32)
            .map(TerminalJobSnapshot::from)
            .collect())
    }

    pub(super) fn poll_background(&self, session: &str, id: &str) -> ToolResult {
        let read = || -> anyhow::Result<String> {
            self.read_background(session, id, |record, log| {
                let range = log.range();
                let truncation = if range.start > 0 {
                    format!("，已截断前 {} 字节", range.start)
                } else if record.output_truncated {
                    "，旧日志已截断，丢失字节数未知".into()
                } else {
                    String::new()
                };
                Ok(format!(
                    "{}\n工作区：{}\n—— 输出尾部（日志保留 {} 字节{}；cursor={} 起可分页）——\n{}",
                    status(record),
                    record.workspace,
                    range.end - range.start,
                    truncation,
                    range.start,
                    log.tail_text(6000),
                ))
            })
        };
        match read() {
            Ok(output) => ToolResult::ok(output),
            Err(error) => ToolResult::err(error.to_string()),
        }
    }

    /// 读取已授权任务及同一状态快照的日志。活作业读时持有该作业记录锁，
    /// 终态读时保留注册锁，避免轮转、淘汰或会话删除混淆游标。
    fn read_background<T>(
        &self,
        session: &str,
        id: &str,
        read: impl FnOnce(&TerminalJobRecord, &OutputLog) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let (key, workspace) = self.background_key(session, id)?;
        let jobs = live_jobs().lock();
        if let Some(live) = jobs.get(&key).cloned() {
            drop(jobs);
            let output = live.output.lock();
            let record = live.record.lock();
            let persistent = live.persist.lock();
            require_owner(&record, session, &workspace, id)?;
            anyhow::ensure!(*persistent, "后台任务不存在或不属于当前会话");
            let log = OutputLog::read(&self.store, id)?;
            log.validate(&record)?;
            drop(output);
            return read(&record, &log);
        }
        let mut record = self.stored_background(session, id, &workspace)?;
        if record.status == "running" {
            recover_interrupted(&self.store, &mut record)?;
        }
        let log = OutputLog::read(&self.store, id)?;
        log.validate(&record)?;
        read(&record, &log)
    }

    fn background_key(&self, session: &str, id: &str) -> anyhow::Result<(JobKey, PathBuf)> {
        // 兼容原短 taskId，但不允许路径分隔符或 FsDb 文件名别名。
        anyhow::ensure!(
            id.starts_with("bg-")
                && id.len() <= 128
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "后台任务不存在或不属于当前会话"
        );
        Ok((
            (root(&self.store)?, id.into()),
            self.workspace_for_session(session).canonicalize()?,
        ))
    }

    fn stored_background(
        &self,
        session: &str,
        id: &str,
        workspace: &Path,
    ) -> anyhow::Result<TerminalJobRecord> {
        let record = self
            .store
            .get_terminal_job(id)?
            .ok_or_else(|| anyhow::anyhow!("后台任务不存在或不属于当前会话"))?;
        require_owner(&record, session, workspace, id)?;
        Ok(record)
    }

    fn background_progress(
        &self,
        session: &str,
        id: &str,
        cursor: u64,
    ) -> anyhow::Result<Option<watch::Receiver<()>>> {
        let (key, workspace) = self.background_key(session, id)?;
        let jobs = live_jobs().lock();
        let Some(live) = jobs.get(&key) else {
            return Ok(None);
        };
        let record = live.record.lock();
        require_owner(&record, session, &workspace, id)?;
        anyhow::ensure!(*live.persist.lock(), "后台任务不存在或不属于当前会话");
        // 和更新端共用记录锁，保证不会错过「检查末尾 → 订阅」间的输出/退出。
        Ok((record.status == "running"
            && record.output_end_byte == Some(cursor)
            && record.error.is_none())
        .then(|| live.progress.subscribe()))
    }

    pub(super) async fn poll_background_page(
        &self,
        session: &str,
        id: &str,
        request: PageRequest,
    ) -> ToolResult {
        let read = async {
            if request.wait_ms != 0 {
                let deadline = tokio::time::Instant::now() + Duration::from_millis(request.wait_ms);
                let cancelled = crate::round_trace::wait_for_cancel(
                    crate::round_trace::subscribe_cancel(session),
                );
                tokio::pin!(cancelled);
                if let Some(mut progress) = self.background_progress(session, id, request.cursor)? {
                    tokio::select! {
                        biased;
                        _ = &mut cancelled => anyhow::bail!("本轮已取消，停止等待后台输出"),
                        _ = progress.changed() => {},
                        _ = tokio::time::sleep_until(deadline) => {},
                    }
                }
            }
            self.read_background(session, id, |record, log| {
                Ok(serde_json::to_string(&BackgroundPage {
                    task_id: &record.id,
                    workspace: &record.workspace,
                    status: &record.status,
                    exit_code: record.exit_code,
                    error: record.error.as_deref(),
                    output: log.page(record, request)?,
                })?)
            })
        };
        match read.await {
            Ok(output) => ToolResult::ok(output),
            Err(error) => ToolResult::err(error.to_string()),
        }
    }

    pub(super) fn list_background(&self, session: &str) -> ToolResult {
        match self.visible_background_records(session) {
            Err(error) => ToolResult::err(error.to_string()),
            Ok(records) if records.is_empty() => ToolResult::ok("当前会话无后台任务。"),
            Ok(records) => ToolResult::ok(
                records
                    .iter()
                    .map(|record| {
                        let preview = record.output_preview.trim().replace('\n', " ⏎ ");
                        let tail = if preview.is_empty() { "（暂无）" } else { &preview };
                        let start = record.output_end_byte.map_or(0, |end| end.saturating_sub(record.output_bytes as u64));
                        let end = record.output_end_byte.unwrap_or(record.output_bytes as u64);
                        let command: String = record.command.chars().take(200).collect();
                        let ellipsis = if command.len() < record.command.len() { "…" } else { "" };
                        format!(
                            "[taskId={}] {command}{ellipsis} [{}]\n   工作区：{}；输出范围 [{start}, {end})\n   输出尾：{tail}",
                            record.id,
                            status(record),
                            record.workspace,
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        }
    }

    pub(super) async fn kill_background(&self, session: &str, id: &str) -> ToolResult {
        let lookup = || -> anyhow::Result<Option<LiveJob>> {
            let (key, workspace) = self.background_key(session, id)?;
            let jobs = live_jobs().lock();
            let live = jobs.get(&key).cloned();
            if let Some(live) = &live {
                require_owner(&live.record.lock(), session, &workspace, id)?;
                anyhow::ensure!(*live.persist.lock(), "后台任务不存在或不属于当前会话");
            } else {
                self.stored_background(session, id, &workspace)?;
            }
            Ok(live)
        };
        match lookup() {
            Err(error) => ToolResult::err(error.to_string()),
            Ok(None) => self.poll_background(session, id),
            Ok(Some(live)) => match stop_and_wait(live).await {
                Ok(()) => self.poll_background(session, id),
                Err(error) => ToolResult::err(error.to_string()),
            },
        }
    }

    pub(crate) fn cancel_background_session(&self, session: &str) {
        if let Ok(root) = root(&self.store) {
            for ((other, _), live) in live_jobs().lock().iter() {
                if *other == root && live.record.lock().session_id == session {
                    live.stop.send_replace(true);
                }
            }
        }
    }

    pub(crate) fn delete_session_with_background(&self, session: &str) -> anyhow::Result<()> {
        let root = root(&self.store)?;
        // 正常停止保留输出；删除才禁止迟到写入。删除期间不得为同一会话启动新任务。
        let jobs = live_jobs().lock();
        for ((other, _), live) in jobs.iter() {
            if *other == root && live.record.lock().session_id == session {
                *live.persist.lock() = false;
                live.stop.send_replace(true);
            }
        }
        self.store.delete_session(session)
    }

    pub(crate) fn stop_owned_background(&self) {
        self.background_accepting.store(false, Ordering::Release);
        let Ok(root) = root(&self.store) else {
            return;
        };
        let owner = Arc::downgrade(&self.background_accepting);
        let jobs = live_jobs()
            .lock()
            .iter()
            .filter(|((other, _), live)| *other == root && live.owner.ptr_eq(&owner))
            .map(|(_, live)| live.clone())
            .collect::<Vec<_>>();
        for live in jobs {
            live.stop.send_replace(true);
        }
    }

    /// 关闭准入并等待此数据根的全部活动后台终端；不重放或操作任何旧 PID。
    pub async fn shutdown_background(&self) -> anyhow::Result<()> {
        self.background_accepting.store(false, Ordering::Release);
        let root = root(&self.store)?;
        let jobs: Vec<_> = {
            let active = live_jobs().lock();
            // 原子关闭同数据根的旧/新网关准入，配置重建不能绕过退出清理。
            let registered = admissions().lock();
            shutdown_roots().lock().insert(root.clone());
            if let Some(values) = registered.get(&root) {
                for value in values.iter().filter_map(Weak::upgrade) {
                    value.store(false, Ordering::Release);
                }
            }
            active
                .iter()
                .filter(|((other, _), _)| *other == root)
                .map(|(_, live)| live.clone())
                .collect()
        };
        for live in &jobs {
            live.stop.send_replace(true);
        }
        let results = futures_util::future::join_all(jobs.into_iter().map(stop_and_wait)).await;
        for result in results {
            result?;
        }
        Ok(())
    }
}

fn require_owner(
    record: &TerminalJobRecord,
    session: &str,
    workspace: &Path,
    id: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        record.id == id
            && record.session_id == session
            && Path::new(&record.workspace) == workspace,
        "后台任务不存在或不属于当前会话"
    );
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BackgroundPage<'a> {
    task_id: &'a str,
    workspace: &'a str,
    status: &'a str,
    exit_code: Option<i32>,
    error: Option<&'a str>,
    #[serde(flatten)]
    output: super::terminal_output::OutputPage<'a>,
}

impl Drop for ToolGateway {
    fn drop(&mut self) {
        // 各代 gateway 只取消自身启动的任务；共享数据根下其他活网关保持服务。
        self.stop_owned_background();
    }
}

async fn stop_and_wait(live: LiveJob) -> anyhow::Result<()> {
    live.stop.send_replace(true);
    tokio::time::timeout(STOP_TIMEOUT, async {
        let mut worker = live.worker.lock().await;
        if let Some(handle) = worker.as_mut() {
            handle.await?;
            worker.take();
        }
        let record = live.record.lock().clone();
        anyhow::ensure!(record.status != "running", "后台终止尚未确认");
        anyhow::ensure!(
            record.status != "failed",
            "后台清理失败: {:?}",
            record.error
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .map_err(|_| anyhow::anyhow!("后台终止已请求，但等待清理超时"))?
}

fn status(record: &TerminalJobRecord) -> String {
    match record.status.as_str() {
        "running" => format!(
            "运行中（剩余约 {}s）",
            record
                .deadline_at_ms
                .saturating_sub(now_ms())
                .div_ceil(1000)
        ),
        "exited" if record.exit_code == Some(0) => "已完成（exit=0）".into(),
        "exited" => format!("已结束（exit={}）", record.exit_code.unwrap_or(-1)),
        "cancelled" => "已取消并回收".into(),
        "timedOut" => "已超时并回收".into(),
        "interrupted" => format!("已中断：{}", record.error.as_deref().unwrap_or_default()),
        _ => format!(
            "执行失败：{}",
            record.error.as_deref().unwrap_or("未知终态")
        ),
    }
}

#[cfg(test)]
mod activity_tests {
    use super::*;

    #[tokio::test]
    async fn background_terminal_publishes_session_scoped_lifecycle_and_output() {
        let root =
            std::env::temp_dir().join(format!("exm-terminal-events-{}", crate::types::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let store = Arc::new(Store::open(root.join("data")).unwrap());
        let registry =
            Arc::new(crate::registry::LocalRegistry::new(root.join("entities")).unwrap());
        let (events, mut receiver) = tokio::sync::broadcast::channel(64);
        let gateway = ToolGateway::new(
            &root,
            store,
            registry,
            events,
            crate::config::SecurityConfig::default(),
        );
        let session = "terminal-activity-session";
        let result = gateway.start_background_at(
            "printf activity-output",
            session,
            Some(3),
            root.clone(),
            false,
        );
        assert!(result.ok, "{}", result.error.unwrap_or_default());
        let id = result
            .output
            .split("taskId=")
            .nth(1)
            .and_then(|value| value.split_whitespace().next())
            .expect("启动结果应包含 task id")
            .to_string();

        let mut saw_started = false;
        let mut saw_finished = false;
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Ok(event) = receiver.recv().await {
                assert_eq!(event.session_id, session);
                if event.payload["taskId"] != id {
                    continue;
                }
                match event.kind.as_str() {
                    "terminal.started" => saw_started = true,
                    "terminal.finished" => {
                        assert!(saw_started, "终态事件不可先于启动事件");
                        assert_eq!(event.payload["status"], "exited");
                        assert_eq!(event.payload["exitCode"], 0);
                        assert!(event.payload["outputPreview"]
                            .as_str()
                            .unwrap()
                            .contains("activity-output"));
                        saw_finished = true;
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("后台终态事件应在有界时间内到达");
        assert!(saw_started && saw_finished);
        let jobs = gateway.terminal_job_snapshots(session).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].task_id, id);
        assert!(jobs[0].output_preview.contains("activity-output"));
        assert!(gateway
            .terminal_job_snapshots("another-session")
            .unwrap()
            .is_empty());
        drop(gateway);
        let _ = std::fs::remove_dir_all(root);
    }
}
