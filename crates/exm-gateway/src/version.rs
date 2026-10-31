//! gateway/version —— 自 lib.rs 按资源域拆出（纯搬移，零行为变更）

use super::*;

// ---------------------------------------------------------------- 版本与更新

/// 构建版本:工作区 Cargo 版本 + 编译期注入的 git 短哈希(Dockerfile ARG GIT_HASH → ENV EXM_GIT_HASH)
pub(crate) fn build_version() -> serde_json::Value {
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        // 运行时读取(runtime 阶段 ENV 注入)——编译期注入会让 commit 变化作废整条编译缓存
        "gitHash": std::env::var("EXM_GIT_HASH").unwrap_or_else(|_| "dev".into()),
    })
}

/// 本机版本(GET /api/version)
pub(crate) async fn version_info() -> impl IntoResponse {
    Json(build_version())
}

/// 更新检查(GET /api/version/check):分支感知——本地在什么分支就对比该分支的上游。
/// main 用户对比 origin/main,dev 开发者对比 origin/dev,互不干扰;经 /host 视图执行 git。
pub(crate) async fn version_check() -> impl IntoResponse {
    let mut out = build_version();
    if !std::path::Path::new("/host/.git").exists() {
        out["note"] = serde_json::json!("未挂载仓库(/host),无法检查更新;请按设置页指引手动更新");
        return Json(out);
    }
    let script = r#"
cd /host || exit 9
git config --global --add safe.directory /host >/dev/null 2>&1
branch=$(git rev-parse --abbrev-ref HEAD) || exit 8
remote=$(git config "branch.$branch.remote" 2>/dev/null || echo origin)
git fetch --quiet "$remote" "$branch" 2>/dev/null
head_sha=$(git rev-parse HEAD)
up_sha=$(git rev-parse --verify "$remote/$branch" 2>/dev/null)
up_date=$(git show -s --format=%cI "$remote/$branch" 2>/dev/null)
url=$(git config "remote.$remote.url" 2>/dev/null)
printf '%s\n%s\n%s\n%s\n%s\n%s\n' "$branch" "$remote" "$head_sha" "$up_sha" "$up_date" "$url"
"#;
    let exec = tokio::time::timeout(
        std::time::Duration::from_secs(12),
        tokio::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .output(),
    )
    .await;
    let lines: Vec<String> = match exec {
        Ok(Ok(out)) => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .collect(),
        Ok(Err(e)) => {
            out["note"] = serde_json::json!(format!("仓库视图不可用:{e}"));
            return Json(out);
        }
        Err(_) => {
            out["note"] = serde_json::json!("检查超时(网络不可达或仓库过大)");
            return Json(out);
        }
    };
    let get = |i: usize| lines.get(i).cloned().unwrap_or_default();
    let (branch, remote, head_sha, up_sha, up_date, remote_url) =
        (get(0), get(1), get(2), get(3), get(4), get(5));
    if branch.is_empty() || head_sha.is_empty() {
        out["note"] = serde_json::json!("无法解析本地仓库状态(分离 HEAD 或视图异常)");
        return Json(out);
    }
    let short = |sha: &str| sha.chars().take(7).collect::<String>();
    out["gitHash"] = serde_json::json!(short(&head_sha));
    out["branch"] = serde_json::json!(branch);
    if up_sha.is_empty() {
        out["note"] = serde_json::json!(format!(
            "无法解析上游 {remote}/{branch}(fetch 失败或上游未设置;可稍后重试,或手动 git fetch 后再查)"
        ));
        out["updateAvailable"] = serde_json::Value::Null;
        return Json(out);
    }
    // github 提交链接:从 remote url 反解 owner/repo(https 或 ssh 形态)
    let commit_url = remote_url
        .strip_suffix(".git")
        .unwrap_or(&remote_url)
        .trim_start_matches("https://github.com/")
        .trim_start_matches("git@github.com:")
        .split('/')
        .collect::<Vec<_>>()
        .join("/");
    let commit_url = if commit_url.contains('/') {
        format!("https://github.com/{commit_url}/commit/{up_sha}")
    } else {
        String::new()
    };
    out["upstream"] = serde_json::json!({
        "ref": format!("{remote}/{branch}"),
        "sha": up_sha,
        "short": short(&up_sha),
        "date": up_date,
        "url": commit_url,
    });
    out["updateAvailable"] = serde_json::json!(head_sha != up_sha);
    Json(out)
}

/// 一键更新任务是否在运行(pgrep 探测;脚本为独立进程组,容器重启后自然消失)
pub(crate) fn update_job_running() -> bool {
    std::process::Command::new("sh")
        .arg("-c")
        .arg("pgrep -f apply-update.sh >/dev/null 2>&1")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 一键更新(POST /api/version/apply):后台执行 scripts/apply-update.sh(git pull → build → up -d)。
/// 前置:compose 挂载仓库根到 /host + docker.sock。返回 202,进度走 /api/version/apply/status。
pub(crate) async fn version_apply() -> impl IntoResponse {
    if !std::path::Path::new("/host/docker-compose.yml").is_file() {
        return (StatusCode::BAD_REQUEST, Json(json!({
            "error": "一键更新需要 compose 挂载:仓库根 → /host 与 /var/run/docker.sock(当前部署未挂载,请按设置页指引手动更新)"
        }))).into_response();
    }
    if update_job_running() {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "更新任务已在进行中" })),
        )
            .into_response();
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // 独立进程组:容器重启(SIGTERM)不会中断 build/up 流程
        let spawned = std::process::Command::new("sh")
            .arg("-c")
            .arg("exec setsid sh /host/scripts/apply-update.sh >/tmp/exm-update.log 2>&1")
            .process_group(0)
            .spawn();
        match spawned {
            Ok(_) => (StatusCode::ACCEPTED, Json(json!({ "started": true }))).into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("任务启动失败:{e}") })),
            )
                .into_response(),
        }
    }
    #[cfg(not(unix))]
    {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "一键更新仅支持 Linux 部署(依赖 /host 挂载与 sh 脚本)" })),
        )
            .into_response()
    }
}

/// 一键更新状态(GET /api/version/apply/status):running + 日志尾部
pub(crate) async fn version_apply_status() -> impl IntoResponse {
    let running = update_job_running();
    let log = std::fs::read_to_string("/tmp/exm-update.log").unwrap_or_default();
    let tail: String = {
        let chars: Vec<char> = log.chars().collect();
        let start = chars.len().saturating_sub(1500);
        chars[start..].iter().collect()
    };
    Json(json!({ "running": running, "log": tail }))
}
