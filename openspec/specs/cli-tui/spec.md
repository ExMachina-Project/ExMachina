# cli-tui Specification

## Purpose
CLI 全屏终端界面：`exm` 裸命令升级为 ratatui 全屏 TUI，对齐终端原生编程助手的交互形态；`exm chat` 保留 readline 简洁形态。

## Requirements

### Requirement: 全屏 TUI 形态
`exm` 裸命令 MUST 启动全屏终端界面：消息流区（对话历史、工具调用卡片、diff 内联渲染）、输入区、状态栏（会话/模型/审批模式）。界面 MUST 支持键盘滚动消息历史，终端尺寸变化时正确重绘。退出 TUI 后终端状态 MUST 完整恢复。

#### Scenario: 启动与退出
- **WHEN** 用户运行 `exm` 并按退出键
- **THEN** 进入全屏界面，退出后终端恢复到进入前的状态

#### Scenario: diff 内联渲染
- **WHEN** 智能体执行 edit/patch 工具
- **THEN** 消息流中以增删行着色的 diff 形式呈现变更

### Requirement: TUI 内审批处理
待审批项（终端命令、git 写动作、计划清单）MUST 在 TUI 中以审批面板呈现：查看详情、单条批准/拒绝、全部批准。审批操作 MUST 与 WebUI 审批单共享同一状态（一处处理，他处消失）。

#### Scenario: 审批面板操作
- **WHEN** 存在待审批终端命令且用户打开审批面板
- **THEN** 可查看命令全文并批准或拒绝，结果同步到所有端

### Requirement: 斜杠命令保留
既有 readline 面板的斜杠命令集（/status /model /approval 等）MUST 在 TUI 输入区可用，语义与原实现一致。

#### Scenario: TUI 内切换模型
- **WHEN** 用户在 TUI 输入 /model 命令
- **THEN** 模型切换交互在 TUI 内完成，效果与 readline 形态一致
