//! api/sessions.ts —— 会话域：会话 CRUD、消息读写、undo/edit/fork、图谱、轮次快照、聊天发送、计划裁决（codebase-refactor 7.1 拆分）。纯搬移。

import { req } from "./core";
import type { ChatMessage, EvidenceItem, PlanAppliedItem, PlanInfo, Session, SessionSearchHit, SessionSnapshot, StoredEvent, TaskGraph } from "./core";
import { tr } from "../i18n/core";
export const apiSessions = {
  speak: async (text: string) => {
    const key = localStorage.getItem("exm.key") ?? "";
    const resp = await fetch("/api/voice/speak", {
      method: "POST",
      headers: { "Content-Type": "application/json", ...(key ? { "X-Auth-Key": key } : {}) },
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
  /** 按流/按通道归档旧会话（session-management）：scope=flow 时 channelId+chatKey
   *  圈定单流；scope=channel 时归档该通道全部流的旧会话。流绑定会话豁免。 */

  archiveSessionsByScope: (scope: "flow" | "channel", channelId: string, chatKey?: string) =>
    req<{ ok: boolean; archived: number; skipped: number }>("/sessions/flow-archive", {
      method: "POST",
      body: JSON.stringify({ scope, channelId, chatKey }),
    }),

  createSession: (title: string) =>
    req<Session>("/sessions", { method: "POST", body: JSON.stringify({ title }) }),

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
    req<ChatMessage[]>(`/sessions/${id}/messages`).then((list) =>
      // 服务端 ToolCallRecord 用 ok:bool（收束落库形态），前端统一水合为 status 三态；
      // 否则刷新后工具卡读到 undefined 会误显「失败」图标。
      list.map((m) =>
        m.toolCalls?.some((c) => !(c as { status?: string }).status)
          ? {
              ...m,
              toolCalls: m.toolCalls.map((c) => ({
                ...c,
                status: (c as { ok?: boolean }).ok === false ? "error" : "ok",
              })),
            }
          : m,
      ),
    ),

  stopSession: (id: string) =>
    req<{ ok: boolean; toolsExecuted: number }>(`/sessions/${id}/stop`, { method: "POST", body: JSON.stringify({}) }),

  chat: (id: string, text: string, images?: string[], mode?: string) =>
    req<{ accepted: boolean }>(`/sessions/${id}/chat`, {
      method: "POST",
      body: JSON.stringify({
        text,
        ...(images?.length ? { images } : {}),
        ...(mode ? { mode } : {}),
      }),
    }),

  graph: (id: string) => req<TaskGraph>(`/sessions/${id}/graph`),

  evidence: (id: string) => req<EvidenceItem[]>(`/sessions/${id}/evidence`),

  sessionEvents: (id: string, limit = 200) =>
    req<StoredEvent[]>(`/sessions/${id}/events?limit=${limit}`),

  getPlan: (id: string) => req<PlanInfo>(`/sessions/${id}/plan`),

  decidePlan: (id: string, action: "approve" | "reject" | "setMode", plan?: boolean) =>
    req<{ sessionId: string; ok?: boolean; okCount?: number; failCount?: number; applied?: PlanAppliedItem[] }>(
      `/sessions/${id}/plan`,
      { method: "POST", body: JSON.stringify({ action, plan }) },
    ),
};
