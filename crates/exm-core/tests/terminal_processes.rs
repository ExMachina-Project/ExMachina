//! 本地终端进程生命周期回归；仅在夹具内创建进程及临时 loopback 监听。

#![cfg(target_os = "linux")]

#[allow(dead_code)]
#[path = "support/agent_acceptance.rs"]
mod acceptance_fixture;

use acceptance_fixture::IsolatedProject;
use exm_core::config::{ExmConfig, SandboxConfig, SecurityConfig};
use exm_core::registry::LocalRegistry;
use exm_core::store::{Store, TerminalJobOutputChunk, TerminalJobRecord};
use exm_core::tools::{execute_shell_command_timed, BackgroundLimits, ToolGateway, ToolResult};
use exm_core::types::ToolName;
use exm_core::Core;
use serde_json::{json, Value};
use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};

struct Tree {
    address: SocketAddr,
    pids: [u32; 3],
}

impl Tree {
    async fn ready(project: &Path) -> Self {
        timeout(Duration::from_secs(2), async {
            let ready = loop {
                match std::fs::read_to_string(project.join("ready")) {
                    Ok(ready) => break ready,
                    Err(err) if err.kind() == ErrorKind::NotFound => {
                        sleep(Duration::from_millis(10)).await;
                    }
                    Err(err) => panic!("read readiness: {err}"),
                }
            };
            let mut fields = ready.split_whitespace();
            let address = (
                Ipv4Addr::LOCALHOST,
                fields.next().unwrap().parse::<u16>().unwrap(),
            )
                .into();
            let pids = std::array::from_fn(|_| fields.next().unwrap().parse().unwrap());
            let mut conn = TcpStream::connect(address).await.unwrap();
            let mut body = [0; 10];
            conn.read_exact(&mut body).await.unwrap();
            assert_eq!(
                &body, b"tree-ready",
                "must observe a real service before stopping it"
            );
            Self { address, pids }
        })
        .await
        .expect("process tree must become ready before the job deadline")
    }

    async fn assert_closed(&self) {
        timeout(Duration::from_secs(1), async {
            loop {
                match TcpStream::connect(self.address).await {
                    Err(err)
                        if matches!(
                            err.kind(),
                            ErrorKind::ConnectionRefused
                                | ErrorKind::ConnectionReset
                                | ErrorKind::BrokenPipe
                        ) =>
                    {
                        break
                    }
                    Err(err) => panic!("unexpected listener probe: {err}"),
                    Ok(_) => sleep(Duration::from_millis(10)).await,
                }
            }
        })
        .await
        .expect("grandchild still accepts connections after terminal operation ended");

        // Allow the kernel to deliver SIGKILL and init to reap orphaned
        // descendants. The direct child belongs to this runner and must be gone.
        timeout(Duration::from_secs(1), async {
            loop {
                let all_stopped = self.pids.iter().enumerate().all(|(i, pid)| {
                    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                        Err(err) if err.kind() == ErrorKind::NotFound => true,
                        Ok(stat)
                            if i != 0
                                && matches!(stat.rsplit_once(") "), Some((_, rest)) if rest.starts_with("Z ") || rest.starts_with("X ")) =>
                        {
                            true
                        }
                        _ => false,
                    }
                });
                if all_stopped {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("terminated process group must stop and the direct child must be reaped within one second");

        for (i, pid) in self.pids.iter().enumerate() {
            if i == 0 {
                assert!(
                    !Path::new(&format!("/proc/{pid}")).exists(),
                    "the runner must reap its direct child"
                );
            }
        }
    }
}

fn fixture(label: &str) -> IsolatedProject {
    let fixture = IsolatedProject::new(label);
    std::fs::write(
        fixture.project().join("process_tree.py"),
        include_str!("support/terminal_tree.py"),
    )
    .unwrap();
    fixture
}

fn gateway(fixture: &IsolatedProject) -> ToolGateway {
    gateway_with_limits(fixture, BackgroundLimits::default())
}

fn gateway_with_limits(fixture: &IsolatedProject, limits: BackgroundLimits) -> ToolGateway {
    gateway_with_security(fixture, limits, SecurityConfig::default())
}

fn gateway_with_security(
    fixture: &IsolatedProject,
    limits: BackgroundLimits,
    security: SecurityConfig,
) -> ToolGateway {
    let store = Arc::new(Store::open(fixture.oracle().join("data")).unwrap());
    let registry = Arc::new(LocalRegistry::new(fixture.oracle().join("agents")).unwrap());
    let (events, _) = tokio::sync::broadcast::channel(16);
    ToolGateway::new(fixture.project(), store, registry, events, security)
        .with_sandbox(SandboxConfig {
            mode: "off".into(),
            ..Default::default()
        })
        .with_background_limits(limits)
        .unwrap()
}

async fn terminal(gateway: &ToolGateway, session: &str, args: Value) -> ToolResult {
    gateway
        .execute(
            "coding-agent",
            session,
            &[ToolName::Terminal],
            ToolName::Terminal,
            &args,
        )
        .await
}

async fn output_page(gateway: &ToolGateway, session: &str, args: Value) -> Value {
    let result = terminal(gateway, session, args).await;
    assert!(result.ok, "读取游标页失败：{:?}", result.error);
    serde_json::from_str(&result.output).expect("查询结果必须是完整的 JSON 页")
}

fn task_id(result: &ToolResult) -> String {
    assert!(result.ok, "background start: {:?}", result.error);
    result
        .output
        .lines()
        .next()
        .unwrap()
        .split_once("taskId=")
        .unwrap()
        .1
        .into()
}

fn store(fixture: &IsolatedProject) -> Store {
    Store::open(fixture.oracle().join("data")).unwrap()
}

fn computer_core(fixture: &IsolatedProject, approval_wait_secs: u64) -> Core {
    let mut cfg = ExmConfig::load(fixture.oracle());
    cfg.workspace_root = fixture.project().to_path_buf();
    cfg.data_dir = fixture.oracle().join("data");
    cfg.agents_dir = fixture.oracle().join("agents");
    cfg.use_mock = true;
    cfg.memory_enabled = false;
    cfg.security.approval_wait_secs = approval_wait_secs;
    cfg.computer.enabled = true;
    cfg.computer.approval = true;
    Core::with_config(cfg).unwrap()
}

async fn wait_terminal(fixture: &IsolatedProject, id: &str) -> TerminalJobRecord {
    timeout(Duration::from_secs(6), async {
        loop {
            let record = store(fixture).get_terminal_job(id).unwrap().unwrap();
            if record.status != "running" {
                return record;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("background job must reach a bounded terminal state")
}

#[tokio::test]
async fn foreground_timeout_reaps_tree_and_preserves_partial_output() {
    let fixture = fixture("foreground-tree");
    let path = fixture.project().to_path_buf();
    let run = tokio::spawn(async move {
        execute_shell_command_timed(&path, "python3 process_tree.py", 3).await
    });
    let tree = Tree::ready(fixture.project()).await;
    let result = timeout(Duration::from_secs(6), run)
        .await
        .expect("foreground deadline")
        .unwrap();
    assert!(!result.ok, "a timed-out service is not successful");
    let error = result.error.unwrap();
    assert!(
        error.contains("超时")
            && error.contains("tree-stdout-ready")
            && error.contains("tree-stderr-ready"),
        "incomplete timeout diagnostics: {error}"
    );
    tree.assert_closed().await;
}

#[tokio::test]
async fn owner_kill_reaps_background_tree_and_keeps_cancelled_status() {
    let fixture = fixture("background-tree");
    let gateway = gateway(&fixture);
    let session = "test-background-owner";
    let started = terminal(
        &gateway,
        session,
        json!({ "command": "python3 process_tree.py", "background": true }),
    )
    .await;
    assert!(started.ok, "background start: {:?}", started.error);
    let task = started
        .output
        .lines()
        .next()
        .unwrap()
        .split_once("taskId=")
        .unwrap()
        .1;
    let tree = Tree::ready(fixture.project()).await;
    let denied = terminal(
        &gateway,
        "test-other-session",
        json!({ "op": "kill", "taskId": task }),
    )
    .await;
    assert!(!denied.ok, "a different session may not stop the listener");
    assert!(TcpStream::connect(tree.address).await.is_ok());
    let stopped = timeout(
        Duration::from_secs(8),
        terminal(&gateway, session, json!({ "op": "kill", "taskId": task })),
    )
    .await
    .expect("kill must be bounded");
    assert!(stopped.ok, "owner kill: {:?}", stopped.error);
    tree.assert_closed().await;
    let status = terminal(&gateway, session, json!({ "op": "poll", "taskId": task })).await;
    assert!(
        status.ok
            && status.output.contains("已取消")
            && status.output.contains("tree-stdout-ready")
            && status.output.contains("tree-stderr-ready"),
        "expected cancelled state and captured diagnostics: {} {:?}",
        status.output,
        status.error
    );
}

#[tokio::test]
async fn session_cancellation_reaps_foreground_tree_and_returns_partial_output() {
    let fixture = fixture("cancelled-foreground");
    let gateway = gateway(&fixture);
    let session = "test-foreground-cancel";
    exm_core::round_trace::begin(session);
    let args = json!({ "command": "python3 process_tree.py" });
    let command = tokio::spawn(async move { terminal(&gateway, session, args).await });
    let tree = Tree::ready(fixture.project()).await;
    assert!(exm_core::round_trace::request_cancel(session));
    let result = timeout(Duration::from_secs(5), command)
        .await
        .expect("cancellation must collect the process group")
        .unwrap();
    assert!(!result.ok);
    let error = result.error.unwrap();
    assert!(
        error.contains("命令已取消")
            && error.contains("tree-stdout-ready")
            && error.contains("tree-stderr-ready"),
        "cancellation result must distinguish stop from normal completion and preserve both streams: {error}"
    );
    tree.assert_closed().await;
    exm_core::round_trace::end(session);
}

#[tokio::test]
async fn dropping_gateway_requests_bounded_background_tree_cleanup() {
    let fixture = fixture("gateway-drop");
    let gateway = gateway(&fixture);
    let session = "test-gateway-drop";
    let started = terminal(
        &gateway,
        session,
        json!({ "command": "python3 process_tree.py", "background": true }),
    )
    .await;
    assert!(started.ok, "background start: {:?}", started.error);
    let tree = Tree::ready(fixture.project()).await;
    drop(gateway);
    tree.assert_closed().await;
}

#[tokio::test]
async fn foreground_leader_exit_does_not_wait_on_grandchild_stdout() {
    let fixture = fixture("exiting-tree");
    let path = fixture.project().to_path_buf();
    let run = tokio::spawn(async move {
        execute_shell_command_timed(&path, "python3 process_tree.py early-exit", 15).await
    });
    let tree = Tree::ready(fixture.project()).await;
    std::fs::write(fixture.project().join("exit-leader"), "exit").unwrap();
    let result = timeout(Duration::from_secs(4), run)
        .await
        .expect("leader exit must have a bounded pipe drain")
        .unwrap();
    assert!(
        result.ok
            && result.output.contains("tree-stdout-ready")
            && result.output.contains("tree-stderr-ready"),
        "failed to capture output on normal exit: {} {:?}",
        result.output,
        result.error
    );
    tree.assert_closed().await;
}

#[tokio::test]
async fn background_deadline_reaps_tree_and_persists_timed_out_state() {
    let fixture = fixture("background-deadline");
    let gateway = gateway_with_limits(
        &fixture,
        BackgroundLimits {
            timeout_secs: 3,
            ..Default::default()
        },
    );
    let started = terminal(
        &gateway,
        "deadline-owner",
        json!({"command":"python3 process_tree.py", "background":true, "timeoutSecs":9999}),
    )
    .await;
    let id = task_id(&started);
    let tree = Tree::ready(fixture.project()).await;
    let record = wait_terminal(&fixture, &id).await;
    assert_eq!(record.status, "timedOut");
    assert_eq!(
        record.exit_code, None,
        "timeout is not a successful process exit"
    );
    assert_eq!(
        record.deadline_at_ms - record.started_at_ms,
        3000,
        "caller cannot enlarge the owner limit"
    );
    tree.assert_closed().await;
    let polled = terminal(
        &gateway,
        "deadline-owner",
        json!({"op":"poll", "taskId":id}),
    )
    .await;
    assert!(
        polled.output.contains("已超时") && polled.output.contains("tree-stdout-ready"),
        "{} {:?}",
        polled.output,
        polled.error
    );
}

#[tokio::test]
async fn background_concurrency_is_shared_and_rejected_before_side_effects() {
    let fixture = fixture("background-concurrency");
    let limits = BackgroundLimits {
        max_running: 1,
        timeout_secs: 5,
        ..Default::default()
    };
    let gateway_a = gateway_with_limits(&fixture, limits);
    let gateway_b = gateway_with_limits(&fixture, limits);
    std::fs::write(
        fixture.project().join("must_not_run.py"),
        "from pathlib import Path\nPath('must-not-exist').touch()\n",
    )
    .unwrap();
    let first = terminal(
        &gateway_a,
        "capacity-owner",
        json!({"command":"sleep 30", "background":true}),
    )
    .await;
    let id = task_id(&first);
    let rejected = terminal(
        &gateway_b,
        "other-owner",
        json!({"command":"python3 must_not_run.py", "background":true}),
    )
    .await;
    assert!(!rejected.ok && rejected.error.unwrap_or_default().contains("并发已达上限"));
    assert!(!fixture.project().join("must-not-exist").exists());
    assert_eq!(store(&fixture).list_terminal_jobs().unwrap().len(), 1);
    let killed = terminal(
        &gateway_b,
        "capacity-owner",
        json!({"op":"kill", "taskId":id}),
    )
    .await;
    assert!(
        killed.ok,
        "same authorized owner can collect a live job through a rebuilt gateway: {:?}",
        killed.error
    );
    gateway_a.shutdown_background().await.unwrap();
}

#[tokio::test]
async fn background_output_and_retained_disk_are_bounded() {
    let fixture = fixture("background-output");
    let limits = BackgroundLimits {
        max_running: 1,
        max_retained: 2,
        max_output_bytes: 4096,
        ..Default::default()
    };
    let gateway = gateway_with_limits(&fixture, limits);
    std::fs::write(fixture.project().join("output.py"), "import sys\nfor _ in range(4000):\n print('bounded-output-marker')\n print('stderr-output-marker', file=sys.stderr)\n").unwrap();
    let started = terminal(
        &gateway,
        "output-owner",
        json!({"command":"python3 output.py", "background":true}),
    )
    .await;
    let first_id = task_id(&started);
    let record = wait_terminal(&fixture, &first_id).await;
    assert_eq!(record.status, "exited");
    assert_eq!(record.exit_code, Some(0));
    assert!(
        record.output_truncated
            && record.log_bytes <= limits.max_output_bytes
            && record.output_bytes <= limits.max_output_bytes
    );
    let file = fixture
        .oracle()
        .join("data/terminal_job_output")
        .join(format!("{first_id}.jsonl"));
    assert_eq!(file.metadata().unwrap().len(), record.log_bytes as u64);
    let polled = terminal(
        &gateway,
        "output-owner",
        json!({"op":"poll", "taskId":first_id}),
    )
    .await;
    assert!(polled.ok && polled.output.contains("已截断"));
    for _ in 0..3 {
        let started = terminal(
            &gateway,
            "output-owner",
            json!({"command":"echo retained", "background":true}),
        )
        .await;
        wait_terminal(&fixture, &task_id(&started)).await;
    }
    let records = store(&fixture).list_terminal_jobs().unwrap();
    assert_eq!(records.len(), limits.max_retained);
    assert!(
        !file.exists(),
        "evicting old metadata must also delete its bounded output log"
    );
    let count = std::fs::read_dir(fixture.oracle().join("data/terminal_job_output"))
        .unwrap()
        .count();
    assert!(count <= limits.max_retained);
}

#[tokio::test]
async fn large_background_output_keeps_completion_tail_and_reports_exact_cursor_gap() {
    let fixture = fixture("background-cursor-rotation");
    let limits = BackgroundLimits {
        max_output_bytes: 32 * 1024,
        ..Default::default()
    };
    let gateway = gateway_with_security(
        &fixture,
        limits,
        SecurityConfig {
            tool_output_spill_chars: 512,
            ..Default::default()
        },
    )
    .with_hooks(exm_core::config::HooksConfig {
        post_tool: vec!["printf 'fixture hook warning'; exit 1".into()],
        ..Default::default()
    });
    let session = "rotation-owner";
    std::fs::write(
        fixture.project().join("large_output.py"),
        r#"
from pathlib import Path
import sys
import time

sys.stdout.buffer.write(b"progress: building\n" * 15000 + "构建结束：build-complete\n".encode())
sys.stdout.flush()
deadline = time.monotonic() + 8
while not Path("finish").is_file() and time.monotonic() < deadline:
    time.sleep(0.01)
if not Path("finish").is_file():
    sys.exit(10)
sys.stderr.buffer.write("测试失败：fixture error 🦀\n".encode())
sys.stderr.flush()
sys.exit(23)
"#,
    )
    .unwrap();
    let id = task_id(
        &terminal(
            &gateway,
            session,
            json!({"command":"python3 large_output.py", "background":true}),
        )
        .await,
    );
    let stdout = "progress: building\n".repeat(15000) + "构建结束：build-complete\n";
    let stderr = "测试失败：fixture error 🦀\n";
    assert!(stdout.len() > 200_000);
    timeout(Duration::from_secs(4), async {
        loop {
            let result = terminal(&gateway, session, json!({"op":"poll", "taskId":id})).await;
            assert!(result.ok, "{:?}", result.error);
            if result.output.contains("build-complete") {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("真实 stdout 必须先被观察到，再释放 stderr");
    std::fs::write(fixture.project().join("finish"), "finish").unwrap();
    let record = wait_terminal(&fixture, &id).await;
    let expected = stdout.clone() + stderr;
    assert_eq!(record.exit_code, Some(23));
    assert_eq!(record.output_end_byte, Some(expected.len() as u64));
    assert!(record.output_truncated && record.log_bytes <= limits.max_output_bytes);

    let first = output_page(
        &gateway,
        session,
        json!({"op":"poll", "taskId":id, "cursor":0, "limitBytes":2048}),
    )
    .await;
    let retained_start = first["retainedStart"].as_u64().unwrap();
    let end = expected.len() as u64;
    assert!(retained_start > 0);
    assert_eq!(first["droppedBytes"], retained_start);
    assert_eq!(first["skippedBytes"], retained_start);
    assert_eq!(first["retainedEnd"], end);
    assert_eq!(first["outputTruncated"], true);
    assert_eq!(first["postToolWarning"], "fixture hook warning");
    let mut pages = 0;
    let mut seen = retained_start;
    let mut last_sequence = 0;
    let mut text = String::new();
    let mut page = first;
    loop {
        assert_eq!(page["status"], "exited");
        assert_eq!(page["exitCode"], 23);
        let page_start = seen;
        for chunk in page["chunks"].as_array().unwrap() {
            assert_eq!(chunk["startByte"], seen);
            let sequence = chunk["sequence"].as_u64().unwrap();
            assert!(sequence >= last_sequence);
            last_sequence = sequence;
            let piece = chunk["text"].as_str().unwrap();
            assert_eq!(
                chunk["stream"],
                if seen < stdout.len() as u64 {
                    "stdout"
                } else {
                    "stderr"
                }
            );
            seen += piece.len() as u64;
            text.push_str(piece);
        }
        assert_eq!(page["nextCursor"], seen);
        assert!((1..=2048).contains(&(seen - page_start)));
        pages += 1;
        assert!(pages < 100, "分页游标必须推进");
        if !page["hasMore"].as_bool().unwrap() {
            break;
        }
        page = output_page(
            &gateway,
            session,
            json!({"op":"poll", "taskId":id, "cursor":seen, "limitBytes":2048}),
        )
        .await;
        assert_eq!(page["skippedBytes"], 0);
    }
    assert!(pages > 1);
    assert_eq!(seen, end);
    assert_eq!(text, &expected[retained_start as usize..]);
    assert!(text.contains("build-complete") && text.ends_with(stderr));
    let invalid = terminal(
        &gateway,
        session,
        json!({"op":"poll", "taskId":id, "cursor":stdout.len() + 1}),
    )
    .await;
    assert!(!invalid.ok && invalid.error.unwrap_or_default().contains("UTF-8"));
    let unauthorized = terminal(
        &gateway,
        "other-owner",
        json!({"op":"poll", "taskId":id, "cursor":end + 10}),
    )
    .await;
    assert!(
        !unauthorized.ok
            && unauthorized
                .error
                .unwrap_or_default()
                .contains("不属于当前会话")
    );
    let listed = terminal(&gateway, session, json!({"op":"list"})).await;
    assert!(listed.ok && listed.output.contains(&id) && listed.output.contains("fixture error"));
    let legacy = terminal(&gateway, session, json!({"op":"poll", "taskId":id})).await;
    assert!(
        legacy.ok
            && legacy.output.contains("build-complete")
            && legacy.output.contains("fixture error")
    );
    assert!(
        !fixture.project().join(".exmachina/tool-output").exists(),
        "查询不得反复在项目里落盘"
    );
}

#[tokio::test]
async fn cursor_wait_is_bounded_and_wakes_on_output_and_exit() {
    let fixture = fixture("background-cursor-wait");
    let gateway = gateway(&fixture);
    let session = "wait-owner";
    std::fs::write(
        fixture.project().join("wait_output.py"),
        r#"
from pathlib import Path
import sys
import time

for flag in ("emit", "exit"):
    deadline = time.monotonic() + 8
    while not Path(flag).is_file() and time.monotonic() < deadline:
        time.sleep(0.01)
    if not Path(flag).is_file():
        sys.exit(10)
    if flag == "emit":
        print("page-ready", flush=True)
"#,
    )
    .unwrap();
    let id = task_id(
        &terminal(
            &gateway,
            session,
            json!({"command":"python3 wait_output.py", "background":true}),
        )
        .await,
    );
    let started = tokio::time::Instant::now();
    let empty = timeout(
        Duration::from_secs(1),
        output_page(
            &gateway,
            session,
            json!({"op":"poll", "taskId":id, "cursor":0, "waitMs":30}),
        ),
    )
    .await
    .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(30));
    assert_eq!(empty["status"], "running");
    assert_eq!(empty["nextCursor"], 0);

    let pending = output_page(
        &gateway,
        session,
        json!({"op":"poll", "taskId":id, "cursor":0, "waitMs":2000}),
    );
    tokio::pin!(pending);
    assert!(futures_util::poll!(&mut pending).is_pending());
    std::fs::write(fixture.project().join("emit"), "emit").unwrap();
    let page = timeout(Duration::from_secs(1), &mut pending)
        .await
        .expect("输出须提前唤醒");
    assert_eq!(page["chunks"][0]["text"], "page-ready\n");
    assert_eq!(page["status"], "running");
    let end = page["nextCursor"].as_u64().unwrap();

    let pending = output_page(
        &gateway,
        session,
        json!({"op":"poll", "taskId":id, "cursor":end, "waitMs":2000}),
    );
    tokio::pin!(pending);
    assert!(futures_util::poll!(&mut pending).is_pending());
    std::fs::write(fixture.project().join("exit"), "exit").unwrap();
    let page = timeout(Duration::from_secs(1), &mut pending)
        .await
        .expect("退出须提前唤醒");
    assert_eq!(page["status"], "exited");
    assert_eq!(page["exitCode"], 0);
    assert_eq!(page["nextCursor"], end);
    assert_eq!(page["chunks"], json!([]));
}

#[tokio::test]
async fn typed_terminal_approval_replays_background_semantics_and_rejects_stale_scope() {
    let fixture = fixture("typed-terminal-approval");
    let mut cfg = ExmConfig::load(fixture.oracle());
    cfg.workspace_root = fixture.project().to_path_buf();
    cfg.data_dir = fixture.oracle().join("data");
    cfg.agents_dir = fixture.oracle().join("agents");
    cfg.use_mock = true;
    cfg.memory_enabled = false;
    cfg.security.exec_approval = "always".into();
    cfg.security.approval_wait_secs = 0;
    let core = Core::with_config(cfg).unwrap();
    let workspace = fixture.project().canonicalize().unwrap();
    let session = core
        .store
        .create_session_in(
            "typed terminal approval",
            "exmachina",
            Some(workspace.to_string_lossy().into_owned()),
        )
        .unwrap();
    let tools = core.orchestrator().tools.clone();

    let pending_result = exm_core::workspace::scope(
        Some(workspace.clone()),
        terminal(
            tools.as_ref(),
            &session.id,
            json!({
                "command": "rm -f approval-marker",
                "background": true,
                "timeoutSecs": 7
            }),
        ),
    )
    .await;
    assert!(!pending_result.ok);
    let pending = core.approval_list(Some("pending"), 10).unwrap();
    assert_eq!(pending.len(), 1);
    let request = &pending[0];
    assert!(request.command.starts_with("[terminal] [红线]"));
    assert_eq!(
        core.approval_workspace(request).unwrap().as_deref(),
        Some(workspace.to_str().unwrap())
    );
    let mut meta: Value = serde_json::from_str(request.meta_json.as_deref().unwrap()).unwrap();
    assert_eq!(meta["terminalAction"]["background"], true);
    assert_eq!(meta["terminalAction"]["timeoutSecs"], 7);
    assert_eq!(
        meta["terminalAction"]["workspace"],
        workspace.to_string_lossy().as_ref()
    );

    let approved = exm_core::workspace::scope(
        Some(fixture.outside().to_path_buf()),
        core.approval_decide(&request.id, true),
    )
    .await
    .unwrap();
    assert_eq!(approved.status, "executed", "{:?}", approved.result);
    let task_id = approved
        .result
        .as_deref()
        .unwrap()
        .lines()
        .find_map(|line| line.split_once("taskId=").map(|(_, id)| id.to_string()))
        .expect("批准后台终端必须返回 taskId");
    let record = wait_terminal(&fixture, &task_id).await;
    assert_eq!(record.workspace, workspace.to_string_lossy());
    assert!(record.deadline_at_ms >= record.started_at_ms + 6_000);
    assert!(record.deadline_at_ms <= record.started_at_ms + 8_000);

    let expired_id = exm_core::types::new_id();
    meta["terminalAction"]["expiresAtMs"] = json!(1);
    let expired = exm_core::types::ApprovalRequest {
        id: expired_id.clone(),
        session_id: session.id.clone(),
        node_id: None,
        agent_id: "coding-agent".into(),
        command: request.command.clone(),
        status: "pending".into(),
        result: None,
        created_at: exm_core::types::now_iso(),
        decided_at: None,
        origin_user: None,
        origin_role: None,
        meta_json: Some(meta.to_string()),
    };
    core.store.add_approval(&expired).unwrap();
    let expired_result = core.approval_decide(&expired_id, true).await.unwrap();
    assert_eq!(expired_result.status, "failed");
    assert!(expired_result.result.unwrap().contains("未执行"));

    let out_of_bound_id = exm_core::types::new_id();
    let outside = fixture.outside().canonicalize().unwrap();
    let bound_session = core
        .store
        .create_session_in(
            "bound session",
            "exmachina",
            Some(workspace.to_string_lossy().into_owned()),
        )
        .unwrap();
    let created_at = meta["terminalAction"]["createdAtMs"].as_u64().unwrap();
    meta["terminalAction"]["expiresAtMs"] = json!(created_at + 300_000);
    meta["terminalAction"]["workspace"] = json!(outside.to_string_lossy());
    meta["projectWorkspace"] = json!(outside.to_string_lossy());
    let out_of_bound = exm_core::types::ApprovalRequest {
        id: out_of_bound_id.clone(),
        session_id: bound_session.id,
        node_id: None,
        agent_id: "coding-agent".into(),
        command: request.command.clone(),
        status: "pending".into(),
        result: None,
        created_at: exm_core::types::now_iso(),
        decided_at: None,
        origin_user: None,
        origin_role: None,
        meta_json: Some(meta.to_string()),
    };
    core.store.add_approval(&out_of_bound).unwrap();
    let out_of_bound_result = core.approval_decide(&out_of_bound_id, true).await.unwrap();
    assert_eq!(out_of_bound_result.status, "failed");
    assert!(out_of_bound_result.result.unwrap().contains("未执行"));
    assert_eq!(core.store.list_terminal_jobs().unwrap().len(), 1);
}

#[tokio::test]
async fn computer_approval_uses_typed_executor_once_and_never_runs_on_deny_or_timeout() {
    let project_fixture = fixture("typed-computer-approval");
    let core = computer_core(&project_fixture, 0);
    let session = core
        .store
        .create_session_in(
            "typed computer approval",
            "exmachina",
            Some(
                project_fixture
                    .project()
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            ),
        )
        .unwrap();
    let tools = core.orchestrator().tools.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
    let calls_in_executor = calls.clone();
    let seen_in_executor = seen.clone();
    tools.set_computer_executor(Some(Arc::new(move |action| {
        calls_in_executor.fetch_add(1, Ordering::SeqCst);
        seen_in_executor
            .lock()
            .unwrap()
            .push((action.op.clone(), action.args.clone()));
        Ok(())
    })));

    let denied_tool = tools
        .execute(
            "coding-agent",
            &session.id,
            &[ToolName::Computer],
            ToolName::Computer,
            &json!({"op":"click", "x":12, "y":34, "button":"right"}),
        )
        .await;
    assert!(!denied_tool.ok);
    let denied = core.approval_list(Some("pending"), 10).unwrap();
    assert_eq!(denied.len(), 1);
    assert!(denied[0]
        .command
        .starts_with("[computer] click right @ (12, 34)"));
    let denied_meta: Value = serde_json::from_str(denied[0].meta_json.as_deref().unwrap()).unwrap();
    assert_eq!(denied_meta["computerAction"]["tool"], "computer");
    assert_eq!(denied_meta["computerAction"]["op"], "click");
    assert_eq!(denied_meta["computerAction"]["args"]["x"], 12);
    let decided_denied = core.approval_decide(&denied[0].id, false).await.unwrap();
    assert_eq!(decided_denied.status, "denied");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "拒绝不得触发机械执行器");

    let approved_tool = tools
        .execute(
            "coding-agent",
            &session.id,
            &[ToolName::Computer],
            ToolName::Computer,
            &json!({"op":"type", "text":"fixture-only text"}),
        )
        .await;
    assert!(!approved_tool.ok);
    let pending = core.approval_list(Some("pending"), 10).unwrap();
    assert_eq!(pending.len(), 1);
    let approval_id = pending[0].id.clone();
    let (first, duplicate) = tokio::join!(
        core.approval_decide(&approval_id, true),
        core.approval_decide(&approval_id, true),
    );
    assert_eq!(first.unwrap().status, "executed");
    match duplicate {
        Ok(result) => assert_eq!(result.status, "executed"),
        Err(error) => assert!(error.to_string().contains("已处理")),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "并发重复批准只能执行一次");
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        &[("type".into(), json!({"text":"fixture-only text"}))]
    );

    // An old string-only computer approval cannot be reinterpreted as a shell command.
    let legacy = exm_core::types::ApprovalRequest {
        id: exm_core::types::new_id()[..8].into(),
        session_id: session.id.clone(),
        node_id: None,
        agent_id: "coding-agent".into(),
        command: "[computer] type「must-not-run".into(),
        status: "pending".into(),
        result: None,
        created_at: exm_core::types::now_iso(),
        decided_at: None,
        origin_user: None,
        origin_role: None,
        meta_json: None,
    };
    core.store.add_approval(&legacy).unwrap();
    let legacy_result = core.approval_decide(&legacy.id, true).await.unwrap();
    assert_eq!(legacy_result.status, "failed");
    assert!(legacy_result.result.unwrap().contains("未执行"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // A real wait timeout expires the typed action; a later click cannot replay it.
    let timeout_fixture = fixture("computer-approval-timeout");
    let timeout_core = computer_core(&timeout_fixture, 1);
    let timeout_session = timeout_core.create_session("computer timeout").unwrap();
    let timeout_tools = timeout_core.orchestrator().tools.clone();
    let timeout_calls = Arc::new(AtomicUsize::new(0));
    let timeout_counter = timeout_calls.clone();
    timeout_tools.set_computer_executor(Some(Arc::new(move |_| {
        timeout_counter.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })));
    let timed_out = timeout_tools
        .execute(
            "coding-agent",
            &timeout_session.id,
            &[ToolName::Computer],
            ToolName::Computer,
            &json!({"op":"key", "combo":"enter"}),
        )
        .await;
    assert!(!timed_out.ok);
    assert!(timed_out.error.unwrap_or_default().contains("超时"));
    assert_eq!(timeout_calls.load(Ordering::SeqCst), 0);
    let timeout_pending = timeout_core.approval_list(Some("pending"), 10).unwrap();
    assert_eq!(timeout_pending.len(), 1);
    let late = timeout_core
        .approval_decide(&timeout_pending[0].id, true)
        .await
        .unwrap();
    assert_eq!(late.status, "failed");
    assert!(late.result.unwrap().contains("过期"));
    assert_eq!(
        timeout_calls.load(Ordering::SeqCst),
        0,
        "超时后的迟到批准不得执行"
    );
}

#[tokio::test]
async fn background_shutdown_closes_admission_and_persists_cancellation() {
    let fixture = fixture("background-shutdown");
    let gateway = gateway(&fixture);
    let started = terminal(
        &gateway,
        "shutdown-owner",
        json!({"command":"python3 process_tree.py", "background":true}),
    )
    .await;
    let id = task_id(&started);
    let tree = Tree::ready(fixture.project()).await;
    gateway.shutdown_background().await.unwrap();
    tree.assert_closed().await;
    assert_eq!(
        store(&fixture)
            .get_terminal_job(&id)
            .unwrap()
            .unwrap()
            .status,
        "cancelled"
    );
    let denied = terminal(
        &gateway,
        "shutdown-owner",
        json!({"command":"echo must-not-start", "background":true}),
    )
    .await;
    assert!(!denied.ok && denied.error.unwrap_or_default().contains("执行器已关闭"));
    drop(gateway);
    let reopened = gateway_with_limits(&fixture, BackgroundLimits::default());
    let polled = terminal(
        &reopened,
        "shutdown-owner",
        json!({"op":"poll", "taskId":id}),
    )
    .await;
    assert!(
        polled.ok
            && polled.output.contains("已取消")
            && polled.output.contains("tree-stderr-ready")
    );
}

#[tokio::test]
async fn reopening_store_marks_lost_job_interrupted_without_replaying_command() {
    let fixture = fixture("background-recovery");
    std::fs::write(
        fixture.project().join("must_not_run.py"),
        "from pathlib import Path\nPath('must-not-replay').touch()\n",
    )
    .unwrap();
    {
        let seed = store(&fixture);
        seed.put_terminal_job(&TerminalJobRecord {
            id: "bg-lost-fixture".into(),
            session_id: "recovery-owner".into(),
            workspace: fixture
                .project()
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            command: "python3 must_not_run.py".into(),
            started_at_ms: 1,
            deadline_at_ms: 2,
            finished_at_ms: None,
            status: "running".into(),
            exit_code: None,
            output_bytes: 14,
            log_bytes: 0,
            output_chunks: 1,
            output_truncated: true,
            output_end_byte: None,
            output_preview: String::new(),
            error: None,
        })
        .unwrap();
        seed.append_terminal_output(
            "bg-lost-fixture",
            &TerminalJobOutputChunk {
                sequence: 0,
                stream: "stdout".into(),
                text: "before-restart".into(),
                start_byte: None,
            },
        )
        .unwrap();
    }
    let reopened = gateway(&fixture);
    let polled = terminal(
        &reopened,
        "recovery-owner",
        json!({"op":"poll", "taskId":"bg-lost-fixture"}),
    )
    .await;
    assert!(
        polled.ok && polled.output.contains("已中断") && polled.output.contains("before-restart"),
        "{} {:?}",
        polled.output,
        polled.error
    );
    assert_eq!(
        store(&fixture)
            .get_terminal_job("bg-lost-fixture")
            .unwrap()
            .unwrap()
            .status,
        "interrupted"
    );
    let killed = terminal(
        &reopened,
        "recovery-owner",
        json!({"op":"kill", "taskId":"bg-lost-fixture"}),
    )
    .await;
    assert!(killed.ok && killed.output.contains("已中断"));
    assert!(!fixture.project().join("must-not-replay").exists());
    let page = output_page(
        &reopened,
        "recovery-owner",
        json!({"op":"poll", "taskId":"bg-lost-fixture", "cursor":0}),
    )
    .await;
    assert_eq!(page["status"], "interrupted");
    assert_eq!(page["retainedEnd"], 14);
    assert_eq!(page["droppedBytes"], Value::Null, "旧日志没有精确截断量");
    assert_eq!(page["chunks"][0]["text"], "before-restart");
    let denied = terminal(
        &reopened,
        "other-owner",
        json!({"op":"poll", "taskId":"bg-lost-fixture"}),
    )
    .await;
    assert!(!denied.ok && !denied.error.unwrap_or_default().contains("must_not_run"));
}

#[tokio::test]
async fn deleting_session_cancels_its_background_tree() {
    let fixture = fixture("background-delete");
    let mut cfg = exm_core::config::ExmConfig::load(fixture.oracle());
    cfg.workspace_root = fixture.project().to_path_buf();
    cfg.data_dir = fixture.oracle().join("data");
    cfg.agents_dir = fixture.oracle().join("agents");
    cfg.llm = Default::default();
    cfg.llm_profiles.clear();
    cfg.use_mock = true;
    cfg.sandbox.mode = "off".into();
    let core = exm_core::Core::with_config(cfg).unwrap();
    let session = core
        .store
        .create_session_in(
            "delete-test",
            "exmachina",
            Some(fixture.project().to_string_lossy().into_owned()),
        )
        .unwrap();
    let gateway = core.orchestrator().tools.clone();
    let started = terminal(
        &gateway,
        &session.id,
        json!({"command":"python3 process_tree.py", "background":true}),
    )
    .await;
    let id = task_id(&started);
    let tree = Tree::ready(fixture.project()).await;
    core.delete_session(&session.id).unwrap();
    tree.assert_closed().await;
    assert!(core.store.get_terminal_job(&id).unwrap().is_none());
    assert!(!fixture
        .oracle()
        .join("data/terminal_job_output")
        .join(format!("{id}.jsonl"))
        .exists());
    assert!(core.store.get_session(&session.id).unwrap().is_none());
}
