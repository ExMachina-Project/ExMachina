/**
 * 设置页：左侧分类导航 + 右侧单分类面板（一屏只看一类，杜绝堆叠杂乱）。
 * 字段定义来自后端 config_schema()；LLM 接入在「提供商」页维护。
 */
import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Alert, Button, Input, InputNumber, message, Select, Switch } from "antd";
import { ReloadOutlined, SearchOutlined, SettingOutlined } from "@ant-design/icons";
import { useExm } from "../../store";
import { useTheme } from "../../theme";
import { useT } from "../../i18n/core";
import { LANGS, useLang } from "../../i18n/core";
import { PageHeader } from "../../components/PageHeader";
import { MaskedPasswordInput } from "../../components/MaskedInput";
import { api, type ConfigSchema } from "../../api";
import { useSubbarSlot } from "../../components/Subbar";
import { useIsNarrow } from "../../useIsNarrow";
import { SettingsSaveBar } from "../../components/SettingsDialog";
import { useUnsavedChanges } from "../../settingsNavigation";

/** 分组元数据。ui 是本地偏好分区，不属于后端 config schema */
import { SECTIONS, FIELD_I18N, ENUM_OVERRIDES, readCurrent } from "./schema";
import { SanitizeStatsCard, LimitsStatsCard, NotifyPrefsCard, VersionPanel, ExperimentRunsCard } from "./cards";

export function SettingsView(): React.ReactElement {
  const { config, saveConfig } = useExm();
  const stream = useTheme((s) => s.stream);
  const lang = useLang((s) => s.lang);
  const setLang = useLang((s) => s.setLang);
  const setStream = useTheme((s) => s.setStream);
  const t = useT();
  // 分类导航挂右栏（subbar-rail，与对话/聊天/任务页同机制）；窄屏回落页内
  const subbarSlot = useSubbarSlot();
  const narrow = useIsNarrow();
  const [schema, setSchema] = useState<ConfigSchema | null>(null);
  const [values, setValues] = useState<Record<string, unknown>>({});
  const [baseline, setBaseline] = useState<Record<string, unknown>>({});
  const [query, setQuery] = useState("");
  const [focusField, setFocusField] = useState("");
  const [selected, setSelected] = useState<string>("ui");
  const [saving, setSaving] = useState(false);
  /** schema 载入态：未就绪时分类导航只剩「界面/版本更新」，面板空白，
   *  与「配置项不存在」难以区分；拉取失败同样静默塌缩且无从重试 */
  const [schemaErr, setSchemaErr] = useState<string | null>(null);
  const [schemaLoading, setSchemaLoading] = useState(true);
  /** 有未保存改动时，config 变化不再回填表单（避免把用户正在编辑的值冲掉） */
  const dirtyRef = useRef(false);
  const dirty = Object.keys(values).some((key) => JSON.stringify(values[key]) !== JSON.stringify(baseline[key]));
  useUnsavedChanges(dirty);

  /** 只拉一次 schema（配置项清单是静态元数据，不必随 config 重取） */
  const reloadSchema = useCallback(async () => {
    setSchemaLoading(true);
    try {
      const s = await api.configSchema();
      setSchemaErr(null);
      setSchema({ ...s, groups: s.groups.filter((g) => g.key !== "llm") });
    } catch (e) {
      setSchemaErr(String(e).slice(0, 160));
    } finally {
      setSchemaLoading(false);
    }
  }, []);

  useEffect(() => {
    void reloadSchema();
  }, [reloadSchema]);

  /** 按当前 config 回填表单值：仅在无未保存改动时执行 */
  useEffect(() => {
    if (!schema || dirtyRef.current) return;
    const init: Record<string, unknown> = {};
    for (const g of schema.groups) {
      for (const f of g.fields) init[f.key] = readCurrent(config ?? null, f.key);
    }
    setValues(init);
    setBaseline(init);
  }, [schema, config]);

  const groups = useMemo(() => schema?.groups ?? [], [schema]);
  const current = groups.find((g) => g.key === selected) ?? null;
  const meta = current ? SECTIONS[current.key] : undefined;

  const setField = (key: string, v: unknown) => {
    const next = { ...values, [key]: v };
    dirtyRef.current = Object.keys(next).some((field) => JSON.stringify(next[field]) !== JSON.stringify(baseline[field]));
    setValues(next);
  };
  useEffect(() => {
    if (focusField) document.querySelector(`[data-config-field="${focusField}"]`)?.scrollIntoView({ block: "center", behavior: "smooth" });
  }, [focusField, selected]);
  const searchResults = query.trim() ? groups.flatMap((group) => group.fields.flatMap((field) => {
    const label = FIELD_I18N[field.key];
    const name = label ? t(label.label) : field.label;
    const help = label?.help ? t(label.help) : field.help;
    return `${name} ${help} ${field.key}`.toLowerCase().includes(query.trim().toLowerCase()) ? [{ group, field, name }] : [];
  })) : [];

  const save = async () => {
    setSaving(true);
    try {
      const body: Record<string, unknown> = {};
      const changedRoots = new Set(Object.keys(values).filter((key) => JSON.stringify(values[key]) !== JSON.stringify(baseline[key])).map((key) => key.split(".")[0]));
      for (const [k, v] of Object.entries(values)) {
        const root = k.split(".")[0];
        if (v === undefined || !changedRoots.has(root) || (JSON.stringify(v) === JSON.stringify(baseline[k]) && !["search", "sandbox", "browser"].includes(root))) continue;
        const parts = k.split(".");
        let target = body;
        for (const part of parts.slice(0, -1)) {
          target[part] ??= {};
          target = target[part] as Record<string, unknown>;
        }
        target[parts[parts.length - 1]] = v;
      }
      // These three endpoints replace a whole section. Preserve fields outside the schema;
      // search backend credentials stay server-side by omitting the masked backend array.
      for (const root of ["search", "sandbox", "browser"] as const) if (changedRoots.has(root)) {
        const complete: Record<string, unknown> = { ...config?.[root], ...body[root] as Record<string, unknown> };
        if (root === "search") delete complete.backends;
        body[root] = complete;
      }
      await saveConfig(body);
      dirtyRef.current = false;
      const latest: Record<string, unknown> = {};
      for (const group of groups) for (const field of group.fields) latest[field.key] = readCurrent(useExm.getState().config, field.key);
      setValues(latest); setBaseline(latest);
      message.success(t("settings.savedToast"));
    } catch (e) {
      message.error(t("common.saveFailed", { err: String(e) }));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="pane-wrap settings-page general-settings-page">
      <PageHeader en="CONFIG" title={t("settings.title")} desc={t("ux.settingsDescription")} />
      <div className="settings-directory-toolbar"><Input prefix={<SearchOutlined />} aria-label={t("ux.searchSettings")} placeholder={t("ux.searchSettings")} value={query} onChange={(event) => setQuery(event.target.value)} allowClear /></div>
      {query.trim() && <div className="settings-search-results">
        {searchResults.map(({ group, field, name }) => <button type="button" className="settings-search-result" key={field.key} onClick={() => { setSelected(group.key); setQuery(""); setFocusField(field.key); }}><strong>{name}</strong><small>{SECTIONS[group.key] ? t(SECTIONS[group.key]!.titleKey) : group.label}</small></button>)}
        {!searchResults.length && <p className="settings-list-empty">{t("ux.noResults")}</p>}
      </div>}
      <div className="settings2">
        {/* 分类导航：桌面端 portal 到右栏（subbar-rail，与对话/聊天/任务页同机制）；
            窄屏回落页内顶部（settings2 变纵排）。「界面」本地偏好置顶 + 后端
            schema 分组 + 版本垫底；内层 cfg-nav-scroll 承担自身滚动 */}
        {(narrow || !subbarSlot) && (
        <nav className="cfg-nav">
        <div className="cfg-nav-scroll">
          {/* 载入中：分类列表尚不可知，给骨架而非空白（空导航会被当成「配置项没了」） */}
          {schemaLoading && !schemaErr &&
            Array.from({ length: 8 }, (_, i) => (
              <span key={`sk-${i}`} className="cfg-nav-item cfg-nav-skeleton" />
            ))}
          {/* 界面（本地偏好）：置顶——最常调，后端分组排其后 */}
          <button
            className={`cfg-nav-item ${selected === "ui" ? "on" : ""}`}
            onClick={() => setSelected("ui")}
          >
            <span className="cfg-nav-icon">{SECTIONS.ui.icon}</span>
            <span>{t("sec.ui")}</span>
          </button>
          {groups.map((g) => (
            <button
              key={g.key}
              className={`cfg-nav-item ${g.key === current?.key ? "on" : ""}`}
              onClick={() => setSelected(g.key)}
            >
              <span className="cfg-nav-icon">{SECTIONS[g.key]?.icon ?? <SettingOutlined />}</span>
              <span>{SECTIONS[g.key] ? t(SECTIONS[g.key]!.titleKey) : g.label}</span>
            </button>
          ))}
          <button className={`cfg-nav-item ${selected === "version" ? "on" : ""}`} onClick={() => setSelected("version")}>
            <span className="cfg-nav-icon">{SECTIONS.version.icon}</span>
            <span>{t("sec.version")}</span>
          </button>
        </div>
        </nav>
        )}
        {/* 右侧面板：仅当前分类 */}
        <section className="cfg-panel">
        {schemaErr ? (
          /* —— 拉取失败：明说原因并给重试，不再静默塌缩成「只有界面与版本」 —— */
          <div className="cfg-fallback">
            <Alert type="error" showIcon title={t("settings.schemaFail", { err: schemaErr })} />
            <Button type="primary" icon={<ReloadOutlined />} onClick={() => void reloadSchema()}>
              {t("common.retry")}
            </Button>
          </div>
        ) : selected === "version" ? (
          /* —— 版本与更新:本地信息 + 上游检查,不参与「保存设置」 —— */
          <VersionPanel />
        ) : selected === "ui" ? (
          /* —— 本地界面偏好：即时生效，不参与「保存设置」 —— */
          <>
            <header className="cfg-panel-head">
              <h3>{t("sec.ui")}</h3>
              <p>{t("sec.ui.desc")}。</p>
            </header>
            <div className="cfg-list">
              <div className="cfg2-row">
                <div className="cfg2-label">
                  <div className="cfg2-name">{t("settings.ui.streamName")}</div>
                  <div className="cfg2-help">{t("settings.ui.streamHelp")}</div>
                </div>
                <div className="cfg2-ctrl">
                  <Switch checked={stream === "on"} onChange={(v) => setStream(v ? "on" : "off")} />
                </div>
              </div>
              {/* 界面语言（原侧栏语言菜单收编至此）：本地偏好，即时生效 */}
              <div className="cfg2-row">
                <div className="cfg2-label">
                  <div className="cfg2-name">{t("settings.ui.langName")}</div>
                  <div className="cfg2-help">{t("settings.ui.langHelp")}</div>
                </div>
                <div className="cfg2-ctrl">
                  <Select
                    value={lang}
                    onChange={(v) => setLang(v)}
                    style={{ minWidth: 140 }}
                    options={LANGS.map((l) => ({ value: l.code, label: l.name }))}
                  />
                </div>
              </div>
            </div>
            <div className="cfg2-foot">
              <span className="dim">{t("settings.localTip")}</span>
            </div>
            <NotifyPrefsCard />
          </>
        ) : current && meta ? (
          <>
            <header className="cfg-panel-head">
              <h3>{t(meta.titleKey)}</h3>
              <p>{t(meta.descKey)}</p>
            </header>
            {current.key === "limits" && <LimitsStatsCard />}
            {current.key === "security" && <SanitizeStatsCard />}
            {current.key === "experiments" && <ExperimentRunsCard />}
            <fieldset className="settings-edit-fields" disabled={saving}><div className="cfg-list">
              {current.fields.filter((f) => {
                // 条件显示（showIf）：依赖字段当前值不匹配则整行隐藏。
                // 值取表单态（用户开了开关、未保存也立即显示配套参数——所见即所编）
                const dep = f.showIf;
                if (!dep) return true;
                const v = values[dep.field];
                if (dep.equals !== undefined) return v === dep.equals;
                if (dep.in !== undefined) return dep.in.includes(v);
                return true;
              }).map((f) => {
                const value = values[f.key];
                const fi = FIELD_I18N[f.key];
                const label = fi ? t(fi.label) : f.label;
                const ctrl = ENUM_OVERRIDES[f.key] ? (
                  <Select
                    aria-label={label}
                    style={{ width: 220 }}
                    value={String(value ?? f.default)}
                    options={ENUM_OVERRIDES[f.key].map((o) => ({ value: o.value, label: t(o.labelKey) }))}
                    onChange={(v) => setField(f.key, v)}
                  />
                ) : f.kind === "boolean" ? (
                  <Switch
                    aria-label={label}
                    checked={value === undefined ? f.default === "true" : Boolean(value)}
                    onChange={(v) => setField(f.key, v)}
                  />
                ) : f.kind === "number" ? (
                  <InputNumber
                    aria-label={label}
                    style={{ width: 180 }}
                    min={f.min}
                    max={f.max}
                    value={value === undefined || value === "" ? Number(f.default) : Number(value)}
                    onChange={(v) => setField(f.key, v)}
                  />
                ) : f.kind === "password" ? (
                  <MaskedPasswordInput
                    aria-label={label}
                    style={{ width: 320 }}
                    value={String(value ?? f.default)}
                    onChange={(e) => setField(f.key, e.target.value)}
                  />
                ) : f.key === "automation.heartbeatPrompt" || f.key === "security.execAllowlist" || f.key === "tools.custom" ? (
                  <Input.TextArea
                    aria-label={label}
                    style={{ width: 320 }}
                    rows={2}
                    value={String(value ?? f.default)}
                    onChange={(e) => setField(f.key, e.target.value)}
                  />
                ) : (
                  <Input aria-label={label} style={{ width: 320 }} value={String(value ?? f.default)} onChange={(e) => setField(f.key, e.target.value)} />
                );
                return (
                  <div className="cfg2-row" data-config-field={f.key} key={f.key}>
                    <div className="cfg2-label">
                      <div className="cfg2-name">{fi ? t(fi.label) : f.label}</div>
                      <div className="cfg2-help">{fi?.help ? t(fi.help) : f.help}</div>
                    </div>
                    <div className="cfg2-ctrl">{ctrl}</div>
                  </div>
                );
              })}
            </div></fieldset>
          </>
        ) : (
          <div className="pane-loading"><i /></div>
        )}
        </section>
      </div>
      {dirty && <div className="settings-savebar-global"><SettingsSaveBar dirty={dirty} saving={saving} label={t("settings.save")} onSave={() => void save()} onReset={() => { setValues({ ...baseline }); dirtyRef.current = false; }} scope={t("ux.changedFields", { n: Object.keys(values).filter((key) => JSON.stringify(values[key]) !== JSON.stringify(baseline[key])).length })} /></div>}
      {/* 分类导航的右栏 portal 版：与页内 .cfg-nav 完全同构（200px 卡片原样），仅位置不同 */}
      {!narrow && subbarSlot && createPortal(
        <nav className="cfg-nav">
          <div className="cfg-nav-scroll">
            {schemaLoading && !schemaErr &&
              Array.from({ length: 8 }, (_, i) => (
                <span key={`sk-r-${i}`} className="cfg-nav-item cfg-nav-skeleton" />
              ))}
            <button
              className={`cfg-nav-item ${selected === "ui" ? "on" : ""}`}
              onClick={() => setSelected("ui")}
            >
              <span className="cfg-nav-icon">{SECTIONS.ui.icon}</span>
              <span>{t("sec.ui")}</span>
            </button>
            {groups.map((g) => (
              <button
                key={g.key}
                className={`cfg-nav-item ${g.key === current?.key ? "on" : ""}`}
                onClick={() => setSelected(g.key)}
              >
                <span className="cfg-nav-icon">{SECTIONS[g.key]?.icon ?? <SettingOutlined />}</span>
                <span>{SECTIONS[g.key] ? t(SECTIONS[g.key]!.titleKey) : g.label}</span>
              </button>
            ))}
            <button className={`cfg-nav-item ${selected === "version" ? "on" : ""}`} onClick={() => setSelected("version")}>
              <span className="cfg-nav-icon">{SECTIONS.version.icon}</span>
              <span>{t("sec.version")}</span>
            </button>
          </div>
        </nav>,
        subbarSlot,
      )}
    </div>
  );
}

/** 用量统计卡（ops-visibility）：消费既有 /limits/stats 快照 */
/** 消毒命中统计（安全区顶卡）：模式 → 计数；零命中显示空态文案 */
