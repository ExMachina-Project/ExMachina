# video-interact Specification

## Purpose
互动执行面：智能体经 `video__act` 工具发表评论/回复——系统侧硬闸门（指纹养护/配额/清洗/审批）保证账号安全，dry-run 语义贯穿。

## Requirements

### Requirement: 互动动作工具

`video__act(videoId, action)`：action 为评论（text）或回复（text + 目标评论 id，目标必须来自本会话已见评论）。工具返回执行结果（成功回执或结构化拒绝原因）。动作是否允许由通道配置面（comment/reply 开关）与闸门共同决定。

#### Scenario: 发表评论往返
- **WHEN** 智能体产出合规评论并调用 act
- **THEN** 评论发送至平台，工具返回成功回执；审计账本记录该动作（来源 plugin:video）

#### Scenario: 回复目标必须真实见过
- **WHEN** 智能体回复的目标 id 不在本会话已获取的评论集合内
- **THEN** 动作被拒绝并提示「目标评论未见」，防止编造 id

### Requirement: 系统硬闸门

闸门在工具执行层强制，智能体无法绕过：字数上限清洗（超长截断）、每日互动配额（达限拒绝并告知）、B站 buvid 指纹养护（互动前自动补齐）、内容清洗规则（去 markdown、去指令痕迹）沿用既有口径。

#### Scenario: 配额耗尽被拒
- **WHEN** 当日互动数已达配置上限且智能体再次调用 act
- **THEN** 动作被拒绝并返回明确原因，轮次继续（智能体可仅产出观感）

### Requirement: 审批模式

通道可配「评论发出前需人工审批」（approval: off/always，默认沿用全局审批口径或 dry-run）。开启时 act 产生审批单（既有审批流），批准后由系统代发；拒绝则记录跳过。dry-run 开启时 act 不落平台，只记录拟发内容到状态面板。

#### Scenario: 审批模式发出
- **WHEN** 通道配置 approval=always 且智能体调用 act
- **THEN** 生成审批单含拟发文本，批准后代发并回执，拒绝则返回跳过结果

#### Scenario: dry-run 不落平台
- **WHEN** dry-run 开启（默认）且 act 被调用
- **THEN** 平台无任何写入，状态面板展示拟发内容与目标

### Requirement: 旧单轮管道移除

系统拼装材料 + 单轮 JSON 动作协议（build_prompt/parse_action）移除；互动产出只能经 act 工具发生。跳过视频时智能体在会话内说明理由（状态面板记录），无需旧 JSON 契约。

#### Scenario: 不再有单轮动作协议
- **WHEN** 观看轮次结束
- **THEN** 互动只能追溯至本会话内的 act 工具调用（审计可查）；无旧协议残留路径
