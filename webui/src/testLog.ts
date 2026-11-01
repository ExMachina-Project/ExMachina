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
}

const KEY = "exm.testLog";

function readAll(): Record<string, TestRecord> {
  try {
    return JSON.parse(localStorage.getItem(KEY) ?? "{}") as Record<string, TestRecord>;
  } catch {
    return {};
  }
}

export function getTestRecord(id: string): TestRecord | undefined {
  return readAll()[id];
}

/** 记录一次测试结果（失败不带历史覆盖语义：只记当次；展示层负责合并上次成功） */
export function putTestRecord(id: string, r: TestRecord): void {
  const all = readAll();
  all[id] = r;
  try {
    localStorage.setItem(KEY, JSON.stringify(all));
  } catch {
    // 配额满等异常：静默（留存是 SHOULD 级体验增益）
  }
}
