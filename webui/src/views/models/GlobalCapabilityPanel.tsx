import { useEffect, useRef, useState } from "react";
import { Alert, Button, Select, Spin, message } from "antd";
import { api, type LlmCapabilities, type LlmProfile } from "../../api";
import { SettingsSaveBar } from "../../components/SettingsDialog";
import { useT, type TKey } from "../../i18n/core";
import { useUnsavedChanges } from "../../settingsNavigation";

const FIELDS = ["speech", "transcribe", "visionRelay", "embedding"] as const;
type Field = typeof FIELDS[number];
const LABEL: Record<Field, TKey> = { speech: "models.capSpeech", transcribe: "models.capStt", visionRelay: "models.capVisionRelay", embedding: "models.capEmbed" };
const HELP: Record<Field, TKey> = { speech: "models.capSpeechHint", transcribe: "models.capSttHint", visionRelay: "models.capVisionHint", embedding: "models.capEmbedHint" };

export function GlobalCapabilityPanel({ profiles, active = true }: { profiles: LlmProfile[]; active?: boolean }) {
  const t = useT();
  const [saved, setSaved] = useState<LlmCapabilities>({ speech: "", transcribe: "", visionRelay: "", embedding: "" });
  const [draft, setDraft] = useState(saved);
  const [loading, setLoading] = useState(true);
  const [loaded, setLoaded] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const dirty = JSON.stringify(saved) !== JSON.stringify(draft);
  const hasDraft = useRef(dirty);
  hasDraft.current = dirty;
  useUnsavedChanges(dirty);
  useEffect(() => {
    if (!active || hasDraft.current) return;
    let current = true;
    setLoading(true);
    void api.llmCapabilities().then((result) => { if (current) { setSaved(result); setDraft(result); setLoaded(true); setError(""); } })
      .catch((failure) => { if (current) setError(String(failure)); }).finally(() => { if (current) setLoading(false); });
    return () => { current = false; };
  }, [attempt, active]);
  const options = profiles.flatMap((profile) => profile.models.length
    ? profile.models.map((model) => ({ value: `${profile.id}/${model.model}`, label: `${profile.name} / ${model.model}`, model: model.model }))
    : [{ value: profile.id, label: `${profile.name} / ${profile.model}`, model: profile.model }]);
  const save = async () => {
    setSaving(true); setError("");
    try {
      const result = await api.saveLlmCapabilities(draft);
      const next: LlmCapabilities = { speech: result.speech, transcribe: result.transcribe, visionRelay: result.visionRelay, embedding: result.embedding };
      setSaved(next); setDraft(next); message.success(t("models.capSaved"));
    }
    catch (failure) { setError(String(failure)); }
    finally { setSaving(false); }
  };
  return <Spin spinning={loading}>
    {error && <Alert type="error" showIcon title={error} action={!loaded && <Button size="small" onClick={() => setAttempt((value) => value + 1)}>{t("common.retry")}</Button>} />}
    <div className="global-cap-workspace">
      <header className="settings-detail-head"><div><span className="settings-eyebrow">{t("ux.globalCapabilities")}</span><h3>{t("models.capTitle")}</h3><p>{t("models.capGlobalHint")}</p></div></header>
      <div className="global-cap-list">{FIELDS.map((field) => <label className="global-cap-row" key={field}>
        <span><strong>{t(LABEL[field])}</strong><small>{t(HELP[field])}</small></span>
        <Select aria-label={t(LABEL[field])} disabled={saving || loading || !loaded} showSearch allowClear optionFilterProp="label" value={draft[field] || undefined}
          placeholder={t(field === "embedding" ? "models.capEmbedPh" : "models.followGlobalShort")} options={options}
          optionRender={(option) => <div className="routing-model-option"><span>{option.label}</span><small>{option.data.model}</small></div>}
          onChange={(value) => setDraft((current) => ({ ...current, [field]: value || "" }))} />
      </label>)}</div>
      <div className="routing-save"><SettingsSaveBar dirty={dirty} saving={saving} onSave={() => void save()} onReset={() => setDraft(saved)} scope={t("ux.globalCapabilities")} /></div>
    </div>
  </Spin>;
}
