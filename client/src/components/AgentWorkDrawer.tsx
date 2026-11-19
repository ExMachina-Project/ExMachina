import { useMemo, useState } from "react";
import type React from "react";
import { Button, Drawer, Empty, Tag } from "antd";
import { CheckCircleOutlined, ClockCircleOutlined, CloseCircleOutlined, LoadingOutlined, RobotOutlined } from "@ant-design/icons";
import type { AgentDefinition, ChatMessage, TaskGraph, ToolCallItem } from "../types";
import type { LivePart, TimelineItem } from "../store";
import { useClientServices } from "../services";

type WorkStatus = "running" | "blocked" | "failed" | "done" | "pending";
interface AgentWork {
  id: string;
  nodes: TaskGraph["nodes"];
  tools: ToolCallItem[];
  events: TimelineItem[];
  live: LivePart[];
  output: string;
  status: WorkStatus;
}

const RUNNING = new Set(["running", "dispatched", "syncing", "arbitrating"]);

export function AgentWorkDrawer({ open, onClose, sessionId, sessionTitle, running, graph, liveParts, toolCalls, messages, timeline, agents, targetLabel }: {
  open: boolean;
  onClose: () => void;
  sessionId: string | null;
  sessionTitle: string;
  running: boolean;
  graph: TaskGraph | null;
  liveParts: Record<string, LivePart[]>;
  toolCalls: ToolCallItem[];
  messages: ChatMessage[];
  timeline: TimelineItem[];
  agents: AgentDefinition[];
  targetLabel: string;
}): React.ReactElement {
  const { t } = useClientServices();
  const [selectedId, setSelectedId] = useState("");
  const work = useMemo(() => {
    const byAgent = new Map<string, AgentWork>();
    const ensure = (id: string) => {
      if (!id) return undefined;
      let entry = byAgent.get(id);
      if (!entry) {
        entry = { id, nodes: [], tools: [], events: [], live: [], output: "", status: "pending" };
        byAgent.set(id, entry);
      }
      return entry;
    };
    for (const node of graph?.nodes ?? []) ensure(node.agentIdentifier)?.nodes.push(node);
    const calls = new Map<string, ToolCallItem>();
    for (const msg of messages) for (const call of msg.toolCalls ?? []) calls.set(call.callId, call);
    for (const call of toolCalls) calls.set(call.callId, call);
    for (const call of calls.values()) ensure(call.agentId)?.tools.push(call);
    for (const [id, parts] of Object.entries(liveParts)) {
      const entry = ensure(id);
      if (entry) entry.live = parts;
    }
    for (const event of timeline) if (event.agentId) ensure(event.agentId)?.events.push(event);
    for (const msg of messages) {
      if (!msg.agentId || msg.role === "user") continue;
      const entry = ensure(msg.agentId);
      const output = msg.statements.map((statement) => statement.text).join("\n").trim();
      if (entry && output) entry.output = output;
    }
    for (const entry of byAgent.values()) {
      const nodeStatuses = entry.nodes.map((node) => node.status);
      const toolStatuses = entry.tools.map((call) => call.status);
      entry.status = running && (entry.live.length > 0 || toolStatuses.includes("running") || nodeStatuses.some((status) => RUNNING.has(status)))
        ? "running"
        : nodeStatuses.includes("blocked") ? "blocked"
          : nodeStatuses.includes("failed") || toolStatuses.includes("error") || entry.events.some((event) => event.kind === "error") ? "failed"
            : nodeStatuses.includes("done") || toolStatuses.includes("ok") || entry.output ? "done" : "pending";
    }
    return [...byAgent.values()].sort((a, b) => {
      const order: WorkStatus[] = ["running", "blocked", "failed", "pending", "done"];
      return order.indexOf(a.status) - order.indexOf(b.status) || a.id.localeCompare(b.id);
    });
  }, [graph, liveParts, toolCalls, messages, timeline, running]);

  const activeId = work.some((entry) => entry.id === selectedId) ? selectedId : work[0]?.id ?? "";
  const active = work.find((entry) => entry.id === activeId);
  const activeCount = work.filter((entry) => entry.status === "running").length;
  const issueCount = work.filter((entry) => entry.status === "blocked" || entry.status === "failed").length;
  const toolCount = work.reduce((count, entry) => count + entry.tools.length, 0);
  const agentName = (id: string) => agents.find((agent) => agent.identifier === id)?.name ?? id;
  const statusLabel = (status: WorkStatus) => t(`agentWork.status.${status}` as never);

  return <Drawer
    open={open}
    onClose={onClose}
    placement="right"
    width="min(760px, calc(100vw - 16px))"
    className="agent-work-drawer"
    title={<span className="agent-work-title"><RobotOutlined /> {t("agentWork.title" as never)}</span>}
    destroyOnHidden
  >
    <div className="agent-work-head">
      <div className="agent-work-session"><span className="agent-work-k">SESSION</span><strong title={sessionTitle}>{sessionTitle || t("agentWork.noSession" as never)}</strong><small className="mono">{sessionId ?? "—"}</small></div>
      <Tag color={running ? "processing" : undefined}>{running ? t("agentWork.status.running" as never) : t("agentWork.status.done" as never)}</Tag>
    </div>
    <div className="agent-work-metrics">
      <div><span>{t("agentWork.agents" as never)}</span><strong>{work.length}</strong></div>
      <div><span>{t("agentWork.active" as never)}</span><strong>{activeCount}</strong></div>
      <div><span>{t("agentWork.issues" as never)}</span><strong className={issueCount ? "is-warning" : ""}>{issueCount}</strong></div>
      <div><span>{t("agentWork.tools" as never)}</span><strong>{toolCount}</strong></div>
    </div>

    {work.length === 0 ? (
      <Empty className="agent-work-empty" image={Empty.PRESENTED_IMAGE_SIMPLE} description={t((sessionId ? "agentWork.noActivity" : "agentWork.noSession") as never)} />
    ) : (
      <div className="agent-work-layout">
        <section className="agent-work-map" aria-label={t("agentWork.map" as never)}>
          <div className="agent-work-map-head"><span>{t("agentWork.executionMap" as never)}</span><small>{graph?.nodes.length ?? work.length} {t("agentWork.nodes" as never)}</small></div>
          <div className="agent-work-orbit">
            <svg viewBox="0 0 360 320" aria-hidden="true">
              <circle cx="180" cy="160" r="108" className="agent-orbit-ring" />
              {work.slice(0, 10).map((entry, index) => {
                const angle = (Math.PI * 2 * index) / Math.min(work.length, 10) - Math.PI / 2;
                const x = 180 + Math.cos(angle) * 108;
                const y = 160 + Math.sin(angle) * 108;
                return <line key={entry.id} x1="180" y1="160" x2={x} y2={y} className={`agent-orbit-link status-${entry.status}`} />;
              })}
            </svg>
            <div className={`agent-work-core ${running ? "is-running" : ""}`}>
              <RobotOutlined />
              <strong>{targetLabel || t("agentWork.target" as never)}</strong>
              <small>{running ? t("agentWork.status.running" as never) : t("agentWork.sessionContext" as never)}</small>
            </div>
            {work.slice(0, 10).map((entry, index) => {
              const angle = (Math.PI * 2 * index) / Math.min(work.length, 10) - Math.PI / 2;
              const x = 180 + Math.cos(angle) * 108;
              const y = 160 + Math.sin(angle) * 108;
              return <button key={entry.id} type="button" className={`agent-orbit-node status-${entry.status} ${activeId === entry.id ? "selected" : ""}`}
                style={{ left: `${(x / 360) * 100}%`, top: `${(y / 320) * 100}%` }}
                onClick={() => setSelectedId(entry.id)} title={`${agentName(entry.id)} · ${statusLabel(entry.status)}`} aria-pressed={activeId === entry.id}>
                <span className="agent-orbit-led" /><span className="agent-orbit-name">{entry.id}</span>
              </button>;
            })}
          </div>
          {work.length > 10 && <p className="agent-work-overflow">+{work.length - 10} {t("agentWork.moreAgents" as never)}</p>}
          <div className="agent-work-legend"><span><i className="status-running" />{t("agentWork.status.running" as never)}</span><span><i className="status-done" />{t("agentWork.status.done" as never)}</span><span><i className="status-blocked" />{t("agentWork.status.blocked" as never)}</span></div>
          <div className="agent-work-list">{work.map((entry) => <button type="button" key={entry.id} className={`agent-work-row ${activeId === entry.id ? "selected" : ""}`} onClick={() => setSelectedId(entry.id)}>
            <span className={`agent-work-status status-${entry.status}`}>{entry.status === "running" ? <LoadingOutlined spin /> : entry.status === "failed" || entry.status === "blocked" ? <CloseCircleOutlined /> : entry.status === "done" ? <CheckCircleOutlined /> : <ClockCircleOutlined />}</span>
            <span className="agent-work-row-main"><strong>{agentName(entry.id)}</strong><small>{entry.nodes.at(-1)?.title || entry.tools.at(-1)?.tool || entry.events.at(-1)?.text || t("agentWork.noTask" as never)}</small></span>
            <Tag>{statusLabel(entry.status)}</Tag>
          </button>)}</div>
        </section>

        <section className="agent-work-detail">
          {!active ? <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} /> : <>
            <header><span className="agent-work-k">AGENT / {active.id}</span><h3>{agentName(active.id)}</h3><Tag className={`agent-work-state status-${active.status}`}>{statusLabel(active.status)}</Tag></header>
            {active.nodes.map((node) => <article className="agent-work-task" key={node.id}>
              <div><strong>{node.title}</strong><Tag>{node.status}</Tag></div>
              <p>{node.objective}</p>
              {node.modelSelection && <small>{t("agentWork.model" as never)} · {node.modelSelection.model} · {node.modelSelection.reason}</small>}
              {node.dependsOn.length > 0 && <small>{t("agentWork.depends" as never)} · {node.dependsOn.join(", ")}</small>}
            </article>)}
            {active.live.length > 0 && <section className="agent-work-section"><h4>{t("agentWork.liveOutput" as never)}</h4><pre>{active.live.map((part) => part.text ?? (part.kind === "tool" ? t("agentWork.toolInProgress" as never) : "")).filter(Boolean).join("\n") || t("agentWork.waitingOutput" as never)}</pre></section>}
            {active.output && <section className="agent-work-section"><h4>{t("agentWork.latestOutput" as never)}</h4><pre>{active.output}</pre></section>}
            <section className="agent-work-section"><h4>{t("agentWork.toolCalls" as never)} <span>{active.tools.length}</span></h4>
              {active.tools.length ? active.tools.slice(-12).reverse().map((call) => <div key={call.callId} className={`agent-work-tool status-${call.status === "error" ? "failed" : call.status === "ok" ? "done" : "running"}`}>
                <span>{call.status === "running" ? <LoadingOutlined spin /> : call.status === "error" ? <CloseCircleOutlined /> : <CheckCircleOutlined />}</span>
                <div><strong>{call.tool}</strong><small>{String(call.args.path ?? call.args.command ?? call.args.query ?? call.args.url ?? "")}</small>{call.summary && <p>{call.summary}</p>}
                  <details className="agent-work-tool-detail"><summary>{t("agentWork.arguments" as never)}</summary><pre>{JSON.stringify(call.args, null, 2)}</pre></details>
                </div>
                {typeof call.durationMs === "number" && <small className="mono">{call.durationMs} ms</small>}
              </div>) : <p className="agent-work-muted">{t("agentWork.noTools" as never)}</p>}
            </section>
            {active.events.length > 0 && <section className="agent-work-section"><h4>{t("agentWork.events" as never)}</h4>{active.events.slice(-8).reverse().map((event, index) => <div className={`agent-work-event event-${event.kind}`} key={`${event.kind}-${index}`}><span>{event.kind.toUpperCase()}</span><p>{event.text}</p></div>)}</section>}
          </>}
        </section>
      </div>
    )}
    <div className="agent-work-footer"><Button onClick={onClose}>{t("common.close" as never)}</Button><span className="mono">{sessionId ?? "NO SESSION"}</span></div>
  </Drawer>;
}
