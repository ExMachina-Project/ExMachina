# update-pipeline Specification

## Purpose
TBD - created by archiving change update-pipeline-and-deploy-isolation. Update Purpose after archive.

## Requirements

### Requirement: 更新检查双模式

系统 SHALL 在 `GET /api/version/check` 中按 `/host` 内容自动判定部署模式并返回 `deployMode`：

- `git`：`/host/.git` 存在（源码仓直挂）。沿用现有行为：fetch 上游、对比 HEAD 与 `origin/<branch>`。
- `image`：`/host` 存在（compose 部署目录）但无 `.git`。SHALL 用 `git ls-remote <upstream-url> refs/heads/<branch>` 匿名查询上游（分支来自环境变量 `EXM_UPDATE_BRANCH`，缺省 `main`），与运行中镜像的 `EXM_GIT_HASH` 前缀比对；上游 URL 来自 `EXM_UPDATE_REMOTE`（缺省仓库公开地址）。
- `none`：`/host` 不存在。返回 `note` 说明，`updateAvailable = null`。

镜像模式下当 `ls-remote` 失败（网络不可达/仓库不可见）时，系统 SHALL 返回 `updateAvailable = null` 与可读错误说明，MUST NOT 报「未挂载仓库」。响应 SHALL 在既有字段外增量包含 `deployMode`（`git|image|none`）；旧字段语义不变。

#### Scenario: 镜像部署下检查更新
- **WHEN** `/host` 存在但无 `.git`（镜像模式部署），且上游分支可访问
- **THEN** 响应含 `deployMode:"image"`，`updateAvailable` 为上游 SHA 与 `EXM_GIT_HASH` 前缀比对结果，并附上游 commit 信息

#### Scenario: 上游不可达时失败可判定
- **WHEN** 镜像模式下 `git ls-remote` 超时或失败（网络不可达）
- **THEN** 响应 `updateAvailable:null` 与网络原因说明，不出现「未挂载仓库」误报

#### Scenario: 源码模式回归不变
- **WHEN** `/host/.git` 存在
- **THEN** 走既有 git fetch 比对逻辑，`deployMode:"git"`，其余字段与现状一致

### Requirement: 更新任务状态机（防死锁）

系统 SHALL 以状态文件（`/tmp/exm-update.state`：启动时间戳、PID、phase、退出码）为唯一事实源判定更新任务状态；`pgrep` 仅作辅助探测，且探测模式 MUST 使用自匹配免疫写法（`pgrep -f '[a]pply-update.sh'`）。`GET /api/version/apply/status` SHALL 返回 `running/phase/exitCode/log/deployMode`。

容器重启后遗留的陈旧状态文件 SHALL 被识别为非运行（PID 不存活即判定结束），MUST NOT 造成永久 `running:true`。同一时刻 SHALL 只允许一个更新任务（并发 apply 依据状态机判定返回 409）。

#### Scenario: 无任务时不误报运行中
- **WHEN** 从未发起过更新（或更新已结束）时查询 `/api/version/apply/status`
- **THEN** `running:false`，`phase` 为 `idle`/`done`/`failed` 之一，绝不因 pgrep 自匹配恒为 true

#### Scenario: 容器重启后陈旧状态自愈
- **WHEN** 更新中途容器被 recreate，残留旧 state 文件（旧 PID 已不存在）
- **THEN** status 判定非运行中并标记 `failed`，可立即重新发起 apply（无 409 死锁）

#### Scenario: 任务进行中阻塞并发请求
- **WHEN** 更新脚本运行中再次 `POST /api/version/apply`
- **THEN** 返回 409 且不启动第二个任务

### Requirement: 一键更新（镜像模式为主）

`POST /api/version/apply` 在镜像模式下 SHALL 经辅助容器（一次性 docker cli 容器，挂载 docker.sock 与部署目录宿主真实路径）执行 `docker compose pull && docker compose up -d`；MUST NOT 在网关容器内或宿主上执行任何源码构建。镜像源 SHALL 按优先级解析：环境变量 `EXM_IMAGE` → 部署 compose 的 `image:` 字段；两者皆无时 SHALL 返回 400 并说明配置方法。

git 模式（`/host/.git` 存在且未配置 `EXM_IMAGE`）SHALL 保留原有「git pull → compose build → 辅助容器 up -d」兼容路径。

#### Scenario: 镜像模式一键更新
- **WHEN** 镜像模式部署下点击一键更新且 `EXM_IMAGE` 或 compose `image:` 可解析
- **THEN** 辅助容器拉取新镜像并 recreate 服务容器，全程无源码构建，完成后版本哈希更新

#### Scenario: 镜像源未配置时报可修复错误
- **WHEN** 镜像模式但 `EXM_IMAGE` 与 compose `image:` 均不可得
- **THEN** `POST /api/version/apply` 返回 400，错误信息含配置指引

#### Scenario: git 兼容路径回归
- **WHEN** `/host/.git` 存在且未配置 `EXM_IMAGE` 时发起一键更新
- **THEN** 走既有 git pull → build → 辅助容器 up -d 流程，行为与改造前一致

### Requirement: 前端版本面板适配

WebUI 设置页版本面板 SHALL 展示 `deployMode` 徽标（源码/镜像/未挂载）与模式说明；更新进行中 SHALL 展示 `phase` 阶段文案（拉取镜像/重建容器/完成/失败）；失败 SHALL 展示日志尾部。zh/en 文案同步。

#### Scenario: 镜像模式徽标与确认文案
- **WHEN** `deployMode:"image"` 时打开设置页版本面板
- **THEN** 显示「镜像部署」徽标，一键更新确认文案提示拉取镜像并重启

#### Scenario: 更新阶段实时可见
- **WHEN** 更新任务处于 `pulling` → `recreating` 流程中
- **THEN** 面板按 phase 依次显示对应阶段文案，完成后展示结果
