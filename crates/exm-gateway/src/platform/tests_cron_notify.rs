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
}
