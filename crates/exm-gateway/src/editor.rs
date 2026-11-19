//! Desktop workbench endpoints. All filesystem operations use the selected workspace.
use super::*;
use std::path::{Path as FsPath, PathBuf};
use tokio::io::AsyncReadExt;

const FILE_CAP: usize = 200 * 1024;
const DIFF_CAP: usize = 100 * 1024;
type ApiError = (StatusCode, Json<Value>);

fn error(status: StatusCode, text: impl ToString) -> ApiError {
    (status, Json(json!({ "error": text.to_string() })))
}

fn root(st: &AppState) -> Result<PathBuf, ApiError> {
    st.core
        .orchestrator()
        .tools
        .editor_path(".")
        .map_err(|e| error(StatusCode::BAD_REQUEST, e))
}

#[derive(Deserialize)]
pub struct FileQuery {
    path: String,
}

pub async fn list(
    State(st): State<AppState>,
    Query(q): Query<FileQuery>,
) -> Result<Json<Value>, ApiError> {
    let root = root(&st)?;
    let dir = st
        .core
        .orchestrator()
        .tools
        .editor_path_in(&root, &q.path)
        .map_err(|e| error(StatusCode::BAD_REQUEST, e))?;
    let mut entries = tokio::fs::read_dir(&dir)
        .await
        .map_err(|e| error(StatusCode::NOT_FOUND, e))?;
    let mut rows = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e))?
    {
        let kind = entry
            .file_type()
            .await
            .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if kind.is_symlink()
            || !(kind.is_dir() || kind.is_file())
            || name == ".git"
            || name == ".exmachina"
        {
            continue;
        }
        let meta = entry
            .metadata()
            .await
            .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        let rel = entry
            .path()
            .strip_prefix(&root)
            .map_err(|e| error(StatusCode::BAD_REQUEST, e))?
            .to_string_lossy()
            .replace('\\', "/");
        rows.push(json!({ "name": name, "path": rel, "dir": kind.is_dir(), "size": meta.len() }));
        if rows.len() >= 2000 {
            break;
        }
    }
    rows.sort_by(|a, b| {
        b["dir"]
            .as_bool()
            .cmp(&a["dir"].as_bool())
            .then(a["name"].as_str().cmp(&b["name"].as_str()))
    });
    Ok(Json(json!({ "path": q.path, "entries": rows })))
}

pub async fn read(
    State(st): State<AppState>,
    Query(q): Query<FileQuery>,
) -> Result<Json<Value>, ApiError> {
    let workspace = root(&st)?;
    let path = st
        .core
        .orchestrator()
        .tools
        .editor_path_in(&workspace, &q.path)
        .map_err(|e| error(StatusCode::BAD_REQUEST, e))?;
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| error(StatusCode::NOT_FOUND, e))?;
    if !meta.is_file() {
        return Err(error(StatusCode::BAD_REQUEST, "不是普通文件"));
    }
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|e| error(StatusCode::NOT_FOUND, e))?;
    let meta = file
        .metadata()
        .await
        .map_err(|e| error(StatusCode::BAD_REQUEST, e))?;
    if !meta.is_file() {
        return Err(error(StatusCode::BAD_REQUEST, "不是普通文件"));
    }
    let mut bytes = Vec::new();
    file.take(FILE_CAP as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| error(StatusCode::BAD_REQUEST, e))?;
    let truncated = bytes.len() > FILE_CAP;
    bytes.truncate(FILE_CAP);
    // A preview may end in the middle of a UTF-8 character; it remains a text file.
    if truncated {
        if let Err(err) = std::str::from_utf8(&bytes) {
            if err.error_len().is_none() {
                bytes.truncate(err.valid_up_to());
            }
        }
    }
    let binary = bytes.contains(&0) || std::str::from_utf8(&bytes).is_err();
    Ok(Json(
        json!({ "path": q.path, "workspace": workspace, "size": meta.len(), "content": String::from_utf8_lossy(&bytes), "truncated": truncated, "binary": binary }),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveBody {
    path: String,
    content: String,
    /// Optimistic concurrency: reject stale editor snapshots instead of overwriting agent edits.
    original_content: Option<String>,
    workspace: Option<PathBuf>,
}

pub async fn save(
    State(st): State<AppState>,
    Json(body): Json<SaveBody>,
) -> Result<Json<Value>, ApiError> {
    if body.content.len() > FILE_CAP {
        return Err(error(StatusCode::BAD_REQUEST, "编辑器保存上限为 200KB"));
    }
    let workspace = root(&st)?;
    if let Some(expected) = &body.workspace {
        if expected.canonicalize().ok().as_ref() != Some(&workspace) {
            return Err(error(StatusCode::CONFLICT, "工作区已切换，请重新打开文件"));
        }
    }
    let path = st
        .core
        .orchestrator()
        .tools
        .editor_path_in(&workspace, &body.path)
        .map_err(|e| error(StatusCode::BAD_REQUEST, e))?;
    static SAVE_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    let _save = SAVE_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    if let Ok(meta) = std::fs::metadata(&path) {
        if !meta.is_file() {
            return Err(error(StatusCode::BAD_REQUEST, "只能保存普通文件"));
        }
        if meta.len() > FILE_CAP as u64 {
            return Err(error(StatusCode::BAD_REQUEST, "文件超过编辑器保存上限"));
        }
        let current = std::fs::read(&path).map_err(|e| error(StatusCode::BAD_REQUEST, e))?;
        if current.contains(&0) || std::str::from_utf8(&current).is_err() {
            return Err(error(StatusCode::BAD_REQUEST, "二进制文件只允许预览"));
        }
    }
    if let Some(original) = body.original_content {
        let current = std::fs::read_to_string(&path).map_err(|e| error(StatusCode::CONFLICT, e))?;
        if original != current {
            return Err(error(
                StatusCode::CONFLICT,
                "文件已被其他操作修改，请重新加载后合并",
            ));
        }
    }
    let message = st
        .core
        .save_workspace_file_in(&workspace, &body.path, &body.content)
        .map_err(|e| error(StatusCode::BAD_REQUEST, e))?;
    Ok(Json(json!({ "ok": true, "message": message })))
}

async fn git(root: &FsPath, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::process::Command::new("git")
            .args(["--no-pager", "--literal-pathspecs"])
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .current_dir(root)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "Git 操作超时".to_string())?
    .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(output.stdout)
}
async fn git_text(root: &FsPath, args: &[&str]) -> Result<String, String> {
    git(root, args)
        .await
        .map(|bytes| String::from_utf8_lossy(&bytes).trim_end().to_owned())
}

pub async fn overview(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let root = root(&st)?;
    if !root.join(".git").exists() {
        return Ok(Json(json!({ "repo": false })));
    }
    let branch = git_text(&root, &["symbolic-ref", "--short", "HEAD"])
        .await
        .unwrap_or_else(|_| "HEAD".into());
    let log = git_text(&root, &["log", "--oneline", "-20"])
        .await
        .unwrap_or_default();
    let branches = git_text(
        &root,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads/"],
    )
    .await
    .map_err(|e| error(StatusCode::BAD_REQUEST, e))?;
    Ok(Json(
        json!({ "repo": true, "branch": branch, "log": log.lines().collect::<Vec<_>>(), "branches": branches.lines().collect::<Vec<_>>() }),
    ))
}

pub async fn changes(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let root = root(&st)?;
    if !root.join(".git").exists() {
        let mut latest = std::collections::BTreeMap::new();
        let checkpoints = root.join(".exmachina/checkpoints");
        for path in [root.join(".exmachina"), checkpoints.clone()] {
            if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
                return Err(error(StatusCode::BAD_REQUEST, "检查点目录不能是符号链接"));
            }
        }
        // Editor checkpoints are local to the selected workspace, not the gateway installation.
        if let Ok(days) = std::fs::read_dir(&checkpoints) {
            for day in days.flatten() {
                if !day
                    .file_type()
                    .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
                {
                    continue;
                }
                let mut stack = vec![day.path()];
                while let Some(dir) = stack.pop() {
                    let Ok(entries) = std::fs::read_dir(&dir) else {
                        continue;
                    };
                    for entry in entries.flatten() {
                        let Ok(kind) = entry.file_type() else {
                            continue;
                        };
                        if kind.is_symlink() {
                            continue;
                        }
                        if kind.is_dir() {
                            stack.push(entry.path());
                        } else if kind.is_file() {
                            let entry_path = entry.path();
                            let Ok(rel) = entry_path.strip_prefix(day.path()) else {
                                continue;
                            };
                            let name = rel.to_string_lossy().replace('\\', "/");
                            latest
                                .entry(name)
                                .and_modify(|p: &mut PathBuf| {
                                    if entry.path() > *p {
                                        *p = entry.path();
                                    }
                                })
                                .or_insert(entry.path());
                        }
                    }
                }
            }
        }
        let mut files = Vec::new();
        for (rel, checkpoint) in latest.into_iter().take(50) {
            let Ok(path) = st.core.orchestrator().tools.editor_path_in(&root, &rel) else {
                continue;
            };
            if std::fs::metadata(&checkpoint).is_ok_and(|m| m.len() > FILE_CAP as u64) {
                continue;
            }
            if std::fs::metadata(&path).is_ok_and(|m| !m.is_file() || m.len() > FILE_CAP as u64) {
                continue;
            }
            let Ok(old) = std::fs::read_to_string(&checkpoint) else {
                continue;
            };
            let new = std::fs::read_to_string(&path).unwrap_or_default();
            if old == new {
                continue;
            }
            let diff = exm_core::patch::simple_unified_diff(&old, &new, &rel, 3);
            files.push(json!({ "path": rel, "status": if path.exists() { "M" } else { "D" }, "staged": false, "diff": clipped_diff(diff) }));
        }
        return Ok(Json(json!({ "source": "checkpoint", "files": files })));
    }
    let branch = git_text(&root, &["symbolic-ref", "--short", "HEAD"])
        .await
        .unwrap_or_default();
    let status = git(
        &root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )
    .await
    .map_err(|e| error(StatusCode::BAD_REQUEST, e))?;
    let mut records = status.split(|b| *b == 0).filter(|r| !r.is_empty());
    let mut files = Vec::new();
    let has_head = git(&root, &["rev-parse", "--verify", "HEAD"]).await.is_ok();
    while let Some(record) = records.next() {
        if record.len() < 4 {
            continue;
        }
        let x = record[0];
        let y = record[1];
        let name = String::from_utf8_lossy(&record[3..]).to_string();
        if matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C') {
            let _ = records.next();
        }
        let untracked = x == b'?';
        let diff = if untracked {
            None
        } else if has_head {
            git_text(
                &root,
                &[
                    "diff",
                    "--no-ext-diff",
                    "--no-textconv",
                    "HEAD",
                    "--",
                    &name,
                ],
            )
            .await
            .ok()
        } else {
            git_text(
                &root,
                &[
                    "diff",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--cached",
                    "--",
                    &name,
                ],
            )
            .await
            .ok()
        };
        files.push(json!({ "path": name, "status": if untracked { "A".to_string() } else { char::from(if x == b' ' { y } else { x }).to_string() },
            "staged": !untracked && x != b' ', "diff": diff.map(clipped_diff) }));
        if files.len() >= 50 {
            break;
        }
    }
    Ok(Json(
        json!({ "source": "git", "branch": branch, "files": files }),
    ))
}

fn clipped_diff(mut text: String) -> Value {
    let truncated = text.len() > DIFF_CAP;
    if truncated {
        let mut end = DIFF_CAP;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    json!({ "text": text, "truncated": truncated })
}

#[derive(Deserialize)]
pub struct GitBody {
    op: String,
    path: Option<String>,
    message: Option<String>,
    branch: Option<String>,
}

pub async fn operation(
    State(st): State<AppState>,
    Json(b): Json<GitBody>,
) -> Result<Json<Value>, ApiError> {
    let root = root(&st)?;
    if !root.join(".git").exists() {
        return Err(error(StatusCode::BAD_REQUEST, "所选工作区不是 Git 仓库"));
    }
    let path = b.path.as_deref().unwrap_or(".");
    if matches!(b.op.as_str(), "stage" | "unstage" | "discard") {
        st.core.orchestrator().tools.editor_path_in(&root, path)
            .map_err(|e| error(StatusCode::BAD_REQUEST, e))?;
    }
    let args: Vec<&str> = match b.op.as_str() {
        "stage" => vec!["add", "--", path],
        "unstage" => {
            if git(&root, &["rev-parse", "--verify", "HEAD"]).await.is_ok() {
                vec!["reset", "--", path]
            } else {
                vec!["rm", "--cached", "-r", "--", path]
            }
        }
        "commit" => {
            let message = b
                .message
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| error(StatusCode::BAD_REQUEST, "提交信息不能为空"))?;
            vec!["commit", "-m", message]
        }
        "switch" => {
            let branch = b
                .branch
                .as_deref()
                .filter(|s| !s.starts_with('-') && !s.is_empty())
                .ok_or_else(|| error(StatusCode::BAD_REQUEST, "分支名不能为空或选项"))?;
            vec!["switch", "--", branch]
        }
        "discard" | "reset_hard" | "push" => {
            let args = match b.op.as_str() {
                "discard" => vec!["restore".into(), "--".into(), path.into()],
                "reset_hard" => vec!["reset".into(), "--hard".into()],
                _ => vec!["push".into()],
            };
            let approval = st
                .core
                .request_editor_git_approval(&root, args)
                .await
                .map_err(|e| error(StatusCode::BAD_REQUEST, e))?;
            return Ok(Json(
                json!({ "pending": true, "approvalId": approval.id, "command": approval.command }),
            ));
        }
        _ => return Err(error(StatusCode::BAD_REQUEST, "未知 Git 操作")),
    };
    let output = git_text(&root, &args)
        .await
        .map_err(|e| error(StatusCode::BAD_REQUEST, e))?;
    st.core.store.audit_tool_best_effort(
        "desktop",
        "git",
        &json!({ "op": b.op, "path": b.path }),
        &output,
        0,
    );
    Ok(Json(json!({ "ok": true, "output": output })))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/fs/list", get(list))
        .route("/api/fs/file", get(read).put(save))
        .route("/api/workspace/changes", get(changes))
        .route("/api/git/overview", get(overview))
        .route("/api/git/op", post(operation))
}
