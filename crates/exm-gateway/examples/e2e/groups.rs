//! e2e 域文件：智能体组与编成。（自 e2e.rs 拆出，纯搬移零断言变更）

use super::*;

/// 智能体组：创建 → 切换 → 主智能体直答 → 切回
pub(super) async fn stage_agents_group(e: &Env, c: &mut Checker) -> anyhow::Result<()> {
    let agents_len = e.agents_len;
    let base = &e.base;
    let client = &e.client;
    let core = &e.core;
    let sid = &e.sid;

// ---- 智能体组：创建 → 切换 → 主智能体直答 → 切回 ----
let groups0: serde_json::Value = client
    .get(format!("{base}/groups"))
    .send()
    .await?
    .json()
    .await?;
c.check("默认组存在且激活", groups0["active"] == "exmachina");

let g: serde_json::Value = client
    .post(format!("{base}/groups"))
    .json(
        &serde_json::json!({"id": "product", "name": "产品组", "description": "端到端演示组"}),
    )
    .send()
    .await?
    .json()
    .await?;
c.check(
    "创建自定义组",
    g["id"] == "product" && g["builtin"] == false,
);

let sw = client
    .put(format!("{base}/groups/active"))
    .json(&serde_json::json!({"id": "product"}))
    .send()
    .await?;
c.check("切换激活组", sw.status().is_success());

// 组级能力模型覆盖：写入（空串=清除该项）→ 组元数据回读 → 清空整块收回
let caps: serde_json::Value = client
    .put(format!("{base}/groups/product/capabilities"))
    .json(&serde_json::json!({
        "speech": "default/tts-x",
        "transcribe": "",
        "visionRelay": "default/vision-x",
        "embedding": "exmachina"
    }))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "组级能力覆盖写入",
    caps["ok"] == true
        && caps["capabilities"]["speech"] == "default/tts-x"
        && caps["capabilities"]["visionRelay"] == "default/vision-x"
        && caps["capabilities"]["embedding"] == "exmachina"
        && caps["capabilities"]["transcribe"].is_null(),
);
let groups1: serde_json::Value = client
    .get(format!("{base}/groups"))
    .send()
    .await?
    .json()
    .await?;
let product_caps = groups1["groups"]
    .as_array()
    .and_then(|a| a.iter().find(|g| g["id"] == "product"))
    .map(|g| g["capabilities"]["speech"] == "default/tts-x")
    .unwrap_or(false);
c.check("组级能力覆盖落盘回读", product_caps);
let caps_clr: serde_json::Value = client
    .put(format!("{base}/groups/product/capabilities"))
    .json(&serde_json::json!({ "speech": "", "transcribe": "", "visionRelay": "", "embedding": "" }))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "组级能力覆盖清空收回",
    caps_clr["ok"] == true && caps_clr["capabilities"].is_null(),
);

let agents_g: serde_json::Value = client
    .get(format!("{base}/agents"))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "新组初始为空",
    agents_g.as_array().map(|a| a.is_empty()).unwrap_or(false),
);

// 只创建一个主智能体（组内首个个体 → 自动成为 primary）
let lead: serde_json::Value = client
    .post(format!("{base}/agents"))
    .json(&serde_json::json!({
        "name": "产品主智能体", "identifier": "product-lead", "tier": "orchestrator",
        "description": "产品组主智能体，直接对接用户"
    }))
    .send()
    .await?
    .json()
    .await?;
c.check("创建主智能体", lead["identifier"] == "product-lead");

// 组内无子个体 → 主智能体 L0 直答（模拟通道）
let mut rx2 = core.subscribe();
let collector2 = tokio::spawn(async move {
    let mut seen = Vec::new();
    while let Ok(evt) = rx2.recv().await {
        seen.push(evt.kind.clone());
        if evt.kind == "run.finished" || evt.kind == "run.error" {
            break;
        }
    }
    seen
});
let resp2 = client
    .post(format!("{base}/sessions/{sid}/chat"))
    .json(&serde_json::json!({"text": "给出产品组的自我介绍"}))
    .send()
    .await?;
c.check("自定义组 chat 202", resp2.status().as_u16() == 202);
let kinds2 = tokio::time::timeout(Duration::from_secs(60), collector2).await??;
c.check(
    "自定义组 L0 直答完成",
    kinds2.iter().any(|k| k == "run.finished"),
);

// 组级记忆隔离（docs/09 §6）：写入自动归属激活组 product
let gmem: serde_json::Value = client
    .post(format!("{base}/memory"))
    .json(&serde_json::json!({"title": "产品组专属结论", "body": "该条目只属于 product 组"}))
    .send()
    .await?
    .json()
    .await?;
c.check("组内记忆写入（product）", gmem["groupId"] == "product");
let gsearch: serde_json::Value = client
    .post(format!("{base}/memory/search"))
    .json(&serde_json::json!({"query": "产品组专属结论"}))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "组内可见本组条目",
    gsearch["hits"]
        .as_array()
        .map(|h| h.iter().any(|x| x["entry"]["title"] == "产品组专属结论"))
        .unwrap_or(false),
);

// 切回默认组
let sw2 = client
    .put(format!("{base}/groups/active"))
    .json(&serde_json::json!({"id": "exmachina"}))
    .send()
    .await?;
c.check("切回默认组", sw2.status().is_success());
let agents_back: serde_json::Value = client
    .get(format!("{base}/agents"))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "切回默认组后个体数与首查一致",
    agents_back.as_array().map(|a| a.len()).unwrap_or(0) == agents_len,
);

// 跨组记忆隔离：default 组检索不到 product 组条目
let dsearch: serde_json::Value = client
    .post(format!("{base}/memory/search"))
    .json(&serde_json::json!({"query": "产品组专属结论"}))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "跨组记忆隔离（default 不可见 product 条目）",
    dsearch["hits"]
        .as_array()
        .map(|h| h.iter().all(|x| x["entry"]["title"] != "产品组专属结论"))
        .unwrap_or(false),
);

// 激活组不可删 → 已切回默认组后删除自定义组
let del: serde_json::Value = client
    .delete(format!("{base}/groups/product"))
    .send()
    .await?
    .json()
    .await?;
c.check("删除自定义组", del["ok"] == true);


    Ok(())
}
