# experience-iteration Specification

## Purpose
TBD - created by archiving change long-horizon-iteration. Update Purpose after archive.

## Requirements

### Requirement: 聊天流面板深化
WebUI 聊天流面板 SHALL 呈现外部通道流（未读徽标、流内会话切换），流状态与通道侧同源；未读计数 SHALL 在用户打开对应会话后清零。

#### Scenario: 流未读徽标
- **WHEN** 某流绑定会话收到新消息且用户当前未打开该会话
- **THEN** 该流条目显示未读计数；用户打开该会话后计数清零

#### Scenario: 流内会话切换
- **WHEN** 用户在流条目展开的会话列表中选择某会话
- **THEN** 该流活跃会话改为所选会话，通道内下一条消息落入该会话，WebUI 与通道指令口径一致

### Requirement: 会话批量管理与导出
WebUI 会话列表 SHALL 支持多选批量操作：归档（移出主列表、可恢复）与删除（带确认）。任一会话 SHALL 可导出为 Markdown（含消息、工具调用摘要与审批记录）与 JSON（结构化全量）两种格式。批量端点 SHALL 幂等：重复归档/恢复成功且不刷新归档时间。

#### Scenario: 批量归档
- **WHEN** 用户勾选多个会话并选择归档
- **THEN** 所选会话移出主列表进入归档区，操作可在归档区撤销

#### Scenario: 幂等归档
- **WHEN** 对已归档会话重复发起归档
- **THEN** 操作成功且归档时间戳不变（归档区排序稳定）

#### Scenario: 导出会话
- **WHEN** 用户对某会话选择导出 Markdown
- **THEN** 下载的文件包含按时间排序的消息、思考与工具调用摘要、审批记录，无平台专有格式依赖

### Requirement: 桌面端托盘与更新
桌面端托盘 SHALL 显示未读消息与待审批角标；托盘菜单 MUST 提供直达主窗口、打开控制台、审批待办入口。桌面端 SHALL 在启动与周期检查时比对自身版本与所连网关版本，不一致时 MUST 提示用户（本机模式提示更新，远程模式仅提示版本差异）。

#### Scenario: 待审批托盘提醒
- **WHEN** 存在待审批项
- **THEN** 托盘图标显示提醒角标，菜单含「待审批 N 项」入口直达审批面板

#### Scenario: 版本差异提示
- **WHEN** 桌面端连接的网关版本与桌面端捆绑版本不一致
- **THEN** 界面提示版本差异及更新建议，不自动更新

### Requirement: 桌面端后端健康监控
本机模式下桌面端 SHALL 监控捆绑网关进程：网关异常退出时 MUST 在界面给出明确状态与重启入口；重启后 MUST 自动恢复会话连接。远程模式连接断开时 SHALL 自动指数退避重连并显示连接状态。

#### Scenario: 网关异常退出
- **WHEN** 捆绑网关进程意外退出
- **THEN** 界面显示异常状态与一键重启入口；重启后自动恢复会话连接
