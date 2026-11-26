//! 任务队列端到端（task-queue）：入队 → 当前任务收束 → 出队自动执行。
//! mock LLM 脚本化响应（e2e_dev_flow 同款路由），验证：
//! ① queued 任务在收束钩子中被出队为 user 消息（queue:auto 标记）
//! ② 出队任务完整跑一轮（mock 收束响应 → orchestrator 回复落库）
//! ③ 队列状态机：queued → running → done
//! ④ 防递归：出队轮次不再触发钩子（不会连环出队）

use axum::Json;
use exm_core::config::ExmConfig;
use exm_core::Core;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn converge_body(text: &str) -> Value {
    json!({
        "id": "chatcmpl-conv", "object": "chat.completion", "model": "scripted",
        "choices": [{ "index": 0, "finish_reason": "stop", "message": {
            "role": "assistant",
            "content": format!("【报告】{text}")
        }}],
        "usage": { "prompt_tokens": 10, "completion_tokens": 10, "total_tokens": 20 }
    })
}

/// L0 直答计划（规划器要求 OrchestratorPlan JSON；L0 = 本机直接回答，不派发）
fn plan_l0_body() -> Value {
    json!({
        "id": "chatcmpl-plan", "object": "chat.completion", "model": "scripted",
        "choices": [{ "index": 0, "finish_reason": "stop", "message": {
            "role": "assistant",
            "content": "```json\n{\"routeLevel\":\"L0\",\"goal\":\"执行输入\",\"boundary\":{\"inScope\":[\"执行\"],\"forbidden\":[]},\"acceptance\":[\"完成\"],\"nodes\":[],\"finalAnswer\":[{\"tag\":\"报告\",\"text\":\"L0 直答\"}]}\n```"
        }}],
        "usage": { "prompt_tokens": 10, "completion_tokens": 10, "total_tokens": 20 }
    })
}

#[tokio::test]
async fn 任务队列_收束钩子出队执行() {
    // 1) 隔离工作区 + 编成副本
    let root = std::env::current_dir().unwrap().join("..").join("..");
    let tmp = std::env::temp_dir().join(format!("exm-tq-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();
    let entities = tmp.join("entities");
    let rec = |src: &std::path::Path, dst: &std::path::Path| {
        fn rec(src: &std::path::Path, dst: &std::path::Path) {
            std::fs::create_dir_all(dst).unwrap();
            for e in std::fs::read_dir(src).unwrap() {
                let e = e.unwrap();
                let d = dst.join(e.file_name());
                if e.file_type().unwrap().is_dir() {
                    rec(&e.path(), &d);
                } else {
                    std::fs::copy(e.path(), d).unwrap();
                }
            }
        }
        rec(src, dst);
    };
    rec(&root.join("entities"), &entities);
    std::fs::write(entities.join("active_group"), "exmachina").unwrap();

    let mut cfg = ExmConfig::load(&tmp);
    cfg.workspace_root = tmp.clone();
    cfg.agents_dir = entities.clone();
    cfg.data_dir = tmp.join("data");
    cfg.config_path = tmp.join("config.json");
    cfg.memory_md_path = tmp.join("memory.md");
    cfg.use_mock = false;
    cfg.security.exec_approval = "off".into();

    // 2) mock LLM：主任务与出队任务各自回一句可区分的报告
    let converge_calls = Arc::new(AtomicUsize::new(0));
    let seen_inputs = Arc::new(Mutex::new(Vec::<String>::new()));
    let cc = converge_calls.clone();
    let si = seen_inputs.clone();
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move |Json(body): Json<Value>| {
            let cc = cc.clone();
            let si = si.clone();
            async move {
                // 记录末条 user 内容（出队任务带 queue:auto 前缀落库，会进历史）
                if let Some(ms) = body["messages"].as_array() {
                    if let Some(last_user) = ms.iter().rev().find(|m| m["role"] == "user") {
                        si.lock()
                            .push(last_user["content"].as_str().unwrap_or("").to_string());
                    }
                }
                // 规划请求（system 含可调度子个体清单）→ L0 直答计划（不派发）
                let system = body["messages"]
                    .as_array()
                    .map(|ms| {
                        ms.iter()
                            .filter(|m| m["role"] == "system")
                            .filter_map(|m| m["content"].as_str())
                            .collect::<String>()
                    })
                    .unwrap_or_default();
                if system.contains("可调度子个体") {
                    return (axum::http::StatusCode::OK, Json(plan_l0_body()));
                }
                cc.fetch_add(1, Ordering::SeqCst);
                let n = cc.load(Ordering::SeqCst);
                (
                    axum::http::StatusCode::OK,
                    Json(converge_body(&format!("第{n}轮完成"))),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await });

    cfg.llm.base_url = format!("http://127.0.0.1:{port}/v1");
    cfg.llm.api_key = "sk-test".into();
    cfg.llm.model = "scripted".into();
    cfg.llm_profiles.clear();

    let core = Arc::new(Core::with_config(cfg).expect("Core 创建失败"));
    let session = core.create_session("tq-e2e").unwrap();
    let sid = session.id.as_str();

    // 3) 入队两条 queued 任务（模拟智能体在当前任务执行中收到新任务）
    exm_core::task_queue::enqueue(sid, "排队任务甲：整理归档", "queued", "").unwrap();
    exm_core::task_queue::enqueue(sid, "排队任务乙：生成周报", "queued", "").unwrap();

    // 4) 跑一轮真实输入 → 收束钩子应出队「任务甲」执行
    core.chat(sid, "请执行当前主任务")
        .await
        .expect("主任务应走通");

    // 断言：任务甲 done、任务乙仍 queued（一次收束只出队一条）
    let items = exm_core::task_queue::list(sid);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].status, "done", "任务甲应已执行：{:?}", items);
    assert_eq!(items[0].task, "排队任务甲：整理归档");
    assert_eq!(items[1].status, "queued", "任务乙应留在队列（防连环独占）");

    // 断言：出队任务落库为 queue:auto user 消息 + orchestrator 回复
    let msgs = core.store.list_messages(sid, 50).unwrap();
    let auto_msgs: Vec<_> = msgs
        .iter()
        .filter(|m| m.agent_id.as_deref() == Some("queue:auto"))
        .collect();
    assert_eq!(
        auto_msgs.len(),
        1,
        "应有且仅有一条出队消息：{}",
        auto_msgs.len()
    );
    assert!(auto_msgs[0].statements[0].text.contains("排队任务甲"));

    // 断言：下一轮输入的收束钩子继续出队任务乙
    core.chat(sid, "再看看还有什么要做的")
        .await
        .expect("第二轮应走通");
    let items = exm_core::task_queue::list(sid);
    assert!(
        items.iter().all(|t| t.status == "done"),
        "两轮后队列应全部完成：{:?}",
        items
            .iter()
            .map(|t| (t.id.clone(), t.status.clone()))
            .collect::<Vec<_>>()
    );
    let msgs = core.store.list_messages(sid, 100).unwrap();
    let auto_n = msgs
        .iter()
        .filter(|m| m.agent_id.as_deref() == Some("queue:auto"))
        .count();
    assert_eq!(auto_n, 2, "两条排队任务都应各自出队执行过");

    // 断言：防递归——出队轮次的收束不再触发钩子（否则入队 2 条会在第一轮连环跑完）
    // （上面 items 全 done 且 auto_n == 2 已隐含验证：每轮出队恰好一条）

    exm_core::task_queue::clear(sid);
}

/// capability-parity 3.6（session-steering spec「运行异常结束」）：
/// 轮次失败收束时，剩余 queued 任务迁移 pending_recovery（不自动连环执行），
/// 落提示消息 + 快照事件；不隐式创建另一会话继续执行、不虚报完成。
#[tokio::test]
async fn 任务队列_轮次失败后剩余项转pending_recovery不自动执行() {
    let root = std::env::current_dir().unwrap().join("..").join("..");
    let tmp = std::env::temp_dir().join(format!("exm-tq-fail-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();
    let entities = tmp.join("entities");
    fn rec(src: &std::path::Path, dst: &std::path::Path) {
        std::fs::create_dir_all(dst).unwrap();
        for e in std::fs::read_dir(src).unwrap() {
            let e = e.unwrap();
            let d = dst.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                rec(&e.path(), &d);
            } else {
                std::fs::copy(e.path(), d).unwrap();
            }
        }
    }
    rec(&root.join("entities"), &entities);
    std::fs::write(entities.join("active_group"), "exmachina").unwrap();

    let mut cfg = ExmConfig::load(&tmp);
    cfg.workspace_root = tmp.clone();
    cfg.agents_dir = entities.clone();
    cfg.data_dir = tmp.join("data");
    cfg.config_path = tmp.join("config.json");
    cfg.memory_md_path = tmp.join("memory.md");
    cfg.use_mock = false;
    cfg.security.exec_approval = "off".into();

    // mock LLM：所有请求（含规划）一律 500 → 轮次在规划阶段即失败
    //（L0 直答会复用 plan.finalAnswer，仅打挂收束请求不足以制造失败）
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(|| async {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": {"message": "mock provider 故障注入"}})),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await });

    cfg.llm.base_url = format!("http://127.0.0.1:{port}/v1");
    cfg.llm.api_key = "sk-test".into();
    cfg.llm.model = "scripted".into();
    cfg.llm_profiles.clear();

    let core = Arc::new(Core::with_config(cfg).expect("Core 创建失败"));
    let session = core.create_session("tq-fail").unwrap();
    let sid = session.id.as_str();

    exm_core::task_queue::enqueue(sid, "失败后排队的任务甲", "queued", "").unwrap();
    exm_core::task_queue::enqueue(sid, "失败后排队的任务乙", "queued", "").unwrap();

    // 轮次失败（收束请求 500）
    let result = core.chat(sid, "触发失败的主任务").await;
    assert!(result.is_err(), "主任务应失败：{result:?}");

    // 剩余 queued 全部迁移 pending_recovery，不自动执行
    let items = exm_core::task_queue::list(sid);
    assert_eq!(items.len(), 2);
    for t in &items {
        assert_eq!(
            t.status, "pending_recovery",
            "失败后剩余项应转 pending_recovery：{:?}",
            items
        );
        assert!(
            t.note.as_deref().unwrap_or("").contains("待用户确认"),
            "note 应提示待恢复：{:?}",
            t.note
        );
    }

    // 落库提示消息（用户可见的异常收束通知，非虚报完成）
    let msgs = core.store.list_messages(sid, 50).unwrap();
    let warn_msg = msgs
        .iter()
        .find(|m| {
            m.statements
                .iter()
                .any(|s| s.text.contains("pending_recovery"))
        })
        .expect("应有 pending_recovery 提示消息落库");
    assert_eq!(warn_msg.role, exm_core::types::MessageRole::System);

    // 快照事件落库可回放（前端队列条迁 🛟 态）
    let evts = core.store.list_events(sid).unwrap();
    let has_recovery_evt = evts.iter().any(|env| {
        env.get("payload")
            .and_then(|p| p.get("type"))
            .and_then(|t| t.as_str())
            == Some("task_queue.updated")
            && env
                .get("payload")
                .and_then(|p| p.get("payload"))
                .and_then(|d| d.get("action"))
                .and_then(|a| a.as_str())
                == Some("pending_recovery")
    });
    assert!(
        has_recovery_evt,
        "应有 task_queue.updated(pending_recovery) 事件落库"
    );

    // 不隐式继续：失败后队列任务绝不出队执行（没有 queue:auto 消息）
    assert!(
        !msgs
            .iter()
            .any(|m| m.agent_id.as_deref() == Some("queue:auto")),
        "失败后不得自动出队执行"
    );
}

/// capability-parity 3.6（session-steering spec「显式停止」）：用户 /stop 后
/// 队列条目按策略保留 queued（不自动执行、不误标 pending_recovery）——
/// 停止语义覆盖队列自动出队，剩余意图由用户决定。
#[tokio::test]
async fn 任务队列_用户停止后队列保留不自动执行不误标恢复() {
    let root = std::env::current_dir().unwrap().join("..").join("..");
    let tmp = std::env::temp_dir().join(format!("exm-tq-stop-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();
    let entities = tmp.join("entities");
    fn rec(src: &std::path::Path, dst: &std::path::Path) {
        std::fs::create_dir_all(dst).unwrap();
        for e in std::fs::read_dir(src).unwrap() {
            let e = e.unwrap();
            let d = dst.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                rec(&e.path(), &d);
            } else {
                std::fs::copy(e.path(), d).unwrap();
            }
        }
    }
    rec(&root.join("entities"), &entities);
    std::fs::write(entities.join("active_group"), "exmachina").unwrap();

    let mut cfg = ExmConfig::load(&tmp);
    cfg.workspace_root = tmp.clone();
    cfg.agents_dir = entities.clone();
    cfg.data_dir = tmp.join("data");
    cfg.config_path = tmp.join("config.json");
    cfg.memory_md_path = tmp.join("memory.md");
    cfg.use_mock = false;
    cfg.security.exec_approval = "off".into();

    // mock LLM：规划请求挂起 8s——期间用户发出 /stop（取消令牌置位）；
    // 挂起结束后返回有效 L0 计划，轮次在 L0 分支的取消检查点收束为
    // finish_stopped_round（真实取消语义路径，而非 HTTP 错误冒充失败）
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(|| async {
            tokio::time::sleep(std::time::Duration::from_secs(8)).await;
            (axum::http::StatusCode::OK, Json(plan_l0_body()))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await });

    cfg.llm.base_url = format!("http://127.0.0.1:{port}/v1");
    cfg.llm.api_key = "sk-test".into();
    cfg.llm.model = "scripted".into();
    cfg.llm_profiles.clear();

    let core = Arc::new(Core::with_config(cfg).expect("Core 创建失败"));
    let session = core.create_session("tq-stop").unwrap();
    let sid = session.id.as_str();

    exm_core::task_queue::enqueue(sid, "停止后应保留的任务", "queued", "").unwrap();

    // 轮次挂起运行中 → 用户停止（finish_stopped_round → Ok，非 Err）
    let chat_core = core.clone();
    let sid_owned = sid.to_string();
    let handle = tokio::spawn(async move { chat_core.chat(&sid_owned, "挂起的主任务").await });

    // 等轮次真正开始（round trace 有运行标记）
    for _ in 0..100 {
        if exm_core::round_trace::is_running(sid) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(exm_core::round_trace::is_running(sid), "轮次应已运行");
    let stopped = core.stop_session(sid).expect("停止应成功");
    assert_eq!(stopped, 0, "尚未执行工具");

    let handle_result: anyhow::Result<()> = handle.await.expect("task join");
    assert!(
        handle_result.is_ok(),
        "chat 应完成（停止=Ok 收束）: {handle_result:?}"
    );

    // 停止收束后队列保留 queued：不自动出队、不误标 pending_recovery
    let items = exm_core::task_queue::list(sid);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].status, "queued", "停止后队列保留原状：{items:?}");

    // 无自动出队执行痕迹
    let msgs = core.store.list_messages(sid, 50).unwrap();
    assert!(
        !msgs
            .iter()
            .any(|m| m.agent_id.as_deref() == Some("queue:auto")),
        "停止后不得自动出队执行"
    );
    // 停止陈述收束落库（真实停止回执，非静默）
    assert!(
        msgs.iter()
            .any(|m| m.statements.iter().any(|s| s.text.contains("已被用户停止"))),
        "应有停止收束陈述落库"
    );
}
