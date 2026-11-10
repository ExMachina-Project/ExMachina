/** 会话分类辅助（信息架构 2026-11）：按「谁发起」划分三页。
 *
 *  - 对话页（ChatView）：用户直接发起的会话，带输入框；
 *  - 聊天页（FlowsView）：外部通道聊天流（群/私聊），流会话 title 前缀 `channel:`，只读呈现；
 *  - 任务页（TasksView）：系统自动会话（定时任务 / 心跳巡检），只读呈现。
 *
 *  口径与后端 session_origin 一致：通道会话以 `channel:` 前缀为事实源
 *  （crates/exm-core/src/session_origin.rs）；任务会话以 cron 复用逻辑的
 *  标题约定为准（`job-<id>` 缺省 / session_title 自命名 / 心跳巡检固定前缀）。
 */

/** 系统任务会话判定：cron 任务（job-* 或任务自定义标题）+ 心跳巡检系。
 *  保守口径：只识别确定性模式（job- 前缀 / 心跳巡检），不误伤用户同名会话——
 *  cron 自定义 session_title 与用户会话无法从 title 区分，归对话页（可用即可）。 */
export function isAutoSessionTitle(title: string): boolean {
  return title.startsWith("job-") || title.startsWith("心跳巡检");
}

/** 派发子会话判定（task-dispatch-child）：编排派发节点的运行台账会话，
 *  标题 `sub:<identifier>·<goal>·<node>`（orchestrator create_child_session）。
 *  归「任务」页只读呈现——此前漏进对话页（部署实测：creative-agent 派发
 *  在对话页出现 sub: 会话）。 */
export function isDispatchChildTitle(title: string): boolean {
  return title.startsWith("sub:");
}

/** 会话是否属于「任务」页（系统自动 + 派发子会话） */
export function isTaskSideTitle(title: string): boolean {
  return isAutoSessionTitle(title) || isDispatchChildTitle(title);
}

/** 通道流会话判定（与后端 parse_session_origin 的前缀事实源一致） */
export function isChannelSessionTitle(title: string): boolean {
  return title.startsWith("channel:");
}
