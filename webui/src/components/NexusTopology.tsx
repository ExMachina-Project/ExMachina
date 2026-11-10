/**
 * Nexus 拓扑图（nexus-topology-admin）：确定性布局自绘 SVG（D6——不引图库）。
 *
 * 布局：节点按连结度降序环形排布（度最高的放正上，语义上「枢纽」可读）；
 * 个体（边端点）环绕其绑定的连结体扇形分布——结构稳定不抖动（对比力导向）。
 * 交互：选中节点 → 详情面板（名称/类型/启用态/绑定个体清单）+ 解绑操作。
 * 窄屏：图区横向滚动（overflow-x），不溢出遮挡。
 */
import React, { useMemo, useState } from "react";
import { Button, Empty, Popconfirm, Tag } from "antd";
import { useT } from "../i18n/core";

export interface TopoNode {
  id: string;
  name: string;
  kind: string;
  enabled: boolean;
}
export interface TopoEdge {
  agentId: string;
  agentName: string;
  linkId: string;
}

const W = 640;
const H = 460;
const CX = W / 2;
const CY = H / 2;
const R = 150; // 节点环半径

/** 环形角度（度最高的节点在正上方，顺时针展开） */
function nodeAngle(i: number, total: number): number {
  return -Math.PI / 2 + (i * 2 * Math.PI) / Math.max(total, 1);
}

export function NexusTopology({
  nodes,
  edges,
  onUnbind,
  busy,
}: {
  nodes: TopoNode[];
  edges: TopoEdge[];
  onUnbind: (agentId: string, linkId: string) => void;
  busy: boolean;
}): React.ReactElement {
  const t = useT();
  const [selected, setSelected] = useState<string | null>(null);

  // 确定性布局：连结度降序（同度按 id 字典序稳定）
  const ordered = useMemo(() => {
    const deg = (id: string) => edges.filter((e) => e.linkId === id).length;
    return [...nodes].sort((a, b) => deg(b.id) - deg(a.id) || a.id.localeCompare(b.id));
  }, [nodes, edges]);
  const pos = useMemo(() => {
    const m = new Map<string, { x: number; y: number }>();
    ordered.forEach((n, i) => {
      const a = nodeAngle(i, ordered.length);
      m.set(n.id, { x: CX + R * Math.cos(a), y: CY + R * Math.sin(a) });
    });
    return m;
  }, [ordered]);

  // 个体锚点：绑定同一连结体的个体沿「连结体→圆心反向」外侧扇形排布
  const agentPos = useMemo(() => {
    const m = new Map<string, { x: number; y: number }>();
    const byLink = new Map<string, TopoEdge[]>();
    for (const e of edges) {
      const list = byLink.get(e.linkId) ?? [];
      list.push(e);
      byLink.set(e.linkId, list);
    }
    for (const [linkId, list] of byLink) {
      const np = pos.get(linkId);
      if (!np) continue;
      const base = Math.atan2(np.y - CY, np.x - CX); // 向外方向
      const spread = Math.min(0.9, 0.35 * (list.length - 1)); // 扇形张角
      list.forEach((e, i) => {
        const off = list.length === 1 ? 0 : base - spread / 2 + (spread * i) / (list.length - 1);
        const r = R + 72;
        m.set(e.agentId, { x: CX + r * Math.cos(off), y: CY + r * Math.sin(off) });
      });
    }
    return m;
  }, [edges, pos]);

  if (nodes.length === 0) {
    return (
      <div className="nexus-topo-canvas nexus-topo-empty" data-empty="1">
        <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description={t("nexus.topoEmpty")} />
      </div>
    );
  }
  const sel = selected ? nodes.find((n) => n.id === selected) : null;
  const selEdges = selected ? edges.filter((e) => e.linkId === selected) : [];

  return (
    <div className="nexus-topo">
      <div className="nexus-topo-canvas">
        <svg width={W} height={H} role="img" aria-label={t("nexus.topoTitle")}>
          {/* 边：个体 → 连结体（任务转交方向） */}
          {edges.map((e) => {
            const a = agentPos.get(e.agentId);
            const b = pos.get(e.linkId);
            if (!a || !b) return null;
            const dim = selected && e.linkId !== selected;
            return (
              <line
                key={`${e.agentId}-${e.linkId}`}
                x1={a.x} y1={a.y} x2={b.x} y2={b.y}
                className={`topo-edge${dim ? " topo-dim" : ""}${e.linkId === selected ? " topo-edge-sel" : ""}`}
              />
            );
          })}
          {/* 节点：连结体 */}
          {ordered.map((n) => {
            const p = pos.get(n.id)!;
            const deg = edges.filter((e) => e.linkId === n.id).length;
            const dim = selected && n.id !== selected;
            return (
              <g
                key={n.id}
                transform={`translate(${p.x},${p.y})`}
                className={`topo-node${dim ? " topo-dim" : ""}${n.id === selected ? " topo-node-sel" : ""}`}
                onClick={() => setSelected(n.id === selected ? null : n.id)}
              >
                <circle r={16} className={`topo-node-circle${n.enabled ? "" : " topo-node-off"}`} />
                <text y={-24} textAnchor="middle" className="topo-node-label">{n.name}</text>
                <text y={34} textAnchor="middle" className="topo-node-meta mono">{n.kind}{deg > 0 ? ` · ${deg}` : ""}</text>
              </g>
            );
          })}
          {/* 个体：边端点（小方点） */}
          {[...new Map(edges.map((e) => [e.agentId, e])).values()].map((e) => {
            const p = agentPos.get(e.agentId);
            if (!p) return null;
            return (
              <g key={e.agentId} transform={`translate(${p.x},${p.y})`} className="topo-agent">
                <rect x={-5} y={-5} width={10} height={10} rx={2} />
                <text y={-10} textAnchor="middle" className="topo-agent-label">{e.agentName}</text>
              </g>
            );
          })}
        </svg>
      </div>
      {/* 详情面板：选中节点的属性与连结清单 */}
      {sel && (
        <div className="nexus-topo-detail">
          <div className="topo-detail-head">
            <b>{sel.name}</b>
            <Tag>{sel.kind}</Tag>
            <Tag color={sel.enabled ? "success" : "default"}>
              {sel.enabled ? t("nexus.enabled") : t("nexus.disabled")}
            </Tag>
          </div>
          <div className="dim topo-detail-sub mono">{sel.id}</div>
          {selEdges.length === 0 ? (
            <div className="dim">{t("nexus.topoNoEdges")}</div>
          ) : (
            <div className="topo-detail-edges">
              {selEdges.map((e) => (
                <div key={e.agentId} className="topo-edge-row">
                  <span>{e.agentName}</span>
                  <span className="mono dim">{e.agentId}</span>
                  <Popconfirm
                    title={t("nexus.topoUnbindConfirm", { name: e.agentName })}
                    onConfirm={() => onUnbind(e.agentId, e.linkId)}
                    disabled={busy}
                  >
                    <Button size="small" danger type="text" disabled={busy}>
                      {t("nexus.topoUnbind")}
                    </Button>
                  </Popconfirm>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
