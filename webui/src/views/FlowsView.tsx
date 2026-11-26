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
import React, { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Button, Empty, Skeleton, Tag } from "antd";
import {
  ArrowLeftOutlined,
  CaretRightFilled,
  MailOutlined,
  NumberOutlined,
  ReloadOutlined,
  UnorderedListOutlined,
} from "@ant-design/icons";
import { api, assetUrl, downloadUrl, type Channel, type ChannelFlow } from "../api";
import type { InboundAttachment } from "../types";
import { relativeTime } from "../time";
import { useExm } from "../store";
import { useT } from "../i18n/core";
import { StatementList } from "../components/Statements";
import { MessageProcess, ProcessSteps } from "../components/ProcessSteps";
import { ToolCallList } from "../components/ToolCall";
import { UsageBar } from "../components/UsageBar";
import { RunActivityPanel } from "../components/RunActivityPanel";
import { useIsNarrow } from "../useIsNarrow";
import { createPortal } from "react-dom";
import { useSubbarSlot } from "../components/Subbar";
import { SessionManagementDrawer } from "../components/SessionManagementDrawer";
import type { Session } from "../types";

/** 平台短标（通道 type → 展示名），与通道页保持一致 */
const PLATFORM_LABEL: Record<string, string> = {
  napcat: "QQ",
  snowluma: "QQ",
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
  const activeSession = useExm((st) => st.selectedSession);
  const running = useExm((st) => st.running);
  const liveOrch = useExm((st) => st.liveOrch);
  const liveThinking = useExm((st) => st.liveThinking);
  const liveParts = useExm((st) => st.liveParts);
  const runToolCalls = useExm((st) => st.runToolCalls);
  const runProgress = useExm((st) => st.runProgress);
  const terminalJobs = useExm((st) => st.terminalJobs);
  const spawns = useExm((st) => st.spawns);
  const clearSpawns = useExm((st) => st.clearSpawns);
  const approvals = useExm((st) => st.approvals);
  const sessionMessages = useExm((st) => st.messages);
  const refreshSessionMessages = useExm((st) => st.refreshSessionMessages);

  const [flows, setFlows] = useState<ChannelFlow[]>([]);
  const [channels, setChannels] = useState<Channel[]>([]);
  const [loading, setLoading] = useState(true);
  /** 选中的流（右侧呈现其绑定会话） */
  const [selected, setSelected] = useState<ChannelFlow | null>(null);
  const [selectedArchived, setSelectedArchived] = useState<Session | null>(null);
  const [sessionManagerOpen, setSessionManagerOpen] = useState(false);
  const pendingFlowSelection = useRef<string | null>(null);
  const railOnly = useRef(false);
  const archivedSelection = useRef(false);
  const selectFlowSession = useCallback((id: string) => {
    if (pendingFlowSelection.current === id || (!pendingFlowSelection.current && useExm.getState().sessionId === id)) return;
    pendingFlowSelection.current = id;
    void selectSession(id).catch(() => undefined).finally(() => {
      if (pendingFlowSelection.current === id) pendingFlowSelection.current = null;
    });
  }, [selectSession]);
  /** 选中流的消息流（只读） */
  const viewingSessionId = selectedArchived?.id ?? selected?.activeSession;
  const msgLoading = Boolean(viewingSessionId && viewingSessionId !== activeSessionId);
  const messages = viewingSessionId === activeSessionId ? sessionMessages : [];
  useEffect(() => {
    if (!viewingSessionId || viewingSessionId !== activeSessionId) return;
    const refresh = (event?: Event) => {
      const id = event && (event as CustomEvent<string | undefined>).detail;
      if ((!id || id === viewingSessionId) && document.visibilityState === "visible") void refreshSessionMessages(viewingSessionId);
    };
    window.addEventListener("exm:bound-refresh", refresh);
    document.addEventListener("visibilitychange", refresh);
    const timer = window.setInterval(refresh, 4000); // Inbound user-message fallback; run activity stays WS-driven.
    return () => {
      window.removeEventListener("exm:bound-refresh", refresh);
      document.removeEventListener("visibilitychange", refresh);
      window.clearInterval(timer);
    };
  }, [viewingSessionId, activeSessionId, refreshSessionMessages]);
  /** 消息流容器：加载/新消息后滚到底（只读页最新消息在底部） */
  const msgBoxRef = useRef<HTMLDivElement | null>(null);
  // 贴底跟随（与对话页同口径）：用户本来就贴底才跟随新消息，中途上翻回看不打断
  const stickRef = useRef(true);
  useEffect(() => {
    stickRef.current = true;
    const el = msgBoxRef.current;
    if (!el) return;
    const onScroll = () => {
      stickRef.current = el.scrollHeight - el.scrollTop - el.clientHeight <= 80;
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, [viewingSessionId]);

  const load = useCallback(async () => {
    try {
      const [f, c] = await Promise.all([api.listChannelFlows(), api.listChannels()]);
      setFlows(f);
      setChannels(c);
      // 选中态纠偏：当前选中流不存在（被删）→ 回落；优先选中绑定会话与
      // 全局会话一致的流（跨页跳转 requestNav 落地时右侧直接呈现目标流）
      setSelected((prev) => {
        if (railOnly.current || archivedSelection.current) return null;
        const updated = prev && f.find((x) => x.id === prev.id);
        if (updated) return updated;
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

  // 管理抽屉可打开历史/归档的通道会话；若它不再绑定到当前 Flow，仍在聊天流
  // 里显示其只读消息，不把选择悄悄落到另一条活跃流。
  const archivedRequest = useRef<string | null>(null);
  useEffect(() => {
    if (!activeSessionId || loading) return;
    if (railOnly.current || (pendingFlowSelection.current && pendingFlowSelection.current !== activeSessionId)) return;
    const activeFlow = flows.find((flow) => flow.activeSession === activeSessionId);
    if (activeFlow) {
      archivedRequest.current = null;
      archivedSelection.current = false;
      setSelectedArchived(null);
      setSelected(activeFlow);
      return;
    }
    if (archivedRequest.current === activeSessionId) return;
    archivedRequest.current = activeSessionId;
    let active = true;
    void api.getSession(activeSessionId).then((session) => {
      if (!active || useExm.getState().sessionId !== activeSessionId || (pendingFlowSelection.current && pendingFlowSelection.current !== activeSessionId) || !session.title.startsWith("channel:")) return;
      archivedSelection.current = true;
      setSelectedArchived(session);
      setSelected(null);
    }).catch(() => undefined);
    return () => { active = false; };
  }, [activeSessionId, flows, loading]);

  // The flow shown on first mount must own the shared session subscription too;
  // otherwise only a click creates a WS for it. Preserve explicit archived views.
  useEffect(() => {
    if (!selected?.activeSession || selectedArchived || activeSessionId === selected.activeSession) return;
    if (activeSession?.title.startsWith("channel:") && !flows.some((flow) => flow.activeSession === activeSessionId)) return;
    selectFlowSession(selected.activeSession);
  }, [activeSession, activeSessionId, flows, selectFlowSession, selected?.activeSession, selectedArchived]);

  /** 选中流变化 → 拉绑定会话消息（只读） */
  const activeSid = viewingSessionId;
  const liveBelongsToViewingSession = Boolean(activeSid && activeSessionId === activeSid);
  const visibleLiveOrch = liveBelongsToViewingSession ? liveOrch : "";
  const visibleLiveThinking = liveBelongsToViewingSession ? liveThinking : "";
  const visibleLiveParts = liveBelongsToViewingSession ? liveParts : {};
  const visibleToolCalls = liveBelongsToViewingSession ? runToolCalls : [];
  const visibleRunning = liveBelongsToViewingSession && running;
  const representedToolIds = new Set(Object.values(visibleLiveParts).flat().filter((part) => part.kind === "tool").map((part) => part.callId));
  const extraToolCalls = visibleToolCalls.filter((call) => !representedToolIds.has(call.callId));
  /** 消息加载完成/更新 → 滚到底（旁观视角默认关注最新；上翻回看时不抢滚动条） */
  // 定位语义：切流后首次拿到「非空消息列表」时瞬时定位到最新（骨架期与空列表不定位——
  // 那时容器还没内容高度，滚了也无效；此后同流增量更新平滑跟随）。
  // 用 useLayoutEffect：绘制前同步定位——useEffect 会先绘制顶部再跳底，
  // 切流时可见「从头滚到尾」闪动（部署实测）。
  const anchoredRef = useRef<string | null>(null);
  useLayoutEffect(() => {
    const sid = activeSid ?? null;
    if (sid && anchoredRef.current !== sid) {
      if (msgLoading || messages.length === 0) return;
      anchoredRef.current = sid;
      if (!stickRef.current) return;
      msgBoxRef.current?.scrollTo({ top: msgBoxRef.current.scrollHeight });
      return;
    }
    if (!stickRef.current) return;
    msgBoxRef.current?.scrollTo({ top: msgBoxRef.current.scrollHeight });
  }, [messages, msgLoading, activeSid, liveOrch, liveThinking, liveParts, runToolCalls, runProgress, terminalJobs, spawns, approvals]);

  /** 打开流：未读清零（乐观）+ 选中；selection effect owns the shared session/WS switch. */
  const openFlow = (f: ChannelFlow) => {
    railOnly.current = false;
    archivedSelection.current = false;
    setSelectedArchived(null);
    setSelected(f);
    if (f.unread) {
      setFlows((prev) => prev.map((x) => (x.id === f.id ? { ...x, unread: 0 } : x)));
      void api.readChannelFlow(f.id).catch(() => undefined);
    }
    if (f.activeSession) selectFlowSession(f.activeSession);
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
  const hasSelectedFlow = Boolean(selected || selectedArchived);

  const rail = (
      <aside className="flows-rail">
        <div className="flows-rail-head">
          <span className="flows-title">{t("nav.flows")}</span>
          {totalUnread > 0 && <Tag color="processing">{totalUnread}</Tag>}
          <Button
            type="text"
            size="small"
            icon={<UnorderedListOutlined />}
            className="flow-session-manager-trigger"
            title={t("chat.manageSessions")}
            aria-label={t("chat.manageSessions")}
            onClick={() => setSessionManagerOpen(true)}
          />
          <Button
            type="text"
            size="small"
            icon={<ReloadOutlined />}
            className="flows-refresh-trigger"
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
    <div className={`flows-shell ${narrow && hasSelectedFlow ? "thread" : ""}`}>
      {/* 流列表（窄屏选中后隐藏——单列切换） */}
      {narrow ? rail : subbarSlot ? createPortal(rail, subbarSlot) : null}

      {/* 右侧：选中流的绑定会话消息流（只读——聊天页无输入框） */}
      <main className="flows-thread">
        {narrow && hasSelectedFlow && (
          <Button
            className="flows-back"
            type="text"
            icon={<ArrowLeftOutlined />}
            onClick={() => { railOnly.current = true; setSelected(null); setSelectedArchived(null); }}
          >
            {t("nav.flows")}
          </Button>
        )}
        {!selected && !selectedArchived && !narrow && (
          <Empty description={t("flows.pickOne")} style={{ marginTop: 120 }} />
        )}
        {hasSelectedFlow && (
          <>
            <div className="flows-thread-head">
              <span className="flows-thread-name">
                {selectedArchived
                  ? selectedArchived.originLabel || selectedArchived.title.replace(/^channel:[^:]+:/, "")
                  : `${selected!.isGroup ? "#" : "@"} ${selected!.displayName || selected!.chatKey}`}
              </span>
              <span className="flows-thread-meta">
                {selectedArchived
                  ? t("flows.archivedSession" as never)
                  : selected!.activeSession
                    ? activeSessionId === selected!.activeSession
                      ? t("flows.boundCurrent")
                      : t("flows.bound")
                    : t("flows.unbound")}
              </span>
            </div>
            <div className="flows-messages" ref={msgBoxRef}>
              {msgLoading && <Skeleton active paragraph={{ rows: 4 }} />}
              {!msgLoading && !viewingSessionId && (
                <Empty description={t("flows.unboundHint")} style={{ marginTop: 80 }} />
              )}
              {!msgLoading && viewingSessionId && messages.length === 0 && (
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
                      {/* 过程透明：聊天流与对话页同源展示思考/工具调用（只读，不挂回滚操作） */}
                      <MessageProcess
                        thinking={m.thinking}
                        steps={m.steps}
                        toolCalls={m.toolCalls}
                        labelThinking={(n) => t("chat.usedThinking", { n: String(n) })}
                        labelTools={(n) => t("chat.usedTools", { n: String(n) })}
                      />
                      <StatementList statements={m.statements} plain />
                      {m.attachments && m.attachments.length > 0 && (
                        <InboundAttachments items={m.attachments} />
                      )}
                    </div>
                  );
                })}
              {liveBelongsToViewingSession && Object.entries(visibleLiveParts).filter(([, parts]) => parts.length > 0).map(([agent, parts]) => (
                <div key={`live-${agent}`} className="flows-msg orchestrator live-card">
                  <div className="flows-msg-head"><span className="flows-msg-role">{agent}</span><Tag color="processing">{t("chat.executing")}</Tag></div>
                  <ProcessSteps parts={parts} toolCalls={visibleToolCalls} live={visibleRunning}
                    labelThinking={(n) => t("chat.usedThinking", { n: String(n) })}
                    labelTools={(n) => t("chat.usedTools", { n: String(n) })} />
                </div>
              ))}
              {liveBelongsToViewingSession && (visibleLiveOrch || visibleLiveThinking || extraToolCalls.length > 0) && (
                <div className="flows-msg orchestrator live-card">
                  <div className="flows-msg-head">
                    <span className="flows-msg-role">{t("flows.fromAgent")}</span>
                    {visibleRunning && <Tag color="processing">{t("chat.streaming")}</Tag>}
                  </div>
                  {visibleLiveThinking.trim() && <UsageBar live label={t("chat.usedThinking", { n: String(visibleLiveThinking.length) })}><pre className="usage-think">{visibleLiveThinking}</pre></UsageBar>}
                  {extraToolCalls.length > 0 && <UsageBar live label={t("chat.usedTools", { n: String(extraToolCalls.length) })}><ToolCallList calls={extraToolCalls} /></UsageBar>}
                  {visibleLiveOrch && <pre className="live-pre">{visibleLiveOrch}</pre>}
                </div>
              )}
              {liveBelongsToViewingSession && <RunActivityPanel running={visibleRunning} progress={runProgress} terminalJobs={terminalJobs} spawns={spawns} onClearSpawns={clearSpawns} />}
              {liveBelongsToViewingSession && approvals.filter((approval) => !approval.sessionId || approval.sessionId === activeSid).map((approval) => (
                <div key={approval.approvalId} className="approval-card flow-approval-card">
                  <div className="approval-head"><span className="approval-title">⏸ {t("chat.approvalTitle")}</span><span className="approval-agent mono">{approval.agentId}</span></div>
                  <pre className="approval-cmd">{approval.command}</pre>
                </div>
              ))}
            </div>
            {/* 只读声明条（替代输入框位置）：聊天页的消息由外部用户发起 */}
            <div className="flows-readonly">{t("flows.readonlyHint")}</div>
          </>
        )}
      </main>
      {sessionManagerOpen && <SessionManagementDrawer open onClose={() => setSessionManagerOpen(false)} />}
    </div>
  );
}

/** 入站附件（im-inbound-attach）：外部用户随消息发来的图片/文件/语音——
 *  后端随 user 消息落库的元数据（工作区相对路径），此前聊天流只显示
 *  「（用户发来附件…）」占位文本，图看不切实存在。图片渲染缩略图
 *  （/api/asset 内联预览，点击原图），文件/语音渲染下载卡片。 */
function InboundAttachments({ items }: { items: InboundAttachment[] }) {
  const t = useT();
  return (
    <div className="msg-artifacts inbound">
      {items.map((a, i) => {
        const href = downloadUrl(a.path);
        return a.kind === "image" ? (
          <a key={i} className="msg-artifact image" href={href} target="_blank" rel="noreferrer">
            <img src={assetUrl(a.path)} alt={a.name} loading="lazy" />
          </a>
        ) : (
          <a key={i} className="msg-artifact file" href={href} download={a.name}>
            <span className="msg-artifact-name">{a.name}</span>
            <span className="msg-artifact-kind">{t(a.kind === "voice" ? "flows.attachVoice" : "flows.attachFile")}</span>
          </a>
        );
      })}
    </div>
  );
}
