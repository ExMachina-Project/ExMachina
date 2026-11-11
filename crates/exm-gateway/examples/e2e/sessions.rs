//! e2e 域文件：会话管理。（自 e2e.rs 拆出，纯搬移零断言变更）

use super::*;

/// 请求体上限（审计补面）：全局 2MB，超限 413
pub(super) async fn stage_body_limit(e: &Env, c: &mut Checker) -> anyhow::Result<()> {
    let base = &e.base;
    let client = &e.client;

// ---- 请求体上限（审计补面）：全局 2MB，超限 413 ----
{
    let big_ok: reqwest::Response = client
        .post(format!("{base}/sessions"))
        .header("x-auth-key", "e2e-secret")
        .json(&serde_json::json!({"title": "x".repeat(1024 * 1024)}))
        .send()
        .await?;
    c.check(
        "1MB 请求体放行（200/400 均非 413）",
        big_ok.status().as_u16() != 413,
    );
    let big_rej: reqwest::Response = client
        .post(format!("{base}/sessions"))
        .header("x-auth-key", "e2e-secret")
        .json(&serde_json::json!({"title": "x".repeat(3 * 1024 * 1024)}))
        .send()
        .await?;
    let rej_code = big_rej.status().as_u16();
    let rej_body = big_rej.text().await.unwrap_or_default();
    c.check(
        &format!("3MB 请求体 413 拒绝（实得 {rej_code}）"),
        rej_code == 413,
    );
    if rej_code != 413 {
        println!("  ↳ 3MB 拒绝响应体：{}", &rej_body[..rej_body.len().min(200)]);
    }
}


    Ok(())
}

/// 会话归档与批量（session-management）：归档→主列表隐身→归档区可见→恢复→回归
pub(super) async fn stage_session_archive(e: &Env, c: &mut Checker) -> anyhow::Result<()> {
    let base = &e.base;
    let client = &e.client;

// ---- 会话归档与批量（session-management）：归档→主列表隐身→归档区可见→恢复→回归 ----
{
    let a1: String = client
        .post(format!("{base}/sessions"))
        .header("x-auth-key", "e2e-secret")
        .json(&serde_json::json!({ "title": "batch-arch-a" }))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?["id"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let a2: String = client
        .post(format!("{base}/sessions"))
        .header("x-auth-key", "e2e-secret")
        .json(&serde_json::json!({ "title": "batch-arch-b" }))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?["id"]
        .as_str()
        .unwrap_or("")
        .to_string();
    // 批量归档 2 条
    let bar: serde_json::Value = client
        .post(format!("{base}/sessions/batch"))
        .header("x-auth-key", "e2e-secret")
        .json(&serde_json::json!({ "action": "archive", "ids": [a1, a2] }))
        .send()
        .await?
        .json()
        .await?;
    c.check("批量归档作用计数=2", bar["affected"] == 2 && bar["ok"] == true);
    // 幂等（长程迭代 4.3）：重复归档成功、archived_at 不刷新（归档区排序稳定）；
    // 不存在 id 进 failed 明细（错误语义保留）
    let arch_first: serde_json::Value = client
        .get(format!("{base}/sessions?archived=true"))
        .header("x-auth-key", "e2e-secret")
        .send()
        .await?
        .json()
        .await?;
    let at_first = arch_first
        .as_array()
        .and_then(|a| a.iter().find(|s| s["id"].as_str() == Some(a1.as_str())))
        .and_then(|s| s["archivedAt"].as_str())
        .unwrap_or("")
        .to_string();
    let bar2: serde_json::Value = client
        .post(format!("{base}/sessions/batch"))
        .header("x-auth-key", "e2e-secret")
        .json(&serde_json::json!({ "action": "archive", "ids": [a1] }))
        .send()
        .await?
        .json()
        .await?;
    c.check("重复归档幂等成功", bar2["affected"] == 1 && bar2["ok"] == true);
    let arch_again: serde_json::Value = client
        .get(format!("{base}/sessions?archived=true"))
        .header("x-auth-key", "e2e-secret")
        .send()
        .await?
        .json()
        .await?;
    let at_again = arch_again
        .as_array()
        .and_then(|a| a.iter().find(|s| s["id"].as_str() == Some(a1.as_str())))
        .and_then(|s| s["archivedAt"].as_str())
        .unwrap_or("")
        .to_string();
    c.check("重复归档不刷新归档时间", !at_first.is_empty() && at_first == at_again);
    let bar_missing: serde_json::Value = client
        .post(format!("{base}/sessions/batch"))
        .header("x-auth-key", "e2e-secret")
        .json(&serde_json::json!({ "action": "archive", "ids": ["no-such-id"] }))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "不存在 id 进 failed 明细",
        bar_missing["affected"] == 0
            && bar_missing["failed"]
                .as_array()
                .map(|f| f.len() == 1 && f[0]["id"] == "no-such-id")
                .unwrap_or(false),
    );
    // 主列表（all 视角）不再含这两条
    let main: serde_json::Value = client
        .get(format!("{base}/sessions?all=true"))
        .header("x-auth-key", "e2e-secret")
        .send()
        .await?
        .json()
        .await?;
    let main_ids = main.as_array().cloned().unwrap_or_default();
    let gone = main_ids
        .iter()
        .all(|s| s["id"].as_str() != Some(a1.as_str()) && s["id"].as_str() != Some(a2.as_str()));
    c.check("归档后主列表不含归档会话", gone);
    // 归档区恰含这两条且带 archivedAt
    let arch: serde_json::Value = client
        .get(format!("{base}/sessions?archived=true"))
        .header("x-auth-key", "e2e-secret")
        .send()
        .await?
        .json()
        .await?;
    let arch_arr = arch.as_array().cloned().unwrap_or_default();
    let arch_hit = arch_arr
        .iter()
        .filter(|s| {
            (s["id"].as_str() == Some(a1.as_str()) || s["id"].as_str() == Some(a2.as_str()))
                && s["archivedAt"].is_string()
        })
        .count();
    c.check("归档区可见且带归档时间", arch_hit == 2);
    // 批量恢复 → 主列表回归
    let un: serde_json::Value = client
        .post(format!("{base}/sessions/batch"))
        .header("x-auth-key", "e2e-secret")
        .json(&serde_json::json!({ "action": "unarchive", "ids": [a1] }))
        .send()
        .await?
        .json()
        .await?;
    c.check("批量恢复作用计数=1", un["affected"] == 1);
    let main2: serde_json::Value = client
        .get(format!("{base}/sessions?all=true"))
        .header("x-auth-key", "e2e-secret")
        .send()
        .await?
        .json()
        .await?;
    let back = main2
        .as_array()
        .map(|a| a.iter().any(|s| s["id"].as_str() == Some(a1.as_str())))
        .unwrap_or(false);
    c.check("恢复后主列表回归", back);
    // 清理：批量删除三条（含未恢复的 a2）
    let del: serde_json::Value = client
        .post(format!("{base}/sessions/batch"))
        .header("x-auth-key", "e2e-secret")
        .json(&serde_json::json!({ "action": "delete", "ids": [a1, a2] }))
        .send()
        .await?
        .json()
        .await?;
    c.check("批量删除作用计数=2", del["affected"] == 2);
    // 非法动作 400
    let bad: reqwest::Response = client
        .post(format!("{base}/sessions/batch"))
        .header("x-auth-key", "e2e-secret")
        .json(&serde_json::json!({ "action": "explode", "ids": [a1] }))
        .send()
        .await?;
    c.check("未知批量动作 400", bad.status().as_u16() == 400);
}


    Ok(())
}

/// 来源标注与按流归档（session-management）
pub(super) async fn stage_archive_by_flow(e: &Env, c: &mut Checker) -> anyhow::Result<()> {
    let base = &e.base;
    let client = &e.client;

// ---- 来源标注与按流归档（session-management）----
{
    // 列表补齐：通道会话带 origin/originLabel，控制台会话 origin=console
    let all: serde_json::Value = client
        .get(format!("{base}/sessions?all=true"))
        .header("x-auth-key", "e2e-secret")
        .send()
        .await?
        .json()
        .await?;
    let arr = all.as_array().cloned().unwrap_or_default();
    let ch_sess = arr.iter().find(|s| s["title"] == "channel:e2e-hook:user-a");
    c.check(
        "通道会话带结构化来源",
        ch_sess
            .map(|s| s["origin"] == "channel:e2e-hook:user-a" && s["originLabel"].is_string())
            .unwrap_or(false),
    );
    c.check(
        "控制台会话来源为 console",
        arr.iter()
            .any(|s| !s["title"].as_str().unwrap_or("").starts_with("channel:") && s["origin"] == "console"),
    );

    // 造第二个流会话（不触发执行——直接建会话 + 绑流，模拟历史会话）
    let old_flow_sess: String = client
        .post(format!("{base}/sessions"))
        .header("x-auth-key", "e2e-secret")
        .json(&serde_json::json!({ "title": "channel:e2e-hook:user-a:旧话题" }))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?["id"]
        .as_str()
        .unwrap_or("")
        .to_string();
    // 按流归档：旧会话归档、活跃绑定豁免
    let fr: serde_json::Value = client
        .post(format!("{base}/sessions/flow-archive"))
        .header("x-auth-key", "e2e-secret")
        .json(&serde_json::json!({
            "scope": "flow", "channelId": "e2e-hook", "chatKey": "user-a"
        }))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "按流归档豁免活跃绑定",
        fr["ok"] == true && fr["archived"].as_u64().unwrap_or(0) >= 1 && fr["skipped"] == 1,
    );
    let all2: serde_json::Value = client
        .get(format!("{base}/sessions?all=true"))
        .header("x-auth-key", "e2e-secret")
        .send()
        .await?
        .json()
        .await?;
    let active_kept = all2
        .as_array()
        .map(|a| a.iter().any(|s| s["title"] == "channel:e2e-hook:user-a"))
        .unwrap_or(false);
    c.check("流活跃会话保留主列表", active_kept);
    // 非法 scope 400
    let bad_scope: reqwest::Response = client
        .post(format!("{base}/sessions/flow-archive"))
        .header("x-auth-key", "e2e-secret")
        .json(&serde_json::json!({ "scope": "flow", "channelId": "e2e-hook" }))
        .send()
        .await?;
    c.check("flow 缺 chatKey 400", bad_scope.status().as_u16() == 400);
    // 清理旧话题会话
    let _ = client
        .post(format!("{base}/sessions/batch"))
        .header("x-auth-key", "e2e-secret")
        .json(&serde_json::json!({ "action": "delete", "ids": [old_flow_sess] }))
        .send()
        .await?;
}


    Ok(())
}
