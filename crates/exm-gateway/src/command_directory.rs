//! HTTP view of the versioned command directory and request-scoped capability snapshot.

use super::*;
use exm_core::command::{
    CapabilityItem, CapabilitySnapshot, CapabilityStatus, CommandActor, CommandActorKind,
    CommandDirectorySnapshot, CommandEntry, CommandRegistry, CommandRegistryDiagnostic,
    CommandScope, COMMAND_PROTOCOL_VERSION,
};
use exm_core::command_service::CommandTransportContext;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DirectoryParams {
    protocol_version: Option<u16>,
    session_id: Option<String>,
    scope: Option<String>,
    #[serde(default)]
    include_deprecated: bool,
}

fn parse_scope(raw: Option<&str>) -> Result<Option<CommandScope>, String> {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(None),
        Some("global") => Ok(Some(CommandScope::Global)),
        Some("project") => Ok(Some(CommandScope::Project)),
        Some("session") => Ok(Some(CommandScope::Session)),
        Some("flow") => Ok(Some(CommandScope::Flow)),
        Some(_) => Err("scope 仅支持 global、project、session、flow".into()),
    }
}

fn context_for_request(
    st: &AppState,
    params: &DirectoryParams,
    headers: &axum::http::HeaderMap,
) -> Result<CommandTransportContext, axum::response::Response> {
    let session = match params.session_id.as_deref() {
        Some(id) => Some(
            st.core
                .store
                .get_session(id)
                .ok()
                .flatten()
                .ok_or_else(|| {
                    (StatusCode::NOT_FOUND, Json(json!({"error":"会话不存在"}))).into_response()
                })?,
        ),
        None => None,
    };

    let workspace_header = headers
        .get("x-exm-workspace")
        .map(|value| {
            value.to_str().map(crate::ws::percent_decode).map_err(|_| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error":"项目目录请求头无效"})),
                )
                    .into_response()
            })
        })
        .transpose()?;

    let project = if let Some(raw) = workspace_header.as_deref() {
        let root = crate::project_scope::pinned(st, raw)?;
        if session
            .as_ref()
            .is_some_and(|session| session.workspace.is_none())
        {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({"error":"会话不属于当前项目"})),
            )
                .into_response());
        }
        if let Some(session_workspace) = session.as_ref().and_then(|s| s.workspace.as_deref()) {
            let owner = exm_core::workspace::pinned(session_workspace).map_err(|_| {
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error":"会话项目不可用"})),
                )
                    .into_response()
            })?;
            if owner != root {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(json!({"error":"会话不属于当前项目"})),
                )
                    .into_response());
            }
        }
        Some(root.to_string_lossy().into_owned())
    } else if session
        .as_ref()
        .is_some_and(|session| session.workspace.is_some())
    {
        // 项目固定会话必须经项目连接访问，和 /api/sessions/:id 的边界一致。
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error":"请通过该会话所属项目连接查询命令目录"})),
        )
            .into_response());
    } else {
        st.core
            .registry()
            .effective_workspace(&st.core.config().workspace_root)
            .canonicalize()
            .ok()
            .map(|root| root.to_string_lossy().into_owned())
    };

    let key_configured = !st.core.config().security.auth_key.is_empty();
    let actor = CommandActor {
        id: if key_configured {
            "http-authenticated-owner".into()
        } else {
            "http-anonymous".into()
        },
        kind: CommandActorKind::User,
        // With authKey disabled, the existing gateway is public; only read commands
        // are discoverable as executable. When enabled, auth_middleware has already
        // verified the instance control key before this handler is reached.
        roles: if key_configured {
            vec!["owner".into()]
        } else {
            Vec::new()
        },
    };
    Ok(CommandTransportContext {
        entry: CommandEntry::Http,
        actor,
        authenticated: true,
        session: params.session_id.clone(),
        project,
        flow: None,
        capabilities: vec![CapabilityItem {
            name: "commands.directory".into(),
            status: CapabilityStatus::Available,
            reason: None,
        }],
    })
}

async fn command_directory(
    st: &AppState,
    params: &DirectoryParams,
    headers: &axum::http::HeaderMap,
) -> Result<(CommandTransportContext, CommandDirectorySnapshot), axum::response::Response> {
    if params
        .protocol_version
        .is_some_and(|version| version != COMMAND_PROTOCOL_VERSION)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": {"category":"version_mismatch", "message":"命令协议版本不受支持"},
                "supportedVersion": COMMAND_PROTOCOL_VERSION
            })),
        )
            .into_response());
    }
    let context = context_for_request(st, params, headers)?;
    let scope = parse_scope(params.scope.as_deref()).map_err(|message| {
        (StatusCode::BAD_REQUEST, Json(json!({"error":message}))).into_response()
    })?;
    let workspace = context.project.as_deref().map(std::path::PathBuf::from);
    exm_core::workspace::scope(workspace, async move {
        let registry = CommandRegistry::with_builtins().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":"命令目录初始化失败"})),
            )
                .into_response()
        })?;
        let query = context.directory_query(scope, params.include_deprecated);
        let mut directory = registry.query(&query);

        // 动态技能/插件目录每次从 Core 事实源重建；依赖失效不保留旧命令。
        match st
            .core
            .extension_command_directory(&context, exm_core::types::ToolScope::Computer)
        {
            Ok(dynamic) => {
                directory.entries.extend(dynamic.entries);
                directory.diagnostics.extend(dynamic.diagnostics);
                directory
                    .entries
                    .sort_by(|left, right| left.descriptor.name.cmp(&right.descriptor.name));
            }
            Err(error) => directory.diagnostics.push(CommandRegistryDiagnostic {
                severity: exm_core::command::CommandDiagnosticSeverity::Warning,
                category: error.category,
                code: "dynamic_command_directory_unavailable".into(),
                command: String::new(),
                alias: None,
                message: "技能/插件命令目录暂不可用".into(),
                replacement: None,
            }),
        }
        Ok((context, directory))
    })
    .await
}

pub(crate) async fn get_commands(
    State(st): State<AppState>,
    Query(params): Query<DirectoryParams>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    match command_directory(&st, &params, &headers).await {
        Ok((_, directory)) => Json(directory).into_response(),
        Err(response) => response,
    }
}

pub(crate) async fn get_capabilities(
    State(st): State<AppState>,
    Query(params): Query<DirectoryParams>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let (context, directory) = match command_directory(&st, &params, &headers).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let mut capabilities = context.capabilities.clone();
    for entry in &directory.entries {
        let status = if entry.executable {
            CapabilityStatus::Available
        } else {
            match entry.unavailable_reason {
                Some(exm_core::command::CommandAvailabilityReason::ForbiddenPermission) => {
                    CapabilityStatus::Forbidden
                }
                Some(exm_core::command::CommandAvailabilityReason::MissingDependency) => {
                    CapabilityStatus::MissingDependency
                }
                Some(exm_core::command::CommandAvailabilityReason::CapabilityUnverified) => {
                    CapabilityStatus::Unverified
                }
                Some(exm_core::command::CommandAvailabilityReason::CapabilityDisabled) => {
                    CapabilityStatus::Disabled
                }
                _ => CapabilityStatus::Unavailable,
            }
        };
        capabilities.push(CapabilityItem {
            name: format!("command.{}", entry.descriptor.name),
            status,
            reason: entry
                .unavailable_reason
                .map(|reason| format!("命令不可用：{reason:?}")),
        });
    }
    let project = if context.actor.roles.is_empty() {
        None
    } else {
        context.project
    };
    let snapshot = CapabilitySnapshot {
        protocol_version: COMMAND_PROTOCOL_VERSION,
        snapshot_id: exm_core::types::new_id(),
        generated_at: exm_core::types::now_iso(),
        entry: context.entry,
        actor: context.actor,
        session: context.session,
        // Pinned projects already require an authenticated project connection;
        // do not disclose the server's fallback root to a public directory caller.
        project,
        flow: context.flow,
        capabilities,
        commands: directory
            .entries
            .into_iter()
            .filter(|entry| entry.executable)
            .map(|entry| entry.descriptor.name)
            .collect(),
    };
    Json(snapshot).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use exm_core::command::CommandAvailabilityReason;
    use tower::ServiceExt;

    fn test_core() -> Arc<Core> {
        let root = std::env::temp_dir().join(format!("exm-command-dir-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut config = ExmConfig::load(&root);
        config.use_mock = true;
        Arc::new(Core::with_config(config).unwrap())
    }

    #[tokio::test]
    async fn http_command_directory_and_capabilities_are_versioned_and_role_filtered() {
        let core = test_core();
        let session = core.create_session("command-directory").unwrap();
        let app = crate::build_router(core);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/commands?sessionId={}", session.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let directory: CommandDirectorySnapshot = serde_json::from_slice(&body).unwrap();
        assert_eq!(directory.protocol_version, COMMAND_PROTOCOL_VERSION);
        let help = directory
            .entries
            .iter()
            .find(|entry| entry.descriptor.name == "help")
            .unwrap();
        assert!(!help.executable);
        assert_eq!(
            help.unavailable_reason,
            Some(CommandAvailabilityReason::UnsupportedEntry)
        );
        let export = directory
            .entries
            .iter()
            .find(|entry| entry.descriptor.name == "spec.export")
            .unwrap();
        assert_eq!(
            export.unavailable_reason,
            Some(CommandAvailabilityReason::UnsupportedEntry)
        );

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/capabilities?sessionId={}", session.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let capabilities: CapabilitySnapshot = serde_json::from_slice(&body).unwrap();
        assert_eq!(capabilities.entry, CommandEntry::Http);
        assert!(capabilities.commands.is_empty());
        assert_eq!(
            capabilities.project, None,
            "public response must not expose the server root"
        );
        assert!(capabilities.capabilities.iter().any(|capability| {
            capability.name == "command.spec.export"
                && capability.status == CapabilityStatus::Unavailable
        }));
    }

    #[tokio::test]
    async fn http_directory_checks_auth_version_and_project_binding_without_fake_handlers() {
        let root = std::env::temp_dir().join(format!("exm-command-auth-{}", uuid::Uuid::new_v4()));
        let project = root.join("project");
        let other = root.join("other");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let mut config = ExmConfig::load(&root);
        config.use_mock = true;
        config.security.auth_key = "fixture-command-key".into();
        let core = Arc::new(Core::with_config(config).unwrap());
        let session = core
            .store
            .create_session_in(
                "directory-project",
                &core.registry().active_scope(),
                Some(project.to_string_lossy().into_owned()),
            )
            .unwrap();
        let app = crate::build_router(core.clone());
        for (path, key, workspace, expected) in [
            (
                "/api/commands".to_string(),
                None,
                None,
                StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/commands?protocolVersion=2".into(),
                Some("fixture-command-key"),
                None,
                StatusCode::BAD_REQUEST,
            ),
            (
                "/api/commands?scope=bogus".into(),
                Some("fixture-command-key"),
                None,
                StatusCode::BAD_REQUEST,
            ),
            (
                format!("/api/commands?sessionId={}", session.id),
                Some("fixture-command-key"),
                None,
                StatusCode::FORBIDDEN,
            ),
            (
                format!("/api/commands?sessionId={}", session.id),
                Some("fixture-command-key"),
                Some(&other),
                StatusCode::NOT_FOUND,
            ),
            (
                format!("/api/commands?sessionId={}", session.id),
                Some("fixture-command-key"),
                Some(&project),
                StatusCode::OK,
            ),
        ] {
            let mut request = Request::builder().uri(&path);
            if let Some(key) = key {
                request = request.header("x-auth-key", key);
            }
            if let Some(workspace) = workspace {
                request = request.header("x-exm-workspace", workspace.to_str().unwrap());
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "{path}");
            if expected == StatusCode::OK {
                let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
                let directory: CommandDirectorySnapshot = serde_json::from_slice(&body).unwrap();
                let export = directory
                    .entries
                    .iter()
                    .find(|entry| entry.descriptor.name == "spec.export")
                    .unwrap();
                assert!(!export.executable);
                assert_eq!(
                    export.unavailable_reason,
                    Some(CommandAvailabilityReason::UnsupportedEntry)
                );
            }
        }
        assert!(core.store.list_events(&session.id).unwrap().is_empty());
        drop(app);
        drop(core);
        let _ = std::fs::remove_dir_all(root);
    }
}
