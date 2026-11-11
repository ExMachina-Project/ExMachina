//! api/limits.ts —— 用量治理域（7.1 拆分）：限流快照、消毒命中统计。纯搬移。

import { req } from "./core";

export interface LimitsStats {
  enabled: boolean;
  windowSecs: number;
  maxRequests: number;
  quotaPeriod: string;
  quotaTokens: number;
  quotaRequests: number;
  counters: { key: string; count: number }[];
}

/** 用量治理：限流计数快照 + 配置口径 */
export function limitsStats(): Promise<LimitsStats> {
  return req(`/limits/stats`);
}

/** 消毒命中统计（inbound-sanitization 设置页安全区）：模式 → 计数（进程级） */
export interface SanitizeStats {
  stats: { pattern: string; count: number }[];
}

export function sanitizeStats(): Promise<SanitizeStats> {
  return req(`/config/sanitize-stats`);
}

