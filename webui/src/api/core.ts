//! api/core.ts —— 共享基座（codebase-refactor 7.1 拆分）：
//! 请求工具（req/authHeaders）、服务地址、全部接口类型定义、资产 URL 与探活。
//! 纯搬移：签名与实现零改动；类型集中此处，域文件按需引入。

/** REST 客户端 —— 契约见 docs/04 §7 */
export type { AgentDefinition, ChatMessage, EvidenceItem, Session, SessionSearchHit, SessionSnapshot, TaskGraph } from "../types";
import { tr } from "../i18n/core";

/** 密钥掩码哨兵（与后端 llm_admin::KEY_MASK / 通道脱敏同一契约）：提交时收到此值 = 沿用服务端旧值 */
export const KEY_MASK = "***已配置***";
/** 密钥清除哨兵（与后端 llm_admin::KEY_CLEAR 同一契约）：提交时收到此值 = 显式删除该密钥。
 *  空串的历史语义是「沿用旧值」，删除意图只能用此哨兵表达。 */
export const KEY_CLEAR = "__CLEAR__";

/** 远程服务器基址（桌面/移动客户端连接任意网关；空 = 同源）。
 *  同一份 WebUI 既能被本机网关托管（桌面/浏览器），也能作为客户端连远程网关（手机 PWA）。 */
export function serverBase(): string {
  return (localStorage.getItem("exm.server") ?? "").replace(/\/+$/, "");
}

export function setServerBase(url: string): void {
  const v = url.trim().replace(/\/+$/, "");
  if (v) localStorage.setItem("exm.server", v);
  else localStorage.removeItem("exm.server");
}

export function authHeaders(): Record<string, string> {
  const key = localStorage.getItem("exm.key") ?? "";
  return key ? { "X-Auth-Key": key } : {};
}

export async function req<T>(path: string, init?: RequestInit): Promise<T> {
  const resp = await fetch(`${serverBase()}/api${path}`, {
    headers: { "Content-Type": "application/json", ...authHeaders() },
    ...init,
  });
  if (resp.status === 401) {
    // 密钥失效才清键回登录门；无 key 的 401 = 未登录（dev 源首访 / 角标预热早于
    // 登录门），不 reload——否则挂载期轮询（approvals/flows 角标）会触发
    // 401→清键→reload 死循环，页面永远卡在 CONNECTING（dev 预览实测踩中）
    if (localStorage.getItem("exm.key") && path !== "/auth/verify") {
      localStorage.removeItem("exm.key");
      window.location.reload();
    }
    throw new Error(tr("api.unauthorized"));
  }
  if (!resp.ok && resp.status !== 202) {
    const raw = await resp.text();
    // 全站错误契约：{"error": "中文说明"}（业务错误与提取器拒绝都已归一），
    // 直接取说明文案给用户看；只有反向代理之类吐非 JSON 时才回退原文。
    let msg = raw.slice(0, 200);
    try {
      const parsed = JSON.parse(raw) as { error?: string };
      if (parsed.error) msg = parsed.error;
    } catch {
      /* 非 JSON：保留原文 */
    }
    throw new Error(msg);
  }
  return (await resp.json()) as T;
}

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
  };
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

// 任务队列（task-queue）：task_queue.updated 事件负载中的单条任务
export interface QueuedTaskItem {
  id: string;
  task: string;
  runMode: "parallel" | "queued";
  status: "queued" | "running" | "done" | "failed" | "cancelled";
  spawnId?: string;
  createdAt: string;
  finishedAt?: string;
  note?: string;
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
  /** 主动回复率 0~100（决策模型前的概率采样闸门；缺省 100 = 全量交决策模型） */
  proactiveRate?: number;
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

export interface LlmProfile {
  id: string;
  name: string;
  baseUrl: string;
  apiFormat?: string;
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

/** 工作区资产 URL（computer 截图 / 工具输出预览）：<img> 无法带请求头，密钥走查询参数 */
export function assetUrl(relPath: string): string {
  const key = localStorage.getItem("exm.key") ?? "";
  return `${serverBase()}/api/asset?path=${encodeURIComponent(relPath)}&key=${encodeURIComponent(key)}`;
}

/** 工作区文件下载 URL（/api/asset/download）：<a target="_blank"> 导航同样无法带
 *  请求头，密钥与 assetUrl 同款走查询参数（网关侧对 GET /api/asset* 开放该通道） */
export function downloadUrl(relPath: string): string {
  const key = localStorage.getItem("exm.key") ?? "";
  return `${serverBase()}/api/asset/download?path=${encodeURIComponent(relPath)}&key=${encodeURIComponent(key)}`;
}

/** 校验远程网关连通性（高级用法：localStorage 手动设 exm.server 后探活） */
export async function probeServer(base: string): Promise<{ ok: boolean; error?: string }> {
  try {
    const resp = await fetch(`${base.replace(/\/+$/, "")}/api/health`, { headers: authHeaders() });
    if (!resp.ok) return { ok: false, error: `HTTP ${resp.status}` };
    return { ok: true };
  } catch (e) {
    return { ok: false, error: String(e) };
  }
}

