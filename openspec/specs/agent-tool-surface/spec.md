# agent-tool-surface Specification

## Purpose
个体工具面声明：编成 JSON 支持按个体裁剪工具清单（覆盖默认全量下发），派发时按交集裁剪、审计可查生效面，让不同角色的智能体各只见其该用的工具。

## Requirements

### Requirement: 个体工具面声明

编成 JSON（agent 定义）SHALL 支持可选的 `tools` 清单字段：声明时覆盖该个体的默认全量工具面（白名单语义），未声明时沿用全量。派发（schema 下发与调用判定）MUST 按该清单裁剪——声明外的工具对会话不可达（调用返回明确错误而非静默执行）。工具名口径 MUST 与审计账本一致（含插件命名空间前缀）。

#### Scenario: 裁剪后的工具不可达

- **WHEN** 某个体声明 `tools: ["filesystem.read", "filesystem.write"]` 后会话尝试调用 `exec.run`

- **THEN** schema 下发只含声明的两个工具；`exec.run` 调用返回「工具不在该个体工具面」明确错误

#### Scenario: 未声明保持全量

- **WHEN** 个体未声明 tools 字段
- **THEN** 该会话工具面与现状完全一致（零回归）

### Requirement: 生效面可观测

个体生效工具面 SHALL 可查询：管理面（个体详情）MUST 展示当前生效工具清单（声明裁剪后的结果），审计条目 MUST 能归因到个体（既有 agentId 口径）。配置变更后的工具面 MUST 在新会话轮次生效（不要求在飞轮次热切换）。

#### Scenario: 管理面查看生效面

- **WHEN** 个体声明裁剪清单后打开个体详情

- **THEN** 详情页显示裁剪后的生效工具清单与声明来源标注
