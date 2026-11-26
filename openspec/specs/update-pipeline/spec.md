# update-pipeline Specification

## Purpose
更新管线：检查双模式（镜像/源码）、防死锁的更新任务状态机、一键更新（镜像模式为主）与前端版本面板适配，让部署目录无源码也能自助升级。

## Requirements

### Requirement: 更新检查双模式

系统 SHALL 在 `GET /api/version/check` 中按 `/host` 内容自动判定部署模式并返回 `deployMode`：

- `git`：`/host/.git` 存在（源码仓直挂）。沿用现有行为：fetch 上游、对比 HEAD 与 `origin/<branch>`。
- `image`：`/host` 存在（compose 部署目录）但无 `.git`。SHALL 用 `git ls-remote <upstream-url> refs/heads/<branch>` 匿名查询上游（分支来自环境变量 `EXM_UPDATE_BRANCH`，缺省 `main`），与运行中镜像的 `EXM_GIT_HASH` 对比；上游 URL 来自 `EXM_UPDATE_REMOTE`（缺省仓库公开地址）。
- `none`：`/host` 不存在。返回 `note` 说明，`updateAvailable = null`。

镜像模式下当 `ls-remote` 失败（网络不可达/仓库不可见）时，系统 SHALL 返回 `updateAvailable = null` 与可读错误说明，MUST NOT 报「未挂载仓库」。

响应 SHALL 在既有字段（version/gitHash/upstream/updateAvailable/note）外增量包含 `deployMode`（`git|image|none`）；旧字段语义不变。

#### Scenario: 镜像模式检查

- **WHEN** `/host` 存在但无 `.git` 且上游分支新于运行镜像的 EXM_GIT_HASH
- **THEN** 响应 deployMode=image 且 updateAvailable=true

#### Scenario: 未挂载仓库

- **WHEN** `/host` 不存在
- **THEN** 响应 deployMode=none，updateAvailable=null，note 说明未挂载

### Requirement: 更新任务状态机（防死锁）

系统 SHALL 以状态文件（`/tmp/exm-update.state`：启动时间戳、PID、phase、退出码）为准判定更新任务状态；`pgrep` 仅作辅助探测，且探测模式 MUST 使用自匹配免疫写法（`pgrep -f '[a]pply-update.sh'`）。

`GET /api/version/apply/status` SHALL 返回：

- `running`（bool）：状态文件存在且 PID 存活且未超过超时（默认 30 分钟）；
- `phase`（`idle|pulling|recreating|done|failed`）；
- `exitCode`（int，任务结束后有值）；
- `log`（尾部 ≤1500 字符）；
- `deployMode`。

容器重启后遗留的陈旧状态文件 SHALL 被识别为非运行（PID 不存活即判定结束），MUST NOT 造成永久 `running:true`。

#### Scenario: 陈旧状态文件

- **WHEN** 容器重启后状态文件遗留但其 PID 已不存活
- **THEN** status 返回 running=false 与上次 exitCode，不出现永久 running:true

### Requirement: 一键更新（镜像模式为主）

`POST /api/version/apply` 在镜像模式下 SHALL 执行：辅助容器（docker:27-cli）在宿主引擎上运行 `docker compose pull && docker compose up -d`（工作目录为部署目录的宿主真实路径，从当前容器 mount 反解），期间本容器被 recreate 属预期；MUST NOT 在网关容器内或宿主上执行任何源码构建。

镜像源 SHALL 按优先级解析：部署 compose 的 `image:` 字段 → 环境变量 `EXM_IMAGE`。两者皆无时 SHALL 返回 400 并说明配置方法。

git 模式（`/host/.git` 存在且未配置 `EXM_IMAGE`/registry）SHALL 保留原有「git pull → compose build → 辅助容器 up -d」路径作为兼容。

同一时刻 SHALL 只允许一个更新任务（并发 apply 返回 409，依据状态机判定而非 pgrep 单一来源）。

#### Scenario: 镜像模式一键更新

- **WHEN** 镜像模式用户触发 apply
- **THEN** 辅助容器执行 pull + up -d，本容器被 recreate 属预期，期间无源码构建

#### Scenario: 并发更新拒绝

- **WHEN** 已有更新任务运行中再次 apply
- **THEN** 返回 409

### Requirement: 前端版本面板适配

WebUI 设置页版本面板 SHALL 展示 `deployMode` 徽标（源码 / 镜像 / 未挂载）与对应说明文案；更新进行中 SHALL 展示 `phase` 阶段文案；失败 SHALL 展示日志尾部。zh/en 文案同步。

#### Scenario: 部署模式徽标

- **WHEN** 镜像模式部署的实例打开设置页版本面板
- **THEN** 显示「镜像」徽标；更新中显示阶段文案，失败显示日志尾部

- 版本信息 `GET /api/version`（version + gitHash）不变；以下两项为引用现有行为，随本规格一并收录。

### Requirement: napcat 伴随容器健康口径

生产编排中 napcat 伴随容器（视频 / QQ 通道的协议端）SHALL 有真实健康判定：容器 healthcheck MUST 探测协议端口的真实服务态（HTTP 探针或 ws 握手），而非仅进程存活；连续失败 MUST 将容器置为 unhealthy（编排层面可见）。网关侧通道自检 MUST 能区分「napcat 容器 unhealthy」与「napcat 在线但账号未登录」两类失败并给出不同的修复指引。

#### Scenario: 假活容器被判 unhealthy

- **WHEN** napcat 容器内进程存活但协议端口持续无响应（超过探测窗口）
- **THEN** 容器状态转为 unhealthy；网关通道自检报告指向容器健康问题而非误导性的「建议重启网关」

#### Scenario: 容器健康但未登录

- **WHEN** napcat 容器协议端口正常但账号处于未登录态
- **THEN** 通道自检报告「等待账号登录」并指引扫码，不报容器故障

### Requirement: webhook 投递超时与重试

webhook 类投递（notify 配置的外发通知）SHALL 有超时与重试契约：单次投递超时上限固定（不悬挂调用方）；失败 MUST 按退避重试有限次；最终失败 MUST 落审计事件（含目标与原因摘要），MUST NOT 静默吞掉。

#### Scenario: 端点僵死不悬挂

- **WHEN** webhook 目标端点建连后不回包
- **THEN** 投递在超时上限内失败返回，走重试链；最终失败产生审计事件

#### Scenario: 失败可追溯

- **WHEN** webhook 重试耗尽仍失败
- **THEN** 审计流中可查该次投递的目标地址与失败原因
