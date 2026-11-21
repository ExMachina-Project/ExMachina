# deep-research Specification

## Purpose
深度研究编排（助理面差距矩阵 G3.1–G3.3）：把「规划 → 并行检索 → 引用聚合 → 带引用报告」封装为单工具长任务，对齐 GPT-Researcher / 商业 Deep Research 四件套，复用既有 spawn 并行与聚合搜索底座。

## Requirements

### Requirement: research_report 工具

系统 SHALL 提供模型可调用的 `research_report` 工具：入参 `topic`（必填）与 `depth`（可选：quick/standard/deep，缺省 standard，控制并行检索扇出与轮次上限）。工具 MUST 编排完整研究回路——生成研究计划（拆解子问题）→ 经 spawn 并行派发检索子任务（每子问题一个，复用 web_search + web_fetch 只读白名单）→ 回流压缩聚合 → 产出结构化 Markdown 报告落工作区 `.exmachina/outbox/` 并登记 file 产物。

- 搜索后端未配置任何可用后端时，该工具 MUST 返回明确的不可用错误（而非空报告）；
- 报告 MUST 含四节：结论摘要、分主题详述、内联引用标记、来源清单；
- 工具执行进度 MUST 以事件进入会话事件流（阶段切换可观测），WebUI/CLI 呈现研究进行态。

#### Scenario: 标准深度研究

- **WHEN** 模型调用 `research_report`（topic 合法、depth 缺省）且搜索后端可用
- **THEN** 事件流出现阶段切换事件，最终产出含四节的 Markdown 报告文件并登记 file 产物，通道按能力直发或降级注记

#### Scenario: 无搜索后端

- **WHEN** 搜索后端均未配置或全部不可用
- **THEN** 工具返回「检索后端不可用」错误，不产出空报告

### Requirement: 引用管理

报告正文中的事实性论断 MUST 带内联引用标记 `[n]`，`n` 对应报告尾部来源清单条目；来源清单每条 MUST 含标题、URL 与访问日期。同一 URL 多次引用 MUST 合并为同一编号；无法溯源到具体来源的内容 MUST 标注为综合推断，不得编造引用编号。

#### Scenario: 内联引用与来源清单对应

- **WHEN** 报告正文出现 `[3]` 标记
- **THEN** 来源清单第 3 条存在且 URL 可回溯到某次实际检索结果

#### Scenario: 引用合并

- **WHEN** 多个子任务回流引用同一 URL
- **THEN** 来源清单中该 URL 只出现一次，正文引用统一使用同一编号

### Requirement: 报告产物与导出

报告 MUST 同时具备：工作区文件（可经 send_file 投递与 WebUI 下载）与会话内摘要（结论摘要节随收束 statements 回给用户）。报告文件名 MUST 含主题slug与生成日期，便于多次研究产物并存不覆盖。

#### Scenario: 多次研究不互相覆盖

- **WHEN** 同一会话先后执行两次不同主题的 research_report
- **THEN** 生成两个独立报告文件，均在产物清单中，互不覆盖
