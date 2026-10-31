---
id: commit-message
name: 提交信息规范
description: 写高质量 git 提交信息：原子提交 + 规范格式
triggers: [提交, commit, 提交信息, commit message]
---

写提交信息前先 git diff --stat 确认本次变更范围。规则：
1. **原子性**：一个提交只做一件事；混合了不相关变更时建议拆分（git add -p 按块暂存）。
2. **标题行**：≤50 字符，祈使句（做什么而非做了什么），不加句号。
3. **正文**：解释为什么（背景/动机），而非复述 diff；引用相关 issue/任务号。
4. **格式**：`<类型>: <摘要>`，类型取 feat/fix/refactor/docs/test/chore/perf。
5. **破坏性变更**：标题或正文含 BREAKING CHANGE 段说明迁移路径。
示例：
```
fix: 会话压缩在空历史时误触发

空会话 list_messages 返回空表，saturating_sub 后 older 也为空，
但 prev_upto(0) == upto(0) 判定跳过，导致首次压缩永远不执行。
改为 upto > prev_upto 才判定已压过。
```
