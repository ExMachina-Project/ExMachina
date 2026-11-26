//! gateway/platform/tests_context_sources —— capability-parity 6.3：
//! REST 面上下文来源治理（GET/POST /api/sessions/:id/context）。
//! ① GET 返回四类来源状态与禁用位（文件/记忆/检索/技能）
//! ② POST disable/enable 持久化 + 单来源独立
//! ③ POST refresh 只读重读（不改变禁用状态）
//! ④ 会话不存在 404；缺 key / 未知 action 400
#[cfg(test)]
mod context_endpoint_tests {

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt; // oneshot
                           // 外层「无真实证据 Core 层拒」等用例直接构造草稿
    use exm_core::learning_candidates::CandidateDraft;

    /// 既有测试用的 Core 夹具（不持有目录守卫——保持原行为）。
    ///
    /// 注意：不要写成 `core_guarded().0`——那会让守卫在表达式结束时立即 drop，
    /// 运行时要落盘的 workspace_root 当场被删。
    fn core() -> exm_core::Core {
        let dir = std::env::temp_dir().join(format!("exm-ctx-api-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        exm_core::Core::with_config(cfg).expect("创建 Core 失败")
    }

    /// 同 [`core`]，但把临时目录交给守卫：持有返回的 `TestDirs` 到测试结束，
    /// 目录在离开作用域时清理（含断言 panic）。本文件新增测试一律用它。
    fn core_guarded() -> (exm_core::Core, crate::platform::TestDirs) {
        let dir = std::env::temp_dir().join(format!("exm-ctx-api-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        let core = exm_core::Core::with_config(cfg).expect("创建 Core 失败");
        (core, crate::platform::TestDirs(vec![dir]))
    }

    fn app(c: exm_core::Core) -> axum::Router {
        crate::build_router(std::sync::Arc::new(c))
    }

    fn app_from_arc(c: std::sync::Arc<exm_core::Core>) -> axum::Router {
        crate::build_router(c)
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// 带项目连接头的 GET 清单：返回 `sources` 数组。
    async fn get_sources(
        app: &axum::Router,
        sid: &str,
        workspace: &std::path::Path,
        key: &str,
    ) -> Vec<serde_json::Value> {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/sessions/{sid}/context"))
                    .header("x-auth-key", key)
                    .header("x-exm-workspace", workspace.to_str().unwrap())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        v["sources"].as_array().cloned().unwrap_or_default()
    }

    /// 无作用域的 GET 清单：返回完整响应体（`sources` 之外还有 `gaps` / `count`）。
    /// 只用于未绑定项目的会话（这类夹具的 Core 不设访问密钥）。
    async fn get_body(app: &axum::Router, sid: &str) -> (StatusCode, serde_json::Value) {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/sessions/{sid}/context"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        (status, body_json(resp).await)
    }

    /// 带项目连接的 GET 清单：返回完整响应体（`sources` 之外还有 `gaps` / `count`）。
    async fn get_body_scoped(
        app: &axum::Router,
        sid: &str,
        workspace: &std::path::Path,
        key: &str,
    ) -> (StatusCode, serde_json::Value) {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/sessions/{sid}/context"))
                    .header("x-auth-key", key)
                    .header("x-exm-workspace", workspace.to_str().unwrap())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        (status, body_json(resp).await)
    }

    async fn post(
        app: &axum::Router,
        sid: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{sid}/context"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        (status, body_json(resp).await)
    }

    #[tokio::test]
    async fn 来源清单_四类在场与状态枚举() {
        let c = core();
        let s = c.create_session("上下文API").unwrap();
        let app = app(c);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/sessions/{}/context", s.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        let sources = v["sources"].as_array().cloned().unwrap_or_default();
        let kinds: Vec<&str> = sources.iter().filter_map(|x| x["kind"].as_str()).collect();
        for kind in ["memory", "retrieval", "skills"] {
            assert!(kinds.contains(&kind), "来源类别 {kind} 在场：{kinds:?}");
        }
        // 状态枚举合法
        for src in &sources {
            let status = src["status"].as_str().unwrap_or_default();
            assert!(
                [
                    "ready",
                    "missing_dependency",
                    "disabled",
                    "loaded",
                    "loaded_truncated",
                    "missing",
                    "rejected",
                    "excluded_sensitive"
                ]
                .contains(&status),
                "状态枚举合法：{src}"
            );
        }
    }

    #[tokio::test]
    async fn 禁用启用_单来源独立且持久() {
        let c = core();
        let s = c.create_session("上下文API2").unwrap();
        let app = app(c);
        // disable memory
        let (st, v) = post(
            &app,
            &s.id,
            serde_json::json!({"action":"disable","key":"memory"}),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["disabled"], true, "{v}");
        let sources = v["sources"].as_array().cloned().unwrap_or_default();
        let memory = sources.iter().find(|x| x["key"] == "memory").unwrap();
        let skills = sources.iter().find(|x| x["key"] == "skills").unwrap();
        assert_eq!(memory["status"], "disabled");
        assert_ne!(skills["status"], "disabled", "其他来源不受影响");
        // 单来源 refresh：只重读该来源，不改变禁用状态（禁用是持久化的叠加位）
        let (st, v) = post(
            &app,
            &s.id,
            serde_json::json!({"action":"refresh","key":"memory"}),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["refreshed"], true, "{v}");
        assert_eq!(v["source"]["status"], "disabled", "refresh 不复活禁用来源");
        assert_eq!(v["source"]["key"], "memory");
        // 其余来源在 others 里原样返回，不含被刷新的那一个
        let others = v["others"].as_array().cloned().unwrap_or_default();
        assert!(others.iter().all(|x| x["key"] != "memory"));
        assert!(others.iter().any(|x| x["key"] == "skills"));
        // 整表 refresh（不带 key）：逐个来源真的刷新（results 每来源一项），仍返回整表 sources；
        // 汇总字段与逐项结果自洽——不硬编码成败数量（用户层文件来源取决于本机 ~/.exmachina）
        let (st, v) = post(&app, &s.id, serde_json::json!({"action":"refresh"})).await;
        assert_eq!(st, StatusCode::OK);
        let sources = v["sources"].as_array().cloned().unwrap_or_default();
        assert!(sources.iter().any(|x| x["key"] == "memory"));
        let results = v["results"].as_array().cloned().unwrap_or_default();
        assert_eq!(results.len(), sources.len(), "每个来源一项：{v}");
        let n_failed = results.iter().filter(|r| r["ok"] == false).count();
        assert_eq!(v["failed"], n_failed, "failed 与逐项结果自洽：{v}");
        assert_eq!(v["refreshed"], n_failed == 0, "有失败就不报整体成功：{v}");
        assert!(
            results.iter().all(|r| r.get("others").is_none()),
            "整表不逐份回传其余来源：{results:?}"
        );
        // enable 恢复
        let (st, v) = post(
            &app,
            &s.id,
            serde_json::json!({"action":"enable","key":"memory"}),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["disabled"], false);
        assert!(
            v["disabledKeys"].as_array().unwrap().is_empty(),
            "启用后清单为空"
        );
        let sources = v["sources"].as_array().cloned().unwrap_or_default();
        let memory = sources.iter().find(|x| x["key"] == "memory").unwrap();
        assert_ne!(memory["status"], "disabled", "启用即恢复");
    }

    /// 带项目连接头的 POST（绑定项目的会话必须经项目连接访问——见 project_scope）。
    async fn post_scoped(
        app: &axum::Router,
        sid: &str,
        workspace: &std::path::Path,
        key: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{sid}/context"))
                    .header("content-type", "application/json")
                    .header("x-auth-key", key)
                    .header("x-exm-workspace", workspace.to_str().unwrap())
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        (status, body_json(resp).await)
    }

    /// 验收句「单来源刷新失败不会改变其他来源」在 REST 面上成立：失败的刷新是
    /// 200 + ok=false（不是请求失败），`others` 与随后 GET 到的其余来源逐项一致，
    /// 禁用清单不变。
    ///
    /// 会话绑定到项目，所以走真实的项目连接（x-auth-key + x-exm-workspace）——
    /// 新的 refresh 路径同样受作用域边界约束：不带项目连接访问绑定会话会被拒。
    #[tokio::test]
    async fn 单来源刷新失败_其余来源不变() {
        let mut cfg0 = {
            let dir = std::env::temp_dir().join(format!("exm-ctx-api-rf-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let mut cfg = exm_core::config::ExmConfig::load(&dir);
            cfg.use_mock = true;
            cfg.memory_enabled = false;
            cfg
        };
        cfg0.security.auth_key = "fixture-context-key".into();
        let ws = std::env::temp_dir().join(format!("exm-xs-rf-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("AGENTS.md"), "规则").unwrap();
        let c = std::sync::Arc::new(exm_core::Core::with_config(cfg0).expect("创建 Core 失败"));
        let s = c
            .store
            .create_session_in(
                "上下文API-rf",
                &c.registry().active_group(),
                Some(ws.to_string_lossy().into_owned()),
            )
            .unwrap();
        c.context_source_set(&s.id, "skills", true).unwrap();
        let app = app_from_arc(c.clone());
        const KEY: &str = "fixture-context-key";

        // 不带项目连接：绑定会话的上下文来源不可达（鉴权/作用域边界对新路径同样
        // 生效）。不断言具体是 401 还是 404——那是中间件顺序的事；要断言的是
        // 「拿不到 200，也拿不到来源数据」
        let (st, v) = post(
            &app,
            &s.id,
            serde_json::json!({"action":"refresh","key":"file:AGENTS.md"}),
        )
        .await;
        assert!(st.is_client_error(), "不带项目连接应被拒，实际 {st}");
        assert!(v.get("source").is_none(), "被拒的请求不得泄露来源数据：{v}");
        assert!(v.get("others").is_none(), "被拒的请求不得泄露来源数据：{v}");

        let before = get_sources(&app, &s.id, &ws, KEY).await;
        // 四类来源都在；文件来源的具体个数取决于本机是否有用户层根（~/.exmachina），不断言数量
        for kind in ["file", "memory", "retrieval", "skills"] {
            assert!(
                before.iter().any(|x| x["kind"] == kind),
                "清单缺 {kind}：{before:?}"
            );
        }
        assert!(before.iter().any(|x| x["key"] == "file:AGENTS.md"));

        // 约定文件位置上放一个目录：读取必然失败
        std::fs::remove_file(ws.join("AGENTS.md")).unwrap();
        std::fs::create_dir(ws.join("AGENTS.md")).unwrap();
        let (st, v) = post_scoped(
            &app,
            &s.id,
            &ws,
            KEY,
            serde_json::json!({"action":"refresh","key":"file:AGENTS.md"}),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "单来源刷新失败不是请求失败");
        assert_eq!(v["ok"], false, "{v}");
        assert_eq!(v["refreshed"], false);
        assert!(
            v["error"].as_str().unwrap_or_default().contains("读取失败"),
            "{v}"
        );
        assert_eq!(v["source"]["status"], "rejected");

        let after = get_sources(&app, &s.id, &ws, KEY).await;
        for o in v["others"].as_array().cloned().unwrap_or_default() {
            let key = o["key"].as_str().unwrap();
            let now = after
                .iter()
                .find(|x| x["key"] == key)
                .expect("其余来源仍在清单里");
            assert_eq!(&o, now, "{key} 被别的来源的刷新失败波及");
        }
        // 禁用清单不变；失败的文件没有被标成禁用
        let skills = after.iter().find(|x| x["key"] == "skills").unwrap();
        assert_eq!(skills["status"], "disabled");
        let agents = after.iter().find(|x| x["key"] == "file:AGENTS.md").unwrap();
        assert_eq!(agents["status"], "rejected");
        assert_eq!(agents["disabled"], false);
        let _ = std::fs::remove_dir_all(ws);
    }

    /// GET 清单附带 `gaps`：整层缺席的文件来源及原因。未绑定的会话清单里没有任何
    /// `file:` 来源——光看清单无从知道为什么，`gaps` 必须给出事实；绑定了项目则
    /// 没有项目层的缺席说明。它是说明不是来源：`sources` / `count` 都不包含它。
    #[tokio::test]
    async fn 清单_整层缺席附带原因() {
        let c = std::sync::Arc::new(core());
        let app = app_from_arc(c.clone());
        // 未绑定会话
        let unbound = c
            .store
            .create_session("未绑定", &c.registry().active_group())
            .unwrap();
        let (st, body) = get_body(&app, &unbound.id).await;
        assert_eq!(st, StatusCode::OK);
        let listed = body["sources"].as_array().cloned().unwrap_or_default();
        assert!(
            listed.iter().all(|s| s["layer"] != "project"),
            "夹具前提：清单里没有项目层来源：{listed:?}"
        );
        let gaps = body["gaps"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| panic!("响应缺 gaps 字段：{body}"));
        let project = gaps
            .iter()
            .find(|g| g["layer"] == "project")
            .unwrap_or_else(|| panic!("缺项目层的缺席说明：{body}"));
        assert_eq!(project["kind"], "file");
        assert!(
            project["reason"].as_str().unwrap().contains("未绑定项目"),
            "{project}"
        );
        assert_eq!(
            body["count"],
            listed.len() as u64,
            "count 只数来源，不数缺席说明：{body}"
        );
        // 缺席说明不混进 sources；与 Core 的事实逐项一致（REST 不自己编原因）
        assert!(
            listed.iter().all(|s| s["key"].is_string()),
            "缺席说明混进了 sources：{listed:?}"
        );
        let expect = serde_json::Value::from(c.context_source_gaps(&unbound.id).unwrap());
        assert_eq!(serde_json::Value::from(gaps), expect);

        // 绑定了项目：项目层在场，没有项目层的缺席说明。
        // 绑定项目的会话必须经项目连接访问（且项目连接要求启用访问密钥）——
        // 用带密钥的项目夹具走合法路径，不在无密钥的 Core 上绕边界。
        let (c, s, ws, app) = scoped_table_fixture("gap-bound").await;
        const KEY: &str = "fixture-context-key";
        let (st, body) = get_body_scoped(&app, &s.id, &ws, KEY).await;
        assert_eq!(st, StatusCode::OK, "经项目连接访问：{body}");
        let gaps = body["gaps"].as_array().cloned().unwrap_or_default();
        assert!(
            gaps.iter().all(|g| g["layer"] != "project"),
            "项目层在场却仍说缺席：{body}"
        );
        assert!(
            body["sources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["key"] == "file:AGENTS.md"),
            "绑定了项目的会话清单里应有项目层来源：{body}"
        );
        let expect = serde_json::Value::from(c.context_source_gaps(&s.id).unwrap());
        assert_eq!(serde_json::Value::from(gaps), expect);
        let _ = std::fs::remove_dir_all(ws);
    }

    /// 整表刷新（不带 key）与 CLI / 通道同一语义：逐个来源真的刷新、逐个留痕；
    /// 一个来源读不出来**不中断其余**——失败只体现在它自己那一项上。
    /// 会话绑定项目，走真实的项目连接；整表路径同样受作用域边界约束。
    #[tokio::test]
    async fn 整表刷新_一个失败不中断其余() {
        let (c, s, ws, app) = scoped_table_fixture("table").await;
        const KEY: &str = "fixture-context-key";
        c.context_source_set(&s.id, "skills", true).unwrap();
        let before = get_sources(&app, &s.id, &ws, KEY).await;
        assert!(
            before
                .iter()
                .any(|x| x["key"] == "file:.exmachina/context.md"),
            "夹具前提：第二个文件来源在场：{before:?}"
        );

        // 不带项目连接：整表刷新同样不可达，且不泄露逐项结果
        let (st, v) = post(&app, &s.id, serde_json::json!({"action":"refresh"})).await;
        assert!(st.is_client_error(), "不带项目连接应被拒，实际 {st}");
        assert!(
            v.get("results").is_none() && v.get("sources").is_none(),
            "被拒的请求不得泄露来源数据：{v}"
        );

        // 约定文件位置上放一个目录：这一个来源读取必然失败
        std::fs::remove_file(ws.join("AGENTS.md")).unwrap();
        std::fs::create_dir(ws.join("AGENTS.md")).unwrap();

        let n_before = refreshed_events(&c, &s.id).len();
        let (st, v) = post_scoped(
            &app,
            &s.id,
            &ws,
            KEY,
            serde_json::json!({"action":"refresh"}),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "整表刷新里有一项失败不是请求失败");
        // 逐项结果：每个来源一项，恰好一项失败，且就是读不出来的那个
        let results = v["results"].as_array().cloned().unwrap_or_default();
        assert_eq!(results.len(), before.len(), "每个来源一项：{results:?}");
        assert_eq!(v["failed"], 1, "{v}");
        assert_eq!(v["refreshed"], false, "有失败就不报整体成功：{v}");
        let by_key = |k: &str| {
            results
                .iter()
                .find(|r| r["key"] == k)
                .cloned()
                .unwrap_or_else(|| panic!("缺 {k}：{results:?}"))
        };
        let agents = by_key("file:AGENTS.md");
        assert_eq!(agents["ok"], false, "{agents}");
        assert!(
            agents["error"]
                .as_str()
                .unwrap_or_default()
                .contains("读取失败"),
            "{agents}"
        );
        assert_eq!(agents["source"]["status"], "rejected");
        let ctx_md = by_key("file:.exmachina/context.md");
        assert_eq!(ctx_md["ok"], true, "另一个文件来源照常刷新成功：{ctx_md}");
        assert_eq!(ctx_md["source"]["status"], "loaded");
        // 已停用的来源：刷新不复活它
        assert_eq!(by_key("skills")["source"]["status"], "disabled");
        assert!(
            results.iter().all(|r| r.get("others").is_none()),
            "{results:?}"
        );
        // 事件：逐个留痕，恰好一条 ok=false
        let batch: Vec<(String, bool)> = refreshed_events(&c, &s.id)
            .into_iter()
            .skip(n_before)
            .collect();
        assert_eq!(batch.len(), before.len(), "{batch:?}");
        assert_eq!(batch.iter().filter(|(_, ok)| !ok).count(), 1, "{batch:?}");
        assert!(
            batch.iter().any(|(k, ok)| k == "file:AGENTS.md" && !ok),
            "{batch:?}"
        );
        // 事实层：其余来源与禁用清单不变
        let after = get_sources(&app, &s.id, &ws, KEY).await;
        for b in &before {
            if b["key"] == "file:AGENTS.md" {
                continue;
            }
            let a = after.iter().find(|x| x["key"] == b["key"]).unwrap();
            assert_eq!(a["status"], b["status"], "{}", b["key"]);
            assert_eq!(a["disabled"], b["disabled"], "{}", b["key"]);
        }
        assert_eq!(
            c.store.disabled_context_sources(&s.id),
            vec!["skills".to_string()]
        );
        let _ = std::fs::remove_dir_all(ws);
    }

    /// 整表刷新的夹具：会话绑定隔离项目，带两个可读的文件来源。
    async fn scoped_table_fixture(
        tag: &str,
    ) -> (
        std::sync::Arc<exm_core::Core>,
        exm_core::types::Session,
        std::path::PathBuf,
        axum::Router,
    ) {
        let mut cfg = {
            let dir =
                std::env::temp_dir().join(format!("exm-ctx-api-{tag}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let mut cfg = exm_core::config::ExmConfig::load(&dir);
            cfg.use_mock = true;
            cfg.memory_enabled = false;
            cfg
        };
        cfg.security.auth_key = "fixture-context-key".into();
        let ws = std::env::temp_dir().join(format!("exm-xs-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(ws.join(".exmachina")).unwrap();
        std::fs::write(ws.join("AGENTS.md"), "规则").unwrap();
        std::fs::write(ws.join(".exmachina").join("context.md"), "补充规则").unwrap();
        let c = std::sync::Arc::new(exm_core::Core::with_config(cfg).expect("创建 Core 失败"));
        let s = c
            .store
            .create_session_in(
                "上下文API-table",
                &c.registry().active_group(),
                Some(ws.to_string_lossy().into_owned()),
            )
            .unwrap();
        let app = app_from_arc(c.clone());
        (c, s, ws, app)
    }

    /// 已落库的 context.source.refreshed 事件：(来源键, 是否成功)
    fn refreshed_events(c: &exm_core::Core, sid: &str) -> Vec<(String, bool)> {
        c.store
            .list_events(sid)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| {
                let p = &e["payload"];
                (p["type"].as_str() == Some("context.source.refreshed")).then(|| {
                    (
                        p["payload"]["key"].as_str().unwrap_or_default().to_string(),
                        p["payload"]["ok"].as_bool().unwrap_or(false),
                    )
                })
            })
            .collect()
    }

    /// 未知来源键 → 400，且不进禁用清单（不回一句「已禁用」却什么都没禁）。
    #[tokio::test]
    async fn 未知key_400且不入清单() {
        let c = std::sync::Arc::new(core());
        let s = c.create_session("上下文API-unknown").unwrap();
        let app = app_from_arc(c.clone());
        let (st, v) = post(
            &app,
            &s.id,
            serde_json::json!({"action":"disable","key":"knowledge"}),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let err = v["error"].as_str().unwrap_or_default();
        assert!(err.contains("未知上下文来源"), "{v}");
        // 这个会话未绑定项目：没有文件来源可停用；报错给出可用键，用户不用猜
        assert!(
            err.contains("memory") && err.contains("retrieval") && err.contains("skills"),
            "{err}"
        );
        let (st, v) = post(
            &app,
            &s.id,
            serde_json::json!({"action":"refresh","key":"nope"}),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(
            v.get("source").is_none() && v.get("others").is_none(),
            "{v}"
        );
        assert!(
            c.store.disabled_context_sources(&s.id).is_empty(),
            "被拒的键不得进入禁用清单"
        );
    }

    #[tokio::test]
    async fn 错误面_缺key未知action与会话不存在() {
        let c = core();
        let s = c.create_session("上下文API3").unwrap();
        let app = app(c);
        // 缺 key
        let (st, v) = post(&app, &s.id, serde_json::json!({"action":"disable"})).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(v["error"].as_str().unwrap_or_default().contains("key"));
        // 未知 action
        let (st, _) = post(
            &app,
            &s.id,
            serde_json::json!({"action":"frob","key":"memory"}),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        // 会话不存在（POST 返回 404——会话校验先于 action 解析）
        let (st, _) = post(
            &app,
            "no-such-session",
            serde_json::json!({"action":"disable","key":"memory"}),
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/sessions/no-such-session/context")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
    /// 真实语义的候选种子：来源会话绑定隔离工作区，本轮暂存里有一条**成功的
    /// filesystem write 回执**指向该文件，证据按工作区相对路径引用——与模型在
    /// 同一轮内「写文件 → 提候选」的实际路径一致（核验要求三者齐备）。
    fn submit_real_candidate(
        c: &exm_core::Core,
        title: &str,
        triggers: &[&str],
    ) -> exm_core::learning_candidates::LearningCandidate {
        use exm_core::learning_candidates::{CandidateDraft, CandidateEvidenceKind};
        let ws = std::env::temp_dir().join(format!("exm-lrn-candidate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("ARTIFACT.md"), "artifact").unwrap();
        let session = c
            .store
            .create_session_in(
                "learn-seed",
                "exmachina",
                Some(ws.to_string_lossy().into_owned()),
            )
            .unwrap();
        exm_core::round_trace::begin(&session.id);
        exm_core::round_trace::push_tool(
            &session.id,
            exm_core::round_trace::ToolCallRecord {
                call_id: "seed-write".into(),
                agent_id: "exmachina".into(),
                tool: "filesystem".into(),
                args: serde_json::json!({ "op": "write", "path": "ARTIFACT.md", "content": "artifact" }),
                ok: true,
                duration_ms: 1,
                summary: String::new(),
            },
        );
        let triggers: Vec<String> = triggers.iter().map(|s| s.to_string()).collect();
        let evidence = vec![(CandidateEvidenceKind::ArtifactFile, "ARTIFACT.md".into())];
        let draft = CandidateDraft {
            target: "skill",
            target_id: Some("learn-seed"),
            title,
            content: "真实产物支撑的经验",
            scope: "global",
            triggers: &triggers,
            source_session: &session.id,
            source_goal: "g",
            evidence: &evidence,
        };
        let cand = c.learning_submit(&draft).expect("真实证据候选过审");
        exm_core::round_trace::end(&session.id);
        let _ = std::fs::remove_dir_all(&ws);
        cand
    }

    #[tokio::test]
    async fn 学习候选_清单与裁决全链路() {
        let (c, _dirs) = core_guarded();
        let cand = submit_real_candidate(&c, "构建走缓存卷", &["构建", "缓存"]);
        let app = app(c);
        // 清单（pending 默认全量可见）
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/learning/candidates")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        let list = v["candidates"].as_array().cloned().unwrap_or_default();
        assert!(list.iter().any(|x| x["id"] == cand.id), "候选在场");
        // approve
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/learning/candidates/{}", cand.id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"action":"approve","decidedBy":"tester"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["status"], "approved");
        // 转正落点与版本号随回执透出（capability-parity 6.6）：与通道回执同一口径
        assert_eq!(
            v["promotion"]["ok"],
            true,
            "转正应成功 —— {}",
            serde_json::to_string(&v).unwrap_or_default()
        );
        assert_eq!(v["promotion"]["version"], 1, "技能转正写入 v1");
        assert_eq!(v["promotion"]["refId"], cand.target_id.clone());
        // 状态过滤
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/learning/candidates?status=approved")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let v = body_json(resp).await;
        let list = v["candidates"].as_array().cloned().unwrap_or_default();
        assert!(list.iter().all(|x| x["status"] == "approved"));
        // 重复裁决被拒
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/learning/candidates/{}", cand.id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"action":"reject"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// capability-parity 6.6：技能目标的转正回执版本号必须取**实际生效**的版本，
    /// 不能写死——同 id 技能已有版本链时，转正是续接而不是 v1。
    /// 期望独立推导：预置技能 v1 → 改写 v2 → 批准转正 = v3（旧版本 v2 归档可回滚）。
    #[tokio::test]
    async fn 学习候选_技能目标转正_回执版本续接既有技能链() {
        let (c, _dirs) = core_guarded();
        let c = std::sync::Arc::new(c);
        let mut skill = exm_core::types::SkillDef {
            id: "learn-seed".into(),
            name: "发布流程".into(),
            instructions: "初版：发布前手工确认".into(),
            ..Default::default()
        };
        let v1 = c.skill_write_versioned(&skill).unwrap();
        skill.instructions = "第二版：发布前跑一遍回归".into();
        let v2 = c.skill_write_versioned(&skill).unwrap();
        assert_eq!((v1.version, v2.version), (1, 2), "前提：技能已有 v1 → v2");

        let cand = submit_real_candidate(&c, "构建走缓存卷", &["构建", "缓存"]);
        let app = app_from_arc(c.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/learning/candidates/{}", cand.id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"action":"approve","decidedBy":"tester"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["promotion"]["ok"], true, "{v}");
        assert_eq!(
            v["promotion"]["version"], 3,
            "转正续接既有技能链应为 v3（不是新技能的 v1）：{v}"
        );
        assert_eq!(v["promotion"]["refId"], "learn-seed");
        // 落点就是那条既有技能：完整链 v1、v2（已归档）、v3（当前，内容为候选内容）
        let hist = c.skill_history_detailed("learn-seed").unwrap();
        let vers: Vec<u64> = hist.iter().map(|r| r.version).collect();
        assert_eq!(vers, vec![1, 2, 3], "链应续接为 v1、v2、v3：{vers:?}");
        assert_eq!(hist[0].instructions, "初版：发布前手工确认");
        assert_eq!(hist[1].instructions, "第二版：发布前跑一遍回归");
        assert_eq!(
            hist[2].instructions, "真实产物支撑的经验",
            "当前行是候选内容"
        );
        let cur = c
            .registry()
            .load_skills()
            .unwrap()
            .into_iter()
            .find(|s| s.id == "learn-seed")
            .expect("技能应仍在清单");
        assert_eq!(cur.version, 3);
        assert_eq!(cur.instructions, "真实产物支撑的经验");
        assert_eq!(cur.source, "learning");
        // 「已生效」事件归**来源会话**（用户在那里批准）：skill.updated 带 v3；不重复进治理流
        let in_source: Vec<serde_json::Value> = c
            .store
            .list_events(&cand.source_session)
            .unwrap_or_default()
            .into_iter()
            .filter(|ev| {
                ev.get("payload")
                    .and_then(|p| p.get("type"))
                    .and_then(|t| t.as_str())
                    == Some("skill.updated")
            })
            .map(|ev| ev["payload"]["payload"].clone())
            .collect();
        assert_eq!(in_source.len(), 1, "来源会话应恰有一条 skill.updated");
        assert_eq!(in_source[0]["id"], "learn-seed");
        assert_eq!(in_source[0]["version"], 3);
        // 治理流里的 skill.updated 应只有前置直写的 v2、v1（新→旧）；转正产生的 v3 不在其中
        let gov_versions: Vec<u64> = c
            .governance_events(200)
            .unwrap()
            .iter()
            .filter(|ev| {
                ev.get("payload")
                    .and_then(|p| p.get("type"))
                    .and_then(|t| t.as_str())
                    == Some("skill.updated")
            })
            .filter_map(|ev| ev["payload"]["payload"]["version"].as_u64())
            .collect();
        assert_eq!(
            gov_versions,
            vec![2, 1],
            "治理流应只有前置直写的 v2、v1；转正的 v3 归来源会话，不进治理流"
        );
    }

    /// capability-parity 6.6：批准 ≠ 生效。内容在批准前被改坏 → REST 回执必须如实：
    /// 200 + status=approved（裁决保留）+ promotion.ok=false + error；落点不得留半成品。
    #[tokio::test]
    async fn 学习候选_转正失败_接口回执如实且裁决保留() {
        let (c, _dirs) = core_guarded();
        let c = std::sync::Arc::new(c);
        let cand = submit_real_candidate(&c, "看起来没问题的经验", &["构建"]);
        // 直接改账本模拟「提交后、批准前被改写」（与 core 层同一手法）
        let mut all = c.store.list_learning_candidates();
        let row = all
            .iter_mut()
            .find(|x| x.id == cand.id)
            .expect("候选应在账本");
        row.content = "部署时用 API_KEY=sk-abcdefghijklmnopqr 直连".into();
        c.store.update_learning_candidate(row).unwrap();

        let app = app_from_arc(c.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/learning/candidates/{}", cand.id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"action":"approve","decidedBy":"tester"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        // 裁决成立（200），不是 400——失败的是转正，不是裁决
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["status"], "approved", "裁决保留：{v}");
        assert_eq!(v["promotion"]["ok"], false, "{v}");
        let err = v["promotion"]["error"].as_str().unwrap_or_default();
        assert!(err.contains("敏感"), "回执带失败原因：{v}");
        assert!(v["promotion"]["version"].is_null(), "失败不得报版本号：{v}");
        // 无半成品：落点技能不存在
        assert!(
            c.registry()
                .load_skills()
                .unwrap()
                .iter()
                .all(|s| s.id != "learn-seed"),
            "失败的转正不得留下半成品"
        );
        // 留痕：来源会话有 promote_failed 事件
        let failed = c
            .store
            .list_events(&cand.source_session)
            .unwrap()
            .iter()
            .filter(|ev| ev["payload"]["type"] == "learning.candidate.promote_failed")
            .count();
        assert_eq!(failed, 1, "应恰有一条 promote_failed 事件");
    }

    #[tokio::test]
    async fn 学习候选_无真实证据拒绝在core层() {
        let c = core();
        // 模型自述（无证据引用）——Core 层红线拒
        let triggers = vec!["发布".to_string()];
        let draft = CandidateDraft {
            target: "skill",
            target_id: Some("learn-seed"),
            title: "发布直推",
            content: "我成功了，直接发布就行",
            scope: "global",
            triggers: &triggers,
            source_session: "seed",
            source_goal: "g",
            evidence: &[],
        };
        let err = c.learning_submit(&draft).unwrap_err().to_string();
        assert!(err.contains("真实证据"), "{err}");
        // 未知 action 400
        let app = app(c);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/learning/candidates/no-such")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::json!({"action":"frob"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// capability-parity 6.5：技能版本治理 REST 面——历史/回滚/不相关不注入。
    mod skill_version_api_tests {
        use super::*;

        fn mk_skill(c: &exm_core::Core, id: &str, instructions: &str) -> exm_core::types::SkillDef {
            let s = exm_core::types::SkillDef {
                id: id.into(),
                name: format!("技能-{id}"),
                description: String::new(),
                triggers: vec!["专属触发词XYZ".into()],
                instructions: instructions.into(),
                agents: vec![],
                created_at: String::new(),
                use_count: 0,
                success_count: 0,
                last_used_at: String::new(),
                source: String::new(),
                version: 0,
                updated_at: String::new(),
            };
            c.skill_write_versioned(&s).expect("版本化写入")
        }

        #[tokio::test]
        async fn 技能版本_历史与回滚全链路() {
            let (c, _dirs) = core_guarded();
            let v1 = mk_skill(&c, "rel", "第一版");
            assert_eq!(v1.version, 1);
            let v2 = mk_skill(&c, "rel", "第二版（带金丝雀）");
            assert_eq!(v2.version, 2);
            let app = app(c);
            // 历史清单
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/api/skills/rel/versions")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let v = body_json(resp).await;
            assert_eq!(v["current"], 2);
            assert_eq!(
                v["versions"].as_array().cloned().unwrap_or_default().len(),
                2
            );
            // 回滚到 v1
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/skills/rel/rollback")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            serde_json::json!({"toVersion": 1, "by": "tester"}).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let v = body_json(resp).await;
            assert_eq!(v["version"], 3, "回滚 = 以旧内容写新版本");
            assert_eq!(v["rolledBackTo"], 1);
            // 不存在的版本 → 400
            let resp = app
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/skills/rel/rollback")
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::json!({"toVersion": 9}).to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        }

        #[test]
        fn 相关性选择_不触发不注入() {
            // SkillDef::matches 语义锚定：触发词不命中 = 不进入注入（即使目录可见）
            let s = exm_core::types::SkillDef {
                id: "rel".into(),
                name: "技能".into(),
                description: String::new(),
                triggers: vec!["专属触发词XYZ".into()],
                instructions: "内容".into(),
                agents: vec![],
                created_at: String::new(),
                use_count: 0,
                success_count: 0,
                last_used_at: String::new(),
                source: String::new(),
                version: 1,
                updated_at: String::new(),
            };
            assert!(!s.matches("完全无关的任务目标"), "不相关技能不注入");
            assert!(s.matches("任务里出现专属触发词XYZ时命中"), "命中即注入");
        }

        /// GET /api/skills/deleted + 版本行的完整定义（capability-parity 6.6）：
        /// 技能删除后从列表消失，「回滚恢复」入口只能靠这个清单找到目标；
        /// 版本行带 instructions——用户得看得出「这一版写了什么」才谈得上选回滚目标。
        #[tokio::test]
        async fn 技能版本_已删除清单_与版本行带完整指令() {
            // Core 不是 Clone：Router 与断言共用同一个 Arc
            let (c, _dirs) = core_guarded();
            let c = std::sync::Arc::new(c);
            let get_deleted = |app: axum::Router| async move {
                let resp = app
                    .oneshot(
                        Request::builder()
                            .uri("/api/skills/deleted")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                (resp.status(), body_json(resp).await)
            };

            // 空库：200 + 空数组（200 同时证明这条静态路径没被 /api/skills/:id 吞掉）
            let (status, v) = get_deleted(app_from_arc(c.clone())).await;
            assert_eq!(status, StatusCode::OK, "{v}");
            assert_eq!(v["skills"].as_array().map(Vec::len), Some(0), "{v}");

            mk_skill(&c, "rel", "第一版");
            mk_skill(&c, "rel", "第二版");
            // 未删除：不在清单里
            let (status, v) = get_deleted(app_from_arc(c.clone())).await;
            assert_eq!(status, StatusCode::OK, "{v}");
            assert_eq!(
                v["skills"].as_array().map(Vec::len),
                Some(0),
                "未删除不得出现：{v}"
            );

            // 经 REST 删除 → 出现在清单，带删除时间（走真实入口，而非直接调 Core）
            let resp = app_from_arc(c.clone())
                .oneshot(
                    Request::builder()
                        .method("DELETE")
                        .uri("/api/skills/rel")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let (status, v) = get_deleted(app_from_arc(c.clone())).await;
            assert_eq!(status, StatusCode::OK, "{v}");
            let rows = v["skills"].as_array().expect("skills 为数组");
            assert_eq!(rows.len(), 1, "删除后出现在清单：{v}");
            assert_eq!(rows[0]["id"], "rel");
            assert!(rows[0]["deletedAt"].is_string(), "{v}");

            // 已删除技能的版本历史仍可读：current=null、deleted=true，且每行带完整指令
            let resp = app_from_arc(c.clone())
                .oneshot(
                    Request::builder()
                        .uri("/api/skills/rel/versions")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let v = body_json(resp).await;
            assert_eq!(
                v["current"],
                serde_json::Value::Null,
                "已删除：没有当前生效版本"
            );
            assert_eq!(v["deleted"], true);
            let versions = v["versions"].as_array().unwrap();
            let bodies: Vec<&str> = versions
                .iter()
                .map(|r| r["instructions"].as_str().unwrap_or(""))
                .collect();
            assert_eq!(
                bodies,
                vec!["第一版", "第二版"],
                "每行带该版本写时的完整指令"
            );

            // 回滚 = 恢复：从已删除清单消失，回到常规列表
            let resp = app_from_arc(c.clone())
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/skills/rel/rollback")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            serde_json::json!({ "toVersion": 1 }).to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let v = body_json(resp).await;
            assert_eq!(v["version"], 3, "回滚 = 以 v1 内容写新版本行：{v}");
            let (status, v) = get_deleted(app_from_arc(c.clone())).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                v["skills"].as_array().map(Vec::len),
                Some(0),
                "恢复后从清单消失：{v}"
            );
            let live = c.skills().unwrap();
            assert!(
                live.iter().any(|s| s.id == "rel"),
                "恢复后回到常规列表：{live:?}"
            );
        }
    }

    /// capability-parity 6.6 跨入口验收：**每一步都走真实入口**（HTTP 路由 / Core 公开
    /// 治理面），不直接调 registry 内部方法。学习闭环不重新实现既有存储——REST 创建、
    /// REST 导入、CLI 共用的导入、聊天候选批准转正，四条写路径落在同一条版本链上；
    /// 版本历史、回滚、删除在三处读到的都是同一事实源。
    mod cross_entry_tests {
        use super::*;

        /// POST/DELETE 任意路径，返回 (status, json)
        async fn call(
            app: &axum::Router,
            method: &str,
            uri: &str,
            body: Option<serde_json::Value>,
        ) -> (StatusCode, serde_json::Value) {
            let req = Request::builder().method(method).uri(uri);
            let req = match body {
                Some(b) => req
                    .header("content-type", "application/json")
                    .body(Body::from(b.to_string()))
                    .unwrap(),
                None => req.body(Body::empty()).unwrap(),
            };
            let resp = app.clone().oneshot(req).await.unwrap();
            let status = resp.status();
            (status, body_json(resp).await)
        }

        async fn versions(app: &axum::Router, id: &str) -> serde_json::Value {
            let (st, v) = call(app, "GET", &format!("/api/skills/{id}/versions"), None).await;
            assert_eq!(st, StatusCode::OK, "读取版本历史失败：{v}");
            v
        }

        /// 当前生效版本（Core 面 = CLI 直连面）
        fn current(c: &exm_core::Core, id: &str) -> exm_core::types::SkillDef {
            c.skills()
                .unwrap()
                .into_iter()
                .find(|s| s.id == id)
                .unwrap_or_else(|| panic!("技能 {id} 应存在"))
        }

        const MD_V2: &str = "---\nid: xe\nname: 跨入口技能\ndescription: 导入的第二版\ntriggers: [跨入口触发]\n---\nv2 内容（导入）\n";
        const MD_V3: &str = "---\nid: xe\nname: 跨入口技能\ndescription: 导入的第三版\ntriggers: [跨入口触发]\n---\nv3 内容（CLI 导入）\n";

        #[tokio::test]
        async fn 四条写入口_同一版本链_经真实路由() {
            let c = std::sync::Arc::new(core());
            let app = app_from_arc(std::sync::Arc::clone(&c));

            // ① REST 创建（POST /api/skills）——此前走未版本化 write_skill，版本号被重置为 0
            let (st, v) = call(
                &app,
                "POST",
                "/api/skills",
                Some(serde_json::json!({
                    "id": "xe", "name": "跨入口技能", "triggers": ["跨入口触发"],
                    "instructions": "v1 内容（REST 创建）"
                })),
            )
            .await;
            assert_eq!(st, StatusCode::OK, "{v}");
            assert_eq!(
                v["version"], 1,
                "REST 响应须返回**落盘**的版本号（不是入参回显的 0）：{v}"
            );

            // ② REST 导入（POST /api/skills/import）同 id → v2，v1 归档
            let (st, v) = call(
                &app,
                "POST",
                "/api/skills/import",
                Some(serde_json::json!({ "content": MD_V2 })),
            )
            .await;
            assert_eq!(st, StatusCode::OK, "{v}");
            assert_eq!(v["version"], 2, "REST 导入应续接版本链：{v}");

            // ③ CLI `skill import` 共用的 Core 面（import_skill_md）→ v3
            let imported = c.import_skill_md(MD_V3).expect("CLI 导入应成功");
            assert_eq!(imported.version, 3, "CLI 导入应续接 REST 写出的版本链");

            // ④ 聊天 /learn 批准 → 转正（update 候选）→ v4，事件归来源会话
            let cand = submit_real_candidate_update(&c, "xe", "跨入口技能", "v4 内容（学习转正）");
            let approved = c.learning_decide(&cand.id, true, "owner", None).unwrap();
            let promo = approved.promotion.as_ref().unwrap();
            assert!(promo.ok, "转正应成功：{:?}", promo.error);
            assert_eq!(promo.version, Some(4), "转正须落在同一条版本链上");

            // 同一条版本链：REST 读到 4 个版本 + current=4；Core 面同为 v4
            let hist = versions(&app, "xe").await;
            let nums: Vec<u64> = hist["versions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x["version"].as_u64().unwrap())
                .collect();
            assert_eq!(nums, vec![1, 2, 3, 4], "四条写入口各产生一个版本：{hist}");
            assert_eq!(hist["current"], 4);
            let cur = current(&c, "xe");
            assert_eq!(cur.version, 4);
            assert_eq!(cur.instructions, "v4 内容（学习转正）");

            // ⑤ REST 回滚到「REST 创建」的 v1 → v5（新版本行，审计链不断）
            let (st, v) = call(
                &app,
                "POST",
                "/api/skills/xe/rollback",
                Some(serde_json::json!({ "toVersion": 1, "by": "rest" })),
            )
            .await;
            assert_eq!(st, StatusCode::OK, "{v}");
            assert_eq!(v["version"], 5);
            assert_eq!(current(&c, "xe").instructions, "v1 内容（REST 创建）");

            // ⑥ REST 删除：停止加载、历史保留可读、治理事件可回读
            let (st, _) = call(&app, "DELETE", "/api/skills/xe", None).await;
            assert_eq!(st, StatusCode::OK);
            assert!(
                c.skills().unwrap().iter().all(|s| s.id != "xe"),
                "删除后不得再注入"
            );
            let hist = versions(&app, "xe").await;
            let nums: Vec<u64> = hist["versions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x["version"].as_u64().unwrap())
                .collect();
            assert_eq!(nums, vec![1, 2, 3, 4, 5], "删除后历史仍可审计：{hist}");
            assert_eq!(hist["deleted"], true, "删除状态须对客户端可见：{hist}");
            assert!(
                hist["current"].is_null(),
                "已删除技能没有「当前生效版本」，不得报成最后一个归档版本：{hist}"
            );
            let gov = c.governance_events(100).unwrap();
            let kinds: Vec<&str> = gov
                .iter()
                .filter_map(|e| e["payload"]["type"].as_str())
                .collect();
            assert!(
                kinds.contains(&"skill.deleted"),
                "删除须留治理事件：{kinds:?}"
            );
            assert!(kinds.contains(&"skill.rolled_back"), "{kinds:?}");
            // REST 回滚后新版本写入也经版本化路径 → 治理流有 skill.updated
            assert!(kinds.contains(&"skill.updated"), "{kinds:?}");
        }

        /// 提交一个「修正既有技能」的候选（带真实证据），用于转正测试。
        fn submit_real_candidate_update(
            c: &exm_core::Core,
            skill_id: &str,
            title: &str,
            content: &str,
        ) -> exm_core::learning_candidates::LearningCandidate {
            use exm_core::learning_candidates::{CandidateDraft, CandidateEvidenceKind};
            let ws = std::env::temp_dir().join(format!("exm-xe-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&ws).unwrap();
            std::fs::write(ws.join("ARTIFACT.md"), "x").unwrap();
            let session = c
                .store
                .create_session_in(
                    "learn",
                    &c.registry().active_group(),
                    Some(ws.to_string_lossy().into_owned()),
                )
                .unwrap();
            exm_core::round_trace::begin(&session.id);
            exm_core::round_trace::push_tool(
                &session.id,
                exm_core::round_trace::ToolCallRecord {
                    call_id: "w1".into(),
                    agent_id: "exmachina".into(),
                    tool: "filesystem".into(),
                    args: serde_json::json!({ "op": "write", "path": "ARTIFACT.md", "content": "x" }),
                    ok: true,
                    duration_ms: 1,
                    summary: String::new(),
                },
            );
            let triggers = vec!["跨入口触发".to_string()];
            let evidence = vec![(CandidateEvidenceKind::ArtifactFile, "ARTIFACT.md".into())];
            let draft = CandidateDraft {
                target: "skill",
                target_id: Some(skill_id),
                title,
                content,
                scope: "global",
                triggers: &triggers,
                source_session: &session.id,
                source_goal: "g",
                evidence: &evidence,
            };
            let cand = c.learning_submit(&draft).expect("真实证据候选应过审");
            exm_core::round_trace::end(&session.id);
            cand
        }

        /// 提交带真实证据、但正文为敏感内容的候选——返回 Result 以便断言被拒。
        fn submit_sensitive_candidate(
            c: &exm_core::Core,
            skill_id: &str,
            content: &str,
        ) -> anyhow::Result<exm_core::learning_candidates::LearningCandidate> {
            use exm_core::learning_candidates::{CandidateDraft, CandidateEvidenceKind};
            let ws = std::env::temp_dir().join(format!("exm-xe-s-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&ws).unwrap();
            let session = c.store.create_session_in(
                "learn",
                &c.registry().active_group(),
                Some(ws.to_string_lossy().into_owned()),
            )?;
            exm_core::round_trace::begin(&session.id);
            exm_core::round_trace::push_tool(
                &session.id,
                exm_core::round_trace::ToolCallRecord {
                    call_id: "w1".into(),
                    agent_id: "exmachina".into(),
                    tool: "filesystem".into(),
                    args: serde_json::json!({ "op": "write", "path": "A.md", "content": "x" }),
                    ok: true,
                    duration_ms: 1,
                    summary: String::new(),
                },
            );
            let triggers = vec!["触发".to_string()];
            let evidence = vec![(CandidateEvidenceKind::ArtifactFile, "A.md".into())];
            let draft = CandidateDraft {
                target: "skill",
                target_id: Some(skill_id),
                title: "受保护技能",
                content,
                scope: "global",
                triggers: &triggers,
                source_session: &session.id,
                source_goal: "g",
                evidence: &evidence,
            };
            let out = c.learning_submit(&draft);
            exm_core::round_trace::end(&session.id);
            out
        }

        #[tokio::test]
        async fn 校验不过_各入口一致拒绝且版本链不动() {
            let c = std::sync::Arc::new(core());
            let app = app_from_arc(std::sync::Arc::clone(&c));
            call(
                &app,
                "POST",
                "/api/skills",
                Some(serde_json::json!({
                    "id": "guarded", "name": "受保护技能", "triggers": ["触发"],
                    "instructions": "原内容"
                })),
            )
            .await;
            // REST：敏感内容 → 400
            let (st, v) = call(
                &app,
                "POST",
                "/api/skills",
                Some(serde_json::json!({
                    "id": "guarded", "name": "受保护技能", "triggers": ["触发"],
                    "instructions": "部署用 API_KEY=sk-abcdefghijklmnopqrst"
                })),
            )
            .await;
            assert_eq!(st, StatusCode::BAD_REQUEST, "敏感内容应被 REST 拒绝：{v}");
            // REST 导入：格式错误 → 400
            let (st, _) = call(
                &app,
                "POST",
                "/api/skills/import",
                Some(serde_json::json!({ "content": "没有 frontmatter 的文本" })),
            )
            .await;
            assert_eq!(st, StatusCode::BAD_REQUEST);
            // CLI 导入面：同样的校验
            assert!(c
                .import_skill_md("---\nid: guarded\nname: x\n---\npassword: hunter2\n")
                .is_err());
            // 候选面：敏感内容在**提交时**即被拒（第一道闸），到不了批准/转正——
            // 「提交后、批准前被改坏」的转正防线见 workflow::promotion_tests::转正失败_裁决保留_失败留痕
            let err = submit_sensitive_candidate(&c, "guarded", "token=abcdefghijklmnop")
                .expect_err("敏感内容候选应在提交时被拒");
            assert!(err.to_string().contains("敏感"), "{err}");
            // 版本链未动
            assert_eq!(current(&c, "guarded").instructions, "原内容");
            let hist = versions(&app, "guarded").await;
            assert_eq!(hist["current"], 1, "被拒的更新不得产生版本：{hist}");
        }
        /// 记忆版本链 REST（capability-parity 6.5）——独立夹具：深层记忆开启。
        /// 期望值独立推算：created=v1 → 编辑=v2 → 回滚到 v1 = v3 → 遗忘行 v4（current=null）
        /// → 撤销遗忘 = v5。
        mod memory_version_api {
            use axum::body::Body;
            use axum::http::{Request, StatusCode};
            use tower::ServiceExt;

            /// 同一个 Arc 既给 Router 又留给测试读治理事件（Core 不是 Clone）。
            /// 第三个返回值是临时目录守卫——持有到测试结束即自动清理。
            fn mem_app() -> (
                std::sync::Arc<exm_core::Core>,
                axum::Router,
                crate::platform::TestDirs,
            ) {
                let dir =
                    std::env::temp_dir().join(format!("exm-memver-api-{}", uuid::Uuid::new_v4()));
                std::fs::create_dir_all(&dir).unwrap();
                let mut cfg = exm_core::config::ExmConfig::load(&dir);
                cfg.use_mock = true;
                cfg.memory_enabled = true;
                let c =
                    std::sync::Arc::new(exm_core::Core::with_config(cfg).expect("创建 Core 失败"));
                let app = crate::build_router(c.clone());
                (c, app, crate::platform::TestDirs(vec![dir]))
            }

            /// 治理流里属于该条目的事件：(类型, 版本号)。
            fn governance_of(c: &exm_core::Core, id: &str) -> Vec<(String, u64)> {
                c.governance_events(100)
                    .unwrap()
                    .into_iter()
                    .filter_map(|ev| {
                        let p = ev.get("payload")?.as_object()?.clone();
                        let inner = p.get("payload")?.as_object()?.clone();
                        if inner.get("id")?.as_str()? != id {
                            return None;
                        }
                        let kind = p.get("type")?.as_str()?.to_string();
                        let version = inner
                            .get("version")
                            .or_else(|| inner.get("newVersion"))?
                            .as_u64()?;
                        Some((kind, version))
                    })
                    .collect()
            }

            async fn body_json(resp: axum::response::Response) -> serde_json::Value {
                let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                    .await
                    .unwrap();
                serde_json::from_slice(&bytes).unwrap()
            }

            async fn post_json(
                app: axum::Router,
                uri: &str,
                body: serde_json::Value,
            ) -> (StatusCode, serde_json::Value) {
                let resp = app
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri(uri)
                            .header("content-type", "application/json")
                            .body(Body::from(body.to_string()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let status = resp.status();
                (status, body_json(resp).await)
            }

            /// 发 PUT /api/memory/:id（面板编辑走的同一端点）。
            async fn put_edit(
                app: axum::Router,
                id: &str,
                body: &str,
            ) -> (StatusCode, serde_json::Value) {
                let resp = app
                    .oneshot(
                        Request::builder()
                            .method("PUT")
                            .uri(format!("/api/memory/{id}"))
                            .header("content-type", "application/json")
                            .body(Body::from(serde_json::json!({ "body": body }).to_string()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let status = resp.status();
                (status, body_json(resp).await)
            }

            #[tokio::test]
            async fn 记忆版本_编辑回滚遗忘撤销_全链路() {
                let (c, app, _dirs) = mem_app();
                let e1 = c
                    .memory_remember(exm_core::memory::MemoryDraft::new(
                        exm_core::memory::MemoryKind::Fact,
                        "部署步骤",
                        "第一版正文",
                    ))
                    .unwrap();
                assert_eq!(e1.version_label(), 1);

                // 编辑 → v2，回执带版本号
                let (status, v) = put_edit(app.clone(), &e1.id, "第二版正文").await;
                assert_eq!(status, StatusCode::OK);
                assert_eq!(v["version"], 2, "编辑回执带新版本号");

                // 历史清单：两行，current=2
                let resp = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .uri(format!("/api/memory/{}/versions", e1.id))
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(resp.status(), StatusCode::OK);
                let v = body_json(resp).await;
                assert_eq!(v["current"], 2);
                assert_eq!(v["forgotten"], false);
                let rows = v["versions"].as_array().cloned().unwrap_or_default();
                assert_eq!(rows.len(), 2);
                assert_eq!(
                    (rows[0]["version"].clone(), rows[0]["change"].clone()),
                    (serde_json::json!(1), serde_json::json!("created"))
                );
                assert_eq!(
                    (rows[1]["version"].clone(), rows[1]["change"].clone()),
                    (serde_json::json!(2), serde_json::json!("updated"))
                );
                // 每行带该版本写时的正文——治理面要能看出「这一版写了什么」才谈得上选回滚目标
                assert_eq!(rows[0]["body"], "第一版正文", "v1 行内存 v1 的正文");
                assert_eq!(rows[1]["body"], "第二版正文", "v2 行内存 v2 的正文");
                assert_eq!(rows[1]["title"], rows[0]["title"], "只改正文时标题不变");
                assert!(rows[0]["changedAt"].is_string() && rows[0]["kind"].is_string());

                // 回滚到 v1 → v3（内容恢复为第一版）
                let (status, v) = post_json(
                    app.clone(),
                    &format!("/api/memory/{}/rollback", e1.id),
                    serde_json::json!({ "toVersion": 1, "by": "tester" }),
                )
                .await;
                assert_eq!(status, StatusCode::OK);
                assert_eq!(v["version"], 3, "回滚 = 以旧内容写新版本");
                assert_eq!(v["rolledBackTo"], 1);

                // 遗忘 → current=null、forgotten=true、历史多一行 forgotten(v4)
                let resp = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method("DELETE")
                            .uri(format!("/api/memory/{}", e1.id))
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(resp.status(), StatusCode::OK);
                let resp = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .uri(format!("/api/memory/{}/versions", e1.id))
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(resp.status(), StatusCode::OK, "已遗忘的条目历史仍可读");
                let v = body_json(resp).await;
                assert_eq!(
                    v["current"],
                    serde_json::Value::Null,
                    "已遗忘：没有生效版本"
                );
                assert_eq!(v["forgotten"], true);
                let rows = v["versions"].as_array().cloned().unwrap_or_default();
                assert_eq!(rows.len(), 4);
                assert_eq!(
                    (rows[3]["version"].clone(), rows[3]["change"].clone()),
                    (serde_json::json!(4), serde_json::json!("forgotten"))
                );

                // 撤销遗忘 → v5
                let (status, v) = post_json(
                    app.clone(),
                    &format!("/api/memory/{}/restore", e1.id),
                    serde_json::json!({ "by": "tester" }),
                )
                .await;
                assert_eq!(status, StatusCode::OK);
                assert_eq!(v["version"], 5);
                assert_eq!(v["title"], "部署步骤");

                // 治理事件留痕：updated / rolled_back / forgotten / restored 四类都在治理流。
                // governance_events 是「最新在前」，所以按时间正序读要 reverse。
                let mut events = governance_of(&c, &e1.id);
                events.reverse();
                let kinds: Vec<&str> = events.iter().map(|(k, _)| k.as_str()).collect();
                assert_eq!(
                    kinds,
                    vec![
                        "memory.updated",
                        "memory.rolled_back",
                        "memory.forgotten",
                        "memory.restored"
                    ],
                    "治理操作走同一版本事实源并留痕"
                );
                let versions: Vec<u64> = events.iter().map(|(_, v)| *v).collect();
                assert_eq!(versions, vec![2, 3, 3, 5], "事件里带的是实际生效的版本号");
            }

            #[tokio::test]
            async fn 记忆版本_错误面_不存在与回滚无效版本() {
                let (_c, app, _dirs) = mem_app();
                // 从未存在的 id：历史 404、撤销 400
                let resp = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .uri("/api/memory/never-existed/versions")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(resp.status(), StatusCode::NOT_FOUND);
                let (status, v) = post_json(
                    app.clone(),
                    "/api/memory/never-existed/rollback",
                    serde_json::json!({ "toVersion": 1 }),
                )
                .await;
                assert_eq!(status, StatusCode::BAD_REQUEST);
                assert!(v["error"].as_str().unwrap_or("").contains("不存在"), "{v}");
                let (status, _) = post_json(
                    app,
                    "/api/memory/never-existed/restore",
                    serde_json::json!({}),
                )
                .await;
                assert_eq!(status, StatusCode::BAD_REQUEST);
            }

            /// GET /api/memory/forgotten（capability-parity 6.6）：撤销遗忘的入口数据源。
            /// 遗忘后条目从常规列表消失，治理面只能靠它找到「可以撤销哪些」。
            /// 首个断言（200 OK）同时确认这条静态路径能路由到——没被 `/api/memory/:id` 吞掉。
            #[tokio::test]
            async fn 已遗忘清单_空库为空_随遗忘出现_撤销后消失() {
                let (c, app, _dirs) = mem_app();
                let get_forgotten = |app: axum::Router| async move {
                    let resp = app
                        .oneshot(
                            Request::builder()
                                .uri("/api/memory/forgotten")
                                .body(Body::empty())
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    let status = resp.status();
                    (status, body_json(resp).await)
                };

                // 空库：200 + 空数组（不是 404——这条路径是集合，不是某个 id）
                let (status, v) = get_forgotten(app.clone()).await;
                assert_eq!(status, StatusCode::OK, "{v}");
                assert_eq!(v["entries"].as_array().map(Vec::len), Some(0), "{v}");

                let e = c
                    .memory_remember(exm_core::memory::MemoryDraft::new(
                        exm_core::memory::MemoryKind::Lesson,
                        "清单端点用例",
                        "遗忘后要能在这里找到并撤销。",
                    ))
                    .unwrap();
                // 未遗忘：仍不在清单里
                let (_, v) = get_forgotten(app.clone()).await;
                assert_eq!(
                    v["entries"].as_array().map(Vec::len),
                    Some(0),
                    "未遗忘不得出现：{v}"
                );

                c.memory_forget(&e.id).unwrap();
                let (status, v) = get_forgotten(app.clone()).await;
                assert_eq!(status, StatusCode::OK, "{v}");
                let rows = v["entries"].as_array().expect("entries 为数组");
                assert_eq!(rows.len(), 1, "遗忘后出现在清单：{v}");
                // 返回的是遗忘当时的完整条目（camelCase，与 MemoryEntry 契约一致）——
                // 前端拿它直接渲染标题/归属，不必再查一次
                assert_eq!(rows[0]["id"], e.id.as_str());
                assert_eq!(rows[0]["title"], "清单端点用例");
                assert_eq!(rows[0]["body"], "遗忘后要能在这里找到并撤销。");
                assert!(
                    rows[0]["agentId"].is_string() || rows[0]["agentId"].is_null(),
                    "{v}"
                );
                assert!(rows[0]["version"].is_u64(), "条目带版本号：{v}");

                // 撤销遗忘后从清单消失
                c.memory_restore(&e.id, "user-1").unwrap();
                let (_, v) = get_forgotten(app).await;
                assert_eq!(
                    v["entries"].as_array().map(Vec::len),
                    Some(0),
                    "撤销后应消失：{v}"
                );
            }

            /// 提交一条**记忆目标**、带真实证据的候选（走 Core 的 learning_submit，
            /// 与 skill_manage / REST 同一入口）。记忆候选不需要 target_id。
            fn submit_memory_candidate(
                c: &exm_core::Core,
                title: &str,
                body: &str,
            ) -> exm_core::learning_candidates::LearningCandidate {
                use exm_core::learning_candidates::{CandidateDraft, CandidateEvidenceKind};
                let ws = std::env::temp_dir().join(format!("exm-lrn-mem-{}", uuid::Uuid::new_v4()));
                std::fs::create_dir_all(&ws).unwrap();
                std::fs::write(ws.join("ARTIFACT.md"), "artifact").unwrap();
                let session = c
                    .store
                    .create_session_in(
                        "learn-mem",
                        "exmachina",
                        Some(ws.to_string_lossy().into_owned()),
                    )
                    .unwrap();
                exm_core::round_trace::begin(&session.id);
                exm_core::round_trace::push_tool(
                    &session.id,
                    exm_core::round_trace::ToolCallRecord {
                        call_id: "seed-write".into(),
                        agent_id: "exmachina".into(),
                        tool: "filesystem".into(),
                        args: serde_json::json!({
                            "op": "write", "path": "ARTIFACT.md", "content": "artifact"
                        }),
                        ok: true,
                        duration_ms: 1,
                        summary: String::new(),
                    },
                );
                let triggers = vec!["发布窗口".to_string()];
                let evidence = vec![(CandidateEvidenceKind::ArtifactFile, "ARTIFACT.md".into())];
                let draft = CandidateDraft {
                    target: "memory",
                    target_id: None,
                    title,
                    content: body,
                    scope: "global",
                    triggers: &triggers,
                    source_session: &session.id,
                    source_goal: "g",
                    evidence: &evidence,
                };
                let cand = c.learning_submit(&draft).expect("真实证据的记忆候选应过审");
                exm_core::round_trace::end(&session.id);
                let _ = std::fs::remove_dir_all(&ws);
                cand
            }

            async fn approve(app: axum::Router, id: &str) -> (StatusCode, serde_json::Value) {
                let resp = app
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri(format!("/api/learning/candidates/{id}"))
                            .header("content-type", "application/json")
                            .body(Body::from(
                                serde_json::json!({"action":"approve","decidedBy":"user-1"})
                                    .to_string(),
                            ))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let status = resp.status();
                (status, body_json(resp).await)
            }

            /// capability-parity 6.6：记忆目标的候选经 REST 批准后转正进**既有记忆存储**
            /// （不另建存储），进的是记忆版本链，回执带实际生效的版本号。
            ///
            /// 期望独立推导：
            /// - 新内容 → 记忆版本链恰一行 v1(created)，promotion.version=1，refId=该条 id，
            ///   来源会话留一条 learning.candidate.promoted（target=memory，带 refId/version）；
            /// - 同内容已存在（先直接写入同标题同正文的全局条目）→ 记忆去重命中已有条目：
            ///   refId 指向已有条目、版本如实取它，**不新增版本行**（不假设晋升一定产生 v1）。
            #[tokio::test]
            async fn 学习候选_记忆目标转正_进既有记忆版本链() {
                // ---------- 新内容 ----------
                let (c, app, _dirs) = mem_app();
                let fresh =
                    submit_memory_candidate(&c, "记忆转正新内容", "新内容：只在维护窗口内发布");
                let (status, v) = approve(app.clone(), &fresh.id).await;
                assert_eq!(status, StatusCode::OK, "{v}");
                assert_eq!(v["status"], "approved", "{v}");
                assert_eq!(v["promotion"]["ok"], true, "{v}");
                let ref_id = v["promotion"]["refId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                assert!(!ref_id.is_empty(), "回执应带落点 id：{v}");
                assert_eq!(v["promotion"]["version"], 1, "新内容转正写入记忆 v1：{v}");
                // 落点是记忆条目（既有存储），不是技能
                let hist = c.memory_history(&ref_id).expect("落点应是记忆条目 id");
                assert_eq!(
                    hist.iter()
                        .map(|r| (r.version, r.change.as_str()))
                        .collect::<Vec<_>>(),
                    vec![(1, "created")],
                    "记忆版本链应只有一行 created"
                );
                assert_eq!(hist[0].title, "记忆转正新内容");
                assert_eq!(hist[0].body, "新内容：只在维护窗口内发布");
                // 追溯线索：这条记忆是从哪次候选转正来的（审计链，不是仅靠事件）
                let entry = c
                    .memory_get_entry(&ref_id)
                    .unwrap()
                    .expect("落点应是现存记忆条目");
                assert_eq!(
                    entry.source_ref.as_deref(),
                    Some(format!("learning-candidate:{}", fresh.id).as_str()),
                    "转正写入的记忆应带来源候选标记"
                );
                assert!(
                    c.registry()
                        .load_skills()
                        .unwrap_or_default()
                        .iter()
                        .all(|s| s.id != ref_id),
                    "记忆目标不得写进技能（技能清单里不应出现 {}）",
                    ref_id
                );
                // 来源会话留痕：promoted 事件带 target/refId/version
                // （事件行形状：外层 payload 含 type 与内层 payload——与 governance_of 同一读法）
                let promoted: Vec<serde_json::Value> = c
                    .store
                    .list_events(&fresh.source_session)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|ev| {
                        ev.get("payload")
                            .and_then(|p| p.get("type"))
                            .and_then(|t| t.as_str())
                            == Some("learning.candidate.promoted")
                    })
                    .map(|ev| {
                        ev["payload"]["payload"]
                            .as_object()
                            .cloned()
                            .unwrap_or_default()
                    })
                    .map(serde_json::Value::Object)
                    .collect();
                assert_eq!(promoted.len(), 1, "应恰好有一条 promoted 事件");
                assert_eq!(promoted[0]["id"], fresh.id.as_str(), "{promoted:?}");
                assert_eq!(promoted[0]["target"], "memory", "{promoted:?}");
                assert_eq!(promoted[0]["refId"], ref_id.as_str(), "{promoted:?}");
                assert_eq!(promoted[0]["version"], 1, "{promoted:?}");

                // ---------- 同内容已存在：去重命中，不新增版本 ----------
                // 已有条目必须是**版本 > 1**：否则「版本如实取已有条目」与「写死 1」无法区分
                // （突变核对时写死 1 的变异体正是靠这一点才被杀）。做法：写入 v1 后改一次成 v2，
                // 候选正文 = 该条目的**当前版本**正文（去重按内容哈希，命中的是当前内容）。
                let (c2, app2, _dirs2) = mem_app();
                let existing = c2
                    .memory_remember_global(exm_core::memory::MemoryDraft::new(
                        exm_core::memory::MemoryKind::Lesson,
                        "记忆转正去重",
                        "初版正文：发布前先跑一遍回归",
                    ))
                    .unwrap();
                let current_body = "修订正文：发布前先跑一遍回归并留档";
                let updated = c2
                    .memory_update(&existing.id, None, Some(current_body), None, None)
                    .unwrap()
                    .expect("更新应返回条目");
                assert_eq!(updated.version, 2, "前提：已有条目应已是 v2");
                let before = c2.memory_history(&existing.id).unwrap().len();
                assert_eq!(before, 2, "前提：版本链应有 created + updated 两行");
                let same = submit_memory_candidate(&c2, "记忆转正去重", current_body);
                let (status, v) = approve(app2, &same.id).await;
                assert_eq!(status, StatusCode::OK, "{v}");
                assert_eq!(v["promotion"]["ok"], true, "{v}");
                assert_eq!(
                    v["promotion"]["refId"].as_str(),
                    Some(existing.id.as_str()),
                    "同内容应命中已有条目：{v}"
                );
                assert_eq!(
                    v["promotion"]["version"].as_u64(),
                    Some(2),
                    "版本如实取已有条目的当前版本（v2），不是新条目的 v1：{v}"
                );
                let after = c2.memory_history(&existing.id).unwrap().len();
                assert_eq!(after, before, "去重命中不得给已有条目新增版本行");
                assert_eq!(
                    c2.memory_list(None, 50)
                        .unwrap()
                        .iter()
                        .filter(|e| e.title == "记忆转正去重")
                        .count(),
                    1,
                    "去重命中不得产生第二条同内容记忆"
                );
            }
        }
    }
}
