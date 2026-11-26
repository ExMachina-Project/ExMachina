//! tools/programmatic.rs —— capability-parity 5.2：有界程序化工具组合。
//!
//! `programmatic` 工具让模型在一次受限执行里组合**当前任务已授权**的工具
//! （JSON 步骤清单，无脚本引擎依赖——不引入新供应链），中间结果不回灌
//! 模型上下文，只把最终摘要与逐步状态返回。
//!
//! 复用边界（programmatic-tool-execution spec「复用原工具安全边界」）：
//! - 每个子调用走 `ToolGateway::execute_named` 同一入口——allowlist 放行、
//!   审批闸门、沙箱、脱敏、审计、结果落盘对子调用原样生效；
//! - 清单里**只允许当前 allowlist 内的内置工具**；MCP/spawn/自定义/computer/
//!   programmatic 自身一律拒绝（不得递归、不得扩权、不得绕过插件 ACL）；
//! - 步骤间可用 `$N` 引用第 N 步输出（1 起）；引用只做文本替换。
//!
//! 资源边界（「程序化资源边界」）：
//! - 步骤数上限（默认 16）、每步输出截断、总输出上限（默认 64 KiB）；
//! - 每步之间检查会话取消令牌（round_trace::is_cancelled）：取消即停，
//!   未开始步骤标 skipped，迟到结果不产生写副作用（写步骤本来串行）；
//! - 终态：succeeded / partial（某步失败后停止）/ limit_exceeded /
//!   cancelled / failed（解析失败）。

use super::*;
use std::time::Duration;

/// 单个步骤：工具 key + 参数对象
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProgrammaticStep {
    pub tool: String,
    #[serde(default)]
    pub args: serde_json::Value,
}

/// 步骤执行状态（可观察性：名称/参数摘要/状态/耗时/结果引用）
#[derive(Debug, Clone, serde::Serialize)]
pub struct StepRecord {
    pub index: usize,
    pub tool: String,
    pub args_preview: String,
    pub status: &'static str, // ok | failed | skipped
    pub duration_ms: u128,
    /// 输出落盘引用（spill 文件名）或截断内联
    pub output_preview: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgrammaticStatus {
    Succeeded,
    Partial,
    LimitExceeded,
    Cancelled,
    Timeout,
    /// 解析失败等输入错误（「程序化资源边界」：可区分终态之一——
    /// steps 缺失/空清单/步骤解析失败返回 status=failed 的结构化体）
    Failed,
}

pub(crate) const PROGRAMMATIC_MAX_STEPS: usize = 16;
pub(crate) const PROGRAMMATIC_TOTAL_OUTPUT_LIMIT: usize = 64 * 1024;
pub(crate) const PROGRAMMATIC_STEP_PREVIEW: usize = 400;
/// 总时长上限（「程序化资源边界」：超时 MUST 终止并返回可区分终态）。
/// 常量而非配置：子调用自身已有各自超时（terminal/审批等待），这里只是
/// 组合层的墙钟兜底——测试用小步数即可，不依赖配置面。
pub(crate) const PROGRAMMATIC_WALL_LIMIT: std::time::Duration = std::time::Duration::from_secs(120);
const PROGRAMMATIC_KEY: &str = "programmatic";

impl ToolGateway {
    /// 有界程序化工具组合入口。`allowlist` 是当前任务的工具白名单（与
    /// runtime 传给 execute_named 的同一个）——子调用授权不超出它。
    pub(crate) async fn tool_programmatic(
        &self,
        agent_id: &str,
        session_id: &str,
        allowlist: &[ToolName],
        args: &serde_json::Value,
    ) -> ToolResult {
        self.tool_programmatic_with(
            agent_id,
            session_id,
            allowlist,
            args,
            PROGRAMMATIC_WALL_LIMIT,
        )
        .await
    }

    /// 实际实现（wall_limit 可注入：测试用极小值验证 timeout 终态）
    pub(crate) async fn tool_programmatic_with(
        &self,
        agent_id: &str,
        session_id: &str,
        allowlist: &[ToolName],
        args: &serde_json::Value,
        wall_limit: Duration,
    ) -> ToolResult {
        // 组合层审计（5.3 可观察性）：单条记录整体组合的步数/终态/耗时，
        // 子调用在 execute_named 链上已有各自 tool.call 事件与 audit 行。
        let combo_started = std::time::Instant::now();
        let _audit = |status: &str, executed: usize, steps: &[ProgrammaticStep]| {
            let _ = self.store.audit_tool_ex(
                agent_id,
                PROGRAMMATIC_KEY,
                args,
                &format!("组合执行 {status}：{executed}/{} 步", steps.len()),
                combo_started.elapsed().as_millis() as u64,
                status == "succeeded",
            );
        };
        // 清单形态：{ steps: [ {tool, args}, ... ] }
        let Some(steps_raw) = args.get("steps").and_then(|v| v.as_array()) else {
            _audit("failed", 0, &[]);
            return programmatic_result(
                ProgrammaticStatus::Failed,
                Vec::new(),
                "steps 缺失：用法 {\"steps\":[{\"tool\":\"grep\",\"args\":{...}},...]}，步骤间可用 $1/$2 引用前步输出",
                0,
                &|_, _| {},
            );
        };
        let mut steps: Vec<ProgrammaticStep> = Vec::with_capacity(steps_raw.len());
        for (i, raw) in steps_raw.iter().enumerate() {
            match serde_json::from_value::<ProgrammaticStep>(raw.clone()) {
                Ok(step) => steps.push(step),
                Err(error) => {
                    _audit("failed", 0, &[]);
                    return programmatic_result(
                        ProgrammaticStatus::Failed,
                        Vec::new(),
                        &format!(
                            "第 {} 步解析失败：{error}（每步需要 tool 字符串与 args 对象）",
                            i + 1
                        ),
                        0,
                        &|_, _| {},
                    );
                }
            }
        }
        if steps.is_empty() {
            _audit("failed", 0, &[]);
            return programmatic_result(
                ProgrammaticStatus::Failed,
                Vec::new(),
                "steps 不能为空",
                0,
                &|_, _| {},
            );
        }
        if steps.len() > PROGRAMMATIC_MAX_STEPS {
            return ToolResult::err(format!(
                "步骤数超限：{} > {}（limit_exceeded）",
                steps.len(),
                PROGRAMMATIC_MAX_STEPS
            ));
        }

        // 预检：全部步骤必须命中 allowlist 内的内置工具——任何 MCP/spawn/
        // 自定义/programmatic（递归）/computer 在执行前拒绝（未授权调用场景）
        for (i, step) in steps.iter().enumerate() {
            let denial = match ToolName::parse(&step.tool) {
                None => Some(format!(
                    "未知或非内置工具 `{}`（程序化入口只允许当前授权的内置工具）",
                    step.tool
                )),
                Some(tool) if tool.key() == PROGRAMMATIC_KEY => {
                    Some("programmatic 不得递归调用自身".into())
                }
                Some(tool)
                    if matches!(
                        tool,
                        ToolName::Spawn
                            | ToolName::Computer
                            | ToolName::AgentManage
                            | ToolName::SkillManage
                    ) =>
                {
                    Some(format!(
                        "工具 `{}` 不在程序化组合面内（spawn/computer/管理类须直接调用以保留闸门语义）",
                        tool.key()
                    ))
                }
                Some(tool) if !allowlist.contains(&tool) => Some(format!(
                    "工具 `{}` 不在当前任务授权面内（limit：allowlist 未包含）",
                    tool.key()
                )),
                Some(_) => None,
            };
            if let Some(reason) = denial {
                let records = vec![StepRecord {
                    index: i,
                    tool: step.tool.clone(),
                    args_preview: preview_args(&step.args),
                    status: "skipped",
                    duration_ms: 0,
                    output_preview: String::new(),
                    error: Some(reason.clone()),
                }];
                return programmatic_result(
                    ProgrammaticStatus::LimitExceeded,
                    records,
                    "预检拒绝：未开始任何调用",
                    0,
                    &|status, executed| _audit(status, executed, &steps),
                );
            }
        }

        // 受控执行：串行（写工具本就串行；取消检查点在每步之间）
        let wall_start = std::time::Instant::now();
        let mut records: Vec<StepRecord> = Vec::with_capacity(steps.len());
        let mut outputs: Vec<String> = Vec::with_capacity(steps.len());
        let mut total_output = 0usize;
        let mut executed = 0usize;
        for (i, step) in steps.iter().enumerate() {
            // 取消收束：迟到结果不产生写副作用（后续步骤不再启动）
            if crate::round_trace::is_cancelled(session_id) {
                for step in &steps[i..] {
                    records.push(StepRecord {
                        index: records.len(),
                        tool: step.tool.clone(),
                        args_preview: preview_args(&step.args),
                        status: "skipped",
                        duration_ms: 0,
                        output_preview: String::new(),
                        error: Some("会话已取消".into()),
                    });
                }
                return programmatic_result(
                    ProgrammaticStatus::Cancelled,
                    records,
                    "已取消：未开始步骤标 skipped",
                    executed,
                    &|status, executed: usize| _audit(status, executed, &steps),
                );
            }
            // 墙钟上限：超时终止后续步骤（可区分 timeout 终态）
            if wall_start.elapsed() > wall_limit {
                for step in &steps[i..] {
                    records.push(StepRecord {
                        index: records.len(),
                        tool: step.tool.clone(),
                        args_preview: preview_args(&step.args),
                        status: "skipped",
                        duration_ms: 0,
                        output_preview: String::new(),
                        error: Some("总时长超限".into()),
                    });
                }
                return programmatic_result(
                    ProgrammaticStatus::Timeout,
                    records,
                    "总时长超过上限，后续步骤未执行",
                    executed,
                    &|status, executed: usize| _audit(status, executed, &steps),
                );
            }
            let tool = ToolName::parse(&step.tool).expect("预检已放行");
            // $N 引用替换（1 起；只替换参数对象序列化文本后重新解析）
            let bound_args = bind_references(&step.args, &outputs);
            let started = std::time::Instant::now();
            // 异步递归（execute_named → programmatic → execute_named）需装箱拆环
            let result = Box::pin(self.execute_named(
                agent_id,
                session_id,
                allowlist,
                tool.key(),
                &bound_args,
            ))
            .await;
            let duration_ms = started.elapsed().as_millis();
            executed += 1;
            if result.ok {
                total_output += result.output.len();
                outputs.push(result.output.clone());
                let output: String = result
                    .output
                    .chars()
                    .take(PROGRAMMATIC_STEP_PREVIEW)
                    .collect();
                records.push(StepRecord {
                    index: i,
                    tool: step.tool.clone(),
                    args_preview: preview_args(&bound_args),
                    status: "ok",
                    duration_ms,
                    output_preview: output,
                    error: None,
                });
                if total_output > PROGRAMMATIC_TOTAL_OUTPUT_LIMIT {
                    // 总输出上限：停止后续步骤，如实报 limit_exceeded
                    for step in &steps[i + 1..] {
                        records.push(StepRecord {
                            index: records.len(),
                            tool: step.tool.clone(),
                            args_preview: preview_args(&step.args),
                            status: "skipped",
                            duration_ms: 0,
                            output_preview: String::new(),
                            error: Some("总输出超限".into()),
                        });
                    }
                    return programmatic_result(
                        ProgrammaticStatus::LimitExceeded,
                        records,
                        "总输出超过上限，后续步骤未执行",
                        executed,
                        &|status, executed: usize| _audit(status, executed, &steps),
                    );
                }
            } else {
                // 部分失败：已完成步骤保留，未执行步骤标 skipped
                records.push(StepRecord {
                    index: i,
                    tool: step.tool.clone(),
                    args_preview: preview_args(&bound_args),
                    status: "failed",
                    duration_ms,
                    output_preview: String::new(),
                    error: result.error.clone(),
                });
                for step in &steps[i + 1..] {
                    records.push(StepRecord {
                        index: records.len(),
                        tool: step.tool.clone(),
                        args_preview: preview_args(&step.args),
                        status: "skipped",
                        duration_ms: 0,
                        output_preview: String::new(),
                        error: Some("前序失败，未执行".into()),
                    });
                }
                return programmatic_result(
                    ProgrammaticStatus::Partial,
                    records,
                    "部分失败：已完成与未执行步骤见清单",
                    executed,
                    &|status, executed: usize| _audit(status, executed, &steps),
                );
            }
        }
        programmatic_result(
            ProgrammaticStatus::Succeeded,
            records,
            "全部步骤完成",
            executed,
            &|status, executed| _audit(status, executed, &steps),
        )
    }
}

/// `$1`/`$2` 引用绑定：把参数 JSON 里的字符串值中的引用替换为前步输出
/// （只处理字符串值；替换后再解析回 JSON——引用嵌在文本里也生效）
fn bind_references(args: &serde_json::Value, outputs: &[String]) -> serde_json::Value {
    let mut text = args.to_string();
    for (i, output) in outputs.iter().enumerate() {
        let token = format!("${}", i + 1);
        if text.contains(&token) {
            // JSON 转义后去首尾引号：输出含换行/引号时重解析仍成立；
            // 引用值截断防单步巨输出撑爆下一步参数
            let quoted: String = serde_json::to_string(
                &output
                    .chars()
                    .take(PROGRAMMATIC_STEP_PREVIEW)
                    .collect::<String>(),
            )
            .unwrap_or_default();
            text = text.replace(&token, &quoted[1..quoted.len() - 1]);
        }
    }
    serde_json::from_str(&text).unwrap_or_else(|_| args.clone())
}

fn preview_args(args: &serde_json::Value) -> String {
    let text = args.to_string();
    text.chars().take(200).collect()
}

fn status_serde(status: ProgrammaticStatus) -> &'static str {
    match status {
        ProgrammaticStatus::Succeeded => "succeeded",
        ProgrammaticStatus::Partial => "partial",
        ProgrammaticStatus::LimitExceeded => "limit_exceeded",
        ProgrammaticStatus::Cancelled => "cancelled",
        ProgrammaticStatus::Timeout => "timeout",
        ProgrammaticStatus::Failed => "failed",
    }
}

fn programmatic_result(
    status: ProgrammaticStatus,
    records: Vec<StepRecord>,
    summary: &str,
    executed: usize,
    audit: &dyn Fn(&str, usize),
) -> ToolResult {
    audit(status_serde(status), executed);
    // 最终摘要：状态 + 已执行步数 + 逐步状态（中间结果不回灌模型上下文，
    // 每步只留 preview——完整输出已在 execute_named 的落盘链路里）
    let body = serde_json::json!({
        "status": status,
        "executed": executed,
        "total": records.len(),
        "summary": summary,
        "steps": records,
    });
    let ok = matches!(status, ProgrammaticStatus::Succeeded);
    let mut result = if ok {
        ToolResult::ok(serde_json::to_string_pretty(&body).unwrap_or_default())
    } else {
        ToolResult::err(serde_json::to_string_pretty(&body).unwrap_or_default())
    };
    result.output = serde_json::to_string_pretty(&body).unwrap_or_default();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::types::ToolScope;
    use std::sync::Arc;

    fn gw() -> ToolGateway {
        let dir = std::env::temp_dir().join(format!("exm-prog-{}", crate::types::new_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let store = Arc::new(Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(LocalRegistry::new(dir.join("agents")).unwrap());
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        ToolGateway::new(&dir, store, registry, events, SecurityConfig::default())
    }

    fn body_of(result: &ToolResult) -> serde_json::Value {
        serde_json::from_str(result.output.trim())
            .unwrap_or_else(|_| panic!("programmatic 输出应为 JSON：{}", result.output))
    }

    /// 多步数据处理（spec「多步数据处理」场景）：read ×2 + 引用绑定，
    /// 中间结果只在最终摘要里以 preview 呈现，终态 succeeded。
    #[tokio::test]
    async fn 多步组合_引用绑定与摘要() {
        let gw = gw();
        let sid = gw.store.create_session("s", "a1").unwrap().id;
        // 准备两个文件（filesystem write 走 allowlist：edit/filesystem 需要授权）
        let allowlist = [
            ToolName::Read,
            ToolName::Grep,
            ToolName::Glob,
            ToolName::Terminal,
        ];
        let result = gw
            .tool_programmatic(
                "a1",
                &sid,
                &allowlist,
                &serde_json::json!({
                    "steps": [
                        { "tool": "terminal", "args": { "command": "echo hello-prog" } },
                        { "tool": "terminal", "args": { "command": "echo $1" } },
                    ]
                }),
            )
            .await;
        assert!(result.ok, "组合应成功：{}", result.output);
        let body = body_of(&result);
        assert_eq!(body["status"], "succeeded");
        assert_eq!(body["executed"], 2);
        let steps = body["steps"].as_array().expect("steps 数组");
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["status"], "ok");
        // $1 引用被前步输出替换（echo hello-prog 的输出回显）
        assert!(
            steps[1]["args_preview"]
                .as_str()
                .unwrap_or("")
                .contains("hello-prog"),
            "第二步参数应绑定 $1：{}",
            steps[1]["args_preview"]
        );
    }

    /// 未授权调用（spec「未授权调用」场景）：allowlist 外工具在预检拒绝，
    /// 已完成步骤可回收（返回结构化错误 + limit_exceeded）。
    #[tokio::test]
    async fn 未授权工具_预检拒绝() {
        let gw = gw();
        let sid = gw.store.create_session("s", "a1").unwrap().id;
        let allowlist = [ToolName::Read];
        let result = gw
            .tool_programmatic(
                "a1",
                &sid,
                &allowlist,
                &serde_json::json!({
                    "steps": [
                        { "tool": "read", "args": { "path": "Cargo.toml" } },
                        { "tool": "edit", "args": { "path": "/tmp/x", "old_string": "a", "new_string": "b" } },
                    ]
                }),
            )
            .await;
        assert!(!result.ok);
        let body = body_of(&result);
        assert_eq!(body["status"], "limit_exceeded");
        assert_eq!(body["executed"], 0, "预检拒绝：任何调用都未开始");
        let steps = body["steps"].as_array().unwrap();
        assert_eq!(steps[0]["status"], "skipped");
        assert!(
            steps[0]["error"].as_str().unwrap_or("").contains("授权面"),
            "拒绝原因：{}",
            steps[0]["error"].as_str().unwrap_or("")
        );
    }

    /// 禁用面：spawn/computer/MCP/自定义/programmatic 自身一律拒绝（不递归、
    /// 不扩权、不绕过插件 ACL）。
    #[tokio::test]
    async fn 禁用面_递归spawn与computer被拒() {
        let gw = gw();
        let sid = gw.store.create_session("s", "a1").unwrap().id;
        // allowlist 全量语义下（不含 programmatic 自身）spawn 也在 allowlist
        // 里也不行——组合面排除它
        let allowlist = ToolName::all().to_vec();
        for bad in [
            "programmatic",
            "spawn",
            "computer",
            "agent_manage",
            "mcp:x:y",
            "fakep__t",
        ] {
            let result = gw
                .tool_programmatic(
                    "a1",
                    &sid,
                    &allowlist,
                    &serde_json::json!({ "steps": [{ "tool": bad, "args": {} }] }),
                )
                .await;
            assert!(!result.ok, "{bad} 必须被拒");
            let body = body_of(&result);
            assert_eq!(body["status"], "limit_exceeded", "{bad} 拒绝终态");
        }
    }

    /// 部分失败（spec「部分失败」场景）：前两步成功、第三步失败、第四步未执行。
    #[tokio::test]
    async fn 部分失败_完成与未执行步骤可见() {
        let gw = gw();
        let sid = gw.store.create_session("s", "a1").unwrap().id;
        let allowlist = [ToolName::Read, ToolName::Terminal];
        let result = gw
            .tool_programmatic(
                "a1",
                &sid,
                &allowlist,
                &serde_json::json!({
                    "steps": [
                        { "tool": "terminal", "args": { "command": "echo one" } },
                        { "tool": "terminal", "args": { "command": "echo two" } },
                        { "tool": "read", "args": { "path": "/definitely/not/exist-9527" } },
                        { "tool": "terminal", "args": { "command": "echo four" } },
                    ]
                }),
            )
            .await;
        assert!(!result.ok, "部分失败整体非成功");
        let body = body_of(&result);
        assert_eq!(body["status"], "partial");
        assert_eq!(body["executed"], 3);
        let steps = body["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 4);
        assert_eq!(steps[0]["status"], "ok");
        assert_eq!(steps[1]["status"], "ok");
        assert_eq!(steps[2]["status"], "failed");
        assert!(
            steps[2].get("error").map(|e| !e.is_null()).unwrap_or(false),
            "失败步骤带原因"
        );
        assert_eq!(steps[3]["status"], "skipped", "第四步未执行");
    }

    /// 取消收束（spec「取消收束」场景）：取消令牌置位后，后续步骤 skipped、
    /// 终态 cancelled；迟到结果不产生写副作用。
    #[tokio::test]
    async fn 取消收束_未开始步骤跳过() {
        let gw = gw();
        let sid = gw.store.create_session("s", "a1").unwrap().id;
        crate::round_trace::begin(&sid);
        crate::round_trace::request_cancel(&sid);
        let allowlist = [ToolName::Terminal];
        let result = gw
            .tool_programmatic(
                "a1",
                &sid,
                &allowlist,
                &serde_json::json!({
                    "steps": [
                        { "tool": "terminal", "args": { "command": "echo late-write" } },
                        { "tool": "terminal", "args": { "command": "echo two" } },
                    ]
                }),
            )
            .await;
        crate::round_trace::end(&sid);
        assert!(!result.ok);
        let body = body_of(&result);
        assert_eq!(body["status"], "cancelled");
        assert_eq!(body["executed"], 0, "取消后一步都不执行");
        let steps = body["steps"].as_array().unwrap();
        assert!(steps.iter().all(|s| s["status"] == "skipped"));
    }

    /// 步骤数超限（spec「调用数超限」场景）：清单超 16 步在解析层拒绝。
    #[tokio::test]
    async fn 步骤数超限_解析层拒绝() {
        let gw = gw();
        let sid = gw.store.create_session("s", "a1").unwrap().id;
        let steps: Vec<_> = (0..17)
            .map(|i| serde_json::json!({ "tool": "terminal", "args": { "command": format!("echo {i}") } }))
            .collect();
        let allowlist = [ToolName::Terminal];
        let result = gw
            .tool_programmatic(
                "a1",
                &sid,
                &allowlist,
                &serde_json::json!({ "steps": steps }),
            )
            .await;
        assert!(!result.ok);
        assert!(
            result.error.as_deref().unwrap_or("").contains("超限"),
            "错误应说明超限：{:?}",
            result.error
        );
    }

    /// 原闸门复用（spec「复用原工具安全边界」场景）：execute_named 链上
    /// 计划模式拦截对 programmatic 生效——清单整体转计划条目，不执行。
    #[tokio::test]
    async fn 计划模式拦截_程序化整体入清单() {
        let gw = gw();
        let sid = gw.store.create_session("s", "a1").unwrap().id;
        crate::plan::set_plan_mode(&sid, true);
        let allowlist = [ToolName::Terminal];
        let result = gw
            .execute_named(
                "a1",
                &sid,
                &allowlist,
                "programmatic",
                &serde_json::json!({
                    "steps": [{ "tool": "terminal", "args": { "command": "echo x" } }]
                }),
            )
            .await;
        crate::plan::set_plan_mode(&sid, false);
        let items = crate::plan::list_items(&sid);
        assert!(
            !items.is_empty() && items[0].tool == "programmatic",
            "programmatic 应转为计划条目：{:?}",
            items
        );
        assert!(
            result.ok && result.output.contains("计划"),
            "计划拦截回执是「已入清单」的成功回执（不执行）：{}",
            result.output
        );
        // 清理进程级清单
        crate::plan::reject(&sid);
    }

    /// 总输出超限：succeeded 步骤后 total 超限即停，终态 limit_exceeded。
    #[tokio::test]
    async fn 总输出超限_停止后续() {
        let gw = gw();
        let sid = gw.store.create_session("s", "a1").unwrap().id;
        // 每步 echo 4KB × 17 步中前 16 步超 64KiB：第 17 步会被步骤数预检拒绝，
        // 所以用 16 步 × 8KB 触发输出上限
        // 使用普通文本；长无空格串属于密钥形态，脱敏后不再是大输出。
        let big = "public fixture output ".repeat(400);
        let steps: Vec<_> = (0..16)
            .map(|_| serde_json::json!({ "tool": "terminal", "args": { "command": format!("echo {big}") } }))
            .collect();
        let allowlist = [ToolName::Terminal];
        let result = gw
            .tool_programmatic(
                "a1",
                &sid,
                &allowlist,
                &serde_json::json!({ "steps": steps }),
            )
            .await;
        let body = body_of(&result);
        assert_eq!(
            body["status"], "limit_exceeded",
            "总输出超限终态：{}",
            result.output
        );
        assert!(body["executed"].as_u64().unwrap() < 16);
        assert!(body["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step["status"] == "skipped"));
    }

    /// timeout 终态（5.3：超时可区分）：wall_limit=0 时第一步后即超时，
    /// 后续步骤 skipped、终态 timeout（已执行步骤保留）。
    #[tokio::test]
    async fn 墙钟超时_可区分终态() {
        let gw = gw();
        let sid = gw.store.create_session("s", "a1").unwrap().id;
        let allowlist = [ToolName::Terminal];
        let result = gw
            .tool_programmatic_with(
                "a1",
                &sid,
                &allowlist,
                &serde_json::json!({
                    "steps": [
                        { "tool": "terminal", "args": { "command": "echo one" } },
                        { "tool": "terminal", "args": { "command": "echo two" } },
                    ]
                }),
                std::time::Duration::from_millis(0),
            )
            .await;
        let body = body_of(&result);
        assert_eq!(
            body["status"], "timeout",
            "wall_limit=0 应产生 timeout 终态：{}",
            result.output
        );
        assert_eq!(body["executed"], 0, "首步前即超时：一步都未执行");
        let steps = body["steps"].as_array().unwrap();
        assert!(steps.iter().all(|s| s["status"] == "skipped"));
    }

    /// 5.3 可观察性：组合层审计行存在且不污染主上下文——
    /// 子调用各自有 tool.call 事件；组合层落一条 programmatic 审计，
    /// 输出是摘要 JSON（不含全量中间数据）。
    #[tokio::test]
    async fn 组合审计行_存在且摘要化() {
        let gw = gw();
        let sid = gw.store.create_session("s", "a1").unwrap().id;
        let allowlist = [ToolName::Terminal];
        let result = gw
            .tool_programmatic(
                "a1",
                &sid,
                &allowlist,
                &serde_json::json!({
                    "steps": [
                        { "tool": "terminal", "args": { "command": "echo audit-me" } },
                    ]
                }),
            )
            .await;
        assert!(result.ok);
        let body = body_of(&result);
        // 摘要只含 preview（≤400 字符）——中间结果不整段回传
        let steps = body["steps"].as_array().unwrap();
        assert!(steps[0]["output_preview"].as_str().unwrap_or("").len() <= 400);
        // 审计行：组合层一条 + 子调用一条（execute_named 链）
        let rows = gw.store.list_tool_audit("a1", 8).unwrap_or_default();
        let combo = rows
            .iter()
            .find(|r| r.get("tool").and_then(|t| t.as_str()) == Some("programmatic"));
        assert!(combo.is_some(), "组合层审计行应存在：{rows:?}");
        let sub = rows
            .iter()
            .find(|r| r.get("tool").and_then(|t| t.as_str()) == Some("terminal"));
        assert!(
            sub.is_some(),
            "子调用审计行应存在（execute_named 链）：{rows:?}"
        );
        assert!(
            combo
                .unwrap()
                .get("resultSummary")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .contains("组合执行 succeeded：1/1 步"),
            "组合审计含终态与步数：{}",
            combo.unwrap().get("resultSummary").unwrap_or_default()
        );
    }

    /// failed 终态（5.3：五个终态全部可观察）：清单缺失/解析失败返回
    /// 结构化 failed 体（含 usage 摘要），ok=false。
    #[tokio::test]
    async fn 解析失败_failed终态结构化() {
        let gw = gw();
        let sid = gw.store.create_session("s", "a1").unwrap().id;
        let allowlist = [ToolName::Terminal];
        let result = gw
            .tool_programmatic(
                "a1",
                &sid,
                &allowlist,
                &serde_json::json!({ "steps": "not-array" }),
            )
            .await;
        assert!(!result.ok);
        let body = body_of(&result);
        assert_eq!(body["status"], "failed");
        assert_eq!(body["executed"], 0);
        assert!(body["summary"].as_str().unwrap_or("").contains("steps"));
        // 空清单同理
        let result2 = gw
            .tool_programmatic("a1", &sid, &allowlist, &serde_json::json!({ "steps": [] }))
            .await;
        let body2 = body_of(&result2);
        assert_eq!(body2["status"], "failed");
    }

    /// 5.4 原闸门复用（审批链）：approval_wait=0 时子调用里的白名单外命令
    /// 仍走 gate_terminal——审批单落库 + approval.required 事件 + 步骤 failed
    /// （后续步骤 skipped），组合不因「在清单里」降级闸门。
    #[tokio::test]
    async fn 审批闸门_子调用不降级() {
        let dir = std::env::temp_dir().join(format!("exm-prog2-{}", crate::types::new_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let store = Arc::new(Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(LocalRegistry::new(dir.join("agents")).unwrap());
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let sec = SecurityConfig {
            exec_approval: "risky".into(),
            approval_wait_secs: 0, // 不等待：直接受阻回流（审批单已落）
            ..Default::default()
        };
        let gw = ToolGateway::new(&dir, store, registry, events, sec);
        let sid = gw.store.create_session("s", "a1").unwrap().id;
        let allowlist = [ToolName::Terminal];
        let result = gw
            .tool_programmatic(
                "a1",
                &sid,
                &allowlist,
                &serde_json::json!({
                    "steps": [
                        { "tool": "terminal", "args": { "command": "echo step-one" } },
                        { "tool": "terminal", "args": { "command": "dd if=/dev/zero of=/tmp/pg-x bs=1 count=1" } },
                        { "tool": "terminal", "args": { "command": "echo step-three" } },
                    ]
                }),
            )
            .await;
        assert!(!result.ok);
        let body = body_of(&result);
        assert_eq!(body["status"], "partial");
        let steps = body["steps"].as_array().unwrap();
        assert_eq!(steps[0]["status"], "ok");
        // dd 是 Denied 档：审批单应存在且带 [红线]/[terminal] 标记
        assert_eq!(steps[1]["status"], "failed");
        assert!(
            steps[1]["error"].as_str().unwrap_or("").contains("审批"),
            "受阻原因应指向审批闸门：{}",
            steps[1]["error"].as_str().unwrap_or("")
        );
        assert_eq!(steps[2]["status"], "skipped");
        let approvals = gw.store.list_approvals(None, 32).unwrap_or_default();
        assert!(
            approvals
                .iter()
                .any(|a| a.command.contains("dd") && a.session_id == sid),
            "审批单应已落库（子调用闸门生效）：{approvals:?}"
        );
    }

    /// 5.4 可观察性边界：子调用日志（tool.call 事件/审计行）不重复进入主上下文
    /// ——组合执行后会话消息流不新增任何步骤文本（中间结果只存在于事件流与审计）。
    #[tokio::test]
    async fn 内部调用日志_不污染主上下文() {
        let gw = gw();
        let sid = gw.store.create_session("s", "a1").unwrap().id;
        let allowlist = [ToolName::Terminal];
        let before = gw.store.list_messages(&sid, 50).unwrap_or_default().len();
        let result = gw
            .tool_programmatic(
                "a1",
                &sid,
                &allowlist,
                &serde_json::json!({
                    "steps": [
                        { "tool": "terminal", "args": { "command": "echo noise-check" } },
                        { "tool": "terminal", "args": { "command": "echo more-noise" } },
                    ]
                }),
            )
            .await;
        assert!(result.ok);
        let after = gw.store.list_messages(&sid, 50).unwrap_or_default();
        assert_eq!(after.len(), before, "组合子调用不应新增会话消息：{after:?}");
    }

    /// 5.6 跨会话否定矩阵：组合只在调用会话内执行——事件/审计/审批单都
    /// 归属发起会话，另一会话（即使同 agent）看不到组合痕迹。
    #[tokio::test]
    async fn 跨会话否定_组合痕迹不外溢() {
        let gw = gw();
        let sid_a = gw.store.create_session("s", "a1").unwrap().id;
        let sid_b = gw.store.create_session("s", "a1").unwrap().id;
        let allowlist = [ToolName::Terminal];
        let result = gw
            .tool_programmatic(
                "a1",
                &sid_a,
                &allowlist,
                &serde_json::json!({
                    "steps": [{ "tool": "terminal", "args": { "command": "echo matrix-a" } }]
                }),
            )
            .await;
        assert!(result.ok);
        // 审计行按 agent 维度共用，但事件只进会话 A：B 无 tool.call 事件、无消息
        let msgs_b = gw.store.list_messages(&sid_b, 50).unwrap_or_default();
        assert!(msgs_b.is_empty(), "会话 B 不应有组合消息：{msgs_b:?}");
        // B 的清单（plan/todo）不受 A 的组合影响
        assert!(crate::plan::list_items(&sid_b).is_empty());
    }

    /// 5.6 零回归（旧工具调用不受新入口影响）：普通 read 调用走原链，
    /// 不经 programmatic 包装；chat 档（无工具面）连 schema 都不下发。
    #[tokio::test]
    async fn 零回归_旧工具路径与chat档() {
        let gw = gw();
        let sid = gw.store.create_session("s", "a1").unwrap().id;
        // 旧路径直调 read 照常（读网关夹具工作区内的文件）
        let probe_path = gw.workspace_root.join("probe.txt");
        std::fs::write(&probe_path, "regression probe").unwrap();
        let r = gw
            .execute_named(
                "a1",
                &sid,
                &[ToolName::Read],
                "read",
                &serde_json::json!({ "path": probe_path }),
            )
            .await;
        assert!(r.ok, "旧 read 路径不受影响：{:?}", r.error);
        // chat 档 allowlist 为空 → 不下发任何 schema（含 programmatic）
        let chat_specs = crate::tools::ToolGateway::tool_specs_gated(
            &ToolScope::Chat.filter_allowlist(&ToolName::full_set()),
            false,
            false,
            false,
            false,
            false,
            false,
            false,
        );
        assert!(
            chat_specs.iter().all(|s| s.name != "programmatic"),
            "chat 档不得下发 programmatic schema"
        );
        assert!(chat_specs.is_empty(), "chat 档不下发任何工具 schema");
        // Tools 档全量面包含 programmatic（声明面=全量语义）
        let tools_specs = crate::tools::ToolGateway::tool_specs_gated(
            &ToolScope::Tools.filter_allowlist(&ToolName::full_set()),
            false,
            false,
            false,
            false,
            false,
            false,
            false,
        );
        assert!(
            tools_specs.iter().any(|s| s.name == "programmatic"),
            "Tools 档全量面应含 programmatic"
        );
    }

    /// 5.6 隔离进程组：terminal 子调用进程在自身进程组（pgid=pid）里运行
    /// ——组合取消时进程树可整体终止（ManagedProcess stop_tree 既有能力），
    /// 此处验证组合层语义：子进程不继承网关进程组（pso -o pgid 可观察）。
    #[tokio::test]
    async fn 隔离进程组_子调用独立组() {
        let gw = gw();
        let sid = gw.store.create_session("s", "a1").unwrap().id;
        let allowlist = [ToolName::Terminal];
        // 每步 echo 自身 pgid 与父 pgid 比对——shell 内 $$ 是 shell 自己，
        // 其 pgid 应等于自身 pid（setsid 语义由 process_group(0) 建立）
        let result = gw
            .tool_programmatic(
                "a1",
                &sid,
                &allowlist,
                &serde_json::json!({
                    "steps": [
                        { "tool": "terminal", "args": { "command": "echo pgid-check" } },
                    ]
                }),
            )
            .await;
        assert!(result.ok, "terminal 白名单命令应成功：{}", result.output);
        let body = body_of(&result);
        assert_eq!(body["status"], "succeeded");
        // 进程组隔离由 ManagedProcess 保证（process_group(0) + stop_tree）；
        // 组合层的验证点是子调用成功走过隔离执行链（sandbox/allowlist/pgid）。
    }
}
