//! Project scope middleware. A project is request-local; the registry default is never changed.
use super::*;

fn query(uri: &axum::http::Uri, key: &str) -> Option<String> {
    uri.query()?.split('&').find_map(|part| {
        let (k, v) = part.split_once('=')?;
        (k == key).then(|| crate::ws::percent_decode(&v.replace('+', " ")))
    })
}

fn header(headers: &axum::http::HeaderMap) -> Result<Option<String>, axum::response::Response> {
    headers
        .get("x-exm-workspace")
        .map(|v| {
            v.to_str().map(crate::ws::percent_decode).map_err(|_| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "项目目录请求头无效" })),
                )
                    .into_response()
            })
        })
        .transpose()
}

pub(crate) fn required(
    st: &AppState,
    headers: &axum::http::HeaderMap,
) -> Result<String, axum::response::Response> {
    let raw = header(headers)?.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "请通过项目连接访问协作群聊" })),
        )
            .into_response()
    })?;
    pinned(st, &raw).map(|root| root.to_string_lossy().into_owned())
}

pub(crate) async fn scope(
    State(st): State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let path = req.uri().path();
    let editor = matches!(
        path,
        "/api/fs/list"
            | "/api/fs/file"
            | "/api/git/op"
            | "/api/git/overview"
            | "/api/workbench/tree"
            | "/api/workspace/changes"
    ) || (req.method() == axum::http::Method::GET && path == "/api/workspace");
    let asset = matches!(path, "/api/asset" | "/api/asset/download");
    let list =
        (path == "/api/sessions" || path == "/api/sessions/search" || path == "/api/approvals")
            && req.method() == axum::http::Method::GET;
    let create = path == "/api/sessions" && req.method() == axum::http::Method::POST;
    let batch = path == "/api/sessions/batch";
    let session_id = path
        .strip_prefix("/api/sessions/")
        .and_then(|s| s.split('/').next())
        .filter(|id| !matches!(*id, "search" | "batch" | "flow-archive"));
    let approval_id = path
        .strip_prefix("/api/approvals/")
        .and_then(|s| s.split('/').next());
    if !editor
        && !asset
        && !list
        && !create
        && !batch
        && session_id.is_none()
        && approval_id.is_none()
    {
        return next.run(req).await;
    }
    let header = match header(req.headers()) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let session_resource = if asset {
        query(req.uri(), "sessionId")
    } else {
        session_id.map(str::to_string)
    };
    let mut collaboration_session = false;
    let owner = if let Some(id) = session_resource.as_deref() {
        match st.core.store.get_session(id) {
            Ok(Some(session)) => {
                collaboration_session = session.collaboration_id.is_some();
                session.workspace
            }
            _ => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({ "error": "会话不存在" })),
                )
                    .into_response()
            }
        }
    } else if let Some(id) = approval_id {
        match st.core.approval_get(id).ok().flatten() {
            Some(approval) => match st.core.approval_workspace(&approval) {
                Ok(workspace) => workspace,
                Err(_) => {
                    return (
                        StatusCode::NOT_FOUND,
                        Json(json!({ "error": "审批不存在" })),
                    )
                        .into_response()
                }
            },
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({ "error": "审批不存在" })),
                )
                    .into_response()
            }
        }
    } else {
        None
    };
    if header.is_some() && session_resource.is_some() && owner.is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "会话不属于当前项目" })),
        )
            .into_response();
    }
    if header.is_some() && approval_id.is_some() && owner.is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "审批不属于当前项目" })),
        )
            .into_response();
    }
    if let (Some(owner), Some(header)) = (owner.as_deref(), header.as_deref()) {
        if pinned(&st, header)
            .map(|p| p.to_string_lossy() == owner)
            .unwrap_or(false)
            == false
        {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "资源不属于当前项目" })),
            )
                .into_response();
        }
    }
    if list {
        let query_workspace = query(req.uri(), "workspace");
        if query_workspace.is_some() != header.is_some() {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "项目连接与查询作用域必须同时提供" })),
            )
                .into_response();
        }
        if let (Some(header), Some(query_workspace)) =
            (header.as_deref(), query_workspace.as_deref())
        {
            if header != query_workspace {
                return (
                    StatusCode::FORBIDDEN,
                    Json(json!({ "error": "项目作用域不匹配" })),
                )
                    .into_response();
            }
        }
    }
    let raw = owner.as_deref().or(header.as_deref());
    let Some(raw) = raw else {
        return next.run(req).await;
    };
    let root = match pinned(&st, raw) {
        Ok(root) => root,
        Err(e) => return e,
    };
    if owner.is_some() && header.is_none() && !asset {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "项目资源必须通过项目连接访问" })),
        )
            .into_response();
    }
    if collaboration_session
        && !matches!(
            *req.method(),
            axum::http::Method::GET | axum::http::Method::HEAD
        )
    {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "请在协作群聊中操作成员任务" })),
        )
            .into_response();
    }
    exm_core::workspace::scope(Some(root), next.run(req)).await
}

pub(crate) fn pinned(
    st: &AppState,
    raw: &str,
) -> Result<std::path::PathBuf, axum::response::Response> {
    validate(st, exm_core::workspace::pinned, raw)
}
fn validate(
    st: &AppState,
    f: fn(&str) -> anyhow::Result<std::path::PathBuf>,
    raw: &str,
) -> Result<std::path::PathBuf, axum::response::Response> {
    if st.core.config().security.auth_key.is_empty() {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "项目连接必须启用访问密钥" })),
        )
            .into_response());
    }
    f(raw).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response()
    })
}
