use exm_core::{config::ExmConfig, Core};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

struct Workspace(PathBuf);

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(workspace: &Workspace, args: &[&str]) -> (bool, Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_exm"))
        .args(["--output-format", "json", "--workspace"])
        .arg(&workspace.0)
        .arg("spec")
        .args(args)
        .output()
        .unwrap();
    let value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!("invalid spec command output: {}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
    });
    (output.status.success(), value)
}

#[test]
fn cli_spec_lifecycle_exports_only_explicitly_and_preserves_revisioned_fields() {
    let workspace = Workspace(std::env::temp_dir().join(format!("exm-cli-spec-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(&workspace.0).unwrap();
    let mut config = ExmConfig::load(&workspace.0);
    config.use_mock = true;
    config.save().unwrap();
    let core = Core::with_config(config).unwrap();
    let session = core.create_session("CLI spec export").unwrap().id;
    drop(core);

    let (ok, draft) = run(&workspace, &["draft", &session, "safe CLI export"]);
    assert!(ok);
    assert_eq!(draft["writtenToProject"], false);
    assert!(!workspace.0.join("docs/spec.md").exists());
    assert!(!workspace.0.join("openspec").exists());

    let (ok, edited) = run(&workspace, &["edit", &session, "revised CLI export", "--revision", "1"]);
    assert!(ok);
    assert_eq!(edited["spec"]["id"], draft["spec"]["id"]);
    assert_eq!(edited["spec"]["revision"], 2);
    let (ok, stale) = run(&workspace, &["edit", &session, "must not replace", "--revision", "1"]);
    assert!(!ok);
    assert_eq!(stale["error"]["category"], "conflict");

    let (ok, receipt) = run(&workspace, &["export", &session, "docs/spec.md"]);
    assert!(ok);
    assert_eq!(receipt["export"]["revision"], 2);
    let content = std::fs::read_to_string(workspace.0.join("docs/spec.md")).unwrap();
    assert!(content.contains("revised CLI export"));
    assert!(!run(&workspace, &["export", &session, "../outside.md"]).0);
    assert!(!run(&workspace, &["export", &session, "openspec/change.md"]).0);
    assert!(!workspace.0.join("openspec").exists());

    assert!(!run(&workspace, &["clear", &session, "--revision", "1"]).0);
    assert!(run(&workspace, &["clear", &session, "--revision", "2"]).0);
    assert!(run(&workspace, &["show", &session]).1.is_null());
    assert!(workspace.0.join("docs/spec.md").is_file(), "clearing session state must not delete an exported artifact");
}
