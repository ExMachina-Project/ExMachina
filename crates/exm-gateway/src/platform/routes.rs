//! gateway/platform/routes —— 自 platform.rs 按资源域拆出（纯搬移，零行为变更）


use super::*;
/// 平台能力路由（挂到 /api 下）
pub fn routes() -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new()
        .route("/api/skills", get(list_skills).post(create_skill))
        .route("/api/skills/:id", axum::routing::delete(delete_skill))
        .route("/api/groups/:id/agents", get(group_agents))
        .route("/api/groups/:id/primary", post(set_group_primary))
        .route("/api/groups/:id/model", axum::routing::put(set_group_model))
        .route(
            "/api/groups/:id/capabilities",
            axum::routing::put(set_group_capabilities),
        )
        .route(
            "/api/groups/:id/workspace",
            axum::routing::put(set_group_workspace),
        )
        .route("/api/groups/:id/overview", get(group_overview))
        .route("/api/sessions/:id/events", get(session_events))
        .route("/api/agents/:id/optimize", post(optimize_agent))
        .route(
            "/api/agents/:id/adaptation",
            get(get_adaptation).delete(reset_adaptation),
        )
        .route("/api/cron", get(list_cron).post(create_cron))
        .route("/api/cron/runs", get(list_cron_runs))
        .route(
            "/api/cron/:id",
            axum::routing::put(update_cron).delete(delete_cron),
        )
        .route("/api/cron/:id/run", post(run_cron_now))
        .route("/api/approvals", get(list_approvals))
        .route("/api/approvals/:id/approve", post(approve_request))
        .route("/api/approvals/:id/deny", post(deny_request))
        .route("/api/sessions/:id/plan", get(plan_list).post(plan_decide))
        .route("/api/workbench/tree", get(workbench_tree))
        .route("/api/workbench/file", get(workbench_file))
        .route("/api/channels/status", get(channel_status))
        .route("/api/channels/flows", get(list_channel_flows))
        .route("/api/channels/bili-qr", post(bili_qr_start))
        .route("/api/channels/bili-qr/:key", get(bili_qr_poll))
        .route("/api/channels", get(list_channels).post(create_channel))
        .route(
            "/api/channels/:id",
            axum::routing::put(update_channel).delete(delete_channel),
        )
        .route("/api/channels/:id/inbound", post(channel_inbound))
}
