/** Provider catalogs describe models; assignments edit individual agents and group members. */
import React, { useCallback, useEffect, useMemo, useState } from "react";
import {
  Button, Form, Input, Modal, Select, Spin, Tabs, message,
} from "antd";
import { MaskedPasswordInput } from "../../components/MaskedInput";
import {
  ApiOutlined, ControlOutlined, DeleteOutlined, KeyOutlined, PlusOutlined, ReloadOutlined, SafetyCertificateOutlined,
} from "@ant-design/icons";
import { api, KEY_MASK, KEY_CLEAR, type ClientIdentity, type LlmProfile, type LlmProfilesInfo, type ProfileModel } from "../../api";
import { putTestRecord } from "../../testLog";
import { PageHeader } from "../../components/PageHeader";
import { SettingsDialog } from "../../components/SettingsDialog";
import { useT } from "../../i18n/core";

/** 厂商预设：选中即填端点、协议与常用模型名（模型名可在表单里改） */
import { PRESETS, PRESET_LABEL, CUSTOM, API_FORMAT_KEYS, type KeyForm, type Tab, liveRows } from "./presets";
import { ModelPicker } from "./ModelPicker";
import { ClientIdentityFields, parseClientIdentity } from "./ClientIdentityFields";
import { ModelAllocationPanel } from "./ModelAllocationPanel";
import { ProviderCatalogFields } from "./ProviderCatalogFields";
import { ProviderWorkspace } from "./ProviderWorkspace";
import { GlobalCapabilityPanel } from "./GlobalCapabilityPanel";

export function ModelsView({ active = true }: { active?: boolean } = {}): React.ReactElement {
  const t = useT();
  const [info, setInfo] = useState<LlmProfilesInfo | null>(null);
  /** 页内分页：providers = 提供商与模型清单；caps = 功能分配（能力模型） */
  const [tab, setTab] = useState<Tab>("providers");
  const [loading, setLoading] = useState(false);
  const [modal, setModal] = useState(false);
  const [editing, setEditing] = useState<LlmProfile | null>(null);
  const [testing, setTesting] = useState<string>("");
  /** 测试留存版本号：putTestRecord 后自增触发重渲染读新记录 */
  const [testTick, setTestTick] = useState(0);
  void testTick;
  const [fetchingModels, setFetchingModels] = useState(false);
  const [saving, setSaving] = useState(false);
  /** 模型选择弹窗：远端清单拉取后先展示，勾选后才进表单（避免全量塞入） */
  const [picker, setPicker] = useState<{ open: boolean; models: string[]; selected: string[] } | null>(null);
  const [pickerSaving, setPickerSaving] = useState(false);
  const [form] = Form.useForm();
  const watched = Form.useWatch((values) => JSON.stringify(values), { form, preserve: true });
  const [baseline, setBaseline] = useState("");
  const [editorSection, setEditorSection] = useState("connection");
  const dirty = modal && watched !== baseline;
  const profiles = useMemo(() => info?.profiles ?? [], [info]);

  /** 拉取端点可用模型：编辑态按档案 id（沿用已存密钥）；新建态用表单当前端点 + 首行明文密钥。
   *  拉取成功后弹选择窗，勾选的模型才合入表单清单（保留既有行的能力标记，仅补缺）。 */
  const fetchModels = async () => {
    setFetchingModels(true);
    try {
      await form.validateFields([["clientIdentity", "userAgent"], "clientHeaders"]);
      const all = form.getFieldsValue(true) as KeyForm & { baseUrl?: string; apiFormat?: string; clientIdentity?: ClientIdentity; clientHeaders?: string };
      const clientIdentity = parseClientIdentity(all.clientIdentity, all.clientHeaders, t("models.clientHeadersInvalid"));
      const rows = liveRows(all.keys);
      if (editing && (!rows[0]?.value || rows[0].value === KEY_MASK)
        && ((all.baseUrl || "").trim() !== editing.baseUrl || (all.apiFormat || "openai") !== (editing.apiFormat || "openai"))) {
        message.warning(t("ux.saveConnectionToFetch"));
        return;
      }
      let body: { id?: string; baseUrl?: string; apiFormat?: string; apiKey?: string; clientIdentity: ClientIdentity } = editing
        ? { id: editing.id, baseUrl: all.baseUrl, apiFormat: all.apiFormat, clientIdentity }
        : {
            baseUrl: (all.baseUrl ?? "").trim(),
            apiFormat: all.apiFormat ?? "openai",
            apiKey: rows[0]?.value ?? "",
            clientIdentity,
          };
      if (!editing && (!body.baseUrl || !rows[0]?.value)) {
        message.warning(t("models.fetchNeedsKey"));
        return;
      }
      if (editing && rows[0]?.value && rows[0].value !== KEY_MASK) {
        // 编辑态且首行填了新明文密钥：用明文探测（与保存语义一致）
        body = { baseUrl: all.baseUrl, apiFormat: all.apiFormat ?? "openai", apiKey: rows[0].value, clientIdentity };
      }
      const r = await api.listProviderModels(body);
      if (!r.ok) {
        message.error(t("models.fetchFailed", { err: (r.error ?? `HTTP ${r.status ?? ""}`).slice(0, 140) }));
        return;
      }
      if (r.configured === false) {
        message.warning(r.message ?? t("models.testNoKey"));
        return;
      }
      if (r.models.length === 0) {
        message.warning(r.message ?? t("models.fetchEmpty"));
        return;
      }
      // 已在表单里的模型不出现在选择窗（弹窗只承担「补充哪些」的选择）
      const current: ProfileModel[] = (form.getFieldValue("models") ?? []).filter((m: ProfileModel) => m?.model?.trim());
      const known = new Set(current.map((m) => m.model));
      const fresh = r.models.filter((m) => !known.has(m));
      if (fresh.length === 0) {
        message.info(t("models.fetchAllKnown"));
        return;
      }
      setPicker({ open: true, models: fresh, selected: [] });
    } catch (e) {
      message.error(t("models.fetchFailed", { err: String(e).slice(0, 140) }));
    } finally {
      setFetchingModels(false);
    }
  };

  /** 选择窗确认：勾选的模型合入表单清单（既有行不动，仅追加勾选项） */
  const applyPickedModels = async () => {
    if (!picker) return;
    const picked = picker.selected;
    setPickerSaving(true);
    try {
      if (picked.length > 0) {
        const current: ProfileModel[] = (form.getFieldValue("models") ?? []).filter((m: ProfileModel) => m?.model?.trim());
        const known = new Set(current.map((m) => m.model));
        const merged: ProfileModel[] = [
          ...current,
          ...picked.filter((m) => !known.has(m)).map((m) => ({ model: m, vision: false, audio: false })),
        ];
        form.setFieldsValue({ models: merged });
        message.success(t("models.fetchOk", { n: picker.models.length, add: merged.length - current.length }));
      }
      setPicker(null);
    } finally {
      setPickerSaving(false);
    }
  };

  const load = useCallback(async () => {
    setLoading(true);
    try {
      setInfo(await api.llmProfiles());
    } catch (error) {
      message.error(String(error));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (active) void load();
  }, [load, active]);

  const openCreate = () => {
    setEditing(null);
    form.resetFields();
    form.setFieldsValue({ preset: "openai", name: "OpenAI", baseUrl: PRESETS[0].baseUrl, apiFormat: "openai", clientIdentity: { preset: "default", requestMode: "native" }, clientHeaders: "", keys: [{ value: "" }], models: [] });
    setBaseline(JSON.stringify(form.getFieldsValue(true)));
    setEditorSection("connection");
    setModal(true);
  };

  const openEdit = (p: LlmProfile, section = "connection") => {
    setEditing(p);
    form.resetFields();
    const preset = PRESETS.find((x) => x.baseUrl === p.baseUrl)?.key ?? CUSTOM;
    // 密钥行合并为一行序列：主 Key 掩码行（keep: -1 = 主沿用，不入池）+ Key 池掩码行（keep = 旧池索引）
    const maskRows = [{ value: p.apiKey ? KEY_MASK : "", keep: -1 }];
    const poolRows = (p.apiKeys ?? []).filter((key) => key?.trim()).map((_, index) => ({ value: KEY_MASK, keep: index }));
    const rows: ProfileModel[] = p.models.map((m) => ({ ...m }));
    form.setFieldsValue({
      preset,
      id: p.id,
      name: p.name,
      baseUrl: p.baseUrl,
      apiFormat: p.apiFormat || "openai",
      clientIdentity: { requestMode: "native", ...(p.clientIdentity ?? { preset: "default" }) },
      clientHeaders: Object.keys(p.clientIdentity?.headers ?? {}).length ? JSON.stringify(p.clientIdentity?.headers, null, 2) : "",
      models: rows,
      model: p.model,
      fallback: p.fallback ?? undefined,
      keys: [...maskRows, ...poolRows],
    });
    setBaseline(JSON.stringify(form.getFieldsValue(true)));
    setEditorSection(section);
    setModal(true);
  };

  /** 选中预设：填「端点 / 协议」，密钥永远要自己贴 */
  const applyPreset = (key: string) => {
    const preset = PRESETS.find((p) => p.key === key);
    if (!preset) {
      form.setFieldsValue({ apiFormat: "openai" });
      return;
    }
    form.setFieldsValue({
      baseUrl: preset.baseUrl,
      apiFormat: preset.apiFormat,
      name: form.getFieldValue("name") || preset.name,
    });
  };

  const submit = async () => {
    // 校验规则用 validateFields；取值用 getFieldsValue(true)——
    // 掩码行的 keep 无控件绑定，validateFields 只返回已注册字段会把它丢掉
    try {
      await form.validateFields();
    } catch (error) {
      const first = (error as { errorFields?: { name: (string | number)[] }[] }).errorFields?.[0]?.name?.[0];
      setEditorSection(first === "models" || first === "model" ? "catalog" : first === "clientIdentity" || first === "clientHeaders" ? "identity" : first === "fallback" ? "advanced" : "connection");
      return;
    }
    const v = form.getFieldsValue(true);
    setSaving(true);
    try {
      const all = form.getFieldsValue(true) as KeyForm & Record<string, unknown>;
      const existing = editing;
      // Explicit references keep old pool entries stable when rows are removed or reordered.
      const rawRows = (all.keys ?? []).map((r) => ({
        value: (r?.value ?? "").trim(),
        keep: r?.keep,
      }));
      const oldPoolSize = existing ? (existing.apiKeys ?? []).filter((k) => k && k.trim()).length : 0;
      let apiKey: string;
      let apiKeys: (string | { keep: number })[];
      let clearKeys = false;
      if (existing) {
        const main = rawRows.find((row) => row.keep === -1);
        apiKey = main?.value || KEY_CLEAR;
        apiKeys = Array.from({ length: oldPoolSize }, (_, index): string | { keep: number } => {
          const row = rawRows.find((item) => item.keep === index);
          return row?.value === KEY_MASK ? { keep: index } : row?.value || KEY_CLEAR;
        });
        apiKeys.push(...rawRows.filter((row) => row.keep === undefined && row.value && row.value !== KEY_MASK).map((row) => row.value));
        clearKeys = !rawRows.some((row) => row.value);
      } else {
        const fresh = rawRows.filter((r) => r.value !== "" && r.value !== KEY_MASK).map((r) => r.value);
        [apiKey = "", ...apiKeys] = fresh;
      }
      await api.saveLlmProfile({
        id: existing?.id ?? ((v.id ?? "").trim() || undefined),
        name: (v.name ?? "").trim() || t("models.unnamed"),
        baseUrl: (v.baseUrl ?? "").trim(),
        apiFormat: v.apiFormat ?? "openai",
        clientIdentity: parseClientIdentity(v.clientIdentity, v.clientHeaders, t("models.clientHeadersInvalid")),
        apiKey,
        apiKeys,
        clearKeys,
        models: ((v.models ?? []) as ProfileModel[])
          .filter((m) => m.model && m.model.trim())
          .map((m) => ({ ...m, model: m.model.trim(), vision: !!m.vision, audio: !!m.audio })),
        model: v.model || undefined,
        fallback: v.fallback ?? "",
      });
      message.success(existing ? t("models.savedName", { name: v.name || existing.name }) : t("models.added", { name: v.name }));
      setModal(false);
      await load();
    } catch (e) {
      message.error(t("common.saveFailed", { err: String(e) }));
    } finally {
      setSaving(false);
    }
  };

  const remove = async (id: string) => {
    try {
      await api.deleteLlmProfile(id);
      message.success(t("models.deletedName", { id }));
      await load();
    } catch (e) {
      message.error(t("models.deleteFailed", { err: String(e) }));
    }
  };

  const test = async (id?: string) => {
    const key = id ?? "__resolved__";
    setTesting(key);
    try {
      const r = await api.testLlmProfile(id);
      // 本地留存（刷新不丢）：失败保留上次成功由展示层合并——记录只反映当次
      if (id) {
        putTestRecord(id, {
          at: new Date().toISOString(),
          ok: !!r.ok,
          latencyMs: r.latencyMs,
          reason: r.ok ? undefined : (r.error ?? r.message ?? `HTTP ${r.status ?? "?"}`).slice(0, 120),
        });
        setTestTick((n) => n + 1);
      }
      if (r.ok) {
        message.success(t("models.testOk", { status: r.status ?? 200 }));
      } else if (r.configured === false || r.mock) {
        message.warning(r.message ?? t("models.testNoKey"));
      } else {
        message.error(
          t("models.testFail", { detail: (r.error ?? `HTTP ${r.status ?? "?"} ${r.snippet ?? ""}`).slice(0, 160) }),
        );
      }
    } catch (e) {
      message.error(t("models.testFailed", { err: String(e) }));
    } finally {
      setTesting("");
    }
  };

  const fallbackOptions = useMemo(
    () =>
      profiles
        .filter((p) => p.id !== (editing?.id ?? ""))
        .map((p) => ({ value: p.id, label: `${p.name}（${p.id}）` })),
    [profiles, editing],
  );

  const presetOptions = [
    ...PRESETS.map((p) => ({ value: p.key, label: PRESET_LABEL[p.key] ? t(PRESET_LABEL[p.key]!) : p.name })),
    { value: CUSTOM, label: t("models.customPreset") },
  ];

  const apiFormats = API_FORMAT_KEYS.map((f) => ({ value: f.value, label: t(f.key) }));

  return (
    <div className="pane-wrap settings-page models-settings-page">
      <PageHeader
        en="MODELS"
        title={t("nav.models")}
        desc={t("ux.modelsDescription")}
        actions={<>
          {tab === "providers" && <Button type="primary" icon={<PlusOutlined />} onClick={openCreate}>{t("models.add")}</Button>}
          {tab === "providers" && <Button icon={<ApiOutlined />} onClick={() => void test(undefined)} loading={testing === "__resolved__"}>{t("models.testGlobal")}</Button>}
          <Button icon={<ReloadOutlined />} loading={loading} onClick={() => void load()} aria-label={t("common.refresh")} />
        </>}
      />
      <Tabs className="settings-page-tabs" activeKey={tab} onChange={(next) => setTab(next as Tab)} items={[
        { key: "providers", label: <>{t("models.tabProviders")} <span className="settings-count">{profiles.length}</span></>, children:
          <Spin spinning={loading}><ProviderWorkspace profiles={profiles} active={info?.active} testing={testing} onEdit={openEdit}
            onTest={(id) => void test(id)} onDelete={(id) => void remove(id)} onCreate={openCreate} onAllocate={() => setTab("caps")} /></Spin> },
        { key: "caps", label: t("routing.assignmentTab"), children: <ModelAllocationPanel profiles={profiles} active={active && tab === "caps"} /> },
        { key: "global", label: t("ux.globalCapabilities"), children: <GlobalCapabilityPanel profiles={profiles} active={active && tab === "global"} /> },
      ]} />
      {modal && <SettingsDialog title={editing ? t("models.editTitle", { name: editing.name }) : t("models.add")} subtitle={editing?.id || t("ux.newConnection")}
        activeSection={editorSection} onSectionChange={setEditorSection} dirty={dirty} saving={saving} onSave={() => void submit()} onClose={() => setModal(false)}
        children={(content) => <Form form={form} layout="vertical" disabled={saving} onFinish={() => void submit()}>{content}</Form>}
        sections={[
          { key: "connection", label: t("ux.connection"), description: t("ux.connectionDescription"), icon: <KeyOutlined />, content: <>
            <Form.Item name="preset" label={t("models.presetLabel")} rules={[{ required: true }]}><Select options={presetOptions} onChange={applyPreset} /></Form.Item>
            <div className="form-grid-2">
              <Form.Item name="name" label={t("models.displayName")}><Input placeholder={t("models.displayNamePlaceholder")} /></Form.Item>
              <Form.Item name="id" label={t("models.profileId")} rules={[{ pattern: /^[A-Za-z0-9_-]{1,48}$/, message: t("models.idPattern") }]}><Input placeholder={t("models.idAuto")} disabled={Boolean(editing)} /></Form.Item>
            </div>
            <Form.Item name="baseUrl" label="Base URL" rules={[{ required: true, whitespace: true, message: t("models.baseUrlRequired") }]}><Input placeholder="https://api.example.com/v1" /></Form.Item>
            <Form.Item name="apiFormat" label={t("models.apiFormat")} extra={t("models.apiFormatExtra")}><Select options={apiFormats} /></Form.Item>
            <div className="settings-section-title"><h3>{t("models.sectionKeys")}</h3></div>
            <Form.List name="keys">{(fields, { add, remove }) => <>
              {fields.map(({ key, name, ...rest }) => <div key={key} className="key-row">
                <Form.Item {...rest} name={[name, "value"]} noStyle><MaskedPasswordInput aria-label={`API Key ${name + 1}`} placeholder="sk-…" /></Form.Item>
                <Button type="text" icon={<DeleteOutlined />} onClick={() => remove(name)} aria-label={t("models.removeKey")} />
              </div>)}
              <Button type="dashed" block icon={<PlusOutlined />} onClick={() => add({ value: "" })}>{t("models.addKey")}</Button>
            </>}</Form.List>
            <p className="settings-context-note">{t("models.keysExtra")} {t(editing ? "models.keysExtraEdit" : "models.keysExtraNew")}</p>
          </> },
          { key: "catalog", label: t("models.modelList"), description: t("ux.catalogDescription"), icon: <ApiOutlined />, content: <ProviderCatalogFields fetching={fetchingModels} onFetch={() => void fetchModels()} /> },
          { key: "identity", label: t("models.clientIdentity"), description: t("ux.identityDescription"), icon: <SafetyCertificateOutlined />, content: <ClientIdentityFields profileId={editing?.id} /> },
          { key: "advanced", label: t("models.sectionAdvanced"), description: t("ux.fallbackDescription"), icon: <ControlOutlined />, content:
            <Form.Item name="fallback" label={t("models.fallback")} extra={t("models.fallbackExtra")}><Select allowClear placeholder={t("models.noFallback")} options={fallbackOptions} /></Form.Item> },
        ]} />}

      {/* 远端模型选择窗：拉取后先勾选再入表单（不全量塞入） */}
      <Modal
        open={Boolean(picker?.open)}
        title={t("models.pickerTitle")}
        onCancel={() => setPicker(null)}
        onOk={() => void applyPickedModels()}
        okText={t("models.pickerAdd", { n: picker?.selected.length ?? 0 })}
        okButtonProps={{ disabled: (picker?.selected.length ?? 0) === 0, loading: pickerSaving }}        cancelText={t("common.cancel")}
        width={520}
      >
        {picker && (
          <ModelPicker
            models={picker.models}
            selected={picker.selected}
            onChange={(selected) => setPicker((p) => (p ? { ...p, selected } : p))}
          />
        )}
      </Modal>
    </div>
  );
}

/** 远端模型勾选列表：搜索过滤 + 全选/全不选；已在表单中的模型标记「已有」不可勾 */
