# video-ondemand Specification

## Purpose
TBD - created by archiving change video-ondemand-admin. Update Purpose after archive.

## Requirements

### Requirement: 点播工具面

系统存在至少一条已启用的视频平台通道时，`video__*` 观看工具组 MUST 对全体个体可见，`video__*` 观看工具组（summary/subtitle/danmaku/comments/uploader/related/search）对全体个体可见（全局动态工具），不依赖自动观看轮次；所有视频通道停用或删除后工具组注销，工具面回收。无可用通道期间个体调用残留工具名得到「未配置视频通道」的结构化提示而非崩溃。

#### Scenario: 普通会话点播观看
- **WHEN** 用户在任意智能体会话（WebUI 或 IM 通道）发送含 BV 号或视频链接的消息，要求智能体观看
- **THEN** 智能体可调用 `video__*` 工具完成观看（元信息/字幕/弹幕/评论），无需开启自动观看

#### Scenario: 通道停用即工具回收
- **WHEN** 最后一条已启用的视频平台通道被停用或删除
- **THEN** 全局 `video__*` 工具组在注册表中注销，个体工具面不再下发这些工具

#### Scenario: 无通道时的残留调用
- **WHEN** 工具注销前已下发的会话内个体仍尝试调用 `video__summary`
- **THEN** 返回「未配置视频通道」结构化提示，不产生恐慌或空回复

### Requirement: videoId 归一解析

所有 `video__*` 工具的 videoId 参数 MUST 统一接受三种形态：裸视频号（B 站 `BV…`/`av…`，YouTube videoId）、平台完整链接、`b23.tv` 短链（经一层重定向解析）。解析在工具分发入口归一收口，各工具实现不感知输入形态；同会话重复输入命中缓存零重复解析。解析失败原样透传（由下游平台接口报「视频不存在」）。

#### Scenario: 完整链接直接可用
- **WHEN** 智能体传入 `https://www.bilibili.com/video/BV1xx411c7mD?p=5` 形态的 videoId
- **THEN** 工具按 `BV1xx411c7mD` 执行，分 P 参数不进入标识

#### Scenario: 短链展开
- **WHEN** 智能体传入 `b23.tv/xxxx` 短链
- **THEN** 系统解析一层重定向得到 `bilibili.com/video/BV…` 并抽取 BV 号执行；解析失败时原样透传由下游报错

### Requirement: 点播评论角色判权

通道配置 `actRole`（admin | member，默认 admin）MUST 决定非轮次会话中 `video__act` 的准入：会话无外部来源（控制台/WebUI）时全权放行；有外部来源时按绑定身份角色判权，未绑定身份或角色低于 actRole 即拒绝并给出绑定/提权指引。判权通过后其余闸门（开关/配额/清洗/dry-run/审批）与自动观看轮次完全同口径执行。

#### Scenario: 控制台点播评论
- **WHEN** WebUI 会话中用户要求智能体对某视频发评论
- **THEN** act 按通道闸门（配额/清洗/dry-run/审批）执行，不受 actRole 判权拦截

#### Scenario: 未绑定用户点播评论被拒
- **WHEN** IM 通道用户未 /pair 绑定身份，要求智能体发评论
- **THEN** 动作被拒绝，提示先完成身份绑定；观看类工具不受影响

#### Scenario: 成员在 admin 口径下被拒
- **WHEN** 通道 actRole=admin，绑定角色 member 的用户要求智能体发评论
- **THEN** 动作被拒绝并提示联系管理员提权；观看类工具不受影响

#### Scenario: 成员在 member 口径下放行
- **WHHEN** 通道 actRole=member，绑定角色 member 的用户要求智能体发评论
- **THEN** 判权通过，进入与轮次相同的配额/清洗/dry-run/审批闸门链

### Requirement: 配对码预设角色

配对码签发 MUST 支持预设角色（admin | member，缺省 member）：兑换时身份直接落预设角色，免除「先 member 后手动提权」两步割裂。既有配对码与控制台角色调整端点行为不变。

#### Scenario: 签发管理者配对码
- **WHEN** 管理员在控制台签发配对码并勾选「设为管理者」
- **THEN** 用户在通道内 /pair 兑换后身份角色为 admin，提示语带角色说明

#### Scenario: 兼容旧码
- **WHEN** 兑换无预设角色字段的存量配对码
- **THEN** 身份落 member（缺省值），行为与既有语义一致
