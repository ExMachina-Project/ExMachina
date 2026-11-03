//! gateway/groups_admin —— 自 lib.rs 按资源域拆出（纯搬移，零行为变更）

use super::*;

// ---------------------------------------------------------------- 智能体组

pub(crate) async fn list_groups(State(st): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "active": st.core.active_group(),
        "groups": st.core.list_groups(),
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateGroupBody {
    name: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    description: Option<String>,
    /// 组模板 id(entities/templates/group/<id>):提供后附加模板 SOUL(组人格骨架)
    #[serde(default)]
    template: Option<String>,
}

/// 编成模板清单(GET /api/templates):unit = 子个体模板,group = 组模板
pub(crate) async fn list_templates(State(st): State<AppState>) -> impl IntoResponse {
    Json(json!({ "templates": st.core.registry().list_templates() }))
}

pub(crate) async fn create_group(
    State(st): State<AppState>,
    Json(b): Json<CreateGroupBody>,
) -> impl IntoResponse {
    // 模板创建:附加模板 SOUL(组人格骨架)
    if let Some(tpl) = b
        .template
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        return match st.core.registry().create_group_from_template(
            tpl,
            b.id,
            &b.name,
            b.description.as_deref().unwrap_or(""),
        ) {
            Ok(meta) => Json(serde_json::to_value(meta).unwrap_or(Value::Null)).into_response(),
            Err(e) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response(),
        };
    }
    match st
        .core
        .create_group(b.id, &b.name, b.description.as_deref().unwrap_or(""))
    {
        Ok(meta) => Json(serde_json::to_value(meta).unwrap_or(Value::Null)).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub(crate) async fn active_group(State(st): State<AppState>) -> impl IntoResponse {
    Json(json!({ "active": st.core.active_group() }))
}

#[derive(Deserialize)]
pub(crate) struct SwitchGroupBody {
    id: String,
}

pub(crate) async fn switch_group(
    State(st): State<AppState>,
    Json(b): Json<SwitchGroupBody>,
) -> impl IntoResponse {
    match st.core.switch_group(&b.id) {
        Ok(_) => Json(json!({ "ok": true, "active": b.id })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub(crate) async fn activate_group(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    switch_group(State(st), Json(SwitchGroupBody { id })).await
}

pub(crate) async fn delete_group(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    match st.core.delete_group(&id) {
        Ok(_) => Json(json!({ "ok": true })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
