# rss-reader Specification

## Purpose
RSS 阅读插件：订阅源管理、轮询去重、新条目唤醒智能体阅读产出简报、按需拉取工具与通道推送——第二个官方内置插件，复刻 video 的「通道 + watcher + 工具组」三件套验证插件边界可复制。

## Requirements

### Requirement: 订阅源管理

订阅源 MUST 经通道配置面管理（插件声明的通道凭据/配置字段）：feed 列表（名称 + URL）、轮询间隔、推送开关。配置变更 SHALL 即时生效，下一轮轮询按新列表执行。

#### Scenario: 添加订阅源
- **WHEN** 管理员在通道配置面添加一个 feed URL
- **THEN** 下一轮轮询拉取该源，新条目进入阅读流程

#### Scenario: 订阅源配置即时生效
- **WHEN** 修改 feed 列表或轮询间隔
- **THEN** 后续轮询按新配置执行，无需重启宿主

### Requirement: 轮询与新条目去重

watcher MUST 按轮询间隔逐源拉取解析（RSS/Atom XML），并以条目链接为去重键：已见链接 SHALL 不再处理，新链接进入阅读流程并记入已见集合（持久化，重启不重复推送）。

#### Scenario: 新条目触发阅读
- **WHEN** 一轮轮询发现某源存在未见过的新条目
- **THEN** 新条目唤醒智能体会话进入阅读流程，已见条目不重复触发

#### Scenario: 去重持久化
- **WHEN** 宿主重启后再次轮询同一源
- **THEN** 重启前已见条目不再次触发阅读

### Requirement: 新条目阅读产出简报

新条目唤醒智能体会话后，智能体 MUST 阅读条目内容（标题/链接/摘要或正文）并产出简短简报（标题要点/一句话要点或值得注意的理由），简报 SHALL 进入会话流（对话页可见）；阅读产生的记忆遵循既有记忆口径。单轮条目数 MUST 有上限（默认 10，可配），超出部分顺延下一轮。

#### Scenario: 简报产出可见
- **WHEN** 一批新条目唤醒阅读会话
- **THEN** 会话产出含每条要点与亮点的简报，对话页可查看该会话与产出

#### Scenario: 单轮上限顺延
- **WHEN** 单轮新条目数超过上限
- **THEN** 仅前 N 条进入本轮阅读，其余顺延下一轮且不丢失

### Requirement: 按需拉取工具

`rss__digest(source?)` 工具 MUST 提供：缺省返回全部订阅源当前条目摘要（每源最新若干条：标题+链接+一句话），指定 source（feed 名称或 URL）时 SHALL 仅返回该源条目。工具 MUST 仅对被授权个体可见（清单 agents 声明，与既有插件工具口径一致）。

#### Scenario: 全源摘要往返
- **WHEN** 智能体调用 rss__digest 无参
- **THEN** 返回每个订阅源的最新条目摘要列表

#### Scenario: 指定源往返
- **WHEN** 智能体调用 rss__digest 指定某 feed
- **THEN** 仅返回该源的条目摘要；源不存在时返回明确错误

### Requirement: 简报推送通道

阅读简报 MUST 可订阅推送至通道（沿用既有自动化结果订阅推送口径）：会话产出简报后 SHALL 按通道订阅配置推送文本（长简报按平台能力分段），推送失败 MUST 不阻塞阅读流程。

#### Scenario: 简报推送
- **WHEN** 阅读会话产出简报且配置了推送通道
- **THEN** 简报文本按通道订阅推送，失败仅记录不重试阻塞

### Requirement: RSS 插件清单声明

RSS MUST 以 builtin 插件形态注册：清单 SHALL 声明 channel（rss kind，凭据无需敏感字段，配置字段含 feed 列表/间隔/推送开关/单轮上限）+ watch（轮询间隔）+ tools（rss__digest）；WebUI 通道页 MUST 按清单渲染配置表单。

#### Scenario: 清单驱动的配置面
- **WHEN** 管理员在 WebUI 通道页添加 rss 类型通道
- **THEN** 表单按清单声明渲染 feed 列表/间隔/推送开关等字段，保存后进入配置
