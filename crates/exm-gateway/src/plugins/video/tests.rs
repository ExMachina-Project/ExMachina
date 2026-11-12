//! video 域测试（自 mod.rs 迁出，纯搬移；各测试 mod 原样保留）。

use super::*;

#[cfg(test)]
#[allow(clippy::module_inception)] // 文件名即 tests.rs，内层 mod 保持搬移前结构
mod tests {
    use super::*;

    #[test]
    fn 全局点播注册_随通道启停同步() {
        let dir = std::env::temp_dir().join(format!("exm-video-od-{}", exm_core::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        let core = Arc::new(exm_core::Core::with_config(cfg).expect("创建 Core 失败"));

        let mk = |enabled: bool| Channel {
            id: "bili-od".into(),
            kind: "bilibili".into(),
            enabled,
            group: None,
            allowed_chats: vec![],
            admins: vec![],
            account: None,
            secret: None,
            reply_webhook: None,
            token: None,
            config: {
                let mut m = std::collections::BTreeMap::new();
                m.insert("sessdata".into(), "x".into());
                m.insert("biliJct".into(), "y".into());
                m
            },
            group_gate: None,
            proactive_reply: None,
            proactive_rate: None,
            capability: "computer".into(),
            memory_isolation: false,
            created_at: String::new(),
        };

        // 初始：无通道 → 无全局工具
        tools::reconcile_ondemand(&core);
        assert!(core
            .orchestrator()
            .tools
            .dynamic_tool("video__summary")
            .is_none());

        // 启用 → 注册（含 act）
        crate::platform::save_channels(&core, &[mk(true)]).unwrap();
        tools::reconcile_ondemand(&core);
        for n in ["video__summary", "video__comments", "video__act"] {
            assert!(
                core.orchestrator().tools.dynamic_tool(n).is_some(),
                "{n} 应已注册"
            );
        }
        // 点播纪律段随注册注入（评论边界 + dry-run 转述口径），与工具面同生命周期
        for n in ["video__summary", "video__act"] {
            let (s, _) = core.orchestrator().tools.dynamic_tool(n).unwrap();
            assert!(
                s.description.contains("评论必须经 video__act"),
                "{n} 描述应含点播纪律段：{}",
                s.description
            );
            assert!(
                s.description.contains("未实际发送"),
                "{n} 描述应含 dry-run 转述口径"
            );
        }
        // 全体可见（agents 空列表）
        let specs = core.orchestrator().tools.custom_specs_for("machina");
        assert!(specs.iter().any(|s| s.name == "video__summary"));

        // 停用 → 注销回收
        crate::platform::save_channels(&core, &[mk(false)]).unwrap();
        tools::reconcile_ondemand(&core);
        assert!(core
            .orchestrator()
            .tools
            .dynamic_tool("video__summary")
            .is_none());
    }

    #[test]
    fn 配对码_预设角色兑换() {
        let dir = std::env::temp_dir().join(format!("exm-pair-role-{}", exm_core::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        let core = exm_core::Core::with_config(cfg).expect("创建 Core 失败");

        // 管理者配对码：兑换即落 admin
        let admin_code = core
            .issue_pairing_code_role("管理员", None, 600, "admin")
            .unwrap();
        let idn = core
            .redeem_pairing(&admin_code.code, "tg-main", "u1", "张三", "member")
            .unwrap();
        assert_eq!(idn.role, "admin", "预设角色应覆盖默认");

        // 普通码（旧签名/无角色）：落 default_role
        let member_code = core.issue_pairing_code("成员", None, 600).unwrap();
        assert_eq!(member_code.role, "member");
        let idn2 = core
            .redeem_pairing(&member_code.code, "tg-main", "u2", "李四", "member")
            .unwrap();
        assert_eq!(idn2.role, "member");

        // 存量码无角色字段（手工造码）：回落 default_role
        let legacy = exm_core::types::PairingCode {
            code: "legacy001".into(),
            note: String::new(),
            channel_platform: None,
            created_by: "console".into(),
            created_at: String::new(),
            expires_at_ms: exm_core::types::now_ms() + 600_000,
            role: String::new(),
        };
        core.save_pairing_code(&legacy).unwrap();
        let idn3 = core
            .redeem_pairing("legacy001", "tg-main", "u3", "王五", "member")
            .unwrap();
        assert_eq!(idn3.role, "member");
    }

    #[test]
    fn 配置解析_默认安全档() {
        let ch = Channel {
            id: "bili".into(),
            kind: "bilibili".into(),
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
            capability: "chat".into(),
            memory_isolation: true,
            created_at: String::new(),
        };
        let s = Settings::from_channel(&ch);
        assert!(s.watch);
        assert!(!s.comment);
        assert!(!s.reply);
        assert!(s.dry_run, "默认必须 dry-run");
        assert_eq!(s.max_actions, 1);
        assert_eq!(ch.memory_partition("g"), Some("g::channel:bili".into()));
        assert_eq!(ch.tool_scope(), exm_core::types::ToolScope::Chat);
    }

    #[test]
    fn 任务框架prompt_候选清单与产出要求() {
        let ch = Channel {
            id: "bili".into(),
            kind: "bilibili".into(),
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
            capability: "chat".into(),
            memory_isolation: true,
            created_at: String::new(),
        };
        let s = Settings::from_channel(&ch);
        let cands = vec![VideoItem {
            platform: "bilibili".into(),
            id: "BV1xx".into(),
            aid: Some(1),
            title: "Rust 教程".into(),
            description: String::new(),
            author: "up主".into(),
            views: 1234,
        }];
        let p = build_prompt(&s, &cands);
        // 候选清单带标识（智能体可据此调工具）
        assert!(p.contains("BV1xx"), "候选 id 应入 prompt：{p}");
        assert!(p.contains("Rust 教程"));
        // 任务框架要素：自主观看 + 产出要求 + 跳过自由
        assert!(p.contains("video__"), "应说明工具用法：{p}");
        assert!(p.contains("观感"));
        assert!(p.contains("跳过"));
        // 未开启互动时不引导 act
        assert!(!s.comment && !s.reply);
        assert!(p.contains("不要调用 video__act"));
        // 不再预拼材料（旧管道特征文本不出现）
        assert!(!p.contains("【视频资料】"));
        assert!(!p.contains("评论区摘录"));
    }

    #[test]
    fn 任务框架prompt_互动开启与dry_run标注() {
        let ch = Channel {
            id: "bili".into(),
            kind: "bilibili".into(),
            enabled: true,
            group: None,
            allowed_chats: vec![],
            admins: vec![],
            account: None,
            secret: None,
            reply_webhook: None,
            token: None,
            config: [
                ("comment".to_string(), "true".to_string()),
                ("dryRun".to_string(), "true".to_string()),
                ("commentStyle".to_string(), "口语化、不用书面语".to_string()),
                ("interests".to_string(), "技术类视频".to_string()),
            ]
            .into_iter()
            .collect(),
            group_gate: None,
            proactive_reply: None,
            proactive_rate: None,
            capability: "chat".into(),
            memory_isolation: true,
            created_at: String::new(),
        };
        let s = Settings::from_channel(&ch);
        assert!(s.comment && s.dry_run);
        let p = build_prompt(&s, &[]);
        assert!(p.contains("video__act"));
        assert!(p.contains("dry-run"), "应提示 dry-run 模式：{p}");
        assert!(p.contains("技术类视频"), "内容倾向应注入：{p}");
        assert!(p.contains("口语化、不用书面语"), "评论风格应注入：{p}");
    }

    #[test]
    fn 标题去标签与文本清洗() {
        assert_eq!(
            strip_html("A <em class=\"keyword\">B站</em> 视频"),
            "A B站 视频"
        );
        assert_eq!(clean_text("  hello\n\nworld  ", 20), "hello world");
        assert_eq!(clean_text("abcdef", 3), "abc");
    }

    #[test]
    fn 关键词大小写不敏感() {
        let v = VideoItem {
            platform: "bilibili".into(),
            id: "BV1".into(),
            aid: Some(1),
            title: "Rust 异步运行时".into(),
            description: String::new(),
            author: "up".into(),
            views: 0,
        };
        assert!(matches_keywords(&v, &["rust".into()]));
        assert!(!matches_keywords(&v, &["python".into()]));
    }
}

#[cfg(test)]
mod interest_tests {
    use super::*;

    #[test]
    fn test_interest_search_terms() {
        let terms = interest_search_terms("只查看AI相关内容，优先二创其次是技术类。");
        println!("提取结果: {terms:?}");
        assert!(!terms.is_empty());
        assert!(terms.iter().any(|t| t.contains("AI")));
    }
}

#[cfg(test)]
mod comment_quality_tests {
    use super::*;

    /// AI 味套话检测：命中清单返回首处，干净短评不误报
    #[test]
    fn 套话检测_命中与误报() {
        assert_eq!(detect_cliche("总的来说这个视频不错"), Some("总的来说"));
        assert_eq!(detect_cliche("不得不说 UP 主真的用心了"), Some("不得不说"));
        assert_eq!(detect_cliche("首先…其次…最后…"), Some("首先"));
        assert_eq!(detect_cliche("太感动了，泪目"), Some("太感动"));
        // 真人短评形态：具体、口语、无套话
        assert_eq!(detect_cliche("3 分 20 秒那个转场直接给我看愣了"), None);
        assert_eq!(detect_cliche("这就是我不敢养猫的原因"), None);
        assert_eq!(
            detect_cliche("最后"),
            Some("最后"),
            "「最后」单独出现也命中（总结腔起手）"
        );
    }

    /// 观看提示注入评论规范：字数预算 + 反 AI 味负面清单 + 人设优先
    #[test]
    fn 观看提示_评论规范注入() {
        let s = Settings {
            comment: true,
            reply: false,
            max_comment_chars: 80,
            ..Settings::from_channel(&Channel {
                id: "bili-q".into(),
                kind: "bilibili".into(),
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
                capability: "computer".into(),
                memory_isolation: false,
                created_at: String::new(),
            })
        };
        let item = VideoItem {
            platform: "bilibili".into(),
            id: "BV1".into(),
            aid: None,
            title: "测试视频".into(),
            description: String::new(),
            author: "up".into(),
            views: 0,
        };
        let prompt = build_prompt(&s, &[item]);
        assert!(prompt.contains("80 字以内"), "字数预算应注入：{prompt}");
        assert!(prompt.contains("禁止 AI 腔"), "反 AI 味清单应注入");
        assert!(prompt.contains("人设优先"), "人设优先应注入");
        assert!(prompt.contains("你是谁就怎么说话"), "人设延续声明应注入");
    }
}

#[cfg(test)]
mod watch_loop_tests {
    use super::*;

    fn item(id: &str) -> VideoItem {
        VideoItem {
            platform: "bilibili".into(),
            id: id.into(),
            aid: None,
            title: format!("视频{id}"),
            description: String::new(),
            author: "up".into(),
            views: 0,
        }
    }

    /// 已看去重沿用 VideoState：seen/mark_video 环路 + 上限滚动
    #[test]
    fn 候选去重_video_state环路() {
        let mut st = VideoState::default();
        assert!(!st.seen(&video_key(&item("BV1"))));
        st.mark_video(&video_key(&item("BV1")));
        assert!(st.seen(&video_key(&item("BV1"))), "标记后应已看");
        // 上限滚动：超过 500 条自动淘汰最旧
        for i in 0..510 {
            st.mark_video(&format!("bilibili:BV{i}"));
        }
        assert!(st.seen("bilibili:BV509"));
        assert!(!st.seen("bilibili:BV1"), "超限后最旧应被淘汰");
    }

    /// 跳过记录：收束文本承载跳过理由（状态面板呈现），WatchSession.skipped
    /// 记录系统面跳过（act 拒绝等）
    #[test]
    fn 跳过理由_会话状态与收束文本() {
        let sid = "skip-test";
        let st = tools::session_state(sid);
        st.lock()
            .skipped
            .push("候选均不感兴趣：题材重复".into());
        let skipped = st.lock().skipped.clone();
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].contains("题材重复"));
        tools::drop_session(sid);
    }
}

#[cfg(test)]
mod tick_smoke_tests {
    use super::*;

    fn core(tag: &str) -> Arc<Core> {
        let dir = std::env::temp_dir().join(format!("exm-tick-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        Arc::new(Core::with_config(cfg).expect("Core 创建失败"))
    }

    /// 无凭据轮次：tick 返回结构化错误（不 panic）；错误说明含凭据指引
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 冒烟_无凭据结构化降级() {
        let core = core("nocred");
        let ch = Channel {
            id: "yt-smoke".into(),
            kind: "youtube".into(),
            enabled: true,
            group: None,
            allowed_chats: vec![],
            admins: vec![],
            account: None,
            secret: None,
            reply_webhook: None,
            token: None,
            config: [("watch".to_string(), "true".to_string())]
                .into_iter()
                .collect(),
            group_gate: None,
            proactive_reply: None,
            proactive_rate: None,
            capability: "computer".into(),
            memory_isolation: false,
            created_at: String::new(),
        };
        let s = Settings::from_channel(&ch);
        let r = tick(&core, &ch, &s).await;
        assert!(r.is_err(), "无 apiKey 应失败：{r:?}");
        let msg = r.unwrap_err().to_string();
        assert!(msg.contains("apiKey"), "错误应指向凭据缺失：{msg}");
    }

    /// 互动全关：tick 直接短路返回（不触网）
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 冒烟_全关短路() {
        let core = core("off");
        let ch = Channel {
            id: "bili-off".into(),
            kind: "bilibili".into(),
            enabled: true,
            group: None,
            allowed_chats: vec![],
            admins: vec![],
            account: None,
            secret: None,
            reply_webhook: None,
            token: None,
            config: [
                ("watch".to_string(), "false".to_string()),
                ("comment".to_string(), "false".to_string()),
                ("reply".to_string(), "false".to_string()),
            ]
            .into_iter()
            .collect(),
            group_gate: None,
            proactive_reply: None,
            proactive_rate: None,
            capability: "computer".into(),
            memory_isolation: false,
            created_at: String::new(),
        };
        let s = Settings::from_channel(&ch);
        let r = tick(&core, &ch, &s).await.unwrap();
        assert!(r.contains("均已关闭"), "{r}");
    }
}
