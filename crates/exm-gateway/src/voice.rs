//! gateway/voice —— 自 lib.rs 按资源域拆出（纯搬移，零行为变更）

use super::*;

/// 语音转写：multipart 字段 audio（webm/mp3/wav）→ 文本（全局档案 whisper）
pub(crate) async fn voice_transcribe(
    State(st): State<AppState>,
    mut multipart: axum::extract::Multipart,
) -> impl IntoResponse {
    let mut audio: Option<(Vec<u8>, String)> = None;
    while let Ok(Some(field)) = multipart.next_field().await {
        if field.name() == Some("audio") {
            let name = field.file_name().unwrap_or("audio.webm").to_string();
            match field.bytes().await {
                Ok(b) => audio = Some((b.to_vec(), name)),
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": format!("读取音频失败: {e}") })),
                    )
                        .into_response()
                }
            }
        }
    }
    let Some((bytes, name)) = audio else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "缺少 audio 字段" })),
        )
            .into_response();
    };
    match st.core.transcribe(&bytes, &name).await {
        Ok(text) => Json(json!({ "text": text })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// 语音合成：{text} → mp3 字节（audio/mpeg）
pub(crate) async fn voice_speak(
    State(st): State<AppState>,
    Json(b): Json<serde_json::Value>,
) -> impl IntoResponse {
    let text = b
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    if text.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "text 不能为空" })),
        )
            .into_response();
    }
    match st.core.speak(&text).await {
        Ok(mp3) => ([(axum::http::header::CONTENT_TYPE, "audio/mpeg")], mp3).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
