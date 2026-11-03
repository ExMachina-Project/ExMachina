//! gateway/platform/cron_admin —— 自 platform.rs 按资源域拆出（纯搬移，零行为变更）

// ---------------------------------------------------------------- 定时任务与调度器


use super::*;
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CronBody {
    pub name: String,
    pub prompt: String,
    /// 五段 cron 表达式；与 at 二选一
    #[serde(default)]
    pub cron: Option<String>,
    /// 一次性触发时间（ISO8601）
    #[serde(default)]
    pub at: Option<String>,
    /// 任务类型：cron | at | queue；缺省按 cron/at 有无推断（见 `exm_core::types::job_kind`）
    #[serde(default)]
    pub kind: Option<String>,
    /// 预约任务（at）：投递到的已有会话 id；空 = 按 session_title 新建/复用
    #[serde(default)]
    pub target_session: Option<String>,
    /// 连续任务（queue）：按序执行的任务 id 列表
    #[serde(default)]
    pub steps: Vec<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub session_title: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    /// 结果推送订阅的通道 id 清单（空 = 不推送）
    #[serde(default)]
    pub notify_channels: Vec<String>,
}

pub async fn list_cron(State(st): State<AppState>) -> impl IntoResponse {
    // 内部心跳条目不外露：心跳由 automation 配置驱动（调度器自持 last_run_minute 防重），
    // 非用户任务；外露会导致用户禁用/删除被下一个调度周期静默推翻
    let now = chrono::Utc::now();
    let jobs: Vec<Value> = st
        .core
        .cron
        .list()
        .unwrap_or_default()
        .into_iter()
        .filter(|j| j.id != "__heartbeat__")
        .map(|j| {
            // 下次执行时间（integration-ux）：启用且 cron 命中的任务向前扫描最近命中
            let mut v = serde_json::to_value(&j).unwrap_or(Value::Null);
            if j.enabled {
                if let Some(expr) = &j.cron {
                    if let Some(t) = exm_core::cron::next_run(expr, now, 527_040) {
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert("nextRunAt".into(), json!(t.to_rfc3339()));
                        }
                    }
                }
            }
            v
        })
        .collect();
    Json(serde_json::to_value(jobs).unwrap_or(Value::Null)).into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CronRunsQuery {
    #[serde(default)]
    pub job: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

pub async fn list_cron_runs(
    State(st): State<AppState>,
    Query(q): Query<CronRunsQuery>,
) -> impl IntoResponse {
    match st.core.cron.runs(q.job.as_deref(), q.limit.unwrap_or(20)) {
        Ok(runs) => Json(serde_json::to_value(runs).unwrap_or(Value::Null)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn create_cron(State(st): State<AppState>, Json(b): Json<CronBody>) -> impl IntoResponse {
    if b.name.trim().is_empty() || b.prompt.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "name/prompt 不能为空" })),
        )
            .into_response();
    }
    // 连续任务（queue）靠 steps 驱动，无 cron/at；其余类型维持「二选一」约束
    let is_queue = matches!(b.kind.as_deref(), Some("queue"));
    if !is_queue && b.cron.is_none() && b.at.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "需要 cron 五段表达式或 at 一次性时间" })),
        )
            .into_response();
    }
    if is_queue && b.steps.iter().all(|s| s.trim().is_empty()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "连续任务（queue）需要至少一个步骤任务 id" })),
        )
            .into_response();
    }
    if let Some(expr) = &b.cron {
        if expr.split_whitespace().count() != 5 {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "cron 须为五段表达式：分 时 日 月 周" })),
            )
                .into_response();
        }
    }
    let job = exm_core::types::CronJob {
        id: String::new(),
        name: b.name.trim().to_string(),
        prompt: b.prompt.clone(),
        cron: b.cron,
        at: b.at,
        kind: b.kind,
        target_session: b.target_session,
        steps: b.steps,
        group: b.group,
        session_title: b.session_title,
        enabled: b.enabled.unwrap_or(true),
        last_run_at: None,
        last_status: None,
        last_run_minute: None,
        notify_channels: b
            .notify_channels
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        created_at: String::new(),
    };
    match st.core.cron.upsert(job) {
        Ok(j) => Json(serde_json::to_value(j).unwrap_or(Value::Null)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CronUpdate {
    #[serde(default)]
    pub enabled: Option<bool>,
}

pub async fn update_cron(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<CronUpdate>,
) -> impl IntoResponse {
    match st.core.cron.get(&id) {
        Ok(Some(mut job)) => {
            if let Some(e) = b.enabled {
                job.enabled = e;
            }
            match st.core.cron.upsert(job) {
                Ok(j) => Json(serde_json::to_value(j).unwrap_or(Value::Null)).into_response(),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": e.to_string() })),
                )
                    .into_response(),
            }
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "任务不存在" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn delete_cron(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    match st.core.cron.remove(&id) {
        Ok(true) => Json(json!({ "ok": true })).into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "任务不存在" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// 手动触发一次（测试/补跑）
pub async fn run_cron_now(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    match st.core.cron.get(&id) {
        Ok(Some(job)) => {
            let run = st.core.run_cron_job(&job).await;
            notify_cron_result(&st.core, &job, &run).await;
            Json(serde_json::to_value(run).unwrap_or(Value::Null)).into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "任务不存在" })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
