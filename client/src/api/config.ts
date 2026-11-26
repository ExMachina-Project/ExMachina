//! Gateway configuration and schema APIs.

import type { RequestJson } from "./core";
import type { ConfigSchema, GatewayConfig } from "./core";
export function createConfigApi(req: RequestJson) { return {
  getConfig: () => req<GatewayConfig>("/config"),

  putConfig: (body: Record<string, unknown>) =>
    req<{ ok: boolean; mock: boolean }>("/config", { method: "PUT", body: JSON.stringify(body) }),

  configSchema: () => req<ConfigSchema>("/config/schema"),
}; }
