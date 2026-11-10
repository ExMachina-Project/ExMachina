# agent-exec-confinement Specification

## Purpose
将 Agent 终端子进程的执行身份、工作目录与提权行为约束在配置范围内：Linux 降权执行、fs 圈禁双保险、root 提权审批与容器内建约束用户，降低智能体误操作对宿主的破坏面。

## Requirements

### Requirement: Linux 执行身份约束

系统 SHALL 支持（且仅限 Linux）将 Agent 终端子进程的执行身份约束到指定的非 root 用户（`security.execUser`，用户名或数字 uid），子进程工作目录与 HOME SHALL 圈禁在该用户主目录（`security.execHome`，默认 `/home/<execUser>`）内。

#### Scenario: 配置 execUser 后子进程降权执行

- **WHEN** Linux 下配置 `security.execUser = "exm"` 且网关以 root 运行
- **THEN** Agent 终端命令经审批/白名单放行后，子进程以 exm 的 uid/gid 执行，cwd 为 `/home/exm`，环境 `HOME=/home/exm`

#### Scenario: execUser 为空时不约束（默认零回归）

- **WHEN** `security.execUser` 未配置或为空
- **THEN** 终端执行行为与既有实现完全一致（无降权、无提权拦截、无 fs 圈禁）

#### Scenario: 约束用户不存在时显式失败

- **WHEN** 配置 `execUser` 为用户名但系统无该用户，且未提供可用回落
- **THEN** 终端工具返回明确错误（提示用户不存在及修复方式），不静默降级为原身份执行

#### Scenario: 数字 uid 直接生效

- **WHEN** 配置 `execUser = "1000"`（数字）且网关 root
- **THEN** 子进程以 uid 1000 执行，无需 /etc/passwd 条目

### Requirement: 工作目录圈禁与 fs 双保险

`security.execUser` 开启时，系统 SHALL 将终端子进程 cwd 与 HOME 限定在 execHome 内，且 filesystem-ops 工具的路径操作 SHALL 在路径规范化后校验目标仍在 execHome 内，越界 MUST 拒绝并提示可走 root 提权审批。

#### Scenario: fs 工具越界拒绝

- **WHEN** execUser 约束开启，fs 工具请求 `/etc/passwd` 写操作
- **THEN** 操作被拒绝，错误信息说明圈禁边界（execHome）与 root 提权审批路径

#### Scenario: 相对路径穿越被拦截

- **WHEN** execUser 约束开启，fs 工具请求 `../../etc/shadow`
- **THEN** 规范化后路径在 execHome 外，操作被拒绝

### Requirement: root 提权审批

系统 SHALL 识别提权命令（首 token 为 `sudo`/`doas`/`run0`/`su`，含 `sudo -u`/`runuser` 切换语义），提权命令 MUST 无视白名单直接进入人工审批，审批单以 `[root] ` 前缀标识；批准后系统 SHALL 以 root 身份代执行（剥离冗余 sudo 前缀、保留 su 语义），拒绝或超时 MUST 受阻回流。

#### Scenario: sudo 命令即使命中白名单也需审批

- **WHEN** execUser 约束开启，白名单含 `apt`，Agent 执行 `sudo apt install -y tmux`
- **THEN** 命令被拦截进入审批（不因白名单放行），审批单命令为 `[root] sudo apt install -y tmux`

#### Scenario: 批准后 root 代执行

- **WHEN** `[root] sudo apt install -y tmux` 审批单被批准
- **THEN** 系统剥离 `sudo` 前缀以 root 执行 `apt install -y tmux`，输出写回审批单与工具结果

#### Scenario: su 语义保留

- **WHEN** `[root] su - postgres -c 'psql ...'` 被批准
- **THEN** 命令原样以 root 执行（su 切换用户是命令语义，不剥离）

#### Scenario: 拒绝受阻回流

- **WHEN** 提权审批单被拒绝
- **THEN** 工具结果为拒绝原因，Agent 流程按既有受阻语义继续

### Requirement: 审批代执行沙箱收口

`approval_decide` 批准普通（非 `[root]`）命令审批单后，代执行 SHALL 走与直通执行相同的沙箱加固路径（环境净化/降权），MUST NOT 绕过 execUser 约束。

#### Scenario: 审批放行不等于特权

- **WHEN** execUser 约束开启，普通审批单（非提权）被批准
- **THEN** 代执行子进程仍以 execUser 身份（uid/gid + cwd + HOME）执行，与白名单直通路径一致

### Requirement: 配置与 UI 暴露

`security.execUser`/`security.execHome` SHALL 经既有配置文件与 `/api/config` 读写，SettingsView sandbox 分组 SHALL 提供两字段的编辑入口（label/help/i18n zh+en）；审批卡片 SHALL 识别 `[root] ` 前缀并以提权样式（红色标识 + 专属文案）渲染。

#### Scenario: Settings 保存 execUser

- **WHEN** 用户在设置页 sandbox 分组填入 execUser=exm 并保存
- **THEN** 配置持久化，终端子进程按新身份执行（下一命令生效）

#### Scenario: 审批卡提权高亮

- **WHEN** 审批单命令以 `[root] ` 开头
- **THEN** 会话内联与审批列表均显示 root 提权标识与警示文案

### Requirement: 容器内建约束用户

官方镜像 SHALL 内建非特权用户（如 `exm`，固定 uid），供部署者启用 execUser 约束；默认部署 MUST 保持既有行为（不自动启用）。

#### Scenario: 镜像内含 exm 用户

- **WHEN** 检查官方镜像 /etc/passwd
- **THEN** 存在固定 uid 的 exm 用户及主目录，且网关默认仍以 root 运行
