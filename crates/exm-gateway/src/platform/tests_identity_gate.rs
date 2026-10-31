//! gateway/platform/tests_identity_gate —— 自 platform.rs 按资源域拆出（纯搬移，零行为变更）
#[cfg(test)]
use super::*;

mod identity_gate_tests {
    use super::*;
    use exm_core::types::{PairingCode, UserIdentity};

    fn test_core() -> exm_core::Core {
        let dir = std::env::temp_dir().join(format!("exm-gate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        exm_core::Core::create(&dir).expect("创建 Core 失败")
    }

    fn chan(gate: Option<bool>) -> Channel {
        Channel {
            id: "tg-main".into(),
            kind: "telegram".into(),
            enabled: true,
            group: None,
            allowed_chats: vec![],
            admins: vec![],
            account: None,
            secret: None,
            reply_webhook: None,
            token: None,
            config: Default::default(),
            group_gate: gate,
            capability: "computer".into(),
            memory_isolation: false,
            created_at: exm_core::types::now_iso(),
        }
    }

    fn ctx<'a>(text: &'a str, uid: &'a str, is_group: bool, mentioned: bool) -> InboundCtx<'a> {
        InboundCtx {
            text,
            external_id: uid,
            display_name: uid,
            is_group,
            mentioned,
            chat_key: "test",
        }
    }

    /// 群聊门控：默认开——未提及忽略、提及放行、私聊豁免
    #[tokio::test]
    async fn 群聊门控_提及与私聊语义() {
        let core = test_core();
        let ch = chan(None);
        assert!(core.config().identity.group_gate_default, "全局默认应开启");
        let group_unmentioned = ctx("大家好", "u1", true, false);
        let group_mentioned = ctx("@bot 帮我看下", "u1", true, true);
        let private_unmentioned = ctx("在吗", "u1", false, false);
        match admit(&core, &ch, &group_unmentioned).await {
            GateDecision::Ignore => {}
            _ => panic!("群聊未提及应忽略"),
        }
        match admit(&core, &ch, &group_mentioned).await {
            GateDecision::Allow => {}
            _ => panic!("群聊提及应放行"),
        }
        match admit(&core, &ch, &private_unmentioned).await {
            GateDecision::Allow => {}
            _ => panic!("私聊应豁免门控"),
        }
    }

    /// 通道级覆盖：显式关闭门控后群聊未提及也放行
    #[tokio::test]
    async fn 群聊门控_通道级关闭() {
        let core = test_core();
        let ch = chan(Some(false));
        match admit(&core, &ch, &ctx("随便聊聊", "u1", true, false)).await {
            GateDecision::Allow => {}
            _ => panic!("通道级关闭后不应拦截"),
        }
        let ch_on = chan(Some(true));
        match admit(&core, &ch_on, &ctx("随便聊聊", "u1", true, false)).await {
            GateDecision::Ignore => {}
            _ => panic!("通道级开启后未提及应忽略"),
        }
    }

    /// 身份管控：开启后未绑定拒绝并给 /pair 指引；绑定后放行
    #[tokio::test]
    async fn 身份管控_未绑定拒绝_绑定放行() {
        let core = test_core();
        let mut cfg = (*core.config()).clone();
        cfg.identity.identity_required = true;
        core.apply_config(cfg).unwrap();
        let ch = chan(None);
        let denied = admit(&core, &ch, &ctx("帮我做事", "u9", false, false)).await;
        match denied {
            GateDecision::Deny(msg) => assert!(msg.contains("/pair"), "拒绝应含配对指引: {msg}"),
            _ => panic!("未绑定应拒绝"),
        }
        // 绑定后放行
        core.save_identity(&UserIdentity {
            id: "tg-main:u9".into(),
            channel: "tg-main".into(),
            external_id: "u9".into(),
            display_name: "用户9".into(),
            role: "member".into(),
            paired_at: exm_core::types::now_iso(),
            note: String::new(),
        })
        .unwrap();
        match admit(&core, &ch, &ctx("帮我做事", "u9", false, false)).await {
            GateDecision::Allow => {}
            _ => panic!("已绑定应放行"),
        }
    }

    /// 管理者账号清单：身份管控开启时，清单命中的外部用户免 /pair 绑定直接放行。
    /// 判权跨通道匹配（人在任一通道的管理者清单即凭证）——清单落在另一通道上同样生效
    #[tokio::test]
    async fn 身份管控_管理者清单豁免() {
        let core = test_core();
        let mut cfg = (*core.config()).clone();
        cfg.identity.identity_required = true;
        core.apply_config(cfg).unwrap();
        let ch = chan(None);
        // 管理者清单挂在另一个通道（napcat）上：判权跟人走，不跟执行通道走
        let holder = Channel {
            id: "nap-main".into(),
            kind: "napcat".into(),
            enabled: true,
            admins: vec!["10086".into(), " 2515681159 ".into()],
            ..chan(None)
        };
        save_channels(&core, &[ch.clone(), holder]).unwrap();
        // 清单命中（含空白容差）：放行，不要求绑定
        for uid in ["10086", "2515681159"] {
            match admit(&core, &ch, &ctx("帮我做事", uid, false, false)).await {
                GateDecision::Allow => {}
                GateDecision::Ignore => panic!("管理员 {uid} 应豁免放行，却被忽略"),
                GateDecision::Deny(m) => panic!("管理员 {uid} 应豁免放行，却被拒绝：{m}"),
            }
        }
        // 清单未命中且未绑定：仍拒绝（给配对指引）
        match admit(&core, &ch, &ctx("帮我做事", "u9", false, false)).await {
            GateDecision::Deny(msg) => assert!(msg.contains("/pair"), "拒绝应含配对指引: {msg}"),
            _ => panic!("非管理员未绑定应拒绝"),
        }
    }

    /// /pair 兑换：有效码绑定成功（单次有效）；过期码拒绝；重复使用拒绝
    #[tokio::test]
    async fn 配对码_兑换流转() {
        let core = test_core();
        let ch = chan(None);
        let code = core.issue_pairing_code("测试", None, 600).unwrap();

        let ok = admit(
            &core,
            &ch,
            &ctx(&format!("/pair {}", code.code), "u7", false, false),
        )
        .await;
        match ok {
            GateDecision::Deny(msg) => assert!(msg.contains("绑定成功"), "{msg}"),
            _ => panic!("/pair 有效码应回复绑定成功"),
        }
        assert!(core.identity_of("tg-main", "u7").is_some(), "兑换后应落库");

        // 已用：再次兑换拒绝
        let reuse = admit(
            &core,
            &ch,
            &ctx(&format!("/pair {}", code.code), "u8", false, false),
        )
        .await;
        match reuse {
            GateDecision::Deny(msg) => {
                assert!(msg.contains("无效") || msg.contains("已被使用"), "{msg}")
            }
            _ => panic!("/pair 已用码应拒绝"),
        }

        // 过期：手工插入一张已过期码
        let expired = PairingCode {
            code: "expired1".into(),
            note: String::new(),
            channel_platform: None,
            created_by: "console".into(),
            created_at: exm_core::types::now_iso(),
            expires_at_ms: exm_core::types::now_ms() - 1000,
            role: String::new(),
        };
        core.save_pairing_code(&expired).unwrap();
        let late = admit(&core, &ch, &ctx("/pair expired1", "u6", false, false)).await;
        match late {
            GateDecision::Deny(msg) => assert!(msg.contains("过期"), "{msg}"),
            _ => panic!("/pair 过期码应拒绝"),
        }
    }

    /// 过期自动清理：未兑换的过期码被周期任务清掉，有效码保留
    #[tokio::test]
    async fn 配对码_过期自动清理() {
        let core = test_core();
        let live = core.issue_pairing_code("待用", None, 600).unwrap();
        let expired = PairingCode {
            code: "stale1".into(),
            note: String::new(),
            channel_platform: None,
            created_by: "console".into(),
            created_at: exm_core::types::now_iso(),
            expires_at_ms: exm_core::types::now_ms() - 1000,
            role: String::new(),
        };
        core.save_pairing_code(&expired).unwrap();

        let purged = core.purge_expired_pairing_codes();
        assert_eq!(purged, 1, "应恰好清掉一张过期码");
        let codes = core.pairing_codes();
        assert!(codes.iter().any(|c| c.code == live.code), "有效码不得误删");
        assert!(!codes.iter().any(|c| c.code == "stale1"), "过期码应已删除");
        // 幂等：无过期码时再跑返回 0
        assert_eq!(core.purge_expired_pairing_codes(), 0);
    }

    /// 平台限定码：其他平台提交无效
    #[tokio::test]
    async fn 配对码_平台限定() {
        let core = test_core();
        let code = core
            .issue_pairing_code("仅 discord", Some("discord-main"), 600)
            .unwrap();
        let ch = Channel {
            id: "tg-main".into(),
            ..chan(None)
        };
        let wrong = admit(
            &core,
            &ch,
            &ctx(&format!("/pair {}", code.code), "u5", false, false),
        )
        .await;
        match wrong {
            GateDecision::Deny(msg) => assert!(msg.contains("discord-main"), "{msg}"),
            _ => panic!("平台不匹配应拒绝"),
        }
        assert!(core.identity_of("tg-main", "u5").is_none());
    }
}
