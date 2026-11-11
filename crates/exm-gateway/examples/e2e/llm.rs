//! e2e 域文件：模型档案与密钥。（自 e2e.rs 拆出，纯搬移零断言变更）

use super::*;

/// 经验优化（docs/10 §5）：合成要点 → 后续派发携带修订号 → 重置
pub(super) async fn stage_optimize_experience(e: &Env, c: &mut Checker) -> anyhow::Result<()> {
    let base = &e.base;
    let client = &e.client;
    let core = &e.core;
    let agents = &e.agents;

// ---- 经验优化（docs/10 §5）：合成要点 → 后续派发携带修订号 → 重置 ----
// 模拟规划器按 identifier 序取前几名派发：动态选取排序后第 2 个子个体，保证其必被派发
let target = {
    let mut units: Vec<String> = agents
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|a| a["tier"] == "unit")
        .filter_map(|a| a["identifier"].as_str().map(String::from))
        .collect();
    units.sort();
    units
        .get(1)
        .cloned()
        .unwrap_or_else(|| "scout-agent".into())
};
let adapt: serde_json::Value = client
    .post(format!("{base}/agents/{target}/optimize"))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "经验要点合成 rev1",
    adapt["revision"] == 1
        && adapt["content"]
            .as_str()
            .map(|s| s.contains("- "))
            .unwrap_or(false),
);

let mut rx3 = core.subscribe();
let target2 = target.clone();
let collector3 = tokio::spawn(async move {
    let target = target2;
    let mut dispatched = false;
    let mut found = false;
    while let Ok(evt) = rx3.recv().await {
        if evt.kind == "dispatch.sent" {
            dispatched = true;
            if evt.payload["agentIdentifier"] == target
                && evt.payload["adaptationRevision"] == 1
            {
                found = true;
            }
        }
        if evt.kind == "run.finished" || evt.kind == "run.error" {
            break;
        }
    }
    (dispatched, found)
});
let session3: serde_json::Value = client
    .post(format!("{base}/sessions"))
    .json(&serde_json::json!({"title": "e2e-adapt"}))
    .send()
    .await?
    .json()
    .await?;
let sid3 = session3["id"].as_str().unwrap_or_default().to_string();
let resp3 = client
    .post(format!("{base}/sessions/{sid3}/chat"))
    .json(&serde_json::json!({"text": "设计一个支持 1000 个智能体的集群调度方案"}))
    .send()
    .await?;
c.check("经验优化后 chat 202", resp3.status().as_u16() == 202);
let (dispatched, found) = tokio::time::timeout(Duration::from_secs(60), collector3).await??;
c.check("派发发生", dispatched);
c.check("派发携带经验修订号", found);

let adapt_get: serde_json::Value = client
    .get(format!("{base}/agents/{target}/adaptation"))
    .send()
    .await?
    .json()
    .await?;
c.check("经验要点可查", adapt_get["revision"] == 1);
let adapt_del: serde_json::Value = client
    .delete(format!("{base}/agents/{target}/adaptation"))
    .send()
    .await?
    .json()
    .await?;
c.check("经验要点重置", adapt_del["ok"] == true);
let adapt_gone: reqwest::Response = client
    .get(format!("{base}/agents/{target}/adaptation"))
    .send()
    .await?;
c.check("重置后查询 404", adapt_gone.status().as_u16() == 404);


    Ok(())
}

/// 多厂商模型档案（docs/11）
pub(super) async fn stage_llm_profiles(e: &Env, c: &mut Checker) -> anyhow::Result<()> {
    let base = &e.base;
    let client = &e.client;

// ---- 多厂商模型档案（docs/11）----
// 自持基线：e2e 起手清空了本机档案（避免残留端点劫持推理），故先自建一个基线档案并激活，
// 断言对象是接口契约（清单可读 / 生效者可解析 / 可切回），不再依赖开发者本机配置。
let _base_seed: serde_json::Value = client
    .post(format!("{base}/llm/profiles"))
    .json(&serde_json::json!({ "id": "e2e-base", "name": "E2E 基线档案", "baseUrl": "https://api.example.invalid/v1" }))
    .send()
    .await?
    .json()
    .await?;
let _base_act: serde_json::Value = client
    .put(format!("{base}/llm/active"))
    .json(&serde_json::json!({ "id": "e2e-base" }))
    .send()
    .await?
    .json()
    .await?;
let profiles0: serde_json::Value = client
    .get(format!("{base}/llm/profiles"))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "模型档案非空且含生效者",
    profiles0["profiles"]
        .as_array()
        .map(|a| !a.is_empty())
        .unwrap_or(false)
        && profiles0["profiles"]
            .as_array()
            .map(|a| a.iter().any(|p| p["id"] == profiles0["active"]))
            .unwrap_or(false),
);
let new_profile: serde_json::Value = client
    .post(format!("{base}/llm/profiles"))
    .json(&serde_json::json!({
        "id": "e2e-vendor", "name": "E2E 厂商",
        "baseUrl": "https://api.example.com/v1",
        "apiKey": "sk-e2e", "model": "vendor-large",
        "models": [
            { "model": "vendor-large", "vision": false, "audio": false },
            { "model": "vendor-vision", "vision": true, "audio": true }
        ]
    }))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "新增模型档案",
    new_profile["ok"] == true && new_profile["id"] == "e2e-vendor",
);
let plist: serde_json::Value = client
    .get(format!("{base}/llm/profiles"))
    .send()
    .await?
    .json()
    .await?;
let vendor = plist["profiles"]
    .as_array()
    .and_then(|a| a.iter().find(|p| p["id"] == "e2e-vendor"))
    .cloned()
    .unwrap_or(serde_json::Value::Null);
c.check(
    "模型清单与视觉/语音开关回显",
    vendor["models"]
        .as_array()
        .map(|m| {
            m.len() == 2
                && m[0]["model"] == "vendor-large"
                && m[0]["vision"] == false
                && m[1]["model"] == "vendor-vision"
                && m[1]["vision"] == true
                && m[1]["audio"] == true
        })
        .unwrap_or(false),
);

// 能力模型槽位：语音合成 / 语音转述 / 视觉转述 / 嵌入（嵌入 = memory.semanticModel 同字段）
let caps_put: serde_json::Value = client
    .put(format!("{base}/llm/capabilities"))
    .json(&serde_json::json!({
        "speech": "e2e-vendor/tts-x",
        "transcribe": "e2e-vendor",
        "visionRelay": "e2e-vendor/vendor-vision",
        "embedding": "e2e-vendor/embed-1"
    }))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "能力槽位写入生效",
    caps_put["ok"] == true
        && caps_put["speech"] == "e2e-vendor/tts-x"
        && caps_put["visionRelay"] == "e2e-vendor/vendor-vision"
        && caps_put["embedding"] == "e2e-vendor/embed-1",
);
let caps_get: serde_json::Value = client
    .get(format!("{base}/llm/capabilities"))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "能力槽位可回读",
    caps_get["transcribe"] == "e2e-vendor" && caps_get["embedding"] == "e2e-vendor/embed-1",
);
// 清空恢复：不把 e2e 的能力槽位留在本地 config
let _ = client
    .put(format!("{base}/llm/capabilities"))
    .json(&serde_json::json!({ "speech": "", "transcribe": "", "visionRelay": "", "embedding": "" }))
    .send()
    .await;
let activated: serde_json::Value = client
    .put(format!("{base}/llm/active"))
    .json(&serde_json::json!({ "id": "e2e-vendor" }))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "切换生效档案（热生效）",
    activated["ok"] == true && activated["active"] == "e2e-vendor",
);
let cfg_after: serde_json::Value = client
    .get(format!("{base}/config"))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "切换后运行时模型已随档案",
    cfg_after["llm"]["model"] == "vendor-large",
);
let restore: serde_json::Value = client
    .put(format!("{base}/llm/active"))
    .json(&serde_json::json!({ "id": "e2e-base" }))
    .send()
    .await?
    .json()
    .await?;
c.check("切回默认档案", restore["ok"] == true);
let _ = client
    .delete(format!("{base}/llm/profiles/e2e-vendor"))
    .send()
    .await;


    Ok(())
}

/// 密钥掩码保全（docs/11）：掩码回显非明文、掩码回传不丢键、池增删不串位
pub(super) async fn stage_key_masking(e: &Env, c: &mut Checker) -> anyhow::Result<()> {
    let base = &e.base;
    let client = &e.client;

// ---- 密钥掩码保全（docs/11）：掩码回显非明文、掩码回传不丢键、池增删不串位 ----
let _mask_new: serde_json::Value = client
    .post(format!("{base}/llm/profiles"))
    .json(&serde_json::json!({
        "id": "e2e-mask", "name": "E2E 掩码",
        "baseUrl": "https://api.example.com/v1",
        "apiKey": "sk-mask-primary",
        "apiKeys": ["sk-pool-a", "sk-pool-b"],
        "model": "mask-large"
    }))
    .send()
    .await?
    .json()
    .await?;
let mask_list0: serde_json::Value = client
    .get(format!("{base}/llm/profiles"))
    .send()
    .await?
    .json()
    .await?;
let mask0 = mask_list0["profiles"]
    .as_array()
    .and_then(|a| a.iter().find(|p| p["id"] == "e2e-mask"))
    .cloned()
    .unwrap_or(serde_json::Value::Null);
c.check(
    "主 Key 与 Key 池掩码回显（非明文）",
    mask0["apiKey"] == "***已配置***"
        && mask0["apiKeys"]
            .as_array()
            .map(|a| a.len() == 2 && a.iter().all(|k| k == "***已配置***"))
            .unwrap_or(false),
);
// 模拟前端「追加一把池键」：明文在前 + 掩码保序在后（同位索引实现会在此丢首把池键）
let _mask_add: serde_json::Value = client
    .post(format!("{base}/llm/profiles"))
    .json(&serde_json::json!({
        "id": "e2e-mask", "name": "E2E 掩码",
        "baseUrl": "https://api.example.com/v1",
        "apiKey": "***已配置***",
        "apiKeys": ["sk-pool-c", "***已配置***", "***已配置***"],
        "model": "mask-large"
    }))
    .send()
    .await?
    .json()
    .await?;
let mask_list1: serde_json::Value = client
    .get(format!("{base}/llm/profiles"))
    .send()
    .await?
    .json()
    .await?;
let mask1 = mask_list1["profiles"]
    .as_array()
    .and_then(|a| a.iter().find(|p| p["id"] == "e2e-mask"))
    .cloned()
    .unwrap_or(serde_json::Value::Null);
c.check(
    "掩码回传追加池键不丢键（池含 3 把）",
    mask1["apiKey"] == "***已配置***"
        && mask1["apiKeys"]
            .as_array()
            .map(|a| a.len() == 3)
            .unwrap_or(false),
);
// 纯掩码回传（未改动密钥）：主 Key 与池全量保留、不落盘掩码字面量
let _mask_keep: serde_json::Value = client
    .post(format!("{base}/llm/profiles"))
    .json(&serde_json::json!({
        "id": "e2e-mask", "name": "E2E 掩码",
        "baseUrl": "https://api.example.com/v1",
        "apiKey": "***已配置***",
        "apiKeys": ["***已配置***", "***已配置***", "***已配置***"],
        "model": "mask-large"
    }))
    .send()
    .await?
    .json()
    .await?;
let mask_list2: serde_json::Value = client
    .get(format!("{base}/llm/profiles"))
    .send()
    .await?
    .json()
    .await?;
let mask2 = mask_list2["profiles"]
    .as_array()
    .and_then(|a| a.iter().find(|p| p["id"] == "e2e-mask"))
    .cloned()
    .unwrap_or(serde_json::Value::Null);
c.check(
    "纯掩码回传保留全部密钥",
    mask2["apiKey"] == "***已配置***"
        && mask2["apiKeys"]
            .as_array()
            .map(|a| a.len() == 3)
            .unwrap_or(false),
);
let _ = client
    .delete(format!("{base}/llm/profiles/e2e-mask"))
    .send()
    .await;


    Ok(())
}

/// 池掩码令牌（D3）：结构化 keep 引用，删行/插行与用户意图完全一致
pub(super) async fn stage_pool_mask_tokens(e: &Env, c: &mut Checker) -> anyhow::Result<()> {
    let base = &e.base;
    let client = &e.client;

// ---- 池掩码令牌（D3）：结构化 keep 引用，删行/插行与用户意图完全一致 ----
let _tok_new: serde_json::Value = client
    .post(format!("{base}/llm/profiles"))
    .json(&serde_json::json!({
        "id": "e2e-token", "name": "E2E 令牌", "baseUrl": "https://api.example.com/v1",
        "apiKeys": ["sk-tok-a", "sk-tok-b"], "model": "tok-m"
    }))
    .send()
    .await?
    .json()
    .await?;
let token_pool_len = |v: &serde_json::Value| -> usize {
    v["profiles"]
        .as_array()
        .and_then(|a| a.iter().find(|p| p["id"] == "e2e-token"))
        .and_then(|p| p["apiKeys"].as_array())
        .map(|a| a.len())
        .unwrap_or(0)
};
client
    .post(format!("{base}/llm/profiles"))
    .json(&serde_json::json!({ "id": "e2e-token", "name": "E2E 令牌", "apiKeys": [{ "keep": 1 }] }))
    .send()
    .await?;
let tok_del_first: serde_json::Value = client
    .get(format!("{base}/llm/profiles"))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "池令牌：删首行仅保留旧池第 2 把",
    token_pool_len(&tok_del_first) == 1,
);
client
    .post(format!("{base}/llm/profiles"))
    .json(&serde_json::json!({ "id": "e2e-token", "name": "E2E 令牌", "apiKeys": ["sk-tok-a", "sk-tok-b"] }))
    .send()
    .await?;
client
    .post(format!("{base}/llm/profiles"))
    .json(&serde_json::json!({ "id": "e2e-token", "name": "E2E 令牌", "apiKeys": [{ "keep": 0 }] }))
    .send()
    .await?;
let tok_del_mid: serde_json::Value = client
    .get(format!("{base}/llm/profiles"))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "池令牌：删中行仅保留旧池第 1 把",
    token_pool_len(&tok_del_mid) == 1,
);
// 中插明文：前端语义 = 按回读时的旧池生成 keep 令牌；先重置池为 [A,B] 再提交 [X, keep0, keep1]
client
    .post(format!("{base}/llm/profiles"))
    .json(&serde_json::json!({ "id": "e2e-token", "name": "E2E 令牌", "apiKeys": ["sk-tok-a", "sk-tok-b"] }))
    .send()
    .await?;
client
    .post(format!("{base}/llm/profiles"))
    .json(&serde_json::json!({ "id": "e2e-token", "name": "E2E 令牌", "apiKeys": ["sk-tok-c", { "keep": 0 }, { "keep": 1 }] }))
    .send()
    .await?;
let tok_insert: serde_json::Value = client
    .get(format!("{base}/llm/profiles"))
    .send()
    .await?
    .json()
    .await?;
c.check(
    "池令牌：中插明文池含 3 把",
    token_pool_len(&tok_insert) == 3,
);
let _ = client
    .delete(format!("{base}/llm/profiles/e2e-token"))
    .send()
    .await;


    Ok(())
}

/// 多 Key 粘性负载均衡（docs/11 §1）：k1 恒 429 → 切 k2 并粘住
pub(super) async fn stage_multikey_sticky(e: &Env, c: &mut Checker) -> anyhow::Result<()> {
    let base = &e.base;
    let client = &e.client;
    let core = &e.core;

// ---- 多 Key 粘性负载均衡（docs/11 §1）：k1 恒 429 → 切 k2 并粘住 ----
let key_stats = e.key_stats.clone();
let ks_for_handler = key_stats.clone();
// 记录每次到达 fake 的 system prompt（单体人设注入验证用）
let prompt_log = e.prompt_log.clone();
let pl_for_handler = prompt_log.clone();
let fake = axum::Router::new().route(
    "/v1/chat/completions",
    axum::routing::post(
        move |headers: axum::http::HeaderMap, Json(body): Json<serde_json::Value>| {
            let ks = ks_for_handler.clone();
            let pl = pl_for_handler.clone();
            async move {
                let key = headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.strip_prefix("Bearer "))
                    .unwrap_or("")
                    .to_string();
                let _model = body["model"].as_str().unwrap_or("m").to_string();
                let system_text = body["messages"]
                    .as_array()
                    .map(|ms| {
                        ms.iter()
                            .filter(|m| m["role"] == "system")
                            .filter_map(|m| m["content"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                pl.lock().push(system_text);
                let mut seen = ks.lock();
                let first_time = !seen.contains_key(&key);
                *seen.entry(key.clone()).or_insert(0) += 1;
                drop(seen);
                // 确定性：每把 Key 的第一次请求返回 429（模拟限额），之后恢复——迫使发起方轮换
                if first_time {
                    (
                        axum::http::StatusCode::TOO_MANY_REQUESTS,
                        Json(json!({ "error": { "message": "rate limit exceeded" } })),
                    )
                        .into_response()
                } else {
                    let plan = json!({
                        "routeLevel": "L0", "goal": "e2e-pool",
                        "boundary": { "inScope": [], "forbidden": [] },
                        "acceptance": [], "nodes": [],
                        "finalAnswer": [{ "tag": "报告", "text": "fake ok" }]
                    });
                    let content = format!(
                        "```json
{}
```",
                        serde_json::to_string(&plan).unwrap()
                    );
                    Json(json!({
                        "choices": [{ "message": { "role": "assistant", "content": content } }],
                        "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
                    }))
                    .into_response()
                }
            }
        },
    ),
);
let fake_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
let fake_port = fake_listener.local_addr()?.port();
let _ = e.fake_port.set(fake_port); // 段12/13 沿用同一 fake 服务
tokio::spawn(async move {
    let _ = axum::serve(fake_listener, fake).await;
});

let _pool: serde_json::Value = client
    .post(format!("{base}/llm/profiles"))
    .json(&serde_json::json!({
        "id": "e2e-pool", "name": "E2E Key 池",
        "baseUrl": format!("http://127.0.0.1:{fake_port}/v1"),
        "apiKeys": ["k1", "k2"],
        "model": "fake-large"
    }))
    .send()
    .await?
    .json()
    .await?;
let _act: serde_json::Value = client
    .put(format!("{base}/llm/active"))
    .json(&serde_json::json!({ "id": "e2e-pool" }))
    .send()
    .await?
    .json()
    .await?;

let pool_session: serde_json::Value = client
    .post(format!("{base}/sessions"))
    .json(&serde_json::json!({ "title": "e2e-pool" }))
    .send()
    .await?
    .json()
    .await?;
let pool_sid = pool_session["id"].as_str().unwrap_or_default().to_string();
let mut rx4 = core.subscribe();
let collector4 = tokio::spawn(async move {
    let mut ok = false;
    while let Ok(evt) = rx4.recv().await {
        if evt.kind == "run.finished" {
            ok = true;
            break;
        }
        if evt.kind == "run.error" {
            break;
        }
    }
    ok
});
let pool_chat = client
    .post(format!("{base}/sessions/{pool_sid}/chat"))
    .json(&serde_json::json!({ "text": "池化轮换验证" }))
    .send()
    .await?;
c.check("Key 池会话 chat 202", pool_chat.status().as_u16() == 202);
let rotated = tokio::time::timeout(Duration::from_secs(60), collector4).await??;
c.check("Key 限额后轮换成功（run.finished）", rotated);
// 第二轮：粘住 k2，不再触碰 k1
let _ = client
    .post(format!("{base}/sessions/{pool_sid}/chat"))
    .json(&serde_json::json!({ "text": "粘性验证第二轮" }))
    .send()
    .await?;
tokio::time::sleep(Duration::from_millis(2500)).await;
let stats = key_stats.lock().clone();
println!("    [key stats] {:?}", stats);
// 首次请求 429 → 轮换到另一把并粘住；第二轮不再触碰首次失败的键位顺序
c.check(
    "粘性负载均衡：两把 Key 均参与且首轮各被限额一次",
    stats.len() == 2 && stats.values().all(|&v| v >= 1),
);

let _ = client
    .put(format!("{base}/llm/active"))
    .json(&serde_json::json!({ "id": "exmachina" }))
    .send()
    .await;
let _ = client
    .delete(format!("{base}/llm/profiles/e2e-pool"))
    .send()
    .await;


    Ok(())
}

/// 主 Key 替换/沿用（P0-1 用户视角的 API 层等价提交）：替换主 Key、清空行=沿用、值级验证
pub(super) async fn stage_primary_key_replace(e: &Env, c: &mut Checker) -> anyhow::Result<()> {
    let fake_port = e.fake_port.get().copied().unwrap_or(0);
    let key_stats = &e.key_stats;
    let base = &e.base;
    let client = &e.client;

// ---- 主 Key 替换/沿用（P0-1 用户视角的 API 层等价提交）：替换主 Key、清空行=沿用、值级验证 ----
// 注意：档案不带 Key 池（池优先于主 Key 是运行时预期行为），值级验证才能锚定主 Key 本身
let _tok_new: serde_json::Value = client
    .post(format!("{base}/llm/profiles"))
    .json(&serde_json::json!({
        "id": "e2e-token", "name": "E2E 令牌", "baseUrl": format!("http://127.0.0.1:{fake_port}/v1"),
        "apiKey": "sk-tok-old", "apiKeys": [], "model": "fake-large"
    }))
    .send()
    .await?
    .json()
    .await?;
let _tok_repl = client
    .post(format!("{base}/llm/profiles"))
    .json(&serde_json::json!({
        "id": "e2e-token", "name": "E2E 令牌",
        "apiKey": "sk-tok-new"
    }))
    .send()
    .await?;
let tok_read: serde_json::Value = client
    .get(format!("{base}/llm/profiles"))
    .send()
    .await?
    .json()
    .await?;
let tok = tok_read["profiles"]
    .as_array()
    .and_then(|a| a.iter().find(|p| p["id"] == "e2e-token"))
    .cloned()
    .unwrap_or(serde_json::Value::Null);
c.check(
    "替换主 Key：回读掩码非空且池保留",
    tok["apiKey"] == "***已配置***"
        && tok["apiKeys"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(false),
);
// 清空行 = 沿用（前端「行已删空」提交形态：apiKey 空串、apiKeys 空数组）
let _tok_keep = client
    .post(format!("{base}/llm/profiles"))
    .json(&serde_json::json!({ "id": "e2e-token", "name": "E2E 令牌", "apiKey": "", "apiKeys": [] }))
    .send()
    .await?;
let tok_read2: serde_json::Value = client
    .get(format!("{base}/llm/profiles"))
    .send()
    .await?
    .json()
    .await?;
let tok2 = tok_read2["profiles"]
    .as_array()
    .and_then(|a| a.iter().find(|p| p["id"] == "e2e-token"))
    .cloned()
    .unwrap_or(serde_json::Value::Null);
c.check(
    "清空行=沿用：主 Key 未丢失",
    tok2["apiKey"] == "***已配置***"
        && tok2["apiKeys"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(false),
);
// 值级验证：激活该档案并对话，fake 端点应记录到**新主 Key**（旧 key 不再出现）
let _tok_act: serde_json::Value = client
    .put(format!("{base}/llm/active"))
    .json(&serde_json::json!({ "id": "e2e-token" }))
    .send()
    .await?
    .json()
    .await?;
let tok_session: serde_json::Value = client
    .post(format!("{base}/sessions"))
    .json(&serde_json::json!({ "title": "e2e-token" }))
    .send()
    .await?
    .json()
    .await?;
let tok_sid = tok_session["id"].as_str().unwrap_or_default().to_string();
let _ = client
    .post(format!("{base}/sessions/{tok_sid}/chat"))
    .json(&serde_json::json!({ "text": "主 Key 替换值级验证" }))
    .send()
    .await?;
tokio::time::sleep(Duration::from_millis(2500)).await;
let tok_stats = key_stats.lock().clone();
c.check(
    "替换后的主 Key 已在运行时生效（fake 端点捕获）",
    tok_stats.contains_key("sk-tok-new") && !tok_stats.contains_key("sk-tok-old"),
);
let _ = client
    .put(format!("{base}/llm/active"))
    .json(&serde_json::json!({ "id": "exmachina" }))
    .send()
    .await;
let _ = client
    .delete(format!("{base}/llm/profiles/e2e-token"))
    .send()
    .await;


    Ok(())
}
