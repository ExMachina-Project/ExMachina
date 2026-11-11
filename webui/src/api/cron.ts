//! api/cron.ts —— 定时任务域：cron/at/queue 任务 CRUD 与执行记录（codebase-refactor 7.1 拆分）。纯搬移。

import { req } from "./core";
import type { CronJob, CronRun } from "./core";
export const apiCron = {
  listCron: () => req<CronJob[]>("/cron"),

  createCron: (body: {
    name: string;
    prompt: string;
    cron?: string;
    at?: string;
    /** 任务类型（v0.4.0）：cron | at | queue；缺省由后端按 cron/at 推断 */
    kind?: string;
    /** at 类型：投递到的已有会话 id */
    targetSession?: string;
    /** queue 类型：按序执行的任务 id 列表 */
    steps?: string[];
    group?: string;
    sessionTitle?: string;
    notifyChannels?: string[];
  }) => req<CronJob>("/cron", { method: "POST", body: JSON.stringify(body) }),

  updateCron: (id: string, body: { enabled?: boolean }) =>
    req<CronJob>(`/cron/${id}`, { method: "PUT", body: JSON.stringify(body) }),

  deleteCron: (id: string) => req<{ ok: boolean }>(`/cron/${id}`, { method: "DELETE" }),

  runCron: (id: string) =>
    req<CronRun>(`/cron/${id}/run`, { method: "POST", body: JSON.stringify({}) }),

  cronRuns: (job?: string, limit = 20) => {
    const p = new URLSearchParams();
    if (job) p.set("job", job);
    p.set("limit", String(limit));
    return req<CronRun[]>(`/cron/runs?${p.toString()}`);
  },
};
