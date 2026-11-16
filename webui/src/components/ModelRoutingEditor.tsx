import { Alert, Collapse, Input, Select, Space, Tag } from "antd";
import type { LlmProfile } from "../api";
import { MODEL_PURPOSES, costLabel, purposeLabel, routingOptions, type AssignmentMode, type ModelAssignment, type ModelPurpose, type ModelRouting } from "../modelRouting";
import { useT } from "../i18n/core";

export function ModelRoutingEditor({ value, onChange, profiles, group = false }: {
  value: ModelRouting; onChange: (value: ModelRouting) => void; profiles: LlmProfile[]; group?: boolean;
}) {
  const t = useT();
  const binding = (purpose?: ModelPurpose) => {
    const current: ModelAssignment = (purpose ? value.purposes?.[purpose] : value.default) ?? { mode: "inherit" };
    const label = purpose ? purposeLabel(purpose) : t("routing.default");
    const options = routingOptions(profiles, purpose);
    const update = (next: ModelAssignment) => onChange(purpose ? { ...value, purposes: { ...value.purposes, [purpose]: next } } : { ...value, default: next });
    return <div className="routing-field" key={purpose || "default"} data-routing-purpose={purpose || "default"}>
      <div className="routing-field-label">{label}</div>
      <Select aria-label={`${label} ${t("routing.mode")}`} value={current.mode} className="routing-mode" onChange={(mode: AssignmentMode) => update({ mode })}
        options={[
          { value: "inherit", label: t("routing.inherit") }, { value: "fixed", label: t("routing.fixed") },
          ...(purpose !== "embedding" ? [{ value: "auto", label: t("routing.auto") }] : []),
          ...(purpose && !["chat", "planning", "coding", "review", "summary", "research"].includes(purpose) ? [{ value: "disabled", label: t("routing.disabled") }] : []),
        ]} />
      {current.mode === "fixed" && <Select aria-label={`${label} ${t("routing.model")}`} showSearch optionFilterProp="label" value={current.model || undefined}
        placeholder={t("routing.chooseModel")} options={options} onChange={(model) => update({ mode: "fixed", model })} />}
      {current.mode === "auto" && <>
        <Select aria-label={`${label} ${t("routing.candidates")}`} mode="multiple" showSearch optionFilterProp="label" maxTagCount="responsive"
          value={current.candidates ?? []} placeholder={t("routing.catalogCandidates")} options={options.filter((o) => o.suitable)}
          onChange={(candidates) => update({ mode: "auto", candidates })} />
        <span className="dim routing-field-help">{t("routing.autoHint")}</span>
      </>}
    </div>;
  };
  return <div className="model-routing-editor">
    <Alert type="info" showIcon title={t(group ? "routing.groupHint" : "routing.agentHint")} />
    {binding()}
    <Collapse className="routing-purposes" size="small" defaultActiveKey={["purposes"]} items={[{
      key: "purposes", label: t("routing.byPurpose"), children: MODEL_PURPOSES.map((purpose) => binding(purpose)),
    }]} />
    <label className="routing-instructions"><span>{t("routing.instructions")}</span>
      <Input.TextArea aria-label={t("routing.instructions")} rows={3} maxLength={4000} showCount value={value.instructions || ""}
        placeholder={t("routing.instructionsPlaceholder")} onChange={(e) => onChange({ ...value, instructions: e.target.value })} />
    </label>
    <div className="dim routing-field-help">{t("routing.failureHint")}</div>
    <Space size={4} wrap className="routing-cost-key">{(["economy", "balanced", "premium"] as const).map((cost) => <Tag key={cost}>{costLabel(cost)}</Tag>)}</Space>
  </div>;
}
