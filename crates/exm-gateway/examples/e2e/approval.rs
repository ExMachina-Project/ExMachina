//! e2e 域文件：审批与鉴权。（自 e2e.rs 拆出，纯搬移零断言变更）

use super::*;

/// 三档放行（approval-ergonomics）：once 不建授权 / session 建授权 /
pub(super) async fn stage_approval_once(_e: &Env, _c: &mut Checker) -> anyhow::Result<()> {

// ---- 三档放行（approval-ergonomics）：once 不建授权 / session 建授权 /

    Ok(())
}

/// allowlist 写配置 + 未知档位 400 + 兼容旧无 body 调用
pub(super) async fn stage_approval_allowlist(e: &Env, c: &mut Checker) -> anyhow::Result<()> {
    let base = &e.base;
    let client = &e.client;
    let core = &e.core;

// ---- allowlist 写配置 + 未知档位 400 + 兼容旧无 body 调用
{
    use exm_core::types::{now_iso, ApprovalRequest};
    let mk_ap = |core: &Core, sid: &str, cmd: &str| {
        let ap = ApprovalRequest {
            id: exm_core::types::new_id()[..8].to_string(),
            session_id: sid.into(),
            node_id: None,
            agent_id: "e2e".into(),
            command: cmd.into(),
            status: "pending".into(),
            result: None,
            created_at: now_iso(),
            decided_at: None,
            origin_user: None,
            origin_role: None,
            meta_json: None,
        };
        core.store.add_approval(&ap).unwrap();
        ap.id
    };
    let mk_session = |core: &Core| core.create_session("档位测试").unwrap().id;
    // once：不建授权
    let sid = mk_session(&core);
    let once_id = mk_ap(&core, &sid, "echo once-scope");
    let r: serde_json::Value = client
        .post(format!("{base}/approvals/{once_id}/approve"))
        .json(&serde_json::json!({ "scope": "once" }))
        .send()
        .await?
        .json()
        .await?;
    c.check("once 档执行成功", r["status"] == "executed");
    c.check(
        "once 档不建会话授权",
        !r["result"].as_str().unwrap_or("").contains("会话授权"),
    );
    // session：建授权,同前缀第二次免审批直通
    let ap2 = mk_ap(&core, &sid, "echo session-scope");
    let r2: serde_json::Value = client
        .post(format!("{base}/approvals/{ap2}/approve"))
        .json(&serde_json::json!({ "scope": "session" }))
        .send()
        .await?
        .json()
        .await?;
    c.check("session 档执行成功", r2["status"] == "executed");
    c.check(
        "session 档建会话授权",
        r2["result"].as_str().unwrap_or("").contains("会话授权"),
    );
    c.check(
        "同前缀命令命中授权",
        core.session_granted(&sid, "echo session-scope again").is_some(),
    );
    // allowlist：前缀写入配置可读回
    let sid3 = mk_session(&core);
    let ap3 = mk_ap(&core, &sid3, "echo allowlist-scope");
    let r3: serde_json::Value = client
        .post(format!("{base}/approvals/{ap3}/approve"))
        .json(&serde_json::json!({ "scope": "allowlist" }))
        .send()
        .await?
        .json()
        .await?;
    c.check("allowlist 档执行成功", r3["status"] == "executed");
    let cfg_now = core.config();
    c.check(
        "allowlist 档写入配置",
        cfg_now
            .security
            .exec_allowlist
            .iter()
            .any(|p| p == "echo allowlist-scope"),
    );
    drop(cfg_now);
    // 未知档位 → 400
    let sid4 = mk_session(&core);
    let ap4 = mk_ap(&core, &sid4, "echo bad-scope");
    let bad: reqwest::Response = client
        .post(format!("{base}/approvals/{ap4}/approve"))
        .json(&serde_json::json!({ "scope": "forever" }))
        .send()
        .await?;
    c.check("未知档位返回 400", bad.status().as_u16() == 400);
    // 旧客户端无 body 兼容（缺省 once）
    let r5: serde_json::Value = client
        .post(format!("{base}/approvals/{ap4}/approve"))
        .send()
        .await?
        .json()
        .await?;
    c.check("无 body 兼容缺省 once", r5["status"] == "executed");
}


    Ok(())
}

/// 后台密钥鉴权（docs/12）
pub(super) async fn stage_backend_auth(e: &Env, c: &mut Checker) -> anyhow::Result<()> {
    let base = &e.base;
    let client = &e.client;

// ---- 后台密钥鉴权（docs/12）----
let _set_key: serde_json::Value = client
    .put(format!("{base}/config"))
    .json(&serde_json::json!({ "security": { "authKey": "e2e-secret" } }))
    .send()
    .await?
    .json()
    .await?;
let no_key: reqwest::Response = client.get(format!("{base}/agents")).send().await?;
c.check("未带密钥访问 401", no_key.status().as_u16() == 401);
let no_key_cfg: reqwest::Response = client.get(format!("{base}/config")).send().await?;
c.check(
    "未带密钥访问数据端点 /config 401",
    no_key_cfg.status().as_u16() == 401,
);
let mut wrong_key_req = client.get(format!("{base}/sessions")).build()?;
wrong_key_req
    .headers_mut()
    .insert("x-auth-key", "bad-key".parse().unwrap());
let wrong_key_resp = client.execute(wrong_key_req).await?;
c.check(
    "错误密钥访问数据端点 401",
    wrong_key_resp.status().as_u16() == 401,
);
let wrong: serde_json::Value = client
    .post(format!("{base}/auth/verify"))
    .json(&serde_json::json!({ "key": "wrong" }))
    .send()
    .await?
    .json()
    .await?;
c.check("错误密钥 verify 拒绝", wrong["ok"] == false);
let right: serde_json::Value = client
    .post(format!("{base}/auth/verify"))
    .json(&serde_json::json!({ "key": "e2e-secret" }))
    .send()
    .await?
    .json()
    .await?;
c.check("正确密钥 verify 通过", right["ok"] == true);
let mut with_key = client.get(format!("{base}/agents")).build()?;
with_key
    .headers_mut()
    .insert("x-auth-key", "e2e-secret".parse().unwrap());
let ok_resp = client.execute(with_key).await?;
c.check("带密钥访问 200", ok_resp.status().as_u16() == 200);
// 首启引导：状态查询免鉴权；已配置密钥时两个引导动作拒绝（防止经豁免端点复位提醒状态）
let st_configured: serde_json::Value = client
    .get(format!("{base}/auth/status"))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "已配置密钥时 status.keyConfigured=true",
    st_configured["keyConfigured"] == true,
);
let snooze_guarded: reqwest::Response =
    client.post(format!("{base}/auth/snooze")).send().await?;
c.check(
    "已配置密钥时「下次再说」拒绝 400",
    snooze_guarded.status().as_u16() == 400,
);
let dismiss_guarded: reqwest::Response =
    client.post(format!("{base}/auth/dismiss")).send().await?;
c.check(
    "已配置密钥时「不再提醒」拒绝 400",
    dismiss_guarded.status().as_u16() == 400,
);
// 通道入站豁免（D10）：有自有 secret 的 webhook 通道放行；无 secret 通道仍被全局鉴权拦截
let _mk_hook: serde_json::Value = client
    .post(format!("{base}/channels"))
    .header("x-auth-key", "e2e-secret")
    .json(&serde_json::json!({ "id": "e2e-hook-auth", "platform": "webhook", "secret": "hook-secret" }))
    .send()
    .await?
    .json()
    .await?;
let inbound_ok: reqwest::Response = client
    .post(format!("{base}/channels/e2e-hook-auth/inbound"))
    .json(&serde_json::json!({ "secret": "hook-secret", "text": "鉴权豁免验证" }))
    .send()
    .await?;
c.check(
    "带 secret 通道入站豁免全局鉴权（202）",
    inbound_ok.status().as_u16() == 202,
);
let inbound_bad: reqwest::Response = client
    .post(format!("{base}/channels/e2e-hook-auth/inbound"))
    .json(&serde_json::json!({ "secret": "wrong", "text": "错密钥" }))
    .send()
    .await?;
c.check(
    "secret 错误入站仍被拒 401",
    inbound_bad.status().as_u16() == 401,
);
let _mk_plain: serde_json::Value = client
    .post(format!("{base}/channels"))
    .header("x-auth-key", "e2e-secret")
    .json(&serde_json::json!({ "id": "e2e-nosecret", "platform": "webhook" }))
    .send()
    .await?
    .json()
    .await?;
let inbound_no: reqwest::Response = client
    .post(format!("{base}/channels/e2e-nosecret/inbound"))
    .json(&serde_json::json!({ "text": "无凭据通道" }))
    .send()
    .await?;
c.check(
    "无 secret 通道入站仍被全局鉴权拦截 401",
    inbound_no.status().as_u16() == 401,
);
let mut rm_hook = client
    .delete(format!("{base}/channels/e2e-hook-auth"))
    .build()?;
rm_hook
    .headers_mut()
    .insert("x-auth-key", "e2e-secret".parse().unwrap());
let _ = client.execute(rm_hook).await;
let mut rm_plain = client
    .delete(format!("{base}/channels/e2e-nosecret"))
    .build()?;
rm_plain
    .headers_mut()
    .insert("x-auth-key", "e2e-secret".parse().unwrap());
let _ = client.execute(rm_plain).await;
let ws_root = base.trim_end_matches("/api").to_string();
let ws_no_key: reqwest::Response = client.get(format!("{ws_root}/ws")).send().await?;
c.check("WS 无密钥 401", ws_no_key.status().as_u16() == 401);
let mut clear_req = client
    .put(format!("{base}/config"))
    .json(&serde_json::json!({ "security": { "authKey": "" } }))
    .build()?;
clear_req
    .headers_mut()
    .insert("x-auth-key", "e2e-secret".parse().unwrap());
let _clear: serde_json::Value = client.execute(clear_req).await?.json().await?;
// 注意：清除后旧客户端仍带 X-Auth-Key 头也无需匹配——中间件放行所有请求
let after_clear: reqwest::Response = client.get(format!("{base}/agents")).send().await?;
c.check("清除密钥后恢复免鉴权", after_clear.status().as_u16() == 200);


    Ok(())
}

/// 首启密钥设置引导（部署后首次启动、尚未设密钥）
pub(super) async fn stage_first_run_setup(e: &Env, c: &mut Checker) -> anyhow::Result<()> {
    let base = &e.base;
    let client = &e.client;
    let root = &e.root;

// ---- 首启密钥设置引导（部署后首次启动、尚未设密钥）----
let st: serde_json::Value = client
    .get(format!("{base}/auth/status"))
    .send()
    .await?
    .json()
    .await?;
c.check("无密钥时 status 提示未配置", st["keyConfigured"] == false);
c.check(
    "初始 snoozed / dismissed 均为假",
    st["setupSnoozed"] == false && st["setupDismissed"] == false,
);
let snooze: serde_json::Value = client
    .post(format!("{base}/auth/snooze"))
    .send()
    .await?
    .json()
    .await?;
c.check("「下次再说」成功", snooze["ok"] == true);
let st: serde_json::Value = client
    .get(format!("{base}/auth/status"))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "「下次再说」后 setupSnoozed=true",
    st["setupSnoozed"] == true,
);
let dismiss: serde_json::Value = client
    .post(format!("{base}/auth/dismiss"))
    .send()
    .await?
    .json()
    .await?;
c.check("「不再提醒」成功", dismiss["ok"] == true);
let st: serde_json::Value = client
    .get(format!("{base}/auth/status"))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "「不再提醒」后 setupDismissed=true",
    st["setupDismissed"] == true,
);
// 持久化落盘：config.json 记录 keySetupDismissed（FileGuard 跑完原样还回）
let cfg_raw =
    std::fs::read_to_string(root.join(".exmachina").join("config.json")).unwrap_or_default();
c.check(
    "「不再提醒」已持久化到配置文件",
    cfg_raw.contains("\"keySetupDismissed\": true"),
);


    Ok(())
}
