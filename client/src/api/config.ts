//! api/config.ts —— 配置域：网关配置读写与 schema（codebase-refactor 7.1 拆分）。纯搬移。

import type { RequestJson } from "./core";
import type { ConfigSchema, GatewayConfig } from "./core";
export function createConfigApi(req: RequestJson) { return {
  getConfig: () => req<GatewayConfig>("/config"),

  putConfig: (body: Record<string, unknown>) =>
    req<{ ok: boolean; mock: boolean }>("/config", { method: "PUT", body: JSON.stringify(body) }),

  configSchema: () => req<ConfigSchema>("/config/schema"),
}; }
