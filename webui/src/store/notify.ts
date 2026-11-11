// store 域文件：通知偏好与触发（自 store.ts 拆出）。
import { useExm } from "./sessions";
export interface NotifyPrefs {
  enabled: boolean;
  approval: boolean;
  roundDone: boolean;
  cronDone: boolean;
}

export function loadNotifyPrefs(): NotifyPrefs {
  const def: NotifyPrefs = { enabled: false, approval: true, roundDone: false, cronDone: true };
  try {
    const raw = localStorage.getItem("exm.notify");
    return raw ? { ...def, ...JSON.parse(raw) } : def;
  } catch {
    return def;
  }
}

export function saveNotifyPrefs(prefs: NotifyPrefs): void {
  try { localStorage.setItem("exm.notify", JSON.stringify(prefs)); } catch { /* 忽略 */ }
}

/** 触发系统通知（授权 + 偏好由调用方判断），点击聚焦并跳转会话 */
export function fireNotification(title: string, body: string, sessionId?: string): void {
  try {
    const n = new Notification(title, { body: body.slice(0, 200), tag: sessionId ?? title });
    n.onclick = () => {
      window.focus();
      if (sessionId) useExm.getState().requestNav("chat", sessionId);
    };
  } catch { /* 通知不可用时静默 */ }
}

/** 是否允许对某会话弹通知：已授权 + 页面隐藏 或 事件非当前会话 */
export function fireNotificationLocal(title: string, body: string, sessionId?: string): void {
  try {
    const n = new Notification(title, { body: body.slice(0, 200), tag: sessionId ?? title });
    n.onclick = () => {
      window.focus();
      if (sessionId) useExm.getState().requestNav("chat", sessionId);
    };
  } catch { /* 忽略 */ }
}

export function shouldNotifySession(sessionId?: string): boolean {
  if (document.visibilityState !== "hidden" && sessionId && sessionId === useExm.getState().sessionId) return false;
  return true;
}

/** 实时过程流追加：同类（思考/正文）并入末段，避免逐字成条 */
