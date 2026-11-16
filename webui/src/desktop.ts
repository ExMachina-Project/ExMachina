/** Narrow bridge shared with the independently designed Tauri client. */
export interface DesktopConfig {
  exitOnClose: boolean;
  shortcut: string;
  notifyApproval: boolean;
  notifyCron: boolean;
  notifyTaskDone: boolean;
}
export interface DesktopGateway { url: string; key?: string; version?: string; remote?: boolean }
declare global {
  interface Window {
    __EXM_GATEWAY__?: DesktopGateway;
    __TAURI__?: {
      core: { invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> };
      event: { listen<T>(event: string, callback: (event: { payload: T }) => void): Promise<() => void> };
    };
  }
}
export function desktopInvoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!window.__TAURI__) return Promise.reject(new Error("此功能需要 Tauri 桌面客户端"));
  return window.__TAURI__.core.invoke<T>(command, args);
}
