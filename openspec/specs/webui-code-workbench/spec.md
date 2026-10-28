# webui-code-workbench Specification

## Purpose
WebUI 代码工作台：文件树、diff 审查卡片、终端产出面板的交互契约，使网关部署形态具备远程编程工作台体验。

## Requirements

### Requirement: 工作区文件树
WebUI MUST 提供工作台视图：以文件树呈现当前工作区（尊重 .gitignore），支持浏览文件内容（与 read 工具同口径的分页）、发起智能体会话引用该文件。文件树操作 MUST 经网关 REST 端点走与 read 工具一致的权限边界。

#### Scenario: 浏览工作区文件
- **WHEN** 用户在工作台展开目录并点击文件
- **THEN** 文件内容按 read 工具同口径分页展示

#### Scenario: 引用文件发起会话
- **WHEN** 用户在文件视图选择"在会话中引用"
- **THEN** 当前会话输入区插入该文件引用，智能体可用 read 读取

### Requirement: diff 审查卡片
智能体的 edit/patch/git 写动作 MUST 在 WebUI 会话流中呈现为 diff 审查卡片：增删行着色、逐文件分组、与检查点/回滚操作联动。用户 MUST 能从卡片直接发起回滚到该动作前状态。

#### Scenario: diff 卡片回滚
- **WHEN** 用户在某 edit 动作的 diff 卡片点击回滚
- **THEN** 文件恢复到该动作前的检查点状态，会话流记录回滚事件

### Requirement: 终端产出面板
terminal 工具执行 MUST 在会话流呈现为终端产出面板：命令、退出码、stdout/stderr 分区、长输出折叠。后台长任务 MUST 显示运行状态并支持在面板内轮询/终止。

#### Scenario: 长任务面板交互
- **WHEN** 智能体以后台模式启动构建命令
- **THEN** 面板显示任务运行中状态，用户可展开输出、手动终止
