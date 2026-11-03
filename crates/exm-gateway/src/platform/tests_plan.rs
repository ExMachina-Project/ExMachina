//! gateway/platform/tests_plan —— 自 platform.rs 按资源域拆出（纯搬移，零行为变更）
#[cfg(test)]
mod plan_endpoint_tests {

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt; // oneshot

    fn core() -> exm_core::Core {
        let dir = std::env::temp_dir().join(format!("exm-plan-e2e-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = exm_core::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        exm_core::Core::with_config(cfg).expect("创建 Core 失败")
    }

    fn app(c: exm_core::Core) -> axum::Router {
        crate::build_router(std::sync::Arc::new(c))
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// 三动作 e2e：setMode →（拦截产生清单）→ GET 清单 → approve 落地
    #[tokio::test]
    async fn 计划端点_三动作全链路() {
        let c1 = core();
        let s = c1.create_session("计划e2e").unwrap();
        let app = app(c1);
        // 1) setMode 开启
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{}/plan", s.id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "action": "setMode", "plan": true }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["mode"], "plan");
        // 2) 模拟拦截产生清单（等价工具网关拦截写操作）
        {
            {
                // core 已移入 app——重建一个同配置 Core 不可行，直接用进程级 plan API
                exm_core::plan::push_item(
                    &s.id,
                    exm_core::plan::to_plan_item(
                        exm_core::types::ToolName::Filesystem,
                        &serde_json::json!({ "op": "write", "path": "e2e.txt", "content": "x" }),
                    ),
                );
            }
        }
        // 3) GET 清单
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/sessions/{}/plan", s.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["count"], 1);
        assert_eq!(v["items"][0]["target"], "e2e.txt");
        // 4) approve：清单消费 + 模式切回 execute
        let router = app;
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{}/plan", s.id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "action": "approve" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["okCount"], 1, "回放应成功：{v}");
        // 5) 未知动作报 400
        let c2 = core();
        let s2 = c2.create_session("计划e2e2").unwrap();
        let app2 = crate::build_router(std::sync::Arc::new(c2));
        let resp = app2
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{}/plan", s2.id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "action": "bogus" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// 工作台端点：树/读取/越权拒绝
    #[tokio::test]
    async fn 工作台端点_树读取与越权拒绝() {
        let c1 = core();
        // 造文件结构
        let ws = c1.config().workspace_root.clone();
        std::fs::create_dir_all(ws.join("src")).unwrap();
        std::fs::write(ws.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(ws.join("README.md"), "# t\nhello\nworld\n").unwrap();
        let app = app(c1);
        // 树
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/workbench/tree")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        let nodes = v["nodes"].as_array().expect("树节点数组");
        assert!(
            nodes.iter().any(|n| n["path"] == "src/main.rs"),
            "树应含 src/main.rs：{v}"
        );
        assert!(
            !nodes
                .iter()
                .any(|n| n["path"].as_str().unwrap_or("").starts_with(".git")),
            "不应含 .git"
        );
        // 读文件（分页）
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/workbench/file?path=README.md&offset=2&limit=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["content"], "hello", "第二行：{v}");
        assert_eq!(v["totalLines"], 3);
        // 越权路径拒绝
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/workbench/file?path=../../../etc/passwd")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "越权路径应 400");
    }

    /// 拒绝动作：清单保留、模式不变
    #[tokio::test]
    async fn 计划端点_拒绝保留清单() {
        let c1 = core();
        let s = c1.create_session("拒绝e2e").unwrap();
        c1.set_plan_mode(&s.id, true);
        exm_core::plan::push_item(
            &s.id,
            exm_core::plan::to_plan_item(
                exm_core::types::ToolName::Edit,
                &serde_json::json!({ "path": "r.rs", "old_string": "a", "new_string": "b" }),
            ),
        );
        let app = app(c1);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{}/plan", s.id))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "action": "reject" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["kept"], 1);
        assert_eq!(v["mode"], "plan");
        assert_eq!(exm_core::plan::list_items(&s.id).len(), 1);
    }
}
