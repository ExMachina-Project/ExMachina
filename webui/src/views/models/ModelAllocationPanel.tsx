import { useCallback, useEffect, useRef, useState } from "react";
import { Alert, Input, Select, Spin, Tag, message } from "antd";
import { SearchOutlined, TeamOutlined, UserOutlined } from "@ant-design/icons";
import { api, type GroupMeta, type LlmProfile } from "../../api";
import { ModelRoutingEditor } from "../../components/ModelRoutingEditor";
import { SettingsSaveBar } from "../../components/SettingsDialog";
import { BrandEmpty } from "../../components/BrandEmpty";
import { initialRouting, purposeLabel, routingLabel, routingValidation, sourceLabel, type ModelRouting, type RoutingPreview } from "../../modelRouting";
import { tr, useLang, useT } from "../../i18n/core";
import { confirmDiscard, useUnsavedChanges } from "../../settingsNavigation";

interface Subject {
  key: string;
  name: string;
  group?: GroupMeta;
  id?: string;
  model?: string | null;
  routing?: ModelRouting | null;
}

/** All entry points edit the same owner-scoped rules, without changing the chat target. */
export function ModelAllocationPanel({ profiles, active = true }: { profiles: LlmProfile[]; active?: boolean }) {
  const t = useT();
  const lang = useLang((state) => state.lang);
  const [subjects, setSubjects] = useState<Subject[]>([]);
  const [key, setKey] = useState("");
  const [draft, setDraft] = useState<ModelRouting>({});
  const [baseline, setBaseline] = useState("{}");
  const [query, setQuery] = useState("");
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState("");
  const [saving, setSaving] = useState(false);
  const [preview, setPreview] = useState<RoutingPreview[]>([]);
  const current = subjects.find((subject) => subject.key === key);
  const request = useRef(0);
  const dirty = JSON.stringify(draft) !== baseline;
  const hasDraft = useRef(dirty);
  hasDraft.current = dirty;
  useUnsavedChanges(dirty);

  const load = useCallback(async () => {
    const version = ++request.current;
    setLoading(true);
    try {
      const [singles, groups] = await Promise.all([api.listSingles(), api.groups()]);
      const members = await Promise.all(groups.groups.map(async (group): Promise<Subject[]> => [
        { key: `group:${group.id}`, name: `${group.name} · ${tr("routing.groupDefault")}`, group, model: group.model, routing: group.modelRouting },
        ...(await api.groupAgents(group.id)).map((agent) => ({
          key: `member:${group.id}:${agent.identifier}`, name: `${group.name} / ${agent.name}`, group,
          id: agent.identifier, model: agent.modelHint, routing: agent.modelRouting,
        })),
      ]));
      const next = [
        ...singles.singles.map((agent) => ({ key: `single:${agent.identifier}`, name: agent.name, id: agent.identifier, model: agent.modelHint, routing: agent.modelRouting })),
        ...members.flat(),
      ];
      if (version === request.current) { setSubjects(next); setLoadError(""); }
      return next;
    } catch (error) {
      if (version === request.current) setLoadError(tr("routing.loadFailed", { err: String(error) }));
      return undefined;
    } finally {
      if (version === request.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (active && !hasDraft.current) void load();
    return () => { request.current += 1; };
  }, [load, lang, active]);
  useEffect(() => {
    if (!subjects.some((subject) => subject.key === key) && subjects[0] && !dirty) {
      const subject = subjects[0];
      const initial = initialRouting(subject.routing, subject.model, subject.id ? undefined : subject.group?.capabilities);
      setKey(subject.key); setDraft(initial); setBaseline(JSON.stringify(initial));
    }
  }, [key, subjects, dirty]);
  useEffect(() => {
    if (!current || saving || dirty) return;
    const next = initialRouting(current.routing, current.model, current.id ? undefined : current.group?.capabilities);
    const serialized = JSON.stringify(next);
    if (serialized !== baseline) { setDraft(next); setBaseline(serialized); }
  }, [current, saving, dirty, baseline]);
  useEffect(() => {
    let active = true;
    setPreview([]);
    if (current?.id) void api.routingPreview(current.id, current.group?.id).then((result) => {
      if (active) setPreview(result.policies || []);
    }).catch((error) => { if (active) message.error(tr("routing.loadFailed", { err: String(error) })); });
    return () => { active = false; };
  }, [current, lang]);

  const select = (nextKey: string) => {
    if (nextKey === key) return;
    confirmDiscard(dirty, () => {
      const subject = subjects.find((item) => item.key === nextKey);
      const initial = initialRouting(subject?.routing, subject?.model, subject?.id ? undefined : subject?.group?.capabilities);
      setKey(nextKey); setDraft(initial); setBaseline(JSON.stringify(initial));
    });
  };
  const save = async () => {
    if (!current) return;
    const error = routingValidation(draft);
    if (error) { message.warning(error); return; }
    setSaving(true);
    try {
      if (!current.group) await api.saveSingleRouting(current.id!, draft);
      else if (current.id) await api.saveMemberRouting(current.group.id, current.id, draft);
      else await api.saveGroupRouting(current.group.id, draft);
      setBaseline(JSON.stringify(draft));
      message.success(t("routing.saved"));
      await load();
    } catch (error) { message.error(t("common.saveFailed", { err: String(error) })); }
    finally { setSaving(false); }
  };
  const shown = subjects.filter((subject) => `${subject.name} ${subject.key}`.toLowerCase().includes(query.trim().toLowerCase()));
  return <Spin spinning={loading}>
    {loadError && <Alert type="error" showIcon title={loadError} />}
    <div className="settings-workspace allocation-workspace">
      <aside className="settings-list-panel">
        <div className="settings-list-head"><strong>{t("routing.subject")}</strong><span className="settings-count">{subjects.length}</span></div>
        <Input prefix={<SearchOutlined />} aria-label={t("ux.searchAgents")} placeholder={t("ux.searchAgents")} value={query} allowClear onChange={(event) => setQuery(event.target.value)} />
        <nav className="settings-object-list" aria-label={t("routing.subject")}>
          {shown.map((subject) => <button key={subject.key} disabled={saving} type="button" aria-current={subject.key === key ? "page" : undefined} onClick={() => select(subject.key)}>
            <span className="settings-object-icon">{subject.group ? <TeamOutlined /> : <UserOutlined />}</span>
            <span><strong>{subject.name}</strong><small>{t(!subject.group ? "singles.sectionSingles" : subject.id ? "routing.member" : "routing.groupDefault")}</small></span>
          </button>)}
          {!shown.length && !loading && <p className="settings-list-empty">{t("ux.noResults")}</p>}
        </nav>
      </aside>
      <section className="settings-detail-panel">
        <div className="allocation-mobile-select"><Select aria-label={t("routing.subject")} showSearch optionFilterProp="label" value={key || undefined}
          placeholder={t("routing.selectSubject")} onChange={select} disabled={saving} options={subjects.map((subject) => ({ value: subject.key, label: subject.name }))} /></div>
        {current ? <>
          <header className="settings-detail-head"><div><span className="settings-eyebrow">{t("routing.title")}</span><h3>{current.name}</h3><p>{t(!current.group ? "ux.singleScope" : current.id ? "ux.memberScope" : "ux.groupScope")}</p></div><Tag>{routingLabel(current.routing, current.model)}</Tag></header>
          {preview.length > 0 && <details className="routing-preview"><summary>{t("routing.effective")} <span className="dim">{dirty ? t("ux.savedPreview") : ""}</span></summary>
            {preview.map((policy) => <div className="routing-preview-row" key={policy.purpose}>
              <span>{purposeLabel(policy.purpose)}</span><Tag>{sourceLabel(policy.source)}</Tag>
              <span className="mono">{policy.assignment.mode === "fixed" ? policy.assignment.model : t(`routing.${policy.assignment.mode}`)}</span>
            </div>)}
          </details>}
          <fieldset className="settings-edit-fields" disabled={saving}>
            <ModelRoutingEditor value={draft} onChange={setDraft} profiles={profiles} group={!current.id} showScope={false}
              inheritedLabel={t(current.group && current.id ? "ux.inheritGroup" : "ux.inheritSystem")} />
          </fieldset>
          <div className="routing-save"><SettingsSaveBar dirty={dirty} saving={saving} onSave={() => void save()} onReset={() => setDraft(JSON.parse(baseline) as ModelRouting)} scope={current.name} /></div>
        </> : <BrandEmpty description={t("routing.noSubjects")} />}
      </section>
    </div>
  </Spin>;
}
