//! 端到端验收（Rust 版，替代原 TS 脚本）
//!
//! 覆盖：REST 全端点 + 核心事件流 + 任务图 + 证据 + 三账 + 配置/记忆接口
//! 运行：cargo run -p exm-gateway --example e2e
use axum::response::IntoResponse;
use axum::Json;
use exm_core::{config::ExmConfig, Core};
use parking_lot::Mutex;
#[allow(unused_imports)]
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

struct Checker {
    pass: usize,
    fail: usize,
}

impl Checker {
    fn new() -> Self {
        Checker { pass: 0, fail: 0 }
    }
    fn check(&mut self, name: &str, ok: bool) {
        if ok {
            self.pass += 1;
            println!("  PASS {name}");
        } else {
            self.fail += 1;
            println!("  FAIL {name}");
        }
    }
}

fn find_root() -> std::path::PathBuf {
    let mut dir = std::env::current_dir().unwrap();
    loop {
        if dir
            .join("entities")
            .join("groups")
            .join("exmachina")
            .join("group.json")
            .exists()
        {
            return dir;
        }
        if !dir.pop() {
            panic!("未找到工作区根目录");
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let root = find_root();
    // 编成目录口径 = entities/（见 ExmConfig::agents_dir）；
    // 迁移前脚本写的是已废弃的 agents/，导致注册表读到空编成 → 个体数 0 → 后续断言全挂。
    let entities = root.join("entities");
    // 先快照、后动手：验收期间会经 REST 写配置与激活组指针，跑完必须原样还回工作机/工作树
    // （config.json / active_group / 出厂人格）。快照必须早于任何清理写入，否则会被当成原状。
    let _guards = [
        FileGuard::watch(root.join(".exmachina").join("config.json")),
        FileGuard::watch(entities.join("active_group")),
        // 出厂人格随项目内容交付：验收可临时改写，但跑完必须原样还回（否则工作树被改脏）
        FileGuard::watch(entities.join("groups").join("exmachina").join("SOUL.md")),
    ];
    // 清理上次运行残留（组数据与激活组状态在 entities/ 内持久化）
    let _ = std::fs::remove_dir_all(entities.join("groups").join("product"));
    let _ = std::fs::remove_dir_all(entities.join("groups").join("e2e-chan"));
    let _ = std::fs::write(entities.join("active_group"), "exmachina");
    let _ = std::fs::remove_file(entities.join("active_single"));
    // 清理残留通道（只动 e2e 自己的条目）
    {
        let ch_path = root.join(".exmachina").join("data").join("channels.json");
        if let Ok(raw) = std::fs::read_to_string(&ch_path) {
            if let Ok(mut list) = serde_json::from_str::<Vec<serde_json::Value>>(&raw) {
                list.retain(|c| {
                    c["id"] != "e2e-hook"
                        && c["id"] != "e2e-bound"
                        && c["id"] != "e2e-qq"
                        && c["id"] != "e2e-tg"
                });
                let _ = std::fs::write(
                    &ch_path,
                    serde_json::to_string_pretty(&list).unwrap_or_default(),
                );
            }
        }
    }

    // 开发者本地 config 在验收期间必须原样还回：脚本会经 REST 写配置
    // （档案、鉴权开关等），跑完不还原就等于把工作机配置改成测试残留。
    struct FileGuard {
        path: std::path::PathBuf,
        original: Option<Vec<u8>>,
    }
    impl FileGuard {
        fn watch(path: std::path::PathBuf) -> Self {
            let original = std::fs::read(&path).ok();
            Self { path, original }
        }
    }
    impl Drop for FileGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(bytes) => {
                    let _ = std::fs::write(&self.path, bytes);
                }
                None => {
                    let _ = std::fs::remove_file(&self.path);
                }
            }
        }
    }

    let mut cfg = ExmConfig::load(&root);
    cfg.data_dir = std::env::temp_dir().join(format!("exm-gw-e2e-{}", uuid::Uuid::new_v4()));
    cfg.use_mock = true;
    // e2e 环境自持：不随开发者本地配置漂移（本机 config 曾关掉深层记忆导致记忆断言全挂）
    cfg.memory_enabled = true;
    cfg.agents_dir = entities.clone();
    // 鉴权闸门：验收全程由尾部「后台密钥鉴权」段自持开关；本机 config 若残留 authKey
    // （局域网部署等），会把该段之前的所有 REST 断言挡成 401 → 整轮飘红。起始必须为关。
    cfg.security.auth_key.clear();
    // 首启引导断言自持：本机 config 若已 dismiss 过引导（keySetupDismissed=true），
    // 会把「初始均为假」断言带挂——验收环境必须回到首启原态
    cfg.security.key_setup_dismissed = false;
    // 关键：provider 只在「未配置端点」时才落到测试替身（见 Core::provider 的通道选择）。
    // 本地 config 若残留 baseUrl/apiKey（脚本自己上一次跑就会留下指向临时 fake 服务的端点），
    // 则会绕过 use_mock 去连那个已消失的端口 → 整轮 run.error → 事件流断言全线飘红。
    // 这里显式清空端点与档案，强制走替身，验收结果不再随本机配置漂移。
    cfg.llm.base_url = String::new();
    cfg.llm.api_key = String::new();
    cfg.llm.api_keys = Vec::new();
    cfg.llm.model = "mock".to_string();
    cfg.active_profile = String::new();
    cfg.llm_profiles = Vec::new();
    let data_dir = cfg.data_dir.clone();
    let core = Arc::new(Core::with_config(cfg)?);

    // 事件订阅（等价于 WS 扇出源；WS 只是把同一事件推给浏览器）
    let mut rx = core.subscribe();
    let events_task = tokio::spawn(async move {
        let mut kinds = std::collections::HashSet::new();
        while let Ok(evt) = rx.recv().await {
            kinds.insert(evt.kind.clone());
            if evt.kind == "run.finished" || evt.kind == "run.error" {
                break;
            }
        }
        kinds
    });

    let router = exm_gateway::build_router(core.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let base = format!("http://127.0.0.1:{port}/api");
    let client = reqwest::Client::new();
    let mut c = Checker::new();

    // 健康与编成（编成是数据：校验一致性，不锁定具体数量）
    let health: serde_json::Value = client
        .get(format!("{base}/health"))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "health ok 且个体 >=13",
        health["ok"] == true && health["agents"].as_u64().unwrap_or(0) >= 13,
    );

    // 版本与更新检查（双模式契约:deployMode 合法值;e2e 环境无 /host → none）
    let version: serde_json::Value = client
        .get(format!("{base}/version"))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "version 契约字段齐备",
        version["version"].is_string() && version["gitHash"].is_string(),
    );
    let vcheck: serde_json::Value = client
        .get(format!("{base}/version/check"))
        .send()
        .await?
        .json()
        .await?;
    let dm = vcheck["deployMode"].as_str().unwrap_or("");
    c.check(
        "version/check deployMode 合法",
        matches!(dm, "git" | "image" | "none"),
    );
    if dm == "none" {
        c.check(
            "none 模式带说明且 updateAvailable 为 null",
            vcheck["note"].is_string() && vcheck["updateAvailable"].is_null(),
        );
    }
    let vstatus: serde_json::Value = client
        .get(format!("{base}/version/apply/status"))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "apply/status 契约字段齐备(phase 小写)",
        vstatus["running"].is_boolean()
            && vstatus["log"].is_string()
            && matches!(
                vstatus["phase"].as_str().unwrap_or(""),
                "idle" | "pulling" | "building" | "recreating" | "done" | "failed"
            ),
    );

    let agents: serde_json::Value = client
        .get(format!("{base}/agents"))
        .send()
        .await?
        .json()
        .await?;
    let agents_len = agents.as_array().map(|a| a.len()).unwrap_or(0);
    c.check(
        "agents 数量与 health 一致",
        agents_len as u64 == health["agents"].as_u64().unwrap_or(0) && agents_len >= 13,
    );
    let agents_all_valid = agents
        .as_array()
        .map(|list| {
            list.iter().all(|a| {
                a["identifier"]
                    .as_str()
                    .map(|s| !s.is_empty())
                    .unwrap_or(false)
                    && a["description"]
                        .as_str()
                        .map(|s| !s.trim().is_empty())
                        .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    c.check("agents 契约字段齐备", agents_all_valid);

    let playbooks: serde_json::Value = client
        .get(format!("{base}/playbooks"))
        .send()
        .await?
        .json()
        .await?;
    let playbooks_valid = playbooks
        .as_array()
        .map(|ps| {
            !ps.is_empty()
                && ps.iter().all(|p| {
                    p["steps"]
                        .as_array()
                        .map(|s| !s.is_empty())
                        .unwrap_or(false)
                        && p["terminal"]
                            .as_str()
                            .map(|t| !t.is_empty())
                            .unwrap_or(false)
                })
        })
        .unwrap_or(false);
    c.check("playbooks 非空且步骤齐备", playbooks_valid);

    // 会话与对话
    let session: serde_json::Value = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({"title": "e2e"}))
        .send()
        .await?
        .json()
        .await?;
    let sid = session["id"].as_str().unwrap_or_default().to_string();
    c.check("会话创建", !sid.is_empty());
    // 空标题校验：显式空/纯空白 → 400；缺省（无 body 字段）仍兜底「新会话」
    let blank_title: reqwest::Response = client
        .post(format!("{base}/sessions"))
        .json(&serde_json::json!({"title": "   "}))
        .send()
        .await?;
    c.check(
        "空标题会话被拒 400",
        blank_title.status().as_u16() == 400,
    );

    // 技能包：写入后热装载，任务目标命中触发词即随派发携带
    // 技能目录口径 = entities/skills（见 LocalRegistry::skills_dir），SKILL.md 格式
    let skills_dir = root.join("entities").join("skills");
    let _ = std::fs::create_dir_all(skills_dir.join("e2e-skill"));
    let _ = std::fs::write(
        skills_dir.join("e2e-skill").join("SKILL.md"),
        "---\nid: e2e-skill\nname: E2E 技能\ndescription: 验收用\ntriggers: [调度]\n---\n\nE2E 技能注入标记：调度任务需说明并发池与拓扑排序。\n",
    );

    let resp = client
        .post(format!("{base}/sessions/{sid}/chat"))
        .json(&serde_json::json!({"text": "设计一个支持 1000 个智能体的集群调度方案"}))
        .send()
        .await?;
    c.check("chat 202 受理", resp.status().as_u16() == 202);

    let kinds = tokio::time::timeout(Duration::from_secs(60), events_task).await??;
    for k in [
        "orchestrator.token",
        "unit.token",
        "graph.updated",
        "dispatch.sent",
        "sync.received",
        "ledger.updated",
        "memory.recall",
        "memory.written",
        "run.finished",
    ] {
        c.check(&format!("事件 {k}"), kinds.contains(k));
    }

    // 技能注入断言：派发事件（持久化 Envelope type=dispatch）的指令包含 kind=skill 输入
    let events = core.store.list_events(&sid).unwrap_or_default();
    let skill_injected = events.iter().any(|e| {
        e["type"] == "dispatch"
            && e["payload"]["inputs"]
                .as_array()
                .map(|inputs| inputs.iter().any(|i| i["kind"] == "skill"))
                .unwrap_or(false)
    });
    c.check("技能包随派发注入", skill_injected);

    // 派发审计（agent-budget-audit 3.4）：该轮派发须落审计记录
    // （派发者会话 / 目标个体 / 摘要 / 耗时 / token / 状态）
    let dispatch_audit = core.store.list_dispatch_audit(&sid, 20).unwrap_or_default();
    c.check(
        "子代理派发落审计记录",
        dispatch_audit
            .iter()
            .any(|a| a["agentId"].is_string() && a["durationMs"].is_u64() && a["status"].is_string()),
    );
    // 流式修复回归：token/thinking 增量只走实时广播、不落事件库（否则事件页被逐字刷屏）
    c.check(
        "流式增量不落事件库",
        !events.iter().any(|e| {
            matches!(
                e["payload"]["kind"].as_str(),
                Some("orchestrator.token")
                    | Some("orchestrator.thinking")
                    | Some("unit.token")
                    | Some("unit.thinking")
            )
        }),
    );
    let _ = std::fs::remove_file(skills_dir.join("e2e-skill.json"));

    // 任务图 / 证据 / 三账
    let graph: serde_json::Value = client
        .get(format!("{base}/sessions/{sid}/graph"))
        .send()
        .await?
        .json()
        .await?;
    let nodes = graph["nodes"].as_array().cloned().unwrap_or_default();
    c.check("图 >=4 节点", nodes.len() >= 4);
    c.check(
        "节点全终态",
        nodes.iter().all(|n| {
            matches!(
                n["status"].as_str(),
                Some("done") | Some("blocked") | Some("failed")
            )
        }),
    );
    c.check(
        "存在并行根节点",
        nodes
            .iter()
            .filter(|n| {
                n["dependsOn"]
                    .as_array()
                    .map(|a| a.is_empty())
                    .unwrap_or(false)
            })
            .count()
            >= 2,
    );

    let evidence: serde_json::Value = client
        .get(format!("{base}/sessions/{sid}/evidence"))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "证据账有记录",
        evidence.as_array().map(|a| !a.is_empty()).unwrap_or(false),
    );

    let detail: serde_json::Value = client
        .get(format!("{base}/sessions/{sid}"))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "三账：任务目标",
        !detail["ledger"]["task"]["goal"]
            .as_str()
            .unwrap_or("")
            .is_empty(),
    );
    c.check(
        "三账：证据确认项",
        detail["ledger"]["evidence"]["confirmed"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false),
    );

    let messages: serde_json::Value = client
        .get(format!("{base}/sessions/{sid}/messages"))
        .send()
        .await?
        .json()
        .await?;

    // 会话导出（experience-iteration）：Markdown 骨架 + JSON 全量 + 404
    let export_md: reqwest::Response = client
        .get(format!("{base}/sessions/{sid}/export"))
        .send()
        .await?;
    c.check("导出 Markdown 200", export_md.status().as_u16() == 200);
    let md_disp = export_md
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    c.check(
        "导出 Markdown 附件名与内容类型",
        md_disp.contains("attachment") && md_disp.contains(".md"),
    );
    let md_body = export_md.text().await.unwrap_or_default();
    c.check(
        "导出 Markdown 含标题与消息",
        md_body.contains("# 会话导出") && md_body.contains("## "),
    );
    let export_json: reqwest::Response = client
        .get(format!("{base}/sessions/{sid}/export?format=json"))
        .send()
        .await?;
    c.check("导出 JSON 200", export_json.status().as_u16() == 200);
    let export_json: serde_json::Value = export_json.json().await?;
    c.check(
        "导出 JSON 结构（session/messages/approvals）",
        export_json["session"]["id"].is_string()
            && export_json["messages"].as_array().is_some()
            && export_json["approvals"].as_array().is_some(),
    );
    let export_404: reqwest::Response = client
        .get(format!("{base}/sessions/not-exist-id/export"))
        .send()
        .await?;
    c.check("导出不存在会话 404", export_404.status().as_u16() == 404);

    // 会话内容检索（session-search）：标题/正文命中、片段可定位、空 q、归档排除、limit 钳制
    let search_title: serde_json::Value = client
        .get(format!("{base}/sessions/search"))
        .query(&[("q", "e2e")])
        .send()
        .await?
        .json()
        .await?;
    let title_hit = search_title["hits"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|h| h["sessionId"].as_str() == Some(sid.as_str()));
    c.check(
        "检索：标题命中且 titleMatch=true",
        title_hit
            .as_ref()
            .map(|h| h["titleMatch"].as_bool().unwrap_or(false))
            .unwrap_or(false),
    );
    let search_body: serde_json::Value = client
        .get(format!("{base}/sessions/search"))
        .query(&[("q", "集群调度")])
        .send()
        .await?
        .json()
        .await?;
    let body_hit = search_body["hits"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|h| h["sessionId"].as_str() == Some(sid.as_str()));
    c.check(
        "检索：正文命中且片段含 messageId/关键词",
        body_hit
            .as_ref()
            .map(|h| {
                h["messageMatches"].as_u64().unwrap_or(0) >= 1
                    && h["snippets"]
                        .as_array()
                        .map(|ss| {
                            !ss.is_empty()
                                && ss.iter().all(|sp| sp["messageId"].is_string())
                                && ss.iter().any(|sp| {
                                    sp["text"].as_str().unwrap_or("").contains("集群调度")
                                })
                        })
                        .unwrap_or(false)
            })
            .unwrap_or(false),
    );
    let empty_q: reqwest::Response = client
        .get(format!("{base}/sessions/search"))
        .query(&[("q", "   ")])
        .send()
        .await?;
    c.check("检索：空 q 返回 400", empty_q.status().as_u16() == 400);
    let limit0: serde_json::Value = client
        .get(format!("{base}/sessions/search"))
        .query(&[("q", "e2e"), ("limit", "0")])
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "检索：limit=0 钳制到 1",
        limit0["hits"].as_array().map(|a| a.len() <= 1).unwrap_or(false),
    );
    let limit999: serde_json::Value = client
        .get(format!("{base}/sessions/search"))
        .query(&[("q", "e2e"), ("limit", "999")])
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "检索：limit=999 钳制到 ≤50",
        limit999["hits"]
            .as_array()
            .map(|a| a.len() <= 50)
            .unwrap_or(false),
    );
    // 归档排除：唯一针眼写入后归档 → 不得命中
    let arch = core.create_session("e2e-archived-needle").unwrap();
    core.store
        .add_message(
            &arch.id,
            exm_core::types::MessageRole::User,
            None,
            vec![exm_core::types::Statement::new(
                exm_core::types::SpeechTag::要求,
                "needle-archived-only-9f2",
            )],
        )
        .unwrap();
    core.store.set_session_archived(&arch.id, true).unwrap();
    let archived_hit: serde_json::Value = client
        .get(format!("{base}/sessions/search"))
        .query(&[("q", "needle-archived-only-9f2")])
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "检索：归档会话排除",
        archived_hit["hits"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(false),
    );

    // 历史检查点（session-history）：快照 preview、fork 继承、undo 截断归档
    let snaps: serde_json::Value = client
        .get(format!("{base}/sessions/{sid}/snapshots"))
        .send()
        .await?
        .json()
        .await?;
    let snaps_arr = snaps.as_array().cloned().unwrap_or_default();
    c.check(
        "快照：preview 与首轮输入一致",
        snaps_arr.iter().any(|sn| {
            sn["turn"].as_u64() == Some(1)
                && sn["preview"]
                    .as_str()
                    .map(|p| p.contains("集群调度"))
                    .unwrap_or(false)
        }),
    );
    c.check(
        "快照：既有字段保留",
        snaps_arr.iter().all(|sn| {
            sn["messageCount"].is_number()
                && sn["checkpointIds"].is_array()
                && sn["status"].is_string()
                && sn["createdAt"].is_string()
        }),
    );
    let orig_count = messages.as_array().map(|a| a.len()).unwrap_or(0);
    let fork_resp: reqwest::Response = client
        .post(format!("{base}/sessions/{sid}/fork"))
        .json(&json!({ "turn": 1 }))
        .send()
        .await?;
    c.check("分叉：201", fork_resp.status().as_u16() == 201);
    let fork: serde_json::Value = fork_resp.json().await?;
    let fork_id = fork["id"].as_str().unwrap_or_default().to_string();
    c.check(
        "分叉：标题标注轮次",
        fork["title"].as_str().unwrap_or("").contains("分支"),
    );
    let fork_msgs: serde_json::Value = client
        .get(format!("{base}/sessions/{fork_id}/messages"))
        .send()
        .await?
        .json()
        .await?;
    let fork_count = fork_msgs.as_array().map(|a| a.len()).unwrap_or(0);
    c.check(
        "分叉：继承前 1 轮消息",
        fork_count > 0 && fork_count <= orig_count,
    );
    let orig_after: serde_json::Value = client
        .get(format!("{base}/sessions/{sid}/messages"))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "分叉：原会话消息不变",
        orig_after.as_array().map(|a| a.len()).unwrap_or(0) == orig_count,
    );
    // undo：独立会话写入 2 条消息后撤销到 0 轮
    let undo_sid = core.create_session("e2e-undo").unwrap().id;
    for text in ["undo-one", "undo-two"] {
        core.store
            .add_message(
                &undo_sid,
                exm_core::types::MessageRole::User,
                None,
                vec![exm_core::types::Statement::new(
                    exm_core::types::SpeechTag::要求,
                    text,
                )],
            )
            .unwrap();
    }
    let undo_resp: reqwest::Response = client
        .post(format!("{base}/sessions/{undo_sid}/undo"))
        .json(&json!({ "turn": 0, "restoreFiles": false }))
        .send()
        .await?;
    c.check("撤销：200", undo_resp.status().is_success());
    let undo_msgs: serde_json::Value = client
        .get(format!("{base}/sessions/{undo_sid}/messages"))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "撤销：活跃消息清空",
        undo_msgs.as_array().map(|a| a.is_empty()).unwrap_or(false),
    );
    let undo_archive: serde_json::Value = client
        .get(format!("{base}/sessions/{undo_sid}/archive"))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "撤销：截断内容归档可查",
        undo_archive.as_array().map(|a| a.len() == 2).unwrap_or(false),
    );

    let roles: Vec<String> = messages
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|m| m["role"].as_str().map(|s| s.to_string()))
        .collect();
    c.check(
        "消息含 user/orchestrator",
        ["user", "orchestrator"]
            .iter()
            .all(|r| roles.iter().any(|x| x == r)),
    );
    c.check(
        "个体消息不留主会话（落子会话）",
        !roles.iter().any(|r| r == "unit"),
    );
    // 子会话：sub: 前缀 + parentSession 归属 + 个体消息带有序步骤（思考/工具交错）
    let all_sessions: serde_json::Value = client
        .get(format!("{base}/sessions?all=true"))
        .send()
        .await?
        .json()
        .await?;
    let child = all_sessions
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|s| {
            s["parentSession"].as_str() == Some(sid.as_str())
                && s["title"]
                    .as_str()
                    .map(|t| t.starts_with("sub:"))
                    .unwrap_or(false)
        });
    c.check(
        "派发创建子会话（sub: 前缀 + parentSession 归属）",
        child.is_some(),
    );
    if let Some(child) = child {
        let cm: serde_json::Value = client
            .get(format!(
                "{base}/sessions/{}/messages",
                child["id"].as_str().unwrap_or("")
            ))
            .send()
            .await?
            .json()
            .await?;
        let unit = cm
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .find(|m| m["role"] == "unit");
        c.check("子会话含个体消息", unit.is_some());
        c.check(
            "子会话消息带有序步骤（思考/工具交错）",
            unit.map(|m| {
                m["steps"]
                    .as_array()
                    .map(|a| !a.is_empty())
                    .unwrap_or(false)
            })
            .unwrap_or(false),
        );
    }

    // 配置接口 + Schema
    let cfg_before: serde_json::Value = client
        .get(format!("{base}/config"))
        .send()
        .await?
        .json()
        .await?;
    c.check("config GET（mock 通道）", cfg_before["mock"] == true);
    let put = client
        .put(format!("{base}/config"))
        .json(&serde_json::json!({"maxConcurrency": 3, "memory": {"recallLimit": 6}}))
        .send()
        .await?;
    c.check("config PUT 200", put.status().is_success());
    c.check(
        "配置热生效",
        core.config().max_concurrency == 3 && core.config().memory_recall_limit == 6,
    );
    // 主动搜索纪律开关（proactive-search）：设置面写入 → 热生效读回；
    // 三态语义与既有 search 字段一致（None=沿用），serde 兼容旧 body 缺字段
    let put_search = client
        .put(format!("{base}/config"))
        .json(&serde_json::json!({
            "search": { "provider": "tavily", "endpoint": "", "apiKey": "e2e-k", "maxResults": 5, "proactive": false }
        }))
        .send()
        .await?;
    c.check("search 配置 PUT 200", put_search.status().is_success());
    c.check(
        "proactive 开关热生效",
        !core.config().search.proactive && !core.orchestrator().tools.search_discipline_ready(),
    );
    let put_search2 = client
        .put(format!("{base}/config"))
        .json(&serde_json::json!({
            "search": { "provider": "tavily", "endpoint": "", "apiKey": "e2e-k", "maxResults": 5 }
        }))
        .send()
        .await?;
    c.check("search 旧 body PUT 200", put_search2.status().is_success());
    c.check(
        "旧 body 缺 proactive 默认开",
        core.config().search.proactive && core.orchestrator().tools.search_discipline_ready(),
    );
    // 浏览器搜索（browser-search）：引擎配置透出 + 旧 browser 配置缺字段默认 bing
    let cfg_browser: serde_json::Value = client
        .get(format!("{base}/config"))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "browser 配置透出 searchEngine",
        cfg_browser["browser"]["searchEngine"].is_string(),
    );
    let put_browser = client
        .put(format!("{base}/config"))
        .json(&serde_json::json!({
            "browser": { "executable": "", "headless": true, "timeoutSecs": 30, "maxChars": 8000 }
        }))
        .send()
        .await?;
    c.check("browser 旧 body PUT 200", put_browser.status().is_success());
    c.check(
        "browser 旧 body 缺 searchEngine 默认 bing",
        core.config().browser.search_engine == "bing",
    );
    let schema: serde_json::Value = client
        .get(format!("{base}/config/schema"))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "config schema 有分组与字段",
        schema["groups"]
            .as_array()
            .map(|g| !g.is_empty())
            .unwrap_or(false),
    );

    // 记忆接口
    let stats: serde_json::Value = client
        .get(format!("{base}/memory/stats"))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "记忆库有条目",
        stats["memory"]["total"].as_u64().unwrap_or(0) > 0,
    );
    c.check(
        "个体可靠性统计",
        stats["agentStats"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false),
    );

    let search: serde_json::Value = client
        .post(format!("{base}/memory/search"))
        .json(&serde_json::json!({"query": "架构风险 决策", "limit": 5}))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "记忆检索有命中且带理由",
        search["hits"]
            .as_array()
            .map(|a| {
                !a.is_empty()
                    && a[0]["reasons"]
                        .as_array()
                        .map(|r| !r.is_empty())
                        .unwrap_or(false)
            })
            .unwrap_or(false),
    );

    let added: serde_json::Value = client
        .post(format!("{base}/memory"))
        .json(&serde_json::json!({
            "kind": "fact", "title": "构建入口", "body": "cargo build --release", "pin": true
        }))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "记忆写入（群体）",
        added["id"].as_str().map(|s| !s.is_empty()).unwrap_or(false),
    );

    // 调试视图（8.4）：刚写入的条目必然词项命中（标题/正文含「构建」「入口」）
    let dbg: serde_json::Value = client
        .post(format!("{base}/memory/search"))
        .json(&serde_json::json!({"query": "构建 入口", "limit": 5}))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "记忆检索调试字段：命中词项透出",
        dbg["hits"]
            .as_array()
            .map(|a| {
                a.iter().any(|h| {
                    h["matchedTerms"]
                        .as_object()
                        .map(|m| !m.is_empty())
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false),
    );

    // 个体记忆：写入归属 scout-agent 的私有记忆，验证不可见于他人检索
    let priv_add: serde_json::Value = client
        .post(format!("{base}/memory"))
        .json(&serde_json::json!({
            "kind": "lesson", "title": "侦察体私有教训", "body": "入口侦察需先确认缓存目录",
            "agentId": "scout-agent", "importance": 0.9
        }))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "个体记忆写入",
        priv_add["agentId"].as_str() == Some("scout-agent"),
    );
    let others: serde_json::Value = client
        .post(format!("{base}/memory/search"))
        .json(&serde_json::json!({"query": "入口侦察 缓存", "limit": 5, "agent": "context-agent"}))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "个体记忆隔离：他人检索不可见",
        others["hits"]
            .as_array()
            .map(|a| {
                a.iter()
                    .all(|h| h["entry"]["agentId"].as_str() != Some("scout-agent"))
            })
            .unwrap_or(true),
    );
    let own: serde_json::Value = client
        .post(format!("{base}/memory/search"))
        .json(&serde_json::json!({"query": "入口侦察 缓存", "limit": 5, "agent": "scout-agent"}))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "个体记忆隔离：归属者可见",
        own["hits"]
            .as_array()
            .map(|a| {
                a.iter()
                    .any(|h| h["entry"]["agentId"].as_str() == Some("scout-agent"))
            })
            .unwrap_or(false),
    );

    let list: serde_json::Value = client
        .get(format!("{base}/memory?limit=50"))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "记忆列表含固定项",
        list.as_array()
            .map(|a| a.iter().any(|e| e["pinned"] == true))
            .unwrap_or(false),
    );

    // ---- 知识库（knowledge-base）：REST 全链路 + CLI/REST 同源去重 ----
    // REST 新增 → 检索命中（source 标注 manual）→ 同题再写去重提升 → 删除
    let kadd: serde_json::Value = client
        .post(format!("{base}/knowledge"))
        .json(&serde_json::json!({
            "title": "e2e 部署约束", "body": "构建环境无 C 编译器，依赖必须纯 Rust（e2e 知识链路验证）",
            "tags": ["e2e", "build"]
        }))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "知识新增（新建）",
        kadd["deduped"] == false
            && kadd["entry"]["id"].as_str().map(|s| !s.is_empty()).unwrap_or(false)
            && kadd["entry"]["sourceAgent"].as_str() == Some("manual"),
    );
    let kid = kadd["entry"]["id"].as_str().unwrap_or("").to_string();
    // 同源去重：同题再写（CLI exm knowledge add 与 REST POST 调同一 Core.knowledge，
    // 同源语义在此以 REST 二次写入验证；CLI 面本身有 36 项单测覆盖）
    let kdup: serde_json::Value = client
        .post(format!("{base}/knowledge"))
        .json(&serde_json::json!({
            "title": "e2e 部署约束",
            "body": "构建环境无 C 编译器，依赖必须纯 Rust（e2e 知识链路验证）"
        }))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "知识同题再写去重提升（同源语义）",
        kdup["deduped"] == true && kdup["entry"]["id"].as_str() == Some(kid.as_str()),
    );
    let kquery: serde_json::Value = client
        .get(format!("{base}/knowledge?q={}", "部署约束 纯 Rust"))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "知识检索命中（热度 ≥ 2：REST 去重提升 + 检索命中）",
        kquery["items"]
            .as_array()
            .map(|a| {
                a.iter()
                    .any(|x| x["id"].as_str() == Some(kid.as_str()) && x["hits"].as_u64().unwrap_or(0) >= 1)
            })
            .unwrap_or(false),
    );
    let kdel: serde_json::Value = client
        .delete(format!("{base}/knowledge/{kid}"))
        .send()
        .await?
        .json()
        .await?;
    c.check("知识删除", kdel["ok"] == true);
    let klist_after: serde_json::Value = client
        .get(format!("{base}/knowledge?limit=50"))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "删除后清单不含该条",
        klist_after["items"]
            .as_array()
            .map(|a| a.iter().all(|x| x["id"].as_str() != Some(kid.as_str())))
            .unwrap_or(false),
    );

    // ---- 图像生成（image-generation）：未配置不下发 + 替身生图产物登记 ----
    // 未配置 imageGen 槽位（e2e 起始态）：工具不在 spec 清单（不承诺不存在的能力）
    {
        use exm_core::tools::ToolGateway;
        use exm_core::types::ToolName;
        let specs = ToolGateway::tool_specs(
            &ToolName::full_set(),
            false,
            false,
            false,
            false,
            core.orchestrator().tools.image_gen_ready(),
            core.orchestrator().tools.chart_ready(),
        );
        c.check(
            "生图未配置：image_gen 不在工具清单",
            specs.iter().all(|s| s.name != "image_gen"),
        );
        // 注入替身闭包（Mock 环境等价回接）：执行 → outbox 登记 image 产物
        core.orchestrator().tools.set_image_gen(std::sync::Arc::new(|_p, _s| {
            Box::pin(async {
                Ok(vec![0x89u8, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3, 4])
            })
        }));
        let r = core
            .orchestrator()
            .tools
            .execute(
                "orchestrator",
                "s-e2e-img",
                &[ToolName::ImageGen],
                ToolName::ImageGen,
                &serde_json::json!({ "prompt": "e2e test image", "size": "1024x1024" }),
            )
            .await;
        c.check("生图替身执行成功", r.ok);
        let outbox = core.store.outbox_drain("s-e2e-img").unwrap_or_default();
        c.check(
            "生图产物登记（kind=image）",
            outbox.len() == 1
                && outbox[0]["kind"] == "image"
                && outbox[0]["path"].as_str().unwrap_or("").ends_with(".png"),
        );
    }

    // ---- 图表渲染（chart-rendering）：mock Kroki 端点 → SVG 产物登记 ----
    {
        use exm_core::tools::ToolGateway;
        use exm_core::types::ToolName;
        // 未配置 chart.endpoint（e2e 起始态空串）：render_chart 不在工具清单
        {
            let specs = ToolGateway::tool_specs(
                &ToolName::full_set(),
                false,
                false,
                false,
                false,
                false,
                false,
            );
            c.check(
                "图表未启用：render_chart 不在工具清单",
                specs.iter().all(|s| s.name != "render_chart"),
            );
        }
        // 起 mock Kroki：POST /mermaid/svg 回固定 SVG
        let kroki_app = axum::Router::new().route(
            "/mermaid/svg",
            axum::routing::post(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "image/svg+xml")],
                    "<svg xmlns=\"http://www.w3.org/2000/svg\"><rect width=\"8\" height=\"8\"/></svg>",
                )
            }),
        );
        let kroki_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let kroki_addr = kroki_listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(kroki_listener, kroki_app).await.unwrap() });
        let orch = core.orchestrator();
        orch.tools.set_chart_endpoint(&format!("http://{kroki_addr}"));
        let r = orch
            .tools
            .execute(
                "orchestrator",
                "s-e2e-chart",
                &[ToolName::RenderChart],
                ToolName::RenderChart,
                &serde_json::json!({ "mermaid": "flowchart TD\n  A[入站] --> B[回复]" }),
            )
            .await;
        c.check("图表渲染执行成功", r.ok);
        let outbox = core.store.outbox_drain("s-e2e-chart").unwrap_or_default();
        c.check(
            "图表产物登记（kind=file / .svg）",
            outbox.len() == 1
                && outbox[0]["kind"] == "file"
                && outbox[0]["path"].as_str().unwrap_or("").ends_with(".svg"),
        );
        // 复位（后续段不依赖该端点）
        orch.tools.set_chart_endpoint("");
    }

    // ---- 深度研究（deep-research 4.3）：mock 全链路 + 无后端报错 ----
    {
        use exm_core::tools::ToolGateway;
        use exm_core::types::ToolName;
        // 前置说明：无后端报错路径由单测「研究工具_无后端明确报错」覆盖
        // （e2e 主流程前段已配 tavily 后端，此处起始即有后端）。
        // 配 tavily 后端（ready 门控通过；检索走 spawn —— e2e mock 环境的
        // spawn 执行器走子个体链路，这里注入替身闭包保证确定性回流）
        let put_rs = client
            .put(format!("{base}/config"))
            .json(&serde_json::json!({
                "search": { "provider": "tavily", "endpoint": "", "apiKey": "e2e-k", "maxResults": 5, "proactive": false }
            }))
            .send()
            .await?;
        c.check("研究前置：search 后端 PUT 200", put_rs.status().is_success());
        let orch = core.orchestrator();
        orch.tools.set_spawn_executor(std::sync::Arc::new(|_a, _s, _task, _tools| {
            Box::pin(async {
                Ok("要点：市场年增 40% [来源: https://example.com/market]".to_string())
            })
        }));
        let r = orch
            .tools
            .execute(
                "orchestrator",
                "s-e2e-rs",
                &[ToolName::ResearchReport],
                ToolName::ResearchReport,
                &serde_json::json!({ "topic": "AI 智能体市场", "depth": "quick" }),
            )
            .await;
        c.check(
            "研究 mock 全链路成功",
            r.ok && r.output.contains("结论摘要") && r.output.contains("research-"),
        );
        let outbox = core.store.outbox_drain("s-e2e-rs").unwrap_or_default();
        c.check(
            "研究报告产物登记（kind=file / .md）",
            outbox.len() == 1
                && outbox[0]["kind"] == "file"
                && outbox[0]["path"].as_str().unwrap_or("").ends_with(".md"),
        );
        // 报告落盘：伪造 [9] 拦截为综合推断（mock 聚合响应含 [9]，表仅 1 条）
        if let Some(p) = outbox.first().and_then(|o| o["path"].as_str()) {
            let report = std::fs::read_to_string(root.join(p)).unwrap_or_default();
            c.check(
                "研究伪造引用拦截",
                report.contains("（综合推断）") && report.contains("[1] https://example.com/market"),
            );
        }
        // 事件：四阶段可观测
        let events = core.store.list_events("s-e2e-rs").unwrap_or_default();
        let stages: Vec<String> = events
            .iter()
            .filter(|env| env["payload"]["type"] == "research.stage")
            .filter_map(|env| env["payload"]["payload"]["stage"].as_str().map(String::from))
            .collect();
        c.check(
            "研究四阶段事件",
            stages == vec!["planning", "searching", "synthesizing", "writing"],
        );
    }

    // ---- SyncReport 容错（sync-report-resilience 5.1/5.2）----
    {
        use exm_core::parse;
        // 缺 summary 修复：首条 statement 截断补齐 + repaired 标记
        let raw = r#"```json
{"sourceAgent":"web-agent","taskNodeId":"T1","status":"done","statements":[{"tag":"报告","text":"调研完成，共 3 个来源支撑结论。"}],"confidence":0.8}
```"#;
        match parse::parse_sync_report(raw) {
            Ok(r) => c.check(
                "SyncReport 缺 summary 修复回流",
                r.repaired == Some(true) && r.summary.starts_with("调研完成") && r.confidence == 0.8,
            ),
            Err(e) => {
                c.check("SyncReport 缺 summary 修复回流", false);
                let _ = e;
            }
        }
        // 核心字段缺失不修复：维持原错误（连续失败计数语义不变）
        let raw2 = r#"```json
{"sourceAgent":"web-agent","status":"done","statements":[{"tag":"报告","text":"完成。"}],"summary":"done","confidence":0.8}
```"#;
        let r2 = parse::parse_sync_report(raw2);
        c.check(
            "SyncReport 核心字段缺失不修复",
            r2.is_err() && r2.unwrap_err().contains("missing field `taskNodeId`"),
        );
    }

    // 人设：默认 → 修改 → 热生效 → 重置
    // 对象口径：SOUL 仅「用户面智能体」持有（组主智能体 / 独立智能体），子个体无人格层。
    // 往返用测试自建单体做（避免抹掉出厂 SOUL：内置组出厂人格受保护、machina 随项目内容交付）。
    let _persona_single: serde_json::Value = client
        .post(format!("{base}/singles"))
        .json(&serde_json::json!({ "name": "e2e 人设单体", "identifier": "e2e-persona", "description": "人设往返用例" }))
        .send()
        .await?
        .json()
        .await?;
    let p0: serde_json::Value = client
        .get(format!("{base}/singles/e2e-persona/persona"))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "人设默认（智械体风格）",
        p0["custom"] == false
            && p0["persona"]
                .as_str()
                .map(|s| s.contains("智械体"))
                .unwrap_or(false),
    );
    let unit_put = client
        .put(format!("{base}/agents/context-agent/persona"))
        .json(&serde_json::json!({"persona": "子个体不该有人格"}))
        .send()
        .await?;
    c.check(
        "子个体人设写入被拒（SOUL 仅用户面）",
        unit_put.status().as_u16() == 400,
    );
    let put = client
        .put(format!("{base}/singles/e2e-persona/persona"))
        .json(&serde_json::json!({"persona": "以武侠风格说话，简洁而有侠气。"}))
        .send()
        .await?;
    c.check("人设修改 200", put.status().is_success());
    let p1: serde_json::Value = client
        .get(format!("{base}/singles/e2e-persona/persona"))
        .send()
        .await?
        .json()
        .await?;
    c.check(
        "人设修改后为自定义",
        p1["custom"] == true
            && p1["persona"]
                .as_str()
                .map(|s| s.contains("武侠"))
                .unwrap_or(false),
    );
    // 内置组出厂人格：可改写，但重置（=删文件）受保护——出厂 SOUL 无运行时副本可复原
    let _builtin_put: serde_json::Value = client
        .put(format!("{base}/agents/orchestrator/persona"))
        .json(&serde_json::json!({"persona": "e2e 临时改写出厂人格"}))
        .send()
        .await?
        .json()
        .await?;
    let builtin_reset = client
        .delete(format!("{base}/agents/orchestrator/persona"))
        .send()
        .await?;
    c.check(
        "内置组人设重置被拒（出厂定义受保护）",
        builtin_reset.status().as_u16() == 400,
    );
    let del = client
        .delete(format!("{base}/singles/e2e-persona/persona"))
        .send()
        .await?;
    c.check("人设重置 200", del.status().is_success());
    let p2: serde_json::Value = client
        .get(format!("{base}/singles/e2e-persona/persona"))
        .send()
        .await?
        .json()
        .await?;
    c.check("人设重置后回默认", p2["custom"] == false);
    let _ = client
        .delete(format!("{base}/singles/e2e-persona"))
        .send()
        .await;


    // 跨段共享态（段函数只读引用；段内新会话等局部状态自建；
    // fake 端点三件套由段11 建立写入、段12/13 读取）
    let env = Env {
        core: core.clone(),
        base: base.clone(),
        client: client.clone(),
        sid: sid.clone(),
        agents: agents.clone(),
        search: search.clone(),
        schema: schema.clone(),
        root: root.clone(),
        entities: entities.clone(),
        data_dir: data_dir.clone(),
        agents_len,
        fake_port: std::sync::OnceLock::new(),
        key_stats: std::sync::Arc::new(Mutex::new(std::collections::HashMap::new())),
        prompt_log: std::sync::Arc::new(Mutex::new(Vec::new())),
    };

    stage_agents_group(&env, &mut c).await?;
    stage_platform_docs10(&env, &mut c).await?;
    stage_approval_once(&env, &mut c).await?;
    stage_approval_allowlist(&env, &mut c).await?;
    stage_optimize_experience(&env, &mut c).await?;
    stage_llm_profiles(&env, &mut c).await?;
    stage_key_masking(&env, &mut c).await?;
    stage_agent_edit(&env, &mut c).await?;
    stage_settings_compat(&env, &mut c).await?;
    stage_pool_mask_tokens(&env, &mut c).await?;
    stage_multikey_sticky(&env, &mut c).await?;
    stage_primary_key_replace(&env, &mut c).await?;
    stage_single_persona(&env, &mut c).await?;
    stage_channel_bind_group(&env, &mut c).await?;
    stage_pairing_lifecycle(&env, &mut c).await?;
    stage_backend_auth(&env, &mut c).await?;
    stage_first_run_setup(&env, &mut c).await?;
    stage_body_limit(&env, &mut c).await?;
    stage_session_archive(&env, &mut c).await?;
    stage_archive_by_flow(&env, &mut c).await?;
    stage_tool_budget_events(&env, &mut c).await?;
    stage_event_redaction(&env, &mut c).await?;
    stage_audit_usage(&env, &mut c).await?;
    stage_asset_semantics(&env, &mut c).await?;
    stage_selfcheck(&env, &mut c).await?;
    stage_nexus_topology(&env, &mut c).await?;
    server.abort();
    println!("\n通过 {}｜失败 {}", c.pass, c.fail);
    if c.fail > 0 {
        // process::exit 不跑 Drop——FileGuard 快照(config.json/active_group/出厂人格)必须先手动还原，
        // 否则失败一次就把生产工作树留在改写态（实测：SOUL.md 曾被留在「e2e 临时改写出厂人格」）
        drop(_guards);
        std::process::exit(1);
    }
    println!("e2e 全部通过。");
    Ok(())
}
// ---------------------------------------------------------------- 域分文件（段函数）

mod agents;
mod approval;
mod audit;
mod groups;
mod llm;
mod misc;
mod platform;
mod sessions;

use agents::*;
use audit::*;
use groups::*;
use llm::*;
use misc::*;
use platform::*;
use approval::*;
use sessions::*;

/// 跨段共享态：主会话/编成快照/配置快照/段11 fake 端点（段间延续的少量状态）。
/// 个别字段（search/schema/entities）现由段内自取——字段保留作结构口径。
#[allow(dead_code)]
struct Env {
    core: Arc<Core>,
    base: String,
    client: reqwest::Client,
    sid: String,
    agents: serde_json::Value,
    search: serde_json::Value,
    schema: serde_json::Value,
    root: std::path::PathBuf,
    entities: std::path::PathBuf,
    /// 数据目录（掩码落盘验证直读）
    data_dir: std::path::PathBuf,
    /// 首查个体数（切回默认组后比对）
    agents_len: usize,
    /// 段11 fake 端点端口（段12/13 沿用同一 fake 服务；OnceLock 由段11 写入）
    fake_port: std::sync::OnceLock<u16>,
    key_stats: std::sync::Arc<Mutex<std::collections::HashMap<String, u32>>>,
    prompt_log: std::sync::Arc<Mutex<Vec<String>>>,
}


