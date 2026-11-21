//! agent 能力第三轮回归（change: harness-agent-round3）：
//! 1. todo 工具：write/list、系统注入渲染、todo.updated 事件、编码个体工具面
//! 2. spawn 后台并行：background 立即返回、status 回收、list 会话隔离、超时终态、未回收提醒

use exm_core::tools::ToolGateway;
use exm_core::types::{AgentDefinition, ToolName};
use exm_core::{config::ExmConfig, Core};
use std::path::{Path, PathBuf};

fn mock_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("exm-h3-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn gateway(dir: &Path) -> (ToolGateway, Core) {
    let cfg = ExmConfig::load(dir);
    let core = Core::with_config(cfg).expect("创建 Core 失败");
    let (events, _rx) = tokio::sync::broadcast::channel(64);
    let sec = exm_core::config::SecurityConfig {
        approval_wait_secs: 2,
        ..Default::default()
    };
    let gw = ToolGateway::new(
        core.config().workspace_root.clone(),
        core.store.clone(),
        core.registry().clone(),
        events,
        sec,
    );
    (gw, core)
}

/// 立即成功的 spawn 执行器
fn instant_exec() -> exm_core::tools::SpawnExec {
    let f = move |_a: &str, _s: &str, task: &str, _t: Vec<ToolName>| {
        let task = task.to_string();
        Box::pin(async move { Ok(format!("完成：{task}")) })
            as futures_util::future::BoxFuture<'static, anyhow::Result<String>>
    };
    std::sync::Arc::new(f)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn todo_写入查询与事件() {
    let dir = mock_dir("todo");
    let (gw, _core) = gateway(&dir);
    // 空清单查询
    let r = gw
        .execute(
            "coding-agent",
            "ses-todo",
            &[ToolName::Todo],
            ToolName::Todo,
            &serde_json::json!({ "op": "list" }),
        )
        .await;
    assert!(r.ok);
    assert!(r.output.contains("无任务清单"));
    // 全量写入
    let r = gw
        .execute(
            "coding-agent",
            "ses-todo",
            &[ToolName::Todo],
            ToolName::Todo,
            &serde_json::json!({ "op": "write", "items": [
                { "content": "定位缺陷", "status": "done" },
                { "content": "修复实现", "status": "in_progress" },
                { "content": "回归验证", "status": "pending" },
            ]}),
        )
        .await;
    assert!(r.ok, "write 应成功：{:?}", r.error);
    assert!(r.output.contains("1/3 完成"), "应含进度：{}", r.output);
    // 渲染注入段
    let block = exm_core::todo::render("ses-todo").unwrap();
    assert!(block.contains("✅ 定位缺陷"));
    assert!(block.contains("🔄 修复实现"));
    assert!(block.contains("⬜ 回归验证"));
    // list 查询
    let r = gw
        .execute(
            "coding-agent",
            "ses-todo",
            &[ToolName::Todo],
            ToolName::Todo,
            &serde_json::json!({ "op": "list" }),
        )
        .await;
    assert!(r.output.contains("任务清单（1/3 完成）"));
    // 空 content 拒绝
    let r = gw
        .execute(
            "coding-agent",
            "ses-todo",
            &[ToolName::Todo],
            ToolName::Todo,
            &serde_json::json!({ "op": "write", "items": [{ "content": "  ", "status": "pending" }] }),
        )
        .await;
    assert!(!r.ok, "空 content 应拒绝");
    // 清空
    let r = gw
        .execute(
            "coding-agent",
            "ses-todo",
            &[ToolName::Todo],
            ToolName::Todo,
            &serde_json::json!({ "op": "write", "items": [] }),
        )
        .await;
    assert!(r.ok);
    assert!(
        exm_core::todo::render("ses-todo").is_none(),
        "清空后不再注入"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn todo_编码个体工具面含todo() {
    // 仓库个体（tool_surface 同口径）：含 edit/patch 的个体必有 todo
    let root = std::env::current_dir().unwrap().join("..").join("..");
    for id in ["coding-agent", "orchestrator", "frontend-agent"] {
        let path = root.join(format!("entities/groups/exmachina/agents/{id}.json"));
        let def: AgentDefinition =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let keys: Vec<&str> = def.tools.iter().map(|t| t.key()).collect();
        assert!(
            keys.contains(&"todo"),
            "{id} 工具面应含 todo（现有 {keys:?}）"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_后台派生_立即返回并回收() {
    let dir = mock_dir("spawn");
    let (gw, _core) = gateway(&dir);
    gw.set_spawn_executor(instant_exec());
    // 后台派生：立即返回 spawnId
    let r = gw
        .execute(
            "orchestrator",
            "ses-sp",
            &[ToolName::Spawn],
            ToolName::Spawn,
            &serde_json::json!({ "task": "调研 A 方案", "background": true }),
        )
        .await;
    assert!(r.ok, "后台派生应立即成功：{:?}", r.error);
    let spawn_id = r
        .output
        .lines()
        .find_map(|l| l.split("spawnId=").nth(1).map(|s| s.trim().to_string()))
        .expect("返回应含 spawnId");
    assert!(!spawn_id.contains('\n'), "spawnId 提取应干净：{spawn_id}");
    // 等任务体完成
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    // status 回收全文
    let r = gw
        .execute(
            "orchestrator",
            "ses-sp",
            &[ToolName::Spawn],
            ToolName::Spawn,
            &serde_json::json!({ "op": "status", "spawnId": spawn_id }),
        )
        .await;
    assert!(r.ok);
    assert!(r.output.contains("已完成"), "应为终态：{}", r.output);
    assert!(
        r.output.contains("调研 A 方案"),
        "应含结果全文：{}",
        r.output
    );
    // 已读后不再提醒
    assert!(
        gw.spawn_pending_note("ses-sp").is_none(),
        "已读终态不应再提醒"
    );
    // list 可见
    let r = gw
        .execute(
            "orchestrator",
            "ses-sp",
            &[ToolName::Spawn],
            ToolName::Spawn,
            &serde_json::json!({ "op": "list" }),
        )
        .await;
    assert!(
        r.output.contains(&spawn_id[..10]),
        "list 应含子任务：{}",
        r.output
    );
    // 会话隔离：另一会话不可见
    let r = gw
        .execute(
            "orchestrator",
            "ses-other",
            &[ToolName::Spawn],
            ToolName::Spawn,
            &serde_json::json!({ "op": "list" }),
        )
        .await;
    assert!(
        r.output.contains("无后台子任务"),
        "跨会话应隔离：{}",
        r.output
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_后台超时终态与未读提醒() {
    let dir = mock_dir("spawn-to");
    let (gw, _core) = gateway(&dir);
    // 慢执行器：睡 30s（timeout 1s 必超时）
    let f = move |_a: &str, _s: &str, _t: &str, _tools: Vec<ToolName>| {
        Box::pin(async move {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            Ok("不应到达".to_string())
        }) as futures_util::future::BoxFuture<'static, anyhow::Result<String>>
    };
    gw.set_spawn_executor(std::sync::Arc::new(f));
    let r = gw
        .execute(
            "orchestrator",
            "ses-to",
            &[ToolName::Spawn],
            ToolName::Spawn,
            &serde_json::json!({ "task": "慢任务", "background": true, "timeoutSecs": 5 }),
        )
        .await;
    assert!(r.ok, "后台启动应成功：{:?}", r.error);
    let spawn_id = r
        .output
        .lines()
        .find_map(|l| l.split("spawnId=").nth(1).map(|s| s.trim().to_string()))
        .unwrap();
    // 超时窗口后查询（5s 超时 + 余量）
    tokio::time::sleep(std::time::Duration::from_millis(6500)).await;
    let r = gw
        .execute(
            "orchestrator",
            "ses-to",
            &[ToolName::Spawn],
            ToolName::Spawn,
            &serde_json::json!({ "op": "status", "spawnId": spawn_id }),
        )
        .await;
    assert!(r.output.contains("超时"), "应为超时终态：{}", r.output);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_未读终态进入提醒注入() {
    let dir = mock_dir("spawn-note");
    let (gw, _core) = gateway(&dir);
    gw.set_spawn_executor(instant_exec());
    let r = gw
        .execute(
            "orchestrator",
            "ses-note",
            &[ToolName::Spawn],
            ToolName::Spawn,
            &serde_json::json!({ "task": "提醒验证", "background": true }),
        )
        .await;
    assert!(r.ok);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    // 未 status 读取 → 提醒注入应出现
    let note = gw.spawn_pending_note("ses-note");
    assert!(note.is_some(), "未读终态应有提醒注入");
    let n = note.unwrap();
    assert!(n.contains("后台子任务结果待收"), "提醒格式：{n}");
    assert!(n.contains("提醒验证"), "提醒应含任务预览：{n}");
    // 终态事件持久化：REST 事件面/前端刷新后可见（append_event 落 store）
    let persisted = _core.store.list_events("ses-note").unwrap_or_default();
    let finished: Vec<_> = persisted
        .iter()
        .filter(|env| {
            env.get("payload")
                .and_then(|p| p.get("type"))
                .and_then(|v| v.as_str())
                == Some("spawn.finished")
        })
        .collect();
    assert_eq!(finished.len(), 1, "spawn.finished 应持久化到事件流");
    // 其它会话不受影响
    assert!(gw.spawn_pending_note("ses-other2").is_none());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_identity_保留项目cwd并拒绝越界项目() {
    let dir = mock_dir("exec-identity");
    let exec_home = dir.join("exec-home");
    let project = exec_home.join("projects").join("demo");
    std::fs::create_dir_all(&project).unwrap();
    let outside = dir.join("outside");
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(&outside).unwrap();
    let gateway_workspace = dir.join("gateway-workspace");
    std::fs::create_dir_all(&gateway_workspace).unwrap();
    let uid = String::from_utf8(
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();

    let mut cfg = ExmConfig::load(&dir);
    cfg.workspace_root = gateway_workspace.clone();
    cfg.security.exec_user = uid.clone();
    cfg.security.exec_home = exec_home.to_string_lossy().into_owned();
    cfg.security.exec_approval = "off".into();
    cfg.security.approval_wait_secs = 10;
    cfg.security.exec_allowlist = vec!["pwd".into()];
    let core = Core::with_config(cfg).unwrap();
    let (events, _rx) = tokio::sync::broadcast::channel(64);
    let gw = ToolGateway::new(
        core.config().workspace_root.clone(),
        core.store.clone(),
        core.registry().clone(),
        events,
        core.config().security.clone(),
    )
    .with_sandbox(exm_core::config::SandboxConfig {
        mode: "off".into(),
        ..Default::default()
    });

    let project_session = core
        .store
        .create_session_in(
            "project",
            "exmachina",
            Some(
                project
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            ),
        )
        .unwrap();
    let result = gw
        .execute(
            "coding-agent",
            &project_session.id,
            &[ToolName::Terminal],
            ToolName::Terminal,
            &serde_json::json!({ "command": "pwd" }),
        )
        .await;
    assert!(result.ok, "项目内命令应成功：{:?}", result.error);
    let identity = gw
        .execute(
            "coding-agent",
            &project_session.id,
            &[ToolName::Terminal],
            ToolName::Terminal,
            &serde_json::json!({ "command": "id -u" }),
        )
        .await;
    assert!(identity.ok, "前台执行身份探测失败：{:?}", identity.error);
    assert_eq!(identity.output.trim(), uid);
    let project_path = project
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(result.output.trim(), project_path);
    let filesystem_result = gw
        .execute(
            "coding-agent",
            &project_session.id,
            &[ToolName::Filesystem],
            ToolName::Filesystem,
            &serde_json::json!({
                "op": "write",
                "path": "project-scope-sentinel.txt",
                "content": "project scoped"
            }),
        )
        .await;
    assert!(
        filesystem_result.ok,
        "文件工具应在绑定项目内写入：{:?}",
        filesystem_result.error
    );
    assert!(project.join("project-scope-sentinel.txt").is_file());
    assert!(
        !core
            .config()
            .workspace_root
            .join("project-scope-sentinel.txt")
            .exists(),
        "文件工具不能回落到全局工作区"
    );
    let symlink_path = project.join("outside-link");
    std::os::unix::fs::symlink(&outside, &symlink_path).unwrap();
    let symlink_write = gw
        .execute(
            "coding-agent",
            &project_session.id,
            &[ToolName::Filesystem],
            ToolName::Filesystem,
            &serde_json::json!({
                "op": "write",
                "path": "outside-link/escaped.txt",
                "content": "must not escape"
            }),
        )
        .await;
    assert!(!symlink_write.ok, "符号链接越界写入必须拒绝");
    assert!(!outside.join("escaped.txt").exists());

    let background = gw
        .execute(
            "coding-agent",
            &project_session.id,
            &[ToolName::Terminal],
            ToolName::Terminal,
            &serde_json::json!({ "command": "pwd", "background": true }),
        )
        .await;
    assert!(background.ok, "后台项目命令应成功：{:?}", background.error);
    let task_id = background
        .output
        .lines()
        .find_map(|line| line.split("taskId=").nth(1).map(str::trim))
        .unwrap()
        .to_string();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let background_result = gw
        .execute(
            "coding-agent",
            &project_session.id,
            &[ToolName::Terminal],
            ToolName::Terminal,
            &serde_json::json!({ "op": "poll", "taskId": task_id }),
        )
        .await;
    assert!(
        background_result.ok,
        "后台 poll 应成功：{:?}",
        background_result.error
    );
    assert!(
        background_result.output.contains(&project_path),
        "前后台 cwd 必须一致：{}",
        background_result.output
    );

    let sentinel_key = format!("EXM_ACCEPTANCE_SENTINEL_{}", std::process::id());
    let sentinel_value = "must-not-reach-exec-child";
    std::env::set_var(&sentinel_key, sentinel_value);
    let env_result = gw
        .execute(
            "coding-agent",
            &project_session.id,
            &[ToolName::Terminal],
            ToolName::Terminal,
            &serde_json::json!({ "command": "env" }),
        )
        .await;
    std::env::remove_var(&sentinel_key);
    assert!(
        env_result.ok,
        "身份净化环境探测失败：{:?}",
        env_result.error
    );
    assert!(
        !env_result.output.contains(sentinel_value),
        "网关 sentinel 不得传给执行子进程"
    );
    assert!(
        env_result
            .output
            .contains(&format!("CARGO_HOME={}/.cargo", exec_home.display())),
        "用户级 cargo 缓存应位于 execHome：{}",
        env_result.output
    );
    std::env::set_var(&sentinel_key, sentinel_value);
    let background_env = gw
        .execute(
            "coding-agent",
            &project_session.id,
            &[ToolName::Terminal],
            ToolName::Terminal,
            &serde_json::json!({ "command": "env", "background": true }),
        )
        .await;
    assert!(
        background_env.ok,
        "后台环境探测应成功：{:?}",
        background_env.error
    );
    let env_task_id = background_env
        .output
        .lines()
        .find_map(|line| line.split("taskId=").nth(1).map(str::trim))
        .unwrap()
        .to_string();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let background_env_result = gw
        .execute(
            "coding-agent",
            &project_session.id,
            &[ToolName::Terminal],
            ToolName::Terminal,
            &serde_json::json!({ "op": "poll", "taskId": env_task_id }),
        )
        .await;
    std::env::remove_var(&sentinel_key);
    assert!(background_env_result.ok);
    assert!(
        !background_env_result.output.contains(sentinel_value),
        "后台子进程不得继承网关 sentinel"
    );
    assert!(
        background_env_result
            .output
            .contains(&format!("PWD={project_path}")),
        "后台环境的 cwd 应为绑定项目：{}",
        background_env_result.output
    );
    let background_identity = gw
        .execute(
            "coding-agent",
            &project_session.id,
            &[ToolName::Terminal],
            ToolName::Terminal,
            &serde_json::json!({ "command": "id -u", "background": true }),
        )
        .await;
    assert!(background_identity.ok);
    let identity_task_id = background_identity
        .output
        .lines()
        .find_map(|line| line.split("taskId=").nth(1).map(str::trim))
        .unwrap()
        .to_string();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let background_identity_result = gw
        .execute(
            "coding-agent",
            &project_session.id,
            &[ToolName::Terminal],
            ToolName::Terminal,
            &serde_json::json!({ "op": "poll", "taskId": identity_task_id }),
        )
        .await;
    assert!(background_identity_result.ok);
    assert!(
        background_identity_result.output.contains(&uid),
        "后台执行身份应与前台相同 ({uid})：{}",
        background_identity_result.output
    );
    assert!(
        env_result
            .output
            .contains(&format!("RUSTUP_HOME={}/.rustup", exec_home.display())),
        "rustup 缓存应位于 execHome：{}",
        env_result.output
    );

    let approval = exm_core::types::ApprovalRequest {
        id: format!("apr-{}", &exm_core::types::new_id()[..8]),
        session_id: project_session.id.clone(),
        node_id: None,
        agent_id: "coding-agent".into(),
        command: "pwd".into(),
        status: "pending".into(),
        result: None,
        created_at: exm_core::types::now_iso(),
        decided_at: None,
        origin_user: None,
        origin_role: None,
        meta_json: Some(
            serde_json::json!({
                "projectWorkspace": project_path,
                "execHome": exec_home.canonicalize().unwrap().to_string_lossy()
            })
            .to_string(),
        ),
    };
    core.store.add_approval(&approval).unwrap();
    let replayed = core.approval_decide(&approval.id, true).await.unwrap();
    assert_eq!(
        replayed.status, "executed",
        "审批代执行应保持项目 cwd：{:?}",
        replayed.result
    );
    assert!(
        replayed
            .result
            .as_deref()
            .unwrap_or_default()
            .contains(&project_path),
        "审批代执行必须留在已批准项目：{:?}",
        replayed.result
    );

    std::env::set_var(&sentinel_key, sentinel_value);
    let mut env_approval = approval.clone();
    env_approval.id = format!("apr-{}", &exm_core::types::new_id()[..8]);
    env_approval.command = "env".into();
    env_approval.status = "pending".into();
    env_approval.decided_at = None;
    env_approval.result = None;
    core.store.add_approval(&env_approval).unwrap();
    let env_replayed = core.approval_decide(&env_approval.id, true).await.unwrap();
    std::env::remove_var(&sentinel_key);
    assert_eq!(env_replayed.status, "executed");
    assert!(
        !env_replayed
            .result
            .as_deref()
            .unwrap_or_default()
            .contains(sentinel_value),
        "审批子进程不得继承网关 sentinel"
    );
    assert!(
        env_replayed
            .result
            .as_deref()
            .unwrap_or_default()
            .contains(&format!("CARGO_HOME={}/.cargo", exec_home.display())),
        "审批执行缓存应落在 execHome：{:?}",
        env_replayed.result
    );
    let mut identity_approval = approval.clone();
    identity_approval.id = format!("apr-{}", &exm_core::types::new_id()[..8]);
    identity_approval.command = "id -u".into();
    identity_approval.status = "pending".into();
    identity_approval.decided_at = None;
    identity_approval.result = None;
    core.store.add_approval(&identity_approval).unwrap();
    let identity_replayed = core
        .approval_decide(&identity_approval.id, true)
        .await
        .unwrap();
    assert_eq!(identity_replayed.status, "executed");
    assert!(
        identity_replayed
            .result
            .as_deref()
            .unwrap_or_default()
            .contains(&uid),
        "审批回放执行身份应与前台一致：{:?}",
        identity_replayed.result
    );

    let unbound_session = core
        .store
        .create_session_in("unbound", "exmachina", None)
        .unwrap();
    let unbound_pwd = gw
        .execute(
            "coding-agent",
            &unbound_session.id,
            &[ToolName::Terminal],
            ToolName::Terminal,
            &serde_json::json!({ "command": "pwd" }),
        )
        .await;
    assert!(
        unbound_pwd.ok,
        "未绑定项目应在 execHome 执行：{:?}",
        unbound_pwd.error
    );
    assert_eq!(
        unbound_pwd.output.trim(),
        exec_home.canonicalize().unwrap().to_string_lossy()
    );
    let unbound_write = gw
        .execute(
            "coding-agent",
            &unbound_session.id,
            &[ToolName::Filesystem],
            ToolName::Filesystem,
            &serde_json::json!({
                "op": "write",
                "path": "unbound-default.txt",
                "content": "execHome default"
            }),
        )
        .await;
    assert!(
        unbound_write.ok,
        "未绑定项目文件工具应使用 execHome：{:?}",
        unbound_write.error
    );
    assert!(exec_home.join("unbound-default.txt").is_file());
    assert!(!gateway_workspace.join("unbound-default.txt").exists());

    exm_core::round_trace::begin(&unbound_session.id);
    let approval_args = serde_json::json!({ "command": "sudo echo approval-scope-probe" });
    let approval_future = gw.execute(
        "coding-agent",
        &unbound_session.id,
        &[ToolName::Terminal],
        ToolName::Terminal,
        &approval_args,
    );
    tokio::pin!(approval_future);
    let unbound_approval_id = tokio::select! {
        _result = &mut approval_future => panic!("approval unexpectedly returned before cancellation"),
        id = async {
            loop {
                if let Some(approval) = core
                    .store
                    .list_approvals(Some("pending"), 100)
                    .unwrap()
                    .into_iter()
                    .find(|approval| approval.session_id == unbound_session.id)
                {
                    break approval.id;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        } => id,
    };
    let tools_before_stop = exm_core::round_trace::tool_count(&unbound_session.id);
    assert_eq!(
        core.stop_session(&unbound_session.id).unwrap(),
        tools_before_stop
    );
    let approval_blocked = approval_future.await;
    assert!(!approval_blocked.ok, "停止后待审批终端动作必须受阻");
    assert!(
        approval_blocked
            .error
            .unwrap_or_default()
            .contains("待审批命令未执行"),
        "停止后的审批等待应返回明确取消而非超时"
    );
    let cancelled = core
        .store
        .get_approval(&unbound_approval_id)
        .unwrap()
        .unwrap();
    assert_eq!(cancelled.status, "cancelled");
    let late_approval = core.approval_decide(&unbound_approval_id, true).await;
    assert!(late_approval.is_err(), "停止后迟到批准不得执行");
    exm_core::round_trace::end(&unbound_session.id);
    let approval_workspace =
        serde_json::from_str::<serde_json::Value>(cancelled.meta_json.as_deref().unwrap_or("{}"))
            .unwrap();
    assert_eq!(
        approval_workspace["projectWorkspace"],
        exec_home.canonicalize().unwrap().to_string_lossy().as_ref(),
        "审批等待/回放必须固定未绑定会话的 execHome"
    );
    let legacy_unbound_approval = exm_core::types::ApprovalRequest {
        id: format!("apr-{}", &exm_core::types::new_id()[..8]),
        session_id: unbound_session.id.clone(),
        node_id: None,
        agent_id: "coding-agent".into(),
        command: "pwd".into(),
        status: "pending".into(),
        result: None,
        created_at: exm_core::types::now_iso(),
        decided_at: None,
        origin_user: None,
        origin_role: None,
        meta_json: None,
    };
    core.store.add_approval(&legacy_unbound_approval).unwrap();
    let legacy_replayed = core
        .approval_decide(&legacy_unbound_approval.id, true)
        .await
        .unwrap();
    assert_eq!(legacy_replayed.status, "executed");
    assert_eq!(
        legacy_replayed.result.as_deref().unwrap_or_default().trim(),
        exec_home.canonicalize().unwrap().to_string_lossy(),
        "旧审批缺少项目元数据时也应使用 execHome，而不是网关全局目录"
    );

    let late_session = core
        .store
        .create_session_in("late-tool", "exmachina", Some(project_path.clone()))
        .unwrap();
    exm_core::round_trace::begin(&late_session.id);
    assert!(exm_core::round_trace::request_cancel(&late_session.id));
    let late_path = project.join("late-tool-must-not-write.txt");
    let late_result = gw
        .execute(
            "coding-agent",
            &late_session.id,
            &[ToolName::Filesystem],
            ToolName::Filesystem,
            &serde_json::json!({
                "op": "write",
                "path": "late-tool-must-not-write.txt",
                "content": "late tool"
            }),
        )
        .await;
    assert!(!late_result.ok, "取消后的迟到工具必须被拦截");
    assert!(
        late_result.error.unwrap_or_default().contains("迟到工具"),
        "迟到工具错误应明确说明取消屏障"
    );
    assert!(!late_path.exists(), "取消后的迟到写操作不得落盘");
    exm_core::round_trace::end(&late_session.id);

    let outside_session = core
        .store
        .create_session_in(
            "outside",
            "exmachina",
            Some(
                outside
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            ),
        )
        .unwrap();
    let rejected = gw
        .execute(
            "coding-agent",
            &outside_session.id,
            &[ToolName::Terminal],
            ToolName::Terminal,
            &serde_json::json!({ "command": "pwd" }),
        )
        .await;
    assert!(!rejected.ok, "execHome 外项目必须拒绝");
    assert!(
        rejected.error.unwrap_or_default().contains("边界"),
        "越界原因应可诊断"
    );
    let _ = std::fs::remove_dir_all(&outside);
}
