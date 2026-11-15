//! e2e 域文件：审计/事件/资产。（自 e2e.rs 拆出，纯搬移零断言变更）

use super::*;

/// 工具预算熔断事件（agent-budget-audit 3.2）
pub(super) async fn stage_tool_budget_events(e: &Env, c: &mut Checker) -> anyhow::Result<()> {
    let core = &e.core;

// ---- 工具预算熔断事件（agent-budget-audit 3.2）----
// 模型驱动的 chat 在 Mock 下不发工具调用，走工具执行漏斗的直连路径：
// 收紧单轮预算 → execute_named 两次 → 第二次必须受限回灌 + tool.budget 事件广播
// （事件页条目 + 通道 take_reply 内联提示的数据源）。
{
    let mut rx_b = core.subscribe();
    let sid = core.create_session("预算熔断测试").unwrap().id;
    // 预算收紧：单轮 1（config 快照由 FileGuard 兜底还原；块尾恢复默认）。
    // 注意先改配置再取 tools 引用——apply_config 重建 orchestrator（新网关带新预算）。
    let mut cfg_b = (*core.config()).clone();
    cfg_b.security.tool_call_budget.per_turn = 1;
    cfg_b.security.tool_call_budget.per_session = 0;
    cfg_b.security.tool_call_budget.per_window = 0;
    core.apply_config(cfg_b).unwrap();
    let tools = core.orchestrator().tools.clone();
    let allow = [exm_core::types::ToolName::parse("read").unwrap()];
    let args = serde_json::json!({ "path": "README.md" });
    let r1 = tools
        .execute_named("e2e", &sid, &allow, "read", &args)
        .await;
    let r2 = tools
        .execute_named("e2e", &sid, &allow, "read", &args)
        .await;
    // 恢复默认预算（后续断言不受影响）
    let mut cfg_r = (*core.config()).clone();
    cfg_r.security.tool_call_budget = exm_core::config::ToolCallBudget::default();
    core.apply_config(cfg_r).unwrap();
    c.check(
        "预算首次放行二次受限",
        !r1.error.clone().unwrap_or_default().contains("预算受限")
            && r2.error.clone().unwrap_or_default().contains("预算受限"),
    );
    // tool.budget 事件广播（熔断条目数据源）
    let mut budget_evt = false;
    while let Ok(evt) = rx_b.try_recv() {
        if evt.kind == "tool.budget" && evt.session_id == sid {
            budget_evt = true;
        }
    }
    c.check("tool.budget 事件广播", budget_evt);
    let _ = core.delete_session(&sid);
}


    Ok(())
}

/// 事件载荷脱敏（agent-budget-audit 3.5）
pub(super) async fn stage_event_redaction(e: &Env, c: &mut Checker) -> anyhow::Result<()> {
    let core = &e.core;

// ---- 事件载荷脱敏（agent-budget-audit 3.5）----
// 带密钥参数的工具调用：tool.call 事件载荷必须打码（事件库是明文面）
{
    let mut rx_s = core.subscribe();
    let sid = core.create_session("脱敏测试").unwrap().id;
    let tools = core.orchestrator().tools.clone();
    let allow = [exm_core::types::ToolName::parse("terminal").unwrap()];
    // 审批 off + 白名单外 → 落审批单（不执行），但 tool.call 事件已发（含参数）
    let _ = tools
        .execute_named(
            "e2e",
            &sid,
            &allow,
            "terminal",
            &serde_json::json!({
                "command": "curl -H 'Authorization: Bearer abcdefghijklmnop12345' https://x.io",
                "api_key": "sk-abcdefgh12345678"
            }),
        )
        .await;
    let mut leaked = true;
    let mut masked = false;
    while let Ok(evt) = rx_s.try_recv() {
        if evt.kind == "tool.call" && evt.session_id == sid {
            let raw = serde_json::to_string(&evt.payload).unwrap_or_default();
            if raw.contains("abcdefghijklmnop12345") || raw.contains("sk-abcdefgh12345678") {
                leaked = true;
            } else if raw.contains("***") {
                leaked = false;
                masked = true;
            }
        }
    }
    c.check("tool.call 事件密钥脱敏", !leaked && masked);
    let _ = core.delete_session(&sid);
}


    Ok(())
}

/// 审计与用量端点（e2e 断言补强 5.2）：/audit 工具面 + kind=dispatch 派发面 + /sessions/:id/tokens
pub(super) async fn stage_audit_usage(e: &Env, c: &mut Checker) -> anyhow::Result<()> {
    let base = &e.base;
    let client = &e.client;
    let core = &e.core;

// ---- 审计与用量端点（e2e 断言补强 5.2）：/audit 工具面 + kind=dispatch 派发面 + /sessions/:id/tokens ----
{
    // 工具审计面：上面预算/脱敏块已产生真实 tool.call 记录 → items 非空且字段齐
    let audit: serde_json::Value = client
        .get(format!("{base}/audit?limit=50"))
        .header("x-auth-key", "e2e-secret")
        .send()
        .await?
        .json()
        .await?;
    let items = audit["items"].as_array().cloned().unwrap_or_default();
    c.check("工具审计端点返回条目", !items.is_empty());
    // 条目结构：agentId + tool + ok（审计页过滤与高亮的数据源）
    let has_fields = items
        .iter()
        .any(|it| it.get("agentId").is_some() && it.get("tool").is_some() && it.get("ok").is_some());
    c.check("审计条目结构完整（agentId/tool/ok）", has_fields);
    // 派发审计面（kind=dispatch）：跨会话聚合结构（可能为空——结构断言即可）
    let dispatch: serde_json::Value = client
        .get(format!("{base}/audit?kind=dispatch&limit=50"))
        .header("x-auth-key", "e2e-secret")
        .send()
        .await?
        .json()
        .await?;
    c.check("派发审计端点结构有效", dispatch["items"].is_array());
    // 会话用量：既有会话（预算块已跑过工具）→ tokens 返回结构含用量字段
    let any_sid = core
        .list_sessions_in_group("exmachina")
        .unwrap_or_default()
        .iter()
        .find(|s| s.title.contains("channel:e2e-hook") || s.title.contains("预算"))
        .map(|s| s.id.clone())
        .unwrap_or_default();
    let tokens: serde_json::Value = client
        .get(format!("{base}/sessions/{any_sid}/tokens"))
        .header("x-auth-key", "e2e-secret")
        .send()
        .await?
        .json()
        .await?;
    let tok_ok = tokens.get("promptTokens").is_some()
        || tokens.get("prompt_tokens").is_some()
        || tokens.get("total").is_some()
        || tokens.get("used").is_some();
    c.check("会话用量端点返回结构", tok_ok);
}


    Ok(())
}

/// 资产下载语义（REST 通用约定：404 不存在 / 403 越界口径分离）
pub(super) async fn stage_asset_semantics(e: &Env, c: &mut Checker) -> anyhow::Result<()> {
    let base = &e.base;
    let client = &e.client;

// ---- 资产下载语义（REST 通用约定：404 不存在 / 403 越界口径分离）----
{
    let dl404: reqwest::Response = client
        .get(format!("{base}/asset/download?path=no-such-file.txt"))
        .header("x-auth-key", "e2e-secret")
        .send()
        .await?;
    c.check("资产下载：不存在文件 404", dl404.status().as_u16() == 404);
    let dl403: reqwest::Response = client
        .get(format!("{base}/asset/download?path=../../etc/passwd"))
        .header("x-auth-key", "e2e-secret")
        .send()
        .await?;
    c.check("资产下载：路径越界 403", dl403.status().as_u16() == 403);
}


    Ok(())
}
