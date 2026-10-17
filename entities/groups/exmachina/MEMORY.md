# MEMORY · exmachina

<!-- 组浅层记忆（自动渲染的人读快照，容量有限）：深层记忆在记忆库（检索/衰减/压缩），编辑请走记忆面板或 exm memory -->

- 更新时间：2026-10-14T19:23:34.004Z
- 组域记忆 8 条（置顶 1）

## 置顶（核心）

- **构建入口**（fact｜重要性 0.60）
  cargo build --release
## 近期重要

- **侦察体私有教训**（lesson｜重要性 0.90）
  入口侦察需先确认缓存目录
- **任务边界裁决：## 最近对话
[用户] 设计一个支持 1000 个智能体的集群调度方案

设计**（decision｜重要性 0.70 `decision` `boundary`）
  范围内：当前会话目标；用户提供的材料 禁区：不可逆变更；越权调度；无证据结论 验收：产出带证据等级的结论；残余未知显式保留
- **证据：coding-agent / 汇报收束**（evidence｜重要性 0.60 `evidence` `coding-agent`）
  [B|reasoning] mock://coding-agent/T4 — 模拟通道占位证据；真实通道下为代码/日志/测试等直接证据。
- **证据：api-agent / 方案比对**（evidence｜重要性 0.60 `evidence` `api-agent`）
  [B|reasoning] mock://api-agent/T3 — 模拟通道占位证据；真实通道下为代码/日志/测试等直接证据。
- **证据：architect-agent / 入口侦察**（evidence｜重要性 0.60 `evidence` `architect-agent`）
  [B|reasoning] mock://architect-agent/T2 — 模拟通道占位证据；真实通道下为代码/日志/测试等直接证据。
- **证据：api-agent / 背景补齐**（evidence｜重要性 0.60 `evidence` `api-agent`）
  [B|reasoning] mock://api-agent/T1 — 模拟通道占位证据；真实通道下为代码/日志/测试等直接证据。
- **会话摘要：## 最近对话
[用户] 设计一个支持 1000 个智能体的集群调度方案

设计**（digest｜重要性 0.45 `session` `digest`）
  目标：## 最近对话 [用户] 设计一个支持 1000 个智能体的集群调度方案  设计一个支持 1000 个智能体的集群调度方案 路由：L2 节点：T1(api-agent):done、T2(architect-agent):done、T3(api-agent):done、T4(coding-agent):done 收束要点：任务边界；目标：以用户输入为准；排除项：不可逆变更与越权动作。；已启用链路
