//! 7.3/7.4：真实 loopback HTTP 连接、断连/取消未确认与回收协议。
//! 只使用临时项目和本机随机端口，不启动真实工作者/容器，不读取账号或配置。

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use exm_core::{
    exec_backend::*,
    execution_context::{ExecutionContext, WorkspaceBoundary},
    types::ToolScope,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

struct Stub {
    descriptor: ExecutionBackendDescriptor,
    mode: u8,
    starts: AtomicUsize,
    cancels: AtomicUsize,
    reaps: AtomicUsize,
    queries: AtomicUsize,
    requests: parking_lot::Mutex<Vec<Value>>,
}

struct Fixture {
    root: std::path::PathBuf,
    endpoint: String,
    state: Arc<Stub>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

async fn capabilities(State(state): State<Arc<Stub>>) -> Json<ExecutionBackendDescriptor> {
    Json(state.descriptor.clone())
}

fn frame(id: &str, result: impl serde::Serialize) -> Response {
    Json(json!({ "schemaVersion": 1, "jobId": id, "result": result })).into_response()
}

fn disconnect() -> Response {
    let body =
        axum::body::Body::from_stream(futures_util::stream::iter([Err::<axum::body::Bytes, _>(
            std::io::Error::new(std::io::ErrorKind::ConnectionReset, "fixture disconnect"),
        )]));
    (StatusCode::OK, body).into_response()
}

async fn start(
    State(state): State<Arc<Stub>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    assert_eq!(headers["idempotency-key"], id);
    assert_eq!(body["jobId"], id);
    state.starts.fetch_add(1, Ordering::SeqCst);
    state.requests.lock().push(body);
    if state.mode == 3 {
        return disconnect();
    }
    frame(&id, json!({ "accepted": true }))
}

async fn status(State(state): State<Arc<Stub>>, Path(id): Path<String>) -> Response {
    state.queries.fetch_add(1, Ordering::SeqCst);
    if state.mode == 1 {
        // 真实关闭响应流，不能靠内存 FakeLauncher 冒充网络断连。
        return disconnect();
    }
    let body = &state.requests.lock()[0];
    let project = body["spec"]["execution"]["workspace"]["projectRoot"]
        .as_str()
        .unwrap();
    let artifact = std::path::Path::new(project).join("receipt.txt");
    std::fs::write(&artifact, "loopback receipt").unwrap();
    frame(
        if state.mode == 4 { "job-wrong" } else { &id },
        BackendJobPoll::Exited {
            code: 0,
            artifacts: vec![artifact.to_string_lossy().into_owned()],
            logs: vec!["fixture complete".into()],
        },
    )
}

async fn cancel(State(state): State<Arc<Stub>>, Path(id): Path<String>) -> Response {
    state.cancels.fetch_add(1, Ordering::SeqCst);
    frame(&id, json!({"stopped": state.mode != 2}))
}

async fn reap(State(state): State<Arc<Stub>>, Path(id): Path<String>) -> Response {
    state.reaps.fetch_add(1, Ordering::SeqCst);
    frame(
        &id,
        JobOutcome {
            code: (state.mode == 0).then_some(0),
            artifacts: vec![],
            logs: vec!["fixture reaped".into()],
        },
    )
}

async fn fixture(mode: u8, verified: bool) -> Fixture {
    let root = std::env::temp_dir().join(format!("exm-backend-http-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let mut descriptor = ExecutionBackendRegistry::builtin(false, false, true)
        .get("remote")
        .unwrap()
        .clone();
    if !verified {
        descriptor.readiness = BackendReadiness::Unverified {
            detail: "fixture dependency unverified".into(),
        };
    }
    let state = Arc::new(Stub {
        descriptor,
        mode,
        starts: AtomicUsize::new(0),
        cancels: AtomicUsize::new(0),
        reaps: AtomicUsize::new(0),
        queries: AtomicUsize::new(0),
        requests: parking_lot::Mutex::new(vec![]),
    });
    let router = Router::new()
        .route("/capabilities", get(capabilities))
        .route("/jobs/:id", post(start).get(status).delete(reap))
        .route("/jobs/:id/cancel", post(cancel))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Fixture {
        root,
        endpoint,
        state,
        server,
    }
}

fn prepared(
    fixture: &Fixture,
    backend: &RemoteBackendLauncher,
) -> Result<PreparedJob, BackendSelectionError> {
    prepared_for_session(fixture, backend, "fixture-session")
}

fn prepared_for_session(
    fixture: &Fixture,
    backend: &RemoteBackendLauncher,
    session: &str,
) -> Result<PreparedJob, BackendSelectionError> {
    let context = ExecutionContext::root(
        session,
        WorkspaceBoundary::project(fixture.root.to_string_lossy()),
        ToolScope::Tools,
        ["terminal"],
        false,
        false,
        Some(20),
    )
    .unwrap();
    let authority = BackendAuthorization::new(
        context.clone(),
        Some("fixture-project".into()),
        IdentityRequest::default(),
    )
    .unwrap();
    let request = BackendSelectionRequest {
        execution: context.clone(),
        project_id: Some("fixture-project".into()),
        required_tools: ["terminal".into()].into_iter().collect(),
        browser: false,
        network: NetworkRequirement::none(),
        mounts: vec![],
        identity: IdentityRequest::default(),
        resources: ResourceRequest {
            memory_mb: None,
            processes: None,
            timeout_ms: 10_000,
        },
        persistence: PersistenceCapability::Ephemeral,
        cancellation: true,
        external_effects: false,
        isolation_required: true,
        preferred: vec![],
        required_backend: Some("remote".into()),
    };
    let mut registry = ExecutionBackendRegistry::new();
    registry.register(backend.descriptor().clone()).unwrap();
    registry.prepare_job(
        &authority,
        &request,
        JobSpec {
            execution: context,
            project_id: Some("fixture-project".into()),
            backend_id: "remote".into(),
            operation: JobOperation::Write,
            operation_id: "fixture-operation".into(),
            command: vec!["fixture-command".into()],
            timeout_ms: 10_000,
            artifacts: vec![],
            summary: "loopback fixture".into(),
        },
    )
}

#[tokio::test]
async fn 远端替身_真实http回执可回读且证据绑定完整() {
    let fixture = fixture(0, true).await;
    let backend = Arc::new(
        RemoteBackendLauncher::connect(&fixture.endpoint, String::new())
            .await
            .unwrap(),
    );
    let prepared = prepared(&fixture, &backend).unwrap();
    let expected = prepared.spec().execution.clone();
    let supervisor = ExecJobSupervisor::new(backend);
    let started = supervisor.start(prepared).await.unwrap();
    let result = supervisor.poll(&started.job_id).await.unwrap();
    assert_eq!(result.state, JobState::Succeeded);
    assert_eq!(result.evidence.session_id, "fixture-session");
    assert_eq!(
        result.evidence.root_execution_id,
        expected.root_execution_id
    );
    assert_eq!(result.evidence.execution_id, expected.execution_id);
    assert_eq!(
        result.evidence.evidence_session_id,
        expected.evidence_session_id
    );
    assert_eq!(
        result.evidence.project_id.as_deref(),
        Some("fixture-project")
    );
    assert_eq!(result.evidence.artifacts.len(), 1);
    assert_eq!(
        std::fs::read_to_string(&result.evidence.artifacts[0]).unwrap(),
        "loopback receipt"
    );
    assert!(supervisor.reap(&started.job_id).await.unwrap().reaped);
    assert_eq!(fixture.state.starts.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.state.cancels.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.state.reaps.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn 远端替身_真实断连不重放且清理不能编造写成功() {
    let fixture = fixture(1, true).await;
    let backend = Arc::new(
        RemoteBackendLauncher::connect(&fixture.endpoint, String::new())
            .await
            .unwrap(),
    );
    let prepared = prepared(&fixture, &backend).unwrap();
    let supervisor = ExecJobSupervisor::new(backend);
    let started = supervisor.start(prepared.clone()).await.unwrap();
    let unknown = supervisor.poll(&started.job_id).await.unwrap();
    assert_eq!(unknown.state, JobState::Unknown);
    assert_eq!(unknown.end_reason, Some(JobEndReason::BackendDisconnected));
    assert_eq!(
        supervisor.start(prepared).await.unwrap().job_id,
        started.job_id
    );
    assert_eq!(
        supervisor.cancel(&started.job_id).await.unwrap().state,
        JobState::Unknown
    );
    let cleaned = supervisor.reap(&started.job_id).await.unwrap();
    assert_eq!(cleaned.state, JobState::Unknown);
    assert!(cleaned.reaped);
    assert_eq!(fixture.state.starts.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.state.queries.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn 远端替身_只接受取消没有停止证明时返回unknown() {
    let fixture = fixture(2, true).await;
    let backend = Arc::new(
        RemoteBackendLauncher::connect(&fixture.endpoint, String::new())
            .await
            .unwrap(),
    );
    let job = prepared(&fixture, &backend).unwrap();
    let supervisor = ExecJobSupervisor::new(backend);
    let started = supervisor.start(job).await.unwrap();
    let result = supervisor.cancel(&started.job_id).await.unwrap();
    assert_eq!(result.state, JobState::Unknown);
    assert_eq!(result.end_reason, Some(JobEndReason::CancelFailed));
    assert_eq!(fixture.state.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn 远端替身_已提交后丢失启动回执不自动重发() {
    let fixture = fixture(3, true).await;
    let backend = Arc::new(
        RemoteBackendLauncher::connect(&fixture.endpoint, String::new())
            .await
            .unwrap(),
    );
    let job = prepared(&fixture, &backend).unwrap();
    let supervisor = ExecJobSupervisor::new(backend);
    let error = supervisor.start(job.clone()).await.unwrap_err();
    let previous = error.job_id.unwrap();
    assert_eq!(
        supervisor.snapshot(&previous).await.unwrap().state,
        JobState::Unknown
    );
    let duplicate = supervisor.start(job).await.unwrap();
    assert_eq!(duplicate.job_id, previous);
    assert_eq!(duplicate.state, JobState::Unknown);
    assert_eq!(fixture.state.starts.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.state.cancels.load(Ordering::SeqCst), 1);
    supervisor.reap(&previous).await.unwrap();
}

#[tokio::test]
async fn 远端替身_其他作业回执不能冒充本作业成功() {
    let fixture = fixture(4, true).await;
    let backend = Arc::new(
        RemoteBackendLauncher::connect(&fixture.endpoint, String::new())
            .await
            .unwrap(),
    );
    let job = prepared(&fixture, &backend).unwrap();
    let supervisor = ExecJobSupervisor::new(backend);
    let started = supervisor.start(job).await.unwrap();
    let result = supervisor.poll(&started.job_id).await.unwrap();
    assert_eq!(result.state, JobState::Unknown);
    assert!(result.evidence.artifacts.is_empty());
    supervisor.reap(&started.job_id).await.unwrap();
}

#[tokio::test]
async fn 远端替身_未验证依赖与非本适配器句柄均不执行() {
    let fixture = fixture(0, false).await;
    let backend = RemoteBackendLauncher::connect(&fixture.endpoint, String::new())
        .await
        .unwrap();
    assert_eq!(
        prepared(&fixture, &backend).unwrap_err().code,
        BackendRejectCode::NoReadyBackend
    );
    let result = backend
        .poll(&BackendJobHandle {
            job_id: "job-not-owned".into(),
            backend_id: "remote".into(),
            reference: "job-not-owned".into(),
        })
        .await;
    assert!(matches!(result, BackendJobPoll::Disconnected { .. }));
    assert_eq!(fixture.state.starts.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.state.queries.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 后端事件_真实core归属与拒绝审计不泄漏到其他会话() {
    let fixture = fixture(0, true).await;
    let mut config = exm_core::config::ExmConfig::load(&fixture.root);
    config.agents_dir = fixture.root.join("entities");
    config.data_dir = fixture.root.join("data");
    config.use_mock = true;
    config.llm.api_key.clear();
    config.llm.api_keys.clear();
    config.llm.base_url.clear();
    config.llm_profiles.clear();
    config.active_profile.clear();
    let core = Arc::new(exm_core::Core::with_config(config).unwrap());
    let session = core
        .store
        .create_session_in(
            "backend-source",
            "fixture",
            Some(fixture.root.to_string_lossy().into_owned()),
        )
        .unwrap();
    let other = core
        .store
        .create_session_in(
            "backend-other",
            "fixture",
            Some(fixture.root.to_string_lossy().into_owned()),
        )
        .unwrap();
    let backend = Arc::new(
        RemoteBackendLauncher::connect(&fixture.endpoint, String::new())
            .await
            .unwrap(),
    );
    let job = prepared_for_session(&fixture, &backend, &session.id).unwrap();
    let context = job.spec().execution.clone();
    let authority = BackendAuthorization::new(
        context.clone(),
        Some("fixture-project".into()),
        IdentityRequest::default(),
    )
    .unwrap();
    let mut request = BackendSelectionRequest {
        execution: context,
        project_id: Some("fixture-project".into()),
        required_tools: ["terminal".into()].into_iter().collect(),
        browser: false,
        network: NetworkRequirement::none(),
        mounts: vec![],
        identity: IdentityRequest::default(),
        resources: ResourceRequest {
            memory_mb: None,
            processes: None,
            timeout_ms: 10_000,
        },
        persistence: PersistenceCapability::Ephemeral,
        cancellation: true,
        external_effects: false,
        isolation_required: true,
        preferred: vec![],
        required_backend: Some("remote".into()),
    };
    request.execution.session_id = other.id.clone();
    assert_eq!(
        core.select_exec_backend(&authority, &request)
            .unwrap_err()
            .code,
        BackendRejectCode::AuthorizationMismatch
    );
    let supervisor = core.execution_supervisor(backend);
    let started = supervisor.start(job).await.unwrap();
    supervisor.poll(&started.job_id).await.unwrap();
    supervisor.reap(&started.job_id).await.unwrap();
    let events = core.store.list_events(&session.id).unwrap();
    assert!(events
        .iter()
        .any(|event| event["payload"]["type"] == "execution.backend.rejected"));
    let snapshots: Vec<_> = events
        .iter()
        .filter(|event| event["payload"]["type"] == "execution.backend.changed")
        .map(|event| &event["payload"]["payload"])
        .collect();
    assert!(snapshots
        .iter()
        .any(|snapshot| snapshot["state"] == "succeeded"));
    assert!(snapshots
        .iter()
        .all(|snapshot| snapshot["evidence"]["sessionId"] == session.id));
    assert!(core.store.list_events(&other.id).unwrap().is_empty());
    assert_eq!(fixture.state.starts.load(Ordering::SeqCst), 1);
}
