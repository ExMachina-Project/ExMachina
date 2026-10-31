# filesystem-ops Specification

## Purpose
TBD - created by archiving change fs-ops-spawn-ui. Update Purpose after archive.

## Requirements

### Requirement: delete 与 rename 操作
filesystem 工具 MUST 支持 `op=delete` 与 `op=rename`（newPath 参数）：

- delete：目标移入 `.exmachina/trash/<日期>/<相对路径>`（保留原内容供人工恢复；同日同名自动加序号后缀），返回回收站路径。目录递归同语义（整树移入）。MUST 拒绝删除工作区根、`.exmachina` 自身及回收站目录。
- rename：newPath 经与 path 相同的安全校验（工作区内、不逃逸）；目标已存在 MUST 拒绝（不覆盖）；移动 MUST 保持原文件内容；源路径写前检查点（undo 链不断）。

两 op 属写类：plan 模式下 MUST 被拦截为计划条目（既有 filesystem 非 list 即拦的口径自动覆盖）。与 write/mkdir 同级不进审批闸门（delete 有回收站兜底、rename 可逆）。

#### Scenario: 删除进回收站
- **WHEN** 智能体 `filesystem(op=delete, path=tmp/build.log)`
- **THEN** 文件从原位消失，出现在 `.exmachina/trash/<今日>/tmp/build.log`，返回回收站路径说明

#### Scenario: 目录递归删除
- **WHEN** 删除一个含子文件的目录
- **THEN** 整树移入回收站对应相对路径

#### Scenario: 删除保护区拒绝
- **WHEN** 尝试删除工作区根或 .exmachina 目录
- **THEN** 拒绝并说明保护区

#### Scenario: 重命名不覆盖
- **WHEN** `filesystem(op=rename, path=a.txt, newPath=b.txt)` 且 b.txt 已存在
- **THEN** 拒绝执行，提示目标已存在

#### Scenario: plan 模式拦截
- **WHEN** 计划模式下调用 delete/rename
- **THEN** 转为计划条目不执行（批准回放时才落地）
