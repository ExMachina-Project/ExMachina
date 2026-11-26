//! 统一连接层的独立开关与硬上限；不启动任何连接或来源服务。
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionFabricConfig {
    pub enabled: bool,
    pub max_operation_secs: u64,
    pub max_calls_per_root: u64,
    pub max_polls_per_root: u64,
    pub max_concurrent_per_connection: u64,
    pub max_event_bytes: u64,
    pub max_result_bytes: u64,
    pub max_events_per_minute: u64,
    pub max_event_depth: u64,
    pub max_pending_events: u64,
    pub catalog_page_size: u64,
    #[serde(skip)]
    invalid_input: bool,
}

impl Default for ConnectionFabricConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_operation_secs: 600,
            max_calls_per_root: 64,
            max_polls_per_root: 256,
            max_concurrent_per_connection: 4,
            max_event_bytes: 64 * 1024,
            max_result_bytes: 1024 * 1024,
            max_events_per_minute: 60,
            max_event_depth: 4,
            max_pending_events: 128,
            catalog_page_size: 50,
            invalid_input: false,
        }
    }
}

// ExmConfig::load retains its legacy infallible API. Preserve a rejection marker
// instead of failing whole-file deserialization (which would discard other
// source configuration through that loader's historical default fallback).
impl<'de> Deserialize<'de> for ConnectionFabricConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let mut config = Self::default();
        let Some(object) = value.as_object() else {
            config.invalid_input = true;
            return Ok(config);
        };
        for (key, value) in object {
            if key == "enabled" {
                if let Some(enabled) = value.as_bool() {
                    config.enabled = enabled;
                } else {
                    config.invalid_input = true;
                }
                continue;
            }
            let target = match key.as_str() {
                "maxOperationSecs" => &mut config.max_operation_secs,
                "maxCallsPerRoot" => &mut config.max_calls_per_root,
                "maxPollsPerRoot" => &mut config.max_polls_per_root,
                "maxConcurrentPerConnection" => &mut config.max_concurrent_per_connection,
                "maxEventBytes" => &mut config.max_event_bytes,
                "maxResultBytes" => &mut config.max_result_bytes,
                "maxEventsPerMinute" => &mut config.max_events_per_minute,
                "maxEventDepth" => &mut config.max_event_depth,
                "maxPendingEvents" => &mut config.max_pending_events,
                "catalogPageSize" => &mut config.catalog_page_size,
                _ => {
                    config.invalid_input = true;
                    continue;
                }
            };
            if let Some(number) = value.as_u64() {
                *target = number;
            } else {
                config.invalid_input = true;
            }
        }
        Ok(config)
    }
}

impl ConnectionFabricConfig {
    /// 拒绝而非把零理解为无限，文件/程序化/HTTP 配置共用校验。
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.invalid_input,
            "experiments.connectionFabric 含无效字段或类型"
        );
        let hard = Self::default();
        for (name, value, max) in [
            (
                "maxOperationSecs",
                self.max_operation_secs,
                hard.max_operation_secs,
            ),
            (
                "maxCallsPerRoot",
                self.max_calls_per_root,
                hard.max_calls_per_root,
            ),
            (
                "maxPollsPerRoot",
                self.max_polls_per_root,
                hard.max_polls_per_root,
            ),
            (
                "maxConcurrentPerConnection",
                self.max_concurrent_per_connection,
                hard.max_concurrent_per_connection,
            ),
            ("maxEventBytes", self.max_event_bytes, hard.max_event_bytes),
            (
                "maxResultBytes",
                self.max_result_bytes,
                hard.max_result_bytes,
            ),
            (
                "maxEventsPerMinute",
                self.max_events_per_minute,
                hard.max_events_per_minute,
            ),
            ("maxEventDepth", self.max_event_depth, hard.max_event_depth),
            (
                "maxPendingEvents",
                self.max_pending_events,
                hard.max_pending_events,
            ),
            ("catalogPageSize", self.catalog_page_size, 200),
        ] {
            anyhow::ensure!(
                (1..=max).contains(&value),
                "experiments.connectionFabric.{name} 必须在 1..={max} 范围内"
            );
        }
        Ok(())
    }

    /// PATCH 省略沿用，null/未知字段/错误类型明确拒绝，避免静默复位限制。
    pub fn patched(&self, patch: &serde_json::Value) -> anyhow::Result<Self> {
        self.validate()?;
        let mut merged = serde_json::to_value(self)?;
        let object = patch
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("connectionFabric 必须为对象"))?;
        for (key, value) in object {
            anyhow::ensure!(
                merged.get(key).is_some() && !value.is_null(),
                "connectionFabric 字段无效"
            );
            merged[key] = value.clone();
        }
        let next: Self = serde_json::from_value(merged)?;
        next.validate()?;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn connection_fabric_config_defaults_and_partial_updates_are_bounded() {
        let config = ConnectionFabricConfig::default();
        config.validate().unwrap();
        assert!(!config.enabled);
        let updated = config
            .patched(&json!({"enabled":true, "maxCallsPerRoot":5}))
            .unwrap();
        assert_eq!(updated.max_calls_per_root, 5);
        assert_eq!(updated.max_operation_secs, 600);
        for patch in [
            json!({"maxCallsPerRoot":0}),
            json!({"maxCallsPerRoot":65}),
            json!({"maxOperationSecs":601}),
            json!({"maxEventBytes":-1}),
            json!({"catalogPageSize":201}),
            json!({"enabled":null}),
            json!({"grantAll":true}),
            json!({"enabled":"true"}),
            json!([]),
        ] {
            assert!(config.patched(&patch).is_err(), "{patch}");
        }
        let serialized = serde_json::to_value(config).unwrap();
        for key in serialized
            .as_object()
            .unwrap()
            .keys()
            .filter(|key| key.as_str() != "enabled")
        {
            assert!(updated.patched(&json!({(key):0})).is_err(), "{key}");
        }
    }

    #[test]
    fn connection_fabric_v3_migration_preserves_sources_and_has_no_authority_import() {
        let dir =
            std::env::temp_dir().join(format!("exm-connection-config-{}", uuid::Uuid::new_v4()));
        let state = dir.join(".exmachina");
        std::fs::create_dir_all(&state).unwrap();
        let path = state.join("config.json");
        let legacy = json!({"configVersion":3, "nexus":{"links":[{"id":"old","name":"Old","kind":"custom","command":"do-not-execute", "endpoint":"", "apiKey":"", "enabled":true}]}, "experiments":{"autoAgent":true}});
        std::fs::write(&path, legacy.to_string()).unwrap();
        let cfg = crate::config::ExmConfig::load(&dir);
        assert!(!cfg.experiments.connection_fabric.enabled);
        assert!(!cfg.experiments.device_connectivity.enabled);
        assert!(cfg.experiments.auto_agent);
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["configVersion"], crate::config::CONFIG_VERSION);
        assert_eq!(written["nexus"], legacy["nexus"]);
        let mut cfg = cfg;
        cfg.experiments.connection_fabric.enabled = true;
        cfg.experiments.connection_fabric.max_calls_per_root = 0;
        let before = std::fs::read(&path).unwrap();
        assert!(cfg.save().is_err());
        assert!(crate::Core::with_config(cfg).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        // Bad new fields must not cause the infallible legacy loader to throw
        // away the entire source configuration and silently accept defaults.
        for value in [
            json!({"enabled":"yes"}),
            json!({"maxCallsPerRoot":-1}),
            json!({"unknown":0}),
            json!([]),
        ] {
            let mut invalid = legacy.clone();
            invalid["configVersion"] = json!(crate::config::CONFIG_VERSION);
            invalid["experiments"]["connectionFabric"] = value;
            std::fs::write(&path, invalid.to_string()).unwrap();
            let loaded = crate::config::ExmConfig::load(&dir);
            assert_eq!(loaded.nexus.links.len(), 1);
            assert!(loaded.experiments.connection_fabric.validate().is_err());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn connection_fabric_migration_preserves_raw_source_json_and_invalid_files() {
        let dir =
            std::env::temp_dir().join(format!("exm-connection-migration-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join(".exmachina")).unwrap();
        let path = dir.join(".exmachina/config.json");
        let raw = json!({
            "configVersion":3,
            "nexus":{"links":[{"id":"legacy","kind":"custom","name":"Legacy", "extraSourceMetadata":{"keep":true}}]},
            "mcpServers":[{"id":"mcp","transport":"http","url":"http://127.0.0.1:1/mcp","sourceExtension":"preserve"}],
            "plugins":{"state":{"example":false},"futureSourceField":"keep"},
            "unknownTopLevel":{"also":"keep"}
        });
        for version in [1, 2, 3] {
            let mut source_config = raw.clone();
            source_config["configVersion"] = json!(version);
            std::fs::write(&path, source_config.to_string()).unwrap();
            let cfg = crate::config::ExmConfig::load(&dir);
            assert!(!cfg.experiments.connection_fabric.enabled);
            let migrated: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            for field in ["nexus", "mcpServers", "plugins"] {
                assert_eq!(migrated[field], raw[field], "v{version}: {field}");
            }
            if version == 3 {
                assert_eq!(migrated["unknownTopLevel"], raw["unknownTopLevel"]);
            }
        }
        let mut invalid = raw;
        invalid["experiments"] = json!({"connectionFabric":{"maxCallsPerRoot":0}});
        let before = invalid.to_string();
        std::fs::write(&path, &before).unwrap();
        let cfg = crate::config::ExmConfig::load(&dir);
        assert!(cfg.experiments.connection_fabric.validate().is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn connection_fabric_startup_does_not_probe_or_execute_and_schema_matches_limits() {
        let dir =
            std::env::temp_dir().join(format!("exm-connection-startup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut cfg = crate::config::ExmConfig::load(&dir);
        cfg.data_dir = dir.join("data");
        cfg.agents_dir = dir.join("entities");
        cfg.use_mock = true;
        cfg.nexus.links.push(crate::config::LinkTarget {
            id: "no-launch".into(),
            name: "No launch".into(),
            kind: "custom".into(),
            command: "no-such-connection-fixture-command".into(),
            ..Default::default()
        });
        cfg.mcp_servers.push(crate::mcp::McpServerConfig {
            id: "no-connect".into(),
            transport: "http".into(),
            command: None,
            args: vec![],
            env: Default::default(),
            url: Some(format!("http://{}/mcp", listener.local_addr().unwrap())),
            allowed_agents: vec![],
            enabled: true,
            call_timeout_secs: 1,
        });
        let launch_marker = dir.join("source-process-was-launched");
        #[cfg(unix)]
        cfg.mcp_servers.push(crate::mcp::McpServerConfig {
            id: "no-stdio-launch".into(),
            transport: "stdio".into(),
            command: Some("/bin/sh".into()),
            args: vec![
                "-c".into(),
                "printf launched > \"$1\"".into(),
                "connection-fixture".into(),
                launch_marker.display().to_string(),
            ],
            env: Default::default(),
            url: None,
            allowed_agents: vec![],
            enabled: true,
            call_timeout_secs: 1,
        });
        // Even enabling the new switch does not implicitly enroll or launch a source.
        cfg.experiments.connection_fabric.enabled = true;
        let reopen_config = cfg.clone();
        let core = crate::Core::with_config(cfg).unwrap();
        let installation_id = core.connection_registrations.instance_id().to_owned();
        assert!(!core.config().experiments.device_connectivity.enabled);
        assert!(core.list_sessions_admin().unwrap().is_empty());
        assert!(!launch_marker.exists(), "配置开关不得拉起已登记的MCP进程");
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        drop(core);
        let reopened = crate::Core::with_config(reopen_config).unwrap();
        assert_eq!(
            reopened.connection_registrations.instance_id(),
            installation_id
        );
        assert!(reopened
            .connection_registrations
            .snapshot()
            .registrations()
            .is_empty());
        drop(reopened);
        let schema = crate::config_schema::config_schema();
        let fields = schema["groups"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|group| group["fields"].as_array().unwrap());
        let defaults = serde_json::to_value(ConnectionFabricConfig::default()).unwrap();
        for (key, value) in defaults.as_object().unwrap() {
            let name = format!("experiments.connectionFabric.{key}");
            let field = fields.clone().find(|field| field["key"] == name).unwrap();
            assert_eq!(field["default"], value.to_string());
            if key != "enabled" {
                assert_eq!(field["min"], 1);
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
