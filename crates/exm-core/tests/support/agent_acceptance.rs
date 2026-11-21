//! Shared deterministic execution fixtures. All files are created below the
//! explicitly approved /tmp/opencode tree and the oracle is outside projectDir.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const FIXTURE_ROOT: &str = "/tmp/opencode/agent-practical-workbench/fixtures";
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

pub struct IsolatedProject {
    root: PathBuf,
    project: PathBuf,
    exec_home: PathBuf,
    outside: PathBuf,
    oracle: PathBuf,
}

impl IsolatedProject {
    pub fn new(label: &str) -> Self {
        let parent = Path::new(FIXTURE_ROOT);
        std::fs::create_dir_all(parent).expect("create approved acceptance fixture root");
        let parent = parent.canonicalize().expect("canonicalize fixture root");
        assert_eq!(
            parent,
            Path::new(FIXTURE_ROOT),
            "fixture root must not follow a symlink outside the approved tmp tree"
        );
        let safe_label = label
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
            .take(32)
            .collect::<String>();
        assert!(
            !safe_label.is_empty(),
            "fixture label must contain safe characters"
        );
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = parent.join(format!("{safe_label}-{}-{id}", std::process::id()));
        std::fs::create_dir(&root).expect("create unique acceptance fixture");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .expect("secure fixture root");
        }

        let project = root.join("project");
        let exec_home = root.join("exec-home");
        let outside = root.join("outside");
        let oracle = root.join("oracle");
        for path in [
            project.as_path(),
            exec_home.join("projects/demo").as_path(),
            outside.as_path(),
            oracle.as_path(),
        ] {
            std::fs::create_dir_all(path).expect("create fixture area");
        }
        Self {
            root,
            project,
            exec_home,
            outside,
            oracle,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn project(&self) -> &Path {
        &self.project
    }

    pub fn exec_home(&self) -> &Path {
        &self.exec_home
    }

    pub fn bound_project(&self) -> PathBuf {
        self.exec_home.join("projects/demo")
    }

    pub fn outside(&self) -> &Path {
        &self.outside
    }

    pub fn oracle(&self) -> &Path {
        &self.oracle
    }

    pub fn write_png(&self, path: impl AsRef<Path>) -> PathBuf {
        let path = self.project.join(path);
        std::fs::create_dir_all(path.parent().expect("PNG has a parent")).unwrap();
        std::fs::write(&path, tiny_png()).expect("write fixture PNG");
        path
    }

    pub async fn spawn_process(
        &self,
        program: &str,
        args: &[&str],
    ) -> std::io::Result<ProcessFixture> {
        let child = tokio::process::Command::new(program)
            .args(args)
            .current_dir(&self.project)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        Ok(ProcessFixture { child: Some(child) })
    }
}

impl Drop for IsolatedProject {
    fn drop(&mut self) {
        // This instance exclusively created this unique child directory.
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub struct ProcessFixture {
    child: Option<tokio::process::Child>,
}

impl ProcessFixture {
    pub fn id(&self) -> Option<u32> {
        self.child.as_ref().and_then(tokio::process::Child::id)
    }

    pub async fn wait_output(mut self) -> std::io::Result<std::process::Output> {
        self.child
            .take()
            .expect("process fixture already consumed")
            .wait_with_output()
            .await
    }

    pub async fn terminate(&mut self) -> std::io::Result<()> {
        if let Some(child) = self.child.as_mut() {
            if child.try_wait()?.is_none() {
                child.start_kill()?;
                let _ = child.wait().await?;
            }
        }
        self.child.take();
        Ok(())
    }
}

impl Drop for ProcessFixture {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
    }
}

/// Deterministic barrier for late model responses and approval races. `arrive`
/// records that the operation reached the fault point and then blocks until
/// the test explicitly releases it.
#[derive(Clone)]
pub struct FaultBarrier {
    entered: tokio::sync::watch::Sender<bool>,
    released: tokio::sync::watch::Sender<bool>,
}

impl FaultBarrier {
    pub fn new() -> Self {
        let (entered, _) = tokio::sync::watch::channel(false);
        let (released, _) = tokio::sync::watch::channel(false);
        Self { entered, released }
    }

    pub async fn arrive_and_wait(&self) {
        self.entered.send_replace(true);
        let mut released = self.released.subscribe();
        while !*released.borrow() {
            if released.changed().await.is_err() {
                break;
            }
        }
    }

    pub async fn wait_until_entered(&self) {
        let mut entered = self.entered.subscribe();
        while !*entered.borrow() {
            if entered.changed().await.is_err() {
                break;
            }
        }
    }

    pub fn release(&self) {
        self.released.send_replace(true);
    }
}

fn tiny_png() -> &'static [u8] {
    &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x62, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ]
}
