//! LLM 输出解析与契约校验 —— 对应 TS 版 zod 校验位置

use crate::types::{OrchestratorPlan, SyncReport};

/// 从文本中提取第一个 JSON 值：优先 ```json 代码块，退化为首尾花括号
pub fn extract_json(text: &str) -> Option<serde_json::Value> {
    let mut candidates: Vec<&str> = Vec::new();

    if let Some(start) = text.find("```") {
        let rest = &text[start + 3..];
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        if let Some(end) = rest.find("```") {
            candidates.push(rest[..end].trim());
        }
    }
    if let (Some(s), Some(e)) = (text.find('{'), text.rfind('}')) {
        if e > s {
            candidates.push(&text[s..=e]);
        }
    }

    for c in candidates {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(c) {
            return Some(v);
        }
    }
    None
}

/// 解析并校验 SyncReport（结构 + 业务约束）。
/// 结构失败时走受控修复层（sync-report-resilience）：失败原因全为
/// missing field 且字段 ∈ 白名单 {summary, confidence, title, agentIdentifier}
/// 时补齐后重反序列化一次（不是循环重试）；其余（核心字段缺失/类型错误/
/// 业务约束失败）维持原错误。
pub fn parse_sync_report(text: &str) -> Result<SyncReport, String> {
    let raw = extract_json(text).ok_or_else(|| "未找到 JSON 输出".to_string())?;
    match serde_json::from_value::<SyncReport>(raw.clone()) {
        Ok(report) => {
            validate_sync_report(&report)?;
            Ok(report)
        }
        Err(e) => {
            let repaired = try_repair_sync_report(&raw, &e)?;
            validate_sync_report(&repaired)?;
            Ok(repaired)
        }
    }
}

/// 判定 serde 错误是否为白名单字段的 missing field，构造修复 JSON
/// 重反序列化一次。修复成功打 repaired: Some(true)；不满足修复条件时
/// 原样返回原错误（调用方维持既有报错路径）。
fn try_repair_sync_report(
    raw: &serde_json::Value,
    orig_err: &serde_json::Error,
) -> Result<SyncReport, String> {
    let orig = format!("SyncReport 结构校验失败: {orig_err}");
    // 修复触发条件：原错误是 missing field 且字段 ∈ 白名单。
    // serde 一次只报第一个缺失字段——类型错误/核心字段缺失直接原错误返回。
    let err_str = orig_err.to_string();
    if !MISSING_WHITELIST
        .iter()
        .any(|f| err_str.contains(&format!("missing field `{f}`")))
    {
        return Err(orig);
    }
    // 一次性补齐全部白名单缺失字段（多字段同时缺也只重反序列化一次）：
    // - 顶层 summary：首条 statement 截 200 字符（statements 空则留空，
    //   由业务校验兜底报错——不越权修复）
    // - 顶层 confidence：0.5（中性值，指挥体可识别低置信）
    // - 嵌套 nextSuggestion.appendNodes 条目的 title/agentIdentifier：空串
    //   （演进期别名漂移；建议性结构，核心身份字段由业务校验兜住）
    let mut patched = raw.clone();
    if let Some(obj) = patched.as_object_mut() {
        if !obj.contains_key("summary") {
            let fallback = obj
                .get("statements")
                .and_then(|s| s.as_array())
                .and_then(|a| a.first())
                .and_then(|s| s.get("text"))
                .and_then(|t| t.as_str())
                .map(|t| t.chars().take(200).collect::<String>())
                .unwrap_or_default();
            obj.insert("summary".into(), serde_json::Value::String(fallback));
        }
        if !obj.contains_key("confidence") {
            obj.insert("confidence".into(), serde_json::Value::from(0.5));
        }
    }
    if let Some(nodes) = patched
        .get_mut("nextSuggestion")
        .and_then(|ns| ns.get_mut("appendNodes"))
        .and_then(|an| an.as_array_mut())
    {
        for node in nodes.iter_mut() {
            if let Some(obj) = node.as_object_mut() {
                for f in ["title", "agentIdentifier"] {
                    if !obj.contains_key(f) {
                        obj.insert(f.into(), serde_json::Value::String(String::new()));
                    }
                }
            }
        }
    }
    // 单次重反序列化；再失败（含补齐后仍缺核心字段）原错误语义返回
    match serde_json::from_value::<SyncReport>(patched) {
        Ok(mut r) => {
            r.repaired = Some(true);
            Ok(r)
        }
        Err(_) => Err(orig),
    }
}

/// 白名单：可推断补齐的缺失字段（sync-report-resilience 白名单口径）
const MISSING_WHITELIST: &[&str] = &["summary", "confidence", "title", "agentIdentifier"];

/// 业务约束校验（对应 ts 版 zod schema 的 min/max/length 约束）
pub fn validate_sync_report(r: &SyncReport) -> Result<(), String> {
    if r.source_agent.trim().is_empty() {
        return Err("sourceAgent 不能为空".into());
    }
    if r.task_node_id.trim().is_empty() {
        return Err("taskNodeId 不能为空".into());
    }
    if r.statements.is_empty() {
        return Err("statements 至少 1 条".into());
    }
    if r.statements.iter().any(|s| s.text.trim().is_empty()) {
        return Err("statement.text 不能为空".into());
    }
    if r.summary.trim().is_empty() {
        return Err("summary 不能为空".into());
    }
    if r.summary.chars().count() > 2000 {
        return Err("summary 超过 2000 字符".into());
    }
    if !(0.0..=1.0).contains(&r.confidence) {
        return Err("confidence 必须落在 0..=1".into());
    }
    if let Some(cs) = &r.conflicts {
        if cs.iter().any(|c| c.parties.len() < 2) {
            return Err("conflict.parties 至少 2 方".into());
        }
    }
    Ok(())
}

pub fn parse_plan(text: &str) -> Result<OrchestratorPlan, String> {
    let raw = extract_json(text).ok_or_else(|| "未找到 JSON 输出".to_string())?;
    let plan: OrchestratorPlan =
        serde_json::from_value(raw).map_err(|e| format!("OrchestratorPlan 结构校验失败: {e}"))?;

    let mut ids = std::collections::HashSet::new();
    for n in &plan.nodes {
        if !ids.insert(n.id.clone()) {
            return Err(format!("节点 id 重复: {}", n.id));
        }
    }
    for n in &plan.nodes {
        for dep in &n.depends_on {
            if !ids.contains(dep) {
                return Err(format!("节点 {} 依赖不存在的 id {}", n.id, dep));
            }
        }
    }

    if matches!(plan.route_level, crate::types::RouteLevel::L0) {
        let ok = plan
            .final_answer
            .as_ref()
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        if !ok {
            return Err("L0 直达计划必须给出非空 finalAnswer".into());
        }
    } else if plan.nodes.is_empty() {
        return Err("L1 及以上计划的 nodes 不能为空".into());
    }
    Ok(plan)
}

/// 识别工具调用约定：{"tool": "...", "args": {...}}
pub fn as_tool_call(v: &serde_json::Value) -> Option<(crate::types::ToolName, serde_json::Value)> {
    let obj = v.as_object()?;
    let tool = obj.get("tool")?.as_str()?;
    let name = crate::types::ToolName::parse(tool)?;
    let args = obj
        .get("args")
        .cloned()
        .unwrap_or(serde_json::Value::Object(Default::default()));
    Some((name, args))
}

#[cfg(test)]
mod sync_repair_tests {
    use super::*;

    /// 最小合法 SyncReport JSON（修复测试的基线）
    fn base_json() -> serde_json::Value {
        serde_json::json!({
            "sourceAgent": "web-agent",
            "taskNodeId": "T1",
            "status": "done",
            "statements": [
                { "tag": "报告", "text": "检索完成，共 3 个来源支撑结论。第二条陈述用于修复回退文本。" },
                { "tag": "肯定", "text": "数据口径一致。" }
            ],
            "summary": "节点完成。",
            "confidence": 0.8
        })
    }

    fn wrap(v: serde_json::Value) -> String {
        format!("```json\n{}\n```", serde_json::to_string(&v).unwrap())
    }

    /// 缺 summary：首条 statement 截 200 补齐，repaired=true
    #[test]
    fn 修复_缺summary取首条陈述() {
        let mut v = base_json();
        v.as_object_mut().unwrap().remove("summary");
        let r = parse_sync_report(&wrap(v)).unwrap();
        assert_eq!(r.repaired, Some(true));
        assert!(r.summary.starts_with("检索完成"));
        assert!(r.summary.chars().count() <= 200);
    }

    /// 缺 confidence：0.5 补齐
    #[test]
    fn 修复_缺confidence中性值() {
        let mut v = base_json();
        v.as_object_mut().unwrap().remove("confidence");
        let r = parse_sync_report(&wrap(v)).unwrap();
        assert_eq!(r.repaired, Some(true));
        assert_eq!(r.confidence, 0.5);
    }

    /// 嵌套 appendNodes 缺 title/agentIdentifier：别名漂移修复
    #[test]
    fn 修复_嵌套别名漂移() {
        let mut v = base_json();
        v["nextSuggestion"] = serde_json::json!({
            "appendNodes": [{ "objective": "补充检索" }]
        });
        let r = parse_sync_report(&wrap(v)).unwrap();
        assert_eq!(r.repaired, Some(true));
        let nodes = r.next_suggestion.append_nodes.unwrap();
        assert_eq!(nodes[0].title, "");
        assert_eq!(nodes[0].agent_identifier, "");
        assert_eq!(nodes[0].objective, "补充检索");
    }

    /// 核心字段（taskNodeId）缺失：不修复维持原错误
    #[test]
    fn 不修复_核心字段缺失() {
        let mut v = base_json();
        v.as_object_mut().unwrap().remove("taskNodeId");
        let err = parse_sync_report(&wrap(v)).unwrap_err();
        assert!(err.contains("missing field `taskNodeId`"), "{err}");
    }

    /// 类型错误（confidence 传字符串）：不修复
    #[test]
    fn 不修复_类型错误() {
        let mut v = base_json();
        v["confidence"] = serde_json::json!("高");
        let err = parse_sync_report(&wrap(v)).unwrap_err();
        assert!(err.contains("结构校验失败"), "{err}");
        assert!(!err.contains("missing field"), "{err}");
    }

    /// 正常完整报告：不走修复路径（repaired=None）
    #[test]
    fn 正常报告_无修复标记() {
        let r = parse_sync_report(&wrap(base_json())).unwrap();
        assert_eq!(r.repaired, None);
    }
}
