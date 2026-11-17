//! Reply recovery through the real OneBot reader and a local model endpoint.
use super::*;
use axum::{
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use exm_core::config::ExmConfig;
use serde_json::{json, Value};
use std::{path::PathBuf, time::Duration};

#[test]
fn reply_text_uses_message_body_and_keeps_sender_and_long_text() {
    let body = format!("{}需要保留的结尾", "引用原文。".repeat(100));
    let quote = parse_reply_quote(
        &json!({
            "sender": { "user_id": "10001", "card": "", "nickname": "原作者" },
            "message": [{ "type": "text", "data": { "text": body } }]
        }),
        Some(10001),
    );
    assert!(quote.hits_self);
    assert!(quote.text.contains(&format!("【回复 原作者：{body}】")));
    assert!(quote.media.is_empty());

    let cq = parse_reply_quote(
        &json!({
            "sender": { "user_id": 10002, "card": "群名片", "nickname": "昵称" },
            "message": "[CQ:at,qq=10001] 上一条原文"
        }),
        Some(10001),
    );
    assert_eq!(cq.text, "【回复 群名片：@10001 上一条原文】\n");
    assert!(!cq.hits_self);
    let raw = parse_reply_quote(&json!({"raw_message": "仅提供 raw_message 的原文"}), None);
    assert!(raw.text.contains("仅提供 raw_message 的原文"));
    assert!(
        !raw.hits_self,
        "Unknown sender and bot IDs must not count as a bot reply"
    );
}

#[test]
fn reply_media_keeps_caption_and_attachment_sources() {
    let quote = parse_reply_quote(
        &json!({
            "sender": { "user_id": 10001 },
            "message": [
                { "type": "text", "data": { "text": "这是需要解释的图片" } },
                { "type": "image", "data": { "url": "https://fixture.invalid/image.png" } },
                { "type": "file", "data": { "file": "file:///fixture/document.txt" } },
                { "type": "record", "data": { "url": "https://fixture.invalid/voice.ogg" } }
            ]
        }),
        Some(10001),
    );
    assert!(quote.text.contains("这是需要解释的图片"));
    assert!(quote.text.contains("图片 x1 + 语音 x1 + 文件 x1"));
    assert_eq!(
        quote.media,
        vec![
            ("image".into(), "https://fixture.invalid/image.png".into()),
            ("file".into(), "file:///fixture/document.txt".into()),
            ("voice".into(), "https://fixture.invalid/voice.ogg".into()),
        ]
    );
}

#[test]
fn reply_ids_accept_numbers_and_deduplicate_without_reordering() {
    assert_eq!(
        reply_para_ids(&json!([
            { "type": "reply", "data": { "id": -12345 } },
            { "type": "reply", "data": { "id": " -12345 " } },
            { "type": "reply", "data": { "id": "67890" } },
            { "type": "reply", "data": { "id": null } },
            { "type": "reply", "data": { "id": " " } }
        ])),
        vec!["-12345", "67890"]
    );
    assert_eq!(
        reply_para_ids(&json!("[CQ:reply,id=12][CQ:reply,id=12][CQ:reply,id=34]")),
        vec!["12", "34"]
    );
}

#[test]
fn reply_text_limits_are_visible_and_unicode_safe() {
    let quote = parse_reply_quote(
        &json!({
            "message": [{ "type": "text", "data": { "text": "字".repeat(MAX_REPLY_TEXT_CHARS + 1) } }]
        }),
        None,
    );
    assert!(quote.text.contains(&"字".repeat(MAX_REPLY_TEXT_CHARS)));
    assert!(quote.text.contains("引用内容过长，已截断"));
    assert!(!quote.text.contains('\u{fffd}'));
}

struct Fixture {
    core: Arc<Core>,
    channel: Channel,
    requests: mpsc::UnboundedReceiver<Value>,
    replies: mpsc::UnboundedReceiver<Value>,
    lookups: Arc<Mutex<Vec<Value>>>,
    upstream: JoinHandle<()>,
    onebot: JoinHandle<()>,
    root: PathBuf,
}

impl Fixture {
    async fn new(cq: bool, image: bool, unavailable: bool) -> Self {
        Self::with_text(cq, image, unavailable, "请解释引用内容", true).await
    }

    async fn with_text(
        cq: bool,
        image: bool,
        unavailable: bool,
        text: &str,
        mention_member: bool,
    ) -> Self {
        let current_text = text.to_string();
        let (request_tx, requests) = mpsc::unbounded_channel();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/chat/completions", post(move |Json(body): Json<Value>| {
                let tx = request_tx.clone();
                async move {
                    let _ = tx.send(body.clone());
                    if body["stream"] == true {
                        ([("content-type", "text/event-stream")], format!(
                            "data: {}\n\ndata: [DONE]\n\n",
                            json!({"choices":[{"delta":{"content":"引用已收到"},"finish_reason":"stop"}]})
                        )).into_response()
                    } else {
                        Json(json!({"choices":[{"message":{"content":"引用已收到"}}]})).into_response()
                    }
                }
            }))
            .route("/quoted.png", get(|| async {
                use base64::Engine as _;
                let png = base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/l9sAAAAASUVORK5CYII=").unwrap();
                ([("content-type", "image/png")], png)
            }));
        let upstream = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let root = std::env::temp_dir().join(format!("exm-reply-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut cfg = ExmConfig::load(&root);
        cfg.workspace_root = root.clone();
        cfg.agents_dir = root.join("entities");
        cfg.data_dir = root.join("data");
        cfg.use_mock = false;
        cfg.memory_enabled = false;
        cfg.llm.base_url = endpoint.clone();
        cfg.llm.api_key = "fixture-key".into();
        cfg.llm.api_keys.clear();
        cfg.llm.model = "quote-model".into();
        cfg.active_profile = "fixture".into();
        cfg.llm_profiles = vec![serde_json::from_value(json!({
            "id": "fixture", "name": "Fixture", "baseUrl": endpoint, "apiKey": "fixture-key",
            "model": "quote-model", "models": [{"model": "quote-model", "vision": true}]
        }))
        .unwrap()];
        let core = Arc::new(Core::with_config(cfg).unwrap());
        core.registry()
            .upsert_single(
                serde_json::from_value(json!({
                    "identifier": "quote-agent", "name": "Quote Agent", "description": "Reply test",
                    "tier": "unit", "domain": "common", "modelHint": "fixture/quote-model"
                }))
                .unwrap(),
                Some("Answer briefly using the quoted message.".into()),
            )
            .unwrap();
        core.registry()
            .set_active_single(Some("quote-agent"))
            .unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let channel: Channel = serde_json::from_value(json!({
            "id": format!("quote-{}", uuid::Uuid::new_v4()), "type": "napcat", "enabled": true,
            "group": "single:quote-agent", "groupGate": true,
            "config": {"url": format!("ws://{}", listener.local_addr().unwrap())},
            "capability": "chat", "createdAt": ""
        }))
        .unwrap();
        let (reply_tx, replies) = mpsc::unbounded_channel();
        let lookups = Arc::new(Mutex::new(Vec::new()));
        let received = lookups.clone();
        let onebot = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut sent = false;
            while let Some(Ok(Message::Text(text))) = socket.next().await {
                let action: Value = serde_json::from_str(&text).unwrap();
                let name = action["action"].as_str().unwrap_or("");
                if name.starts_with("send_") {
                    let _ = reply_tx.send(action);
                    continue;
                }
                let response = match name {
                    "get_login_info" => json!({"user_id": 10001, "nickname": "Quote Bot"}),
                    "get_group_member_info" => {
                        json!({"user_id": 20001, "card": "群友", "nickname": "Member"})
                    }
                    "get_msg" => {
                        received.lock().push(action.clone());
                        let mut segments = vec![
                            json!({"type":"text","data":{"text":format!("{}引用原文结尾", "原文细节。".repeat(60))}}),
                        ];
                        if image {
                            segments.push(json!({"type":"image","data":{"url":format!("{endpoint}/quoted.png")}}));
                        }
                        json!({
                            "sender":{"user_id":"10001","card":"","nickname":"原作者"},
                            "message": if cq { json!("CQ 格式引用原文") } else { json!(segments) }
                        })
                    }
                    _ => panic!("Unexpected OneBot action: {name}"),
                };
                socket.send(Message::text(json!({
                    "echo": action["echo"], "retcode": if unavailable && name == "get_msg" { 100 } else { 0 },
                    "data": response
                }).to_string())).await.unwrap();
                if name == "get_login_info" && !sent {
                    sent = true;
                    let message = if !mention_member {
                        json!([
                            {"type":"reply","data":{"id":-54321}},
                            {"type":"text","data":{"text":current_text}}
                        ])
                    } else if cq {
                        json!(format!(
                            "[CQ:reply,id=-54321][CQ:at,qq=20001] {current_text}"
                        ))
                    } else {
                        json!([
                            {"type":"reply","data":{"id":-54321}},
                            {"type":"at","data":{"qq":"20001"}},
                            {"type":"text","data":{"text":format!(" {current_text}")}}
                        ])
                    };
                    socket.send(Message::text(json!({
                        "post_type":"message", "message_id":123, "self_id":10001, "user_id":20002,
                        "group_id": if unavailable || !mention_member { Value::Null } else { json!(30001) },
                        "sender":{"nickname":"提问者"}, "message":message
                    }).to_string())).await.unwrap();
                }
            }
        });
        let handle = tokio::spawn(connect_loop(core.clone(), channel.clone()));
        conns().lock().insert(
            channel.id.clone(),
            Conn {
                handle,
                fingerprint: fingerprint(&channel),
            },
        );
        Self {
            core,
            channel,
            requests,
            replies,
            lookups,
            upstream,
            onebot,
            root,
        }
    }

    async fn request(&mut self) -> Value {
        tokio::time::timeout(Duration::from_secs(5), self.requests.recv()).await
            .expect("OneBot reader must process lookup responses without waiting for the 8s API timeout")
            .expect("Model should receive a request")
    }

    async fn finish(&mut self) -> Value {
        tokio::time::timeout(Duration::from_secs(5), self.replies.recv())
            .await
            .expect("Reply must finish")
            .expect("OneBot should receive the answer")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.onebot.abort();
        self.upstream.abort();
        action_txs().lock().remove(&self.channel.id);
        if let Some(conn) = conns().lock().remove(&self.channel.id) {
            conn.handle.abort();
        }
        note_conn_reset(&self.channel.id);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn reply_reaches_real_model_over_shared_socket_array_and_cq() {
    for cq in [false, true] {
        let mut fixture = Fixture::new(cq, false, false).await;
        let request = fixture.request().await;
        let messages = request["messages"].to_string();
        assert!(
            messages.contains("【回复 原作者："),
            "Quoted sender must reach the model"
        );
        assert!(
            messages.contains(if cq {
                "CQ 格式引用原文"
            } else {
                "引用原文结尾"
            }),
            "Quoted body must reach the model"
        );
        assert!(
            messages.contains("@群友 请解释引用内容"),
            "Current message and resolved mentions must survive"
        );
        assert_eq!(
            fixture.lookups.lock().len(),
            1,
            "Reply lookup must be reused for the group mention gate"
        );
        assert_eq!(fixture.lookups.lock()[0]["params"]["message_id"], -54321);
        fixture.finish().await;
        let sid = fixture
            .core
            .flow_active_session(&fixture.channel.id, "30001")
            .unwrap();
        let messages = fixture.core.store.list_messages(&sid, 0).unwrap();
        assert!(messages.iter().any(|message| message
            .statements
            .iter()
            .any(|s| s.text.contains("【回复 原作者："))));
    }
}

#[tokio::test]
async fn reply_image_reaches_model_without_relying_on_ambient_history() {
    let mut fixture = Fixture::new(false, true, false).await;
    let request = fixture.request().await;
    let messages = request["messages"].to_string();
    assert!(messages.contains("引用原文结尾"));
    assert!(
        messages.contains("data:image/png;base64,"),
        "Quoted image bytes must reach the model"
    );
    fixture.finish().await;
    let sid = fixture
        .core
        .flow_active_session(&fixture.channel.id, "30001")
        .unwrap();
    let messages = fixture.core.store.list_messages(&sid, 0).unwrap();
    assert!(messages.iter().any(|message| message
        .attachments
        .iter()
        .any(|attachment| attachment.kind == "image")));
}

#[tokio::test]
async fn reply_lookup_failure_stays_explicit_in_model_context() {
    let mut fixture = Fixture::new(false, false, true).await;
    let request = fixture.request().await;
    let messages = request["messages"].to_string();
    assert!(messages.contains("原消息读取失败，内容不可用"));
    assert!(messages.contains("请解释引用内容"));
    fixture.finish().await;
}

#[tokio::test]
async fn reply_channel_commands_keep_their_direct_handling() {
    for (text, expected) in [("/status", "通道状态"), ("plan:yes", "当前无待审批计划")]
    {
        let mut fixture = Fixture::with_text(false, false, false, text, false).await;
        let reply = fixture.finish().await;
        assert!(reply["params"]["message"].to_string().contains(expected));
        assert!(
            fixture.requests.try_recv().is_err(),
            "Channel commands must not start a model request"
        );
        assert_eq!(fixture.lookups.lock().len(), 1);
    }
    assert!(!is_reply_command("【回复 管理者：/stop】\n请解释这条指令"));
    assert!(!is_reply_command("/status 后面跟着普通问题"));
}
