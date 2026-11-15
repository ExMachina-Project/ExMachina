# 验收：UI 品牌对齐（ui-brand-alignment）

> 变更：`openspec/changes/ui-brand-alignment`
> 基线：dev 分支 `a216bfe` + `f0ef98f`（todo 回放修复）
> 对齐源：`ExMachina-Project/.github` profile/assets 五张 SVG（hero/swarm/snapshot/language/activity）

## 1. 设计语言提取（资产 → 令牌）

| 资产元素 | 值 | WebUI 令牌 |
|---|---|---|
| 卡片底 | `#0B0D10` | `--bg` / `--bg-panel` |
| 描边 | `#20262F` | `--line` |
| 层叠槽 | `#161B22` | `--bg-raise` |
| 亮白主角 | `#F2F5F9` | `--text` / `--text-hi`（提亮，原 #d4dae5 降级） |
| 次级灰 | `#9AA3B0` | `--muted` |
| 灰阶梯度 | F2F5F9→AEB7C4→7C8695→59626F | `--rank-1..4`（新增） |
| 品牌紫 | `#7C8CFF`（点缀） | `--accent`（用量收敛） |
| 亮紫/光晕 | `#A9B4FF` / 16% | `--accent-bright` / `--accent-halo`（新增） |
| 等宽结构文本 | ui-monospace 系 | `.label-caps`/`.stat-num`/`.section-comment` + 组件迁移 |
| 双层状态点 | r4.6 光晕 + r2.2 实心 | `.dot-halo` + LED/live-dot 升级 |
| 直角条形 | h9 叠槽色、主条亮白 | `.bar-track`/`.bar-fill` + todo-bar 迁移 |
| 注释风标题 | `// SNAPSHOT` | `.section-comment` + 空态标题 |

## 2. 改动清单

| 面 | 内容 |
|---|---|
| token | midnight 六值精确对齐 + rank/accent-bright/accent-halo 新增；minimal/graphite 不动 |
| 工具类 | .label-caps / .stat-num / .section-comment / .dot-halo / .bar-track+.bar-fill |
| 结构文本 | 侧栏导航、页头/面板标题、主题选项 → 等宽 |
| 紫色收敛 | 侧栏选中态紫底 → 亮白左指示（inset box-shadow）；todo 条绿填充 → 亮白 |
| 状态点 | tbar-led / usage-live-dot 升级双层圈（::before 光晕） |
| 空态 | 标题 `// ` 前缀 + 光标闪烁动画 + 等宽亮白（AWAITING DIRECTIVE） |
| 行为修复 | todo 切会话回放（selectSession 时事件尾部找回最近 todo.updated，提交 f0ef98f） |

## 3. 验证记录（部署实例浏览器 DOM 走查）

| # | 验证点 | 结果 |
|---|---|---|
| V1 | token 生效 | ✅ bg #0b0d10 / text #f2f5f9 / rank1-4 / accent-bright #a9b4ff / halo rgba(124,140,255,.16) |
| V2 | 侧栏结构文本等宽 | ✅ JetBrains Mono 生效 |
| V3 | 选中态亮白指示 | ✅ `rgb(242,245,249) 2px 0 0 0 inset`（原紫底已移除） |
| V4 | LED 双层圈 | ✅ ::before 光晕 rgba(124,140,255,0.16) 渲染 |
| V5 | 空态注释风标题 | ✅ `// ` 前缀 + acquire-caret 闪烁 + mono + 亮白 |
| V6 | todo 卡（回放后） | ✅ H9 直角条 / 填充 rgb(242,245,249) 亮白 / chip 2/2 / 进度标签 |
| V7 | 遥测条读数等宽 | ✅ JetBrains Mono |
| V8 | minimal 回归 | ✅ 原 #fafafa 色板、无 rank 注入，切换正常 |
| V9 | 主题往返 | ✅ minimal → midnight 恢复 #0b0d10 |

发现并修复：todo 快照纯 WS 事件维护导致切会话/刷新丢卡 → selectSession 事件尾部回放（V6 验证）。

## 4. 遗留与后续

- antd 弹层（下拉/tooltip 内部）保持原字体——深层覆盖性价比低，接受过渡期差异。
- spawns 卡片切会话回放（与 todo 同构，未做）。
- 浅色主题品牌化（当前仅 midnight 对齐，按规格保留 minimal 现状）。
