# agent-terminal-tools Specification

## Purpose
TBD - created by archiving change harness-tools-round2. Update Purpose after archive.

## Requirements

### Requirement: 白名单只读与工具类命令
终端白名单 MUST 覆盖日常只读/信息/文本处理/压缩归档/远程与容器/数据库类命令（见下表）。白名单内命令 MUST 免审批直接执行；子命令受限的命令 MUST 按声明的子命令前缀匹配。联网语义命令（curl/wget/ssh 等）MUST 仍受沙箱联网闸门与审批口径约束（strict 沙箱 + 未开联网时转审批），不得因入白名单而绕过。

放行类目（首命令，subs 为子命令约束，空 = 全部子命令）：

| 类目 | 命令 |
|---|---|
| 信息只读 | stat、du、df、env、ps、file、basename、dirname、realpath、md5sum、sha256sum、free、uptime、hostname、id、groups、locale |
| 文本处理 | awk、sort、uniq、xargs、cut、tr、column、bc、jq、yq |
| 网络获取 | curl、wget、ping、dig、nslookup、host、ip（subs: addr/route/link/show） |
| 压缩归档 | tar、unzip、zip、gzip、gunzip、xz、zcat |
| 权限与时间 | chmod、chown、sleep |
| 远程同步 | ssh、scp、rsync、sftp |
| 容器 | docker（subs: ps/images/inspect/logs/exec/run/version）、docker compose（子命令全放） |
| 数据库 | sqlite3（psql/mysql 不入白名单，仍走审批） |
| 媒体与其他 | ffmpeg、convert、openssl、base64、uuidgen、xxd |

#### Scenario: 白名单命令免审批执行
- **WHEN** 智能体执行 `stat src/main.rs`
- **THEN** 命令直接执行返回，不产生审批单

#### Scenario: 子命令约束匹配
- **WHEN** 智能体执行 `docker ps -a`
- **THEN** 首命令 docker 命中且子命令 ps 在约束表内，直接执行

#### Scenario: 联网命令仍受沙箱闸门
- **WHEN** 沙箱 strict 且未开启联网，智能体执行 `curl https://example.com`
- **THEN** curl 虽在白名单，仍被联网闸门拦截转审批

### Requirement: 后台任务可寻址
terminal 工具 MUST 支持 `op=list`：返回当前会话的全部后台任务（taskId、命令、状态 running/finished、退出码、输出尾部摘要），供模型在丢失 taskId 后重新寻址。list MUST 不经过审批（只读）。既有 poll/kill 语义不变。

#### Scenario: 跨轮次寻址后台任务
- **WHEN** 智能体上一轮启动了后台构建，本轮记不清 taskId
- **THEN** 调用 `terminal op=list` 拿到任务清单与状态，继续 poll 或 kill

### Requirement: grep 上下文行
grep 工具 MUST 支持 `contextLines` 参数（默认 0，0-5）：命中行前后各带 N 行上下文，行间以 `—` 分隔多块命中。无参数时行为与现状一致（单行输出）。

#### Scenario: 带上下文检索
- **WHEN** 智能体 `grep pattern=x contextLines=2`
- **THEN** 每处命中返回目标行与前后各 2 行，上下文行标注行号

### Requirement: read 图像副产物
read 工具遇到图像文件（png/jpg/jpeg/gif/webp/bmp，按扩展名识别）MUST 不再报错：返回文本摘要（文件、字节数、格式）并把绝对路径放入 `ToolResult.images`，由 runtime 在目标模型具备视觉能力时回灌。

#### Scenario: 读取图像文件
- **WHEN** 智能体 read 一张 png 截图
- **THEN** 返回图像摘要文本，且 images 携带路径供视觉回灌
