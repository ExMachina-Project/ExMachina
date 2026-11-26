//! gateway/platform/tests_channel_plan —— capability-parity 4.4：
//! 聊天通道 /plan 命令族与 CLI/TUI/WebUI 的等价性测试。
//!
//! 锚定 command-surface spec「/plan 只读规划 + 审批执行」：
//! ① /plan status 如实报告模式与清单（无会话时如实说，不虚造）；
//! ② /plan start <任务> 进入计划模式并跑只读轮（写工具转 plan.item——
//!    core 侧既有链路，此处验证模式位与回执）；
//! ③ /plan approve 逐项回放、/plan reject 保留清单（事件一致）；
//! ④ approve/reject 非管理员被拒（与 /stop 同口径——共享会话裁决权）；
//! ⑤ plan.* 事件广播 + 落库（四入口同一事实源，可回放审计）。
#[cfg(test)]
use super::*;

mod channel_plan_tests {
    use super::*;

    fn plan_core() -> exm_core::Core {
        let dir = std::env::temp_dir().join(format!("exm-plan-ch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        cfg.security.approval_wait_secs = 0;
        exm_core::Core::with_config(cfg).expect("创建 Core 失败")
    }

    fn chan(admins: Vec<&str>) -> Channel {
        Channel {
            id: "plan-ch".into(),
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

    fn ctx<'a>(
        text: &'a str,
        external_id: &'a str,
        mentioned: bool,
        chat_key: &'a str,
    ) -> InboundCtx<'a> {
        InboundCtx {
            text,
            external_id,
            display_name: "测试用户",
            is_group: false,
            mentioned,
            chat_key,
            flow_name: None,
            media: &[],
        }
    }

    /// 建流会话并绑定（/plan 定位会话与 /stop 同口径）
    fn setup_flow(core: &exm_core::Core, ch: &Channel, key: &str) -> String {
        let title = format!("channel:{}:{}", ch.id, key);
        let s = core.create_session(&title).unwrap();
        let _ = core.bind_flow_session(&ch.id, key, &s.id);
        s.id
    }

    #[tokio::test]
    async fn 无会话时_plan_如实提示不虚造() {
        let core = std::sync::Arc::new(plan_core());
        let ch = chan(vec!["admin-1"]);
        let c = ctx("/plan status", "u1", true, "p1");
        match admit(&core, &ch, &c).await {
            GateDecision::Deny(reply) => {
                assert!(reply.contains("还没有会话"), "回执：{reply}")
            }
            _ => panic!("期望 Deny 回执，得到其他分支"),
        }
    }

    #[tokio::test]
    async fn plan_status_报告模式与空清单() {
        let core = std::sync::Arc::new(plan_core());
        let ch = chan(vec!["admin-1"]);
        let sid = setup_flow(&core, &ch, "p2");
        let c = ctx("/plan status", "admin-1", true, "p2");
        match admit(&core, &ch, &c).await {
            GateDecision::Deny(reply) => {
                assert!(reply.contains("execute"), "空闲模式如实报告：{reply}");
                assert!(reply.contains("暂无计划条目"), "{reply}");
            }
            _ => panic!("期望 Deny 回执，得到其他分支"),
        }
        let _ = sid;
    }

    #[tokio::test]
    async fn plan_start_进入计划模式_事件落库一致() {
        let core = std::sync::Arc::new(plan_core());
        let ch = chan(vec!["admin-1"]);
        let sid = setup_flow(&core, &ch, "p3");
        // 空任务 start：只切模式位（避免依赖规划轮的 mock 行为，链路已由
        // core 侧 plan 套件覆盖）
        let c = ctx("/plan start", "admin-1", true, "p3");
        match admit(&core, &ch, &c).await {
            GateDecision::Deny(reply) => assert!(reply.contains("计划模式"), "{reply}"),
            _ => panic!("期望 Deny 回执，得到其他分支"),
        }
        assert!(
            exm_core::plan::is_plan_mode(&sid),
            "模式位已置（core 事实源）"
        );
        // plan.enabled 事件广播 + 落库（四入口一致的事实源）
        let evts = core.store.list_events(&sid).unwrap();
        let enabled = evts.iter().any(|env| {
            env.get("payload")
                .and_then(|p| p.get("type"))
                .and_then(|t| t.as_str())
                == Some("plan.enabled")
        });
        assert!(enabled, "plan.enabled 事件应落库可回放");

        // 切回执行模式
        let c2 = ctx("/plan execute", "admin-1", true, "p3");
        match admit(&core, &ch, &c2).await {
            GateDecision::Deny(reply) => assert!(reply.contains("执行模式"), "{reply}"),
            _ => panic!("期望 Deny 回执，得到其他分支"),
        }
        assert!(!exm_core::plan::is_plan_mode(&sid));
    }

    #[tokio::test]
    async fn 非管理员不能裁决_plan_但可查看() {
        let core = std::sync::Arc::new(plan_core());
        let ch = chan(vec!["admin-1"]);
        let _sid = setup_flow(&core, &ch, "p4");

        // 非管理员 @ 场景：approve 被拒（裁决影响共享会话）
        let c = ctx("/plan approve", "u9", true, "p4");
        match admit(&core, &ch, &c).await {
            GateDecision::Deny(reply) => assert!(reply.contains("管理员"), "{reply}"),
            _ => panic!("期望 Deny 拒绝回执，得到其他分支"),
        }
        // 非管理员未 @：静默忽略
        let c2 = ctx("/plan reject", "u9", false, "p4");
        assert!(matches!(admit(&core, &ch, &c2).await, GateDecision::Ignore));
        // status 是只读面：非管理员可用
        let c3 = ctx("/plan status", "u9", true, "p4");
        match admit(&core, &ch, &c3).await {
            GateDecision::Deny(reply) => assert!(reply.contains("计划模式"), "{reply}"),
            _ => panic!("期望 Deny 回执，得到其他分支"),
        }
    }

    #[tokio::test]
    async fn plan_reject_保留清单_事件一致() {
        let core = std::sync::Arc::new(plan_core());
        let ch = chan(vec!["admin-1"]);
        let sid = setup_flow(&core, &ch, "p5");
        // 造两个计划条目（core 事实源直填——工具转换链路由 core 套件覆盖）
        exm_core::plan::push_item(
            &sid,
            exm_core::plan::PlanItem {
                id: "pi-1".into(),
                action: "write".into(),
                tool: "fs_write".into(),
                target: "a.txt".into(),
                args: serde_json::json!({ "path": "a.txt", "content": "x" }),
                created_at: exm_core::types::now_iso(),
            },
        );
        let c = ctx("/plan reject", "admin-1", true, "p5");
        match admit(&core, &ch, &c).await {
            GateDecision::Deny(reply) => {
                assert!(reply.contains("保留") && reply.contains('1'), "{reply}")
            }
            _ => panic!("期望 Deny 回执，得到其他分支"),
        }
        // plan.rejected 事件落库
        let evts = core.store.list_events(&sid).unwrap();
        assert!(evts.iter().any(|env| {
            env.get("payload")
                .and_then(|p| p.get("type"))
                .and_then(|t| t.as_str())
                == Some("plan.rejected")
        }));
    }

    #[tokio::test]
    async fn 未知子命令_用法提示不误触() {
        let core = std::sync::Arc::new(plan_core());
        let ch = chan(vec!["admin-1"]);
        let _sid = setup_flow(&core, &ch, "p6");
        let c = ctx("/plan frobnicate", "admin-1", true, "p6");
        match admit(&core, &ch, &c).await {
            GateDecision::Deny(reply) => {
                assert!(reply.contains("用法"), "{reply}");
                assert!(reply.contains("frobnicate"), "回显未知子命令：{reply}")
            }
            _ => panic!("期望 Deny 回执，得到其他分支"),
        }
    }
}
