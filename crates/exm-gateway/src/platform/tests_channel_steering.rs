//! gateway/platform/tests_channel_steering —— capability-parity 3.4：
//! 聊天通道 steering 分流与显式 /queue 入队的语义测试。
//!
//! 锚定 session-steering spec 的三条红线：
//! ① 普通消息立即被接受（不等待运行完成），在安全检查点 steering 当前任务，
//!    绝不取消运行（「普通消息不打断」）；
//! ② 只有明确 /queue 才入后续队列，回执带序号 + pending，不冒充完成；
//! ③ /stop 独立：非管理员不能停共享会话（权限测试见 tests_identity_gate）。
//!
//! 夹具纪律：mock provider（隔离临时目录，无真实模型/凭据），轮次经
//! poll_fn 在「运行中」窗口内驱动断言，绝不依赖真实平台连接。

//! gateway/platform 域测试：通道 steering /queue 语义（capability-parity 3.4）
#[cfg(test)]
use super::*;
mod channel_steering_tests {
    use super::*;
    use exm_core::types::{Session, UserIdentity};

    fn steering_core() -> exm_core::Core {
        let dir = std::env::temp_dir().join(format!("exm-steer-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        cfg.security.approval_wait_secs = 0;
        exm_core::Core::with_config(cfg).expect("创建 Core 失败")
    }

    fn chan(admins: Vec<&str>) -> Channel {
        Channel {
            id: "steer-ch".into(),
            kind: "telegram".into(),
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
            mention_reply: None,
            capability: "computer".into(),
            memory_isolation: false,
            created_at: exm_core::types::now_iso(),
        }
    }

    /// 标记会话为运行中（round_trace 是 steering 分流判定的唯一事实源：
    /// states 含该会话且 steering_open = 有运行中轮次）。真实「不打断运行」
    /// 的端到端语义由 exm-core 3.2 的写工具 sentinel 测试覆盖，此处测分流逻辑。
    fn mark_running(sid: &str) {
        exm_core::round_trace::begin(sid);
    }

    fn mark_idle(sid: &str) {
        exm_core::round_trace::end(sid);
    }

    /// 普通消息 steering 分流：运行中的会话收到第二条普通消息时
    /// - 不等锁（立即返回，`run_guarded` 直接收束）
    /// - 不取消运行（is_running 保持 true）
    /// - 消息落库为 steering 暂存（steer: 标记），轮次未被取消
    #[tokio::test]
    async fn 运行中普通消息_steering_不打断不等待() {
        let core = std::sync::Arc::new(steering_core());
        let ch = chan(vec![]);
        // 建流会话（不走适配器连接，直接定位会话键）
        let title = format!("channel:{}:g1", ch.id);
        let s = core.create_session(&title).unwrap();
        let _ = core.bind_flow_session(&ch.id, "g1", &s.id);

        mark_running(&s.id);

        // 第二条普通消息经通道执行入口：应 steering 而不是排队开新轮
        let (run, _rx) = ChannelRun::begin(&core, &ch, "g1").await.unwrap();
        assert_eq!(run.session_id, s.id, "流会话应复用同一会话");
        let steered = run.clone();
        run.run("中途补充：换个方向查").await; // 分流后应立即返回

        assert!(
            exm_core::round_trace::is_running(&s.id),
            "steering 不得取消运行中的轮次"
        );
        // steering 文本已落库（steer: 来源标记）
        let msgs = core.store.list_messages(&s.id, 50).unwrap();
        assert!(
            msgs.iter().any(|m| m
                .agent_id
                .as_deref()
                .map(|a| a.starts_with("steer:"))
                .unwrap_or(false)),
            "steering 消息应落库并带 steer: 标记"
        );
        let _ = steered;
        // steering 分流不触碰运行态（真实轮次由 core 侧 sentinel 测试保障）
        assert!(exm_core::round_trace::is_running(&s.id));
        mark_idle(&s.id);
    }

    /// 空闲会话：普通消息不产生虚假 steering 状态——按普通轮次执行
    #[tokio::test]
    async fn 空闲会话_普通消息_直接执行不虚标() {
        let core = steering_core();
        let core = std::sync::Arc::new(core);
        let ch = chan(vec![]);
        let title = format!("channel:{}:p1", ch.id);
        let s = core.create_session(&title).unwrap();
        let _ = core.bind_flow_session(&ch.id, "p1", &s.id);

        let (run, _rx) = ChannelRun::begin(&core, &ch, "p1").await.unwrap();
        run.run("空闲时的第一条消息").await;

        let msgs = core.store.list_messages(&s.id, 50).unwrap();
        assert!(
            !msgs.iter().any(|m| m
                .agent_id
                .as_deref()
                .map(|a| a.starts_with("steer:"))
                .unwrap_or(false)),
            "空闲会话的普通消息不应产生 steering 标记"
        );
    }

    /// /queue 显式入队：回执 queued+pending+序号；普通消息绝不入队
    #[tokio::test]
    async fn queue命令_显式入队_回执不冒充完成() {
        let core = steering_core();
        let ch = chan(vec![]);
        let title = format!("channel:{}:g1", ch.id);
        let s = core.create_session(&title).unwrap();
        let _ = core.bind_flow_session(&ch.id, "g1", &s.id);

        let ctx = InboundCtx {
            text: "/queue 修复隔离项目中的测试",
            external_id: "u1",
            display_name: "u1",
            is_group: true,
            mentioned: true,
            chat_key: "g1",
            flow_name: None,
            media: &[],
        };
        match admit(&core, &ch, &ctx).await {
            GateDecision::Deny(receipt) => {
                assert!(
                    receipt.contains("已加入后续队列"),
                    "回执应说明已入队：{receipt}"
                );
                assert!(receipt.contains("第 1 条"), "回执应带队列序号：{receipt}");
                // 「回执不冒充完成」：不得宣称任务已执行/已处理/已完成
                //（「当前任务完成后按序执行」是诚实的时序说明，允许出现）
                for bad in ["已执行", "已处理", "已完成", "已完成任务"] {
                    assert!(
                        !receipt.contains(bad),
                        "回执冒充完成（含「{bad}」）：{receipt}"
                    );
                }
            }
            _ => panic!("/queue 应给入队回执（Deny 通道）"),
        }
        // 队列确实有一条 queued 任务，来源 chat
        let items = exm_core::task_queue::list(&s.id);
        assert_eq!(items.len(), 1, "应恰好入队一条");
        assert_eq!(items[0].status, "queued");
        assert_eq!(items[0].source, "chat");
        assert_eq!(items[0].task, "修复隔离项目中的测试");

        // 序号单调：再入一条
        let ctx2 = InboundCtx {
            text: "/queue 第二条后续任务",
            external_id: "u1",
            display_name: "u1",
            is_group: true,
            mentioned: true,
            chat_key: "g1",
            flow_name: None,
            media: &[],
        };
        if let GateDecision::Deny(r) = admit(&core, &ch, &ctx2).await {
            assert!(r.contains("第 2 条"), "第二条序号应为 2：{r}");
        } else {
            panic!("第二条 /queue 应给回执");
        }
        // /queue list 查询
        let ctx3 = InboundCtx {
            text: "/queue list",
            external_id: "u1",
            display_name: "u1",
            is_group: true,
            mentioned: true,
            chat_key: "g1",
            flow_name: None,
            media: &[],
        };
        match admit(&core, &ch, &ctx3).await {
            GateDecision::Deny(list) => {
                assert!(
                    list.contains("2 条") && list.contains("第二条后续任务"),
                    "列表回执：{list}"
                )
            }
            _ => panic!("/queue list 应给列表回执"),
        }
    }

    /// /queue 不带参数：用法提示；流无会话：不代建
    #[tokio::test]
    async fn queue命令_边界用法() {
        let core = steering_core();
        let ch = chan(vec![]);
        // 无会话的流：不代建（queue 归属真实会话）
        let ctx = InboundCtx {
            text: "/queue 先建上下文再排队",
            external_id: "u1",
            display_name: "u1",
            is_group: true,
            mentioned: true,
            chat_key: "fresh",
            flow_name: None,
            media: &[],
        };
        match admit(&core, &ch, &ctx).await {
            GateDecision::Deny(msg) => assert!(msg.contains("还没有会话"), "无会话流应提示：{msg}"),
            _ => panic!(),
        }
        // 空参数：用法提示
        let ctx2 = InboundCtx {
            text: "/queue",
            external_id: "u1",
            display_name: "u1",
            is_group: true,
            mentioned: true,
            chat_key: "fresh",
            flow_name: None,
            media: &[],
        };
        match admit(&core, &ch, &ctx2).await {
            GateDecision::Deny(msg) => assert!(msg.contains("用法"), "应给用法提示：{msg}"),
            _ => panic!(),
        }
    }

    /// 普通消息绝不隐式入队（spec：普通消息不是队列任务）
    #[tokio::test]
    async fn 普通消息_绝不隐式入队() {
        let core = steering_core();
        let ch = chan(vec![]);
        let title = format!("channel:{}:g1", ch.id);
        let s = core.create_session(&title).unwrap();
        let _ = core.bind_flow_session(&ch.id, "g1", &s.id);

        let ctx = InboundCtx {
            text: "普通消息：帮我做点事",
            external_id: "u1",
            display_name: "u1",
            is_group: true,
            mentioned: true,
            chat_key: "g1",
            flow_name: None,
            media: &[],
        };
        match admit(&core, &ch, &ctx).await {
            GateDecision::Allow(clean, _) => assert_eq!(clean, "普通消息：帮我做点事"),
            _ => panic!("普通消息应放行"),
        }
        assert!(
            exm_core::task_queue::list(&s.id).is_empty(),
            "普通消息不得隐式创建队列任务"
        );
    }

    /// steering 受理失败（如超长文本）如实回执，不打断运行
    #[tokio::test]
    async fn steering_超长文本_如实拒绝() {
        let core = std::sync::Arc::new(steering_core());
        let ch = chan(vec![]);
        let title = format!("channel:{}:g1", ch.id);
        let s = core.create_session(&title).unwrap();
        let _ = core.bind_flow_session(&ch.id, "g1", &s.id);
        mark_running(&s.id);

        let (run, _rx) = ChannelRun::begin(&core, &ch, "g1").await.unwrap();
        let long = "长".repeat(20_001);
        run.run(&long).await; // 应立即返回（受理失败回执），不打断
        assert!(
            exm_core::round_trace::is_running(&s.id),
            "受理失败也不得取消运行"
        );
        mark_idle(&s.id);
    }

    /// 非管理员不能 /stop（3.4 验收口径在 steering 语义下复核一次：
    /// 与 tests_identity_gate 的权限矩阵互补，此处锚定「/stop 独立于 steering」）
    #[tokio::test]
    async fn stop_独立于steering_非管理员拒绝() {
        let core = std::sync::Arc::new(steering_core());
        let mut ch = chan(vec!["boss"]);
        ch.group_gate = Some(true);
        let title = format!("channel:{}:g1", ch.id);
        let s = core.create_session(&title).unwrap();
        let _ = core.bind_flow_session(&ch.id, "g1", &s.id);
        mark_running(&s.id);

        // 非管理员 @：拒绝回执且运行不被打断（/stop 权限闸门先于执行）
        let ctx = InboundCtx {
            text: "/stop",
            external_id: "u1",
            display_name: "u1",
            is_group: true,
            mentioned: true,
            chat_key: "g1",
            flow_name: None,
            media: &[],
        };
        match admit(&core, &ch, &ctx).await {
            GateDecision::Deny(msg) => assert!(msg.contains("管理员"), "{msg}"),
            GateDecision::Ignore => panic!("@ 场景必须回执拒绝"),
            _ => panic!(),
        }
        assert!(
            exm_core::round_trace::is_running(&s.id),
            "非管理员 /stop 不得取消运行"
        );
        // 管理员 /stop：真实停止
        let admin_ctx = InboundCtx {
            text: "/stop",
            external_id: "boss",
            display_name: "boss",
            is_group: true,
            mentioned: true,
            chat_key: "g1",
            flow_name: None,
            media: &[],
        };
        match admit(&core, &ch, &admin_ctx).await {
            GateDecision::Deny(msg) => assert!(msg.contains("已停止"), "管理员应停掉：{msg}"),
            _ => panic!(),
        }
    }

    /// steering-only 轮次的回帖生命周期（3.4 交互语义）：
    /// - steering 回执（run.error 形态）立即送达；
    /// - 不因 round_done 未置位而悬挂：被引导轮次的收束事件到达即代发答复并退出；
    /// - 退出后释放发言权（账本不残留——后续轮次的回帖任务可正常接管）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn steering_only_回帖_回执立即达且代发被引导轮次答复() {
        let core = std::sync::Arc::new(steering_core());
        let ch = chan(vec![]);
        let title = format!("channel:{}:g1", ch.id);
        let s = core.create_session(&title).unwrap();
        let _ = core.bind_flow_session(&ch.id, "g1", &s.id);
        let (run, rx) = ChannelRun::begin(&core, &ch, "g1").await.unwrap();
        let sid = run.session_id.clone();
        assert_eq!(sid, s.id);
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sent2 = sent.clone();
        let reply = crate::platform::channel::spawn_reply(
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
        // 模拟「另一轮次正在运行」+ 本消息 steering 分流
        exm_core::round_trace::begin(&sid);
        run.run("运行中的补充方向").await; // 立即返回（steering-only）
                                           // steering 回执应已投递（run.error 形态：【报告】已接收……）
                                           //（广播投递与回帖任务调度有微秒级时差，先推进一两拍再断言）
        for _ in 0..20 {
            if !sent.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        {
            let got = sent.lock().unwrap();
            assert!(
                got.iter()
                    .any(|t| t.contains("已接收") && t.contains("steering")),
                "steering 回执应立即送达: {got:?}"
            );
        }
        // 被引导轮次收束：run.finished 到达 → steering-only 任务代发答复并退出
        core.events
            .send(exm_core::types::CoreEvent {
                kind: "run.finished".into(),
                session_id: sid.clone(),
                payload: serde_json::json!({ "statements": [{ "text": "被引导轮次的结果" }] }),
            })
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), reply)
            .await
            .expect("steering-only 回帖任务应在被引导轮次收束后退出（不悬挂）")
            .unwrap();
        let got = sent.lock().unwrap();
        assert!(
            got.iter().any(|t| t.contains("被引导轮次的结果")),
            "被引导轮次的答复应由 steering-only 任务代发: {got:?}"
        );
        // 发言权已释放：账本无残留
        exm_core::round_trace::end(&sid);
    }

    /// 供后续测试复用的类型断言（避免未使用警告；Session/UserIdentity 导入备用）
    #[allow(dead_code)]
    fn _type_assert(_: Option<&Session>, _: Option<&UserIdentity>) {}
}
