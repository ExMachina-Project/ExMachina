//! tools/computer.rs —— 自 tools.rs 按领域拆出（纯搬移，零行为变更）

use super::*;

impl ToolGateway {
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
        let mut guard = self.computer.lock().await;
        // 懒初始化：首次调用才建立键鼠控制句柄
        if guard.is_none() {
            match crate::computer::ComputerSession::new() {
                Ok(s) => *guard = Some(s),
                Err(e) => return ToolResult::err(format!("Computer Use 初始化失败：{e}")),
            }
        }
        let session = guard.as_mut().unwrap();
        let interval = self.computer_cfg.action_interval_ms;

        // 截屏：只读，不审批
        if op == "screenshot" {
            let index = args.get("monitor").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let stamp = crate::types::now_iso().replace([':', '-', '.'], "");
            let name = format!("computer-{stamp}");
            let dir = self.workspace_root.join(".exmachina").join("screenshots");
            let result = session.screenshot(index, self.computer_cfg.max_edge, &dir, &name);
            return match result {
                Ok(shot) => {
                    let rel = std::path::Path::new(&shot.path)
                        .strip_prefix(&self.workspace_root)
                        .map(|p| p.display().to_string().replace('\\', "/"))
                        .unwrap_or_else(|_| shot.path.clone());
                    // 产物登记（D12）：截图自动进入会话 outbox，通道投递
                    let _ = self
                        .store
                        .outbox_push(session_id, "image", &rel, "屏幕截图");
                    ToolResult::ok(format!(
                        "【报告】截屏完成：{}，缩放后 {}x{}，文件 {rel}\n（截图已作为图像附带本轮回灌，可直接描述画面内容）",
                        shot.monitor, shot.width, shot.height
                    ))
                    .with_images(vec![rel])
                }
                Err(e) => ToolResult::err(format!("截屏失败：{e}")),
            };
        }

        // 输入类动作：统一审批闸门 + 节流
        let (desc, action): (String, InputAction) = match op {
            "move" => {
                let x = args.get("x").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                let y = args.get("y").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                (format!("move ({x}, {y})"), Box::new(move |s| s.move_mouse(x, y)))
            }
            "click" => {
                let x = args.get("x").and_then(|v| v.as_i64());
                let y = args.get("y").and_then(|v| v.as_i64());
                let button = args.get("button").and_then(|v| v.as_str()).unwrap_or("left").to_string();
                let double = args.get("double").and_then(|v| v.as_bool()).unwrap_or(false);
                let label = format!(
                    "click {}{}",
                    if double { "double " } else { "" },
                    button
                );
                let label = match (x, y) {
                    (Some(x), Some(y)) => format!("{label} @ ({x}, {y})"),
                    _ => label,
                };
                (label, Box::new(move |s| {
                    if let (Some(x), Some(y)) = (x, y) {
                        s.move_mouse(x as i32, y as i32)?;
                    }
                    s.click(&button, double)
                }))
            }
            "scroll" => {
                let dx = args.get("dx").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                let dy = args.get("dy").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                (format!("scroll dx={dx} dy={dy}"), Box::new(move |s| s.scroll(dx, dy)))
            }
            "type" => {
                let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let label = format!("type「{}」", text.chars().take(40).collect::<String>());
                (label, Box::new(move |s| s.type_text(&text)))
            }
            "key" => {
                let combo = args.get("combo").and_then(|v| v.as_str()).unwrap_or("").to_string();
                (format!("key {combo}"), Box::new(move |s| s.hotkey(&combo)))
            }
            other => return ToolResult::err(format!(
                "未知 computer 操作: {other}（支持 screenshot / click / move / scroll / type / key）"
            )),
        };

        // 审批：human 输入权确认（computer.approval=true 时拦截；等待期间批准即放行）
        if self.computer_cfg.approval {
            let gated = self
                .gate_command(agent_id, session_id, &format!("[computer] {desc}"))
                .await;
            if !gated.ok {
                return gated;
            }
        }

        session.throttle(interval);
        let result = action(session);
        match result {
            Ok(()) => ToolResult::ok(format!(
                "【报告】Computer Use 动作已执行：{desc}。需要确认画面可再 screenshot。"
            )),
            Err(e) => ToolResult::err(format!("Computer Use 动作失败（{desc}）：{e}")),
        }
    }
}
