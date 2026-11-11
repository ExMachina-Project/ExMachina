//! api/nexus.ts —— 连结网络域：连结体关系图谱与探测（codebase-refactor 7.1 拆分）。纯搬移。

import { req } from "./core";
import type { NexusLink, NexusProbeResult } from "./core";
export const apiNexus = {
  nexusLinks: () => req<{ links: NexusLink[] }>("/nexus/links"),
  /** 拓扑聚合视图（nexus-topology-admin）：节点（连结体）+ 边（个体绑定） */

  nexusGraph: () =>
    req<{
      nodes: { id: string; name: string; kind: string; enabled: boolean }[];
      edges: { agentId: string; agentName: string; linkId: string }[];
    }>("/nexus/graph"),
  /** 连边编辑：bind / unbind（重复边与状态冲突 409，连结体不存在 404） */

  editNexusEdge: (action: "bind" | "unbind", agentId: string, linkId: string) =>
    req<{ ok: boolean; agentId: string; linkId: string | null }>("/nexus/edges", {
      method: "PUT",
      body: JSON.stringify({ action, agentId, linkId }),
    }),

  saveNexusLink: (body: NexusLink) =>
    req<{ ok: boolean; link: NexusLink }>("/nexus/links", { method: "POST", body: JSON.stringify(body) }),

  deleteNexusLink: (id: string) => req<{ ok: boolean }>(`/nexus/links/${id}`, { method: "DELETE" }),
  /** 连通性测试：地址可达 / 鉴权通过 / 命令存在（秒级，不做任务级执行） */

  probeNexusLink: (id: string) =>
    req<NexusProbeResult>(`/nexus/links/${id}/probe`, { method: "POST" }),
};
