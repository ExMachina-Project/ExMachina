//! 插件管理端点：/api/plugins。
//!
//! 列表/详情/启停/卸载 + 健康摘要。builtin 与 virtual 形态不可卸载（DELETE 拒绝）；
//! 启停持久化到 config.plugins.state（清单只读）。

use crate::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/plugins", get(list_plugins))
        .route(
            "/api/plugins/:id",
            get(get_plugin).put(put_enabled).delete(delete_plugin),
        )
        .route("/api/plugins/:id/credentials", get(get_credentials).put(put_credentials))
        .route("/api/plugins/install", post(post_install))
        .route("/api/plugins/:id/update", post(post_update))
        .route("/api/plugins/index", get(get_index))
}

/// 凭据掩码（插件凭据区一律敏感口径——无论清单是否标记）
const CRED_MASK: &str = "********";

/// GET /api/plugins/:id/credentials：凭据区回显（值一律掩码，键名可见）
pub async fn get_credentials(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> axum::response::Response {
    if st.core.plugins().get(&id).is_none() {
        return err_not_found(&id).into_response();
    }
    let cfg = st.core.config();
    let masked: serde_json::Map<String, Value> = cfg
        .plugins
        .credentials
        .get(&id)
        .map(|m| {
            m.iter()
                .map(|(k, v)| {
                    let masked_v = if v.is_empty() { String::new() } else { CRED_MASK.into() };
                    (k.clone(), Value::String(masked_v))
                })
                .collect()
        })
        .unwrap_or_default();
    Json(json!({ "id": id, "credentials": masked, "configured": !masked.is_empty() }))
        .into_response()
}

/// PUT /api/plugins/:id/credentials：批量写入凭据（body: { "credentials": {key: value} }）。
/// 值为掩码串的键跳过（WebUI 回显掩码直传场景不覆盖真值）；空值删除该键。
pub async fn put_credentials(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<Value>,
) -> axum::response::Response {
    if st.core.plugins().get(&id).is_none() {
        return err_not_found(&id).into_response();
    }
    let Some(incoming) = b.get("credentials").and_then(|v| v.as_object()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "body 需为 { \"credentials\": { 键: 值 } }" })),
        )
            .into_response();
    };
    let mut cfg = (*st.core.config()).clone();
    let slot = cfg.plugins.credentials.entry(id.clone()).or_default();
    for (k, v) in incoming {
        let Some(val) = v.as_str() else {
            continue;
        };
        if val == CRED_MASK {
            continue; // 掩码回传：保持原值
        }
        if val.is_empty() {
            slot.remove(k);
        } else {
            slot.insert(k.clone(), val.to_string());
        }
    }
    if slot.is_empty() {
        cfg.plugins.credentials.remove(&id);
    }
    if let Err(e) = st.core.apply_config(cfg) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("保存插件凭据失败：{e}") })),
        )
            .into_response();
    }
    // T2 进程插件：凭据变更即时重下发（握手后桥自动同步，此处仅持久化）
    Json(json!({ "id": id, "saved": true })).into_response()
}

/// 能力摘要（tools/channel/watch/resources 计数与要点）
fn capability_summary(m: &exm_core::config::PluginManifest) -> Value {
    json!({
        "tools": m.tools.len(),
        "channel": m.channel.as_ref().map(|c| json!({
            "kinds": c.kinds,
            "credentials": c.credentials.len(),
        })),
        "watch": m.watch.is_some(),
        "resources": m.resources.len(),
        "process": m.is_process(),
    })
}

/// GET /api/plugins：全量插件列表（状态/形态/能力摘要）
pub async fn list_plugins(State(st): State<AppState>) -> impl IntoResponse {
    let pm = st.core.plugins();
    let list: Vec<Value> = pm
        .list()
        .into_iter()
        .map(|e| {
            json!({
                "id": e.manifest.id,
                "name": if e.manifest.name.is_empty() { e.manifest.id.clone() } else { e.manifest.name.clone() },
                "version": e.manifest.version,
                "description": e.manifest.description,
                "form": e.form,
                "enabled": e.enabled,
                "error": e.error,
                "capabilities": capability_summary(&e.manifest),
                // 进程形态握手发现的工具名（列表直接可见；详情端点另有完整快照）
                "toolsSnapshot": e.tools_snapshot,
            })
        })
        .collect();
    Json(json!({ "plugins": list }))
}

/// GET /api/plugins/:id：详情（清单 + 健康）
pub async fn get_plugin(State(st): State<AppState>, Path(id): Path<String>) -> axum::response::Response {
    let pm = st.core.plugins();
    let Some(e) = pm.get(&id) else {
        return err_not_found(&id).into_response();
    };
    let cfg = st.core.config();
    Json(json!({
        "id": e.manifest.id,
        "name": e.manifest.name,
        "version": e.manifest.version,
        "description": e.manifest.description,
        "form": e.form,
        "enabled": e.enabled,
        "error": e.error,
        "manifest": e.manifest,
        "capabilities": capability_summary(&e.manifest),
        "health": if e.enabled && e.error.is_none() { "ok" } else { "degraded" },
        "installation": cfg.plugins.installations.get(&id),
        "credentialsConfigured": cfg.plugins.credentials.contains_key(&id),
        // T2 握手后 tools/list 快照（装载期发现面；空 = 未拉起或非进程形态）
        "toolsSnapshot": e.tools_snapshot,
        // 会话工具面注册诊断：该插件工具是否已并入 ToolGateway（下发面可见性）
        "toolSurfaceRegistered": tool_surface_registered(&st, &id, &e.tools_snapshot),
    }))
    .into_response()
}

/// 该插件工具是否已注册进会话工具面（详情页健康诊断：快照有但未注册 = 装配断链）
fn tool_surface_registered(st: &AppState, id: &str, snapshot: &[String]) -> bool {
    if snapshot.is_empty() {
        return false;
    }
    let specs = st.core.orchestrator().tools.custom_specs_for("orchestrator");
    snapshot
        .iter()
        .any(|n| specs.iter().any(|s| s.name == format!("{id}__{n}")))
}

/// PUT /api/plugins/:id：启停（body: { "enabled": bool }；持久化 config.plugins.state）
pub async fn put_enabled(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<Value>,
) -> axum::response::Response {
    let Some(enabled) = b.get("enabled").and_then(|v| v.as_bool()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "body 需为 { \"enabled\": bool }" })),
        )
            .into_response();
    };
    let pm = st.core.plugins();
    if pm.get(&id).is_none() {
        return err_not_found(&id).into_response();
    }
    if !pm.set_enabled(&id, enabled) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "内置虚拟插件（CustomTool/skills 收编）不可禁用" })),
        )
            .into_response();
    }
    // 持久化：写回 config.plugins.state
    let mut cfg = (*st.core.config()).clone();
    cfg.plugins.state.insert(id.clone(), enabled);
    if let Err(e) = st.core.apply_config(cfg) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("保存插件状态失败：{e}") })),
        )
            .into_response();
    }
    Json(json!({ "id": id, "enabled": enabled })).into_response()
}

/// DELETE /api/plugins/:id：卸载（builtin/virtual 拒绝；目录插件删 plugins/<id>/）
pub async fn delete_plugin(State(st): State<AppState>, Path(id): Path<String>) -> axum::response::Response {
    let pm = st.core.plugins();
    let Some(e) = pm.get(&id) else {
        return err_not_found(&id).into_response();
    };
    match e.form.as_str() {
        "builtin" | "virtual" => {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({
                    "error": format!("{} 插件随宿主发布，不可卸载（可禁用）", e.form),
                })),
            )
                .into_response()
        }
        _ => {}
    }
    let Some(dir) = e.dir.clone() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "插件目录未知，无法卸载" })),
        )
            .into_response();
    };
    // 安全闸门：目录必须在 plugins/ 根下（防路径逃逸）
    let plugins_root = st.core.config().workspace_root.join("plugins");
    if !dir.starts_with(&plugins_root) || dir == plugins_root {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "插件目录越界，拒绝卸载" })),
        )
            .into_response();
    }
    pm.set_enabled(&id, false);
    let removed = match std::fs::remove_dir_all(&dir) {
        Ok(()) => true,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("删除插件目录失败：{err}") })),
            )
                .into_response()
        }
    };
    // 状态面清理：摘除 entry + 移除启停覆盖
    pm.remove_entry(&id);
    let mut cfg = (*st.core.config()).clone();
    cfg.plugins.state.remove(&id);
    if let Err(err) = st.core.apply_config(cfg) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("清理插件状态失败：{err}"), "removed": removed })),
        )
            .into_response();
    }
    Json(json!({ "id": id, "removed": true })).into_response()
}

/// POST /api/plugins/install：安装插件（body: { "source": "<索引id或URL>", "overwrite"?: bool }）。
/// 索引 id → 查官方/自定义索引；URL 直判（git / tar.gz）。
pub async fn post_install(
    State(st): State<AppState>,
    Json(b): Json<Value>,
) -> axum::response::Response {
    let Some(source) = b.get("source").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "body 需为 { \"source\": \"<索引id或URL>\", \"overwrite\"?: bool }" })),
        )
            .into_response();
    };
    let overwrite = b.get("overwrite").and_then(|v| v.as_bool()).unwrap_or(false);
    // 来源解析：URL/本地包直判；索引 id 查索引（与 CLI resolve_source 同口径）
    let src = if source.contains("://")
        || source.ends_with(".git")
        || source.starts_with("git@")
        || std::path::Path::new(source).is_file()
    {
        exm_core::plugin::install::parse_source_url(source)
    } else {
        let cfg = st.core.config();
        match exm_core::plugin::install::fetch_index(&exm_core::plugin::install::effective_index_url(
            &cfg.plugins.index_url,
        ))
        .await
        {
            Ok(entries) => {
                let host = exm_core::plugin::host_version_pub();
                match entries.iter().find(|e| e.id == source) {
                    Some(hit) => {
                        if !hit.exm_min.trim().is_empty()
                            && !exm_core::plugin::version_ok(&hit.exm_min, &host)
                        {
                            // 412 而非 422：422 是 axum serde 拒绝的固定状态码，
                            // 会被请求体改写中间件截获重写成「请求体字段有误」
                            return (
                                StatusCode::PRECONDITION_FAILED,
                                Json(json!({ "error": format!("插件 {} 需要宿主 ≥ {}，当前 {}", hit.id, hit.exm_min, host) })),
                            )
                                .into_response();
                        }
                        hit.to_install_source()
                    }
                    None => {
                        return (
                            StatusCode::NOT_FOUND,
                            Json(json!({ "error": format!("索引中未找到插件 {source}（或直传 tar.gz / git URL）") })),
                        )
                            .into_response()
                    }
                }
            }
            Err(e) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({ "error": format!("插件索引拉取失败：{e}") })),
                )
                    .into_response()
            }
        }
    };
    // process 插件：安装后立即拉起（与网关启动面同口径）
    let out = match st.core.plugin_install(&src, overwrite).await {
        Ok(o) => o,
        Err(e) => {
            let msg = e.to_string();
            return (install_error_status(&msg), Json(json!({ "error": msg }))).into_response();
        }
    };
    spawn_process_if_any(&st, &out.id).await;
    Json(json!({
        "id": out.id, "version": out.version, "updated": out.updated, "installed": true,
    }))
    .into_response()
}

/// 安装/更新错误的 HTTP 状态分类（安装层错误是 String 口径——CLI 与端点共用，
/// 按消息特征映射：冲突 409 / 版本不满足 422 / 来源拉取失败 502 / 其余 400）
fn install_error_status(msg: &str) -> StatusCode {
    if msg.contains("已安装") {
        StatusCode::CONFLICT
    } else if msg.contains("无安装来源记录") {
        StatusCode::NOT_FOUND
    } else if msg.contains("需要宿主") {
        // 412 而非 422：422 会被 axum serde 拒绝改写中间件重写
        StatusCode::PRECONDITION_FAILED
    } else if msg.contains("拉取失败") || msg.contains("克隆失败") || msg.contains("下载失败") {
        StatusCode::BAD_GATEWAY
    } else {
        StatusCode::BAD_REQUEST
    }
}

/// POST /api/plugins/:id/update：按 installation 来源记录更新（已是最新返回 updated=false）
pub async fn post_update(State(st): State<AppState>, Path(id): Path<String>) -> axum::response::Response {
    match st.core.plugin_update(&id).await {
        Ok((updated, msg)) => {
            if updated {
                spawn_process_if_any(&st, &id).await;
            }
            Json(json!({ "id": id, "updated": updated, "message": msg })).into_response()
        }
        Err(e) => {
            let msg = e.to_string();
            (install_error_status(&msg), Json(json!({ "error": msg }))).into_response()
        }
    }
}

/// GET /api/plugins/index：索引目录（已装/可更新/不兼容标记）
pub async fn get_index(State(st): State<AppState>) -> axum::response::Response {
    let cfg = st.core.config();
    let host = exm_core::plugin::host_version_pub();
    let entries = match exm_core::plugin::install::fetch_index(&exm_core::plugin::install::effective_index_url(
        &cfg.plugins.index_url,
    ))
    .await
    {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": format!("插件索引拉取失败：{e}") })),
            )
                .into_response()
        }
    };
    let pm = st.core.plugins();
    let installed = |id: &str| -> Option<(String, bool)> {
        pm.get(id).map(|e| (e.manifest.version.clone(), e.enabled))
    };
    let catalog: Vec<Value> = exm_core::plugin::install::compare_index(&entries, &installed, &host)
        .into_iter()
        .map(|(e, s)| {
            let mut v = json!({
                "id": e.id, "name": e.name, "version": e.version, "description": e.description,
            });
            match s {
                exm_core::plugin::install::IndexStatus::Available => {
                    v["status"] = json!("available");
                }
                exm_core::plugin::install::IndexStatus::Installed(local) => {
                    v["status"] = json!("installed");
                    v["localVersion"] = json!(local);
                }
                exm_core::plugin::install::IndexStatus::Updatable(local, _) => {
                    v["status"] = json!("updatable");
                    v["localVersion"] = json!(local);
                }
                exm_core::plugin::install::IndexStatus::Incompatible(need) => {
                    v["status"] = json!("incompatible");
                    v["exmMin"] = json!(need);
                }
            }
            v
        })
        .collect();
    Json(json!({ "indexUrl": exm_core::plugin::install::effective_index_url(&cfg.plugins.index_url), "entries": catalog })).into_response()
}

/// process 插件安装/更新后拉起（失败记 entry.error，不阻塞返回）
async fn spawn_process_if_any(st: &AppState, id: &str) {
    let pm = st.core.plugins();
    let Some(e) = pm.get(id) else { return };
    if e.form != "process" || !e.enabled {
        return;
    }
    let Some(dir) = e.dir.clone() else { return };
    // 先摘旧 provider（更新场景），再重新拉起
    exm_core::plugin::bridge::unregister_process(id);
    let creds = st.core.config().plugins.credentials.get(id).cloned();
    match exm_core::plugin::bridge::spawn_process_plugin(&pm, &e.manifest, &dir, creds).await {
        Ok(_) => {
            pm.record_error(id, None);
            // T2 工具面回接：握手发现的工具并入会话工具网关（specs + 执行）
            attach_process_tools(&st.core, &pm, id);
        }
        Err(err) => pm.record_error(id, Some(format!("拉起失败：{err}"))),
    }
}

/// 进程插件工具面回接：provider 的 tool_provider 挂进 ToolGateway
/// （specs 随 custom_specs_for 下发；执行转发 supervisor → 子进程）。
/// 幂等：重复注册覆盖（重启/更新场景新 supervisor 顶旧）。
pub(crate) fn attach_process_tools(
    core: &std::sync::Arc<exm_core::Core>,
    pm: &std::sync::Arc<exm_core::plugin::PluginManager>,
    id: &str,
) {
    let Some(p) = pm.provider_of(id) else {
        eprintln!("[plugins] {id} 工具面回接失败：provider 不存在");
        return;
    };
    match p.tool_provider() {
        Some(tp) => {
            core.orchestrator().tools.register_plugin_runtime_tools(id, tp);
            eprintln!("[plugins] {id} 工具面已并入会话工具网关");
        }
        None => eprintln!("[plugins] {id} 工具面回接失败：provider 无 tool_provider"),
    }
}

fn err_not_found(id: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": format!("插件不存在：{id}") })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn app() -> Router {
        let dir = std::env::temp_dir().join(format!("exm-plugin-admin-{}", exm_core::types::new_id()));
        std::fs::create_dir_all(dir.join("plugins/hello")).unwrap();
        std::fs::write(
            dir.join("plugins/hello/plugin.json"),
            r#"{ "id": "hello", "name": "示例", "tools": [ { "name": "ping", "description": "回声", "kind": "http", "target": "https://example.com", "method": "GET" } ] }"#,
        )
        .unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        let core = Arc::new(exm_core::Core::with_config(cfg).unwrap());
        crate::plugins::spawn_all(core.clone());
        routes().with_state(AppState { core })
    }

    #[tokio::test]
    async fn 端点_列表与详情往返() {
        let app = app();
        let r = app
            .clone()
            .oneshot(Request::builder().uri("/api/plugins").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        let ids: Vec<&str> = v["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&"video"), "列表应含 builtin video：{ids:?}");
        assert!(ids.contains(&"hello"), "列表应含目录插件 hello：{ids:?}");

        let r = app
            .oneshot(Request::builder().uri("/api/plugins/video").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["form"], "builtin");
        assert_eq!(v["health"], "ok");
        assert!(v["manifest"]["channel"]["kinds"].to_string().contains("bilibili"));
    }

    #[tokio::test]
    async fn 端点_启停往返与持久化() {
        let app = app();
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/plugins/hello")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{ "enabled": false }"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        // 再查详情确认 disabled
        let r = app
            .oneshot(Request::builder().uri("/api/plugins/hello").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["enabled"], false);
    }

    #[tokio::test]
    async fn 端点_builtin与virtual不可卸载() {
        let app = app();
        for id in ["video"] {
            let r = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("DELETE")
                        .uri(format!("/api/plugins/{id}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(r.status(), 403, "builtin {id} 卸载应 403");
        }
        // virtual（custom-tools）在无 CustomTool 时不登记——构造有内容的 case 走 403
        let r = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/plugins/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 404);
    }

    /// 凭据区往返（plugin-ecosystem 1.2）：写入 → 回显掩码 → 掩码回传不覆盖 → 空值删除
    #[tokio::test]
    async fn 端点_插件凭据掩码往返() {
        let app = app();
        // 写入两个键
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/plugins/hello/credentials")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{ "credentials": { "api_token": "sk-secret-123", "other": "v" } }"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "写入应成功");
        // 回显：值一律掩码
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/plugins/hello/credentials")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["credentials"]["api_token"], CRED_MASK, "值必须掩码");
        assert_eq!(v["credentials"]["other"], CRED_MASK);
        assert_eq!(v["configured"], true);
        assert!(!body.is_empty() && !String::from_utf8_lossy(&body).contains("sk-secret-123"), "明文不得出现");
        // 掩码回传 + 空值删除：api_token 保持、other 删除
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/plugins/hello/credentials")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{ "credentials": { "api_token": "********", "other": "" } }"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/plugins/hello/credentials")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert!(v["credentials"].get("other").is_none(), "空值应删除键");
        assert_eq!(v["credentials"]["api_token"], CRED_MASK, "掩码回传不覆盖真值");
        // 详情含凭据配置标记
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/plugins/hello")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["credentialsConfigured"], true);
    }

    /// 凭据持久化往返：apply_config 落盘后重载仍在（掩码口径不变）
    #[tokio::test]
    async fn 端点_插件凭据持久化() {
        let dir = std::env::temp_dir().join(format!("exm-plugin-cred-{}", exm_core::types::new_id()));
        std::fs::create_dir_all(dir.join("plugins/hello")).unwrap();
        std::fs::write(
            dir.join("plugins/hello/plugin.json"),
            r#"{ "id": "hello", "tools": [ { "name": "ping", "kind": "http", "target": "https://example.com", "method": "GET" } ] }"#,
        )
        .unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        {
            let core = exm_core::Core::with_config(cfg.clone()).unwrap();
            let mut c2 = (*core.config()).clone();
            c2.plugins.credentials.insert(
                "hello".into(),
                [("k".to_string(), "v1".to_string())].into_iter().collect(),
            );
            core.apply_config(c2).unwrap();
        }
        // 重载：凭据区仍在（持久化生效）
        let core2 = exm_core::Core::with_config(exm_core::config::ExmConfig::load(&dir)).unwrap();
        assert_eq!(
            core2.config().plugins.credentials.get("hello").and_then(|m| m.get("k").map(String::as_str)),
            Some("v1"),
            "凭据应持久化"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// REST 安装流（plugin-ecosystem 3.3）：本地 tar.gz → 安装→详情→冲突→覆盖→更新→卸载
    #[tokio::test]
    async fn 端点_安装更新卸载往返() {
        // 准备 v1/v2 两个包 + 本地索引文件
        let dir = std::env::temp_dir().join(format!("exm-plugin-flow-{}", exm_core::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mk_pkg = |tag: &str, ver: &str| -> std::path::PathBuf {
            let src = dir.join(format!("{tag}/src/pkg"));
            std::fs::create_dir_all(&src).unwrap();
            std::fs::write(
                src.join("plugin.json"),
                format!(r#"{{ "id": "inst", "version": "{ver}", "tools": [ {{ "name": "ping", "kind": "http", "target": "https://example.com", "method": "GET" }} ] }}"#),
            )
            .unwrap();
            let tgz = dir.join(format!("{tag}.tgz"));
            let f = std::fs::File::create(&tgz).unwrap();
            let enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
            let mut tar = tar::Builder::new(enc);
            tar.append_dir_all("pkg", &src).unwrap();
            tar.into_inner().unwrap().finish().unwrap();
            tgz
        };
        let v1 = mk_pkg("v1", "0.1.0");
        let v2 = mk_pkg("v2", "0.2.0");
        // 本地索引（fetch_index 支持文件路径）
        let index = dir.join("index.json");
        std::fs::write(
            &index,
            format!(
                r#"[ {{ "id": "inst", "version": "0.2.0", "source": {{ "kind": "targz", "url": "{}" }} }} ]"#,
                v2.to_string_lossy().replace('\\', "/")
            ),
        )
        .unwrap();
        // app（indexUrl 指本地）
        let ws = dir.join("ws");
        std::fs::create_dir_all(ws.join("plugins")).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&ws);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        cfg.plugins.index_url = index.to_string_lossy().to_string();
        let core = Arc::new(exm_core::Core::with_config(cfg).unwrap());
        let app = routes().with_state(AppState { core: core.clone() });

        // ① 安装（本地包 URL）
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/plugins/install")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "source": v1.to_string_lossy() }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "安装应成功");
        let body = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["id"], "inst");
        assert_eq!(v["version"], "0.1.0");
        // 详情：installation 记录在
        let r = app
            .clone()
            .oneshot(Request::builder().uri("/api/plugins/inst").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["installation"]["kind"], "targz");

        // ② 冲突：同 id 不带 overwrite → 409
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/plugins/install")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "source": v1.to_string_lossy() }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 409, "重复安装应 409");

        // ③ 索引端点：inst 显示 updatable（索引 v0.2.0 > 本地 v0.1.0）
        let r = app
            .clone()
            .oneshot(Request::builder().uri("/api/plugins/index").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        let entry = v["entries"].as_array().unwrap().iter().find(|e| e["id"] == "inst").unwrap();
        assert_eq!(entry["status"], "updatable");

        // ④ 更新（索引来源安装记录 → 改指 v2 后 update 走索引）
        {
            let mut c = (*core.config()).clone();
            c.plugins.installations.insert(
                "inst".into(),
                exm_core::config::PluginInstallation {
                    kind: "targz".into(),
                    url: index.to_string_lossy().to_string().replace("index.json", "v2.tgz"),
                    ..Default::default()
                },
            );
            core.apply_config(c).unwrap();
        }
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/plugins/inst/update")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["updated"], true, "应更新：{v}");
        // 更新后版本
        let r = app
            .clone()
            .oneshot(Request::builder().uri("/api/plugins/inst").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["version"], "0.2.0");

        // ⑤ 卸载
        let r = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/plugins/inst")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        assert!(!ws.join("plugins/inst").exists(), "目录应删除");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
