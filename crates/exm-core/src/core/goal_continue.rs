//! goal 自动继续（capability-parity 4.2）：有界推进 + 恢复 admission。
//!
//! 设计（复用既有机制，不新开执行路径）：
//! - **admission**：仅用户明确 start/resume 置 `GoalState.auto_continue=true`
//!   （pause/block/complete/clear 收回）。未授权的 Active goal 绝不自动续跑。
//! - **推进轮**：复用 `chat`（完整模型路由/工具循环/预算/事件链路），提示词
//!   注入 goal 纪律（推进到下一可验证增量即收束）。
//! - **judge 裁决**：非流式一次调用，输出结构化 JSON
//!   `{"verdict": "done|blocked|needs_user|continue", "evidence": "…"}`；
//!   解析失败按 needs_user 处理（保守停，不虚报）。
//! - **有界**：轮数（goal_continue_max_rounds）/单轮步数（复用步数预算）/
//!   单轮时间（timeout 包裹）/token 预算（GoalState.token_budget × 已用对照）。
//!   任一耗尽 → block + note 说明，不无限自我续跑。
//! - **运行中不打断**：会话已在跑（round_trace::is_running）时本轮 tick 跳过；
//!   运行中改目标不静默替换当前轮次（edit 只改状态，下一次 tick 才生效）。

use crate::Core;
use serde_json::json;

/// judge 提示词（裁决当前 goal 是否完成/受阻/需用户决策/继续）。
pub const GOAL_JUDGE_PROMPT: &str = r#"你是 goal 裁决体（judge）。根据目标与会话最新进展，裁决当前 goal 的状态。

裁决纪律：
- "done"：目标已达成，且进展中有可指认的验证证据（测试/构建/产物引用）；
- "blocked"：外部依赖缺失/权限不足/连续失败，本地无法推进；
- "needs_user"：需要用户设计决策或授权才能继续；
- "continue"：还有明确可推进的下一步（默认兜底）。

诚实优先：证据不足不许判 done；不确定判 needs_user 而不是猜。

只输出一个 JSON（不要任何其他文本）：
{"verdict": "done|blocked|needs_user|continue", "evidence": "一句话证据或原因"}"#;

/// 推进轮系统提示（工作法：一轮 = 一个可验证增量）。
pub const GOAL_CONTINUE_DISCIPLINE: &str = "## 工作法（goal 自动继续轮）

本会话有一个持久 goal，你正在自动继续的一轮里。本轮纪律：

- **一个可验证增量**：选择能推进目标的最小完整一步（实现+验证），做完即收束；
  不贪多、不开与新并行线；
- **诚实汇报**：收束回复说明本轮做了什么、验证证据（命令输出/测试结果/产物路径）、
  下一步建议；未完成就明说未完成；
- **需要决策即停**：遇到必须用户裁决的分叉，输出明确的问题与候选，不要自行猜测重大取舍；
- **预算意识**：工具调用精打细算，重复失败的路径换方法而不是硬重试。";

/// 解析 judge 输出（容错：markdown 围栏/前后缀噪声中提取首个 JSON 对象）。
/// 解析失败返回 None（调用方按 needs_user 保守处理）。
pub fn parse_goal_judge(output: &str) -> Option<(String, String)> {
    let text = output.trim();
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    let raw = &text[start..=end];
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let verdict = v.get("verdict").and_then(|x| x.as_str())?.to_string();
    if !matches!(
        verdict.as_str(),
        "done" | "blocked" | "needs_user" | "continue"
    ) {
        return None;
    }
    let evidence = v
        .get("evidence")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    Some((verdict, evidence))
}

/// 一次 goal 继续决策（tick 内对单个会话的完整处理）。
/// 返回 Some(verdict) 表示执行了一轮推进；None 表示本 tick 无事可做。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn goal_continue_once(
    core: &Core,
    session_id: &str,
    timeout_secs: u64,
) -> Option<String> {
    let cfg = core.config();
    let max_rounds = cfg.automation.goal_continue_max_rounds as u64;
    let mut goal = core.goal_get(session_id).ok().flatten()?;
    // admission：未明确授权（start/resume）不自动续跑
    if !goal.auto_continue || goal.status != crate::workflow::GoalStatus::Active {
        return None;
    }
    // 有界：轮数上限
    if goal.continue_rounds_used >= max_rounds {
        let _ = core.goal_update(
            session_id,
            "block",
            None,
            Some(&format!(
                "自动继续轮数上限（{max_rounds}）已耗尽；请 resume 重新授权或 clear"
            )),
            None,
            None,
        );
        core.emit_session_event(
            session_id,
            "goal.continue",
            json!({"status": "blocked", "reason": "rounds_limit", "rounds": goal.continue_rounds_used}),
        );
        return Some("blocked".into());
    }
    // 运行中不打断：会话忙（用户正在交互或上一轮未收束）时跳过本 tick
    if crate::round_trace::is_running(session_id) {
        return None;
    }

    // 推进轮：goal 纪律 + 目标 + 验收口径（复用完整 chat 链路）
    let prompt = format!(
        "{}\n\n## 当前 goal\n目标：{}\n验收：{}\n已完成轮数：{}",
        GOAL_CONTINUE_DISCIPLINE,
        goal.objective,
        if goal.acceptance.is_empty() {
            "（未声明——以目标语义自验）".to_string()
        } else {
            goal.acceptance.join("；")
        },
        goal.continue_rounds_used,
    );
    core.emit_session_event(
        session_id,
        "goal.continue",
        json!({"status": "running", "round": goal.continue_rounds_used + 1}),
    );
    let round = if timeout_secs > 0 {
        tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            core.chat(session_id, &prompt),
        )
        .await
        .map_err(|_| anyhow::anyhow!("goal 自动继续单轮超时（{timeout_secs}s）"))
        .and_then(|r| r)
    } else {
        core.chat(session_id, &prompt).await
    };
    if let Err(e) = round {
        // 运行错误：如实收束（block + note），不虚报不重试
        let _ = core.goal_update(
            session_id,
            "block",
            None,
            Some(&format!("自动继续失败：{e}")),
            None,
            None,
        );
        core.emit_session_event(
            session_id,
            "goal.continue",
            json!({"status": "blocked", "reason": "error", "error": e.to_string()}),
        );
        return Some("blocked".into());
    }
    // 推进成功：写回轮数（有界计数）
    let rounds_used = core.goal_continue_step(session_id).unwrap_or_default();

    // token 预算对照（GoalState.token_budget × 会话累计用量：usage_total 是
    // 快照差额法的权威累计源，与 TurnSnapshot 同口径）
    if let Some(budget) = core
        .goal_get(session_id)
        .ok()
        .flatten()
        .and_then(|g| g.token_budget)
    {
        let (p, c, _) = core.store.usage_total(session_id);
        let used = p + c;
        if used >= budget {
            apply_verdict(
                core,
                session_id,
                "needs_user",
                &format!("token 预算耗尽（{used}/{budget}）；请确认后 resume 或调整预算"),
            );
            core.emit_session_event(
                session_id,
                "goal.continue",
                json!({"status": "judged", "verdict": "needs_user", "reason": "budget", "round": rounds_used}),
            );
            return Some("needs_user".into());
        }
    }

    // judge：非流式一次（工具不可用——裁决只读判断）
    goal = core.goal_get(session_id).ok().flatten()?;
    let orch = core.orchestrator();
    let latest = core.store.list_messages(session_id, 4).unwrap_or_default();
    let progress: String = latest
        .iter()
        .rev()
        .filter_map(|m| {
            m.statements
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>()
                .join("\n")
                .into()
        })
        .next()
        .unwrap_or_default();
    let req = crate::provider::ChatRequest::new(
        orch.orch_model.clone(),
        vec![
            crate::provider::ChatMessage::system(GOAL_JUDGE_PROMPT),
            crate::provider::ChatMessage::user(format!(
                "目标：{}\n验收：{}\n最新进展：\n{}",
                goal.objective,
                goal.acceptance.join("；"),
                progress.chars().take(4_000).collect::<String>(),
            )),
        ],
    );
    let verdict = match orch.orch_provider.chat(req).await {
        Ok(r) => {
            let (verdict, evidence) = parse_goal_judge(&r.content)
                .unwrap_or_else(|| ("needs_user".into(), "judge 输出无法解析（保守停）".into()));
            apply_verdict(core, session_id, &verdict, &evidence);
            verdict
        }
        Err(e) => {
            // judge 失败：保守 needs_user（不虚报 done，也不盲目 continue）
            apply_verdict(
                core,
                session_id,
                "needs_user",
                &format!("judge 调用失败：{e}"),
            );
            "needs_user".into()
        }
    };
    core.emit_session_event(
        session_id,
        "goal.continue",
        json!({"status": "judged", "verdict": verdict, "round": goal.continue_rounds_used}),
    );
    Some(verdict)
}

/// 裁决落地：done→complete（带证据）；blocked/needs_user→block/pause + note；
/// continue→仅累计轮数（下一 tick 继续）。
fn apply_verdict(core: &Core, session_id: &str, verdict: &str, evidence: &str) {
    match verdict {
        "done" => {
            let _ = core.goal_update(session_id, "complete", None, None, Some(evidence), None);
        }
        "blocked" => {
            let _ = core.goal_update(session_id, "block", None, Some(evidence), None, None);
        }
        "needs_user" => {
            // 需要用户决策：pause（不丢目标，收回自动继续授权等 resume）
            let _ = core.goal_update(session_id, "pause", None, Some(evidence), None, None);
        }
        _ => {}
    }
}

impl Core {
    /// goal 自动继续 tick（cron_scheduler 驱动）：扫描有 Active goal 的会话，
    /// 逐个判定 admission 与界限后推进一轮。返回处理的会话数。
    /// 单会话串行（goal 轮自带锁语义）；会话间互不阻塞（失败跳下一个）。
    pub async fn goal_continue_tick(&self, now_minute: i64) -> usize {
        let cfg = self.config();
        let interval = cfg.automation.goal_continue_interval_minutes as i64;
        if interval <= 0 {
            return 0;
        }
        if now_minute % interval != 0 {
            return 0;
        }
        let sessions = match self.list_sessions() {
            Ok(s) => s,
            Err(_) => return 0,
        };
        let mut handled = 0usize;
        for s in sessions {
            let Some(goal) = self.goal_get(&s.id).ok().flatten() else {
                continue;
            };
            if !goal.auto_continue || goal.status != crate::workflow::GoalStatus::Active {
                continue;
            }
            // 累计轮数在推进后写回（goal_update 递增 revision），这里只做 tick 级
            // 去重：本轮已达到上限的 goal 已在 once 里 block。
            let timeout = cfg.automation.goal_continue_round_timeout_secs;
            if goal_continue_once(self, &s.id, timeout).await.is_some() {
                handled += 1;
            }
        }
        handled
    }

    /// 显式恢复 admission（用户命令面）：resume 时由 CLI/panel 调用
    /// （goal_update 已置 auto_continue；这里只补 continue_rounds_used 复位语义
    /// ——paused/blocked 恢复不重置已耗轮数，只有 clear+start 才重新计数）。
    pub fn goal_continue_admission(&self, session_id: &str) -> Option<bool> {
        self.goal_get(session_id)
            .ok()
            .flatten()
            .map(|g| g.auto_continue && g.status == crate::workflow::GoalStatus::Active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn judge输出解析容错围栏与噪声() {
        // 纯 JSON
        assert_eq!(
            parse_goal_judge(r#"{"verdict":"done","evidence":"tests pass"}"#),
            Some(("done".into(), "tests pass".into()))
        );
        // markdown 围栏
        assert_eq!(
            parse_goal_judge(
                "```json\n{\"verdict\":\"continue\",\"evidence\":\"下一步写测试\"}\n```"
            ),
            Some(("continue".into(), "下一步写测试".into()))
        );
        // 前后噪声
        assert_eq!(
            parse_goal_judge(
                "裁决如下：{\"verdict\":\"needs_user\",\"evidence\":\"需选择方案\"} 以上。"
            ),
            Some(("needs_user".into(), "需选择方案".into()))
        );
        // 非法 verdict / 缺字段 / 非 JSON → None（保守）
        assert_eq!(parse_goal_judge(r#"{"verdict":"maybe"}"#), None);
        assert_eq!(parse_goal_judge(r#"{"evidence":"x"}"#), None);
        assert_eq!(parse_goal_judge("不是 JSON"), None);
    }

    #[test]
    fn judge缺evidence字段容错为空() {
        assert_eq!(
            parse_goal_judge(r#"{"verdict":"blocked"}"#),
            Some(("blocked".into(), String::new()))
        );
    }
}
