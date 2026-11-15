//! trajectory.rs —— 会话轨迹导出（训练/评测用）
//!
//! 把一条会话的消息按轮次整理为结构化轨迹：用户输入、指挥体/子个体输出、
//! 工具调用（含参数与结果摘要）、token 用量。默认剥离思维链、截断超长工具结果，
//! 产物可直接落 JSONL 供批处理/微调/评测使用。
//!
//! 与 `/api/sessions/:id/export?format=json` 的区别：导出面是「全量归档」（人读/回放），
//! 本模块是「训练样本」（按轮次切分、可控裁剪、稳定 schema）。

use crate::types::{ChatMessage, MessageRole, Session};
use serde_json::{json, Value};

/// 轨迹构建选项（缺省：剥离思维链、保留工具参数、工具摘要截断 2000 字符）
#[derive(Debug, Clone)]
pub struct TrajectoryOptions {
    pub include_thinking: bool,
    pub include_tool_args: bool,
    pub max_tool_chars: usize,
}

impl Default for TrajectoryOptions {
    fn default() -> Self {
        Self {
            include_thinking: false,
            include_tool_args: true,
            max_tool_chars: 2000,
        }
    }
}

fn role_key(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Orchestrator => "orchestrator",
        MessageRole::Unit => "unit",
        MessageRole::System => "system",
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("…[truncated]");
    out
}

fn truncate_json(v: &Value, max: usize) -> Value {
    let raw = serde_json::to_string(v).unwrap_or_default();
    if raw.chars().count() <= max {
        return v.clone();
    }
    json!({
        "truncated": true,
        "preview": truncate(&raw, max),
    })
}

fn message_json(m: &ChatMessage, opts: &TrajectoryOptions) -> Value {
    let content = m
        .statements
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let mut obj = json!({
        "id": m.id,
        "role": role_key(m.role),
        "agentId": m.agent_id,
        "content": content,
        "createdAt": m.created_at,
    });
    if let Some(turn) = m.turn {
        obj["turn"] = json!(turn);
    }
    if opts.include_thinking {
        if let Some(t) = m.thinking.as_deref().filter(|t| !t.trim().is_empty()) {
            obj["thinking"] = json!(truncate(t, opts.max_tool_chars.max(500)));
        }
    }
    if !m.tool_calls.is_empty() {
        let calls: Vec<Value> = m
            .tool_calls
            .iter()
            .map(|t| {
                let mut call = json!({
                    "callId": t.call_id,
                    "agentId": t.agent_id,
                    "tool": t.tool,
                    "ok": t.ok,
                    "durationMs": t.duration_ms,
                    "summary": truncate(&t.summary, opts.max_tool_chars),
                });
                if opts.include_tool_args {
                    call["args"] = truncate_json(&t.args, opts.max_tool_chars);
                }
                call
            })
            .collect();
        obj["toolCalls"] = json!(calls);
    }
    if let Some(u) = &m.token_usage {
        obj["tokenUsage"] = json!({ "prompt": u.prompt, "completion": u.completion });
    }
    obj
}

/// 构建单会话轨迹：`{schema, session, generatedAt, messageCount, turns:[{turn,messages}]}`
///
/// 切轮规则：user 消息开启新一轮；消息显式 `turn` 变化时也切分（兼容子个体消息先落库、
/// 用户消息后补轮次号的旧数据）。没有 user 消息的会话仍会产出一个 turn（全部消息）。
pub fn build_trajectory(
    session: &Session,
    messages: &[ChatMessage],
    opts: &TrajectoryOptions,
) -> Value {
    let mut turns: Vec<Value> = Vec::new();
    let mut current: Vec<Value> = Vec::new();
    let mut current_turn: Option<usize> = None;

    for m in messages {
        let turn_changed = match (m.turn, current_turn) {
            (Some(a), Some(b)) => a != b,
            _ => false,
        };
        let starts_new = m.role == MessageRole::User || turn_changed;
        if starts_new && !current.is_empty() {
            turns.push(json!({
                "turn": current_turn,
                "messages": std::mem::take(&mut current),
            }));
        }
        if starts_new {
            current_turn = m.turn;
        }
        current.push(message_json(m, opts));
    }
    if !current.is_empty() {
        turns.push(json!({
            "turn": current_turn,
            "messages": current,
        }));
    }

    json!({
        "schema": "exm.trajectory/v1",
        "session": {
            "id": session.id,
            "title": session.title,
            "groupId": session.group_id,
            "status": session.status,
            "createdAt": session.created_at,
            "updatedAt": session.updated_at,
        },
        "generatedAt": crate::types::now_iso(),
        "messageCount": messages.len(),
        "turns": turns,
    })
}

/// 轨迹 → JSONL（每轮一行；保留会话元信息，便于批处理按行读取）
pub fn trajectory_jsonl(trajectory: &Value) -> String {
    let session = trajectory
        .get("session")
        .cloned()
        .unwrap_or(Value::Null);
    let schema = trajectory
        .get("schema")
        .cloned()
        .unwrap_or_else(|| json!("exm.trajectory/v1"));
    let Some(turns) = trajectory.get("turns").and_then(|t| t.as_array()) else {
        return String::new();
    };
    turns
        .iter()
        .map(|t| {
            json!({
                "schema": schema,
                "session": session,
                "turn": t.get("turn").cloned().unwrap_or(Value::Null),
                "messages": t.get("messages").cloned().unwrap_or_else(|| json!([])),
            })
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Statement, TokenUsage};

    fn session() -> Session {
        Session {
            id: "s1".into(),
            title: "轨迹测试".into(),
            status: "idle".into(),
            group_id: "g1".into(),
            memory_partition: None,
            rolling_summary: None,
            summary_upto: None,
            parent_session: None,
            parent_upto: None,
            last_message_preview: None,
            last_active_at: None,
            pinned: false,
            archived_at: None,
            origin: None,
            origin_label: None,
            message_count: None,
            ledger: Default::default(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:01.000Z".into(),
        }
    }

    fn msg(
        id: &str,
        role: MessageRole,
        turn: Option<usize>,
        text: &str,
    ) -> ChatMessage {
        ChatMessage {
            id: id.into(),
            session_id: "s1".into(),
            role,
            agent_id: None,
            statements: vec![Statement::report(text)],
            created_at: "2026-01-01T00:00:00.000Z".into(),
            turn,
            token_usage: None,
            thinking: None,
            last_message_preview: None,
            last_active_at: None,
            tool_calls: Vec::new(),
            steps: Vec::new(),
            attachments: Vec::new(),
        }
    }

    #[test]
    fn 轨迹_按用户消息切轮_剥离思维链() {
        let mut a = msg("m1", MessageRole::User, Some(1), "第一问");
        a.thinking = Some("不应出现".into());
        let b = msg("m2", MessageRole::Orchestrator, Some(1), "第一答");
        let c = msg("m3", MessageRole::User, Some(2), "第二问");
        let d = msg("m4", MessageRole::Orchestrator, Some(2), "第二答");
        let t = build_trajectory(&session(), &[a, b, c, d], &TrajectoryOptions::default());
        assert_eq!(t["schema"], "exm.trajectory/v1");
        assert_eq!(t["turns"].as_array().unwrap().len(), 2);
        assert_eq!(t["turns"][0]["messages"].as_array().unwrap().len(), 2);
        assert_eq!(t["turns"][1]["turn"], 2);
        let raw = t.to_string();
        assert!(!raw.contains("不应出现"), "默认应剥离思维链");
    }

    #[test]
    fn 轨迹_保留思维链与工具参数_超长截断() {
        let mut a = msg("m1", MessageRole::User, Some(1), "查一下");
        a.thinking = Some("先检索".into());
        a.token_usage = Some(TokenUsage {
            prompt: 10,
            completion: 20,
        });
        let mut b = msg("m2", MessageRole::Orchestrator, Some(1), "完成");
        b.tool_calls = vec![crate::round_trace::ToolCallRecord {
            call_id: "c1".into(),
            agent_id: "a1".into(),
            tool: "grep".into(),
            args: json!({ "pattern": "x".repeat(500) }),
            ok: true,
            duration_ms: 12,
            summary: "y".repeat(500),
        }];
        let opts = TrajectoryOptions {
            include_thinking: true,
            include_tool_args: true,
            max_tool_chars: 100,
        };
        let t = build_trajectory(&session(), &[a, b], &opts);
        let raw = t.to_string();
        assert!(raw.contains("先检索"));
        assert!(raw.contains("truncated"));
        assert_eq!(t["turns"][0]["messages"][1]["toolCalls"][0]["ok"], true);
        assert_eq!(t["turns"][0]["messages"][0]["tokenUsage"]["prompt"], 10);
    }

    #[test]
    fn 轨迹_jsonl每轮一行() {
        let a = msg("m1", MessageRole::User, Some(1), "问");
        let b = msg("m2", MessageRole::Orchestrator, Some(1), "答");
        let t = build_trajectory(&session(), &[a, b], &TrajectoryOptions::default());
        let jsonl = trajectory_jsonl(&t);
        assert_eq!(jsonl.lines().count(), 1);
        let v: Value = serde_json::from_str(jsonl.lines().next().unwrap()).unwrap();
        assert_eq!(v["session"]["id"], "s1");
        assert_eq!(v["messages"].as_array().unwrap().len(), 2);
    }
}
