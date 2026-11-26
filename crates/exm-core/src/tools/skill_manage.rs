//! tools/skill_manage.rs —— 技能包自管理（SKILL.md 标准格式）
//!
//! 智能体在完成任务后把可复用流程沉淀为技能；后续派发命中触发词时自动注入。
//! 这是「自改进闭环」的写入口：create/update 由模型自主调用，统计由注册表侧车聚合。
//! 工具面默认只给组主智能体与独立指挥体（runtime_allowlist），子个体只读注入。

use super::*;

impl ToolGateway {
    /// 能力对齐 6.4：模型侧**不直写**长期技能——create/update/import 只能提交
    /// 学习候选（须绑定本会话真实证据），人工确认后才落盘（spec：不得把模型文本
    /// 直接写入长期技能）。delete 同理收归人工面（不可逆且模型无权裁决长期资产）。
    pub(crate) fn tool_skill_manage(
        &self,
        session_id: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        let op = args
            .get("op")
            .and_then(|v| v.as_str())
            .unwrap_or("list")
            .trim();
        match op {
            "list" => match self.registry.load_skills() {
                Ok(list) => {
                    let items: Vec<serde_json::Value> = list
                        .iter()
                        .map(|s| {
                            serde_json::json!({
                                "id": s.id,
                                "name": s.name,
                                "description": s.description,
                                "triggers": s.triggers,
                                "agents": s.agents,
                                "source": s.source,
                                "useCount": s.use_count,
                                "successCount": s.success_count,
                                "lastUsedAt": s.last_used_at,
                                "instructionsPreview": s.instructions.chars().take(200).collect::<String>(),
                            })
                        })
                        .collect();
                    ToolResult::ok(
                        serde_json::json!({ "count": items.len(), "skills": items }).to_string(),
                    )
                }
                Err(e) => ToolResult::err(format!("技能包读取失败: {e}")),
            },
            "create" | "update" => {
                let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("").trim();
                let name = args
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                let instructions = args
                    .get("instructions")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if id.is_empty() || name.is_empty() || instructions.is_empty() {
                    return ToolResult::err("create/update 需要 id、name、instructions");
                }
                if instructions.chars().count() > 30_000 {
                    return ToolResult::err("instructions 超过 30000 字符上限");
                }
                let triggers: Vec<String> = list_arg(args, "triggers");
                let agents: Vec<String> = list_arg(args, "agents");
                let scope = if agents.is_empty() {
                    "global".to_string()
                } else {
                    // 限定个体：scope 落到首个适用个体（候选 scope 是单值）
                    format!("agent:{}", agents[0])
                };
                let goal = self
                    .store
                    .get_session(session_id)
                    .ok()
                    .flatten()
                    .map(|s| s.title)
                    .unwrap_or_default();
                // 证据解析：[{kind, reference}]——没有证据 = 模型自述，Core 红线拒
                let evidence = parse_evidence(args);
                let draft = crate::learning_candidates::CandidateDraft {
                    target: "skill",
                    target_id: Some(id),
                    title: name,
                    content: instructions,
                    scope: &scope,
                    triggers: &triggers,
                    source_session: session_id,
                    source_goal: &goal,
                    evidence: &evidence,
                };
                let workspace = self.workspace_for_session_opt(session_id);
                match crate::learning_candidates::CandidateBook::new(&self.store)
                    .submit(workspace.as_deref(), &draft)
                {
                    Ok(cand) => {
                        self.emit_and_persist(crate::types::CoreEvent {
                            kind: "learning.candidate.created".into(),
                            session_id: session_id.to_string(),
                            payload: serde_json::json!({
                                "id": cand.id, "target": cand.target, "title": cand.title,
                                "scope": cand.scope, "confidence": cand.confidence,
                                "by": "agent",
                            }),
                        });
                        ToolResult::ok(
                            serde_json::json!({
                                "ok": true,
                                "candidateId": cand.id,
                                "status": "pending",
                                "confidence": cand.confidence,
                                "evidence": cand.evidence,
                                "note": "已提交为学习候选（未写入技能目录）：候选须绑定本会话真实证据并经人工确认后才生效。确认入口：控制台 /learn approve <candidateId>、REST POST /api/learning/candidates/:id 或 CLI。",
                            })
                            .to_string(),
                        )
                    }
                    Err(reasons) => ToolResult::err(format!(
                        "技能候选被拒：{}",
                        reasons.join("；")
                    )),
                }
            }
            "delete" => ToolResult::err(
                "模型不得直接删除长期技能（不可逆、影响全部后续派发）。请由用户经控制台/REST/CLI 删除：exm skill remove <id> 或 DELETE /api/skills/:id。",
            ),
            "export" => {
                let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("").trim();
                if id.is_empty() {
                    return ToolResult::err("export 需要 id");
                }
                match self.registry.export_skill_md(id) {
                    Ok(Some(md)) => ToolResult::ok(md),
                    Ok(None) => ToolResult::err(format!("技能不存在: {id}")),
                    Err(e) => ToolResult::err(format!("技能导出失败: {e}")),
                }
            }
            "import" => {
                let content = args
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if content.is_empty() {
                    return ToolResult::err("import 需要 content（SKILL.md 原文）");
                }
                // 与 create/update 同红线：导入内容同样是模型提供的文本，只入候选
                let parsed = match self.registry.import_skill_md_dry_run(content) {
                    Ok(p) => p,
                    Err(e) => return ToolResult::err(format!("SKILL.md 解析失败：{e}")),
                };
                let triggers = parsed.triggers.clone();
                let agents = parsed.agents.clone();
                let scope = if agents.is_empty() {
                    "global".to_string()
                } else {
                    format!("agent:{}", agents[0])
                };
                let goal = self
                    .store
                    .get_session(session_id)
                    .ok()
                    .flatten()
                    .map(|s| s.title)
                    .unwrap_or_default();
                let evidence = parse_evidence(args);
                let draft = crate::learning_candidates::CandidateDraft {
                    target: "skill",
                    target_id: Some(&parsed.id),
                    title: &parsed.name,
                    content: &parsed.instructions,
                    scope: &scope,
                    triggers: &triggers,
                    source_session: session_id,
                    source_goal: &goal,
                    evidence: &evidence,
                };
                let workspace = self.workspace_for_session_opt(session_id);
                match crate::learning_candidates::CandidateBook::new(&self.store)
                    .submit(workspace.as_deref(), &draft)
                {
                    Ok(cand) => ToolResult::ok(
                        serde_json::json!({
                            "ok": true,
                            "candidateId": cand.id,
                            "status": "pending",
                            "id": parsed.id,
                            "name": parsed.name,
                            "note": "导入内容已提交为学习候选（未写入技能目录）；人工确认后生效。",
                        })
                        .to_string(),
                    ),
                    Err(reasons) => {
                        ToolResult::err(format!("技能候选被拒：{}", reasons.join("；")))
                    }
                }
            }
            other => ToolResult::err(format!(
                "未知 op: {other}（支持 list / create / update / export / import）"
            )),
        }
    }
}

fn list_arg(args: &serde_json::Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// 解析证据引用数组 `evidence: [{kind, reference}]`；未知 kind 丢弃（丢弃即
/// 视为缺证据，不会被当成已核验）。
fn parse_evidence(
    args: &serde_json::Value,
) -> Vec<(crate::learning_candidates::CandidateEvidenceKind, String)> {
    use crate::learning_candidates::CandidateEvidenceKind as K;
    args.get("evidence")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    let kind = match x.get("kind")?.as_str()? {
                        "tool_receipt" => K::ToolReceipt,
                        "test_result" => K::TestResult,
                        "artifact_file" => K::ArtifactFile,
                        "session_evidence" => K::SessionEvidence,
                        _ => return None,
                    };
                    let reference = x.get("reference")?.as_str()?.trim().to_string();
                    if reference.is_empty() {
                        return None;
                    }
                    Some((kind, reference))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod candidate_gate_tests {
    use super::*;
    use crate::round_trace::ToolCallRecord;

    /// 真实夹具：激活组 + 带工作区的会话 + 本轮内存暂存里的两条回执
    /// （`ok-call`：成功；`bad-call`：失败）。不落盘任何消息——候选是在同一轮内提交的。
    struct Rig {
        _dir: std::path::PathBuf,
        tools: ToolGateway,
        store: std::sync::Arc<Store>,
        session: String,
        allow: Vec<ToolName>,
    }

    async fn rig(tag: &str) -> Rig {
        let dir = std::env::temp_dir().join(format!("exm-sm-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("entities/groups/g1/prompts")).unwrap();
        std::fs::create_dir_all(dir.join("ws")).unwrap();
        std::fs::write(dir.join("ws/README.md"), "x").unwrap();
        let reg = LocalRegistry::new(dir.join("entities/groups/g1")).unwrap();
        reg.create_group(Some("g1".into()), "组一", "").unwrap();
        reg.set_active_group("g1").unwrap();
        let store = std::sync::Arc::new(Store::open(dir.join("data")).unwrap());
        let session = store
            .create_session_in(
                "s",
                "g1",
                Some(dir.join("ws").to_string_lossy().into_owned()),
            )
            .unwrap()
            .id;
        let (events, _rx) = tokio::sync::broadcast::channel::<CoreEvent>(64);
        let tools = ToolGateway::new(
            dir.join("ws"),
            store.clone(),
            std::sync::Arc::new(reg),
            events,
            SecurityConfig::default(),
        );
        let rec = |id: &str, ok: bool| ToolCallRecord {
            call_id: id.into(),
            agent_id: "a".into(),
            tool: "terminal".into(),
            args: serde_json::json!({ "command": "cargo test" }),
            ok,
            duration_ms: 1,
            summary: String::new(),
        };
        crate::round_trace::begin(&session);
        crate::round_trace::push_tool(&session, rec("ok-call", true));
        crate::round_trace::push_tool(&session, rec("bad-call", false));
        Rig {
            _dir: dir,
            tools,
            store,
            session,
            allow: vec![ToolName::SkillManage],
        }
    }

    fn skills_on_disk(r: &Rig) -> usize {
        std::fs::read_dir(r._dir.join("entities/skills"))
            .map(|rd| rd.count())
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn 模型直写_无证据被拒且技能目录不变() {
        let r = rig("no-ev").await;
        let out = r
            .tools
            .execute_named(
                "a",
                &r.session,
                &r.allow,
                "skill_manage",
                &serde_json::json!({
                    "op": "create", "id": "deploy-x", "name": "部署",
                    "instructions": "直接把密钥写进脚本", "triggers": ["部署"]
                }),
            )
            .await;
        assert!(!out.ok, "无证据的模型自述不得写入：{}", out.output);
        assert!(
            out.error.as_deref().unwrap_or("").contains("真实证据"),
            "拒绝原因应指向证据缺失：{:?}",
            out.error
        );
        assert_eq!(skills_on_disk(&r), 0, "被拒候选不得在技能目录留下任何东西");
        crate::round_trace::end(&r.session);
    }

    #[tokio::test]
    async fn 编造回执_被拒() {
        let r = rig("fake").await;
        let out = r
            .tools
            .execute_named(
                "a",
                &r.session,
                &r.allow,
                "skill_manage",
                &serde_json::json!({
                    "op": "create", "id": "deploy-x", "name": "部署",
                    "instructions": "部署前跑全量测试", "triggers": ["部署"],
                    "evidence": [{ "kind": "tool_receipt", "reference": "made-up-call" }]
                }),
            )
            .await;
        assert!(!out.ok, "编造的回执不得通过：{}", out.output);
        assert_eq!(skills_on_disk(&r), 0);
        crate::round_trace::end(&r.session);
    }

    #[tokio::test]
    async fn 失败回执_不能作为证据() {
        let r = rig("bad").await;
        let out = r
            .tools
            .execute_named(
                "a",
                &r.session,
                &r.allow,
                "skill_manage",
                &serde_json::json!({
                    "op": "create", "id": "deploy-x", "name": "部署",
                    "instructions": "部署前跑全量测试", "triggers": ["部署"],
                    "evidence": [{ "kind": "tool_receipt", "reference": "bad-call" }]
                }),
            )
            .await;
        assert!(!out.ok, "失败调用的回执不是证据：{}", out.output);
        assert_eq!(skills_on_disk(&r), 0);
        crate::round_trace::end(&r.session);
    }

    /// 核心验收：带本会话真实回执 → 进入候选（pending），**技能目录仍然没有写入**；
    /// 人工批准之前不生效——spec「不得把模型文本直接写入长期技能」。
    #[tokio::test]
    async fn 真实回执_入候选但不直写_待人工确认() {
        let r = rig("ok").await;
        let out = r
            .tools
            .execute_named(
                "a",
                &r.session,
                &r.allow,
                "skill_manage",
                &serde_json::json!({
                    "op": "create", "id": "deploy-x", "name": "部署",
                    "instructions": "部署前跑全量测试，通过后再发布", "triggers": ["部署"],
                    "evidence": [
                        { "kind": "tool_receipt", "reference": "ok-call" },
                        { "kind": "test_result", "reference": "ok-call" }
                    ]
                }),
            )
            .await;
        assert!(out.ok, "真实回执应进入候选：{:?}", out.error);
        let v: serde_json::Value = serde_json::from_str(&out.output).unwrap();
        assert_eq!(v["status"], "pending", "提交后是候选态，不是已生效技能");
        let cand_id = v["candidateId"].as_str().unwrap().to_string();
        assert_eq!(skills_on_disk(&r), 0, "候选阶段不得写入技能目录");
        let cands = crate::learning_candidates::CandidateBook::new(&r.store).list(None);
        let c = cands.iter().find(|c| c.id == cand_id).expect("候选应入账");
        assert_eq!(c.status, "pending");
        assert_eq!(c.source_session, r.session, "候选须归属来源会话");
        assert!(
            c.evidence.iter().all(|e| e.verification == "verified"),
            "两条证据均应核验通过：{:?}",
            c.evidence
        );
        crate::round_trace::end(&r.session);
    }

    #[tokio::test]
    async fn 模型不得直删长期技能() {
        let r = rig("del").await;
        let out = r
            .tools
            .execute_named(
                "a",
                &r.session,
                &r.allow,
                "skill_manage",
                &serde_json::json!({ "op": "delete", "id": "whatever" }),
            )
            .await;
        assert!(!out.ok, "模型不得直接删除长期技能");
        crate::round_trace::end(&r.session);
    }

    /// 文件证据走真实链路：`filesystem write` 真实落盘 → 回执进本轮暂存 → 同一轮内
    /// 引用该文件作为证据可通过；只 `std::fs` 摆在那里的旧文件、越界文件不可。
    #[tokio::test]
    async fn 文件证据_须由本会话真实写工具产出() {
        let r = rig("file").await;
        // 让 skill_manage 与 filesystem 都在白名单内
        let allow = vec![ToolName::SkillManage, ToolName::Filesystem];
        let call = |reference: &str| {
            serde_json::json!({
                "op": "create", "id": "doc-x", "name": "文档",
                "instructions": "改动后同步更新 NOTES.md", "triggers": ["文档"],
                "evidence": [{ "kind": "artifact_file", "reference": reference }]
            })
        };
        let submit = |args: serde_json::Value| {
            let allow = allow.clone();
            let r = &r;
            async move {
                r.tools
                    .execute_named("a", &r.session, &allow, "skill_manage", &args)
                    .await
            }
        };
        // 只是被摆在工作区里的旧文件（rig 用 std::fs 写的 README.md）：不算本会话产出
        let old = submit(call("README.md")).await;
        assert!(
            !old.ok,
            "未由本会话写工具产出的文件不得作为证据：{}",
            old.output
        );
        // 越界：拒
        let out = submit(call("../../etc/hosts")).await;
        assert!(!out.ok, "越界文件不得作为证据：{}", out.output);
        // 真实写工具落盘一个产物
        let wrote = r
            .tools
            .execute_named(
                "a",
                &r.session,
                &allow,
                "filesystem",
                &serde_json::json!({ "op": "write", "path": "NOTES.md", "content": "# 产物\n" }),
            )
            .await;
        assert!(wrote.ok, "filesystem write 应成功：{:?}", wrote.error);
        assert!(
            std::path::Path::new(&r._dir).join("ws/NOTES.md").is_file(),
            "文件应真实落盘"
        );
        // 同一轮内引用它：过
        let good = submit(call("NOTES.md")).await;
        assert!(good.ok, "本会话真实写出的文件应核验通过：{:?}", good.error);
        crate::round_trace::end(&r.session);
    }
}
