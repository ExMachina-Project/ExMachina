//! e2e 域文件：自检与 Nexus。（自 e2e.rs 拆出，纯搬移零断言变更）

use super::*;

/// 开箱自检（onboarding-selfcheck）：四类体检结构 + 分级 + 指引
pub(super) async fn stage_selfcheck(e: &Env, c: &mut Checker) -> anyhow::Result<()> {
    let base = &e.base;
    let client = &e.client;

// ---- 开箱自检（onboarding-selfcheck）：四类体检结构 + 分级 + 指引 ----
{
    let sc: serde_json::Value = client
        .get(format!("{base}/selfcheck"))
        .header("x-auth-key", "e2e-secret")
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "自检：总体结构与四类条目",
        sc.get("overall").is_some()
            && sc.get("ok").is_some()
            && sc["items"].as_array().map(|a| a.len() == 4).unwrap_or(false),
    );
    let names_ok = sc["items"]
        .as_array()
        .map(|a| {
            a.iter().all(|i| {
                i.get("level").is_some()
                    && i.get("name").is_some()
                    && i.get("detail").is_some()
            })
        })
        .unwrap_or(false);
    c.check("自检：每项含 level/name/detail", names_ok);
    // e2e 环境（mock 或配了探针端点）：存储类应为 ok 且含「可写」
    let storage_ok = sc["items"]
        .as_array()
        .and_then(|a| a.iter().find(|i| i["name"] == "存储").map(|i| i["level"].clone()))
        .map(|l| l == "ok")
        .unwrap_or(false);
    c.check("自检：存储体检 ok（数据目录可写）", storage_ok);
    // error 级条目必须带可操作指引（hint）
    let err_hint_ok = sc["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|i| i["level"] == "error")
                .all(|i| i["hint"].as_str().is_some_and(|h| !h.is_empty()))
        })
        .unwrap_or(true);
    c.check("自检：error 项必附指引", err_hint_ok);
}


    Ok(())
}

/// Nexus 拓扑与连边（nexus-topology-admin）：graph 聚合 + bind/unbind + 冲突语义
pub(super) async fn stage_nexus_topology(e: &Env, c: &mut Checker) -> anyhow::Result<()> {
    let base = &e.base;
    let client = &e.client;

// ---- Nexus 拓扑与连边（nexus-topology-admin）：graph 聚合 + bind/unbind + 冲突语义 ----
{
    // 造连结体 + 智能体
    let r: reqwest::Response = client
        .post(format!("{base}/nexus/links"))
        .header("x-auth-key", "e2e-secret")
        .json(&serde_json::json!({
            "id": "e2e-link", "name": "E2E 连结", "kind": "exmachina",
            "endpoint": "http://127.0.0.1:59999", "enabled": true
        }))
        .send()
        .await?;
    c.check("连结体：创建", r.status().as_u16() == 200);

    let g: serde_json::Value = client
        .get(format!("{base}/nexus/graph"))
        .header("x-auth-key", "e2e-secret")
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "拓扑：节点含新建连结体",
        g["nodes"].as_array().map(|a| a.iter().any(|n| n["id"] == "e2e-link")).unwrap_or(false),
    );

    // bind 个体 → 边出现
    let r: serde_json::Value = client
        .put(format!("{base}/nexus/edges"))
        .header("x-auth-key", "e2e-secret")
        .json(&serde_json::json!({ "action": "bind", "agentId": "machina", "linkId": "e2e-link" }))
        .send()
        .await?
        .json()
        .await?;
    c.check("连边：绑定成功", r["ok"] == true);
    let g: serde_json::Value = client
        .get(format!("{base}/nexus/graph"))
        .header("x-auth-key", "e2e-secret")
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "拓扑：边出现",
        g["edges"].as_array().map(|a| a.iter().any(|e| e["agentId"] == "machina" && e["linkId"] == "e2e-link")).unwrap_or(false),
    );

    // 重复 bind → 409
    let r: reqwest::Response = client
        .put(format!("{base}/nexus/edges"))
        .header("x-auth-key", "e2e-secret")
        .json(&serde_json::json!({ "action": "bind", "agentId": "machina", "linkId": "e2e-link" }))
        .send()
        .await?;
    c.check("连边：重复绑定 409", r.status().as_u16() == 409);

    // unbind → 边消失
    let r: reqwest::Response = client
        .put(format!("{base}/nexus/edges"))
        .header("x-auth-key", "e2e-secret")
        .json(&serde_json::json!({ "action": "unbind", "agentId": "machina", "linkId": "e2e-link" }))
        .send()
        .await?;
    c.check("连边：解绑成功", r.status().as_u16() == 200);
    let g: serde_json::Value = client
        .get(format!("{base}/nexus/graph"))
        .header("x-auth-key", "e2e-secret")
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "拓扑：边已清",
        g["edges"].as_array().map(|a| !a.iter().any(|e| e["linkId"] == "e2e-link")).unwrap_or(false),
    );

    // 清理
    let _: reqwest::Response = client
        .delete(format!("{base}/nexus/links/e2e-link"))
        .header("x-auth-key", "e2e-secret")
        .send()
        .await?;
    }

    Ok(())
}

