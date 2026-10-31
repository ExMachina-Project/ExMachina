# 验收：filesystem delete/rename + spawn 后台 WebUI 卡片（fs-ops-spawn-ui）

> 变更：`openspec/changes/fs-ops-spawn-ui`
> 基线：dev 分支 `95e177d`（harness-agent-round3 归档后）

## 1. 改动清单

| # | 项 | 落点 |
|---|---|---|
| 1 | filesystem delete | tool_fs `op=delete`：整树移入 `.exmachina/trash/<日期>/<相对路径>`（同日同名 `.2` 序号）；工作区根 / `.exmachina` 系统区拒绝 |
| 2 | filesystem rename | tool_fs `op=rename`：newPath 安全校验 + 目标存在拒绝（不覆盖）+ 源检查点 + 父目录自动创建 |
| 3 | schema 更新 | op enum 加 delete/rename；newPath 参数描述教学（delete 可恢复 / rename 不覆盖） |
| 4 | WebUI spawns-strip | store spawns 状态（spawn.started/finished 事件维护，会话过滤 + 20 上限）；ChatView 卡片（running spinner / 终态着色 + 预览展开 + 清除） |

## 2. 设计要点

- **回收站语义而非裸删**：delete 是可恢复操作（人工从 trash 取回），高危性降级——与 write/mkdir 同级不进审批闸门。
- **保护区**：`.exmachina` 整目录（检查点/回收站/沙箱）禁经 filesystem 删除；确需清理走终端过审批。
- **rename 不覆盖**：目标存在直接拒绝（提示先 delete），避免静默破坏；源文件检查点先行，undo 链完整。
- **plan 模式零改动覆盖**：既有口径「filesystem 非 list 即拦」自动包含 delete/rename。
- **WebUI 卡片只跟踪后台派生**：同步 spawn 的结果在工具调用内直接可见，不重复建卡。

## 3. 测试记录

- `cargo test -p exm-core --lib`：**202 全过**（新增 fs-ops 5 项）。
- `cargo test -p exm-gateway --lib`：**109 全过**。
- `cargo clippy`（core/gateway/cli）：**0 warning**；webui `tsc` + `build` 通过。

### fs-ops 单测明细

| 用例 | 断言要点 |
|---|---|
| fs删除_移入回收站可恢复 | 原位消失 + trash 按日期目录可找到 + 重复删除报不存在 |
| fs删除_目录递归与同名序号 | 目录树整体移入 + 同日二次删除带 `.2` |
| fs删除_保护区拒绝 | `.exmachina` 系统区与工作区根均拒绝 |
| fs重命名_成功不覆盖与跨目录 | 内容保持 + 目标存在拒绝（原内容不破坏）+ 跨目录父目录自动创建 |
| fs删除重命名_plan模式拦截 | 两 op 均转计划条目不真执行 |

## 4. 部署实测记录（2026-10-30，容器重建后浏览器 DOM 走查）

巡检 26/26 全过。

### 发现与修复（两处真实缺陷）

| # | 缺陷 | 根因 | 修复 |
|---|---|---|---|
| 1 | delete 报 `Invalid cross-device link (os error 18)` | `.exmachina` 是独立挂载卷（docker volume），`fs::rename` 跨设备 EXDEV | rename 失败 fallback `copy_tree` 逐条目复制后删源（提交 `b2cd40c`） |
| 2 | WebUI 看不到 spawn.finished 事件 | 后台任务体只 `events.send`（广播）未 `append_event`（落 store），REST 事件面/刷新后不可见 | 任务体内补 Envelope 持久化（提交 `d5dc144`，集成断言覆盖） |

### delete/rename 走查（machina 直聊）

| # | 场景 | 结果 |
|---|---|---|
| D1 | 删除目录树（fs-test-dir 含嵌套） | ✅ 整树移入 `.exmachina/trash/2026-10-29/`，原位消失 |
| D2 | 删除单文件 | ✅ 移入回收站，返回回收站路径说明 |
| D3 | 删除 .exmachina 系统区 | ✅ 拒绝并提示走终端过审批 |
| D4 | 回收站恢复演练 | ✅ trash 内文件内容完整（v1/x），cp 取回可用 |
| R1 | rename 成功 | ✅ restored.txt → final.txt，内容保持 |
| R2 | rename 目标已存在 | ✅ 拒绝不覆盖，两文件内容双保全，machina 复核目录后如实汇报 |

machina 对 EXDEV 失败的第一次汇报：如实引用错误原文、未虚构成功（诚实性旁证）。

### spawn 事件面走查

双后台派发（sp-850ff0a8/sp-17a7501e）：spawn.started（含 background 标记）入事件流 ✅；修复后 spawn.finished 持久化可查 ✅。WebUI 卡片渲染在 ui-brand-alignment 变更走查中一并覆盖（todo 卡验证通过，spawns 卡沿用同事件通道）。

## 5. 遗留与后续

- 回收站自动清理/配额（当前人工恢复语义；可加 cron 巡检）。
- WebUI spawns 卡片切会话回放（与 todo 回放同构，本轮未做——事件驱动可接受）。
- copy_tree 无进度反馈（大目录删除静默较久，可加条目计数回显）。
