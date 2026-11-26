//! Authenticated, read-only management projection of the existing connection sources.
//!
//! This endpoint is not an Agent directory or an execution interface. It deliberately
//! exposes only the sanitized projection emitted by Core, applies bounded filtering and
//! paging after that projection, and never probes or refreshes a source.

use crate::AppState;
use axum::extract::{rejection::QueryRejection, Extension, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use exm_core::connections::identity::{ConnectionKind, SourceKind};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Arc;

const MAX_LIMIT: usize = 200;
const MAX_OFFSET: usize = 1_000_000;
const MAX_SEARCH_BYTES: usize = 256;
const CURSOR_TTL_MS: u64 = 5 * 60 * 1000;
const MAX_CURSORS: usize = 1024;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DirectoryQuery {
    #[serde(default)]
    search: String,
    kind: Option<ConnectionKind>,
    source_kind: Option<SourceKind>,
    limit: Option<usize>,
    offset: Option<usize>,
    cursor: Option<String>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/connections", get(list_connections))
        .layer(Extension(Arc::new(parking_lot::Mutex::new(
            DirectoryPager::new(),
        ))))
}

struct CursorState {
    binding: Vec<u8>,
    created_at: u64,
    expected_offset: usize,
}

struct DirectoryPager {
    secret: Vec<u8>,
    cursors: HashMap<String, CursorState>,
}

impl DirectoryPager {
    fn new() -> Self {
        let first = uuid::Uuid::new_v4();
        let second = uuid::Uuid::new_v4();
        let mut secret = Vec::with_capacity(32);
        secret.extend_from_slice(first.as_bytes());
        secret.extend_from_slice(second.as_bytes());
        Self {
            secret,
            cursors: HashMap::new(),
        }
    }

    fn next_cursor(
        &mut self,
        material: &[u8],
        supplied: Option<&str>,
        offset: usize,
        next_offset: Option<usize>,
        now: u64,
    ) -> Result<Option<String>, ()> {
        self.cursors
            .retain(|_, state| now >= state.created_at && now - state.created_at < CURSOR_TTL_MS);
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.secret).map_err(|_| ())?;
        mac.update(material);
        let binding = mac.finalize().into_bytes().to_vec();
        if let Some(cursor) = supplied {
            let state = self.cursors.get(cursor).ok_or(())?;
            if state.binding != binding || state.expected_offset != offset {
                return Err(());
            }
            if let Some(next_offset) = next_offset {
                let next = uuid::Uuid::new_v4().to_string();
                if self.cursors.len() >= MAX_CURSORS {
                    if let Some(oldest) = self
                        .cursors
                        .iter()
                        .min_by_key(|(_, state)| state.created_at)
                        .map(|(key, _)| key.clone())
                    {
                        self.cursors.remove(&oldest);
                    }
                }
                self.cursors.insert(
                    next.clone(),
                    CursorState {
                        binding,
                        created_at: now,
                        expected_offset: next_offset,
                    },
                );
                return Ok(Some(next));
            }
            return Ok(None);
        }
        if offset != 0 {
            return Err(());
        }
        let Some(expected_offset) = next_offset else {
            return Ok(None);
        };
        if self.cursors.len() >= MAX_CURSORS {
            if let Some(oldest) = self
                .cursors
                .iter()
                .min_by_key(|(_, state)| state.created_at)
                .map(|(key, _)| key.clone())
            {
                self.cursors.remove(&oldest);
            }
        }
        let cursor = uuid::Uuid::new_v4().to_string();
        self.cursors.insert(
            cursor.clone(),
            CursorState {
                binding,
                created_at: now,
                expected_offset,
            },
        );
        Ok(Some(cursor))
    }
}

fn connection_kind_name(kind: ConnectionKind) -> &'static str {
    match kind {
        ConnectionKind::Software => "software",
        ConnectionKind::ExternalAgent => "external_agent",
        ConnectionKind::Device => "device",
    }
}

fn source_kind_name(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Nexus => "nexus",
        SourceKind::Mcp => "mcp",
        SourceKind::Plugin => "plugin",
        SourceKind::Device => "device",
        SourceKind::Software => "software",
    }
}

async fn list_connections(
    State(state): State<AppState>,
    Extension(pager): Extension<Arc<parking_lot::Mutex<DirectoryPager>>>,
    headers: HeaderMap,
    query: Result<Query<DirectoryQuery>, QueryRejection>,
) -> impl IntoResponse {
    // A connection catalogue contains instance/account metadata and is an admin surface.
    // Unlike a read-only source projection, an unconfigured global admin credential is
    // not treated as implicit authorization for this new endpoint.
    let config = state.core.config();
    if config.security.auth_key.trim().is_empty() {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"连接目录需要启用管理访问密钥"})),
        )
            .into_response();
    }
    let provided = headers
        .get("x-auth-key")
        .and_then(|value| value.to_str().ok())
        .or_else(|| {
            headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
        });
    if provided != Some(config.security.auth_key.as_str()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"需要管理访问密钥"})),
        )
            .into_response();
    }
    // This first interface is an installation-management surface, not a project or
    // session ACL endpoint. Never silently turn a project connection into instance read.
    if [
        "x-exm-workspace",
        "x-exm-project",
        "x-exm-session",
        "x-exm-actor",
    ]
    .iter()
    .any(|name| headers.contains_key(*name))
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"请通过实例管理连接查询目录"})),
        )
            .into_response();
    }
    let query = match query {
        Ok(Query(query)) => query,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"目录查询参数无效"})),
            )
                .into_response()
        }
    };
    let limit = query
        .limit
        .unwrap_or_else(|| config.experiments.connection_fabric.catalog_page_size as usize);
    let offset = query.offset.unwrap_or(0);
    if !(1..=MAX_LIMIT).contains(&limit) || offset > MAX_OFFSET {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"目录分页参数超出范围"})),
        )
            .into_response();
    }
    if query.search.len() > MAX_SEARCH_BYTES || query.search.chars().any(char::is_control) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"目录搜索条件无效或超限"})),
        )
            .into_response();
    }
    if query
        .cursor
        .as_ref()
        .is_some_and(|cursor| cursor.len() != 36 || uuid::Uuid::parse_str(cursor).is_err())
    {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"目录游标无效或过期，请重新查询"})),
        )
            .into_response();
    }

    let snapshot = match state.core.admin_connection_source_snapshot() {
        Ok(snapshot) => snapshot,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":"连接目录暂不可用"})),
            )
                .into_response()
        }
    };
    let (Some(all), Some(issues)) = (
        snapshot.get("records").and_then(Value::as_array),
        snapshot.get("issues").and_then(Value::as_array),
    ) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"连接目录快照无效"})),
        )
            .into_response();
    };
    let search = query.search.to_lowercase();
    let matched: Vec<Value> = all
        .iter()
        .filter(|record| {
            query.kind.is_none_or(|kind| {
                record.get("kind").and_then(Value::as_str) == Some(connection_kind_name(kind))
            }) && query.source_kind.is_none_or(|kind| {
                record.pointer("/sourceRef/kind").and_then(Value::as_str)
                    == Some(source_kind_name(kind))
            }) && (search.is_empty()
                || record
                    .get("displayName")
                    .and_then(Value::as_str)
                    .is_some_and(|value| value.to_lowercase().contains(&search))
                || record
                    .pointer("/sourceRef/sourceId")
                    .and_then(Value::as_str)
                    .is_some_and(|value| value.to_lowercase().contains(&search))
                || record
                    .get("capabilities")
                    .and_then(Value::as_array)
                    .is_some_and(|capabilities| {
                        capabilities.iter().any(|capability| {
                            capability
                                .get("name")
                                .and_then(Value::as_str)
                                .is_some_and(|value| value.to_lowercase().contains(&search))
                        })
                    }))
        })
        .cloned()
        .collect();
    // Only the already-authorized installation view is bound into a cursor. The HMAC
    // remains server-local, including the authentication epoch, never a public hash.
    let material = serde_json::to_vec(&json!({
        "key": config.security.auth_key,
        "scope": state.core.connection_registrations.instance_id(),
        "search": query.search, "kind": query.kind, "sourceKind": query.source_kind,
        "limit": limit, "records": matched, "issues": issues,
    }))
    .unwrap();
    let total = matched.len();
    let records: Vec<_> = matched.into_iter().skip(offset).take(limit).collect();
    let next_offset = offset
        .checked_add(records.len())
        .filter(|next| *next < total);
    let cursor = match pager.lock().next_cursor(
        &material,
        query.cursor.as_deref(),
        offset,
        next_offset,
        exm_core::types::now_ms(),
    ) {
        Ok(cursor) => cursor,
        Err(()) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error":"目录游标无效或过期，请重新查询"})),
            )
                .into_response()
        }
    };
    Json(json!({
        "records": records,
        "issues": issues,
        "total": total,
        "limit": limit,
        "offset": offset,
        "truncated": next_offset.is_some(),
        "nextOffset": next_offset,
        "nextCursor": cursor,
        "managementOnly": true,
        "executionAuthorization": "unknown"
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use exm_core::config::{ExmConfig, LinkTarget};
    use exm_core::Core;
    use std::sync::Arc;
    use tower::ServiceExt;

    #[test]
    fn cursors_are_bound_to_query_snapshot_and_sequential_offset() {
        let mut pager = DirectoryPager::new();
        let first = pager
            .next_cursor(b"snapshot-a", None, 0, Some(10), 100)
            .unwrap()
            .unwrap();
        assert_eq!(
            pager.next_cursor(b"snapshot-a", Some(&first), 0, Some(10), 101),
            Err(())
        );
        assert_eq!(
            pager.next_cursor(b"snapshot-b", Some(&first), 10, Some(20), 102),
            Err(())
        );
        let second = pager
            .next_cursor(b"snapshot-a", Some(&first), 10, Some(20), 102)
            .unwrap()
            .unwrap();
        assert_ne!(first, second);
        // A read cursor can be reused to navigate backward without expanding its scope.
        assert!(pager
            .next_cursor(b"snapshot-a", Some(&first), 10, Some(20), 103)
            .is_ok());
        assert_eq!(
            pager.next_cursor(b"snapshot-a", Some(&second), 20, None, 104),
            Ok(None)
        );
        assert_eq!(
            pager.next_cursor(b"snapshot-a", Some(&second), 10, None, 105),
            Err(())
        );
    }

    #[test]
    fn cursors_expire_and_nonzero_offset_needs_a_cursor() {
        let mut pager = DirectoryPager::new();
        assert_eq!(
            pager.next_cursor(b"snapshot", None, 1, Some(2), 100),
            Err(())
        );
        let cursor = pager
            .next_cursor(b"snapshot", None, 0, Some(2), 100)
            .unwrap()
            .unwrap();
        assert_eq!(
            pager.next_cursor(b"snapshot", Some(&cursor), 2, None, 100 + CURSOR_TTL_MS),
            Err(())
        );
    }

    struct Fixture {
        root: std::path::PathBuf,
        mcp_process_marker: std::path::PathBuf,
        source_listener: std::net::TcpListener,
        app: Router,
    }

    fn tree_snapshot(root: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
        fn visit(
            root: &std::path::Path,
            directory: &std::path::Path,
            out: &mut Vec<(std::path::PathBuf, Vec<u8>)>,
        ) {
            for entry in std::fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let kind = entry.file_type().unwrap();
                if kind.is_dir() {
                    visit(root, &path, out);
                } else if kind.is_file() {
                    out.push((
                        path.strip_prefix(root).unwrap().to_path_buf(),
                        std::fs::read(path).unwrap(),
                    ));
                }
            }
        }
        let mut files = Vec::new();
        visit(root, root, &mut files);
        files.sort_by(|left, right| left.0.cmp(&right.0));
        files
    }

    impl Fixture {
        fn new(auth_key: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "exm-connection-directory-http-{}",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&root).unwrap();
            let mut config = ExmConfig::load(&root);
            config.data_dir = root.join("data");
            config.agents_dir = root.join("entities");
            config.use_mock = true;
            config.security.auth_key = auth_key.into();
            config.experiments.connection_fabric.catalog_page_size = 1;
            let source_listener =
                std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
            source_listener.set_nonblocking(true).unwrap();
            let source_endpoint = format!("http://{}", source_listener.local_addr().unwrap());
            config.llm.base_url = source_endpoint.clone();
            config.llm.api_key = "directory-model-api-key-canary".into();
            config.llm.model = "directory-fixture-model".into();
            let mcp_process_marker = root.join("mcp-process-started");
            #[cfg(unix)]
            {
                // If a directory GET ever starts a configured stdio connector, the
                // process leaves a host-owned marker in this isolated test root.
                config.mcp_servers = vec![serde_json::from_value(json!({
                    "id": "read-only-process-sentinel",
                    "transport": "stdio",
                    "command": "/bin/sh",
                    "args": ["-c", format!("touch '{}'", mcp_process_marker.display())],
                    "enabled": true
                }))
                .unwrap()];
            }
            config.mcp_servers.push(
                serde_json::from_value(json!({
                    "id": "read-only-network-sentinel",
                    "transport": "http",
                    "url": source_endpoint,
                    "enabled": true
                }))
                .unwrap(),
            );
            config.nexus.links = vec![
                LinkTarget {
                    id: "remote-one".into(),
                    name: "Remote One".into(),
                    kind: "exmachina".into(),
                    endpoint: format!("{source_endpoint}/path?token=directory-canary"),
                    api_key: "directory-api-key-canary".into(),
                    ..Default::default()
                },
                LinkTarget {
                    id: "remote-two".into(),
                    name: "Remote Two".into(),
                    kind: "custom".into(),
                    command: "private-command-canary".into(),
                    ..Default::default()
                },
            ];
            let core = Arc::new(Core::with_config(config).unwrap());
            Self {
                root,
                mcp_process_marker,
                source_listener,
                app: crate::build_router(core),
            }
        }

        async fn get_with_headers(
            &self,
            uri: &str,
            key: Option<&str>,
            extra_headers: &[(&str, &str)],
        ) -> (StatusCode, Value) {
            let mut request = Request::builder().uri(uri);
            if let Some(key) = key {
                request = request.header("x-auth-key", key);
            }
            for (name, value) in extra_headers {
                request = request.header(*name, *value);
            }
            let response = self
                .app
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            let status = response.status();
            let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
            (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
        }

        async fn get(&self, uri: &str, key: Option<&str>) -> (StatusCode, Value) {
            self.get_with_headers(uri, key, &[]).await
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[tokio::test]
    async fn admin_directory_is_authenticated_bounded_filtered_and_secret_free() {
        let fixture = Fixture::new("connection-directory-fixture-key");
        assert!(
            !fixture.mcp_process_marker.exists(),
            "Core startup must not launch the configured stdio source"
        );
        let before = tree_snapshot(&fixture.root);
        let (status, _) = fixture.get("/api/connections", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = fixture
            .get("/api/connections", Some("wrong-directory-key"))
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, first) = fixture
            .get(
                "/api/connections?sourceKind=nexus&limit=1",
                Some("connection-directory-fixture-key"),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{first}");
        assert_eq!(first["total"], 2);
        assert_eq!(first["records"].as_array().unwrap().len(), 1);
        assert_eq!(first["truncated"], true);
        assert_eq!(first["nextOffset"], 1);
        let cursor = first["nextCursor"].as_str().unwrap();
        assert_eq!(cursor.len(), 36);
        assert_eq!(first["managementOnly"], true);
        assert_eq!(first["executionAuthorization"], "unknown");

        let second_uri =
            format!("/api/connections?sourceKind=nexus&limit=1&offset=1&cursor={cursor}");
        let (status, second) = fixture
            .get(&second_uri, Some("connection-directory-fixture-key"))
            .await;
        assert_eq!(status, StatusCode::OK, "{second}");
        assert_eq!(second["records"].as_array().unwrap().len(), 1);
        assert_ne!(
            first["records"][0]["sourceRef"]["sourceId"],
            second["records"][0]["sourceRef"]["sourceId"]
        );
        assert_eq!(second["nextCursor"], Value::Null);

        // A bare offset, changed filters, and replay of a consumed cursor must not
        // produce a page under a different pagination chain.
        let invalid_pages = [
            "/api/connections?sourceKind=nexus&limit=1&offset=1".to_owned(),
            format!("/api/connections?sourceKind=mcp&limit=1&offset=1&cursor={cursor}"),
            format!("/api/connections?sourceKind=nexus&limit=1&offset=0&cursor={cursor}"),
        ];
        for invalid in invalid_pages {
            let (status, _) = fixture
                .get(&invalid, Some("connection-directory-fixture-key"))
                .await;
            assert_eq!(status, StatusCode::CONFLICT, "{invalid}");
        }

        for scope_header in [
            "x-exm-workspace",
            "x-exm-project",
            "x-exm-session",
            "x-exm-actor",
        ] {
            let (status, _) = fixture
                .get_with_headers(
                    "/api/connections",
                    Some("connection-directory-fixture-key"),
                    &[(scope_header, "client-supplied-scope")],
                )
                .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{scope_header}");
        }

        let (status, filtered) = fixture
            .get(
                "/api/connections?sourceKind=nexus&search=remote-two",
                Some("connection-directory-fixture-key"),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{filtered}");
        assert_eq!(filtered["total"], 1);
        assert_eq!(
            filtered["records"][0]["sourceRef"]["sourceId"],
            "remote-two"
        );

        let invalid_queries = [
            "limit=0".to_owned(),
            "limit=201".to_owned(),
            "offset=1000001".to_owned(),
            "unexpected=1".to_owned(),
            "search=%00".to_owned(),
            format!("search={}", "x".repeat(MAX_SEARCH_BYTES + 1)),
        ];
        for invalid in invalid_queries {
            let (status, _) = fixture
                .get(
                    &format!("/api/connections?{invalid}"),
                    Some("connection-directory-fixture-key"),
                )
                .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid}");
        }

        let (_, all) = fixture
            .get(
                "/api/connections?limit=200",
                Some("connection-directory-fixture-key"),
            )
            .await;
        let serialized = serde_json::to_string(&all).unwrap();
        for secret in [
            "directory-canary",
            "directory-api-key-canary",
            "directory-model-api-key-canary",
            "private-command-canary",
        ] {
            assert!(!serialized.contains(secret), "response leaked {secret}");
        }
        assert_eq!(
            tree_snapshot(&fixture.root),
            before,
            "GET mutated local files"
        );
        assert!(
            !fixture.mcp_process_marker.exists(),
            "Directory GET must not launch the configured stdio source"
        );
        assert_eq!(
            fixture.source_listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "Directory GET must not contact Nexus, MCP, or a model endpoint"
        );
    }

    #[tokio::test]
    async fn connection_directory_requires_an_explicit_admin_auth_key() {
        let fixture = Fixture::new("");
        let (status, body) = fixture.get("/api/connections", None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body["error"].as_str().is_some());
    }
}
