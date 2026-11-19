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
} from "./types";
import type { GatewayConfig, GroupMeta, PlanAppliedItem, PlanInfo, QueuedTaskItem, RecallHit, SpawnBgItem, TodoItem } from "./api";
import type { ExmClient } from "./api/client";
import type { Translate } from "./translate";

export interface TimelineItem {
  kind: "dispatch" | "sync" | "arbitration" | "memory" | "error" | "cron" | "budget" | "model";
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
  selectedSession: Session | null;
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
  send: (text: string, images?: string[], mode?: string | null, agent?: string) => Promise<boolean>;
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
const scopedDraftKey = (scope: string) => scope === "default" ? "exm.drafts" : `exm.drafts.${scope}`;

function loadDrafts(scope: string): Record<string, string> {
  try {
    const key = scopedDraftKey(scope);
    const raw = sessionStorage.getItem(key);
    return raw ? JSON.parse(raw) as Record<string, string> : {};
  } catch {
    return {};
  }
}

/** 实时过程流追加：同类（思考/正文）并入末段，避免逐字成条 */
function appendPart(map: Record<string, LivePart[]>, agent: string, part: LivePart): Record<string, LivePart[]> {
  const list = [...(map[agent] ?? [])];
  const last = list[list.length - 1];
  if (last && last.kind === part.kind && (part.kind === "thinking" || part.kind === "text")) {
    list[list.length - 1] = { ...last, text: (last.text ?? "") + (part.text ?? "") };
  } else {
    list.push(part);
  }
  return { ...map, [agent]: list };
}

/** 会话列表默认选中：跳过子会话（自动落到最近的主会话） */
function firstMainSession(list: Session[]): Session | undefined {
  return list.find((s) => !s.title.startsWith("sub:")) ?? list[0];
}

type StoreApi = Pick<ExmClient["api"], "listSessions" | "agents" | "getConfig" | "groups" | "setTarget" | "switchGroup" | "createGroup"
  | "getSession" | "graph" | "messages" | "sessionEvents" | "createSession" | "chat" | "putConfig" | "decideApproval"
  | "getPlan" | "decidePlan" | "listApprovals" | "pinSession" | "sessionTokens" | "sessionArchive" | "listArchivedSessions"
  | "listAllSessions" | "batchSessions" | "renameSession" | "deleteSession" | "searchSessions" | "stopSession"
  | "forkSession" | "editSession" | "sessionSnapshots" | "evidence" | "sessionEvents" | "decidePlan" | "createGroup"
  | "setGroupModel" | "setAgentModel" | "groupAgents" | "listSingles" | "getTarget" | "llmProfiles" | "decidePlan">;

interface Lifecycle {
  ws: WebSocket | null;
  wsGen: number;
  selectSeq: number;
  stateRevision: number;
  approvalRevision: number;
  approvalRequest: number;
  localMessageSeq: number;
  wsBackoff: number;
  wsLastFrame: number;
  wsWatch: ReturnType<typeof setInterval> | null;
  wsHeartbeat: ReturnType<typeof setInterval> | null;
  wsRetry: ReturnType<typeof setTimeout> | null;
}

function socketRuntime(life: Lifecycle, api: StoreApi, serverBase: () => string, accessKey: () => string,
  state: () => ExmState, update: (patch: Partial<ExmState>) => void, disposed: () => boolean) {
  const connect = () => {
    const sessionId = state().sessionId;
    if (!sessionId || disposed()) return;
    const gen = ++life.wsGen;
    if (life.wsRetry !== null) { clearTimeout(life.wsRetry); life.wsRetry = null; }
    if (life.wsHeartbeat !== null) { clearInterval(life.wsHeartbeat); life.wsHeartbeat = null; }
    life.ws?.close();
    state().setWs(false);
    const base = serverBase();
    const proto = base.startsWith("https") || (!base && location.protocol === "https:") ? "wss" : "ws";
    const host = base ? base.replace(/^https?:\/\//, "") : location.host;
    const workspace = state().selectedSession?.workspace;
    const scoped = workspace ? `&workspace=${encodeURIComponent(workspace)}` : "";
    const socket = new WebSocket(`${proto}://${host}/ws?sessionId=${encodeURIComponent(sessionId)}${scoped}&key=${encodeURIComponent(accessKey())}`);
    life.ws = socket;
    life.wsLastFrame = Date.now();
    socket.onopen = () => {
      if (gen !== life.wsGen || disposed()) return;
      life.wsBackoff = 0;
      life.wsLastFrame = Date.now();
      state().setWs(true);
      void state().refreshPendingApprovals();
      const sequence = life.selectSeq;
      const revision = life.stateRevision;
      void Promise.all([api.messages(sessionId), api.graph(sessionId), api.getSession(sessionId)]).then(([messages, graph, session]) => {
        if (gen !== life.wsGen || state().sessionId !== sessionId || life.selectSeq !== sequence || life.stateRevision !== revision) return;
        const running = session.running ?? (graph.nodes ?? []).some((node) => ["running", "dispatched", "syncing", "arbitrating"].includes(String(node.status ?? "")));
        state().setWs(true);
        update({ messages, graph: graph.nodes?.length ? graph : null, running, ledger: session.ledger,
          ...(!running ? { liveOrch: "", liveThinking: "", liveParts: {}, runToolCalls: [] } : {}) });
      }).catch(() => undefined);
    };
    socket.onclose = () => {
      if (gen !== life.wsGen) return;
      if (life.wsHeartbeat !== null) { clearInterval(life.wsHeartbeat); life.wsHeartbeat = null; }
      state().setWs(false);
      const delay = [2000, 5000, 10000, 30000][Math.min(life.wsBackoff++, 3)];
      life.wsRetry = setTimeout(() => { life.wsRetry = null; if (gen === life.wsGen && state().sessionId === sessionId) connect(); }, delay);
    };
    socket.onmessage = (message) => {
      if (gen !== life.wsGen) return;
      life.wsLastFrame = Date.now();
      if (message.data === "pong") return;
      try { state().handleEvent(JSON.parse(String(message.data)) as WsEvent); } catch { /* ignore malformed frames */ }
    };
    life.wsHeartbeat = setInterval(() => {
      if (socket.readyState === WebSocket.OPEN) { try { socket.send("ping"); } catch { /* watchdog reconnects */ } }
    }, 20_000);
    if (life.wsWatch === null) life.wsWatch = setInterval(() => {
      const current = life.ws;
      if (current && (current.readyState === WebSocket.OPEN || current.readyState === WebSocket.CONNECTING)
        && Date.now() - life.wsLastFrame > 45_000) connect();
    }, 10_000);
  };
  const reconnect = () => {
    if (life.ws?.readyState === WebSocket.OPEN && Date.now() - life.wsLastFrame <= 45_000) return;
    life.wsBackoff = 0; connect();
  };
  const visible = () => { if (document.visibilityState === "visible") reconnect(); };
  window.addEventListener("online", reconnect);
  document.addEventListener("visibilitychange", visible);
  return {
    connect,
    reconnect: connect,
    dispose: () => {
      ++life.wsGen;
      if (life.wsRetry !== null) clearTimeout(life.wsRetry);
      if (life.wsHeartbeat !== null) clearInterval(life.wsHeartbeat);
      if (life.wsWatch !== null) clearInterval(life.wsWatch);
      life.ws?.close(); life.ws = null;
      window.removeEventListener("online", reconnect);
      document.removeEventListener("visibilitychange", visible);
    },
  };
}

export function createExmStore(options: Pick<ExmClient, "serverBase" | "accessKey"> & {
  api: StoreApi;
  tr: Translate;
  project?: { id: string; workspace: string };
  onEvent?: (event: WsEvent, state: ExmState) => void;
}) {
const { api, serverBase, accessKey, tr, project } = options;
const draftScope = project?.id ?? "default";
const belongs = (session: Session) => !project || session.workspace === project.workspace;
let disposed = false;

const lifecycle: Lifecycle = { ws: null, wsGen: 0, selectSeq: 0, stateRevision: 0, approvalRevision: 0, approvalRequest: 0,
  localMessageSeq: 0, wsBackoff: 0, wsLastFrame: 0, wsWatch: null, wsHeartbeat: null, wsRetry: null };

async function createSession(target?: string) {
  const selection = lifecycle.selectSeq;
  const session = await api.createSession(tr("session.new"), target ? { target } : undefined);
  if (disposed) return;
  if (!belongs(session)) throw new Error("网关没有将新会话绑定到当前项目");
  useExm.setState((state) => ({ sessions: [session, ...state.sessions] }));
  if (selection === lifecycle.selectSeq) await useExm.getState().selectSession(session.id);
}

const useExm = create<ExmState>((set, get) => ({
  sessions: [],
  unreadCounts: {},
  sessionsLoaded: false,
  sessionId: null,
  selectedSession: null,
  messages: [],
  graph: null,
  agents: [],
  ledger: null,
  config: null,
  running: false,
  wsConnected: false,
  liveOrch: "",
  liveThinking: "",
  liveParts: {},
  runToolCalls: [],
  approvals: [],
  plan: null,
  planSession: null,
  planApplied: null,
  planPending: false,
  planDirtyAt: 0,
  todo: null,
  spawns: [],
  taskQueue: null,
  timeline: [],
  timelineSession: null,
  lastRecall: [],
  memoryVersion: 0,
  groups: [],
  activeGroup: "default",

  init: async () => {
    const [sessions, agents, config] = await Promise.all([
      api.listSessions(),
      api.agents(),
      project ? null : api.getConfig(),
    ]);
    if (disposed) return;
    if (!sessions.every(belongs)) throw new Error("网关返回了其他项目的会话，请检查网关版本");
    set({ sessions, agents, config });
    get().markSessionsLoaded();
    await get().loadGroups();
    const preferred = project ? sessionStorage.getItem(`exm.desktop.session.${project.id}`) : null;
    const main = sessions.find((session) => session.id === preferred) ?? firstMainSession(sessions);
    if (main) {
      await get().selectSession(main.id);
    } else {
      await get().newSession();
    }
  },

  loadGroups: async () => {
    const { active, groups } = await api.groups();
    set({ activeGroup: active, groups });
  },

  setTarget: async (mode, id) => {
    if (project && id) {
      const target = mode === "single" ? `single:${id}` : id;
      const next = firstMainSession(get().sessions.filter((s) => s.groupId === target));
      if (next) await get().selectSession(next.id); else await createSession(target);
      return;
    }
    await api.setTarget(mode, id);
    const agents = await api.agents();
    const sessions = await api.listSessions();
    set({
      agents,
      sessions,
      graph: null,
      timeline: [],
      timelineSession: null,
      liveOrch: "",
      liveThinking: "",
      liveParts: {},
      runToolCalls: [],
      approvals: [],
      messages: [],
    });
    const main = firstMainSession(sessions);
    if (main) {
      await get().selectSession(main.id);
    } else {
      await get().newSession();
    }
  },

  switchGroup: async (id) => {
    await api.switchGroup(id);
    const agents = await api.agents();
    // 组是交互对象：切组即切换会话上下文（会话归属组），自动选中该组最近会话
    const sessions = await api.listSessions();
    set({
      activeGroup: id,
      agents,
      sessions,
      graph: null,
      timeline: [],
      timelineSession: null,
      liveOrch: "",
      liveThinking: "",
      liveParts: {},
      runToolCalls: [],
      approvals: [],
      messages: [],
    });
    await get().loadGroups();
    const main = firstMainSession(sessions);
    if (main) {
      await get().selectSession(main.id);
    } else {
      await get().newSession();
    }
  },

  createGroup: async (body) => {
    await api.createGroup(body);
    await get().loadGroups();
  },

  refreshAgents: async () => {
    set({ agents: await api.agents() });
  },

  selectSession: async (id) => {
    // 代际守卫：快速切会话时旧请求的响应可能晚于新请求到达（网络乱序），
    // 旧快照整体覆盖会抹掉新状态（切页面后回复消失 bug 的一半根因）。
    // 每次选择递增序号，只有最新请求的响应才允许落盘。
    const seq = ++lifecycle.selectSeq;
    const [messages, graph, session] = await Promise.all([
      api.messages(id),
      api.graph(id),
      api.getSession(id),
    ]);
    if (seq !== lifecycle.selectSeq || disposed) return;
    if (!belongs(session)) throw new Error("该会话不属于当前项目");
    if (project) {
      try { sessionStorage.setItem(`exm.desktop.session.${project.id}`, session.id); } catch { /* storage unavailable */ }
    }
    get().clearUnread(id);
    set({
      sessionId: id,
      selectedSession: session,
      messages,
      graph: graph.nodes?.length ? graph : null,
      ledger: session.ledger,
      liveOrch: "",
      liveThinking: "",
      liveParts: {},
      runToolCalls: [],
      approvals: [],
      plan: null,
      planSession: id,
      planApplied: null,
      planPending: false,
      todo: null,
      spawns: [],
      taskQueue: null,
      timeline: [],
      timelineSession: null,
      running: session.running ?? (graph.nodes ?? []).some((n) =>
        ["running", "dispatched", "syncing", "arbitrating"].includes(n.status)),
    });
    // 计划模式：进会话拉取当前清单与模式位（后续靠 plan.* 事件增量刷新）
    void get().refreshPlan(id);
    // 任务清单回放（spec agent-todo）：todo 快照只由事件维护，切会话/刷新后
    // 从事件流尾部找回最近一次 todo.updated（含清空语义——找到空清单即无卡）
    void api.sessionEvents(id, 200).then((evts) => {
      if (get().sessionId !== id || seq !== lifecycle.selectSeq) return;
      for (let i = evts.length - 1; i >= 0; i--) {
        const p = evts[i].payload as { type?: string; payload?: Record<string, unknown> } | undefined;
        if (p?.type === "todo.updated") {
          const d = (p.payload ?? {}) as Record<string, unknown>;
          set({
            todo: {
              count: Number(d.count ?? 0),
              doneCount: Number(d.doneCount ?? 0),
              allDone: Boolean(d.allDone),
              items: (d.items ?? []) as unknown as TodoItem[],
            },
          });
          break;
        }
      }
      // 任务队列回放：快照只由事件维护（与 todo 同口径），刷新后从事件尾恢复
      for (let i = evts.length - 1; i >= 0; i--) {
        const p = evts[i].payload as { type?: string; payload?: Record<string, unknown> } | undefined;
        if (p?.type === "task_queue.updated") {
          const d = (p.payload ?? {}) as Record<string, unknown>;
          set({
            taskQueue: {
              running: Number(d.running ?? 0),
              queued: Number(d.queued ?? 0),
              items: (d.items ?? []) as unknown as QueuedTaskItem[],
            },
          });
          break;
        }
      }
    }).catch(() => { /* 回放失败不阻断进会话 */ });
    // 内联审批恢复：approvals 只由 WS 事件维护，刷新/换页后清空——
    // approval.required 是纯广播事件不落事件流，无法回放；审批 API 的 pending 表
    // 自带 sessionId 归属，恢复【全部】pending 单（与 WS 直推口径一致——跨会话
    // 审批也恢复，卡上「跳转子会话」按钮承担归属导航）
    void get().refreshPendingApprovals();
    connection.connect();
  },

  newSession: () => createSession(project ? get().selectedSession?.groupId : undefined),

  send: async (text, images, sendMode, agent): Promise<boolean> => {
    const id = get().sessionId;
    if (!id || !text.trim()) return false;
    const seq = lifecycle.selectSeq;
    lifecycle.stateRevision += 1;
    const label = images?.length ? tr("store.withImages", { text, n: images.length }) : text;
    const localMessageId = `local-${Date.now()}-${++lifecycle.localMessageSeq}`;
    const previousPreview = get().sessions.find((s) => s.id === id)?.lastMessagePreview;
    const wasRunning = get().running;
    set((s) => ({
      running: true,
      ...(wasRunning ? {} : {
        liveOrch: "",
        liveThinking: "",
        liveParts: {},
        runToolCalls: [],
        timeline: [],
        timelineSession: null,
        lastRecall: [],
      }),
      messages: [
        ...s.messages,
        {
          id: localMessageId,
          sessionId: id,
          role: "user",
          statements: [{ tag: "要求", text: label }],
          createdAt: new Date().toISOString(),
        },
      ],
    }));
    get().updateSessionPreview(id, label);
    let accepted = false;
    try {
      const result = await api.chat(id, text, images, sendMode ?? undefined, agent);
      if (!result.accepted) throw new Error("网关未受理消息");
      accepted = true;
    } catch (e) {
      // 发送失败（网络断/网关拒/会话忙）：running 复位 + 时间线错误条 + toast。
      // HTTP 层失败不会广播 run.error 事件，无此兜底则「执行中」转圈永挂、
      // 回到可编辑的输入框后重试，不保留一个无法区分已发送状态的本地消息。
      set((s) => s.sessionId !== id || lifecycle.selectSeq !== seq ? {} : {
        running: wasRunning ? s.running : false,
        messages: s.messages.filter((m) => m.id !== localMessageId),
        sessions: s.sessions.map((session) => session.id === id && session.lastMessagePreview === label
          ? { ...session, lastMessagePreview: previousPreview }
          : session),
        timeline: [...s.timeline, { kind: "error", text: tr("store.tl.error", { msg: String(e) }) }],
        timelineSession: id,
      });
      message.error(String(e));
    } finally {
      // 仅在请求已受理且草稿仍是本次发送内容时清除，保留失败或后续新输入。
      // 修 bug：历史版本逐会话存 exm.draft.<id>，现行复合存 exm.drafts JSON——
      // 此前只 removeItem 旧键而复合键未清，切视图重挂载后草稿初始化把
      // 已发送文本读回输入框。两处都清（旧键兜底 + 复合键同步）。
      const sid = id;
      if (accepted) {
        set((s) => {
          if (s.drafts[sid] !== text) return {};
          const drafts = { ...s.drafts, [sid]: "" };
          try { sessionStorage.setItem(scopedDraftKey(draftScope), JSON.stringify(drafts)); } catch { /* 隐私模式忽略 */ }
          return { drafts };
        });
        try {
          sessionStorage.removeItem(draftKey(sid));
        } catch { /* 隐私模式忽略 */ }
      }
    }
    return accepted;
  },

  saveConfig: async (body) => {
    await api.putConfig(body);
    set({ config: await api.getConfig() });
  },

  setWs: (ok) => set({ wsConnected: ok }),
  bumpMemory: () => set({ memoryVersion: get().memoryVersion + 1 }),

  decideApproval: async (id, approve, scope = "once") => {
    try {
      await api.decideApproval(id, approve, scope);
      lifecycle.approvalRevision += 1;
      // 乐观移除（approval.resolved 事件到达时幂等）
      set({ approvals: get().approvals.filter((a) => a.approvalId !== id) });
      void get().refreshPendingApprovals();
      // 审批页列表自刷新（若正看着该页，行状态即刻流转）
      options.onEvent?.({ type: "approval.resolved", sessionId: "", payload: { approvalId: id } }, get());
    } catch (e) {
      message.error(String(e));
    }
  },

  /** 拉取计划清单（plan.item 事件高频到达时 300ms 防抖合并） */
  refreshPlan: async (sessionId) => {
    if (get().planSession !== sessionId) {
      set({ plan: null, planSession: sessionId, planApplied: null, planPending: false });
    }
    const stamp = Date.now();
    set({ planDirtyAt: stamp });
    await new Promise((r) => setTimeout(r, 300));
    if (get().planDirtyAt !== stamp) return; // 已有更新的一次拉取接管
    try {
      const info = await api.getPlan(sessionId);
      if (get().planSession === sessionId) set({ plan: info, planSession: sessionId });
    } catch { /* 会话删除等场景静默 */ }
  },

  decidePlan: async (action, plan) => {
    const sid = get().sessionId;
    if (!sid) return;
    try {
      if (action === "approve") {
        const r = await api.decidePlan(sid, "approve");
        set({ planApplied: r.applied ?? [], planPending: false });
        message.success(tr("store.plan.approved", { n: String(r.okCount ?? 0) }));
      } else if (action === "reject") {
        await api.decidePlan(sid, "reject");
        set({ planPending: false });
        message.info(tr("store.plan.rejected"));
      } else {
        await api.decidePlan(sid, "setMode", plan);
      }
      await get().refreshPlan(sid);
    } catch (e) {
      message.error(String(e));
    }
  },

  clearSpawns: () => set({ spawns: [] }),

  navRequest: null,
  focusMessage: null,
  requestNav: (view, sessionId, messageId) =>
    set({
      navRequest: { view, sessionId, messageId, nonce: Date.now() },
      ...(sessionId && messageId
        ? { focusMessage: { sessionId, messageId, nonce: Date.now() } }
        : {}),
    }),
  markSessionsLoaded: () => set({ sessionsLoaded: true }),

  drafts: loadDrafts(draftScope),
  setDraft: (sessionId, text) => {
    set((s) => ({ drafts: { ...s.drafts, [sessionId]: text } }));
    try {
      const state = useExm.getState();
      sessionStorage.setItem(scopedDraftKey(draftScope), JSON.stringify(state.drafts));
    } catch { /* 隐私模式忽略 */ }
  },
  pendingApprovals: 0,
  refreshPendingApprovals: async () => {
    const request = ++lifecycle.approvalRequest;
    const revision = lifecycle.approvalRevision;
    try {
      const items = await api.listApprovals("pending", 100);
      if (request !== lifecycle.approvalRequest || revision !== lifecycle.approvalRevision) return;
      set({
        pendingApprovals: items.length,
        approvals: items.map((item) => ({ approvalId: item.id, agentId: item.agentId, command: item.command, sessionId: item.sessionId })),
      });
    } catch {
      /* 审批接口不可用时保持旧值 */
    }
  },
  updateSessionPreview: (sessionId, preview) => {
    const cur = get().sessionId;
    // 未读计数：非当前会话收到新末条（后台回复/通道消息）→ 未读 +1（当前会话即时已读）
    if (cur !== sessionId) {
      const n = (get().unreadCounts[sessionId] ?? 0) + 1;
      set((s) => ({ unreadCounts: { ...s.unreadCounts, [sessionId]: n } }));
    }
    set((s) => ({ sessions: s.sessions.map((x) => (x.id === sessionId ? { ...x, lastMessagePreview: preview, lastActiveAt: new Date().toISOString() } : x)) }));
  },
  /** 进入会话即清未读（切走再切回重新计数） */
  clearUnread: (sessionId) => {
    if (get().unreadCounts[sessionId]) {
      set((s) => {
        const next = { ...s.unreadCounts };
        delete next[sessionId];
        return { unreadCounts: next };
      });
    }
  },
  /** 全部已读：清空未读表（内存态，无需服务端对账） */
  markAllRead: () => {
    if (Object.keys(get().unreadCounts).length > 0) {
      set({ unreadCounts: {} });
    }
  },
  /** 会话置顶切换：乐观更新 + 服务端落库（失败回滚） */
  toggleSessionPin: async (sessionId, pinned) => {
    const prev = get().sessions;
    set({ sessions: prev.map((x) => (x.id === sessionId ? { ...x, pinned } : x)) });
    try {
      await api.pinSession(sessionId, pinned);
    } catch {
      set({ sessions: prev });
    }
  },

  handleEvent: (evt) => {
    const p = evt.payload as Record<string, unknown>;
    if (evt.sessionId === get().sessionId) lifecycle.stateRevision += 1;
    // 审批不受当前会话限制：子会话（子个体）里的审批请求同样要在视图内可裁决
    if (evt.type === "approval.required") {
      lifecycle.approvalRevision += 1;
      // 全局计数即时刷新（侧栏角标；不依赖轮询周期）
      void get().refreshPendingApprovals();
      // Project clients only surface the server-filtered pending list, including child approvals.
      if (project) return;
      options.onEvent?.(evt, get());
      set({
        approvals: [
          ...get().approvals.filter((a) => a.approvalId !== String(p.approvalId)),
          {
            approvalId: String(p.approvalId),
            agentId: String(p.agentId ?? ""),
            command: String(p.command ?? ""),
            sessionId: evt.sessionId || undefined,
          },
        ],
      });
      return;
    }
    if (evt.type === "approval.resolved") {
      lifecycle.approvalRevision += 1;
      set({ approvals: get().approvals.filter((a) => a.approvalId !== String(p.approvalId)) });
      void get().refreshPendingApprovals();
      options.onEvent?.(evt, get());
      return;
    }
    // 任务清单（spec agent-todo）：todo.updated 快照（仅当前会话）
    if (evt.type === "todo.updated") {
      if (evt.sessionId === get().sessionId) {
        set({
          todo: {
            count: Number(p.count ?? 0),
            doneCount: Number(p.doneCount ?? 0),
            allDone: Boolean(p.allDone),
            items: ((p.items ?? []) as unknown as TodoItem[]),
          },
        });
      }
      return;
    }
    // 任务队列（task-queue）：入队/出队/状态回填实时快照
    if (evt.type === "task_queue.updated") {
      if (evt.sessionId === get().sessionId) {
        set({
          taskQueue: {
            running: Number(p.running ?? 0),
            queued: Number(p.queued ?? 0),
            items: (p.items ?? []) as unknown as QueuedTaskItem[],
          },
        });
      }
      return;
    }
    // 后台子任务卡（spec subagent-spawn WebUI 呈现）：started 建条目 / finished 收终态
    if (evt.type === "spawn.started" || evt.type === "spawn.finished") {
      if (evt.sessionId !== get().sessionId) return;
      const spawnId = String(p.spawnId ?? "");
      if (!spawnId) return;
      if (evt.type === "spawn.started") {
        // 只跟踪后台派生（同步派生的结果在工具调用内直接可见）
        if (!p.background) return;
        const preview = String(p.taskPreview ?? "");
        const fresh: SpawnBgItem = { spawnId, preview, status: "running", startedAt: Date.now() };
        set({ spawns: [fresh, ...get().spawns.filter((s) => s.spawnId !== spawnId)].slice(0, 20) });
        return;
      }
      // finished：更新终态；running 条目缺失时补建（事件先于 started 到达等边缘）
      const status = p.status === "done" ? "done" : p.status === "timeout" ? "timeout" : "failed";
      const existing = get().spawns.find((s) => s.spawnId === spawnId);
      const next: SpawnBgItem = existing
        ? { ...existing, status, resultPreview: String(p.summaryPreview ?? ""), finishedAt: Date.now() }
        : {
            spawnId,
            preview: String(p.taskPreview ?? p.summaryPreview ?? ""),
            status,
            resultPreview: String(p.summaryPreview ?? ""),
            startedAt: Date.now(),
            finishedAt: Date.now(),
          };
      set({ spawns: [next, ...get().spawns.filter((s) => s.spawnId !== spawnId)] });
      return;
    }
    // 计划事件（spec plan-mode：WebUI 卡片实时刷新，不靠轮询）
    // 卡片只跟随当前会话：子会话的计划条目不混入本视图（切过去自然加载）
    if (evt.type.startsWith("plan.")) {
      const sid = evt.sessionId || get().sessionId;
      if (!sid || sid !== get().sessionId) return;
      switch (evt.type) {
        case "plan.item":
          // 新条目累积：防抖拉取最新清单（条目可能连续到达）
          void get().refreshPlan(sid);
          break;
        case "plan.pending":
          set({ planPending: true });
          void get().refreshPlan(sid);
          break;
        case "plan.mode":
          // agent 经 plan__mode 工具切换模式（plan-mode-tools）：plan 接口返回 mode 位，
          // 防抖拉取刷新徽标（reason 由工具结果呈现给用户，前端不重复展示）
          void get().refreshPlan(sid);
          break;
        case "plan.approved":
          // 回放中：applied 事件会带逐条结果
          set({ planPending: false });
          break;
        case "plan.rejected":
          set({ planPending: false });
          void get().refreshPlan(sid);
          break;
        case "plan.applied":
          // 事件逐条发出（每 payload 单项）：增量累积成回放结果
          set({
            planApplied: [
              ...(get().planApplied ?? []),
              {
                itemId: String(p.itemId ?? ""),
                tool: String(p.tool ?? ""),
                target: String(p.target ?? ""),
                ok: Boolean(p.ok),
              },
            ],
            planPending: false,
          });
          void get().refreshPlan(sid);
          break;
      }
      return;
    }
    if (evt.sessionId !== get().sessionId) return;
    switch (evt.type) {
      case "model.selected":
      case "model.used": {
        const model = [p.profile, p.model].filter(Boolean).join("/");
        const text = evt.type === "model.used" ? tr("routing.used", { agent: String(p.agentId ?? ""), model }) : tr("routing.selected", {
          agent: String(p.agentId ?? ""), purpose: tr(`routing.purpose.${p.purpose ?? "chat"}`), model,
          reason: String(p.reason || tr("routing.manualReason")),
        });
        set({ timelineSession: evt.sessionId, timeline: [...get().timeline, { kind: "model", agentId: String(p.agentId ?? ""), text }] });
        break;
      }
      case "orchestrator.token":
        set({ running: true, liveOrch: get().liveOrch + String(p.delta ?? "") });
        break;
      case "orchestrator.thinking":
        set({ running: true, liveThinking: get().liveThinking + String(p.delta ?? "") });
        break;
      case "unit.thinking": {
        if (p.direct) set({ running: true });
        const agent = String(p.agentId ?? "");
        set({ liveParts: appendPart(get().liveParts, agent, { kind: "thinking", text: String(p.delta ?? "") }) });
        break;
      }
      case "tool.call": {
        const call: ToolCallItem = {
          callId: String(p.callId),
          agentId: String(p.agentId ?? ""),
          tool: String(p.tool ?? ""),
          args: (p.args ?? {}) as Record<string, unknown>,
          status: "running",
        };
        set({
          runToolCalls: [...get().runToolCalls.filter((c) => c.callId !== call.callId), call],
          liveParts: appendPart(get().liveParts, String(p.agentId ?? ""), { kind: "tool", callId: call.callId }),
        });
        break;
      }
      case "tool.result": {
        const callId = String(p.callId);
        set({
          runToolCalls: get().runToolCalls.map((c) =>
            c.callId === callId
              ? {
                  ...c,
                  status: p.ok ? "ok" : "error",
                  durationMs: Number(p.durationMs ?? 0),
                  summary: String(p.summary ?? "").slice(0, 400),
                  images: (p.images ?? []) as string[],
                }
              : c,
          ),
        });
        break;
      }
      case "unit.token": {
        if (p.direct) set({ running: true });
        const agent = String(p.agentId ?? "");
        set({ liveParts: appendPart(get().liveParts, agent, { kind: "text", text: String(p.delta ?? "") }) });
        break;
      }
      case "unit.finished": {
        if (p.direct) set({ running: false });
        // 子会话实时视图收尾：清实时段并回拉持久消息（含有序步骤）
        const agent = String(p.agentId ?? "");
        const parts = { ...get().liveParts };
        delete parts[agent];
        set({ liveParts: parts });
        const sid2 = evt.sessionId;
        // 回填代际守卫（同 run.finished）：回拉期间的选择切换不得被旧快照覆盖
        const refilSeq2 = lifecycle.selectSeq;
        const revision2 = lifecycle.stateRevision;
        void (async () => {
          try {
            const msgs = await api.messages(sid2);
            if (useExm.getState().sessionId === sid2 && lifecycle.selectSeq === refilSeq2 && lifecycle.stateRevision === revision2) useExm.setState({ messages: msgs });
          } catch {
            /* 回拉失败静默：刷新/切会话时自然补齐 */
          }
        })();
        break;
      }
      case "graph.updated":
        set({ graph: p as unknown as TaskGraph });
        break;
      case "dispatch.sent": {
        const childSessionId =
          typeof p.childSessionId === "string" && p.childSessionId ? String(p.childSessionId) : undefined;
        set({
          timelineSession: evt.sessionId,
          timeline: [
            ...get().timeline,
            {
              kind: "dispatch",
              agentId: String(p.agentIdentifier),
              nodeId: String(p.nodeId),
              childSessionId,
              text: tr("store.tl.dispatch", { agent: String(p.agentIdentifier), node: String(p.nodeId) }),
            },
          ],
        });
        // 子会话是运行中新出现的会话：列表补拉一次（派发频率低，开销可忽略）
        if (childSessionId && !get().sessions.some((s) => s.id === childSessionId)) {
          void api
            .listSessions()
            .then((list) => set({ sessions: list }))
            .catch(() => {
              /* 列表补拉失败静默 */
            });
        }
        break;
      }
      case "sync.received": {
        const report = p.report as {
          sourceAgent: string;
          taskNodeId: string;
          summary: string;
          confidence: number;
        };
        const parts = { ...get().liveParts };
        delete parts[report.sourceAgent];
        set({
          liveParts: parts,
          timelineSession: evt.sessionId,
          timeline: [
            ...get().timeline,
            {
              kind: "sync",
              agentId: report.sourceAgent,
              nodeId: report.taskNodeId,
              text: tr("store.tl.sync", { agent: report.sourceAgent, summary: report.summary.slice(0, 80), conf: report.confidence }),
            },
          ],
        });
        break;
      }
      case "memory.recall": {
        const hits = (p.hits ?? []) as RecallHit[];
        set({
          lastRecall: hits,
          timelineSession: evt.sessionId,
          timeline: [...get().timeline, { kind: "memory", text: tr("store.tl.memory", { n: hits.length }) }],
        });
        break;
      }
      case "memory.written": {
        const entries = (p.entries ?? []) as unknown[];
        set({
          timelineSession: evt.sessionId,
          timeline: [
            ...get().timeline,
            {
              kind: "memory",
              text: tr("store.tl.memoryWrite", { n: entries.length, pinned: Number(p.pinned ?? 0) }),
            },
          ],
        });
        get().bumpMemory();
        break;
      }
      case "arbitration.required":
        set({
          timelineSession: evt.sessionId,
          timeline: [
            ...get().timeline,
            { kind: "arbitration", text: tr("store.tl.arbitration") },
          ],
        });
        break;
      case "ledger.updated":
        set({ ledger: p as unknown as SessionLedger });
        break;
      case "run.finished": {
        options.onEvent?.(evt, get());
        const statements = (p.statements ?? []) as Statement[];
        // 产物附件（send-file 能力）：控制台会话的文件投递——IM 通道由适配器
        // 直发（不进消息流），这里只渲染控制台侧
        const artifacts = (p.artifacts ?? []) as ArtifactItem[];
        const id = get().sessionId!;
        // 权威回填代际：回拉期间若用户切走（selectSession 落了新会话），不得覆盖。
        // 比较选择代际：回拉发起时的序号仍是最新 → 没有更新的选择发生 → 安全落盘。
        const refilSeq = lifecycle.selectSeq;
        const revision = lifecycle.stateRevision;
        const toolCalls = get().runToolCalls;
        const thinking = get().liveThinking;
        set({
          running: false,
          liveOrch: "",
          liveThinking: "",
          liveParts: {},
          runToolCalls: [],
          messages: [
            ...get().messages,
            {
              id: `fin-${Date.now()}`,
              sessionId: id,
              role: "orchestrator",
              agentId: typeof p.agentId === "string" ? p.agentId : "orchestrator",
              statements,
              toolCalls: toolCalls.length > 0 ? toolCalls : undefined,
              thinking: thinking.trim() ? thinking : undefined,
              artifacts: artifacts.length > 0 ? artifacts : undefined,
              createdAt: new Date().toISOString(),
            },
          ],
        });
        const finText = statements.map((st) => st.text).join(" ");
        if (finText.trim()) get().updateSessionPreview(id, finText);
        // 权威回填：最终消息含服务端留存的有序步骤（本地乐观消息只带聚合过程）
        void (async () => {
          try {
            const msgs = await api.messages(id);
            // 双守卫：会话号一致 + 选择代际未变（回拉期间切走又切回的乱序覆盖）。
            // 服务端快照含最终消息，覆盖是安全的；只在「已发生更新的选择」时让位。
            if (useExm.getState().sessionId === id && lifecycle.selectSeq === refilSeq && lifecycle.stateRevision === revision) useExm.setState({ messages: msgs });
          } catch {
            /* 回拉失败静默：保留本地乐观消息 */
          }
        })();
        break;
      }
      case "cron.finished": {
        options.onEvent?.(evt, get());
        set({
          timelineSession: evt.sessionId,
          timeline: [...get().timeline, { kind: "cron", text: tr("store.tl.cron", { name: String(p.jobName ?? ""), status: String(p.status ?? "") }) }],
        });
        break;
      }
      case "run.error":
        set({
          running: false,
          liveOrch: "",
          liveThinking: "",
          liveParts: {},
          runToolCalls: [],
          timelineSession: evt.sessionId,
          timeline: [...get().timeline, { kind: "error", text: tr("store.tl.error", { msg: String(p.message) }) }],
        });
        break;
      // 工具预算熔断（agent-budget-audit）：消息流内联提示——说明本轮为何停止。
      // reason 文案后端已定稿；模型收束前可能连续多次拦截，相邻重复只提示一次。
      case "tool.budget": {
        const reason = String(p.reason ?? "");
        const last = get().timeline[get().timeline.length - 1];
        if (!last || last.kind !== "budget" || last.text !== reason) {
          set({
            timelineSession: evt.sessionId,
            timeline: [...get().timeline, { kind: "budget", text: reason }],
          });
        }
        break;
      }
    }
  },
}));
const connection = socketRuntime(lifecycle, api, serverBase, accessKey, useExm.getState, useExm.setState, () => disposed);
return Object.assign(useExm, {
  reconnect: () => connection.reconnect(),
  dispose: () => {
    disposed = true;
    ++lifecycle.selectSeq;
    ++lifecycle.approvalRequest;
    connection.dispose();
  },
});
}

export type ExmStore = ReturnType<typeof createExmStore>;
