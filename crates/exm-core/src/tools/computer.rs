//! tools/computer.rs —— 自 tools.rs 按领域拆出（纯搬移，零行为变更）

use super::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

pub type ComputerActionExecutor =
    Arc<dyn Fn(&ComputerApprovalAction) -> anyhow::Result<()> + Send + Sync>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerApprovalAction {
    pub version: u8,
    pub tool: String,
    pub op: String,
    pub args: Value,
    pub description: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn canonical_action(op: &str, args: &Value) -> anyhow::Result<(Value, String)> {
    match op {
        "move" => {
            let x = args.get("x").and_then(Value::as_i64).unwrap_or(0) as i32;
            let y = args.get("y").and_then(Value::as_i64).unwrap_or(0) as i32;
            Ok((json!({ "x": x, "y": y }), format!("move ({x}, {y})")))
        }
        "click" => {
            let x = args.get("x").and_then(Value::as_i64);
            let y = args.get("y").and_then(Value::as_i64);
            let button = args.get("button").and_then(Value::as_str).unwrap_or("left");
            anyhow::ensure!(
                matches!(button, "left" | "right" | "middle"),
                "button 必须是 left/right/middle"
            );
            let double = args.get("double").and_then(Value::as_bool).unwrap_or(false);
            let label = format!("click {}{}", if double { "double " } else { "" }, button);
            let description = match (x, y) {
                (Some(x), Some(y)) => format!("{label} @ ({x}, {y})"),
                _ => label,
            };
            Ok((
                json!({ "x": x, "y": y, "button": button, "double": double }),
                description,
            ))
        }
        "scroll" => {
            let dx = args.get("dx").and_then(Value::as_i64).unwrap_or(0) as i32;
            let dy = args.get("dy").and_then(Value::as_i64).unwrap_or(0) as i32;
            Ok((
                json!({ "dx": dx, "dy": dy }),
                format!("scroll dx={dx} dy={dy}"),
            ))
        }
        "type" => {
            let text = args.get("text").and_then(Value::as_str).unwrap_or("");
            anyhow::ensure!(!text.is_empty(), "text 不能为空");
            anyhow::ensure!(text.len() <= 4096, "type 文本超过 4KB 上限");
            Ok((
                json!({ "text": text }),
                format!("type「{}」", text.chars().take(40).collect::<String>()),
            ))
        }
        "key" => {
            let combo = args.get("combo").and_then(Value::as_str).unwrap_or("");
            anyhow::ensure!(!combo.trim().is_empty(), "按键组合不能为空");
            anyhow::ensure!(combo.len() <= 64, "按键组合过长");
            Ok((json!({ "combo": combo }), format!("key {combo}")))
        }
        other => anyhow::bail!(
            "未知 computer 操作: {other}（支持 screenshot / click / move / scroll / type / key）"
        ),
    }
}

impl ComputerApprovalAction {
    pub(crate) fn from_args(args: &Value, approval_wait_secs: u64) -> anyhow::Result<Self> {
        let op = args
            .get("op")
            .and_then(Value::as_str)
            .unwrap_or("screenshot");
        let (canonical, description) = canonical_action(op, args)?;
        let created_at_ms = now_ms();
        let ttl_secs = if approval_wait_secs == 0 {
            300
        } else {
            approval_wait_secs.clamp(1, 24 * 60 * 60)
        };
        Ok(Self {
            version: 1,
            tool: "computer".into(),
            op: op.into(),
            args: canonical,
            description,
            created_at_ms,
            expires_at_ms: created_at_ms.saturating_add(ttl_secs.saturating_mul(1000)),
        })
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.version == 1, "computer 审批版本不受支持");
        anyhow::ensure!(self.tool == "computer", "computer 审批动作类型无效");
        anyhow::ensure!(
            self.created_at_ms <= now_ms().saturating_add(60_000),
            "computer 审批创建时间无效"
        );
        anyhow::ensure!(
            self.expires_at_ms > self.created_at_ms
                && self.expires_at_ms - self.created_at_ms <= 24 * 60 * 60 * 1000,
            "computer 审批有效期无效"
        );
        anyhow::ensure!(now_ms() < self.expires_at_ms, "computer 审批已过期，未执行");
        let (canonical, description) = canonical_action(&self.op, &self.args)?;
        anyhow::ensure!(canonical == self.args, "computer 审批参数不是规范动作");
        anyhow::ensure!(
            description == self.description,
            "computer 审批描述与动作不一致"
        );
        Ok(())
    }

    fn execute(&self, session: &mut crate::computer::ComputerSession) -> anyhow::Result<()> {
        match self.op.as_str() {
            "move" => session.move_mouse(
                self.args["x"].as_i64().unwrap_or(0) as i32,
                self.args["y"].as_i64().unwrap_or(0) as i32,
            ),
            "click" => {
                if let (Some(x), Some(y)) = (self.args["x"].as_i64(), self.args["y"].as_i64()) {
                    session.move_mouse(x as i32, y as i32)?;
                }
                session.click(
                    self.args["button"].as_str().unwrap_or("left"),
                    self.args["double"].as_bool().unwrap_or(false),
                )
            }
            "scroll" => session.scroll(
                self.args["dx"].as_i64().unwrap_or(0) as i32,
                self.args["dy"].as_i64().unwrap_or(0) as i32,
            ),
            "type" => session.type_text(self.args["text"].as_str().unwrap_or("")),
            "key" => session.hotkey(self.args["combo"].as_str().unwrap_or("")),
            other => anyhow::bail!("未知 computer 动作：{other}"),
        }
    }
}

pub(crate) fn computer_action_from_approval(
    request: &crate::types::ApprovalRequest,
) -> anyhow::Result<Option<ComputerApprovalAction>> {
    let Some(raw) = request.meta_json.as_deref() else {
        return Ok(None);
    };
    let value: Value = serde_json::from_str(raw)?;
    let Some(action) = value.get("computerAction") else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_value(action.clone())?))
}

impl ToolGateway {
    /// Inject a deterministic mechanical executor for isolated acceptance tests.
    /// Production leaves this unset and always uses the local ComputerSession.
    pub fn set_computer_executor(&self, executor: Option<ComputerActionExecutor>) {
        *self.computer_executor.write() = executor;
    }

    async fn execute_computer_action(&self, action: &ComputerApprovalAction) -> anyhow::Result<()> {
        anyhow::ensure!(self.computer_cfg.enabled, "Computer Use 未启用");
        action.validate()?;
        if let Some(executor) = self.computer_executor.read().clone() {
            return executor(action);
        }
        let mut guard = self.computer.lock().await;
        if guard.is_none() {
            *guard = Some(crate::computer::ComputerSession::new()?);
        }
        let session = guard.as_mut().expect("computer session initialized");
        session.throttle(self.computer_cfg.action_interval_ms);
        action.execute(session)
    }

    pub(crate) async fn execute_computer_approval(
        &self,
        action: &ComputerApprovalAction,
    ) -> anyhow::Result<String> {
        self.execute_computer_action(action).await?;
        Ok(format!("Computer Use 动作已执行：{}", action.description))
    }

    /// Computer Use 工具：截屏（只读）与键鼠动作（过审批闸门 + 动作节流）。
    /// 截图落盘 `.exmachina/screenshots/`，路径随 `ToolResult.images` 带回——
    /// 视觉模型直接看到屏幕画面；渠道在工具卡里同样可预览。
    pub(crate) async fn tool_computer(
        &self,
        agent_id: &str,
        session_id: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        if !self.computer_cfg.enabled {
            return ToolResult::err(
                "Computer Use 未启用：请到「设置 → Computer Use」开启（整机输入权须显式交出）",
            );
        }
        let op = args
            .get("op")
            .and_then(|v| v.as_str())
            .unwrap_or("screenshot");
        // 截屏：只读，不审批
        if op == "screenshot" {
            let mut guard = self.computer.lock().await;
            if guard.is_none() {
                match crate::computer::ComputerSession::new() {
                    Ok(s) => *guard = Some(s),
                    Err(e) => return ToolResult::err(format!("Computer Use 初始化失败：{e}")),
                }
            }
            let session = guard.as_mut().expect("computer session initialized");
            let index = args.get("monitor").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let stamp = crate::types::now_iso().replace([':', '-', '.'], "");
            let name = format!("computer-{stamp}");
            let root = self.effective_root();
            let dir = root.join(".exmachina").join("screenshots");
            let result = session.screenshot(index, self.computer_cfg.max_edge, &dir, &name);
            return match result {
                Ok(shot) => {
                    let rel = std::path::Path::new(&shot.path)
                        .strip_prefix(&root)
                        .map(|p| p.display().to_string().replace('\\', "/"))
                        .unwrap_or_else(|_| shot.path.clone());
                    // 产物登记（D12）：截图自动进入会话 outbox，通道投递
                    let _ = self
                        .store
                        .outbox_push(session_id, "image", &rel, "屏幕截图");
                    ToolResult::ok(format!(
                        "【报告】截屏完成：{}，缩放后 {}x{}，文件 {rel}\n（图像将根据当前模型的视觉能力回灌；若模型不支持视觉，会明确报告降级）",
                        shot.monitor, shot.width, shot.height
                    ))
                    .with_images(vec![rel])
                }
                Err(e) => ToolResult::err(format!("截屏失败：{e}")),
            };
        }

        // 输入类动作先规范化为有限的类型化载荷；审批描述永不被解释为命令。
        let action = match ComputerApprovalAction::from_args(args, self.security.approval_wait_secs)
        {
            Ok(action) => action,
            Err(error) => return ToolResult::err(error.to_string()),
        };

        // 审批：human 输入权确认（computer.approval=true 时拦截；等待期间批准即放行）
        if self.computer_cfg.approval {
            return self.gate_computer(agent_id, session_id, action).await;
        }

        match self.execute_computer_action(&action).await {
            Ok(()) => ToolResult::ok(format!(
                "【报告】Computer Use 动作已执行：{}。需要确认画面可再 screenshot。",
                action.description
            )),
            Err(error) => ToolResult::err(format!(
                "Computer Use 动作失败（{}）：{error}",
                action.description
            )),
        }
    }
}
