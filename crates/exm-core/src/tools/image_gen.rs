//! tools/image_gen.rs —— 图像生成工具（image-generation 任务 2.2）
//!
//! 链路：prompt → orchestrator image_gen_bytes（能力槽位链：组覆盖 → 全局）
//! → 魔数嗅探校验（PNG/JPEG/WebP）→ 落盘 `.exmachina/outbox/` → outbox
//! 登记 `image` artifact → 收束时经平台直发（napcat/qqbot 图片消息、
//! telegram sendPhoto 等）或 WebUI 图片卡片。
//!
//! 门控：生图槽位未配置（capabilities.imageGen 空）时工具不下发；
//! 槽位装配后未回接（重建窗口）执行返回明确错误。

use super::*;

/// 图片字节上限（20MB）：与通道侧图片上限对齐（qqbot file_data ≤20MB）
const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

/// 魔数嗅探：PNG / JPEG / WebP（真校验字节而非信任扩展名/Content-Type——
/// 上游端点可能回 HTML 错误页，直接落盘会产出损坏图片）
fn sniff_format(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 8 && bytes[..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A] {
        Some("png")
    } else if bytes.len() >= 2 && bytes[..2] == [0xFF, 0xD8] {
        Some("jpg")
    } else if bytes.len() >= 12 && bytes[..4] == [0x52, 0x49, 0x46, 0x46] && bytes[8..12] == [0x57, 0x45, 0x42, 0x50] {
        Some("webp")
    } else {
        None
    }
}

impl ToolGateway {
    pub(crate) async fn tool_image_gen(&self, session_id: &str, args: &serde_json::Value) -> ToolResult {
        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if prompt.is_empty() {
            return ToolResult::err("prompt 不能为空（画面描述）");
        }
        let size = args
            .get("size")
            .and_then(|v| v.as_str())
            .unwrap_or("1024x1024")
            .trim()
            .to_string();
        let size = match size.as_str() {
            "1024x1024" | "1024x1536" | "1536x1024" => size,
            other => {
                return ToolResult::err(format!(
                    "不支持的 size「{other}」（可选 1024x1024 / 1024x1536 / 1536x1024）"
                ))
            }
        };
        let Some(gen) = self.image_gen.read().clone() else {
            return ToolResult::err(
                "图像生成未装配（该运行环境不支持 image_gen；或未配置生图能力模型）",
            );
        };
        // 120s 硬超时：生图上游普遍 10-60s，无界等待会占死会话轮次
        let bytes = match tokio::time::timeout(
            std::time::Duration::from_secs(120),
            gen(prompt.clone(), size.clone()),
        )
        .await
        {
            Ok(Ok(b)) => b,
            Ok(Err(e)) => return ToolResult::err(format!("图像生成失败：{e}")),
            Err(_) => {
                return ToolResult::err("图像生成超时（120 秒）：请精炼 prompt 后重试，或改用文字描述")
            }
        };
        if bytes.is_empty() {
            return ToolResult::err("图像生成返回空内容");
        }
        if bytes.len() > MAX_IMAGE_BYTES {
            return ToolResult::err(format!(
                "生成图片过大（{} KB > 20MB 上限），已丢弃",
                bytes.len() / 1024
            ));
        }
        // 魔数嗅探：非图片字节（HTML 错误页等）直接拒绝
        let Some(fmt) = sniff_format(&bytes) else {
            return ToolResult::err(format!(
                "生成内容不是有效图片（未知魔数，前 16 字节：{:02X?}）——上游可能返回了错误页",
                bytes.iter().take(16).collect::<Vec<_>>()
            ));
        };
        // 落盘 + outbox 登记（与 speech/filesystem 写 outbox 同口径）
        let dir = self.workspace_root.join(".exmachina/outbox");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return ToolResult::err(format!("创建发件箱目录失败：{e}"));
        }
        let fname = format!("image-{}.{}", &crate::types::new_id()[..8], fmt);
        let abs = dir.join(&fname);
        if let Err(e) = std::fs::write(&abs, &bytes) {
            return ToolResult::err(format!("图片落盘失败：{e}"));
        }
        let rel = format!(".exmachina/outbox/{fname}");
        let preview: String = prompt.chars().take(30).collect();
        let caption = format!(
            "生成图片：{preview}{}（{size}）",
            if prompt.chars().count() > 30 { "…" } else { "" }
        );
        if let Err(e) = self.store.outbox_push(session_id, "image", &rel, &caption) {
            return ToolResult::err(format!("发件箱登记失败：{e}"));
        }
        ToolResult::ok(format!(
            "图片已生成并登记投递（{fmt}，{}×{}，{kib:.0} KB，收束时随回复送达）",
            size.split_once('x').map(|(w, _)| w).unwrap_or("?"),
            size.split_once('x').map(|(_, h)| h).unwrap_or("?"),
            kib = bytes.len() as f64 / 1024.0
        ))
    }
}
