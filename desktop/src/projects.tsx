import { createContext, useContext } from "react";
import { createClient, type ExmClient } from "../../client/src/api/client";
import { createExmStore, type ExmStore } from "../../client/src/store";
import { desktopInvoke } from "./native";
import { translateFrom } from "../../client/src/translate";
import { chatZh } from "../../client/src/chatZh";

export interface Connection {
  id: string;
  name: string;
  kind: "local" | "remote";
  url: string;
}
export interface Project { id: string; name: string; connectionId: string; workspace: string }
export interface ProjectCatalog { connections: Connection[]; projects: Project[]; activeId?: string }
export interface ProjectRuntime { project: Project; client: ExmClient; store: ExmStore; local: boolean; consoleUrl: string; mobileUrl?: string }
const STORAGE_KEY = "exm.desktop.projects.v1";
export const desktopTranslate = translateFrom(chatZh);
export const newProjectId = () => Array.from(crypto.getRandomValues(new Uint8Array(16)), (b) => b.toString(16).padStart(2, "0")).join("");

export function normalizeServer(raw: string): string {
  const text = raw.trim();
  if (!text) throw new Error("请输入实例地址");
  const url = new URL(/^[a-z]+:\/\//i.test(text) ? text : `http://${text}`);
  if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.search || url.hash) {
    throw new Error("地址只允许 HTTP(S)，请将密钥填写在独立输入框中");
  }
  return url.toString().replace(/\/+$/, "");
}

export function loadCatalog(): ProjectCatalog {
  try {
    const value = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "null") as ProjectCatalog | null;
    if (!value || !Array.isArray(value.connections) || !Array.isArray(value.projects)) return { connections: [], projects: [] };
    const connections = value.connections.filter((c) => typeof c.id === "string" && typeof c.name === "string" && ["local", "remote"].includes(c.kind) && typeof c.url === "string");
    const projects = value.projects.filter((p) => typeof p.id === "string" && typeof p.name === "string" && typeof p.workspace === "string" && connections.some((c) => c.id === p.connectionId));
    return { connections, projects, activeId: value.activeId };
  } catch { return { connections: [], projects: [] }; }
}

export function saveCatalog(value: ProjectCatalog): void {
  // Credentials and ephemeral localhost ports never enter the public catalog.
  localStorage.setItem(STORAGE_KEY, JSON.stringify({ ...value, connections: value.connections.map((c) => ({ ...c, url: c.kind === "local" ? "" : c.url })) }));
}

export function connectionKey(id: string): string {
  return sessionStorage.getItem(`exm.desktop.key.${id}`) ?? "";
}
export function setConnectionKey(id: string, key: string): void {
  if (key) sessionStorage.setItem(`exm.desktop.key.${id}`, key);
  else sessionStorage.removeItem(`exm.desktop.key.${id}`);
}

export async function resolveConnection(connection: Connection): Promise<{ url: string; key: string; mobileUrl?: string }> {
  if (connection.kind === "local") {
    if (!window.__TAURI__) throw new Error("本地实例需要原生桌面客户端");
    const gateway = await desktopInvoke<{ url: string; key: string; lanUrl?: string }>("get_local_gateway");
    return { url: gateway.url, key: gateway.key, mobileUrl: gateway.lanUrl };
  }
  return { url: normalizeServer(connection.url), key: connectionKey(connection.id), mobileUrl: normalizeServer(connection.url) };
}

export async function connectProject(connection: Connection, path?: string) {
  const { url, key, mobileUrl } = await resolveConnection(connection);
  if (!key) throw new Error("请输入实例的访问密钥");
  const config = { url, accessKey: connection.kind === "local" ? () => key : () => connectionKey(connection.id) };
  const initial = createClient(config, desktopTranslate);
  const health = await initial.req<{ ok: boolean; features?: string[] }>("/health", { signal: AbortSignal.timeout(10_000) });
  if (!health.ok || !health.features?.includes("session-workspace-v1")) {
    throw new Error("此实例尚不支持项目会话隔离，请先升级网关；不会回退为修改全局工作区。");
  }
  const candidate = path ?? (await initial.editorApi.workspace()).workspace;
  const scoped = createClient({ ...config, workspace: candidate }, desktopTranslate);
  const { workspace, scoped: confirmed } = await scoped.req<{ workspace: string; scoped?: boolean }>("/workspace", { signal: AbortSignal.timeout(10_000) });
  if (!confirmed || !workspace) throw new Error("未能验证项目目录；请确认代理未移除项目请求头");
  return { client: createClient({ ...config, workspace }, desktopTranslate), workspace, mobileUrl };
}

export function makeRuntime(project: Project, client: ExmClient, local: boolean, consoleUrl: string, mobileUrl?: string): ProjectRuntime {
  return { project, client, local, consoleUrl: local ? client.serverBase() : consoleUrl, mobileUrl, store: createExmStore({ ...client, tr: desktopTranslate, project }) };
}

export const ProjectContext = createContext<ProjectRuntime | null>(null);
export function useProjectClient(): ProjectRuntime {
  const value = useContext(ProjectContext);
  if (!value) throw new Error("项目上下文尚未就绪");
  return value;
}
