//! api/identity.ts —— 通道身份域（7.1 拆分）：绑定清单、配对码签发/撤销、角色调整。纯搬移。

import { req } from "./core";

export interface IdentityItem {
  id: string;
  channel: string;
  externalId: string;
  displayName: string;
  role: string;
  pairedAt: string;
  note: string;
}

export interface CodeItem {
  code: string;
  note: string;
  platform?: string | null;
  createdAt: string;
  expiresAtMs: number;
  expired: boolean;
}
export interface IdentityList {
  identityRequired: boolean;
  identities: IdentityItem[];
  codes: CodeItem[];
}


/** 通道身份：清单（绑定 + 配对码） */
export function identityList(): Promise<IdentityList> {
  return req(`/identity/list`);
}

/** 通道身份：签发配对码（单次有效、限时） */
export function identityIssue(note: string, role?: "admin" | "member"): Promise<{ code: string; expiresAtMs: number }> {
  return req(`/identity/code`, {
    method: "POST",
    body: JSON.stringify({ note, role: role ?? "member" }),
  });
}

/** 通道身份：调整角色（admin | member） */
export function identitySetRole(id: string, role: string): Promise<{ ok: boolean }> {
  return req(`/identity/role`, { method: "POST", body: JSON.stringify({ id, role }) });
}

/** 通道身份：解绑 */
export function identityUnbind(id: string): Promise<{ ok: boolean }> {
  return req(`/identity/${encodeURIComponent(id)}`, { method: "DELETE" });
}

/** 通道身份：撤销配对码（过期清理 / 误签发撤销；幂等） */
export function identityRevokeCode(code: string): Promise<{ ok: boolean }> {
  return req(`/identity/code/${encodeURIComponent(code)}`, { method: "DELETE" });
}

/** 通道身份：一键清理全部过期配对码 */
export function identityPurgeCodes(): Promise<{ ok: boolean; purged: number }> {
  return req(`/identity/codes/purge`, { method: "POST", body: JSON.stringify({}) });
}

