# MEMORY · exmachina

<!-- 组浅层记忆（自动渲染的人读快照，容量有限）：深层记忆在记忆库（检索/衰减/压缩），编辑请走记忆面板或 exm memory -->

- 更新时间：2026-10-28T19:45:24.058Z
- 组域记忆 17 条（置顶 0）

## 置顶（核心）

_（暂无）_

## 近期重要

- **任务边界裁决：向用户交付 ./self-intro.html 的文件路径，并产出该页面的本地渲**（decision｜重要性 0.70 `decision` `boundary`）
  范围内：./self-intro.html 的只读访问与绝对路径确认；以 file:// 本地方式渲染页面并截图，产物仅写入 ./self-intro-screenshot.png（独占写入路径）；向用户回报 HTML 文件与截图的绝对路径 禁区：不修改 ./self-intro.html 及工作区内任何既有文件；不引入外部网络资源、不启动 HTTP 服务（仅本地文件渲染）；不执行安装、构建等全局副作用操作；不触碰 ./self-intro-screenshot.png 以外的新增写入路径 验收：给出 ./self-intro.html 绝对路径，且文件存在、大小>0；./self-intro-screenshot.png 存在、为 PNG 格式、大小>10KB；截图可见页面标题与内容区块的正常渲染；工作区内既有文件零改动
- **任务边界裁决：交付一个自包含的静态自我介绍 HTML 页面（介绍 ExMachina 集群），**（decision｜重要性 0.70 `decision` `boundary`）
  范围内：新建单个自包含静态文件 ./self-intro.html：介绍 ExMachina 集群的自我介绍页；页面文案与内联样式编写，中文内容，浏览器直接打开即可查看 禁区：不修改 ./self-intro.html 以外的任何既有文件；不引入外部 CDN、字体、图片、脚本等任何网络资源；不启动 HTTP 服务、不做部署（本轮仅交付本地文件）；不执行任何安装、构建或全局副作用操作 验收：./self-intro.html 存在且为单文件自包含页面（CSS 内联，无外链）；文件内不含任何 http://、https:// 或协议相对 // 的资源引用；页面包含四类内容区块：集群名称与定位、职能域清单、工作原则、页脚说明；经独立个体核验通过（实现者不自证）
- **任务边界裁决：产出一份支持 1000 个智能体的集群调度设计方案终稿：对象=调度架构设计文档，**（decision｜重要性 0.70 `decision` `boundary`）
  范围内：./design/ 目录下新建调度方案文档（草稿、终稿、反证报告、审核报告四个独占写入路径，互不重叠）；对本集群现行调度机制（单指挥体直派、全连结消息总线、SyncReport 回流契约、注册表约 40 个职能个体、P0–P3 优先级、L0–L3 路由）的只读参照分析；会话编号 1792264142459 作为交付追踪标签写入文档头部 禁区：不修改集群现有运行代码、配置与个体注册表；不实际扩容、部署或启动任何服务进程；不执行真实压测；容量估算仅做量化推演并标注证据等级；不触碰 ./design/ 以外的任何既有文件 验收：终稿覆盖 5 个必选维度（分层架构、任务分配策略、状态同步机制、故障恢复、容量估算），每维度有独立章节与明确结论；关键决策 ≥3 项，每项附 ≥2 个候选方案对比及取舍理由；失效场景 ≥5 个，每个附明确判定条件与处置/回退路径；容量估算含 1000 规模量化数值依据（消息扇出量级、状态同步频率、存储/带宽量级），数字可追溯至 T1 背景参数或显式标注 C 级假设；终稿附反证检验裁决记录，compare-agent 提出的全部反证意见逐条被吸收或裁决；review-agent 对终稿逐条核验 6 项验收条款，判定为 pass
- **任务边界裁决：对『支持 1000 个智能体的集群调度方案』交付物执行验收：逐项判定 5 个必选**（decision｜重要性 0.70 `decision` `boundary`）
  范围内：./design/ 目录下『支持 1000 个智能体的集群调度方案』交付物（草稿与终稿）的只读核验；对本集群现行调度机制的只读参照分析；验收结论与验收报告的文本产出 禁区：不修改方案文档及工作区内任何既有文件；不实施任何生产代码变更；不执行真实压测、部署或扩容动作；不触碰 ./design/ 以外的任何写入路径 验收：交付物定位结论明确：给出方案文档路径清单，或明确判定产物缺失并列出已检索范围；5 个必选维度逐项有『通过/不通过』判定，每项附文档章节级证据引用；关键决策核验结论量化：决策项数与各决策附带的候选对比数量均给出具体数字；失效场景核验结论量化：场景总数与含判定/处置的场景数均给出具体数字；容量估算核验有结论：1000 智能体规模下数值依据是否充分，判定为充分/不充分并引用出处；最终裁决唯一且三选一；若非『通过』，整改/缺失项可追溯到具体维度与证据等级
- **任务边界裁决：产出一份支持 1000 个智能体的集群调度设计方案终稿：含分层架构（各层管辖上限**（decision｜重要性 0.70 `decision` `boundary`）
  范围内：新建 ./design/ 目录下的调度方案文档（T1 草稿与 T4 终稿，独占写入路径互不重叠）；对本集群现行调度机制（单指挥体直派、全连结消息总线、SyncReport 回流契约、注册表约 40 个职能个体、P0–P3 优先级与 L0–L3 路由）的只读参照分析 禁区：不修改集群现有运行代码、配置与个体注册表；不实际扩容、部署或启动任何新服务进程；不触碰 ./design/ 以外的任何既有文件；文档中不得以『合理/尽量/更好』等不可判定词充当验收或指标 验收：终稿含三层调度架构描述，每层给出具体管辖数量上限数值及推导依据；终稿含 1000 个体规模下的消息扇出量化估算（分层方案与单体直派的量级对比）；个体失联、协调者故障、指挥体故障三类场景各有可执行的检测手段与恢复路径；终稿含量化指标：单协调者管辖上限、调度延迟目标、总线吞吐估算，均为具体数值且相互自洽（各层管辖上限乘积覆盖 1000）；终稿含从现行单指挥体全连结架构到千级目标的分阶段演进路径，每阶段有进入与退出条件；终稿对全部验收条款逐条标注证据等级（A–D），并附非空的风险与未知清单，每条含验证路径
- **任务边界裁决：产出一个支持 1000 个智能体的集群调度方案：对象=调度架构（分层、分配策略、**（decision｜重要性 0.70 `decision` `boundary`）
  范围内：调度方案设计文档：分层架构、任务分配算法、状态同步机制、故障恢复、容量估算；关键决策的候选对比与取舍记录；失效场景清单、修订裁决记录与回退路径汇总 禁区：不实施任何生产代码变更；不执行真实压测或部署动作；不修改现有 ExMachina 运行时代码与配置 验收：方案文档覆盖 5 个必选维度（分层架构、任务分配策略、状态同步机制、故障恢复、容量估算），缺一项即不通过；容量估算给出 1000 智能体下调度延迟与吞吐目标数值，并附估算推导依据（B 级以上证据）；每个关键设计决策（≥3 项）附至少 2 个候选方案对比、取舍理由与回退路径；至少 5 个失效场景（调度器单点、状态脑裂、任务惊群、级联过载、状态不一致）被识别且每个有成立性判定与处置方案；T3 全部修订建议在 T4 被逐条裁决（采纳/驳回），无静默搁置；终稿含残余风险与回退路径汇总
- **任务边界裁决：交付一个介绍 ExMachina 集群的纯静态单页网页，并以 HTTP 服务在局**（decision｜重要性 0.70 `decision` `boundary`）
  范围内：./exmachina-site/ 目录（新建，网页静态资源唯一写入点）；静态 HTTP 服务的启动与停止（用户态后台进程）；本机局域网 IP 的探测与访问 URL 组装 禁区：不占用 80/443 等特权端口，仅使用 8080 起的空闲高端口；不修改系统配置、防火墙永久规则、开机自启项；不触碰 ./exmachina-site/ 以外的任何既有文件；网页不引入外部 CDN/网络依赖，必须离线可打开 验收：./exmachina-site/index.html 存在，为合法 HTML5 文档，离线打开无外部资源请求；页面包含信息块：集群名称 ExMachina、指挥体与子个体协作说明、能力分层（对话层 L0 / 工程层 L1-L3）、主要子个体域清单；HTTP 服务监听 0.0.0.0:<端口>，本机 GET /index.html 返回 200 且响应体非空；通过局域网 IP:端口 GET / 返回 200，响应体包含字符串 ExMachina；向用户交付可复制的局域网访问 URL
- **证据：test-agent / 页面核验回归**（evidence｜重要性 0.60 `evidence` `test-agent`）
  [A|code] ./self-intro.html L1-L6 — doctype、lang=zh、charset=UTF-8、title 逐行直读确认 [A|command] grep -nE 'https?://' ./self-intro.html — 退出码 1、零输出，http(s) 引用计数 0 [A|command] grep -nE '(src|href)\s*=' ./self-intro.html — 退出码 1、零输出，src/href 属性计数 0 [A|code] ./self-intro.html L120-L160 — 四类内容区块行号级证据：L122 h1、L129-139 职能域 9 项、L145-150 原则 4 条、L156-158 footer [A|command] grep -cE '<!DOCTYPE html>|lang="zh"|charset=|<title>' ./self-intro.html — 结构断言计数 = 4（期望 4） [B|command] ls -la — 交付窗口内唯一新增 self-intro.html(4147B, mtime 18:49)；其余条目 mtime 均早于交付；目录级快照非全量 diff，故 B 级
- **证据：frontend-agent / 实现自我介绍静态页**（evidence｜重要性 0.60 `evidence` `frontend-agent`）
  [A|code] ./self-intro.html — 新建自包含静态页：内联 style，语义化结构，含 hero 定位区、职能域清单（9 项）、工作原则（4 条）、纯静态页脚 [A|log] filesystem.write 结果 — written: /app/./self-intro.html（160 行 / 3660 字符） [A|command] grep pattern 'https?://' — 0 命中，验收②外部引用零 [A|command] grep pattern '(src|href)\s*=' — 0 命中，无任何资源加载属性 [A|command] grep pattern 'doctype|<html lang="zh"|charset="UTF-8"|<title>|职能域|工作原则|纯静态页面' — 11 条命中，覆盖验收③④全部结构标记与内容区块 [B|command] glob 'self-intro.html'（写入前） — 无匹配，确认新建无覆盖
- **证据：context-agent / 背景与约束盘点**（evidence｜重要性 0.60 `evidence` `context-agent`）
  [A|code] entities/groups/exmachina/prompts/orchestrator.md L1–L62 — 单指挥体直派、全连结总线、P0–P3 优先级表、L0–L3 路由表、挂载与简报契约 [A|code] entities/groups/exmachina/protocol/13_分派协议.md 全文 — 简报 6 必含项、写入边界互斥、全局副作用单持有、冲突裁决顺序、实施/校验分离、收束前置 [A|doc] entities/groups/exmachina/protocol/README.md L7–L21 — 协议触发词注入机制、单次上限 4、PROTOCOL_INJECT_CAP 引用 crates/exm-core/src/orchestrator.rs（crate 未在本工作区） [A|config] entities/groups/exmachina/agents/（40 个 JSON）+ entities/agents/（1 个）+ playbooks/（22 个）+ prompts/（40 份） — 目录枚举：注册表规模精确为组内 40、顶层 1、链路模板 22 [A|config] entities/groups/exmachina/group.json — builtin 组、primary=orchestrator、描述「指挥体+子个体集群（定义受保护）」 [A|config] .exmachina/config.json L10、L34–41、L88–94、L125–133 — maxConcurrency=4、unitMaxSteps=16、心跳关闭、沙箱禁网、限流关闭 [A|doc] 调度单注入协议 reflow / rollback（inputs[].kind=protocol） — SyncReport 回流字段与不合格口径；回滚自动触发阈值（错误率>5%、响应>2x、可用性<99%） [A|code] entities/groups/exmachina/protocol/04_工作区与协作协议.md 全文 — 工作区边界、跨链路 handoff 契约、共享状态唯一责任 [B|reasoning] protocol/ 目

_（浅层记忆已达容量上限，仅保留前段；完整记忆在深层记忆库）_
