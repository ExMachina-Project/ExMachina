// models 域文件：模型选择器（自 ModelsView.tsx 拆出）。
import React, { useState } from "react";
import { Button, Input, Select, Space } from "antd";
import { useT } from "../../i18n/core";
export function ModelPicker({ models, selected, onChange }: {
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
      <Space style={{ width: "100%", marginBottom: 8 }} orientation="vertical" size={6}>
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
