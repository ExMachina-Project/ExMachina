# assistant-parity Specification

## Purpose

助理面 Top N 补齐（差距矩阵产出）：定时任务时区与推送回投、语音回复产出链、记忆条目编辑——通用助理面高频能力对齐主流。

## Requirements

### Requirement: 定时任务时区解释

系统 SHALL 支持 `automation.timezone` 配置项（IANA 时区名，如 `Asia/Shanghai`）。配置非空时，cron 表达式 MUST 按该时区解释；空串或非法时区名 MUST 回落 UTC，与未引入该配置项前的行为完全一致（零回归）。

`schedule` 工具回显的 `nextRunAt` MUST 按生效时区本地渲染（含时区缩写），不得把 UTC 时间当作本地时间展示。

#### Scenario: 上海八点即 UTC 零点

- **WHEN** `automation.timezone = "Asia/Shanghai"` 且 cron 为 `0 8 * * *`
- **THEN** UTC 00:00 命中该 cron，UTC 08:00 不命中（北京 16 点）

#### Scenario: 缺省与非法值零回归

- **WHEN** `automation.timezone` 为空串或非法名
- **THEN** cron 按 UTC 解释，行为与未配置时完全一致

### Requirement: 定时任务结果推送与回投

`schedule` 工具 create MUST 支持可选的 `notify_channels[]`（任务完成后把结果摘要推送至订阅通道）与 `session_title`（任务结果落库到同名会话，复用已有会话；缺省 `job-<id>`）。两者缺省时 MUST 保持旧行为：不推送、按默认标题落库。

#### Scenario: 带推送与标题创建

- **WHEN** create 带 `notify_channels: ["qq:12345"]` 与 `session_title: "每日报表"`
- **THEN** 任务完成后摘要推送至该通道；执行结果落在标题为「每日报表」的会话（已存在则复用）

#### Scenario: 缺省不推送

- **WHEN** create 未带 `notify_channels` 与 `session_title`
- **THEN** 任务完成后不推送任何通道，结果落在默认 `job-<id>` 会话

### Requirement: 语音回复产出链

系统 SHALL 提供模型可显式调用的 `speech_reply` 工具：文本经语音合成能力槽位（`speech`）生成 mp3，落 `.exmachina/outbox` 并登记为 voice 产物，由收束产物链按通道能力直发或降级为链接说明。

- 未配置语音合成能力时，该工具 MUST NOT 出现在下发给模型的工具表中；
- 文本长度 MUST 受 600 字符护栏约束（超限拒绝并提示拆分）；
- 既有文本回复主形态 MUST 保持不变（不自动 TTS）。

#### Scenario: 显式语音回复

- **WHEN** 模型调用 `speech_reply`（text ≤ 600 字符）且语音槽位已配置
- **THEN** 生成 mp3 落 outbox，`run.finished` 产物清单含 voice 条目，支持语音的通道直发音频

#### Scenario: 未配置语音能力

- **WHEN** `speech` 能力槽位为空
- **THEN** 工具 schema 不含 `speech_reply`，模型无法调用；文本回复不受影响

### Requirement: 记忆条目编辑

系统 SHALL 支持编辑既有记忆条目的 `title` / `body` / `tags` / `kind`（REST `PUT /api/memory/:id` 与 CLI `exm memory edit <id>`）。

- 缺省字段 MUST 不改动；空串 MUST 视为「未提供」（删除条目走 forget/DELETE）；
- 内容（title/body）变更时 MUST 重算内容哈希并迁移去重索引、重建倒排索引；`createdAt` MUST 不变，`updatedAt` MUST 刷新；
- 与其它条目内容哈希相撞 MUST 拒绝（400 / CLI 报错），不得覆盖对方的去重索引；
- 非法 `kind` MUST 拒绝；id 不存在 MUST 返回 404（CLI 报错）；
- 编辑成功后 MUST 同步重渲染 memory.md。

#### Scenario: 编辑后检索命中新内容

- **WHEN** 编辑条目的正文并执行检索
- **THEN** 新正文可被召回，旧正文不再命中该条目；`createdAt` 保持原值

#### Scenario: 哈希迁移

- **WHEN** 条目正文改为新内容后，再次 remember 相同新内容
- **THEN** 命中编辑后的条目（同 id、不新增）；旧内容可重新成为新条目（旧哈希已删除）

#### Scenario: 非法输入

- **WHEN** 提交未知 `kind` 或编辑不存在的 id
- **THEN** 分别返回 400 / 404（CLI 对应报错），条目内容不发生任何变化
