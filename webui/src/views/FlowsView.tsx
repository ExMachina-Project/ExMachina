/** 聊天页（FlowsView，信息架构 2026-11）：外部通道聊天流的只读会话视图。
 *
 *  与「对话」页的分工：对话页 = 用户直接发起（带输入框）；聊天页 = 外部 IM
 *  群聊/私聊流（QQ 群、私聊等），消息由外部用户发起、智能体应答——用户在
 *  这里旁观与介入（查看上下文 / 流内会话切换），不发消息，无输入框。
 *
 *  布局：左侧流列表（按通道分组，未读徽标 + 最后消息预览 + 流内会话切换），
 *  右侧选中流的绑定会话消息流（StatementList 只读渲染）。窄屏单列切换。
 *  数据与通道侧/WebUI 同源（/channels/flows 系端点 + /sessions/:id/messages）。
 */
import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Button, Empty, Skeleton, Tag } from "antd";
import {
  ArrowLeftOutlined,
  CaretRightFilled,
  MailOutlined,
  NumberOutlined,
  ReloadOutlined,
} from "@ant-design/icons";
import { api, type Channel, type ChannelFlow } from "../api";
import type { ChatMessage } from "../types";
import { relativeTime } from "../time";
import { useExm } from "../store";
import { useT } from "../i18n/core";
import { StatementList } from "../components/Statements";
import { useIsNarrow } from "../useIsNarrow";
import { createPortal } from "react-dom";
import { useSubbarSlot } from "../components/Subbar";

/** 平台短标（通道 type → 展示名），与 FlowRailSection 同口径 */
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

export function FlowsView(): React.ReactElement {
  const t = useT();
  const narrow = useIsNarrow();
  /** 桌面端：页面侧栏 portal 到二级侧边栏；窄屏仍在页内（单列切换） */
  const subbarSlot = useSubbarSlot();
  const selectSession = useExm((st) => st.selectSession);
  const activeSessionId = useExm((st) => st.sessionId);

  const [flows, setFlows] = useState<ChannelFlow[]>([]);
  const [channels, setChannels] = useState<Channel[]>([]);
  const [loading, setLoading] = useState(true);
  /** 选中的流（右侧呈现其绑定会话） */
  const [selected, setSelected] = useState<ChannelFlow | null>(null);
  /** 选中流的消息流（只读） */
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [msgLoading, setMsgLoading] = useState(false);
  /** 消息流容器：加载/新消息后滚到底（只读页最新消息在底部） */
  const msgBoxRef = useRef<HTMLDivElement | null>(null);
  // 贴底跟随（与对话页同口径）：用户本来就贴底才跟随新消息，中途上翻回看不打断
  const stickRef = useRef(true);
  useEffect(() => {
    const el = msgBoxRef.current;
    if (!el) return;
    const onScroll = () => {
      stickRef.current = el.scrollHeight - el.scrollTop - el.clientHeight <= 80;
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, [selected?.id]);

  const load = useCallback(async () => {
    try {
      const [f, c] = await Promise.all([api.listChannelFlows(), api.listChannels()]);
      setFlows(f);
      setChannels(c);
      // 选中态纠偏：当前选中流不存在（被删）→ 回落；优先选中绑定会话与
      // 全局会话一致的流（跨页跳转 requestNav 落地时右侧直接呈现目标流）
      setSelected((prev) => {
        if (prev && f.some((x) => x.id === prev.id)) return prev;
        const cur = useExm.getState().sessionId;
        return f.find((x) => x.activeSession && x.activeSession === cur) ?? f[0] ?? null;
      });
    } catch {
      // 接口不可用（无通道部署）：空态呈现
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
    const h = () => void load();
    window.addEventListener("exm:flows-refresh", h);
    const iv = window.setInterval(() => void load(), 30000); // 30s 轮询兜底（未读/预览）
    return () => {
      window.removeEventListener("exm:flows-refresh", h);
      window.clearInterval(iv);
    };
  }, [load]);

  /** 选中流变化 → 拉绑定会话消息（只读） */
  const activeSid = selected?.activeSession;
  useEffect(() => {
    const sid = activeSid;
    if (!sid) {
      setMessages([]);
      return;
    }
    setMsgLoading(true);
    void api
      .messages(sid)
      .then(setMessages)
      .catch(() => setMessages([]))
      .finally(() => setMsgLoading(false));
  }, [activeSid]);
  /** 绑定会话新轮次收束（run.finished）→ 静默重拉消息（聊天页旁观视角保持实时） */
  const reloadBound = useCallback((e?: Event) => {
    const sid = selected?.activeSession;
    if (!sid) return;
    if (e && "detail" in e && (e as CustomEvent<string | undefined>).detail && (e as CustomEvent<string | undefined>).detail !== sid) return;
    void api.messages(sid).then(setMessages).catch(() => undefined);
  }, [selected?.activeSession]);
  useEffect(() => {
    window.addEventListener("exm:bound-refresh", reloadBound as EventListener);
    return () => window.removeEventListener("exm:bound-refresh", reloadBound as EventListener);
  }, [reloadBound]);
  /** 消息加载完成/更新 → 滚到底（旁观视角默认关注最新；上翻回看时不抢滚动条） */
  // 定位语义：切流后首次拿到「非空消息列表」时瞬时定位到最新（骨架期与空列表不定位——
  // 那时容器还没内容高度，滚了也无效；此后同流增量更新平滑跟随）
  const anchoredRef = useRef<string | null>(null);
  useEffect(() => {
    const sid = activeSid ?? null;
    if (sid && anchoredRef.current !== sid) {
      if (msgLoading || messages.length === 0) return;
      anchoredRef.current = sid;
      if (!stickRef.current) return;
      msgBoxRef.current?.scrollTo({ top: msgBoxRef.current.scrollHeight });
      return;
    }
    if (!stickRef.current) return;
    msgBoxRef.current?.scrollTo({ top: msgBoxRef.current.scrollHeight, behavior: "smooth" });
  }, [messages, msgLoading, activeSid]);

  /** 打开流：未读清零（乐观）+ 切换全局会话（与其他页同源）+ 选中 */
  const openFlow = (f: ChannelFlow) => {
    setSelected(f);
    if (f.unread) {
      setFlows((prev) => prev.map((x) => (x.id === f.id ? { ...x, unread: 0 } : x)));
      void api.readChannelFlow(f.id).catch(() => undefined);
    }
    if (f.activeSession) void selectSession(f.activeSession);
  };

  /** 按通道分组（保持通道顺序） */
  const byChannel = useMemo(() => {
    const m = new Map<string, { channel?: Channel; flows: ChannelFlow[] }>();
    for (const f of flows) {
      if (!m.has(f.channelId)) m.set(f.channelId, { channel: channels.find((c) => c.id === f.channelId), flows: [] });
      m.get(f.channelId)!.flows.push(f);
    }
    return [...m.entries()];
  }, [flows, channels]);

  // 账号（通道）筛选（flow-account-filter）：多账号接入时分别查看——
  // 点通道头只看该账号，再点一次恢复全部；选中态通道高亮
  const [accountFilter, setAccountFilter] = useState<string | null>(null);
  const shownChannels = accountFilter ? byChannel.filter(([chId]) => chId === accountFilter) : byChannel;
  const totalUnread = shownChannels.reduce((n, [, { flows: group }]) => group.reduce((m, f) => m + (f.unread ?? 0), n), 0);

  const rail = (
      <aside className="flows-rail">
        <div className="flows-rail-head">
          <span className="flows-title">{t("nav.flows")}</span>
          {totalUnread > 0 && <Tag color="processing">{totalUnread}</Tag>}
          <Button
            type="text"
            size="small"
            icon={<ReloadOutlined />}
            onClick={() => void load()}
            aria-label={t("common.refresh")}
          />
        </div>
        <div className="flows-list">
          {loading && <Skeleton active paragraph={{ rows: 6 }} />}
          {!loading && flows.length === 0 && (
            <Empty description={t("flows.empty")} style={{ marginTop: 80 }} />
          )}
          {shownChannels.map(([chId, { channel, flows: group }]) => (
            <div key={chId} className="flows-channel">
              <div
                className={`flows-channel-head ${accountFilter === chId ? "filter-on" : ""}`}
                onClick={() => setAccountFilter(accountFilter === chId ? null : chId)}
                role="button"
                tabIndex={0}
                onKeyDown={(e) => e.key === "Enter" && setAccountFilter(accountFilter === chId ? null : chId)}
                title={t("flows.accountFilterTip")}
              >
                <span className="flows-channel-name">{channel?.account || channel?.id}</span>
                <span className="flows-channel-platform">
                  {PLATFORM_LABEL[channel?.type ?? ""] ?? channel?.type ?? ""}
                  <CaretRightFilled className={`flows-filter-caret ${accountFilter === chId ? "on" : ""}`} />
                </span>
              </div>
              {group.map((f) => {
                const active = selected?.id === f.id;
                return (
                  <div key={f.id}>
                    <div
                      className={`flow-item ${active ? "active" : ""}`}
                      onClick={() => openFlow(f)}
                      role="button"
                      tabIndex={0}
                      onKeyDown={(e) => e.key === "Enter" && openFlow(f)}
                    >
                      <span className={`flow-kind-icon ${f.isGroup ? "group" : "dm"}`}>
                        {f.isGroup ? <NumberOutlined /> : <MailOutlined />}
                      </span>
                      <span className="flow-main">
                        <span className="flow-name">{f.displayName || f.chatKey}</span>
                        <span className="flow-preview">{f.lastMessage || t("chat.flowEmpty")}</span>
                      </span>
                      <span className="flow-side">
                        <span className="flow-time">{relativeTime(f.lastActiveAt)}</span>
                        {!!f.unread && f.unread > 0 && (
                          <span className="flow-unread">{f.unread > 99 ? "99+" : f.unread}</span>
                        )}
                      </span>
                    </div>
                    {/* 流内会话切换 */}
                  </div>
                );
              })}
            </div>
          ))}
        </div>
      </aside>
  );

  return (
    <div className={`flows-shell ${narrow && selected ? "thread" : ""}`}>
      {/* 流列表（窄屏选中后隐藏——单列切换） */}
      {narrow ? rail : subbarSlot ? createPortal(rail, subbarSlot) : null}

      {/* 右侧：选中流的绑定会话消息流（只读——聊天页无输入框） */}
      <main className="flows-thread">
        {narrow && selected && (
          <Button
            className="flows-back"
            type="text"
            icon={<ArrowLeftOutlined />}
            onClick={() => setSelected(null)}
          >
            {t("nav.flows")}
          </Button>
        )}
        {!selected && !narrow && (
          <Empty description={t("flows.pickOne")} style={{ marginTop: 120 }} />
        )}
        {selected && (
          <>
            <div className="flows-thread-head">
              <span className="flows-thread-name">
                {selected.isGroup ? "#" : "@"} {selected.displayName || selected.chatKey}
              </span>
              <span className="flows-thread-meta">
                {selected.activeSession
                  ? activeSessionId === selected.activeSession
                    ? t("flows.boundCurrent")
                    : t("flows.bound")
                  : t("flows.unbound")}
              </span>
            </div>
            <div className="flows-messages" ref={msgBoxRef}>
              {msgLoading && <Skeleton active paragraph={{ rows: 4 }} />}
              {!msgLoading && !selected.activeSession && (
                <Empty description={t("flows.unboundHint")} style={{ marginTop: 80 }} />
              )}
              {!msgLoading && selected.activeSession && messages.length === 0 && (
                <Empty description={t("flows.noMessages")} style={{ marginTop: 80 }} />
              )}
              {!msgLoading &&
                messages.map((m) => {
                  // 发送者署名：user 消息解 agentId 的 im:/imctx: 标记（群聊真名，
                  // 无标记回落「外部用户」）；智能体侧显示 agentId 或「智能体」
                  const isUser = m.role === "user";
                  const mark = isUser ? (m.agentId ?? "") : "";
                  const sender =
                    mark.startsWith("im:") || mark.startsWith("imctx:")
                      ? mark.slice(mark.indexOf(":") + 1) || t("flows.fromUser")
                      : isUser
                        ? t("flows.fromUser")
                        : (m.agentId || t("flows.fromAgent"));
                  return (
                    <div key={m.id} className={`flows-msg ${m.role}`}>
                      <div className="flows-msg-head">
                        <span className="flows-msg-role">{sender}</span>
                        <span className="flows-msg-time">{relativeTime(m.createdAt)}</span>
                      </div>
                      <StatementList statements={m.statements} plain />
                    </div>
                  );
                })}
            </div>
            {/* 只读声明条（替代输入框位置）：聊天页的消息由外部用户发起 */}
            <div className="flows-readonly">{t("flows.readonlyHint")}</div>
          </>
        )}
      </main>
    </div>
  );
}
