import { useCallback, useEffect, useRef, useState } from "react";
import { Button, Card, Select, Space, Spin, Tag, message } from "antd";
import { api, type GroupMeta, type LlmProfile } from "../../api";
import { ModelRoutingEditor } from "../../components/ModelRoutingEditor";
import { BrandEmpty } from "../../components/BrandEmpty";
import { initialRouting, purposeLabel, sourceLabel, type ModelRouting, type RoutingPreview } from "../../modelRouting";
import { tr, useLang, useT } from "../../i18n/core";

interface Subject {
  key: string;
  name: string;
  group?: GroupMeta;
  id?: string;
  model?: string | null;
  routing?: ModelRouting | null;
}

/** The models page edits the same owner-scoped assignments as the agent and group pages. */
export function ModelAllocationPanel({ profiles }: { profiles: LlmProfile[] }) {
  const t = useT();
  const lang = useLang((state) => state.lang);
  const [subjects, setSubjects] = useState<Subject[]>([]);
  const [key, setKey] = useState("");
  const [draft, setDraft] = useState<ModelRouting>({});
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [preview, setPreview] = useState<RoutingPreview[]>([]);
  const current = subjects.find((s) => s.key === key);
  const request = useRef(0);

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
      if (version === request.current) setSubjects(next);
    } catch (error) {
      message.error(tr("routing.loadFailed", { err: String(error) }));
    } finally {
      if (version === request.current) setLoading(false);
    }
  }, []);

  useEffect(() => { void load(); }, [load, lang]);
  useEffect(() => {
    let active = true;
    setPreview([]);
    if (current?.id) void api.routingPreview(current.id, current.group?.id).then((result) => {
      if (active) setPreview(result.policies);
    }).catch((error) => { if (active) message.error(tr("routing.loadFailed", { err: String(error) })); });
    return () => { active = false; };
  }, [current, lang]);

  const select = (nextKey: string) => {
    const subject = subjects.find((s) => s.key === nextKey);
    setKey(nextKey);
    setDraft(initialRouting(subject?.routing, subject?.model, subject?.id ? undefined : subject?.group?.capabilities));
  };
  const save = async () => {
    if (!current) return;
    setSaving(true);
    try {
      if (!current.group) await api.saveSingleRouting(current.id!, draft);
      else if (current.id) await api.saveMemberRouting(current.group.id, current.id, draft);
      else await api.saveGroupRouting(current.group.id, draft);
      message.success(t("routing.saved"));
      await load();
    } catch (error) { message.error(t("common.saveFailed", { err: String(error) })); }
    finally { setSaving(false); }
  };
  return <Card className="hud allocation-card" size="small" title={t("routing.title")}>
    <Spin spinning={loading}>
      <p className="dim">{t("routing.description")}</p>
      <label className="routing-subject"><span>{t("routing.subject")}</span>
        <Select aria-label={t("routing.subject")} showSearch optionFilterProp="label" value={key || undefined}
          placeholder={t("routing.selectSubject")} onChange={select} disabled={saving}
          options={subjects.map((subject) => ({ value: subject.key, label: subject.name }))} />
      </label>
      {current && <>
        {preview.length > 0 && <details className="routing-preview"><summary>{t("routing.effective")}</summary>
          {preview.map((policy) => <div className="routing-preview-row" key={policy.purpose}>
            <span>{purposeLabel(policy.purpose)}</span><Tag>{sourceLabel(policy.source)}</Tag>
            <span className="mono">{policy.assignment.mode === "fixed" ? policy.assignment.model : t(`routing.${policy.assignment.mode}`)}</span>
          </div>)}
        </details>}
        <ModelRoutingEditor value={draft} onChange={setDraft} profiles={profiles} group={!current.id} />
        <Space className="routing-save"><Button type="primary" loading={saving} onClick={() => void save()}>{t("common.save")}</Button></Space>
      </>}
      {!loading && !subjects.length && <BrandEmpty description={t("routing.noSubjects")} />}
    </Spin>
  </Card>;
}
