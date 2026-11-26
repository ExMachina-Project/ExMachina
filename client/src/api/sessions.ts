//! api/sessions.ts —— 会话域：会话 CRUD、消息读写、undo/edit/fork、图谱、轮次快照、聊天发送、计划裁决（codebase-refactor 7.1 拆分）。纯搬移。

import type { RequestJson } from "./core";
import type { ChatMessage, ContextSourceSetResponse, ContextSourcesResponse, ContextSourceRefreshReport, EvidenceItem, PlanAppliedItem, PlanInfo, QueuedTask, Session, SessionSearchHit, SessionSnapshot, StoredEvent, TaskGraph, WorkflowRelation } from "./core";
import type { Translate } from "../translate";

/** Persisted tool records use ok:bool; the chat UI uses the three-state status. */
export function hydrateToolStatuses(message: ChatMessage): ChatMessage {
  if (!message.toolCalls?.some((call) => !call.status)) return message;
  return { ...message, toolCalls: message.toolCalls.map((call) => ({
    ...call,
    status: call.status ?? ((call as { ok?: boolean }).ok === false ? "error" : "ok"),
  })) };
}

export function createSessionsApi(transport: { req: RequestJson; serverBase: () => string; authHeaders: () => Record<string, string>; workspace?: string }, tr: Translate) {
  const { req, serverBase, authHeaders } = transport;
  // Older gateways silently ignore unknown target fields. Probe before any
  // explicit-target operation; never emulate it by changing global active.
  const requireTargetSupport = async () => {
    const health = await req<{ features?: string[] }>("/health");
    if (!Array.isArray(health.features) || !health.features.includes("session-target-v1")) throw new Error(tr("operation.unsupported"));
  };
  const checkTarget = (session: Session, target: string) => {
    if (session.groupId !== target || (session.workspace ?? undefined) !== transport.workspace) {
      throw new Error(tr("operation.scopeMismatch"));
    }
    return session;
  };
  return {
  listSessionsForTarget: async (target: string) => {
    await requireTargetSupport();
    const query = new URLSearchParams({ target, ...(transport.workspace ? { workspace: transport.workspace } : {}) });
    const list = await req<Session[]>(`/sessions?${query}`);
    return list.map((session) => checkTarget(session, target));
  },

  createSessionForTarget: async (title: string, target: string) => {
    await requireTargetSupport();
    const session = await req<Session>("/sessions", { method: "POST", body: JSON.stringify({ title, target, ...(transport.workspace ? { workspace: transport.workspace } : {}) }) });
    return checkTarget(session, target);
  },
  speak: async (text: string) => {
    const resp = await fetch(`${serverBase()}/api/voice/speak`, {
      method: "POST",
      headers: { "Content-Type": "application/json", ...authHeaders() },
      body: JSON.stringify({ text }),
    });
    if (!resp.ok) {
      const err = (await resp.json().catch(() => ({}))) as { error?: string };
      throw new Error(err.error ?? tr("api.synthFailed", { status: resp.status }));
    }
    return resp.blob();
  },

  sessionTokens: (id: string) =>
    req<{ estimate: number; budget: number; unlimited: boolean }>(`/sessions/${id}/tokens`),

  listSessions: () => req<Session[]>("/sessions"),
  /** 管理视角全量会话（含所有组；归档态默认排除） */

  listAllSessions: () => req<Session[]>("/sessions?all=true"),
  /** 归档区列表（仅归档态，按归档时间倒序） */

  listArchivedSessions: () => req<Session[]>("/sessions?archived=true"),
  /** 批量操作（archive / unarchive / delete）：返回作用计数与失败明细 */

  batchSessions: (action: "archive" | "unarchive" | "delete", ids: string[]) =>
    req<{ ok: boolean; affected: number; failed: { id: string; error: string }[] }>("/sessions/batch", {
      method: "POST",
      body: JSON.stringify({ action, ids }),
    }),
  /** 按流/按通道归档旧会话：scope=flow 时 channelId+chatKey
   *  圈定单流；scope=channel 时归档该通道全部流的旧会话。流绑定会话豁免。 */

  archiveSessionsByScope: (scope: "flow" | "channel", channelId: string, chatKey?: string) =>
    req<{ ok: boolean; archived: number; skipped: number }>("/sessions/flow-archive", {
      method: "POST",
      body: JSON.stringify({ scope, channelId, chatKey }),
    }),

  createSession: (title: string, options?: { workspace?: string; target?: string }) =>
    req<Session>("/sessions", { method: "POST", body: JSON.stringify({ title, ...options }) }),

  getSession: (id: string) => req<Session>(`/sessions/${id}`),

  renameSession: (id: string, title: string) =>
    req<{ ok: boolean; title: string }>(`/sessions/${id}/title`, {
      method: "PUT",
      body: JSON.stringify({ title }),
    }),

  pinSession: (id: string, pinned: boolean) =>
    req<{ ok: boolean; pinned: boolean }>(`/sessions/${id}/pin`, {
      method: "PUT",
      body: JSON.stringify({ pinned }),
    }),

  deleteSession: (id: string) => req<{ ok: boolean }>(`/sessions/${id}`, { method: "DELETE" }),
  // 会话历史管控（组 9）：撤销 / 编辑重发 / 分支派生 / 归档与快照视图,

  undoSession: (id: string, turn: number, restoreFiles = true) =>
    req<{ uptoTurn: number; archived: number; restored: string[] }>(`/sessions/${id}/undo`, {
      method: "POST",
      body: JSON.stringify({ turn, restoreFiles }),
    }),

  editSession: (id: string, turn: number, text: string) =>
    req<{ resent: boolean }>(`/sessions/${id}/edit`, {
      method: "POST",
      body: JSON.stringify({ turn, text }),
    }),

  forkSession: (id: string, turn: number) =>
    req<{ id: string; title: string }>(`/sessions/${id}/fork`, {
      method: "POST",
      body: JSON.stringify({ turn }),
    }),

  sessionArchive: (id: string) => req<ChatMessage[]>(`/sessions/${id}/archive`),

  sessionSnapshots: (id: string) => req<SessionSnapshot[]>(`/sessions/${id}/snapshots`),
  /** 会话内容检索：标题 + 最近 200 条消息（空 q 由服务端 400，前端不发） */

  searchSessions: (q: string, limit = 20) =>
    req<{ query: string; hits: SessionSearchHit[] }>(
      `/sessions/search?${new URLSearchParams({ q, limit: String(limit) }).toString()}`,
    ),

  messages: (id: string) =>
    req<ChatMessage[]>(`/sessions/${id}/messages`).then((list) => list.map(hydrateToolStatuses)),

  stopSession: (id: string) =>
    req<{ ok: boolean; toolsExecuted: number }>(`/sessions/${id}/stop`, { method: "POST", body: JSON.stringify({}) }),

  chat: (id: string, text: string, images?: string[], mode?: string, agent?: string) =>
    req<{ accepted: boolean }>(`/sessions/${id}/chat`, {
      method: "POST",
      body: JSON.stringify({
        text,
        ...(images?.length ? { images } : {}),
        ...(mode ? { mode } : {}),
        ...(agent ? { agent } : {}),
      }),
    }),

  graph: (id: string) => req<TaskGraph>(`/sessions/${id}/graph`),

  evidence: (id: string) => req<EvidenceItem[]>(`/sessions/${id}/evidence`),

  sessionEvents: (id: string, limit = 200) =>
    req<StoredEvent[]>(`/sessions/${id}/events?limit=${limit}`),

  getPlan: (id: string) => req<PlanInfo>(`/sessions/${id}/plan`),

  /** 会话工作流关联视图（4.5/4.6）：goal/spec/plan 只读快照 */
  getWorkflow: (id: string) => req<WorkflowRelation>(`/sessions/${id}/workflow`),

  /** 上下文来源清单（capability-parity 6.3）：文件/记忆/检索/技能 + 状态/禁用位/缺失原因 */
  getContextSources: (id: string) => req<ContextSourcesResponse>(`/sessions/${id}/context`),

  /** 上下文来源治理：停用/恢复只关掉**自动注入**（模型主动召回、用户显式 /skill 不受影响）；
   *  refresh 单来源重读，失败是 200 + ok=false（不影响其余来源）。 */
  decideContextSource: (
    id: string,
    action: "disable" | "enable" | "refresh",
    key?: string,
  ) =>
    req<ContextSourceSetResponse | ContextSourceRefreshReport>(
      `/sessions/${id}/context`,
      { method: "POST", body: JSON.stringify({ action, key }) },
    ),

  decidePlan: (id: string, action: "approve" | "reject" | "setMode", plan?: boolean) =>
    req<{ sessionId: string; ok?: boolean; okCount?: number; failCount?: number; applied?: PlanAppliedItem[] }>(
      `/sessions/${id}/plan`,
      { method: "POST", body: JSON.stringify({ action, plan }) },
    ),

  /** 后续任务队列（capability-parity 3.3/3.5）：显式入队 / 查询。
   *  回执 queued+pending 不冒充完成（session-steering spec）。 */
  queueAdd: (id: string, task: string) =>
    req<{ ok: boolean; status: string; queueId: string; seq: number; pending: boolean }>(
      `/sessions/${id}/queue`,
      { method: "POST", body: JSON.stringify({ task, source: "webui" }) },
    ),

  queueList: (id: string) =>
    req<{ sessionId: string; items: QueuedTask[] }>(`/sessions/${id}/queue`),

  queueRemove: (id: string, qid: string) =>
    req<{ ok: boolean }>(`/sessions/${id}/queue/${qid}`, { method: "DELETE" }),

  queueEdit: (id: string, qid: string, task: string, revision: number) =>
    req<{ ok: boolean }>(`/sessions/${id}/queue/${qid}`, {
      method: "PATCH",
      body: JSON.stringify({ task, revision }),
    }),
}; }
