//! Request/task-local project directory. Never mutate the registry to select a project.
use std::{
    future::Future,
    path::{Component, Path, PathBuf},
};

tokio::task_local! {
    static WORKSPACE: Option<PathBuf>;
}

pub fn current() -> Option<PathBuf> {
    WORKSPACE.try_with(Clone::clone).ok().flatten()
}

pub async fn scope<T>(root: Option<PathBuf>, future: impl Future<Output = T>) -> T {
    WORKSPACE.scope(root, future).await
}

/// Resolve a user-selected root, without changing any global default.
pub fn resolve(raw: &str) -> anyhow::Result<PathBuf> {
    let path = Path::new(raw);
    anyhow::ensure!(path.is_absolute(), "项目目录必须是网关所在机器上的绝对路径");
    let root = path.canonicalize()?;
    anyhow::ensure!(root.is_dir(), "项目路径不是目录");
    anyhow::ensure!(root.to_str().is_some(), "项目目录必须是 UTF-8 路径");
    Ok(root)
}

/// A persisted root must not silently follow a replacement symlink to another project.
pub fn pinned(raw: &str) -> anyhow::Result<PathBuf> {
    let root = resolve(raw)?;
    anyhow::ensure!(root == Path::new(raw), "项目目录已变化，请重新打开项目");
    Ok(root)
}

/// Resolve an internal checkpoint path without following symlinks or accepting traversal.
pub(crate) fn checkpoint(root: &Path, id: &str, relative: &Path) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "无效的检查点标识"
    );
    let tail = Path::new(".exmachina/checkpoints").join(id).join(relative);
    anyhow::ensure!(!relative.is_absolute(), "检查点路径越界");
    let mut path = root.to_path_buf();
    for part in tail.components() {
        let Component::Normal(part) = part else {
            anyhow::bail!("检查点路径越界")
        };
        path.push(part);
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            anyhow::ensure!(!meta.file_type().is_symlink(), "检查点路径不能包含符号链接");
        }
    }
    Ok(path)
}
