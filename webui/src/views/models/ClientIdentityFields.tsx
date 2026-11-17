import React, { useState } from "react";
import { Alert, Button, Form, Input, Select, Space, Tag, message } from "antd";
import { api, type ClientIdentity, type ClientIdentityPreview } from "../../api";
import { useT } from "../../i18n/core";

const DEFAULT_IDENTITY: ClientIdentity = { preset: "default" };

export function ClientIdentityFields({ profileId }: { profileId?: string }): React.ReactElement {
  const t = useT();
  const form = Form.useFormInstance();
  const preset = Form.useWatch(["clientIdentity", "preset"], form) ?? "default";
  const disabled = preset === "default";
  const draft = Form.useWatch((values) => JSON.stringify([values.baseUrl, values.apiFormat, values.model, values.clientIdentity, values.clientHeaders]), form);
  const [inspected, setInspected] = useState<{ draft: string; value: ClientIdentityPreview } | null>(null);
  const preview = inspected?.draft === draft ? inspected.value : null;
  const [busy, setBusy] = useState(false);
  const versions = { "claude-code": "2.1.326", codex: "0.169.0", opencode: "2.3.0", cline: "4.1.26" };
  const inspect = async (probe: boolean) => {
    setBusy(true);
    try {
      await form.validateFields([["clientIdentity", "version"], ["clientIdentity", "userAgent"], "clientHeaders"]);
      const values = form.getFieldsValue(true);
      const body = { id: profileId, baseUrl: values.baseUrl, apiFormat: values.apiFormat, model: values.model,
        clientIdentity: parseClientIdentity(values.clientIdentity, values.clientHeaders, t("models.clientHeadersInvalid")) };
      if (probe && !body.id) { message.info(t("models.clientSaveToTest")); return; }
      if (probe) {
        const result = await api.testLlmProfile(body.id, body);
        setInspected(result.identity ? { draft, value: result.identity } : null);
        if (result.ok) message.success(t("models.clientSent", { status: result.status ?? 200 }));
        else message.error(result.error || result.message || t("models.clientProbeFailed", { status: result.status ?? "?" }));
      } else setInspected({ draft, value: await api.previewClientIdentity(body) });
    } catch (error) { message.error(String(error)); }
    finally { setBusy(false); }
  };
  return (
    <>
      <Form.Item name={["clientIdentity", "preset"]} label={t("models.clientIdentity")} extra={t("models.clientIdentityHint")}>
        <Select data-testid="client-identity-preset" onChange={(value: ClientIdentity["preset"]) => {
          form.setFieldsValue({ clientIdentity: { preset: value, version: "", requestMode: "native", userAgent: "", headers: {} }, clientHeaders: "" });
          setInspected(null);
        }} options={[
          { value: "default", label: t("models.clientDefault") },
          { value: "claude-code", label: "Claude Code" },
          { value: "codex", label: "Codex CLI" },
          { value: "opencode", label: "OpenCode" },
          { value: "cline", label: "Cline" },
          { value: "custom", label: t("models.clientCustom") },
        ]} />
      </Form.Item>
      <div className="form-grid-2">
        <Form.Item name={["clientIdentity", "requestMode"]} label={t("models.clientRequestMode")}>
          <Select disabled={disabled || preset === "custom"} data-testid="client-identity-mode" options={[
            { value: "native", label: t("models.clientNative") }, { value: "headers", label: t("models.clientHeadersOnly") },
          ]} />
        </Form.Item>
        <Form.Item name={["clientIdentity", "version"]} label={t("models.clientVersion")} extra={t("models.clientVersionHint")}
          rules={[{ pattern: /^[A-Za-z0-9._+-]{0,64}$/, message: t("models.clientVersionInvalid") }]}>
          <Input disabled={disabled || preset === "custom"} data-testid="client-identity-version" maxLength={64}
            placeholder={versions[preset as keyof typeof versions] ?? ""} />
        </Form.Item>
      </div>
      <Form.Item name={["clientIdentity", "userAgent"]} label="User-Agent" extra={t("models.clientAgentHint")}
        dependencies={[["clientIdentity", "preset"]]}
        rules={[{ validator: async (_, value: string | undefined) => {
          if (form.getFieldValue(["clientIdentity", "preset"]) !== "default" && !/^[\x20-\x7e]{0,2048}$/.test(value ?? "")) {
            throw new Error(t("models.clientValueInvalid"));
          }
        } }]}>
        <Input data-testid="client-identity-user-agent" disabled={disabled} placeholder="my-agent/1.0" maxLength={2048} />
      </Form.Item>
      <Form.Item name="clientHeaders" label={t("models.clientHeaders")} extra={t("models.clientHeadersHint")}
        dependencies={[["clientIdentity", "preset"]]}
        rules={[{ validator: async (_, value: string | undefined) => {
          parseClientIdentity(form.getFieldValue("clientIdentity"), value, t("models.clientHeadersInvalid"));
        } }]}>
        <Input.TextArea data-testid="client-identity-headers" disabled={disabled} rows={4} placeholder={'{\n  "x-title": "My Agent"\n}'} />
      </Form.Item>
      <Space wrap>
      <Button size="small" onClick={() => { form.setFieldsValue({ clientIdentity: { preset: "default", version: "", requestMode: "native", userAgent: "" }, clientHeaders: "" }); setInspected(null); }}>
        {t("models.clientReset")}
      </Button>
      <Button size="small" data-testid="client-identity-preview-button" loading={busy} onClick={() => void inspect(false)}>{t("models.clientPreview")}</Button>
      <Button size="small" data-testid="client-identity-test-button" loading={busy} onClick={() => void inspect(true)}>{t("models.clientTestDraft")}</Button>
      </Space>
      {preview && <Alert className="client-identity-preview" type="info" showIcon title={t("models.clientEffective")}
        description={<div data-testid="client-identity-preview">
          <div><Tag>{preview.effectiveFormat}</Tag><code>{preview.path}</code></div>
          {!preview.nativeCompatible && <p role="status">{t("models.clientProtocolMismatch")}</p>}
          <pre>{Object.entries(preview.headers).map(([name, value]) => `${name}: ${value}`).join("\n")}</pre>
          {preview.metadata && <p>{t("models.clientMetadata")}</p>}
          <p>{t("models.clientPreviewScope")}</p>
        </div>} />}
    </>
  );
}

/** JSON 字段在提交与拉取远端模型时使用同一解析逻辑。 */
export function parseClientIdentity(identity: ClientIdentity | undefined, rawHeaders: string | undefined, errorMessage: string): ClientIdentity {
  if (!identity || identity.preset === "default") return DEFAULT_IDENTITY;
  let headers: unknown;
  try {
    headers = JSON.parse(rawHeaders?.trim() || "{}");
  } catch {
    throw new Error(errorMessage);
  }
  if (!headers || typeof headers !== "object" || Array.isArray(headers)) throw new Error(errorMessage);
  const entries = Object.entries(headers);
  const names = new Set<string>();
  if (entries.length > 32) throw new Error(errorMessage);
  for (const [name, value] of entries) {
    const lower = name.toLowerCase();
    if (!/^(originator|version|x-app|x-title|x-openrouter-title|x-openrouter-categories|http-referer|editor-version|editor-plugin-version|anthropic-beta|anthropic-dangerous-direct-browser-access|x-claude-code-session-id|x-session-id|x-session-affinity|session_id|x-task-id|x-is-multiroot|x-platform|x-platform-version|x-core-version|x-opencode-[a-z0-9!#$%&'*+.^_`|~-]+|x-client-[a-z0-9!#$%&'*+.^_`|~-]+|x-stainless-[a-z0-9!#$%&'*+.^_`|~-]+)$/.test(lower)
      || names.has(lower) || typeof value !== "string" || !/^[\x20-\x7e]{0,2048}$/.test(value)) {
      throw new Error(errorMessage);
    }
    names.add(lower);
  }
  if (!/^[A-Za-z0-9._+-]{0,64}$/.test(identity.version ?? "")) throw new Error(errorMessage);
  return { preset: identity.preset, requestMode: identity.requestMode ?? "native", version: identity.version ?? "", userAgent: identity.userAgent ?? "", headers: headers as Record<string, string> };
}
