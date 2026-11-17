//! Public OpenAPI matches the router, auth policy and model settings payloads.
use axum::{
    body::{to_bytes, Body},
    http::{Method, Request, StatusCode},
    Router,
};
use exm_core::{config::ExmConfig, Core};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tower::ServiceExt;

struct Fixture {
    app: Router,
    root: PathBuf,
    exchanges: Mutex<Vec<Value>>,
}

impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("exm-openapi-contract-{}", uuid::Uuid::new_v4()));
        let mut config = ExmConfig::load(&root);
        config.memory_enabled = false;
        config.security.auth_key = "contract-fixture".into();
        let core = Arc::new(Core::with_config(config).unwrap());
        Self {
            app: exm_gateway::build_router(core),
            root,
            exchanges: Mutex::new(Vec::new()),
        }
    }

    async fn request(
        &self,
        method: Method,
        uri: &str,
        body: Value,
        auth: Option<(&str, &str)>,
    ) -> (StatusCode, Value) {
        let operation = method.as_str().to_lowercase();
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json");
        if let Some((name, value)) = auth {
            request = request.header(name, value);
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 5_000_000).await.unwrap();
        let response: Value = serde_json::from_slice(&bytes).unwrap();
        let path = if uri == "/api/llm/profiles/primary" {
            "/api/llm/profiles/{id}"
        } else {
            uri
        };
        self.exchanges.lock().unwrap().push(json!({
            "path": path, "method": operation, "status": status.as_u16(),
            "request": body, "response": response,
        }));
        (status, response)
    }

    async fn authorized(&self, method: Method, uri: &str, body: Value) -> Value {
        let (status, response) = self
            .request(method, uri, body, Some(("x-auth-key", "contract-fixture")))
            .await;
        assert_eq!(status, StatusCode::OK, "{uri}: {response}");
        response
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn normalized(path: &str) -> String {
    path.split('/')
        .map(|part| {
            part.strip_prefix(':')
                .map(|name| format!("{{{name}}}"))
                .unwrap_or_else(|| part.into())
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[tokio::test]
async fn public_document_uses_standard_paths_and_matches_all_router_operations() {
    let f = Fixture::new();
    let (status, doc) = f
        .request(Method::GET, "/api/openapi.json", Value::Null, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        doc,
        exm_gateway::openapi::generate(),
        "HTTP and offline exports must be identical"
    );
    let actual: BTreeMap<String, BTreeSet<String>> = exm_gateway::openapi::router_paths(&f.app)
        .into_iter()
        .filter(|(path, _)| path.starts_with("/api/"))
        .map(|(path, methods)| {
            (
                normalized(&path),
                methods.into_iter().map(|m| m.to_lowercase()).collect(),
            )
        })
        .collect();
    assert!(
        actual.len() >= 130,
        "Router extraction must not silently produce an empty catalog"
    );
    let mut listed = BTreeMap::new();
    let mut ids = BTreeSet::new();
    for (path, entry) in doc["paths"].as_object().unwrap() {
        assert!(!path.contains(':'));
        let expected: BTreeSet<&str> = path
            .split('/')
            .filter_map(|part| part.strip_prefix('{').and_then(|p| p.strip_suffix('}')))
            .collect();
        let parameters: BTreeSet<&str> = entry["parameters"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|p| {
                assert_eq!(p["in"], "path");
                assert_eq!(p["required"], true);
                assert_eq!(p["schema"]["type"], "string");
                p["name"].as_str().unwrap()
            })
            .collect();
        assert_eq!(parameters, expected, "{path}");
        let mut methods = BTreeSet::new();
        for (method, op) in entry.as_object().unwrap() {
            if method == "parameters" {
                continue;
            }
            assert!(
                ["get", "post", "put", "delete", "patch", "options", "head", "trace"]
                    .contains(&method.as_str())
            );
            assert!(
                ids.insert(op["operationId"].as_str().unwrap()),
                "Operation ids must be unique"
            );
            methods.insert(method.to_string());
        }
        listed.insert(path.clone(), methods);
    }
    assert_eq!(listed, actual);
    fn references_exist(value: &Value, doc: &Value) {
        match value {
            Value::Object(map) => {
                if let Some(Value::String(reference)) = map.get("$ref") {
                    assert!(
                        doc.pointer(reference.strip_prefix('#').unwrap()).is_some(),
                        "Unresolved reference: {reference}"
                    );
                }
                for child in map.values() {
                    references_exist(child, doc);
                }
            }
            Value::Array(values) => {
                for child in values {
                    references_exist(child, doc);
                }
            }
            _ => {}
        }
    }
    references_exist(&doc, &doc);
}

#[tokio::test]
async fn security_requirements_match_public_endpoints_and_both_auth_headers() {
    let f = Fixture::new();
    let doc = exm_gateway::openapi::generate();
    assert_eq!(
        doc["components"]["securitySchemes"]["bearerAuth"]["scheme"],
        "bearer"
    );
    for path in ["/api/health", "/api/version", "/api/llm/profiles"] {
        assert_eq!(
            doc["paths"][path]["get"]["security"],
            json!([{ "authKey": [] }, { "bearerAuth": [] }])
        );
        assert_eq!(
            f.request(Method::GET, path, Value::Null, None).await.0,
            StatusCode::UNAUTHORIZED
        );
        for auth in [
            ("x-auth-key", "contract-fixture"),
            ("authorization", "Bearer contract-fixture"),
        ] {
            assert_eq!(
                f.request(Method::GET, path, Value::Null, Some(auth))
                    .await
                    .0,
                StatusCode::OK
            );
        }
    }
    for (method, path) in [
        (Method::GET, "/api/openapi.json"),
        (Method::GET, "/api/auth/status"),
        (Method::POST, "/api/auth/verify"),
        (Method::POST, "/api/auth/snooze"),
        (Method::POST, "/api/auth/dismiss"),
    ] {
        assert_eq!(
            doc["paths"][path][method.as_str().to_lowercase()]["security"],
            json!([])
        );
    }
    assert_eq!(
        f.request(Method::GET, "/api/auth/status", Value::Null, None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        f.request(
            Method::POST,
            "/api/auth/verify",
            json!({ "key": "contract-fixture" }),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        f.request(Method::POST, "/api/auth/snooze", json!({}), None)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        f.request(Method::POST, "/api/auth/dismiss", json!({}), None)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn published_model_examples_work_with_the_real_handlers_and_mask_keys() {
    let f = Fixture::new();
    let doc = exm_gateway::openapi::generate();
    let request = |path: &str, method: &str| {
        doc["paths"][path][method]["requestBody"]["content"]["application/json"]["example"].clone()
    };
    for (path, entry) in doc["paths"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(path, _)| path.starts_with("/api/llm/"))
    {
        for (method, op) in entry.as_object().unwrap() {
            if method == "parameters" {
                continue;
            }
            assert!(
                op["responses"]["200"]["content"]["application/json"]["schema"]["$ref"].is_string(),
                "Missing response schema: {method} {path}"
            );
            if ["post", "put"].contains(&method.as_str()) {
                assert_eq!(op["requestBody"]["required"], true, "{method} {path}");
                assert!(
                    op["requestBody"]["content"]["application/json"]["schema"]["$ref"].is_string()
                );
            }
        }
    }
    // Save the published example without credentials so probes never contact an external endpoint.
    let input = request("/api/llm/profiles", "post");
    let saved = f
        .authorized(Method::POST, "/api/llm/profiles", input.clone())
        .await;
    assert_eq!(saved["id"], "primary");
    f.authorized(
        Method::PUT,
        "/api/llm/active",
        request("/api/llm/active", "put"),
    )
    .await;
    let profiles = f
        .authorized(Method::GET, "/api/llm/profiles", Value::Null)
        .await;
    let profile = profiles["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "primary")
        .unwrap();
    assert_eq!(profile["clientIdentity"], input["clientIdentity"]);
    assert_eq!(profile["apiKey"], "");
    assert_eq!(profile["models"][0]["contextWindow"], 128000);
    let identity = f
        .authorized(
            Method::POST,
            "/api/llm/identity",
            request("/api/llm/identity", "post"),
        )
        .await;
    assert_eq!(identity["effectiveFormat"], "responses");
    assert_eq!(identity["path"], "/v1/responses");
    assert_eq!(identity["upstreamVerified"], false);
    for path in ["/api/llm/test", "/api/llm/models"] {
        let result = f
            .authorized(Method::POST, path, request(path, "post"))
            .await;
        assert_eq!(result["ok"], false);
        assert_eq!(result["configured"], false);
    }
    let caps = f
        .authorized(
            Method::PUT,
            "/api/llm/capabilities",
            request("/api/llm/capabilities", "put"),
        )
        .await;
    assert_eq!(caps["speech"], "primary/tts-1");
    f.authorized(
        Method::POST,
        "/api/llm/profiles",
        json!({ "id": "primary", "apiKey": "private-fixture-key" }),
    )
    .await;
    let profiles = f
        .authorized(Method::GET, "/api/llm/profiles", Value::Null)
        .await;
    assert!(!profiles.to_string().contains("private-fixture-key"));
    for path in [
        "/api/llm/test",
        "/api/llm/models",
        "/api/llm/active",
        "/api/llm/identity",
    ] {
        let method = if path.ends_with("active") {
            Method::PUT
        } else {
            Method::POST
        };
        let (status, _) = f
            .request(
                method,
                path,
                json!({ "id": "missing" }),
                Some(("x-auth-key", "contract-fixture")),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    let (status, _) = f.request(Method::POST, "/api/llm/profiles", json!({
        "id": "primary", "clientIdentity": { "preset": "custom", "headers": { "authorization": "forbidden" } },
    }), Some(("x-auth-key", "contract-fixture"))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    f.authorized(
        Method::POST,
        "/api/llm/profiles",
        json!({ "id": "primary", "clientIdentity": null }),
    )
    .await;
    let profiles = f
        .authorized(Method::GET, "/api/llm/profiles", Value::Null)
        .await;
    assert_eq!(
        profiles["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "primary")
            .unwrap()["clientIdentity"],
        input["clientIdentity"]
    );
    f.authorized(
        Method::POST,
        "/api/llm/profiles",
        json!({ "id": "primary", "clientIdentity": { "preset": "default" } }),
    )
    .await;
    let profiles = f
        .authorized(Method::GET, "/api/llm/profiles", Value::Null)
        .await;
    assert_eq!(
        profiles["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "primary")
            .unwrap()["clientIdentity"]["preset"],
        "default"
    );
    f.authorized(Method::GET, "/api/llm/capabilities", Value::Null)
        .await;
    f.authorized(Method::GET, "/api/llm/effort", Value::Null)
        .await;
    let old_effort = exm_core::provider::reasoning_effort();
    let old_cap = exm_core::provider::reasoning_token_cap();
    let updated = f
        .authorized(
            Method::PUT,
            "/api/llm/effort",
            request("/api/llm/effort", "put"),
        )
        .await;
    assert_eq!(updated["effectiveCap"], 8192);
    let unchanged = f
        .authorized(
            Method::PUT,
            "/api/llm/effort",
            json!({ "effort": null, "reasoningTokenCap": null }),
        )
        .await;
    assert_eq!(unchanged["effectiveCap"], 8192);
    exm_core::provider::set_reasoning_effort(old_effort.as_deref());
    exm_core::provider::set_reasoning_token_cap(old_cap);
    f.authorized(Method::DELETE, "/api/llm/profiles/primary", Value::Null)
        .await;
    if let Ok(path) = std::env::var("EXM_OPENAPI_CONTRACT_EXCHANGES") {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&*f.exchanges.lock().unwrap()).unwrap(),
        )
        .unwrap();
    }
}
