# plugin-admin Specification

## Purpose
插件管理面：REST 管理端点、WebUI 插件管理页、doctor 诊断输出——让用户能看到已装插件、能力面、运行状态并启停。

## Requirements

### Requirement: 插件列表与详情端点

`GET /api/plugins` 返回插件列表（id、名称、版本、形态 builtin/process、状态 enabled/error、能力面摘要）；`GET /api/plugins/:id` 返回单插件详情（含清单原文、能力明细、进程插件的健康信息与重启计数）。

#### Scenario: 列表含状态与形态
- **WHEN** 请求 GET /api/plugins
- **THEN** 返回全部已发现插件，含内置与进程形态标注、启停状态、错误插件的失败原因

### Requirement: 启停与安装卸载端点

`PUT /api/plugins/:id/enabled`（body 含 enabled 布尔）切换启停即时生效；`DELETE /api/plugins/:id` 卸载进程插件（终止进程并移除目录），内置插件不可卸载（返回明确错误）。

#### Scenario: 启停即时生效
- **WHEN** PUT 启停某插件
- **THEN** 返回操作结果，能力面按 plugin-manifest 的启停语义立即摘除或恢复

#### Scenario: 内置插件不可卸载
- **WHEN** DELETE 目标为 builtin 形态插件
- **THEN** 返回 4xx 错误并说明内置插件不可卸载，插件不受影响

### Requirement: WebUI 插件管理页

WebUI 新增「插件」导航页：插件卡片列表（状态/形态/能力面计数）、详情抽屉（清单、能力明细、进程健康）、启停开关；通道配置页的凭据表单按插件声明的字段清单渲染。

#### Scenario: 插件页可管理
- **WHEN** 用户打开插件页
- **THEN** 看到全部插件及状态，可启停进程插件、查看能力面与健康信息

### Requirement: doctor 诊断

`exm doctor` 输出插件段：已加载/禁用/失败插件计数、失败原因、进程插件健康摘要（运行时长、重启计数）。作为部署自检的一部分。

#### Scenario: doctor 反映插件健康
- **WHEN** 运行 exm doctor 且存在错误状态插件
- **THEN** 诊断输出明确列出失败插件与原因，不影响其余检查项
