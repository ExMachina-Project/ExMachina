# chart-rendering Specification

## Purpose
图表渲染（助理面差距矩阵 G2.1）：WebUI 端 mermaid 代码块直接成图；IM 端经 render_chart 工具把 mermaid 渲染为 SVG 图片产物直发——模型输出图表在两端都可视化。

## Requirements

### Requirement: WebUI mermaid 渲染

WebUI 聊天流的 Markdown 渲染 SHALL 识别 `mermaid` 代码块并渲染为图表（引入 mermaid 渲染库，纯前端）；渲染失败（语法错误）MUST 回落原代码块展示并附错误提示，不得空白。渲染 MUST 在深浅主题下均可用（跟随 WebUI 主题变量）。未引入渲染库前模型输出的 mermaid 代码块行为（当代码展示）作为回落路径保留。

#### Scenario: 正常渲染

- **WHEN** 模型输出含合法 mermaid 代码块的回复，WebUI 聊天流收到该消息
- **THEN** 代码块位置渲染为图表，主题配色跟随当前 WebUI 主题

#### Scenario: 语法错误回落

- **WHEN** mermaid 代码块存在语法错误无法成图
- **THEN** 该块回落为代码块展示，附渲染失败原因，不影响消息其余部分

### Requirement: render_chart 工具（IM 端成图）

系统 SHALL 提供模型可调用的 `render_chart` 工具：入参 `mermaid`（必填）与可选 `caption`；经图表渲染服务（Kroki 公共 API，可配自定义端点）把 mermaid 渲染为 SVG，落工作区并按扩展名语义登记产物（SVG 无法内嵌的通道登记 file 走附件语义，产物注记如实说明）。渲染失败 MUST 返回含错误摘要的失败（不登记产物），模型可修正语法后重试。mermaid 源码护栏默认 20000 字符。

- 该工具 MUST 恒可下发（不依赖任何能力槽位——渲染服务为公共端点，`chart.endpoint` 可配覆盖）；配置为空串时工具不下发（离线环境零外呼）。

#### Scenario: IM 直发渲染图

- **WHEN** 模型调用 `render_chart`（合法 mermaid）且通道支持图片直发（png/jpg 转换可用）或产物走附件/注记语义
- **THEN** SVG 落工作区并登记产物，收束时按通道能力投递

#### Scenario: 渲染失败不产半截产物

- **WHEN** mermaid 源码语法错误导致渲染服务返回错误
- **THEN** 工具返回含服务端错误摘要的失败，不登记产物，模型修正后重试成功

### Requirement: 渲染服务配置与离线零外呼

系统 SHALL 支持 `chart.endpoint` 配置项：缺省为 Kroki 公共端点；显式配置为空串时 `render_chart` 工具 MUST NOT 下发（离线部署零意外外呼）。渲染请求 MUST 有超时上限（默认 15s），超时按渲染失败处理。

#### Scenario: 离线环境零外呼

- **WHEN** `chart.endpoint` 配置为空串
- **THEN** render_chart 不在工具表中，系统不对任何图表服务发起请求
