//! api/auth.ts —— 系统域：鉴权、健康、自检、审计、版本与更新（codebase-refactor 7.1 拆分）。纯搬移。

import { req } from "./core";
export const apiAuth = {
  verifyAuth: (key: string) =>
    req<{ ok: boolean; required?: boolean; error?: string }>("/auth/verify", {
      method: "POST",
      body: JSON.stringify({ key }),
    }),
  /** 首启引导状态：是否已配置访问密钥 + 提醒是否已被跳过 / 关闭（免鉴权） */

  authStatus: () =>
    req<{ ok: boolean; keyConfigured: boolean; setupSnoozed: boolean; setupDismissed: boolean }>("/auth/status"),
  /** 「下次再说」：本次进程内不再提示（网关重启后恢复提醒） */

  snoozeKeySetup: () => req<{ ok: boolean }>("/auth/snooze", { method: "POST" }),
  /** 「不再提醒」：持久化关闭首启设置引导 */

  dismissKeySetup: () => req<{ ok: boolean }>("/auth/dismiss", { method: "POST" }),

  health: () => req<{ ok: boolean; mock: boolean; agents: number }>("/health"),
  /** 开箱自检（onboarding-selfcheck）：四类体检分级结果（引导卡数据源） */

  selfcheck: () =>
    req<{
      ok: boolean;
      overall: "ok" | "warn" | "error";
      errors: number;
      warns: number;
      items: { level: "ok" | "warn" | "error"; name: string; detail: string; hint?: string }[];
    }>("/selfcheck"),

  audit: (params: { agent?: string; tool?: string; kind?: string; limit?: number }) =>
    req<{ items: Array<Record<string, unknown>> }>("/audit?" + new URLSearchParams(Object.entries(params).filter(([, v]) => v !== undefined && v !== "").map(([k, v]) => [k, String(v)])).toString()),

  versionInfo: () => req<{ version: string; gitHash: string }>("/version"),
  /** 一键更新(image 模式:compose pull + 辅助容器 up -d;git 模式:pull → build → up -d) */

  applyUpdate: () => req<{ started: boolean }>("/version/apply", { method: "POST" }),
  /** 一键更新状态(phase: pulling/building/recreating/done/failed/idle) */

  updateStatus: () =>
    req<{ running: boolean; phase: string; exitCode: number | null; log: string; deployMode: string }>(
      "/version/apply/status",
    ),
  /** 更新检查(双模式:git=源码部署比对上游分支,image=镜像部署比对注册表发布源 SHA) */

  versionCheck: () =>
    req<{
      version: string;
      gitHash: string;
      deployMode?: "git" | "image" | "none";
      branch?: string;
      upstream?: { ref: string; sha: string; short: string; url: string; date: string };
      updateAvailable?: boolean | null;
      note?: string;
    }>("/version/check"),
};
