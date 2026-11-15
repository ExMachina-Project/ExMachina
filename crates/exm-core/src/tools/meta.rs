//! tools/meta.rs —— Meta Agent 工具组（meta-agent 变更，实验性）
//! meta_observe：读运行数据聚合（只读）
//! meta_propose：结构化改进提议 → [meta] 审批单
//! meta_build：新智能体/组构建草稿 → [meta] 审批单
//! 全部经 execute() 统一审计；持久改动一律走审批（无自动生效路径）。

use super::*;

impl ToolGateway {
    /// meta 工具组入口（op 由工具名路由）
    pub(crate) async fn tool_meta(
        &self,
        agent_id: &str,
        session_id: &str,
        tool: ToolName,
        args: &serde_json::Value,
    ) -> ToolResult {
        match tool {
            ToolName::MetaObserve => self.meta_observe(args),
            ToolName::MetaPropose => self.meta_propose(agent_id, session_id, args),
            ToolName::MetaBuild => self.meta_build(agent_id, session_id, args),
            _ => ToolResult::err("非 meta 工具"),
        }
    }

    /// 读聚合观测（7 天窗口）：全体或指定 agent
    fn meta_observe(&self, args: &serde_json::Value) -> ToolResult {
        let obs = crate::meta::system_observation(&self.store, &self.registry);
        let target = args.get("agent").and_then(|v| v.as_str()).unwrap_or("");
        let filtered = if target.is_empty() {
            obs
        } else {
            let agents = obs
                .agents
                .into_iter()
                .filter(|a| a.identifier == target)
                .collect();
            crate::meta::SystemObservation { agents, ..obs }
        };
        if !target.is_empty() && filtered.agents.is_empty() {
            return ToolResult::err(format!("个体不存在或近期无审计记录: {target}"));
        }
        ToolResult::ok(serde_json::to_string_pretty(&filtered).unwrap_or_default())
    }

    /// 改进提议 → [meta] 审批单。
    /// kind=prompt（提示词补段）/ adaptation（经验要点）/ roster（成员增删调，≤5 op）
    fn meta_propose(&self, agent_id: &str, session_id: &str, args: &serde_json::Value) -> ToolResult {
        let proposal = match validate_proposal(self, args) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e),
        };
        // 未决单去重：同 target+kind 已 pending 则拒
        if let Ok(pending) = self.store.list_approvals(Some("pending"), 200) {
            let sig = format!("[meta] {} {}", proposal.kind, proposal.target);
            if pending
                .iter()
                .any(|p| p.command.starts_with(&sig))
            {
                return ToolResult::err(format!(
                    "已有未决提议（{sig}…）: 等待该审批处理后再提交同类提议"
                ));
            }
        }
        let req = self.make_meta_approval(agent_id, session_id, &proposal);
        match self.store.add_approval(&req) {
            Ok(_) => {
                // 实验可观测（assistant-parity）：提议生成事件（meta.proposal，
                // kind+target 明细）——广播 + 落事件库（事件页可检索提议轨迹）
                self.emit_and_persist(CoreEvent {
                    kind: "meta.proposal".into(),
                    session_id: session_id.to_string(),
                    payload: serde_json::json!({
                        "phase": "created",
                        "approvalId": req.id,
                        "agentId": agent_id,
                        "kind": proposal.kind,
                        "target": proposal.target,
                    }),
                });
                let _ = self.events.send(CoreEvent {
                    kind: "approval.required".into(),
                    session_id: session_id.to_string(),
                    payload: serde_json::json!({
                        "approvalId": req.id, "agentId": agent_id,
                        "command": req.command,
                        "meta": { "kind": proposal.kind, "target": proposal.target },
                    }),
                });
                ToolResult::ok(format!(
                    "提议已落审批单 {}（等待人工审批;批准后自动应用,拒绝则回流）",
                    req.id
                ))
            }
            Err(e) => ToolResult::err(format!("审批单落库失败: {e}")),
        }
    }

    /// 构建提议：定义草稿由模型给出（工具零 LLM），落 kind=build 审批单
    fn meta_build(&self, agent_id: &str, session_id: &str, args: &serde_json::Value) -> ToolResult {
        let description = args
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if description.len() < 8 {
            return ToolResult::err("description 过短（≥8 字符,说清新个体的职责）");
        }
        let definition = args.get("definition").cloned().unwrap_or(serde_json::Value::Null);
        if !definition.is_object() {
            return ToolResult::err("definition 必须是对象（agent 定义草稿:name/domain/tier/description/tools/prompt）");
        }
        let name = definition.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if name.trim().is_empty() {
            return ToolResult::err("definition.name 不能为空");
        }
        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if prompt.len() < 16 {
            return ToolResult::err("prompt 过短（≥16 字符的系统提示词草稿）");
        }
        let as_group = args
            .get("asGroup")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let proposal = MetaProposal {
            kind: "build".into(),
            target: if as_group {
                format!("group:{name}")
            } else {
                name.to_string()
            },
            change: serde_json::json!({
                "asGroup": as_group,
                "definition": definition,
                "prompt": prompt,
            }),
            rationale: description,
        };
        let req = self.make_meta_approval(agent_id, session_id, &proposal);
        match self.store.add_approval(&req) {
            Ok(_) => {
                let _ = self.events.send(CoreEvent {
                    kind: "approval.required".into(),
                    session_id: session_id.to_string(),
                    payload: serde_json::json!({
                        "approvalId": req.id, "agentId": agent_id,
                        "command": req.command,
                        "meta": { "kind": "build", "target": proposal.target },
                    }),
                });
                ToolResult::ok(format!(
                    "构建提议已落审批单 {}（批准后注册为{}）",
                    req.id,
                    if as_group { "组" } else { "个体" }
                ))
            }
            Err(e) => ToolResult::err(format!("审批单落库失败: {e}")),
        }
    }

    /// [meta] 审批单构造（提议 JSON 存 meta_json 扩展位）
    fn make_meta_approval(
        &self,
        agent_id: &str,
        session_id: &str,
        p: &MetaProposal,
    ) -> crate::types::ApprovalRequest {
        crate::types::ApprovalRequest {
            id: crate::types::new_id()[..8].to_string(),
            session_id: session_id.to_string(),
            node_id: None,
            agent_id: agent_id.to_string(),
            command: format!(
                "[meta] {} {}: {}",
                p.kind,
                p.target,
                p.rationale.chars().take(60).collect::<String>()
            ),
            status: "pending".into(),
            result: None,
            created_at: crate::types::now_iso(),
            decided_at: None,
            origin_user: None,
            origin_role: None,
            meta_json: Some(serde_json::to_string(p).unwrap_or_default()),
        }
    }
}

/// 结构化提议（meta_json 暂存格式）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MetaProposal {
    /// prompt | adaptation | roster | build
    pub kind: String,
    /// 目标 agent identifier（build 时为新个体名/group:<name>）
    pub target: String,
    /// kind 对应的结构化变更
    pub change: serde_json::Value,
    /// 提议理由（必填,展示给审批人）
    pub rationale: String,
}

/// 提议校验（落单前全验,坏单直接工具错误）
fn validate_proposal(gw: &ToolGateway, args: &serde_json::Value) -> Result<MetaProposal, String> {
    let kind = args.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    if !matches!(kind, "prompt" | "adaptation" | "roster") {
        return Err(format!("kind 必须是 prompt/adaptation/roster 之一: {kind}"));
    }
    let target = args.get("target").and_then(|v| v.as_str()).unwrap_or("");
    if target.trim().is_empty() {
        return Err("target 不能为空".into());
    }
    let rationale = args.get("rationale").and_then(|v| v.as_str()).unwrap_or("");
    if rationale.trim().len() < 8 {
        return Err("rationale 过短（≥8 字符,说明依据的数据与预期收益）".into());
    }
    let change = args.get("change").cloned().unwrap_or(serde_json::Value::Null);
    match kind {
        "prompt" => {
            let patch = change
                .get("promptPatch")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if patch.trim().len() < 16 {
                return Err("change.promptPatch 过短（≥16 字符的提示词补段）".into());
            }
        }
        "adaptation" => {
            let content = change.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if content.trim().len() < 16 {
                return Err("change.content 过短（≥16 字符的经验要点全文）".into());
            }
        }
        "roster" => {
            let Some(ops) = change.get("ops").and_then(|v| v.as_array()) else {
                return Err("change.ops 必须是数组".into());
            };
            if ops.is_empty() || ops.len() > 5 {
                return Err("change.ops 数量须在 1~5".into());
            }
            for (i, op) in ops.iter().enumerate() {
                let opk = op.get("op").and_then(|v| v.as_str()).unwrap_or("");
                if !matches!(opk, "add" | "remove" | "update") {
                    return Err(format!("ops[{i}].op 必须是 add/remove/update: {opk}"));
                }
            }
            // 目标存在性（update/remove 要求个体在激活组内）
            let defs = gw.registry.list();
            {
                let exists = |id: &str| defs.iter().any(|d| d.identifier == id);
                if !exists(target) {
                    return Err(format!("目标个体不在激活组: {target}"));
                }
                for op in ops {
                    if op.get("op").and_then(|v| v.as_str()) == Some("remove")
                        || op.get("op").and_then(|v| v.as_str()) == Some("update")
                    {
                        let oid = op.get("identifier").and_then(|v| v.as_str()).unwrap_or("");
                        if !exists(oid) {
                            return Err(format!("ops 中个体不存在: {oid}"));
                        }
                    }
                }
            }
        }
        _ => {}
    }
    Ok(MetaProposal {
        kind: kind.to_string(),
        target: target.to_string(),
        change,
        rationale: rationale.to_string(),
    })
}

#[cfg(test)]
mod meta_tool_tests {
    use super::*;

    fn tool_gw() -> ToolGateway {
        let dir = std::env::temp_dir().join(format!("exm-meta-gw-{}", crate::types::new_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let store = Arc::new(Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(LocalRegistry::new(dir.join("agents")).unwrap());
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        ToolGateway::new(&dir, store, registry, events, SecurityConfig::default())
    }

    /// 提议校验:kind 白名单 / rationale 长度 / change 结构
    #[test]
    fn 提议校验_拒绝坏载荷() {
        let gw = tool_gw();
        let mk = |kind: &str, change: serde_json::Value, rationale: &str| {
            serde_json::json!({ "kind": kind, "target": "worker", "change": change, "rationale": rationale })
        };
        // kind 非法
        assert!(validate_proposal(&gw, &mk("evil", serde_json::json!({}), "理由足够长的提议")).is_err());
        // rationale 过短
        assert!(validate_proposal(&gw, &mk("prompt", serde_json::json!({ "promptPatch": "一段足够长的提示词补段内容" }), "短")).is_err());
        // promptPatch 过短
        assert!(validate_proposal(&gw, &mk("prompt", serde_json::json!({ "promptPatch": "短" }), "依据数据做出的充分理由")).is_err());
        // ops 非数组 / 超量
        assert!(validate_proposal(&gw, &mk("roster", serde_json::json!({ "ops": "x" }), "依据数据做出的充分理由")).is_err());
        let six: Vec<serde_json::Value> = (0..6)
            .map(|i| serde_json::json!({ "op": "add", "identifier": format!("a{i}"), "name": format!("a{i}") }))
            .collect();
        assert!(validate_proposal(&gw, &mk("roster", serde_json::json!({ "ops": six }), "依据数据做出的充分理由")).is_err());
        // 合法载荷通过
        let ok = mk("prompt", serde_json::json!({ "promptPatch": "编码任务先读相邻文件再动手" }), "edit 失败集中于未读上下文");
        assert!(validate_proposal(&gw, &ok).is_ok());
    }

    /// roster 目标存在性:不在组内的个体被拒
    #[test]
    fn 提议校验_roster目标须存在() {
        let gw = tool_gw();
        let args = serde_json::json!({
            "kind": "roster", "target": "nonexistent-agent",
            "change": { "ops": [{ "op": "update", "identifier": "nonexistent-agent", "name": "x", "description": "y" }] },
            "rationale": "目标不存在应被拒绝"
        });
        assert!(validate_proposal(&gw, &args).is_err());
    }

    /// meta_observe:过滤不存在的个体报错;全体返回快照
    /// meta_propose 经工具入口:合法载荷落 [meta] 审批单;同签名未决单去重
    #[tokio::test]
    async fn 提议工具_落单与去重() {
        let gw = tool_gw();
        let args = serde_json::json!({
            "kind": "prompt", "target": "worker",
            "change": { "promptPatch": "编码任务先读相邻文件再动手" },
            "rationale": "edit 失败集中于未读上下文"
        });
        let r1 = gw
            .tool_meta("meta", "s1", ToolName::MetaPropose, &args)
            .await;
        assert!(r1.ok, "首次提议应落单: {:?}", r1.error);
        // 落单后审批库里应有 [meta] 前缀未决单
        let pending = gw.store.list_approvals(Some("pending"), 50).unwrap();
        assert!(
            pending.iter().any(|p| p.command.starts_with("[meta]")),
            "审批库应有 [meta] 未决单: {:?}",
            pending.iter().map(|p| p.command.clone()).collect::<Vec<_>>()
        );
        // 同 target+kind 再提 → 去重拒绝
        let r2 = gw
            .tool_meta("meta", "s1", ToolName::MetaPropose, &args)
            .await;
        assert!(!r2.ok, "同签名未决单应被去重拒绝");
        assert!(r2.error.as_deref().unwrap_or("").contains("未决提议"));
    }

    #[test]
    fn 观测_过滤与不存在个体() {
        let gw = tool_gw();
        let res = gw.meta_observe(&serde_json::json!({ "agent": "nonexistent-agent" }));
        assert!(!res.ok, "不存在的个体应报错");
        let all = gw.meta_observe(&serde_json::json!({}));
        assert!(all.ok, "全体观测应成功");
    }

    /// 实验可观测（assistant-parity 7.1）：提议落单同时产生 meta.proposal
    /// 事件（phase=created，含 kind/target 明细）——事件页可检索提议轨迹
    #[tokio::test]
    async fn 提议事件_meta_proposal落库() {
        let gw = tool_gw();
        let args = serde_json::json!({
            "kind": "prompt", "target": "worker",
            "change": { "promptPatch": "测试补段：编码任务先读相邻文件再动手" },
            "rationale": "事件断言用"
        });
        let r = gw
            .tool_meta("meta", "s-ev", ToolName::MetaPropose, &args)
            .await;
        assert!(r.ok, "提议应落单: {:?}", r.error);
        let events = gw.store.list_events("s-ev").unwrap_or_default();
        let hit = events
            .iter()
            .any(|env| env["payload"]["type"] == "meta.proposal"
                && env["payload"]["payload"]["phase"] == "created"
                && env["payload"]["payload"]["kind"] == "prompt"
                && env["payload"]["payload"]["target"] == "worker");
        assert!(hit, "meta.proposal(created) 事件应落库: {events:?}");
    }
}
