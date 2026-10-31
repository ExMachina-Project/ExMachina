//! gateway/platform/tests_plugin_secret_mask —— 自 platform.rs 按资源域拆出（纯搬移，零行为变更）
#[cfg(test)]
use super::*;

mod plugin_secret_mask_tests {
    use super::*;

    fn core_with_video() -> exm_core::Core {
        let dir = std::env::temp_dir().join(format!("exm-mask-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        exm_core::Core::with_config(cfg).expect("创建 Core 失败")
    }

    /// 插件清单声明的 sensitive 键（video：sessdata/biliJct/apiKey）脱敏回显
    #[test]
    fn 脱敏_插件清单敏感键掩码() {
        let core = core_with_video();
        // 手动注册 video builtin（spawn_all 需 Arc<Core>；此处仅需注册面凭据声明）
        core.plugins()
            .register_builtin_late(Box::new(crate::plugins::VideoPlugin::new()));
        let ch = load_channels(&core)
            .into_iter()
            .find(|c| c.kind == "bilibili")
            .unwrap_or(Channel {
                id: "bili-main".into(),
                kind: "bilibili".into(),
                enabled: true,
                group: None,
                allowed_chats: vec![],
                admins: vec![],
                account: None,
                secret: None,
                token: None,
                reply_webhook: None,
                group_gate: None,
                capability: "computer".into(),
                memory_isolation: false,
                created_at: String::new(),
                config: [
                    ("sessdata".to_string(), "real-secret-value".to_string()),
                    ("biliJct".to_string(), "real-jct".to_string()),
                    ("uid".to_string(), "12345".to_string()),
                ]
                .into_iter()
                .collect(),
            });
        let v = channel_json(&ch, &core);
        let cfg = &v["config"];
        assert_eq!(cfg["sessdata"], crate::llm_admin::KEY_MASK);
        assert_eq!(cfg["biliJct"], crate::llm_admin::KEY_MASK);
        // 非敏感键明文
        assert_eq!(cfg["uid"], "12345");
        // 原值不泄露
        assert!(!v.to_string().contains("real-secret-value"));
    }

    /// 审计归因：插件工具调用的 tool.call/tool.result 事件带 plugin 字段
    #[tokio::test]
    async fn 审计_插件工具归因字段() {
        let dir = std::env::temp_dir().join(format!("exm-audit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        let core = exm_core::Core::with_config(cfg).unwrap();
        let sid = core.store.create_session("审计", "a1").unwrap().id;
        let orch = core.orchestrator();
        let gw = orch.tools.clone();
        // 订阅事件流
        let mut rx = core.subscribe();
        // 注册一个插件工具并调用（不可达地址，走错误路径也带归因）
        gw.append_plugin_tools(vec![exm_core::config::CustomTool {
            name: "demo__ping".into(),
            description: "审计用".into(),
            kind: "http".into(),
            method: "GET".into(),
            target: "https://127.0.0.1:1/x".into(),
            headers: Default::default(),
            parameters: serde_json::json!({"type":"object","properties":{}}),
            agents: vec![],
            response_max_chars: None,
            response_select: None,
        }]);
        let _ = gw
            .execute_named("a1", &sid, &[], "demo__ping", &serde_json::json!({}))
            .await;
        // 收事件验证 plugin 归因
        let mut saw_call = false;
        let mut saw_result = false;
        while let Ok(evt) = rx.try_recv() {
            match evt.kind.as_str() {
                "tool.call" if evt.payload["tool"] == "demo__ping" => {
                    assert_eq!(evt.payload["plugin"], "demo", "call 事件应带 plugin 归因");
                    saw_call = true;
                }
                "tool.result" if evt.payload["tool"] == "demo__ping" => {
                    assert_eq!(evt.payload["plugin"], "demo", "result 事件应带 plugin 归因");
                    saw_result = true;
                }
                _ => {}
            }
        }
        assert!(saw_call, "应有 tool.call 事件");
        assert!(saw_result, "应有 tool.result 事件");
    }
}
