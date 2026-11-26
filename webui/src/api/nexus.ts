//! Nexus agent-link graph and probe APIs.

import { authHeaders, req, serverBase } from "./core";
import type { NexusLink, NexusProbeResult } from "./core";

export type ConnectionKind = "software" | "external_agent" | "device";
export type ConnectionSourceKind = "nexus" | "mcp" | "plugin" | "device" | "software";

/** Display identity only. Neither a source reference nor a registration grants execution. */
export interface ConnectionSourceRef {
  kind: ConnectionSourceKind;
  sourceId: string;
  scope: { instanceId: string; authorityId: string; accountId: string; projectId?: string | null };
  subresourceId?: string | null;
  lifecycle: { state: "known"; generation: string } | { state: "legacy_unresolved" };
  revision: string;
}

export interface ConnectionReference {
  instanceId: string;
  connectionId: string;
  generation: string;
  kind: ConnectionKind;
}

export interface ConnectionDirectoryCapability {
  name: string;
  role: string;
  origin: string;
  effects: string[];
  verification: string;
  authorization: string;
}

export interface ConnectionDirectoryRelation {
  relation?: { id: string; kind: string; revision: number; from?: ConnectionReference; to?: ConnectionReference };
  id?: string;
  kind?: string;
  revision?: number;
  from?: { reference?: ConnectionReference; displayName?: string };
  to?: { reference?: ConnectionReference; displayName?: string };
}

export interface ConnectionDirectoryRecord {
  kind: ConnectionKind;
  displayName: string;
  sourceRef: ConnectionSourceRef;
  sourceEditTarget: { surface: string; sourceId: string };
  reference?: ConnectionReference;
  registrationState?: string;
  readiness: {
    enabled: boolean;
    availability: string;
    handshake: string;
    freshness: string;
    reasons: string[];
  };
  capabilities: ConnectionDirectoryCapability[];
  capabilitiesTotal: number;
  capabilitiesTruncated: boolean;
  relations?: ConnectionDirectoryRelation[];
  relationsTotal?: number;
  relationsTruncated?: boolean;
  device?: { kind: string; status: string; pairing: string; backend: string };
}

export interface ConnectionDirectoryPage {
  records: ConnectionDirectoryRecord[];
  issues: Array<{ source: string; reason: string }>;
  total: number;
  limit: number;
  offset: number;
  truncated: boolean;
  nextOffset: number | null;
  nextCursor: string | null;
  managementOnly: true;
  executionAuthorization: "unknown";
}

export interface ConnectionDirectoryQuery {
  search?: string;
  kind?: ConnectionKind;
  sourceKind?: ConnectionSourceKind;
  limit?: number;
  /** Server paging position. A non-zero offset MUST be paired with the server-issued cursor. */
  offset?: number;
  /** Opaque, query-bound cursor from the preceding page. Omit on the first page. */
  cursor?: string;
}

export class ConnectionDirectoryError extends Error {
  constructor(public readonly status: number, public readonly code: "unsupported" | "forbidden" | "cursor_expired" | "unavailable" | "invalid_response") {
    super(code);
    this.name = "ConnectionDirectoryError";
  }
}

const CONNECTION_KINDS = new Set<ConnectionKind>(["software", "external_agent", "device"]);
const SOURCE_KINDS = new Set<ConnectionSourceKind>(["nexus", "mcp", "plugin", "device", "software"]);

function isObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isNonEmptyString(value: unknown): value is string {
  return typeof value === "string" && value.trim().length > 0;
}

function isConnectionReference(value: unknown): value is ConnectionReference {
  return isObject(value)
    && isNonEmptyString(value.instanceId)
    && isNonEmptyString(value.connectionId)
    && isNonEmptyString(value.generation)
    && typeof value.kind === "string"
    && CONNECTION_KINDS.has(value.kind as ConnectionKind);
}

function isDirectoryRecord(value: unknown): value is ConnectionDirectoryRecord {
  if (!isObject(value) || !CONNECTION_KINDS.has(value.kind as ConnectionKind)
    || !isNonEmptyString(value.displayName) || !isObject(value.sourceRef)
    || !isObject(value.sourceRef.scope) || !isObject(value.sourceRef.lifecycle)
    || !isObject(value.sourceEditTarget) || !isObject(value.readiness)
    || !Array.isArray(value.capabilities)) return false;

  const source = value.sourceRef;
  const scope = source.scope as Record<string, unknown>;
  const lifecycle = source.lifecycle as Record<string, unknown>;
  const readiness = value.readiness as Record<string, unknown>;
  if (typeof source.kind !== "string" || !SOURCE_KINDS.has(source.kind as ConnectionSourceKind)
    || !isNonEmptyString(source.sourceId)
    || !isNonEmptyString(scope.instanceId) || !isNonEmptyString(scope.authorityId)
    || !isNonEmptyString(scope.accountId)
    || !(scope.projectId === undefined || scope.projectId === null || typeof scope.projectId === "string")
    || !(source.subresourceId === undefined || source.subresourceId === null || typeof source.subresourceId === "string")
    || !isNonEmptyString(source.revision)
    || !(lifecycle.state === "legacy_unresolved"
      || (lifecycle.state === "known" && isNonEmptyString(lifecycle.generation)))
    || !isNonEmptyString(value.sourceEditTarget.surface)
    || !isNonEmptyString(value.sourceEditTarget.sourceId)
    || typeof readiness.enabled !== "boolean"
    || !isNonEmptyString(readiness.availability)
    || !isNonEmptyString(readiness.handshake)
    || !isNonEmptyString(readiness.freshness)
    || !Array.isArray(readiness.reasons)
    || !readiness.reasons.every(isNonEmptyString)
    || !Number.isSafeInteger(value.capabilitiesTotal) || (value.capabilitiesTotal as number) < value.capabilities.length
    || typeof value.capabilitiesTruncated !== "boolean"
    || value.capabilitiesTruncated !== ((value.capabilitiesTotal as number) > value.capabilities.length)
    || value.capabilities.some((capability) => !isObject(capability)
      || !isNonEmptyString(capability.name) || !isNonEmptyString(capability.role)
      || !isNonEmptyString(capability.origin) || !Array.isArray(capability.effects)
      || !capability.effects.every(isNonEmptyString)
      || !isNonEmptyString(capability.verification) || !isNonEmptyString(capability.authorization))) {
    return false;
  }
  if (value.reference !== undefined && !isConnectionReference(value.reference)) return false;
  if (value.registrationState !== undefined && !isNonEmptyString(value.registrationState)) return false;
  if (value.relations !== undefined && (!Array.isArray(value.relations)
    || value.relations.some((relation) => !isObject(relation)
      || !isNonEmptyString(relation.id) || !isNonEmptyString(relation.kind)
      || !Number.isSafeInteger(relation.revision)
      || !isObject(relation.from) || !isConnectionReference(relation.from.reference)
      || !isNonEmptyString(relation.from.displayName)
      || !isObject(relation.to) || !isConnectionReference(relation.to.reference)
      || !isNonEmptyString(relation.to.displayName)))) return false;
  if (value.relationsTotal !== undefined
    && (!Number.isSafeInteger(value.relationsTotal) || (value.relationsTotal as number) < (value.relations?.length ?? 0))) return false;
  if (value.relationsTruncated !== undefined && typeof value.relationsTruncated !== "boolean") return false;
  return true;
}

/** Only the narrow management snapshot endpoint is used; never fall back to source APIs. */
async function connectionDirectory(query: ConnectionDirectoryQuery = {}, signal?: AbortSignal): Promise<ConnectionDirectoryPage> {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query)) {
    if (value !== undefined && value !== "") params.set(key, String(value));
  }
  // Read the response here rather than through req so 404/405/501 remain distinguishable.
  // Source error bodies are deliberately not echoed into a management directory.
  const response = await fetch(`${serverBase()}/api/connections${params.size ? `?${params}` : ""}`, {
    method: "GET",
    headers: { "Content-Type": "application/json", ...authHeaders() },
    signal,
  });
  if (!response.ok) {
    const code = [404, 405, 501].includes(response.status) ? "unsupported"
      : [401, 403].includes(response.status) ? "forbidden"
        : response.status === 409 ? "cursor_expired" : "unavailable";
    throw new ConnectionDirectoryError(response.status, code);
  }
  let page: ConnectionDirectoryPage;
  try {
    page = await response.json() as ConnectionDirectoryPage;
  } catch {
    throw new ConnectionDirectoryError(response.status, "invalid_response");
  }
  if (!page || !Array.isArray(page.records) || !page.records.every(isDirectoryRecord)
    || !Array.isArray(page.issues) || !page.issues.every((issue) => isObject(issue)
      && isNonEmptyString(issue.source) && isNonEmptyString(issue.reason))
    || page.managementOnly !== true || page.executionAuthorization !== "unknown"
    || !Number.isSafeInteger(page.total) || page.total < 0
    || !Number.isSafeInteger(page.limit) || page.limit < 1 || page.limit > 200
    || !Number.isSafeInteger(page.offset) || page.offset < 0
    || page.records.length > page.limit || page.records.length > page.total
    || typeof page.truncated !== "boolean"
    || !(page.nextOffset === null || (Number.isSafeInteger(page.nextOffset) && page.nextOffset >= 0))
    || !(page.nextCursor === null || (typeof page.nextCursor === "string" && page.nextCursor.length > 0))
    || page.offset !== (query.offset ?? 0)
    || (page.truncated && (page.nextOffset !== page.offset + page.records.length
      || page.nextOffset >= page.total || page.nextCursor === null))
    || (!page.truncated && (page.nextOffset !== null || page.nextCursor !== null))) {
    throw new ConnectionDirectoryError(response.status, "invalid_response");
  }
  return page;
}

export const apiNexus = {
  nexusLinks: () => req<{ links: NexusLink[] }>("/nexus/links"),
  connectionDirectory,
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
