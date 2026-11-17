import { useState } from "react";
import { Alert, Form, Input, Switch, message } from "antd";
import { ApiOutlined, ControlOutlined, TeamOutlined } from "@ant-design/icons";
import { api, type GroupMeta, type LlmProfile } from "../api";
import { initialRouting, routingValidation } from "../modelRouting";
import { useT } from "../i18n/core";
import { ModelRoutingEditor } from "./ModelRoutingEditor";
import { SettingsDialog } from "./SettingsDialog";

export function GroupSettingsDialog({ group, profiles, onClose, onSaved, allowIdChange = false, active = false, initialSection = "general" }: {
  group: GroupMeta; profiles: LlmProfile[]; onClose: () => void; onSaved: (id: string) => void | Promise<void>; allowIdChange?: boolean; active?: boolean; initialSection?: string;
}) {
  const t = useT();
  const initial = { name: group.name, id: group.id, workspace: group.workspace || "", selfOptimize: group.selfOptimize || false, routing: initialRouting(group.modelRouting, group.model, group.capabilities) };
  const [draft, setDraft] = useState(initial);
  const [baseline, setBaseline] = useState(initial);
  const [liveId, setLiveId] = useState(group.id);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const [section, setSection] = useState(initialSection);
  const dirty = JSON.stringify(draft) !== JSON.stringify(baseline);
  const save = async () => {
    if (!draft.name.trim() || !/^[A-Za-z0-9_-]{1,48}$/.test(draft.id)) { setSection("general"); message.warning(t(!draft.name.trim() ? "groups.f.nameRequired" : "groups.idPattern")); return; }
    const invalid = routingValidation(draft.routing);
    if (invalid) { setSection("routing"); message.warning(invalid); return; }
    setSaving(true); setError("");
    let id = liveId;
    try {
      if (draft.name !== baseline.name || draft.id !== baseline.id) {
        const next = await api.updateGroup(id, { name: draft.name.trim(), ...(draft.id !== id ? { id: draft.id } : {}) });
        id = next.id; setLiveId(id);
        setBaseline((current) => ({ ...current, name: next.name, id })); setDraft((current) => ({ ...current, name: next.name, id }));
      }
      if (draft.workspace !== baseline.workspace) {
        await api.setGroupWorkspace(id, draft.workspace.trim());
        setBaseline((current) => ({ ...current, workspace: draft.workspace.trim() })); setDraft((current) => ({ ...current, workspace: draft.workspace.trim() }));
      }
      if (JSON.stringify(draft.routing) !== JSON.stringify(baseline.routing)) {
        await api.saveGroupRouting(id, draft.routing); setBaseline((current) => ({ ...current, routing: draft.routing }));
      }
      if (draft.selfOptimize !== baseline.selfOptimize) {
        await api.setGroupSelfOptimize(id, draft.selfOptimize); setBaseline((current) => ({ ...current, selfOptimize: draft.selfOptimize }));
      }
      message.success(t("groups.settingsSaved")); await onSaved(id);
    } catch (failure) { setError(t("ux.partialSave", { err: String(failure) })); }
    finally { setSaving(false); }
  };
  return <SettingsDialog title={t("groups.modalSettings", { name: baseline.name })} subtitle={liveId} dirty={dirty} saving={saving}
    activeSection={section} onSectionChange={setSection} onSave={() => void save()} onClose={onClose}
    children={(content) => <Form layout="vertical" disabled={saving}>{error && <Alert type="error" showIcon title={error} />}{content}</Form>}
    sections={[
      { key: "general", label: t("ux.groupGeneral"), description: t("ux.groupScope"), icon: <TeamOutlined />, content: <>
        <Form.Item label={t("groups.f.name")} required><Input aria-label={t("groups.f.name")} value={draft.name} onChange={(event) => setDraft({ ...draft, name: event.target.value })} /></Form.Item>
        <Form.Item label={t("groups.f.workspace")} extra={t("groups.f.workspaceHelp")}><Input aria-label={t("groups.f.workspace")} placeholder={t("groups.f.wsPh")} value={draft.workspace} onChange={(event) => setDraft({ ...draft, workspace: event.target.value })} /></Form.Item>
        {allowIdChange && <Form.Item label={t("groups.f.idLabel")} extra={t("groups.f.idEditExtra")}><Input disabled={active || group.builtin} value={draft.id} aria-label={t("groups.f.idLabel")} onChange={(event) => setDraft({ ...draft, id: event.target.value })} /></Form.Item>}
      </> },
      { key: "routing", label: t("ux.modelsAndCapabilities"), description: t("ux.groupModelsDescription"), icon: <ApiOutlined />, content:
        <fieldset className="settings-edit-fields" disabled={saving}><ModelRoutingEditor value={draft.routing} onChange={(routing) => setDraft({ ...draft, routing })} profiles={profiles} group /></fieldset> },
      { key: "behavior", label: t("ux.groupBehavior"), description: t("ux.groupBehaviorDescription"), icon: <ControlOutlined />, content:
        <div className="settings-toggle-row"><div><strong>{t("groups.selfOptLabel")}</strong><p>{t("groups.selfOptHint")}</p>{group.builtin && <p>{t("groups.selfOptBuiltinHint")}</p>}</div>
          <Switch aria-label={t("groups.selfOptLabel")} checked={draft.selfOptimize} onChange={(selfOptimize) => setDraft({ ...draft, selfOptimize })} /></div> },
    ]} />;
}
