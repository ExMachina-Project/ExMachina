//! api/models.ts —— 模型域：LLM 档案、全局槽位、供应商模型列举、能力矩阵（codebase-refactor 7.1 拆分）。纯搬移。

import { req } from "./core";
import type { LlmCapabilities, LlmProfilesInfo, ProfileModel } from "./core";
export const apiModels = {
  llmProfiles: () => req<LlmProfilesInfo>("/llm/profiles"),

  saveLlmProfile: (body: { id?: string; name?: string; baseUrl?: string; apiFormat?: string; apiKey?: string; apiKeys?: (string | { keep: number })[]; clearKeys?: boolean; model?: string; models?: ProfileModel[]; fallback?: string }) =>
    req<{ ok: boolean; id: string; mock?: boolean }>("/llm/profiles", { method: "POST", body: JSON.stringify(body) }),

  deleteLlmProfile: (id: string) => req<{ ok: boolean }>(`/llm/profiles/${id}`, { method: "DELETE" }),

  activateLlmProfile: (id: string) =>
    req<{ ok: boolean; active: string; mock: boolean }>("/llm/active", { method: "PUT", body: JSON.stringify({ id }) }),

  testLlmProfile: (id?: string) =>
    req<{ ok: boolean; mock?: boolean; configured?: boolean; status?: number; latencyMs?: number; snippet?: string; message?: string; error?: string }>("/llm/test", {
      method: "POST",
      body: JSON.stringify(id ? { id } : {}),
    }),
  // 系统提示词（PROMPT）读写：组内个体按组作用域；智能体走 singles 端点,

  listProviderModels: (body: { id?: string; baseUrl?: string; apiKey?: string; apiFormat?: string }) =>
    req<{ ok: boolean; configured?: boolean; status?: number; models: string[]; message?: string; error?: string }>("/llm/models", {
      method: "POST",
      body: JSON.stringify(body),
    }),

  llmCapabilities: () => req<LlmCapabilities>("/llm/capabilities"),

  saveLlmCapabilities: (body: Partial<LlmCapabilities>) =>
    req<LlmCapabilities & { ok: boolean }>("/llm/capabilities", { method: "PUT", body: JSON.stringify(body) }),
};
