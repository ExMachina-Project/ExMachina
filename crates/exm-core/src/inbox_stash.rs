//! 入站附件暂存（im-inbound-attach）：外部通道收到的图片/文件/语音随 user 消息落库，
//! WebUI 会话视图据此渲染缩略图/下载卡片（此前只落占位文本，聊天流里看不到用户发过的图）。
//! 进程级单例：与 image_stash 同模型——网关为单 Core 进程，通道注入、规划落库取走即清。

use crate::types::InboundAttachment;
use std::collections::HashMap;
use std::sync::OnceLock;

fn stash() -> &'static parking_lot::Mutex<HashMap<String, Vec<InboundAttachment>>> {
    static STASH: OnceLock<parking_lot::Mutex<HashMap<String, Vec<InboundAttachment>>>> =
        OnceLock::new();
    STASH.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

/// 暂存入站附件（工作区相对路径），追加到该会话的待落库队列
pub fn stage(session_id: &str, items: Vec<InboundAttachment>) {
    if items.is_empty() {
        return;
    }
    stash()
        .lock()
        .entry(session_id.to_string())
        .or_default()
        .extend(items);
}

/// 取走该会话全部暂存附件（随本轮 user 消息落库后清空）
pub fn take(session_id: &str) -> Vec<InboundAttachment> {
    stash().lock().remove(session_id).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(path: &str) -> InboundAttachment {
        InboundAttachment {
            kind: "image".into(),
            path: path.into(),
            name: "pic.png".into(),
        }
    }

    #[test]
    fn 取走即清() {
        stage("ses-a", vec![item(".exmachina/inbox/a.png")]);
        stage("ses-a", vec![item(".exmachina/inbox/b.png")]);
        let taken = take("ses-a");
        assert_eq!(taken.len(), 2);
        assert!(take("ses-a").is_empty(), "取走后应清空");
    }

    #[test]
    fn 会话隔离() {
        stage("ses-x", vec![item(".exmachina/inbox/x.png")]);
        assert!(take("ses-y").is_empty());
        assert_eq!(take("ses-x").len(), 1);
    }
}
