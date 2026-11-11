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
            proactive_reply: None,
            proactive_rate: None,
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

    /// run.error 文案已含句式前缀（【报告】/【警告】）时不再二次包裹——
    /// 部署实测「【警告】【报告】…」双层前缀的回归锚点
    #[test]
    fn 回帖_前缀不双层() {
        let core = exm_core::Core::create(std::env::temp_dir().join(format!(
            "exm-prefix-{}",
            uuid::Uuid::new_v4()
        )))
        .expect("创建 Core 失败");
        let run = ChannelRun {
            core: std::sync::Arc::new(core),
            session_id: "s".into(),
            prev_group: String::new(),
            prev_single: None,
            switched: false,
            kind: "webhook".into(),
            scope: exm_core::types::ToolScope::Tools,
            origin: None,
            external_id: None,
            flow: None,
            round_done: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        // 已含【报告】：原样透传
        let evt = exm_core::types::CoreEvent {
            kind: "run.error".into(),
            session_id: "s".into(),
            payload: serde_json::json!({ "message": "【报告】当前无待审批计划（清单为空或已处理）。" }),
        };
        let text = run.take_reply(&evt, 500).unwrap();
        assert_eq!(text, "【报告】当前无待审批计划（清单为空或已处理）。");
        assert!(!text.contains("【警告】"), "不得二次包裹：{text}");
        // 已含【警告】：原样透传
        let evt2 = exm_core::types::CoreEvent {
            kind: "run.error".into(),
            session_id: "s".into(),
            payload: serde_json::json!({ "message": "【警告】计划回放失败：x" }),
        };
        let text2 = run.take_reply(&evt2, 500).unwrap();
        assert_eq!(text2, "【警告】计划回放失败：x");
        // 裸消息：正常包裹【警告】
        let evt3 = exm_core::types::CoreEvent {
            kind: "run.error".into(),
            session_id: "s".into(),
            payload: serde_json::json!({ "message": "会话 token 预算已耗尽" }),
        };
        let text3 = run.take_reply(&evt3, 500).unwrap();
        assert_eq!(text3, "【警告】会话 token 预算已耗尽");
    }

    /// 沉默约定（IM silence）：模型输出 `<沉默/>` = 无话可说 → take_reply 返回
    /// None（不投递、不记未读）——群聊里不再出现「不参与群聊闲聊」式废话。
    /// 独立成行 / 与少量文字混行 / 全半角变体都识别；正常回复不受影响。
    #[test]
    fn 回帖_沉默约定整轮丢弃() {
        let core = exm_core::Core::create(std::env::temp_dir().join(format!(
            "exm-silence-{}",
            uuid::Uuid::new_v4()
        )))
        .expect("创建 Core 失败");
        let run = ChannelRun {
            core: std::sync::Arc::new(core),
            session_id: "s".into(),
            prev_group: String::new(),
            prev_single: None,
            switched: false,
            kind: "webhook".into(),
            scope: exm_core::types::ToolScope::Tools,
            origin: None,
            external_id: None,
            flow: None,
            round_done: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let evt = |text: &str| exm_core::types::CoreEvent {
            kind: "run.finished".into(),
            session_id: "s".into(),
            payload: serde_json::json!({ "statements": [{ "tag": "报告", "text": text }] }),
        };
        // 独立成行：整轮丢弃
        assert!(run.take_reply(&evt("<沉默/>"), 500).is_none());
        // 混行（前后有解释文字）：仍整轮丢弃
        assert!(run
            .take_reply(&evt("刚才群里的对话我没参与。\n<沉默/>"), 500)
            .is_none());
        // 全角尖括号变体：识别
        assert!(run.take_reply(&evt("＜沉默/＞"), 500).is_none());
        // 正常回复：原样投递，不加强制句式前缀（前缀由模型自己决定）
        assert_eq!(
            run.take_reply(&evt("已把任务跑完，结果见文件。"), 500).unwrap(),
            "已把任务跑完，结果见文件。"
        );
        // 模型自己写了前缀：保留，且投递层不再叠加第二层
        assert_eq!(
            run.take_reply(&evt("【报告】本机已完成推演。"), 500).unwrap(),
            "【报告】本机已完成推演。"
        );
        // 文本里偶然提到「沉默」二字（非标记形态）：不误杀
        assert!(run.take_reply(&evt("我方选择了沉默策略。"), 500).is_some());
    }

    /// 长回复不再被收束平铺截断吞掉（回归锚点）：flatten_statements 曾把
    /// 全文截到单段上限（650 字符级）再交给 split_reply——分段机制形同虚设，
    /// 超出部分直接丢失。现在平铺保留全文（仅防异常超长的总预算天花板），
    /// 由 split_reply 做边界切分、回帖循环做段数封顶。
    #[test]
    fn 回帖_长文不因平铺截断丢内容() {
        let long = "第一段。\n\n".to_string() + &"正文细节。".repeat(400); // ~2400 字符
        let evt = exm_core::types::CoreEvent {
            kind: "run.finished".into(),
            session_id: "s".into(),
            payload: serde_json::json!({ "statements": [{ "tag": "报告", "text": long }] }),
        };
        let core = exm_core::Core::create(std::env::temp_dir().join(format!(
            "exm-longreply-{}",
            uuid::Uuid::new_v4()
        )))
        .expect("创建 Core 失败");
        let run = ChannelRun {
            core: std::sync::Arc::new(core),
            session_id: "s".into(),
            prev_group: String::new(),
            prev_single: None,
            switched: false,
            kind: "webhook".into(),
            scope: exm_core::types::ToolScope::Tools,
            origin: None,
            external_id: None,
            flow: None,
            round_done: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let text = run.take_reply(&evt, 650).expect("应有文本回复");
        assert!(
            text.chars().count() > 2_000,
            "超长文不应被平铺截到单段上限：{}",
            text.chars().count()
        );
        assert!(text.ends_with("正文细节。"), "结尾内容不得丢失");
        // 分段切分后内容完整（段数 > 1 才证明分段机制真正参与）；
        // 硬切片自带「（续 N/M）」标注，允许尾段带标注，但正文不得丢
        let segments = super::super::channel::split_reply(&text, 650);
        assert!(segments.len() > 3, "应切出多段: {}", segments.len());
        let last = segments.last().unwrap();
        assert!(
            last.contains("正文细节。"),
            "最后一段应含正文结尾内容: {last}"
        );
        for seg in &segments {
            assert!(seg.chars().count() <= 650 + 20, "单段不得远超预算");
        }
    }
}
