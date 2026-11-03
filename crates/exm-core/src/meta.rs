//! meta.rs —— Meta Agent 数据聚合层（meta-agent 变更）
//! 纯数据层:从既有工具审计存储聚合按 agent 维度的运行统计,
//! 供 meta.observe 工具与 meta_agent_tick 的 Eval 自评共用。零 LLM 依赖。

use crate::store::Store;
use std::collections::BTreeMap;

/// 单工具统计（按 agent 维度聚合）
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolStats {
    pub calls: u64,
    pub failures: u64,
    /// 平均耗时（毫秒）
    pub avg_ms: u64,
    /// 最近一次失败摘要
    pub last_error: Option<String>,
    /// 聚合期内部累计（序列化跳过）
    #[serde(skip)]
    pub _total_ms: u64,
}

/// 单 agent 观测视图
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentObservation {
    pub identifier: String,
    pub tool_stats: BTreeMap<String, ToolStats>,
    /// 最近失败（工具, 错误摘要）× 最多 10
    pub recent_failures: Vec<(String, String)>,
    pub adaptation_revision: Option<u32>,
    pub adaptation_updated_at: Option<String>,
}

/// 系统观测视图（meta.observe 输出）
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemObservation {
    pub agents: Vec<AgentObservation>,
    /// 统计窗口描述
    pub window: String,
    pub generated_at: String,
}

use crate::registry::LocalRegistry;

/// 聚合系统观测（纯同步查询,7 天窗口）：
/// - 工具统计从审计存储读取（新条目带 ok 位;旧条目缺省视为成功）
/// - adaptation 现行版本来自注册表
pub fn system_observation(store: &Store, registry: &LocalRegistry) -> SystemObservation {
    let mut agents: std::collections::BTreeMap<String, AgentObservation> = Default::default();
    let entries = store
        .list_tool_audit_filtered(None, None, 20_000)
        .unwrap_or_default();
    let cutoff = days_ago_iso(7);
    for e in entries {
        let Some(created) = e.get("createdAt").and_then(|v| v.as_str()) else {
            continue;
        };
        if created < cutoff.as_str() {
            continue;
        }
        let Some(agent_id) = e.get("agentId").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(tool) = e.get("tool").and_then(|v| v.as_str()) else {
            continue;
        };
        let ok = e.get("ok").and_then(|v| v.as_bool()).unwrap_or(true);
        let dur = e.get("durationMs").and_then(|v| v.as_u64()).unwrap_or(0);
        let summary = e
            .get("resultSummary")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let obs = agents.entry(agent_id.to_string()).or_insert_with(|| AgentObservation {
            identifier: agent_id.to_string(),
            tool_stats: BTreeMap::new(),
            recent_failures: Vec::new(),
            adaptation_revision: None,
            adaptation_updated_at: None,
        });
        let st = obs.tool_stats.entry(tool.to_string()).or_insert(ToolStats {
            calls: 0,
            failures: 0,
            avg_ms: 0,
            last_error: None,
            _total_ms: 0,
        });
        st.calls += 1;
        st._total_ms += dur;
        if !ok {
            st.failures += 1;
            st.last_error = Some(summary.chars().take(120).collect());
            obs.recent_failures
                .push((tool.to_string(), summary.chars().take(80).collect()));
        }
    }
    for obs in agents.values_mut() {
        for st in obs.tool_stats.values_mut() {
            st.avg_ms = st._total_ms.checked_div(st.calls).unwrap_or(0);
        }
        obs.recent_failures.truncate(10);
        if let Ok(Some(ad)) = registry.load_adaptation(&obs.identifier) {
            obs.adaptation_revision = Some(ad.revision);
            obs.adaptation_updated_at = Some(ad.updated_at);
        }
    }
    SystemObservation {
        agents: agents.into_values().collect(),
        window: "7d".into(),
        generated_at: crate::types::now_iso(),
    }
}

/// ISO 时间戳 N 天前的截断比较串（粗粒度窗口过滤,精确判定无需时区库）
fn days_ago_iso(days: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let past = now - (days as i64) * 86_400;
    // epoch → YYYY-MM-DDTHH:MM:SS(UTC,纯算术,免 chrono 依赖)
    let days = past.div_euclid(86_400);
    let secs = past.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Howard Hinnant 的 days→civil 算法（ chrono 免依赖的公历换算）
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 聚合_统计与失败率() {
        let dir = std::env::temp_dir().join(format!("exm-meta-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir).unwrap();
        // agent a1:web_search 3 调 1 失;terminal 1 调全成
        store
            .audit_tool_ex("a1", "web_search", &serde_json::json!({}), "ok r1", 100, true)
            .unwrap();
        store
            .audit_tool_ex("a1", "web_search", &serde_json::json!({}), "timeout", 300, false)
            .unwrap();
        store
            .audit_tool_ex("a1", "web_search", &serde_json::json!({}), "ok r3", 200, true)
            .unwrap();
        store
            .audit_tool_ex("a1", "terminal", &serde_json::json!({}), "done", 50, true)
            .unwrap();
        let registry = crate::registry::LocalRegistry::new(&dir).unwrap();
        let obs = system_observation(&store, &registry);
        let a1 = obs.agents.iter().find(|a| a.identifier == "a1").expect("a1");
        let ws = a1.tool_stats.get("web_search").expect("web_search");
        assert_eq!(ws.calls, 3);
        assert_eq!(ws.failures, 1);
        assert_eq!(ws.avg_ms, 200); // (100+300+200)/3
        assert_eq!(a1.recent_failures.len(), 1);
        assert_eq!(a1.recent_failures[0].0, "web_search");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 聚合_旧条目缺ok视为成功() {
        let dir = std::env::temp_dir().join(format!("exm-meta-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir).unwrap();
        // 旧 schema:audit_tool(无 ok 位)
        store
            .audit_tool("a2", "grep", &serde_json::json!({}), "3 matches", 30)
            .unwrap();
        let registry = crate::registry::LocalRegistry::new(&dir).unwrap();
        let obs = system_observation(&store, &registry);
        let a2 = obs.agents.iter().find(|a| a.identifier == "a2").expect("a2");
        let g = a2.tool_stats.get("grep").expect("grep");
        assert_eq!(g.failures, 0, "旧条目缺 ok 位应视为成功");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 时间窗口_七天截止串格式() {
        let s = days_ago_iso(7);
        assert_eq!(s.len(), 19, "YYYY-MM-DDTHH:MM:SS: {s}");
        assert!(s.starts_with("20"), "{s}");
        // 0 天前 ≈ 现在(2026)
        let now = days_ago_iso(0);
        assert!(now >= s, "7 天前应早于现在: {s} vs {now}");
    }
}

// ---------------------------------------------------------------- Meta Agent 巡检（meta-agent）

/// Meta Eval 提示词：判断「有没有值得改进的点」（数据快照由调用方注入用户消息）
pub const META_EVAL_PROMPT: &str = r#"你是系统的元智能体巡评体（Meta Agent Eval）。基于下方的运行数据快照，判断**现在有没有值得提出改进的点**。

判定口径（保守优先——不确定 = 不提）：
- 某工具失败率明显高（如 ≥30% 且失败 ≥3 次）→ 值得分析失败模式,可能需要经验要点或提示词补段；
- 同类失败近期重复出现 → 值得沉淀 adaptation；
- 明显的能力缺口（任务反复失败但无适配个体）→ 可考虑 build 提议；
- 数据平稳、无异常模式 → 不提议（act=false）。

只输出 JSON：
{"act": true/false, "focus": "值得关注的 agent 或现象（act=true 时必填）", "why": "简述依据"}
不确定 = 不做。"#;

/// 解析 Meta Eval 输出：{"act": bool, "focus": string, "why": string}
pub fn parse_meta_eval(output: &str) -> Option<(String, String)> {
    let trimmed = output.trim();
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end <= start {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&trimmed[start..=end]).ok()?;
    if !v.get("act")?.as_bool()? {
        return None;
    }
    let focus = v.get("focus")?.as_str()?.trim().to_string();
    if focus.is_empty() {
        return None;
    }
    let why = v.get("why").and_then(|x| x.as_str()).unwrap_or("").to_string();
    Some((focus, why))
}

/// Meta Agent 单次巡检的行动系统提示（自主会话用）
pub const META_ACT_PROMPT: &str = r#"你是元智能体（Meta Agent），正在自主巡检会话中。工作方式：

1. 先用 meta_observe 读运行数据（全体或聚焦个体），把判断落在数据上；
2. 有明确改进点 → 用 meta_propose / meta_build 落提议单（附数据依据）；
3. 无充分依据 → 不提议（宁缺毋滥,提议质量决定用户信任）；
4. 一次巡检最多落 2 张提议单;完成后简要总结发现（无论是否提议）。

纪律：所有持久改动都须经人工审批——你不直接改任何定义,只负责「观测 → 提议」。"#;

#[cfg(test)]
mod eval_tests {
    use super::*;

    #[test]
    fn 解析_meta_eval() {
        let (f, w) = parse_meta_eval(r#"{"act": true, "focus": "researcher", "why": "web_search 失败率 42%"}"#).unwrap();
        assert_eq!(f, "researcher");
        assert!(w.contains("42%"));
        // act=false / 围栏 / 空 focus
        assert!(parse_meta_eval(r#"{"act": false}""#).is_none());
        assert!(parse_meta_eval("```json\n{\"act\": true, \"focus\": \"x\", \"why\": \"\"}\n```").is_some());
        assert!(parse_meta_eval(r#"{"act": true, "focus": "", "why": ""}"#).is_none());
        assert!(parse_meta_eval("非 JSON 输出").is_none());
    }
}
