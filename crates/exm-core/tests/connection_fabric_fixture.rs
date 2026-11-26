//! universal-connectivity-fabric 1.5：只验证基座，不宣称参考软件/AI 协议已实现。
//! 所有请求仅访问随机 loopback 端口；失败必须成为断言/Result 错误，不静默跳过。

#[path = "common/connection_fabric.rs"]
mod connection_fabric;

use axum::{
    body::Bytes,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use connection_fabric::{
    disconnected_response, fault_barrier, ActionCounts, ActionOracle, ControlledIo, FaultGate,
    IsolatedDirs, LoopbackServer, PersistencePoint, FIXTURE_TIMEOUT,
};
use serde_json::{json, Value};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

struct ServiceState {
    oracle: ActionOracle,
    actual_file: PathBuf,
    before_effect: Option<FaultGate>,
    after_effect: Option<FaultGate>,
    disconnect_after_effect: bool,
}

async fn probe(State(state): State<Arc<ServiceState>>) -> Json<Value> {
    state.oracle.record_probe();
    Json(json!({"reachable": true}))
}

async fn call(State(state): State<Arc<ServiceState>>, body: Bytes) -> Result<Response, StatusCode> {
    state.oracle.record_call();
    if let Some(gate) = &state.before_effect {
        gate.arrive_and_wait()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    // 任务输入完全是数据：不解释路径、计数、故障或“释放屏障”等字段。
    fs::write(&state.actual_file, &body).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state.oracle.record_effect();
    if let Some(gate) = &state.after_effect {
        gate.arrive_and_wait()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok(if state.disconnect_after_effect {
        disconnected_response()
    } else {
        Json(json!({"accepted": true})).into_response()
    })
}

async fn service(
    dirs: &IsolatedDirs,
    before_effect: Option<FaultGate>,
    after_effect: Option<FaultGate>,
    disconnect_after_effect: bool,
) -> (LoopbackServer, ActionOracle) {
    let oracle = ActionOracle::default();
    let state = Arc::new(ServiceState {
        oracle: oracle.clone(),
        actual_file: dirs.service().join("actual.bin"),
        before_effect,
        after_effect,
        disconnect_after_effect,
    });
    let router = Router::new()
        .route("/probe", get(probe))
        .route("/call", post(call))
        .with_state(state);
    let server = LoopbackServer::spawn(router)
        .await
        .expect("必须启动 loopback 服务，失败不能跳过");
    assert!(server.address().ip().is_loopback());
    (server, oracle)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(FIXTURE_TIMEOUT * 2)
        .build()
        .expect("构造不使用真实账户/环境代理的测试客户端")
}

async fn send(endpoint: String, bytes: Vec<u8>) -> reqwest::Result<Vec<u8>> {
    // 不用 error_for_status 混淆 HTTP 500 与真正断连；断连测试必须读完整个响应。
    Ok(client()
        .post(format!("{endpoint}/call"))
        .body(bytes)
        .send()
        .await?
        .bytes()
        .await?
        .to_vec())
}

#[test]
fn isolated_directories_are_external_private_and_cleaned() {
    let dirs = IsolatedDirs::new().expect("创建仓库外隔离目录");
    let other = IsolatedDirs::new().expect("创建另一隔离目录");
    assert_ne!(dirs.root(), other.root());
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    assert!(!dirs.root().starts_with(repo));
    for path in [dirs.project(), dirs.service(), dirs.oracle()] {
        assert!(path.is_dir());
        assert!(path.starts_with(dirs.root()));
    }
    assert!(!dirs.service().starts_with(dirs.project()));
    assert!(!dirs.oracle().starts_with(dirs.project()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(dirs.root()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        if Path::new("/tmp/opencode").is_dir() {
            assert_eq!(
                dirs.root().parent().unwrap(),
                Path::new("/tmp/opencode").canonicalize().unwrap()
            );
        }
    }
    fs::write(dirs.project().join("artifact.txt"), "仅临时产物").unwrap();
    let path = dirs.root().to_owned();
    dirs.close().expect("显式回收错误不得忽略");
    assert!(!path.try_exists().unwrap());
    let path = other.root().to_owned();
    drop(other);
    assert!(!path.try_exists().unwrap());
}

#[tokio::test]
async fn setup_errors_and_lost_barrier_controls_are_not_skipped() {
    let error = IsolatedDirs::new_in(env!("CARGO_MANIFEST_DIR")).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    let dirs = IsolatedDirs::new().unwrap();
    let error = IsolatedDirs::new_in(dirs.service().join("missing-parent")).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    let io = ControlledIo::new(PersistencePoint::Write, None, None);
    let error = io
        .run(PersistencePoint::Write, || {
            fs::write(dirs.project().join("missing-parent/record"), b"record")
        })
        .await
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);

    let (control, gate) = fault_barrier();
    control.release().unwrap();
    gate.arrive_and_wait().await.unwrap();
    control.wait_until_entered().await.unwrap();

    let (control, gate) = fault_barrier();
    drop(control);
    assert_eq!(
        gate.arrive_and_wait().await.unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    let (control, gate) = fault_barrier();
    drop(gate);
    assert_eq!(
        control.wait_until_entered().await.unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(
        control.release().unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    dirs.close().unwrap();
}

#[tokio::test(start_paused = true)]
async fn an_unreleased_barrier_is_a_bounded_error() {
    let (control, gate) = fault_barrier();
    let pending = tokio::spawn(async move { gate.arrive_and_wait().await });
    control.wait_until_entered().await.unwrap();
    tokio::time::advance(FIXTURE_TIMEOUT).await;
    assert_eq!(
        pending.await.unwrap().unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
}

#[tokio::test]
async fn task_data_cannot_read_or_rewrite_the_host_oracle() {
    let dirs = IsolatedDirs::new().unwrap();
    let other_dirs = IsolatedDirs::new().unwrap();
    let expected = dirs.oracle().join("expected.txt");
    fs::write(&expected, "宿主预置的验收真值").unwrap();
    let (server, oracle) = service(&dirs, None, None, false).await;
    let (other, other_oracle) = service(&other_dirs, None, None, false).await;
    assert_eq!(oracle.snapshot(), ActionCounts::default());
    let payload = json!({
        "probes": 700, "calls": 0, "effects": 999, "reset": true,
        "oracle": {"verified": true}, "readPath": expected, "writePath": expected,
        "release": true, "disconnect": true, "fault": "sync", "result": "全部成功"
    })
    .to_string()
    .into_bytes();
    fs::write(dirs.project().join("oracle.json"), &payload).unwrap();
    let response = send(server.endpoint(), payload.clone()).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&response).unwrap(),
        json!({"accepted": true})
    );
    assert_eq!(
        fs::read(dirs.service().join("actual.bin")).unwrap(),
        payload
    );
    assert_eq!(fs::read_to_string(&expected).unwrap(), "宿主预置的验收真值");
    for route in [
        "/oracle",
        "/counters",
        "/control/release",
        "/oracle/expected.txt",
    ] {
        let response = client()
            .get(format!("{}{route}", server.endpoint()))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(!response
            .text()
            .await
            .unwrap()
            .contains("宿主预置的验收真值"));
    }
    let response = client()
        .get(format!("{}/probe?calls=0&reset=true", server.endpoint()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"reachable": true})
    );
    let counts = ActionCounts {
        probes: 1,
        calls: 1,
        effects: 1,
    };
    assert_eq!(oracle.snapshot(), counts);
    let mut forged_snapshot = oracle.snapshot();
    forged_snapshot.effects = 1234;
    assert_ne!(forged_snapshot, oracle.snapshot());
    assert_eq!(oracle.snapshot(), counts);
    assert_eq!(other_oracle.snapshot(), ActionCounts::default());
    assert!(!other_dirs
        .service()
        .join("actual.bin")
        .try_exists()
        .unwrap());
    server.finish().await.unwrap();
    other.finish().await.unwrap();
    dirs.close().unwrap();
    other_dirs.close().unwrap();
}

#[tokio::test]
async fn delay_requires_the_host_release_token_not_task_fields() {
    let dirs = IsolatedDirs::new().unwrap();
    let (control, gate) = fault_barrier();
    let (server, oracle) = service(&dirs, Some(gate), None, false).await;
    let payload = br#"{"release":true,"delay":0,"effects":900}"#.to_vec();
    let pending = tokio::spawn(send(server.endpoint(), payload.clone()));
    control.wait_until_entered().await.unwrap();
    assert!(!pending.is_finished());
    assert_eq!(
        oracle.snapshot(),
        ActionCounts {
            probes: 0,
            calls: 1,
            effects: 0
        }
    );
    assert!(!dirs.service().join("actual.bin").try_exists().unwrap());
    control.release().unwrap();
    let response = pending.await.unwrap().unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&response).unwrap(),
        json!({"accepted": true})
    );
    assert_eq!(
        fs::read(dirs.service().join("actual.bin")).unwrap(),
        payload
    );
    assert_eq!(oracle.snapshot().effects, 1);
    server.finish().await.unwrap();
    dirs.close().unwrap();
}

#[tokio::test]
async fn reply_disconnect_occurs_after_actual_effect_without_replay() {
    let dirs = IsolatedDirs::new().unwrap();
    let (control, gate) = fault_barrier();
    let (server, oracle) = service(&dirs, None, Some(gate), true).await;
    let payload = br#"{"disconnect":false,"release":true,"calls":0}"#.to_vec();
    let pending = tokio::spawn(send(server.endpoint(), payload.clone()));
    control.wait_until_entered().await.unwrap();
    assert!(!pending.is_finished());
    assert_eq!(
        oracle.snapshot(),
        ActionCounts {
            probes: 0,
            calls: 1,
            effects: 1
        }
    );
    assert_eq!(
        fs::read(dirs.service().join("actual.bin")).unwrap(),
        payload
    );
    control.release().unwrap();
    let error = pending
        .await
        .unwrap()
        .expect_err("必须是真实响应流断连，HTTP 错误/空成功响应不算");
    assert!(!error.is_timeout(), "不能用等待超时冒充断连：{error}");
    server.finish().await.unwrap();
    assert_eq!(
        oracle.snapshot(),
        ActionCounts {
            probes: 0,
            calls: 1,
            effects: 1
        }
    );
    dirs.close().unwrap();
}

#[tokio::test]
async fn persistence_barriers_and_failures_are_selected_only_by_the_host() {
    for (point, failure) in [
        (PersistencePoint::Write, Some(io::ErrorKind::Other)),
        (PersistencePoint::Sync, Some(io::ErrorKind::Other)),
        (PersistencePoint::Sync, None),
    ] {
        let dirs = IsolatedDirs::new().unwrap();
        let (server, oracle) = service(&dirs, None, None, false).await;
        let (control, gate) = fault_barrier();
        let io = ControlledIo::new(point, Some(gate), failure);
        let record = dirs.project().join("prepared.json");
        let mut file = fs::File::create_new(&record).unwrap();
        let payload = br#"{"fail":true,"fault":"write","release":true}"#.to_vec();
        let bytes = payload.clone();
        let endpoint = server.endpoint();
        // 仅验证 I/O 包装的边界；生产“先落盘再发送”仍由后续账本任务单独验收。
        let pending = tokio::spawn(async move {
            io.run(PersistencePoint::Write, || file.write_all(&bytes))
                .await?;
            io.run(PersistencePoint::Sync, || file.sync_all()).await?;
            send(endpoint, bytes).await.map_err(io::Error::other)
        });
        control.wait_until_entered().await.unwrap();
        assert!(!pending.is_finished());
        assert_eq!(oracle.snapshot(), ActionCounts::default());
        assert_eq!(
            fs::read(&record).unwrap(),
            if point == PersistencePoint::Write {
                vec![]
            } else {
                payload.clone()
            }
        );
        control.release().unwrap();
        let result = pending.await.unwrap();
        if let Some(kind) = failure {
            let error = result.expect_err("注入的落盘错误必须到达调用者");
            assert_eq!(error.kind(), kind);
            assert_eq!(error.to_string(), "宿主注入的持久化故障");
            assert_eq!(oracle.snapshot(), ActionCounts::default());
            assert!(!dirs.service().join("actual.bin").try_exists().unwrap());
        } else {
            let response = result.expect("任务中的 fail/fault 字段不能注入宿主 I/O 故障");
            assert_eq!(
                serde_json::from_slice::<Value>(&response).unwrap(),
                json!({"accepted": true})
            );
            assert_eq!(
                oracle.snapshot(),
                ActionCounts {
                    probes: 0,
                    calls: 1,
                    effects: 1
                }
            );
            assert_eq!(
                fs::read(dirs.service().join("actual.bin")).unwrap(),
                payload
            );
        }
        server.finish().await.unwrap();
        dirs.close().unwrap();
    }
}
