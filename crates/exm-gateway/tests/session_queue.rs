//! 后续任务队列 REST 面（capability-parity 3.3）：
//! 显式入队有序、CRUD 不重复执行旧文本、空闲会话保留 pending、跨会话拒绝。
use axum::body::Body;
use axum::http::{Request, StatusCode};
use exm_core::config::ExmConfig;
use std::sync::Arc;
use tower::ServiceExt;

struct Fixture {
    core: Arc<exm_core::Core>,
    app: axum::Router,
    dir: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("exm-queue-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        cfg.security.auth_key = "queue-fixture".into();
        let core = Arc::new(exm_core::Core::with_config(cfg).unwrap());
        Self {
            app: exm_gateway::build_router(core.clone()),
            core,
            dir,
        }
    }

    async fn request(
        &self,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("x-auth-key", "queue-fixture");
        let request = match body {
            Some(payload) => {
                builder = builder.header("content-type", "application/json");
                builder.body(Body::from(payload.to_string())).unwrap()
            }
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
        };
        (status, json)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test]
async fn 三条后续指令按接收顺序入队且序号单调() {
    let f = Fixture::new();
    let session = f.core.create_session("queue-order").unwrap();
    let base = format!("/api/sessions/{}/queue", session.id);
    for task in ["第一条", "第二条", "第三条"] {
        let (status, body) = f
            .request(
                "POST",
                &base,
                Some(serde_json::json!({ "task": task, "source": "chat" })),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "入队 202: {body}");
    }
    let (status, body) = f.request("GET", &base, None).await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    let texts: Vec<&str> = items.iter().map(|t| t["task"].as_str().unwrap()).collect();
    assert_eq!(texts, vec!["第一条", "第二条", "第三条"], "接收顺序");
    let seqs: Vec<u64> = items.iter().map(|t| t["seq"].as_u64().unwrap()).collect();
    assert_eq!(seqs, vec![1, 2, 3], "单调序号");
    // 回执不冒充完成：pending=true / status=queued
    assert_eq!(items[0]["status"], "queued");
    assert_eq!(items[0]["source"], "chat");
}

#[tokio::test]
async fn 编辑移除移动不重复执行旧文本() {
    let f = Fixture::new();
    let session = f.core.create_session("queue-crud").unwrap();
    let base = format!("/api/sessions/{}/queue", session.id);
    let (_, added) = f
        .request("POST", &base, Some(serde_json::json!({ "task": "旧文本" })))
        .await;
    let qid = added["queueId"].as_str().unwrap().to_string();
    // 当前 revision 从列表取（回执不重复携带）
    let rev = f.request("GET", &base, None).await.1["items"][0]["revision"].as_u64();

    // 编辑：乐观锁错误 revision 拒绝
    let (status, body) = f
        .request(
            "PATCH",
            &format!("{base}/{qid}"),
            Some(serde_json::json!({ "task": "新文本", "revision": 999 })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "错误 revision 拒绝: {body}"
    );
    // 正确编辑
    let (status, body) = f
        .request(
            "PATCH",
            &format!("{base}/{qid}"),
            Some(serde_json::json!({ "task": "新文本", "revision": rev })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 列表只剩新文本；审计记录在（旧文本废弃不重放）
    let (_, list) = f.request("GET", &base, None).await;
    assert_eq!(list["items"][0]["task"], "新文本");
    assert!(!list["items"][0]["audit"].as_array().unwrap().is_empty());

    // 移动（单条钳制无实义但成功）+ 移除
    let (status, _) = f
        .request(
            "POST",
            &format!("{base}/{qid}/move"),
            Some(serde_json::json!({ "position": 1 })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = f.request("DELETE", &format!("{base}/{qid}"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 移除后编辑被拒绝（不复活旧文本）
    let (status, _) = f
        .request(
            "PATCH",
            &format!("{base}/{qid}"),
            Some(serde_json::json!({ "task": "复活" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, list) = f.request("GET", &base, None).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn 空闲会话入队保留pending不自动启动() {
    let f = Fixture::new();
    let session = f.core.create_session("queue-idle").unwrap();
    let base = format!("/api/sessions/{}/queue", session.id);
    let (status, body) = f
        .request(
            "POST",
            &base,
            Some(serde_json::json!({ "task": "空闲任务" })),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["status"], "queued", "回执为排队而非执行中");
    assert_eq!(body["pending"], true);
    // 未启动轮次：会话消息区没有 assistant/unit 回复冒充执行
    let messages = f.core.store.list_messages(&session.id, 50).unwrap();
    assert!(
        messages
            .iter()
            .all(|m| m.role != exm_core::types::MessageRole::Unit),
        "空闲入队不得产生执行痕迹"
    );
}

#[tokio::test]
async fn 跨会话队列id操作被拒绝() {
    let f = Fixture::new();
    let session_a = f.core.create_session("queue-a").unwrap();
    let session_b = f.core.create_session("queue-b").unwrap();
    let base_a = format!("/api/sessions/{}/queue", session_a.id);
    let (_, added) = f
        .request(
            "POST",
            &base_a,
            Some(serde_json::json!({ "task": "A 的任务" })),
        )
        .await;
    let qid = added["queueId"].as_str().unwrap();
    // B 会话持 A 的 qid：查询（list 是会话级，单条无独立 GET）、编辑、删除、移动
    let base_b = format!("/api/sessions/{}/queue", session_b.id);
    let (status, _) = f
        .request(
            "PATCH",
            &format!("{base_b}/{qid}"),
            Some(serde_json::json!({ "task": "篡改" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = f.request("DELETE", &format!("{base_b}/{qid}"), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = f
        .request(
            "POST",
            &format!("{base_b}/{qid}/move"),
            Some(serde_json::json!({ "position": 1 })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // B 的列表不含 A 的条目（不泄漏正文）
    let (_, list_b) = f.request("GET", &base_b, None).await;
    assert_eq!(list_b["items"].as_array().unwrap().len(), 0);
    // A 的条目原样
    let (_, list_a) = f.request("GET", &base_a, None).await;
    assert_eq!(list_a["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn 清空队列与不存在会话() {
    let f = Fixture::new();
    let session = f.core.create_session("queue-clear").unwrap();
    let base = format!("/api/sessions/{}/queue", session.id);
    f.request("POST", &base, Some(serde_json::json!({ "task": "待清" })))
        .await;
    let (status, body) = f.request("DELETE", &base, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, list) = f.request("GET", &base, None).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 0);
    // 不存在会话 404
    let (status, _) = f.request("GET", "/api/sessions/no-such/queue", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn 队列事件落库可回放且快照完整() {
    // capability-parity 3.5：WS 重连/刷新从事件恢复队列——事件必须落库
    //（不只是广播）且 payload 带全量快照（items/running/queued），前端回放
    // 与实时同一渲染路径；「不显示虚假完成」：入队事件里 status=queued。
    let f = Fixture::new();
    let session = f.core.create_session("queue-replay").unwrap();
    let base = format!("/api/sessions/{}/queue", session.id);
    for task in ["A 任务", "B 任务"] {
        let (status, _) = f
            .request("POST", &base, Some(serde_json::json!({ "task": task })))
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }
    // 事件库回放（与 WebUI 刷新同口径：取事件尾的 task_queue.updated）
    let evts = f.core.store.list_events(&session.id).unwrap();
    // 解包路径与 webui store 回放同构：envelope.payload = CoreEvent{type, payload}
    let snapshot = evts
        .iter()
        .rev()
        .find_map(|env| {
            let evt = env.get("payload")?.as_object()?;
            (evt.get("type").and_then(|v| v.as_str()) == Some("task_queue.updated"))
                .then(|| evt.get("payload").cloned())?
        })
        .expect("队列事件应落库（可回放）");
    let items = snapshot["items"].as_array().expect("快照带全量 items");
    assert_eq!(items.len(), 2, "快照 items 全量：{snapshot}");
    assert_eq!(snapshot["queued"].as_u64(), Some(2));
    assert_eq!(items[1]["status"], "queued", "入队事件不冒充完成");
    assert_eq!(items[1]["task"].as_str(), Some("B 任务"));
}
