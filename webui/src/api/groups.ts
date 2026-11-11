//! api/groups.ts —— 编成域：组/个体/独立智能体、人格、提示词、模板、目标与工作目录（codebase-refactor 7.1 拆分）。纯搬移。

import { req } from "./core";
import type { AgentDefinition, GroupCapabilities, GroupMeta, GroupOverview, PersonaInfo, TemplateInfo } from "./core";
export const apiGroups = {
  agents: () => req<AgentDefinition[]>("/agents"),  groups: () => req<{ active: string; groups: GroupMeta[] }>("/groups"),

  createGroup: (body: { name: string; id?: string; description?: string; template?: string }) =>
    req<GroupMeta>("/groups", { method: "POST", body: JSON.stringify(body) }),
  /** 编成模板清单(unit=子个体模板,group=组模板) */

  listTemplates: () => req<{ templates: TemplateInfo[] }>("/templates"),
  /** 本机版本 */

  switchGroup: (id: string) =>
    req<{ ok: boolean }>("/groups/active", { method: "PUT", body: JSON.stringify({ id }) }),

  deleteGroup: (id: string) => req<{ ok: boolean }>(`/groups/${id}`, { method: "DELETE" }),

  createAgent: (body: {
    name: string;
    identifier: string;
    description: string;
    domain?: string;
    tier?: string;
    prompt?: string;
    group?: string;
    template?: string;
  }) => req<AgentDefinition>("/agents", { method: "POST", body: JSON.stringify(body) }),

  removeAgent: (identifier: string) =>
    req<{ ok: boolean }>(`/agents/${identifier}`, { method: "DELETE" }),

  removeGroupAgent: (gid: string, identifier: string) =>
    req<{ ok: boolean }>(`/groups/${gid}/agents/${identifier}`, { method: "DELETE" }),

  getPersona: (identifier: string) => req<PersonaInfo>(`/agents/${identifier}/persona`),

  putPersona: (identifier: string, persona: string) =>
    req<{ ok: boolean }>(`/agents/${identifier}/persona`, {
      method: "PUT",
      body: JSON.stringify({ persona }),
    }),

  resetPersona: (identifier: string) =>
    req<{ ok: boolean }>(`/agents/${identifier}/persona`, { method: "DELETE" }),
  /** 智能体人设（entities/agents/<id>/SOUL.md，与组内个体同语义） */

  singlePersona: (id: string) => req<PersonaInfo>(`/singles/${id}/persona`),

  singlesSetPersona: (id: string, persona: string) =>
    req<{ ok: boolean }>(`/singles/${id}/persona`, { method: "PUT", body: JSON.stringify({ persona }) }),

  singlesResetPersona: (id: string) =>
    req<{ ok: boolean }>(`/singles/${id}/persona`, { method: "DELETE" }),

  groupAgents: (gid: string) => req<AgentDefinition[]>(`/groups/${gid}/agents`),
  /** 组信息更新（group-card-menu）：改名 + 可选改 id（改 id 迁移会话/记忆/任务/触发器引用） */

  updateGroup: (gid: string, body: { name: string; id?: string }) =>
    req<GroupMeta>(`/groups/${gid}`, { method: "PUT", body: JSON.stringify(body) }),
  /** 自我优化开关（group-self-optimize）：开启后主智能体可自行增删改子个体 */

  setGroupSelfOptimize: (gid: string, enabled: boolean) =>
    req<{ ok: boolean; selfOptimize: boolean }>(`/groups/${gid}/self-optimize`, {
      method: "PUT",
      body: JSON.stringify({ enabled }),
    }),

  getTarget: () => req<{ mode: "group" | "single"; id: string; name?: string; primary?: string }>("/target"),

  setTarget: (mode: "group" | "single", id?: string) =>
    req<{ ok: boolean }>("/target", { method: "PUT", body: JSON.stringify({ mode, id }) }),

  listSingles: () => req<{ active?: string; singles: { identifier: string; name: string; description: string; domain: string; modelHint?: string | null }[] }>("/singles"),

  setAgentModel: (identifier: string, model: string) =>
    req<{ ok: boolean }>(`/agents/${identifier}/model`, {
      method: "PUT",
      body: JSON.stringify({ model }),
    }),
  /** 更新个体可编辑字段（name/domain/description/capabilities/tools/modelHint/link；identifier/tier/promptFile 不可改） */

  updateAgent: (
    identifier: string,
    body: { name?: string; domain?: string; description?: string; capabilities?: string[]; tools?: string[]; modelHint?: string; link?: string },
  ) => req<AgentDefinition>(`/agents/${identifier}`, { method: "PUT", body: JSON.stringify(body) }),
  /** 更新智能体可编辑字段（同 updateAgent 字段集） */

  updateSingle: (
    id: string,
    body: { name?: string; domain?: string; description?: string; capabilities?: string[]; tools?: string[]; modelHint?: string; link?: string },
  ) => req<AgentDefinition>(`/singles/${id}`, { method: "PUT", body: JSON.stringify(body) }),

  setGroupModel: (gid: string, model: string) =>
    req<{ ok: boolean }>(`/groups/${gid}/model`, {
      method: "PUT",
      body: JSON.stringify({ model }),
    }),

  setGroupCapabilities: (gid: string, body: GroupCapabilities) =>
    req<{ ok: boolean; capabilities?: GroupCapabilities | null }>(`/groups/${gid}/capabilities`, {
      method: "PUT",
      body: JSON.stringify(body),
    }),

  createSingle: (body: { name: string; identifier: string; description: string; domain?: string; prompt?: string; link?: string }) =>
    req<AgentDefinition>("/singles", { method: "POST", body: JSON.stringify(body) }),

  deleteSingle: (id: string) => req<{ ok: boolean }>(`/singles/${id}`, { method: "DELETE" }),

  groupOverview: (gid: string) => req<GroupOverview>(`/groups/${gid}/overview`),

  getAgentPrompt: (gid: string, identifier: string) =>
    req<{ identifier: string; promptFile: string; prompt: string }>(`/groups/${gid}/agents/${identifier}/prompt`),

  putAgentPrompt: (gid: string, identifier: string, prompt: string) =>
    req<{ ok: boolean }>(`/groups/${gid}/agents/${identifier}/prompt`, { method: "PUT", body: JSON.stringify({ prompt }) }),

  getSinglePrompt: (id: string) =>
    req<{ identifier: string; promptFile: string; prompt: string }>(`/singles/${id}/prompt`),

  putSinglePrompt: (id: string, prompt: string) =>
    req<{ ok: boolean }>(`/singles/${id}/prompt`, { method: "PUT", body: JSON.stringify({ prompt }) }),
  // 拉取端点可用模型清单：按档案 id（沿用已存密钥）或显式端点+明文密钥,

  setGroupWorkspace: (gid: string, workspace: string) =>
    req<{ ok: boolean }>(`/groups/${gid}/workspace`, { method: "PUT", body: JSON.stringify({ workspace }) }),
};
