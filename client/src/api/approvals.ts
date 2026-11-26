//! Approval-request listing and decision APIs.

import type { RequestJson } from "./core";
import type { ApprovalRequest } from "./core";
export function createApprovalsApi(req: RequestJson) { return {
  listApprovals: (status?: string, limit = 50) => {
    const p = new URLSearchParams();
    if (status) p.set("status", status);
    p.set("limit", String(limit));
    return req<ApprovalRequest[]>(`/approvals?${p.toString()}`);
  },

  decideApproval: (id: string, approve: boolean, scope: "once" | "session" | "allowlist" = "once") =>
    req<ApprovalRequest>(`/approvals/${id}/${approve ? "approve" : "deny"}`, {
      method: "POST",
      body: approve ? JSON.stringify({ scope }) : JSON.stringify({}),
    }),

  // 计划模式（spec plan-mode：跨端审批呈现——WebUI 计划卡片）,
}; }
