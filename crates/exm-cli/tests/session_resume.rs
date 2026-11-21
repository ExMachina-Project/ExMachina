use exm_core::{config::ExmConfig, Core};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Workspace(PathBuf);

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn cli_sessions_resume_continues_the_requested_session_without_creating_another() {
    let workspace =
        Workspace(std::env::temp_dir().join(format!("exm-cli-resume-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(&workspace.0).unwrap();
    let mut config = ExmConfig::load(&workspace.0);
    config.use_mock = true;
    config.save().unwrap();
    let core = Core::with_config(config).unwrap();
    let session = core.create_session("resume target").unwrap();
    let session_id = session.id;
    drop(core);

    let mut child = Command::new(env!("CARGO_BIN_EXE_exm"))
        .args([
            "--workspace",
            workspace.0.to_str().unwrap(),
            "sessions",
            "resume",
            &session_id,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"exit\n").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "resume failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&session_id),
        "did not resume requested session: {stdout}"
    );

    let core = Core::create(&workspace.0).unwrap();
    let sessions = core.list_sessions_in_scope().unwrap();
    assert_eq!(
        sessions.len(),
        1,
        "resume must not create a replacement session"
    );
    assert_eq!(sessions[0].id, session_id);
}
