//! gateway/selfcheck —— 开箱自检端点（onboarding-selfcheck 规格）。
//!
//! `GET /api/selfcheck` 与 `exm doctor` 同一判定口径（规格 D4：同一内核、避免两套口径打架），
//! 但输出机器可读结构（WebUI 首页引导卡数据源）：四类体检（LLM / 通道 / 存储 / 版本）
//! 各给 ok / warn / error 三级结论与每项可读说明。
//!
//! 约束（规格「首次运行体检」）：
//! · 只读不写——不产生任何配置或状态副作用；
//! · 探测只做可达性（不发真实推理请求），固定超时上限（LLM 探测 8s）；
//! · 缺密钥等 error 项必须附可操作指引文案。

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

use crate::AppState;

/// 单项体检结果
fn item(level: &str, name: &str, detail: String, hint: Option<&str>) -> serde_json::Value {
    json!({ "level": level, "name": name, "detail": detail, "hint": hint })
}

/// 开箱自检（GET /api/selfcheck）：只读体检，四级输出。
pub(crate) async fn selfcheck(State(st): State<AppState>) -> impl IntoResponse {
    let cfg = st.core.config();
    let mut items: Vec<serde_json::Value> = Vec::new();

    // ---- LLM：密钥存在性（error 时给指引）+ 端点可达性（可达性探测，不发推理请求）----
    let key_set = !cfg.llm.api_key.trim().is_empty()
        || !cfg.llm.api_keys.is_empty()
        || cfg.llm_profiles
            .iter()
            .any(|p| !p.api_key.trim().is_empty() || !p.api_keys.is_empty());
    items.push(if st.core.is_mock() {
        item("warn", "LLM 配置", "测试替身通道（EXM_LLM_MOCK=1）".into(), None)
    } else if !key_set {
        item(
            "error",
            "LLM 配置",
            "未配置任何模型密钥".into(),
            Some("到 WebUI「模型」页添加提供商与密钥，或设置环境变量 EXM_LLM_API_KEY"),
        )
    } else {
        // 端点可达性（只探可达，不发推理请求；8s 上限由 client 超时 + 外层 timeout 保证）
        // 生效档案口径：active_profile 命中者，未命中回落主 llm 配置
        let active = cfg.llm_profiles.iter().find(|p| p.id == cfg.active_profile);
        let base = active
            .map(|p| p.base_url.clone())
            .filter(|u| !u.trim().is_empty())
            .or_else(|| {
                let u = cfg.llm.base_url.clone();
                (!u.trim().is_empty()).then_some(u)
            })
            .unwrap_or_default();
        let reachable = if base.trim().is_empty() {
            false
        } else {
            let url = base.trim_end_matches('/').to_string() + "/models";
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                async move {
                    let client = reqwest::Client::builder()
                        .timeout(std::time::Duration::from_secs(8))
                        .build()
                        .ok()?;
                    let resp = client.get(&url).send().await.ok()?;
                    Some(resp.status().as_u16() < 500)
                },
            )
            .await
            .ok()
            .flatten()
            .unwrap_or(false)
        };
        if reachable {
            item("ok", "LLM 配置", format!("密钥已配置，端点可达（{base}）"), None)
        } else {
            item(
                "warn",
                "LLM 配置",
                format!("密钥已配置，端点探测未确认（{base}）"),
                Some("端点不可达或无 /models 探针——若使用内网中转可忽略，否则检查 base URL"),
            )
        }
    });

    // ---- 通道：已启用通道数 + 连接态（复用平台状态表）----
    let channels = crate::platform::load_channels(&st.core);
    let enabled: Vec<_> = channels.iter().filter(|c| c.enabled).collect();
    let degraded = enabled
        .iter()
        .filter(|c| {
            crate::platform::status_of(&c.id)
                .map(|s| s.state != "ok")
                .unwrap_or(true)
        })
        .count();
    items.push(if enabled.is_empty() {
        item(
            "warn",
            "通道配置",
            "无已启用通道".into(),
            Some("控制台会话可直接使用；需要外部消息入口时到「通道」页接入（如 QQ / Telegram）"),
        )
    } else if degraded > 0 {
        item(
            "error",
            "通道配置",
            format!("{} 个通道已启用，其中 {} 个连接异常", enabled.len(), degraded),
            Some("到「通道」页查看异常通道的详细状态与修复指引"),
        )
    } else {
        item("ok", "通道配置", format!("{} 个通道已启用且连接正常", enabled.len()), None)
    });

    // ---- 存储：数据目录可写 + 磁盘余量 ----
    let data = &cfg.data_dir;
    let writable = std::fs::write(data.join(".selfcheck-probe"), b"ok")
        .and_then(|_| std::fs::remove_file(data.join(".selfcheck-probe")))
        .is_ok();
    let disk_note = disk_free_note(data);
    items.push(if !writable {
        item(
            "error",
            "存储",
            format!("数据目录不可写：{}", data.display()),
            Some("检查目录权限（容器部署时确认卷挂载正确）"),
        )
    } else {
        match disk_note {
            Some(note) if note.starts_with("ok") => {
                item("ok", "存储", format!("数据目录可写，{note}"), None)
            }
            Some(note) => item(
                "warn",
                "存储",
                format!("数据目录可写，{note}"),
                Some("磁盘余量偏低——清理 .exmachina/tool-output 与 checkpoints 可回收空间"),
            ),
            None => item("ok", "存储", "数据目录可写".into(), None),
        }
    });

    // ---- 版本：当前版本 + 上游可更新性（复用 version.rs 的判定）----
    let version = env!("CARGO_PKG_VERSION");
    let hash = option_env!("EXM_GIT_HASH").unwrap_or("dev");
    items.push(item(
        "ok",
        "版本",
        format!("v{version}（{hash}）"),
        Some("到「设置」页版本面板可检查上游更新"),
    ));

    // 汇总
    let errors = items.iter().filter(|i| i["level"] == "error").count();
    let warns = items.iter().filter(|i| i["level"] == "warn").count();
    let overall = if errors > 0 { "error" } else if warns > 0 { "warn" } else { "ok" };
    (
        StatusCode::OK,
        Json(json!({
            "ok": errors == 0,
            "overall": overall,
            "errors": errors,
            "warns": warns,
            "items": items,
        })),
    )
        .into_response()
}

/// 磁盘余量速览：`ok xxx 可用` / `低 xxx 可用`；不可得返回 None（不猜）
fn disk_free_note(dir: &std::path::Path) -> Option<String> {
    // 不引 statfs 依赖：用 df 输出（镜像内已含 coreutils）
    let out = std::process::Command::new("df")
        .arg("-h")
        .arg(dir)
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let line = stdout.lines().nth(1)?.to_string();
    let avail = line.split_whitespace().nth(3)?.to_string();
    let pct: u32 = line.split_whitespace().nth(4)?.trim_end_matches('%').parse().ok()?;
    Some(if pct >= 85 {
        format!("低：{avail} 可用（{pct}% 已用）")
    } else {
        format!("ok：{avail} 可用（{pct}% 已用）")
    })
}
