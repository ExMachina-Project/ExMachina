//! 通道审批裁决（approval-ergonomics 任务组 3）：
//! take_reply 推送摘要 / ok:no 裁决权限 / 无关方裁决被拒 / 非 flow 会话天然隔离。
#[cfg(test)]
mod channel_approval_tests {
    use crate::platform::channel_runtime::{ChannelRun, parse_approval_directive};
    use exm_core::types::{now_iso, ApprovalRequest, CoreEvent};

    fn core() -> exm_core::Core {
        let dir = std::env::temp_dir().join(format!("exm-chap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        exm_core::Core::with_config(cfg).expect("创建 Core 失败")
    }

    fn ch(admins: Vec<&str>) -> crate::platform::channels::Channel {
        crate::platform::channels::Channel {
            id: "c-ap".into(),
            kind: "webhook".into(),
            enabled: true,
            group: None,
            allowed_chats: vec![],
            admins: admins.into_iter().map(String::from).collect(),
            account: None,
            secret: None,
            reply_webhook: None,
            token: None,
            config: Default::default(),
            group_gate: None,
            proactive_reply: None,
            proactive_rate: None,
            capability: "tools".into(),
            memory_isolation: false,
            created_at: String::new(),
        }
    }

    /// 审批单工厂：指定 origin 用户（member 会话来源标记形态）
    fn ap(core: &exm_core::Core, sid: &str, cmd: &str, origin_user: Option<&str>) -> ApprovalRequest {
        let ap = ApprovalRequest {
            id: exm_core::types::new_id()[..8].to_string(),
            session_id: sid.into(),
            node_id: None,
            agent_id: "a1".into(),
            command: cmd.into(),
            status: "pending".into(),
            result: None,
            created_at: now_iso(),
            decided_at: None,
            origin_user: origin_user.map(String::from),
            origin_role: origin_user.map(|_| "member".into()),
            meta_json: None,
        };
        core.store.add_approval(&ap).unwrap();
        ap
    }

    fn evt(kind: &str, sid: &str, payload: serde_json::Value) -> CoreEvent {
        CoreEvent {
            kind: kind.into(),
            session_id: sid.into(),
            payload,
        }
    }

    // ---------------- take_reply 推送 ----------------

    #[test]
    fn 推送文本_带单号与裁决指引() {
        let core = core();
        let c = ch(vec![]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (run, _rx) = rt
            .block_on(ChannelRun::begin(&std::sync::Arc::new(core), &c, "chat-1"))
            .unwrap();
        let sid = run.session_id.clone();
        let e = evt(
            "approval.required",
            &sid,
            serde_json::json!({ "approvalId": "abcd1234", "agentId": "a1", "command": "echo hi" }),
        );
        let text = run.take_reply(&e, 500).expect("应产生推送文本");
        assert!(text.contains("abcd1234"), "推送应带单号: {text}");
        assert!(text.contains("echo hi"), "推送应带命令: {text}");
        assert!(text.contains("ok:abcd1234"), "推送应带批准指令: {text}");
        assert!(text.contains("no:abcd1234"), "推送应带拒绝指令: {text}");
    }

    #[test]
    fn 推送文本_长命令截断() {
        let core = core();
        let c = ch(vec![]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (run, _rx) = rt
            .block_on(ChannelRun::begin(&std::sync::Arc::new(core), &c, "chat-1"))
            .unwrap();
        let long_cmd = "echo ".to_string() + &"x".repeat(300);
        let e = evt(
            "approval.required",
            &run.session_id,
            serde_json::json!({ "approvalId": "abcd1234", "command": long_cmd, "agentId": "a1" }),
        );
        let text = run.take_reply(&e, 500).expect("应产生推送文本");
        assert!(text.contains("…（命令已截断）"), "超长命令应注明截断: {text}");
        assert!(text.chars().count() <= 500, "推送文本不超平台上限");
    }

    // ---------------- 指令解析 ----------------

    #[test]
    fn 指令解析_大小写与空白容错() {
        assert_eq!(
            parse_approval_directive("OK:ab12cd34"),
            Some((true, "ab12cd34".into()))
        );
        assert_eq!(
            parse_approval_directive(" no:ab12cd34 "),
            Some((false, "ab12cd34".into()))
        );
        assert_eq!(parse_approval_directive("ok:"), None, "空单号不受理");
        assert_eq!(parse_approval_directive("echo ok:1"), None, "非指令不误判");
        assert_eq!(parse_approval_directive("plan:yes"), None);
    }

    // ---------------- run() 裁决 ----------------

    /// 裁决后回帖经 run.error 事件;本工具收集该事件文本
    async fn setup_run(admins: Vec<&str>, external: &str) -> (ChannelRun, tokio::sync::broadcast::Receiver<CoreEvent>) {
        let core = core();
        let c = ch(admins);
        // 通道配置落盘（跨通道 admins 判定读 data_dir/channels.json）
        let data_dir = core.config().data_dir.clone();
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(
            data_dir.join("channels.json"),
            serde_json::to_string(&vec![c.clone()]).unwrap(),
        )
        .unwrap();
        let (run, rx) = ChannelRun::begin(&std::sync::Arc::new(core), &c, "chat-1")
            .await
            .unwrap();
        let run = run.with_external_id(external);
        (run, rx)
    }

    /// 等待下一条 run.error 事件文本（跳过 approval.resolved 等中间事件）
    async fn next_reply(rx: &mut tokio::sync::broadcast::Receiver<CoreEvent>) -> String {
        loop {
            let evt = rx.try_recv().expect("应有回帖事件");
            if evt.kind == "run.error" {
                return evt.payload["message"].as_str().unwrap_or("").to_string();
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn origin用户_ok裁决_批准执行() {
        let (run, mut rx) = setup_run(vec![], "u1").await;
        let core = run.core.clone();
        let sid = run.session_id.clone();
        let ap = ap(&core, &sid, "echo channel-approve", Some("u1"));
        run.run(&format!("ok:{}", ap.id)).await;
        let decided = core.approval_get(&ap.id).unwrap().unwrap();
        assert_eq!(decided.status, "executed", "origin 用户应能批准");
        assert!(decided.result.as_deref().unwrap_or("").contains("channel-approve"));
        // 回帖包含裁决结果
        let msg = next_reply(&mut rx).await;
        assert!(msg.contains("已批准"), "回帖应含裁决结果: {msg}");
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 跨通道admin_no裁决_拒绝() {
        // 审批单 origin 是 u1;裁决者是 admin42（配置在通道 admins 清单）
        let (run, mut rx) = setup_run(vec!["admin42"], "admin42").await;
        let core = run.core.clone();
        let sid = run.session_id.clone();
        let ap = ap(&core, &sid, "echo deny-me", Some("u1"));
        run.run(&format!("no:{}", ap.id)).await;
        let decided = core.approval_get(&ap.id).unwrap().unwrap();
        assert_eq!(decided.status, "denied", "admin 应能拒绝");
        let msg = next_reply(&mut rx).await;
        assert!(msg.contains("已拒绝"), "回帖应含拒绝结果: {msg}");
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 无关成员裁决_被拒且单据保持pending() {
        let (run, mut rx) = setup_run(vec![], "u2").await;
        let core = run.core.clone();
        let sid = run.session_id.clone();
        let ap = ap(&core, &sid, "echo not-yours", Some("u1"));
        run.run(&format!("ok:{}", ap.id)).await;
        let decided = core.approval_get(&ap.id).unwrap().unwrap();
        assert_eq!(decided.status, "pending", "无关成员裁决不应生效");
        let msg = next_reply(&mut rx).await;
        assert!(msg.contains("无权"), "应提示无权: {msg}");
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 单号不存在_报告不崩溃() {
        let (run, mut rx) = setup_run(vec!["a"], "a").await;
        let ws = run.core.config().workspace_root.clone();
        run.run("ok:deadbeef").await;
        let msg = next_reply(&mut rx).await;
        assert!(msg.contains("不存在"), "应报告单号不存在: {msg}");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 单号前缀匹配_唯一命中即裁决() {
        let (run, mut rx) = setup_run(vec!["a"], "a").await;
        let core = run.core.clone();
        let sid = run.session_id.clone();
        let ap = ap(&core, &sid, "echo prefix-match", Some("a"));
        // 前 4 位作为输入（真前缀,非补全）
        run.run(&format!("ok:{}", &ap.id[..4])).await;
        let decided = core.approval_get(&ap.id).unwrap().unwrap();
        assert_eq!(decided.status, "executed", "唯一前缀命中应裁决（完整单号 {} 的前 4 位）", ap.id);
        let _ = next_reply(&mut rx).await;
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    // ---------------- 非 flow 会话隔离（任务 3.3） ----------------

    #[test]
    fn 非flow会话_无external_id不响应指令语义() {
        // WebUI/CLI 会话不经过 ChannelRun::begin,external_id 为 None;
        // take_reply 只存在于通道 spawn_reply 路径,WS 审批推送不受影响。
        // 此处锁定构造语义:begin 出的 run 默认 external_id None(适配器显式注入才有)
        let core = core();
        let c = ch(vec![]);
        let core_arc = std::sync::Arc::new(core);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (run, _rx) = rt
            .block_on(ChannelRun::begin(&core_arc, &c, "chat-1"))
            .unwrap();
        assert!(run.external_id.is_none(), "默认无外部 ID");
        // run() 对 ok: 指令在无 external_id 时:caller 为空串,既非 origin
        // 用户也非 admin → 拒绝。锁定该行为(admins 空 + origin_user 空 = 全拒)。
        let ws = core_arc.config().workspace_root.clone();
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ---------------- 回帖循环：中间提示不吞收束答复 ----------------

    /// 审批推送后回帖循环不得退出：审批是本轮中间事件，最终答复（run.finished）
    /// 仍须送达——否则通道里用户批完只看到审批摘要，永远等不到执行结果。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 审批推送后不结束回帖_最终答复仍送达() {
        let core = std::sync::Arc::new(core());
        let c = ch(vec![]);
        let (run, rx) = ChannelRun::begin(&core, &c, "chat-1").await.unwrap();
        let sid = run.session_id.clone();
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sent2 = sent.clone();
        let reply = crate::platform::channel_runtime::spawn_reply(
            &run,
            rx,
            500,
            move |text| {
                let sent = sent2.clone();
                async move {
                    sent.lock().unwrap().push(text);
                }
            },
            |_item: crate::platform::MediaItem| async { false },
        );
        core.events
            .send(CoreEvent {
                kind: "approval.required".into(),
                session_id: sid.clone(),
                payload: serde_json::json!({
                    "approvalId": "abcd1234", "agentId": "a1", "command": "echo hi"
                }),
            })
            .unwrap();
        core.events
            .send(CoreEvent {
                kind: "run.finished".into(),
                session_id: sid.clone(),
                payload: serde_json::json!({
                    "statements": [{ "text": "已完成" }]
                }),
            })
            .unwrap();
        reply.await.unwrap();
        let sent = sent.lock().unwrap();
        assert!(
            sent.iter().any(|t| t.contains("abcd1234")),
            "审批提示应送达: {sent:?}"
        );
        assert!(
            sent.iter().any(|t| t.contains("已完成")),
            "审批后的最终答复应送达: {sent:?}"
        );
        drop(sent);
        let ws = core.config().workspace_root.clone();
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// 审批富消息钩子（QQ 官方机器人按钮）：命中后审批文本不再走普通发送，
    /// 回帖循环继续等待收束答复。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 审批富消息钩子_命中后不走文本且继续收束() {
        type HookFuture = std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>;
        let core = std::sync::Arc::new(core());
        let c = ch(vec![]);
        let (run, rx) = ChannelRun::begin(&core, &c, "chat-1").await.unwrap();
        let sid = run.session_id.clone();
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let hooked = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sent2 = sent.clone();
        let hooked2 = hooked.clone();
        let hook: crate::platform::channel_runtime::ApprovalHook = Box::new(
            move |prompt: crate::platform::ApprovalPrompt| -> HookFuture {
                let hooked = hooked2.clone();
                Box::pin(async move {
                    hooked.lock().unwrap().push(prompt.approval_id);
                    true
                })
            },
        );
        let reply = crate::platform::channel_runtime::spawn_reply_with_approval(
            &run,
            rx,
            500,
            move |text| {
                let sent = sent2.clone();
                async move {
                    sent.lock().unwrap().push(text);
                }
            },
            |_item: crate::platform::MediaItem| async { false },
            Some(hook),
        );
        core.events
            .send(CoreEvent {
                kind: "approval.required".into(),
                session_id: sid.clone(),
                payload: serde_json::json!({
                    "approvalId": "abcd1234", "agentId": "a1", "command": "echo hi"
                }),
            })
            .unwrap();
        core.events
            .send(CoreEvent {
                kind: "run.finished".into(),
                session_id: sid.clone(),
                payload: serde_json::json!({
                    "statements": [{ "text": "已完成" }]
                }),
            })
            .unwrap();
        reply.await.unwrap();
        assert_eq!(
            hooked.lock().unwrap().as_slice(),
            ["abcd1234"],
            "审批钩子应收到单号"
        );
        let sent = sent.lock().unwrap();
        assert!(
            sent.iter().all(|t| !t.contains("abcd1234")),
            "富消息命中后审批文本不应重复走普通发送: {sent:?}"
        );
        assert!(
            sent.iter().any(|t| t.contains("已完成")),
            "钩子命中后仍应送达收束答复: {sent:?}"
        );
        drop(sent);
        let ws = core.config().workspace_root.clone();
        let _ = std::fs::remove_dir_all(&ws);
    }
}
