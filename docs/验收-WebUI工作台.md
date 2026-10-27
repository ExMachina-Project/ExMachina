# WebUI 代码工作台手动验收清单（任务 6.5）

> **[已废弃]** 独立「代码」视图已移除（对话即编码：@ 文件提及 + 工具卡 diff/回滚承担全部编码体验）。
> 本文档保留作历史验收记录；其中会话流 diff 卡片、终端面板、回滚等条目仍有效（见 `docs/验收-对话即编码.md`）。
>
> 对象：WebUI「代码」视图（CodeWorkbenchView）+ 会话流 diff 审查卡片 + 终端产出面板。
> 后端：`GET /api/workbench/tree`、`GET /api/workbench/file`（e2e 已覆盖树/分页/越权拒绝）。

## 验收记录

| # | 路径 | 操作 | 预期 | 结果 |
|---|------|------|------|------|
| 1 | 文件树浏览 | 侧栏「代码」→ 树展开目录 → 选中文件 | 树形目录（目录优先排序，排除 .git/target/node_modules），点击文件右侧分页展示内容（每页 400 行） | ✅（e2e：树含 src/main.rs、不含 .git；分页 offset=2 取第二行） |
| 2 | 路径过滤 | 过滤框输入关键字 | 平铺路径列表实时过滤（前 200 条） | ✅（TS 编译 + 手走查） |
| 3 | 引用到会话 | 选中文件 → 「在会话中引用」 | 路径插入当前会话输入草稿（不清空已有草稿），自动跳转对话页 | ✅（insertRef 追加 draft + requestNav("chat")） |
| 4 | diff 卡片 | 会话中智能体执行 edit/patch | 工具卡展开显示逐行着色 diff（`-` 红 / `+` 绿） | ✅（DiffView 既有路径保留） |
| 5 | 回滚 | edit/filesystem.write/git 写卡片上「回滚」按钮 | 调 `POST /sessions/{id}/undo`（restoreFiles=true）→ 检查点联动恢复文件，卡片标记「已回滚」，消息流截断到该轮前 | ✅（undoSession 调用 + turn 标注：收束消息自动带轮次号） |
| 6 | git 卡片 | git 写动作（commit 等） | 同回滚类卡片；动作摘要 `action pathSpec branch` | ✅（callTitle git 分支 + rollbackable 含 git 写动作） |
| 7 | 终端面板 | terminal 工具卡展开 | 分区呈现：命令行（等宽字体）/ 输出区（长输出折叠至 40 行 + 计数提示）/ 退出码 Tag；后台任务显示 taskId + 轮询说明 | ✅（TerminalView 组件 + styles） |
| 8 | 移动端窄屏 | 375px 宽度打开「代码」 | 文件树 240px 固定 + 内容区自适应；侧栏折叠为汉堡菜单（既有响应式布局继承） | ✅（flex 布局 + minWidth 240；App 既有窄屏导航） |
| 9 | 越权拒绝 | 地址栏直击 `?path=../../../etc/passwd` | 400 错误（工作区边界校验在服务端 resolve_safe） | ✅（e2e 断言 400） |

## 已知边界

- 文件树深度上限 6 层 / 2000 节点（防大仓全量遍历；深层用路径过滤框直达）。
- 只读工作台：编辑仍走会话内 edit/patch 工具（审批闸门与检查点联动不绕过）。
- stdout/stderr 在工具层合流返回，面板按整段渲染（分区着色待工具层分离流后启用）。
