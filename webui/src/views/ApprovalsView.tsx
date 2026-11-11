/** 执行审批：被闸门拦截的命令，批准后代执行 / 拒绝 */
import React, { useCallback, useEffect, useState } from "react";
import { Button, Segmented, Space, Spin, Table, Tag, message } from "antd";
import { BrandEmpty } from "../components/BrandEmpty";
import { ArrowRightOutlined, CheckOutlined, CloseOutlined, ReloadOutlined } from "@ant-design/icons";
import { api, type ApprovalRequest, isDestructiveCommand } from "../api";
import { PageHeader } from "../components/PageHeader";
import { useExm } from "../store";
import { useT } from "../i18n/core";
import { useIsNarrow } from "../useIsNarrow";

export function ApprovalsView(): React.ReactElement {
  const t = useT();
  const requestNav = useExm((st) => st.requestNav);
  const narrow = useIsNarrow();
  const [status, setStatus] = useState<string>("pending");
  const [items, setItems] = useState<ApprovalRequest[]>([]);
  const [loading, setLoading] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      setItems(await api.listApprovals(status === "all" ? undefined : status, 100));
    } finally {
      setLoading(false);
    }
  }, [status]);

  useEffect(() => {
    void load();
    // 审批事件驱动刷新：approval.required / 裁决动作都会广播 exm:approvals-refresh
    // （store 侧发），列表页借同链路自刷新（WS 在线时免手动刷新）
    const h = () => void load();
    window.addEventListener("exm:approvals-refresh", h);
    return () => window.removeEventListener("exm:approvals-refresh", h);
  }, [load]);

  const decide = async (id: string, approve: boolean, scope: "once" | "session" | "allowlist" = "once") => {
    try {
      const r = await api.decideApproval(id, approve, scope);
      if (r.status === "executed") {
        message.success(t("approvals.approvedExecuted", { id }));
      } else if (r.status === "failed") {
        message.warning(t("approvals.approvedFailed", { id }));
      } else {
        message.info(t("approvals.rejected", { id }));
      }
      await load();
    } catch (e) {
      message.error(t("common.opFailed", { err: String(e) }));
    }
  };

  return (
    <div className="pane-wrap">
      <PageHeader
        en="APPROVALS"
        title={t("nav.approvals")}
        desc={t("approvals.desc")}
        actions={
          <>
            <Segmented
              value={status}
              onChange={(v) => setStatus(v as string)}
              options={[
                { value: "pending", label: t("approvals.tabPending") },
                { value: "all", label: t("approvals.tabAll") },
              ]}
            />
            <Button icon={<ReloadOutlined />} onClick={() => void load()}>
              {t("common.refresh")}
            </Button>
          </>
        }
      />
      <Spin spinning={loading}>
        <div className={narrow ? "tbl-fit" : undefined}>
        <Table<ApprovalRequest>
          size="small"
          rowKey="id"
          pagination={{ pageSize: 12 }}
          scroll={{ x: narrow ? 560 : undefined }}
          /* 固定布局：命令列的 .cmd-code（inline-block 长命令不可断行）在自动布局下
             会把最小内容宽度撑到 infinity，表格从无宽度列（结果列）抢宽——表头被
             压成一字宽竖排。fixed 布局按 width 均分剩余宽度，长命令在列内自行省略 */
          tableLayout="fixed"
          dataSource={items}
          locale={{ emptyText: <BrandEmpty description={t("approvals.empty")} className="pane-empty" /> }}
          columns={[
            {
              title: t("approvals.colStatus"),
              width: 100,
              render: (_, r) =>
                r.status === "pending" ? (
                  <Tag color="warning">{t("approvals.stPending")}</Tag>
                ) : r.status === "executed" ? (
                  <Tag color="success">{t("approvals.stExecuted")}</Tag>
                ) : r.status === "approved" ? (
                  <Tag color="success">{t("approvals.stApproved")}</Tag>
                ) : r.status === "failed" ? (
                  <Tag color="error">{t("approvals.stFailed")}</Tag>
                ) : (
                  <Tag>{t("approvals.stRejected")}</Tag>
                ),
            },
            {
              title: t("approvals.colCommand"),
              render: (_, r) => (
                <Space orientation="vertical" size={0} className="full-width">
                  {r.command.startsWith("[root] ") && (
                    <Tag color="error" style={{ marginBottom: 4 }}>
                      🚨 {t("approval.rootElevation")}
                    </Tag>
                  )}
                  {r.command.startsWith("[meta] ") && (
                    <Tag color="purple" style={{ marginBottom: 4 }}>
                      🧠 {t("approval.metaProposal")}
                    </Tag>
                  )}
                  <code className="cmd-code">$ {r.command}</code>
                  {/* 窄屏：会话跳转并入元信息行（原独立列已裁、操作列只留批准/拒绝
                      ——icon 钮在 390px 仍差 10px 出视口，元信息行点击热区更大且省一列宽） */}
                  {narrow ? (
                    r.sessionId ? (
                      <button
                        className="link-btn mono dim"
                        onClick={() => requestNav("chat", r.sessionId)}
                        aria-label={t("approvals.jump")}
                      >
                        {r.agentId} · {String(r.createdAt ?? "").replace("T", " ").slice(5, 16)} ↗
                      </button>
                    ) : (
                      <span className="mono dim">{r.agentId} · {String(r.createdAt ?? "").replace("T", " ").slice(5, 16)}</span>
                    )
                  ) : (
                    <span className="mono dim">
                      {r.agentId} · {String(r.createdAt ?? "").replace("T", " ").slice(5, 16)}
                    </span>
                  )}
                </Space>
              ),
            },
            // 窄屏降级：结果与所属会话列隐藏（详情在命令 cell 元信息与跳转里可达），
            // 操作列（批准/拒绝）必须常驻——移动端功能不可达比信息缺失严重
            {
              title: t("approvals.colResult"),
              responsive: ["md"],
              render: (_, r) =>
                r.result ? <span className="dim result-cell">{r.result.slice(0, 240)}</span> : <span className="dim">—</span>,
            },
            {
              title: t("approvals.colSession"),
              width: 90,
              responsive: ["md"],
              render: (_, r) =>
                r.sessionId ? (
                  <Button
                    size="small"
                    icon={<ArrowRightOutlined />}
                    onClick={() => requestNav("chat", r.sessionId)}
                    title={t("approvals.jump")}
                  >
                    {t("approvals.jump")}
                  </Button>
                ) : (
                  <span className="dim">—</span>
                ),
            },
            {
              title: t("approvals.colOps"),
              width: 240,
              // 窄屏固定右侧：批准/拒绝是审批页核心操作，恒可达（不依赖横滚）
              fixed: narrow ? "right" : undefined,
              render: (_, r) =>
                r.status === "pending" ? (
                  <Space>
                    <Button size="small" type="primary" icon={<CheckOutlined />} onClick={() => void decide(r.id, true)}>
                      {t("approvals.approve")}
                    </Button>
                    {/* 三档放行（approval-ergonomics）：升档按钮仅普通命令单可见——
                        [root] 提权单与破坏性命令后端红线降级 once,UI 直接隐藏避免误导 */}
                    {!r.command.startsWith("[") && !isDestructiveCommand(r.command) && (
                      <>
                        <Button size="small" onClick={() => void decide(r.id, true, "session")}>
                          {t("approvals.session")}
                        </Button>
                        <Button size="small" onClick={() => void decide(r.id, true, "allowlist")}>
                          {t("approvals.allowlist")}
                        </Button>
                      </>
                    )}
                    <Button size="small" danger icon={<CloseOutlined />} onClick={() => void decide(r.id, false)}>
                      {t("approvals.reject")}
                    </Button>
                    {/* 窄屏：跳转已并入命令列元信息行（narrow 分支），操作列只留批准/拒绝 */}
                  </Space>
                ) : (
                  <span className="mono dim" title={r.decidedAt ?? ""}>{r.decidedAt ? String(r.decidedAt).replace("T", " ").slice(5, 16) : "—"}</span>
                ),
            },
          ]}
        />
        </div>
      </Spin>
    </div>
  );
}
