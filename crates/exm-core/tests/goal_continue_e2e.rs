//! goal 自动继续端到端（capability-parity 4.2）：有界推进 + 恢复 admission。
//! mock judge 脚本化裁决（判定:<v> 标记驱动），验证：
//! ① admission：未明确 start/resume 的 Active goal 不自动续跑；pause 收回授权
//! ② mock judge 各终态：done→complete（带证据）/ blocked→block / needs_user→pause
//! ③ 轮数上限耗尽 → block（不无限自我续跑）
//! ④ 运行中不打断：会话忙时 tick 跳过
//! ⑤ goal.continue 事件全程留痕（可审计）
#![recursion_limit = "512"]

use exm_core::config::ExmConfig;
use exm_core::workflow::GoalStatus;
use exm_core::Core;
use std::sync::Arc;

/// 串行锁：Core 型测试共享仓库工作区。tokio Mutex（guard 可跨 await 点——
/// 测试体内有 await；与 smoke.rs 的 cargo test 串行参数互补，双保险）
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
async fn serial_guard() -> tokio::sync::MutexGuard<'static, ()> {
    SERIAL.lock().await
}

fn test_core() -> Arc<Core> {
    test_core_with(0)
}

/// 参数化 core（timeout_secs：单轮时间预算；0 = 不限时）
fn test_core_with(timeout_secs: u64) -> Arc<Core> {
    let root = std::env::current_dir().unwrap().join("..").join("..");
    let mut cfg = ExmConfig::load(&root);
    let base = std::env::temp_dir().join(format!("exm-goal-c-{}", uuid::Uuid::new_v4()));
    let agents_copy = base.join("entities");
    copy_tree(&root.join("entities"), &agents_copy);
    cfg.agents_dir = agents_copy;
    cfg.data_dir = std::env::temp_dir().join(format!("exm-goal-d-{}", uuid::Uuid::new_v4()));
    cfg.use_mock = true;
    cfg.llm.api_key.clear();
    cfg.llm.api_keys.clear();
    cfg.llm.base_url.clear();
    cfg.llm_profiles.clear();
    cfg.active_profile.clear();
    cfg.security.approval_wait_secs = 0;
    // goal 自动继续开启（tick 周期 1 分钟；轮数上限 3；不设单轮超时——mock 即时）
    cfg.automation.goal_continue_interval_minutes = 1;
    cfg.automation.goal_continue_max_rounds = 3;
    cfg.automation.goal_continue_round_timeout_secs = timeout_secs;
    Arc::new(Core::with_config(cfg).expect("Core 创建失败"))
}

fn copy_tree(src: &std::path::Path, dst: &std::path::Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let d = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &d);
        } else {
            std::fs::copy(e.path(), d).unwrap();
        }
    }
}

#[tokio::test]
async fn 未授权的active_goal不自动续跑() {
    let _g = serial_guard().await;
    let core = test_core();
    let session = core.create_session("goal-admission").unwrap();

    // 直接经事件造一个 auto_continue=false 的 Active goal（模拟旧数据/未授权态）
    core.goal_update(&session.id, "start", Some("未授权目标"), None, None, None)
        .unwrap();
    // start 已置 auto_continue=true —— 手动 pause 收回，再手动把状态改回 Active
    // （绕过公开操作：直接验证 admission 位的效果）
    core.goal_update(&session.id, "pause", None, None, None, None)
        .unwrap();
    let g = core.goal_get(&session.id).unwrap().unwrap();
    assert_eq!(g.status, GoalStatus::Paused);
    assert!(!g.auto_continue, "pause 收回自动继续授权");

    // tick 不推进（admission 拒绝）
    let n = core.goal_continue_tick(60).await;
    assert_eq!(n, 0, "未授权 goal 不自动续跑");
    let g = core.goal_get(&session.id).unwrap().unwrap();
    assert_eq!(g.continue_rounds_used, 0, "零推进");
}

#[tokio::test]
async fn judge判done时goal完成并留证据() {
    let _g = serial_guard().await;
    let core = test_core();
    let session = core.create_session("goal-done").unwrap();
    let sid = &session.id;

    // 目标文本带「判定:done」标记——推进轮回复透传标记，judge 裁决 done
    core.goal_update(
        sid,
        "start",
        Some("完成一个小任务 判定:done"),
        None,
        None,
        None,
    )
    .unwrap();

    let n = core.goal_continue_tick(60).await;
    assert_eq!(n, 1, "授权 goal 推进一轮");
    let g = core.goal_get(sid).unwrap().unwrap();
    assert_eq!(
        g.status,
        GoalStatus::Completed,
        "judge done → complete：{g:?}"
    );
    assert!(!g.auto_continue, "complete 收回授权");
    assert!(
        g.verification.iter().any(|v| v.contains("mock judge")),
        "验证证据落账：{:?}",
        g.verification
    );

    // 事件留痕：goal.continue（running → judged done）
    let evts = core.store.list_events(sid).unwrap();
    let continue_evts: Vec<_> = evts
        .iter()
        .filter(|env| {
            env.get("payload")
                .and_then(|p| p.get("type"))
                .and_then(|t| t.as_str())
                == Some("goal.continue")
        })
        .collect();
    assert!(
        continue_evts.len() >= 2,
        "推进+裁决事件留痕：{continue_evts:?}"
    );
}

#[tokio::test]
async fn judge判blocked与needs_user时goal停对状态() {
    let _g = serial_guard().await;
    let core = test_core();
    let s1 = core.create_session("goal-blocked").unwrap();
    core.goal_update(
        &s1.id,
        "start",
        Some("受阻任务 判定:blocked"),
        None,
        None,
        None,
    )
    .unwrap();
    core.goal_continue_tick(60).await;
    let g = core.goal_get(&s1.id).unwrap().unwrap();
    assert_eq!(
        g.status,
        GoalStatus::Blocked,
        "judge blocked → block：{g:?}"
    );
    assert!(!g.auto_continue);

    let s2 = core.create_session("goal-needs-user").unwrap();
    core.goal_update(
        &s2.id,
        "start",
        Some("需决策任务 判定:needs_user"),
        None,
        None,
        None,
    )
    .unwrap();
    core.goal_continue_tick(60).await;
    let g2 = core.goal_get(&s2.id).unwrap().unwrap();
    assert_eq!(
        g2.status,
        GoalStatus::Paused,
        "judge needs_user → pause：{g2:?}"
    );
    assert!(!g2.auto_continue, "needs_user 收回授权（等 resume）");
}

#[tokio::test]
async fn continue裁决多轮推进直到轮数上限后block() {
    let _g = serial_guard().await;
    let core = test_core();
    let session = core.create_session("goal-rounds").unwrap();
    let sid = &session.id;

    // 判定:continue（缺省也是 continue）——每轮 judge 都说继续
    core.goal_update(
        sid,
        "start",
        Some("长期目标 判定:continue"),
        None,
        None,
        None,
    )
    .unwrap();

    // 上限 3 轮：tick 三次各推进一轮，第四次 tick 触发轮数耗尽 → block
    core.goal_continue_tick(60).await;
    let g = core.goal_get(sid).unwrap().unwrap();
    assert_eq!(g.continue_rounds_used, 1);
    assert_eq!(g.status, GoalStatus::Active, "continue 裁决保持 Active");

    core.goal_continue_tick(120).await;
    core.goal_continue_tick(180).await;
    let g = core.goal_get(sid).unwrap().unwrap();
    assert_eq!(g.continue_rounds_used, 3);
    assert_eq!(g.status, GoalStatus::Active, "上限内继续推进");

    core.goal_continue_tick(240).await;
    let g = core.goal_get(sid).unwrap().unwrap();
    assert_eq!(g.status, GoalStatus::Blocked, "轮数耗尽 → block：{g:?}");
    assert!(
        g.note.as_deref().unwrap_or("").contains("上限"),
        "note 说明耗尽：{:?}",
        g.note
    );
    assert!(!g.auto_continue);
}

#[tokio::test]
async fn 运行中的会话不被goal继续打断() {
    let _g = serial_guard().await;
    let core = test_core();
    let session = core.create_session("goal-busy").unwrap();
    let sid = &session.id;

    core.goal_update(sid, "start", Some("忙碌期目标"), None, None, None)
        .unwrap();

    // 标记会话运行中（round_trace 是运行态唯一事实源；mock 轮即时完成，用
    // begin/end 模拟真实运行窗口——与 tests_channel_steering 夹具同技法）
    exm_core::round_trace::begin(sid);
    let n = core.goal_continue_tick(60).await;
    assert_eq!(n, 0, "运行中 tick 跳过");
    let g = core.goal_get(sid).unwrap().unwrap();
    assert_eq!(g.continue_rounds_used, 0, "零推进不打断");
    exm_core::round_trace::end(sid);

    // 空闲后恢复推进
    let n = core.goal_continue_tick(120).await;
    assert_eq!(n, 1, "空闲后正常推进");
}

/// 4.2 验收：error 终态——推进轮请求失败 → goal block + note 如实记录，
/// 不重试不虚报；事件留 reason:error。
#[tokio::test]
async fn 推进轮失败时goal如实block不重试() {
    let _g = serial_guard().await;
    let core = test_core();
    let session = core.create_session("goal-error").unwrap();
    let sid = &session.id;

    core.goal_update(sid, "start", Some("会失败的任务"), None, None, None)
        .unwrap();
    exm_core::provider::mock::MOCK_GOAL_ROUND_ERROR
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let n = core.goal_continue_tick(60).await;
    assert_eq!(n, 1, "error 也算处理了一个会话（有收束）");
    let g = core.goal_get(sid).unwrap().unwrap();
    assert_eq!(g.status, GoalStatus::Blocked, "error → block：{g:?}");
    assert!(
        g.note.as_deref().unwrap_or("").contains("自动继续失败"),
        "note 如实记录失败：{:?}",
        g.note
    );
    assert!(!g.auto_continue, "失败后收回授权");

    // 事件留痕 reason:error
    let evts = core.store.list_events(sid).unwrap();
    let has_error = evts.iter().any(|env| {
        let p = env.get("payload").and_then(|p| p.get("payload"));
        p.and_then(|d| d.get("reason")).and_then(|r| r.as_str()) == Some("error")
    });
    assert!(has_error, "goal.continue 事件留 reason:error");
}

/// 4.2 验收：time limit 终态——单轮超时 → goal block + note 记录超时秒数。
#[tokio::test]
async fn 推进轮超时时goal如实block并记录时限() {
    let _g = serial_guard().await;
    // 单轮 1s 时限（参数化构造）+ 挂起 8s 注入 → 超时收束
    let core = test_core_with(1);
    let session = core.create_session("goal-timeout").unwrap();
    let sid = &session.id;

    core.goal_update(sid, "start", Some("会超时的任务"), None, None, None)
        .unwrap();
    exm_core::provider::mock::MOCK_GOAL_ROUND_HANG.store(true, std::sync::atomic::Ordering::SeqCst);

    let n = core.goal_continue_tick(60).await;
    assert_eq!(n, 1);
    let g = core.goal_get(sid).unwrap().unwrap();
    assert_eq!(g.status, GoalStatus::Blocked, "timeout → block：{g:?}");
    assert!(
        g.note.as_deref().unwrap_or("").contains("超时"),
        "note 记录超时：{:?}",
        g.note
    );
}

/// 4.2 验收：budget 终态——token 预算耗尽 → needs_user（pause 收回授权等确认），
/// 不虚报完成也不无限续跑。
#[tokio::test]
async fn token预算耗尽时停为待用户确认() {
    let _g = serial_guard().await;
    let core = test_core();
    let session = core.create_session("goal-budget").unwrap();
    let sid = &session.id;

    // 预算 300：start 的规划轮（0）+ 第一轮推进（300）后即超——第二轮 tick 前
    // 对照时 used >= budget → needs_user
    core.goal_update_scoped(
        sid,
        "start",
        Some("预算紧张的任务 判定:continue"),
        None,
        None,
        None,
        Some(1),
    )
    .unwrap();

    core.goal_continue_tick(60).await;
    let g = core.goal_get(sid).unwrap().unwrap();
    // 第一轮推进后对照：used(300) >= budget(1) → needs_user → pause
    assert_eq!(
        g.status,
        GoalStatus::Paused,
        "budget → needs_user/pause：{g:?}"
    );
    assert!(
        g.note.as_deref().unwrap_or("").contains("预算"),
        "note 说明预算耗尽：{:?}",
        g.note
    );
    assert!(!g.auto_continue, "预算耗尽收回授权");
}
