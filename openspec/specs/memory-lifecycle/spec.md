# memory-lifecycle Specification

## Purpose
记忆生命周期治理收尾（助理面差距矩阵 G5.2/G5.3）：自动衰减不再依赖手动触发；全量导出支持备份与迁移。条目编辑已由 assistant-parity 覆盖，本能力只收衰减自动化与导出两格。

## Requirements

### Requirement: 每日自动衰减

系统 SHALL 支持 `memory.autoDecay` 配置项（默认 true）：开启时调度器每日低峰窗口（与既有自主记忆摘要同窗口机制）执行一次全量记忆衰减（复用既有 decay 语义与半衰期配置）；关闭时不注册任务（零任务零开销）。衰减执行 MUST 产生可观测事件（执行条数落会话无关的全局事件流），手动 `POST /api/memory/decay` 端点 MUST 保留（二者语义一致，可并存）。

- 自动衰减 MUST 在低峰窗口内完成且当日只执行一次（与既有 cron 表标记行去重同机制）；
- 衰减只调整 importance 数值，MUST NOT 删除任何条目。

#### Scenario: 开启后每日自动执行

- **WHEN** `memory.autoDecay = true` 且调度器进入低峰窗口
- **THEN** 执行一次全量衰减，事件流记录执行条数，当日后续窗口不重复执行

#### Scenario: 关闭零行为

- **WHEN** `memory.autoDecay = false`（或未配置字段缺省为 true 前的旧实例）
- **THEN** 不注册衰减任务，行为与未引入该配置前一致（手动端点仍可用）

#### Scenario: 衰减不删除

- **WHEN** 某条目 importance 衰减到低于 floor
- **THEN** 条目仍存在（importance 钳到 floor），不触发任何删除

### Requirement: 记忆全量导出

系统 SHALL 提供 `GET /api/memory/export`：以 JSONL（每行一条完整记忆条目 JSON）流式返回全量条目，字段与 REST list 单条完全一致。MUST 支持按 agent 过滤（可选 `?agent=`）。CLI `exm memory export [file]` 同语义（落盘）。导出 MUST NOT 包含派生索引数据（倒排/哈希索引），仅条目本体。

- 导出条目数上限沿用管理面清单上限语义（全量 = 当前库内全部实体条目）；
- 响应 `Content-Disposition` MUST 带文件名（`memory-export-<日期>.jsonl`）便于直接下载归档。

#### Scenario: 全量导出可回读

- **WHEN** 库内有 N 条记忆，请求 `GET /api/memory/export`
- **THEN** 响应为 N 行 JSONL，每行反序列化后字段与 GET /api/memory 单条一致

#### Scenario: 按 agent 过滤导出

- **WHEN** 请求带 `?agent=machina`
- **THEN** 仅返回归属该智能体的条目，行数与按同条件 list 一致
