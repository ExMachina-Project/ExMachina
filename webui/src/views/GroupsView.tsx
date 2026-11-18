/** Group composition and configuration share the same agent settings surface. */
import { useCallback, useEffect, useState } from "react";
import { Alert, Button, Card, Form, Input, Modal, Popconfirm, Select, Spin, Tag, message } from "antd";
import { ArrowLeftOutlined, DeleteOutlined, PlusOutlined, SearchOutlined, SettingOutlined, UserOutlined } from "@ant-design/icons";
import { api, type GroupOverview, type LlmProfile, type TemplateInfo } from "../api";
import type { AgentDefinition } from "../types";
import { AgentSettingsDialog } from "../components/AgentSettingsDialog";
import { GroupSettingsDialog } from "../components/GroupSettingsDialog";
import { BrandEmpty } from "../components/BrandEmpty";
import { PageHeader } from "../components/PageHeader";
import { routingLabel } from "../modelRouting";
import { useExm } from "../store";
import { useT } from "../i18n/core";
import { confirmDiscard, useUnsavedChanges } from "../settingsNavigation";

export function GroupDetailPanel({ groupId, onBack, onDeleted, active = true }: { groupId: string; onBack: () => void; onDeleted?: () => void; active?: boolean }) {
  const t = useT();
  const { groups, activeGroup, refreshAgents, loadGroups } = useExm();
  const [members, setMembers] = useState<AgentDefinition[]>([]);
  const [overview, setOverview] = useState<GroupOverview | null>(null);
  const [profiles, setProfiles] = useState<LlmProfile[]>([]);
  const [templates, setTemplates] = useState<TemplateInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [settings, setSettings] = useState(false);
  const [target, setTarget] = useState<{ agent: AgentDefinition; section: string } | null>(null);
  const [create, setCreate] = useState(false);
  const [saving, setSaving] = useState(false);
  const [query, setQuery] = useState("");
  const [form] = Form.useForm();
  const values = Form.useWatch((all) => all, { form, preserve: true });
  const createDirty = create && Object.values(values || {}).some((value) => typeof value === "string" && !!value.trim());
  useUnsavedChanges(createDirty);
  const meta = groups.find((group) => group.id === groupId);
  const primary = members.find((agent) => agent.identifier === meta?.primary);
  const graph = useExm((state) => state.graph);
  const busy = new Set((graph?.nodes || []).filter((node) => ["running", "dispatched", "syncing"].includes(node.status)).map((node) => node.agentIdentifier));
  const load = useCallback(async () => {
    setLoading(true);
    try {
      const [agents, stats] = await Promise.all([api.groupAgents(groupId), api.groupOverview(groupId)]);
      setMembers(agents); setOverview(stats); setError("");
    } catch (failure) { setError(String(failure)); }
    finally { setLoading(false); }
  }, [groupId]);
  useEffect(() => { if (active) void load(); }, [load, active]);
  useEffect(() => {
    void loadGroups().catch((failure) => setError(String(failure)));
    void api.llmProfiles().then((result) => setProfiles(result.profiles)).catch((failure) => setError(String(failure)));
    void api.listTemplates().then((result) => setTemplates(result.templates || [])).catch(() => undefined);
  }, [loadGroups]);
  const savedAgent = async () => { await load(); await refreshAgents(); };
  const submit = async () => {
    let value;
    try { value = await form.validateFields(); } catch { return; }
    setSaving(true);
    try {
      await api.createAgent({ name: value.name || "", identifier: value.identifier, description: value.description || "", prompt: value.prompt || undefined, group: groupId, template: value.template || undefined });
      message.success(t("groups.agentCreated", { id: value.identifier })); setCreate(false); form.resetFields(); await savedAgent();
    } catch (failure) { message.error(t("groups.createFailed", { err: String(failure) })); }
    finally { setSaving(false); }
  };
  const removeAgent = async (id: string) => {
    try { await api.removeGroupAgent(groupId, id); await savedAgent(); message.success(t("groups.agentDeleted", { id })); }
    catch (failure) { message.error(t("groups.deleteFailed", { err: String(failure) })); }
  };
  const removeGroup = async () => {
    try { await api.deleteGroup(groupId); await loadGroups(); message.success(t("groups.groupDeleted", { id: groupId })); onDeleted?.(); }
    catch (failure) { message.error(t("groups.deleteFailed", { err: String(failure) })); }
  };
  const filtered = members.filter((agent) => agent.identifier !== meta?.primary && `${agent.name} ${agent.identifier} ${agent.description}`.toLowerCase().includes(query.trim().toLowerCase()));
  if (!meta) return <div className="pane-wrap"><Button icon={<ArrowLeftOutlined />} onClick={onBack}>{t("groups.back")}</Button>{loading ? <Spin /> : <BrandEmpty description={error || t("groups.none")} />}</div>;
  return <div className="pane-wrap settings-page group-settings-page">
    <Button className="settings-back" type="text" icon={<ArrowLeftOutlined />} onClick={onBack}>{t("groups.back")}</Button>
    <PageHeader title={meta.name} en="GROUP" desc={t("ux.groupDirectoryHint")} actions={<>
      <Button icon={<SettingOutlined />} onClick={() => setSettings(true)}>{t("groups.settings")}</Button>
      {!meta.builtin && <Button type="primary" icon={<PlusOutlined />} onClick={() => { form.resetFields(); setCreate(true); }}>{t("groups.newAgent")}</Button>}
      <Popconfirm title={t("groups.groupDeleteConfirm", { name: meta.name })} disabled={meta.builtin || meta.id === activeGroup} onConfirm={() => void removeGroup()}><Button danger type="text" icon={<DeleteOutlined />} aria-label={t("groups.del")} disabled={meta.builtin || meta.id === activeGroup} /></Popconfirm>
    </>} />
    {error && <Alert type="error" showIcon title={t("common.loadFailed", { err: error })} />}
    <div className="group-context-strip"><code>{meta.id}</code><Tag>{t(meta.builtin ? "groups.builtinProtected" : "groups.customGroup")}</Tag>{meta.id === activeGroup && <Tag>{t("groups.activeTag")}</Tag>}
      <span>{t("routing.groupDefault")}: <strong>{routingLabel(meta.modelRouting, meta.model)}</strong></span>
    </div>
    <Spin spinning={loading}>
      {primary && <Card size="small" className="hud group-primary-card">
        <div className="settings-section-title"><div><span className="settings-eyebrow">{t("groups.primaryTitle")}</span><h3><UserOutlined /> {primary.name}</h3><p>{t("groups.primaryHint")}</p></div><Tag>{routingLabel(primary.modelRouting, primary.modelHint)}</Tag></div>
       <p>{primary.description}</p><div className="settings-detail-actions"><Button type="primary" ghost icon={<SettingOutlined />} onClick={() => setTarget({ agent: primary, section: "general" })}>{t("ux.configureAgent")}</Button></div>
      </Card>}
      <div className="settings-section-title"><div><h3>{t("groups.unitsTitle")} <span className="settings-count">{members.filter((agent) => agent.identifier !== meta.primary).length}</span></h3><p>{t("ux.memberListHint")}</p></div></div>
      <div className="settings-directory-toolbar"><Input prefix={<SearchOutlined />} aria-label={t("ux.searchMembers")} placeholder={t("ux.searchMembers")} allowClear value={query} onChange={(event) => setQuery(event.target.value)} /></div>
      <div className="group-member-list">{filtered.map((agent) => <article className="group-member-row" key={agent.identifier}>
        <div className="group-member-info"><h4>{agent.name}<Tag color={busy.has(agent.identifier) ? "processing" : undefined}>{t(busy.has(agent.identifier) ? "agents.busy" : "agents.idle")}</Tag></h4><code>{agent.identifier}</code><p>{agent.description}</p>
          <details className="member-tools"><summary>{t("agents.colTools")} · {agent.tools?.length || t("agents.toolsFull")}</summary><div>{agent.tools?.map((tool) => <Tag key={tool}>{tool}</Tag>)}</div></details>
        </div>
        <div className="group-member-model"><span>{t("routing.default")}</span><Button type="link" onClick={() => setTarget({ agent, section: "routing" })}>{routingLabel(agent.modelRouting, agent.modelHint)}</Button></div>
        <div className="group-member-actions"><Button icon={<SettingOutlined />} onClick={() => setTarget({ agent, section: "general" })}>{t("ux.configureAgent")}</Button>
          {!meta.builtin && <Popconfirm title={t("groups.agentDeleteConfirm")} onConfirm={() => void removeAgent(agent.identifier)}><Button type="text" danger icon={<DeleteOutlined />} aria-label={t("common.delete")} /></Popconfirm>}
        </div>
      </article>)}</div>
      {!filtered.length && <BrandEmpty description={query ? t("ux.noResults") : t("groups.emptyUnits")} />}
      {overview && <details className="group-stats-details"><summary>{t("ux.groupStats")}</summary><div className="overview-chips">
        <div className="chip"><span className="chip-num">{overview.agents}</span><span className="chip-label">{t("groups.chipAgents")}</span></div>
        <div className="chip"><span className="chip-num">{overview.sessions}</span><span className="chip-label">{t("groups.chipSessions")}</span></div>
        <div className="chip"><span className="chip-num">{overview.playbooks}</span><span className="chip-label">{t("groups.chipPlaybooks")}</span></div>
        <div className="chip"><span className="chip-num">{overview.memory?.group || 0}</span><span className="chip-label">{t("groups.chipGroupMem")}</span></div>
        <div className="chip"><span className="chip-num">{overview.memory?.shared || 0}</span><span className="chip-label">{t("groups.chipShared")}</span></div>
      </div>{overview.agentStats?.slice(0, 6).map((stats) => <div className="member-stat-row" key={stats.agentId}><code>{stats.agentId}</code><span>{t("groups.stat", { runs: stats.runs, done: stats.done, blocked: stats.blocked, conf: stats.avgConfidence.toFixed(2) })}</span></div>)}</details>}
    </Spin>
    {target && <AgentSettingsDialog key={target.agent.identifier} agent={target.agent} profiles={profiles} groupId={groupId} groupName={meta.name} personaProtected={meta.builtin}
      initialSection={target.section} onClose={() => { setTarget(null); void load(); }} onSaved={savedAgent} />}
    {settings && <GroupSettingsDialog group={meta} profiles={profiles} onClose={() => { setSettings(false); void loadGroups(); }} onSaved={async () => { await loadGroups(); await load(); }} />}
    <Modal open={create} title={t("groups.modalNewAgent", { name: meta.name })} onCancel={() => { if (!saving) confirmDiscard(createDirty, () => setCreate(false)); }} onOk={() => void submit()} confirmLoading={saving} okText={t("groups.create")} destroyOnHidden maskClosable={false}>
      <Form form={form} layout="vertical" disabled={saving} onFinish={() => void submit()}>
        <Form.Item name="template" label={t("groups.f.template")}><Select allowClear placeholder={t("groups.f.templateBlank")} options={templates.filter((template) => template.kind === "unit").map((template) => ({ value: template.id, label: template.name }))} /></Form.Item>
        <Form.Item name="name" label={t("groups.f.agentName")}><Input placeholder={t("groups.f.agentNamePh")} /></Form.Item>
        <Form.Item name="identifier" label={t("groups.f.identifier")} rules={[{ required: true }, { pattern: /^[A-Za-z0-9_-]{1,48}$/, message: t("groups.idPattern") }]}><Input placeholder={t("groups.f.identifierPh")} /></Form.Item>
        <Form.Item name="description" label={t("groups.duty")}><Input.TextArea rows={2} placeholder={t("groups.f.dutyPh")} /></Form.Item>
        <Form.Item name="prompt" label={t("agents.f.prompt")} extra={t("agents.f.promptExtra")}><Input.TextArea rows={4} placeholder={t("agents.f.promptPh")} /></Form.Item>
      </Form>
    </Modal>
  </div>;
}
