import { useEffect, useState } from "react";
import { Alert, Button, Form, Input, Popconfirm, Select, Spin, Tag, message } from "antd";
import { ApiOutlined, FileTextOutlined, SmileOutlined, UndoOutlined, UserOutlined } from "@ant-design/icons";
import { api, type LlmProfile, type ModelRouting, type NexusLink, type PersonaInfo } from "../api";
import { initialRouting, routingValidation } from "../modelRouting";
import { ModelRoutingEditor } from "./ModelRoutingEditor";
import { SettingsDialog } from "./SettingsDialog";
import { useT } from "../i18n/core";

export interface SettingsAgent {
  identifier: string; name: string; description: string; domain?: string; link?: string | null;
  modelHint?: string | null; modelRouting?: ModelRouting | null;
}
interface Draft {
  name: string; description: string; domain: string; link: string;
  routing: ModelRouting; prompt: string | null; persona: string | null;
}

/** Shared by standalone agents, group members, and the primary agent. */
export function AgentSettingsDialog({ agent, profiles, groupId, groupName, personaProtected = false, initialSection = "general", onClose, onSaved }: {
  agent: SettingsAgent; profiles: LlmProfile[]; groupId?: string; groupName?: string; personaProtected?: boolean;
  initialSection?: string; onClose: () => void; onSaved: () => void | Promise<void>;
}) {
  const t = useT();
  const initial: Draft = { name: agent.name, description: agent.description, domain: agent.domain || "", link: agent.link || "",
    routing: initialRouting(agent.modelRouting, agent.modelHint), prompt: null, persona: null };
  const [draft, setDraft] = useState(initial);
  const [baseline, setBaseline] = useState(initial);
  const [persona, setPersona] = useState<PersonaInfo | null>(null);
  const [links, setLinks] = useState<NexusLink[]>([]);
  const [loading, setLoading] = useState(true);
  const [readErrors, setReadErrors] = useState({ prompt: "", persona: "" });
  const [saveError, setSaveError] = useState("");
  const [saving, setSaving] = useState(false);
  const [section, setSection] = useState(initialSection);
  const dirty = JSON.stringify(draft) !== JSON.stringify(baseline);

  useEffect(() => {
    let active = true;
    const read = async () => {
      const [prompt, soul, nexus] = await Promise.allSettled([
        groupId ? api.getAgentPrompt(groupId, agent.identifier) : api.getSinglePrompt(agent.identifier),
        groupId ? api.getPersona(agent.identifier) : api.singlePersona(agent.identifier), api.nexusLinks(),
      ]);
      if (!active) return;
      const text = { prompt: prompt.status === "fulfilled" ? prompt.value.prompt : null, persona: soul.status === "fulfilled" ? soul.value.persona : null };
      setDraft((current) => ({ ...current, ...text })); setBaseline((current) => ({ ...current, ...text }));
      if (soul.status === "fulfilled") setPersona(soul.value);
      if (nexus.status === "fulfilled") setLinks(nexus.value.links);
      setReadErrors({ prompt: prompt.status === "rejected" ? String(prompt.reason) : "", persona: soul.status === "rejected" ? String(soul.reason) : "" });
      setLoading(false);
    };
    void read();
    return () => { active = false; };
  }, [agent.identifier, groupId]);

  const save = async () => {
    if (!draft.name.trim()) { setSection("general"); message.warning(t("singles.f.nameRequired")); return; }
    if (draft.description !== baseline.description && !draft.description.trim()) { setSection("general"); message.warning(t("singles.f.descPh")); return; }
    const invalid = routingValidation(draft.routing);
    if (invalid) { setSection("routing"); message.warning(invalid); return; }
    if (draft.prompt !== baseline.prompt && !draft.prompt?.trim()) { setSection("prompt"); message.warning(t("agents.f.promptRequired")); return; }
    setSaving(true); setSaveError("");
    try {
      const basic = { ...(draft.name !== baseline.name ? { name: draft.name.trim() } : {}),
        ...(draft.description !== baseline.description ? { description: draft.description.trim() } : {}),
        ...(draft.domain !== baseline.domain ? { domain: draft.domain } : {}), ...(draft.link !== baseline.link ? { link: draft.link } : {}) };
      if (Object.keys(basic).length) {
        if (groupId) await api.updateAgent(agent.identifier, basic); else await api.updateSingle(agent.identifier, basic);
        setBaseline((current) => ({ ...current, ...basic }));
        setDraft((current) => ({ ...current, ...basic }));
      }
      if (JSON.stringify(draft.routing) !== JSON.stringify(baseline.routing)) {
        if (groupId) await api.saveMemberRouting(groupId, agent.identifier, draft.routing); else await api.saveSingleRouting(agent.identifier, draft.routing);
        setBaseline((current) => ({ ...current, routing: draft.routing }));
      }
      if (draft.prompt !== null && draft.prompt !== baseline.prompt) {
        if (groupId) await api.putAgentPrompt(groupId, agent.identifier, draft.prompt); else await api.putSinglePrompt(agent.identifier, draft.prompt);
        setBaseline((current) => ({ ...current, prompt: draft.prompt }));
      }
      if (draft.persona !== null && draft.persona !== baseline.persona) {
        if (groupId) await api.putPersona(agent.identifier, draft.persona); else await api.singlesSetPersona(agent.identifier, draft.persona);
        setBaseline((current) => ({ ...current, persona: draft.persona }));
        setPersona((current) => current && { ...current, custom: true });
      }
      message.success(t("ux.agentSaved"));
      await onSaved();
    } catch (error) { setSaveError(t("ux.partialSave", { err: String(error) })); }
    finally { setSaving(false); }
  };
  const resetPersona = async () => {
    setSaving(true);
    try {
      if (groupId) await api.resetPersona(agent.identifier); else await api.singlesResetPersona(agent.identifier);
      const next = groupId ? await api.getPersona(agent.identifier) : await api.singlePersona(agent.identifier);
      setPersona(next); setDraft((current) => ({ ...current, persona: next.persona })); setBaseline((current) => ({ ...current, persona: next.persona }));
      message.success(t("agents.personaReset"));
    } catch (error) { message.error(t("common.saveFailed", { err: String(error) })); }
    finally { setSaving(false); }
  };
  return <SettingsDialog title={baseline.name} subtitle={<>{groupName ? `${groupName} / ` : ""}{agent.identifier}</>}
    dirty={dirty} saving={saving} activeSection={section} onSectionChange={setSection} onSave={() => void save()} onClose={onClose}
    children={(content) => <Form layout="vertical" disabled={saving}>{saveError && <Alert type="error" showIcon title={saveError} />}{content}</Form>}
    sections={[
      { key: "general", label: t("ux.agentGeneral"), description: t(groupId ? "ux.memberScope" : "ux.singleScope"), icon: <UserOutlined />, content: <>
        <Form.Item label={t("singles.f.name")} required><Input aria-label={t("singles.f.name")} value={draft.name} onChange={(event) => setDraft({ ...draft, name: event.target.value })} /></Form.Item>
        <Form.Item label={t("singles.f.desc")}><Input.TextArea aria-label={t("singles.f.desc")} rows={3} value={draft.description} onChange={(event) => setDraft({ ...draft, description: event.target.value })} /></Form.Item>
        <Form.Item label={t("ux.agentDomain")}><Input value={draft.domain} aria-label={t("ux.agentDomain")} onChange={(event) => setDraft({ ...draft, domain: event.target.value })} /></Form.Item>
        {groupId && <Form.Item label={t("agents.f.link")} extra={t("agents.f.linkExtra")}><Select allowClear aria-label={t("agents.f.link")} value={draft.link || undefined} onChange={(link) => setDraft({ ...draft, link: link || "" })}
          options={links.map((link) => ({ value: link.id, label: `${link.name} (${link.id})`, disabled: !link.enabled }))} placeholder={t("agents.f.linkPh")} /></Form.Item>}
        <p className="settings-context-note">{t("singles.renameIdHint", { id: agent.identifier })}</p>
      </> },
      { key: "routing", label: t("ux.modelsAndCapabilities"), description: t("ux.agentModelsDescription"), icon: <ApiOutlined />, content:
        <fieldset className="settings-edit-fields" disabled={saving}><ModelRoutingEditor value={draft.routing} onChange={(routing) => setDraft({ ...draft, routing })} profiles={profiles} inheritedLabel={t(groupId ? "ux.inheritGroup" : "ux.inheritSystem")} /></fieldset> },
      { key: "prompt", label: t("ux.agentPrompt"), description: t("ux.promptDescription"), icon: <FileTextOutlined />, content: <Spin spinning={loading}>
        {readErrors.prompt ? <Alert type="error" title={t("common.loadFailed", { err: readErrors.prompt })} /> : <Input.TextArea aria-label={t("ux.agentPrompt")} rows={18} className="prompt-editor" spellCheck={false} value={draft.prompt || ""} disabled={loading || saving} onChange={(event) => setDraft({ ...draft, prompt: event.target.value })} />}
      </Spin> },
      { key: "persona", label: t("ux.agentPersona"), description: t("ux.personaDescription"), icon: <SmileOutlined />, content: <Spin spinning={loading}>
        {readErrors.persona ? <Alert type="error" title={t("common.loadFailed", { err: readErrors.persona })} /> : <>
          <div className="settings-toolbar"><Tag>{t(persona?.custom ? "agents.customPersona" : "agents.defaultPersona")}</Tag>
            <Popconfirm title={t("agents.resetConfirm")} onConfirm={() => void resetPersona()} disabled={personaProtected || !persona?.custom || saving}>
              <Button icon={<UndoOutlined />} disabled={personaProtected || !persona?.custom || saving}>{t("agents.resetBtn")}</Button>
            </Popconfirm>
          </div>
          {personaProtected && <p className="settings-context-note">{t("agents.soulProtectedHint")}</p>}
          <Input.TextArea aria-label={t("ux.agentPersona")} rows={14} className="prompt-editor" spellCheck={false} value={draft.persona || ""} disabled={loading || saving} onChange={(event) => setDraft({ ...draft, persona: event.target.value })} />
        </>}
      </Spin> },
    ]} />;
}
