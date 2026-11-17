import { useState } from "react";
import { Alert, Button, Input, Popconfirm, Select, Tag } from "antd";
import { ApiOutlined, DeleteOutlined, EditOutlined, PlusOutlined, SearchOutlined } from "@ant-design/icons";
import type { LlmProfile } from "../../api";
import { BrandEmpty } from "../../components/BrandEmpty";
import { costLabel, purposeLabel } from "../../modelRouting";
import { getTestRecord } from "../../testLog";
import { useT } from "../../i18n/core";
import { FORMAT_LABEL_KEY } from "./presets";

export function ProviderWorkspace({ profiles, active, testing, onEdit, onTest, onDelete, onCreate, onAllocate }: {
  profiles: LlmProfile[]; active?: string; testing: string;
  onEdit: (profile: LlmProfile, section?: string) => void; onTest: (id: string) => void;
  onDelete: (id: string) => void; onCreate: () => void; onAllocate: () => void;
}) {
  const t = useT();
  const [selected, setSelected] = useState("");
  const [query, setQuery] = useState("");
  const [modelQuery, setModelQuery] = useState("");
  const current = profiles.find((profile) => profile.id === selected) || profiles[0];
  const shown = profiles.filter((profile) => `${profile.name} ${profile.id} ${profile.baseUrl} ${profile.models.map((model) => model.model).join(" ")}`.toLowerCase().includes(query.trim().toLowerCase()));
  const record = current && getTestRecord(current.id);
  const modelMatches = current?.models.filter((model) => `${model.model} ${model.usageNotes || ""}`.toLowerCase().includes(modelQuery.trim().toLowerCase())) || [];
  const keys = current ? (current.apiKeys?.length || 0) + (current.apiKey ? 1 : 0) : 0;
  if (!profiles.length) return <div className="settings-welcome"><BrandEmpty description={t("models.none")} /><p>{t("ux.providerEmptyHint")}</p><Button type="primary" icon={<PlusOutlined />} onClick={onCreate}>{t("models.add")}</Button></div>;
  return <div className="settings-workspace provider-workspace">
    <aside className="settings-list-panel">
      <div className="settings-list-head"><strong>{t("models.tabProviders")}</strong><span className="settings-count">{profiles.length}</span></div>
      <Input prefix={<SearchOutlined />} placeholder={t("ux.searchProviders")} aria-label={t("ux.searchProviders")} allowClear value={query} onChange={(event) => setQuery(event.target.value)} />
      <nav className="settings-object-list" aria-label={t("models.tabProviders")}>{shown.map((profile) => {
        const keyCount = (profile.apiKeys?.length || 0) + (profile.apiKey ? 1 : 0);
        return <button type="button" key={profile.id} aria-current={profile.id === current?.id ? "page" : undefined} onClick={() => { setSelected(profile.id); setModelQuery(""); }}>
          <span className="settings-object-icon"><ApiOutlined /></span><span><strong>{profile.name}</strong><small>{profile.models.length} {t("routing.model")} · {profile.id}</small></span>
          <span className={`settings-status-dot ${keyCount ? "ready" : "warning"}`} title={t(keyCount ? "models.keyConfigured" : "models.keyMissing")} />
        </button>;
      })}{!shown.length && <p className="settings-list-empty">{t("ux.noResults")}</p>}</nav>
      <Button block icon={<PlusOutlined />} onClick={onCreate}>{t("models.add")}</Button>
    </aside>
    {current && <section className="settings-detail-panel model-card">
      <div className="allocation-mobile-select"><Select aria-label={t("models.tabProviders")} value={current.id} onChange={(id) => { setSelected(id); setModelQuery(""); }} options={profiles.map((profile) => ({ value: profile.id, label: profile.name }))} /></div>
      <header className="settings-detail-head">
        <div><span className="settings-eyebrow">{t("ux.connectionProfile")}</span><h3>{current.name}</h3><p className="mono">{current.baseUrl}</p></div>
        <div className="settings-detail-actions"><Button aria-label={t("common.edit")} icon={<EditOutlined />} onClick={() => onEdit(current)}>{t("common.edit")}</Button><Button aria-label={t("models.test")} icon={<ApiOutlined />} loading={testing === current.id} onClick={() => onTest(current.id)}>{t("models.test")}</Button></div>
      </header>
      <div className="provider-facts">
        <div><span>{t("models.apiFormat")}</span><strong>{t(FORMAT_LABEL_KEY[current.effectiveApiFormat || current.apiFormat || "openai"] ?? "models.fmtShort.openai")}</strong></div>
        <div><span>{t("models.keyLabel")}</span><strong className={!keys ? "routing-warning" : ""}>{keys ? t("models.keyConfiguredN", { n: keys }) : t("models.keyMissing")}</strong></div>
        <div><span>{t("models.clientIdentity")}</span><strong>{current.clientIdentity?.preset && current.clientIdentity.preset !== "default" ? current.clientIdentity.preset : t("models.clientDefault")}</strong></div>
        <div><span>{t("routing.providerDefault")}</span><strong className="mono">{current.model || t("models.capUnset")}</strong></div>
      </div>
      {!keys && <Alert type="warning" showIcon title={t("ux.missingKeyTitle")} action={<Button size="small" onClick={() => onEdit(current, "connection")}>{t("common.edit")}</Button>} />}
      {record && <Alert className="provider-test-result" type={record.ok ? "success" : "error"} showIcon title={record.ok ? t("models.lastTestOk", { ms: record.latencyMs ?? "?" }) : t("models.lastTestFail", { reason: record.reason || "?" })}
        description={<span>{new Date(record.at).toLocaleString()}{!record.ok && record.lastSuccess && <> · {t("ux.lastSuccess", { time: new Date(record.lastSuccess.at).toLocaleString(), ms: record.lastSuccess.latencyMs ?? "?" })}</>}</span>} />}
      {!record && <p className="settings-context-note">{t("ux.notTested")}</p>}
      <div className="settings-section-title"><div><h3>{t("routing.catalog")} <span className="settings-count">{current.models.length}</span></h3><p>{t("ux.catalogDescription")}</p></div><Button size="small" onClick={() => onEdit(current, "catalog")}>{t("ux.manageModels")}</Button></div>
      {!!current.models.length && <Input prefix={<SearchOutlined />} allowClear value={modelQuery} onChange={(event) => setModelQuery(event.target.value)} placeholder={t("models.pickerSearch")} aria-label={t("models.pickerSearch")} />}
      <div className="provider-model-list">{modelMatches.map((model) => <div key={model.model} className="provider-model-entry">
        <div><strong className="mono">{model.model}</strong><div className="provider-model-tags"><Tag>{costLabel(model.cost)}</Tag>{model.autoEligible && <Tag>{t("routing.autoEligible")}</Tag>}{model.vision && <Tag>{t("models.visionCap")}</Tag>}{model.audio && <Tag>{t("models.audioCap")}</Tag>}</div></div>
        {!!model.purposes?.length && <small>{model.purposes.map(purposeLabel).join(" · ")}</small>}{model.usageNotes && <p>{model.usageNotes}</p>}
      </div>)}</div>
      {!!current.models.length && !modelMatches.length && <p className="settings-list-empty">{t("models.pickerEmpty")}</p>}
      {!current.models.length && <div className="settings-list-empty"><p>{t("ux.noCatalog")}</p><Button icon={<PlusOutlined />} onClick={() => onEdit(current, "catalog")}>{t("models.addModel")}</Button></div>}
      <div className="provider-detail-foot"><span className="dim">{current.id === active && <Tag>{t("ux.systemDefault")}</Tag>}{current.fallback && t("models.fallbackTo", { id: current.fallback })}</span><Button onClick={onAllocate}>{t("ux.assignModels")}</Button>
        {profiles.length > 1 && <Popconfirm title={t("common.confirmDelete", { name: current.name })} onConfirm={() => onDelete(current.id)}><Button type="text" danger icon={<DeleteOutlined />} aria-label={t("common.delete")} /></Popconfirm>}
      </div>
    </section>}
  </div>;
}
