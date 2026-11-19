import { tr } from "../i18n/core";
import { gatewayAssetUrl } from "../../../client/src/api/core";
export * from "../../../client/src/api/core";

/** WebUI 只管理托管当前页面的实例；独立应用使用 client/ 的隔离连接。
 *  不读取历史 exm.server，避免把管理凭证发送到另一个实例。 */
export function serverBase(): string {
  return "";
}

export function accessKey(): string {
  return localStorage.getItem("exm.key") ?? "";
}

export function authHeaders(): Record<string, string> {
  const key = accessKey();
  return key ? { "X-Auth-Key": key } : {};
}

export async function req<T>(path: string, init?: RequestInit): Promise<T> {
  const resp = await fetch(`${serverBase()}/api${path}`, {
    headers: { "Content-Type": "application/json", ...authHeaders() },
    ...init,
  });
  if (resp.status === 401) {
    // 密钥失效才清键回登录门；无 key 的 401 = 未登录（dev 源首访 / 角标预热早于
    // 登录门），不 reload——否则挂载期轮询（approvals/flows 角标）会触发
    // 401→清键→reload 死循环，页面永远卡在 CONNECTING（dev 预览实测踩中）
    if (localStorage.getItem("exm.key") && path !== "/auth/verify") {
      localStorage.removeItem("exm.key");
      window.location.reload();
    }
    throw new Error(tr("api.unauthorized"));
  }
  if (!resp.ok && resp.status !== 202) {
    const raw = await resp.text();
    // 全站错误契约：{"error": "中文说明"}（业务错误与提取器拒绝都已归一），
    // 直接取说明文案给用户看；只有反向代理之类吐非 JSON 时才回退原文。
    let msg = raw.slice(0, 200);
    try {
      const parsed = JSON.parse(raw) as { error?: string };
      if (parsed.error) msg = parsed.error;
    } catch {
      /* 非 JSON：保留原文 */
    }
    throw new Error(msg);
  }
  return (await resp.json()) as T;
}

/** 工作区资产 URL（computer 截图 / 工具输出预览）：<img> 无法带请求头，密钥走查询参数 */
export function assetUrl(relPath: string, sessionId?: string): string {
  return gatewayAssetUrl(serverBase(), accessKey(), relPath, sessionId);
}

/** 工作区文件下载 URL（/api/asset/download）：<a target="_blank"> 导航同样无法带
 *  请求头，密钥与 assetUrl 同款走查询参数（网关侧对 GET /api/asset* 开放该通道） */
export function downloadUrl(relPath: string, sessionId?: string): string {
  return gatewayAssetUrl(serverBase(), accessKey(), relPath, sessionId, true);
}
