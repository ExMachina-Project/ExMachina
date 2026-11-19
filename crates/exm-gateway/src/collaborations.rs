//! Project-scoped collaboration REST API. The room belongs to the host project;
//! the immutable roster is the only cross-project authority for this API.
use super::*;
use axum::http::HeaderMap;
use exm_core::collaboration::{CollaborationError, MemberInput, TurnInput};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/collaborations", get(list).post(create))
        .route("/api/collaborations/:id", get(detail))
        .route("/api/collaborations/:id/turns", post(send))
        .route("/api/collaborations/:id/turns/:turn_id/stop", post(stop))
}

fn failure(error: CollaborationError) -> axum::response::Response {
    let status = match error {
        CollaborationError::Invalid(_) => StatusCode::BAD_REQUEST,
        CollaborationError::NotFound => StatusCode::NOT_FOUND,
        CollaborationError::Conflict(_) => StatusCode::CONFLICT,
        CollaborationError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(json!({ "error": error.to_string() }))).into_response()
}

async fn list(State(st): State<AppState>, headers: HeaderMap) -> axum::response::Response {
    let workspace = match crate::project_scope::required(&st, &headers) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    match st.core.list_collaborations(&workspace) {
        Ok(rooms) => Json(rooms).into_response(),
        Err(error) => failure(error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateBody {
    title: String,
    members: Vec<MemberInput>,
}

async fn create(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateBody>,
) -> axum::response::Response {
    let workspace = match crate::project_scope::required(&st, &headers) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    match st
        .core
        .create_collaboration(&workspace, &body.title, body.members)
    {
        Ok(room) => (StatusCode::CREATED, Json(room)).into_response(),
        Err(error) => failure(error),
    }
}

#[derive(Deserialize)]
struct HistoryQuery {
    before: Option<String>,
}

async fn detail(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> axum::response::Response {
    let workspace = match crate::project_scope::required(&st, &headers) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    match st
        .core
        .collaboration_page(&workspace, &id, query.before.as_deref())
    {
        Ok(page) => Json(page).into_response(),
        Err(error) => failure(error),
    }
}

async fn send(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<TurnInput>,
) -> axum::response::Response {
    let workspace = match crate::project_scope::required(&st, &headers) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    match st.core.start_collaboration(&workspace, &id, body) {
        Ok(turn) => (StatusCode::ACCEPTED, Json(json!({ "id": turn }))).into_response(),
        Err(error) => failure(error),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StopBody {
    session_id: Option<String>,
}

async fn stop(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path((id, turn)): Path<(String, String)>,
    Json(body): Json<StopBody>,
) -> axum::response::Response {
    let workspace = match crate::project_scope::required(&st, &headers) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    match st
        .core
        .stop_collaboration(&workspace, &id, &turn, body.session_id.as_deref())
    {
        Ok(stopped) => Json(json!({ "stopped": stopped })).into_response(),
        Err(error) => failure(error),
    }
}
