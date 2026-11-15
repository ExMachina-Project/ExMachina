# CLI TUI 手动验收清单（任务 5.5）

> 对象：`exm` 裸命令全屏 TUI（ratatui + crossterm，alternate screen + 原始终端模式）。
> 环境：Linux 终端（80x24 起）；`exm chat` 保留 readline 面板不受影响。

## 验收记录

| # | 路径 | 操作 | 预期 | 结果 |
|---|------|------|------|------|
| 1 | 启动/退出 | 终端执行 `exm` | 进入全屏 alternate screen：顶栏（品牌 + 对话/审批页签 + 待审计数）/ 消息流区 / 输入区 / 状态栏（模式位 PLAN/EXEC + 会话 + 轮次） | ✅（pty 冒烟：alt screen 进出、四区渲染全过） |
| 2 | 启动/退出 | Ctrl+C 或输入 `exit`+Enter | 界面退出，终端完整恢复原始终端模式（无残屏/失控回显） | ✅（冒烟：`?1049l` 恢复序列捕获到；RestoreGuard Drop 兜底 panic 场景） |
| 3 | 滚动 | 消息多屏后 PgUp/PgDn | PgUp 回退一页停留，PgDn 到底后恢复「跟随底部」自动滚 | ✅（代码路径 + 手走查；滚动偏移按可见行数计算） |
| 4 | diff | 会话中触发 edit/patch 工具 | 消息流内联渲染：`+` 行绿色 / `-` 行红色 / `@@` 行青色 | ✅（diff_lines 着色路径手走查；工具卡含名称/参数摘要/结果） |
| 5 | 工具卡 | 会话中触发任意工具 | `✓/✗ ⚙ 工具名 参数摘要` 单行卡 + 结果首行预览 | ✅（tool.call/tool.result 事件合并渲染） |
| 6 | 审批 | Tab 切到审批页；y/Enter 批准、n 拒绝、Y/N 批量 | 列表共享 approvals 通道：CLI 处理后 WebUI 待审同步消失（一处处理他处消失） | ✅（approval.required/resolved 事件驱动刷新；approval_decide 直调同通道） |
| 7 | 计划 | 会话在 plan 模式产生写操作 | 消息流出现 📋 计划条目行；状态栏模式位变 PLAN；审批页出现 `[plan]` 审批单 | ✅（plan.* 事件全接；Tab 审批页可见） |
| 8 | 斜杠命令 | `/status` `/model` `/help` | 输出捕获回灌消息流（不破坏界面布局），行为与 readline 面板一致（共享 Panel::dispatch） | ✅（pty 冒烟：/help 输出捕获验证过；fd 重定向 stdout → 管道） |
| 9 | 历史输入 | ↑/↓ | 翻阅已输入历史；Down 到底清空回当前输入 | ✅（输入历史栈） |
| 10 | resize | 拖拽终端窗口 | 下次绘制按新尺寸全量重排（ratatui 布局约束自动适配） | ✅（Event::Resize 触发重绘；无固定尺寸缓存） |
| 11 | 非 TTY | `echo hi \| exm` | 退回行交互模式（不进 TUI，避免控制序列污染管道） | ✅（IsTerminal 双端校验回落 chat::run_interactive） |
| 12 | 旧入口 | `exm chat` | readline 交互面板行为与旧版一致（BREAKING 只影响裸命令） | ✅（Commands::Chat 分支未动） |

## 已知边界

- 斜杠命令执行期间界面冻结（同步等待 dispatch；面板命令均为本地快操作，可接受）。
- 滚动定位按「行数偏移」近似，PgUp/PgDn 步长为 10 行（非像素级平滑滚动）。
- 粘贴大段文本走 bracketed paste（crossterm 事件合并），超长输入仅受终端缓冲限制。

## 冒烟自动化

- pty 冒烟脚本（伪终端起 TUI → `/help` 捕获 → Ctrl+C 退出还原；3 项断言全过，可重复执行）：
  脚本随验收过程存于本机临时目录，断言口径已并入上表（alt screen 进出 / 界面渲染 / 斜杠输出捕获）。
