# compaction-tuning Specification

## Purpose

压缩调优：上下文压缩触发从「硬编码条数阈值」升级为「token 预算占比驱动」——长会话压缩时机与模型窗口对齐。

## Requirements

### Requirement: token 预算触发

会话历史估算 token 用量（沿既有估算口径）超过「上下文预算 × `compaction.triggerRatio`（默认 0.6，可配 0.1–0.95）」时 MUST 触发滚动压缩——早于预算硬顶（contextBudgetRatio 截断）介入，让压缩先于截断发生。

#### Scenario: 超占比触发
- **WHEN** 会话历史估算 token 达预算的 60%（triggerRatio 默认值）且尚未压缩
- **THEN** 触发滚动压缩（旧段落折叠为滚动摘要），`context.compact` 事件留痕含触发原因 `token-ratio`

#### Scenario: 未达占比不触发
- **WHEN** 估算用量低于占比阈值
- **THEN** 不因 token 触发压缩（条数兜底阈值仍独立生效）

### Requirement: 条数兜底保留

既有「历史条数阈值」MUST 保留为兜底：条数超限仍触发压缩（防估算器系统性低估）；两触发条件 MUST 为「或」关系，任一满足即压缩。

#### Scenario: 条数先到
- **WHEN** 条数超既有阈值但估算 token 仍低于占比
- **THEN** 仍触发压缩，事件触发原因标注 `count-fallback`

### Requirement: 配置与事件

`compaction.triggerRatio` 配置项（settings 页可见，0 = 关闭 token 触发仅条数兜底）SHALL 生效；每次压缩事件（既有 `context.compact`/`context.trim` 族）MUST 携带触发原因与压缩前后估算用量。

#### Scenario: 配置关闭
- **WHEN** `triggerRatio: 0`
- **THEN** 压缩仅由条数兜底触发，行为与现状一致

#### Scenario: 用量可观测
- **WHEN** 任一原因触发压缩
- **THEN** 事件载荷含 `trigger`（token-ratio / count-fallback）、`beforeTokens`、`afterTokens` 估算值
