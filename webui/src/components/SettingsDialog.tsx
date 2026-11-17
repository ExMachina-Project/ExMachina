import { useEffect, useRef, useState, type ReactNode } from "react";
import { Button, Modal, Tag } from "antd";
import { CheckOutlined, LoadingOutlined, SaveOutlined } from "@ant-design/icons";
import { useT } from "../i18n/core";
import { confirmDiscard, useUnsavedChanges } from "../settingsNavigation";

export interface SettingsSection {
  key: string;
  label: string;
  description?: string;
  icon?: ReactNode;
  content: ReactNode;
}

export function SettingsSaveBar({ dirty, saving, onSave, onReset, label, scope }: {
  dirty: boolean; saving?: boolean; onSave: () => void; onReset?: () => void; label?: string; scope?: string;
}) {
  const t = useT();
  return <div className="settings-savebar">
    <div className="settings-save-state" role="status">
      {saving ? <LoadingOutlined /> : dirty ? <span className="settings-dirty-dot" /> : <CheckOutlined />}
      <span>{saving ? t("ux.saving") : dirty ? t("ux.unsaved") : t("ux.upToDate")}{scope && <small>{scope}</small>}</span>
    </div>
    <div className="settings-save-actions">
      {onReset && <Button disabled={!dirty || saving} onClick={() => confirmDiscard(dirty, onReset)}>{t("ux.reset")}</Button>}
      <Button type="primary" aria-label={label || t("common.save")} icon={<SaveOutlined />} disabled={!dirty} loading={saving} onClick={onSave}>{label || t("common.save")}</Button>
    </div>
  </div>;
}

/** One configuration surface, persistent sections, and a footer outside the scroll region. */
export function SettingsDialog({ title, subtitle, sections, initialSection, dirty, saving, onSave, onClose, children, saveLabel, activeSection, onSectionChange }: {
  title: ReactNode; subtitle?: ReactNode; sections: SettingsSection[]; initialSection?: string;
  dirty: boolean; saving?: boolean; onSave: () => void; onClose: () => void; saveLabel?: string;
  children?: (content: ReactNode) => ReactNode; activeSection?: string; onSectionChange?: (key: string) => void;
}) {
  const t = useT();
  const [selected, setSelected] = useState(initialSection || sections[0]?.key);
  const current = activeSection || selected;
  const scroll = useRef<HTMLDivElement>(null);
  useEffect(() => { if (scroll.current) scroll.current.scrollTop = 0; }, [current]);
  useUnsavedChanges(dirty);
  const content = sections.map((section) => <section key={section.key} hidden={section.key !== current}
    className="settings-dialog-section" data-settings-section={section.key}>
    <header className="settings-section-head"><h3>{section.label}</h3>{section.description && <p>{section.description}</p>}</header>
    {section.content}
  </section>);
  return <Modal open title={<div className="settings-dialog-title"><span>{title}</span>{subtitle && <small>{subtitle}</small>}</div>}
    className="settings-dialog" width={1040} centered maskClosable={false} keyboard={!saving}
    onCancel={() => { if (!saving) confirmDiscard(dirty, onClose); }}
    footer={<SettingsSaveBar dirty={dirty} saving={saving} onSave={onSave} label={saveLabel} />}>
    <div className="settings-dialog-layout">
      <nav className="settings-dialog-nav" aria-label={t("ux.sections")}>
        {sections.map((section, index) => <button key={section.key} type="button" aria-label={section.label} aria-current={section.key === current ? "page" : undefined}
          onClick={() => { setSelected(section.key); onSectionChange?.(section.key); }}>
          <span className="settings-nav-index" aria-hidden="true">{section.icon || String(index + 1).padStart(2, "0")}</span><span>{section.label}</span>
        </button>)}
        <Tag className="settings-nav-tip">{t("settings.hotTip")}</Tag>
      </nav>
      <div className="settings-dialog-content" ref={scroll}>{children ? children(content) : content}</div>
    </div>
  </Modal>;
}
