/* Shared gateway contracts, independent of browser or native storage. */
export type { AgentDefinition, ChatMessage, EvidenceItem, QueuedTask, Session, SessionSearchHit, SessionSnapshot, TaskGraph } from "../types";
import type { ModelCost, ModelPurpose, ModelRouting } from "../modelRouting";
export type { ModelCost, ModelPurpose, ModelRouting, ModelAssignment } from "../modelRouting";

/** 密钥掩码哨兵（与后端 llm_admin::KEY_MASK / 通道脱敏同一契约）：提交时收到此值 = 沿用服务端旧值 */
export const KEY_MASK = "***已配置***";
/** 密钥清除哨兵（与后端 llm_admin::KEY_CLEAR 同一契约）：提交时收到此值 = 显式删除该密钥。
 *  空串的历史语义是「沿用旧值」，删除意图只能用此哨兵表达。 */
export const KEY_CLEAR = "__CLEAR__";

export type RequestJson = <T>(path: string, init?: RequestInit) => Promise<T>;

export interface GatewayConfig {
  llm: { baseUrl: string; apiKey: string; model: string };
  maxConcurrency: number;
  /** 会话 token 预算（估算；0 = 不限） */
  maxSessionTokens: number;
  memory: {
    enabled: boolean;
    recallLimit: number;
    halfLifeDays: number;
    /** memory.md 字数上限（超限触发 AI 自主压缩） */
    mdMaxChars?: number;
    /** 语义检索目标："档案ID" 或 "档案ID/模型名"；空 = 仅词项召回 */
    semanticModel: string;
    autoDecay?: boolean;
  };
  compaction?: { triggerRatio: number };
  knowledge?: { injectTopK: number; injectMaxChars: number };
  security: {
    execApproval: string;
    execAllowlist: string;
    /** 后台访问密钥（服务端掩码回显；空 = 免鉴权） */
    authKey?: string;
    /** 终端命令超时（秒）；后台任务不受此限 */
    terminalTimeoutSecs?: number;
    /** 工具结果落盘阈值（字符） */
    toolOutputSpillChars?: number;
    /** 审批等待（秒）：拦截后原地等待人工决定的时长；0 = 直接受阻 */
    approvalWaitSecs?: number;
    /** 执行身份约束（Linux）：终端子进程降权到此用户（用户名或数字 uid；空 = 不约束） */
    execUser?: string;
    /** 圈禁根目录（绝对路径；空 = 自动推导） */
    execHome?: string;
    sanitizeRules?: { fakeSystemPrompt: boolean; hijackDirective: boolean; fakeToolResult: boolean; hiddenUnicode: boolean };
  };
  /** Computer Use（截屏 + 键鼠控制本机；默认关闭） */
  computer?: { enabled: boolean; approval: boolean; maxEdge: number; actionIntervalMs: number };
  automation: {
    heartbeatEnabled: boolean;
    heartbeatIntervalMinutes: number;
    heartbeatPrompt: string;
    autoAdapt: boolean;
    /** 心跳目标个体 identifier（空 = 全局巡检激活组） */
    heartbeatAgent?: string | null;
    /** 子个体单次派发的最大工具步数 */
    unitMaxSteps?: number;
    /** Auto Agent 自评周期（分钟；experiments.autoAgent 开启才生效） */
    autoAgentIntervalMinutes?: number;
    /** Auto Agent 单次行动步数预算（0 = 用 unitMaxSteps） */
    autoAgentMaxSteps?: number;
    /** Auto Agent 空转退避上限（倍） */
    autoAgentBackoffMax?: number;
    /** Meta Agent 巡检周期（分钟；experiments.metaAgent 开启才生效） */
    metaIntervalMin?: number;
    /** 通道流会话闲置归档天数（0 = 关） */
    flowIdleDays?: number;
    /** 长会话记忆归纳触发轮数下限（experiments.autoMemoryDigest 配套） */
    memoryDigestMinTurns?: number;
  };
  /** 联网搜索后端（web_search 工具；未配置则不下发该工具） */
  search?: { provider: string; endpoint: string; apiKey: string; maxResults: number; contentFetchN?: number; backends?: { provider: string; endpoint: string; apiKey: string; weight: number }[] };
  /** 沙箱执行（环境净化 / bwrap / strict 闸门） */
  sandbox?: { mode: string; allowNetwork: boolean; useBwrap: boolean; memoryMb: number; maxProcesses: number };
  /** 浏览器自动化（browser 工具） */
  browser?: { executable: string; headless: boolean; timeoutSecs: number; maxChars: number };
  /** 生命周期钩子 */
  hooks?: { preTool: string[]; postTool: string[]; onRunEnd: string[] };
  /** 工具面（声明式自定义工具） */
  tools?: { custom: Array<Record<string, unknown>> };
  /** 回收站保留契约（保留期 + 容量上限） */
  trash?: { retentionDays: number; maxBytes: number };
  /** 实验性功能开关 */
  experiments?: {
    /** 独立默认关闭；这里只是配置，不代表连接执行已就绪。 */
    connectionFabric?: {
      enabled: boolean;
      maxOperationSecs: number;
      maxCallsPerRoot: number;
      maxPollsPerRoot: number;
      maxConcurrentPerConnection: number;
      maxEventBytes: number;
      maxResultBytes: number;
      maxEventsPerMinute: number;
      maxEventDepth: number;
      maxPendingEvents: number;
      catalogPageSize: number;
    };
    dynamicCompaction: boolean;
    groupSharedContext: boolean;
    llmDrivenMemory: boolean;
    slidingContextWindow: boolean;
    superAgent: boolean;
    autoAgent: boolean;
    metaAgent: boolean;
    autoMemoryDigest: boolean;
  };
  /** 通道身份（配对绑定与群聊唤醒门控） */
  identity?: {
    identityRequired: boolean;
    pairingTtlSecs: number;
    pairingMaxFailures: number;
    pairingLockoutSecs: number;
    groupGateDefault: boolean;
  };
  /** 用量限制（限流与配额） */
  limits?: {
    enabled: boolean;
    windowSecs: number;
    maxRequests: number;
    quotaPeriod: string;
    quotaTokens: number;
    quotaRequests: number;
    quotaRunningPolicy: string;
  };
  /** 事件触发器（文件监听 / 事件 webhook） */
  triggers?: {
    fileWatch: { enabled: boolean; debounceMs: number; exclude: string[] };
    eventWebhook: { enabled: boolean; secret: string };
  };
  /** MCP 服务端（工具对外暴露） */
  mcpServe?: { enabled: boolean; httpPath: string; allowedGroups: string[]; allowedTools: string[] };
  /** 结果推送 */
  notify?: { enabled: boolean; maxChars: number };
  mock: boolean;
}

/** 配置 Schema（后端 config_schema() 输出，设置页据此渲染 ⇒ 新增配置项零 UI 改动） */
export interface ConfigSchemaField {
  key: string;
  label: string;
  kind: "string" | "password" | "number" | "boolean";
  default: string;
  required: boolean;
  help?: string;
  min?: number;
  max?: number;
  /** 条件显示：依赖字段值匹配（equals 单值 / in 多值）才渲染该行 */
  showIf?: { field: string; equals?: unknown; in?: unknown[] };
  /** 组主开关标记：组内其余字段通常以其 showIf 联动 */
  primary?: boolean;
}

export interface ConfigSchemaGroup {
  key: string;
  label: string;
  fields: ConfigSchemaField[];
}

export interface ConfigSchema {
  configVersion: number;
  groups: ConfigSchemaGroup[];
}

export interface MemoryEntry {
  id: string;
  kind: string;
  scope: string;
  sessionId?: string;
  agentId?: string;
  title: string;
  body: string;
  tags: string[];
  importance: number;
  confidence: number;
  pinned: boolean;
  sourceRef?: string;
  createdAt: string;
  updatedAt: string;
  accessCount: number;
  /** 版本号（capability-parity 6.5）：0 = 引入版本字段前的旧条目，展示时按 v1；
   *  编辑/回滚/撤销遗忘都会产生新版本，单调递增。 */
  version?: number;
}

/** 共享知识条目（knowledge-base）：实例级通用知识，全智能体共享不分区 */
export interface KnowledgeEntry {
  id: string;
  title: string;
  body: string;
  tags: string[];
  /** 产出来源：智能体标识（manual = 管理面手写） */
  sourceAgent: string;
  sourceSession?: string;
  /** 检索命中计数（热度信号） */
  hits: number;
  createdAt: string;
  updatedAt: string;
}

export interface PersonaInfo {
  identifier: string;
  persona: string;
  custom: boolean;
  default: string;
}

export interface RecallHit {
  entry: MemoryEntry;
  score: number;
  reasons: string[];
  /** 调试视图：命中的查询词项 → 权重贡献（词项召回路径才有） */
  matchedTerms?: Record<string, number>;
  /** 调试视图：语义相似分数值（0~1 余弦；语义/混合路径才有） */
  semanticScore?: number;
}

export interface AgentStat {
  agentId: string;
  runs: number;
  done: number;
  blocked: number;
  failed: number;
  avgConfidence: number;
  updatedAt: string;
}

/** 组级能力模型覆盖（"档案ID" 或 "档案ID/模型名"）：每组可用不同模型栈；缺省 = 跟随全局槽位 */
export interface GroupCapabilities {
  speech?: string;
  transcribe?: string;
  visionRelay?: string;
  embedding?: string;
  imageGen?: string;
}

export interface TemplateInfo {
  id: string;
  name: string;
  description: string;
  /** unit = 子个体模板;group = 组模板 */
  kind: string;
}

export interface GroupMeta {
  id: string;
  name: string;
  description: string;
  primary?: string;
  workspace?: string;
  /** 组默认模型（"档案ID" 或 "档案ID/模型名"；空 = 跟随全局生效档案） */
  model?: string;
  modelRouting?: ModelRouting | null;
  /** 组级能力模型覆盖；缺省字段跟随「模型设置」页的全局槽位 */
  capabilities?: GroupCapabilities | null;
  /** 自我优化开关（group-self-optimize）：true = 主智能体可自行增删改子个体 */
  selfOptimize?: boolean | null;
  builtin: boolean;
  createdAt: string;
}

export interface SkillDef {
  id: string;
  name: string;
  description: string;
  triggers: string[];
  instructions: string;
  agents: string[];
  createdAt: string;
  /** 运行期统计（侧车聚合；不写入 SKILL.md） */
  useCount?: number;
  successCount?: number;
  lastUsedAt?: string;
  /** manual | agent | import */
  source?: string;
  /** 版本号（capability-parity 6.5）：每次版本化写入 +1，回滚 = 以旧内容写新版本行 */
  version?: number;
}

// ---------------- 版本链（capability-parity 6.5）----------------
// CLI / WebUI / 通道的治理操作共用后端同一版本事实源；这里只是它的只读投影。

/** 记忆版本历史里的一行（GET /api/memory/:id/versions） */
export interface MemoryHistoryRow {
  version: number;
  /** created | updated | rolled_back | forgotten | restored（置顶不是内容变更，不入版本历史） */
  change: string;
  title: string;
  /** 该版本写时的正文全文（治理面据此判断回滚目标） */
  body: string;
  kind: string;
  tags: string[];
  changedAt: string;
}

/** GET /api/memory/:id/versions —— 已遗忘的条目历史仍可读：current=null、forgotten=true */
export interface MemoryVersionsResponse {
  id: string;
  /** 旧 → 新 */
  versions: MemoryHistoryRow[];
  /** 当前生效版本号；条目已遗忘时为 null */
  current: number | null;
  forgotten: boolean;
}

/** POST /api/memory/:id/rollback 的回执：回滚 = 以旧内容写新版本行 */
export interface MemoryRollbackReceipt {
  id: string;
  /** 新版本号（单调递增，大于 toVersion） */
  version: number;
  rolledBackTo: number;
  by: string;
}

/** POST /api/memory/:id/restore 的回执 */
export interface MemoryRestoreReceipt {
  id: string;
  version: number;
  title: string;
  by: string;
}

/** 技能版本历史里的一行（GET /api/skills/:id/versions） */
export interface SkillHistoryRow {
  version: number;
  name: string;
  /** 该版本写时的描述（归档损坏时为空串） */
  description: string;
  /** 该版本写时的完整指令（= SKILL.md 正文）——据此判断回滚目标 */
  instructions: string;
}

/** GET /api/skills/:id/versions —— 已删除：current=null、deleted=true */
export interface SkillVersionsResponse {
  id: string;
  /** 旧 → 新 */
  versions: SkillHistoryRow[];
  /** 当前生效版本号；技能已删除时为 null */
  current: number | null;
  deleted: boolean;
}

/** GET /api/skills/deleted —— 技能删除后从列表消失，回滚恢复的入口靠这份清单找到目标 */
export interface DeletedSkill {
  id: string;
  /** 删除时间（ISO） */
  deletedAt: string;
}

/** POST /api/skills/:id/rollback 的回执 */
export interface SkillRollbackReceipt {
  id: string;
  /** 新版本号（单调递增，大于 toVersion） */
  version: number;
  updatedAt: string;
  rolledBackTo: number;
  by: string;
}

/** 插件列表项（GET /api/plugins） */
export interface PluginSummary {
  id: string;
  name: string;
  version: string;
  description: string;
  form: "builtin" | "process" | "declarative" | "virtual" | "unknown";
  enabled: boolean;
  error?: string | null;
  capabilities: {
    tools: number;
    channel?: { kinds: string[]; credentials: number };
    watch: boolean;
    resources: number;
    process: boolean;
  };
  /** 进程形态握手发现的工具名（列表页直接可见） */
  toolsSnapshot?: string[];
}

/** 插件详情（GET /api/plugins/:id） */
export interface PluginDetail extends PluginSummary {
  manifest: Record<string, unknown>;
  health: "ok" | "degraded";
  installation?: { kind: string; url: string; subdir?: string; ref?: string } | null;
  credentialsConfigured?: boolean;
  /** 进程插件握手后 tools/list 快照（空 = 未拉起或非进程形态） */
  toolsSnapshot?: string[];
}

/** 插件凭据区回显（值一律掩码） */
export interface PluginCredentials {
  id: string;
  credentials: Record<string, string>;
  configured: boolean;
}

/** 索引目录条目（GET /api/plugins/index） */
export interface PluginIndexEntry {
  id: string;
  name: string;
  version: string;
  description: string;
  status: "available" | "installed" | "updatable" | "incompatible";
  localVersion?: string;
  exmMin?: string;
}

export interface CronJob {
  id: string;
  name: string;
  prompt: string;
  cron?: string;
  at?: string;
  /** 任务类型：cron（周期）| at（一次性）| queue（连续任务）；缺省按 cron/at 推断 */
  kind?: "cron" | "at" | "queue" | string;
  /** at 类型：投递到的已有会话 id（空 = 按 sessionTitle 新建/复用） */
  targetSession?: string;
  /** queue 类型：按序执行的任务 id 列表 */
  steps?: string[];
  group?: string;
  sessionTitle?: string;
  enabled: boolean;
  /** 结果推送订阅的通道 id 清单（空 = 不推送；受「结果推送」总开关约束） */
  notifyChannels?: string[];
  lastRunAt?: string;
  lastStatus?: string;
  createdAt: string;
  /** 下次执行时间（启用且 cron 命中时由服务端计算） */
  nextRunAt?: string;
}

export interface CronRun {
  id: string;
  jobId: string;
  jobName: string;
  sessionId: string;
  startedAt: string;
  finishedAt: string;
  status: string;
  summary: string;
}

/** 智能连结网络连结体（实验性） */
export interface NexusLink {
  id: string;
  name: string;
  kind: string;
  endpoint?: string;
  apiKey?: string;
  command?: string;
  enabled: boolean;
}

/** 连通性测试结果 */
export interface NexusProbeResult {
  ok: boolean;
  message: string;
}

export interface ApprovalRequest {
  id: string;
  sessionId: string;
  nodeId?: string;
  agentId: string;
  command: string;
  status: string;
  result?: string;
  createdAt: string;
  decidedAt?: string;
}

/**
 * 破坏性命令前端判定（approval-ergonomics）：与后端
 * exm-core `DESTRUCTIVE_PATTERNS`（tools/allowlist.rs）口径保持同步——
 * 命中时审批卡隐藏「本会话允许/加入白名单」档（后端同样降级,此处仅 UI 预避）。
 */
export function isDestructiveCommand(command: string): boolean {
  const pats = [
    "rm ", "rm -", "del ", "rmdir", "rd /s", "format ", "shutdown", "taskkill",
    "reg delete", "reg add", "dd if=", "mkfs", "git push --force", "git push -f",
    "git reset --hard", "git clean", "git rebase", "drop table", "drop database",
    "truncate table", "remove-item", "invoke-expression", "curl | sh", "curl | bash",
  ];
  const lowered = command.toLowerCase();
  return pats.some((p) => lowered.includes(p));
}

// 计划模式（spec plan-mode：WebUI 计划卡片数据契约）
export interface PlanItem {
  id: string;
  tool: string;
  target: string;
  action: string;
  args: Record<string, unknown>;
  createdAt: string;
}

// 任务清单（spec agent-todo）：todo.updated 事件负载
export interface TodoItem {
  id: string;
  content: string;
  status: "pending" | "in_progress" | "done";
}

// 任务队列（task-queue）：task_queue.updated 事件负载中的单条任务。
// capability-parity 3.5：补齐 seq/revision/source/workspace 与 blocked/pending_recovery
// 终态（后续任务队列六态呈现；与 REST /sessions/:id/queue 的 QueuedTask 同形）。
export interface QueuedTaskItem {
  id: string;
  seq?: number;
  task: string;
  runMode: "parallel" | "queued";
  status: "queued" | "running" | "done" | "failed" | "cancelled" | "blocked" | "pending_recovery";
  spawnId?: string;
  source?: string;
  workspace?: string | null;
  revision?: number;
  createdAt: string;
  finishedAt?: string;
  note?: string;
  audit?: string[];
}

// 后台子任务卡（spec subagent-spawn WebUI 呈现）：spawn.started/finished 事件数据
export interface SpawnBgItem {
  spawnId: string;
  preview: string;
  status: "running" | "done" | "failed" | "timeout";
  /** 终态结果预览（spawn.finished 的 summaryPreview） */
  resultPreview?: string;
  startedAt: number;
  finishedAt?: number;
}

export interface PlanInfo {
  sessionId: string;
  mode: "plan" | "execute";
  items: PlanItem[];
  count: number;
}

/** 会话工作流关联视图（capability-parity 4.5/4.6）：goal/spec/plan 三层
 *  同源共享的只读快照——关系是派生视图，事实仍在 workflow.* 事件。 */
export interface WorkflowGoalState {
  id: string;
  revision: number;
  objective: string;
  status: "active" | "paused" | "completed" | "blocked" | "cleared";
  acceptance: string[];
  verification: string[];
  autoContinue: boolean;
  continueRoundsUsed: number;
  updatedAt: string;
}

export interface WorkflowSpecState {
  id: string;
  revision: number;
  goalId?: string;
  objective: string;
  scope: string[];
  constraints: string[];
  acceptance: string[];
  verification: string[];
  source: string;
  updatedAt: string;
}

export interface WorkflowRelation {
  goal?: WorkflowGoalState;
  spec?: WorkflowSpecState;
  planMode: boolean;
  planItems: number;
  todoPresent: boolean;
}

/** 上下文来源的一条清单项（capability-parity 6.3；REST GET /sessions/:id/context）。
 *  `status` 是叠加了禁用位之后的当前状态；`baseStatus` 是未禁用时的原状态
 *  （仅 disabled 时有意义）；`reason` 是未就绪/读取失败的真实原因。 */
export interface ContextSource {
  kind: "file" | "memory" | "retrieval" | "skills" | string;
  key: string;
  status: string;
  baseStatus?: string;
  disabled: boolean;
  /** 文件来源：层级（project / user / agent_prompt / system_safety）与字符数 */
  layer?: string;
  /** 文件来源：约定文件的相对路径（键去掉 `file:` 前缀） */
  file?: string;
  chars?: number | null;
  /** 文件来源：修改时间（Unix 秒；0 = 未知） */
  modifiedAt?: number;
  /** 缺失/未就绪/读取失败的原因；ready / loaded 时为空 */
  reason?: string | null;
}

/** 整层缺席的文件来源说明（Core 给出的事实，REST GET /sessions/:id/context 的 `gaps`）。
 *  某一层没有根时，它的约定文件根本不在清单里——例如未绑定项目的会话没有项目层。
 *  它是说明不是来源：不在 `sources` 里，不可停用 / 不可刷新。 */
export interface ContextSourceGap {
  kind: "file";
  layer: "project" | "user" | string;
  reason: string;
}

/** GET /sessions/:id/context 的响应 */
export interface ContextSourcesResponse {
  sessionId: string;
  sources: ContextSource[];
  /** 整层缺席的文件来源及原因（清单里一个 file: 都没有时，这是唯一的解释） */
  gaps?: ContextSourceGap[];
}

/** 单来源刷新回报（REST POST {action:"refresh", key}）：`ok=false` 时带 `error`；
 *  `others` 是刷新前其余来源的原样快照——调用方据此断言「失败不波及其余」。 */
export interface ContextSourceRefreshReport extends ContextSourcesResponse {
  key: string;
  ok: boolean;
  refreshed: boolean;
  error?: string | null;
  source: ContextSource;
  others?: ContextSource[];
}

/** disable / enable 的响应 */
export interface ContextSourceSetResponse extends ContextSourcesResponse {
  key: string;
  disabled: boolean;
  disabledKeys: string[];
}

export interface PlanAppliedItem {
  itemId: string;
  tool: string;
  target: string;
  ok: boolean;
}

export interface Channel {
  id: string;
  type: string;
  enabled: boolean;
  group?: string;
  account?: string;
  secret?: string;
  replyWebhook?: string;
  token?: string;
  /** 平台扩展配置：qqbot → appId/appSecret/sandbox；napcat → url/token */
  config?: Record<string, string>;
  /** 会话白名单（Telegram chat id / QQ openid / OneBot 群号或 QQ 号；空 = 不限） */
  allowedChats?: string[];
  /** 管理者账号：外部用户 ID 清单，判权视同 admin（免配对绑定） */
  admins?: string[];
  /** 群聊唤醒门控逐通道覆盖：true = 群内仅提及/回复触发；缺省 = 跟随全局默认 */
  groupGate?: boolean;
  /** 主动回复（实验性）：群聊免 @ 时经决策模型判断是否回应；缺省/false = 关 */
  proactiveReply?: boolean;
  /** 主动回复率 0~100（决策口径软权重；缺省 100 = 全量标准口径） */
  proactiveRate?: number;
  /** 提及必回（缺省 true）：@ / 引用回复机器人直接放行，不经主动回复漏斗 */
  mentionReply?: boolean;
  /** 能力边界：chat = 仅聊天；tools = 可调工具；computer = 电脑操控（缺省） */
  capability?: "chat" | "tools" | "computer";
  /** 记忆隔离：true = 本通道独立记忆；false/缺省 = 同组跨平台共享 */
  memoryIsolation?: boolean;
  createdAt: string;
}

/** 通道聊天流（chat flow）：外部 IM 的一个对话单元（群聊/私聊各一条；无细分对话的平台整通道一条）。
 *  一条流绑定一个活跃会话——不主动切换固定复用同一会话，跨轮上下文与记忆自然延续。 */
export interface ChannelFlow {
  id: string;
  channelId: string;
  chatKey: string;
  displayName: string;
  isGroup: boolean;
  /** 当前绑定的会话 id（空 = 尚未有会话，下条消息自动开新） */
  activeSession?: string;
  /** 未读数：智能体在流绑定会话产出回复且用户未打开该会话时累积；打开即清零 */
  unread?: number;
  lastMessage: string;
  lastActiveAt: string;
  createdAt: string;
}

/** 通道运行状态（内置适配器上报；桥接类型无运行时不产生条目） */
export interface ChannelStatus {
  state: "ok" | "error";
  detail: string;
  at: string;
}

export interface ClientIdentity {
  preset: "default" | "claude-code" | "codex" | "opencode" | "cline" | "custom";
  version?: string;
  requestMode?: "native" | "headers";
  userAgent?: string;
  headers?: Record<string, string>;
}

export interface ClientIdentityPreview {
  preset: ClientIdentity["preset"];
  version: string;
  requestMode: "native" | "headers";
  configuredFormat: string;
  effectiveFormat: string;
  path: string;
  headers: Record<string, string>;
  metadata: boolean;
  nativeCompatible: boolean;
  upstreamVerified: false;
}

export interface LlmProfile {
  id: string;
  name: string;
  baseUrl: string;
  apiFormat?: string;
  effectiveApiFormat?: string;
  clientIdentity?: ClientIdentity;
  apiKey: string;
  /** 多 Key 池（掩码行 = 沿用旧池同位键） */
  apiKeys?: string[];
  /** 该提供商的默认模型名（模型清单里设为默认的那一个） */
  model: string;
  /** 模型清单（模型名 + 视觉/语音能力开关）；空 = 未标记（能力未知，输入直通） */
  models: ProfileModel[];
  /** 失败回退：下一个档案 id（请求失败且未发出内容时切换） */
  fallback?: string | null;
}

/** 模型条目：视觉/语音开关决定多模态路由（不支持时走视觉转述/语音转述） */
export interface ProfileModel {
  model: string;
  vision: boolean;
  audio: boolean;
  contextWindow?: number;
  purposes?: ModelPurpose[];
  cost?: ModelCost;
  usageNotes?: string;
  autoEligible?: boolean;
}

/** 能力模型槽位："档案ID" 或 "档案ID/模型名"；空 = 全局档案默认（TTS tts-1 / STT whisper-1 / 仅词项召回） */
export interface LlmCapabilities {
  speech: string;
  transcribe: string;
  visionRelay: string;
  embedding: string;
}

export interface LlmProfilesInfo {
  active: string;
  mock: boolean;
  profiles: LlmProfile[];
}

export interface GroupOverview {
  group: GroupMeta | null;
  agents: number;
  playbooks: number;
  sessions: number;
  memory: { group: number; shared: number };
  agentStats: AgentStat[];
}

export interface StoredEvent {
  id?: string;
  topic?: string;
  from?: string;
  to?: string;
  type?: string;
  kind?: string;
  sessionId?: string;
  payload?: unknown;
  createdAt?: string;
}

export interface MemoryStats {
  memory: {
    total: number;
    pinned: number;
    individual: number;
    shared: number;
    terms: number;
    byKind: Record<string, number>;
    dbPath: string;
  };
  agentStats: AgentStat[];
}

export function gatewayAssetUrl(base: string, key: string, path: string, sessionId?: string, download = false): string {
  return `${base}/api/asset${download ? "/download" : ""}?path=${encodeURIComponent(path)}&key=${encodeURIComponent(key)}${sessionId ? `&sessionId=${encodeURIComponent(sessionId)}` : ""}`;
}
