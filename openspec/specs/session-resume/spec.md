# session-resume Specification

## Purpose

会话可恢复：跨 CLI 调用快速回到既有会话——`-c` 接续最近、`-r` 按匹配找会话，三种交互形态（TUI/panel/无头）语义一致。

## Requirements

### Requirement: 接续最近会话

`-c/--continue` MUST 让本轮落在「最近活跃的会话」（全工作区按最后活动时间倒序第一，排除归档）。

#### Scenario: 有历史会话
- **WHEN** 存在未归档的历史会话，运行 `exm -c`（或 `exm -p "..." -c`）
- **THEN** 打开最近活跃会话接续（TUI/panel 载入其历史；无头在其内追加执行），不新建会话

#### Scenario: 无历史会话
- **WHEN** 工作区无任何未归档会话，运行 `exm -c`
- **THEN** 行为与不带 `-c` 一致：新建会话，且以提示性输出说明（无头模式 json 输出仍含新 `sessionId`）

### Requirement: 按匹配恢复会话

`-r/--resume <匹配串>` MUST 在会话标题与 ID 中做前缀/子串匹配（大小写不敏感，排除归档），命中唯一即恢复；命中多条 MUST 报错并列出候选（最多 10 条，含标题短码与相对时间）；零命中 MUST 报错。

#### Scenario: 唯一命中
- **WHEN** `exm -r "调试"` 且恰有一个标题含「调试」的未归档会话
- **THEN** 打开该会话接续

#### Scenario: 多命中
- **WHEN** `exm -r "调试"` 命中 3 个会话
- **THEN** 不打开任何会话，错误信息列出候选清单（标题 + ID 前 8 位 + 相对时间），退出码非零（无头模式）；交互模式提示用户改用更精确匹配或 `--session <id>`

#### Scenario: 零命中
- **WHEN** `exm -r "不存在的标题"` 无匹配
- **THEN** 报错「无匹配会话」，退出码非零（无头模式）

### Requirement: 参数优先序与共存

`--session <id>`、`-r <匹配>`、`-c` 同时出现时 MUST 按 **显式 ID > 按匹配恢复 > 接续最近** 取一，其余忽略（不报错）；三者在 TUI/panel/无头三形态下 MUST 行为一致。

#### Scenario: 显式 ID 优先
- **WHEN** `exm -p "..." --session abc -c`
- **THEN** 使用会话 `abc`，`-c` 被忽略

#### Scenario: 匹配优先于接续
- **WHEN** `exm -p "..." -r "调试" -c`
- **THEN** 执行 `-r` 匹配逻辑，`-c` 被忽略
