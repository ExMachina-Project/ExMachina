//! runtime 域分文件（自 runtime.rs 按域拆出，纯搬移零行为变更）。

use super::*;

impl AgentRuntime {
    /// 同轮工具调用：只读工具并发（join_all），写工具与 MCP 按原顺序串行。
    /// 返回与 `calls` 同序的 `ToolFeedback`——保证与 tool_call_id 配对。
    /// `trace` = 工具记录与工具事件的归集键（派发运行指向子会话）。
    pub(crate) async fn run_tool_calls(
        &self,
        agent_id: &str,
        trace: &str,
        allowlist: &[ToolName],
        calls: &[ToolCall],
        attach_images: bool,
        scope: ToolScope,
    ) -> Vec<ToolFeedback> {
        // 仅聊天档：即使模型绕过 schema 硬造 tool call，也在执行层全部拒绝。
        if !scope.allows_tools() {
            return calls
                .iter()
                .map(|c| {
                    (
                        format!(
                            "【警告】当前通道能力边界为「仅聊天」，工具 {} 被拒绝执行。",
                            c.name
                        ),
                        None,
                    )
                })
                .collect();
        }
        let mut out: Vec<Option<ToolFeedback>> = vec![None; calls.len()];
        let mut readonly: Vec<usize> = Vec::new();
        let mut serial: Vec<usize> = Vec::new();
        for (i, c) in calls.iter().enumerate() {
            let ro = !c.name.starts_with("mcp:")
                && ToolName::parse(&c.name)
                    .map(|t| t.is_readonly())
                    .unwrap_or(false);
            if ro {
                readonly.push(i);
            } else {
                serial.push(i);
            }
        }
        // 只读并发
        if !readonly.is_empty() {
            let futs = readonly.iter().map(|i| {
                let c = &calls[*i];
                async move {
                    let tool = ToolName::parse(&c.name).unwrap_or(ToolName::Read);
                    let r = self
                        .tools
                        .execute(agent_id, trace, allowlist, tool, &c.arguments)
                        .await;
                    (tool_feedback(&c.name, &r), None)
                }
            });
            let results = futures_util::future::join_all(futs).await;
            for (i, pair) in readonly.iter().zip(results) {
                out[*i] = Some(pair);
            }
        }
        // 写 / MCP / 自定义 / 未知：串行
        for i in serial {
            let c = &calls[i];
            let (r, name) = if ToolName::parse(&c.name) == Some(ToolName::Computer)
                && !scope.allows_computer()
            {
                // 工具档硬闸门：即使 allowlist 意外混入 computer，也不执行桌面操作
                (
                    crate::tools::ToolResult {
                        ok: false,
                        output: String::new(),
                        error: Some("当前通道未授权电脑操控（computer）".into()),
                        images: Vec::new(),
                    },
                    c.name.clone(),
                )
            } else if c.name.starts_with("mcp:") {
                (
                    self.tools
                        .execute_mcp(agent_id, trace, &c.name, &c.arguments)
                        .await,
                    c.name.clone(),
                )
            } else {
                // 统一入口：内置按白名单放行，声明式自定义按 agents 可见性放行
                (
                    self.tools
                        .execute_named(agent_id, trace, allowlist, &c.name, &c.arguments)
                        .await,
                    c.name.clone(),
                )
            };
            let images = if attach_images {
                self.image_note(&name, &r.images)
            } else {
                None
            };
            out[i] = Some((tool_feedback(&name, &r), images));
        }
        out.into_iter().map(|x| x.unwrap_or_default()).collect()
    }

}
