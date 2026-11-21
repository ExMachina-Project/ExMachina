# knowledge-base Specification

## Purpose
跨智能体共享知识库：与个体记忆分离的「通用知识层」——知识是可复用的客观沉淀（部署约束、平台特性、方法论、踩坑结论），不属于任何个体或会话；全部智能体共享读写，支持模型工具自我积累与管理面治理。

## Requirements

### Requirement: 知识条目与全局共享语义

系统 SHALL 提供独立于记忆的 KnowledgeStore：条目含 `id`、`title`、`body`（Markdown）、`tags`、`source`（产出智能体标识 + 来源会话引用）、`hits`（检索命中计数）、`createdAt`、`updatedAt`。知识库 MUST 实例内全局共享：任何智能体（指挥体/子个体/独立智能体）读写同一集合，MUST NOT 按智能体、组或会话分区（与 MemoryStore 的分区语义明确分离）。

- 内容哈希去重：与既有条目正文哈希相同的写入 MUST NOT 新建条目，改为提升原条目 `hits` 并刷新 `updatedAt`（返回去重标记）；
- `title` + `body` 均为必填；`body` 长度护栏默认 8000 字符（超限拒绝并提示拆分为多条）。

#### Scenario: 跨智能体共享

- **WHEN** 子个体 creative-agent 经工具写入一条知识，随后另一会话中指挥体检索相关主题
- **THEN** 指挥体能命中该条目（无需任何共享配置），`source` 标注原写入个体与会话

#### Scenario: 重复内容去重

- **WHEN** 任一智能体写入与既有条目正文完全相同的内容
- **THEN** 不新建条目，原条目 `hits` 与 `updatedAt` 更新，工具返回「已存在（去重）」与原条目 id

### Requirement: 模型知识工具面（自我积累）

系统 SHALL 提供模型可调用的知识工具：`knowledge_write`（title/body/tags → 落库或去重提升）与 `knowledge_read`（`op=search` 按查询检索 top-k、`op=get` 按 id 取全文、`op=list` 最近条目）。两工具 MUST 对全部智能体下发，不受个体工具档与记忆分区影响；工具描述 MUST 注入沉淀纪律（存通用可复用的客观知识，不存会话私有细节与个人信息）。

- `knowledge_write` 成功 MUST 回显条目 id 与「新建 / 去重提升」状态；
- `knowledge_search` 命中 MUST 累加条目 `hits`（积累热度信号）。

#### Scenario: 检索命中计数

- **WHEN** 模型连续多次检索命中同一条知识
- **THEN** 该条目 `hits` 随命中累加，`op=list` 可按热度排序参考

#### Scenario: 正文超限

- **WHEN** `knowledge_write` 的 body 超过 8000 字符
- **THEN** 工具拒绝并提示拆分为多条更聚焦的知识

### Requirement: 知识管理面（REST / CLI / WebUI）

系统 SHALL 提供知识库管理三面，口径一致：

- REST：`GET /api/knowledge`（清单 + `q=` 检索参数）、`POST /api/knowledge`（新增）、`DELETE /api/knowledge/:id`（删除）；
- CLI：`exm knowledge list/search/add/delete` 同套语义；
- WebUI：知识库面板（清单 / 检索调试 / 新增 / 删除），交互风格与记忆面板一致；
- 新端点 MUST 收录 OpenAPI 清单与《协议与契约》文档。

#### Scenario: REST 全链路

- **WHEN** 经 `POST /api/knowledge` 新增一条知识，再 `GET /api/knowledge?q=<关键词>`
- **THEN** 清单含新条目（source 标注 manual），检索按相关性与热度合排

#### Scenario: CLI 与 REST 同源

- **WHEN** `exm knowledge add` 与 REST POST 写入同题内容
- **THEN** 两者落同一存储，去重语义一致（后者去重提升而非新建）

### Requirement: 提示词注入（可选，默认关）

系统 SHALL 支持 `knowledge.injectTopK` 配置（默认 0）：为 0 时构建系统提示 MUST NOT 注入任何知识内容（零行为变化）；大于 0 时构建派发/直答系统提示 MUST 按当前任务目标检索 top-k 条知识注入（每条截断上限可配，标注「知识库参考」与来源），检索失败或超时 MUST 静默跳过，不得阻塞会话。

#### Scenario: 默认零注入

- **WHEN** `knowledge.injectTopK` 未配置（或为 0）
- **THEN** 系统提示与未引入知识库前完全一致

#### Scenario: 开启后注入

- **WHEN** `knowledge.injectTopK = 3` 且知识库含命中条目
- **THEN** 系统提示附最多 3 条相关知识摘录（截断标注），检索失败时正常继续不注入
