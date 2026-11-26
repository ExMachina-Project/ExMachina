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
            repaired: None,
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
    /// 尾部真实 @ 消息正常收集（带群聊署名）；旁听消息跳过且不打断连续窗口扫描。
    #[test]
    fn 旁听消息_跳过_真实输入正常合并() {
        let msgs = vec![
            msg(MessageRole::User, Some("imctx:张三"), "今晚吃什么"),
            msg(MessageRole::User, Some("imctx:李四"), "随便"),
            msg(MessageRole::User, Some("im:王五"), "@bot 帮我们推荐"),
        ];
        let goal = pending_inputs_from(&msgs).expect("应收集到真实输入");
        assert_eq!(goal, "[群聊·王五] @bot 帮我们推荐");
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
        // 真实输入独立成条、无背景标注；群聊真实输入带署名前缀（群聊参与感：
        // 模型看到「谁在说话」，不是无差别「在跟我说话」）
        assert_eq!(hist[3].content, "[群聊·赵六] @bot 在吗");
    }

    /// 历史装配：普通会话（无旁听）行为不变——真实输入与助手消息原样呈现；
    /// 群聊真实输入（im:）带署名前缀
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
        assert_eq!(hist[2].content, "[群聊·甲] 群聊真实输入");
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

    /// 非user 消息是窗口边界：旁听在其后、边界前的 @ 消息不应被回溯收集
    #[test]
    fn 轮次边界_不回溯() {
        let msgs = vec![
            msg(MessageRole::User, Some("im:王五"), "@bot 旧任务"),
            msg(MessageRole::Orchestrator, None, "旧回答"),
            msg(MessageRole::User, Some("imctx:张三"), "闲聊"),
            msg(MessageRole::User, Some("im:王五"), "@bot 新任务"),
        ];
        let goal = pending_inputs_from(&msgs).expect("应有新任务");
        assert_eq!(goal, "[群聊·王五] @bot 新任务");
    }

    /// 群聊参与感（im-group-context）：排队窗口内多人先后发言合并为一轮时
    /// 每条带署名——模型能分清「谁在说话」，不会当成单人连环指令；
    /// 这是对话流多人语境的最小锚定。
    #[test]
    fn 排队合并_多人署名保留() {
        let msgs = vec![
            msg(MessageRole::User, Some("im:张三"), "刚才那个报错怎么解"),
            msg(MessageRole::User, Some("im:李四"), "对，我也想知道"),
        ];
        let goal = pending_inputs_from(&msgs).expect("应合并多人输入");
        assert!(
            goal.contains("[群聊·张三] 刚才那个报错怎么解"),
            "张三署名丢失：{goal}"
        );
        assert!(
            goal.contains("[群聊·李四] 对，我也想知道"),
            "李四署名丢失：{goal}"
        );
        assert_ne!(goal.lines().count(), 1, "多人都应保留独立行，不是拼成一段");
    }
}

// ---------------------------------------------------------------- 轮次编排冒烟

mod five_stage_smoke {
    use super::*;

    fn core() -> crate::Core {
        let dir = std::env::temp_dir().join(format!("exm-5st-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = crate::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        crate::Core::with_config(cfg).expect("创建 Core 失败")
    }

    /// 排队窗口合并：连续两条用户输入合并为一轮目标
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

    /// capability-parity 6.3 验收：`/context disable <来源>` 必须**真的改变发往模型的
    /// 提示词**，而不只是翻一个标志位、回一句「下轮生效」。走真实轮次（`Core::chat`），
    /// 用内容探测器观察模型实际收到的请求。三条注入代码路径各有各的闸，都要验证：
    ///
    /// | 路径 | 何时走 | 注入点 |
    /// |---|---|---|
    /// | 规划（`Orchestrator::plan`） | 组模式 | 技能简表、滚动摘要、记忆召回 |
    /// | 派发（dispatch inputs） | 组模式，有执行个体 | 技能指令、记忆、共享上下文里的摘要 |
    /// | 智能体直连（`single_direct_round`） | 智能体模式 | 系统提示里的滚动摘要 |
    ///
    /// 断言只依赖闸直接控制的**注入结构**（块标题、输入前缀），不依赖召回排序之类
    /// 闸管不了的结果——见各测试的说明。
    mod context_source_gates {
        use super::*;
        use crate::provider::mock_probe_scope;

        /// 唯一标记：并行测试各用各的，探测器互不干扰。用带连字符的 uuid——
        /// 不用 32 位连续十六进制：其中的长数字子串会被敏感内容检测当成卡号
        /// （技能写入经同一校验），随机命中时夹具自己就把测试弄挂了。
        fn marker(tag: &str) -> String {
            format!("{tag}-{}", uuid::Uuid::new_v4())
        }

        /// 探测作用域：本测试独有、且会出现在它**每个**模型请求里的片段（进智能体
        /// 名称/提示词 → 系统提示）。Mock 是全进程共用的，没有它，邻居测试的请求会
        /// 给本测试的探针计数（见 `provider::mock` 的说明）。
        fn scope() -> String {
            format!("csg-{}", uuid::Uuid::new_v4())
        }

        fn core_with_memory(scope: &str) -> crate::Core {
            let dir = std::env::temp_dir().join(format!("exm-csg-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let mut cfg = crate::config::ExmConfig::load(&dir);
            cfg.use_mock = true;
            cfg.memory_enabled = true;
            core_from_config(cfg, scope)
        }

        /// 组模式 Core（带主智能体与执行个体，名称都含 `scope`——探测作用域）。
        /// 规划路径以组主智能体的职责提示词为基底；默认空目录落的是智能体模式
        /// （无主智能体），所以这里建一个带主智能体的组并激活。
        fn core_from_config(cfg: crate::config::ExmConfig, scope: &str) -> crate::Core {
            let orch = format!("orch-{scope}");
            let worker = format!("worker-{scope}");
            let core = crate::Core::with_config(cfg).expect("创建 Core 失败");
            let reg = core.registry();
            reg.create_group(Some("csg".into()), "来源闸测试组", "")
                .unwrap();
            reg.upsert_agent(
                "csg",
                AgentDefinition {
                    name: orch.clone(),
                    identifier: orch,
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
                    model_routing: None,
                },
                None,
            )
            .unwrap();
            reg.set_primary("csg", &format!("orch-{scope}")).unwrap();
            // 一个执行个体：Mock 规划据此产出 L1 派发（只有主智能体时走 L0 直答，
            // 派发路径——技能/记忆/共享上下文的注入点——不会执行）
            reg.upsert_agent(
                "csg",
                AgentDefinition {
                    name: worker.clone(),
                    identifier: worker,
                    domain: "测试".into(),
                    tier: Tier::Unit,
                    description: "测试执行个体".into(),
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
                    model_routing: None,
                },
                None,
            )
            .unwrap();
            reg.set_active_group("csg").unwrap();
            core
        }

        fn skill(id: &str, name: &str, body_mark: &str) -> crate::types::SkillDef {
            crate::types::SkillDef {
                id: id.into(),
                name: name.into(),
                description: String::new(),
                triggers: vec!["触发".into()],
                // 指令正文带独立标记：派发路径注入的是 instructions（不是 name）
                instructions: format!("指令正文 {body_mark}"),
                agents: vec![],
                created_at: String::new(),
                use_count: 0,
                success_count: 0,
                last_used_at: String::new(),
                source: String::new(),
                version: 0,
                updated_at: String::new(),
            }
        }

        /// 跑**完整一轮**（规划 + 派发，或智能体直连），返回各标记是否出现在发往
        /// 模型的任一请求里。第二个返回值是各标记命中的消息数——断言失败时连同
        /// 打出，便于区分「没注入」与「注入了多次」。
        ///
        /// `scope` 是探测作用域：只有含它的请求才计数（见 [`scope`]）。
        ///
        /// 轮次 future 在**构造点就地装箱**：`core.chat` 的 future 约 40KB，若按值传给
        /// `mock_probe_scope`，它就整份嵌进本函数的状态机，再嵌进测试体（同一测试体
        /// 顺序跑三轮）——三层叠起来超过 2MB 线程栈（实测溢出，且余量本来就只有几十
        /// 字节：被测代码任何一点增长都会把它推过去）。装箱后栈上只剩一个指针。
        async fn round_and_probe(
            core: &std::sync::Arc<crate::Core>,
            scope: &str,
            sid: &str,
            text: &str,
            marks: &[&str],
        ) -> (Vec<bool>, Vec<u32>) {
            let round: std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>>>> = {
                let core = core.clone();
                let sid = sid.to_string();
                let text = text.to_string();
                Box::pin(async move { core.chat(&sid, &text).await })
            };
            let (table, out) = mock_probe_scope(scope, marks, round).await;
            out.expect("完整一轮应成功（禁用来源不是故障）");
            let hits: Vec<u32> = marks
                .iter()
                .map(|m| table.get(*m).copied().unwrap_or(0))
                .collect();
            let present: Vec<bool> = hits.iter().map(|n| *n > 0).collect();
            (present, hits)
        }

        /// (Core, 会话 id, 探测作用域)
        fn setup() -> (std::sync::Arc<crate::Core>, String, String) {
            let scope = scope();
            let core = std::sync::Arc::new(core_with_memory(&scope));
            let sid = core.create_session("csg").unwrap().id;
            (core, sid, scope)
        }

        /// 技能闸与记忆闸，覆盖规划与派发两条路径的注入点：
        ///
        /// | 来源 | 规划路径 | 派发路径 |
        /// |---|---|---|
        /// | skills | 技能简表里的技能 name | `[来源：技能 <id> vN]` 指令注入 |
        /// | memory | `## 历史记忆（群体记忆库召回…` 块 | `[群体记忆|…]` / `[个体记忆|…]` 输入 |
        ///
        /// 探针一律用**注入结构自带的标记**，不用「我写的那条内容」：每轮结束
        /// write_memories 会把执行回流写成记忆（个体 evidence 归属 worker、标题与派发
        /// 查询完全重合），下一轮派发召回窗口里占位的就是它而非任意一条群体教训——
        /// 那是召回排序的结果，不是闸能负责的不变量。
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn 禁用技能与记忆_规划与派发提示词里都消失() {
            let (core, sid, scope) = setup();
            // 短历史：用户输入即 goal，派发路径按 goal 命中技能触发词
            let text = "触发一下 做一个东西";
            let skill_name = marker("技能目录");
            core.add_skill(skill("csg-skill", &skill_name, "指令正文"))
                .unwrap();
            core.memory_remember(
                crate::memory::MemoryDraft::new(
                    crate::memory::MemoryKind::Lesson,
                    format!("{text}：一条教训"),
                    "教训正文",
                )
                .importance(1.0),
            )
            .expect("写入记忆");

            // [技能简表, 技能指令注入, 规划记忆块, 派发记忆输入]
            const PLAN_MEM: &str = "## 历史记忆（群体记忆库召回";
            const DISPATCH_MEM_A: &str = "[群体记忆|";
            const DISPATCH_MEM_B: &str = "[个体记忆|";
            const SKILL_INJECT: &str = "[来源：技能 csg-skill v";
            let marks = [
                skill_name.as_str(),
                SKILL_INJECT,
                PLAN_MEM,
                DISPATCH_MEM_A,
                DISPATCH_MEM_B,
            ];
            fn show(v: &[bool], hits: &[u32]) -> String {
                ["技能简表", "技能指令注入", "规划记忆块", "派发记忆输入"]
                    .iter()
                    .zip([v[0], v[1], v[2], v[3] || v[4]])
                    .zip([hits[0], hits[1], hits[2], hits[3] + hits[4]])
                    .map(|((n, b), h)| format!("{n}={b}×{h}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            }

            let (base, base_hits) = round_and_probe(&core, &scope, &sid, text, &marks).await;
            assert_eq!(
                [base[0], base[1], base[2], base[3] || base[4]],
                [true; 4],
                "基线：四处注入都应在场 —— {}",
                show(&base, &base_hits)
            );

            // ① 只禁用 skills → 技能简表（规划）与技能指令注入（派发）消失；记忆两处照常
            core.context_source_set(&sid, "skills", true).unwrap();
            let (v, v_hits) = round_and_probe(&core, &scope, &sid, text, &marks).await;
            assert!(!v[0], "禁用 skills：规划提示词不得再出现技能简表");
            assert!(!v[1], "禁用 skills：派发不得再注入技能指令");
            assert!(
                v[2],
                "禁用 skills 不得波及规划路径的记忆召回 —— {}",
                show(&v, &v_hits)
            );
            assert!(
                v[3] || v[4],
                "禁用 skills 不得波及派发路径的记忆输入 —— {}",
                show(&v, &v_hits)
            );

            // ② 再禁用 memory → 记忆两处消失（技能仍关闭）
            core.context_source_set(&sid, "memory", true).unwrap();
            let (v, v_hits) = round_and_probe(&core, &scope, &sid, text, &marks).await;
            assert!(!v[2], "禁用 memory：规划提示词不得再出现记忆块");
            assert!(
                !v[3] && !v[4],
                "禁用 memory：派发不得再带记忆输入 —— {}",
                show(&v, &v_hits)
            );
            assert!(
                !v[0] && !v[1],
                "memory 的开关不得波及 skills —— {}",
                show(&v, &v_hits)
            );

            // ③ 只重新启用 skills → 技能两处回来；记忆仍关闭（启用只影响自己）
            core.context_source_set(&sid, "skills", false).unwrap();
            let (v, v_hits) = round_and_probe(&core, &scope, &sid, text, &marks).await;
            assert_eq!(
                (v[0], v[1]),
                (true, true),
                "启用 skills 只恢复 skills —— {}",
                show(&v, &v_hits)
            );
            assert!(
                !v[2],
                "启用 skills 不得顺带恢复 memory —— {}",
                show(&v, &v_hits)
            );
            assert!(!v[3] && !v[4]);

            // ④ 再启用 memory → 全部回来
            core.context_source_set(&sid, "memory", false).unwrap();
            let (v, v_hits) = round_and_probe(&core, &scope, &sid, text, &marks).await;
            assert_eq!(
                [v[0], v[1], v[2], v[3] || v[4]],
                [true; 4],
                "全部恢复 —— {}",
                show(&v, &v_hits)
            );

            let ws = core.config().workspace_root.clone();
            let _ = std::fs::remove_dir_all(&ws);
        }

        /// 检索闸：滚动摘要属于「会话检索」来源。规划路径（plan.rs 会话上下文块）与
        /// 派发路径（groupSharedContext 的共享上下文输入）都要被闸住；最近对话窗口
        /// 是会话历史本身，不是独立来源，始终在场。
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn 禁用检索_滚动摘要不再注入_最近窗口仍在() {
            let dir = std::env::temp_dir().join(format!("exm-csg-r-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let mut cfg = crate::config::ExmConfig::load(&dir);
            cfg.use_mock = true;
            cfg.memory_enabled = false;
            cfg.experiments.group_shared_context = true; // 派发侧共享上下文（实验开关）
            let scope = scope();
            let core = std::sync::Arc::new(core_from_config(cfg, &scope));
            let sid = core.create_session("csg-retrieval").unwrap().id;

            // 长历史：超过条数阈值（COMPACT_TRIGGER=20）→ 本轮触发压缩，Mock 压缩器
            // 产出固定摘要（「目标与进展：…」）——以此作为滚动摘要的探针，两条路径同源。
            const SUMMARY_MARK: &str = "目标与进展：用户持续在推进当前任务";
            // 最近窗口 = 尾部 keep=12 条原文：其中一条带标记。它是会话历史本身，
            // 不是独立来源——禁用 retrieval 也必须在场。
            let recent_mark = marker("最近窗口");
            for i in 0..30 {
                let text = if i == 25 {
                    recent_mark.clone()
                } else {
                    format!("早期消息 {i}")
                };
                core.store
                    .add_message(
                        &sid,
                        crate::types::MessageRole::User,
                        None,
                        vec![Statement::new(SpeechTag::要求, text)],
                    )
                    .unwrap();
            }
            let marks = [SUMMARY_MARK, recent_mark.as_str()];
            let show = |v: &[bool], hits: &[u32]| {
                format!(
                    "滚动摘要={}×{} 最近窗口={}×{}",
                    v[0], hits[0], v[1], hits[1]
                )
            };
            let text = "做一个东西";

            let (base, base_hits) = round_and_probe(&core, &scope, &sid, text, &marks).await;
            assert_eq!(
                base,
                [true, true],
                "基线：滚动摘要与最近窗口都应注入 —— {}",
                show(&base, &base_hits)
            );

            // 禁用 retrieval → 摘要消失；最近窗口仍在（不是独立来源）
            core.context_source_set(&sid, "retrieval", true).unwrap();
            let (v, v_hits) = round_and_probe(&core, &scope, &sid, text, &marks).await;
            assert_eq!(v, [false, true], "禁用 retrieval —— {}", show(&v, &v_hits));

            // 重新启用 → 摘要回来
            core.context_source_set(&sid, "retrieval", false).unwrap();
            let (v, v_hits) = round_and_probe(&core, &scope, &sid, text, &marks).await;
            assert_eq!(
                v,
                [true, true],
                "启用 retrieval 恢复摘要 —— {}",
                show(&v, &v_hits)
            );

            let ws = core.config().workspace_root.clone();
            let _ = std::fs::remove_dir_all(&ws);
        }

        /// 智能体模式的轮次走 `single_direct_round`，与组模式的规划/派发是不同的注入
        /// 代码路径——滚动摘要的闸在这里也要成立。这条路径没有技能/记忆的自动注入，
        /// 所以只需验证 retrieval。
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn 智能体模式_禁用检索_滚动摘要不再注入() {
            let dir = std::env::temp_dir().join(format!("exm-csg-s-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let mut cfg = crate::config::ExmConfig::load(&dir);
            cfg.use_mock = true;
            cfg.memory_enabled = false;
            let core = std::sync::Arc::new(crate::Core::with_config(cfg).expect("创建 Core 失败"));
            // 智能体模式：一个单智能体并激活（`exm agent create` 后的形态）。
            // 提示词里放探测作用域：这条路径的请求只有一条系统提示，作用域必须在其中
            let scope = scope();
            let reg = core.registry();
            let single = reg
                .upsert_single(
                    AgentDefinition {
                        name: "csg-single".into(),
                        identifier: "csg-single".into(),
                        domain: "测试".into(),
                        tier: Tier::Orchestrator,
                        description: "测试单智能体".into(),
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
                        model_routing: None,
                    },
                    Some(format!("你是测试单智能体。探测域 {scope}")),
                )
                .unwrap();
            reg.set_active_single(Some(&single.identifier)).unwrap();
            assert!(reg.single_mode(), "前置：处于智能体模式");
            let sid = core.create_session("csg-single").unwrap().id;

            const SUMMARY_MARK: &str = "目标与进展：用户持续在推进当前任务";
            let recent_mark = marker("最近窗口");
            // 智能体模式的压缩阈值比组模式宽（trigger=40 / keep=20，见 single_direct_round）：
            // 历史必须超过 40 条才会真正发生压缩、产生滚动摘要。标记消息放在尾部窗口内。
            for i in 0..50 {
                let text = if i == 48 {
                    recent_mark.clone()
                } else {
                    format!("早期消息 {i}")
                };
                core.store
                    .add_message(
                        &sid,
                        crate::types::MessageRole::User,
                        None,
                        vec![Statement::new(SpeechTag::要求, text)],
                    )
                    .unwrap();
            }
            let marks = [SUMMARY_MARK, recent_mark.as_str()];
            let show = |v: &[bool], hits: &[u32]| {
                format!(
                    "滚动摘要={}×{} 最近窗口={}×{}",
                    v[0], hits[0], v[1], hits[1]
                )
            };
            let text = "做一个东西";

            let (base, base_hits) = round_and_probe(&core, &scope, &sid, text, &marks).await;
            assert_eq!(base, [true, true], "基线 —— {}", show(&base, &base_hits));

            core.context_source_set(&sid, "retrieval", true).unwrap();
            let (v, v_hits) = round_and_probe(&core, &scope, &sid, text, &marks).await;
            assert_eq!(
                v,
                [false, true],
                "禁用 retrieval（智能体模式）—— {}",
                show(&v, &v_hits)
            );

            core.context_source_set(&sid, "retrieval", false).unwrap();
            let (v, v_hits) = round_and_probe(&core, &scope, &sid, text, &marks).await;
            assert_eq!(
                v,
                [true, true],
                "启用 retrieval 恢复 —— {}",
                show(&v, &v_hits)
            );

            let ws = core.config().workspace_root.clone();
            let _ = std::fs::remove_dir_all(&ws);
        }
    }
}
