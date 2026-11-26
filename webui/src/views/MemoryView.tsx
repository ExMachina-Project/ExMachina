/**
 * 记忆视图：深层记忆（条目/检索/固定/维护）+ 个体可靠性统计
 * 数据与操作契约见 docs/08 §6
 */
import React, { useCallback, useEffect, useMemo, useState } from "react";
import {
  Button,
  Card,
  Checkbox,
  Descriptions,
  Input,
  List,
  message,
  Modal,
  Popconfirm,
  Select,
  Space,
  Table,
  Tag,
  Tooltip,
} from "antd";
import { BrandEmpty } from "../components/BrandEmpty";
import {
  DeleteOutlined,
  EditOutlined,
  HistoryOutlined,
  PushpinOutlined,
  ReloadOutlined,
  SearchOutlined,
  UndoOutlined,
} from "@ant-design/icons";
import {
  api,
  type LlmProfile,
  type MemoryEntry,
  type MemoryHistoryRow,
  type MemoryStats,
  type RecallHit,
} from "../api";
import { PageHeader } from "../components/PageHeader";
import { useExm } from "../store";
import { tr, useT } from "../i18n/core";

function MdEditor(): React.ReactElement {
  const t = useT();
  const [content, setContent] = useState("");
  const [path, setPath] = useState("");
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    void (async () => {
      try {
        const key = localStorage.getItem("exm.key") ?? "";
        const resp = await fetch("/api/memory/md", {
          headers: key ? { "X-Auth-Key": key } : {},
        });
        if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
        const data = (await resp.json()) as { content?: string; path?: string; deepEnabled?: boolean };
        if (typeof data.content !== "string") throw new Error("Invalid memory response");
        setContent(data.content);
        setPath(typeof data.path === "string" ? data.path : "");
      } catch (e) {
        message.error(tr("memory.mdReadFailed", { err: String(e) }));
      }
    })();
  }, []);

  const save = async () => {
    setSaving(true);
    try {
      const key = localStorage.getItem("exm.key") ?? "";
      const resp = await fetch("/api/memory/md", {
        method: "PUT",
        headers: { "Content-Type": "application/json", ...(key ? { "X-Auth-Key": key } : {}) },
        body: JSON.stringify({ content }),
      });
      const data = (await resp.json()) as { ok?: boolean; compacting?: boolean; error?: string };
      setDirty(false);
      if (data.compacting) {
        message.info(t("memory.mdCompacting"));
      } else if (data.error) {
        message.error(data.error);
      } else {
        message.success(t("memory.mdSaved"));
      }
    } catch (e) {
      message.error(t("common.saveFailed", { err: String(e) }));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="memory-wrap">
      <PageHeader
        en="MEMORY GRAPH"
        title={t("nav.memory")}
        desc={t("memory.mdDesc")}
      />
      <p className="pane-hint" role="note">{t("memory.graphPending")}</p>
      <Card
        size="small"
        title={t("memory.mdCard")}
        extra={
          <Button type="primary" size="small" loading={saving} disabled={!dirty} onClick={() => void save()}>
            {t("common.save")}
          </Button>
        }
      >
        <div className="dim" style={{ marginBottom: 8 }}>
          {t("memory.mdHint", { path: path || "memory.md", n: content.length })}
        </div>
        <Input.TextArea
          value={content}
          onChange={(e) => {
            setContent(e.target.value);
            setDirty(true);
          }}
          rows={22}
          style={{ fontFamily: "inherit" }}
        />
      </Card>
    </div>
  );
}

// 记忆类别 → 资产灰阶语汇：decision（裁决）保留品牌紫点缀，其余中性 mono 标签
const KIND_COLORS: Record<string, string | undefined> = {
  fact: undefined,
  decision: "purple",
  preference: undefined,
  evidence: undefined,
  digest: undefined,
  lesson: "red",
  stat: undefined,
};

export function MemoryView(): React.ReactElement {
  const t = useT();
  const deepEnabled = useExm((s) => s.config?.memory?.enabled !== false);
  const memoryVersion = useExm((s) => s.memoryVersion);
  const agents = useExm((s) => s.agents);
  const groups = useExm((s) => s.groups);
  const activeGroup = useExm((s) => s.activeGroup);
  const [entries, setEntries] = useState<MemoryEntry[]>([]);
  const [stats, setStats] = useState<MemoryStats | null>(null);
  // 已遗忘清单（capability-parity 6.6）：条目从常规列表消失后，「撤销遗忘」只能从这里进
  const [forgotten, setForgotten] = useState<MemoryEntry[]>([]);
  // 版本历史弹窗：某条目的版本链 + 当前生效版本 + 是否已遗忘
  const [historyFor, setHistoryFor] = useState<MemoryEntry | null>(null);
  const [history, setHistory] = useState<MemoryHistoryRow[]>([]);
  const [historyCurrent, setHistoryCurrent] = useState<number | null>(null);
  const [historyForgotten, setHistoryForgotten] = useState(false);
  const [historyLoading, setHistoryLoading] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  // 编辑弹窗（T3）：仅提交用户改过的字段，空标题/正文视为未提供
  const [editing, setEditing] = useState<MemoryEntry | null>(null);
  const [editTitle, setEditTitle] = useState("");
  const [editBody, setEditBody] = useState("");
  const [savingEdit, setSavingEdit] = useState(false);
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<RecallHit[] | null>(null);
  // 检索调试视图（8.4）：显示命中词项明细与语义分数
  const [debugRecall, setDebugRecall] = useState(false);
  const [kindFilter, setKindFilter] = useState<string | undefined>(undefined);
  const [agentFilter, setAgentFilter] = useState<string | undefined>(undefined);
  // 语义检索目标（档案ID，或 档案ID/模型名）；空 = 仅词项召回
  const semanticCfg = useExm((s) => s.config?.memory?.semanticModel ?? "");
  const [profiles, setProfiles] = useState<LlmProfile[]>([]);
  const [semanticDraft, setSemanticDraft] = useState(semanticCfg);
  useEffect(() => setSemanticDraft(semanticCfg), [semanticCfg]);
  useEffect(() => {
    void (async () => {
      try {
        setProfiles((await api.llmProfiles()).profiles);
      } catch {
        /* 提供商未加载时保持空列表 */
      }
    })();
  }, []);
  const semanticOptions = useMemo(() => {
    const opts = [{ value: "", label: t("memory.noSemantic") }];
    for (const p of profiles) {
      opts.push({ value: p.id, label: `${p.name}${p.model ? ` · ${p.model}` : ""}` });
    }
    return opts;
  }, [profiles, t]);
  const saveSemantic = async () => {
    try {
      await api.putConfig({ memory: { semanticModel: semanticDraft } });
      message.success(semanticDraft ? t("memory.semanticOn") : t("memory.semanticOff"));
    } catch (e) {
      message.error(t("common.saveFailed", { err: String(e) }));
    }
  };

  const reload = useCallback(async () => {
    const [list, st, fg] = await Promise.all([
      api.listMemory({ kind: kindFilter, agent: agentFilter, limit: 60 }),
      api.memoryStats(),
      api.forgottenMemory().then((r) => r.entries),
    ]);
    setEntries(list);
    setStats(st);
    setForgotten(fg);
  }, [kindFilter, agentFilter]);

  // ---- 版本链（capability-parity 6.5 / 6.6）----
  // 回滚/撤销遗忘都与 CLI、面板共用后端同一版本事实源；回执带新版本号。
  const bumpStoreMemory = useExm((s) => s.bumpMemory);

  const openHistory = async (e: MemoryEntry) => {
    setHistoryFor(e);
    setHistory([]);
    setHistoryCurrent(null);
    setHistoryForgotten(false);
    setHistoryLoading(true);
    try {
      const r = await api.memoryVersions(e.id);
      setHistory(r.versions);
      setHistoryCurrent(r.current);
      setHistoryForgotten(r.forgotten);
    } catch (err) {
      message.error(tr("common.loadFailed", { err: String(err) }));
      setHistoryFor(null);
    } finally {
      setHistoryLoading(false);
    }
  };

  const doRollback = async (id: string, to: number) => {
    setBusyId(id);
    try {
      const r = await api.rollbackMemory(id, to);
      message.success(tr("memory.rollbackDone", { to, v: r.version }));
      const [h] = await Promise.all([api.memoryVersions(id), reload()]);
      setHistory(h.versions);
      setHistoryCurrent(h.current);
      setHistoryForgotten(h.forgotten);
      bumpStoreMemory();
      // 弹窗标题跟着当前生效版本走（回滚后内容变了）
      const cur = h.versions.find((row) => row.version === h.current);
      setHistoryFor((prev) =>
        prev && cur ? { ...prev, title: cur.title, body: cur.body, tags: cur.tags, version: r.version } : prev,
      );
      return r.version;
    } catch (err) {
      message.error(tr("common.saveFailed", { err: String(err) }));
      return null;
    } finally {
      setBusyId(null);
    }
  };

  const doRestore = async (e: MemoryEntry) => {
    setBusyId(e.id);
    try {
      const r = await api.restoreMemory(e.id);
      message.success(tr("memory.restoreDone", { title: r.title, v: r.version }));
      await reload();
      bumpStoreMemory();
    } catch (err) {
      message.error(tr("common.saveFailed", { err: String(err) }));
    } finally {
      setBusyId(null);
    }
  };

  // 遗忘回执现在提示可撤销——入口在「已遗忘的记忆」卡片
  const doForget = async (id: string) => {
    setBusyId(id);
    try {
      await api.forgetMemory(id);
      await reload();
      bumpStoreMemory();
    } catch (err) {
      message.error(tr("common.saveFailed", { err: String(err) }));
    } finally {
      setBusyId(null);
    }
  };

  const openEdit = (e: MemoryEntry) => {
    setEditing(e);
    setEditTitle(e.title);
    setEditBody(e.body);
  };

  const saveEdit = async () => {
    if (!editing) return;
    const patch: { title?: string; body?: string } = {};
    if (editTitle.trim() && editTitle !== editing.title) patch.title = editTitle.trim();
    if (editBody.trim() && editBody !== editing.body) patch.body = editBody;
    if (Object.keys(patch).length === 0) {
      setEditing(null);
      return;
    }
    setSavingEdit(true);
    try {
      await api.updateMemory(editing.id, patch);
      message.success(t("memory.updated", { id: editing.id }));
      setEditing(null);
      void reload();
    } catch (err) {
      message.error(t("common.saveFailed", { err: String(err) }));
    } finally {
      setSavingEdit(false);
    }
  };

  useEffect(() => {
    void reload();
  }, [reload, memoryVersion]);

  if (!deepEnabled) {
    return <MdEditor />;
  }

  return (
    <div className="memory-wrap">
      <PageHeader
        en="MEMORY GRAPH"
        title={t("nav.memory")}
        desc={t("memory.desc")}
      />
      <p className="pane-hint" role="note">{t("memory.graphPending")}</p>
      <div className="memory-stats" aria-label={t("memory.statTotal")}>
        {[
          [t("memory.statTotal"), stats?.memory?.total ?? 0],
          [t("memory.statShared"), stats?.memory?.shared ?? 0],
          [t("memory.statIndividual"), stats?.memory?.individual ?? 0],
          [t("memory.statPinned"), stats?.memory?.pinned ?? 0],
        ].map(([label, value]) => <div className="memory-stat-item" key={label}><span>{label}</span><strong>{value}</strong></div>)}
      </div>

      <Card
        size="small"
        className="memory-card"
        title={t("memory.semanticCard")}
        extra={
          <Button size="small" type="primary" onClick={() => void saveSemantic()}>
            {t("common.save")}
          </Button>
        }
      >
        <div className="model-kv">
          <span className="k">{t("memory.embedModel")}</span>
          <Select
            size="small"
            className="memory-semantic-select"
            style={{ width: "min(420px, 100%)" }}
            value={semanticDraft}
            onChange={setSemanticDraft}
            options={semanticOptions}
          />
        </div>
        <div className="pane-hint" style={{ marginTop: 8 }}>
          {t("memory.semanticHint")}
        </div>
      </Card>

      <Card
        size="small"
        className="memory-card"
        title={t("memory.searchCard")}
        extra={
          <Space wrap size={[6, 6]}>
            <Button size="small" icon={<ReloadOutlined />} onClick={() => void reload()}>
              {t("common.refresh")}
            </Button>
            <Button
              size="small"
              onClick={async () => {
                await api.reindexMemory();
                message.success(t("memory.reindexed"));
                void reload();
              }}
            >
              {t("memory.reindex")}
            </Button>
            <Button
              size="small"
              onClick={async () => {
                await api.decayMemory();
                message.success(t("memory.decayed"));
                void reload();
              }}
            >
              {t("memory.decay")}
            </Button>
            <Button
              size="small"
              onClick={async () => {
                await api.renderMemory();
                message.success(t("memory.rendered"));
              }}
            >
              {t("memory.render")}
            </Button>
          </Space>
        }
      >
        <Space.Compact style={{ width: "100%" }}>
          <Input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t("memory.searchPh")}
            onPressEnter={async () => {
              const r = await api.searchMemory(query, 8, agentFilter);
              setHits(r.hits);
            }}
          />
          <Button
            type="primary"
            icon={<SearchOutlined />}
            onClick={async () => {
              const r = await api.searchMemory(query, 8, agentFilter);
              setHits(r.hits);
            }}
          >
            {t("memory.search")}
          </Button>
        </Space.Compact>
        <div className="memory-scope-hint">
          {t("memory.scope")}{agentFilter ? t("memory.scopeAgent", { id: agentFilter }) : t("memory.scopeGroup")}
          <Tooltip title={t("memory.debugOn")}>
            <Checkbox
              style={{ marginLeft: 12 }}
              checked={debugRecall}
              onChange={(e) => setDebugRecall(e.target.checked)}
            >
              {t("memory.debug")}
            </Checkbox>
          </Tooltip>
        </div>

        {hits !== null && (
          <List
            className="memory-hits"
            size="small"
            dataSource={hits}
            locale={{ emptyText: t("memory.noHits") }}
            renderItem={(h) => (
              <List.Item>
                <Space orientation="vertical" style={{ width: "100%" }}>
                  <Space>
                    <Tag color={KIND_COLORS[h.entry.kind]}>{h.entry.kind}</Tag>
                    <b>{h.entry.title}</b>
                    <Tag>{t("memory.score", { s: h.score.toFixed(2) })}</Tag>
                    <span className="memory-reason">{h.reasons.join("；")}</span>
                  </Space>
                  <span>{h.entry.body.slice(0, 200)}</span>
                  {debugRecall && (h.matchedTerms || h.semanticScore !== undefined) && (
                    <div className="memory-debug">
                      {h.matchedTerms && Object.keys(h.matchedTerms).length > 0 && (
                        <span>
                          {t("memory.matchedTerms")}：
                          {Object.entries(h.matchedTerms)
                            .sort((a, b) => b[1] - a[1])
                            .map(([term, w]) => `${term}(${w.toFixed(2)})`)
                            .join("、")}
                        </span>
                      )}
                      {h.semanticScore !== undefined && (
                        <Tag style={{ marginLeft: 8 }}>
                          {t("memory.semanticScore", { s: h.semanticScore.toFixed(3) })}
                        </Tag>
                      )}
                    </div>
                  )}
                </Space>
              </List.Item>
            )}
          />
        )}
      </Card>

      <Card size="small" className="memory-card" title={t("memory.entriesCard")}>
        <div className="memory-filters">
          <div className="memory-filter-kinds">
            {["", "fact", "decision", "preference", "evidence", "digest", "lesson"].map((k) => (
              <Tag.CheckableTag
                key={k || "all"}
                checked={(kindFilter ?? "") === k}
                onChange={() => setKindFilter(k || undefined)}
              >
                {k === "" ? t("memory.allKinds") : k}
              </Tag.CheckableTag>
            ))}
          </div>
          <div className="memory-filter-agent">
            <span className="memory-scope-label">{t("memory.agentScope")}</span>
            <Select
              size="small"
              allowClear
              placeholder={t("memory.allAgents")}
              style={{ width: 220, maxWidth: "100%" }}
              value={agentFilter}
              onChange={(v) => setAgentFilter(v ?? undefined)}
              options={agents
                .filter(
                  (a) => a.identifier !== groups.find((g) => g.id === activeGroup)?.primary,
                )
                .map((a) => ({ value: a.identifier, label: `${a.name} (${a.identifier})` }))}
            />
          </div>
        </div>
        <Table
          size="small"
          rowKey="id"
          dataSource={entries}
          pagination={{ pageSize: 10 }}
          tableLayout="fixed"
          locale={{ emptyText: <BrandEmpty description={t("memory.emptyEntries")} /> }}
          columns={[
            {
              title: t("memory.colKind"),
              dataIndex: "kind",
              width: 100,
              render: (k: string) => <Tag color={KIND_COLORS[k]}>{k}</Tag>,
            },
            {
              title: t("memory.colLayer"),
              key: "layer",
              width: 80,
              responsive: ["md"],
              render: (_, e) =>
                e.agentId ? (
                  <Tag color="purple" title={t("memory.belongTo", { id: e.agentId })}>
                    {t("memory.layerIndividual")}
                  </Tag>
                ) : (
                  <Tag color="blue">{t("memory.layerGroup")}</Tag>
                ),
            },
            {
              title: t("memory.colTitle"),
              key: "title",
              render: (_, e) => (
                <Space orientation="vertical" size={0}>
                  <span>
                    {e.pinned && <Tag color="orange">{t("memory.pinned")}</Tag>}
                    <b>{e.title}</b>
                  </span>
                  <span className="memory-body">{e.body.slice(0, 140)}</span>
                </Space>
              ),
            },
            {
              title: t("memory.colImportance"),
              dataIndex: "importance",
              width: 90,
              responsive: ["sm"],
              render: (v: number) => v.toFixed(2),
              sorter: (a: MemoryEntry, b: MemoryEntry) => a.importance - b.importance,
            },
            {
              title: t("memory.colConf"),
              dataIndex: "confidence",
              width: 90,
              responsive: ["xl"],
              render: (v: number) => v.toFixed(2),
            },
            { title: t("memory.colAccess"), dataIndex: "accessCount", width: 70, responsive: ["xl"] },
            {
              // 版本号（capability-parity 6.5）：0 = 引入版本字段前的旧条目，按 v1 陈述
              title: t("memory.colVersion"),
              key: "version",
              width: 64,
              responsive: ["md"],
              render: (_, e) => <Tag>{tr("memory.versionTag", { v: Math.max(e.version ?? 1, 1) })}</Tag>,
            },
            {
              title: t("memory.colOps"),
              key: "ops",
              width: 180,
              render: (_, e) => (
                <Space>
                  <Tooltip title={t("memory.editTip")}>
                    <Button
                      size="small"
                      type="text"
                      icon={<EditOutlined />}
                      disabled={busyId === e.id}
                      onClick={() => openEdit(e)}
                    />
                  </Tooltip>
                  <Tooltip title={e.pinned ? t("memory.unpin") : t("memory.pinTip")}>
                    <Button
                      size="small"
                      type="text"
                      icon={<PushpinOutlined />}
                      disabled={busyId === e.id}
                      onClick={async () => {
                        await api.pinMemory(e.id, !e.pinned);
                        void reload();
                      }}
                    />
                  </Tooltip>
                  <Tooltip title={t("memory.historyTip")}>
                    <Button
                      size="small"
                      type="text"
                      icon={<HistoryOutlined />}
                      disabled={busyId === e.id}
                      onClick={() => void openHistory(e)}
                    />
                  </Tooltip>
                  <Popconfirm
                    title={t("memory.forgetConfirm")}
                    onConfirm={() => doForget(e.id)}
                  >
                    <Button
                      size="small"
                      type="text"
                      danger
                      icon={<DeleteOutlined />}
                      disabled={busyId === e.id}
                    />
                  </Popconfirm>
                </Space>
              ),
            },
          ]}
        />
      </Card>

      <Card
        size="small"
        className="memory-card"
        title={
          <Space>
            <span>{t("memory.forgottenCard")}</span>
            <Tag>{tr("memory.forgottenCount", { n: forgotten.length })}</Tag>
          </Space>
        }
      >
        {forgotten.length === 0 ? (
          <BrandEmpty description={t("memory.forgottenEmpty")} />
        ) : (
          <List
            size="small"
            rowKey="id"
            dataSource={forgotten}
            pagination={forgotten.length > 5 ? { pageSize: 5 } : false}
            renderItem={(e) => (
              <List.Item
                actions={[
                  <Popconfirm
                    key="restore"
                    title={t("memory.restoreConfirm")}
                    onConfirm={() => doRestore(e)}
                  >
                    <Button
                      size="small"
                      type="link"
                      icon={<UndoOutlined />}
                      disabled={busyId === e.id}
                    >
                      {t("memory.restoreTip")}
                    </Button>
                  </Popconfirm>,
                ]}
              >
                <List.Item.Meta
                  title={
                    <Space>
                      <Tag color="default">{tr("memory.versionTag", { v: Math.max(e.version ?? 1, 1) })}</Tag>
                      <b>{e.title}</b>
                    </Space>
                  }
                  description={e.body.slice(0, 120)}
                />
              </List.Item>
            )}
          />
        )}
      </Card>

      <Modal
        open={historyFor !== null}
        title={
          historyFor
            ? tr("memory.historyTitle", { title: historyFor.title })
            : t("memory.historyTip")
        }
        footer={null}
        width={720}
        onCancel={() => setHistoryFor(null)}
        destroyOnHidden
      >
        {historyForgotten && (
          <p>
            <Tag color="red">{t("memory.forgottenBadge")}</Tag>
          </p>
        )}
        {historyLoading ? null : history.length === 0 ? (
          <BrandEmpty description={t("memory.historyEmpty")} />
        ) : (
          <Table<MemoryHistoryRow>
            size="small"
            rowKey="version"
            dataSource={history}
            pagination={false}
            columns={[
              {
                title: t("memory.colVersion"),
                dataIndex: "version",
                width: 90,
                render: (v: number) => (
                  <Space size={4}>
                    <Tag color={v === historyCurrent ? "blue" : "default"}>
                      {tr("memory.versionTag", { v })}
                    </Tag>
                    {v === historyCurrent && <Tag color="blue">{t("memory.currentVersion")}</Tag>}
                  </Space>
                ),
              },
              {
                title: t("memory.colChange"),
                dataIndex: "change",
                width: 110,
                render: (c: string) => <Tag>{c}</Tag>,
              },
              {
                title: t("memory.colTitle"),
                key: "title",
                render: (_, r) => (
                  <Space orientation="vertical" size={0}>
                    <span>
                      {r.tags.length > 0 && (
                        <span className="memory-body">{r.tags.map((x) => `#${x}`).join(" ")} </span>
                      )}
                      <b>{r.title}</b>
                    </span>
                    {/* 正文摘要：不看内容没法判断回滚到哪一版 */}
                    <span className="memory-body">{r.body.slice(0, 160)}</span>
                  </Space>
                ),
              },
              { title: t("memory.colChangedAt"), dataIndex: "changedAt", width: 170, responsive: ["md"] },
              {
                title: t("memory.colOps"),
                key: "ops",
                width: 150,
                render: (_, r) =>
                  // 当前行就是当前生效版本：无需回滚；遗忘行是墓碑不是内容：不能作为回滚目标
                  r.version === historyCurrent || r.change === "forgotten" ? null : (
                    <Popconfirm
                      title={tr("memory.rollbackConfirm", { to: r.version })}
                      onConfirm={() => doRollback(historyFor?.id ?? "", r.version)}
                    >
                      <Button
                        size="small"
                        type="link"
                        icon={<ReloadOutlined />}
                        disabled={busyId === historyFor?.id}
                      >
                        {t("memory.rollbackTip")}
                      </Button>
                    </Popconfirm>
                  ),
              },
            ]}
          />
        )}
      </Modal>

      <Card size="small" className="memory-card" title={t("memory.reliabilityCard")}>
        <Descriptions size="small" column={1} className="memory-dbpath">
          <Descriptions.Item label={t("memory.dbLabel")}>{stats?.memory?.dbPath ?? "-"}</Descriptions.Item>
        </Descriptions>
        <Table
          size="small"
          rowKey="agentId"
          dataSource={stats?.agentStats ?? []}
          pagination={{ pageSize: 8 }}
          tableLayout="fixed"
          locale={{ emptyText: t("memory.emptyStats") }}
          columns={[
            { title: t("memory.colAgent"), dataIndex: "agentId" },
            { title: t("memory.colRuns"), dataIndex: "runs", width: 70 },
            { title: t("memory.colDone"), dataIndex: "done", width: 70 },
            { title: t("memory.colBlocked"), dataIndex: "blocked", width: 70 },
            { title: t("memory.colFailed"), dataIndex: "failed", width: 70 },
            {
              title: t("memory.colAvgConf"),
              dataIndex: "avgConfidence",
              width: 120,
              responsive: ["md"],
              render: (v: number) => v.toFixed(2),
            },
          ]}
        />
      </Card>

      <Modal
        title={t("memory.editTitle")}
        open={editing !== null}
        onCancel={() => setEditing(null)}
        onOk={() => void saveEdit()}
        confirmLoading={savingEdit}
        okText={t("common.save")}
        cancelText={t("common.cancel")}
        destroyOnHidden
      >
        <Space orientation="vertical" style={{ width: "100%" }}>
          <div>
            <div className="dim">{t("memory.fieldTitle")}</div>
            <Input
              value={editTitle}
              onChange={(e) => setEditTitle(e.target.value)}
              maxLength={200}
            />
          </div>
          <div>
            <div className="dim">{t("memory.fieldBody")}</div>
            <Input.TextArea
              value={editBody}
              onChange={(e) => setEditBody(e.target.value)}
              rows={8}
            />
          </div>
        </Space>
      </Modal>
    </div>
  );
}
