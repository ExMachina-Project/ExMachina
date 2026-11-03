# EXMACHINA 生产部署指南（镜像模式）

本目录是「无源码」的生产部署模板：镜像从注册表拉取，部署目录只含编成数据与配置，与开发仓库完全隔离。

## 首次部署

```bash
# 1. 准备部署目录（本目录内容）
mkdir -p /opt/exmachina && cd /opt/exmachina
#    放入 docker-compose.yml、entities/（编成数据）、.env（密钥等环境变量）

# 2. 拉取镜像并启动
docker compose up -d

# 3. 验证
curl http://localhost:4173/api/version   # version + gitHash
```

`.env` 最小示例：

```ini
EXM_AUTH_KEY=<后台访问密钥>
# 可选:覆盖镜像源
# EXM_IMAGE=ghcr.io/exmachina-project/exmachina:latest
```

## 开启一键更新（可选）

默认模板不含 docker.sock 挂载（最小权限）。要使用 WebUI「一键更新」：

1. 编辑 `docker-compose.yml`，取消 `docker.sock` 与 `docker cli` 两段挂载的注释；
2. `docker compose up -d` 重建容器；
3. WebUI 设置页 → 版本 → 检查更新 / 一键更新。

更新流程：`compose pull` → 一次性辅助容器 `compose up -d --force-recreate`（独立于网关容器生命周期，避免 compose 客户端随旧容器被杀）。进度与阶段见设置页（pulling → recreating → done/failed）。

> 安全提示：挂载 docker.sock 等于授予容器宿主 root 等价权限。仅在信任边界内开启，并保持 `EXM_AUTH_KEY` 强随机。

### 私有镜像仓库（私有 ghcr 包 / 自建 registry）

镜像不是公开可匿名拉取时，`compose pull` 需要凭据。凭据只放部署目录，**不进镜像、不进仓库**：

```bash
# 在部署目录（与 docker-compose.yml 同级）
mkdir -p .docker
docker login ghcr.io -u <用户名>     # 输入带 read:packages 的 token
cp ~/.docker/config.json .docker/config.json
chmod 600 .docker/config.json
```

一键更新脚本（`scripts/apply-update.sh`）检测到 `/host/.docker/config.json` 会自动复制到容器内
临时 `DOCKER_CONFIG` 再 `compose pull`；无该文件时按匿名拉取处理（公开镜像）。
`.docker/config.json` 已在仓库 `.gitignore` 中，不会被提交。

## 从源码部署迁移到镜像部署

原有「仓库根 compose 直接跑」的部署迁移步骤：

```bash
# 1. 停旧栈(数据在命名卷,不丢)
cd /path/to/ExMachina && docker compose down

# 2. 新部署目录:拷贝编成数据与配置
mkdir -p /opt/exmachina && cd /opt/exmachina
cp /path/to/ExMachina/deploy/docker-compose.yml .
cp -r /path/to/ExMachina/entities .
# .env 若在旧目录,一并拷贝

# 3. 卷迁移:沿用旧命名卷(项目名不变则卷名不变)
#    旧项目名 exmachina 的卷:exmachina_exmachina-state / exmachina_exmachina-plugins
#    新目录项目名不同时,需显式指定或重标卷——最简单:compose 里用外部卷声明
docker volume ls | grep exmachina

# 4. 启动(镜像模式)
docker compose up -d
```

如需沿用旧命名卷，在 `volumes:` 顶层段声明：

```yaml
volumes:
  exmachina-state:
    external: true
    name: exmachina_exmachina-state
  exmachina-plugins:
    external: true
    name: exmachina_exmachina-plugins
```

## 与开发仓库隔离

- 开发仓库根的 `docker-compose.yml` 是**开发栈**（build 源码、项目名 `exmachina-dev`、端口 4174）；
- 本目录是**生产栈**（拉镜像、项目名 `exmachina`、端口 4173）；
- 两者项目名/容器名/卷名互不冲突，可同机共存。
