//! config 域测试（自 config.rs 迁出，纯搬移）。

use super::*;

#[cfg(test)]
mod config_tests {
    use super::*;
    use crate::config_schema::config_schema;

    /// 新装（无配置文件）：全部新段取 Default，群聊门控默认开
    #[test]
    fn 配置默认值_新装门控默认开() {
        let dir = std::env::temp_dir().join(format!("exm-cfg-fresh-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join(".exmachina")).unwrap();
        let cfg = ExmConfig::load(&dir);
        assert_eq!(cfg.config_version, CONFIG_VERSION);
        assert!(cfg.identity.group_gate_default, "新装门控应默认开");
        assert_eq!(cfg.identity.pairing_ttl_secs, 600);
        assert!(
            !cfg.identity.identity_required,
            "身份管控默认关（白名单即全权）"
        );
        assert!(!cfg.triggers.file_watch.enabled, "文件监听默认关");
        assert_eq!(cfg.triggers.file_watch.exclude.len(), 3, "默认排除清单");
        assert!(!cfg.limits.enabled, "限流默认关");
        assert_eq!(cfg.limits.quota_running_policy, "finish");
        assert!(!cfg.mcp_serve.enabled, "MCP 服务端默认关");
        assert_eq!(cfg.mcp_serve.http_path, "/mcp");
        assert!(cfg.notify.enabled, "推送总开关默认开");
        assert_eq!(cfg.notify.max_chars, 500);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// v1 → v2 迁移：补齐全部新段；既有安装群聊门控显式置关（保持升级前行为）；
    /// 迁移结果回写配置文件（版本号 + 新段落盘）
    #[test]
    fn 配置迁移_v1补齐新段并关门控() {
        let dir = std::env::temp_dir().join(format!("exm-cfg-mig-{}", uuid::Uuid::new_v4()));
        let state = dir.join(".exmachina");
        std::fs::create_dir_all(&state).unwrap();
        // 一份 v1 时代的历史配置：不含任何新段
        std::fs::write(
            state.join("config.json"),
            r#"{"configVersion":1,"maxConcurrency":4}"#,
        )
        .unwrap();

        let cfg = ExmConfig::load(&dir);
        assert_eq!(cfg.config_version, 2, "迁移后应为 v2");
        assert!(!cfg.identity.group_gate_default, "升级迁移门控应置关");
        assert_eq!(cfg.identity.pairing_ttl_secs, 600);
        assert_eq!(cfg.max_concurrency, 4, "历史字段保留");
        assert!(!cfg.triggers.file_watch.enabled);
        assert!(!cfg.limits.enabled);
        assert!(!cfg.mcp_serve.enabled);
        assert!(cfg.notify.enabled);

        // 回写校验：文件落盘为 v2 且新段可再读出
        let raw = std::fs::read_to_string(state.join("config.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["configVersion"], 2);
        assert_eq!(v["identity"]["groupGateDefault"], false);
        assert!(v["triggers"]["fileWatch"].is_object());
        assert!(v["mcpServe"].is_object());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// schema 驱动 UI：新段字段必须全部进入 config_schema（安装向导/设置页零改动渲染）
    #[test]
    fn 配置schema_覆盖全部新段() {
        let schema = config_schema();
        let groups = schema["groups"].as_array().unwrap();
        let keys: Vec<&str> = groups.iter().filter_map(|g| g["key"].as_str()).collect();
        for expected in ["identity", "limits", "triggers", "mcpServe", "notify"] {
            assert!(
                keys.contains(&expected),
                "config_schema 缺少组 {}",
                expected
            );
        }
        assert_eq!(schema["configVersion"], CONFIG_VERSION);
    }

    /// meta-agent 配置面完整：两个新键都必须进 schema（设置页零 UI 改动渲染的前提）
    #[test]
    fn 配置schema_含meta_agent开关与周期() {
        let schema = config_schema();
        let all_keys: Vec<String> = schema["groups"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| {
                g["fields"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|f| f["key"].as_str().map(String::from))
            })
            .collect();
        for expected in ["experiments.metaAgent", "automation.metaIntervalMin"] {
            assert!(
                all_keys.contains(&expected.to_string()),
                "config_schema 缺少字段 {expected}"
            );
        }
    }

    /// 半衰期配置清洗：非法值（0/负数/NaN/∞）回落默认 30 天，合法值原样透传。
    /// 保证设置页显示的半衰期与 decay 实际执行的一致（所见即所跑）。
    #[test]
    fn 半衰期配置_非法值回落默认() {
        assert_eq!(sanitize_half_life_days(None), 30.0, "未配置取默认");
        assert_eq!(sanitize_half_life_days(Some(7.0)), 7.0, "合法值透传");
        assert_eq!(
            sanitize_half_life_days(Some(0.1)),
            0.1,
            "小于 1 天也是合法正数（decay 内部另有下限）"
        );
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                sanitize_half_life_days(Some(bad)),
                30.0,
                "非法值 {bad} 应回落默认 30 天"
            );
        }
    }

    /// 实验开关：默认全关（新装环境既有行为不变）；旧 config（无新字段）可加载；
    /// schema 收录两个新开关（设置页实验区自动渲染）。
    #[test]
    fn 实验开关_新字段默认关且schema收录() {
        let d = ExperimentsConfig::default();
        assert!(!d.llm_driven_memory, "自主记忆摘要默认关");
        assert!(!d.sliding_context_window, "滑动窗口默认关");
        // 旧 config（只有既有实验字段）可反序列化：新字段回落 false
        let legacy = serde_json::json!({ "dynamicCompaction": true, "groupSharedContext": false });
        let parsed: ExperimentsConfig = serde_json::from_value(legacy).expect("旧格式可加载");
        assert!(parsed.dynamic_compaction);
        assert!(!parsed.llm_driven_memory);
        assert!(!parsed.sliding_context_window);
        // schema 收录
        let schema = config_schema();
        let text = serde_json::to_string(&schema).unwrap();
        assert!(text.contains("llmDrivenMemory"), "schema 缺自主记忆开关");
        assert!(text.contains("slidingContextWindow"), "schema 缺滑动窗口开关");
    }
}

#[cfg(test)]
mod exec_identity_tests {
    use super::*;

    #[test]
    fn 空_不约束() {
        assert_eq!(resolve_exec_identity("", "").unwrap(), None);
        assert_eq!(resolve_exec_identity("  ", "").unwrap(), None);
    }

    #[test]
    fn 数字uid_直接生效() {
        let id = resolve_exec_identity("1000", "").unwrap().unwrap();
        assert_eq!(id.uid, 1000);
        assert_eq!(id.gid, 1000);
        assert_eq!(id.home, "/home/1000");
    }

    #[test]
    fn 数字uid_home覆盖() {
        let id = resolve_exec_identity("1000", "/srv/exm-home").unwrap().unwrap();
        assert_eq!(id.home, "/srv/exm-home");
    }

    #[test]
    fn 相对路径home_拒绝() {
        assert!(resolve_exec_identity("1000", "srv/exm").is_err());
        assert!(resolve_exec_identity("1000", "relative").is_err());
    }

    #[test]
    fn 不存在用户名_显式失败() {
        let r = resolve_exec_identity("no-such-user-xyz-911", "");
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("不存在"));
    }

    #[cfg(unix)]
    #[test]
    fn root用户可解析() {
        // 容器/CI 环境 root 必在 passwd;但 root 是约束的反面——仅验证解析通路
        let id = resolve_exec_identity("root", "").unwrap().unwrap();
        assert_eq!(id.uid, 0);
        assert_eq!(id.home, "/root");
    }

    #[cfg(unix)]
    #[test]
    fn 用户名_home覆盖优先() {
        let id = resolve_exec_identity("root", "/home/exm").unwrap().unwrap();
        assert_eq!(id.uid, 0);
        assert_eq!(id.home, "/home/exm");
    }
}
