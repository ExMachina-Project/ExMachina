# session-search Specification

## Purpose
让用户按标题与历史消息内容找到会话，并能从搜索结果直接定位到命中的消息；覆盖会话管理页检索与全局速查两种入口。

## Requirements

### Requirement: 会话检索端点

系统 SHALL 提供 `GET /api/sessions/search?q=<关键词>&limit=<条数>`：在**未归档**会话的标题与最近 200 条消息正文中做大小写不敏感的子串匹配，返回命中会话及其片段。`q` 去除首尾空白后为空时 MUST 返回 400；`limit` 缺省 20，超出 1~50 时 MUST 钳制到该区间。

#### Scenario: 标题命中
- **WHEN** 请求 `q=e2e` 且某会话标题包含 `e2e`
- **THEN** 响应 `hits` 包含该会话，且 `titleMatch=true`

#### Scenario: 消息正文命中
- **WHEN** 某会话最近 200 条消息正文包含关键词（与大小写无关）
- **THEN** 该会话出现在 `hits` 中，`messageMatches>=1` 且 `snippets` 含命中消息

#### Scenario: 空查询拒绝
- **WHEN** 请求 `q=` 或 `q=%20%20`
- **THEN** 返回 400，错误信息说明关键词不能为空

#### Scenario: 归档会话不参与检索
- **WHEN** 关键词只出现在一个已归档会话中
- **THEN** 该会话 MUST NOT 出现在 `hits` 中

#### Scenario: 结果上限钳制
- **WHEN** 请求 `limit=0` 或 `limit=999`
- **THEN** 实际返回条数分别不超过 1 与 50

### Requirement: 检索结果结构与排序

每个命中项 SHALL 包含 `sessionId`、`title`、`titleMatch`、`messageMatches`、`snippets`（至多 2 条）、`lastActiveAt`；每条片段 SHALL 包含 `messageId`、`role`、`text`、`createdAt`，消息来源为群聊时 SHALL 包含 `agentId`。标题命中的会话 MUST 排在仅正文命中的会话之前；同组内按会话活跃时间倒序。片段正文 MUST 截断到约 80 字符且保留关键词本身。

#### Scenario: 标题优先
- **WHEN** 一个会话仅标题命中、另一个仅正文命中
- **THEN** 标题命中的结果排在前面

#### Scenario: 同一会话去重
- **WHEN** 同一会话有多条消息命中
- **THEN** 该会话只出现一次，`messageMatches` 为命中条数，`snippets` 至多 2 条

#### Scenario: 片段含关键词
- **WHEN** 关键词位于长消息中部
- **THEN** 返回片段的可见文本包含关键词且总长约 80 字符

### Requirement: 检索范围有界

检索 SHALL 只扫描未归档会话（最多 200 个）各自最近 200 条消息，且 MUST NOT 产生任何写入、归档或分页副作用；在基准数据集（200 会话 × 200 条消息）上单次请求 SHOULD 在 1 秒内完成。

#### Scenario: 只读检索
- **WHEN** 连续调用检索端点两次
- **THEN** 会话、消息、归档状态均不发生变化

#### Scenario: 超范围内容不命中
- **WHEN** 关键词只存在于某会话 201 条之前的消息中
- **THEN** 该会话不出现在结果中（与「最近 200 条」窗口口径一致）

### Requirement: 会话管理页检索交互

会话管理页搜索框 SHALL 在输入停止约 250ms 后发起服务端检索；请求期间显示加载态，结果以「会话 + 命中片段」列表呈现，命中关键词 MUST 以高亮样式标记；点击结果 MUST 打开该会话并定位到命中消息（滚动到消息并短暂高亮）。清空关键词后 MUST 恢复原会话列表；接口失败时 MUST 回退为本地标题/预览过滤并给出可见提示。

#### Scenario: 防抖只发一次
- **WHEN** 用户快速连续输入 5 个字符后停止
- **THEN** 只发出一次检索请求（内容为最终关键词）

#### Scenario: 点击结果定位
- **WHEN** 用户点击某条命中片段
- **THEN** 打开对应会话，命中消息滚动到可视区并获得短暂高亮

#### Scenario: 失败降级
- **WHEN** 检索接口返回错误
- **THEN** 页面仍按标题/末条预览过滤，并显示「内容检索不可用」提示

#### Scenario: 归档视图不受影响
- **WHEN** 用户切换到归档区
- **THEN** 搜索仍使用本地过滤，不调用内容检索接口

### Requirement: 全局会话速查

系统 SHALL 支持在任意视图按 Ctrl/Cmd+K 唤起会话速查浮层：输入框自动聚焦，输入约 250ms 防抖后调用检索接口，最多展示 12 条结果；↑/↓ 移动选中项、Enter 打开选中会话并定位命中消息、Esc 关闭；点击结果同样打开并定位。浮层关闭或跳转后 MUST 清除检索状态。

#### Scenario: 快捷键唤起与关闭
- **WHEN** 用户在任意页面按 Ctrl/Cmd+K，随后按 Esc
- **THEN** 浮层先出现、后关闭，页面视图不跳转

#### Scenario: 键盘选择打开
- **WHEN** 浮层有结果时按 ↓ 再按 Enter
- **THEN** 打开选中会话并定位到其命中消息

#### Scenario: 空结果提示
- **WHEN** 关键词没有命中
- **THEN** 浮层显示「无匹配会话」空态，且不显示错误

### Requirement: 高亮安全与可访问性

命中高亮 MUST 以纯文本切分方式渲染（MUST NOT 将用户输入作为 HTML 注入）；关键词与正文中的 HTML 片段 MUST 按字面文本显示。速查浮层 MUST 声明 `role="dialog"` 与 `aria-modal="true"`，输入框 MUST 有可读标签；↑/↓/Enter/Esc 键盘操作 MUST 可用。

#### Scenario: HTML 按文本显示
- **WHEN** 搜索关键词为 `<img src=x>`
- **THEN** 页面显示该字面文本，不产生图片元素或脚本执行

#### Scenario: 高亮保留原文
- **WHEN** 关键词以不同大小写出现在标题中
- **THEN** 高亮命中该子串且显示原始大小写文本
