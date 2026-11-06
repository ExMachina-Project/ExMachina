/** 任务页（TasksView，信息架构 2026-11）：系统自动会话的只读视图。
 *
 *  覆盖：定时任务会话（cron，标题 `job-<id>` 或任务自定义 session_title）、
 *  心跳巡检会话（`心跳巡检` / `心跳巡检-<个体>`）。这些会话由系统发起、
 *  无人工输入——页面只读呈现运行产物（无输入框），输入与配置在「自动化」页。
 *
 *  判定口径与 webui/src/session.ts 的 isAutoSessionTitle 一致（保守识别，
 *  cron 自定义标题与用户会话不可区分时归对话页——不影响使用）。
 */
import React, { useCallback, useEffect, useMemo, useState } from "react";
import { Button, Empty, Skeleton, Tag } from "antd";
import { ArrowLeftOutlined, ReloadOutlined, ThunderboltOutlined } from "@ant-design/icons";
import { api } from "../api";
import type { ChatMessage, Session } from "../types";
import { relativeTime } from "../time";
import { useExm } from "../store";
import { useT } from "../i18n/core";
import { StatementList } from "../components/Statements";
import { useIsNarrow } from "../useIsNarrow";
import { isAutoSessionTitle } from "../session";

export function TasksView(): React.ReactElement {
  const t = useT();
  const narrow = useIsNarrow();
  const selectSession = useExm((st) => st.selectSession);

  const [sessions, setSessions] = useState<Session[]>([]);
  const [loading, setLoading] = useState(true);
  const [selected, setSelected] = useState<Session | null>(null);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [msgLoading, setMsgLoading] = useState(false);

  const load = useCallback(async () => {
    try {
      const all = await api.listAllSessions();
      setSessions(all.filter((s) => !s.archivedAt && isAutoSessionTitle(s.title)));
    } catch {
      // 网关不可用：空态
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
    const h = () => void load();
    window.addEventListener("exm:flows-refresh", h);
    return () => window.removeEventListener("exm:flows-refresh", h);
  }, [load]);

  /** 选中任务会话 → 拉消息流（只读） */
  const selId = selected?.id;
  useEffect(() => {
    if (!selId) {
      setMessages([]);
      return;
    }
    setMsgLoading(true);
    void api
      .messages(selId)
      .then(setMessages)
      .catch(() => setMessages([]))
      .finally(() => setMsgLoading(false));
  }, [selId]);

  const openTask = (s: Session) => {
    setSelected(s);
    void selectSession(s.id);
  };

  /** 活跃倒序（最近运行的任务在前） */
  const sorted = useMemo(
    () =>
      [...sessions].sort((a, b) =>
        (b.lastActiveAt ?? b.updatedAt ?? "").localeCompare(a.lastActiveAt ?? a.updatedAt ?? ""),
      ),
    [sessions],
  );

  return (
    <div className={`flows-shell tasks-shell ${narrow && selected ? "thread" : ""}`}>
      <aside className="flows-rail">
        <div className="flows-rail-head">
          <span className="flows-title">{t("nav.tasks")}</span>
          <Tag>{sorted.length}</Tag>
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
          {!loading && sorted.length === 0 && (
            <Empty description={t("tasks.empty")} style={{ marginTop: 80 }} />
          )}
          {sorted.map((s) => {
            const active = selected?.id === s.id;
            return (
              <div
                key={s.id}
                className={`flow-item ${active ? "active" : ""}`}
                onClick={() => openTask(s)}
                role="button"
                tabIndex={0}
                onKeyDown={(e) => e.key === "Enter" && openTask(s)}
              >
                <span className="flow-kind-icon group">
                  <ThunderboltOutlined />
                </span>
                <span className="flow-main">
                  <span className="flow-name">{s.title}</span>
                  <span className="flow-preview">{s.lastMessagePreview || s.title}</span>
                </span>
                <span className="flow-side">
                  <span className="flow-time">{relativeTime(s.lastActiveAt ?? s.updatedAt)}</span>
                </span>
              </div>
            );
          })}
        </div>
      </aside>
      <main className="flows-thread">
        {narrow && selected && (
          <Button
            className="flows-back"
            type="text"
            icon={<ArrowLeftOutlined />}
            onClick={() => setSelected(null)}
          >
            {t("nav.tasks")}
          </Button>
        )}
        {!selected && !narrow && (
          <Empty description={t("tasks.pickOne")} style={{ marginTop: 120 }} />
        )}
        {selected && (
          <>
            <div className="flows-thread-head">
              <span className="flows-thread-name">{selected.title}</span>
              <span className="flows-thread-meta">{t("tasks.autoOnly")}</span>
            </div>
            <div className="flows-messages">
              {msgLoading && <Skeleton active paragraph={{ rows: 4 }} />}
              {!msgLoading && messages.length === 0 && (
                <Empty description={t("flows.noMessages")} style={{ marginTop: 80 }} />
              )}
              {!msgLoading &&
                messages.map((m) => (
                  <div key={m.id} className={`flows-msg ${m.role}`}>
                    <div className="flows-msg-head">
                      <span className="flows-msg-role">
                        {m.role === "user" ? t("tasks.trigger") : m.agentId || t("flows.fromAgent")}
                      </span>
                      <span className="flows-msg-time">{relativeTime(m.createdAt)}</span>
                    </div>
                    <StatementList statements={m.statements} />
                  </div>
                ))}
            </div>
            <div className="flows-readonly">{t("tasks.readonlyHint")}</div>
          </>
        )}
      </main>
    </div>
  );
}
