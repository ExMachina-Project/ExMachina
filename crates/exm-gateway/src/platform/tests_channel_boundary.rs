//! gateway/platform/tests_channel_boundary —— 自 platform.rs 按资源域拆出（纯搬移，零行为变更）
#[cfg(test)]
use super::*;

mod channel_boundary_tests {
    use super::*;

    fn core() -> exm_core::Core {
        let dir = std::env::temp_dir().join(format!("exm-chan-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        exm_core::Core::with_config(cfg).expect("创建 Core 失败")
    }

    fn ch(kind: &str, capability: &str, isolation: bool) -> Channel {
        Channel {
            id: format!("c-{kind}"),
            kind: kind.into(),
            enabled: true,
            group: None,
            allowed_chats: vec![],
            admins: vec![],
            account: None,
            secret: None,
            reply_webhook: None,
            token: None,
            config: Default::default(),
            group_gate: None,
            capability: capability.into(),
            memory_isolation: isolation,
            created_at: String::new(),
        }
    }

    /// 能力边界 + 记忆分区是通道的「安全合同」：所有平台共用 ChannelRun，
    /// 在这里锁定一次语义，六个 IM 适配器与视频通道即全部继承。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 通道执行_能力与记忆隔离落到会话() {
        let core = core();
        let c = ch("webhook", "tools", true);
        let (run, _rx) = ChannelRun::begin(&std::sync::Arc::new(core), &c, "chat-1")
            .await
            .expect("建立执行上下文");
        assert_eq!(run.scope, exm_core::types::ToolScope::Tools);
        let s = run
            .core
            .store
            .get_session(&run.session_id)
            .unwrap()
            .unwrap();
        let gid = run.core.active_group();
        assert_eq!(
            s.memory_partition.as_deref(),
            Some(format!("{gid}::channel:c-webhook").as_str())
        );
    }

    #[test]
    fn 通道语义_旧配置回退与边界裁剪() {
        // capability 未知值回退完整档：老通道不因字段拼写/裁剪而失效
        assert_eq!(
            ch("telegram", "full-access", false).tool_scope(),
            exm_core::types::ToolScope::Computer
        );
        assert_eq!(
            ch("telegram", "chat", false).tool_scope(),
            exm_core::types::ToolScope::Chat
        );
        // 关闭隔离 = None（共享记忆）；开启 = 组内唯一分区键
        assert_eq!(ch("telegram", "chat", false).memory_partition("g"), None);
        assert_eq!(
            ch("telegram", "chat", true).memory_partition("g"),
            Some("g::channel:c-telegram".into())
        );
        // 共享档同组同键：不同通道在同组共用同一记忆命名空间
        assert_eq!(
            ch("telegram", "chat", true).memory_partition("g"),
            ch("telegram", "chat", true).memory_partition("g")
        );
        assert_ne!(
            ch("telegram", "chat", true).memory_partition("g"),
            ch("telegram", "chat", true).memory_partition("h")
        );
    }

    // ---------------- 计划模式跨端审批（spec plan-mode 通道面） ----------------

    /// 快捷审批指令：plan:yes 回放 / plan:no 保留，均不进模型轮次
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 通道计划审批_快捷指令直达裁决() {
        let core = std::sync::Arc::new(core());
        let c = ch("webhook", "tools", false);
        let (run, _rx) = ChannelRun::begin(&core, &c, "plan-chat")
            .await
            .expect("建立执行上下文");
        let sid = run.session_id.clone();
        // 无清单时提示，不报错
        run.run("plan:yes").await;
        // 造清单（等价拦截）并拒绝
        core.set_plan_mode(&sid, true);
        exm_core::plan::push_item(
            &sid,
            exm_core::plan::to_plan_item(
                exm_core::types::ToolName::Filesystem,
                &serde_json::json!({ "op": "write", "path": "ch.txt", "content": "x" }),
            ),
        );
        let (run2, _rx2) = ChannelRun::begin(&core, &c, "plan-chat")
            .await
            .expect("建立执行上下文");
        run2.run("plan:no").await;
        assert!(exm_core::plan::is_plan_mode(&sid), "拒绝后仍在 plan 模式");
        assert_eq!(exm_core::plan::list_items(&sid).len(), 1, "清单保留");
        // 再批准：回放落地
        let (run3, _rx3) = ChannelRun::begin(&core, &c, "plan-chat")
            .await
            .expect("建立执行上下文");
        run3.run("plan:yes").await;
        assert!(!exm_core::plan::is_plan_mode(&sid), "批准后切 execute");
        assert!(
            core.config().workspace_root.join("ch.txt").exists(),
            "回放应写文件"
        );
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// plan.pending 事件 → 通道文本摘要卡（含快捷审批指引）
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 通道计划审批_待审事件转文本摘要() {
        let core = std::sync::Arc::new(core());
        let c = ch("webhook", "tools", false);
        let (run, _rx) = ChannelRun::begin(&core, &c, "plan-chat2")
            .await
            .expect("建立执行上下文");
        let sid = run.session_id.clone();
        core.set_plan_mode(&sid, true);
        exm_core::plan::push_item(
            &sid,
            exm_core::plan::to_plan_item(
                exm_core::types::ToolName::Edit,
                &serde_json::json!({ "path": "s.rs", "old_string": "a", "new_string": "b" }),
            ),
        );
        let evt = exm_core::types::CoreEvent {
            kind: "plan.pending".into(),
            session_id: sid.clone(),
            payload: serde_json::json!({ "count": 1 }),
        };
        let text = run.take_reply(&evt, 500).expect("应有文本回复");
        assert!(text.contains("计划待审批"), "摘要标题：{text}");
        assert!(text.contains("edit"), "含条目工具：{text}");
        assert!(text.contains("plan:yes"), "含快捷审批指引：{text}");
        // plan.applied → 逐条结果
        let evt2 = exm_core::types::CoreEvent {
            kind: "plan.applied".into(),
            session_id: sid,
            payload: serde_json::json!({ "ok": true, "target": "s.rs" }),
        };
        let text2 = run.take_reply(&evt2, 500).expect("应有结果回复");
        assert!(text2.contains("s.rs"), "含目标：{text2}");
    }
}
