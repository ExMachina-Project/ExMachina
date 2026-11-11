//! api/plugins.ts —— 插件域：插件清单、启停、凭据、市场索引与安装更新（codebase-refactor 7.1 拆分）。纯搬移。

import { req } from "./core";
import type { PluginCredentials, PluginDetail, PluginIndexEntry, PluginSummary } from "./core";
export const apiPlugins = {
  plugins: () => req<{ plugins: PluginSummary[] }>("/plugins"),

  pluginDetail: (id: string) => req<PluginDetail>(`/plugins/${id}`),

  setPluginEnabled: (id: string, enabled: boolean) =>
    req<{ id: string; enabled: boolean }>(`/plugins/${id}`, {
      method: "PUT",
      body: JSON.stringify({ enabled }),
    }),

  deletePlugin: (id: string) =>
    req<{ id: string; removed: boolean }>(`/plugins/${id}`, { method: "DELETE" }),

  pluginCredentials: (id: string) =>
    req<PluginCredentials>(`/plugins/${id}/credentials`),

  setPluginCredentials: (id: string, credentials: Record<string, string>) =>
    req<{ id: string; saved: boolean }>(`/plugins/${id}/credentials`, {
      method: "PUT",
      body: JSON.stringify({ credentials }),
    }),

  pluginIndex: () => req<{ indexUrl: string; entries: PluginIndexEntry[] }>("/plugins/index"),

  installPlugin: (source: string, overwrite = false) =>
    req<{ id: string; version: string; updated: boolean }>("/plugins/install", {
      method: "POST",
      body: JSON.stringify({ source, overwrite }),
    }),

  updatePlugin: (id: string) =>
    req<{ id: string; updated: boolean; message: string }>(`/plugins/${id}/update`, {
      method: "POST",
    }),
};
