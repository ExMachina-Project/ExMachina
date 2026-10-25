//! 智能连结网络执行器（实验性）——把任务派发给外部连结体执行。
//!
//! 三层结构：全连结指挥体（本集群指挥体）→ 连结指挥体（外部连结体的主智能体）→ 子个体。
//! 连结体两种形态（可混用）：
//! - exmachina：另一台机器/进程的 EXMACHINA 网关（REST + 轮询取结果）
//! - CLI 智能体：opencode / codex / claude 等本机命令行智能体（其子代理即子个体）
//!
//! 执行契约：输入目标与验收，输出（摘要, 全文）。失败/超时/非零退出返回 Err。
//!
//! 轮询契约：对端 `GET /api/sessions/:id/messages` 返回**尾部窗口**（tail 200）——
//! 新回复识别不依赖条数比较（窗口滚满后 len 恒定，按数比对会永久卡死），
//! 而以「已见末条消息 id」为游标定位增量（见 [`messages_after`]）。

use crate::config::{ExmConfig, LinkTarget};
use anyhow::{Context, Result};

pub const DEFAULT_TIMEOUT_SECS: u64 = 900;
/// probe（连通性测试）的单请求超时：探测不该等执行级超时
const PROBE_TIMEOUT_SECS: u64 = 10;

fn find_link<'a>(cfg: &'a ExmConfig, link_id: &str) -> Result<&'a LinkTarget> {
    cfg.nexus
        .links
        .iter()
        .find(|l| l.id == link_id && l.enabled)
        .context(format!("连结体不存在或未启用: {link_id}"))
}

/// 派发入口：按连结体类型路由
pub async fn execute_link(cfg: &ExmConfig, link_id: &str, objective: &str, acceptance: &[String]) -> Result<(String, String)> {
    let link = find_link(cfg, link_id)?;
    let prompt = build_prompt(objective, acceptance);
    match link.kind.as_str() {
        "exmachina" => execute_exmachina(cfg, link, &prompt).await,
        "opencode" | "codex" | "claude" | "custom" => execute_cli(link, &prompt).await,
        other => Err(anyhow::anyhow!("未知连结体类型: {other}")),
    }
}

/// 连通性测试：验证连结体配置可达、鉴权通过、（CLI）命令可执行。
/// 返回人类可读的诊断文本（成功为空串）。不做任务级执行，秒级返回。
pub async fn probe_link(cfg: &ExmConfig, link_id: &str) -> Result<String> {
    let link = find_link(cfg, link_id)?;
    match link.kind.as_str() {
        "exmachina" => {
            let base = link.endpoint.trim_end_matches('/');
            if base.is_empty() {
                anyhow::bail!("未配置网关地址");
            }
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(PROBE_TIMEOUT_SECS))
                .build()?;
            let r = client
                .get(format!("{base}/api/version"))
                .header("X-Auth-Key", &link.api_key)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("网关不可达: {e}"))?;
            match r.status().as_u16() {
                200..=299 => Ok(String::new()),
                401 | 403 => anyhow::bail!("鉴权失败：请检查访问密钥（X-Auth-Key）"),
                404 => anyhow::bail!("网关可达但 /api/version 不存在：对端不是 EXMACHINA 网关或版本过旧"),
                code => anyhow::bail!("对端返回 HTTP {code}"),
            }
        }
        // CLI 连结体：探测命令在 PATH 上是否可解析（含 custom 自定义命令行）
        "opencode" | "codex" | "claude" | "custom" => {
            let argv = cli_command(link);
            let program = argv.first().cloned().unwrap_or_default();
            if program.is_empty() {
                anyhow::bail!("未配置命令");
            }
            // Windows 标准环境无 which（对应物是 where.exe），跨平台探测
            #[cfg(windows)]
            let finder = "where";
            #[cfg(not(windows))]
            let finder = "which";
            let found = tokio::process::Command::new(finder)
                .arg(&program)
                .stdin(std::process::Stdio::null())
                .output()
                .await
                .map(|o| o.status.success())
                .unwrap_or(false);
            if found {
                Ok(String::new())
            } else {
                anyhow::bail!("命令 {program} 不在 PATH 上")
            }
        }
        other => Err(anyhow::anyhow!("未知连结体类型: {other}")),
    }
}

fn build_prompt(objective: &str, acceptance: &[String]) -> String {
    if acceptance.is_empty() {
        objective.to_string()
    } else {
        format!("{objective}\n\n验收标准：\n{}", acceptance.iter().map(|a| format!("- {a}")).collect::<Vec<_>>().join("\n"))
    }
}

// ---------------------------------------------------------------- exmachina（远程网关）

async fn execute_exmachina(_cfg: &ExmConfig, link: &LinkTarget, prompt: &str) -> Result<(String, String)> {
    let base = link.endpoint.trim_end_matches('/');
    if base.is_empty() {
        anyhow::bail!("连结体 {} 未配置网关地址", link.id);
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    // 鉴权头在各请求处直接 .header("X-Auth-Key", ...) 注入

    // 1) 复用/创建专属会话（标题约定 nexus-<link_id>，同连结体的任务在同一会话内延续上下文）
    let title = format!("nexus-{}", link.id);
    let sessions: serde_json::Value = client
        .get(format!("{base}/api/sessions?all=true"))
        .header("X-Auth-Key", &link.api_key)
        .send()
        .await?
        .json()
        .await
        .context("会话清单解析失败")?;
    let session_id = sessions
        .as_array()
        .and_then(|list| list.iter().find(|s| s["title"] == serde_json::json!(title)))
        .and_then(|s| s["id"].as_str())
        .map(|s| s.to_string());
    let session_id = match session_id {
        Some(id) => id,
        None => {
            let r: serde_json::Value = client
                .post(format!("{base}/api/sessions"))
                .header("X-Auth-Key", &link.api_key)
                .json(&serde_json::json!({ "title": title }))
                .send()
                .await?
                .json()
                .await
                .context("会话创建失败")?;
            r["id"].as_str().context("会话创建响应缺少 id")?.to_string()
        }
    };

    // 2) 记录已见末条消息 id（新回复识别游标——消息接口是尾部窗口语义，
    //    按条数比对在窗口滚满后失灵）。游标必须成功建立：拉取失败就放弃本轮，
    //    不降级为「全窗视为增量」——复用会话里旧任务的回复混进增量，
    //    会把上一次的结论错当本次产出（张冠李戴且无感知）。
    let before: serde_json::Value = client
        .get(format!("{base}/api/sessions/{session_id}/messages"))
        .header("X-Auth-Key", &link.api_key)
        .send()
        .await
        .context("游标建立失败：对端既有消息拉取不可达")?
        .json()
        .await
        .context("游标建立失败：消息清单解析失败")?;
    let cursor = before
        .as_array()
        .and_then(|a| a.last())
        .and_then(|m| m["id"].as_str())
        .map(str::to_string);

    let r = client
        .post(format!("{base}/api/sessions/{session_id}/chat"))
        .header("X-Auth-Key", &link.api_key)
        .json(&serde_json::json!({ "text": prompt }))
        .send()
        .await?;
    if r.status().as_u16() >= 400 {
        let body = r.text().await.unwrap_or_default();
        anyhow::bail!("对端拒绝对话: {}", body.chars().take(200).collect::<String>());
    }

    // 3) 轮询等待新回复（对端收束后会有新 orchestrator 消息）
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS);
    let poll_interval = std::time::Duration::from_secs(3);
    // 瞬时抖动容忍：单次轮询失败不放弃（对端可能仍在正常执行），连续失败才退出
    let mut poll_failures = 0u32;
    const POLL_FAILURE_TOLERANCE: u32 = 10; // ≈30s 连续失败
    loop {
        tokio::time::sleep(poll_interval).await;
        if std::time::Instant::now() > deadline {
            anyhow::bail!("连结体执行超时（{}s）", DEFAULT_TIMEOUT_SECS);
        }
        let msgs: serde_json::Value = match client
            .get(format!("{base}/api/sessions/{session_id}/messages"))
            .header("X-Auth-Key", &link.api_key)
            .send()
            .await
        {
            Ok(r) => r.json().await.unwrap_or(serde_json::Value::Null),
            Err(_) => {
                poll_failures += 1;
                if poll_failures >= POLL_FAILURE_TOLERANCE {
                    anyhow::bail!("轮询对端连续 {POLL_FAILURE_TOLERANCE} 次失败，放弃等待");
                }
                continue;
            }
        };
        poll_failures = 0;
        let Some(list) = msgs.as_array() else { continue };
        let fresh = messages_after(list, cursor.as_deref());
        // 无新增：继续等
        if fresh.is_empty() {
            continue;
        }
        // 以本轮 user 回显为锚（见 [`anchor_of_echo`]）：锚之前的一律当旧账
        let Some(anchor) = anchor_of_echo(&fresh, prompt) else {
            continue; // 本轮回显尚未落库——继续等
        };
        let since = &fresh[anchor + 1..];
        // 对端执行失败早退：chat 是 202 异步受理，spawn 失败只经 WS 广播 run.error、
        // 不落 orchestrator 消息——识别 system 警告（含失败标记）直接带错返回，不再干等超时
        if let Some(err) = failure_text_since(since) {
            anyhow::bail!("对端执行失败: {}", err.chars().take(300).collect::<String>());
        }
        // 取锚后最后一条 orchestrator 陈述文本
        if let Some(text) = orchestrator_reply_since(since) {
            if !text.trim().is_empty() {
                let summary: String = text.chars().take(400).collect();
                return Ok((summary, text));
            }
        }
    }
}

/// 尾部窗口内定位增量：返回游标（已见末条 id）之后的消息。
/// 游标已在窗口外（被滚没）时返回整个窗口——宁可重取不可漏取。
fn messages_after(list: &[serde_json::Value], cursor: Option<&str>) -> Vec<serde_json::Value> {
    let Some(cursor) = cursor else { return list.to_vec() };
    match list.iter().position(|m| m["id"].as_str() == Some(cursor)) {
        Some(idx) => list[idx + 1..].to_vec(),
        None => list.to_vec(),
    }
}

/// 本轮 user 回显的锚点：窗口内最后一条陈述含本轮 prompt 的 user 消息下标。
/// 必须按 prompt 文本匹配而非「任意 user 消息」——游标滚出窗口 + 回显未落库时，
/// 最后一条 user 是上一轮的提问，按任意 user 锚定会把旧结论错当本次产出。
fn anchor_of_echo(fresh: &[serde_json::Value], prompt: &str) -> Option<usize> {
    fresh.iter().rposition(|m| {
        m["role"] == serde_json::json!("user")
            && m["statements"]
                .as_array()
                .map(|sts| sts.iter().filter_map(|s| s["text"].as_str()).collect::<Vec<_>>().join(" "))
                .map(|t| t.contains(prompt))
                .unwrap_or(false)
    })
}

/// 锚后 system 消息里的失败标记（含「失败 / 错误 / error」的第一个命中文本）
fn failure_text_since(since: &[serde_json::Value]) -> Option<String> {
    since
        .iter()
        .filter(|m| m["role"] == serde_json::json!("system"))
        .filter_map(|m| {
            m["statements"]
                .as_array()
                .map(|sts| sts.iter().filter_map(|s| s["text"].as_str()).collect::<Vec<_>>().join(" "))
        })
        .find(|text| text.contains("失败") || text.contains("错误") || text.to_lowercase().contains("error"))
}

/// 锚后最后一条 orchestrator 陈述的合并文本
fn orchestrator_reply_since(since: &[serde_json::Value]) -> Option<String> {
    since
        .iter()
        .rev()
        .filter(|m| m["role"] == serde_json::json!("orchestrator"))
        .find_map(|m| m["statements"].as_array())
        .map(|sts| {
            sts.iter()
                .filter_map(|s| s["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n\n")
        })
}

// ---------------------------------------------------------------- CLI 智能体

fn cli_command(link: &LinkTarget) -> Vec<String> {
    if !link.command.trim().is_empty() {
        return link.command.split_whitespace().map(|s| s.to_string()).collect();
    }
    match link.kind.as_str() {
        // 交互式 CLI 的非交互形态：提示词作为位置参数
        "opencode" => vec!["opencode".into(), "run".into()],
        "codex" => vec!["codex".into(), "exec".into()],
        "claude" => vec!["claude".into(), "-p".into()],
        _ => vec![],
    }
}

async fn execute_cli(link: &LinkTarget, prompt: &str) -> Result<(String, String)> {
    let mut cmd = cli_command(link);
    if cmd.is_empty() {
        anyhow::bail!("连结体 {} 未配置命令", link.id);
    }
    let program = cmd.remove(0);
    cmd.push(prompt.to_string());
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        tokio::process::Command::new(&program)
            .args(&cmd)
            .stdin(std::process::Stdio::null())
            // 超时取消 output() future 时随 drop 杀掉子进程——否则孤儿进程继续跑，超时形同虚设
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("CLI 智能体执行超时（{}s）", DEFAULT_TIMEOUT_SECS))?
    .with_context(|| format!("启动 CLI 智能体失败: {program}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("CLI 智能体退出码 {:?}: {}", output.status.code(), err.chars().take(200).collect::<String>());
    }
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    if text.trim().is_empty() {
        anyhow::bail!("CLI 智能体无输出");
    }
    let summary: String = text.chars().take(400).collect();
    Ok((summary, text))
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(links: Vec<LinkTarget>) -> ExmConfig {
        let dir = std::env::temp_dir().join(format!("exm-nexus-cfg-{}", crate::types::new_id()));
        let mut cfg = ExmConfig::load(&dir);
        cfg.nexus.links = links;
        cfg
    }

    #[tokio::test]
    async fn cli连结_执行与摘要() {
        #[cfg(windows)]
        let cmd = "cmd /c echo";
        #[cfg(not(windows))]
        let cmd = "echo";
        let link = LinkTarget { id: "cli-1".into(), name: "回显连结".into(), kind: "custom".into(), command: cmd.into(), ..Default::default() };
        let cfg = cfg_with(vec![link.clone()]);
        let (summary, output) = execute_link(&cfg, "cli-1", "hello nexus", &[]).await.unwrap();
        assert!(output.contains("hello nexus"), "{output}");
        assert!(!summary.is_empty());
    }

    #[tokio::test]
    async fn cli连结_非零退出报错() {
        #[cfg(windows)]
        let cmd = "cmd /c exit 3";
        #[cfg(not(windows))]
        let cmd = "sh -c \"exit 3\"";
        let link = LinkTarget { id: "cli-bad".into(), name: "失败连结".into(), kind: "custom".into(), command: cmd.into(), ..Default::default() };
        let cfg = cfg_with(vec![link]);
        let r = execute_link(&cfg, "cli-bad", "x", &[]).await;
        assert!(r.is_err());
    }

    #[test]
    fn 连结体不存在或未启用() {
        let cfg = cfg_with(vec![LinkTarget { id: "off".into(), name: "停用".into(), kind: "custom".into(), command: "echo".into(), enabled: false, ..Default::default() }]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let r = rt.block_on(execute_link(&cfg, "off", "x", &[]));
        assert!(r.is_err());
        assert!(r.unwrap_err().to_string().contains("未启用"));
        let r2 = rt.block_on(execute_link(&cfg, "ghost", "x", &[]));
        assert!(r2.is_err());
    }

    #[test]
    fn 轮询游标_尾部窗口定位增量() {
        // 窗口滚满后 len 恒定（200 条），按游标仍能取出游标之后的新消息
        let mk = |id: &str, role: &str| serde_json::json!({ "id": id, "role": role, "statements": [{ "tag": "报告", "text": format!("msg-{id}") }] });
        let window: Vec<serde_json::Value> = (0..200).map(|i| mk(&format!("m{i}"), "user")).collect();
        // 游标在窗口内：只取其后
        let fresh = messages_after(&window, Some("m198"));
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0]["id"], serde_json::json!("m199"));
        // 游标是末条：无新增
        assert!(messages_after(&window, Some("m199")).is_empty());
        // 游标被滚出窗口：整个窗口视为潜在增量（安全降级，宁可重取）
        assert_eq!(messages_after(&window, Some("m-old")).len(), 200);
        // 无游标（首次）：全部
        assert_eq!(messages_after(&window, None).len(), 200);
    }

    /// 回复认领必须锚定本轮 user 回显（按 prompt 文本匹配）：游标失效降级时
    /// 旧任务的回复混在窗口里，锚之前的一律当旧账——防「上次结论错当本次产出」
    #[test]
    fn 轮询认领_锚定本轮user回显() {
        let mk = |id: &str, role: &str, text: &str| {
            serde_json::json!({ "id": id, "role": role, "statements": [{ "tag": "报告", "text": text }] })
        };
        // 旧账（上一轮提问 + 回复）+ 本轮回显 + system 警告 + 新回复
        let fresh = vec![
            mk("m1", "user", "旧目标"),
            mk("m2", "orchestrator", "旧结论"),
            mk("m3", "user", "新目标\n\n验收标准：\n- 完"),
            mk("m4", "system", "警告：上游错误"),
            mk("m5", "orchestrator", "新结论"),
        ];
        let anchor = anchor_of_echo(&fresh, "新目标").expect("本轮回显应建立锚点");
        let since = &fresh[anchor + 1..];
        assert_eq!(orchestrator_reply_since(since).as_deref(), Some("新结论"));
        assert_eq!(failure_text_since(since).as_deref(), Some("警告：上游错误"));

        // 本轮回显未落库（窗口里只有上一轮的旧账）：无锚点 → 不认领任何旧结论
        let stale_window = vec![mk("m1", "user", "旧目标"), mk("m2", "orchestrator", "旧结论")];
        assert!(anchor_of_echo(&stale_window, "新目标").is_none());
        // 回显是旧账的另一轮目标：同样不为本轮建立锚点
        assert!(anchor_of_echo(&stale_window, "旧目标").is_some(), "匹配旧目标时锚点存在（语义自检）");
    }

    #[tokio::test]
    async fn probe_cli连结_命令存在性() {
        #[cfg(windows)]
        let cmd = "cmd /c echo";
        #[cfg(not(windows))]
        let cmd = "echo";
        let link = LinkTarget { id: "p1".into(), name: "探测".into(), kind: "custom".into(), command: cmd.into(), ..Default::default() };
        let cfg = cfg_with(vec![link]);
        assert!(probe_link(&cfg, "p1").await.is_ok());
        // 命令不存在：带人话诊断失败
        let bad = LinkTarget { id: "p2".into(), name: "不存在".into(), kind: "custom".into(), command: "no-such-cli-xyz".into(), ..Default::default() };
        let cfg2 = cfg_with(vec![bad]);
        let err = probe_link(&cfg2, "p2").await.unwrap_err().to_string();
        assert!(err.contains("不在 PATH"), "{err}");
    }

    #[tokio::test]
    async fn probe_exmachina_未配地址报错() {
        let link = LinkTarget { id: "p3".into(), name: "网关".into(), kind: "exmachina".into(), ..Default::default() };
        let cfg = cfg_with(vec![link]);
        let err = probe_link(&cfg, "p3").await.unwrap_err().to_string();
        assert!(err.contains("网关地址"), "{err}");
    }
}
