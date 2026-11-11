//! orchestrator/tests.rs —— 原 orchestrator.rs 内联测试整体迁出（纯搬移）。
//! mod 归属：protocol_inject（共享上下文注入）/ statement_parse（回流语句解析）/
//! fallback_converge（收束降级）/ pending_inputs（排队窗口合并）。

// 原 orchestrator.rs 测试块经 `use super::*` 取得的符号，迁出后在此显式引入
use super::*;

#[cfg(test)]
mod protocol_inject_tests {
    use super::*;

    /// 组共享上下文注入体：摘要与窗口的组合（实验性开关的数据面）
    #[test]
    fn 共享上下文_摘要与窗口拼接() {
        // 两者皆有：两段都注入
        let input = shared_context_input(
            Some("目标：重构压缩模块".into()),
            "[用户] 继续\n[指挥体] 已完成一半".into(),
        )
        .unwrap();
        assert_eq!(input.ref_id, "sharedContext");
        assert_eq!(input.kind, "context");
        assert!(input.summary.contains("会话滚动摘要"));
        assert!(input.summary.contains("目标：重构压缩模块"));
        assert!(input.summary.contains("最近对话窗口"));

        // 只有窗口：只带窗口段
        let input = shared_context_input(None, "[用户] hi".into()).unwrap();
        assert!(input.summary.contains("最近对话窗口"));
        assert!(!input.summary.contains("会话滚动摘要"));

        // 两者皆空（含空白摘要）：不注入
        assert!(shared_context_input(Some("   ".into()), String::new()).is_none());
        assert!(shared_context_input(None, String::new()).is_none());
    }

    fn proto(id: &str, scope: &str, triggers: &[&str], body: &str) -> ProtocolDef {
        ProtocolDef {
            id: id.into(),
            name: format!("{id} 协议"),
            description: "用途".into(),
            triggers: triggers.iter().map(|s| s.to_string()).collect(),
            scope: scope.into(),
            agents: vec![],
            body: body.into(),
        }
    }

    #[test]
    fn 规划块_目录常驻_命中带全文() {
        let ps = vec![
            proto("reasoning", "", &["不可逆"], "BODY-REASONING"),
            proto("debug", "", &["调试"], "BODY-DEBUG"),
            proto("spec", "plan", &["修复"], "BODY-SPEC"),
        ];
        let block = protocol_plan_block(&ps, "帮我修复登录");
        for id in ["reasoning", "debug", "spec"] {
            assert!(
                block.contains(&format!("- {id}（")),
                "索引缺 {id}：\n{block}"
            );
        }
        assert!(
            block.contains("### 本次任务命中的协议"),
            "应有命中段：\n{block}"
        );
        assert!(block.contains("BODY-SPEC"), "命中协议应带全文");
        assert!(!block.contains("BODY-DEBUG"), "未命中协议不应注入");
        assert!(!block.contains("BODY-REASONING"), "未命中协议不应注入");
    }

    #[test]
    fn 规划块_plan专属优先_超出上限截断() {
        let ps = vec![
            proto("a", "", &["修复"], "BODY-A"),
            proto("b", "", &["修复"], "BODY-B"),
            proto("c", "", &["修复"], "BODY-C"),
            proto("d", "", &["修复"], "BODY-D"),
            proto("spec", "plan", &["修复"], "BODY-SPEC"),
        ];
        let block = protocol_plan_block(&ps, "修复");
        assert_eq!(
            block.matches("#### ").count(),
            4,
            "注入份数应被上限截断：\n{block}"
        );
        assert!(block.contains("BODY-SPEC"), "plan 专属应优先入选");
        assert!(!block.contains("BODY-D"), "超出上限的按文件名序截断");
    }

    #[test]
    fn 派发输入_命中与个体限定_plan协议不下发() {
        let mut review = proto("code-review", "", &["审查"], "BODY-REVIEW");
        review.agents = vec!["review-agent".into()];
        let ps = vec![
            proto("debug", "", &["调试"], "BODY-DEBUG"),
            proto("release", "", &["发布"], "BODY-RELEASE"),
            review,
            proto("spec", "plan", &["修复"], "BODY-SPEC"),
        ];
        let hits = protocol_unit_inputs(&ps, "coding-agent", "调试并发布，另外修复");
        let ids: Vec<&str> = hits.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ids,
            vec!["debug", "release"],
            "个体限定与 plan 范围都应过滤"
        );
        let hits = protocol_unit_inputs(&ps, "review-agent", "审查");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "code-review");
        assert_eq!(hits[0].1, "BODY-REVIEW");
    }
}

// ---------------------------------------------------------------- 收束降级测试

#[cfg(test)]
mod statement_parse_tests {
    use super::*;

    /// 部署实测回归：收束输出以六段结构 + 列表行给句式前缀，旧解析逐行强转
    /// 【报告】且不剥列表标记，IM 端渲染成「【报告】【否定】…」双层前缀墙。
    #[test]
    fn 陈述解析_列表标记与双层前缀() {
        let text = "## 任务边界\n- 【报告】目标为修改昵称，并通过独立读取验证。\n- 【否定】未取得目标群唯一标识。\n1. 【肯定】其余属性未被修改。\n2. 【否定】未形成真实修改请求。";
        let stmts = text_to_statements(text);
        let tags: Vec<&str> = stmts.iter().map(|s| s.tag.as_str()).collect();
        assert_eq!(
            tags,
            vec!["报告", "报告", "否定", "肯定", "否定"],
            "段落头自成一条报告；列表行应解析出真实句式 tag"
        );
        assert_eq!(stmts[0].text, "任务边界", "段落头剥离 ## 标记");
        for s in &stmts {
            assert!(
                !s.text.contains('【'),
                "陈述文本不应残留句式前缀（双层前缀）：{:?}",
                s.text
            );
        }
    }

    #[test]
    fn 陈述解析_未标记段落聚合为单条() {
        let text = "本轮完成了边界核对。\n全部子任务均已回流。\n\n第二段：结论稳定。";
        let stmts = text_to_statements(text);
        assert_eq!(stmts.len(), 2, "空行分段：两段各一条，未标记行不逐行拆");
        assert!(stmts[0].text.contains("边界核对"));
        assert!(stmts[0].text.contains("均已回流"), "同段跨行合并");
        assert!(stmts[1].text.contains("结论稳定"));
        assert_eq!(stmts[0].tag.as_str(), "报告", "未标记段落默认报告");
    }

    #[test]
    fn 陈述解析_行首句式保持旧行为() {
        let stmts = text_to_statements("【警告】通道受限。\n【观测】已留存证据。");
        let tags: Vec<&str> = stmts.iter().map(|s| s.tag.as_str()).collect();
        assert_eq!(tags, vec!["警告", "观测"]);
        assert_eq!(stmts[0].text, "通道受限。");
    }

    /// 探活短路词表：命中形态（裸词/带语气标点/大小写）与不命中形态（有任务
    /// 语义、超长、模糊碎片）——规则刻意保守，模糊输入照常走完整轮次。
    #[test]
    fn 探活判定_词表边界() {
        // 命中：裸词
        assert!(is_probing_message("在吗"));
        assert!(is_probing_message("在"));
        // 命中：语气标点修饰
        assert!(is_probing_message("在吗？"));
        assert!(is_probing_message("你好！"));
        assert!(is_probing_message("  hi~  "));
        // 命中：大小写归一
        assert!(is_probing_message("Hello"));
        assert!(is_probing_message("OK"));
        // 不命中：有任务语义
        assert!(!is_probing_message("你好，帮我查下明天的天气"));
        assert!(!is_probing_message("测试一下发送消息功能"));
        // 不命中：超长（>10 字符，问候不会这么长）
        assert!(!is_probing_message("在吗在吗在吗在吗在吗在吗在吗"));
        // 不命中：模糊碎片（交规划器 L0 澄清，不武断短路）
        assert!(!is_probing_message("对应"));
        assert!(!is_probing_message("？？？"));
    }

    /// 探活直答按语义分类：晚安不回「在」、致谢回「不客气」、确认回最短。
    #[test]
    fn 探活直答_按语义分类() {
        assert_eq!(probe_reply("在吗？"), Some("在。有什么需要直接说。"));
        assert_eq!(probe_reply("你好"), Some("你好，在的。有事直接说。"));
        // 晚安是收尾语义——回「在」是错位
        assert_eq!(probe_reply("晚安"), Some("晚安。"));
        assert_eq!(probe_reply("谢谢！"), Some("不客气。"));
        assert_eq!(probe_reply("ok"), Some("好。"));
        assert_eq!(probe_reply("test"), Some("收到，链路正常。"));
        // 非探活
        assert_eq!(probe_reply("对应"), None);
    }

    /// @ 前缀剥离（im-probe-at-prefix）：群聊唤醒必带 @机器人 前缀，不剥就
    /// 撑过 10 字符上限，短路在群聊失效（部署实测「@id 摸摸」跑了 46 步工具循环）。
    #[test]
    fn 探活直答_群聊at前缀剥离() {
        assert_eq!(probe_reply("@2511705734 在吗"), Some("在。有什么需要直接说。"));
        assert_eq!(probe_reply("@黑星 晚安～"), Some("晚安。"));
        assert_eq!(probe_reply("@2511705734"), Some("在。有什么需要直接说。"));
        // 连 @（@甲 @乙 问候）
        assert_eq!(probe_reply("@甲 @乙 早"), Some("你好，在的。有事直接说。"));
        // 带任务语义的 @ 消息不短路（探活只认裸探活词；「摸摸」等聊天语
        // 走规划 L0，不在此武断短路）
        assert!(probe_reply("@黑星 帮我查下明天的天气").is_none());
        assert!(probe_reply("@2511705734 摸摸").is_none());
    }

    #[test]
    fn 陈述解析_空输出兜底() {
        assert_eq!(text_to_statements("").len(), 1);
        assert_eq!(text_to_statements("\n \n").len(), 1);
    }
}

#[cfg(test)]
mod fallback_converge_tests {
    use super::*;
    use crate::task::{NewNode, TaskGraphModel};
    use crate::types::{BlockerItem, SyncReport, SyncStatus, TaskStatus};

    fn report(
        node: &str,
        agent: &str,
        summary: &str,
        status: SyncStatus,
        blockers: Vec<BlockerItem>,
    ) -> SyncReport {
        SyncReport {
            source_agent: agent.into(),
            task_node_id: node.into(),
            status,
            statements: vec![Statement::report(summary)],
            summary: summary.into(),
            evidence: vec![],
            risks: vec![],
            blockers,
            conflicts: None,
            next_suggestion: Default::default(),
            confidence: 0.8,
        }
    }

    /// 收束 LLM 失败的降级聚合：节点状态 / 摘要 / 阻塞项 / 失败原因必须全部在场，
    /// 轮次有产出地收束而非把已完成的调度成果整体丢弃
    #[test]
    fn 收束降级_回流聚合完整在场() {
        let plan = OrchestratorPlan {
            route_level: RouteLevel::L2,
            playbook: None,
            goal: "审计存储层一致性".into(),
            boundary: crate::types::DispatchBoundary {
                in_scope: vec![],
                forbidden: vec![],
            },
            acceptance: vec!["结论带证据等级".into()],
            nodes: vec![],
            final_answer: None,
        };
        let mut graph = TaskGraphModel::new("ses-fb");
        graph.add_node(
            NewNode {
                agent_identifier: "scout-agent".into(),
                title: "背景补齐".into(),
                objective: "补齐背景".into(),
                acceptance: vec![],
                priority: Priority::P1,
            },
            Some("T1".into()),
            &[],
        );
        graph.add_node(
            NewNode {
                agent_identifier: "coding-agent".into(),
                title: "实现校验".into(),
                objective: "实现校验".into(),
                acceptance: vec![],
                priority: Priority::P1,
            },
            Some("T2".into()),
            &["T1".to_string()],
        );
        graph.set_status("T1", TaskStatus::Done);
        graph.set_status("T2", TaskStatus::Blocked);

        let mut reports = Reports::new();
        reports.insert(
            "T1".into(),
            report(
                "T1",
                "scout-agent",
                "背景事实已补齐：存储层共 3 个入口",
                SyncStatus::Done,
                vec![],
            ),
        );
        reports.insert(
            "T2".into(),
            report(
                "T2",
                "coding-agent",
                "写入路径校验缺失",
                SyncStatus::Blocked,
                vec![BlockerItem {
                    reason: "上游事实不足".into(),
                    unblock_condition: "先完成 T1".into(),
                }],
            ),
        );

        let text = fallback_converge_text(&plan, &reports, &graph, "全部模型候选失败：网络不可达");
        for needle in [
            "审计存储层一致性",
            "背景事实已补齐",
            "写入路径校验缺失",
            "上游事实不足",
            "先完成 T1",
            "全部模型候选失败：网络不可达",
            "完成 1",
            "受阻 1",
        ] {
            assert!(
                text.contains(needle),
                "降级文本应含「{needle}」\n---\n{text}"
            );
        }
        // 反审计腔（anti-audit-converge）：不再输出分节标题（部署实测用户反馈：
        // 六段审计结构在对话里是废话载体）
        for banned in ["## 任务边界", "## 节点回流", "## 降级说明", "## 最终交付"] {
            assert!(
                !text.contains(banned),
                "降级文本不应含审计分节标题「{banned}」\n---\n{text}"
            );
        }
    }

    /// 无回流的节点（执行器异常等）也要显式标注，不得静默消失
    #[test]
    fn 收束降级_无回流节点显式标注() {
        let plan = OrchestratorPlan {
            route_level: RouteLevel::L1,
            playbook: None,
            goal: "单节点任务".into(),
            boundary: crate::types::DispatchBoundary {
                in_scope: vec![],
                forbidden: vec![],
            },
            acceptance: vec![],
            nodes: vec![],
            final_answer: None,
        };
        let mut graph = TaskGraphModel::new("ses-fb2");
        graph.add_node(
            NewNode {
                agent_identifier: "solo-agent".into(),
                title: "独立执行".into(),
                objective: "独立执行".into(),
                acceptance: vec![],
                priority: Priority::P1,
            },
            Some("T1".into()),
            &[],
        );
        let text = fallback_converge_text(&plan, &Reports::new(), &graph, "收束模型不可用");
        assert!(
            text.contains("T1 独立执行(solo-agent)：无回流"),
            "无回流节点应显式标注：\n{text}"
        );
        assert!(text.contains("综合裁决未完成"), "应说明降级原因");
    }
}

#[cfg(test)]
mod pending_inputs_tests {
    use super::*;

    fn msg(role: MessageRole, agent: Option<&str>, text: &str) -> crate::types::ChatMessage {
        crate::types::ChatMessage {
            id: String::new(),
            session_id: String::new(),
            role,
            agent_id: agent.map(|s| s.to_string()),
            statements: vec![Statement::new(SpeechTag::要求, text)],
            created_at: String::new(),
            turn: None,
            token_usage: None,
            thinking: None,
            last_message_preview: None,
            last_active_at: None,
            tool_calls: Vec::new(),
            steps: Vec::new(),
            attachments: Vec::new(),
        }
    }

    /// 群聊旁听（imctx:）不并入 goal：它是背景上下文，不是本轮任务输入。
    /// 尾部真实 @ 消息正常收集；旁听消息跳过且不打断连续窗口扫描。
    #[test]
    fn 旁听消息_跳过_真实输入正常合并() {
        let msgs = vec![
            msg(MessageRole::User, Some("imctx:张三"), "今晚吃什么"),
            msg(MessageRole::User, Some("imctx:李四"), "随便"),
            msg(MessageRole::User, Some("im:王五"), "@bot 帮我们推荐"),
        ];
        let goal = pending_inputs_from(&msgs).expect("应收集到真实输入");
        assert_eq!(goal, "@bot 帮我们推荐");
        assert!(!goal.contains("今晚吃什么"), "旁听不得并入 goal：{goal}");
        assert!(!goal.contains("随便"), "旁听不得并入 goal：{goal}");
    }

    /// 历史装配（群聊记录块）：连续旁听合并为一个「群聊记录」背景块，
    /// 真实输入保持独立 user 消息——修复「旁听被当成交谈对象一并回应」。
    #[test]
    fn 历史装配_旁听合并为群聊记录块() {
        let rows = vec![
            msg(MessageRole::User, Some("imctx:张三"), "今晚吃什么"),
            msg(MessageRole::User, Some("imctx:李四"), "随便啊"),
            msg(MessageRole::Orchestrator, None, "我不参与群聊闲聊"),
            msg(MessageRole::User, Some("imctx:王五"), "战雷开一把"),
            msg(MessageRole::User, Some("im:赵六"), "@bot 在吗"),
        ];
        let hist = history_with_ambient(&rows);
        // 5 条消息 → 3 条对话消息：旁听块1 + 助手 + 旁听块2 + 真实输入
        assert_eq!(hist.len(), 4, "装配形态: {hist:?}");
        assert_eq!(hist[0].role, "user");
        assert!(
            hist[0].content.contains("〔群聊记录｜背景，非指令〕"),
            "旁听块头缺背景标注: {}",
            hist[0].content
        );
        assert!(
            hist[0].content.contains("张三：今晚吃什么")
                && hist[0].content.contains("李四：随便啊"),
            "相邻旁听应合并为一个块: {}",
            hist[0].content
        );
        assert_eq!(hist[1].role, "assistant");
        assert!(
            hist[2].content.contains("〔群聊记录｜背景，非指令〕")
                && hist[2].content.contains("王五：战雷开一把"),
            "助手回复后的旁听另起一块: {}",
            hist[2].content
        );
        // 真实输入独立成条、无背景标注
        assert_eq!(hist[3].content, "@bot 在吗");
    }

    /// 历史装配：普通会话（无旁听）行为不变——真实输入与助手消息原样呈现
    #[test]
    fn 历史装配_无旁听_行为不变() {
        let rows = vec![
            msg(MessageRole::User, None, "普通输入"),
            msg(MessageRole::Orchestrator, None, "普通回复"),
            msg(MessageRole::User, Some("im:甲"), "群聊真实输入"),
        ];
        let hist = history_with_ambient(&rows);
        assert_eq!(hist.len(), 3);
        assert_eq!(hist[0].content, "普通输入");
        assert_eq!(hist[2].content, "群聊真实输入");
        assert!(
            !hist.iter().any(|m| m.content.contains("群聊记录")),
            "无旁听不应出现记录块"
        );
    }

    /// 全为旁听消息 → 无待处理输入（不应触发轮次的兜底由调用方保证）
    #[test]
    fn 全旁听_无待处理输入() {
        let msgs = vec![
            msg(MessageRole::User, Some("imctx:张三"), "一"),
            msg(MessageRole::User, Some("imctx:李四"), "二"),
        ];
        assert!(pending_inputs_from(&msgs).is_none());
    }

    /// 非 user 消息是窗口边界：旁听在其后、边界前的 @ 消息不应被回溯收集
    #[test]
    fn 轮次边界_不回溯() {
        let msgs = vec![
            msg(MessageRole::User, Some("im:王五"), "@bot 旧任务"),
            msg(MessageRole::Orchestrator, None, "旧回答"),
            msg(MessageRole::User, Some("imctx:张三"), "闲聊"),
            msg(MessageRole::User, Some("im:王五"), "@bot 新任务"),
        ];
        let goal = pending_inputs_from(&msgs).expect("应有新任务");
        assert_eq!(goal, "@bot 新任务");
    }
}

// ---------------------------------------------------------------- 五阶段提取冒烟（5.2）
// 行为零变更验证：探活短路 / 排队合并 / steer 中断三路径经提取后的
// handle_user_message_scoped 编排（ingest→probe→merge→dispatch→finalize）。

mod five_stage_smoke {
    use super::*;
    use crate::orchestrator::mark_im_session;

    fn core() -> crate::Core {
        let dir = std::env::temp_dir().join(format!("exm-5st-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = crate::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        crate::Core::with_config(cfg).expect("创建 Core 失败")
    }

    /// 探活短路：IM 会话纯问候零 LLM 直答——run.finished 带 shortCircuit=probe，
    /// 不进 merge 的台账 goal 改写（goal 保持上轮值/空）。
    #[tokio::test]
    async fn 探活短路_直答收束不进规划() {
        let core = core();
        let sid = core.create_session("smoke-probe").unwrap().id;
        mark_im_session(&sid);
        // 订阅事件验证收束
        let mut rx = core.subscribe();
        core.chat(&sid, "在吗").await.unwrap();
        let mut saw_probe = false;
        // 事件已在广播环里，drain 验证
        while let Ok(evt) = rx.try_recv() {
            if evt.kind == "run.finished"
                && evt
                    .payload
                    .get("shortCircuit")
                    .and_then(|v| v.as_str())
                    == Some("probe")
            {
                saw_probe = true;
            }
        }
        assert!(saw_probe, "探活短路应发 shortCircuit=probe 的 run.finished");
        // 直答已落库（orchestrator 角色）
        let msgs = core.store.list_messages(&sid, 10).unwrap();
        assert!(
            msgs.iter()
                .any(|m| m.role == crate::types::MessageRole::Orchestrator),
            "短路直答应落库"
        );
        let ws = core.config().workspace_root.clone();
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// 排队窗口合并：非 IM 探活路径下，连续两条用户输入合并为一轮目标
    ///（collect_pending_inputs 尾部连续 user 扫描）。
    #[tokio::test]
    async fn 排队窗口_连续输入合并单轮() {
        // 纯函数验证（无 Core 依赖）：collect 的基础形态——尾部连续 user
        // 消息按序合并为单轮 goal。Core 侧链路由探活/steer 冒烟覆盖。
        let msgs = vec![
            crate::types::ChatMessage {
                id: "m1".into(),
                session_id: "smoke-merge".into(),
                role: crate::types::MessageRole::User,
                agent_id: None,
                statements: vec![Statement::new(SpeechTag::要求, "任务B")],
                created_at: String::new(),
                turn: None,
                token_usage: None,
                thinking: None,
                last_message_preview: None,
                last_active_at: None,
                tool_calls: Vec::new(),
                steps: Vec::new(),
                attachments: Vec::new(),
            },
            crate::types::ChatMessage {
                id: "m2".into(),
                session_id: "smoke-merge".into(),
                role: crate::types::MessageRole::User,
                agent_id: None,
                statements: vec![Statement::new(SpeechTag::要求, "补充：要快")],
                created_at: String::new(),
                turn: None,
                token_usage: None,
                thinking: None,
                last_message_preview: None,
                last_active_at: None,
                tool_calls: Vec::new(),
                steps: Vec::new(),
                attachments: Vec::new(),
            },
        ];
        let goal = crate::orchestrator::pending_inputs_for_test(&msgs).expect("应合并");
        assert_eq!(goal, "任务B\n补充：要快", "连续输入按序合并");
    }

    /// steer 中断：request_cancel 置位后，下一轮 dispatch 前的取消检查生效
    ///（is_cancelled 在 run_round L0 分支/执行节点检查——这里验证令牌链路）。
    #[tokio::test]
    async fn steer中断_取消令牌置位与清除() {
        let core = core();
        let sid = core.create_session("smoke-steer").unwrap().id;
        // 无运行轮次：cancel 返回 false（无轮可停）
        assert!(!crate::round_trace::request_cancel(&sid));
        // 运行中置位（chat_scoped_origin 的 begin/end 链路内）
        let core_arc = std::sync::Arc::new(core);
        let sid2 = sid.clone();
        let core2 = core_arc.clone();
        let handle = tokio::spawn(async move { core2.chat(&sid2, "跑一会").await });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let _ = crate::round_trace::request_cancel(&sid);
        let _ = handle.await;
        // 轮次结束（round_trace::end）后取消态清除
        assert!(!crate::round_trace::is_cancelled(&sid));
        let ws = core_arc.config().workspace_root.clone();
        let _ = std::fs::remove_dir_all(&ws);
    }
}
