# 验收：agent 能力第三轮（harness-agent-round3）

> 变更：`openspec/changes/harness-agent-round3`（TodoWrite 任务清单 + spawn 后台并行化）
> 基线：dev 分支 `e7b91d0`（harness-tools-round2 归档后）

## 1. 改动清单

| # | 项 | 落点 |
|---|---|---|
| 1 | TodoWrite 清单 | `crates/exm-core/src/todo.rs`（新，write/list/render + 4 单测）+ `ToolName::Todo` + `tool_todo`（write 全量重写/list，todo.updated 事件）+ runtime 系统提示注入（30 条截断）+ 5 个体工具面 |
| 2 | spawn 后台并行 | ToolGateway `spawns_bg` 注册表（SpawnBgTask/终态/已读标记）+ background 分支（许可随任务体）+ op=status/list + `spawn_pending_note` 未回收提醒注入（限 5） |
| 3 | 前端呈现 | store todo.updated 快照；ChatView 进度条 + chip 带；CLI TUI todo/spawn 后台事件行；timeoutSecs 下限 5 |

## 2. 设计要点

- **全量重写语义**：todo write 一次调用替换整个清单（现代 harness 惯例），避免条目级 CRUD 碎片调用；空数组 = 清空。
- **同步语义零破坏**：不带 background 的 spawn 走原路径（提示词存量兼容）；spawn.started/finished 事件只增字段（background 标记）。
- **并发池共用**：后台许可在任务体内持有至终结，同步+后台合计不超既有 semaphore 上限。
- **未回收提醒**：终态未读的子任务下一轮注入「结果待收」（status/list 读取即消），模型不主动查询也不丢结果。

## 3. 测试记录

- `cargo test -p exm-core`：**243 全过**（lib 197 + 集成 46；新增 harness_round3 5 项）。
- `cargo test -p exm-gateway`：**109 全过**。
- `cargo clippy`（core/gateway/cli）：**0 warning**；webui `tsc` + `build` 通过。

### harness_round3 明细

| 用例 | 断言要点 |
|---|---|
| todo_写入查询与事件 | write 全量重写/进度统计/渲染符号（✅🔄⬜）/list/空 content 拒绝/清空后不注入 |
| todo_编码个体工具面含todo | coding-agent/orchestrator/frontend-agent 均含 |
| spawn_后台派生_立即返回并回收 | 立即返 spawnId → status 收全文 → 已读不提醒 → list 可见 → 会话隔离 |
| spawn_后台超时终态与未读提醒 | timeoutSecs=5 慢执行器 → 超时终态可查 |
| spawn_未读终态进入提醒注入 | 未读时 spawn_pending_note 含任务预览；他會話不受影响 |

## 4. 部署实测记录（2026-10-30，容器重建后实机走查）

巡检 26/26 全过后，machina 直聊实测（两轮镜像：第二轮补 spawn 个体接入）。

### 发现与修复：spawn 无个体下发

第一轮实测发现 machina 报「没有 spawn 工具」——全库无任何个体配置 spawn（此前面只有测试注入）。核心 agent 能力从未接入部署面。已补：machina（全能单聊体）与 orchestrator（组内主智能体）工具面加 spawn（提交 `7e55452`）。

### todo 自组织（三步任务）

| # | 场景 | 结果 |
|---|---|---|
| T1 | 建清单 + 状态流转 | ✅ 两次 write（初始态/全完态）正确理解**全量重写**语义并复述 |
| T2 | 下一轮注入验证 | ✅ 逐字引用注入段（`## 当前任务清单（3/3 完成）` + ✅ 条目 + 全完提示语），并诚实指出注入措辞与用户原始描述的差异 |

### spawn 后台并行

| # | 场景 | 结果 |
|---|---|---|
| S1 | 双后台派发不阻塞 | ✅ sp-c3ebb345/sp-77296282 立即返回；machina 派完不等、继续执行 terminal、本轮正常收束 |
| S2 | 未回收提醒自动注入 | ✅ 下轮上下文含「## 后台子任务结果待收」（两任务终态摘要 48s/22 行/28 文件），machina 逐字引用并转述——不主动查询也不丢结果 |
| S3 | status 收全文 | ✅ 两个子任务完整结果回收，一句话汇总正确 |

machina 在无 spawn 时如实报告能力边界并用 terminal background 等价并行（第一轮实测的意外收获——诚实性 + 适应性都符合 PROMPT 协议）。

## 5. 遗留与后续

- spawn 后台任务的 WebUI 卡片化（当前仅 CLI 事件行 + 系统注入面）。
- filesystem delete/rename op（下轮候选）。
