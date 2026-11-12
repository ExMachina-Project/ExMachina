//! tools/speech.rs —— 语音回复工具（assistant-parity T2）
//!
//! 链路：文本 → TTS 合成（orchestrator speak_audio 能力槽位链）→ mp3 落盘
//! `.exmachina/outbox/` → outbox 登记 `voice` artifact → 收束时经平台直发
//! （telegram sendVoice / napcat record / matrix audio；discord 无语音条目形态
//! 降级链接注记）或 WebUI 语音卡片。
//!
//! 定位：模型显式调用（「用语音回复我」「把这段念出来」），不做收束自动 TTS——
//! 既保文本回复主形态零变更，也让语音产出可被模型按需编排。

use super::*;

impl ToolGateway {
    pub(crate) async fn tool_speech(&self, session_id: &str, args: &serde_json::Value) -> ToolResult {
        let text = args
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if text.is_empty() {
            return ToolResult::err("text 不能为空（要合成为语音的正文）");
        }
        // 长文本护栏：TTS 按字符计费且语音条目过长不可用——超限拒绝并引导精炼
        let max = 600;
        if text.chars().count() > max {
            return ToolResult::err(format!(
                "文本过长（{} 字符 > {max}）：语音条目应短小，请先精炼再合成",
                text.chars().count()
            ));
        }
        let Some(synth) = self.speech_synth.read().clone() else {
            return ToolResult::err(
                "语音合成未装配（该运行环境不支持 speech_reply；或未配置语音能力模型）",
            );
        };
        let mp3 = match synth(text.clone()).await {
            Ok(b) => b,
            Err(e) => return ToolResult::err(format!("语音合成失败：{e}")),
        };
        if mp3.is_empty() {
            return ToolResult::err("语音合成返回空音频");
        }
        // 落盘 + outbox 登记（与 send_file/filesystem 写 outbox 同口径）
        let dir = self.workspace_root.join(".exmachina/outbox");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return ToolResult::err(format!("创建发件箱目录失败：{e}"));
        }
        let fname = format!("voice-{}.mp3", crate::types::new_id()[..8].to_string());
        let abs = dir.join(&fname);
        if let Err(e) = std::fs::write(&abs, &mp3) {
            return ToolResult::err(format!("音频落盘失败：{e}"));
        }
        let rel = format!(".exmachina/outbox/{fname}");
        let preview: String = text.chars().take(30).collect();
        let caption = format!(
            "语音回复：{preview}{}",
            if text.chars().count() > 30 { "…" } else { "" }
        );
        if let Err(e) = self.store.outbox_push(session_id, "voice", &rel, &caption) {
            return ToolResult::err(format!("发件箱登记失败：{e}"));
        }
        ToolResult::ok(format!(
            "语音已合成并登记投递（{kib:.0} KB，收束时随回复送达）：{caption}",
            kib = mp3.len() as f64 / 1024.0
        ))
    }
}
