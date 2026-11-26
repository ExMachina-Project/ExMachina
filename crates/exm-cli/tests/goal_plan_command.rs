//! exm-cli/tests/goal_plan_command —— capability-parity 4.6：goal/plan 的 CLI
//! 子进程级 golden tests（JSON 输出 + 退出码），与 spec_command.rs 同夹具。
//!
//! 锚定验收：
//! ① goal 生命周期 JSON 结构稳定（status/objective/revision/verification）；
//! ② 修订冲突（--revision 过期）→ 非零退出 + error.category（cli 全局错误壳）；
//! ③ plan status/approve/reject/execute JSON 稳定，plan start 只切模式位；
//! ④ 同一会话 goal/spec/plan 三命令共享状态：spec.goalId 自动关联、
//!    /plan 模式位独立于 goal 状态（workflow 关联视图一致）；
//! ⑤ goal/plan 命令不产生 cron 项、todo 快照、队列项（互不相干）；
//! ⑥ 命令不进模型链路：goal/plan 命令不产生新的 assistant 消息
//!    （chat 轮次才写消息——命令文本不交给模型猜测）。
use exm_core::{config::ExmConfig, Core};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

struct Workspace(PathBuf);

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(workspace: &Workspace, args: &[&str]) -> (bool, Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_exm"))
        .args(["--output-format", "json", "--workspace"])
        .arg(&workspace.0)
        .args(args)
        .output()
        .unwrap();
    let value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "invalid command output: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.success(), value)
}

fn setup() -> (Workspace, String) {
    let workspace =
        Workspace(std::env::temp_dir().join(format!("exm-cli-gp-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(&workspace.0).unwrap();
    let mut config = ExmConfig::load(&workspace.0);
    config.use_mock = true;
    config.save().unwrap();
    let core = Core::with_config(config).unwrap();
    let session = core.create_session("CLI goal/plan golden").unwrap().id;
    drop(core);
    (workspace, session)
}

#[test]
fn cli_goal_lifecycle_json_and_exit_codes() {
    let (workspace, session) = setup();

    // 空态如实（null），退出码 0
    let (ok, empty) = run(&workspace, &["goal", "status", &session]);
    assert!(ok);
    assert!(empty.is_null(), "空态 goal 应输出 null：{empty}");

    // start
    let (ok, started) = run(&workspace, &["goal", "start", &session, "交付解析器"]);
    assert!(ok, "start 应成功：{started}");
    assert_eq!(started["goal"]["status"], "active");
    assert_eq!(started["goal"]["objective"], "交付解析器");
    assert_eq!(started["goal"]["revision"], 1);

    // 修订冲突：--revision 过期 → 非零退出 + error.category
    let (ok, stale) = run(
        &workspace,
        &["goal", "edit", &session, "过期修订", "--revision", "99"],
    );
    assert!(!ok, "过期 revision 必须失败：{stale}");
    assert_eq!(stale["error"]["category"], "conflict");

    // pause / resume
    let (ok, paused) = run(&workspace, &["goal", "pause", &session]);
    assert!(ok);
    assert_eq!(paused["goal"]["status"], "paused");
    let (ok, resumed) = run(&workspace, &["goal", "resume", &session]);
    assert!(ok);
    assert_eq!(resumed["goal"]["status"], "active");

    // complete 必须带真实验证证据：空验证字符串被拒（core required_text）
    let (ok, no_evidence) = run(&workspace, &["goal", "complete", &session, "   "]);
    assert!(!ok, "空验证证据必须拒绝：{no_evidence}");
    // 带证据过
    let (ok, done) = run(
        &workspace,
        &["goal", "complete", &session, "cargo test 全绿（证据）"],
    );
    assert!(ok, "complete 带证据应成功：{done}");
    assert_eq!(done["goal"]["status"], "completed");
    assert_eq!(
        done["goal"]["verification"][0], "cargo test 全绿（证据）",
        "证据入档：{done}"
    );
}

#[test]
fn cli_plan_surfaces_json_and_mode_flags() {
    let (workspace, session) = setup();

    // status：默认 execute 模式、空清单
    let (ok, status) = run(&workspace, &["plan", "status", &session]);
    assert!(ok);
    assert_eq!(status["mode"], "execute");
    assert_eq!(status["items"].as_array().map(Vec::len), Some(0));

    // start（无任务文本）：只切模式位（进程级暂存——CLI 每次调用是独立进程，
    // 跨进程断言走同进程 core 面校验，见 shared_session_state 测试）
    let (ok, started) = run(&workspace, &["plan", "start", &session]);
    assert!(ok);
    assert_eq!(started["mode"], "plan", "json 输出契约：{started}");

    // 同进程验证模式位确实生效（模拟网关常驻进程内 plan start→status 的链路）
    {
        let mut config = ExmConfig::load(&workspace.0);
        config.use_mock = true;
        let core = Core::with_config(config).unwrap();
        core.set_plan_mode(&session, true);
        assert!(exm_core::plan::is_plan_mode(&session));
        assert_eq!(core.plan_list(&session).len(), 0);
        let (ok, rejected) = run(&workspace, &["plan", "reject", &session]);
        assert!(ok);
        assert_eq!(rejected["retained"], 0);
        // approve 空清单是显式失败（core 契约：无条目可批准）——非零退出 + 稳定 category
        let (ok, empty_approve) = run(&workspace, &["plan", "approve", &session]);
        assert!(!ok, "空清单 approve 必须失败：{empty_approve}");
        assert_eq!(empty_approve["error"]["category"], "internal");
        core.set_plan_mode(&session, false);
    }

    // execute：切回执行模式
    let (ok, back) = run(&workspace, &["plan", "execute", &session]);
    assert!(ok);
    assert_eq!(back["mode"], "execute");
    let (ok, status) = run(&workspace, &["plan", "status", &session]);
    assert!(ok);
    assert_eq!(status["mode"], "execute");
}

/// golden：同一会话 goal/spec/plan 三命令共享 workflow 状态；命令不与
/// cron/todo/队列混淆；goal/plan 命令不把命令文本交给模型（无新消息）。
#[test]
fn cli_goal_spec_plan_shared_session_state_no_leakage() {
    let (workspace, session) = setup();

    // 三命令族先后落状态
    assert!(run(&workspace, &["goal", "start", &session, "交付解析器"]).0);
    assert!(run(&workspace, &["spec", "draft", &session, "实现解析器"]).0);
    assert!(run(&workspace, &["plan", "start", &session]).0);

    // 会话消息面：goal/plan 命令不产生消息（不进模型链路）
    let mut config = ExmConfig::load(&workspace.0);
    config.use_mock = true;
    let core = Core::with_config(config).unwrap();
    let msgs = core.store.list_messages(&session, 100).unwrap();
    assert!(
        !msgs
            .iter()
            .flat_map(|m| m.statements.iter())
            .any(|st| st.text.contains("goal") || st.text.contains("解析器")),
        "命令文本不得进入会话消息流：{:?}",
        msgs.iter()
            .flat_map(|m| m.statements.iter().map(|st| st.text.clone()))
            .collect::<Vec<_>>()
    );

    // workflow 关联视图（4.5）：goal/spec/plan 三层数据同源共享
    // （goal/spec 是落库事件可跨进程恢复；plan 模式位是进程级暂存，
    //  常驻进程内三命令共享——这里在 core 同进程内补齐 plan 位验证）
    let relation = core.workflow_relation(&session).unwrap();
    let g = relation.goal.as_ref().expect("goal 应已建立");
    let s = relation.spec.as_ref().expect("spec 应已建立");
    assert_eq!(s.goal_id.as_deref(), Some(g.id.as_str()), "spec→goal 关联");
    core.set_plan_mode(&session, true);
    let relation = core.workflow_relation(&session).unwrap();
    assert!(relation.plan_mode, "plan 模式位独立于 goal/spec");
    core.set_plan_mode(&session, false);

    // 互不混淆：goal/plan 命令不产生 cron 项、todo 快照、队列项
    assert!(core.cron.list().unwrap().is_empty(), "cron 不受影响");
    assert!(
        exm_core::todo::list_items(&session).is_empty(),
        "todo 不受影响"
    );
    assert!(
        exm_core::task_queue::list(&session).is_empty(),
        "任务队列不受影响"
    );
}
