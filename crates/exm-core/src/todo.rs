//! 任务清单（spec: agent-todo）：会话级 Todo 自组织面。
//!
//! 长任务（多文件重构/多步调研）中模型用 todo 工具维护「做了什么/还剩什么」，
//! 全量重写语义（一次调用写全整个清单，同现代 harness 惯例）。
//! 进程内注册表（与 session_modes/plan_items 同口径——轮次级行为不落库）。
//!
//! 清单非空时系统提示每轮注入渲染段（✅/🔄/⬜），模型无需主动查询。

use std::collections::HashMap;
use std::sync::OnceLock;

/// 清单条目：内容 + 状态
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    /// pending | in_progress | done（写入时归一，非法值落 pending）
    pub status: String,
}

/// 系统提示注入段的条目上限（防膨胀；超出截断并提示）
const RENDER_CAP: usize = 30;

fn todos() -> &'static parking_lot::Mutex<HashMap<String, Vec<TodoItem>>> {
    static REG: OnceLock<parking_lot::Mutex<HashMap<String, Vec<TodoItem>>>> = OnceLock::new();
    REG.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

/// 状态归一：非 pending/in_progress/done 一律落 pending
fn norm_status(s: &str) -> String {
    match s {
        "in_progress" | "done" => s.to_string(),
        _ => "pending".to_string(),
    }
}

/// 全量替换清单：items 为 (content, status) 序列；返回替换后的渲染摘要
pub fn write(session_id: &str, items: Vec<(String, String)>) -> Vec<TodoItem> {
    let now_items: Vec<TodoItem> = items
        .into_iter()
        .map(|(content, status)| TodoItem {
            id: format!("td-{}", &crate::types::new_id()[..6]),
            content,
            status: norm_status(&status),
        })
        .collect();
    if now_items.is_empty() {
        todos().lock().remove(session_id);
    } else {
        todos()
            .lock()
            .insert(session_id.to_string(), now_items.clone());
    }
    now_items
}

/// 当前清单（无 = 空 vec）
pub fn list_items(session_id: &str) -> Vec<TodoItem> {
    todos().lock().get(session_id).cloned().unwrap_or_default()
}

/// 清单是否全 done（空清单不算）
pub fn all_done(session_id: &str) -> bool {
    let items = list_items(session_id);
    !items.is_empty() && items.iter().all(|t| t.status == "done")
}

/// 系统提示注入段：清单非空时 Some（状态渲染 + 全完尾注 + 截断提示）；空清单 None
pub fn render(session_id: &str) -> Option<String> {
    let items = list_items(session_id);
    if items.is_empty() {
        return None;
    }
    let done = items.iter().filter(|t| t.status == "done").count();
    let mut out = format!("## 当前任务清单（{done}/{} 完成）\n", items.len());
    for t in items.iter().take(RENDER_CAP) {
        let mark = match t.status.as_str() {
            "done" => "✅",
            "in_progress" => "🔄",
            _ => "⬜",
        };
        out.push_str(&format!("- {mark} {}\n", t.content));
    }
    if items.len() > RENDER_CAP {
        out.push_str(&format!(
            "- …（另有 {} 条未展示）\n",
            items.len() - RENDER_CAP
        ));
    }
    if done == items.len() {
        out.push_str("\n清单已全部完成；如任务收尾请总结产出。\n");
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 清单生命周期_写入查询清空() {
        let sid = "todo-test-1";
        assert!(list_items(sid).is_empty());
        write(
            sid,
            vec![
                ("定位缺陷".into(), "done".into()),
                ("修复实现".into(), "in_progress".into()),
                ("回归验证".into(), "pending".into()),
            ],
        );
        let items = list_items(sid);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].status, "done");
        assert_eq!(items[1].status, "in_progress");
        assert!(!all_done(sid));
        // 全量重写：旧条目整体消失
        write(sid, vec![("收尾".into(), "done".into())]);
        assert_eq!(list_items(sid).len(), 1);
        assert!(all_done(sid));
        // 空写 = 清空
        write(sid, vec![]);
        assert!(list_items(sid).is_empty());
        assert!(!all_done(sid));
    }

    #[test]
    fn 状态归一_非法值落pending() {
        let sid = "todo-test-2";
        write(
            sid,
            vec![("x".into(), "DONE".into()), ("y".into(), "".into())],
        );
        let items = list_items(sid);
        assert_eq!(
            items[0].status, "pending",
            "大写 DONE 应归一为 pending（只认小写）"
        );
        assert_eq!(items[1].status, "pending");
    }

    #[test]
    fn 渲染_空清单none_非空含状态符号() {
        let sid = "todo-test-3";
        assert!(render(sid).is_none());
        write(
            sid,
            vec![
                ("a".into(), "done".into()),
                ("b".into(), "in_progress".into()),
                ("c".into(), "pending".into()),
            ],
        );
        let r = render(sid).unwrap();
        assert!(r.contains("1/3 完成"), "应含进度统计：{r}");
        assert!(r.contains("✅ a"));
        assert!(r.contains("🔄 b"));
        assert!(r.contains("⬜ c"));
        assert!(!r.contains("全部完成"));
        write(sid, vec![("only".into(), "done".into())]);
        assert!(render(sid).unwrap().contains("全部完成"));
    }

    #[test]
    fn 渲染截断_超上限提示() {
        let sid = "todo-test-4";
        let items: Vec<(String, String)> = (0..40)
            .map(|i| (format!("条目{i}"), "pending".into()))
            .collect();
        write(sid, items);
        let r = render(sid).unwrap();
        assert!(
            r.contains("另有 10 条未展示"),
            "超出 30 条应有截断提示：{}",
            r.lines().count()
        );
    }
}
