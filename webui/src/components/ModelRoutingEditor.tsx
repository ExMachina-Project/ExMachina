import { useState } from "react";
import { Alert, Button, Input, Select, Tabs, Tag } from "antd";
import { AudioOutlined, ControlOutlined, FileTextOutlined } from "@ant-design/icons";
import type { LlmProfile } from "../api";
import { MODEL_PURPOSES, purposeLabel, routingOptions, type AssignmentMode, type ModelAssignment, type ModelPurpose, type ModelRouting } from "../modelRouting";
import { useT } from "../i18n/core";

const TASKS = MODEL_PURPOSES.slice(0, 6);
const CAPABILITIES = MODEL_PURPOSES.slice(6);

export function ModelRoutingEditor({ value, onChange, profiles, group = false, inheritedLabel, showScope = true }: {
  value: ModelRouting; onChange: (value: ModelRouting) => void; profiles: LlmProfile[]; group?: boolean; inheritedLabel?: string; showScope?: boolean;
}) {
  const t = useT();
  const [section, setSection] = useState("tasks");
  const inherit = inheritedLabel || t(group ? "ux.inheritSystem" : "ux.inheritOwner");
  const customized = (purposes: readonly ModelPurpose[]) => purposes.filter((purpose) => value.purposes?.[purpose]?.mode && value.purposes[purpose]?.mode !== "inherit").length;
  const binding = (purpose?: ModelPurpose) => {
    const current: ModelAssignment = (purpose ? value.purposes?.[purpose] : value.default) ?? { mode: "inherit" };
    const label = purpose ? purposeLabel(purpose) : t("routing.default");
    const options = routingOptions(profiles, purpose);
    const automatic = routingOptions(profiles, purpose || "chat").filter((option) => option.suitable);
    const chosen = options.find((option) => option.value === current.model);
    const candidates = automatic.filter((option) => current.candidates?.length ? current.candidates.includes(option.value) : option.eligible);
    const update = (next: ModelAssignment) => onChange(purpose ? { ...value, purposes: { ...value.purposes, [purpose]: next } } : { ...value, default: next });
    return <div className={`routing-field ${purpose ? "" : "routing-default-field"}`} key={purpose || "default"} data-routing-purpose={purpose || "default"}>
      <div className="routing-field-label"><strong>{label}</strong><span>{purpose ? t(`ux.purpose.${purpose}`) : t("ux.defaultHint")}</span></div>
      <Select aria-label={`${label} ${t("routing.mode")}`} value={current.mode} className="routing-mode" onChange={(mode: AssignmentMode) => update({ mode })}
        options={[
          { value: "inherit", label: t("routing.inherit") }, { value: "fixed", label: t("routing.fixed") },
          ...(purpose !== "embedding" ? [{ value: "auto", label: t("routing.auto") }] : []),
          ...(purpose && CAPABILITIES.includes(purpose) ? [{ value: "disabled", label: t("routing.disabled") }] : []),
        ]} />
      {current.mode === "inherit" && !purpose && <div className="routing-field-help dim">{inherit}</div>}
      {current.mode === "disabled" && <div className="routing-field-help dim">{t("ux.capabilityOff")}</div>}
      {current.mode === "fixed" && <>
        <Select aria-label={`${label} ${t("routing.model")}`} showSearch optionFilterProp="label" value={current.model || undefined}
          status={!current.model ? "warning" : undefined} placeholder={t("routing.chooseModel")} options={options}
          optionRender={(option) => <div className="routing-model-option"><span>{option.label}</span>{option.data.notes && <small>{option.data.notes}</small>}</div>}
          onChange={(model) => update({ mode: "fixed", model })} />
        <div className="routing-field-help dim">{chosen?.notes || t("ux.manualHint")}</div>
      </>}
      {current.mode === "auto" && <>
        <Select aria-label={`${label} ${t("routing.candidates")}`} mode="multiple" showSearch optionFilterProp="label" maxTagCount="responsive"
          value={current.candidates ?? []} placeholder={t("routing.catalogCandidates")} options={automatic}
          onChange={(next) => update({ mode: "auto", candidates: next })} />
        <div className="routing-field-help"><span className={candidates.length ? "dim" : "routing-warning"}>{t("ux.candidateCount", { n: candidates.length })}</span> <span className="dim">{t("routing.autoHint")}</span></div>
      </>}
    </div>;
  };
  const purposeSection = (purposes: readonly ModelPurpose[]) => <>
    <div className="routing-section-toolbar"><span className="dim">{t("ux.overrideCount", { n: customized(purposes) })}</span>
      <Button size="small" type="text" disabled={!customized(purposes)} onClick={() => {
        const next = { ...value.purposes };
        purposes.forEach((purpose) => { delete next[purpose]; });
        onChange({ ...value, purposes: next });
      }}>{t("ux.inheritAll")}</Button>
    </div>
    <div className="routing-fields">{purposes.map((purpose) => binding(purpose))}</div>
  </>;
  return <div className="model-routing-editor">
    {showScope && <p className="routing-scope-hint">{t(group ? "routing.groupHint" : "routing.agentHint")}</p>}
    {!profiles.length && <Alert type="warning" showIcon title={t("routing.noModels")} />}
    {binding()}
    <Tabs className="routing-purpose-tabs" activeKey={section} onChange={setSection} items={[
      { key: "tasks", label: <><FileTextOutlined /> {t("ux.taskModels")} <span className="settings-count">{customized(TASKS)}</span></>, children: purposeSection(TASKS) },
      { key: "capabilities", label: <><AudioOutlined /> {t("ux.capabilityModels")} <span className="settings-count">{customized(CAPABILITIES)}</span></>, children: purposeSection(CAPABILITIES) },
      { key: "instructions", label: <><ControlOutlined /> {t("ux.autoPolicy")}</>, children: <label className="routing-instructions"><span>{t("routing.instructions")}</span>
        <Input.TextArea aria-label={t("routing.instructions")} rows={5} maxLength={4000} showCount value={value.instructions || ""}
          placeholder={t("routing.instructionsPlaceholder")} onChange={(event) => onChange({ ...value, instructions: event.target.value })} />
        <p className="dim">{t("ux.autoInstructionsHint")}</p>
      </label> },
    ]} />
    <div className="settings-context-note">{t("routing.failureHint")}</div>
    <div className="routing-mode-key"><Tag>{t("routing.fixed")}</Tag><span>{t("ux.manualHint")}</span></div>
  </div>;
}
