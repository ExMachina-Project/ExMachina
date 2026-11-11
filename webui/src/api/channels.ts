//! api/channels.ts —— 通道域：通道配置、状态、流、B 站/QQ 官方配对（codebase-refactor 7.1 拆分）。纯搬移。

import { req } from "./core";
import type { Channel, ChannelFlow, ChannelStatus } from "./core";
export const apiChannels = {
  channelTest: (id: string) =>
    req<{ ok: boolean; category?: string; identity?: string; message?: string; error?: string }>(`/channels/${id}/test`, { method: "POST", body: JSON.stringify({}) }),

  listChannels: () => req<Channel[]>("/channels"),

  channelStatus: () => req<Record<string, ChannelStatus>>("/channels/status"),
  /** 聊天流列表（按最后活跃倒序；?channel= 过滤单通道） */

  listChannelFlows: () => req<ChannelFlow[]>("/channels/flows"),

  readChannelFlow: (id: string) => req<{ ok: boolean }>(`/channels/flows/${encodeURIComponent(id)}/read`, { method: "POST" }),
  /** B 站扫码登录：出码（返回二维码内容 URL）→ 前端渲染二维码 → 轮询到 confirmed 拿登录 Cookie */

  biliQrStart: () => req<{ qrcodeKey: string; url: string }>("/channels/bili-qr", { method: "POST" }),

  biliQrPoll: (key: string) =>
    req<
      | { state: "waiting"; code: number }
      | { state: "confirmed"; sessdata: string; biliJct: string; uid: string; uname: string }
    >(`/channels/bili-qr/${encodeURIComponent(key)}`),
  /** QQ 官方机器人扫码绑定：出绑定任务（二维码 URL + 解密密钥）→ 轮询到 confirmed 拿 AppID/AppSecret */

  qqbotBindStart: () =>
    req<{ taskId: string; key: string; url: string }>("/channels/qqbot-bind", { method: "POST", body: "{}" }),

  qqbotBindPoll: (taskId: string, key: string) =>
    req<
      | { state: "waiting" | "scanned" | "expired" }
      | { state: "confirmed"; appId: string; appSecret: string; userOpenid?: string }
    >(`/channels/qqbot-bind/${encodeURIComponent(taskId)}?key=${encodeURIComponent(key)}`),

  createChannel: (body: {
    id: string;
    platform?: string;
    group?: string;
    account?: string;
    secret?: string;
    replyWebhook?: string;
    token?: string;
    enabled?: boolean;
    allowedChats?: string[];
    admins?: string[];
    groupGate?: boolean;
    proactiveReply?: boolean;
    proactiveRate?: number;
    capability?: "chat" | "tools" | "computer";
    memoryIsolation?: boolean;
    config?: Record<string, string>;
  }) => req<Channel>("/channels", { method: "POST", body: JSON.stringify(body) }),

  updateChannel: (id: string, body: { enabled?: boolean; group?: string; token?: string; secret?: string; replyWebhook?: string; account?: string; allowedChats?: string[]; admins?: string[]; groupGate?: boolean; proactiveReply?: boolean; proactiveRate?: number; capability?: "chat" | "tools" | "computer"; memoryIsolation?: boolean; config?: Record<string, string> }) =>
    req<Channel>(`/channels/${id}`, { method: "PUT", body: JSON.stringify(body) }),

  deleteChannel: (id: string) => req<{ ok: boolean }>(`/channels/${id}`, { method: "DELETE" }),
};
