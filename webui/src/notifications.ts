import type { ExmState, WsEvent } from "../../client/src/store";

export interface NotifyPrefs { enabled: boolean; approval: boolean; roundDone: boolean; cronDone: boolean }

export function loadNotifyPrefs(): NotifyPrefs {
  const defaults: NotifyPrefs = { enabled: false, approval: true, roundDone: false, cronDone: true };
  try { return { ...defaults, ...JSON.parse(localStorage.getItem("exm.notify") ?? "{}") }; }
  catch { return defaults; }
}

export function saveNotifyPrefs(prefs: NotifyPrefs): void {
  try { localStorage.setItem("exm.notify", JSON.stringify(prefs)); } catch { /* storage may be unavailable */ }
}

export function onConsoleEvent(event: WsEvent, state: ExmState): void {
  const { type, sessionId } = event;
  if (type.startsWith("approval.")) window.dispatchEvent(new Event("exm:approvals-refresh"));
  if (type === "run.finished" || type === "cron.finished") window.dispatchEvent(new Event("exm:flows-refresh"));
  if (type === "run.finished") window.dispatchEvent(new CustomEvent("exm:bound-refresh", { detail: sessionId }));
  const prefs = loadNotifyPrefs();
  if (!prefs.enabled || typeof Notification === "undefined" || Notification.permission !== "granted") return;
  const payload = event.payload as Record<string, unknown>;
  let notice: [title: string, text: string] | undefined;
  if (type === "approval.required" && prefs.approval && document.hidden) notice = ["审批请求待处理", String(payload.command ?? payload.agentId ?? "")];
  if (type === "run.finished" && prefs.roundDone && (document.hidden || sessionId !== state.sessionId)) {
    const first = (payload.statements as { text?: string }[] | undefined)?.[0];
    notice = ["轮次完成", first?.text ?? "已完成"];
  }
  if (type === "cron.finished" && prefs.cronDone) notice = [`定时任务${payload.status === "done" ? "完成" : "失败"}：${payload.jobName ?? ""}`, String(payload.summary ?? payload.status ?? "")];
  if (!notice) return;
  try {
    const [title, body] = notice;
    const notification = new Notification(title, { body: body.slice(0, 200), tag: sessionId || title });
    notification.onclick = () => { window.focus(); if (sessionId) state.requestNav("chat", sessionId); };
  } catch { /* platform does not support notifications */ }
}
