# headless-print Specification

## Purpose

无头单轮调用（headless print）：`exm` 以非交互、机器可消费形态执行单轮任务——脚本、CI 与外部编排的稳定入口。

## Requirements

### Requirement: 无头模式入口

`-p/--print` 进入无头模式：MUST 不进入任何交互 UI（TUI/panel/readline），执行完单轮即退出。

#### Scenario: 显式文本参数
- **WHEN** 运行 `exm -p "任务描述"` 且工作区配置可用
- **THEN** 不启动交互界面，任务单轮执行完毕后进程退出，标准输出为收束正文（纯文本，无装饰横幅、无控制序列）

#### Scenario: 无参数时读 stdin
- **WHEN** 运行 `exm -p` 且 stdin 为管道或重定向（非 TTY）
- **THEN** 读全部 stdin 内容（去除首尾空白）作为任务文本；空输入时报错退出（非零退出码），不发起 LLM 调用

#### Scenario: TTY 误用防护
- **WHEN** 运行 `exm -p` 且 stdin 为 TTY、无文本参数
- **THEN** 报错提示「-p 需要文本参数或管道输入」并以非零退出码退出，不挂起等待输入

### Requirement: 输出格式

`--output-format text|json|stream-json` 控制无头模式输出（缺省 `text`）。各格式 MUST 满足下述可解析契约。

#### Scenario: text（缺省）
- **WHEN** 未指定 `--output-format` 或指定 `text`
- **THEN** 标准输出仅含收束正文；错误信息走标准错误，退出码非零

#### Scenario: json
- **WHEN** 指定 `--output-format json`
- **THEN** 标准输出为单行合法 JSON 对象，含 `sessionId`、`text`（收束正文）、`turn`（轮次号）、`usage`（token 用量，可得时）；失败时 JSON 含 `error` 字段，退出码非零

#### Scenario: stream-json
- **WHEN** 指定 `--output-format stream-json`
- **THEN** 标准输出为 JSONL（每行一个 JSON 对象），按发生顺序发射 `turn.start` / `tool.call` / `tool.result` / `token`（可聚合）/ `turn.end` 类事件；进程结束时最后一条为终态事件（含收束正文或 error）

### Requirement: 退出码契约

无头模式退出码 MUST 可被脚本稳定消费：执行完成 0；使用错误（参数/无匹配会话/空输入）2；执行失败（LLM/审批拒绝/运行错误）1。错误输出通道与格式 MUST 与所选输出格式一致。

#### Scenario: 成功
- **WHEN** 单轮执行完成（无论模型输出内容如何）
- **THEN** 退出码 0

#### Scenario: 失败
- **WHEN** 配置缺失、LLM 调用失败、审批最终被拒或执行出错
- **THEN** 退出码非 0；text 模式错误走标准错误，json/stream-json 模式终态事件含 `error` 字段

### Requirement: 会话参数正交

无头模式与会话参数 MUST 可自由组合：`--session <id>` 指定会话、`-c` 接续最近会话、`-r <匹配>` 按名找会话（见 session-resume capability）；三者同时给出时按显式 `--session` > `-r` > `-c` 优先序取一。

#### Scenario: 无会话参数
- **WHEN** `exm -p "..."` 未带任何会话参数
- **THEN** 新建会话执行；`--output-format json` 时输出含新 `sessionId`，供后续 `-r`/`--session` 复用

#### Scenario: 会话接续
- **WHEN** `exm -p "继续" -c` 且存在历史会话
- **THEN** 在最近活跃会话内追加执行，上下文延续
