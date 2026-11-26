//! Memory CRUD, retrieval, statistics, indexing, and rendering APIs.

import { req } from "./core";
import type {
  MemoryEntry,
  MemoryRestoreReceipt,
  MemoryRollbackReceipt,
  MemoryStats,
  MemoryVersionsResponse,
  RecallHit,
} from "./core";
export const apiMemory = {  listMemory: (opts?: { kind?: string; agent?: string; limit?: number }) => {
    const p = new URLSearchParams();
    if (opts?.kind) p.set("kind", opts.kind);
    if (opts?.agent) p.set("agent", opts.agent);
    p.set("limit", String(opts?.limit ?? 50));
    return req<MemoryEntry[]>(`/memory?${p.toString()}`);
  },

  searchMemory: (query: string, limit = 8, agent?: string) =>
    req<{ query: string; hits: RecallHit[] }>("/memory/search", {
      method: "POST",
      body: JSON.stringify({ query, limit, agent }),
    }),

  memoryStats: () => req<MemoryStats>("/memory/stats"),

  addMemory: (body: {
    kind?: string;
    title: string;
    body: string;
    tags?: string[];
    pin?: boolean;
    importance?: number;
    agentId?: string;
  }) => req<MemoryEntry>("/memory", { method: "POST", body: JSON.stringify(body) }),

  pinMemory: (id: string, pinned: boolean) =>
    req<{ ok: boolean }>(`/memory/${id}/pin`, { method: "POST", body: JSON.stringify({ pinned }) }),

  forgetMemory: (id: string) => req<{ ok: boolean }>(`/memory/${id}`, { method: "DELETE" }),

  updateMemory: (id: string, body: { title?: string; body?: string; tags?: string[]; kind?: string }) =>
    req<MemoryEntry>(`/memory/${id}`, { method: "PUT", body: JSON.stringify(body) }),

  reindexMemory: () => req<{ ok: boolean }>("/memory/reindex", { method: "POST" }),

  decayMemory: () => req<{ ok: boolean }>("/memory/decay", { method: "POST" }),

  renderMemory: () => req<{ ok: boolean }>("/memory/render", { method: "POST" }),

  // ---------------- 版本链（capability-parity 6.5）----------------
  // 与 CLI（exm memory versions/rollback/restore）、面板（/memory versions…）
  // 共用后端同一版本事实源；这里是它的 WebUI 入口。

  /** 版本历史（旧 → 新）。已遗忘的条目仍可读：current=null、forgotten=true。 */
  memoryVersions: (id: string) => req<MemoryVersionsResponse>(`/memory/${id}/versions`),

  /** 已遗忘（可撤销）的条目，遗忘时间倒序。条目从常规列表消失后，从这里撤销。 */
  forgottenMemory: () => req<{ entries: MemoryEntry[] }>("/memory/forgotten"),

  /** 回滚到历史版本：以旧内容写新版本行（回执带新版本号）。 */
  rollbackMemory: (id: string, toVersion: number) =>
    req<MemoryRollbackReceipt>(`/memory/${id}/rollback`, {
      method: "POST",
      body: JSON.stringify({ toVersion }),
    }),

  /** 撤销遗忘：以被遗忘时的内容重建条目，归属恢复原样。 */
  restoreMemory: (id: string) =>
    req<MemoryRestoreReceipt>(`/memory/${id}/restore`, { method: "POST" }),
};
