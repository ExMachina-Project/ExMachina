/** Searchable agent directory; every agent uses one shared settings surface. */
import { useCallback, useEffect, useState } from "react";
import { Alert, Button, Card, Dropdown, Form, Input, Modal, Select, Spin, Tag, message } from "antd";
import { ArrowRightOutlined, MoreOutlined, PlusOutlined, ReloadOutlined, SearchOutlined, SettingOutlined, TeamOutlined, UserOutlined } from "@ant-design/icons";
import { api, type GroupMeta, type LlmProfile, type NexusLink, type TemplateInfo } from "../api";
import { AgentSettingsDialog, type SettingsAgent } from "../components/AgentSettingsDialog";
import { GroupSettingsDialog } from "../components/GroupSettingsDialog";
import { BrandEmpty } from "../components/BrandEmpty";
import { PageHeader } from "../components/PageHeader";
import { SkeletonList } from "../components/SkeletonList";
import { routingLabel } from "../modelRouting";
import { GroupDetailPanel } from "./GroupsView";
import { useExm } from "../store";
import { useT } from "../i18n/core";
import { confirmDiscard, useUnsavedChanges } from "../settingsNavigation";

export function SinglesView({ active = true }: { active?: boolean } = {}) {
  const t = useT();
  const { groups, loadGroups, activeGroup } = useExm();
  const groupList = groups ?? [];
  const [singles, setSingles] = useState<SettingsAgent[]>([]);
  const [profiles, setProfiles] = useState<LlmProfile[]>([]);
  const [links, setLinks] = useState<NexusLink[]>([]);
  const [templates, setTemplates] = useState<TemplateInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState("");
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState("all");
  const [target, setTarget] = useState<{ agent: SettingsAgent; section: string } | null>(null);
  const [groupTarget, setGroupTarget] = useState<{ group: GroupMeta; section: string } | null>(null);
  const [enteredGroup, setEnteredGroup] = useState<string | null>(null);
  const [create, setCreate] = useState<"single" | "group" | null>(null);
  const [saving, setSaving] = useState(false);
  const [deleting, setDeleting] = useState<SettingsAgent | null>(null);
  const [form] = Form.useForm();
  const values = Form.useWatch((all) => all, { form, preserve: true });
  const createDirty = !!create && Object.values(values || {}).some((value) => typeof value === "string" && !!value.trim());
  useUnsavedChanges(createDirty);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const [agents, models] = await Promise.all([api.listSingles(), api.llmProfiles()]);
      setSingles(agents.singles); setProfiles(models.profiles); setLoadError("");
      await loadGroups();
    } catch (error) { setLoadError(String(error)); }
    finally { setLoading(false); }
  }, [loadGroups]);
  useEffect(() => { if (active) void load(); }, [load, active]);
  useEffect(() => {
    void api.nexusLinks().then((result) => setLinks(result.links || [])).catch(() => undefined);
    void api.listTemplates().then((result) => setTemplates(result.templates || [])).catch(() => undefined);
  }, []);

  const openCreate = (type: "single" | "group") => { form.resetFields(); setCreate(type); };
  const submit = async () => {
    let value;
    try { value = await form.validateFields(); } catch { return; }
    setSaving(true);
    try {
      if (create === "group") {
        const group = await api.createGroup({ name: value.name.trim(), id: value.identifier?.trim() || undefined, description: value.description?.trim(), template: value.template || undefined });
        message.success(t("groups.created", { name: group.name }));
        setCreate(null); await load(); setEnteredGroup(group.id);
      } else {
        await api.createSingle({ name: value.name.trim(), identifier: value.identifier.trim(), description: value.description.trim(), prompt: value.prompt || undefined, link: value.link || undefined });
        message.success(t("singles.created", { id: value.identifier })); setCreate(null); await load();
      }
      form.resetFields();
    } catch (error) { message.error(t("common.saveFailed", { err: String(error) })); }
    finally { setSaving(false); }
  };
  const remove = async () => {
    if (!deleting) return;
    setSaving(true);
    try { await api.deleteSingle(deleting.identifier); setDeleting(null); await load(); message.success(t("common.deleted")); }
    catch (error) { message.error(t("singles.deleteFailed", { err: String(error) })); }
    finally { setSaving(false); }
  };
  const matches = (name: string, id: string, description: string) => `${name} ${id} ${description}`.toLowerCase().includes(query.trim().toLowerCase());
  const shownSingles = singles.filter((agent) => matches(agent.name, agent.identifier, agent.description));
  const shownGroups = groupList.filter((group) => matches(group.name, group.id, group.description || ""));
  const singleMenu = (agent: SettingsAgent) => [
    { key: "general", label: t("ux.agentGeneral"), onClick: () => setTarget({ agent, section: "general" }) },
    { key: "routing", label: t("ux.modelsAndCapabilities"), onClick: () => setTarget({ agent, section: "routing" }) },
    { key: "prompt", label: t("ux.agentPrompt"), onClick: () => setTarget({ agent, section: "prompt" }) },
    { key: "persona", label: t("ux.agentPersona"), onClick: () => setTarget({ agent, section: "persona" }) },
    { type: "divider" as const },
    { key: "delete", label: t("common.delete"), danger: true, onClick: () => setDeleting(agent) },
  ];
  if (enteredGroup) return <GroupDetailPanel key={enteredGroup} groupId={enteredGroup} active={active} onBack={() => { setEnteredGroup(null); void load(); }} onDeleted={() => { setEnteredGroup(null); void load(); }} />;
  return <div className="pane-wrap settings-page agents-settings-page">
    <PageHeader title={t("nav.agent")} en="AGENTS" desc={t("ux.agentsDescription")} actions={<>
      <Button type="primary" icon={<PlusOutlined />} onClick={() => openCreate("single")}>{t("singles.new")}</Button>
      <Button icon={<TeamOutlined />} onClick={() => openCreate("group")}>{t("groups.new")}</Button>
      <Button icon={<ReloadOutlined />} loading={loading} onClick={() => void load()} aria-label={t("common.refresh")} />
    </>} />
    <div className="settings-directory-toolbar">
      <Input prefix={<SearchOutlined />} allowClear placeholder={t("ux.searchAgents")} aria-label={t("ux.searchAgents")} value={query} onChange={(event) => setQuery(event.target.value)} />
      <div className="settings-filter-list" role="group" aria-label={t("ux.agentType")}>
        {[["all", t("ux.allAgents"), singles.length + groupList.length], ["single", t("singles.sectionSingles"), singles.length], ["group", t("singles.sectionGroups"), groupList.length]].map(([key, label, count]) =>
          <button type="button" key={key} aria-pressed={filter === key} onClick={() => setFilter(String(key))}>{label}<span className="settings-count">{count}</span></button>)}
      </div>
    </div>
    {loadError && <Alert type="error" showIcon title={t("common.loadFailed", { err: loadError })} />}
    <Spin spinning={loading}>
      {loading && !singles.length ? <SkeletonList rows={4} variant="card" /> : <>
        {filter !== "group" && <section className="settings-directory-section"><div className="settings-section-title"><h3>{t("singles.sectionSingles")} <span className="settings-count">{shownSingles.length}</span></h3><p>{t("ux.singleDirectoryHint")}</p></div>
          <div className="agent-directory">{shownSingles.map((agent) => <Card key={agent.identifier} size="small" className="hud agent-directory-card">
            <div className="agent-directory-heading"><span className="settings-object-icon"><UserOutlined /></span><div><h3>{agent.name}</h3><code>{agent.identifier}</code></div><Dropdown menu={{ items: singleMenu(agent) }} trigger={["click"]}><Button type="text" icon={<MoreOutlined />} aria-label={t("singles.cardMenu")} /></Dropdown></div>
            <p className="agent-directory-description">{agent.description || t("ux.noDescription")}</p>
            <div className="agent-model-summary"><span>{t("routing.default")}</span><strong>{routingLabel(agent.modelRouting, agent.modelHint)}</strong></div>
            <div className="agent-directory-actions"><Button type="primary" ghost icon={<SettingOutlined />} onClick={() => setTarget({ agent, section: "general" })}>{t("ux.configureAgent")}</Button><Button type="text" onClick={() => setTarget({ agent, section: "routing" })}>{t("ux.modelsAndCapabilities")}</Button></div>
          </Card>)}</div>
          {!shownSingles.length && <BrandEmpty description={query ? t("ux.noResults") : t("singles.empty")} />}
        </section>}
        {filter !== "single" && <section className="settings-directory-section"><div className="settings-section-title"><h3>{t("singles.sectionGroups")} <span className="settings-count">{shownGroups.length}</span></h3><p>{t("ux.groupDirectoryHint")}</p></div>
          <div className="agent-directory">{shownGroups.map((group) => <Card key={group.id} size="small" className="hud agent-directory-card group-card">
            <div className="agent-directory-heading"><span className="settings-object-icon"><TeamOutlined /></span><div><h3>{group.name}</h3><code>{group.id}</code></div><Dropdown trigger={["click"]} menu={{ items: [
              { key: "general", label: t("groups.menuEditInfo"), onClick: () => setGroupTarget({ group, section: "general" }) },
              { key: "routing", label: t("ux.modelsAndCapabilities"), onClick: () => setGroupTarget({ group, section: "routing" }) },
              { key: "behavior", label: t("groups.menuSelfOpt"), onClick: () => setGroupTarget({ group, section: "behavior" }) },
            ] }}><Button type="text" icon={<MoreOutlined />} aria-label={t("singles.cardMenu")} /></Dropdown></div>
            <p className="agent-directory-description">{group.description || t("ux.noDescription")}</p>
            <div className="agent-model-summary"><span>{t("routing.groupDefault")}</span><strong>{routingLabel(group.modelRouting, group.model)}</strong></div>
            <div className="agent-directory-tags">{group.builtin && <Tag>{t("groups.builtin")}</Tag>}{group.selfOptimize && <Tag>{t("groups.selfOptTag")}</Tag>}</div>
            <div className="agent-directory-actions"><Button type="primary" ghost icon={<ArrowRightOutlined />} onClick={() => setEnteredGroup(group.id)}>{t("ux.manageMembers")}</Button><Button type="text" icon={<SettingOutlined />} onClick={() => setGroupTarget({ group, section: "general" })}>{t("groups.settings")}</Button></div>
          </Card>)}</div>
          {!shownGroups.length && <BrandEmpty description={query ? t("ux.noResults") : t("groups.none")} />}
        </section>}
      </>}
    </Spin>
    {target && <AgentSettingsDialog key={target.agent.identifier} agent={target.agent} profiles={profiles} initialSection={target.section} onClose={() => { setTarget(null); void load(); }} onSaved={load} />}
    {groupTarget && <GroupSettingsDialog key={groupTarget.group.id} group={groupTarget.group} profiles={profiles} initialSection={groupTarget.section} allowIdChange active={groupTarget.group.id === activeGroup} onClose={() => { setGroupTarget(null); void load(); }} onSaved={async () => { await load(); }} />}
    <Modal open={!!deleting} title={t("singles.deleteConfirm")} onCancel={() => setDeleting(null)} onOk={() => void remove()} confirmLoading={saving} okText={t("common.delete")} okButtonProps={{ danger: true }}>
      {t("singles.deleteHint", { name: deleting?.name || "", id: deleting?.identifier || "" })}
    </Modal>
    <Modal open={!!create} title={t(create === "group" ? "groups.modalNew" : "singles.new")} onCancel={() => { if (!saving) confirmDiscard(createDirty, () => setCreate(null)); }}
      onOk={() => void submit()} confirmLoading={saving} okText={t("singles.create")} destroyOnHidden maskClosable={false}>
      <Form form={form} layout="vertical" disabled={saving} onFinish={() => void submit()}>
        <Form.Item name="name" label={t(create === "group" ? "groups.f.name" : "singles.f.name")} rules={[{ required: true, whitespace: true }]}><Input autoFocus /></Form.Item>
        <Form.Item name="identifier" label={t(create === "group" ? "groups.f.id" : "singles.f.identifier")} rules={[{ required: create === "single" }, { pattern: /^[A-Za-z0-9_-]{1,48}$/, message: t("singles.idPattern") }]}><Input placeholder={create === "group" ? t("groups.idAuto") : t("singles.f.identifierPh")} /></Form.Item>
        <Form.Item name="description" label={t("singles.f.desc")} rules={[{ required: create === "single", whitespace: true }]}><Input.TextArea rows={2} placeholder={t("singles.f.descPh")} /></Form.Item>
        {create === "single" ? <>
          <Form.Item name="prompt" label={t("singles.f.prompt")}><Input.TextArea rows={4} placeholder={t("singles.f.promptPh")} /></Form.Item>
          <Form.Item name="link" label={t("agents.f.link")} extra={t("agents.f.linkExtra")}><Select allowClear options={links.map((link) => ({ value: link.id, label: `${link.name} (${link.id})`, disabled: !link.enabled }))} placeholder={t("agents.f.linkPh")} /></Form.Item>
        </> : <><Form.Item name="template" label={t("groups.f.template")}><Select allowClear placeholder={t("groups.f.templateBlank")} options={templates.filter((template) => template.kind === "group").map((template) => ({ value: template.id, label: template.name }))} /></Form.Item><p className="settings-context-note">{t("groups.newHint")}</p></>}
      </Form>
    </Modal>
  </div>;
}
