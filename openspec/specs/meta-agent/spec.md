# meta-agent Specification

## Purpose
元智能体（Meta Agent）双轨运行：聚合运行数据、经提议-审批-应用管线驱动配置演进，并在轮内做元认知反思，审计全程可观测。

## Requirements

### Requirement: Meta Agent 开关与双轨运行

系统 SHALL 提供 `experiments.metaAgent` 实验性开关（默认 false，关 = 零行为变更）；开启后 Meta Agent SHALL 以定时自主巡检（`automation.metaIntervalMin`，默认 60，0 = 仅会话召唤）与会话召唤双轨运行。

#### Scenario: 默认关闭零回归

- **WHEN** `experiments.metaAgent` 未开启
- **THEN** 无 meta 工具装配、无自主巡检、无元认知钩子，系统行为与既有实现完全一致

#### Scenario: 定时自主巡检产出提议

- **WHEN** metaAgent 开启且距上次巡检 ≥ metaIntervalMin 分钟，Eval 自评判定存在值得改进的点
- **THEN** 系统开启自主会话（装 meta 工具组）执行分析，产出的改进以提议单进入审批闸门

#### Scenario: 会话召唤即时分析

- **WHEN** metaAgent 开启，任一会话中的模型调用 meta.observe / meta.propose / meta.build
- **THEN** 工具正常执行并返回结果，调用全程审计落库

### Requirement: 运行数据聚合

系统 SHALL 提供按 agent 维度的工具调用聚合（次数/失败率/平均耗时/最近失败）与轨迹采样（最近 5 条含工具调用的消息的工具序列），作为 meta.observe 与自主巡检 Eval 的数据面。

#### Scenario: 聚合反映失败模式

- **WHEN** 某 agent 近 7 天内 web_search 工具失败率 42%
- **THEN** meta.observe 输出该 agent 的 tool_stats 含 web_search 的失败率与最近失败摘要

### Requirement: 提议-审批-应用管线

meta.propose / meta.build 产出的改动 MUST 以 `[meta]` 前缀提议单进入人工审批，批准后系统 SHALL 按提议类型代为应用（prompt 写回留锚点与历史备份、adaptation 版本化保存、roster 两阶段校验应用、build 注册新个体/组），拒绝或超时 MUST 受阻回流；同一 target+kind 存在未决提议单时 MUST 拒绝重复落单。

#### Scenario: prompt 调优提议全流程

- **WHEN** kind=prompt 提议（researcher，提示词补充段落）被批准
- **THEN** 该 agent 提示词文件追加带 `<!-- meta: 日期 -->` 锚点的段落，旧版备份至 prompts/history/，经验留痕可回滚

#### Scenario: roster 两阶段与回滚

- **WHEN** kind=roster 提议含 add+remove 两个操作，remove 因目标不存在校验失败
- **THEN** 全量校验阶段即拒绝整单，无任何变更落盘

#### Scenario: 未决单去重

- **WHEN** 对 target=researcher、kind=adaptation 已有 pending 提议单，再次调用 meta.propose 同 target+kind
- **THEN** 工具返回「已有未决提议」错误，不产生新审批单

#### Scenario: 内置组保护不受影响

- **WHEN** 提议目标为内置组（builtin）
- **THEN** 提议在应用阶段被拒绝，内置组保护语义与既有 agent_manage 一致

### Requirement: 轮内元认知反思

metaAgent 开启时，轮内工具循环 SHALL 在连续失败 ≥2 或（步数 ≥8 且实质产出占比 <30%）时触发一次反思（读本轮轨迹输出策略 JSON），策略 SHALL 以系统提示注入后续循环（replan/redirect/compact/continue），每轮反思 MUST 不超过 1 次，反思调用失败 MUST 降级为 continue 不阻塞主流程。

#### Scenario: 连续失败触发反思

- **WHEN** 本轮工具连续失败达到 2 次
- **THEN** 触发一次反思调用，后续循环注入策略调整提示

#### Scenario: 每轮上限一次

- **WHEN** 反思已触发过一次，本轮再次满足阈值
- **THEN** 不再触发第二次反思

### Requirement: 审计可观测

meta 工具调用、巡检 Eval、提议应用、轮内反思 SHALL 分别落审计事件（meta.enabled/meta.eval/meta.applied/meta.reflect），经既有事件通道展示。

#### Scenario: 提议应用可追溯

- **WHEN** 一条 [meta] 提议被批准并应用
- **THEN** 审计流出现 meta.applied 事件（含 kind/target/结果）

### Requirement: 配置与 UI 暴露

`experiments.metaAgent` 与 `automation.metaIntervalMin` SHALL 经既有配置面读写；SettingsView 实验性分组 SHALL 提供开关与周期编辑；审批卡 SHALL 识别 `[meta] ` 前缀以专属样式（紫色 badge + 类型文案）渲染，提议 JSON SHALL 以只读折叠预览展示。

#### Scenario: 审批卡元提议标识

- **WHEN** 审批单 command 以 `[meta] prompt researcher: …` 开头
- **THEN** 审批卡显示「元智能体提议」badge 与「提示词调优」类型文案及只读 JSON 预览
