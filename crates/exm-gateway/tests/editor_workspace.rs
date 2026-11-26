//! The desktop editor exercises the real REST router in disposable projects.
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use exm_core::{config::ExmConfig, Core};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};
use tower::ServiceExt;

struct Fixture {
    core: Arc<Core>,
    app: axum::Router,
    dir: PathBuf,
    project: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("exm-editor-{}", uuid::Uuid::new_v4()));
        let project = dir.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let mut cfg = ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        cfg.security.auth_key = "fixture-key".into();
        let core = Arc::new(Core::with_config(cfg).unwrap());
        core.registry()
            .set_active_workspace(Some(project.to_str().unwrap()))
            .unwrap();
        let app = exm_gateway::build_router(core.clone());
        Self {
            core,
            app,
            dir,
            project,
        }
    }

    async fn request(&self, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("x-auth-key", "fixture-key")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 2_000_000)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn get(&self, uri: &str) -> Value {
        let (status, body) = self.request(Method::GET, uri, Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        body
    }

    async fn get_in_project(&self, uri: &str, workspace: &Path) -> Value {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(uri)
                    .header("x-auth-key", "fixture-key")
                    .header("x-exm-workspace", workspace.to_str().unwrap())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        let bytes = axum::body::to_bytes(response.into_body(), 2_000_000)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn get_project_list(&self, uri: &str, workspace: &Path) -> Value {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(uri)
                    .header("x-auth-key", "fixture-key")
                    .header("x-exm-workspace", workspace.to_str().unwrap())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        let bytes = axum::body::to_bytes(response.into_body(), 2_000_000)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn write(&self, name: &str, content: impl AsRef<[u8]>) {
        let target = self.project.join(name);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, content).unwrap();
    }

    fn init_git(&self) {
        git(&self.project, &["init", "-q"]);
    }

    fn commit(&self) {
        git(&self.project, &["add", "."]);
        git(
            &self.project,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-qm",
                "initial",
            ],
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn query(path: &str) -> String {
    let mut params = reqwest::Url::parse("http://fixture.invalid/api/fs/file").unwrap();
    params.query_pairs_mut().append_pair("path", path);
    format!("{}?{}", params.path(), params.query().unwrap())
}

#[tokio::test]
async fn editor_save_checkpoints_and_rejects_stale_snapshots() {
    let f = Fixture::new();
    f.write("src/main.rs", "original\n");
    assert_eq!(f.get(&query("src/main.rs")).await["content"], "original\n");
    let list = f.get("/api/fs/list?path=.").await;
    assert!(list["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["name"] == "src" && row["dir"] == true));
    let (status, _) = f.request(Method::PUT, "/api/fs/file", json!({ "path": "src/main.rs", "content": "edited\n", "originalContent": "original\n" })).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = f
        .request(
            Method::PUT,
            "/api/fs/file",
            json!({ "path": "src/main.rs", "content": "stale\n", "originalContent": "original\n" }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        std::fs::read_to_string(f.project.join("src/main.rs")).unwrap(),
        "edited\n"
    );
    let changes = f.get("/api/workspace/changes").await;
    assert_eq!(changes["source"], "checkpoint");
    assert_eq!(changes["files"][0]["path"], "src/main.rs");
    assert!(changes["files"][0]["diff"]["text"]
        .as_str()
        .unwrap()
        .contains("original"));
    let day = exm_core::types::now_iso()
        .chars()
        .take(10)
        .collect::<String>();
    assert_eq!(
        std::fs::read_to_string(
            f.project
                .join(format!(".exmachina/checkpoints/{day}/src/main.rs"))
        )
        .unwrap(),
        "original\n"
    );
    assert!(!f.dir.join(".exmachina/checkpoints").exists());
}

#[tokio::test]
async fn preview_bounds_and_binary_files_cannot_be_saved() {
    let f = Fixture::new();
    f.write("large.txt", "中".repeat(80_000));
    let preview = f.get(&query("large.txt")).await;
    assert_eq!(preview["truncated"], true);
    assert_eq!(preview["binary"], false);
    assert!(preview["content"].as_str().unwrap().len() <= 200 * 1024);
    f.write("binary.bin", [0, 1, 2, 3]);
    assert_eq!(f.get(&query("binary.bin")).await["binary"], true);
    for path in ["large.txt", "binary.bin", "."] {
        let (status, _) = f
            .request(
                Method::PUT,
                "/api/fs/file",
                json!({ "path": path, "content": "replacement" }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
    }
    let (status, _) = f.request(Method::GET, &query("."), Value::Null).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = f
        .request(
            Method::PUT,
            "/api/fs/file",
            json!({ "path": "new.txt", "content": "x".repeat(200 * 1024 + 1) }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        std::fs::read(f.project.join("binary.bin")).unwrap(),
        [0, 1, 2, 3]
    );
}

#[tokio::test]
async fn editor_paths_reject_escape_and_internal_metadata() {
    let f = Fixture::new();
    let outside = f.dir.join("outside.txt");
    std::fs::write(&outside, "outside").unwrap();
    for path in [
        "../outside.txt",
        "src/../../outside.txt",
        "/etc/passwd",
        r"C:\outside.txt",
        ".git/config",
        ".exmachina/config.json",
    ] {
        let (status, _) = f.request(Method::GET, &query(path), Value::Null).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        let (status, _) = f
            .request(
                Method::PUT,
                "/api/fs/file",
                json!({ "path": path, "content": "changed" }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
    }
    assert_eq!(std::fs::read_to_string(outside).unwrap(), "outside");
}

#[tokio::test]
async fn project_sessions_pin_editor_scope_without_changing_the_instance_workspace() {
    let f = Fixture::new();
    let second = f.dir.join("second-project");
    std::fs::create_dir_all(&second).unwrap();
    std::fs::write(second.join("only-second.txt"), "project two").unwrap();
    f.write("only-first.txt", "project one");
    let global_workspace = f
        .core
        .registry()
        .effective_workspace(&f.core.config().workspace_root);
    let target = f.core.registry().active_scope();
    let (status, created) = f.request(Method::POST, "/api/sessions", json!({
        "title": "Second project", "target": target, "workspace": second.canonicalize().unwrap(),
    })).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let session_id = created["id"].as_str().unwrap();
    assert_eq!(
        created["workspace"],
        second.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(
        f.core
            .registry()
            .effective_workspace(&f.core.config().workspace_root),
        global_workspace
    );

    let project_session = f.core.store.get_session(session_id).unwrap().unwrap();
    assert!(project_session
        .memory_partition
        .as_deref()
        .is_some_and(|partition| partition.starts_with("project-")));
    assert!(
        f.get("/api/sessions?all=true")
            .await
            .as_array()
            .unwrap()
            .is_empty(),
        "instance session list must not leak project sessions"
    );
    let hidden = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/api/sessions/{session_id}"))
                .header("x-auth-key", "fixture-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(hidden.status(), StatusCode::NOT_FOUND);
    let wrong_project = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/api/sessions/{session_id}"))
                .header("x-auth-key", "fixture-key")
                .header("x-exm-workspace", f.project.to_str().unwrap())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(wrong_project.status(), StatusCode::NOT_FOUND);

    let listed = f
        .get_project_list(
            &format!(
                "/api/sessions?all=true&workspace={}",
                second.canonicalize().unwrap().to_str().unwrap()
            ),
            &second.canonicalize().unwrap(),
        )
        .await;
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["id"], session_id);

    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/fs/list?path=.")
                .header("x-auth-key", "fixture-key")
                .header(
                    "x-exm-workspace",
                    second.canonicalize().unwrap().to_str().unwrap(),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 2_000_000)
        .await
        .unwrap();
    let listing: Value = serde_json::from_slice(&bytes).unwrap();
    let names: Vec<&str> = listing["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"only-second.txt"));
    assert!(!names.contains(&"only-first.txt"));

    let session = f
        .get_in_project(
            &format!("/api/sessions/{session_id}"),
            &second.canonicalize().unwrap(),
        )
        .await;
    assert_eq!(session["workspace"], created["workspace"]);

    let mut third = second.clone();
    third.push("nested");
    std::fs::create_dir_all(&third).unwrap();
    let (status, sibling) = f.request(Method::POST, "/api/sessions", json!({
        "title": "Sibling project", "target": target, "workspace": third.canonicalize().unwrap(),
    })).await;
    assert_eq!(status, StatusCode::OK, "{sibling}");
    let sibling_session = f
        .core
        .store
        .get_session(sibling["id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_ne!(
        project_session.memory_partition, sibling_session.memory_partition,
        "project memory namespaces must differ"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_checkpoint_redirects_and_fifos_are_rejected() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    f.write("main.txt", "original");
    let outside = f.dir.join("outside.txt");
    std::fs::write(&outside, "outside").unwrap();
    symlink(&outside, f.project.join("link.txt")).unwrap();
    assert_eq!(
        f.request(Method::GET, &query("link.txt"), Value::Null)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        f.request(
            Method::PUT,
            "/api/fs/file",
            json!({ "path": "link.txt", "content": "changed" })
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let day = exm_core::types::now_iso()
        .chars()
        .take(10)
        .collect::<String>();
    let checkpoint = f.project.join(format!(".exmachina/checkpoints/{day}"));
    std::fs::create_dir_all(&checkpoint).unwrap();
    symlink(&outside, checkpoint.join("main.txt")).unwrap();
    assert_eq!(
        f.request(
            Method::PUT,
            "/api/fs/file",
            json!({ "path": "main.txt", "content": "changed" })
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "outside");
    assert_eq!(
        std::fs::read_to_string(f.project.join("main.txt")).unwrap(),
        "original"
    );
    assert!(Command::new("mkfifo")
        .arg(f.project.join("pipe"))
        .status()
        .unwrap()
        .success());
    let (status, _) = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        f.request(Method::GET, &query("pipe"), Value::Null),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let listing = f.get("/api/fs/list?path=.").await;
    assert!(!listing["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["name"] == "pipe" || row["name"] == "link.txt"));
    let checkpoint_day = f.project.join(".exmachina/checkpoints/2099-01-01");
    symlink(&f.dir, checkpoint_day).unwrap();
    let changes = f.get("/api/workspace/changes").await;
    assert!(changes["files"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn git_supports_unborn_repositories_and_literal_special_filenames() {
    let f = Fixture::new();
    assert_eq!(f.get("/api/git/overview").await["repo"], false);
    assert_eq!(
        f.request(
            Method::POST,
            "/api/git/op",
            json!({ "op": "stage", "path": "." })
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    f.init_git();
    f.write("wild*.txt", "literal");
    f.write("wild-other.txt", "other");
    f.write("space and\nnewline.txt", "special");
    assert_eq!(f.get("/api/git/overview").await["repo"], true);
    assert_eq!(
        f.request(
            Method::POST,
            "/api/git/op",
            json!({ "op": "stage", "path": "wild*.txt" })
        )
        .await
        .0,
        StatusCode::OK
    );
    let staged = git(&f.project, &["diff", "--cached", "--name-only", "-z"]);
    assert_eq!(staged, "wild*.txt\0");
    assert_eq!(
        f.request(
            Method::POST,
            "/api/git/op",
            json!({ "op": "unstage", "path": "wild*.txt" })
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(git(&f.project, &["diff", "--cached", "--name-only"]).is_empty());
    let changes = f.get("/api/workspace/changes").await;
    assert!(changes["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["path"] == "space and\nnewline.txt"));
    assert_eq!(
        f.request(
            Method::POST,
            "/api/git/op",
            json!({ "op": "switch", "branch": "--help" })
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    f.commit();
    git(&f.project, &["branch", "feature"]);
    let (status, body) = f
        .request(
            Method::POST,
            "/api/git/op",
            json!({ "op": "switch", "branch": "feature" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(f.get("/api/git/overview").await["branch"], "feature");
}

#[tokio::test]
async fn git_approval_stays_bound_to_its_original_project() {
    let f = Fixture::new();
    f.init_git();
    f.write("main.txt", "original");
    f.commit();
    f.write("main.txt", "edited");
    let (status, approval) = f
        .request(
            Method::POST,
            "/api/git/op",
            json!({ "op": "discard", "path": "main.txt" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(approval["pending"], true);
    assert_eq!(
        std::fs::read_to_string(f.project.join("main.txt")).unwrap(),
        "edited"
    );
    let other = f.dir.join("other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("main.txt"), "other project").unwrap();
    assert_eq!(
        f.request(Method::PUT, "/api/workspace", json!({ "path": other }))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        f.request(
            Method::PUT,
            "/api/fs/file",
            json!({ "path": "main.txt", "content": "stale editor write", "workspace": f.project })
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let id = approval["approvalId"].as_str().unwrap();
    let resolved = f.core.approval_decide(id, true).await.unwrap();
    assert_eq!(resolved.status, "executed", "{:?}", resolved.result);
    assert_eq!(
        std::fs::read_to_string(f.project.join("main.txt")).unwrap(),
        "original"
    );
    assert_eq!(
        std::fs::read_to_string(other.join("main.txt")).unwrap(),
        "other project"
    );
    assert!(f.core.approval_decide(id, true).await.is_err());
    let denied = f
        .core
        .request_editor_git_approval(&f.project, vec!["reset".into(), "--hard".into()])
        .await
        .unwrap();
    f.write("main.txt", "keep denied changes");
    assert_eq!(
        f.core
            .approval_decide(&denied.id, false)
            .await
            .unwrap()
            .status,
        "denied"
    );
    assert_eq!(
        std::fs::read_to_string(f.project.join("main.txt")).unwrap(),
        "keep denied changes"
    );
}

#[tokio::test]
async fn nested_configuration_round_trips_and_preserves_unspecified_fields() {
    let f = Fixture::new();
    let initial = f.get("/api/config").await;
    let (status, body) = f.request(Method::PUT, "/api/config", json!({
        "memory": { "autoDecay": false }, "compaction": { "triggerRatio": 0.77 },
        "computer": { "enabled": false, "maxEdge": 1024, "actionIntervalMs": 350 },
        "knowledge": { "injectTopK": 3, "injectMaxChars": 777 },
        "security": { "sanitizeRules": { "fakeSystemPrompt": false, "hiddenUnicode": false } }
    })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let next = f.get("/api/config").await;
    assert_eq!(next["memory"]["autoDecay"], false);
    assert_eq!(next["computer"]["maxEdge"], 1024);
    assert_eq!(next["computer"]["actionIntervalMs"], 350);
    assert_eq!(next["knowledge"]["injectTopK"], 3);
    assert_eq!(next["knowledge"]["injectMaxChars"], 777);
    assert!((next["compaction"]["triggerRatio"].as_f64().unwrap() - 0.77).abs() < 0.001);
    assert_eq!(next["security"]["sanitizeRules"]["fakeSystemPrompt"], false);
    assert_eq!(next["security"]["sanitizeRules"]["hiddenUnicode"], false);
    assert_eq!(
        next["security"]["sanitizeRules"]["hijackDirective"],
        initial["security"]["sanitizeRules"]["hijackDirective"]
    );
    assert_eq!(f.core.config().security.auth_key, "fixture-key");
    let reloaded = ExmConfig::load(&f.dir);
    assert!(!reloaded.memory_auto_decay);
    assert_eq!(reloaded.knowledge_inject_top_k, 3);
    assert!(!reloaded.security.sanitize_rules.hidden_unicode);
    assert!((reloaded.compaction.trigger_ratio - 0.77).abs() < 0.001);
}
