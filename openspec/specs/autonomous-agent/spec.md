# autonomous-agent Specification

## Purpose
TBD - created by archiving change long-horizon-iteration. Update Purpose after archive.

## Requirements

### Requirement: 实验能力默认关闭
方向 H 全部实验能力（自主记忆摘要/自主行动/Super Agent 委托）SHALL 默认关闭（`config.labs` 显式开启）；关闭态 MUST 零任务调度、零 token 消耗、零 UI 入口开销。

#### Scenario: 默认零开销
- **WHEN** 网关以默认配置启动
- **THEN** 不产生任何实验性定时任务与相关 token 消耗

### Requirement: 自主记忆摘要（labs）
开启后低峰期（cron 04:30）SHALL 将会话超过 N 轮的长历史归纳为记忆条目（含 token 预算与去重）；归纳产物落普通记忆库可审计。

#### Scenario: 低峰归纳
- **WHEN** 实验开启且某会话历史超阈值
- **THEN** 低峰期产出归纳记忆条目（去重后），连续观察期记忆质量与 token 消耗可查

### Requirement: 自主行动（labs）
开启后定时器触发 Eval 自评（「有没有值得做的事」），自评通过才自主开会话执行；自主会话落普通会话（可审计）；连续无任务 SHALL 退避。

#### Scenario: Eval 门控
- **WHEN** 定时自评结论为无需行动
- **THEN** 不创建会话；连续无任务后退避间隔拉长

### Requirement: Super Agent 深度委托（labs）
开启后会话可进入「给目标静默推进」模式（工具循环 + spawn 并行子代理）；停机条件三态（交付/决策/无法继续）SHALL 显式呈现；全过程可审计。

#### Scenario: 停机三态
- **WHEN** 深度委托任务达到停机条件
- **THEN** 会话呈现三态之一（交付物/待决策点/无法继续的原因），不无限循环
