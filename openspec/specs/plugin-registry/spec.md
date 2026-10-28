# plugin-registry Specification

## Purpose
核心注册面：定义内置功能与第三方插件共同遵循的能力注册契约（Rust trait）——工具组、通道适配、watcher、资源注入四类能力的注册时序与宿主集成口径，保证「内置零开销、第三方走同一边界」。

## Requirements

### Requirement: 统一能力注册契约

宿主定义四类能力 trait（工具组执行面 / 通道工厂 / watcher 生命周期 / 资源装载），插件以实现 trait 的方式注册能力。内置功能（含迁移后的 video）与进程插件桥都实现同一契约，注册后对系统其余部分不可区分。

#### Scenario: 内置与第三方走同一注册面
- **WHEN** 一个内置 Rust 插件与一个进程插件注册了同名能力的不同实例
- **THEN** 两者都能通过注册面被枚举、启停与审计，调用方无需感知实现形态差异

### Requirement: 工具组接入工具网关

插件工具经注册面进入工具网关：工具名以插件 id 作命名空间前缀（`<plugin>__<tool>`）避免冲突；schema 随 function calling 正常下发；执行结果与错误沿用既有 ToolResult 契约。插件工具的可见性可按个体收窄（对齐 CustomTool 的 agents 口径）。

#### Scenario: 插件工具随 schema 下发
- **WHEN** 启用插件声明了 tools 能力且会话个体的工具面包含该插件
- **THEN** 模型收到的工具清单含命名空间前缀的工具项，调用后得到该插件的执行结果

#### Scenario: 工具名冲突被拒绝
- **WHEN** 两个插件产生相同命名空间工具名
- **THEN** 后注册者被拒绝并诊断记录，先注册者不受影响

### Requirement: 通道工厂接入

通道适配能力以工厂形式注册：声明平台 kind、能力矩阵（收发/媒体/凭据字段）、凭据字段清单。通道 supervisor 启动序列枚举注册面创建适配器，而非硬编码模块列表。WebUI 通道配置页按插件声明的凭据字段清单渲染表单。

#### Scenario: 通道页按清单渲染
- **WHEN** 插件声明 channel 能力且包含凭据字段（如 sessdata/biliJct）
- **THEN** 通道配置页为该平台渲染对应字段输入，未声明字段不出现

#### Scenario: 通道枚举含插件平台
- **WHEN** 网关启动且插件通道适配器已注册启用
- **THEN** 该平台 kind 出现在可创建通道的平台列表中，行为与内置平台一致

### Requirement: watcher 生命周期

watch 能力注册为带生命周期的循环体：宿主统一调度启动、配置变更时重建、禁用/关闭时优雅退出（在飞轮询完成后停止，不硬杀）。

#### Scenario: 禁用后 watcher 优雅退出
- **WHEN** 插件在 watcher 轮询进行中被禁用
- **THEN** 当前轮次完成后循环退出，不再发起新轮次；进程不崩溃、无孤儿任务

### Requirement: 资源注入

resources 能力把插件目录内的技能/提示词文件注入既有装载面（与 entities/skills 同格式），热装载语义一致（文件变更即生效）；注入的资源以插件 id 标注来源，可在管理面区分内置与插件来源。

#### Scenario: 插件技能进目录
- **WHEN** 启用插件的 resources 含技能 JSON
- **THEN** 该技能出现在可用技能目录，来源标注为插件 id；禁用后从目录消失

### Requirement: 既有机制收编

CustomTool（config 声明式）与 entities/skills 装载逻辑收编到注册面之后执行，外部行为完全不变：既有配置无需迁移，工具名、技能 id、审计口径保持原样。

#### Scenario: 既有配置零迁移
- **WHEN** 未安装任何插件但配置了 CustomTool 与 entities/skills
- **THEN** 系统行为与收编前一致（工具可调用、技能可命中、审计来源不变）
