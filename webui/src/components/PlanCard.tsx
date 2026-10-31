/** 计划模式卡片（spec plan-mode：WebUI 跨端审批呈现）：
 *  逐条目标/动作 + edit/patch 增删行着色预览 + terminal 命令块；
 *  批量批准（回放结果逐条 ok/失败）/拒绝（保留变灰）；模式开关由 ChatView 会话头承载。
 */
import React, { useState } from "react";
import { Button, Card, Popconfirm, Tag } from "antd";
import { CheckOutlined, CloseOutlined } from "@ant-design/icons";
import { useT } from "../i18n/core";
import type { PlanAppliedItem, PlanInfo } from "../api";

/** edit 条目轻量 diff 预览：old_string 红 / new_string 绿（前端生成，不调后端） */
function EditPreview({ args }: { args: Record<string, unknown> }): React.ReactElement | null {
  const old = typeof args.old_string === "string" ? args.old_string : "";
  const neu = typeof args.new_string === "string" ? args.new_string : "";
  if (!old && !neu) return null;
  return (
    <pre className="plan-diff">
      {old.split("\n").map((l, i) => (
        <div key={`o${i}`} className="plan-del">{l || " "}</div>
      ))}
      {neu.split("\n").map((l, i) => (
        <div key={`n${i}`} className="plan-add">{l || " "}</div>
      ))}
    </pre>
  );
}

/** patch 条目：diff 语法着色（+绿 -绿 全文按行） */
function PatchPreview({ args }: { args: Record<string, unknown> }): React.ReactElement | null {
  const text = typeof args.patch === "string" ? args.patch : "";
  if (!text) return null;
  return (
    <pre className="plan-diff">
      {text.split("\n").map((l, i) => {
        const cls = l.startsWith("+") ? "plan-add" : l.startsWith("-") ? "plan-del" : "";
        return <div key={i} className={cls}>{l || " "}</div>;
      })}
    </pre>
  );
}

/** 大预览折叠：超 40 行截断 + 展开/收起 */
function Foldable({ lines, children }: { lines: number; children: React.ReactNode }): React.ReactElement {
  const [open, setOpen] = useState(false);
  const t = useT();
  if (lines <= 40) return <>{children}</>;
  return (
    <div className="plan-fold">
      <div style={open ? undefined : { maxHeight: "22em", overflow: "hidden" }}>{children}</div>
      <Button size="small" type="link" onClick={() => setOpen(!open)}>
        {open ? t("plan.foldLess") : t("plan.foldMore", { n: String(lines) })}
      </Button>
    </div>
  );
}

const ACTION_TAG: Record<string, { color: string; label: string }> = {
  write: { color: "blue", label: "WRITE" },
  edit: { color: "orange", label: "EDIT" },
  patch: { color: "purple", label: "PATCH" },
  exec: { color: "red", label: "EXEC" },
  input: { color: "magenta", label: "INPUT" },
  git: { color: "cyan", label: "GIT" },
};

export interface PlanCardProps {
  info: PlanInfo;
  /** 批准回放的逐条结果（plan.applied 事件 / approve 响应） */
  applied: PlanAppliedItem[] | null;
  pending: boolean;
  onApprove: () => void;
  onReject: () => void;
}

export function PlanCard({ info, applied, pending, onApprove, onReject }: PlanCardProps): React.ReactElement | null {
  const t = useT();
  const byId = new Map((applied ?? []).map((a) => [a.itemId, a]));
  const done = applied !== null;
  if (info.items.length === 0) return null;
  return (
    <Card
      size="small"
      className={`plan-card${done ? " plan-card-done" : ""}`}
      title={
        <span className="plan-title">
          📋 {t("plan.cardTitle", { n: String(info.items.length) })}
          {info.mode === "plan" && <Tag color="gold" style={{ marginLeft: 8 }}>PLAN</Tag>}
        </span>
      }
      extra={
        done ? (
          <span className="plan-result-summary">
            {t("plan.replayed", { ok: String(applied?.filter((a) => a.ok).length ?? 0), fail: String(applied?.filter((a) => !a.ok).length ?? 0) })}
          </span>
        ) : (
          <span className="plan-ops">
            <Popconfirm title={t("plan.approveConfirm")} onConfirm={onApprove}>
              <Button type="primary" size="small" icon={<CheckOutlined />}>
                {t("plan.approve")}
              </Button>
            </Popconfirm>
            <Button size="small" danger icon={<CloseOutlined />} onClick={onReject}>
              {t("plan.reject")}
            </Button>
          </span>
        )
      }
    >
      {info.items.map((item, idx) => {
        const res = byId.get(item.id);
        const tag = ACTION_TAG[item.action] ?? { color: "default", label: item.action.toUpperCase() };
        let lines = 1;
        if (item.tool === "edit" || item.tool === "patch") {
          const s = typeof item.args.old_string === "string" ? item.args.old_string : "";
          const n = typeof item.args.new_string === "string" ? item.args.new_string : "";
          const p = typeof item.args.patch === "string" ? item.args.patch : "";
          lines = Math.max(s.split("\n").length, n.split("\n").length, p.split("\n").length);
        }
        return (
          <div key={item.id} className={`plan-item${done ? (res?.ok ? " plan-item-ok" : " plan-item-fail") : ""}`}>
            <div className="plan-item-head">
              <span className="plan-idx">#{idx + 1}</span>
              <Tag color={tag.color}>{tag.label}</Tag>
              <span className="plan-target mono">{item.target}</span>
              {res && <span className={`plan-item-badge ${res.ok ? "ok" : "fail"}`}>{res.ok ? "✅" : "❌"}</span>}
            </div>
            {item.tool === "edit" && <Foldable lines={lines}><EditPreview args={item.args} /></Foldable>}
            {item.tool === "patch" && <Foldable lines={lines}><PatchPreview args={item.args} /></Foldable>}
            {(item.tool === "terminal" || item.tool === "computer") && (
              <pre className="plan-cmd">{String(item.target)}</pre>
            )}
            {item.tool === "filesystem" && typeof item.args.content === "string" && (
              <Foldable lines={item.args.content.split("\n").length}>
                <pre className="plan-diff">
                  {item.args.content.split("\n").map((l, i) => (
                    <div key={i} className="plan-add">{l || " "}</div>
                  ))}
                </pre>
              </Foldable>
            )}
          </div>
        );
      })}
      {done && pending === false && <div className="plan-hint">{t("plan.doneHint")}</div>}
    </Card>
  );
}
