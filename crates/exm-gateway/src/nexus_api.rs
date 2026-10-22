//! 智能连结网络管理端点（实验性）——连结体清单 CRUD。
//! 存储于 config.nexus（apply_config 热生效），路由 /api/nexus/links。

use crate::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use exm_core::config::{ExmConfig, LinkTarget};
use serde::Deserialize;
use serde_json::json;

fn links_of(st: &AppState) -> Vec<LinkTarget> {
    st.core.config().nexus.links.clone()
}

/// 连结体清单（GET /api/nexus/links）
pub async fn list_links(State(st): State<AppState>) -> impl IntoResponse {
    Json(json!({ "links": links_of(&st) })).into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkBody {
    pub id: String,
    pub name: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub command: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_kind() -> String {
    "exmachina".into()
}
fn default_enabled() -> bool {
    true
}

fn validate(body: &LinkBody) -> Result<(), String> {
    let id = body.id.trim();
    if id.is_empty() || id.len() > 48 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("id 须为 1-48 位字母/数字/-/_".into());
    }
    if body.name.trim().is_empty() {
        return Err("name 不能为空".into());
    }
    if !matches!(body.kind.as_str(), "exmachina" | "opencode" | "codex" | "claude" | "custom") {
        return Err(format!("未知连结体类型: {}", body.kind));
    }
    if body.kind == "exmachina" && body.endpoint.trim().is_empty() {
        return Err("exmachina 连结体须配置网关地址".into());
    }
    if body.kind == "custom" && body.command.trim().is_empty() {
        return Err("custom 连结体须配置命令行".into());
    }
    Ok(())
}

/// 新增/更新连结体（PUT /api/nexus/links）：直接改配置并热生效
pub async fn save_link(State(st): State<AppState>, Json(b): Json<LinkBody>) -> impl IntoResponse {
    if let Err(e) = validate(&b) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": e }))).into_response();
    }
    let mut cfg: ExmConfig = (*st.core.config()).clone();
    let link = LinkTarget {
        id: b.id.trim().to_string(),
        name: b.name.trim().to_string(),
        kind: b.kind.clone(),
        endpoint: b.endpoint.trim().to_string(),
        api_key: if b.api_key.trim().is_empty() {
            // 覆盖语义：同 id 已有配置则沿用旧密钥
            cfg.nexus.links.iter().find(|l| l.id == b.id).map(|l| l.api_key.clone()).unwrap_or_default()
        } else {
            b.api_key.trim().to_string()
        },
        command: b.command.trim().to_string(),
        enabled: b.enabled,
    };
    if let Some(slot) = cfg.nexus.links.iter_mut().find(|l| l.id == link.id) {
        *slot = link;
    } else {
        cfg.nexus.links.push(link);
    }
    match st.core.apply_config(cfg) {
        Ok(_) => {
            let saved = links_of(&st).into_iter().find(|l| l.id == b.id.trim());
            Json(json!({ "ok": true, "link": saved })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

/// 删除连结体（DELETE /api/nexus/links/:id）：同步清理个体上的死绑定（link 置空）
pub async fn delete_link(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let mut cfg: ExmConfig = (*st.core.config()).clone();
    let before = cfg.nexus.links.len();
    cfg.nexus.links.retain(|l| l.id != id);
    if cfg.nexus.links.len() == before {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "连结体不存在" }))).into_response();
    }
    match st.core.apply_config(cfg) {
        Ok(_) => {
            let pruned = st.core.registry().prune_stale_links(&id);
            if pruned > 0 {
                eprintln!("[nexus] 连结体 {id} 删除：清理 {pruned} 个个体的死绑定");
            }
            Json(json!({ "ok": true, "pruned": pruned })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

/// 连通性测试（POST /api/nexus/links/:id/probe）：验证地址可达 / 鉴权通过 / 命令存在。
/// 秒级返回，不做任务级执行。
pub async fn probe_link_handler(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let cfg = (*st.core.config()).clone();
    match exm_core::nexus::probe_link(&cfg, &id).await {
        Ok(_) => Json(json!({ "ok": true, "message": "" })).into_response(),
        Err(e) => Json(json!({ "ok": false, "message": e.to_string() })).into_response(),
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/nexus/links", get(list_links).post(save_link))
        .route("/api/nexus/links/:id", axum::routing::delete(delete_link))
        .route("/api/nexus/links/:id/probe", axum::routing::post(probe_link_handler))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn test_state() -> AppState {
        let dir = std::env::temp_dir().join(format!("exm-nexus-api-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let core = exm_core::Core::create(&dir).expect("创建 Core 失败");
        AppState { core: std::sync::Arc::new(core) }
    }

    /// oneshot 消耗 Router：每次由 mk_app 重建（state 共享同一 Arc<Core>，配置/注册表状态跨请求保留）
    async fn call(app: Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
        let resp = app.oneshot(req).await.unwrap();
        let code = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let v = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (code, v)
    }

    fn json_req(method: &str, uri: &str, body: Option<serde_json::Value>) -> Request<Body> {
        let mut b = Request::builder().method(method).uri(uri);
        if body.is_some() {
            b = b.header("content-type", "application/json");
        }
        b.body(Body::from(body.map(|v| v.to_string()).unwrap_or_default())).unwrap()
    }

    fn mk_app(st: &AppState) -> Router {
        routes().with_state(AppState { core: st.core.clone() })
    }

    /// 保存 → 列表 → probe（CLI 形态）→ 删除回路
    #[tokio::test]
    async fn 连结体_crud与探测回路() {
        let st = test_state();

        // 保存 CLI 连结体（echo 存在于 PATH）
        #[cfg(windows)]
        let echo_cmd = "cmd /c echo";
        #[cfg(not(windows))]
        let echo_cmd = "echo";
        let (code, v) = call(
            mk_app(&st),
            json_req(
                "POST",
                "/api/nexus/links",
                Some(serde_json::json!({
                    "id": "cli-echo", "name": "回显", "kind": "custom",
                    "command": echo_cmd,
                    "enabled": true
                })),
            ),
        )
        .await;
        assert_eq!(code, StatusCode::OK, "{v:?}");
        // 列表可见
        let (_, list) = call(mk_app(&st), json_req("GET", "/api/nexus/links", None)).await;
        assert_eq!(list["links"].as_array().unwrap().len(), 1);

        // probe：命令在 PATH 上 → ok
        let (_, p) = call(mk_app(&st), json_req("POST", "/api/nexus/links/cli-echo/probe", None)).await;
        assert_eq!(p["ok"], serde_json::json!(true), "{p:?}");

        // probe 不存在的连结体：不 ok，带人话诊断
        let (_, p2) = call(mk_app(&st), json_req("POST", "/api/nexus/links/ghost/probe", None)).await;
        assert_eq!(p2["ok"], serde_json::json!(false));

        // 删除
        let (code, _) = call(mk_app(&st), json_req("DELETE", "/api/nexus/links/cli-echo", None)).await;
        assert_eq!(code, StatusCode::OK);
        let (_, list2) = call(mk_app(&st), json_req("GET", "/api/nexus/links", None)).await;
        assert!(list2["links"].as_array().unwrap().is_empty());
    }

    /// 删除连结体时清理个体死绑定：绑定 → 删连结体 → 个体 link 置空
    #[tokio::test]
    async fn 删除连结体_清理个体死绑定() {
        let st = test_state();
        let core = st.core.clone();
        {
            let mut cfg = (*core.config()).clone();
            cfg.nexus.links.push(LinkTarget {
                id: "lnk1".into(), name: "测试连结".into(), kind: "custom".into(),
                command: "echo".into(), ..Default::default()
            });
            core.apply_config(cfg).unwrap();
        }
        let saved = core.registry().upsert_single(
            exm_core::types::AgentDefinition {
                name: "代理体".into(), identifier: "proxy-1".into(), domain: "通用".into(),
                tier: exm_core::types::Tier::Orchestrator, description: "连结体代理".into(),
                capabilities: vec![], tools: vec![], when_to_call: String::new(),
                dependencies: vec![], composable_with: vec![],
                input_schema: Default::default(), output_schema: Default::default(),
                prompt_file: String::new(), model_hint: None,
                link: Some("lnk1".into()),
            },
            None,
        )
        .expect("创建智能体失败");
        assert_eq!(saved.link.as_deref(), Some("lnk1"));

        let (code, v) = call(mk_app(&st), json_req("DELETE", "/api/nexus/links/lnk1", None)).await;
        assert_eq!(code, StatusCode::OK, "{v:?}");
        assert_eq!(v["pruned"], serde_json::json!(1), "应清理 1 个死绑定：{v:?}");

        // 个体仍在，link 已置空（registry 直接验证；core 与 app 共享同一 Arc）
        let singles = core.registry().list_singles();
        let proxy = singles.iter().find(|d| d.identifier == "proxy-1").expect("个体应保留");
        assert_eq!(proxy.link, None, "死绑定应被清理");
    }
}
