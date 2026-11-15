//! api/memory.ts —— 记忆域：记忆条目 CRUD 与检索、统计、重索引、衰减、渲染（codebase-refactor 7.1 拆分）。纯搬移。

import { req } from "./core";
import type { MemoryEntry, MemoryStats, RecallHit } from "./core";
export const apiMemory = {
  listMemory: (opts?: { kind?: string; agent?: string; limit?: number }) => {
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
};
