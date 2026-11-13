//! api/knowledge.ts —— 知识库域（knowledge-base）：共享知识的清单/检索/新增/删除。

import { req } from "./core";
import type { KnowledgeEntry } from "./core";

export const apiKnowledge = {
  listKnowledge: (opts?: { q?: string; limit?: number }) => {
    const p = new URLSearchParams();
    if (opts?.q) p.set("q", opts.q);
    p.set("limit", String(opts?.limit ?? 50));
    return req<{ items: KnowledgeEntry[]; total: number }>(`/knowledge?${p.toString()}`);
  },

  addKnowledge: (body: { title: string; body: string; tags?: string[] }) =>
    req<{ entry: KnowledgeEntry; deduped: boolean }>("/knowledge", {
      method: "POST",
      body: JSON.stringify(body),
    }),

  deleteKnowledge: (id: string) => req<{ ok: boolean }>(`/knowledge/${id}`, { method: "DELETE" }),
};
