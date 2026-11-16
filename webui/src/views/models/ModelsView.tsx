/** Provider catalogs describe models; assignments edit individual agents and group members. */
import React, { useCallback, useEffect, useMemo, useState } from "react";
import {
  Alert, Button, Card, Collapse, Form, Input, InputNumber, Modal, Popconfirm, Segmented, Select, Space, Spin, Switch,
  Tag, Tooltip, message,
} from "antd";
import { BrandEmpty } from "../../components/BrandEmpty";
import { MaskedPasswordInput } from "../../components/MaskedInput";
import {
  ApiOutlined, CloudDownloadOutlined, DeleteOutlined, EditOutlined,
  EyeOutlined, MinusCircleOutlined, PlusOutlined, ReloadOutlined, SoundOutlined,
} from "@ant-design/icons";
import { api, KEY_MASK, KEY_CLEAR, type ClientIdentity, type LlmProfile, type LlmProfilesInfo, type ProfileModel } from "../../api";
import { getTestRecord, putTestRecord } from "../../testLog";
import { PageHeader } from "../../components/PageHeader";
import { useT } from "../../i18n/core";

/** 厂商预设：选中即填端点、协议与常用模型名（模型名可在表单里改） */
import { PRESETS, PRESET_LABEL, CUSTOM, API_FORMAT_KEYS, FORMAT_LABEL_KEY, type KeyForm, type Tab, liveRows } from "./presets";
import { ModelPicker } from "./ModelPicker";
import { ClientIdentityFields, parseClientIdentity } from "./ClientIdentityFields";
import { ModelAllocationPanel } from "./ModelAllocationPanel";
import { MODEL_PURPOSES, costLabel, purposeLabel } from "../../modelRouting";

export function ModelsView(): React.ReactElement {
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
      let body: { id?: string; baseUrl?: string; apiFormat?: string; apiKey?: string; clientIdentity: ClientIdentity } = editing
        ? { id: editing.id, clientIdentity }
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
    void load();
  }, [load]);

  const openCreate = () => {
    setEditing(null);
    form.resetFields();
    form.setFieldsValue({ preset: "openai", apiFormat: "openai", clientIdentity: { preset: "default" }, clientHeaders: "", keys: [{ value: "" }], models: [] });
    setModal(true);
  };

  const openEdit = (p: LlmProfile) => {
    setEditing(p);
    form.resetFields();
    const preset = PRESETS.find((x) => x.baseUrl === p.baseUrl)?.key ?? CUSTOM;
    // 密钥行合并为一行序列：主 Key 掩码行（keep: -1 = 主沿用，不入池）+ Key 池掩码行（keep = 旧池索引）
    const maskRows = p.apiKey ? [{ value: KEY_MASK, keep: -1 }] : [];
    const poolRows = (p.apiKeys ?? []).map((k, j) => (k ? { value: KEY_MASK, keep: j } : { value: "" }));
    const rows: ProfileModel[] = p.models.map((m) => ({ ...m }));
    form.setFieldsValue({
      preset,
      id: p.id,
      name: p.name,
      baseUrl: p.baseUrl,
      apiFormat: p.apiFormat || "openai",
      clientIdentity: p.clientIdentity ?? { preset: "default" },
      clientHeaders: Object.keys(p.clientIdentity?.headers ?? {}).length ? JSON.stringify(p.clientIdentity?.headers, null, 2) : "",
      models: rows,
      model: p.model,
      fallback: p.fallback ?? undefined,
      keys: [...maskRows, ...poolRows],
    });
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
    } catch {
      return;
    }
    const v = form.getFieldsValue(true);
    setSaving(true);
    try {
      const all = form.getFieldsValue(true) as KeyForm & Record<string, unknown>;
      const existing = editing;
      // 行级意图三态：掩码 = 沿用旧值；明文 = 设置/替换；掩码行被清空/删除 = 显式删除（KEY_CLEAR 哨兵）。
      // 空串在后端是「沿用旧值」——删除必须用哨兵表达，否则「清空输入框保存」永远删不掉密钥。
      // 首行 = 主 Key 位（keep: -1 = 主沿用），其余行按原序进 Key 池（keep = 旧池索引）。
      // 关键：Form.List 删行是物理移除，被删行不会出现在 all.keys 里——
      // 后端会把「apiKeys 缺省/空列表」当旧客户端沿用旧池，删行保存就白删了。
      // 所以池提交序列必须与「初始池掩码行」等长对齐：初始池第 j 把若不在当前行里，补 KEY_CLEAR 占位。
      const rawRows = (all.keys ?? []).map((r) => ({
        value: (r?.value ?? "").trim(),
        keep: r?.keep,
      }));
      // 行是否携带沿用标记（编辑时由 openEdit 注入；-1 = 主 Key 位，>=0 = 旧池索引）
      const hasKeep = (r: { keep?: number } | undefined) => r?.keep !== undefined;
      // 当前各行表达的旧池意图：keep=j 的掩码行 = 沿用第 j 把；keep=j 的空行 = 删除第 j 把
      const oldPoolSize = existing ? (existing.apiKeys ?? []).filter((k) => k && k.trim()).length : 0;
      const poolIntent = new Map<number, string>(); // j -> KEY_MASK(沿用) | KEY_CLEAR(删除)
      for (const r of rawRows) {
        if (hasKeep(r) && (r.keep ?? -1) >= 0) {
          poolIntent.set(r.keep ?? 0, r.value === KEY_MASK ? KEY_MASK : r.value === "" ? KEY_CLEAR : r.value);
        }
      }
      // 新增明文行（无 keep 标记且非空、非掩码）按序追加在池序列之后
      const freshPool = rawRows.filter((r) => !hasKeep(r) && r.value !== "" && r.value !== KEY_MASK).map((r) => r.value);
      const firstRaw = rawRows[0];
      let apiKey: string;
      let apiKeys: (string | { keep: number })[];
      let clearKeys = false;
      if (existing) {
        const first = firstRaw;
        // 池提交序列：按初始池索引 0..oldPoolSize-1 对齐（缺位 = 该行被删除 = KEY_CLEAR），再接新增明文
        apiKeys = Array.from({ length: oldPoolSize }, (_, j) => poolIntent.get(j) ?? KEY_CLEAR).concat(freshPool);
        // 主 Key 行（keep=-1）已被物理删除 → 删除主 Key 意图（剩余行的池意图已在 poolIntent 里对齐）
        const mainRowGone = hasKeep(first) && (first.keep ?? -1) >= 0;
        if (mainRowGone && (existing.apiKey ?? "") !== "") {
          apiKey = KEY_CLEAR;
        } else if (first && first.value === KEY_MASK) {
          // 首行掩码保留：主 Key 沿用旧值（服务端无主 Key 时保持空）
          apiKey = existing.apiKey ? KEY_MASK : "";
        } else if (first && first.value !== "") {
          // 首行明文：设置/替换主 Key
          apiKey = first.value;
        } else if (first && first.value === "" && hasKeep(first)) {
          // 首行是掩码位但被用户清空 = 显式删除主 Key（KEY_CLEAR）
          apiKey = KEY_CLEAR;
        } else {
          // 行已删空（或首行是新的空行）：显式清空全部密钥（clearKeys）——
          // 空串在后端是「沿用旧值」，不带标志位保存后密钥依然存在
          apiKey = "";
          apiKeys = [];
          clearKeys = (existing.apiKey ?? "") !== "" || (existing.apiKeys ?? []).length > 0;
        }
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
    <div className="pane-wrap">
      <PageHeader
        en="MODELS"
        title={t("nav.models")}
        desc={tab === "caps" ? t("routing.description") : t("models.desc")}
        actions={
          <>
            {/* 视图切换与动作分组（视觉走查：切换/操作分层，Divider 区隔主次） */}
            <Space separator={<span className="head-divider" />}>
              <Segmented
                value={tab}
                onChange={(v) => setTab(v as Tab)}
                options={[
                  { value: "providers", label: t("models.tabProviders") },
                  { value: "caps", label: t("routing.assignmentTab") },
                ]}
              />
              {tab === "providers" && (
                <>
                  <Button type="primary" icon={<PlusOutlined />} onClick={openCreate}>
                    {t("models.add")}
                  </Button>
                  <Button icon={<ApiOutlined />} onClick={() => void test(undefined)} loading={testing === "__resolved__"}>
                    {t("models.testGlobal")}
                  </Button>
                </>
              )}
              <Button icon={<ReloadOutlined />} onClick={() => void load()}>
                {t("common.refresh")}
              </Button>
            </Space>
          </>
        }
      />

      {tab === "providers" && (
      <Spin spinning={loading}>
        <Alert type="info" showIcon className="routing-catalog-hint" title={t("routing.catalogHint")} />
        <div className="skill-grid">
          {profiles.map((p) => {
            const keyCount = (p.apiKeys?.length ?? 0) + (p.apiKey ? 1 : 0);
            const hasKey = keyCount > 0;
            return (
              <Card
                key={p.id}
                size="small"
                className="hud model-card"
                title={
                  <div className="model-head">
                    <span>{p.name}</span>
                    <span className="mono dim">{p.id}</span>
                  </div>
                }
                extra={
                  <Space size={4}>
                    <Button size="small" icon={<EditOutlined />} onClick={() => openEdit(p)} />
                    <Button size="small" loading={testing === p.id} onClick={() => void test(p.id)}>
                      {t("models.test")}
                    </Button>
                    {profiles.length > 1 && (
                      <Popconfirm title={t("common.confirmDelete", { name: p.name })} onConfirm={() => void remove(p.id)}>
                        <Button size="small" danger icon={<DeleteOutlined />} />
                      </Popconfirm>
                    )}
                  </Space>
                }
              >
                {/* 模型清单主角行：provider 卡只管接入（端点/密钥/模型清单），
                    用什么模型是智能体层面的选择（agent modelHint / 组默认） */}
                <div className="model-status-row">
                  <span className="model-status-item">
                    <span className="k">{t("models.keyLabel")}</span>
                    {hasKey ? (
                      <Tag className="tag-ok">{t("models.keyPoolHealth", { ok: keyCount, total: keyCount })}</Tag>
                    ) : (
                      <Tag color="warning">{t("models.keyMissing")}</Tag>
                    )}
                  </span>
                  <span className={`model-status-item test-state ${(() => {
                    const rec = getTestRecord(p.id);
                    return rec ? (rec.ok ? "ok" : "fail") : "";
                  })()}`}>
                    <span className="k">{t("models.statusLabel")}</span>
                    {(() => {
                      const rec = getTestRecord(p.id);
                      if (!rec) return <span className="dim">{t("models.followGlobal")}</span>;
                      const time = new Date(rec.at);
                      const rel = `${Math.max(1, Math.round((Date.now() - time.getTime()) / 60000))}min`;
                      return rec.ok ? (
                        <span className="mono test-ok">{t("models.lastTestOk", { ms: rec.latencyMs ?? "?" })} · {rel}</span>
                      ) : (
                        <Tooltip title={t("models.lastTestFail", { reason: rec.reason ?? "?" })}>
                          <span className="test-fail-text">{t("models.lastTestFail", { reason: (rec.reason ?? "?").slice(0, 40) })}</span>
                        </Tooltip>
                      );
                    })()}
                  </span>
                </div>
                <div className="model-kv">
                  <span className="k">{t("models.endpoint")}</span>
                  <span className="mono">{p.baseUrl}</span>
                  <Tag>{t(FORMAT_LABEL_KEY[p.apiFormat || "openai"] ?? "models.fmtShort.openai")}</Tag>
                  {p.clientIdentity && p.clientIdentity.preset !== "default" && (
                    <Tag>{t("models.clientIdentity")}: {p.clientIdentity.preset}</Tag>
                  )}
                </div>
                <div className="model-kv">
                  <span className="k">{t("models.keyLabel")}</span>
                  {hasKey ? (
                    // 已配置是正常态，用中性标签；只有缺密钥才需要警示
                    <Tag className="tag-ok">{keyCount > 1 ? t("models.keyConfiguredN", { n: keyCount }) : t("models.keyConfigured")}</Tag>
                  ) : (
                    <Tag color="warning">{t("models.keyMissing")}</Tag>
                  )}
                  {p.fallback ? <Tag>{t("models.fallbackTo", { id: p.fallback })}</Tag> : null}
                </div>
                <div className="model-catalog-list">{p.models.map((model) => <div key={model.model} className="model-catalog-entry">
                  <div><span className="mono">{model.model}</span> <Tag>{costLabel(model.cost)}</Tag>{model.autoEligible && <Tag>{t("routing.autoEligible")}</Tag>}</div>
                  {!!model.purposes?.length && <div className="dim">{model.purposes.map(purposeLabel).join(" · ")}</div>}
                  {model.usageNotes && <div className="dim">{model.usageNotes}</div>}
                </div>)}</div>
              </Card>
            );
          })}
        </div>
        {!loading && profiles.length === 0 && <BrandEmpty description={t("models.none")} className="pane-empty" />}
      </Spin>
      )}

      {tab === "caps" && <ModelAllocationPanel profiles={profiles} />}

      <Modal
        open={modal}
        title={editing ? t("models.editTitle", { name: editing.name }) : t("models.add")}
        onCancel={() => setModal(false)}
        onOk={() => void submit()}
        confirmLoading={saving}
        okText={t("common.save")}
        destroyOnHidden
        width={620}
      >
        <Form form={form} layout="vertical" onFinish={() => void submit()}>
          {/* 基础节常驻（认知负荷最低的核心三件：预设 → 端点 → 命名） */}
          <Form.Item name="preset" label={t("models.presetLabel")} rules={[{ required: true }]}>
            <Select options={presetOptions} onChange={(k) => applyPreset(k)} />
          </Form.Item>
          <Form.Item
            name="baseUrl"
            label="Base URL"
            rules={[{ required: true, whitespace: true, message: t("models.baseUrlRequired") }]}
          >
            <Input placeholder="https://api.deepseek.com/v1" />
          </Form.Item>
          <div className="form-grid-2">
            <Form.Item name="name" label={t("models.displayName")}>
              <Input placeholder={t("models.displayNamePlaceholder")} />
            </Form.Item>
            <Form.Item
              name="id"
              label={t("models.profileId")}
              rules={[{ pattern: /^[A-Za-z0-9_-]{1,48}$/, message: t("models.idPattern") }]}
            >
              <Input placeholder={t("models.idAuto")} disabled={Boolean(editing)} />
            </Form.Item>
          </div>
          <Form.Item name="apiFormat" label={t("models.apiFormat")} extra={t("models.apiFormatExtra")}>
            <Select options={apiFormats} />
          </Form.Item>

          <Collapse size="small" className="form-section" items={[{
            key: "clientIdentity",
            label: t("models.clientIdentity"),
            forceRender: true,
            children: <ClientIdentityFields />,
          }]} />

          {/* 密钥池独立小节（增删/掩码/三态语义说明） */}
          <Collapse
            size="small"
            className="form-section"
            items={[{
              key: "keys",
              label: `${t("models.sectionKeys")}${(() => {
                const rows = (form.getFieldValue("keys") ?? []) as { value?: string }[];
                const n = rows.filter((r) => (r?.value ?? "").trim() && (r?.value ?? "") !== KEY_MASK).length;
                return n > 0 ? ` · ${n}` : "";
              })()}`,
              children: (
                <Form.Item label="API Key" style={{ marginBottom: 0 }}>
                  <Form.List name="keys">
                    {(fields, { add, remove }) => (
                      <>
                        {fields.map(({ key, name, ...rest }) => (
                          <div key={key} className="key-row">
                            <Form.Item {...rest} name={[name, "value"]} noStyle>
                              <MaskedPasswordInput placeholder="sk-…" />
                            </Form.Item>
                            {fields.length > 1 && (
                              <Button
                                type="text"
                                icon={<MinusCircleOutlined />}
                                onClick={() => remove(name)}
                                aria-label={t("models.removeKey")}
                              />
                            )}
                          </div>
                        ))}
                        <Button type="dashed" block icon={<PlusOutlined />} onClick={() => add("")}>
                          {t("models.addKey")}
                        </Button>
                        <div className="ant-form-item-extra" style={{ marginTop: 6 }}>
                          {t("models.keysExtra")}
                          {editing ? t("models.keysExtraEdit") : t("models.keysExtraNew")}
                        </div>
                      </>
                    )}
                  </Form.List>
                </Form.Item>
              ),
            }]}
          />

          {/* 模型清单独立小节（provider 只管「有哪些模型」；用什么模型在智能体/组设置） */}
          <Collapse
            size="small"
            className="form-section"
            items={[{
              key: "models",
              label: `${t("models.sectionModels")}${(() => {
                const rows = (form.getFieldValue("models") ?? []) as { model?: string }[];
                const n = rows.filter((m) => (m?.model ?? "").trim()).length;
                return n > 0 ? ` · ${n}` : "";
              })()}`,
              children: (
                <Form.Item
                  label={t("models.modelList")}
                  tooltip={t("models.fetchModelsTip")}
                  style={{ marginBottom: 0 }}
                >
                  <Button
                    size="small"
                    icon={<CloudDownloadOutlined />}
                    loading={fetchingModels}
                    onClick={() => void fetchModels()}
                    style={{ marginBottom: 8 }}
                  >
                    {t("models.fetchModels")}
                  </Button>
                  <Form.List name="models">
                    {(fields, { add, remove }) => (
                      <>
                        {fields.map(({ key, name, ...rest }) => (
                           <div key={key} className="model-catalog-form">
                           <div className="model-row">
                            <Form.Item
                              {...rest}
                              name={[name, "model"]}
                              noStyle
                              rules={[{ required: true, whitespace: true, message: t("models.modelNameRequired") }]}
                            >
                              <Input placeholder="gpt-4o" />
                            </Form.Item>
                            <label className="cap-switch">
                              <Form.Item {...rest} name={[name, "vision"]} valuePropName="checked" noStyle>
                                <Switch size="small" />
                              </Form.Item>
                              <EyeOutlined />
                              {t("models.visionCap")}
                            </label>
                            <label className="cap-switch">
                              <Form.Item {...rest} name={[name, "audio"]} valuePropName="checked" noStyle>
                                <Switch size="small" />
                              </Form.Item>
                              <SoundOutlined />
                              {t("models.audioCap")}
                            </label>
                            <Button
                              type="text"
                              icon={<MinusCircleOutlined />}
                              onClick={() => remove(name)}
                              aria-label={t("common.delete")}
                            />
                           </div>
                           <div className="form-grid-2">
                             <Form.Item {...rest} name={[name, "purposes"]} label={t("routing.purposes")}>
                               <Select mode="multiple" options={MODEL_PURPOSES.map((purpose) => ({ value: purpose, label: purposeLabel(purpose) }))} />
                             </Form.Item>
                             <Form.Item {...rest} name={[name, "cost"]} label={t("routing.cost")} initialValue="unknown">
                               <Select options={(["unknown", "economy", "balanced", "premium"] as const).map((cost) => ({ value: cost, label: costLabel(cost) }))} />
                             </Form.Item>
                             <Form.Item {...rest} name={[name, "contextWindow"]} label={t("routing.contextWindow")}>
                               <InputNumber min={0} max={100000000} precision={0} style={{ width: "100%" }} />
                             </Form.Item>
                             <Form.Item {...rest} name={[name, "autoEligible"]} label={t("routing.autoEligible")} valuePropName="checked" tooltip={t("routing.autoEligibleHint")}>
                               <Switch />
                             </Form.Item>
                           </div>
                           <Form.Item {...rest} name={[name, "usageNotes"]} label={t("routing.usageNotes")}>
                             <Input.TextArea rows={2} maxLength={2000} placeholder={t("routing.usageNotesPlaceholder")} />
                           </Form.Item>
                           </div>
                        ))}
                        <Button
                          type="dashed"
                          block
                          icon={<PlusOutlined />}
                          onClick={() => add({ model: "", vision: false, audio: false })}
                        >
                          {t("models.addModel")}
                        </Button>
                        <div className="ant-form-item-extra" style={{ marginTop: 6 }}>
                          {t("models.modelListExtra")}
                        </div>
                      </>
                    )}
                  </Form.List>
                  <Form.Item name="model" label={t("routing.providerDefault")} style={{ marginTop: 12 }}>
                    <Input placeholder={editing?.model || t("models.modelNameRequired")} />
                  </Form.Item>
                </Form.Item>
              ),
            }]}
          />

          {/* 高级项默认折叠（低频配置） */}
          <Collapse
            size="small"
            className="form-section"
            items={[{
              key: "advanced",
              label: t("models.sectionAdvanced"),
              children: (
                <Form.Item
                  name="fallback"
                  label={t("models.fallback")}
                  extra={t("models.fallbackExtra")}
                  style={{ marginBottom: 0 }}
                >
                  <Select allowClear placeholder={t("models.noFallback")} options={fallbackOptions} />
                </Form.Item>
              ),
            }]}
          />
        </Form>
      </Modal>

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
