//! tools/schedule.rs —— 自 tools.rs 按领域拆出（纯搬移，零行为变更）

use super::*;

impl ToolGateway {
    /// 自主排程：创建/查看/删除定时与一次性任务（AI 自驱工作的时间维度）
    pub(crate) fn tool_schedule(&self, args: &serde_json::Value) -> ToolResult {
        let Some(cron) = &self.cron else {
            return ToolResult::err("排程存储未接入（该运行环境不支持 schedule）");
        };
        let op = args.get("op").and_then(|v| v.as_str()).unwrap_or("list");
        match op {
            "create" => {
                let name = args
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let prompt = args
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let expr = args
                    .get("cron")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                let at = args
                    .get("at")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                if name.is_empty() || prompt.is_empty() {
                    return ToolResult::err("create 需要 name 与 prompt");
                }
                if expr.is_none() && at.is_none() {
                    return ToolResult::err("create 需要 cron（周期）或 at（一次性时间）之一");
                }
                if let Some(e) = &expr {
                    if e.split_whitespace().count() != 5 {
                        return ToolResult::err("cron 需为五段：分 时 日 月 周，如 0 9 * * *");
                    }
                }
                let job = crate::types::CronJob {
                    id: crate::types::new_id()[..8].to_string(),
                    name,
                    prompt,
                    cron: expr,
                    at,
                    kind: None,
                    target_session: None,
                    steps: Vec::new(),
                    group: Some(self.registry.active_group()),
                    session_title: None,
                    enabled: true,
                    last_run_at: None,
                    last_status: None,
                    last_run_minute: None,
                    notify_channels: Vec::new(),
                    created_at: crate::types::now_iso(),
                };
                match cron.upsert(job) {
                    Ok(j) => ToolResult::ok(format!(
                        "已排程：id={} 名称={} 触发={}（激活组 {}；到期由调度器唤醒本组执行）",
                        j.id,
                        j.name,
                        j.cron.clone().or(j.at.clone()).unwrap_or_default(),
                        self.registry.active_group()
                    )),
                    Err(e) => ToolResult::err(format!("排程失败: {e}")),
                }
            }
            "list" => match cron.list() {
                Ok(jobs) => {
                    let filter = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let rows: Vec<String> = jobs
                        .iter()
                        .filter(|j| filter.is_empty() || j.id == filter)
                        .map(|j| {
                            format!(
                                "- {} [{}] {} 触发={} 启用={}{}",
                                j.name,
                                j.id,
                                if j.prompt.chars().count() > 40 {
                                    format!("{}…", j.prompt.chars().take(40).collect::<String>())
                                } else {
                                    j.prompt.clone()
                                },
                                j.cron.clone().or(j.at.clone()).unwrap_or_default(),
                                if j.enabled { "是" } else { "否" },
                                j.last_status
                                    .as_ref()
                                    .map(|s| format!(" 上次={s}"))
                                    .unwrap_or_default()
                            )
                        })
                        .collect();
                    if rows.is_empty() {
                        ToolResult::ok("（无排程任务）")
                    } else {
                        ToolResult::ok(rows.join("\n"))
                    }
                }
                Err(e) => ToolResult::err(format!("读取排程失败: {e}")),
            },
            "remove" | "enable" | "disable" => {
                let id = args
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if id.is_empty() {
                    return ToolResult::err(format!("{op} 需要 id"));
                }
                if op == "remove" {
                    return match cron.remove(&id) {
                        Ok(true) => ToolResult::ok(format!("已删除排程：{id}")),
                        Ok(false) => ToolResult::err(format!("排程不存在：{id}")),
                        Err(e) => ToolResult::err(format!("删除失败: {e}")),
                    };
                }
                match cron.get(&id) {
                    Ok(Some(mut job)) => {
                        job.enabled = op == "enable";
                        match cron.upsert(job) {
                            Ok(_) => ToolResult::ok(format!(
                                "已{}排程：{id}",
                                if op == "enable" { "启用" } else { "停用" }
                            )),
                            Err(e) => ToolResult::err(format!("更新失败: {e}")),
                        }
                    }
                    Ok(None) => ToolResult::err(format!("排程不存在：{id}")),
                    Err(e) => ToolResult::err(format!("读取失败: {e}")),
                }
            }
            other => ToolResult::err(format!(
                "未知 schedule 操作: {other}（支持 create/list/remove/enable/disable）"
            )),
        }
    }
}
