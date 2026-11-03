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

/// 更新检查(GET /api/version/check):按部署形态分双模式。
/// - git 模式(/host/.git 存在):分支感知——本地在什么分支就对比该分支的上游,互不干扰;
/// - image 模式(/host 存在但无 .git,如 deploy/ 模板的镜像部署):git ls-remote 匿名查上游
///   分支 SHA,与运行镜像的 EXM_GIT_HASH 前缀比对(无需源码与认证);
/// - none(/host 未挂载):无法检查,明示原因。
pub(crate) async fn version_check() -> impl IntoResponse {
    let mut out = build_version();
    let mode = deploy_mode();
    out["deployMode"] = serde_json::json!(mode);
    match mode {
        DeployMode::Git => { /* 落到下方既有 git 比对流程 */ }
        DeployMode::Image => return Json(version_check_image(out).await),
        DeployMode::None => {
            out["note"] = serde_json::json!(
                "未挂载部署目录(/host),无法检查更新;镜像部署请挂载 deploy/ 目录,源码部署请挂载仓库根"
            );
            out["updateAvailable"] = serde_json::Value::Null;
            return Json(out);
        }
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
        tokio::process::Command::new("sh").arg("-c").arg(script).output(),
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

/// 镜像模式的更新检查:git ls-remote 匿名查上游分支(无需源码与凭据),
/// 与运行镜像的 EXM_GIT_HASH 前缀比对。上游/分支可用 EXM_UPDATE_REMOTE /
/// EXM_UPDATE_BRANCH 覆盖(缺省公开仓库 main)。
async fn version_check_image(mut out: serde_json::Value) -> serde_json::Value {
    let remote = std::env::var("EXM_UPDATE_REMOTE")
        .unwrap_or_else(|_| "https://github.com/ExMachina-Project/ExMachina.git".to_string());
    let branch = std::env::var("EXM_UPDATE_BRANCH").unwrap_or_else(|_| "main".to_string());
    let script = format!(
        "git ls-remote --heads {remote} refs/heads/{branch} 2>/dev/null | awk '{{print $1}}'"
    );
    let exec = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::process::Command::new("sh").arg("-c").arg(script).output(),
    )
    .await;
    let up_sha = match exec {
        Ok(Ok(o)) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Ok(Ok(_)) => {
            out["note"] = serde_json::json!(format!(
                "无法访问上游 {remote}(网络不可达或仓库不可见);可稍后重试"
            ));
            out["updateAvailable"] = serde_json::Value::Null;
            return out;
        }
        Ok(Err(e)) => {
            out["note"] = serde_json::json!(format!("ls-remote 执行失败:{e}"));
            out["updateAvailable"] = serde_json::Value::Null;
            return out;
        }
        Err(_) => {
            out["note"] =
                serde_json::json!("检查超时(上游不可达);可稍后重试,或检查 EXM_UPDATE_REMOTE 配置");
            out["updateAvailable"] = serde_json::Value::Null;
            return out;
        }
    };
    if up_sha.is_empty() {
        out["note"] = serde_json::json!(format!(
            "上游分支 {branch} 不存在(检查 EXM_UPDATE_BRANCH 配置)"
        ));
        out["updateAvailable"] = serde_json::Value::Null;
        return out;
    }
    let short = |sha: &str| sha.chars().take(7).collect::<String>();
    let local_hash = out["gitHash"].as_str().unwrap_or("").to_string();
    // 前缀比对:EXM_GIT_HASH 是短哈希(7 位),ls-remote 返回全量 SHA
    let up_short = short(&up_sha);
    let update_available = if local_hash.len() >= up_short.len() {
        local_hash != up_short
    } else {
        !up_sha.starts_with(&local_hash)
    };
    let owner_repo = remote
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .trim_start_matches("https://github.com/")
        .trim_start_matches("git@github.com:")
        .to_string();
    let commit_url = if owner_repo.contains('/') {
        format!("https://github.com/{owner_repo}/commit/{up_sha}")
    } else {
        String::new()
    };
    out["branch"] = serde_json::json!(branch);
    out["upstream"] = serde_json::json!({
        "ref": branch,
        "sha": up_sha,
        "short": up_short,
        "url": commit_url,
    });
    out["updateAvailable"] = serde_json::json!(update_available);
    out
}

/// 用 File::try_clone 复制日志文件(stderr 与 stdout 同一落点,互不抢所有权)
fn dup_log(f: &std::fs::File) -> Option<std::fs::File> {
    f.try_clone().ok()
}

/// 一键更新任务是否在运行(状态机判定;pgrep 只是窗口期兜底)
pub(crate) fn update_job_running() -> bool {
    update_state().running
}

/// 一键更新(POST /api/version/apply):按部署模式后台执行 scripts/apply-update.sh
/// (image:辅助容器 compose pull && up -d;git 兼容:pull → build → up -d)。
/// 返回 202,进度/阶段走 /api/version/apply/status(状态机)。
/// 镜像源解析(EXM_IMAGE > 部署 compose 的 image: 字段 > None)。
/// git 模式不需要镜像源(本地 build),故仅在 image 模式下强校验。
fn resolve_image_ref() -> Option<String> {
    if let Ok(img) = std::env::var("EXM_IMAGE") {
        let img = img.trim().to_string();
        if !img.is_empty() {
            return Some(img);
        }
    }
    // compose image: 字段(docker compose config 需要 sock;无 sock 的模板部署靠 EXM_IMAGE)
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg("cd /host 2>/dev/null && docker compose config --format json 2>/dev/null")
        .output()
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    v.get("services")?.get("exmachina")?.get("image")?.as_str().map(String::from)
}

pub(crate) async fn version_apply() -> impl IntoResponse {
    let mode = deploy_mode();
    // 前置:至少要有一种可执行的更新路径
    let image_ref = resolve_image_ref();
    match mode {
        DeployMode::None => {
            return (StatusCode::BAD_REQUEST, Json(json!({
                "error": "一键更新需要挂载部署目录(/host)与 /var/run/docker.sock;镜像部署请挂载 deploy/ 目录并开启 sock 段,源码部署请挂载仓库根"
            }))).into_response();
        }
        DeployMode::Image if image_ref.is_none() => {
            return (StatusCode::BAD_REQUEST, Json(json!({
                "error": "镜像部署更新需要镜像源:设置环境变量 EXM_IMAGE(如 ghcr.io/exmachina-project/exmachina:latest),或 compose 使用可拉取的 image: 字段"
            }))).into_response();
        }
        _ => {}
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
        // 脚本路径:/host 有则用其版本(随代码更新),否则用镜像内副本(精简部署目录)
        let host_script = "/host/scripts/apply-update.sh";
        let script_path = if std::path::Path::new(host_script).is_file() {
            host_script
        } else {
            "/app/scripts/apply-update.sh"
        };
        // 先落 state(spawn 前写,消灭「已 spawn 未写 state」窗口),脚本内会覆写 pid
        let _ = write_update_state(UpdatePhase::Pulling);
        // 独立进程组:容器重启(SIGTERM)不会中断 pull/up 流程
        let log = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open("/tmp/exm-update.log")
            .ok();
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c")
            .arg(format!("exec setsid sh {script_path}"))
            .process_group(0);
        if let Some(f) = log {
            let err = dup_log(&f);
            cmd.stdout(std::process::Stdio::from(f));
            if let Some(e) = err {
                cmd.stderr(std::process::Stdio::from(e));
            }
        }
        match cmd.spawn() {
            Ok(child) => {
                // 记录真实 pid(独立进程组组长);phase 用枚举序列化(小写,与读取侧一致)
                let _ = std::fs::write(
                    UPDATE_STATE_FILE,
                    serde_json::json!({
                        "pid": child.id(),
                        "startedAt": std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0),
                        "phase": UpdatePhase::Pulling,
                        "exitCode": null,
                    })
                    .to_string(),
                );
                (StatusCode::ACCEPTED, Json(json!({ "started": true }))).into_response()
            }
            Err(e) => {
                let _ = std::fs::remove_file(UPDATE_STATE_FILE);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("任务启动失败:{e}") })),
                )
                    .into_response()
            }
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

/// 写更新状态文件(spawn 前网关写入初始值;脚本运行期只推进 phase——
/// 读-改-写保留 pid/startedAt/exitCode,不整文件覆写;phase 用枚举序列化小写)
pub(crate) fn write_update_state(phase: UpdatePhase) -> std::io::Result<()> {
    let raw = std::fs::read_to_string(UPDATE_STATE_FILE).unwrap_or_default();
    let mut f: UpdateStateFile = serde_json::from_str(&raw).unwrap_or(UpdateStateFile {
        pid: std::process::id(),
        started_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        exit_code: None,
        phase: None,
    });
    f.phase = Some(phase);
    std::fs::write(UPDATE_STATE_FILE, serde_json::to_string(&f)?)
}

/// 一键更新状态(GET /api/version/apply/status):状态机 + 日志尾部。
/// running 判定唯一事实源是 state 文件(pid 存活 + 未超时),pgrep 仅在脚本
/// 已启动但尚未写 state 的窗口期兜底(模式串用字符类避免 pgrep 匹配到自身命令行)。
pub(crate) async fn version_apply_status() -> impl IntoResponse {
    let st = update_state();
    let log = std::fs::read_to_string("/tmp/exm-update.log").unwrap_or_default();
    let tail: String = {
        let chars: Vec<char> = log.chars().collect();
        let start = chars.len().saturating_sub(1500);
        chars[start..].iter().collect()
    };
    Json(json!({
        "running": st.running,
        "phase": st.phase,               // 枚举 serde 序列化(小写),与 check 的 deployMode 一致
        "exitCode": st.exit_code,
        "log": tail,
        "deployMode": deploy_mode(),     // 同上,弃 Debug 大写格式
    }))
}

// ---------------------------------------------------------------- 部署模式与更新状态机

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DeployMode {
    /// 源码仓直挂(/host/.git 存在):更新 = git pull + compose build
    Git,
    /// 镜像部署(/host 为部署目录但无 .git):更新 = compose pull
    Image,
    /// /host 未挂载:无法检查/更新
    None,
}

/// 部署模式探测:git 仓直挂 > 镜像部署目录 > 未挂载
pub(crate) fn deploy_mode() -> DeployMode {
    let host = std::path::Path::new("/host");
    if host.join(".git").exists() {
        DeployMode::Git
    } else if host.is_dir() {
        DeployMode::Image
    } else {
        DeployMode::None
    }
}

const UPDATE_STATE_FILE: &str = "/tmp/exm-update.state";
/// 更新任务硬超时:超过即视为僵死(pull 卡网络/build 卡层缓存),可重新发起
const UPDATE_TIMEOUT_SECS: u64 = 30 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum UpdatePhase {
    Idle,
    Pulling,
    Recreating,
    Done,
    Failed,
}

/// 更新状态文件结构(状态机唯一事实源;spawn 前由网关写入,脚本运行期覆写 phase,
/// 收尾写 exitCode;读取侧统一按此结构反序列化——此前元组解析 JSON 对象恒 None)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateStateFile {
    pub pid: u32,
    pub started_at: u64,
    /// None = 运行中;Some(0) = 成功;Some(n) = 失败
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub phase: Option<UpdatePhase>,
}

/// 更新任务状态(判定结果)
#[derive(Debug, Clone)]
pub(crate) struct UpdateState {
    pub(crate) running: bool,
    pub(crate) phase: UpdatePhase,
    pub(crate) exit_code: Option<i32>,
}

/// 纯函数核心:给定状态文件内容 + PID 存活判定 + 是否超时,给出运行判定。
/// 容器重启后旧 PID 命名空间失效 → kill -0 失败 → 判定结束(防永久 running 死锁)。
fn judge_update_state(
    parsed: Option<UpdateStateFile>,
    pid_alive: impl Fn(u32) -> bool,
    now_unix: u64,
    pgrep_alive: bool,
) -> UpdateState {
    match parsed {
        Some(f) => {
            let timed_out = now_unix.saturating_sub(f.started_at) > UPDATE_TIMEOUT_SECS;
            let script_alive = f.exit_code.is_none() && pid_alive(f.pid) && !timed_out;
            if script_alive {
                // 运行中:phase 以脚本落盘为准(未写时默认 Pulling)
                UpdateState {
                    running: true,
                    phase: f.phase.unwrap_or(UpdatePhase::Pulling),
                    exit_code: None,
                }
            } else if let Some(code) = f.exit_code {
                // 脚本收尾已写 exitCode:0 = Done,非 0 = Failed
                UpdateState {
                    running: false,
                    phase: if code == 0 { UpdatePhase::Done } else { UpdatePhase::Failed },
                    exit_code: Some(code),
                }
            } else {
                // exitCode 未写:脚本被杀/断电/超时,按失败处理
                UpdateState {
                    running: false,
                    phase: UpdatePhase::Failed,
                    exit_code: None,
                }
            }
        }
        None => {
            // 无 state:脚本可能在「已 spawn 但未写 state」窗口,pgrep 兜底(自匹配免疫)
            if pgrep_alive {
                UpdateState { running: true, phase: UpdatePhase::Pulling, exit_code: None }
            } else {
                UpdateState { running: false, phase: UpdatePhase::Idle, exit_code: None }
            }
        }
    }
}

/// 读取并判定更新任务状态(含 phase/exitCode 落盘值)
pub(crate) fn update_state() -> UpdateState {
    let raw = std::fs::read_to_string(UPDATE_STATE_FILE).unwrap_or_default();
    let parsed = serde_json::from_str::<UpdateStateFile>(&raw).ok();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    judge_update_state(parsed, pid_alive, now, pgrep_update_script())
}

fn pid_alive(pid: u32) -> bool {
    // kill -0 探测(避免引 libc 依赖):sh 内建 kill 对不存在 PID 返回非零;
    // 属主外的进程(EPERM)在这里不会出现——state 里的 pid 是本容器进程
    std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("kill -0 {pid} 2>/dev/null"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// pgrep 兜底探测:模式串 '[a]pply-update.sh' 的字符类写法让 pgrep 自身
/// 命令行(含字面模式串)不匹配该正则——修复恒 running 的自匹配 bug。
fn pgrep_update_script() -> bool {
    std::process::Command::new("sh")
        .arg("-c")
        .arg("pgrep -f '[a]pply-update.sh' >/dev/null 2>&1")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------- 单元测试

#[cfg(test)]
mod tests {
    use super::*;

    fn file(pid: u32, started: u64, exit: Option<i32>, phase: Option<UpdatePhase>) -> UpdateStateFile {
        UpdateStateFile { pid, started_at: started, exit_code: exit, phase }
    }

    #[test]
    fn judge_running_script_keeps_phase() {
        // 脚本存活(pid 活 + 未写 exitCode + 未超时):running,phase 取落盘值
        let st = judge_update_state(Some(file(1, 1000, None, Some(UpdatePhase::Recreating))), |_| true, 1100, false);
        assert!(st.running);
        assert_eq!(st.phase, UpdatePhase::Recreating);
        assert_eq!(st.exit_code, None);
    }

    #[test]
    fn judge_running_defaults_to_pulling() {
        let st = judge_update_state(Some(file(1, 1000, None, None)), |_| true, 1100, false);
        assert!(st.running);
        assert_eq!(st.phase, UpdatePhase::Pulling);
    }

    #[test]
    fn judge_done_when_exit_zero() {
        let st = judge_update_state(Some(file(1, 1000, Some(0), Some(UpdatePhase::Recreating))), |_| true, 1100, false);
        assert!(!st.running);
        assert_eq!(st.phase, UpdatePhase::Done);
        assert_eq!(st.exit_code, Some(0));
    }

    #[test]
    fn judge_failed_when_exit_nonzero() {
        let st = judge_update_state(Some(file(1, 1000, Some(3), None)), |_| true, 1100, false);
        assert!(!st.running);
        assert_eq!(st.phase, UpdatePhase::Failed);
        assert_eq!(st.exit_code, Some(3));
    }

    #[test]
    fn judge_dead_pid_without_exit_is_failed() {
        // 容器重启后旧 PID 命名空间失效:pid 死 + exitCode 未写(被杀/断电)→ 失败,不永久 running
        let st = judge_update_state(Some(file(1, 1000, None, None)), |_| false, 1100, false);
        assert!(!st.running);
        assert_eq!(st.phase, UpdatePhase::Failed);
    }

    #[test]
    fn judge_timeout_is_failed() {
        // 超过 30min 硬超时:即使 pid 活着也判失败(僵死任务可重新发起)
        let now = 1000 + UPDATE_TIMEOUT_SECS + 1;
        let st = judge_update_state(Some(file(1, 1000, None, Some(UpdatePhase::Pulling))), |_| true, now, false);
        assert!(!st.running);
        assert_eq!(st.phase, UpdatePhase::Failed);
    }

    #[test]
    fn judge_no_state_pgrep_fallback() {
        // 无 state 但 pgrep 见脚本(spawn 后写 state 前的窗口):兜底 running
        let st = judge_update_state(None, |_| false, 1000, true);
        assert!(st.running);
        // 无 state 也无进程:回到 Idle
        let st = judge_update_state(None, |_| false, 1000, false);
        assert!(!st.running);
        assert_eq!(st.phase, UpdatePhase::Idle);
    }

    #[test]
    fn state_file_serde_roundtrip() {
        // JSON 对象(网关 spawn 段写入)可正确反序列化——修复点:此前元组解析恒 None
        let raw = serde_json::json!({
            "pid": 4242, "startedAt": 1730000000, "phase": "recreating", "exitCode": null
        });
        let f: UpdateStateFile = serde_json::from_str(&raw.to_string()).unwrap();
        assert_eq!(f.pid, 4242);
        assert_eq!(f.started_at, 1730000000);
        assert_eq!(f.phase, Some(UpdatePhase::Recreating));
        assert_eq!(f.exit_code, None);
        // 小写枚举值(serde rename_all)与脚本写入兼容
        let raw2 = r#"{"pid":1,"startedAt":9,"phase":"done","exitCode":0}"#;
        let f2: UpdateStateFile = serde_json::from_str(raw2).unwrap();
        assert_eq!(f2.phase, Some(UpdatePhase::Done));
        assert_eq!(f2.exit_code, Some(0));
        // 序列化输出小写(status 输出与 check 的 deployMode 风格一致)
        let j = serde_json::to_value(file(1, 1, None, Some(UpdatePhase::Pulling))).unwrap();
        assert_eq!(j["phase"], "pulling");
    }

    #[test]
    fn image_sha_prefix_compare() {
        // 短哈希(7位)与全量 SHA 的前缀比对:版本一致 → 无更新
        let local = "abc1234";
        let up_full = "abc1234deadbeef0000000000000000000000000";
        let short = |s: &str| s.chars().take(7).collect::<String>();
        let up_short = short(up_full);
        let same = if local.len() >= up_short.len() {
            local == up_short
        } else {
            up_full.starts_with(local)
        };
        assert!(same);
        let local2 = "fff9999";
        let same2 = if local2.len() >= up_short.len() {
            local2 == up_short
        } else {
            up_full.starts_with(local2)
        };
        assert!(!same2);
    }
}
