/** 历史检查点（session-history）：按轮次总览快照，支持回到某轮与从该轮分叉。
 *
 *  入口只在会话管理页行菜单（聊天页消息操作条保持现状）；复用既有
 *  `GET /sessions/:id/snapshots`、`POST /sessions/:id/undo`、`POST /sessions/:id/fork`。
 *  轮次口径 = 第 N 条用户消息（与聊天页一致）。
 */
import React, { useCallback, useEffect, useState } from "react";
import { Button, Modal, Popconfirm, Spin, Tag } from "antd";
import { api } from "../api";
import type { Session, SessionSnapshot } from "../types";
import { useExm } from "../store";
import { useT } from "../i18n/core";
import { relativeTime } from "../time";
import { toastErr, toastOk } from "../feedback";

interface Props {
  /** 目标会话（null = 关闭态） */
  session: Session | null;
  open: boolean;
  onClose: () => void;
  /** 回滚/分叉后通知父级刷新列表 */
  onChanged: () => void;
}

export function SessionHistoryModal({ session, open, onClose, onChanged }: Props): React.ReactElement {
  const t = useT();
  const selectSession = useExm((st) => st.selectSession);
  const requestNav = useExm((st) => st.requestNav);
  const [snaps, setSnaps] = useState<SessionSnapshot[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    if (!session) return;
    setLoading(true);
    setError(null);
    try {
      const list = await api.sessionSnapshots(session.id);
      // 轮次倒序（新→旧）：最近的可回滚轮次在顶部
      setSnaps([...list].sort((a, b) => b.turn - a.turn));
    } catch (e) {
      setError(String(e));
      setSnaps(null);
    } finally {
      setLoading(false);
    }
  }, [session]);

  useEffect(() => {
    if (open) void load();
  }, [open, load]);

  /** 回到该轮：撤销到第 N 轮（消息归档 + 文件检查点联动回滚） */
  const restore = async (snap: SessionSnapshot) => {
    if (!session) return;
    setBusy(true);
    try {
      const r = await api.undoSession(session.id, snap.turn, true);
      toastOk(t("history.restored", { turn: snap.turn, count: r.archived }));
      await load();
      onChanged();
      window.dispatchEvent(new Event("exm:flows-refresh"));
    } catch (e) {
      const msg = String(e);
      toastErr(e, msg.includes("处理中") ? t("history.busy") : t("chat.turn.failed", { err: "" }));
    } finally {
      setBusy(false);
    }
  };

  /** 从该轮分叉：新会话继承前 N 轮历史，原会话不变；成功后直接跳转新分支 */
  const fork = async (snap: SessionSnapshot) => {
    if (!session) return;
    setBusy(true);
    try {
      const f = await api.forkSession(session.id, snap.turn);
      toastOk(t("history.forked"));
      useExm.setState({
        sessions: [{ id: f.id, title: f.title } as never, ...useExm.getState().sessions],
      });
      onChanged();
      window.dispatchEvent(new Event("exm:flows-refresh"));
      void selectSession(f.id);
      requestNav("chat", f.id);
      onClose();
    } catch (e) {
      toastErr(e, t("chat.turn.failed", { err: "" }));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      open={open}
      onCancel={onClose}
      footer={null}
      width={680}
      destroyOnHidden
      title={t("history.title")}
      className="history-modal"
    >
      {session && (
        <div className="history-session mono dim" title={session.title}>
          {session.title}
        </div>
      )}
      <p className="dim history-desc">{t("history.desc")}</p>
      {loading ? (
        <div className="history-status"><Spin /></div>
      ) : error ? (
        <div className="history-status">{t("history.loadFailed", { err: error })}</div>
      ) : !snaps || snaps.length === 0 ? (
        <div className="history-status">{t("history.empty")}</div>
      ) : (
        <div className="history-list">
          {snaps.map((snap) => (
            <div className="history-item" key={snap.turn}>
              <div className="history-item-head">
                <b>{t("history.turn", { turn: snap.turn })}</b>
                <Tag color={snap.status === "done" ? "success" : "error"}>
                  {snap.status === "done" ? t("history.statusDone") : t("history.statusFailed")}
                </Tag>
                <span className="mono dim">{relativeTime(snap.createdAt)}</span>
                <span className="mono dim">{t("history.msgCount", { n: snap.messageCount })}</span>
                {snap.checkpointIds.length > 0 && (
                  <span className="mono dim">
                    {t("history.checkpoints", { n: snap.checkpointIds.length })}
                  </span>
                )}
              </div>
              <div className="history-item-preview">
                {snap.preview || t("history.previewEmpty")}
              </div>
              <div className="history-item-ops">
                <Popconfirm
                  title={t("history.restoreConfirm", { turn: snap.turn })}
                  onConfirm={() => void restore(snap)}
                  disabled={busy}
                >
                  <Button size="small" loading={busy}>{t("history.restore")}</Button>
                </Popconfirm>
                <Button size="small" onClick={() => void fork(snap)} disabled={busy}>
                  {t("history.fork")}
                </Button>
              </div>
            </div>
          ))}
        </div>
      )}
    </Modal>
  );
}

export default SessionHistoryModal;
