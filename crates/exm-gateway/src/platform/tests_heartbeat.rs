//! gateway/platform/tests_heartbeat —— 自 platform.rs 按资源域拆出（纯搬移，零行为变更）
#[cfg(test)]
use super::*;

// ---------------------------------------------------------------- 心跳 per-agent 粒度（7.4 单测）

mod heartbeat_tests {
    use super::*;

    fn core_with_agent(heartbeat_agent: Option<&str>) -> exm_core::Core {
        let dir = std::env::temp_dir().join(format!("exm-hb-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.automation.heartbeat_agent = heartbeat_agent.map(|s| s.to_string());
        let core = exm_core::Core::with_config(cfg).expect("创建 Core 失败");
        // 测试组（内置组定义受保护，不可增删）：主智能体 + 可派发的执行个体
        core.registry()
            .create_group(Some("t".into()), "心跳组", "")
            .unwrap();
        let mk = |name: &str, identifier: &str, tier: exm_core::types::Tier| {
            exm_core::types::AgentDefinition {
                name: name.into(),
                identifier: identifier.into(),
                domain: "测试".into(),
                tier,
                description: "测试个体".into(),
                capabilities: vec![],
                tools: vec![],
                when_to_call: String::new(),
                link: None,
                dependencies: vec![],
                composable_with: vec![],
                input_schema: Default::default(),
                output_schema: Default::default(),
                prompt_file: String::new(),
                model_hint: None,
                max_steps: None,
            }
        };
        core.registry()
            .upsert_agent(
                "t",
                mk("值守指挥", "duty-orch", exm_core::types::Tier::Orchestrator),
                None,
            )
            .unwrap();
        core.registry()
            .upsert_agent(
                "t",
                mk("值守执行", "duty-worker", exm_core::types::Tier::Unit),
                None,
            )
            .unwrap();
        core.registry().set_primary("t", "duty-orch").unwrap();
        core
    }

    #[test]
    fn 心跳_未配置维持全局() {
        let core = core_with_agent(None);
        let job = build_heartbeat_job(&core, "k1");
        assert_eq!(
            job.session_title.as_deref(),
            Some("心跳巡检"),
            "全局心跳会话名不变"
        );
        assert!(job.group.is_none(), "未配置 = 激活组执行（group 缺省）");
    }

    #[test]
    fn 心跳_目标个体不存在维持全局() {
        let core = core_with_agent(Some("ghost-agent"));
        let job = build_heartbeat_job(&core, "k2");
        assert_eq!(
            job.session_title.as_deref(),
            Some("心跳巡检"),
            "个体不存在回落全局"
        );
        assert!(job.group.is_none());
    }

    #[test]
    fn 心跳_目标个体以其组巡检且产出进其记忆() {
        let core = core_with_agent(Some("duty-worker"));
        core.registry().set_active_group("t").unwrap();
        let job = build_heartbeat_job(&core, "k3");
        assert_eq!(
            job.session_title.as_deref(),
            Some("心跳巡检-duty-worker"),
            "per-agent 心跳独立会话"
        );
        assert_eq!(job.group.as_deref(), Some("t"), "以个体所属组为执行上下文");

        // 实际执行一轮（替身通道派发）：产出（会话摘要等）应落在该组的记忆体系
        let run = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(core.run_cron_job(&job));
        assert_eq!(run.status, "done", "替身通道执行应完成：{}", run.summary);
        let stats = core.memory_stats().unwrap();
        let group_entries = stats["byGroup"]["t"].as_u64().unwrap_or(0);
        assert!(
            group_entries >= 1,
            "心跳产出应写入目标组记忆（byGroup.t = {group_entries}）"
        );
        // 独立演进：per-agent 会话存在，全局心跳会话不被创建（两种模式互不污染）
        let titles: Vec<String> = core
            .list_sessions_in_group("t")
            .unwrap()
            .into_iter()
            .map(|s| s.title)
            .collect();
        assert!(
            titles.iter().any(|t| t == "心跳巡检-duty-worker"),
            "per-agent 会话存在：{titles:?}"
        );
        assert!(
            !titles.iter().any(|t| t == "心跳巡检"),
            "全局心跳会话不应被 per-agent 心跳创建"
        );
    }
}
