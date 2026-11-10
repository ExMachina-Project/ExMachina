//! 旁听媒体滚动窗口（ambient-media）：群聊里未 @ 的图片/语音先落窗口，
//! 用户随后 @ 提问时把窗口内的媒体并入本轮多模态注入。
//!
//! 背景（部署实测缺陷）：「仅 @ 回复」模式下，未 @ 的纯图消息走旁听路径
//! （imctx: 占位文本），不下载不注入；用户紧接着 @ 机器人问「这是什么」，
//! 本轮拿到的只有占位符——图片语义整个丢失。窗口把「先发图、后 @ 问」
//! 这一高频序列接通：旁听媒体落窗口（带 TTL），@ 轮次取走并入注入。
//!
//! 容量语义：每流（channel:chat_key）一个窗口，最多 4 条、10 分钟过期；
//! 超限丢最旧。内存态即可——进程重启丢窗口只影响刚发图的旁听，无一致性风险。

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

/// 窗口条目：已下载落盘的媒体（kind, 绝对路径, 文件名）——与
/// `stage_inbound_media` 入参同构，@ 轮次直接复用注入链。
#[derive(Debug, Clone)]
pub struct AmbientMedia {
    pub kind: String,
    pub path: String,
    pub name: String,
    pub at_ms: u64,
}

#[derive(Default)]
struct Window {
    items: Vec<AmbientMedia>,
}

/// std Mutex 统一解药（parking_lot 之外的 std::sync::Mutex 毒化兜底）
fn lock_std<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn windows() -> &'static std::sync::Mutex<HashMap<String, Arc<std::sync::Mutex<Window>>>> {
    static W: OnceLock<std::sync::Mutex<HashMap<String, Arc<std::sync::Mutex<Window>>>>> =
        OnceLock::new();
    W.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// 每流窗口上限（条数）与 TTL（ms）
const MAX_ITEMS: usize = 4;
const TTL_MS: u64 = 10 * 60 * 1000;

fn flow_key(ch_id: &str, chat_key: &str) -> String {
    format!("{ch_id}:{chat_key}")
}

/// 旁听媒体入窗（已下载落盘的条目；纯文本旁听不进这里）。
/// 顺手做过期清理与容量裁剪（丢最旧）。
pub fn push(ch_id: &str, chat_key: &str, items: Vec<(String, String, String)>) {
    if items.is_empty() {
        return;
    }
    let now = exm_core::types::now_ms();
    let key = flow_key(ch_id, chat_key);
    let win = lock_std(windows()).entry(key).or_default().clone();
    let mut w = lock_std(&win);
    w.items.retain(|m| now - m.at_ms < TTL_MS);
    for (kind, path, name) in items {
        w.items.push(AmbientMedia {
            kind,
            path,
            name,
            at_ms: now,
        });
    }
    if w.items.len() > MAX_ITEMS {
        let drain = w.items.len() - MAX_ITEMS;
        w.items.drain(0..drain);
    }
}

/// @ 轮次取走窗口全部媒体（取走即清——只并入触发轮次一次）。
/// 返回条目与 `stage_inbound_media` 的 saved 参数同构。
pub fn take(ch_id: &str, chat_key: &str) -> Vec<(String, String, String)> {
    let key = flow_key(ch_id, chat_key);
    let win = {
        let map = lock_std(windows());
        map.get(&key).cloned()
    };
    let Some(win) = win else {
        return Vec::new();
    };
    let mut w = lock_std(&win);
    let now = exm_core::types::now_ms();
    w.items.retain(|m| now - m.at_ms < TTL_MS);
    w.items
        .drain(..)
        .map(|m| (m.kind, m.path, m.name))
        .collect()
}

/// 窗口只读快照（不取走；ambient-media-attach 用）：旁听消息落库时
/// 回填附件展示元数据用——窗口条目仍留给后续 @ 轮次 take 注入模型。
pub fn snapshot(ch_id: &str, chat_key: &str) -> Vec<AmbientMedia> {
    let key = flow_key(ch_id, chat_key);
    let win = {
        let map = lock_std(windows());
        map.get(&key).cloned()
    };
    let Some(win) = win else {
        return Vec::new();
    };
    let w = lock_std(&win);
    let now = exm_core::types::now_ms();
    w.items
        .iter()
        .filter(|m| now - m.at_ms < TTL_MS)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 入窗 → 取走即清；每流窗口互相隔离
    #[test]
    fn 旁听窗口_入窗取走与流隔离() {
        push(
            "ch1",
            "g1",
            vec![("image".into(), "/tmp/a.png".into(), "a.png".into())],
        );
        push(
            "ch1",
            "g2",
            vec![("image".into(), "/tmp/b.png".into(), "b.png".into())],
        );
        let g1 = take("ch1", "g1");
        assert_eq!(g1.len(), 1, "ch1:g1 应有一条");
        assert_eq!(g1[0].1, "/tmp/a.png");
        assert!(take("ch1", "g1").is_empty(), "取走即清");
        let g2 = take("ch1", "g2");
        assert_eq!(g2.len(), 1, "其他流窗口不受影响");
    }

    /// 容量上限：超限丢最旧
    #[test]
    fn 旁听窗口_容量上限丢最旧() {
        let key = ("cap", "g");
        for i in 0..6 {
            push(
                key.0,
                key.1,
                vec![("image".into(), format!("/tmp/{i}.png"), format!("{i}.png"))],
            );
        }
        let items = take(key.0, key.1);
        assert_eq!(items.len(), MAX_ITEMS, "超限裁到上限");
        assert_eq!(items[0].2, "2.png", "最旧两条被丢弃");
    }
}
