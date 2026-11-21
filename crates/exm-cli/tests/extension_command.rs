use exm_core::{config::ExmConfig, Core};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Command;

struct Workspace(PathBuf);

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn cli_command_lists_dependencies_and_rejects_stale_versions() {
    let workspace =
        Workspace(std::env::temp_dir().join(format!("exm-cli-{}", uuid::Uuid::new_v4())));
    let plugin = workspace.0.join("plugins/fixture");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        json!({
            "id": "fixture",
            "commands": { "read": { "version": 3 } },
            "tools": [{ "name": "read", "target": "http://127.0.0.1:1/fixture",
                "headers": { "X-Fixture": "${credentials.fixture}" } }]
        })
        .to_string(),
    )
    .unwrap();
    let mut cfg = ExmConfig::load(&workspace.0);
    cfg.use_mock = true;
    cfg.memory_enabled = false;
    cfg.save().unwrap();
    let core = Core::with_config(cfg).unwrap();
    core.registry.upsert_single(serde_json::from_value(json!({
        "identifier": "local-fixture", "name": "夹具", "domain": "测试", "tier": "unit", "description": "测试"
    })).unwrap(), None).unwrap();
    core.registry
        .set_active_single(Some("local-fixture"))
        .unwrap();
    let session = core.create_session("测试").unwrap().id;
    drop(core);

    let run = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_exm"))
            .args([
                "--output-format",
                "json",
                "--session",
                &session,
                "--workspace",
            ])
            .arg(&workspace.0)
            .arg("command")
            .args(args)
            .output()
            .unwrap();
        let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
            panic!(
                "无效命令结果: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        });
        (output.status.code(), value)
    };
    let (code, listing) = run(&["list"]);
    assert_eq!(code, Some(0));
    let entry = &listing["entries"][0];
    assert_eq!(entry["descriptor"]["name"], "plugin.fixture.read");
    assert_eq!(entry["descriptor"]["version"], 3);
    assert_eq!(entry["executable"], false);
    assert_eq!(entry["unavailableReason"], "missing_dependency");

    for (args, expected) in [
        (&["run", "plugin.fixture.read"][..], "version_mismatch"),
        (
            &["run", "plugin.fixture.read", "--command-version", "3"][..],
            "unavailable",
        ),
    ] {
        let (code, result) = run(args);
        assert_eq!(code, Some(1));
        assert_eq!(result["error"]["category"], expected);
        assert!(!result["requestId"].as_str().unwrap().is_empty());
    }
}
