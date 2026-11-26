//! Workbench file-tree types and API.

import { req } from "./core";
import type { WorkbenchTree } from "../../../client/src/api/workbench";
export type { WorkbenchNode, WorkbenchTree } from "../../../client/src/api/workbench";

/** 工作台文件树（.gitignore 口径过滤；工作区边界校验在服务端）：对话内 @ 文件提及的数据源 */
export function workbenchTree(path?: string, depth?: number): Promise<WorkbenchTree> {
  const qs = new URLSearchParams();
  if (path) qs.set("path", path);
  if (depth) qs.set("depth", String(depth));
  const q = qs.toString();
  return req(`/workbench/tree${q ? `?${q}` : ""}`);
}
