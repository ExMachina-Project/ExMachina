# agent-budget-audit Specification

## Purpose
TBD - created by archiving change long-horizon-iteration. Update Purpose after archive.

## Requirements

### Requirement: 工具调用预算
工具网关 SHALL 按 ToolCallBudget（perTurn/perSession/perWindow）与通道档位表（chat=1/4、tools=1/2、computer=全量）对工具调用熔断；超限 MUST 回灌受限 ToolResult（模型可收束）并发 `tool.budget` 事件；预算 SHALL 在设置页可配。

#### Scenario: 单轮超限
- **WHEN** 会话单轮工具调用数超过 perTurn 预算
- **THEN** 后续调用被拒绝并回灌受限结果（含剩余额度说明），`tool.budget` 事件广播且事件页可见

### Requirement: 终端命令三档分档
终端命令 SHALL 按 Safe（只读探测）/ Controlled（白名单受控）/ Denied（破坏性清单）三档归类；分档 × 通道 capability × 审批档联动判定；Denied 档命令任何审批档位 MUST NOT 白名单直通（off 档也须落审批单）。

#### Scenario: 同命令异档
- **WHEN** 破坏性命令（如 rm）在 computer+always 通道发起
- **THEN** 进入人工审批流程；同一命令在 chat 档被直接拒绝并说明能力边界

### Requirement: 子代理派发审计
每次子代理派发 SHALL 记录审计条目（派发者会话/目标个体/任务摘要/耗时/token 消耗/结果状态）；失败/受阻条目 MUST 可在 WebUI 审计视图高亮检索。

#### Scenario: 派发失败审计
- **WHEN** 子代理节点执行失败
- **THEN** 派发审计记录状态 Failed 与错误信息，审计页红标呈现

### Requirement: 审计载荷脱敏
工具调用事件与审计记录中的密钥类参数（敏感键名 / sk- 前缀 / Bearer / PAT / AWS key / 长 hex-base64 形态）MUST 在广播与落库前脱敏为掩码。

#### Scenario: 密钥参数脱敏
- **WHEN** 工具调用参数含 `api_key: sk-xxxx` 或命令内嵌 Bearer token
- **THEN** 事件与审计载荷中该值呈掩码（***），原文不进任何明文面
