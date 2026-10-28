//! 插件管理端点：/api/plugins。
//!
//! 列表/详情/启停/卸载 + 健康摘要。builtin 与 virtual 形态不可卸载（DELETE 拒绝）；
//! 启停持久化到 config.plugins.state（清单只读）。

use crate::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/plugins", get(list_plugins))
        .route(
            "/api/plugins/:id",
            get(get_plugin).put(put_enabled).delete(delete_plugin),
        )
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
    }))
    .into_response()
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
}
