# deploy-isolation Specification

## Purpose
镜像注册表发布与零源码生产部署模板：生产目录不含仓库源码、镜像从注册表拉取，开发编排与生产部署互不干扰。

## Requirements

### Requirement: 镜像注册表发布

release 流水线（发版时）SHALL 构建并发布 linux/amd64 容器镜像：注册表 ghcr.io 默认、`EXM_REGISTRY` 可覆盖；凭据来自 repository secrets，未配置时该 job SHALL 跳过且不阻塞发版。Tag 策略：`latest`、`vX.Y.Z`、`main`；构建 SHALL 注入 `GIT_HASH` build-arg（镜像内 `EXM_GIT_HASH`）。镜像 MUST NOT 包含运行时数据（.exmachina/、.env、.git 等）。

#### Scenario: 发版自动出镜像
- **WHEN** dev→main 合入触发发版且镜像凭据 secrets 已配置
- **THEN** 注册表出现 `latest`/`vX.Y.Z`/`main` 三个 tag 的 amd64 镜像，`EXM_GIT_HASH` 为发版 commit

#### Scenario: 凭据缺失不阻塞发版
- **WHEN** 镜像凭据 secrets 未配置
- **THEN** docker job 跳过（日志明示原因），tar 包与其余发版产物正常发布

### Requirement: 生产部署模板（零源码挂载）

仓库 SHALL 提供 `deploy/docker-compose.yml` 生产模板：`image:` 引用注册表镜像（`EXM_IMAGE` 可覆盖）；MUST NOT 含 `build:` 段、`/host` 挂载、docker.sock 挂载与宿主 docker 二进制挂载。数据层边界 SHALL 为 named volume（state/plugins）+ 绑定挂载 `./entities`、`./skills`。一键更新所需的 docker.sock SHALL 以注释段落提供（显式开启才生效）。

#### Scenario: 模板开箱即用
- **WHEN** 按模板目录结构（模板 + entities/skills + .env）执行 `docker compose up -d`
- **THEN** 服务以注册表镜像启动，容器内无任何宿主源码路径，数据落在 volume 与绑定目录

#### Scenario: 一键更新为显式可选项
- **WHEN** 使用默认模板（未取消 sock 注释）
- **THEN** 容器无引擎访问能力；取消注释并配置 `EXM_IMAGE` 后，WebUI 一键更新可用

### Requirement: 开发编排隔离

仓库根 `docker-compose.yml` SHALL 定位为开发用途：compose 项目名 `exmachina-dev`、容器名 `exmachina-dev`、宿主端口 `4174:4173`、卷与网络带 `-dev` 前缀；MUST NOT 与生产编排的同名资源冲突。生产容器名 `exmachina` MUST 只由部署目录的 compose 项目管理，两套编排 MUST NOT 互相接管对方容器。

#### Scenario: 双编排并存不互抢
- **WHEN** 生产（部署目录 compose，4173）与开发（仓库根 compose，4174）同时 `up -d`
- **THEN** 两个容器并存、各自归属各自 compose 项目，互不接管、互不覆盖

#### Scenario: 开发编排数据独立
- **WHEN** 开发容器写入会话/配置数据
- **THEN** 数据落在 `-dev` 前缀的 volume，不影响生产 named volume

### Requirement: 部署文档与迁移

`docs/安装指南.md` SHALL 覆盖镜像模式部署、开发模式部署、一键更新两种模式的前提与差异、从「部署目录手工 build」迁移到镜像模式的步骤；MUST 明确生产推荐镜像模式。

#### Scenario: 按文档可完成迁移
- **WHEN** 运维按安装指南迁移章节操作既有手工 build 部署
- **THEN** 部署切换为镜像拉取模式，数据（会话/配置/编成数据）完整保留，服务版本可追溯
