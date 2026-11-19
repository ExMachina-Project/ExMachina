import type { RequestJson } from "./core";
import { hydrateToolStatuses } from "./sessions";
import type { ChatMessage } from "../types";

export const COLLABORATION_FEATURE = "collaboration-v1";

export interface CollaborationMemberInput { target: string; workspace: string }
export interface CollaborationMember extends CollaborationMemberInput { sessionId: string; label: string }
export interface CollaborationRoom {
  id: string;
  title: string;
  workspace: string;
  members: CollaborationMember[];
  updatedAt: string;
}
export interface CollaborationRecipient { sessionId: string; instruction: string }
export interface CollaborationTurnInput { requestId: string; text: string; recipients: CollaborationRecipient[] }
export type CollaborationDeliveryStatus = "running" | "done" | "failed" | "stopped" | "interrupted";
export interface CollaborationDelivery extends CollaborationRecipient {
  status: CollaborationDeliveryStatus;
  messages?: ChatMessage[];
  error?: string;
}
export interface CollaborationTurn { id: string; text: string; createdAt: string; deliveries: CollaborationDelivery[] }
export interface CollaborationApproval { memberSessionId: string; id: string; command: string; agentId: string }
export interface CollaborationPage {
  room: CollaborationRoom;
  turns: CollaborationTurn[];
  nextBefore: string | null;
  activeTurnId: string | null;
  approvals: CollaborationApproval[];
}

export function createCollaborationsApi(req: RequestJson) {
  const roomPath = (id: string) => `/collaborations/${encodeURIComponent(id)}`;
  return {
    list: (signal?: AbortSignal) => req<CollaborationRoom[]>("/collaborations", { signal, cache: "no-store" }),
    create: (title: string, members: CollaborationMemberInput[]) => req<CollaborationRoom>("/collaborations", {
      method: "POST", body: JSON.stringify({ title, members }),
    }),
    get: async (id: string, before?: string, signal?: AbortSignal) => {
      const page = await req<CollaborationPage>(`${roomPath(id)}${before ? `?${new URLSearchParams({ before })}` : ""}`, { signal, cache: "no-store" });
      for (const turn of page.turns) for (const delivery of turn.deliveries) {
        if (delivery.messages) delivery.messages = delivery.messages.map(hydrateToolStatuses);
      }
      return page;
    },
    send: (id: string, body: CollaborationTurnInput) => req<{ id: string }>(`${roomPath(id)}/turns`, {
      method: "POST", body: JSON.stringify(body),
    }),
    stop: (id: string, turnId: string, sessionId?: string) => req<{ stopped: number }>(`${roomPath(id)}/turns/${encodeURIComponent(turnId)}/stop`, {
      method: "POST", body: JSON.stringify({ sessionId }),
    }),
  };
}
