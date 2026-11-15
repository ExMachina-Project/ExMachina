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

TUI 与 panel 的斜杠命令族（/flows、/copy、/caps 等）SHALL 保持既有清单与语义；不移除、不改名任何既有命令。新增能力经顶层 CLI 参数（-p/-c/-r/--output-format）进入，不占用斜杠命令面。

#### Scenario: TUI 内切换模型

- **WHEN** TUI 内执行既有模型切换命令
- **THEN** 行为与现状一致（命令族零回归锚点）

#### Scenario: 既有命令零回归

- **WHEN** 升级后在 TUI/panel 输入任一既有斜杠命令
- **THEN** 行为与升级前一致

### Requirement: 会话参数面扩展

CLI 顶层 SHALL 支持 `-c/--continue`（接续最近会话）与 `-r/--resume <匹配>`（按标题/ID 匹配恢复）：对 TUI、panel、`-p` 无头三形态一致生效；与 `--session <id>` 组合时按显式 ID > `-r` > `-c` 优先序取一（匹配与恢复语义详见 session-resume capability）。

#### Scenario: 三形态一致

- **WHEN** 分别以 `exm -c`、`exm chat -c`（panel）、`exm -p "..." -c` 运行且存在历史会话
- **THEN** 三者都打开同一最近活跃会话接续

### Requirement: 无头参数面

CLI 顶层 SHALL 支持 `-p/--print [text]` 与 `--output-format text|json|stream-json`（缺省 text）：`-p` 不进入交互 UI、单轮执行即退（输出格式与退出码契约详见 headless-print capability）；无 `-p` 时 `--output-format` 被忽略且不报错。

#### Scenario: 无 -p 时格式参数无效

- **WHEN** 运行 `exm --output-format json`（无 -p）
- **THEN** 按无该参数的既有行为启动（TUI/panel），不报错
