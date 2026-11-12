# search-aggregation Specification

## Purpose

搜索聚合：web_search 工具多后端并发聚合与正文抓取增强——「助理面」检索质量对齐主流。

## Requirements

### Requirement: 多后端聚合

配置多个可用搜索后端（tavily/brave/searxng/custom 中 ≥2 个凭据齐全）时，单次 web_search MUST 并发查询全部已配置后端，合并去重（按 URL 规范化）后按「后端权重 + 相关性排序」返回。

#### Scenario: 双后端聚合
- **WHEN** tavily 与 brave 均已配置且可用，模型调用 web_search
- **THEN** 两后端并发查询，结果合并：同 URL（规范化后）只保留一条（来源并注），排序综合两源；总条数不超过 `maxResults` 上限

#### Scenario: 单后端降级
- **WHEN** 仅一个后端可用（或其余后端查询失败/超时）
- **THEN** 返回可用后端的结果，行为与现状单后端一致；失败后端不影响整体成功（单后端也失败时工具报错）

#### Scenario: 后端超时护栏
- **WHEN** 某后端超过其超时上限（默认 10s）
- **THEN** 该后端结果丢弃，其余后端正常合并；不因单后端悬挂阻塞整体

### Requirement: 正文抓取增强

`include_content` 参数为 true 时，MUST 对排名前 N（默认 3，上限 5）的结果并发抓取正文，提取与查询词相关的高亮段落（复用既有正文提取器），随结果返回 `content` 字段（截断上限默认 1000 字符，标注截断）；抓取失败的结果 `content` MUST 留空，不影响其余结果。

#### Scenario: 带正文返回
- **WHEN** `include_content: true` 且抓取成功
- **THEN** 前 N 条结果各含 `content` 字段（查询相关段落，含来源标注与截断说明）

#### Scenario: 抓取失败容错
- **WHEN** 某结果页抓取失败或超时（默认 8s/页）
- **THEN** 该条 `content` 为空，工具整体仍成功返回其余结果

### Requirement: 结果结构兼容

结果对象 MUST 保留既有全部字段（title/url/snippet 等），新增字段（content、aggregatedFrom 等）MUST 均为可选追加——旧消费方（提示词注入格式、既有测试）不受影响。

#### Scenario: 旧格式兼容
- **WHEN** 模型不带 `include_content` 调用（或 false）
- **THEN** 返回结构与现状一致（无 content 字段或为空），提示词注入格式不变

### Requirement: 配置面

搜索配置 SHALL 支持多后端声明（数组形态，旧单后端形态向后兼容自动迁移）；未配置任何后端时 web_search 工具 MUST 不下发（现状语义不变）。

#### Scenario: 旧配置兼容
- **WHEN** 配置文件仍是旧单后端形态（search.provider/apiKey 平铺）
- **THEN** 启动后等效为单后端数组配置，行为与升级前一致

#### Scenario: 零配置不下发
- **WHEN** 未配置任何搜索后端
- **THEN** web_search 工具不出现在模型工具面（不承诺不存在的能力，现状保持）
