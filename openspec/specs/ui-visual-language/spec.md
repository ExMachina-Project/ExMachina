# ui-visual-language Specification

## Purpose
TBD - created by archiving change ui-brand-alignment. Update Purpose after archive.

## Requirements

### Requirement: 品牌视觉令牌
WebUI 默认主题（midnight）的核心色值 MUST 与 GitHub profile 资产（`ExMachina-Project/.github` profile/assets 五张 SVG）对齐：卡片底 `#0B0D10`、描边 `#20262F`、层叠槽 `#161B22`、次级灰 `#9AA3B0`、弱化灰 `#59626F`、亮白 `#F2F5F9`、品牌紫 `#7C8CFF`。

基调 MUST 为「大片黑底 + 亮白内容」：主文本、标题、统计数字、图表主填充 MUST 使用亮白（`#F2F5F9`），而非中灰或品牌紫；灰阶梯度（`#F2F5F9`→`#AEB7C4`→`#7C8695`→`#59626F`）MUST 以令牌提供，供占比/排序呈现按序分级取色；品牌紫 MUST 保持点缀用量（交互强调/状态光晕），不得用于大面积背景或图表主填充。MUST 提供亮紫变体 `#A9B4FF` 与品牌光晕层（紫 16% 透明）令牌。浅色主题（minimal）与 graphite 主题不受此对齐约束（可访问性备选）。

#### Scenario: 令牌一致性
- **WHEN** 检查 midnight 主题的自定义属性
- **THEN** 上述七色与灰阶梯度、光晕令牌与资产 SVG 中的取值逐一对齐

#### Scenario: 亮白主角
- **WHEN** 渲染消息主文本、区块标题、统计数字与进度条主填充
- **THEN** 呈现为亮白（#F2F5F9 系），非中灰/紫色

#### Scenario: 紫色仅点缀
- **WHEN** 检查主题中品牌紫的出现位置
- **THEN** 仅限交互强调、状态光晕等小面积点缀，无大面积紫色背景/填充

#### Scenario: 备选主题不受影响
- **WHEN** 切换到 minimal 或 graphite 主题
- **THEN** 各自原有色板保持不变

### Requirement: 等宽排版体系
结构性文本（区块标题、数字读数、状态标签、侧栏导航、遥测条）MUST 使用等宽字体族；长文正文（消息段落、输入框）保留无衬线以保证可读性。MUST 提供标签风格：全大写 + 宽字距（letter-spacing ≥ .08em）+ 小号（≤12px）等宽。区块标题 MUST 支持 `// ` 前缀注释风装饰类。

#### Scenario: 结构文本等宽
- **WHEN** 检查侧栏导航、区块标题与统计数字的字体
- **THEN** 全部解析为等宽字体族

#### Scenario: 大写标签
- **WHEN** 渲染状态/区块小标签
- **THEN** 呈现为全大写、宽字距的等宽小号文本

### Requirement: 状态点与图表语汇
状态指示点 MUST 采用双层圈语汇（外层品牌色 16% 透明光晕 + 内层实心）；进度条与占比图 MUST 为直角矩形 + 层叠槽色分层：槽用层叠槽色、**主填充用亮白**（灰阶梯度按序降级），无圆角。卡片容器 MUST 使用 1px 描边（主题 line 令牌）。

#### Scenario: 双层状态点
- **WHEN** 渲染运行中/在线状态指示
- **THEN** 出现光晕外圈与实心内圈的同心双层结构

#### Scenario: 直角图表
- **WHEN** 渲染进度条或占比条
- **THEN** 矩形直角，亮白填充叠于槽色之上

### Requirement: 大数字读数
统计数字（计数、耗时、容量类读数）MUST 使用等宽大号**亮白**字重呈现（tabular-nums），配灰色全大写小标签（如 `24 / TASKS`）。

#### Scenario: 读数排版
- **WHEN** 呈现统计卡或遥测数字
- **THEN** 数字等宽大号亮白，标签大写小号等宽灰
