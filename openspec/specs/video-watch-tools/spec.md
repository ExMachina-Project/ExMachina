# video-watch-tools Specification

## Purpose
观看工具组：B站/YouTube 双平台的观看面工具语义——每个工具的输入输出契约、分页口径与错误行为；经插件注册面以 `video__*` 命名空间下发。

## Requirements

### Requirement: 视频元信息工具

`video__summary(videoId)`：返回标题、UP主、时长、播放/弹幕/评论数、简介、分区、发布时间。两平台同构。

#### Scenario: 元信息一次拿全
- **WHEN** 智能体传入候选清单中的视频标识
- **THEN** 返回结构化元信息，字段缺失时以占位值标注而非报错

### Requirement: 字幕工具

`video__subtitle(videoId, page?)`：返回字幕全文（按行带时间戳），超长分页；无字幕时明确返回「该视频无字幕」而非空串。

#### Scenario: 字幕分页
- **WHEN** 字幕超过单页上限
- **THEN** 返回当前页内容与分页信息（总页数/下一页提示），智能体可续取

#### Scenario: 无字幕明确告知
- **WHEN** 视频没有可用字幕（B站无 CC 且无 AI 字幕、YouTube 无 caption 轨）
- **THEN** 返回明确的「无字幕」说明，智能体转用其他工具（如弹幕/简介）补观看材料

### Requirement: 弹幕工具（仅B站）

`video__danmaku(videoId, segment?)`：返回弹幕抽样（按时间段分片，含热度权重），总量与分片上限固定；智能体可看不同时间段的弹幕感知节奏与氛围。

#### Scenario: 弹幕分段抽样
- **WHEN** 智能体请求某视频弹幕
- **THEN** 返回按时间分片的弹幕样本（每片限量），标注所在时间段；未指定段时返回全片抽样概览

### Requirement: 评论工具

`video__comments(videoId, page?, sort?)`：返回评论区页（热度/时间排序可选），每页固定条数，含作者/赞数/文本/回复数；B站支持展开单条评论的回复页。

#### Scenario: 评论区翻页
- **WHEN** 智能体翻取评论区后续页
- **THEN** 返回该页评论与分页信息；空页明确标注「没有更多」

### Requirement: UP主与相关视频工具

`video__uploader(uploaderId)`：UP主/频道公开信息与近期投稿（限量）；`video__related(videoId)`：相关/推荐视频（限量，结构同候选清单）。

#### Scenario: UP主近作可见
- **WHEN** 智能体查询 UP主
- **THEN** 返回其公开信息与近期投稿标题/数据，智能体可判断内容脉络

### Requirement: 站内搜索工具

`video__search(query)`：平台内搜索，返回结构同候选清单（标识/标题/UP主/数据），供智能体按倾向自行发现视频。

#### Scenario: 自主搜索发现
- **WHEN** 智能体对候选不感兴趣，用倾向词搜索
- **THEN** 返回搜索结果清单，智能体可从中选择观看并进入同一工具探索流程

### Requirement: 平台差异与错误契约

工具面隐藏平台差异（同一工具名双平台同构，凭通道配置路由）；YouTube 无弹幕，该工具在 youtube 通道返回「该平台无弹幕数据」。平台接口失败（风控/限流/网络）返回结构化错误（含可读原因），不中断会话轮次——智能体可换工具或放弃该视频。

#### Scenario: 平台接口失败不中断
- **WHEN** 某工具调用遇到接口错误
- **THEN** 返回错误工具结果（含原因），会话继续；智能体基于其余材料决定是否继续该视频
