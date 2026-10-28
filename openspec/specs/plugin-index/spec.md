# plugin-index Specification

## Purpose
插件索引与安装流：官方索引文件契约、tar.gz 包与 git 仓库目录两种安装源、CLI 与 REST 两条安装入口、更新（保留数据热重载）——把安装从「手工拷目录」升级为「可发现、可安装、可更新」。

## Requirements

### Requirement: 插件索引文件契约

宿主 MUST 读取插件索引（默认官方源，可配置自定义源）。索引为 JSON 数组，每项含插件 id、名称、版本、简介、来源（tar.gz URL 或 git 仓库地址）与最低宿主版本。索引项 id 与已装插件冲突时安装入口 SHALL 提示冲突并要求显式覆盖。

#### Scenario: 索引拉取与展示
- **WHEN** 用户请求插件索引（CLI 或 WebUI）
- **THEN** 返回可安装插件列表（id/名称/版本/简介/是否已安装），索引不可达时返回明确错误并提示检查网络或自定义源配置

#### Scenario: 已装插件在索引中的呈现
- **WHEN** 索引中的插件 id 已在本地安装
- **THEN** 该项标记为已安装；版本高于本地时标记为可更新

### Requirement: tar.gz 包安装

安装入口 MUST 接受 tar.gz 包 URL 或本地路径。包内必须恰好包含一个 plugin.json（可在根或唯一子目录），宿主 SHALL 解包校验清单合法性（含 id 与目录名一致、能力声明非空、版本兼容）后落盘 `plugins/<id>/` 并热装载；校验失败 MUST 不落盘，目录无残留。

#### Scenario: 包安装成功热装载
- **WHEN** 安装一个合法 tar.gz 包
- **THEN** 插件出现在插件列表且能力面即时生效，无需重启宿主

#### Scenario: 非法包拒绝且无残留
- **WHEN** 包内清单非法（无 plugin.json、多个清单、id 不合法、宿主版本不满足）
- **THEN** 安装被拒绝并返回具体原因，`plugins/` 目录无半成品残留

### Requirement: git 仓库目录安装

安装入口 MUST 接受 git 仓库 URL（可指定分支/子目录）：宿主 SHALL 浅克隆到临时区，校验子目录或仓库根含合法清单后落盘 `plugins/<id>/` 并热装载；克隆失败或清单非法 MUST 拒绝安装，无残留。

#### Scenario: 仓库安装成功
- **WHEN** 安装一个根目录含 plugin.json 的 git 仓库
- **THEN** 插件落盘装载成功，`.git` 目录不进入插件目录

#### Scenario: 指定子目录安装
- **WHEN** 仓库清单在子目录（如 plugins/my-plugin）
- **THEN** 以该子目录为插件根落盘，仓库其余内容不进入 `plugins/<id>/`

### Requirement: 插件更新

已装插件 SHALL 可从其来源（索引项或安装时记录的源）更新到最新版：更新 MUST 保留插件数据目录（`plugins/<id>/data/`）与启用状态，替换其余文件后热重载；更新后版本低于或等于本地时 SHALL 提示已是最新。

#### Scenario: 更新保留数据与状态
- **WHEN** 更新一个进程插件到新版本
- **THEN** 其 data/ 目录内容与启用状态在更新后保持，新版本能力面即时生效

#### Scenario: 已是最新
- **WHEN** 更新时来源版本不高于本地版本
- **THEN** 返回已是最新提示，不做任何落盘变更

### Requirement: CLI 插件命令面

`exm plugin` 子命令族 MUST 提供：`list`（含索引对照的版本状态）、`install <id|URL>`（索引 id 或直接源地址）、`update [id]`（缺省全部已装插件）、`uninstall <id>`（与既有卸载语义一致：终止进程、移除目录、保留 config 状态清理）。

#### Scenario: 一键安装往返
- **WHEN** 运行 `exm plugin install <索引id>`
- **THEN** 插件从索引解析来源、下载校验、落盘装载，输出安装结果与插件能力摘要

#### Scenario: 更新全部
- **WHEN** 运行 `exm plugin update`
- **THEN** 逐个检查已装插件来源并更新有新版的，输出每个插件的结果（更新/已是最新/失败原因）

### Requirement: REST 安装端点

`POST /api/plugins/install`（body：索引 id 或源地址，可选覆盖标记）与 `POST /api/plugins/:id/update` MUST 提供与 CLI 等价的安装更新能力；安装中状态可查询；WebUI 插件页 SHALL 新增索引目录视图（列表/搜索/已装标记/一键安装与更新）。

#### Scenario: REST 安装与 WebUI 目录
- **WHEN** 经 REST 发起安装并在 WebUI 插件页查看
- **THEN** 安装完成后插件出现在列表；索引目录页展示可安装项并支持一键安装/更新

#### Scenario: 冲突覆盖
- **WHEN** 安装的插件 id 与已装插件相同且未显式覆盖
- **THEN** 返回冲突错误提示覆盖途径；显式覆盖时走更新语义（保数据热重载）
