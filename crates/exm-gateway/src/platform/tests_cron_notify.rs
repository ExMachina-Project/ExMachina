//! gateway/platform/tests_cron_notify —— 自 platform.rs 按资源域拆出（纯搬移，零行为变更）
#[cfg(test)]
use super::*;

// ---------------------------------------------------------------- 定时任务结果推送（7.3 单测）

mod cron_notify_tests {
    use super::*;

    fn core_with_channel(reply_url: &str) -> exm_core::Core {
        let dir = std::env::temp_dir().join(format!("exm-notify-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join(".exmachina").join("data")).unwrap();
        let core = exm_core::Core::create(&dir).expect("创建 Core 失败");
        // 通道落库：webhook 桥接 + 回调地址指向测试收端
        let channels = serde_json::json!([{
            "id": "ch-hook", "type": "webhook", "enabled": true,
            "allowedChats": [], "replyWebhook": reply_url, "createdAt": exm_core::types::now_iso(),
        }]);
        std::fs::write(
            core.config().data_dir.join("channels.json"),
            serde_json::to_string_pretty(&channels).unwrap(),
        )
        .unwrap();
        core
    }

    fn job(subscribed: bool) -> exm_core::types::CronJob {
        exm_core::types::CronJob {
            id: "job1".into(),
            name: "每日报表".into(),
            prompt: "生成报表".into(),
            cron: Some("0 9 * * *".into()),
            at: None,
            kind: None,
            target_session: None,
            steps: Vec::new(),
            group: None,
            session_title: None,
            enabled: true,
            last_run_at: None,
            last_status: None,
            last_run_minute: None,
            notify_channels: if subscribed {
                vec!["ch-hook".into()]
            } else {
                vec![]
            },
            created_at: String::new(),
        }
    }

    fn run() -> exm_core::types::CronRun {
        exm_core::types::CronRun {
            id: "r1".into(),
            job_id: "job1".into(),
            job_name: "每日报表".into(),
            session_id: "s1".into(),
            started_at: exm_core::types::now_iso(),
            finished_at: exm_core::types::now_iso(),
            status: "done".into(),
            summary: "报表已生成：12 项指标正常".into(),
        }
    }

    /// 本地 HTTP 收端：记录收到的 POST 文本
    async fn spawn_receiver() -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let app = axum::Router::new().route(
            "/hook",
            axum::routing::post(move |body: String| async move {
                let _ = tx.send(body);
                "ok"
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}/hook"), rx)
    }

    #[tokio::test]
    async fn 推送_订阅通道收到摘要() {
        let (url, mut rx) = spawn_receiver().await;
        let core = core_with_channel(&url);
        notify_cron_result(&core, &job(true), &run()).await;
        let body = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("订阅推送应在超时前到达")
            .expect("通道收端应收到消息");
        assert!(body.contains("每日报表"), "摘要含任务名：{body}");
        assert!(body.contains("报表已生成"), "摘要含结论：{body}");
    }

    #[tokio::test]
    async fn 推送_无订阅维持现状() {
        let (url, mut rx) = spawn_receiver().await;
        let core = core_with_channel(&url);
        notify_cron_result(&core, &job(false), &run()).await;
        let got = tokio::time::timeout(std::time::Duration::from_millis(400), rx.recv()).await;
        assert!(got.is_err(), "未订阅任务不得产生通道消息");
    }

    #[tokio::test]
    async fn 推送_总开关关闭不推() {
        let (url, mut rx) = spawn_receiver().await;
        let core = core_with_channel(&url);
        let mut cfg = (*core.config()).clone();
        cfg.notify.enabled = false;
        core.apply_config(cfg).unwrap();
        notify_cron_result(&core, &job(true), &run()).await;
        let got = tokio::time::timeout(std::time::Duration::from_millis(400), rx.recv()).await;
        assert!(got.is_err(), "总开关关闭 = 全部不推");
    }

    /// 投递契约（update-pipeline「webhook 投递超时与重试」）：先失败后成功的端点，
    /// 重试链应在退避后拿到成功，且不产生 notify.delivery_failed 审计事件。
    #[tokio::test]
    async fn 推送_先失败后成功_重试拿到成功且不落审计() {
        // 收端：第一次 500，之后 200
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let c2 = counter.clone();
        let app = axum::Router::new().route(
            "/hook",
            axum::routing::post(move |body: String| {
                let c = c2.clone();
                async move {
                    if c.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                        (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom".to_string())
                    } else {
                        (axum::http::StatusCode::OK, body)
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let core = core_with_channel(&format!("http://{addr}/hook"));
        let mut events = core.events.subscribe();
        notify_cron_result(&core, &job(true), &run()).await;
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "第一次失败后应有一次重试成功（共 2 次尝试）"
        );
        // 不应产生投递失败事件
        let got = tokio::time::timeout(std::time::Duration::from_millis(300), events.recv()).await;
        assert!(
            got.is_err() || !matches!(&got, Ok(Ok(e)) if e.kind == "notify.delivery_failed"),
            "重试成功不得产生失败审计事件"
        );
    }

    /// 投递契约：端点持续失败时重试 3 次（1 + 2 重试）后放弃，产生含目标与原因的审计事件。
    #[tokio::test]
    async fn 推送_持续失败_重试耗尽落审计事件() {
        let (url, _rx) = spawn_receiver_failing().await;
        let core = core_with_channel(&url);
        let mut events = core.events.subscribe();
        notify_cron_result(&core, &job(true), &run()).await;
        // 收端确实被请求过（但每次都 500，无成功投递可言——以 attempts 审计为准）
        // 审计事件必达：含通道目标与原因摘要
        let mut found = None;
        for _ in 0..20 {
            match tokio::time::timeout(std::time::Duration::from_millis(500), events.recv()).await
            {
                Ok(Ok(e)) if e.kind == "notify.delivery_failed" => {
                    found = Some(e);
                    break;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        let evt = found.expect("重试耗尽必须产生 notify.delivery_failed 审计事件");
        assert_eq!(evt.payload["channel"], "ch-hook", "事件含通道目标");
        assert_eq!(evt.payload["attempts"], 3, "共 3 次尝试（1 + 2 重试）");
        assert!(
            evt.payload["reason"].as_str().is_some_and(|r| !r.is_empty()),
            "事件含原因摘要"
        );
    }

    /// 持续 500 的收端（用于重试耗尽路径）
    async fn spawn_receiver_failing() -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let app = axum::Router::new().route(
            "/hook",
            axum::routing::post(move |body: String| async move {
                let _ = tx.send(body);
                (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "always fail")
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}/hook"), rx)
    }
}
