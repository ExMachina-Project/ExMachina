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
            mention_reply: None,
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
            mention_reply: None,
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
            mention_reply: None,
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
            mention_reply: None,
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
            mention_reply: None,
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
            mention_reply: None,
            capability: "computer".into(),
            memory_isolation: false,
            created_at: String::new(),
        };
        platform::save_channels(&core, &[ch.clone()]).unwrap();
        let r = tick(&core, &ch).await;
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
            mention_reply: None,
            capability: "computer".into(),
            memory_isolation: false,
            created_at: String::new(),
        };
        platform::save_channels(&core, &[ch.clone()]).unwrap();
        let r = tick(&core, &ch).await.unwrap();
        assert!(r.contains("主动观看已关闭"), "{r}");
    }
}

#[cfg(test)]
mod watch_switch_tests {
    use super::*;
    use exm_core::tools::DynamicToolExec;
    use serde_json::json;
    use std::future::Future;
    use std::task::Poll;

    fn core() -> Arc<Core> {
        let dir = std::env::temp_dir().join(format!("exm-watch-switch-{}", exm_core::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        cfg.security.approval_wait_secs = 0;
        let core = Core::with_config(cfg).unwrap();
        let agent = serde_json::from_value(json!({
            "name": "Mock 观众", "identifier": "machina", "description": "local fixture",
            "domain": "test", "tier": "unit"
        })).unwrap();
        core.registry().upsert_single(agent, Some("本地测试，只产出观感".into())).unwrap();
        core.registry().set_active_single(Some("machina")).unwrap();
        Arc::new(core)
    }

    fn channel(kind: &str, watch: bool, comment: bool, reply: bool) -> Channel {
        serde_json::from_value(json!({
            "id": format!("watch-switch-{kind}"),
            "type": kind,
            "enabled": true,
            "createdAt": "",
            "config": {
                "watch": watch.to_string(),
                "comment": comment.to_string(),
                "reply": reply.to_string(),
                "dryRun": "true"
            }
        })).unwrap()
    }

    fn exec(core: &Arc<Core>, ch: &Channel, trigger: WatchTrigger, name: &str) -> DynamicToolExec {
        let injections = match trigger {
            WatchTrigger::Automatic => tools::build_watch_injections(core, ch),
            WatchTrigger::OnDemand => tools::build_injections(core, ch),
        };
        injections.into_iter().find(|(spec, _)| spec.name == name).unwrap().1
    }

    #[test]
    fn watch_false不被评论回复开关重新开启() {
        for kind in ["bilibili", "youtube"] {
            for (comment, reply) in [(false, false), (true, false), (false, true), (true, true)] {
                let ch = channel(kind, false, comment, reply);
                let s = Settings::from_channel(&ch);
                assert!(!s.watch);
                assert_eq!(s.comment, comment);
                assert_eq!(s.reply, reply);
            }
        }
    }

    #[tokio::test]
    async fn 关闭观看不调度不建会话且保留互动配置() {
        let core = core();
        for (comment, reply) in [(false, false), (true, false), (false, true), (true, true)] {
            let channels: Vec<_> = ["bilibili", "youtube"].map(|k| channel(k, false, comment, reply)).into();
            platform::save_channels(&core, &channels).unwrap();
            assert!(watch_channels(&core).is_empty(), "不应创建自动 runner");
            // YouTube 未配 apiKey，即便回归也只在本地报错，绝不会请求外部平台。
            let note = tick(&core, &channels[1]).await.unwrap();
            assert!(note.contains("主动观看已关闭"), "{note}");
            assert!(ask_agent(&core, &channels[0], "不应入队").await.is_err());
            assert!(core.list_sessions().unwrap().is_empty());
            assert!(!state_path(&core, &channels[1].id).exists());
            let saved = platform::load_channels(&core);
            assert_eq!(Settings::from_channel(&saved[0]).comment, comment);
            assert_eq!(Settings::from_channel(&saved[0]).reply, reply);
        }
    }

    #[tokio::test]
    async fn 旧通道快照与已排队工具在关闭后拒绝执行() {
        let core = core();
        let old = channel("youtube", true, true, true);
        platform::save_channels(&core, &[old.clone()]).unwrap();
        assert_eq!(watch_channels(&core).len(), 1);
        let sid = "watch-switch-queued";
        tools::register_act_gate(sid, tools::ActGate::from_settings(&Settings::from_channel(&old), 0));
        let act = exec(&core, &old, WatchTrigger::Automatic, "video__act");
        let first = act("video", sid, json!({"videoId": "local", "comment": "测试短评"})).await;
        assert!(first.ok && first.text.contains("dry-run"), "{}", first.text);
        let queued = act("video", sid, json!({"videoId": "local", "comment": "待执行短评"}));
        let mut latest = old.clone();
        latest.config.insert("watch".into(), "false".into());
        platform::save_channels(&core, &[latest]).unwrap();
        assert!(watch_channels(&core).is_empty());
        assert!(tick(&core, &old).await.unwrap().contains("主动观看已关闭"));
        assert!(ask_agent(&core, &old, "旧快照不得启动轮次").await.is_err());
        let denied = queued.await;
        assert!(!denied.ok && denied.text.contains("主动观看已关闭"), "{}", denied.text);
        assert_eq!(tools::act_gate_count(sid), 1, "不得消耗配额或执行动作");
        tools::drop_act_gate(sid);
        // 闸门清理后也不能把滞留的自动闭包误当成点播放行。
        let denied = act("video", sid, json!({"videoId": "local", "comment": "禁止回放"})).await;
        assert!(!denied.ok && denied.text.contains("主动观看已关闭"), "{}", denied.text);
        assert_eq!(tools::session_state(sid).lock().acted, 1);
        tools::drop_session(sid);
    }

    #[tokio::test]
    async fn 旧闭包立即检查通道停用删除平台变化和插件停用() {
        let core = core();
        let old = channel("bilibili", true, false, false);
        let automatic = exec(&core, &old, WatchTrigger::Automatic, "video__summary");
        let manual = exec(&core, &old, WatchTrigger::OnDemand, "video__summary");
        let mut stopped = old.clone();
        stopped.enabled = false;
        let mut changed = old.clone();
        changed.kind = "youtube".into();
        for (channels, reason) in [(vec![stopped], "停用"), (vec![], "删除"), (vec![changed], "平台已变更")] {
            platform::save_channels(&core, &channels).unwrap();
            assert!(watch_channels(&core).values().all(|ch| ch.kind != old.kind));
            for tool in [&automatic, &manual] {
                // 不传 videoId，任何意外放行也只会触发本地参数校验，不触网。
                let result = tool("video", "watch-switch-stopped", json!({})).await;
                assert!(!result.ok && result.text.contains(reason), "{}", result.text);
            }
        }
        platform::save_channels(&core, &[old]).unwrap();
        core.plugins().register_builtin_late(Box::new(crate::plugins::VideoPlugin::new()));
        assert!(core.plugins().set_enabled("video", false));
        assert!(watch_channels(&core).is_empty());
        let result = automatic("video", "watch-switch-stopped", json!({})).await;
        assert!(!result.ok && result.text.contains("视频插件已停用"), "{}", result.text);
        assert!(core.plugins().set_enabled("video", true));
        let mut cfg = (*core.config()).clone();
        cfg.plugins.state.insert("video".into(), false);
        core.apply_config(cfg).unwrap();
        assert!(watch_channels(&core).is_empty());
        let result = manual("video", "watch-switch-stopped", json!({})).await;
        assert!(!result.ok && result.text.contains("视频插件已停用"), "{}", result.text);
        tools::drop_session("watch-switch-stopped");
    }

    #[tokio::test]
    async fn 关闭主动观看仍允许显式点播与独立评论回复() {
        let core = core();
        let old = channel("youtube", true, true, true);
        let summary = exec(&core, &old, WatchTrigger::OnDemand, "video__summary");
        let act = exec(&core, &old, WatchTrigger::OnDemand, "video__act");
        let mut latest = old.clone();
        latest.config.insert("watch".into(), "false".into());
        platform::save_channels(&core, &[latest.clone()]).unwrap();
        let result = summary("machina", "watch-switch-manual", json!({})).await;
        assert!(!result.ok && result.text.contains("缺少 videoId"), "观看应通过开关闸门：{}", result.text);
        // 凭据校验是本地执行；到达 apiKey 检查证明已进入手动观看而非被 watch 拦截。
        let result = summary("machina", "watch-switch-manual", json!({"videoId": "local"})).await;
        assert!(!result.ok && result.text.contains("apiKey"), "{}", result.text);
        let sid = "watch-switch-manual";
        let result = act("machina", sid, json!({"videoId": "local", "comment": "手动短评"})).await;
        assert!(result.ok && result.text.contains("dry-run"), "{}", result.text);
        tools::session_state(sid).lock().seen_comments.insert("local-comment".into());
        let result = act("machina", sid, json!({"videoId": "local", "replyText": "手动回复", "replyTo": "local-comment"})).await;
        assert!(result.ok && result.text.contains("dry-run"), "{}", result.text);
        latest.config.insert("comment".into(), "false".into());
        platform::save_channels(&core, &[latest]).unwrap();
        let result = act("machina", sid, json!({"videoId": "local", "comment": "已关闭评论"})).await;
        assert!(result.text.contains("未开启评论"), "旧点播闭包也必须读最新评论开关：{}", result.text);
        assert_eq!(tools::session_state(sid).lock().acted, 2);
        tools::drop_session(sid);
    }

    #[tokio::test]
    async fn 自动审批在观看关闭后不代发且手动审批不受观看开关限制() {
        let core = core();
        let mut ch = channel("youtube", true, true, false);
        ch.config.insert("dryRun".into(), "false".into());
        ch.config.insert("approval".into(), "always".into());
        platform::save_channels(&core, &[ch.clone()]).unwrap();
        let act = exec(&core, &ch, WatchTrigger::Automatic, "video__act");
        let result = act("video", "watch-switch-approval", json!({"videoId": "local", "comment": "待审批短评"})).await;
        assert!(result.ok && result.text.contains("人工审批"), "{}", result.text);
        let pending = core.approval_list(Some("pending"), 10).unwrap();
        assert_eq!(pending.len(), 1);
        let payload = pending[0].command.strip_prefix("[video] ").unwrap();
        let mut manual: Value = serde_json::from_str(payload).unwrap();
        assert_eq!(manual["automatic"], true);
        ch.config.insert("watch".into(), "false".into());
        platform::save_channels(&core, &[ch]).unwrap();
        let error = tools::execute_act_payload(&core, payload).await.unwrap_err();
        assert!(error.to_string().contains("主动观看已关闭"), "{error}");
        manual["automatic"] = json!(false);
        // 无 API 密钥，只验证手动审批能通过 watch 闸门，绝不实际发布。
        let error = tools::execute_act_payload(&core, &manual.to_string()).await.unwrap_err();
        assert!(error.to_string().contains("apiKey"), "{error}");
        assert_eq!(VideoState::load(&core, "watch-switch-youtube").actions_today, 0);
        tools::drop_session("watch-switch-approval");
    }

    #[tokio::test]
    async fn 升级前自动审批也不能绕过观看关闭而点播审批保留() {
        let core = core();
        let mut ch = channel("youtube", false, true, false);
        ch.config.insert("dryRun".into(), "false".into());
        platform::save_channels(&core, &[ch.clone()]).unwrap();
        for (title, reason) in [(format!("channel:{}:video", ch.id), "主动观看已关闭"), ("手动点播会话".into(), "apiKey")] {
            let session = core.create_session(&title).unwrap();
            let payload = json!({"channelId": ch.id, "videoId": title, "text": "升级前待审短评", "replyTo": null}).to_string();
            let request = exm_core::types::ApprovalRequest {
                id: exm_core::types::new_id(),
                session_id: session.id,
                node_id: None,
                agent_id: "video".into(),
                command: format!("[video] {payload}"),
                status: "pending".into(),
                result: None,
                created_at: exm_core::types::now_iso(),
                decided_at: None,
                origin_user: None,
                origin_role: None,
                meta_json: None,
            };
            core.store.add_approval(&request).unwrap();
            let error = tools::execute_act_payload(&core, &payload).await.unwrap_err();
            assert!(error.to_string().contains(reason), "{error}");
        }
    }

    #[tokio::test]
    async fn 等待中的自动工具在恢复前校验新配置且不继续后续动作() {
        let core = core();
        let old = channel("youtube", true, true, false);
        platform::save_channels(&core, &[old.clone()]).unwrap();
        let resume = tokio::sync::Notify::new();
        let continued = std::sync::atomic::AtomicBool::new(false);
        let mut work = Box::pin(with_live_guard(&core, &old, WatchTrigger::Automatic, async {
            resume.notified().await;
            continued.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }));
        std::future::poll_fn(|cx| {
            assert!(work.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        }).await;
        let mut latest = old.clone();
        latest.config.insert("watch".into(), "false".into());
        platform::save_channels(&core, &[latest]).unwrap();
        resume.notify_one();
        let error = work.await.unwrap_err();
        assert!(error.to_string().contains("主动观看已关闭"), "{error}");
        assert!(!continued.load(std::sync::atomic::Ordering::SeqCst));
    }

    async fn poll_until_running<F: Future>(core: &Core, future: &mut std::pin::Pin<Box<F>>) -> String {
        std::future::poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending(), "Mock 轮次应在流式输出处挂起");
            match core.list_sessions().unwrap().into_iter().find(|s| exm_core::round_trace::is_running(&s.id)) {
                Some(s) => Poll::Ready(s.id),
                None => Poll::Pending,
            }
        }).await
    }

    #[tokio::test]
    async fn 取消观看future同步清理执行任务闸门和注入工具() {
        let core = core();
        let ch = channel("youtube", true, false, false);
        platform::save_channels(&core, &[ch.clone()]).unwrap();
        let mut future = Box::pin(ask_agent(&core, &ch, "本地 Mock 观看"));
        let sid = poll_until_running(&core, &mut future).await;
        let gateway = core.orchestrator().tools.clone();
        assert!(!gateway.session_tool_specs(&sid).is_empty());
        assert!(gateway.session_step_budget(&sid).is_some());
        drop(future);
        assert!(!exm_core::round_trace::is_running(&sid), "不得留下游离观看任务");
        assert!(gateway.session_tool_specs(&sid).is_empty());
        assert_eq!(gateway.session_step_budget(&sid), None);
        assert_eq!(tools::act_gate_count(&sid), 0);
    }

    #[tokio::test]
    async fn 运行中关闭观看在下个安全边界停止并清理() {
        let core = core();
        let mut ch = channel("youtube", true, false, false);
        platform::save_channels(&core, &[ch.clone()]).unwrap();
        let old = ch.clone();
        let mut future = Box::pin(ask_agent(&core, &old, "本地 Mock 观看"));
        let sid = poll_until_running(&core, &mut future).await;
        ch.config.insert("watch".into(), "false".into());
        platform::save_channels(&core, &[ch]).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(3), future).await.unwrap();
        assert!(result.err().unwrap().to_string().contains("主动观看已关闭"));
        assert!(!exm_core::round_trace::is_running(&sid));
        assert!(core.orchestrator().tools.session_tool_specs(&sid).is_empty());
        assert_eq!(core.orchestrator().tools.session_step_budget(&sid), None);
    }

    #[tokio::test]
    async fn 会话锁排队期间关闭观看不得落库或启动自动轮次() {
        let core = core();
        let ch = channel("youtube", true, false, false);
        platform::save_channels(&core, &[ch.clone()]).unwrap();
        let (run, _) = ChannelRun::begin(&core, &ch, "video").await.unwrap();
        let sid = run.session_id.clone();
        let mut blocker = Box::pin(core.chat(&sid, "手动 Mock 轮次暂占会话锁"));
        std::future::poll_fn(|cx| {
            assert!(blocker.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        }).await;
        assert!(exm_core::round_trace::is_running(&sid));
        let mut automatic = Box::pin(ask_agent(&core, &ch, "关闭后绝不能执行的自动轮次"));
        std::future::poll_fn(|cx| {
            assert!(automatic.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        }).await;
        let mut latest = ch.clone();
        latest.config.insert("watch".into(), "false".into());
        platform::save_channels(&core, &[latest]).unwrap();
        // 先释放锁，再立即恢复排队 future，不依赖一秒周期检查恰好先到达。
        blocker.await.unwrap();
        let error = automatic.await.err().unwrap();
        assert!(error.to_string().contains("主动观看已关闭"), "{error}");
        let messages = core.store.list_messages(&sid, 100).unwrap();
        assert!(!messages.iter().any(|m| m.agent_id.as_deref() == Some("auto:video")));
        assert!(!messages.iter().flat_map(|m| &m.statements).any(|s| s.text.contains("关闭后绝不能执行的自动轮次")));
        assert!(!exm_core::round_trace::is_running(&sid));
        assert!(core.orchestrator().tools.session_tool_specs(&sid).is_empty());
    }
}
