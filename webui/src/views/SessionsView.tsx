/**
 * 会话管理页（session-management）：全量会话的检索 / 排序 / 批量操作 / 归档区。
 *
 * 设计原则：**管理职能从聊天左栏解耦到这里**——左栏只做「切换」（最近会话 + 流面板），
 * 批量 / 归档 / 恢复 / 全字段表格只在管理页提供；两处读同一后端事实源，操作即时互通。
 * 表格形态：会话是高密度结构化数据（多字段 + 批量），表格信息效率远高于卡片流。
 */
import React, { useCallback, useEffect, useMemo, useState } from "react";
import {
  Button, Checkbox, Dropdown, Input, Modal, Segmented, Space, Spin, Tooltip, message,
} from "antd";
import {
  CheckOutlined, DeleteOutlined, EditOutlined, ExportOutlined, HistoryOutlined,
  InboxOutlined, PushpinOutlined, ReloadOutlined,
} from "@ant-design/icons";
import { api } from "../api";
import { toastOk, toastErr } from "../feedback";
import type { Session, SessionSearchHit } from "../types";
import type { Channel } from "../api";
import { Highlight, useSessionSearch } from "../components/sessionSearch";
import { SessionHistoryModal } from "../components/SessionHistoryModal";
import { useExm } from "../store";
import { useT, tr } from "../i18n/core";
import { relativeTime } from "../time";

type SortKey = "active" | "created" | "messages";
type KindFilter = "all" | "normal" | "channel";
type LayoutMode = "flat" | "grouped";

/** 平台短标（通道 type → 展示名）：与 FlowRailSection 的 PLATFORM_LABEL 同口径 */
const PLATFORM_LABEL: Record<string, string> = {
  napcat: "QQ",
  qqbot: "QQ 机器人",
  telegram: "Telegram",
  discord: "Discord",
  slack: "Slack",
  matrix: "Matrix",
  bilibili: "B站",
  youtube: "YouTube",
  webhook: "Webhook",
};

/** 行选择集合（id 集合；全选 = 当前过滤视图全部） */
export function SessionsView(): React.ReactElement {
  const t = useT();
  const selectSession = useExm((st) => st.selectSession);
  const requestNav = useExm((st) => st.requestNav);

  const [sessions, setSessions] = useState<Session[]>([]);
  const [channels, setChannels] = useState<Channel[]>([]);
  const [loading, setLoading] = useState(true);
  const [view, setListView] = useState<"main" | "archived">("main");
  // 组织形态（session-management）：分组视图按来源聚簇（控制台 / 通道流）；
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
        (s.lastMessagePreview ?? "").toLowerCase().includes(kw);
      const hitKind =
        kind === "all" ? true : kind === "channel" ? s.title.startsWith("channel:") : !s.title.startsWith("channel:");
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

  /** 来源分组（session-management）：控制台一组 + 每通道一组（组内按流聚簇）。
   *  分组基于 origin 派生字段（channel:{channelId}:{chatKey} / console）；
   *  通道名从通道列表 join（平台标），流名用 origin_label 前段。 */
  const groups = useMemo(() => {
    type FlowGroup = { flowKey: string; label: string; sessions: Session[] };
    type ChanGroup = { channelId: string; title: string; flows: FlowGroup[]; total: number };
    const console_: Session[] = [];
    const byChannel = new Map<string, ChanGroup>();
    for (const s of visible) {
      const origin = s.origin ?? "console";
      if (origin === "console") {
        console_.push(s);
        continue;
      }
      const rest = origin.slice("channel:".length);
      const [channelId] = rest.split(":");
      let cg = byChannel.get(channelId);
      if (!cg) {
        const ch = channels.find((c) => c.id === channelId);
        const plat = ch ? PLATFORM_LABEL[ch.type] ?? ch.type : channelId;
        cg = { channelId, title: `${plat}（${channelId}）`, flows: [], total: 0 };
        byChannel.set(channelId, cg);
      }
      cg.total += 1;
      let fg = cg.flows.find((f) => f.flowKey === origin);
      if (!fg) {
        fg = { flowKey: origin, label: s.originLabel ?? rest, sessions: [] };
        cg.flows.push(fg);
      }
      fg.sessions.push(s);
    }
    // 流/组内排序沿用 visible 的整体序（活跃倒序为主口径），这里只做聚簇
    return { console: console_, channels: [...byChannel.values()] };
  }, [visible, channels]);

  const hitList = useMemo(() => {
    const list = search.hits ?? [];
    return kind === "all"
      ? list
      : list.filter((h) =>
          kind === "channel" ? h.title.startsWith("channel:") : !h.title.startsWith("channel:"),
        );
  }, [search.hits, kind]);

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

  /** 按流/按通道归档旧会话（session-management）：流内非绑定会话批量归档，
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
  };
  /** 打开检索命中：跳会话并定位到命中消息（无片段时只打开会话） */
  const openHit = (h: SessionSearchHit) => {
    void selectSession(h.sessionId);
    requestNav("chat", h.sessionId, h.snippets[0]?.messageId);
  };
  /** 导出（与聊天页同源服务端导出：含审批与全量消息） */
  const exportOne = async (s: Session, format: "markdown" | "json") => {
    try {
      const base = (window as unknown as { __EXM_API_BASE__?: string }).__EXM_API_BASE__ ?? "";
      const key = localStorage.getItem("exm.key") ?? "";
      const resp = await fetch(`${base}/api/sessions/${s.id}/export?format=${format}`, {
        headers: { "X-Auth-Key": key },
      });
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      const blob = await resp.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      const day = new Date().toISOString().slice(0, 10);
      a.download = `${s.title.replace(/[\\/:*?"<>|]/g, "_") || "session"}-${day}.${format === "json" ? "json" : "md"}`;
      a.click();
      URL.revokeObjectURL(url);
      toastOk(t("chat.exportDone"));
    } catch (e) {
      toastErr(e, t("chat.exportFailed", { err: "" }));
    }
  };

  const rowMenu = (s: Session) => ({
    items: [
      { key: "open", icon: <EditOutlined />, label: t("sessions.open") },
      { key: "rename", icon: <EditOutlined />, label: t("common.rename") },
      view === "main" ? { key: "pin", icon: <PushpinOutlined />, label: s.pinned ? t("chat.unpin") : t("chat.pin") } : null,
      { key: "export", icon: <ExportOutlined />, label: t("chat.export") },
      { key: "exportJson", icon: <ExportOutlined />, label: `${t("chat.export")} (JSON)` },
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
      else if (key === "history") setHistoryFor(s);
      else if (key === "archive") void archiveOne(s, true);
      else if (key === "unarchive") void archiveOne(s, false);
      else if (key === "delete") deleteOne(s);
    },
  });

  /** 单行渲染（平铺表 / 分组流内共用）：分组形态下通道会话主显 originLabel
   *  （流名·标签），原始 title 退到 tooltip（寻址/调试仍可查）。 */
  const rowEl = (s: Session) => {
    const isChannel = s.origin ? s.origin !== "console" : s.title.startsWith("channel:");
    const grouped = layout === "grouped" && hasOrigin;
    const displayTitle = grouped && s.originLabel ? s.originLabel : s.title;
    const sel = selected.has(s.id);
    return (
      <div
        key={s.id}
        className={`sessions-tr sessions-row ${sel ? "selected" : ""}`}
        role="row"
        onClick={() => toggleOne(s.id)}
      >
        <span className="sessions-td sessions-td-check">
          <Checkbox checked={sel} onChange={() => toggleOne(s.id)} onClick={(e) => e.stopPropagation()} />
        </span>
        <span className="sessions-td sessions-td-title">
          {s.pinned && <PushpinOutlined className="sessions-pin" />}
          <span className="sessions-title-text" title={s.title}>{displayTitle}</span>
          {(s.lastMessagePreview ?? "") && (
            <span className="sessions-preview">{s.lastMessagePreview}</span>
          )}
        </span>
        <span className="sessions-td sessions-td-kind">
          <span className={`sessions-kind-tag ${isChannel ? "channel" : "normal"}`}>
            {isChannel ? t("sessions.kindChannelShort") : t("sessions.kindNormalShort")}
          </span>
        </span>
        <span className="sessions-td sessions-td-scope mono">{s.groupId}</span>
        <span className="sessions-td sessions-td-active">{relativeTime(s.lastActiveAt ?? s.updatedAt)}</span>
        <span className="sessions-td sessions-td-count mono">{s.messageCount ?? "–"}</span>
        <span className="sessions-td sessions-td-ops" onClick={(e) => e.stopPropagation()}>
          <Tooltip title={view === "archived" ? t("sessions.unarchiveOne") : t("sessions.open")}>
            <Button
              size="small"
              type="text"
              icon={view === "archived" ? <CheckOutlined /> : <EditOutlined />}
              onClick={() => (view === "archived" ? void archiveOne(s, false) : openChat(s))}
            />
          </Tooltip>
          <Dropdown trigger={["click"]} menu={rowMenu(s)}>
            <Button size="small" type="text" icon={<MoreIcon />} />
          </Dropdown>
        </span>
      </div>
    );
  };

  return (
    <div className="sessions-view view-pad">
      <header className="page-head">
        <div className="page-head-row">
          <div className="page-head-main">
            <h2 className="page-title">{t("sessions.title")}</h2>
            <span className="page-en">SESSIONS</span>
          </div>
          <Space>
            <Segmented
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
            <Button icon={<ReloadOutlined />} onClick={() => void load()}>{t("common.refresh")}</Button>
          </Space>
        </div>
        <p className="page-desc">
          {view === "main" ? t("sessions.descMain") : t("sessions.descArchived")}
        </p>
      </header>

      <div className="sessions-toolbar">
        <Input.Search
          className="sessions-search"
          placeholder={t("sessions.searchPlaceholder")}
          title={t("sessions.searchScope")}
          allowClear
          loading={search.loading}
          value={search.query}
          onChange={(e) => search.setQuery(e.target.value)}
        />
        <Segmented
          value={kind}
          onChange={(v) => setKind(v as KindFilter)}
          options={[
            { value: "all", label: t("sessions.kindAll") },
            { value: "normal", label: t("sessions.kindNormal") },
            { value: "channel", label: t("sessions.kindChannel") },
          ]}
        />
        <Segmented
          value={sort}
          onChange={(v) => setSort(v as SortKey)}
          options={[
            { value: "active", label: t("sessions.sortActive") },
            { value: "created", label: t("sessions.sortCreated") },
            { value: "messages", label: t("sessions.sortMessages") },
          ]}
        />
        {view === "main" && hasOrigin && (
          <Segmented
            value={layout}
            onChange={(v) => setLayout(v as LayoutMode)}
            options={[
              { value: "flat", label: t("sessions.layoutFlat") },
              { value: "grouped", label: t("sessions.layoutGrouped") },
            ]}
          />
        )}
        <span className="sessions-count mono">{showSearchPanel ? hitList.length : visible.length}</span>
      </div>

      {selectedCount > 0 && !searching && (
        <div className="sessions-batchbar">
          <span className="mono">{t("sessions.selected", { count: selectedCount })}</span>
          <Space>
            {view === "main" ? (
              <Button size="small" icon={<InboxOutlined />} loading={busy} onClick={() => batch("archive")}>
                {t("sessions.batchArchive", { count: selectedCount })}
              </Button>
            ) : (
              <Button size="small" icon={<CheckOutlined />} loading={busy} onClick={() => batch("unarchive")}>
                {t("sessions.batchUnarchive", { count: selectedCount })}
              </Button>
            )}
            <Button size="small" danger icon={<DeleteOutlined />} loading={busy} onClick={() => batch("delete")}>
              {t("sessions.batchDelete", { count: selectedCount })}
            </Button>
            <Button size="small" type="text" onClick={() => setSelected(new Set())}>
              {t("sessions.clearSelection")}
            </Button>
          </Space>
        </div>
      )}

      {search.error && searching && (
        <div className="search-degraded">{t("sessions.searchDegraded")}</div>
      )}

      {showSearchPanel ? (
        search.loading && search.hits === null ? (
          <div className="sessions-loading"><Spin /></div>
        ) : hitList.length === 0 ? (
          <div className="sessions-empty">{t("sessions.noContentHits")}</div>
        ) : (
          <div className="search-results">
            {hitList.map((h) => {
              const snippet = h.snippets[0];
              return (
                <button key={h.sessionId} type="button" className="search-result" onClick={() => openHit(h)}>
                  <div className="search-result-head">
                    <span className="search-result-title">
                      <Highlight text={h.title} query={search.query} />
                    </span>
                    {h.titleMatch && <span className="search-badge">{t("sessions.searchTitleHit")}</span>}
                    <span className="search-result-meta mono">
                      {h.messageMatches > 0 ? `${t("sessions.searchHitCount", { n: h.messageMatches })} · ` : ""}
                      {relativeTime(h.lastActiveAt)}
                    </span>
                  </div>
                  {snippet && (
                    <div className="search-result-snippet">
                      <Highlight text={snippet.text} query={search.query} />
                    </div>
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
        <div className="sessions-empty">{view === "archived" ? t("sessions.emptyArchived") : t("sessions.emptyMain")}</div>
      ) : layout === "grouped" && hasOrigin ? (
        <div className="sessions-groups">
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
                    <span className="sessions-flow-name" title={fg.flowKey}>{fg.label}</span>
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
        <div className="sessions-table" role="table">
          <div className="sessions-tr sessions-th" role="row">
            <span className="sessions-td sessions-td-check">
              <Checkbox checked={allSelected} indeterminate={!allSelected && selectedCount > 0} onChange={toggleAll} />
            </span>
            <span className="sessions-td sessions-td-title">{t("sessions.colTitle")}</span>
            <span className="sessions-td sessions-td-kind">{t("sessions.colKind")}</span>
            <span className="sessions-td sessions-td-scope">{t("sessions.colScope")}</span>
            <span className="sessions-td sessions-td-active">{t("sessions.colActive")}</span>
            <span className="sessions-td sessions-td-count">{t("sessions.colMessages")}</span>
            <span className="sessions-td sessions-td-ops" />
          </div>
          {visible.map((s) => rowEl(s))}
        </div>
      )}

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
        destroyOnClose
      >
        <Input value={renameText} onChange={(e) => setRenameText(e.target.value)} onPressEnter={() => void doRename()} />
      </Modal>
    </div>
  );
}

/** 行操作「更多」图标（内联避免额外依赖） */
function MoreIcon(): React.ReactElement {
  return <span style={{ fontWeight: 700, letterSpacing: 1 }}>⋯</span>;
}
