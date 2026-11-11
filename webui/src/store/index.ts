// store 桶（自 store.ts 拆出，调用点零改动）：index 保类型与导出形状。
/** 全局状态（Zustand）+ WS 事件处理 */
import { create } from "zustand";
import { message } from "antd";
import type {
  AgentDefinition,
  ApprovalItem,
  ArtifactItem,
  ChatMessage,
  Session,
  SessionLedger,
  Statement,
  TaskGraph,
  ToolCallItem,
} from "../types";
import { api, serverBase, type GatewayConfig, type GroupMeta, type PlanAppliedItem, type PlanInfo, type QueuedTaskItem, type RecallHit, type SpawnBgItem, type TodoItem } from "../api";
import { tr } from "../i18n/core";

export interface TimelineItem {
  kind: "dispatch" | "sync" | "arbitration" | "memory" | "error" | "cron" | "budget";
  agentId?: string;
  nodeId?: string;
  /** 派发对应的子会话（可点击跳转查看子个体过程流） */
  childSessionId?: string;
  text: string;
}

/** 子个体实时过程流的一段（思考 / 正文 / 工具，按事件到达顺序交错） */
export interface LivePart {
  kind: "thinking" | "text" | "tool";
  text?: string;
  callId?: string;
}

export interface WsEvent {
  type: string;
  sessionId: string;
  payload: unknown;
}

export interface ExmState {
  sessions: Session[];
  /** 未读计数（会话 id → 条数；切走后该会话的新末条计入） */
  unreadCounts: Record<string, number>;
  sessionId: string | null;
  messages: ChatMessage[];
  graph: TaskGraph | null;
  agents: AgentDefinition[];
  ledger: SessionLedger | null;
  config: GatewayConfig | null;
  running: boolean;
  wsConnected: boolean;
  liveOrch: string;
  /** 指挥体思维链（分轨流式，不与回答混流） */
  liveThinking: string;
  /** 子个体实时过程流：思考/工具/输出按发生顺序交错（子会话实时视图数据源） */
  liveParts: Record<string, LivePart[]>;
  /** 当前轮工具执行轨迹（编程软件式过程透明；run.finished 后并入最终消息） */
  runToolCalls: ToolCallItem[];
  /** 待人工审批单（聊天内联裁决） */
  approvals: ApprovalItem[];
  /** 计划模式（spec plan-mode WebUI 卡片）：当前会话计划数据 + 拉取防抖 */
  plan: PlanInfo | null;
  planSession: string | null;
  planApplied: PlanAppliedItem[] | null;
  planPending: boolean;
  planDirtyAt: number;
  refreshPlan: (sessionId: string) => Promise<void>;
  decidePlan: (action: "approve" | "reject" | "setMode", plan?: boolean) => Promise<void>;
  /** 任务清单（spec agent-todo）：todo.updated 事件快照 */
  todo: { count: number; doneCount: number; allDone: boolean; items: TodoItem[] } | null;
  /** 后台子任务卡（spec subagent-spawn WebUI 呈现）：spawn.started/finished 事件维护 */
  spawns: SpawnBgItem[];
  /** 任务队列（task-queue）：task_queue.updated 事件快照（切会话/刷新后回放恢复） */
  taskQueue: { running: number; queued: number; items: QueuedTaskItem[] } | null;
  /** 清除全部后台子任务卡（终态收讫后手动清理） */
  clearSpawns: () => void;
  timeline: TimelineItem[];
  /** 时间线归属会话：切到子会话/其它会话时不在那里展示别处的调度轨迹 */
  timelineSession: string | null;
  lastRecall: RecallHit[];
  memoryVersion: number;
  groups: GroupMeta[];
  activeGroup: string;

  init: () => Promise<void>;
  loadGroups: () => Promise<void>;
  switchGroup: (id: string) => Promise<void>;
  setTarget: (mode: "group" | "single", id?: string) => Promise<void>;
  createGroup: (body: { name: string; id?: string; description?: string }) => Promise<void>;
  refreshAgents: () => Promise<void>;
  selectSession: (id: string) => Promise<void>;
  newSession: () => Promise<void>;
  send: (text: string, images?: string[], mode?: string | null) => Promise<void>;
  /** 会话列表已完成首次加载（骨架屏判定） */
  sessionsLoaded: boolean;
  /** 会话输入草稿（内存态；sessionStorage 镜像持久化） */
  drafts: Record<string, string>;
  setDraft: (sessionId: string, text: string) => void;
  /** 全局待审批数（侧栏角标数据源；轮询兜底——WS 丢事件时也能恢复） */
  pendingApprovals: number;
  refreshPendingApprovals: () => Promise<void>;
  /** 新消息落会话列表时刷新该会话的末条预览（chat-ergonomics） */
  updateSessionPreview: (sessionId: string, preview: string) => void;
  /** 会话置顶切换（乐观更新 + 服务端落库） */
  toggleSessionPin: (sessionId: string, pinned: boolean) => Promise<void>;
  /** 清会话未读（进入会话时） */
  clearUnread: (sessionId: string) => void;
  /** 全部已读（清 unreadCounts 全表） */
  markAllRead: () => void;
  saveConfig: (body: Record<string, unknown>) => Promise<void>;
  decideApproval: (id: string, approve: boolean, scope?: "once" | "session" | "allowlist") => Promise<void>;
  handleEvent: (evt: WsEvent) => void;
  setWs: (ok: boolean) => void;
  bumpMemory: () => void;
  /** 跨视图导航请求（审批→会话跳转等）：App 订阅消费 */
  navRequest: { view: string; sessionId?: string; messageId?: string; nonce: number } | null;
  /** 消息定位请求（检索命中）：ChatView 加载消息后滚动并短暂高亮，消费即清空 */
  focusMessage: { sessionId: string; messageId: string; nonce: number } | null;
  requestNav: (view: string, sessionId?: string, messageId?: string) => void;
  markSessionsLoaded: () => void;
}

/** 会话输入草稿（chat-ergonomics）：内存 + sessionStorage 双写；发送成功即清除 */
const draftKey = (id: string) => `exm.draft.${id}`;

/** 浏览器通知偏好（web-notifications）：localStorage 持久化 */

export * from "./notify";
export * from "./ws";
export { useExm } from "./sessions";
