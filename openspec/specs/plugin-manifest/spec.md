# plugin-manifest Specification

## Purpose
插件清单与生命周期：定义 `plugins/` 目录布局、plugin.json 清单契约、插件的加载/启停/禁用语义与版本兼容性检查——插件作为统一能力包（工具组/通道/watcher/资源）的打包与安装单元。

## Requirements

### Requirement: 插件目录与清单布局

仓库根 `plugins/` 目录（独立于 `entities/`），每个插件一个子目录，内含必需的 `plugin.json` 清单。插件子目录内可含插件自有资源（提示词、技能 JSON、可执行文件）。

#### Scenario: 合法清单被加载
- **WHEN** Core 启动或热装载扫描 `plugins/`，某子目录含可解析的 `plugin.json` 且 `id` 唯一
- **THEN** 该插件进入已加载清单，状态为清单声明的初始状态（enabled 默认 true）

#### Scenario: 非法清单被拒绝且不阻塞启动
- **WHEN** 某子目录缺少 `plugin.json`、JSON 不可解析、或 `id` 与已加载插件冲突
- **THEN** 该插件不加载，Core 正常启动，诊断信息记录该目录与拒绝原因

### Requirement: 清单能力声明

plugin.json 清单 MUST 声明插件元信息与四类能力：`tools`（工具组）、`channel`（通道适配器）、`watch`（自动化 watcher）、`resources`（技能/提示词资源注入）。

工具项（CustomTool 字面形状）MUST 支持两个可选声明字段：

- **凭据变量**：headers 值与 target 中可引用 `${credentials.<key>}`，运行时解析为宿主侧该插件凭据区对应键值；清单内不出现任何明文密钥。引用的键无对应凭据时工具调用返回明确错误（指引到插件凭据配置），不发送部分鉴权请求。
- **响应裁剪**：可选 `maxChars`（响应文本上限，缺省沿用现有 8000 截断口径）与 `select`（JSON path 表达式，响应为 JSON 时按表达式摘取片段后返回；非 JSON 响应忽略 select）。

插件级凭据存储于 config 的 `plugins.credentials.<插件id>` 键值区（与通道凭据同等脱敏口径）；清单 installation 段记录安装来源（索引 id/包 URL/git 地址与 ref），供更新流解析。

#### Scenario: 凭据变量注入
- **WHEN** 清单工具 header 声明 `Authorization: Bearer ${credentials.api_token}` 且宿主侧已配该插件 api_token
- **THEN** 实际请求携带解析后的真实 token，清单与日志中不出现明文

#### Scenario: 凭据缺失拒绝调用
- **WHEN** 工具声明引用 `${credentials.api_token}` 而插件凭据区无该键
- **THEN** 调用被拒绝并返回「凭据未配置」指引，不发出请求

#### Scenario: 响应裁剪声明生效
- **WHEN** 工具声明 maxChars=2000 且响应为 100KB JSON
- **THEN** 工具返回内容不超过 2000 字符；声明 select 时返回按表达式摘取的片段

#### Scenario: 非 JSON 响应忽略 select
- **WHEN** 工具声明 select 而响应不是 JSON
- **THEN** select 被忽略，仅按 maxChars 截断返回

#### Scenario: 能力面可查询
- **WHEN** 管理面或诊断请求某插件的能力面
- **THEN** 返回四类能力的声明结果（含每类数量与明细），未声明的能力显示为空

#### Scenario: 空能力插件被拒绝
- **WHEN** 清单四类能力全部未声明
- **THEN** 该插件加载被拒绝，诊断信息注明「未声明任何能力」

#### Scenario: 内联 HTTP 工具零代码生效
- **WHEN** 用户在 plugins/ 放入仅含内联 tools 声明的插件目录（无 process 字段）
- **THEN** 热装载扫描后工具即注册（命名空间前缀），绑定个体可调用；无需任何编译或进程

### Requirement: 启停与禁用语义

插件可被启用/禁用（管理端点或配置），状态持久化。禁用插件的能力面从系统中完全摘除：工具不再随 schema 下发、通道 supervisor 停止、watcher 循环退出、资源不再注入。运行中会话不受已派发调用影响，但新调用被拒。

#### Scenario: 禁用即时生效
- **WHEN** 已加载插件被禁用
- **THEN** 其工具从下一次 schema 下发中消失、watcher 停止轮询、通道进入未配置状态；启用后能力面恢复

#### Scenario: 启停持久化
- **WHEN** 插件被禁用后进程重启
- **THEN** 该插件仍为禁用状态，不自动恢复

### Requirement: 版本与兼容性检查

清单声明 `exmMin`（最低宿主版本，SemVer）。加载时宿主当前版本低于该值则拒绝加载并诊断提示；未声明视为兼容。

#### Scenario: 版本不满足拒绝加载
- **WHEN** 清单 `exmMin` 高于宿主当前版本
- **THEN** 插件不加载，诊断信息注明「需要宿主 ≥ {exmMin}，当前 {version}」
