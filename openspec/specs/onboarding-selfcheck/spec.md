# onboarding-selfcheck Specification

## Purpose
开箱自检与空态引导：首次部署后的分级体检（LLM / 通道 / 存储 / 版本）与 WebUI 空态分步指引，让没有文档背景的新用户能自助判断「还差什么才能用」并按指引补齐。

## Requirements

### Requirement: 首次运行体检

系统 SHALL 提供开箱自检（`exm doctor` 与 `GET /api/selfcheck` 同一判定内核）：对 LLM 配置（密钥存在性 / 端点可达）、通道配置（已启用通道的连接态）、存储（数据目录可写 / 磁盘余量）、版本（当前版本与上游可更新性）四类做分级体检，输出 ok / warn / error 三级结论与每项的可读说明。体检 MUST 只读不写（不产生任何副作用）。

#### Scenario: 全配齐体检通过

- **WHEN** LLM 密钥已配、通道在线、存储可写时运行自检
- **THEN** 四类结论均为 ok，输出无 error 项

#### Scenario: 缺密钥给出指引

- **WHEN** LLM 密钥未配置时运行自检
- **THEN** LLM 项为 error 并附「到设置页配置密钥」的可操作指引文案

### Requirement: WebUI 空态引导

WebUI SHALL 在关键空态提供分步指引：无 LLM 密钥时的首页引导卡（指引入口与最小配置示例）、无通道时的通道页空态（说明可先在控制台会话使用、通道接入后自动激活）、无会话时的聊天页空态（首条消息示例）。引导卡 MUST 可关闭且关闭态持久（不反复弹出）。

#### Scenario: 无密钥引导

- **WHEN** 未配置任何 LLM 密钥的用户首次打开 WebUI
- **THEN** 首页显示「配置模型密钥」引导卡含入口按钮；配置完成并刷新后引导卡不再出现

#### Scenario: 引导可关闭

- **WHEN** 用户点击引导卡的「不再提示」
- **THEN** 引导卡关闭且刷新 / 重登后保持关闭

### Requirement: 部署文档落地路径校验

部署文档（deploy/README）SHALL 与实际部署路径一致：镜像模式部署步骤、环境变量清单、更新方式（含本地构建回退路径）三项 MUST 与仓库实际行为吻合；文档校验随自检产出（doctor 输出文档版本指向）。

#### Scenario: 文档步骤可复现

- **WHEN** 新用户按 deploy/README 从零执行到服务可访问
- **THEN** 各步骤命令可原样执行成功，无文档与实际行为的断裂点
