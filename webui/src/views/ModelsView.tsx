/**
 * 提供商页：API 端点与密钥的唯一下发处。
 *
 * 设计原则：**这一页只回答一件事——这家提供商的端点在哪、密钥是什么。**
 * 模型名只是「连上这个端点后默认用哪个模型」的附注（预设自动填，可改）。
 * 指挥体 / 子个体的模型差异属于编成，在「智能体组」与「子个体」页各自选择；
 * 嵌入模型属于记忆检索，在「记忆」页指定——都**不在这里**出现。
 */
import React, { useCallback, useEffect, useMemo, useState } from "react";
import {
  Button, Card, Form, Input, Modal, Popconfirm, Select, Space, Spin, Switch,
  Tag, Tooltip, message,
} from "antd";
import { BrandEmpty } from "../components/BrandEmpty";
import { MaskedPasswordInput } from "../components/MaskedInput";
import {
  ApiOutlined, CheckCircleOutlined, CloudDownloadOutlined, DeleteOutlined, EditOutlined,
  EyeOutlined, MinusCircleOutlined, PlusOutlined, ReloadOutlined, SoundOutlined,
} from "@ant-design/icons";
import { api, KEY_MASK, KEY_CLEAR, type LlmCapabilities, type LlmProfile, type LlmProfilesInfo, type ProfileModel } from "../api";
import { buildCapabilityOptions } from "../models";
import { PageHeader } from "../components/PageHeader";
import { useT, type TKey } from "../i18n/core";

/** 厂商预设：选中即填端点、协议与常用模型名（模型名可在表单里改） */
interface Preset {
  key: string;
  name: string;
  baseUrl: string;
  model: string;
  apiFormat: string;
}

const PRESETS: Preset[] = [
  { key: "openai", name: "OpenAI", baseUrl: "https://api.openai.com/v1", model: "gpt-4o", apiFormat: "openai" },
  { key: "anthropic", name: "Anthropic", baseUrl: "https://api.anthropic.com", model: "claude-sonnet-4-5", apiFormat: "anthropic" },
  { key: "gemini", name: "Google Gemini", baseUrl: "https://generativelanguage.googleapis.com/v1beta", model: "gemini-2.5-flash", apiFormat: "gemini" },
  { key: "azure", name: "Azure OpenAI", baseUrl: "https://<resource>.openai.azure.com", model: "<deployment>", apiFormat: "azure" },
  { key: "deepseek", name: "DeepSeek", baseUrl: "https://api.deepseek.com/v1", model: "deepseek-chat", apiFormat: "openai" },
  { key: "moonshot", name: "Moonshot / Kimi", baseUrl: "https://api.moonshot.cn/v1", model: "moonshot-v1-32k", apiFormat: "openai" },
  { key: "qwen", name: "Qwen", baseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1", model: "qwen-plus", apiFormat: "openai" },
  { key: "zhipu", name: "Zhipu GLM", baseUrl: "https://open.bigmodel.cn/api/paas/v4", model: "glm-4-plus", apiFormat: "openai" },
  { key: "minimax", name: "MiniMax", baseUrl: "https://api.minimaxi.com/v1", model: "MiniMax-M1", apiFormat: "openai" },
  { key: "ollama", name: "Ollama", baseUrl: "http://127.0.0.1:11434/v1", model: "llama3.1", apiFormat: "openai" },
];

/** 预设的本地化显示名（覆盖 PRESETS.name 的只有中英写法不同的厂商） */
const PRESET_LABEL: Partial<Record<string, TKey>> = {
  qwen: "models.preset.qwen",
  zhipu: "models.preset.zhipu",
  ollama: "models.preset.ollama",
  minimax: "models.preset.minimax",
};

const CUSTOM = "__custom__";

/** API 协议选项的显示名走 i18n（值为协议标识，与后端契约） */
const API_FORMAT_KEYS: { value: string; key: TKey }[] = [
  { value: "openai", key: "models.fmt.openai" },
  { value: "anthropic", key: "models.fmt.anthropic" },
  { value: "gemini", key: "models.fmt.gemini" },
  { value: "azure", key: "models.fmt.azure" },
];

/** 密钥掩码哨兵：KEY_MASK 从 api.ts 引入（与后端契约一致：提交掩码位 = 服务端沿用旧值） */

/** 协议短标签（卡片上的 Tag 用），显示名走 i18n */
const FORMAT_LABEL_KEY: Record<string, TKey> = {
  openai: "models.fmtShort.openai",
  anthropic: "models.fmtShort.anthropic",
  gemini: "models.fmtShort.gemini",
  azure: "models.fmtShort.azure",
};

/** 表单字段：密钥为可增删的动态行；掩码行携带 keep = 旧池索引（提交结构化沿用，展示仍为掩码） */
interface KeyForm {
  keys?: { value?: string; keep?: number }[];
}

/** 取有效密钥行（去空白），保序返回行对象 */
function liveRows(rows: KeyForm["keys"]): { value: string; keep?: number }[] {
  return (rows ?? [])
    .filter((r) => (r?.value ?? "").trim() !== "")
    .map((r) => ({ value: (r?.value ?? "").trim(), keep: r?.keep }));
}

export function ModelsView(): React.ReactElement {
  const t = useT();
  const [info, setInfo] = useState<LlmProfilesInfo | null>(null);
  const [caps, setCaps] = useState<LlmCapabilities>({ speech: "", transcribe: "", visionRelay: "", embedding: "" });
  const [capSaving, setCapSaving] = useState(false);
  const [loading, setLoading] = useState(false);
  const [modal, setModal] = useState(false);
  const [editing, setEditing] = useState<LlmProfile | null>(null);
  const [testing, setTesting] = useState<string>("");
  const [fetchingModels, setFetchingModels] = useState(false);
  const [saving, setSaving] = useState(false);
  /** 模型选择弹窗：远端清单拉取后先展示，勾选后才进表单（避免全量塞入） */
  const [picker, setPicker] = useState<{ open: boolean; models: string[]; selected: string[] } | null>(null);
  const [pickerSaving, setPickerSaving] = useState(false);
  const [form] = Form.useForm();
  /** 模型清单行（Form.useWatch 实时跟随表单）：默认模型与「获取清单」按钮的数据源 */
  const modelWatch: ProfileModel[] = Form.useWatch("models", form) ?? [];

  /** 拉取端点可用模型：编辑态按档案 id（沿用已存密钥）；新建态用表单当前端点 + 首行明文密钥。
   *  拉取成功后弹选择窗，勾选的模型才合入表单清单（保留既有行的能力标记，仅补缺）。 */
  const fetchModels = async () => {
    setFetchingModels(true);
    try {
      const all = form.getFieldsValue(true) as KeyForm & { baseUrl?: string; apiFormat?: string };
      const rows = liveRows(all.keys);
      let body: { id?: string; baseUrl?: string; apiFormat?: string; apiKey?: string } = editing
        ? { id: editing.id }
        : {
            baseUrl: (all.baseUrl ?? "").trim(),
            apiFormat: all.apiFormat ?? "openai",
            apiKey: rows[0]?.value ?? "",
          };
      if (!editing && (!body.baseUrl || !rows[0]?.value)) {
        message.warning(t("models.fetchNeedsKey"));
        return;
      }
      if (editing && rows[0]?.value && rows[0].value !== KEY_MASK) {
        // 编辑态且首行填了新明文密钥：用明文探测（与保存语义一致）
        body = { baseUrl: all.baseUrl, apiFormat: all.apiFormat ?? "openai", apiKey: rows[0].value };
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
      setCaps(await api.llmCapabilities());
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const profiles = info?.profiles ?? [];

  const openCreate = () => {
    setEditing(null);
    form.resetFields();
    form.setFieldsValue({ preset: "openai", apiFormat: "openai", keys: [{ value: "" }], models: [] });
    setModal(true);
  };

  const openEdit = (p: LlmProfile) => {
    setEditing(p);
    const preset = PRESETS.find((x) => x.baseUrl === p.baseUrl)?.key ?? CUSTOM;
    // 密钥行合并为一行序列：主 Key 掩码行（keep: -1 = 主沿用，不入池）+ Key 池掩码行（keep = 旧池索引）
    const maskRows = p.apiKey ? [{ value: KEY_MASK, keep: -1 }] : [];
    const poolRows = (p.apiKeys ?? []).map((k, j) => (k ? { value: KEY_MASK, keep: j } : { value: "" }));
    // 模型清单：空清单且默认模型存在时，先列出默认模型行（能力未标记，用户自行勾选）
    const rows: ProfileModel[] = p.models.length
      ? p.models.map((m) => ({ ...m }))
      : p.model
        ? [{ model: p.model, vision: false, audio: false }]
        : [];
    form.setFieldsValue({
      preset,
      id: p.id,
      name: p.name,
      baseUrl: p.baseUrl,
      apiFormat: p.apiFormat || "openai",
      model: p.model,
      models: rows,
      fallback: p.fallback ?? undefined,
      keys: [...maskRows, ...poolRows],
    });
    setModal(true);
  };

  /** 选中预设：填「端点 / 协议 / 默认模型名」，密钥永远要自己贴 */
  const applyPreset = (key: string) => {
    const preset = PRESETS.find((p) => p.key === key);
    if (!preset) {
      form.setFieldsValue({ apiFormat: "openai" });
      return;
    }
    form.setFieldsValue({
      baseUrl: preset.baseUrl,
      apiFormat: preset.apiFormat,
      model: preset.model,
      name: form.getFieldValue("name") || preset.name,
    });
  };

  const submit = async () => {
    // 校验规则用 validateFields；取值用 getFieldsValue(true)——
    // 掩码行的 keep 无控件绑定，validateFields 只返回已注册字段会把它丢掉
    const v = await form.validateFields();
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
      const hasKeep = (r: { keep?: number }) => r.keep !== undefined;
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
        apiKey,
        apiKeys,
        clearKeys,
        model: (v.model ?? "").trim(),
        models: ((v.models ?? []) as ProfileModel[])
          .filter((m) => m.model && m.model.trim())
          .map((m) => ({ model: m.model.trim(), vision: !!m.vision, audio: !!m.audio })),
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

  const activate = async (id: string) => {
    try {
      await api.activateLlmProfile(id);
      message.success(t("models.activated", { id }));
      await load();
    } catch (e) {
      message.error(t("models.switchFailed", { err: String(e) }));
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

  /** 能力槽位候选：每个档案的默认模型 + 清单里的每个模型（"档案ID" / "档案ID/模型名"） */
  const capOptions = useMemo(() => buildCapabilityOptions(profiles), [profiles]);

  const capField = (
    key: keyof LlmCapabilities,
    label: string,
    hint: string,
    placeholder: string,
  ) => (
    <div className="cap-row" key={key}>
      <div className="cap-label">{label}</div>
      <Select
        allowClear
        showSearch
        value={caps[key] || undefined}
        placeholder={placeholder}
        options={capOptions}
        onChange={(v) => setCaps((c) => ({ ...c, [key]: v ?? "" }))}
      />
      <div className="dim cap-hint">{hint}</div>
    </div>
  );

  const saveCaps = async () => {
    setCapSaving(true);
    try {
      setCaps(await api.saveLlmCapabilities(caps));
      message.success(t("models.capSaved"));
    } catch (e) {
      message.error(t("common.saveFailed", { err: String(e) }));
    } finally {
      setCapSaving(false);
    }
  };

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
        desc={t("models.desc")}
        actions={
          <>
            <Button type="primary" icon={<PlusOutlined />} onClick={openCreate}>
              {t("models.add")}
            </Button>
            <Button icon={<ApiOutlined />} onClick={() => void test(undefined)} loading={testing === "__resolved__"}>
              {t("models.testGlobal")}
            </Button>
            <Button icon={<ReloadOutlined />} onClick={() => void load()}>
              {t("common.refresh")}
            </Button>
          </>
        }
      />

      <Spin spinning={loading}>
        <div className="skill-grid">
          {profiles.map((p) => {
            const active = info?.active === p.id;
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
                    {!active && (
                      <Tooltip title={t("models.setDefaultTip")}>
                        <Button size="small" icon={<CheckCircleOutlined />} onClick={() => void activate(p.id)}>
                          {t("models.setDefault")}
                        </Button>
                      </Tooltip>
                    )}
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
                <div className="model-kv">
                  <span className="k">{t("models.endpoint")}</span>
                  <span className="mono">{p.baseUrl}</span>
                  <Tag>{t(FORMAT_LABEL_KEY[p.apiFormat || "openai"] ?? "models.fmtShort.openai")}</Tag>
                </div>
                <div className="model-kv">
                  <span className="k">{t("models.modelName")}</span>
                  <Tag>{p.model || t("models.unspecified")}</Tag>
                </div>
                <div className="model-kv">
                  <span className="k">{t("models.modelList")}</span>
                  <span className="model-caps">
                    {p.models.length ? (
                      p.models.map((m) => (
                        <Tag key={m.model}>
                          {m.model === p.model ? <CheckCircleOutlined /> : null}
                          {m.model}
                          {m.vision ? <EyeOutlined style={{ marginLeft: 4 }} /> : null}
                          {m.audio ? <SoundOutlined style={{ marginLeft: 4 }} /> : null}
                        </Tag>
                      ))
                    ) : (
                      <span className="dim">{t("models.capNone")}</span>
                    )}
                  </span>
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
                <div className="model-kv">
                  <span className="k">{t("models.statusLabel")}</span>
                  {active ? (
                    <Tag className="tag-ok">{t("models.globalDefault")}</Tag>
                  ) : (
                    <span className="dim">{t("models.followGlobal")}</span>
                  )}
                </div>
              </Card>
            );
          })}
        </div>
        {!loading && profiles.length === 0 && <BrandEmpty description={t("models.none")} className="pane-empty" />}
      </Spin>

      <Card
        size="small"
        className="hud cap-card"
        title={t("models.capTitle")}
        extra={
          <Button size="small" type="primary" loading={capSaving} onClick={() => void saveCaps()}>
            {t("common.save")}
          </Button>
        }
      >
        <div className="dim cap-hint" style={{ marginBottom: 10 }}>{t("models.capGlobalHint")}</div>
        <div className="cap-grid">
          {capField("speech", t("models.capSpeech"), t("models.capSpeechHint"), t("models.capUnset"))}
          {capField("transcribe", t("models.capStt"), t("models.capSttHint"), t("models.capUnset"))}
          {capField("visionRelay", t("models.capVisionRelay"), t("models.capVisionHint"), t("models.capUnset"))}
          {capField("embedding", t("models.capEmbed"), t("models.capEmbedHint"), t("models.capEmbedPh"))}
        </div>
      </Card>

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

          <Form.Item
            name="model"
            label={t("models.defaultModel")}
            extra={t("models.defaultModelExtra")}
            tooltip={modelWatch.length === 0 ? t("models.defaultModelEmptyTip") : undefined}
          >
            <Select
              allowClear
              showSearch
              options={modelWatch.map((m) => ({ value: m.model, label: m.model }))}
              filterOption={(input, opt) => String(opt?.value ?? "").toLowerCase().includes(input.toLowerCase())}
              placeholder={modelWatch.length === 0 ? t("models.defaultModelEmptyPh") : t("models.defaultModelPh")}
              notFoundContent={t("models.defaultModelEmptyPh")}
            />
          </Form.Item>

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
                    <div key={key} className="model-row">
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
          <Form.Item
            name="fallback"
            label={t("models.fallback")}
            extra={t("models.fallbackExtra")}
          >
            <Select allowClear placeholder={t("models.noFallback")} options={fallbackOptions} />
          </Form.Item>
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
function ModelPicker({ models, selected, onChange }: {
  models: string[];
  selected: string[];
  onChange: (selected: string[]) => void;
}): React.ReactElement {
  const t = useT();
  const [q, setQ] = useState("");
  const lowered = q.trim().toLowerCase();
  const shown = lowered ? models.filter((m) => m.toLowerCase().includes(lowered)) : models;
  const allChecked = shown.length > 0 && shown.every((m) => selected.includes(m));
  const toggle = (m: string) =>
    onChange(selected.includes(m) ? selected.filter((x) => x !== m) : [...selected, m]);
  return (
    <div className="model-picker">
      <Space style={{ width: "100%", marginBottom: 8 }} direction="vertical" size={6}>
        <Input.Search
          placeholder={t("models.pickerSearch")}
          value={q}
          onChange={(e) => setQ(e.target.value)}
          allowClear
          size="small"
        />
        <Space size={8}>
          <Button size="small" type="link" onClick={() => onChange(allChecked ? selected.filter((m) => !shown.includes(m)) : Array.from(new Set([...selected, ...shown])))}>
            {allChecked ? t("models.pickerUnselectAll") : t("models.pickerSelectAll")}
          </Button>
          <span className="dim">{t("models.pickerCount", { sel: selected.length, total: models.length })}</span>
        </Space>
      </Space>
      <div className="model-picker-list">
        {shown.map((m) => {
          const on = selected.includes(m);
          return (
            <label key={m} className={`model-picker-item ${on ? "on" : ""}`}>
              <input type="checkbox" checked={on} onChange={() => toggle(m)} />
              <span className="mono">{m}</span>
            </label>
          );
        })}
        {shown.length === 0 && <div className="dim" style={{ padding: 12 }}>{t("models.pickerEmpty")}</div>}
      </div>
    </div>
  );
}
