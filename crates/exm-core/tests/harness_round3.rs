//! agent 能力第三轮回归（change: harness-agent-round3）：
//! 1. todo 工具：write/list、系统注入渲染、todo.updated 事件、编码个体工具面
//! 2. spawn 后台并行：background 立即返回、status 回收、list 会话隔离、超时终态、未回收提醒

use exm_core::tools::ToolGateway;
use exm_core::types::{AgentDefinition, ToolName};
use exm_core::{config::ExmConfig, Core};
use std::path::PathBuf;

fn mock_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("exm-h3-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn gateway(dir: &PathBuf) -> (ToolGateway, Core) {
    let cfg = ExmConfig::load(&dir);
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
    assert!(exm_core::todo::render("ses-todo").is_none(), "清空后不再注入");
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
        assert!(keys.contains(&"todo"), "{id} 工具面应含 todo（现有 {keys:?}）");
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
    assert!(r.output.contains("调研 A 方案"), "应含结果全文：{}", r.output);
    // 已读后不再提醒
    assert!(gw.spawn_pending_note("ses-sp").is_none(), "已读终态不应再提醒");
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
    assert!(r.output.contains(&spawn_id[..10]), "list 应含子任务：{}", r.output);
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
    assert!(r.output.contains("无后台子任务"), "跨会话应隔离：{}", r.output);
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
        })
            as futures_util::future::BoxFuture<'static, anyhow::Result<String>>
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
    // 其它会话不受影响
    assert!(gw.spawn_pending_note("ses-other2").is_none());
}
