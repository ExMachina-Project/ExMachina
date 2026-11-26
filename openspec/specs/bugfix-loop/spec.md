# bugfix-loop Specification

## Purpose
TBD - created by archiving change long-horizon-iteration. Update Purpose after archive.

## Requirements

### Requirement: P1 缺陷闭环
审计产出的 P1 缺陷（错误处理/REST 契约/竞态）SHALL 逐项修复并附回归断言（单测或 e2e）；P2 项入滚动区按批次消化。完成标准 = P1 清零 + 全量回归绿。

#### Scenario: 审批并发互斥
- **WHEN** 两个并发决定（双击/多端）命中同一审批单
- **THEN** 命令只执行一次；后到者返回先到者的执行结果与时间戳

#### Scenario: 回帖循环容错
- **WHEN** 通道回帖任务的事件订阅发生 broadcast Lagged
- **THEN** 收流循环继续（新事件仍可达），回复不静默丢失

#### Scenario: cron 持久失败不杀调度
- **WHEN** 定时任务会话创建失败（磁盘满/库忙）
- **THEN** 记录 failed 运行与错误事件后调度循环继续，不 panic

### Requirement: REST 契约统一
路径资源不存在 SHALL 返回 404；DELETE 目标不存在 SHALL 按 D4 决议返回 200 幂等；状态冲突返回 409。口径写入 `docs/协议与契约.md`。

#### Scenario: 不存在会话改名
- **WHEN** PUT /api/sessions/:id/title 目标会话不存在
- **THEN** 返回 404（而非 500/400）

#### Scenario: 重复删除通道
- **WHEN** 对已删除通道再次 DELETE
- **THEN** 返回 200 {"ok":true}（幂等）
