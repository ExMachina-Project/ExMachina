//! Explicit owner-scoped model assignment APIs. Saving does not switch the active target.
use crate::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, put},
    Json, Router,
};
use exm_core::model_routing::{ModelPurpose, ModelRouting};
use serde_json::{json, Value};

fn error(error: impl std::fmt::Display) -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":error.to_string()})),
    )
        .into_response()
}

async fn single_preview(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> axum::response::Response {
    match st.core.orchestrator().routing_policies(None, &id).await {
        Ok(value) => Json(value).into_response(),
        Err(e) => error(e),
    }
}

async fn member_preview(
    State(st): State<AppState>,
    Path((group, id)): Path<(String, String)>,
) -> axum::response::Response {
    match st
        .core
        .orchestrator()
        .routing_policies(Some(&group), &id)
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(e) => error(e),
    }
}

async fn save_single(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(routing): Json<ModelRouting>,
) -> axum::response::Response {
    if let Err(e) = st.core.orchestrator().validate_routing(&routing) {
        return error(e);
    }
    match st
        .core
        .registry()
        .set_agent_model_routing(None, &id, routing)
    {
        Ok(agent) => Json(json!({"ok":true,"modelRouting":agent.model_routing})).into_response(),
        Err(e) => error(e),
    }
}
async fn save_member(
    State(st): State<AppState>,
    Path((group, id)): Path<(String, String)>,
    Json(routing): Json<ModelRouting>,
) -> axum::response::Response {
    if let Err(e) = st.core.orchestrator().validate_routing(&routing) {
        return error(e);
    }
    match st
        .core
        .registry()
        .set_agent_model_routing(Some(&group), &id, routing)
    {
        Ok(agent) => Json(json!({"ok":true,"modelRouting":agent.model_routing})).into_response(),
        Err(e) => error(e),
    }
}
async fn save_group(
    State(st): State<AppState>,
    Path(group): Path<String>,
    Json(routing): Json<ModelRouting>,
) -> axum::response::Response {
    if let Err(e) = st.core.orchestrator().validate_routing(&routing) {
        return error(e);
    }
    match st.core.registry().set_group_model_routing(&group,routing) {
        Ok(())=>Json(json!({"ok":true,"modelRouting":st.core.group_meta(&group).and_then(|m|m.model_routing)})).into_response(),Err(e)=>error(e),
    }
}
async fn catalog(State(st): State<AppState>) -> Json<Value> {
    let cfg = st.core.config();
    let models:Vec<_>=cfg.llm_profiles.iter().flat_map(|p|p.models.iter().map(move|m|json!({
        "reference":format!("{}/{}",p.id,m.model),"provider":p.name,"model":m.model,"purposes":m.purposes,"cost":m.cost,
        "usageNotes":m.usage_notes,"autoEligible":m.auto_eligible,"vision":m.vision,"audio":m.audio,"contextWindow":m.context_window,
    }))).collect();
    Json(json!({"purposes":ModelPurpose::ALL,"models":models}))
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/llm/catalog", get(catalog))
        .route(
            "/api/singles/:id/model-routing",
            get(single_preview).put(save_single),
        )
        .route("/api/groups/:id/model-routing", put(save_group))
        .route(
            "/api/groups/:gid/agents/:identifier/model-routing",
            get(member_preview).put(save_member),
        )
}
