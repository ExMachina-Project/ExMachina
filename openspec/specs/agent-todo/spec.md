# agent-todo Specification

## Purpose
TBD - created by archiving change harness-agent-round3. Update Purpose after archive.

## Requirements

### Requirement: Todo 任务清单工具
系统 MUST 提供会话级任务清单工具 `todo`：`op=write` 全量替换清单（条目含内容与状态 pending/in_progress/done），`op=list` 返回当前清单。清单为会话级内存态（不落库、不跨会话）。todo 工具 MUST 为只读语义面（不进审批闸门）。

清单非空时系统提示 MUST 每轮注入当前清单（状态渲染：done ✅ / in_progress 🔄 / pending ⬜），模型无需主动查询即可看到进度全景。清单每次变更 MUST 发出 `todo.updated` 事件进会话事件流（CLI/WebUI 呈现进度）。

#### Scenario: 全量重写清单
- **WHEN** 智能体调用 `todo(op=write, items=[{content:"定位缺陷",status:"done"},{content:"修复",status:"in_progress"}])`
- **THEN** 清单替换为这两条，返回确认；下一轮系统提示含清单渲染

#### Scenario: 查询清单
- **WHEN** 智能体调用 `todo(op=list)`
- **THEN** 返回当前全部条目与状态统计

#### Scenario: 完成态反馈
- **WHEN** 清单全部条目为 done
- **THEN** 系统提示注入「全部完成」渲染，事件流 todo.updated 携带全完标记

#### Scenario: 空清单不注入
- **WHEN** 会话无清单或清单被清空
- **THEN** 系统提示不包含任务清单段

### Requirement: 编码类个体默认下发
todo 工具 MUST 进入编码类个体的默认工具面（coding-agent/orchestrator/frontend-agent 等既有工具白名单含 terminal/edit 的个体），提示词不额外教条（工具 schema 自描述）。

#### Scenario: 编码个体可用
- **WHEN** 检查 coding-agent 个体工具面
- **THEN** 含 todo 工具
