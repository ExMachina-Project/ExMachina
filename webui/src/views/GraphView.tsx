/** 编排视图（原任务图）：DAG 实时渲染（自绘 SVG，按拓扑深度分层）。
 *  渲染主体抽到 components/TaskGraphDag（与聊天页内嵌卡共享同一口径）。 */
import React from "react";
import { Select } from "antd";
import { BrandEmpty } from "../components/BrandEmpty";
import { useExm } from "../store";
import { PageHeader } from "../components/PageHeader";
import { TaskGraphDag } from "../components/TaskGraphDag";
import { useT } from "../i18n/core";

export function GraphView(): React.ReactElement {
  const t = useT();
  const { graph, sessions, sessionId, selectSession } = useExm();
  const sessionLabel = sessions.find((s) => s.id === sessionId)?.title ?? t("graph.noSession");

  if (!graph?.nodes?.length) {
    return (
      <div className="pane-wrap">
        <PageHeader
          en="DAG"
          title={t("graph.title", { session: sessionLabel })}
          desc={t("graph.descEmpty")}
          actions={
            <Select size="small" style={{ minWidth: 180 }} value={sessionId ?? undefined} onChange={(v) => void selectSession(v)} options={sessions.map((s) => ({ value: s.id, label: s.title }))} placeholder={t("graph.pickSession")} />
          }
        />
        <BrandEmpty description={t("graph.empty")} className="graph-empty" />
      </div>
    );
  }

  return (
    <div className="pane-wrap">
      <PageHeader
        en="DAG"
        title={t("graph.title", { session: sessionLabel })}
        desc={t("graph.desc")}
        actions={
          <>
          <span className="readout">
            <span className="k">NODES</span>
            <span className="v">{graph.nodes.length}</span>
            <span className="k" style={{ marginLeft: 10 }}>EDGES</span>
            <span className="v">{graph.nodes.reduce((n, x) => n + x.dependsOn.length, 0)}</span>
          </span>
          <Select size="small" style={{ minWidth: 180, marginLeft: 12 }} value={sessionId ?? undefined} onChange={(v) => void selectSession(v)} options={sessions.map((s) => ({ value: s.id, label: s.title }))} placeholder={t("graph.pickSession")} />
          </>
        }
      />
      <TaskGraphDag graph={graph} />
    </div>
  );
}
