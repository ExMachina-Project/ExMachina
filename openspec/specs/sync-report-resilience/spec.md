# sync-report-resilience Specification

## Purpose
SyncReport 解析容错（生产正确性修复）：LLM 产出的收束报告缺单一可推断字段（title/agentIdentifier 等演进中的字段漂移）时，连续三次契约校验失败直接判死节点，浪费已完成的大量工具工作。本能力让解析层对可推断缺失做一次受控修复重试，把「字段漂移」从节点死刑降级为可恢复抖动，并让契约失败可观测。

## Requirements

### Requirement: 字段缺失受控修复

SyncReport 结构校验失败且失败原因仅为「缺失字段」时，解析层 MUST 尝试修复后重试一次：

- 缺失字段为 `summary` 且 statements 非空 → 以首条 statement 文本截断（≤200 字符）补齐；
- 缺失字段为 `confidence` → 以 0.5 补齐（中性值，指挥体可识别低置信）；
- 缺失字段为演进期的兼容别名（如 `title`、`agentIdentifier`）且 `sourceAgent` / `taskNodeId` 等核心身份字段齐全 → 丢弃未知字段要求，按已解析核心字段接受；
- 修复路径 MUST 在返回的报告上打 `repaired: true` 标记（事件与回流可见）；
- 除上述情形外（核心字段缺失、类型错误、业务约束失败）MUST 维持原行为报错，不做静默修复。

#### Scenario: 缺 summary 修复

- **WHEN** LLM 输出的 SyncReport JSON 缺 `summary` 但 statements 有 2 条
- **THEN** 解析成功，summary 取首条 statement 截断，报告带 `repaired: true`

#### Scenario: 核心字段缺失不修复

- **WHEN** 输出 JSON 缺 `taskNodeId`（节点身份字段）
- **THEN** 校验失败维持原行为（进入既有连续失败计数），不做静默补齐

### Requirement: 契约失败可观测

SyncReport 校验失败进入重派/判死流程时，MUST 产生含失败原因明细（首个失败字段与原文长度）的事件落会话事件流（`report.rejected`），WebUI 会话视图与事件页可查；既有「连续 N 次失败」计数语义不变，但每次失败的原因 MUST 独立可读，不再只暴露一行汇总串。

#### Scenario: 失败原因可查

- **WHEN** 某节点派发连续 3 次 SyncReport 校验失败被判死
- **THEN** 事件流含 3 条 `report.rejected`，各带当次失败字段明细，会话事件页可检索
