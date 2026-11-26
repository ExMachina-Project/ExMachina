/** Isolated, immutable transport for independently connected client workspaces. */
import { createGroupsApi } from "./groups";
import { createConfigApi } from "./config";
import { createApprovalsApi } from "./approvals";
import { createModelsApi } from "./models";
import { createEditorApi } from "./editor";
import { createSessionsApi } from "./sessions";
import { createCollaborationsApi } from "./collaborations";
import { gatewayAssetUrl, type RequestJson, type ApprovalRequest } from "./core";
import type { Session, SessionSearchHit } from "../types";
import type { WorkbenchTree } from "./workbench";
import type { Translate } from "../translate";

export interface ClientConnection { url: string; accessKey: () => string; workspace?: string }

export function createClient(connection: ClientConnection, tr: Translate) {
  const base = connection.url.replace(/\/+$/, "");
  const serverBase = () => base;
  const { accessKey, workspace } = connection;
  const authHeaders = (): Record<string, string> => {
    const key = accessKey();
    return key ? { "X-Auth-Key": key } : {};
  };
  const req: RequestJson = async <T,>(path: string, init?: RequestInit): Promise<T> => {
    const headers = new Headers(init?.headers);
    headers.set("Content-Type", "application/json");
    Object.entries(authHeaders()).forEach(([key, value]) => headers.set(key, value));
    if (workspace) {
      headers.set("X-Exm-Workspace", encodeURIComponent(workspace));
    }
    const response = await fetch(`${base}/api${path}`, { ...init, headers, credentials: "omit", redirect: "error" });
    if (!response.ok) {
      const value = await response.json().catch(() => null) as { error?: string } | null;
      throw new Error(value?.error ?? (response.status === 401 ? "连接密钥无效或已失效" : `HTTP ${response.status}`));
    }
    return response.json() as Promise<T>;
  };
  const sessions = createSessionsApi({ req, serverBase, authHeaders, workspace }, tr);
  const approvals = createApprovalsApi(req);
  const query = (extra: Record<string, string> = {}) => new URLSearchParams({ ...(workspace ? { workspace } : {}), ...extra }).toString();
  const clientApi = {
    ...sessions, ...createGroupsApi(req), ...createConfigApi(req),
    ...approvals, ...createModelsApi(req),
    listSessions: () => req<Session[]>(`/sessions?${query({ all: "true" })}`),
    listAllSessions: () => req<Session[]>(`/sessions?${query({ all: "true" })}`),
    listArchivedSessions: () => req<Session[]>(`/sessions?${query({ archived: "true" })}`),
    searchSessions: (q: string, limit = 20) => req<{ query: string; hits: SessionSearchHit[] }>(`/sessions/search?${query({ q, limit: String(limit) })}`),
    listApprovals: (status?: string, limit = 50) => req<ApprovalRequest[]>(`/approvals?${query({ limit: String(limit), ...(status ? { status } : {}) })}`),
    createSession: (title: string, options?: { target?: string }) => req<Session>(`/sessions${workspace ? `?${query()}` : ""}`, {
      method: "POST", body: JSON.stringify({ title, ...options, ...(workspace ? { workspace } : {}) }),
    }),
  };
  return {
    api: clientApi, req, serverBase, accessKey, authHeaders,
    editorApi: createEditorApi(req),
    collaborations: createCollaborationsApi(req),
    assetUrl: (path: string, sessionId?: string) => gatewayAssetUrl(base, accessKey(), path, sessionId),
    downloadUrl: (path: string, sessionId?: string) => gatewayAssetUrl(base, accessKey(), path, sessionId, true),
    workbenchTree: (path = "", depth = 6) => req<WorkbenchTree>(`/workbench/tree?${new URLSearchParams({ path, depth: String(depth) })}`),
  };
}

export type ExmClient = ReturnType<typeof createClient>;
