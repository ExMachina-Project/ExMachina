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
# LLM 环境变量（可选——也可启动后在 WebUI「模型」页配置，二者等价）
# EXM_LLM_BASE_URL=https://<openai 兼容端点>
# EXM_LLM_API_KEY=<密钥>
# 可选:覆盖镜像源
# EXM_IMAGE=ghcr.io/exmachina-project/exmachina:latest
```

## QQ 通道的 napcat 伴随容器（可选）

需要 QQ 通道时,部署 NapCat 提供 OneBot 11 正向 WS 协议端。`docker-compose.yml`
内置了注释掉的 `napcat` 服务段（含 healthcheck）,按需启用:

1. 编辑 `docker-compose.yml`,取消 `napcat:` 整段注释;
2. `docker compose up -d` 拉起;
3. 浏览器开 `http://<主机>:6100`（NapCat WebUI）完成扫码登录;
4. 网关通道配置 `url: ws://napcat:3001`（同 compose 网络内直连;跨编排则用宿主映射口 `ws://<主机>:3002`）。

**healthcheck 口径**（假活防护）:探针探测协议端口 3001 本体——进程存活但协议
端口无响应的「假活」容器会在连续失败后被判 `unhealthy`（`docker ps` 可见）,
不必等网关侧事件自检才发现。网关侧自检另有细分:容器 unhealthy（协议端口无响应）
→ 指引重启容器;端口正常但账号未登录 → 指引扫码（无需重启）。

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

### 本地构建回退（拉不动注册表时）

网络受限（注册表大层被重置 / 私有环境）时，在任一有源码的机器本地构建镜像再切 `.env`：

```bash
# 1. 在源码仓库构建（GIT_HASH 带进 /api/version 便于核对）
docker build --build-arg GIT_HASH=<commit> -t exmachina:<commit> .

# 2. 部署目录 .env 切换镜像源
#    EXM_IMAGE=exmachina:<commit>

# 3. 借道一次性辅助容器 recreate（不能在容器内直接 up -d——compose 客户端
#    会随旧容器一起被杀，recreate 做一半；辅助容器独立于其生命周期）
docker run --rm \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v <部署目录绝对路径>:<同绝对路径> -w <同绝对路径> \
  docker:27.5.1-cli compose -p exmachina up -d --force-recreate --no-deps exmachina

# 4. 验证
curl http://localhost:4173/api/version   # gitHash 应为 <commit>
```

> 注意 `-p exmachina`（项目名）与「部署目录挂**宿主原始绝对路径**」两处不能省：
> 省略 `-p` 会让 compose 按挂载目录名起项目、误建新卷；挂到 `/host` 之类非原始路径
> 则 compose 里的相对挂载（`./entities` 等）会解析到错误位置。

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
