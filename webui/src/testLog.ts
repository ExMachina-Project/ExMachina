/**
 * 连通性测试结果本地留存（model-settings-ux）：
 * 测试结果是时效性观测而非配置事实——留在 localStorage（刷新不丢），
 * 不进后端（避免服务端状态膨胀）。
 * 口径：每次测试覆盖该档案的旧记录；失败保留上次成功（spec：失败 MUST 保留上次成功记录并显示当前失败原因）。
 */

export interface TestRecord {
  at: string;
  ok: boolean;
  latencyMs?: number;
  reason?: string;
  lastSuccess?: { at: string; latencyMs?: number };
}

const KEY = "exm.testLog";

function readAll(): Record<string, TestRecord> {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(KEY) ?? "{}");
    return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, TestRecord> : {};
  } catch {
    return {};
  }
}

export function getTestRecord(id: string): TestRecord | undefined {
  return readAll()[id];
}

/** Retain the last successful observation alongside the latest failure. */
export function putTestRecord(id: string, r: TestRecord): void {
  const all = readAll();
  const previous = all[id];
  all[id] = { ...r, lastSuccess: r.ok ? { at: r.at, latencyMs: r.latencyMs }
    : previous?.ok ? { at: previous.at, latencyMs: previous.latencyMs } : previous?.lastSuccess };
  try {
    localStorage.setItem(KEY, JSON.stringify(all));
  } catch {
    // 配额满等异常：静默（留存是 SHOULD 级体验增益）
  }
}
