import { useState } from "react";
import { AutoComplete, Button, Collapse, Form, Input, InputNumber, Select, Switch, Tag } from "antd";
import { CloudDownloadOutlined, DeleteOutlined, PlusOutlined, SearchOutlined } from "@ant-design/icons";
import type { ProfileModel } from "../../api";
import { MODEL_PURPOSES, costLabel, purposeLabel } from "../../modelRouting";
import { useT } from "../../i18n/core";

/** Compact model rows; capabilities and catalog metadata are edited on demand. */
export function ProviderCatalogFields({ fetching, onFetch }: { fetching: boolean; onFetch: () => void }) {
  const t = useT();
  const form = Form.useFormInstance();
  const models: ProfileModel[] = Form.useWatch("models", { form, preserve: true }) || [];
  const [query, setQuery] = useState("");
  const [expanded, setExpanded] = useState<string[]>([]);
  return <>
    <div className="settings-toolbar">
      <Input prefix={<SearchOutlined />} value={query} onChange={(event) => setQuery(event.target.value)} allowClear aria-label={t("models.pickerSearch")} placeholder={t("models.pickerSearch")} />
      <Button icon={<CloudDownloadOutlined />} loading={fetching} onClick={onFetch}>{t("models.fetchModels")}</Button>
    </div>
    <Form.List name="models">{(fields, { add, remove }) => <>
      <div className="provider-catalog-fields">
        {fields.map(({ key, name, ...rest }) => {
          const model = models[name];
          const visible = !query.trim() || !model?.model || model.model.toLowerCase().includes(query.trim().toLowerCase());
          return <div className="provider-model-field" key={key} hidden={!visible}>
            <div className="provider-model-name">
              <Form.Item {...rest} name={[name, "model"]} rules={[{ required: true, whitespace: true, message: t("models.modelNameRequired") }]}>
                <Input aria-label={`${t("routing.model")} ${name + 1}`} placeholder="provider-model-name" />
              </Form.Item>
              <Button type="text" icon={<DeleteOutlined />} aria-label={t("common.delete")} onClick={() => remove(name)} />
            </div>
            <Collapse size="small" activeKey={expanded} onChange={(keys) => setExpanded(typeof keys === "string" ? [keys] : keys)} items={[{
              key: String(key), label: <span className="provider-model-summary"><span>{t("ux.modelDetails")}</span><Tag>{costLabel(model?.cost)}</Tag>
                {model?.vision && <Tag>{t("models.visionCap")}</Tag>}{model?.audio && <Tag>{t("models.audioCap")}</Tag>}
                {model?.autoEligible && <Tag>{t("routing.autoEligible")}</Tag>}</span>,
              children: <>
                <div className="settings-switch-grid">
                  <Form.Item {...rest} name={[name, "vision"]} label={t("models.visionCap")} valuePropName="checked" extra={t("ux.visionHint")}><Switch /></Form.Item>
                  <Form.Item {...rest} name={[name, "audio"]} label={t("models.audioCap")} valuePropName="checked" extra={t("ux.audioHint")}><Switch /></Form.Item>
                  <Form.Item {...rest} name={[name, "autoEligible"]} label={t("routing.autoEligible")} valuePropName="checked" extra={t("routing.autoEligibleHint")}><Switch /></Form.Item>
                </div>
                <Form.Item {...rest} name={[name, "purposes"]} label={t("routing.purposes")}>
                  <Select mode="multiple" maxTagCount="responsive" options={MODEL_PURPOSES.map((purpose) => ({ value: purpose, label: purposeLabel(purpose) }))} />
                </Form.Item>
                <div className="form-grid-2">
                  <Form.Item {...rest} name={[name, "cost"]} label={t("routing.cost")}><Select placeholder={costLabel("unknown")} options={(["unknown", "economy", "balanced", "premium"] as const).map((cost) => ({ value: cost, label: costLabel(cost) }))} /></Form.Item>
                  <Form.Item {...rest} name={[name, "contextWindow"]} label={t("routing.contextWindow")}><InputNumber min={0} max={100000000} precision={0} style={{ width: "100%" }} /></Form.Item>
                </div>
                <Form.Item {...rest} name={[name, "usageNotes"]} label={t("routing.usageNotes")}><Input.TextArea rows={2} maxLength={2000} placeholder={t("routing.usageNotesPlaceholder")} /></Form.Item>
              </>,
            }]} />
          </div>;
        })}
      </div>
      {query && !models.some((model) => !model?.model || model.model.toLowerCase().includes(query.trim().toLowerCase())) && <p className="settings-list-empty">{t("models.pickerEmpty")}</p>}
      <Button type="dashed" block icon={<PlusOutlined />} onClick={() => { setQuery(""); add({ model: "", vision: false, audio: false, autoEligible: false, cost: "unknown" }); }}>{t("models.addModel")}</Button>
    </>}</Form.List>
    <div className="settings-context-note">{t("models.modelListExtra")}</div>
    <Form.Item name="model" label={t("routing.providerDefault")} extra={t("ux.providerDefaultHint")}>
      <AutoComplete allowClear filterOption={(input, option) => String(option?.value).toLowerCase().includes(input.toLowerCase())} options={models.filter((model) => model?.model?.trim()).map((model) => ({ value: model.model }))} />
    </Form.Item>
  </>;
}
