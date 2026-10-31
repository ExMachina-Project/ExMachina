//! 端到端演练：真实 HTTP 协议（OpenAI 兼容 mock 服务）驱动完整开发任务。
//!
//! 与 runtime 单测的区别：这里从 Core::chat 网关入口进入，走完整编排
//! （规划 → 派发 → 子个体工具循环 → 回流 → 收束），且 LLM 是真实 HTTP
//! 流式端点——最贴近生产路径。Mock 通道（MockLlmProvider）不走 HTTP、
//! 不走原生 function calling，覆盖不到这些层。
//!
//! 运行：cargo test -p exm-core --test e2e_dev_flow -- --nocapture
use axum::{routing::post, Json};
use exm_core::config::ExmConfig;
use exm_core::Core;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// 脚本化 OpenAI 兼容端点：
/// - 规划请求（system 含 OrchestratorPlan）→ 返回单节点计划（派给 coding-agent）
/// - 子个体执行（messages 含 DispatchOrder）→ 第一轮发 edit 工具调用，收到 tool 结果后回流 SyncReport
/// - 收束请求（system 含 收束/Converge 标记）→ 输出最终答复
fn plan_body() -> Value {
    json!({
        "id": "chatcmpl-p1",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "```json\n{\"routeLevel\":\"L1\",\"goal\":\"在 demo.rs 加一行\",\"boundary\":{\"inScope\":[\"src\"],\"forbidden\":[]},\"acceptance\":[\"文件被修改\"],\"nodes\":[{\"id\":\"T1\",\"title\":\"实现\",\"agentIdentifier\":\"coding-agent\",\"objective\":\"在 demo.rs 里加一行 println\",\"acceptance\":[\"文件被修改\"],\"dependsOn\":[],\"priority\":\"P1\"}],\"finalAnswer\":null}\n```"
            },
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 20}
    })
}

fn sync_report_body() -> Value {
    json!({
        "id": "chatcmpl-s1",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "```json\n{\"sourceAgent\":\"coding-agent\",\"taskNodeId\":\"T1\",\"status\":\"done\",\"statements\":[{\"tag\":\"报告\",\"text\":\"已修改\"}],\"summary\":\"edit 完成\",\"evidence\":[{\"level\":\"B\",\"kind\":\"code\",\"ref\":\"src/demo.rs\"}],\"risks\":[],\"confidence\":0.9}\n```"
            },
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 20}
    })
}

fn tool_call_body() -> Value {
    json!({
        "id": "chatcmpl-t1",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_edit_1",
                    "type": "function",
                    "function": {
                        "name": "edit",
                        "arguments": "{\"path\":\"src/demo.rs\",\"old_string\":\"fn work() {}\",\"new_string\":\"fn work() { println!(\\\"e2e-ok\\\"); }\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 20}
    })
}

fn converge_body() -> Value {
    json!({
        "id": "chatcmpl-c1",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "【报告】T1 已完成：demo.rs 已加输出行。"
            },
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 20}
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn 端到端_开发任务_从对话到文件落盘() {
    // 1) 隔离工作区 + 编成副本
    let root = std::env::current_dir().unwrap().join("..").join("..");
    let tmp = std::env::temp_dir().join(format!("exm-e2e-dev-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(tmp.join("src/demo.rs"), "fn work() {}\n").unwrap();
    let entities = tmp.join("entities");
    copy_tree(&root.join("entities"), &entities);
    std::fs::write(entities.join("active_group"), "exmachina").unwrap();

    let mut cfg = ExmConfig::load(&tmp);
    cfg.workspace_root = tmp.clone();
    cfg.agents_dir = entities.clone();
    cfg.data_dir = tmp.join("data");
    cfg.config_path = tmp.join("config.json");
    cfg.memory_md_path = tmp.join("memory.md");
    cfg.use_mock = false;
    cfg.security.exec_approval = "off".into();

    // 2) 脚本化端点：按请求特征路由响应
    let plan_calls = Arc::new(AtomicUsize::new(0));
    let unit_tool_calls = Arc::new(AtomicUsize::new(0));
    let converge_calls = Arc::new(AtomicUsize::new(0));
    let tool_args_seen = Arc::new(Mutex::new(Vec::<String>::new()));

    let pc = plan_calls.clone();
    let uc = unit_tool_calls.clone();
    let cc = converge_calls.clone();
    let tas = tool_args_seen.clone();

    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let pc = pc.clone();
            let uc = uc.clone();
            let cc = cc.clone();
            let tas = tas.clone();
            async move {
                let system = body["messages"]
                    .as_array()
                    .map(|ms| {
                        ms.iter()
                            .filter(|m| m["role"] == "system")
                            .filter_map(|m| m["content"].as_str())
                            .collect::<String>()
                    })
                    .unwrap_or_default();
                let msgs_str = serde_json::to_string(&body["messages"]).unwrap_or_default();
                // 规划请求：system 含可调度子个体清单（OrchestratorPlan 契约）
                if system.contains("可调度子个体") {
                    pc.fetch_add(1, Ordering::SeqCst);
                    return (axum::http::StatusCode::OK, Json(plan_body()));
                }
                // 收束请求
                if system.contains("收束") {
                    cc.fetch_add(1, Ordering::SeqCst);
                    return (axum::http::StatusCode::OK, Json(converge_body()));
                }
                // 子个体执行：第一轮发工具调用，收到 tool 结果后回流报告
                if msgs_str.contains("DispatchOrder") || msgs_str.contains("调度指令") {
                    let has_tool_result = body["messages"]
                        .as_array()
                        .map(|ms| ms.iter().any(|m| m["role"] == "tool"))
                        .unwrap_or(false);
                    if has_tool_result {
                        return (axum::http::StatusCode::OK, Json(sync_report_body()));
                    }
                    // 记录下发的工具 schema，供断言「edit 在工具面里」
                    if let Some(tools) = body["tools"].as_array() {
                        let names: Vec<String> = tools
                            .iter()
                            .filter_map(|t| t["function"]["name"].as_str().map(String::from))
                            .collect();
                        *tas.lock().unwrap() = names;
                    }
                    uc.fetch_add(1, Ordering::SeqCst);
                    return (axum::http::StatusCode::OK, Json(tool_call_body()));
                }
                // 兜底：直答
                (axum::http::StatusCode::OK, Json(converge_body()))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await });

    // 3) 配置 LLM 指向 mock 端点
    cfg.llm.base_url = format!("http://127.0.0.1:{port}/v1");
    cfg.llm.api_key = "sk-test".into();
    cfg.llm.model = "scripted".into();
    cfg.llm_profiles.clear();

    let core = Arc::new(Core::with_config(cfg).expect("Core 创建失败"));

    // 4) 发起对话（完整编排链路）
    let session = core.create_session("e2e-dev").unwrap();
    core.chat(&session.id, "请在 src/demo.rs 的 work 函数里加一行输出")
        .await
        .expect("对话应完整走通");

    // 5) 断言：文件真的被 edit 改了
    let content = std::fs::read_to_string(tmp.join("src/demo.rs")).unwrap();
    assert!(
        content.contains("e2e-ok"),
        "edit 应真实落盘（原生 function calling 经 HTTP 全链路），实际: {content}"
    );

    // 6) 断言：编排层走了 规划 → 执行(带工具) → 收束
    assert!(plan_calls.load(Ordering::SeqCst) >= 1, "应有规划请求");
    assert!(
        unit_tool_calls.load(Ordering::SeqCst) >= 1,
        "应有子个体工具调用请求"
    );
    assert!(converge_calls.load(Ordering::SeqCst) >= 1, "应有收束请求");

    // 7) 断言：下发的工具面包含 edit（修复的编成白名单在真实链路生效）
    let tools = tool_args_seen.lock().unwrap().clone();
    assert!(
        tools.iter().any(|t| t == "edit"),
        "子个体工具面应含 edit（编成修复应在真实链路生效），实际: {tools:?}"
    );

    // 8) 事件层：工具调用与结果留痕（过程透明）。
    //    设计口径：工具事件归集到子会话（trace_key = childSessionId），
    //    主会话经 dispatch.sent.childSessionId 关联——历史回放从子会话事件库读
    let events = core.store.list_events(&session.id).unwrap_or_default();
    let events_str = serde_json::to_string(&events).unwrap_or_default();
    let child_id = events
        .iter()
        .filter_map(|e| {
            e.get("payload")
                .and_then(|p| p.get("payload"))
                .and_then(|p| p.get("childSessionId"))
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .find_map(Some)
        .or_else(|| {
            // Envelope 结构：payload.payload.childSessionId（dispatch.sent 事件）
            serde_json::from_str::<serde_json::Value>(&events_str)
                .ok()
                .and_then(|v| {
                    v.as_array().and_then(|arr| {
                        arr.iter().find_map(|e| {
                            e.pointer("/payload/payload/childSessionId")
                                .and_then(|x| x.as_str())
                                .map(String::from)
                        })
                    })
                })
        });
    let trace_session = child_id.unwrap_or_else(|| session.id.clone());
    let child_events = core.store.list_events(&trace_session).unwrap_or_default();
    let child_str = serde_json::to_string(&child_events).unwrap_or_default();
    assert!(
        child_str.contains("tool.call"),
        "子会话({trace_session})事件库应有 tool.call 留痕；主会话事件: {events_str}"
    );
    assert!(
        child_str.contains("tool.result"),
        "子会话({trace_session})事件库应有 tool.result 留痕"
    );

    println!("✅ 端到端开发任务全链路通过：chat → plan → dispatch → edit(HTTP原生调用) → SyncReport → converge");
}

fn copy_tree(src: &std::path::Path, dst: &std::path::Path) {
    fn rec(src: &std::path::Path, dst: &std::path::Path) {
        if !src.exists() {
            return;
        }
        if src.is_dir() {
            std::fs::create_dir_all(dst).unwrap();
            for e in std::fs::read_dir(src).unwrap() {
                let e = e.unwrap();
                rec(&e.path(), &dst.join(e.file_name()));
            }
        } else if let Some(name) = src.file_name().and_then(|s| s.to_str()) {
            if name == "active_group" || name == "active_single" {
                return; // 运行时残留：由测试自建
            }
            std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
            std::fs::copy(src, dst).unwrap();
        }
    }
    rec(src, dst);
}
