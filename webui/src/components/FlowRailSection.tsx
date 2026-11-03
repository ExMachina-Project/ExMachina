/** 聊天流面板（chat flow rail section）：外部 IM 的对话单元列表。
 *
 *  模型：一个群 / 一个私聊各是一条流；无细分对话的平台（视频等）整通道一条流。
 *  一条流绑定一个活跃会话——不主动切换固定复用同一会话（跨轮上下文与记忆延续）。
 *  这里是控制台视角：按通道分组展示各流（连接状态 + 最后消息预览 + 最近活跃），
 *  点流即跳到它绑定的会话；无绑定则提示「下条消息自动开新会话」。
 */
import React, { useCallback, useEffect, useState } from "react";
import { api, type Channel, type ChannelFlow, type ChannelStatus, type FlowSessionInfo } from "../api";
import { relativeTime } from "../time";
import { useExm } from "../store";

/** 平台短标（通道 type → 展示名），与通道页 platformTag 同口径 */
const PLATFORM_LABEL: Record<string, string> = {
  napcat: "QQ",
  qqbot: "QQ 机器人",
  telegram: "Telegram",
  discord: "Discord",
  slack: "Slack",
  matrix: "Matrix",
  bilibili: "B站",
  youtube: "YouTube",
  webhook: "Webhook",
};

export function FlowRailSection({ t }: { t: (k: never) => string }): React.ReactElement {
  const [flows, setFlows] = useState<ChannelFlow[]>([]);
  const [channels, setChannels] = useState<Channel[]>([]);
  const [status, setStatus] = useState<Record<string, ChannelStatus>>({});
  const [loading, setLoading] = useState(true);
  const selectSession = useExm((st) => st.selectSession);
  const sessions = useExm((st) => st.sessions);
  const sessionId = useExm((st) => st.sessionId);
  /** 展开的流 id（同屏只展开一条；再点收起）——hooks 必须在早退之前：
   *  条件 return 后再声明 hooks 会触发 React Hooks 顺序违规（白屏）。 */
  const [expanded, setExpanded] = useState<string | null>(null);
  /** 流内会话列表（展开的流） */
  const [flowSessions, setFlowSessions] = useState<FlowSessionInfo[]>([]);
  const [fsLoading, setFsLoading] = useState(false);

  const load = useCallback(async () => {
    try {
      const [f, c, s] = await Promise.all([
        api.listChannelFlows(),
        api.listChannels(),
        api.channelStatus().catch(() => ({} as Record<string, ChannelStatus>)),
      ]);
      setFlows(f);
      setChannels(c);
      setStatus(s);
    } catch {
      // 接口不可用（老版本网关）：整段不展示比报错好
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
    // 会话切换时刷新（绑定可能被 /new /use 改）
    const h = () => void load();
    window.addEventListener("exm:flows-refresh", h);
    return () => window.removeEventListener("exm:flows-refresh", h);
  }, [load, sessionId]);

  /** 展开流 → 拉流内会话列表（与通道 /sessions 指令同源） */
  const toggleExpand = useCallback(
    (f: ChannelFlow) => {
      if (expanded === f.id) {
        setExpanded(null);
        return;
      }
      setExpanded(f.id);
      setFsLoading(true);
      setFlowSessions([]);
      void api
        .listFlowSessions(f.id)
        .then(setFlowSessions)
        .catch(() => setFlowSessions([]))
        .finally(() => setFsLoading(false));
    },
    [expanded],
  );

  /** 改绑流活跃会话（与 /use 同源状态；绑定后打开该会话并刷新流列表） */
  const switchFlowSession = useCallback(
    (f: ChannelFlow, sid: string) => {
      void api
        .setFlowActiveSession(f.id, sid)
        .then(() => {
          setFlows((prev) => prev.map((x) => (x.id === f.id ? { ...x, activeSession: sid } : x)));
          setExpanded(null);
          void selectSession(sid);
          window.dispatchEvent(new Event("exm:flows-refresh"));
        })
        .catch(() => undefined);
    },
    [selectSession],
  );

  if (loading || flows.length === 0) return <></>;

  // 按通道分组（保持通道在列表中的顺序）
  const byChannel = new Map<string, { channel: Channel | undefined; flows: ChannelFlow[] }>();
  for (const f of flows) {
    if (!byChannel.has(f.channelId)) {
      byChannel.set(f.channelId, { channel: channels.find((c) => c.id === f.channelId), flows: [] });
    }
    byChannel.get(f.channelId)!.flows.push(f);
  }

  const sessionById = new Map(sessions.map((s) => [s.id, s]));
  void sessionById; // 预留：流会话与本地列表的交叉标记（未读数等后续接入）
  /** 打开流绑定会话：未读清零（spec：打开该会话后未读 MUST 清零）+ 跳转。
   *  乐观清零本地计数（REST 异步补刀），失败也不回滚——下次拉列表自然对齐。 */
  const openFlow = (f: ChannelFlow) => {
    if (!f.activeSession) return;
    if (f.unread) {
      setFlows((prev) => prev.map((x) => (x.id === f.id ? { ...x, unread: 0 } : x)));
      void api.readChannelFlow(f.id).catch(() => undefined);
    }
    void selectSession(f.activeSession);
  };

  return (
    <div className="rail-section">
      <div className="rail-label">
        <span className="rail-no">02</span> {t("chat.flows" as never)} [FLOWS]
      </div>
      <div className="flow-list">
        {[...byChannel.entries()].map(([chId, { channel, flows: group }]) => {
          const st = status[chId];
          const connected = st?.state === "ok";
          return (
            <div className="flow-channel" key={chId}>
              <div className="flow-channel-head">
                <span className={`flow-led ${connected ? "ok" : "off"}`} />
                <span className="flow-channel-name">
                  {channel?.account || channel?.id}
                </span>
                <span className="flow-channel-type">
                  {PLATFORM_LABEL[channel?.type ?? ""] ?? channel?.type ?? chId}
                </span>
              </div>
              {group.map((f) => {
                const active = !!f.activeSession && f.activeSession === sessionId;
                // 绑定会话可能不在当前 scope 的列表里（通道绑定别的组）——仍可打开（消息接口按 id 直查）
                const bindable = !!f.activeSession;
                return (
                  <React.Fragment key={f.id}>
                  <div
                    className={`flow-row ${active ? "active" : ""}`}
                    onClick={() => {
                      if (f.activeSession && bindable) {
                        openFlow(f);
                      }
                    }}
                    role="button"
                    tabIndex={0}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" && f.activeSession && bindable) openFlow(f);
                    }}
                  >
                    <span className={`flow-kind-icon ${f.isGroup ? "group" : "dm"}`}>
                      {f.isGroup ? "#" : "@"}
                    </span>
                    <span className="flow-main">
                      <span className="flow-name">{f.displayName || f.chatKey}</span>
                      <span className="flow-preview">{f.lastMessage || t("chat.flowEmpty" as never)}</span>
                    </span>
                    <span className="flow-side">
                      <span className="flow-time">{relativeTime(f.lastActiveAt)}</span>
                      {!!f.unread && f.unread > 0 && !active && (
                        <span className="flow-unread">{f.unread > 99 ? "99+" : f.unread}</span>
                      )}
                      {active && <span className="flow-live" />}
                      {/* 流内会话切换入口（spec：与 /new /use 同源绑定状态） */}
                      <button
                        className={`flow-expand ${expanded === f.id ? "open" : ""}`}
                        title={t("chat.flowSessions" as never)}
                        onClick={(e) => {
                          e.stopPropagation();
                          toggleExpand(f);
                        }}
                      >
                        {expanded === f.id ? "▾" : "◂"}
                      </button>
                    </span>
                  </div>
                  {expanded === f.id && (
                    <div className="flow-session-list">
                      {fsLoading && <div className="flow-session-empty">…</div>}
                      {!fsLoading && flowSessions.length === 0 && (
                        <div className="flow-session-empty">{t("chat.flowNoSessions" as never)}</div>
                      )}
                      {flowSessions.map((s) => (
                        <div
                          key={s.id}
                          className={`flow-session-item ${s.active ? "active" : ""}`}
                          role="button"
                          tabIndex={0}
                          onClick={() => switchFlowSession(f, s.id)}
                          onKeyDown={(e) => {
                            if (e.key === "Enter") switchFlowSession(f, s.id);
                          }}
                        >
                          <span className="flow-session-label">{s.active ? "● " : "○ "}{s.label}</span>
                          <span className="flow-session-time">{relativeTime(s.updatedAt)}</span>
                        </div>
                      ))}
                    </div>
                  )}
                  </React.Fragment>
                );
              })}
            </div>
          );
        })}
      </div>
    </div>
  );
}
