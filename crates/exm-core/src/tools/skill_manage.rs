//! tools/skill_manage.rs —— 技能包自管理（SKILL.md 标准格式）
//!
//! 智能体在完成任务后把可复用流程沉淀为技能；后续派发命中触发词时自动注入。
//! 这是「自改进闭环」的写入口：create/update 由模型自主调用，统计由注册表侧车聚合。
//! 工具面默认只给组主智能体与独立指挥体（runtime_allowlist），子个体只读注入。

use super::*;
use crate::types::SkillDef;

impl ToolGateway {
    pub(crate) fn tool_skill_manage(&self, args: &serde_json::Value) -> ToolResult {
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
                let id = args
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
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
                let triggers: Vec<String> = args
                    .get("triggers")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str())
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                let agents: Vec<String> = args
                    .get("agents")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str())
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                let description = args
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                // update 保留既有创建时间与来源（技能是被迭代的资产，不是每次重写的新文件）
                let existing = self
                    .registry
                    .load_skills()
                    .ok()
                    .and_then(|l| l.into_iter().find(|s| s.id == id));
                let skill = SkillDef {
                    id: id.to_string(),
                    name: name.to_string(),
                    description,
                    triggers,
                    instructions: instructions.to_string(),
                    agents,
                    created_at: existing
                        .as_ref()
                        .map(|s| s.created_at.clone())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(crate::types::now_iso),
                    source: existing
                        .as_ref()
                        .map(|s| s.source.clone())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "agent".into()),
                    ..Default::default()
                };
                match self.registry.write_skill(&skill) {
                    Ok(_) => ToolResult::ok(
                        serde_json::json!({
                            "ok": true,
                            "id": skill.id,
                            "name": skill.name,
                            "triggers": skill.triggers,
                            "source": skill.source,
                            "note": "技能已写入全局技能目录（热装载，后续派发命中触发词即注入）",
                        })
                        .to_string(),
                    ),
                    Err(e) => ToolResult::err(format!("技能写入失败: {e}")),
                }
            }
            "delete" => {
                let id = args
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if id.is_empty() {
                    return ToolResult::err("delete 需要 id");
                }
                match self.registry.remove_skill(id) {
                    Ok(true) => ToolResult::ok(
                        serde_json::json!({ "ok": true, "id": id, "removed": true }).to_string(),
                    ),
                    Ok(false) => ToolResult::ok(
                        serde_json::json!({ "ok": true, "id": id, "removed": false }).to_string(),
                    ),
                    Err(e) => ToolResult::err(format!("技能删除失败: {e}")),
                }
            }
            "export" => {
                let id = args
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
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
                match self.registry.import_skill_md(content) {
                    Ok(s) => ToolResult::ok(
                        serde_json::json!({
                            "ok": true,
                            "id": s.id,
                            "name": s.name,
                            "source": s.source,
                        })
                        .to_string(),
                    ),
                    Err(e) => ToolResult::err(format!("技能导入失败: {e}")),
                }
            }
            other => ToolResult::err(format!(
                "未知 op: {other}（支持 list / create / update / delete / export / import）"
            )),
        }
    }
}
