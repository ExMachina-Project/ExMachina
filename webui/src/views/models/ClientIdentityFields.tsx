import React from "react";
import { Button, Form, Input, Select } from "antd";
import type { ClientIdentity } from "../../api";
import { useT } from "../../i18n/core";

const DEFAULT_IDENTITY: ClientIdentity = { preset: "default" };

export function ClientIdentityFields(): React.ReactElement {
  const t = useT();
  const form = Form.useFormInstance();
  const preset = Form.useWatch(["clientIdentity", "preset"], form) ?? "default";
  const disabled = preset === "default";
  return (
    <>
      <Form.Item name={["clientIdentity", "preset"]} label={t("models.clientIdentity")} extra={t("models.clientIdentityHint")}>
        <Select data-testid="client-identity-preset" options={[
          { value: "default", label: t("models.clientDefault") },
          { value: "claude-code", label: "Claude Code" },
          { value: "codex", label: "Codex CLI" },
          { value: "opencode", label: "OpenCode" },
          { value: "cline", label: "Cline" },
          { value: "custom", label: t("models.clientCustom") },
        ]} />
      </Form.Item>
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
      <Button size="small" onClick={() => form.setFieldsValue({ clientIdentity: { preset: "default", userAgent: "" }, clientHeaders: "" })}>
        {t("models.clientReset")}
      </Button>
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
    if (!/^(originator|x-app|x-title|http-referer|editor-version|editor-plugin-version|x-client-[a-z0-9!#$%&'*+.^_`|~-]+|x-stainless-[a-z0-9!#$%&'*+.^_`|~-]+)$/.test(lower)
      || names.has(lower) || typeof value !== "string" || !/^[\x20-\x7e]{0,2048}$/.test(value)) {
      throw new Error(errorMessage);
    }
    names.add(lower);
  }
  return { preset: identity.preset, userAgent: identity.userAgent ?? "", headers: headers as Record<string, string> };
}
