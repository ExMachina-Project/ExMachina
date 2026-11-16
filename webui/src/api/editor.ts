import { req } from "./core";

export interface EditorEntry { name: string; path: string; dir: boolean; size: number }
export interface EditorFile { path: string; workspace?: string; content: string; size: number; truncated: boolean; binary: boolean }
export interface WorkspaceChange { path: string; status: string; staged: boolean; diff?: { text: string; truncated: boolean } | null }
export interface WorkspaceChanges { source: string; branch?: string; files: WorkspaceChange[] }
export interface GitOverview { repo: boolean; branch?: string; branches?: string[]; log?: string[] }
export interface GitOperation { op: "stage" | "unstage" | "commit" | "switch" | "discard" | "push" | "reset_hard"; path?: string; message?: string; branch?: string }
export const editorApi = {
  workspace: () => req<{ workspace: string }>("/workspace"),
  setWorkspace: (path: string) => req<{ workspace: string }>("/workspace", { method: "PUT", body: JSON.stringify({ path }) }),
  list: (path = ".") => req<{ entries: EditorEntry[] }>(`/fs/list?path=${encodeURIComponent(path)}`),
  read: (path: string) => req<EditorFile>(`/fs/file?path=${encodeURIComponent(path)}`),
  save: (path: string, content: string, originalContent: string, workspace?: string) => req<{ ok: boolean }>("/fs/file", { method: "PUT", body: JSON.stringify({ path, content, originalContent, workspace }) }),
  changes: () => req<WorkspaceChanges>("/workspace/changes"),
  git: () => req<GitOverview>("/git/overview"),
  operation: (body: GitOperation) => req<{ ok?: boolean; pending?: boolean; command?: string }>("/git/op", { method: "POST", body: JSON.stringify(body) }),
};
