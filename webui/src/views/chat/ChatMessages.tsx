//! views/chat/ChatMessages.tsx —— 消息流渲染（codebase-refactor 7.2 拆分）：
//! 空态目标捕获盘、消息气泡、流式卡（指挥体/子个体/加载兜底）、轮次轨迹卡、
//! 内联审批卡、回底按钮。纯搬移：渲染结构与状态流转零改动。

import { Button, Card, Space, Spin, Tag } from "antd";
import { CheckOutlined, RobotOutlined } from "@ant-design/icons";
import type React from "react";
import { isDestructiveCommand } from "../../api";
import type { TurnOpsProps } from "../../components/HistoryOps";
import { ToolCallList } from "../../components/ToolCall";
import { UsageBar } from "../../components/UsageBar";
import { OrbitRings } from "../../components/OrbitRings";
import { Emblem } from "../../components/Emblem";
import { ProcessSteps } from "../../components/ProcessSteps";
import type { ChatMessage, ToolCallItem } from "../../types";
import type { TimelineItem } from "../../store";

import { MessageBubble } from "../ChatView";

export interface ChatMessagesProps {
  renderMessages: { m: ChatMessage; turn?: number }[];
  running: boolean;
  target: { mode: string; id: string };
  orchLabel: string;
  activeMeta?: { name?: string; primary?: string } | null;
  activeGroup: string;
  wsConnected: boolean;
  liveOrch: string | null;
  liveThinking: string;
  liveUnitEntries: [string, unknown[]][];
  timeline: TimelineItem[];
  timelineSession: string | null;
  runToolCalls: ToolCallItem[];
  approvals: {
    approvalId: string;
    agentId: string;
    command: string;
    sessionId?: string | null;
  }[];
  sessionId: string | null;
  showBottomBtn: boolean;
  bottomRef: React.RefObject<HTMLDivElement | null>;
  onScrollBottom: () => void;
  onOpenEdit: (turn: number) => void;
  onUndo: (turn: number) => Promise<void>;
  onFork: (turn: number) => Promise<void>;
  onDecideApproval: (id: string, ok: boolean, scope?: "once" | "session" | "allowlist") => void;
  onOpenSubSession: (id: string) => void;
  t: (key: string, params?: Record<string, string>) => string;
  TurnOps: React.ComponentType<TurnOpsProps>;
  TimelineRows: React.ComponentType<{ items: TimelineItem[]; tb: (k: string, v?: Record<string, string>) => string; onOpenSub: (id: string) => void }>;
}

export function ChatMessages(p: ChatMessagesProps): React.ReactElement {
  const {
    renderMessages, running, target, orchLabel, activeMeta, activeGroup, wsConnected,
    liveOrch, liveThinking, liveUnitEntries, timeline, timelineSession, runToolCalls,
    approvals, sessionId, showBottomBtn, bottomRef, onScrollBottom,
    onOpenEdit, onUndo, onFork, onDecideApproval, onOpenSubSession, t, TurnOps, TimelineRows,
  } = p;
  return (
    <div className="chat-scroll">
      {/* 空态：当前会话尚无消息且未在运行 —— 目标捕获盘（与桌面端空态同源母题） */}
      {renderMessages.length === 0 && !running && (
        <div className="chat-acquire">
          <span className="capture-frame chat-acquire-figure">
            <OrbitRings />
            <Emblem size={224} />
          </span>
          <div className="chat-acquire-title">{t("chat.acquireTitle")}</div>
          {/* 文案按目标模式分叉：单智能体说的是它自己的话（独立接单到交付），
              组目标才讲「切组/派发」——此前单一文案让单智能体空态也在讲组的故事 */}
          <div className="chat-acquire-desc">
            {target.mode === "single"
              ? t("chat.acquireDescSingle", { name: orchLabel })
              : t("chat.acquireDesc")}
          </div>
          <div className="chat-acquire-readouts">
            <span className="readout"><span className="k">TGT</span><span className="v">{orchLabel}</span></span>
            {target.mode === "single" ? (
              <span className="readout"><span className="k">AGENT</span><span className="v">{target.id}</span></span>
            ) : (
              <span className="readout"><span className="k">GROUP</span><span className="v">{activeMeta?.name ?? activeGroup}</span></span>
            )}
            <span className="readout"><span className="k">LINK</span><span className={`v ${wsConnected ? "ok" : "err"}`}>{wsConnected ? "ONLINE" : "RECONN"}</span></span>
          </div>
        </div>
      )}
      {renderMessages.map(({ m, turn }) => (
        <MessageBubble
          key={m.id}
          m={m}
          turn={turn}
          orchLabel={orchLabel}
          ops={
            turn != null && !running ? (
              <TurnOps turn={turn} onEdit={onOpenEdit} onUndo={onUndo} onFork={onFork} />
            ) : undefined
          }
        />
      ))}

      {/* 等待首个输出：加载卡（一旦有流式输出/子个体产出即让位，避免与输出卡并存） */}
      {running && !liveOrch && liveUnitEntries.length === 0 && (
        <Card size="small" className="msg-bubble msg-agent">
          <Space orientation="vertical" className="full-width">
            <div>
              <Spin size="small" /> <b>{`${orchLabel} …`}</b>
            </div>
            {(liveThinking.trim() || timeline.length > 0 || runToolCalls.length > 0) && (
              <div className="msg-process">
                {liveThinking.trim() && (
                  <UsageBar label={t("chat.usedThinking", { n: String(liveThinking.length) })} live>
                    <pre className="usage-think">{liveThinking}</pre>
                  </UsageBar>
                )}
                {(timeline.length > 0 || runToolCalls.length > 0) && (
                  <UsageBar label={t("chat.usedTools", { n: String(runToolCalls.length + timeline.length) })} live>
                    <TimelineRows items={timeline} tb={t} onOpenSub={onOpenSubSession} />
                    {runToolCalls.length > 0 && <ToolCallList calls={runToolCalls} />}
                  </UsageBar>
                )}
              </div>
            )}
          </Space>
        </Card>
      )}

      {/* 本轮调度轨迹（收束后保留）：派发/回流条目 + 子会话入口，直到下一次发送或切会话 */}
      {!running && timeline.length > 0 && timelineSession === sessionId && (
        <Card size="small" className="msg-bubble msg-agent turn-timeline-card">
          <div className="msg-process">
            <UsageBar label={t("chat.turnTimeline", { n: String(timeline.length) })} defaultOpen>
              <TimelineRows items={timeline} tb={t} onOpenSub={onOpenSubSession} />
            </UsageBar>
          </div>
        </Card>
      )}

      {liveUnitEntries.map(([agent, parts]) => (
        <Card key={agent} size="small" className="msg-bubble msg-agent live-card">
          <div className="msg-head">
            <RobotOutlined /> <b>{agent}</b> <Tag color="processing">{t("chat.executing")}</Tag>
          </div>
          {/* 思考/工具/输出按发生顺序交错展示（连续工具调用聚合为同一列表） */}
          <ProcessSteps
            parts={parts as never}
            toolCalls={runToolCalls}
            live
            labelThinking={(n) => t("chat.usedThinking", { n: String(n) })}
            labelTools={(n) => t("chat.usedTools", { n: String(n) })}
          />
        </Card>
      ))}

      {liveOrch && (
        <Card size="small" className="msg-bubble msg-agent live-card">
          <div className="msg-head">
            <RobotOutlined /> <b>{orchLabel}</b> <Tag color="processing">{t("chat.streaming")}</Tag>
          </div>
          {(liveThinking.trim() || timeline.length > 0 || runToolCalls.length > 0) && (
            <div className="msg-process">
              {liveThinking.trim() && (
                <UsageBar label={t("chat.usedThinking", { n: String(liveThinking.length) })} live>
                  <pre className="usage-think">{liveThinking}</pre>
                </UsageBar>
              )}
              {(timeline.length > 0 || runToolCalls.length > 0) && (
                <UsageBar label={t("chat.usedTools", { n: String(runToolCalls.length + timeline.length) })} live>
                  <TimelineRows items={timeline} tb={t} onOpenSub={onOpenSubSession} />
                  {runToolCalls.length > 0 && <ToolCallList calls={runToolCalls} />}
                </UsageBar>
              )}
            </div>
          )}
          <pre className="live-pre">{liveOrch}</pre>
        </Card>
      )}
      {/* 内联审批：渲染在消息流内（live 区之后、贴底锚之前）——
          审批到达时随消息流滚动进视线；原先渲染在 chat-scroll 外的静态区，
          流式输出期间用户盯着底部，上方的卡根本看不到（审批交互不可见 bug）。
          只渲染本会话单（approval-session-scope）：跨会话单不再在别的会话里
          弹卡（曾导致任何会话都挂着子会话的审批卡、批准入口错位）——
          跨会话单走侧栏审批页（带「跳转子会话」导航）。 */}
      {approvals
        .filter((a) => !a.sessionId || a.sessionId === sessionId)
        .map((a) => (
        <Card key={a.approvalId} size="small" className="approval-card">
          <div className="approval-head">
            <span className="approval-title">⏸ {t("chat.approvalTitle")}</span>
            <span className="approval-agent mono">{a.agentId}</span>
          </div>
          {a.command.startsWith("[root] ") && (
            <div className="approval-root-flag" role="alert">
              🚨 {t("approval.rootElevation")} — {t("approval.rootElevationHelp")}
            </div>
          )}
          <pre className="approval-cmd">{a.command}</pre>
          <div className="approval-ops">
            <Button
              type="primary"
              size="small"
              icon={<CheckOutlined />}
              onClick={() => onDecideApproval(a.approvalId, true)}
            >
              {t("chat.approvalApprove")}
            </Button>
            {/* 三档放行（approval-ergonomics）：升档按钮仅普通命令单可见——
                [root] 提权单与破坏性命令后端红线降级 once,UI 直接隐藏避免误导 */}
            {!a.command.startsWith("[") && !isDestructiveCommand(a.command) && (
              <>
                <Button
                  size="small"
                  onClick={() => onDecideApproval(a.approvalId, true, "session")}
                >
                  {t("chat.approvalSession")}
                </Button>
                <Button
                  size="small"
                  onClick={() => onDecideApproval(a.approvalId, true, "allowlist")}
                >
                  {t("chat.approvalAllowlist")}
                </Button>
              </>
            )}
            <Button size="small" danger onClick={() => onDecideApproval(a.approvalId, false)}>
              {t("chat.approvalDeny")}
            </Button>
            {a.sessionId && a.sessionId !== sessionId && (
              <Button size="small" type="link" onClick={() => onOpenSubSession(a.sessionId!)}>
                {t("chat.openSubSession")}
              </Button>
            )}
            <span className="approval-hint">{t("chat.approvalHint")}</span>
          </div>
        </Card>
        ))}
      {showBottomBtn && (
        <button
          className="scroll-bottom-btn"
          aria-label={t("chat.scrollToBottom")}
          onClick={onScrollBottom}
        >
          ↓
        </button>
      )}
      <div ref={bottomRef} />
    </div>
  );
}
