/**
 * 会话管理抽屉：从对话与聊天流就地打开，提供全量会话的检索 / 批量操作 / 归档区。
 *
 * 设计原则：管理留在聊天上下文中，不额外占据一个导航页；读取同一后端事实源。
 * 表格形态：会话是高密度结构化数据（多字段 + 批量），表格信息效率远高于卡片流。
 */
import React, { useCallback, useEffect, useMemo, useState } from "react";
import {
  Button, Checkbox, Dropdown, Drawer, Input, Modal, Segmented, Select, Space, Spin, Tooltip, message,
} from "antd";
import {
  ArrowRightOutlined, CheckOutlined, DeleteOutlined, EditOutlined, ExportOutlined, HistoryOutlined,
  InboxOutlined, PushpinOutlined, ReloadOutlined,
} from "@ant-design/icons";
import { api, authHeaders, serverBase } from "../api";
import { toastOk, toastErr } from "../feedback";
import type { Session, SessionSearchHit } from "../types";
import type { Channel } from "../api";
import { Highlight, useSessionSearch } from "./sessionSearch";
import { SessionHistoryModal } from "./SessionHistoryModal";
import { useExm } from "../store";
import { useT, tr } from "../i18n/core";
import { relativeTime } from "../time";
import { isAutoSessionTitle } from "../session";
import "../sessionManagement.css";

type SortKey = "active" | "created" | "messages";
type SessionKind = "normal" | "channel" | "subtask" | "task";
type KindFilter = "all" | SessionKind;
type LayoutMode = "flat" | "grouped";
type SessionIdentity = Pick<Session, "title"> & Partial<Pick<Session, "origin" | "originLabel" | "groupId">>;

/** 平台短标（通道 type → 展示名）：与 FlowRailSection 的 PLATFORM_LABEL 同口径 */
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

/** Display-only classification: never rewrite the title used by APIs and routing. */
export function sessionKind(session: SessionIdentity): SessionKind {
  if (session.origin?.startsWith("channel:") || session.title.startsWith("channel:")) return "channel";
  if (session.title.startsWith("sub:")) return "subtask";
  return isAutoSessionTitle(session.title) ? "task" : "normal";
}

function channelOrigin(session: SessionIdentity): { channelId: string; flow: string } | null {
  const raw = session.origin?.startsWith("channel:") ? session.origin : session.title;
  if (!raw.startsWith("channel:")) return null;
  const rest = raw.slice("channel:".length);
  const split = rest.indexOf(":");
  if (split <= 0 || split === rest.length - 1) return null;
  return { channelId: rest.slice(0, split), flow: rest.slice(split + 1) };
}

function readableIdentifier(id: string): string {
  const name = id.replace(/^single:/, "");
  if (name.toLowerCase() === "exmachina") return "ExMachina";
  return name ? name[0].toUpperCase() + name.slice(1) : name;
}

function readableFlowLabel(raw: string, t: ReturnType<typeof useT>): string {
  const [name, ...labels] = raw.split("·");
  const external = /^external[:_-]?([a-f\d-]{12,})$/i.exec(name.trim());
  const opaque = /^[a-f\d-]{20,}$/i.test(name.trim()) ? name.trim() : null;
  const id = external?.[1] ?? opaque;
  if (!id) return raw.replace(/\s+/g, " ").trim();
  const short = id.length > 16 ? `${id.slice(0, 8)}…${id.slice(-4)}` : id;
  return [t("sessions.externalConversation", { id: short }), ...labels].join(" · ");
}

export function sessionPresentation(session: SessionIdentity, channels: Channel[], t: ReturnType<typeof useT>) {
  const kind = sessionKind(session);
  const channel = channelOrigin(session);
  const platform = channel ? channels.find((c) => c.id === channel.channelId)?.type : undefined;
  // Support both current sub:<agent>:<goal>·<node> and older middle-dot titles.
  const child = /^sub:([^:·]+)[:·]([\s\S]*)$/.exec(session.title);
  const goal = child?.[2].replace(/·(?:[a-f\d]{8}-(?:[a-f\d]{4}-){3}[a-f\d]{12}|T\d+)$/i, "").replace(/\s+/g, " ").trim();
  const flowLabel = channel ? readableFlowLabel(session.originLabel?.trim() || channel.flow, t) : "";
  const title = kind === "subtask"
    ? goal || t("sessions.kindSubtask")
    : session.title.startsWith("channel:") && channel ? flowLabel : session.title;
  const group = session.groupId === "default" ? t("sessions.defaultGroup") : readableIdentifier(session.groupId ?? "");
  return {
    kind,
    title: title || t("sessions.untitled"),
    kindLabel: t(kind === "channel" ? "sessions.kindChannelShort" : kind === "subtask" ? "sessions.kindSubtask"
      : kind === "task" ? "sessions.kindTask" : "sessions.kindConversation"),
    agent: child?.[1],
    source: group || t("sessions.groupConsole"),
    sourceDetail: channel
      ? [readableIdentifier(channel.channelId), platform ? PLATFORM_LABEL[platform] ?? readableIdentifier(platform) : ""].filter(Boolean).join(" · ")
      : t("sessions.groupConsole"),
    flowLabel,
  };
}

/** 行选择集合（id 集合；全选 = 当前过滤视图全部） */
export function SessionManagementDrawer({ open, onClose }: { open: boolean; onClose: () => void }): React.ReactElement {
  const t = useT();
  const selectSession = useExm((st) => st.selectSession);
  const requestNav = useExm((st) => st.requestNav);

  const [sessions, setSessions] = useState<Session[]>([]);
  const [channels, setChannels] = useState<Channel[]>([]);
  const [loading, setLoading] = useState(true);
  const [view, setListView] = useState<"main" | "archived">("main");
  // 组织形态：分组视图按来源聚簇（控制台 / 通道流）；
  // 旧网关不返回 origin 字段 → 分组入口隐藏，锁定平铺（降级口径）。
  const [layout, setLayout] = useState<LayoutMode>("flat");
  const hasOrigin = useMemo(
    () => sessions.length > 0 && sessions.every((s) => typeof s.origin === "string"),
    [sessions],
  );
  // 内容检索（session-search）：主列表走服务端（标题 + 最近 200 条消息），
  // 归档视图保持本地过滤（不调用内容检索接口）。
  const search = useSessionSearch({ enabled: view === "main" });
  const [kind, setKind] = useState<KindFilter>("all");
  const [sort, setSort] = useState<SortKey>("active");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [renaming, setRenaming] = useState<Session | null>(null);
  const [renameText, setRenameText] = useState("");
  const [historyFor, setHistoryFor] = useState<Session | null>(null);
  const sessionsById = useMemo(() => new Map(sessions.map((s) => [s.id, s])), [sessions]);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const list = view === "archived"
        ? await api.listArchivedSessions()
        : await api.listAllSessions();
      setSessions(list);
      // 通道列表（分组视图平台标 join；失败不阻塞主列表）
      try {
        setChannels(await api.listChannels());
      } catch {
        setChannels([]);
      }
    } catch (e) {
      toastErr(e, tr("sessions.loadFailed", { err: "" }));
    } finally {
      setLoading(false);
    }
  }, [view]);

  const reconcileActiveChat = async (remaining: Session[]) => {
    const state = useExm.getState();
    if (view !== "main" || !state.sessionId || remaining.some((session) => session.id === state.sessionId && !session.archivedAt)) return;
    const next = remaining.find((session) => session.groupId === state.activeGroup && !session.archivedAt);
    if (next) await state.selectSession(next.id);
    else await state.newSession();
  };

  useEffect(() => {
    void load();
    // 会话/绑定变动（其它页操作 / run 结束）时刷新——与聊天左栏同一事实源
    const h = () => void load();
    window.addEventListener("exm:flows-refresh", h);
    return () => window.removeEventListener("exm:flows-refresh", h);
  }, [load]);

  /** 本地过滤：关键字（标题 + 预览，服务端检索失败/归档视图的降级口径）→ 类型 → 排序 */
  const visible = useMemo(() => {
    const kw = search.query.trim().toLowerCase();
    let list = sessions.filter((s) => {
      const hitKw =
        !kw ||
        s.title.toLowerCase().includes(kw) ||
        (s.originLabel ?? "").toLowerCase().includes(kw) ||
        s.groupId.toLowerCase().includes(kw) ||
        (s.lastMessagePreview ?? "").toLowerCase().includes(kw);
      const hitKind = kind === "all" || sessionKind(s) === kind;
      return hitKw && hitKind;
    });
    list = [...list].sort((a, b) => {
      if (sort === "messages") return (b.messageCount ?? 0) - (a.messageCount ?? 0);
      if (sort === "created") return b.createdAt.localeCompare(a.createdAt);
      // 置顶优先，再按活跃倒序（与左栏口径一致）
      return (
        Number(b.pinned ?? false) - Number(a.pinned ?? false) ||
        (b.lastActiveAt ?? b.updatedAt).localeCompare(a.lastActiveAt ?? a.updatedAt)
      );
    });
    return list;
  }, [sessions, search.query, kind, sort]);

  /** 服务端检索生效条件：主列表 + 有关键词 + 接口未失败（失败回退本地过滤） */
  const searching = view === "main" && search.query.trim().length > 0;
  const showSearchPanel = searching && !search.error;

  /** 来源分组：控制台一组 + 每通道一组（组内按流聚簇）。
   *  分组基于 origin 派生字段（channel:{channelId}:{chatKey} / console）；
   *  通道名从通道列表 join（平台标），流名用 origin_label 前段。 */
  const groups = useMemo(() => {
    type FlowGroup = { flowKey: string; label: string; sessions: Session[] };
    type ChanGroup = { channelId: string; title: string; flows: FlowGroup[]; total: number };
    const console_: Session[] = [];
    const byChannel = new Map<string, ChanGroup>();
    for (const s of visible) {
      const channel = channelOrigin(s);
      if (!channel) {
        console_.push(s);
        continue;
      }
      const origin = s.origin?.startsWith("channel:") ? s.origin : s.title;
      const { channelId } = channel;
      let cg = byChannel.get(channelId);
      if (!cg) {
        const ch = channels.find((c) => c.id === channelId);
        const plat = ch ? PLATFORM_LABEL[ch.type] ?? readableIdentifier(ch.type) : "";
        cg = { channelId, title: [readableIdentifier(channelId), plat].filter(Boolean).join(" · "), flows: [], total: 0 };
        byChannel.set(channelId, cg);
      }
      cg.total += 1;
      let fg = cg.flows.find((f) => f.flowKey === origin);
      if (!fg) {
        fg = { flowKey: origin, label: s.originLabel || channel.flow, sessions: [] };
        cg.flows.push(fg);
      }
      fg.sessions.push(s);
    }
    // 流/组内排序沿用 visible 的整体序（活跃倒序为主口径），这里只做聚簇
    return { console: console_, channels: [...byChannel.values()] };
  }, [visible, channels]);

  const hitList = useMemo(() => {
    // Keep server content hits (last 200 messages), and let human-readable source
    // labels match locally as well. Never transform the query sent to the server.
    const hits = new Map((search.hits ?? []).map((h) => [h.sessionId, h]));
    const kw = search.query.trim().toLowerCase();
    if (kw && search.hits !== null) {
      for (const s of sessions) {
        if (!hits.has(s.id) && [s.title, s.originLabel, s.groupId].some((text) => text?.toLowerCase().includes(kw))) {
          hits.set(s.id, { sessionId: s.id, title: s.title, titleMatch: s.title.toLowerCase().includes(kw), messageMatches: 0, snippets: [], lastActiveAt: s.lastActiveAt ?? s.updatedAt });
        }
      }
    }
    return [...hits.values()]
      .filter((h) => kind === "all" || sessionKind(sessionsById.get(h.sessionId) ?? h) === kind)
      .sort((a, b) => {
        const sa = sessionsById.get(a.sessionId);
        const sb = sessionsById.get(b.sessionId);
        if (sort === "messages") return (sb?.messageCount ?? b.messageMatches) - (sa?.messageCount ?? a.messageMatches);
        if (sort === "created") return (sb?.createdAt ?? b.lastActiveAt).localeCompare(sa?.createdAt ?? a.lastActiveAt);
        return Number(sb?.pinned ?? false) - Number(sa?.pinned ?? false) || b.lastActiveAt.localeCompare(a.lastActiveAt);
      });
  }, [search.hits, search.query, sessions, sessionsById, kind, sort]);

  const allSelected = visible.length > 0 && visible.every((s) => selected.has(s.id));
  const selectedCount = visible.filter((s) => selected.has(s.id)).length;

  const toggleAll = () => {
    setSelected(allSelected ? new Set() : new Set(visible.map((s) => s.id)));
  };
  const toggleOne = (id: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  /** 批量动作（反馈带作用计数，spec 管理页交互态基线） */
  const batch = (action: "archive" | "unarchive" | "delete") => {
    const ids = visible.filter((s) => selected.has(s.id)).map((s) => s.id);
    if (ids.length === 0) return;
    const doRun = async () => {
      setBusy(true);
      try {
        const r = await api.batchSessions(action, ids);
        if (r.failed.length > 0) {
          message.warning(t("sessions.batchPartial", { ok: r.affected, fail: r.failed.length }));
        } else {
          toastOk(t(`sessions.batchOk.${action}`, { count: r.affected } as never));
        }
        setSelected(new Set());
        await load();
        if (action === "archive" || action === "delete") {
          const remaining = await api.listAllSessions();
          await reconcileActiveChat(remaining);
        }
        window.dispatchEvent(new Event("exm:flows-refresh"));
      } catch (e) {
        toastErr(e, t("sessions.batchFailed", { err: "" }));
      } finally {
        setBusy(false);
      }
    };
    if (action === "delete") {
      // 危险确认：含计数与不可逆说明（spec：确认文案含后果）
      Modal.confirm({
        title: t("sessions.deleteConfirmTitle", { count: ids.length }),
        content: t("sessions.deleteConfirmBody"),
        okButtonProps: { danger: true },
        onOk: doRun,
      });
    } else {
      void doRun();
    }
  };

  /** 按流/按通道归档旧会话：流内非绑定会话批量归档，
   *  活跃绑定会话由服务端豁免并在回执说明。 */
  const archiveByScope = async (scope: "flow" | "channel", channelId: string, chatKey?: string) => {
    setBusy(true);
    try {
      const r = await api.archiveSessionsByScope(scope, channelId, chatKey);
      toastOk(
        t("sessions.flowArchiveOk", { archived: r.archived, skipped: r.skipped } as never),
      );
      setSelected(new Set());
      await load();
      window.dispatchEvent(new Event("exm:flows-refresh"));
    } catch (e) {
      toastErr(e, t("sessions.flowArchiveFailed", { err: "" }));
    } finally {
      setBusy(false);
    }
  };
  const archiveFlow = (channelId: string, flowOrigin: string) => {
    // flowOrigin = channel:{channelId}:{chatKey} → 拆出 chatKey
    const chatKey = flowOrigin.slice(`channel:${channelId}:`.length);
    void archiveByScope("flow", channelId, chatKey);
  };
  const archiveChannel = (channelId: string) => {
    Modal.confirm({
      title: t("sessions.channelArchiveConfirm"),
      content: t("sessions.channelArchiveConfirmBody"),
      onOk: () => archiveByScope("channel", channelId),
    });
  };

  /** 单条归档 / 恢复 / 删除 / 置顶 / 重命名 */
  const archiveOne = async (s: Session, archived: boolean) => {    try {
      await api.batchSessions(archived ? "archive" : "unarchive", [s.id]);
      toastOk(t(archived ? "sessions.archivedOne" : "sessions.unarchivedOne", { name: s.title } as never));
      await load();
      if (archived) {
        const remaining = await api.listAllSessions();
        await reconcileActiveChat(remaining);
      }
      window.dispatchEvent(new Event("exm:flows-refresh"));
    } catch (e) {
      toastErr(e, tr("common.saveFailed", { err: "" }));
    }
  };
  const deleteOne = (s: Session) => {
    Modal.confirm({
      title: t("common.confirmDelete", { name: s.title }),
      content: t("sessions.deleteConfirmBody"),
      okButtonProps: { danger: true },
      onOk: async () => {
        try {
          await api.deleteSession(s.id);
          toastOk(t("sessions.deletedOne", { name: s.title } as never));
          await load();
          const remaining = await api.listAllSessions();
          await reconcileActiveChat(remaining);
          window.dispatchEvent(new Event("exm:flows-refresh"));
        } catch (e) {
          toastErr(e, tr("common.saveFailed", { err: "" }));
        }
      },
    });
  };
  const pinOne = async (s: Session, pinned: boolean) => {
    try {
      await api.pinSession(s.id, pinned);
      setSessions((prev) => prev.map((x) => (x.id === s.id ? { ...x, pinned } : x)));
      window.dispatchEvent(new Event("exm:flows-refresh"));
    } catch (e) {
      toastErr(e, tr("common.saveFailed", { err: "" }));
    }
  };
  const openRename = (s: Session) => {
    setRenaming(s);
    setRenameText(s.title);
  };
  const doRename = async () => {
    if (!renaming) return;
    const title = renameText.trim();
    if (!title) return;
    try {
      await api.renameSession(renaming.id, title);
      setSessions((prev) => prev.map((x) => (x.id === renaming.id ? { ...x, title } : x)));
      toastOk(t("sessions.renamedOne"));
      setRenaming(null);
      window.dispatchEvent(new Event("exm:flows-refresh"));
    } catch (e) {
      toastErr(e, tr("common.saveFailed", { err: "" }));
    }
  };
  /** 打开：跳聊天页并选中该会话 */
  const openChat = (s: Session) => {
    void selectSession(s.id);
    requestNav("chat", s.id);
    onClose();
  };
  /** 打开检索命中：跳会话并定位到命中消息（无片段时只打开会话） */
  const openHit = (h: SessionSearchHit) => {
    void selectSession(h.sessionId);
    requestNav("chat", h.sessionId, h.snippets[0]?.messageId);
    onClose();
  };
  /** 导出（与聊天页同源服务端导出：含审批与全量消息） */
  const exportOne = async (s: Session, format: "markdown" | "json") => {
    try {
      const resp = await fetch(`${serverBase()}/api/sessions/${s.id}/export?format=${format}`, { headers: authHeaders() });
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      const blob = await resp.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      const day = new Date().toISOString().slice(0, 10);
      a.download = `${s.title.replace(/[\\/:*?"<>|]/g, "_") || "session"}-${day}.${format === "json" ? "json" : "md"}`;
      a.click();
      window.setTimeout(() => URL.revokeObjectURL(url), 1000);
      toastOk(t("chat.exportDone"));
    } catch (e) {
      toastErr(e, t("chat.exportFailed", { err: "" }));
    }
  };

  /** 训练轨迹导出（按轮次切分、默认剥离思维链；JSONL 每轮一行） */
  const exportTrajectory = async (s: Session) => {
    try {
      const resp = await fetch(`${serverBase()}/api/sessions/${s.id}/trajectory?format=jsonl`, { headers: authHeaders() });
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      const blob = await resp.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      const day = new Date().toISOString().slice(0, 10);
      a.download = `${s.title.replace(/[\\/:*?"<>|]/g, "_") || "session"}-trajectory-${day}.jsonl`;
      a.click();
      window.setTimeout(() => URL.revokeObjectURL(url), 1000);
      toastOk(t("sessions.trajectoryDone"));
    } catch (e) {
      toastErr(e, t("chat.exportFailed", { err: "" }));
    }
  };

  const rowMenu = (s: Session) => ({
    items: [
      { key: "open", icon: <ArrowRightOutlined />, label: t("sessions.open") },
      { key: "rename", icon: <EditOutlined />, label: t("common.rename") },
      view === "main" ? { key: "pin", icon: <PushpinOutlined />, label: s.pinned ? t("chat.unpin") : t("chat.pin") } : null,
      { key: "export", icon: <ExportOutlined />, label: t("chat.export") },
      { key: "exportJson", icon: <ExportOutlined />, label: `${t("chat.export")} (JSON)` },
      { key: "exportTrajectory", icon: <ExportOutlined />, label: t("sessions.exportTrajectory") },
      { key: "history", icon: <HistoryOutlined />, label: t("sessions.history") },
      { type: "divider" as const },
      view === "main"
        ? { key: "archive", icon: <InboxOutlined />, label: t("sessions.archiveOne") }
        : { key: "unarchive", icon: <CheckOutlined />, label: t("sessions.unarchiveOne") },
      { key: "delete", icon: <DeleteOutlined />, label: t("chat.delete"), danger: true },
    ].filter((x): x is NonNullable<typeof x> => x !== null),
    onClick: ({ key }: { key: string }) => {
      if (key === "open") openChat(s);
      else if (key === "rename") openRename(s);
      else if (key === "pin") void pinOne(s, !s.pinned);
      else if (key === "export") void exportOne(s, "markdown");
      else if (key === "exportJson") void exportOne(s, "json");
      else if (key === "exportTrajectory") void exportTrajectory(s);
      else if (key === "history") setHistoryFor(s);
      else if (key === "archive") void archiveOne(s, true);
      else if (key === "unarchive") void archiveOne(s, false);
      else if (key === "delete") deleteOne(s);
    },
  });

  /** Both layouts use readable labels; raw titles remain inspectable and unchanged. */
  const rowEl = (s: Session) => {
    const display = sessionPresentation(s, channels, t);
    const sel = selected.has(s.id);
    return (
      <div
        key={s.id}
        className={`sessions-tr sessions-row ${sel ? "selected" : ""}`}
        data-session-id={s.id}
        data-session-kind={display.kind}
        data-raw-title={s.title}
        role="row"
        aria-selected={sel}
        onClick={() => toggleOne(s.id)}
      >
        <span className="sessions-td sessions-td-check" role="cell">
          <Checkbox aria-label={t("sessions.selectSession", { name: display.title })} checked={sel} onChange={() => toggleOne(s.id)} onClick={(e) => e.stopPropagation()} />
        </span>
        <span className="sessions-td sessions-td-title" role="cell">
          <span className="sessions-title-line">
            {s.pinned && <PushpinOutlined className="sessions-pin" />}
            <button
              className="sessions-title-text sessions-title-button"
              type="button"
              title={s.title}
              aria-label={`${t("sessions.open")}: ${display.title}`}
              onClick={(e) => { e.stopPropagation(); openChat(s); }}
            >{display.title}</button>
          </span>
          <span className="sessions-row-details">
            <span className={`sessions-kind-tag sessions-row-kind-inline ${display.kind}`}>{display.kindLabel}</span>
            {display.agent && <span className="sessions-row-agent" title={display.agent}>{display.agent}</span>}
            <span className="sessions-row-source-inline" title={[s.groupId, s.origin].filter(Boolean).join(" · ")}>{display.source} · {display.sourceDetail}</span>
            {s.lastMessagePreview && <span className="sessions-preview">{s.lastMessagePreview}</span>}
          </span>
        </span>
        <span className="sessions-td sessions-td-kind" role="cell">
          <span className={`sessions-kind-tag ${display.kind}`}>{display.kindLabel}</span>
        </span>
        <span className="sessions-td sessions-td-scope" role="cell" title={[s.groupId, s.origin].filter(Boolean).join(" · ")}>
          <span className="sessions-source-name">{display.source}</span>
          <span className="sessions-source-detail">{display.sourceDetail}</span>
        </span>
        <span className="sessions-td sessions-td-active" role="cell" title={s.lastActiveAt ?? s.updatedAt}>{relativeTime(s.lastActiveAt ?? s.updatedAt)}</span>
        <span className="sessions-td sessions-td-count mono" role="cell">{s.messageCount ?? "–"}</span>
        <span className="sessions-td sessions-td-ops" role="cell" onClick={(e) => e.stopPropagation()}>
          <Tooltip title={view === "archived" ? t("sessions.unarchiveOne") : t("sessions.open")}>
            <Button
              size="small"
              type="text"
              aria-label={view === "archived" ? t("sessions.unarchiveOne") : t("sessions.open")}
              icon={view === "archived" ? <CheckOutlined /> : <ArrowRightOutlined />}
              onClick={() => (view === "archived" ? void archiveOne(s, false) : openChat(s))}
            />
          </Tooltip>
          <Dropdown trigger={["click"]} menu={rowMenu(s)}>
            <Button size="small" type="text" aria-label={t("sessions.moreActions", { name: display.title })} icon={<MoreIcon />} />
          </Dropdown>
        </span>
      </div>
    );
  };

  const tableHead = (
    <div className="sessions-tr sessions-th" role="row">
      <span className="sessions-td sessions-td-check" role="columnheader">
        <Checkbox aria-label={t("sessions.selectAll")} checked={allSelected} indeterminate={!allSelected && selectedCount > 0} onChange={toggleAll} />
      </span>
      <span className="sessions-td sessions-td-title" role="columnheader">{t("sessions.colTitle")}</span>
      <span className="sessions-td sessions-td-kind" role="columnheader">{t("sessions.colKind")}</span>
      <span className="sessions-td sessions-td-scope" role="columnheader">{t("sessions.colSource")}</span>
      <span className="sessions-td sessions-td-active" role="columnheader">{t("sessions.colActive")}</span>
      <span className="sessions-td sessions-td-count" role="columnheader">{t("sessions.colMessages")}</span>
      <span className="sessions-td sessions-td-ops" role="columnheader" aria-label={t("sessions.actionsLabel")} />
    </div>
  );

  return (
    <Drawer
      open={open}
      title={<span className="session-manager-title">{t("sessions.title")}</span>}
      onClose={onClose}
      placement="right"
      width="min(1040px, 100vw)"
      className="session-manager-drawer"
      classNames={{ body: "session-manager-body" }}
      destroyOnHidden
    >
      <div className="session-manager-content">
        <div className="sessions-controls" data-testid="sessions-controls">
          <p className="sessions-drawer-description">{view === "main" ? t("sessions.managementHint") : t("sessions.descArchived")}</p>
          <div className="sessions-toolbar">
            <Input.Search
              className="sessions-search"
              placeholder={view === "main" ? t("sessions.searchPlaceholder") : t("sessions.archiveSearchPlaceholder")}
              aria-label={view === "main" ? t("sessions.searchScope") : t("sessions.archiveSearchScope")}
              title={view === "main" ? t("sessions.searchScope") : t("sessions.archiveSearchScope")}
              allowClear
              loading={search.loading}
              value={search.query}
              onChange={(e) => search.setQuery(e.target.value)}
            />
            <Segmented
              className="sessions-view-switch"
              aria-label={t("sessions.viewLabel")}
              value={view}
              onChange={(v) => {
                setListView(v as "main" | "archived");
                setSelected(new Set());
              }}
              options={[
                { value: "main", label: t("sessions.viewMain") },
                { value: "archived", label: t("sessions.viewArchived") },
              ]}
            />
            <Tooltip title={t("common.refresh")}>
              <Button aria-label={t("common.refresh")} icon={<ReloadOutlined />} loading={loading} onClick={() => void load()} />
            </Tooltip>
          </div>
          <div className="sessions-filterbar">
            <Segmented
              className="sessions-kind-filter"
              aria-label={t("sessions.colKind")}
              value={kind}
              onChange={(v) => setKind(v as KindFilter)}
              options={[
                { value: "all", label: t("sessions.kindAll") },
                { value: "normal", label: t("sessions.kindConversation") },
                { value: "channel", label: t("sessions.kindChannel") },
                { value: "subtask", label: t("sessions.kindSubtask") },
                { value: "task", label: t("sessions.kindTask") },
              ]}
            />
            <div className="sessions-view-controls">
              <label className="sessions-control">
                <span>{t("sessions.sortLabel")}</span>
                <Select
                  className="sessions-sort-select"
                  aria-label={t("sessions.sortLabel")}
                  value={sort}
                  onChange={setSort}
                  popupMatchSelectWidth={false}
                  options={[
                    { value: "active", label: t("sessions.sortActive") },
                    { value: "created", label: t("sessions.sortCreated") },
                    { value: "messages", label: t("sessions.sortMessages") },
                  ]}
                />
              </label>
              {hasOrigin && (
                <label className="sessions-control">
                  <span>{t("sessions.layoutLabel")}</span>
                  <Select
                    className="sessions-layout-select"
                    aria-label={t("sessions.layoutLabel")}
                    value={layout}
                    onChange={setLayout}
                    disabled={showSearchPanel}
                    popupMatchSelectWidth={false}
                    options={[
                      { value: "flat", label: t("sessions.layoutFlat") },
                      { value: "grouped", label: t("sessions.layoutGrouped") },
                    ]}
                  />
                </label>
              )}
            </div>
          </div>
          {search.error && searching && <div className="search-degraded" role="status">{t("sessions.searchDegraded")}</div>}
        </div>

      <div className="sessions-list-scroll" data-testid="sessions-list-scroll" role="region" aria-label={t("sessions.listLabel")} aria-busy={showSearchPanel ? search.loading : loading} tabIndex={0}>
      {showSearchPanel ? (
        search.loading && search.hits === null ? (
          <div className="sessions-loading"><Spin /></div>
        ) : hitList.length === 0 ? (
          <div className="sessions-empty">{t("sessions.noContentHits")}</div>
        ) : (
          <div className="search-results">
            {hitList.map((h) => {
              const snippet = h.snippets[0];
              const display = sessionPresentation(sessionsById.get(h.sessionId) ?? h, channels, t);
              const showRawMatch = h.titleMatch && display.title !== h.title && !display.title.toLowerCase().includes(search.query.trim().toLowerCase());
              return (
                <button key={h.sessionId} type="button" className="search-result" data-session-id={h.sessionId} data-session-kind={display.kind} data-raw-title={h.title} title={h.title} onClick={() => openHit(h)}>
                  <span className="search-result-head">
                    <span className="search-result-title">
                      <Highlight text={display.title} query={search.query} />
                    </span>
                    <span className={`sessions-kind-tag ${display.kind}`}>{display.kindLabel}</span>
                    {h.titleMatch && <span className="search-badge">{t("sessions.searchTitleHit")}</span>}
                    {!h.titleMatch && !h.messageMatches && <span className="search-badge">{t("sessions.searchSourceHit")}</span>}
                    <span className="search-result-meta">
                      {h.messageMatches > 0 ? `${t("sessions.searchHitCount", { n: h.messageMatches })} · ` : ""}
                      {relativeTime(h.lastActiveAt)}
                    </span>
                  </span>
                  {showRawMatch && <span className="sessions-search-raw mono"><Highlight text={h.title} query={search.query} /></span>}
                  <span className="sessions-search-source">{[display.agent, display.source, display.sourceDetail].filter(Boolean).join(" · ")}</span>
                  {snippet && (
                    <span className="search-result-snippet">
                      <Highlight text={snippet.text} query={search.query} />
                    </span>
                  )}
                  <span className="search-result-open">{t("sessions.searchOpen")} →</span>
                </button>
              );
            })}
          </div>
        )
      ) : loading ? (
        <div className="sessions-loading"><Spin /></div>
      ) : visible.length === 0 ? (
        <div className="sessions-empty">{sessions.length > 0 ? t("sessions.emptyFiltered") : view === "archived" ? t("sessions.emptyArchived") : t("sessions.emptyMain")}</div>
      ) : layout === "grouped" && hasOrigin ? (
        <div className="sessions-table sessions-groups" role="table" aria-label={t("sessions.listLabel")}>
          {tableHead}
          {groups.console.length > 0 && (
            <section className="sessions-group">
              <header className="sessions-group-head">
                <span className="sessions-group-title">{t("sessions.groupConsole")}</span>
                <span className="mono sessions-group-count">{groups.console.length}</span>
              </header>
              {groups.console.map((s) => rowEl(s))}
            </section>
          )}
          {groups.channels.map((cg) => (
            <section className="sessions-group" key={cg.channelId}>
              <header className="sessions-group-head">
                <span className="sessions-group-title">{cg.title}</span>
                <span className="mono sessions-group-count">{cg.total}</span>
                {view === "main" && (
                  <Button
                    size="small"
                    type="text"
                    icon={<InboxOutlined />}
                    title={t("sessions.channelArchiveTip")}
                    onClick={() => archiveChannel(cg.channelId)}
                  >
                    {t("sessions.archiveChannel")}
                  </Button>
                )}
              </header>
              {cg.flows.map((fg) => (
                <div className="sessions-flow" key={fg.flowKey}>
                  <div className="sessions-flow-head">
                    <span className="sessions-flow-name" title={fg.flowKey}>{readableFlowLabel(fg.label, t)}</span>
                    <span className="mono sessions-flow-count">{fg.sessions.length}</span>
                    {view === "main" && (
                      <Button
                        size="small"
                        type="text"
                        icon={<InboxOutlined />}
                        title={t("sessions.archiveFlowTip")}
                        onClick={() => void archiveFlow(cg.channelId, fg.flowKey)}
                      >
                        {t("sessions.archiveFlow")}
                      </Button>
                    )}
                  </div>
                  {fg.sessions.map((s) => rowEl(s))}
                </div>
              ))}
            </section>
          ))}
        </div>
      ) : (
        <div className="sessions-table" role="table" aria-label={t("sessions.listLabel")}>
          {tableHead}
          {visible.map((s) => rowEl(s))}
        </div>
      )}
      </div>

      <footer className="sessions-footer" data-testid="sessions-footer">
        {selectedCount > 0 && !searching && (
          <div className="sessions-batchbar">
            <span className="sessions-selected-count" aria-live="polite">{t("sessions.selected", { count: selectedCount })}</span>
            <Space wrap>
              {view === "main" ? (
                <Button size="small" aria-label={t("sessions.batchArchive", { count: selectedCount })} icon={<InboxOutlined />} loading={busy} onClick={() => batch("archive")}>
                  {t("sessions.batchArchive", { count: selectedCount })}
                </Button>
              ) : (
                <Button size="small" aria-label={t("sessions.batchUnarchive", { count: selectedCount })} icon={<CheckOutlined />} loading={busy} onClick={() => batch("unarchive")}>
                  {t("sessions.batchUnarchive", { count: selectedCount })}
                </Button>
              )}
              <Button size="small" aria-label={t("sessions.batchDelete", { count: selectedCount })} danger icon={<DeleteOutlined />} loading={busy} onClick={() => batch("delete")}>
                {t("sessions.batchDelete", { count: selectedCount })}
              </Button>
              <Button size="small" type="text" onClick={() => setSelected(new Set())}>{t("sessions.clearSelection")}</Button>
            </Space>
          </div>
        )}
        <div className="sessions-footer-status">
          <span className="sessions-count" aria-live="polite">{t("sessions.resultCount", { count: showSearchPanel ? hitList.length : visible.length })}</span>
          <span className="sessions-footer-hint">{view === "main" ? t("sessions.searchScope") : t("sessions.archiveSearchScope")}</span>
          <Button size="small" onClick={onClose}>{t("common.close")}</Button>
        </div>
      </footer>

      <SessionHistoryModal
        session={historyFor}
        open={!!historyFor}
        onClose={() => setHistoryFor(null)}
        onChanged={() => void load()}
      />

      <Modal
        open={!!renaming}
        title={t("common.rename")}
        okText={t("common.save")}
        cancelText={t("common.cancel")}
        onOk={() => void doRename()}
        onCancel={() => setRenaming(null)}
        destroyOnHidden
      >
        <Input value={renameText} onChange={(e) => setRenameText(e.target.value)} onPressEnter={() => void doRename()} />
      </Modal>
      </div>
    </Drawer>
  );
}

/** 行操作「更多」图标（内联避免额外依赖） */
function MoreIcon(): React.ReactElement {
  return <span style={{ fontWeight: 700, letterSpacing: 1 }}>⋯</span>;
}
