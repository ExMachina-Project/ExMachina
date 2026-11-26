//! gateway/platform/tests_channel_workflow —— capability-parity 4.6：
//! 聊天通道 /goal 与 /spec 命令族的权限、回执与「不交给模型猜测」边界。
//!
//! 锚定验收：
//! ① 命令在 gate 拦截直答（Deny 回执），绝不进 chat 链路（命令不是对话）；
//! ② 读面普通成员可用，写面仅管理员（未 @ 静默忽略）；
//! ③ /goal /spec /plan 同会话共享 workflow 状态（golden 关系）；
//! ④ goal/spec 操作不触碰 cron、todo 看板与 task_queue（互不相干）；
//! ⑤ complete 必须带真实验证证据。
#[cfg(test)]
use super::*;

mod channel_workflow_tests {
    use super::*;

    /// 既有测试用的 Core 夹具（不持有目录守卫——保持原行为）。
    ///
    /// 注意：不要写成 `wf_core_guarded().0`——那会让守卫在表达式结束时立即 drop，
    /// 运行时要落盘的 workspace_root 当场被删。
    fn wf_core() -> exm_core::Core {
        let dir = std::env::temp_dir().join(format!("exm-wf-ch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        cfg.security.approval_wait_secs = 0;
        exm_core::Core::with_config(cfg).expect("创建 Core 夌败")
    }

    /// 同 [`wf_core`]，但把临时目录交给守卫：持有返回的 `TestDirs` 到测试结束，
    /// 离开作用域即清理（含断言 panic）。`/learn` 等新增测试一律用它。
    fn wf_core_guarded() -> (exm_core::Core, crate::platform::TestDirs) {
        let dir = std::env::temp_dir().join(format!("exm-wf-ch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        cfg.security.approval_wait_secs = 0;
        (
            exm_core::Core::with_config(cfg).expect("创建 Core 失败"),
            crate::platform::TestDirs(vec![dir]),
        )
    }

    fn chan(admins: Vec<&str>) -> Channel {
        Channel {
            id: "wf-ch".into(),
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

    fn setup_flow(core: &exm_core::Core, ch: &Channel, key: &str) -> String {
        let title = format!("channel:{}:{}", ch.id, key);
        let s = core
            .store
            .create_session(&title, "default")
            .expect("建立流会话失败");
        let _ = core.bind_flow_session(&ch.id, key, &s.id);
        s.id
    }

    fn deny_of(d: GateDecision) -> String {
        match d {
            GateDecision::Deny(reply) => reply,
            _ => panic!("期望 Deny 回执，得到其他分支"),
        }
    }

    #[tokio::test]
    async fn goal_全生命周期_管理员回执如实() {
        let core = std::sync::Arc::new(wf_core());
        let ch = chan(vec!["admin-1"]);
        let sid = setup_flow(&core, &ch, "w1");

        // 空态如实
        let r = deny_of(admit(&core, &ch, &ctx("/goal", "admin-1", true, "w1")).await);
        assert!(r.contains("没有 goal"), "{r}");

        // start
        let r = deny_of(
            admit(
                &core,
                &ch,
                &ctx("/goal start 交付解析器", "admin-1", true, "w1"),
            )
            .await,
        );
        assert!(r.contains("active"), "start 回执：{r}");
        assert!(core.goal_get(&sid).unwrap().is_some(), "workflow 状态已建");

        // pause / resume
        let r = deny_of(admit(&core, &ch, &ctx("/goal pause", "admin-1", true, "w1")).await);
        assert!(r.contains("paused"), "{r}");
        let r = deny_of(admit(&core, &ch, &ctx("/goal resume", "admin-1", true, "w1")).await);
        assert!(r.contains("active"), "{r}");

        // complete 空证据被拒（真实验证证据红线）
        let r = deny_of(admit(&core, &ch, &ctx("/goal complete", "admin-1", true, "w1")).await);
        assert!(
            r.contains("用法") || r.contains("验证证据"),
            "空 complete 拒：{r}"
        );
        // 带证据过
        let r = deny_of(
            admit(
                &core,
                &ch,
                &ctx("/goal complete cargo test 全绿", "admin-1", true, "w1"),
            )
            .await,
        );
        assert!(r.contains("completed"), "{r}");
        let g = core.goal_get(&sid).unwrap().unwrap();
        assert_eq!(g.verification.len(), 1, "证据入档");
    }

    #[tokio::test]
    async fn goal_写面权限_非管理员拒未提及静默() {
        let core = std::sync::Arc::new(wf_core());
        let ch = chan(vec!["admin-1"]);
        let _sid = setup_flow(&core, &ch, "w2");

        // @ 场景：拒并说明
        let r = deny_of(admit(&core, &ch, &ctx("/goal start 目标", "u9", true, "w2")).await);
        assert!(r.contains("管理员"), "{r}");
        // 未 @：静默忽略
        assert!(matches!(
            admit(&core, &ch, &ctx("/goal start 目标", "u9", false, "w2")).await,
            GateDecision::Ignore
        ));
        // 读面普通成员可用
        let r = deny_of(admit(&core, &ch, &ctx("/goal", "u9", true, "w2")).await);
        assert!(r.contains("没有 goal"), "{r}");
    }

    #[tokio::test]
    async fn spec_草拟修订清除_不触碰计划清单() {
        let core = std::sync::Arc::new(wf_core());
        let ch = chan(vec!["admin-1"]);
        let sid = setup_flow(&core, &ch, "w3");

        // 关联 goal
        let _ = admit(
            &core,
            &ch,
            &ctx("/goal start 交付解析器", "admin-1", true, "w3"),
        )
        .await;

        // draft
        let r = deny_of(
            admit(
                &core,
                &ch,
                &ctx("/spec draft 实现解析器与错误恢复", "admin-1", true, "w3"),
            )
            .await,
        );
        assert!(r.contains("草拟"), "{r}");
        let spec = core.spec_get(&sid).unwrap().unwrap();
        assert!(spec.goal_id.is_some(), "spec 自动关联当前 goal");

        // edit（修订升版）
        let r = deny_of(
            admit(
                &core,
                &ch,
                &ctx("/spec edit 实现解析器（v2）", "admin-1", true, "w3"),
            )
            .await,
        );
        assert!(r.contains("修订"), "{r}");
        assert!(core.spec_get(&sid).unwrap().unwrap().revision >= 2);

        // 修改 spec 不自动批准 plan：模式位/清单不动、无 plan.approved 事件
        assert!(!exm_core::plan::is_plan_mode(&sid));
        assert!(exm_core::plan::list_items(&sid).is_empty());
        assert!(!core
            .store
            .list_events(&sid)
            .unwrap()
            .iter()
            .any(|e| e["kind"].as_str().or(e["type"].as_str()) == Some("plan.approved")));

        // export 口径说明（聊天不开工作区写）
        let r = deny_of(admit(&core, &ch, &ctx("/spec export a.md", "admin-1", true, "w3")).await);
        assert!(r.contains("CLI"), "导出指引：{r}");

        // clear
        let r = deny_of(admit(&core, &ch, &ctx("/spec clear", "admin-1", true, "w3")).await);
        assert!(r.contains("清除"), "{r}");
        assert!(core.spec_get(&sid).unwrap().is_none());
    }

    /// golden：/goal /spec /plan 同一会话共享 workflow 状态；命令互不串扰，
    /// 且不触碰 cron / todo 看板 / task_queue（互不相干）。
    #[tokio::test]
    async fn goal_spec_plan_同会话共享状态_不混淆cron_todo_queue() {
        let core = std::sync::Arc::new(wf_core());
        let ch = chan(vec!["admin-1"]);
        let sid = setup_flow(&core, &ch, "w4");

        // 三个命令族先后落状态
        let _ = admit(
            &core,
            &ch,
            &ctx("/goal start 交付解析器", "admin-1", true, "w4"),
        )
        .await;
        let _ = admit(
            &core,
            &ch,
            &ctx("/spec draft 实现目标", "admin-1", true, "w4"),
        )
        .await;
        let _ = admit(&core, &ch, &ctx("/plan start", "admin-1", true, "w4")).await;

        // workflow 关联视图（4.5）三层数据同源共享
        let relation = core.workflow_relation(&sid).unwrap();
        let g = relation.goal.as_ref().unwrap();
        let s = relation.spec.as_ref().unwrap();
        assert_eq!(s.goal_id.as_deref(), Some(g.id.as_str()), "spec→goal 关联");
        assert!(relation.plan_mode, "plan 模式位独立于 goal/spec 状态");

        // 互不混淆：goal/spec/plan 命令不产生 cron 项、todo 快照、队列项
        assert!(core.cron.list().unwrap().is_empty(), "cron 不受影响");
        assert!(
            exm_core::todo::list_items(&sid).is_empty(),
            "todo 看板不受影响"
        );
        assert!(
            exm_core::task_queue::list(&sid).is_empty(),
            "任务队列不受影响"
        );

        // 命令不进模型链路：无轮次被触发（命令文本不交给模型猜测）
        let msgs = core.store.list_messages(&sid, 50).unwrap();
        assert!(
            !msgs
                .iter()
                .flat_map(|m| m.statements.iter())
                .any(|st| st.text.contains("/goal") || st.text.contains("/spec")),
            "命令文本不得进入会话消息流：{:?}",
            msgs.iter()
                .flat_map(|m| m.statements.iter().map(|st| st.text.clone()))
                .collect::<Vec<_>>()
        );
    }

    /// 命令在 gate 拦截：即使流里有会话，/goal 也不触发轮次（不进 chat 链路）
    #[tokio::test]
    async fn 命令文本不交给模型_不触发轮次() {
        let core = std::sync::Arc::new(wf_core());
        let ch = chan(vec!["admin-1"]);
        let sid = setup_flow(&core, &ch, "w5");
        let before = core.store.list_messages(&sid, 100).unwrap().len();

        let _ = admit(&core, &ch, &ctx("/goal status", "admin-1", true, "w5")).await;
        let _ = admit(&core, &ch, &ctx("/spec show", "admin-1", true, "w5")).await;
        let _ = admit(&core, &ch, &ctx("/plan status", "admin-1", true, "w5")).await;

        let after = core.store.list_messages(&sid, 100).unwrap().len();
        assert_eq!(before, after, "三个命令零消息落库（gate 直答）");
        assert!(
            !exm_core::round_trace::is_running(&sid),
            "命令不触发运行轮次"
        );
    }
    #[tokio::test]
    async fn context_读面_来源清单与缺失原因() {
        let core = std::sync::Arc::new(wf_core());
        let ch = chan(vec!["admin-1"]);
        let _sid = setup_flow(&core, &ch, "c1");
        // 普通成员读面可用（status 是只读）
        let r = deny_of(admit(&core, &ch, &ctx("/context", "user-9", true, "c1")).await);
        assert!(r.contains("上下文来源"), "清单回执：{r}");
        // refresh 同口径
        let r = deny_of(admit(&core, &ch, &ctx("/context refresh", "user-9", true, "c1")).await);
        assert!(r.contains("上下文来源"), "{r}");
    }

    #[tokio::test]
    async fn context_写面权限_非管理员拒() {
        let core = std::sync::Arc::new(wf_core());
        let ch = chan(vec!["admin-1"]);
        let sid = setup_flow(&core, &ch, "c2");
        // 非管理员（即使 @）：拒绝并说明
        let r = admit(
            &core,
            &ch,
            &ctx("/context disable memory", "user-9", true, "c2"),
        )
        .await;
        match r {
            GateDecision::Deny(msg) => assert!(msg.contains("管理员"), "{msg}"),
            _ => panic!("期望 Deny"),
        }
        // 未 @ 的非管理员：静默忽略
        let r = admit(
            &core,
            &ch,
            &ctx("/context disable memory", "user-9", false, "c2"),
        )
        .await;
        assert!(matches!(r, GateDecision::Ignore), "静默忽略");
        // 管理员禁用成功 + 事件落库
        let r = deny_of(
            admit(
                &core,
                &ch,
                &ctx("/context disable memory", "admin-1", true, "c2"),
            )
            .await,
        );
        assert!(r.contains("已停用") && r.contains("memory"), "{r}");
        let evt = core
            .store
            .list_events(&sid)
            .unwrap_or_default()
            .into_iter()
            .rfind(|e| {
                e.get("payload")
                    .and_then(|p| p.get("type"))
                    .and_then(|t| t.as_str())
                    == Some("context.sources.updated")
            });
        assert!(evt.is_some(), "context.sources.updated 事件落库");
    }

    #[tokio::test]
    async fn context_单来源独立_禁用不影响其他() {
        let core = std::sync::Arc::new(wf_core());
        let ch = chan(vec!["admin-1"]);
        let sid = setup_flow(&core, &ch, "c3");
        // 禁用 skills：其他来源仍在清单（memory/retrieval 状态独立汇报）
        deny_of(
            admit(
                &core,
                &ch,
                &ctx("/context disable skills", "admin-1", true, "c3"),
            )
            .await,
        );
        let sources = core.context_sources(&sid).unwrap();
        let skills = sources.iter().find(|s| s["key"] == "skills").unwrap();
        assert_eq!(skills["status"], "disabled");
        let memory = sources.iter().find(|s| s["key"] == "memory").unwrap();
        assert_ne!(memory["status"], "disabled", "memory 不受影响");
        // enable 恢复
        deny_of(
            admit(
                &core,
                &ch,
                &ctx("/context enable skills", "admin-1", true, "c3"),
            )
            .await,
        );
        let sources = core.context_sources(&sid).unwrap();
        let skills = sources.iter().find(|s| s["key"] == "skills").unwrap();
        assert_ne!(skills["status"], "disabled");
    }

    /// 绑定隔离项目工作区的流会话：让会话有**项目层**文件来源（AGENTS.md）。
    /// 返回 (sid, 工作区路径)。不写真实仓库根。
    fn setup_flow_in_project(
        core: &exm_core::Core,
        ch: &Channel,
        key: &str,
        agents_md: Option<&str>,
    ) -> (String, std::path::PathBuf) {
        let ws = std::env::temp_dir().join(format!("exm-wf-ctx-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&ws).unwrap();
        if let Some(text) = agents_md {
            std::fs::write(ws.join("AGENTS.md"), text).unwrap();
        }
        let ws_str = ws.canonicalize().unwrap().to_string_lossy().into_owned();
        let title = format!("channel:{}:{}", ch.id, key);
        let s = core
            .store
            .create_session_in(&title, "default", Some(ws_str))
            .expect("建立流会话失败");
        let _ = core.bind_flow_session(&ch.id, key, &s.id);
        (s.id, ws)
    }

    fn cleanup(ws: &std::path::Path) {
        let _ = std::fs::remove_dir_all(ws);
    }

    /// 读面的一条清单行：按 key 取回执里对应的行
    fn line_of<'a>(reply: &'a str, key: &str) -> &'a str {
        reply
            .lines()
            .find(|l| l.contains(&format!("] {key}")))
            .unwrap_or_else(|| panic!("回执缺少来源 {key} 的行：\n{reply}"))
    }

    #[tokio::test]
    async fn context_清单_缺失原因如实可见() {
        let core = std::sync::Arc::new(wf_core());
        let ch = chan(vec!["admin-1"]);
        let sid = setup_flow(&core, &ch, "c-reason");
        let r = deny_of(admit(&core, &ch, &ctx("/context", "user-9", true, "c-reason")).await);
        // 三类运行时来源都在清单里，且各自带**真实的**缺失原因
        // （wf_core：memory 关闭；新会话无滚动摘要；技能目录为空）
        let memory = line_of(&r, "memory");
        assert!(
            memory.contains("missing_dependency"),
            "memory 未就绪要如实标注：{memory}"
        );
        assert!(
            memory.contains("记忆服务未启用"),
            "缺失原因要说明为什么：{memory}"
        );
        let retrieval = line_of(&r, "retrieval");
        assert!(
            retrieval.contains("滚动摘要"),
            "retrieval 的缺失原因：{retrieval}"
        );
        let skills = line_of(&r, "skills");
        assert!(
            skills.contains("技能目录为空"),
            "skills 的缺失原因：{skills}"
        );
        // 清单状态与会话事实一致：没有任何来源被禁用
        let sources = core.context_sources(&sid).unwrap();
        assert!(sources.iter().all(|s| s["disabled"] == false));
        assert!(
            !r.contains("已停用自动注入"),
            "无禁用项时不该出现禁用提示：\n{r}"
        );
    }

    /// 未绑定项目的会话清单里没有任何文件来源——回执必须说明**为什么**
    /// （项目层未加载：会话未绑定项目），而不是让人对着 3 项运行时来源猜。
    /// 原因由 Core 给出，通道原样呈现。
    #[tokio::test]
    async fn context_清单_整层缺席要说明原因() {
        let core = std::sync::Arc::new(wf_core());
        let ch = chan(vec!["admin-1"]);
        let sid = setup_flow(&core, &ch, "c-gap");
        assert!(
            core.store
                .get_session(&sid)
                .unwrap()
                .unwrap()
                .workspace
                .is_none(),
            "夹具前提：未绑定项目"
        );
        assert!(
            core.context_sources(&sid)
                .unwrap()
                .iter()
                .all(|s| s["layer"] != "project"),
            "夹具前提：清单里没有项目层来源"
        );
        let r = deny_of(admit(&core, &ch, &ctx("/context", "user-9", true, "c-gap")).await);
        assert!(
            r.contains("未加载的文件来源"),
            "清单里没有文件来源时要说明有层未加载：\n{r}"
        );
        assert!(
            r.contains("项目层") && r.contains("未绑定项目"),
            "项目层缺席的原因是未绑定项目：\n{r}"
        );
        assert!(r.contains("AGENTS.md"), "要说出没加载的是哪些文件：\n{r}");
        // 它是说明不是来源：不能出现在可操作的清单行里（清单行以来源键开头）
        assert!(
            !r.contains("\nfile:"),
            "缺席说明不是可操作的来源条目：\n{r}"
        );

        // 绑定了项目：项目层在场，就不能再说「未绑定项目」
        let (_sid2, ws) = setup_flow_in_project(&core, &ch, "c-gap-bound", Some("# 规则"));
        let r2 = deny_of(admit(&core, &ch, &ctx("/context", "user-9", true, "c-gap-bound")).await);
        assert!(
            !r2.contains("未绑定项目"),
            "已绑定项目的会话不能说未绑定：\n{r2}"
        );
        assert!(
            !r2.contains("项目层："),
            "项目层在场就不该有项目层的缺席说明：\n{r2}"
        );
        let _ = std::fs::remove_dir_all(ws);
    }

    #[tokio::test]
    async fn context_detail_单项详情与未知键() {
        let core = std::sync::Arc::new(wf_core());
        let ch = chan(vec!["admin-1"]);
        let (sid, ws) = setup_flow_in_project(&core, &ch, "c-detail", Some("# 项目规则\n详情测试"));
        // detail 是读面：非管理员可用
        let r = deny_of(
            admit(
                &core,
                &ch,
                &ctx("/context detail file:AGENTS.md", "user-9", true, "c-detail"),
            )
            .await,
        );
        assert!(r.contains("来源详情"), "{r}");
        assert!(
            r.contains("file:AGENTS.md") && r.contains("loaded"),
            "文件来源详情：{r}"
        );
        assert!(r.contains("项目 层"), "标明层级：{r}");
        assert!(r.contains("自动注入开启中"), "{r}");
        assert!(
            !r.contains("Unix "),
            "修改时间应是人类可读，不是裸时间戳：{r}"
        );
        // 运行时来源：详情里带缺失原因
        let r = deny_of(
            admit(
                &core,
                &ch,
                &ctx("/context detail memory", "user-9", true, "c-detail"),
            )
            .await,
        );
        assert!(r.contains("记忆服务未启用"), "{r}");
        // 未知键：给出可用键，不包装成「失败」
        let r = deny_of(
            admit(
                &core,
                &ch,
                &ctx("/context detail file:nope.md", "user-9", true, "c-detail"),
            )
            .await,
        );
        assert!(r.contains("没有上下文来源 file:nope.md"), "{r}");
        assert!(
            r.contains("file:AGENTS.md") && r.contains("memory"),
            "未知键要列出可用键：{r}"
        );
        // 已禁用的来源：详情说明停用只管自动注入、恢复后回到什么状态
        deny_of(
            admit(
                &core,
                &ch,
                &ctx(
                    "/context disable file:AGENTS.md",
                    "admin-1",
                    true,
                    "c-detail",
                ),
            )
            .await,
        );
        let r = deny_of(
            admit(
                &core,
                &ch,
                &ctx("/context detail file:AGENTS.md", "user-9", true, "c-detail"),
            )
            .await,
        );
        assert!(r.contains("自动注入已停用"), "{r}");
        assert!(r.contains("/skill <id> 不受影响"), "要说清停用范围：{r}");
        assert!(r.contains("恢复后回到原状态 loaded"), "{r}");
        // 回执说的与 Core 事实一致：该来源 disabled，原状态 loaded
        let src = core
            .context_sources(&sid)
            .unwrap()
            .into_iter()
            .find(|s| s["key"] == "file:AGENTS.md")
            .unwrap();
        assert_eq!(
            (src["disabled"].clone(), src["baseStatus"].clone()),
            (true.into(), "loaded".into())
        );
        cleanup(&ws);
    }

    #[tokio::test]
    async fn context_refresh_单来源成功_其余不变() {
        let core = std::sync::Arc::new(wf_core());
        let ch = chan(vec!["admin-1"]);
        let (sid, ws) = setup_flow_in_project(&core, &ch, "c-refresh-ok", Some("# v1"));
        // 先让另一个来源处于与默认不同的状态，验证刷新不碰它
        deny_of(
            admit(
                &core,
                &ch,
                &ctx("/context disable skills", "admin-1", true, "c-refresh-ok"),
            )
            .await,
        );
        let before = core.context_sources(&sid).unwrap();
        // 改文件后单来源刷新：读面（非管理员可用），读到新内容
        std::fs::write(ws.join("AGENTS.md"), "# v2 刷新后的内容").unwrap();
        let r = deny_of(
            admit(
                &core,
                &ch,
                &ctx(
                    "/context refresh file:AGENTS.md",
                    "user-9",
                    true,
                    "c-refresh-ok",
                ),
            )
            .await,
        );
        assert!(r.contains("✅ 已重读来源 file:AGENTS.md"), "{r}");
        assert!(r.contains("loaded"), "{r}");
        assert!(!r.contains("⚠️"), "成功不该带失败标记：{r}");
        // 其余来源逐项未变（状态、禁用位、原因）
        let after = core.context_sources(&sid).unwrap();
        for b in &before {
            if b["key"] == "file:AGENTS.md" {
                continue;
            }
            let a = after.iter().find(|x| x["key"] == b["key"]).unwrap();
            assert_eq!(
                a["status"], b["status"],
                "刷新一个来源不得改变 {} 的状态",
                b["key"]
            );
            assert_eq!(
                a["disabled"], b["disabled"],
                "不得改变 {} 的禁用位",
                b["key"]
            );
            assert_eq!(a["reason"], b["reason"], "不得改变 {} 的缺失原因", b["key"]);
        }
        assert_eq!(
            core.store.disabled_context_sources(&sid),
            vec!["skills".to_string()],
            "禁用清单不变"
        );
        // 刷新留痕
        let evt = core
            .store
            .list_events(&sid)
            .unwrap_or_default()
            .into_iter()
            .any(|e| {
                e.get("payload")
                    .and_then(|p| p.get("type"))
                    .and_then(|t| t.as_str())
                    == Some("context.source.refreshed")
            });
        assert!(evt, "context.source.refreshed 事件落库");
        cleanup(&ws);
    }

    #[tokio::test]
    async fn context_refresh_单来源失败_只影响自己() {
        let core = std::sync::Arc::new(wf_core());
        let ch = chan(vec!["admin-1"]);
        let (sid, ws) = setup_flow_in_project(&core, &ch, "c-refresh-fail", Some("# v1"));
        std::fs::create_dir_all(ws.join(".exmachina")).unwrap();
        std::fs::write(ws.join(".exmachina/context.md"), "# 项目补充").unwrap();
        let before = core.context_sources(&sid).unwrap();
        // 让 AGENTS.md 读不出来（路径上是目录）→ 真实的读取失败
        std::fs::remove_file(ws.join("AGENTS.md")).unwrap();
        std::fs::create_dir_all(ws.join("AGENTS.md")).unwrap();
        let r = deny_of(
            admit(
                &core,
                &ch,
                &ctx(
                    "/context refresh file:AGENTS.md",
                    "admin-1",
                    true,
                    "c-refresh-fail",
                ),
            )
            .await,
        );
        assert!(r.contains("⚠️ 重读来源 file:AGENTS.md 失败"), "{r}");
        assert!(r.contains("读取失败"), "失败原因要如实给出：{r}");
        assert!(!r.contains("✅"), "失败不得报成功：{r}");
        // 「其他 N 个来源未受影响」里的 N 不含被刷新的那个
        let others = before.len() - 1;
        assert!(r.contains(&format!("其他 {others} 个来源未受影响")), "{r}");
        // 事实层面：其余来源逐项未变，禁用清单未变，没有留下任何写入
        let after = core.context_sources(&sid).unwrap();
        for b in &before {
            if b["key"] == "file:AGENTS.md" {
                continue;
            }
            let a = after.iter().find(|x| x["key"] == b["key"]).unwrap();
            assert_eq!(
                (a["status"].clone(), a["disabled"].clone()),
                (b["status"].clone(), b["disabled"].clone()),
                "{} 不受影响",
                b["key"]
            );
        }
        assert!(
            core.store.disabled_context_sources(&sid).is_empty(),
            "失败不改禁用清单"
        );
        // 修复后同一命令恢复成功（失败不是终态）
        std::fs::remove_dir_all(ws.join("AGENTS.md")).unwrap();
        std::fs::write(ws.join("AGENTS.md"), "# 修好了").unwrap();
        let r = deny_of(
            admit(
                &core,
                &ch,
                &ctx(
                    "/context refresh file:AGENTS.md",
                    "admin-1",
                    true,
                    "c-refresh-fail",
                ),
            )
            .await,
        );
        assert!(
            r.contains("✅ 已重读来源 file:AGENTS.md"),
            "修好后应恢复：{r}"
        );
        cleanup(&ws);
    }

    #[tokio::test]
    async fn context_refresh_运行时来源_如实说是重新观测() {
        let core = std::sync::Arc::new(wf_core());
        let ch = chan(vec!["admin-1"]);
        let _sid = setup_flow(&core, &ch, "c-refresh-rt");
        // 记忆未启用：没有可重读的文件，也没有「就绪」可言——不得报 ✅ 已重读
        let r = deny_of(
            admit(
                &core,
                &ch,
                &ctx("/context refresh memory", "user-9", true, "c-refresh-rt"),
            )
            .await,
        );
        assert!(
            r.contains("没有可重读的文件"),
            "运行时来源的刷新要如实措辞：{r}"
        );
        assert!(r.contains("未就绪：记忆服务未启用"), "观测结果如实：{r}");
        assert!(!r.contains("✅"), "未就绪的观测不能报成功标记：{r}");
        // 未知键：是用法错误，不是「刷新失败」
        let r = deny_of(
            admit(
                &core,
                &ch,
                &ctx(
                    "/context refresh file:AGENTS.md",
                    "user-9",
                    true,
                    "c-refresh-rt",
                ),
            )
            .await,
        );
        assert!(r.contains("没有上下文来源 file:AGENTS.md，无从刷新"), "{r}");
        assert!(
            r.contains("可用：") && r.contains("memory"),
            "给出可用键：{r}"
        );
        assert!(
            !r.contains("刷新来源 file:AGENTS.md 失败"),
            "未知键不是刷新失败：{r}"
        );
        assert!(
            !r.contains("其他来源不受影响）"),
            "什么都没发生，别叠一层失败包装：{r}"
        );
    }

    #[tokio::test]
    async fn context_未知子命令_不是写操作_对所有人给用法() {
        let core = std::sync::Arc::new(wf_core());
        let ch = chan(vec!["admin-1"]);
        let _sid = setup_flow(&core, &ch, "c-usage");
        // 非管理员打错子命令：得到用法提示，而不是「需要管理员权限」
        // （权限提示会让人以为是权限问题，还暗示存在这个子命令）
        let r = admit(
            &core,
            &ch,
            &ctx("/context frobnicate", "user-9", true, "c-usage"),
        )
        .await;
        let msg = deny_of(r);
        assert!(msg.contains("用法"), "{msg}");
        assert!(!msg.contains("管理员"), "未知子命令不是写操作：{msg}");
        // 真正的写子命令仍然只对管理员开放
        let r = admit(
            &core,
            &ch,
            &ctx("/context disable memory", "user-9", true, "c-usage"),
        )
        .await;
        match r {
            GateDecision::Deny(msg) => assert!(msg.contains("管理员"), "{msg}"),
            _ => panic!("期望 Deny"),
        }
        // 管理员禁用未知键：如实说明未做任何改动 + 为什么没有这个来源
        let r = deny_of(
            admit(
                &core,
                &ch,
                &ctx("/context disable file:nope.md", "admin-1", true, "c-usage"),
            )
            .await,
        );
        assert!(r.contains("未做任何改动"), "{r}");
        assert!(r.contains("可用："), "要给出可用键：{r}");
        assert!(
            core.store.disabled_context_sources(&_sid).is_empty(),
            "失败的禁用不留痕"
        );
        // 缺 key 同样是用法错误（管理员视角）：给用法，不做任何改动
        for cmd in ["/context disable", "/context enable", "/context detail"] {
            let r = deny_of(admit(&core, &ch, &ctx(cmd, "admin-1", true, "c-usage")).await);
            assert!(r.contains("用法"), "{cmd} 缺 key 应给用法提示：{r}");
        }
        assert!(core.store.disabled_context_sources(&_sid).is_empty());
    }

    /// 同一缺陷类在 /goal /spec /learn 上同样成立：未知子命令走用法提示，
    /// 不按写操作判权。
    #[tokio::test]
    async fn 未知子命令_各族一致_不当写操作判权() {
        let core = std::sync::Arc::new(wf_core());
        let ch = chan(vec!["admin-1"]);
        let _sid = setup_flow(&core, &ch, "c-usage-all");
        for (cmd, family) in [
            ("/goal frobnicate", "/goal"),
            ("/spec frobnicate", "/spec"),
            ("/learn frobnicate", "/learn"),
            ("/plan frobnicate", "/plan"),
            ("/context frobnicate", "/context"),
        ] {
            let r = deny_of(admit(&core, &ch, &ctx(cmd, "user-9", true, "c-usage-all")).await);
            assert!(r.contains("用法"), "{family} 未知子命令应给用法提示：{r}");
            assert!(!r.contains("管理员"), "{family} 未知子命令不是写操作：{r}");
        }
        // 写子命令照旧只对管理员开放（各族各验一个）
        for (cmd, family) in [
            ("/goal clear", "/goal"),
            ("/spec clear", "/spec"),
            ("/learn approve abc", "/learn"),
            ("/plan approve", "/plan"),
            ("/context disable memory", "/context"),
        ] {
            let r = admit(&core, &ch, &ctx(cmd, "user-9", true, "c-usage-all")).await;
            match r {
                GateDecision::Deny(msg) => assert!(
                    msg.contains("管理员"),
                    "{family} 写子命令应要求管理员：{msg}"
                ),
                _ => panic!("{family} 期望 Deny"),
            }
        }
    }

    /// capability-parity 6.4：/learn 命令族——清单读面、裁决写面权限、
    /// 候选提交红线（无真实证据 Core 层拒）。
    mod learn_cmd_tests {
        use super::*;
        use exm_core::learning_candidates::CandidateDraft;

        /// 真实语义的候选种子：来源会话绑定隔离工作区，本轮暂存里有一条**成功的
        /// filesystem write 回执**指向该文件，证据按工作区相对路径引用——与模型在
        /// 同一轮内「写文件 → 提候选」的实际路径一致（核验要求三者齐备）。
        fn submit_real_ch(
            c: &exm_core::Core,
            title: &str,
            triggers: &[&str],
        ) -> exm_core::learning_candidates::LearningCandidate {
            use exm_core::learning_candidates::{CandidateDraft, CandidateEvidenceKind};
            let ws = std::env::temp_dir().join(format!("exm-lrn-ch-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&ws).unwrap();
            std::fs::write(ws.join("ARTIFACT.md"), "artifact").unwrap();
            let session = c
                .store
                .create_session_in(
                    "learn-seed",
                    "exmachina",
                    Some(ws.to_string_lossy().into_owned()),
                )
                .unwrap();
            exm_core::round_trace::begin(&session.id);
            exm_core::round_trace::push_tool(
                &session.id,
                exm_core::round_trace::ToolCallRecord {
                    call_id: "seed-write".into(),
                    agent_id: "exmachina".into(),
                    tool: "filesystem".into(),
                    args: serde_json::json!({ "op": "write", "path": "ARTIFACT.md", "content": "artifact" }),
                    ok: true,
                    duration_ms: 1,
                    summary: String::new(),
                },
            );
            let triggers: Vec<String> = triggers.iter().map(|s| s.to_string()).collect();
            let evidence = vec![(CandidateEvidenceKind::ArtifactFile, "ARTIFACT.md".into())];
            let draft = CandidateDraft {
                target: "skill",
                target_id: Some("learn-seed"),
                title,
                content: "真实产物支撑的经验",
                scope: "global",
                triggers: &triggers,
                source_session: &session.id,
                source_goal: "g",
                evidence: &evidence,
            };
            let cand = c.learning_submit(&draft).expect("真实证据候选过审");
            exm_core::round_trace::end(&session.id);
            let _ = std::fs::remove_dir_all(&ws);
            cand
        }

        /// 模拟「提交后、批准前内容被改坏」：直接改账本把候选内容换成敏感形态
        /// （与 core 层 转正失败_裁决保留_失败留痕 同一手法）。转正时的第二道闸必须拦下。
        fn tamper_to_sensitive(core: &exm_core::Core, id: &str) {
            let mut all = core.store.list_learning_candidates();
            let row = all.iter_mut().find(|c| c.id == id).expect("候选应在账本里");
            row.content = "部署时用 API_KEY=sk-abcdefghijklmnopqr 直连".into();
            core.store
                .update_learning_candidate(row)
                .expect("改写候选账本");
        }

        #[tokio::test]
        async fn learn_读面_候选清单() {
            let (c, _dirs) = wf_core_guarded();
            let core = std::sync::Arc::new(c);
            let ch = chan(vec!["admin-1"]);
            let cand = submit_real_ch(&core, "发布走金丝雀", &["发布", "金丝雀"]);
            setup_flow(&core, &ch, "l1");
            // 普通成员读面可用
            let r = deny_of(admit(&core, &ch, &ctx("/learn", "user-9", true, "l1")).await);
            assert!(r.contains("学习候选") && r.contains("发布走金丝雀"), "{r}");
            // 状态过滤
            let r = deny_of(
                admit(
                    &core,
                    &ch,
                    &ctx("/learn list approved", "user-9", true, "l1"),
                )
                .await,
            );
            assert!(r.contains("学习候选"), "{r}");
            let _ = cand;
        }

        #[tokio::test]
        async fn learn_裁决写面_权限与一次定局() {
            let (c, _dirs) = wf_core_guarded();
            let core = std::sync::Arc::new(c);
            let ch = chan(vec!["admin-1"]);
            // 落点技能预置版本链 v1 → v2：转正应续接为 v3，回执必须如实报 v3
            // （若回执版本写死或错位成 v1，这里才区分得出来）
            let mut pre = exm_core::types::SkillDef {
                id: "learn-seed".into(),
                name: "发布流程".into(),
                instructions: "初版：发布前手工确认".into(),
                ..Default::default()
            };
            core.skill_write_versioned(&pre).unwrap();
            pre.instructions = "第二版：发布前跑一遍回归".into();
            core.skill_write_versioned(&pre).unwrap();
            let cand = submit_real_ch(&core, "构建缓存卷", &["构建"]);
            setup_flow(&core, &ch, "l2");
            // 非管理员拒绝（@）
            match admit(
                &core,
                &ch,
                &ctx(&format!("/learn approve {}", cand.id), "user-9", true, "l2"),
            )
            .await
            {
                GateDecision::Deny(msg) => assert!(msg.contains("管理员"), "{msg}"),
                _ => panic!("期望 Deny"),
            }
            // 未 @ 静默
            assert!(matches!(
                admit(
                    &core,
                    &ch,
                    &ctx(
                        &format!("/learn approve {}", cand.id),
                        "user-9",
                        false,
                        "l2"
                    )
                )
                .await,
                GateDecision::Ignore
            ));
            // 管理员批准：回执带转正落点与版本号（与 REST 同一事实源的如实转述）
            let r = deny_of(
                admit(
                    &core,
                    &ch,
                    &ctx(
                        &format!("/learn approve {}", cand.id),
                        "admin-1",
                        true,
                        "l2",
                    ),
                )
                .await,
            );
            assert!(r.contains("已批准"), "{r}");
            assert!(
                r.contains("learn-seed v3"),
                "回执应带落点 id 与**实际生效**版本号（续接既有链 = v3，不是 v1）—— {r}"
            );
            assert!(!r.contains("learn-seed v1"), "回执不得把落点报成 v1 —— {r}");
            // 落点确实是版本链里的当前版本（跨入口同一事实源）
            let hist = core.skill_history_detailed("learn-seed").unwrap();
            assert_eq!(hist.last().map(|row| row.version), Some(3));
            // 重复裁决拒绝
            let r = deny_of(
                admit(
                    &core,
                    &ch,
                    &ctx(&format!("/learn reject {}", cand.id), "admin-1", true, "l2"),
                )
                .await,
            );
            assert!(r.contains("失败"), "{r}");

            // 批准 ≠ 生效：内容在批准前被改坏 → 转正失败，回执必须如实说失败，
            // 不得渲染成「已写入」（否则用户以为共享技能/记忆已更新）
            let leaky = submit_real_ch(&core, "泄露的部署脚本", &["泄露"]);
            tamper_to_sensitive(&core, &leaky.id);
            let r = deny_of(
                admit(
                    &core,
                    &ch,
                    &ctx(
                        &format!("/learn approve {}", leaky.id),
                        "admin-1",
                        true,
                        "l2",
                    ),
                )
                .await,
            );
            assert!(r.contains("已批准"), "裁决本身成立：{r}");
            assert!(r.contains("转正失败"), "回执应明示转正失败：{r}");
            assert!(!r.contains("已写入"), "转正失败不得渲染成已写入：{r}");
            assert!(
                r.contains("敏感"),
                "回执应带真实失败原因（不是「未知原因」）：{r}"
            );
            // 落点技能未被创建（失败转正不得留下半成品）；候选状态是 approved（裁决保留）
            assert!(
                core.skill_history_detailed("learn-seed")
                    .unwrap()
                    .last()
                    .map(|row| row.version)
                    == Some(3),
                "失败的转正不得改动既有技能链"
            );
            let after = core
                .learning_candidates(Some("approved"))
                .unwrap()
                .into_iter()
                .find(|c| c.id == leaky.id)
                .expect("裁决保留：候选应为 approved");
            let promo = after.promotion.as_ref().expect("应记录转正结果");
            assert!(!promo.ok, "转正应失败：{promo:?}");
            assert!(
                promo.error.as_deref().unwrap_or("").contains("敏感"),
                "{:?}",
                promo.error
            );
        }

        #[tokio::test]
        async fn learn_红线_模型自述不进候选() {
            let (c, _dirs) = wf_core_guarded();
            let core = std::sync::Arc::new(c);
            let triggers = vec!["直推".to_string()];
            let draft = CandidateDraft {
                target: "skill",
                target_id: Some("learn-seed"),
                title: "直推上线",
                content: "我确认成功了，直接推",
                scope: "global",
                triggers: &triggers,
                source_session: "seed",
                source_goal: "g",
                evidence: &[],
            };
            let err = core.learning_submit(&draft).unwrap_err().to_string();
            assert!(err.contains("真实证据"), "模型自述被拒：{err}");
        }
    }
}
