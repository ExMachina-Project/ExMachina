//! e2e 域文件：智能体编辑与设置兼容。（自 e2e.rs 拆出，纯搬移零断言变更）

use super::*;

/// 智能体编辑（docs/09）：受限字段更新（name/domain/description/capabilities/tools/model_hint）；identifier/tier/prompt_file 不可改
pub(super) async fn stage_agent_edit(e: &Env, c: &mut Checker) -> anyhow::Result<()> {
    let base = &e.base;
    let client = &e.client;

// ---- 智能体编辑（docs/09）：受限字段更新（name/domain/description/capabilities/tools/model_hint）；identifier/tier/prompt_file 不可改 ----
let orch_upd: serde_json::Value = client
    .put(format!("{base}/agents/orchestrator"))
    .json(&serde_json::json!({
        "name": "e2e-改名验证",
        "capabilities": ["routing", "e2e-cap"]
    }))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "内置组主智能体受限编辑放行（identifier/tier 不变）",
    orch_upd["name"] == "e2e-改名验证"
        && orch_upd["identifier"] == "orchestrator"
        && orch_upd["tier"] == "orchestrator"
        && orch_upd["capabilities"]
            .as_array()
            .map(|a| a.contains(&json!("e2e-cap")))
            .unwrap_or(false),
);
let orch_restore: serde_json::Value = client
    .put(format!("{base}/agents/orchestrator"))
    .json(&serde_json::json!({
        "name": "指挥体",
        "capabilities": ["routing", "decomposition", "dispatch", "arbitration", "convergence"]
    }))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "内置组主智能体名称归位（指挥体）",
    orch_restore["name"] == "指挥体",
);
let _single_new: serde_json::Value = client
    .post(format!("{base}/singles"))
    .json(&serde_json::json!({
        "name": "e2e 单体", "identifier": "e2e-single", "description": "e2e 单体描述"
    }))
    .send()
    .await?
    .json()
    .await?;
let single_upd: serde_json::Value = client
    .put(format!("{base}/singles/e2e-single"))
    .json(&serde_json::json!({
        "name": "e2e 单体·改", "domain": "测试域",
        "capabilities": ["cap-a", "cap-b"]
    }))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "单体编辑：名称/领域/能力标签生效",
    single_upd["name"] == "e2e 单体·改"
        && single_upd["domain"] == "测试域"
        && single_upd["capabilities"]
            .as_array()
            .map(|a| a.len() == 2)
            .unwrap_or(false),
);
// 单体人设（SOUL.MD）：与组内个体同语义，落盘 agents/singles/personas/<id>.md
let _soul_put: serde_json::Value = client
    .put(format!("{base}/singles/e2e-single/persona"))
    .json(&serde_json::json!({ "persona": "e2e 灵魂：客观简洁零情绪" }))
    .send()
    .await?
    .json()
    .await?;
let soul_get: serde_json::Value = client
    .get(format!("{base}/singles/e2e-single/persona"))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "单体人设写入并回读（custom=true）",
    soul_get["persona"]
        .as_str()
        .map(|s| s.trim() == "e2e 灵魂：客观简洁零情绪")
        .unwrap_or(false)
        && soul_get["custom"] == true,
);
let _soul_reset: serde_json::Value = client
    .delete(format!("{base}/singles/e2e-single/persona"))
    .send()
    .await?
    .json()
    .await?;
let soul_def: serde_json::Value = client
    .get(format!("{base}/singles/e2e-single/persona"))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "单体人设重置回落默认（custom=false）",
    soul_def["custom"] == false
        && soul_def["persona"]
            .as_str()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false),
);
let _ = client
    .delete(format!("{base}/singles/e2e-single"))
    .send()
    .await;


    Ok(())
}

/// 设置页保存兼容（D1/D6/D8）：execAllowlist 双形态、search key 三态、memory.mdMaxChars
pub(super) async fn stage_settings_compat(e: &Env, c: &mut Checker) -> anyhow::Result<()> {
    let base = &e.base;
    let client = &e.client;

// ---- 设置页保存兼容（D1/D6/D8）：execAllowlist 双形态、search key 三态、memory.mdMaxChars ----
let d1_put: serde_json::Value = client
    .put(format!("{base}/config"))
    .json(&serde_json::json!({
        "security": { "execApproval": "risky", "execAllowlist": "git status, cargo test" }
    }))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "设置页保存 200（execAllowlist 逗号字符串）",
    d1_put["ok"] == true,
);
let d1_cfg: serde_json::Value = client
    .get(format!("{base}/config"))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "execAllowlist 字符串已转数组生效",
    d1_cfg["security"]["execAllowlist"]
        .as_array()
        .map(|a| a.len() == 2)
        .unwrap_or(false),
);
let d1_arr: serde_json::Value = client
    .put(format!("{base}/config"))
    .json(&serde_json::json!({ "security": { "execApproval": "risky", "execAllowlist": ["echo hi"] } }))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "设置页保存 200（execAllowlist 数组形态）",
    d1_arr["ok"] == true,
);
let d6_set: serde_json::Value = client
    .put(format!("{base}/config"))
    .json(&serde_json::json!({
        "search": { "provider": "tavily", "endpoint": "", "apiKey": "sk-search-e2e", "maxResults": 5 }
    }))
    .send()
    .await?
    .json()
    .await?;
c.check("搜索 API Key 明文写入 200", d6_set["ok"] == true);
let _d6_clear: serde_json::Value = client
    .put(format!("{base}/config"))
    .json(&serde_json::json!({
        "search": { "provider": "tavily", "endpoint": "", "apiKey": "", "maxResults": 5 }
    }))
    .send()
    .await?
    .json()
    .await?;
let d6_cfg: serde_json::Value = client
    .get(format!("{base}/config"))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "搜索 API Key 显式空串已清除",
    d6_cfg["search"]["apiKey"] == "",
);
let d8_put: serde_json::Value = client
    .put(format!("{base}/config"))
    .json(&serde_json::json!({ "memory": { "mdMaxChars": 6000 } }))
    .send()
    .await?
    .json()
    .await?;
c.check("memory.mdMaxChars 写入 200", d8_put["ok"] == true);
let d8_cfg: serde_json::Value = client
    .get(format!("{base}/config"))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "memory.mdMaxChars 回显运行值",
    d8_cfg["memory"]["mdMaxChars"] == 6000,
);
let d12_cron: serde_json::Value = client
    .get(format!("{base}/cron"))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "cron 清单不含内部心跳条目",
    d12_cron
        .as_array()
        .map(|a| !a.iter().any(|j| j["id"] == "__heartbeat__"))
        .unwrap_or(false),
);


    Ok(())
}

/// 单体人设注入（追加）：plan 与 converge 两个拼装点的 system prompt 均须含 single_persona
pub(super) async fn stage_single_persona(e: &Env, c: &mut Checker) -> anyhow::Result<()> {
    let fake_port = e.fake_port.get().copied().unwrap_or(0);
    let prompt_log = &e.prompt_log;
    let base = &e.base;
    let client = &e.client;

// ---- 单体人设注入（追加）：plan 与 converge 两个拼装点的 system prompt 均须含 single_persona ----
// 让指挥体（plan/converge 的 orch provider）指向 fake：热更新全局 llm 通道
let _orch_fake: serde_json::Value = client
    .put(format!("{base}/config"))
    .json(&serde_json::json!({
        "llm": { "baseUrl": format!("http://127.0.0.1:{fake_port}/v1"), "apiKey": "sk-orch", "model": "fake-large" }
    }))
    .send()
    .await?
    .json()
    .await?;
let _soul_single: serde_json::Value = client
    .post(format!("{base}/singles"))
    .json(&serde_json::json!({
        "name": "e2e 灵魂体", "identifier": "e2e-soul", "description": "单体人设注入验证"
    }))
    .send()
    .await?
    .json()
    .await?;
let _soul_set: serde_json::Value = client
    .put(format!("{base}/singles/e2e-soul/persona"))
    .json(&serde_json::json!({ "persona": "e2e 灵魂标记 XYZQ：说话必须冷峻" }))
    .send()
    .await?
    .json()
    .await?;
let _tgt: serde_json::Value = client
    .put(format!("{base}/target"))
    .json(&serde_json::json!({ "mode": "single", "id": "e2e-soul" }))
    .send()
    .await?
    .json()
    .await?;
let soul_session: serde_json::Value = client
    .post(format!("{base}/sessions"))
    .json(&serde_json::json!({ "title": "e2e-soul" }))
    .send()
    .await?
    .json()
    .await?;
let soul_sid = soul_session["id"].as_str().unwrap_or_default().to_string();
// 第一轮：消耗 sk-orch 的首次 429（handler 仍会记录 system prompt）
let _ = client
    .post(format!("{base}/sessions/{soul_sid}/chat"))
    .json(&serde_json::json!({ "text": "人设注入第一轮（消耗首次限额）" }))
    .send()
    .await?;
tokio::time::sleep(Duration::from_millis(2000)).await;
// 第二轮：plan 与 converge 均成功，各记录一段 system prompt
let _ = client
    .post(format!("{base}/sessions/{soul_sid}/chat"))
    .json(&serde_json::json!({ "text": "人设注入第二轮" }))
    .send()
    .await?;
tokio::time::sleep(Duration::from_millis(2500)).await;
{
    let pl = prompt_log.lock().clone();
    let tail = pl.iter().rev().take(3).cloned().collect::<Vec<_>>();
    c.check(
        "单体人设注入：plan/converge 的 system prompt 均含人设文本",
        tail.len() >= 3
            && tail
                .iter()
                .all(|s| s.contains("e2e 灵魂标记 XYZQ") && s.contains("## SOUL（人格）")),
    );
}
let _soul_reset2: serde_json::Value = client
    .delete(format!("{base}/singles/e2e-soul/persona"))
    .send()
    .await?
    .json()
    .await?;
let _ = client
    .post(format!("{base}/sessions/{soul_sid}/chat"))
    .json(&serde_json::json!({ "text": "人设重置后回落默认" }))
    .send()
    .await?;
tokio::time::sleep(Duration::from_millis(2500)).await;
{
    let pl = prompt_log.lock().clone();
    let tail = pl.iter().rev().take(2).cloned().collect::<Vec<_>>();
    c.check(
        "单体人设重置：回落默认人设且不再含自定义文本",
        tail.len() >= 2
            && tail
                .iter()
                .all(|s| s.contains("智械体") && !s.contains("e2e 灵魂标记 XYZQ")),
    );
}
let _tgt_back: serde_json::Value = client
    .put(format!("{base}/target"))
    .json(&serde_json::json!({ "mode": "group" }))
    .send()
    .await?
    .json()
    .await?;
let _ = client
    .delete(format!("{base}/singles/e2e-soul"))
    .send()
    .await;


    Ok(())
}
