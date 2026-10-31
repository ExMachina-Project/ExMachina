# git-tools Specification

## Purpose
Git 工具组：git 多合一工具的动作面、审批归类与检查点联动的行为契约，让编程工作流中的版本控制操作无需退化为裸终端命令。

## Requirements

### Requirement: git 多合一工具
系统 MUST 提供 git 工具，支持只读动作（status/diff/log/branch 列表/show/stash 列表）与写动作（add/commit/restore/stash push/stash pop/checkout -b）。动作与参数 MUST 在工具 schema 中声明，输出为结构化文本。仓库不在工作区内时 MUST 明确报错。

#### Scenario: 只读状态查询
- **WHEN** 智能体调用 git 工具的 status 动作
- **THEN** 返回工作区仓库的状态摘要（分支、暂存、改动文件列表）

#### Scenario: 非仓库报错
- **WHEN** 工作区目录不是 git 仓库且智能体调用 git 工具
- **THEN** 返回明确错误说明工作区无仓库

### Requirement: 写动作审批闸门
git 写动作（add/commit/restore/stash/checkout -b）MUST 归入终端写操作同级的审批档位：审批模式为严格时需人工批准，宽松时直接执行，且 commit 的 --no-verify 与 push 类破坏性动作在任意档位下 MUST 需人工审批。push 动作 MUST NOT 由 git 工具直接提供（超出工作区边界）。

#### Scenario: 严格模式 commit 需审批
- **WHEN** 审批模式为严格且智能体调用 commit
- **THEN** 产生审批单，批准后执行提交

#### Scenario: push 不在动作面
- **WHEN** 智能体尝试通过 git 工具执行 push
- **THEN** 工具返回该动作不支持的说明（push 需经终端走既有审批链）

### Requirement: 与检查点系统联动
git 写动作执行前 MUST 与文件编辑工具共用同一检查点机制（可回滚到动作前状态）。检查点记录 MUST 标注来源为 git 动作。

#### Scenario: commit 前检查点
- **WHEN** git commit 执行成功
- **THEN** 系统在执行前记录检查点，来源标注为 git 动作，可回滚
