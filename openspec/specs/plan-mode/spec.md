# plan-mode Specification

## Purpose
计划模式：只读规划、计划清单生成、审批后切换执行、跨端审批 UI 的行为契约，让编程任务在动手前获得人工确认。

## Requirements

### Requirement: 只读规划模式
会话进入计划模式后，系统 MUST 拦截所有写入类工具调用（文件写入/编辑/补丁/终端写操作/计算机控制输入类），只允许读取类工具（read/grep/glob/web 检索等）。被拦截的写操作 MUST 转换为计划条目（目标文件、拟执行动作、理由），累积为计划清单而非执行。

#### Scenario: 规划期拦截写操作
- **WHEN** 计划模式下智能体调用 edit 工具修改文件
- **THEN** 该调用不执行，转换为一条计划条目记录目标文件与拟变更内容

#### Scenario: 读取工具不受限
- **WHEN** 计划模式下智能体调用 read/grep/glob
- **THEN** 调用正常执行并返回结果

### Requirement: 计划审批与模式切换
用户 MUST 能对计划清单执行批准或拒绝。批准后系统 MUST 将会话切换到执行模式并按计划条目顺序生成对应工具调用；拒绝后系统 MUST 保留计划供修改且不执行任何条目。计划审批状态 MUST 在会话事件流中广播（CLI/WebUI/通道一致）。

#### Scenario: 批准后落地执行
- **WHEN** 用户批准一份含 3 个文件编辑条目的计划
- **THEN** 会话切换到执行模式，3 个编辑按计划内容依次应用

#### Scenario: 拒绝保留计划
- **WHEN** 用户拒绝计划清单
- **THEN** 无任何条目执行，计划内容保留在会话中供继续修改

### Requirement: 跨端审批呈现
计划清单 MUST 在 CLI 与 WebUI 中以结构化卡片呈现（每条目含文件、动作类型、diff 预览），且提供批量批准/拒绝操作。通道（Telegram/QQ/Discord 等）收到计划审批事件时 MUST 以文本摘要 + 审批快捷操作呈现。

WebUI 会话视图 MUST 提供：计划模式开关（plan/execute 切换，切换后状态栏或会话头呈现当前模式）；计划卡片（逐条目标、动作类型、edit/patch 条目的增删行着色 diff 预览、terminal 条目显示命令）；批量批准/拒绝按钮。计划卡片 MUST 随 `plan.item` / `plan.pending` / `plan.approved` / `plan.rejected` / `plan.applied` 事件实时刷新（WS 推送，不靠轮询），批准回放结果 MUST 逐条呈现成功/失败。

#### Scenario: WebUI 计划卡片
- **WHEN** 会话产生待审批计划
- **THEN** WebUI 显示结构化计划卡片，含逐条 diff 预览与批准/拒绝按钮

#### Scenario: WebUI 模式切换
- **WHEN** 用户在会话视图切换到计划模式并让智能体执行编码任务
- **THEN** 智能体的写工具调用被拦截为计划条目，卡片实时累加呈现

#### Scenario: WebUI 批准回放呈现
- **WHEN** 用户点击批准
- **THEN** 卡片切换为回放结果视图（逐条 ok/失败标注），会话切回执行模式

#### Scenario: 通道文本摘要审批
- **WHEN** 计划审批事件到达 Telegram 通道
- **THEN** 通道推送计划文本摘要及批准/拒绝的快捷回复方式
