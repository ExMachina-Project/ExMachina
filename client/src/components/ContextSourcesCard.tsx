/** 上下文来源卡（capability-parity 6.3）：本会话注入的文件 / 记忆 / 检索 / 技能四类来源，
 *  逐项呈现状态、禁用叠加位（含原状态）与**缺失原因**。
 *
 *  纯展示组件：清单与所有写操作都经 Core 的同一治理接口（GET/POST /sessions/:id/context），
 *  通道 /context 与 CLI exm context 共用同一事实源——本组件不推导、不缓存任何状态。
 *  停用只关掉**自动注入**；模型主动的记忆召回与用户显式 /skill <id> 不受影响，
 *  界面上如实说明这一范围（不写没有验证过的保证）。
 */
import React, { useState } from "react";
import { Button, Popconfirm, Tag, Tooltip } from "antd";
import { ReloadOutlined } from "@ant-design/icons";
import { useT } from "../i18n/core";
import type { ContextSource, ContextSourceGap } from "../api/core";

const STATUS_CLASS: Record<string, string> = {
  ready: "ok",
  loaded: "ok",
  loaded_truncated: "warn",
  missing: "muted",
  rejected: "bad",
  excluded_sensitive: "muted",
  missing_dependency: "muted",
  disabled: "off",
};

type T = (key: Parameters<ReturnType<typeof useT>>[0], params?: Record<string, string | number>) => string;

/** 未登记的枚举值原样显示（t() 对缺失键返回键名）——不把不认识的状态伪装成已知状态 */
function label(t: T, key: string, raw: string): string {
  const v = t(key);
  return v === key ? raw : v;
}

function statusLabel(t: T, s: ContextSource): string {
  return label(t, `ctx.status.${s.status}`, s.status);
}

function kindLabel(t: T, kind: string): string {
  return label(t, `ctx.kind.${kind}`, kind);
}

function metaOf(t: T, s: ContextSource): string {
  const parts: string[] = [];
  if (s.layer) parts.push(label(t, `ctx.layer.${s.layer}`, s.layer));
  if (typeof s.chars === "number") parts.push(t("ctx.chars", { n: s.chars }));
  return parts.join(" · ");
}

export interface ContextSourcesCardProps {
  sources: ContextSource[];
  /** 整层缺席的文件来源及原因（Core 给出）。清单里一个 file: 都没有时，
   *  这段说明是「为什么」的唯一来源——照实呈现，不在前端推断。 */
  gaps?: ContextSourceGap[];
  /** 首次拉取尚未返回（或失败且从未成功）：显示「暂不可用」而不是空清单 */
  loading: boolean;
  onDisable: (key: string) => void;
  onEnable: (key: string) => void;
  onRefresh: (key: string) => void;
}

export function ContextSourcesCard({ sources, gaps = [], loading, onDisable, onEnable, onRefresh }: ContextSourcesCardProps): React.ReactElement {
  const t = useT();
  const [open, setOpen] = useState(false);
  const disabledCount = sources.filter((s) => s.disabled).length;
  const notReady = sources.filter((s) => !s.disabled && !["ready", "loaded", "loaded_truncated"].includes(s.status)).length;
  return (
    <div className="ctx-inline" data-testid="context-sources-card">
      <button type="button" className="ctx-inline-head" onClick={() => setOpen((v) => !v)} title={t("ctx.toggleHint")}>
        <span className="ctx-inline-title">{t("ctx.title")}</span>
        <span className="ctx-inline-meta">
          {loading ? "…" : `${sources.length}`}
          {disabledCount > 0 ? ` · ${t("ctx.status.disabled")} ${disabledCount}` : ""}
          {notReady > 0 ? ` · ${t("ctx.status.missing_dependency")} ${notReady}` : ""}
          {gaps.length > 0 ? ` · ${t("ctx.gapsHead", { n: gaps.length })}` : ""}
        </span>
        <span className="ctx-inline-caret">{open ? "▾" : "▸"}</span>
      </button>
      {open && (
        <div className="ctx-inline-body">
          {loading && sources.length === 0 ? (
            <div className="ctx-empty">{t("ctx.unavailable")}</div>
          ) : sources.length === 0 ? (
            <div className="ctx-empty">{t("ctx.empty")}</div>
          ) : (
            <ul className="ctx-list">
              {sources.map((s) => {
                const was = s.disabled && s.baseStatus && s.baseStatus !== s.status ? t("ctx.wasStatus", { status: statusLabel(t, { ...s, status: s.baseStatus }) }) : "";
                return (
                  <li key={s.key} className={`ctx-item ctx-${STATUS_CLASS[s.status] ?? "muted"}${s.disabled ? " ctx-off" : ""}`} data-testid={`ctx-item-${s.key}`}>
                    <div className="ctx-item-main">
                      <Tag className="ctx-kind">{kindLabel(t, s.kind)}</Tag>
                      <code className="ctx-key">{s.key}</code>
                      <Tag className={`ctx-status ctx-st-${STATUS_CLASS[s.status] ?? "muted"}`}>{statusLabel(t, s)}</Tag>
                      {was && <span className="ctx-was">{was}</span>}
                      {metaOf(t, s) && <span className="ctx-meta">{metaOf(t, s)}</span>}
                      {s.reason && <div className="ctx-reason">{s.reason}</div>}
                    </div>
                    <div className="ctx-item-actions">
                      <Tooltip title={t("ctx.refresh")}>
                        <Button
                          size="small"
                          type="text"
                          icon={<ReloadOutlined />}
                          aria-label={`${t("ctx.refresh")} ${s.key}`}
                          onClick={() => onRefresh(s.key)}
                        />
                      </Tooltip>
                      {s.disabled ? (
                        <Button size="small" type="link" onClick={() => onEnable(s.key)}>
                          {t("ctx.enable")}
                        </Button>
                      ) : (
                        <Popconfirm
                          title={t("ctx.disable")}
                          description={t("ctx.disableHint")}
                          okText={t("ctx.disable")}
                          onConfirm={() => onDisable(s.key)}
                        >
                          <Button size="small" type="link" danger>
                            {t("ctx.disable")}
                          </Button>
                        </Popconfirm>
                      )}
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
          {gaps.length > 0 && (
            <div className="ctx-gaps" data-testid="context-source-gaps">
              <div className="ctx-gaps-title">{t("ctx.gapsTitle")}</div>
              <ul className="ctx-gap-list">
                {gaps.map((g) => (
                  <li key={`${g.kind}:${g.layer}`} className="ctx-gap" data-testid={`ctx-gap-${g.layer}`}>
                    <Tag className="ctx-kind">{kindLabel(t, g.kind)}</Tag>
                    <span className="ctx-meta">{label(t, `ctx.layer.${g.layer}`, g.layer)}</span>
                    <span className="ctx-reason">{g.reason}</span>
                  </li>
                ))}
              </ul>
            </div>
          )}
          <div className="ctx-note">
            {t("ctx.scopeNote")}
            <br />
            {t("ctx.cliHint")}
          </div>
        </div>
      )}
    </div>
  );
}
