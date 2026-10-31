//! gateway/auth_setup —— 自 lib.rs 按资源域拆出（纯搬移，零行为变更）

use super::*;

/// 首启密钥设置引导的进程内状态：「下次再说」置位；网关重启自然清零 → 下次启动恢复提醒
/// 「不再提醒」另持久化到 config.security.keySetupDismissed（跨重启）
pub(crate) static KEY_SETUP_SNOOZED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 通道入站端点识别：POST /api/channels/{id}/inbound → Some(通道 id)；其余 None
pub(crate) fn inbound_channel_id<'a>(path: &'a str, method: &axum::http::Method) -> Option<&'a str> {
    if method != axum::http::Method::POST {
        return None;
    }
    path.strip_prefix("/api/channels/")?
        .strip_suffix("/inbound")
        .filter(|id| !id.is_empty() && !id.contains('/'))
}

pub(crate) async fn verify_auth(State(st): State<AppState>, Json(b): Json<Value>) -> impl IntoResponse {
    let required = !st.core.config().security.auth_key.is_empty();
    if !required {
        return Json(json!({ "ok": true, "required": false })).into_response();
    }
    let key = b.get("key").and_then(|v| v.as_str()).unwrap_or("");
    if key == st.core.config().security.auth_key {
        Json(json!({ "ok": true, "required": true })).into_response()
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "ok": false, "error": "密钥错误" })),
        )
            .into_response()
    }
}

/// 首启引导状态（免鉴权，同 verify）：是否已配置访问密钥 + 提醒是否已被跳过/关闭。
/// keyConfigured=false 且未 snoozed/dismissed 时，前端展示「设置访问密钥」页
/// （部署后首次启动、尚未设置密钥的场景）。
pub(crate) async fn auth_status(State(st): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "ok": true,
        "keyConfigured": !st.core.config().security.auth_key.is_empty(),
        "setupSnoozed": KEY_SETUP_SNOOZED.load(std::sync::atomic::Ordering::SeqCst),
        "setupDismissed": st.core.config().security.key_setup_dismissed,
    }))
}

/// 「下次再说」：本次进程内不再提示（网关重启后恢复提醒）。已配置密钥时拒绝。
pub(crate) async fn auth_snooze(State(st): State<AppState>) -> impl IntoResponse {
    if !st.core.config().security.auth_key.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "已配置密钥" })),
        )
            .into_response();
    }
    KEY_SETUP_SNOOZED.store(true, std::sync::atomic::Ordering::SeqCst);
    Json(json!({ "ok": true })).into_response()
}

/// 「不再提醒」：持久化关闭首启设置引导（config.security.keySetupDismissed）。已配置密钥时拒绝。
pub(crate) async fn auth_dismiss(State(st): State<AppState>) -> impl IntoResponse {
    if !st.core.config().security.auth_key.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "已配置密钥" })),
        )
            .into_response();
    }
    let mut next = (*st.core.config()).clone();
    next.security.key_setup_dismissed = true;
    match st.core.apply_config(next) {
        Ok(_) => Json(json!({ "ok": true })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": e.to_string() })),
        )
            .into_response(),
    }
}
