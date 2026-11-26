# inbound-sanitization Specification

## Purpose
TBD - created by archiving change long-horizon-iteration. Update Purpose after archive.

## Requirements

### Requirement: 入站内容消毒
通道入站文本 SHALL 经消毒管道（`sanitize`）：伪装系统指令模式检测与中性化、零宽/双向 Unicode 剥离、工具结果伪装标记；Findings 仅审计不阻断（放行文本必经消毒）。

#### Scenario: 注入模式中性化
- **WHEN** 入站消息含「忽略以上指令」类伪装系统指令或零宽字符注入
- **THEN** 放行文本已中性化（剥隐藏字符/标注已中性化前缀），且产生结构化审计事件（通道/账号/模式类型，不含原文全文）

#### Scenario: 正常技术讨论不误杀
- **WHEN** 正常技术讨论包含敏感关键词（如讨论注入攻击的防御文章）
- **THEN** 消毒不阻断、不破坏正常语义

### Requirement: 外部内容隔离包装
fetch/RSS/视频描述类外部内容进入上下文前 SHALL 统一隔离包装（来源标注 + 数据边界围栏 + 单源长度上限截断标注）；包装内容作为数据传递不执行。

#### Scenario: 超长外部内容截断
- **WHEN** 工具拉取的外部内容超过单源长度上限
- **THEN** 截断并带标注进入上下文，注入样本文本仅作数据传递

### Requirement: 配对码防爆破
配对码验证 SHALL 限速：`pair:{channel}:{external_id}` 键失败计数 + 锁定窗口（默认 5 次/15 分钟，config 可调）；锁定产生审计事件。

#### Scenario: 连续错误锁定
- **WHEN** 同一账号连续输错配对码达上限
- **THEN** 锁定窗口内正确码也拒绝；窗口过期后可重试；锁定发审计事件；不同账号互不影响

### Requirement: 系统提示与用户内容边界
会话历史装配 SHALL 保持 User/Assistant/System 角色显式分隔（结构化边界，抵御跨轮注入）；消毒标记透传装配层。

#### Scenario: 角色边界不可覆盖
- **WHEN** 用户内容中包含伪造的 System 角色文本
- **THEN** 装配后仍归 User 角色，系统提示不被覆盖
