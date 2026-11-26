# deploy-isolation Specification

## Purpose
镜像注册表发布与零源码生产部署模板：生产目录不含仓库源码、镜像从注册表拉取，开发编排与生产部署互不干扰。

## Requirements

### Requirement: 镜像注册表发布

release 流水线（dev→main 合入发版时）SHALL 构建并发布多架构（linux/amd64；arm64 待后续）容器镜像：

- 注册表：ghcr.io 默认，`EXM_REGISTRY` 可覆盖（如 docker.io）；凭据来自 repository secrets（`GHCR_TOKEN`/`DOCKERHUB_TOKEN`，未配置时该 job 跳过且不阻塞发版）。
- Tag 策略：`latest`、`vX.Y.Z`（发版 tag）、`main`；构建注入 `GIT_HASH` build-arg（镜像内 `EXM_GIT_HASH`）。
- 镜像 MUST NOT 包含运行时数据（.exmachina/、entities 运行态文件、.env、.git）；`.dockerignore` 保持排除。

CI（PR）MUST NOT 发布镜像，仅本地构建校验 Dockerfile 可构建。

#### Scenario: 发版发布镜像

- **WHEN** dev 合入 main 并打 vTag 发版
- **THEN** 流水线发布 latest/vX.Y.Z/main 三个 tag 的镜像；PR 构建不发布

### Requirement: 生产部署模板（零源码挂载）

仓库 SHALL 提供 `deploy/docker-compose.yml` 生产模板：

- `image:` 引用注册表镜像（环境变量 `EXM_IMAGE` 可覆盖，缺省 ghcr.io 最新版）；MUST NOT 带 `build:` 段。
- MUST NOT 挂载 `/host`、docker.sock、宿主 docker 二进制；MUST NOT 挂载仓库源码目录。
- 数据层边界：`exmachina-state` named volume（/app/.exmachina）+ `exmachina-plugins`（/app/plugins）+ 绑定挂载 `./entities`、`./skills`（编成数据，可持久化编辑）。
- 一键更新的引擎访问 SHALL 收敛为可选段落（注释模板）：需要 WebUI 一键更新时，取消注释 docker.sock 只读挂载与 `EXM_IMAGE` 环境变量；默认模板不含。
- 端口/密钥等参数与现有生产一致（4173、EXM_AUTH_KEY via .env）。

#### Scenario: 模板零源码

- **WHEN** 检查 deploy/docker-compose.yml
- **THEN** 无 build: 段、无 /host 与 docker.sock 挂载（docker.sock 仅存在于注释的可选段落）

### Requirement: 开发编排隔离

仓库根 `docker-compose.yml` SHALL 定位为开发用途并满足：

- compose 项目名固定 `exmachina-dev`（`name:` 字段），容器名 `exmachina-dev`，端口 `4174:4173`（不与生产 4173 冲突）。
- 卷/网络带 `-dev` 前缀，MUST NOT 与生产 named volume 重名。
- 保留 `build:` 本地构建与 `/host`+docker.sock 挂载（开发一键更新走 git 模式）。
- 生产容器名 `exmachina` MUST 只由部署目录的 compose 项目管理；两套编排 MUST NOT 互相接管对方容器。

#### Scenario: 两套编排不互接管

- **WHEN** 仓库根 compose（exmachina-dev，端口 4174）与部署目录 compose（exmachina，端口 4173）并存
- **THEN** 卷/网络名带 -dev 前缀互不重名，任一 compose 不接管对方容器

### Requirement: 部署文档与迁移

`docs/安装指南.md` SHALL 覆盖：镜像模式部署（复制 deploy/ 模板 → .env → up -d）、开发模式部署（仓库根 compose）、一键更新两种模式的前提与行为差异、从「部署目录手工 build」迁移到镜像模式的步骤。文档 MUST 明确生产推荐镜像模式。

#### Scenario: 文档覆盖两种模式

- **WHEN** 阅读 docs/安装指南.md
- **THEN** 镜像模式部署、开发模式部署、一键更新差异与手工 build 迁移步骤齐备，且注明生产推荐镜像模式
