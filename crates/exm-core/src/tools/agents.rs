//! tools/agents.rs —— 自 tools.rs 按领域拆出（纯搬移，零行为变更）

use super::*;

impl ToolGateway {
    /// 智能体管理（docs/09）：仅激活组的主智能体可用；内置组定义受保护。
    /// 审计由 execute() 统一落库。
    pub(crate) fn tool_agent_manage(&self, args: &serde_json::Value) -> ToolResult {
        // 权限闸门：调用者必须是激活组的主智能体，且当前组非内置
        let Some(meta) = self.registry.active_group_meta() else {
            return ToolResult::err("无激活组");
        };
        if meta.builtin {
            return ToolResult::err("内置组的定义受保护：不可增删改个体");
        }
        // 调用者身份由网关在 args 上层校验（execute 时传入 agent_id），
        // 这里校验 op 与内容；调用者==主智能体的强校验在 execute() 的 allowlist 注入处保证。

        let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("");
        match self.agent_manage_op(&meta, op, args) {
            Ok(v) => ToolResult::ok(serde_json::to_string_pretty(&v).unwrap_or_default()),
            Err(e) => ToolResult::err(e.to_string()),
        }
    }

    pub(crate) fn agent_manage_op(
        &self,
        meta: &crate::types::GroupMeta,
        op: &str,
        args: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let s = |k: &str| {
            args.get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        match op {
            "list" => {
                let defs = self.registry.list();
                Ok(serde_json::json!({
                    "group": meta,
                    "agents": defs.iter().map(|d| serde_json::json!({
                        "name": d.name, "identifier": d.identifier,
                        "domain": d.domain, "tier": d.tier, "description": d.description,
                    })).collect::<Vec<_>>()
                }))
            }
            "group_info" => Ok(serde_json::json!({
                "group": meta,
                "count": self.registry.count(),
                "isBuiltin": self.registry.is_builtin(&meta.id),
            })),
            "templates" => {
                let all = self.registry.list_templates();
                Ok(serde_json::json!({
                    "templates": all.iter().map(|t| serde_json::json!({
                        "id": t.id, "name": t.name, "description": t.description, "kind": t.kind,
                    })).collect::<Vec<_>>(),
                    "usage": "create 时传 template=<unit 模板 id>,仅需 identifier;name/description 缺省用模板默认"
                }))
            }
            "create" | "update" => {
                let identifier = s("identifier");
                let name = s("name");
                let description = s("description");
                let template = s("template");
                // 模板创建:仅 identifier 必填,工具面/能力/提示词由模板提供
                if op == "create" && !template.is_empty() {
                    let def = self.registry.create_agent_from_template(
                        &meta.id,
                        &template,
                        &identifier,
                        &name,
                        &description,
                    )?;
                    return Ok(serde_json::json!({ "created": def, "fromTemplate": template }));
                }
                if (op == "create"
                    && (identifier.is_empty() || name.is_empty() || description.is_empty()))
                    || (op == "update" && identifier.is_empty())
                {
                    bail!("create 需要 identifier/name/description（或提供 template）；update 需要 identifier");
                }
                // update：基于现有定义合并
                let mut def = match (op, self.registry.get(&identifier)) {
                    ("update", Some(existing)) => existing,
                    _ => AgentDefinition {
                        name: name.clone(),
                        identifier: identifier.clone(),
                        domain: if s("domain").is_empty() {
                            "自定义".into()
                        } else {
                            s("domain")
                        },
                        tier: Tier::Unit,
                        description: description.clone(),
                        capabilities: args
                            .get("capabilities")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        tools: vec![],
                        when_to_call: s("whenToCall"),
                        dependencies: vec![],
                        composable_with: vec![],
                        input_schema: Default::default(),
                        output_schema: Default::default(),
                        prompt_file: String::new(),
                        model_hint: None,
                        link: None,
                        max_steps: None,
                    },
                };
                if !name.is_empty() {
                    def.name = name;
                }
                if !description.is_empty() {
                    def.description = description;
                }
                if !s("domain").is_empty() {
                    def.domain = normalize_domain(&s("domain"));
                }
                if let Some(t) = args.get("tier").and_then(|v| v.as_str()) {
                    if t == "orchestrator" {
                        def.tier = Tier::Orchestrator;
                    }
                }
                if let Some(t) = args.get("prompt").and_then(|v| v.as_str()) {
                    def.prompt_file = format!("{}.md", def.identifier);
                    self.registry.write_prompt(&def.prompt_file, t)?;
                }
                if let Some(m) = args.get("maxSteps").and_then(|v| v.as_u64()) {
                    def.max_steps = Some(m as usize);
                }
                let saved = self.registry.upsert_agent(&meta.id, def, None)?;
                // 主智能体由系统指定：仅组的首个个体自动成为主智能体，不支持手动换主
                if meta.primary.is_none() {
                    self.registry.set_primary(&meta.id, &saved.identifier)?;
                }
                Ok(serde_json::to_value(&saved)?)
            }
            "delete" => {
                let identifier = s("identifier");
                self.registry.remove_agent(&meta.id, &identifier)?;
                Ok(serde_json::json!({ "deleted": identifier }))
            }
            other => bail!("未知 agent_manage 操作: {other}"),
        }
    }
}
