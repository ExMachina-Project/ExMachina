//! core/tests.rs —— 原 lib.rs 内联测试随 Core 领域拆分整体迁出（纯搬移）。
//! 各 mod 归属域以 mod 名标注（im_discipline/history/plan/root_approval/
//! session_grant/meta_approval/session_origin/flow_archive/once_flag）。

// 原 lib.rs 测试块经 `use super::*` 取得的符号，迁出后在此显式引入（纯搬移不改断言）
use super::super::Core;
use crate::config::ExmConfig;
use crate::types::*;
use parking_lot::Mutex;
use std::sync::Arc;

// ---------------------------------------------------------------- 会话历史管控（组 9 单测）

#[cfg(test)]
mod im_discipline_tests {
    /// 会话归档状态机（spec session-management）：归档移出主列表、
    /// 归档区可见、恢复回主列表原分组、组内列表同口径过滤。
    #[test]
    fn 会话归档_过滤恢复与隔离() {
        let dir = std::env::temp_dir().join(format!("exm-arch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        let core = crate::Core::create(&dir).expect("创建 Core 失败");
        let a = core.create_session("arch-a").unwrap();
        let b = core.create_session("arch-b").unwrap();
        // 归档 a：主列表（组内 + 全量）只剩 b
        core.set_session_archived(&a.id, true).unwrap();
        let group_ids: Vec<String> = core
            .list_sessions_in_group(&a.group_id)
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert!(!group_ids.contains(&a.id), "归档态不进组内主列表");
        assert!(group_ids.contains(&b.id));
        let all_ids: Vec<String> = core.list_sessions().unwrap().into_iter().map(|s| s.id).collect();
        assert!(!all_ids.contains(&a.id), "归档态不进全量主列表");
        // 归档区：a 可见且带 archived_at 时间戳
        let archived = core.list_archived_sessions().unwrap();
        assert_eq!(archived.len(), 1, "归档区恰一条");
        assert_eq!(archived[0].id, a.id);
        assert!(archived[0].archived_at.is_some(), "归档时间落档");
        // 恢复：主列表回归（同组原位），归档区清空
        core.set_session_archived(&a.id, false).unwrap();
        let group_ids: Vec<String> = core
            .list_sessions_in_group(&a.group_id)
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert!(group_ids.contains(&a.id), "恢复后回主列表");
        assert!(core.list_archived_sessions().unwrap().is_empty(), "归档区清空");
        // 不存在的会话：报错不 panic
        assert!(core.set_session_archived("no-such", true).is_err());
    }
    /// 流名维护（会话管理可用性修复）：空填不覆盖——既有名（用户自定义或
    /// 更早来源）不被新名冲掉；空名流首名即落。
    #[test]
    fn 流名维护_空填不覆盖() {
        let dir = std::env::temp_dir().join(format!("exm-flowname-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        let core = crate::Core::create(&dir).expect("创建 Core 失败");
        let sid = core.create_session("fn-test").unwrap().id;
        // 建流带名
        core.bind_flow_session_named("ch1", "12345", &sid, Some("萝莉领主二群"), Some(true))
            .unwrap();
        let f = core.channel_flow("ch1", "12345").unwrap();
        assert_eq!(f.display_name, "萝莉领主二群");
        assert!(f.is_group, "群聊标记应落");
        // 再补名不覆盖（群改名事件到达也不冲掉——展示名以首次为准，
        // 用户在控制台改名后更不能被冲）
        core.ensure_flow_named("ch1", "12345", "新群名", true);
        let f = core.channel_flow("ch1", "12345").unwrap();
        assert_eq!(f.display_name, "萝莉领主二群", "已有名不得被覆盖");
        // 空名流首名即落（ensure_flow_named 空填路径）
        let sid2 = core.create_session("fn-test-2").unwrap().id;
        core.bind_flow_session_named("ch1", "67890", &sid2, None, None)
            .unwrap();
        assert_eq!(
            core.channel_flow("ch1", "67890").unwrap().display_name,
            "",
            "无名建流保持空"
        );
        core.ensure_flow_named("ch1", "67890", "终夜", false);
        assert_eq!(core.channel_flow("ch1", "67890").unwrap().display_name, "终夜");
        // 空串补名是 no-op（解析失败的适配器不该写入空名）
        core.ensure_flow_named("ch1", "67890", "", false);
        assert_eq!(core.channel_flow("ch1", "67890").unwrap().display_name, "终夜");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 流未读计数（spec experience-iteration 未读徽标服务端口径）：
    /// 有流才计数、累积不溢出、清零即除名、无流 bump 静默忽略。
    #[test]
    fn 流未读_累积与清零() {
        let dir = std::env::temp_dir().join(format!("exm-unread-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        let core = crate::Core::create(&dir).expect("创建 Core 失败");
        // 建流（bind_flow_session 顺带建流文档）
        let s = core.create_session("unread-test").unwrap();
        core.bind_flow_session("ch1", "g100", &s.id).unwrap();
        assert_eq!(core.flow_unread_count("ch1", "g100"), 0, "初始零未读");
        core.bump_flow_unread("ch1", "g100");
        core.bump_flow_unread("ch1", "g100");
        assert_eq!(core.flow_unread_count("ch1", "g100"), 2, "两次回复累积 2");
        core.clear_flow_unread("ch1", "g100");
        assert_eq!(core.flow_unread_count("ch1", "g100"), 0, "打开会话清零");
        // 无流 bump：静默忽略不 panic、不产生幽灵计数
        core.bump_flow_unread("ch-none", "g-none");
        assert_eq!(core.flow_unread_count("ch-none", "g-none"), 0);
    }
    /// IM 会话标记状态机：登记幂等、除名后不再命中、独立于轮次消费（不 take）。
    /// 注入路径（single_direct_round / converge 读 is_im_session 拼 IM_DISCIPLINE）
    /// 由编译器保证走同一分支，此处锚定标记语义与纪律文本的关键约束项。
    #[test]
    fn im会话标记_登记幂等与除名() {
        let sid = format!("im-test-{}", crate::types::new_id());
        assert!(!crate::orchestrator::is_im_session(&sid));
        crate::orchestrator::mark_im_session(&sid);
        crate::orchestrator::mark_im_session(&sid); // 幂等
        assert!(crate::orchestrator::is_im_session(&sid));
        crate::orchestrator::unmark_im_session(&sid);
        assert!(!crate::orchestrator::is_im_session(&sid));
    }

    #[test]
    fn im纪律文本_含关键约束() {
        let d = crate::orchestrator::IM_DISCIPLINE;
        assert!(d.contains("先给结论"), "结论先行：{d}");
        assert!(d.contains("1~5 句"), "句数上限：{d}");
        assert!(d.contains("不逐条播报"), "过程静默：{d}");
        assert!(d.contains("噪音"), "禁播报前缀：{d}");
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;
    use crate::types::{AgentDefinition, Tier};

    fn mock_core() -> Core {
        let dir = std::env::temp_dir().join(format!("exm-hist-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false; // 历史管控测试不关心记忆写入
        let core = Core::with_config(cfg).expect("创建 Core 失败");
        core.registry()
            .create_group(Some("t".into()), "历史测试组", "")
            .unwrap();
        let def = AgentDefinition {
            name: "问答体".into(),
            identifier: "hist-orch".into(),
            domain: "测试".into(),
            tier: Tier::Orchestrator,
            description: "测试主智能体".into(),
            capabilities: vec![],
            tools: vec![],
            when_to_call: String::new(),
            link: None,
            max_steps: None,
            dependencies: vec![],
            composable_with: vec![],
            input_schema: Default::default(),
            output_schema: Default::default(),
            prompt_file: String::new(),
            model_hint: None,
        };
        core.registry().upsert_agent("t", def, None).unwrap();
        core.registry().set_primary("t", "hist-orch").unwrap();
        core.registry().set_active_group("t").unwrap();
        core
    }

    async fn three_rounds(core: &Core, sid: &str) {
        core.chat(sid, "第一轮问题").await.unwrap();
        core.chat(sid, "第二轮问题").await.unwrap();
        core.chat(sid, "第三轮问题").await.unwrap();
    }

    #[tokio::test]
    async fn 轮次快照_多轮对话后快照链完整() {
        let core = mock_core();
        let s = core.create_session("快照链").unwrap();
        three_rounds(&core, &s.id).await;
        let snaps = core.store.list_turn_snapshots(&s.id).unwrap();
        assert_eq!(snaps.len(), 3, "每轮一条快照");
        for (i, snap) in snaps.iter().enumerate() {
            assert_eq!(snap.turn, i + 1, "轮次从 1 递增");
            assert_eq!(snap.status, "done");
        }
        // 游标单调不减且等于当前消息总数
        let total = core.store.list_messages(&s.id, 0).unwrap().len();
        assert_eq!(snaps.last().unwrap().message_count, total);
        assert!(
            snaps
                .windows(2)
                .all(|w| w[0].message_count <= w[1].message_count),
            "游标单调"
        );
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    #[tokio::test]
    async fn undo_空闲回退_归档可查_快照裁剪() {
        let core = mock_core();
        let s = core.create_session("撤销").unwrap();
        three_rounds(&core, &s.id).await;
        let total = core.store.list_messages(&s.id, 0).unwrap().len();

        let info = core.undo_session(&s.id, 2, false).await.unwrap();
        assert_eq!(
            info["archived"].as_u64().unwrap() as usize,
            total - snaps_at(&core, &s.id, 2),
            "归档数 = 第 2 轮之后的消息数"
        );
        // 活跃流截断到第 2 轮游标
        let cursor = core
            .store
            .list_turn_snapshots(&s.id)
            .unwrap()
            .last()
            .unwrap()
            .message_count;
        let now_len = core.store.list_messages(&s.id, 0).unwrap().len();
        assert_eq!(now_len, cursor, "截断后游标一致");
        assert!(now_len < total, "确实发生了截断");
        // 归档可查：被截断内容保留
        let archive = core.store.list_archived_messages(&s.id).unwrap();
        assert_eq!(archive.len(), total - now_len);
        assert!(
            archive
                .iter()
                .any(|m| m.statements.iter().any(|st| st.text.contains("第三轮"))),
            "第三轮内容在归档中"
        );
        // 孤儿快照已裁剪
        assert_eq!(snaps_len(&core, &s.id), 2);
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    fn snaps_len(core: &Core, sid: &str) -> usize {
        core.store.list_turn_snapshots(sid).unwrap().len()
    }

    fn snaps_at(core: &Core, sid: &str, turn: usize) -> usize {
        core.store
            .list_turn_snapshots(sid)
            .unwrap()
            .iter()
            .find(|s| s.turn == turn)
            .map(|s| s.message_count)
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn undo_处理中拒绝() {
        let core = mock_core();
        let s = core.create_session("忙碌").unwrap();
        core.chat(&s.id, "先来一轮").await.unwrap();
        // 占住运行锁（模拟处理中）
        let lock = core.run_lock(&s.id);
        let guard = lock.lock().await;
        let err = core.undo_session(&s.id, 1, false).await.unwrap_err();
        assert!(err.to_string().contains("处理中"), "应拒绝：{err}");
        drop(guard);
        // 空闲后可撤销
        assert!(core.undo_session(&s.id, 1, false).await.is_ok());
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    #[tokio::test]
    async fn undo_检查点联动回滚文件() {
        let core = mock_core();
        let s = core.create_session("文件联动").unwrap();
        // 构造两轮：第 1 轮登记检查点（文件 v1 → v2），第 2 轮普通
        let file = core.config().workspace_root.join("a.txt");
        std::fs::write(&file, "v1").unwrap();
        let day: String = now_iso().chars().take(10).collect();
        // 检查点文件（写类工具写前快照的同款落点）+ 轮次链登记
        let cp = core
            .config()
            .workspace_root
            .join(".exmachina")
            .join("checkpoints")
            .join(&day)
            .join("a.txt");
        std::fs::create_dir_all(cp.parent().unwrap()).unwrap();
        std::fs::write(&cp, "v1").unwrap();
        core.store
            .record_checkpoint_ref(&s.id, &day, "a.txt")
            .unwrap();
        std::fs::write(&file, "v2").unwrap();
        core.store
            .put_turn_snapshot(&TurnSnapshot {
                id: format!("{}:1", s.id),
                session_id: s.id.clone(),
                turn: 1,
                message_count: 2,
                checkpoint_ids: vec![format!("{day}/a.txt")],
                prompt_tokens: 0,
                completion_tokens: 0,
                status: "done".into(),
                created_at: now_iso(),
            })
            .unwrap();
        core.store
            .add_message(
                &s.id,
                MessageRole::User,
                None,
                vec![Statement::new(SpeechTag::要求, "改文件")],
            )
            .unwrap();
        core.store
            .put_turn_snapshot(&TurnSnapshot {
                id: format!("{}:2", s.id),
                session_id: s.id.clone(),
                turn: 2,
                message_count: 3,
                checkpoint_ids: vec![],
                prompt_tokens: 0,
                completion_tokens: 0,
                status: "done".into(),
                created_at: now_iso(),
            })
            .unwrap();

        // 撤销到第 1 轮（保留）→ 第 2 轮无检查点；撤销到第 0 轮 → 第 1 轮的检查点回滚文件
        let info = core.undo_session(&s.id, 0, true).await.unwrap();
        let restored: Vec<String> = serde_json::from_value(info["restored"].clone()).unwrap();
        assert_eq!(
            restored,
            vec![format!("{day}/a.txt")],
            "检查点引用被联动回滚"
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "v1",
            "文件内容回到检查点版本"
        );
        assert_eq!(
            core.store.list_messages(&s.id, 0).unwrap().len(),
            0,
            "游标回退到 0"
        );
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    #[tokio::test]
    async fn 编辑重发_原内容归档_新内容重执行() {
        let core = mock_core();
        let s = core.create_session("改写").unwrap();
        three_rounds(&core, &s.id).await;
        let before = core.store.list_messages(&s.id, 0).unwrap().len();

        core.edit_resend(&s.id, 2, "第二轮问题（已修改）")
            .await
            .unwrap();
        // 原内容归档可查
        let archive_text: String = core
            .store
            .list_archived_messages(&s.id)
            .unwrap()
            .iter()
            .flat_map(|m| m.statements.iter().map(|st| st.text.clone()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            archive_text.contains("第二轮问题"),
            "原第 2 轮内容在归档中：{archive_text}"
        );
        assert!(archive_text.contains("第三轮问题"), "第 3 轮一并被回退归档");
        // 新内容重执行：活跃流 = 第 1 轮 + 新一轮，最后一条用户消息为新文本
        let msgs = core.store.list_messages(&s.id, 0).unwrap();
        assert!(msgs.len() < before + 2, "重发后活跃流短于原全程");
        let last_user = msgs
            .iter()
            .rev()
            .find(|m| matches!(m.role, MessageRole::User))
            .unwrap();
        assert!(
            last_user.statements[0].text.contains("已修改"),
            "以编辑后的内容重执行"
        );
        // 新一轮也落了快照
        let snaps = core.store.list_turn_snapshots(&s.id).unwrap();
        assert_eq!(snaps.len(), 2, "第 1 轮保留 + 重发轮新快照");
        assert_eq!(snaps.last().unwrap().turn, 2);
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    #[tokio::test]
    async fn fork_派生独立演进_原会话不变() {
        let core = mock_core();
        let s = core.create_session("分叉源").unwrap();
        three_rounds(&core, &s.id).await;
        let src_msgs = core.store.list_messages(&s.id, 0).unwrap().len();
        let src_snaps = snaps_len(&core, &s.id);

        let fork = core.fork_session(&s.id, 2).unwrap();
        assert_ne!(fork.id, s.id);
        assert_eq!(
            fork.parent_session.as_deref(),
            Some(s.id.as_str()),
            "派生来源标注"
        );
        assert_eq!(fork.parent_upto, Some(2));
        assert_eq!(fork.group_id, "t", "组归属继承");
        // 历史副本 = 截至第 2 轮
        let copied = core.store.list_messages(&fork.id, 0).unwrap();
        let cursor = core
            .store
            .list_turn_snapshots(&s.id)
            .unwrap()
            .iter()
            .find(|x| x.turn == 2)
            .unwrap()
            .message_count;
        assert_eq!(copied.len(), cursor, "副本 = 前缀游标");
        assert!(copied.len() < src_msgs);
        // 原会话不变
        assert_eq!(core.store.list_messages(&s.id, 0).unwrap().len(), src_msgs);
        assert_eq!(snaps_len(&core, &s.id), src_snaps);
        // 分支独立演进：新消息只进分支
        core.chat(&fork.id, "分支上的新问题").await.unwrap();
        assert!(core.store.list_messages(&fork.id, 0).unwrap().len() > copied.len());
        assert_eq!(
            core.store.list_messages(&s.id, 0).unwrap().len(),
            src_msgs,
            "原会话不受分支影响"
        );
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    #[tokio::test]
    async fn undo_不存在的轮次与会话报错() {
        let core = mock_core();
        let s = core.create_session("边界").unwrap();
        assert!(
            core.undo_session(&s.id, 5, false).await.is_err(),
            "无快照轮次拒绝"
        );
        assert!(
            core.undo_session("no-such-session", 1, false)
                .await
                .is_err(),
            "不存在会话拒绝"
        );
        assert!(core.fork_session("no-such-session", 1).is_err());
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }
}

// ---------------------------------------------------------------- 计划模式集成（组 4）

#[cfg(test)]
mod plan_tests {
    use super::*;

    fn plan_core() -> Core {
        let dir = std::env::temp_dir().join(format!("exm-plan-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        Core::with_config(cfg).expect("创建 Core 失败")
    }

    /// 计划模式全链路：开启 → 拦截写转条目 + 落审批单 → 批准（approvals 通道）→ 回放落地
    #[tokio::test]
    async fn 计划审批_批准经approvals通道回放落地() {
        let core = plan_core();
        let s = core.create_session("计划").unwrap();
        core.set_plan_mode(&s.id, true);
        assert!(crate::plan::is_plan_mode(&s.id));
        // 拦截一次写（filesystem write）→ 1 条计划 + 关联审批单
        let orch = core.orchestrator.read().clone();
        let r = orch
            .tools
            .execute(
                "a1",
                &s.id,
                &[crate::types::ToolName::Filesystem],
                crate::types::ToolName::Filesystem,
                &serde_json::json!({ "op": "write", "path": "p.txt", "content": "v" }),
            )
            .await;
        assert!(r.ok && r.output.contains("计划模式"));
        assert_eq!(core.plan_list(&s.id).len(), 1);
        let pending = core.approval_list(Some("pending"), 10).unwrap();
        assert_eq!(pending.len(), 1, "应落一张计划审批单");
        assert!(
            pending[0].command.starts_with("[plan]"),
            "计划审批单带前缀标"
        );
        let ap_id = pending[0].id.clone();
        // 批准（经 approvals 通道）→ 回放执行
        let decided = core.approval_decide(&ap_id, true).await.unwrap();
        assert_eq!(decided.status, "executed");
        assert!(!crate::plan::is_plan_mode(&s.id), "批准后切 execute");
        assert!(core.plan_list(&s.id).is_empty(), "清单已消费");
        assert!(
            core.config().workspace_root.join("p.txt").exists(),
            "回放应真正写文件"
        );
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// 拒绝（approvals 通道）：清单保留、模式不变、文件不落
    #[tokio::test]
    async fn 计划审批_拒绝保留清单() {
        let core = plan_core();
        let s = core.create_session("计划拒绝").unwrap();
        core.set_plan_mode(&s.id, true);
        let orch = core.orchestrator.read().clone();
        let _ = orch
            .tools
            .execute(
                "a1",
                &s.id,
                &[crate::types::ToolName::Filesystem],
                crate::types::ToolName::Filesystem,
                &serde_json::json!({ "op": "write", "path": "q.txt", "content": "v" }),
            )
            .await;
        let ap_id = core.approval_list(Some("pending"), 10).unwrap()[0]
            .id
            .clone();
        core.approval_decide(&ap_id, false).await.unwrap();
        assert!(crate::plan::is_plan_mode(&s.id), "拒绝后仍在 plan 模式");
        assert_eq!(core.plan_list(&s.id).len(), 1, "清单保留");
        assert!(!core.config().workspace_root.join("q.txt").exists());
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// 空清单批准报错；模式位不影响读操作
    #[tokio::test]
    async fn 计划审批_空清单与读取放行() {
        let core = plan_core();
        let s = core.create_session("边界").unwrap();
        core.set_plan_mode(&s.id, true);
        assert!(core.plan_approve(&s.id).await.is_err(), "空清单应拒绝");
        let orch = core.orchestrator.read().clone();
        let r = orch
            .tools
            .execute(
                "a1",
                &s.id,
                &[crate::types::ToolName::Grep],
                crate::types::ToolName::Grep,
                &serde_json::json!({ "pattern": "fn " }),
            )
            .await;
        assert!(r.ok, "读操作放行：{:?}", r.error);
        // 清理：关模式并丢弃清单
        crate::plan::approve(&s.id);
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// 视频动作审批往返（[video] 前缀）：批准 = 回调代发执行器；拒绝 = 记录跳过
    #[tokio::test]
    async fn 视频动作审批_批准回调与拒绝跳过() {
        let core = plan_core();
        // 注入代发执行器（模拟网关侧注册；记录被调载荷）
        let called = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let sink = called.clone();
        core.register_video_act_executor(Arc::new(move |payload| {
            let sink = sink.clone();
            Box::pin(async move {
                sink.lock().push(payload);
                Ok("已代发：测试文本".into())
            })
        }));
        // 产一张 [video] 审批单（模拟 tool_act 的 create_act_approval）
        let req = crate::types::ApprovalRequest {
            id: format!("apr{}", &crate::types::new_id()[..14]),
            session_id: "s-video".into(),
            node_id: None,
            agent_id: "video".into(),
            command: "[video] {\"channelId\":\"bili\",\"videoId\":\"BV1\",\"text\":\"测试文本\"}"
                .into(),
            status: "pending".into(),
            result: None,
            created_at: crate::types::now_iso(),
            decided_at: None,
            origin_user: None,
            origin_role: None,
        
            meta_json: None,
        };
        core.store.add_approval(&req).unwrap();
        // 批准 → 执行器被调、审批单 executed
        let decided = core.approval_decide(&req.id, true).await.unwrap();
        assert_eq!(decided.status, "executed");
        assert!(decided.result.as_deref().unwrap_or("").contains("已代发"));
        assert_eq!(called.lock().len(), 1, "代发执行器应被调用一次");
        assert!(called.lock()[0].contains("BV1"));
        // 拒绝路径：再产一张 → denied，不触执行器
        let mut req2 = req.clone();
        req2.id = format!("apr{}", &crate::types::new_id()[..14]);
        req2.status = "pending".into();
        req2.decided_at = None;
        core.store.add_approval(&req2).unwrap();
        let denied = core.approval_decide(&req2.id, false).await.unwrap();
        assert_eq!(denied.status, "denied");
        assert_eq!(called.lock().len(), 1, "拒绝不应触代发");
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }
}

// ---------------- root 提权审批（agent-exec-confinement） ----------------

#[cfg(test)]
mod root_approval_tests {
    use super::*;

    fn root_core() -> Core {
        let dir = std::env::temp_dir().join(format!("exm-root-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        cfg.security.exec_user = "0".into(); // 数字 uid:约束开启(测试以 uid 0 近似)
        Core::with_config(cfg).expect("创建 Core 失败")
    }

    /// 提权命令无视白名单直达审批,审批单带 [root] 前缀
    #[tokio::test]
    async fn 提权命令_落root前缀审批单() {
        let core = root_core();
        let s = core.create_session("提权").unwrap();
        let orch = core.orchestrator.read().clone();
        // approval_wait_secs 默认 120,测试不等:落单后立即校验
        let _ = orch
            .tools
            .execute(
                "a1",
                &s.id,
                &[crate::types::ToolName::Terminal],
                crate::types::ToolName::Terminal,
                &serde_json::json!({ "command": "echo hello" }),
            )
            .await;
        // 直接走 gate 验证前缀语义(不经白名单拦截路径的等待)
        let ap = crate::types::ApprovalRequest {
            id: crate::types::new_id()[..8].to_string(),
            session_id: s.id.clone(),
            node_id: None,
            agent_id: "a1".into(),
            command: "[root] sudo id".into(),
            status: "pending".into(),
            result: None,
            created_at: crate::types::now_iso(),
            decided_at: None,
            origin_user: None,
            origin_role: None,
        
            meta_json: None,
        };
        core.store.add_approval(&ap).unwrap();
        // 批准 → 剥 sudo 壳以 root 执行 id
        let decided = core.approval_decide(&ap.id, true).await.unwrap();
        assert_eq!(decided.status, "executed");
        assert!(decided.result.as_deref().unwrap_or("").contains("uid="), "应执行 id 并带输出");
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// [root] 审批拒绝:受阻回流语义
    #[tokio::test]
    async fn 提权审批_拒绝回流() {
        let core = root_core();
        let ap = crate::types::ApprovalRequest {
            id: crate::types::new_id()[..8].to_string(),
            session_id: core.create_session("拒绝").unwrap().id,
            node_id: None,
            agent_id: "a1".into(),
            command: "[root] sudo rm -rf /".into(),
            status: "pending".into(),
            result: None,
            created_at: crate::types::now_iso(),
            decided_at: None,
            origin_user: None,
            origin_role: None,
        
            meta_json: None,
        };
        core.store.add_approval(&ap).unwrap();
        let decided = core.approval_decide(&ap.id, false).await.unwrap();
        assert_eq!(decided.status, "denied");
        assert_eq!(decided.result.as_deref(), Some("root 提权被拒绝"));
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// su 切换语义保留(不剥壳)
    #[tokio::test]
    async fn 提权审批_su语义保留() {
        let core = root_core();
        let ap = crate::types::ApprovalRequest {
            id: crate::types::new_id()[..8].to_string(),
            session_id: core.create_session("su").unwrap().id,
            node_id: None,
            agent_id: "a1".into(),
            command: "[root] su - nonexistentuser -c id".into(),
            status: "pending".into(),
            result: None,
            created_at: crate::types::now_iso(),
            decided_at: None,
            origin_user: None,
            origin_role: None,
        
            meta_json: None,
        };
        core.store.add_approval(&ap).unwrap();
        let decided = core.approval_decide(&ap.id, true).await.unwrap();
        // su 原样执行:用户不存在 → failed(但结果里应见 [root 执行] su 原文,证明未剥壳)
        let text = decided.result.as_deref().unwrap_or("");
        assert!(text.contains("[root 执行] su - nonexistentuser"), "su 语义保留: {text}");
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// approval_decide 代执行 cwd 回归:无身份约束时 cwd=workspace_root,
    /// 加固路径(shell_opts/bwrap)下 git 必须能识别仓库(/tmp 独立挂载桥接回归)
    #[tokio::test]
    async fn 审批代执行_cwd回归() {
        // 无 exec_user(约束关)——约束开启时 cwd 切 execHome 是另一语义,不在此测
        let dir = std::env::temp_dir().join(format!("exm-cwd-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        let core = Core::with_config(cfg).expect("创建 Core 失败");
        let ws = core.config().workspace_root.clone();
        std::process::Command::new("git").arg("init").current_dir(&ws).output().unwrap();
        // 直通参照
        let orch = core.orchestrator.read().clone();
        let eff = orch.tools.effective_root();
        eprintln!("ws={} eff={}", ws.display(), eff.display());
        let ap = crate::types::ApprovalRequest {
            id: crate::types::new_id()[..8].to_string(),
            session_id: core.create_session("cwd").unwrap().id,
            node_id: None,
            agent_id: "a1".into(),
            command: "git rev-parse --is-inside-work-tree".into(),
            status: "pending".into(),
            result: None,
            created_at: crate::types::now_iso(),
            decided_at: None,
            origin_user: None,
            origin_role: None,
        
            meta_json: None,
        };
        core.store.add_approval(&ap).unwrap();
        let decided = core.approval_decide(&ap.id, true).await.unwrap();
        eprintln!("decided={} result={:?}", decided.status, decided.result);
        assert_eq!(decided.status, "executed", "cwd 回归: {:?}", decided.result);
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// 普通审批单批准后走 shell_opts 加固路径(不降权环境下也不绕过沙箱)
    #[tokio::test]
    async fn 普通审批_批准后代执行仍受约束() {
        let core = root_core();
        let ap = crate::types::ApprovalRequest {
            id: crate::types::new_id()[..8].to_string(),
            session_id: core.create_session("普通").unwrap().id,
            node_id: None,
            agent_id: "a1".into(),
            command: "echo confined".into(),
            status: "pending".into(),
            result: None,
            created_at: crate::types::now_iso(),
            decided_at: None,
            origin_user: None,
            origin_role: None,
        
            meta_json: None,
        };
        core.store.add_approval(&ap).unwrap();
        let decided = core.approval_decide(&ap.id, true).await.unwrap();
        assert_eq!(decided.status, "executed");
        assert!(decided.result.as_deref().unwrap_or("").contains("confined"));
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }
}

/// 会话级审批授权（approval-ergonomics）：
/// 三档粒度 / 红线（[root]、破坏性永不入表）/ 会话删除清理 / gate 短路。
#[cfg(test)]
mod session_grant_tests {
    use super::*;
    use crate::types::ApprovalScope;

    fn grant_core() -> Core {
        let dir = std::env::temp_dir().join(format!("exm-grant-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        Core::with_config(cfg).expect("创建 Core 失败")
    }

    fn pending_ap(core: &Core, sid: &str, cmd: &str) -> crate::types::ApprovalRequest {
        let ap = crate::types::ApprovalRequest {
            id: crate::types::new_id()[..8].to_string(),
            session_id: sid.into(),
            node_id: None,
            agent_id: "a1".into(),
            command: cmd.into(),
            status: "pending".into(),
            result: None,
            created_at: crate::types::now_iso(),
            decided_at: None,
            origin_user: None,
            origin_role: None,
            meta_json: None,
        };
        core.store.add_approval(&ap).unwrap();
        ap
    }

    #[tokio::test]
    async fn session档_建立授权并命中() {
        let core = grant_core();
        let sid = core.create_session("授权").unwrap().id;
        let ap = pending_ap(&core, &sid, "echo hello");
        let decided = core
            .approval_decide_scoped(&ap.id, ApprovalScope::Session)
            .await
            .unwrap();
        assert_eq!(decided.status, "executed");
        assert!(decided.result.as_deref().unwrap_or("").contains("会话授权"));
        // 命中判定：前缀（echo hello）后接任意参数命中
        assert!(core.session_granted(&sid, "echo hello world").is_some());
        // 次 token 不同即不同类别（口径与白名单「命令+子命令」对齐）
        assert!(core.session_granted(&sid, "echo world").is_none());
        // 完全不同的命令不命中
        assert!(core.session_granted(&sid, "mkdir /tmp/x").is_none());
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    #[tokio::test]
    async fn once档_不建立授权() {
        let core = grant_core();
        let sid = core.create_session("单次").unwrap().id;
        let ap = pending_ap(&core, &sid, "mkdir -p /tmp/x");
        core.approval_decide_scoped(&ap.id, ApprovalScope::Once { approve: true })
            .await
            .unwrap();
        assert!(core.session_granted(&sid, "mkdir /tmp/y").is_none(), "once 档不建授权");
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    #[tokio::test]
    async fn root单_session档被降级() {
        let core = grant_core();
        let sid = core.create_session("红线").unwrap().id;
        let ap = pending_ap(&core, &sid, "[root] sudo id");
        let decided = core
            .approval_decide_scoped(&ap.id, ApprovalScope::Session)
            .await
            .unwrap();
        assert_eq!(decided.status, "executed");
        // 红线：[root] 单不允许升档——结果无 [会话授权] 注记（有 [红线] 降级说明）,也不建授权
        assert!(!decided.result.as_deref().unwrap_or("").contains("[会话授权]"));
        assert!(decided.result.as_deref().unwrap_or("").contains("[红线]"), "应注明降级");
        assert!(core.session_granted(&sid, "id").is_none());
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    #[tokio::test]
    async fn 破坏性命令_session档被降级() {
        let core = grant_core();
        let sid = core.create_session("破坏").unwrap().id;
        let ap = pending_ap(&core, &sid, "git push --force origin main");
        let decided = core
            .approval_decide_scoped(&ap.id, ApprovalScope::Session)
            .await
            .unwrap();
        // 执行本身按 once 发生(可能失败——force push 无远端;关键是档位被降)
        assert!(!decided.result.as_deref().unwrap_or("").contains("[会话授权]"));
        assert!(core.session_granted(&sid, "git push").is_none());
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    #[tokio::test]
    async fn allowlist档_写入配置() {
        let core = grant_core();
        let sid = core.create_session("白名单").unwrap().id;
        let ap = pending_ap(&core, &sid, "echo allowlist-test");
        let decided = core
            .approval_decide_scoped(&ap.id, ApprovalScope::Allowlist)
            .await
            .unwrap();
        assert!(decided.result.as_deref().unwrap_or("").contains("白名单"));
        // 前缀口径 = 命令名 + 首 token（echo allowlist-test）,与 command_head 一致
        assert!(core
            .config()
            .security
            .exec_allowlist
            .iter()
            .any(|p| p == "echo allowlist-test"), "前缀应写入 execAllowlist");
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    #[tokio::test]
    async fn 会话删除后授权失效() {
        let core = grant_core();
        let sid = core.create_session("删除").unwrap().id;
        core.grant_session(&sid, "cargo build");
        assert!(core.session_granted(&sid, "cargo build --release").is_some());
        core.delete_session(&sid).unwrap();
        assert!(core.session_granted(&sid, "cargo build --release").is_none());
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// gate_command 短路：探针注入 + 授权命中 → 不落审批单直接执行
    #[tokio::test]
    async fn gate短路_授权命中不落单() {
        let core = grant_core();
        let sid = core.create_session("短路").unwrap().id;
        // 授权前缀与 command_head 口径一致（echo + 首参数 token）
        core.grant_session(&sid, "echo granted-ok");
        // 探针注入（生产由 attach_session_grant_probe 完成;测试手动）
        let core = std::sync::Arc::new(core);
        core.attach_session_grant_probe();
        let orch = core.orchestrator.read().clone();
        let r = orch.tools.gate_command("a1", &sid, "echo granted-ok").await;
        assert!(r.ok, "授权直通应执行成功: {:?}", r.output);
        assert!(r.output.contains("granted-ok"));
        // 无新审批单产生
        let pendings = core.approval_list(Some("pending"), 10).unwrap();
        assert!(pendings.is_empty(), "授权命中不应落审批单");
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }
}

/// [meta] 提议单全链路（meta-agent）：落单 → 批准应用 / 拒绝回流 → 各 kind 语义
#[cfg(test)]
mod meta_approval_tests {
    use super::*;

    /// 自定义组场景（[meta] 的 prompt/roster/build 只对非内置组生效）：
    /// 创组 → upsert 成员（含提示词）→ 切激活
    fn meta_core() -> Core {
        let dir = std::env::temp_dir().join(format!("exm-meta-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        let core = Core::with_config(cfg).expect("创建 Core 失败");
        {
            let reg = &core.orchestrator.read().registry;
            reg.create_group(Some("wg-test".into()), "测试组", "meta 测试用")
                .unwrap();
            reg.upsert_agent(
                "wg-test",
                crate::types::AgentDefinition {
                    identifier: "worker".into(),
                    name: "worker".into(),
                    domain: "通用".into(),
                    tier: crate::types::Tier::Unit,
                    description: "测试成员".into(),
                    capabilities: vec![],
                    tools: vec![],
                    when_to_call: String::new(),
                    dependencies: vec![],
                    composable_with: vec![],
                    input_schema: crate::types::AgentIoSchema::default(),
                    output_schema: crate::types::AgentIoSchema::default(),
                    prompt_file: String::new(),
                    model_hint: None,
                    link: None,
                    max_steps: None,
                },
                Some("# worker\n\n测试提示词。".into()),
            )
            .unwrap();
            reg.set_active_group("wg-test").unwrap();
        }
        core
    }

    fn meta_ap(
        _core: &Core,
        session: &str,
        kind: &str,
        target: &str,
        change: serde_json::Value,
        rationale: &str,
    ) -> crate::types::ApprovalRequest {
        let p = crate::tools::MetaProposal {
            kind: kind.into(),
            target: target.into(),
            change,
            rationale: rationale.into(),
        };
        crate::types::ApprovalRequest {
            id: crate::types::new_id()[..8].to_string(),
            session_id: session.into(),
            node_id: None,
            agent_id: "meta".into(),
            command: format!("[meta] {kind} {target}: {rationale}"),
            status: "pending".into(),
            result: None,
            created_at: crate::types::now_iso(),
            decided_at: None,
            origin_user: None,
            origin_role: None,
            meta_json: Some(serde_json::to_string(&p).unwrap()),
        }
    }

    /// adaptation 类：批准 → save_adaptation 版本化（rev 递增）
    #[tokio::test]
    async fn meta提议_adaptation_批准后版本化沉淀() {
        let core = meta_core();
        let s = core.create_session("meta-adapt").unwrap();
        let ap = meta_ap(
            &core,
            &s.id,
            "adaptation",
            "worker",
            serde_json::json!({ "content": "- 高失败时优先降步验证,一次只改一个变量" }),
            "orchestrator 连续 3 次 edit 失败,失败模式重复",
        );
        core.store.add_approval(&ap).unwrap();
        let decided = core.approval_decide(&ap.id, true).await.unwrap();
        assert_eq!(decided.status, "executed");
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// 拒绝 → denied 回流,不落任何改动
    #[tokio::test]
    async fn meta提议_拒绝回流() {
        let core = meta_core();
        let s = core.create_session("meta-deny").unwrap();
        let ap = meta_ap(
            &core,
            &s.id,
            "adaptation",
            "worker",
            serde_json::json!({ "content": "- 不应被写入的内容" }),
            "测试拒绝路径",
        );
        core.store.add_approval(&ap).unwrap();
        let decided = core.approval_decide(&ap.id, false).await.unwrap();
        assert_eq!(decided.status, "denied");
        assert!(core
            .orchestrator()
            .registry
            .load_adaptation("worker")
            .unwrap()
            .is_none());
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// prompt 类:批准 → 组内提示词追加锚点补段 + history 备份
    #[tokio::test]
    async fn meta提议_prompt_补段与备份() {
        let core = meta_core();
        let s = core.create_session("meta-prompt").unwrap();
        // 主智能体在激活组内(prompts/ 是组目录约定)
        let ap = meta_ap(
            &core,
            &s.id,
            "prompt",
            "worker",
            serde_json::json!({ "promptPatch": "编码任务先读相邻文件再动手,失败两次必须换路径" }),
            "工具统计显示 edit 失败集中于未读上下文",
        );
        core.store.add_approval(&ap).unwrap();
        let decided = core.approval_decide(&ap.id, true).await.unwrap();
        assert_eq!(decided.status, "executed");
        assert!(decided.result.as_deref().unwrap_or("").contains("锚点"));
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// 内置组保护:roster/build 类对内置组一律 bail(提议单应用被拒)
    #[tokio::test]
    async fn meta提议_内置组保护() {
        let core = meta_core();
        core.orchestrator.read()
            .registry
            .set_active_group("exmachina")
            .unwrap();
        let s = core.create_session("meta-builtin").unwrap();
        let ap = meta_ap(
            &core,
            &s.id,
            "roster",
            "orchestrator",
            serde_json::json!({ "ops": [{ "op": "remove", "identifier": "orchestrator" }] }),
            "测试内置组保护",
        );
        core.store.add_approval(&ap).unwrap();
        let decided = core.approval_decide(&ap.id, true).await.unwrap();
        assert_eq!(decided.status, "failed");
        assert!(decided
            .result
            .as_deref()
            .unwrap_or("")
            .contains("内置组"));
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// build 类:批准 → 注册新个体(组内可见)
    #[tokio::test]
    async fn meta提议_build_注册新个体() {
        let core = meta_core();
        let s = core.create_session("meta-build").unwrap();
        let ap = meta_ap(
            &core,
            &s.id,
            "build",
            "builder",
            serde_json::json!({
                "asGroup": false,
                "definition": { "name": "builder", "domain": "通用", "tier": "unit", "description": "构建出的新个体" },
                "prompt": "你是构建出的新个体,负责数据整理与汇报。"
            }),
            "数据整理任务无适配个体,重复阻塞 4 次",
        );
        core.store.add_approval(&ap).unwrap();
        let decided = core.approval_decide(&ap.id, true).await.unwrap();
        assert_eq!(decided.status, "executed");
        assert!(core
            .orchestrator()
            .registry
            .get("builder")
            .is_some(), "新个体应注册进激活组");
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// roster 类:批准 → 两阶段应用(add 后 update)
    #[tokio::test]
    async fn meta提议_roster_两阶段应用() {
        let core = meta_core();
        let s = core.create_session("meta-roster").unwrap();
        let ap = meta_ap(
            &core,
            &s.id,
            "roster",
            "worker",
            serde_json::json!({ "ops": [
                { "op": "add", "identifier": "scout", "name": "scout", "description": "roster 新增" },
                { "op": "update", "identifier": "worker", "name": "worker", "description": "职责更新:数据整理" }
            ]}),
            "能力缺口:侦察类任务无个体承接",
        );
        core.store.add_approval(&ap).unwrap();
        let decided = core.approval_decide(&ap.id, true).await.unwrap();
        assert_eq!(decided.status, "executed");
        let reg = &core.orchestrator().registry;
        assert!(reg.get("scout").is_some(), "scout 应被加入");
        assert!(reg
            .get("worker")
            .map(|d| d.description.contains("数据整理"))
            .unwrap_or(false), "worker 描述应被更新");
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }
}

/// 会话来源派生（session-management）：title 解析 + flow join 口径。
#[cfg(test)]
mod session_origin_tests {
    use super::*;
    use crate::types::ChannelFlow;

    fn core() -> Core {
        let dir = std::env::temp_dir().join(format!("exm-origin-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = ExmConfig::load(&dir);
        Core::with_config(cfg).expect("创建 Core 失败")
    }

    fn flow(id: &str, key: &str, name: &str) -> ChannelFlow {
        ChannelFlow {
            id: format!("{id}:{key}"),
            channel_id: id.into(),
            chat_key: key.into(),
            display_name: name.into(),
            is_group: true,
            active_session: None,
            last_message: String::new(),
            last_active_at: String::new(),
            created_at: String::new(),
        }
    }

    #[test]
    fn 列表补齐_通道会话带来源与流名() {
        let c = core();
        // 流存在（display_name 提供友好名）
        c.save_channel_flow(&flow("c-tg", "123", "技术群")).unwrap();
        c.create_session("channel:c-tg:123").unwrap();
        c.create_session("channel:c-tg:123:20251101").unwrap();
        // 流不在 flow 表（已删流）：回落 chat_key
        c.create_session("channel:c-qq:g9").unwrap();
        c.create_session("控制台随手记").unwrap();

        let list = c.list_sessions_admin().unwrap();
        let by_title: std::collections::HashMap<_, _> =
            list.iter().map(|s| (s.title.clone(), s.clone())).collect();

        let bare = &by_title["channel:c-tg:123"];
        assert_eq!(bare.origin.as_deref(), Some("channel:c-tg:123"));
        assert_eq!(bare.origin_label.as_deref(), Some("技术群"));

        let labeled = &by_title["channel:c-tg:123:20251101"];
        assert_eq!(labeled.origin.as_deref(), Some("channel:c-tg:123"));
        assert_eq!(labeled.origin_label.as_deref(), Some("技术群·20251101"));

        let orphan = &by_title["channel:c-qq:g9"];
        assert_eq!(orphan.origin_label.as_deref(), Some("g9"), "无流记录回落 chat_key");

        let console = &by_title["控制台随手记"];
        assert_eq!(console.origin.as_deref(), Some("console"));
        assert!(console.origin_label.is_none(), "控制台会话 label 缺省");
        let _ = std::fs::remove_dir_all(&c.config().workspace_root);
    }

    #[test]
    fn 归档列表_同样补齐() {
        let c = core();
        c.save_channel_flow(&flow("c-x", "k1", "测试流")).unwrap();
        let s = c.create_session("channel:c-x:k1").unwrap();
        c.set_session_archived(&s.id, true).unwrap();
        let archived = c.list_archived_sessions().unwrap();
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].origin.as_deref(), Some("channel:c-x:k1"));
        assert_eq!(archived[0].origin_label.as_deref(), Some("测试流"));
        let _ = std::fs::remove_dir_all(&c.config().workspace_root);
    }
}

/// 闲置归档与按流归档（session-management 生命周期治理）。
#[cfg(test)]
mod flow_archive_tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn core() -> Core {
        let dir = std::env::temp_dir().join(format!("exm-idle-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        Core::with_config(ExmConfig::load(&dir)).expect("创建 Core 失败")
    }

    fn at(days_ago: i64) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 4, 4, 0, 0).unwrap() - chrono::Duration::days(days_ago)
    }

    /// 把会话 updated_at 拨回 N 天前（直接改存储行）
    fn age(c: &Core, id: &str, days: i64) {
        let iso = (Utc.with_ymd_and_hms(2026, 11, 4, 4, 0, 0).unwrap()
            - chrono::Duration::days(days))
            .to_rfc3339();
        c.store.set_session_active_at(id, &iso).unwrap();
    }

    #[test]
    fn 闲置归档_阈值与豁免() {
        let c = core();
        let s_old = c.create_session("channel:c-tg:g1").unwrap();
        let s_fresh = c.create_session("channel:c-tg:g2").unwrap();
        let s_bound = c.create_session("channel:c-tg:g3").unwrap();
        let s_console = c.create_session("控制台会话").unwrap();
        c.bind_flow_session("c-tg", "g3", &s_bound.id).unwrap();
        age(&c, &s_old.id, 10);
        age(&c, &s_bound.id, 10); // 绑定会话同样老旧 → 仍须豁免
        age(&c, &s_console.id, 10);

        let now = at(0);
        let (archived, skipped) = c.archive_idle_flow_sessions(7, now).unwrap();
        assert_eq!(archived, 1, "只有闲置非绑定流会话归档");
        assert_eq!(skipped, 1, "绑定会话豁免计数");
        let archived_list = c.list_archived_sessions().unwrap();
        assert_eq!(archived_list.len(), 1);
        assert_eq!(archived_list[0].id, s_old.id);
        // 新鲜流会话与控制台会话不受影响
        assert!(c.store.get_session(&s_fresh.id).unwrap().unwrap().archived_at.is_none());
        assert!(c.store.get_session(&s_console.id).unwrap().unwrap().archived_at.is_none());
        let _ = std::fs::remove_dir_all(&c.config().workspace_root);
    }

    #[test]
    fn 闲置归档_零天关闭() {
        let c = core();
        let s = c.create_session("channel:c-x:g").unwrap();
        age(&c, &s.id, 365);
        let (archived, _) = c.archive_idle_flow_sessions(0, at(0)).unwrap();
        assert_eq!(archived, 0, "0 = 关闭零行为");
        assert!(c.store.get_session(&s.id).unwrap().unwrap().archived_at.is_none());
        let _ = std::fs::remove_dir_all(&c.config().workspace_root);
    }

    #[test]
    fn 按流归档_绑定豁免与范围圈定() {
        let c = core();
        let f1_active = c.create_session("channel:c-a:g1").unwrap();
        let f1_old = c.create_session("channel:c-a:g1:旧话题").unwrap();
        let f2 = c.create_session("channel:c-a:g2").unwrap();
        let other = c.create_session("channel:c-b:g1").unwrap();
        c.bind_flow_session("c-a", "g1", &f1_active.id).unwrap();

        let (archived, skipped) = c
            .archive_flow_sessions(crate::session_origin::ArchiveScope::Flow {
                channel_id: "c-a".into(),
                chat_key: "g1".into(),
            })
            .unwrap();
        assert_eq!((archived, skipped), (1, 1), "流内旧会话归档、绑定豁免");
        assert!(c.store.get_session(&f1_old.id).unwrap().unwrap().archived_at.is_some());
        assert!(c.store.get_session(&f1_active.id).unwrap().unwrap().archived_at.is_none());
        // 范围外不受影响
        assert!(c.store.get_session(&f2.id).unwrap().unwrap().archived_at.is_none());
        assert!(c.store.get_session(&other.id).unwrap().unwrap().archived_at.is_none());
        let _ = std::fs::remove_dir_all(&c.config().workspace_root);
    }

    #[test]
    fn 按通道归档_全流旧会话() {
        let c = core();
        let a1 = c.create_session("channel:c-a:g1:old1").unwrap();
        let a2 = c.create_session("channel:c-a:g2:old2").unwrap();
        let bound = c.create_session("channel:c-a:g2").unwrap();
        let b = c.create_session("channel:c-b:g1").unwrap();
        c.bind_flow_session("c-a", "g2", &bound.id).unwrap();

        let (archived, skipped) = c
            .archive_flow_sessions(crate::session_origin::ArchiveScope::Channel {
                channel_id: "c-a".into(),
            })
            .unwrap();
        assert_eq!((archived, skipped), (2, 1));
        assert!(c.store.get_session(&a1.id).unwrap().unwrap().archived_at.is_some());
        assert!(c.store.get_session(&a2.id).unwrap().unwrap().archived_at.is_some());
        assert!(c.store.get_session(&bound.id).unwrap().unwrap().archived_at.is_none());
        assert!(c.store.get_session(&b.id).unwrap().unwrap().archived_at.is_none());
        let _ = std::fs::remove_dir_all(&c.config().workspace_root);
    }
}

/// 一次性标记（once_flags）：预算内联提示去重等
#[cfg(test)]
mod once_flag_tests {
    use super::*;

    #[test]
    fn 一次性标记_首次置位后续拒绝() {
        let dir = std::env::temp_dir().join(format!("exm-once-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = Core::with_config(ExmConfig::load(&dir)).expect("创建 Core 失败");
        let key = "budget-notified:s1";
        assert!(c.consume_once_flag(key), "首次返回 true");
        assert!(!c.consume_once_flag(key), "后续返回 false");
        assert!(!c.consume_once_flag(key));
        assert!(c.consume_once_flag("budget-notified:s2"), "不同 key 互不影响");
        let _ = std::fs::remove_dir_all(&c.config().workspace_root);
    }
}
