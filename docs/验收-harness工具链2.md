# 验收：harness 工具链第二轮（harness-tools-round2）

> 变更：`openspec/changes/harness-tools-round2`（5 项：WebUI 计划卡片 / terminal 白名单扩充 / grep 上下文行 / read 图像 / 后台任务清单）
> 基线：dev 分支（T2 插件贯通后，22 笔提交）

## 1. 改动清单

| # | 项 | 落点 |
|---|---|---|
| 1 | WebUI 计划卡片 | `webui/src/components/PlanCard.tsx`（新）+ `ChatView.tsx` 模式开关与卡片 + `store.ts` plan.* 事件处理 + `api.ts` getPlan/decidePlan + i18n zh/en + styles.css |
| 2 | terminal 白名单扩充 | `crates/exm-core/src/tools.rs` TERMINAL_ALLOW 追加约 60 条目（9 类目）+ NETWORK_COMMAND_MARKS 补 rsync/sftp/git push/docker compose pull |
| 3 | grep 上下文行 | tool_grep `contextLines`（0-5，默认 0 行为不变）；块合并输出，命中行 `>` 标注 |
| 4 | read 图像 | tool_read 扩展名识别（png/jpg/jpeg/gif/webp/bmp）→ 摘要 + `ToolResult.images`；read_image_data_url 补 bmp mime |
| 5 | 后台任务清单 | BgTask 增 session 归属；terminal `op=list` 按会话过滤返回 taskId/命令/状态/退出码/输出尾 |

## 2. 安全模型核验（白名单扩充不破防）

- **单一命令约束不变**：`&&`/`|`/`;`/重定向仍一律拒绝（SHELL_CHAIN_MARKS 全表不变）。
- **联网闸门后置不变**：白名单放行 → 审批闸门（risky/always）→ 沙箱联网闸门（strict+未联网）。curl/wget/ssh/scp/rsync/docker 虽入白名单，联网特征命中仍转审批——集成测试 `白名单扩充_联网命令仍受沙箱闸门` 断言此行为。
- **子命令约束**：docker 仅 ps/images/inspect/logs/exec/run/version；ip 仅 addr/route/link；psql/mysql 不入白名单（可 DROP，走审批）；sqlite3 入表（本地文件库）。
- **高危特征不变**：DESTRUCTIVE_PATTERNS 全表保留；`docker rmi`/`docker system prune`/`ip netns` 白名单层面即拒（测试断言）。
- **既有测试更新**：`白名单_危险与未知命令不放行` 中 curl/wget/chmod 的断言按新分层语义更新（入白名单 + 后置闸门把关），并新增 docker/ip 子命令拒绝断言。

## 3. 测试记录

- `cargo test -p exm-core`：**234 全过**（lib 193 + 集成 41；新增 `harness_round2.rs` 6 项：白名单直执/联网闸门/docker 约束/grep 上下文/read 图像/bg list 会话隔离与寻址）。
- `cargo test -p exm-gateway`：**113 全过**（lib 109 + 集成 4）。
- `cargo clippy -p exm-core -p exm-gateway`：**0 warning**。
- `webui`：`tsc --noEmit` 通过；`npm run build` 通过（ChatView 38.08 kB gzip 12.91 kB）。

### harness_round2 测试明细

| 用例 | 断言要点 |
|---|---|
| 白名单扩充_新命令免审批直执 | stat/jq/tar/sqlite3/awk 直执无审批单 |
| 白名单扩充_联网命令仍受沙箱闸门 | strict+未联网：curl/ssh 转审批；is_network_command 覆盖 rsync/sftp/git push/docker pull；本地命令（stat/jq/tar）不误伤 |
| 白名单扩充_docker子命令约束 | docker ps 白名单命中但联网特征拦截（docker 需外联守护进程，strict 下转审批） |
| grep_上下文行_块输出 | contextLines=2：命中行 `>` 标注、上下文带行号、多块 `—` 分隔；默认参数输出格式逐字不变 |
| read_图像_副产物回灌 | png 读成功、images 携带路径、data URL 链路通、非图像二进制仍报错 |
| 后台任务_list_跨轮次寻址 | 会话 A 启动 bg 任务 → B 的 list 不可见（隔离）→ A 的 list 含 taskId/命令/状态 → kill 收尾 |

## 4. WebUI 计划卡片验收

- 路由 `GET/POST /api/sessions/:id/plan`（platform.rs 既有）零改动复用；`plan.item/pending/approved/rejected/applied` 事件经 store 处理实时刷新（300ms 防抖合并）。
- ChatView：会话头下方 Segmented 执行/计划开关；计划卡片逐条 `#序号 [ACTION] target`，edit 条目 old/new_string 红绿着色，patch 全文按 diff 语法着色，terminal/computer 条目命令块，filesystem write 条目全文绿色预览；超 40 行折叠可展开。
- 批准（Popconfirm 二次确认）→ 卡片切回放结果视图（逐条 ✅/❌ + 成功/失败计数摘要）；拒绝 → 清单保留、卡片变灰（plan-card-done）。
- 切会话即重置 plan 状态并拉取新会话清单。

## 5. 部署实测记录（2026-10-30，容器重建后实机走查）

镜像重建（含 Dockerfile 补 jq/sqlite3/xxd）+ 巡检 26/26 全过后，按走查单执行：

### A 组 terminal 白名单（machina 直聊）

| # | 场景 | 结果 |
|---|---|---|
| A1 | `jq --version` | ✅ 免审批直执，回传 `jq-1.6` |
| A2 | `sqlite3 --version` | ✅ 直执回传 `3.40.1` |
| A3a | workspace 沙箱 `curl example.com` | ✅ 直执返回 200，无审批单（bwrap 隔离兜底） |
| A3b | strict 沙箱（未开联网）同命令 | ✅ 转审批单（pending=1）；批准后系统代执行 status=executed result=200 |
| A4 | 后台 `sleep 60` → `op=list` | ✅ 返回 `taskId=bg-cf95ade6 [运行中（已 1s）]`——跨轮次寻址贯通 |

### B 组 计划模式全链路（REST 层模拟 WebUI 行为）

| # | 场景 | 结果 |
|---|---|---|
| B1 | setMode plan | ✅ mode: plan |
| B2 | edit + terminal 写操作 | ✅ 双条目拦截（args 完整：old/new_string、command），清单 count=2 |
| B3 | 批准回放 | ✅ 2/2 ok；文件实际改为 line2 done；会话切回 execute、清单清空 |
| B4 | 拒绝保留 | ✅ kept=1、line3 未动、仍在 plan 模式 |
| B5 | 跨端审批单联动 | ✅ `[plan] 会话计划清单` 审批单随 REST 批准收口为 executed（CLI/WebUI/通道共享状态） |

### C 组 检索与图像

| # | 场景 | 结果 |
|---|---|---|
| C1 | grep contextLines=2 | ✅ 输出 `t.txt:2:> line2 target` + 上下文行号 + 块间 `t.txt—` 分隔（模型如实贴出，块语义正确） |
| C2 | read shot.png | ✅ 返回 `图像文件：shot.png（png，68 字节）` + images 回灌声明（视觉模型判定链路由本地测试覆盖；当前部署模型非视觉时不回灌属预期） |

走查中 machina 主动指出 grep 输出的「块分隔」形态并如实报告未收到图像回灌——不猜测不编造，PROMPT 协议生效的旁证。

## 6. 遗留与后续

- **spawn 子智能体并行化**（异步任务面）与 **TodoWrite 任务清单工具**未纳入本轮，作为下轮候选。
- read 图像的视觉回灌在部署环境的端到端体验（生成截图→模型描述画面）待视觉模型接入后走查。
