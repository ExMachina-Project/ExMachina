/**
 * 空态引导卡（onboarding-selfcheck 规格）：
 * 首次上手引导——selfcheck 发现 error/warn 级缺口时在聊天页顶部给分步指引卡。
 * 口径（spec「WebUI 空态引导」）：
 * · 引导卡可关闭且关闭态持久（localStorage，不反复弹出）；
 * · 缺密钥 → 指引到模型页；无通道 → 说明控制台可用（不制造「不能用的焦虑」）；
 * · 有 error 时卡不可关（必须先解决硬缺口）——仅 warn 可「不再提示」。
 */
import React, { useEffect, useState } from "react";
import { Alert, Button, Space } from "antd";
import { api } from "../api";
import { useT } from "../i18n/core";
import { useExm } from "../store";

const DISMISS_KEY = "exm.guide.dismissed";

type CheckItem = { level: "ok" | "warn" | "error"; name: string; detail: string; hint?: string };

/** error 项的跳转目标（按缺口类型分发） */
function targetOf(item: CheckItem): { view: string; labelKey: string } | null {
  if (item.name.includes("LLM") || item.name.includes("密钥")) return { view: "models", labelKey: "guide.goModels" };
  if (item.name.includes("通道")) return { view: "channels", labelKey: "guide.goChannels" };
  return null;
}

export function GuideCard(): React.ReactElement | null {
  const t = useT();
  const [items, setItems] = useState<CheckItem[] | null>(null);
  const [dismissed, setDismissed] = useState(() => localStorage.getItem(DISMISS_KEY) === "1");

  useEffect(() => {
    // 引导只在明显需要时出现：已关闭则不拉自检（省一次探测）
    if (dismissed) return;
    api.selfcheck().then(
      (r) => setItems(r.items.filter((i) => i.level !== "ok")),
      () => setItems([]), // 自检失败不制造焦虑：静默不显示引导
    );
  }, [dismissed]);

  if (dismissed || !items || items.length === 0) return null;
  const hasError = items.some((i) => i.level === "error");
  const primary = items.find((i) => i.level === "error") ?? items[0];
  const target = targetOf(primary);

  return (
    <Alert
      className="guide-card"
      type={hasError ? "error" : "warning"}
      showIcon
      closable={!hasError /* error 级不可关：必须先解决硬缺口（spec） */}
      onClose={() => {
        localStorage.setItem(DISMISS_KEY, "1");
        setDismissed(true);
      }}
      message={t("guide.title")}
      description={
        <Space direction="vertical" size={4}>
          {items.slice(0, 3).map((i) => (
            <div key={i.name} className="guide-item">
              <span className={`guide-dot guide-${i.level}`} />
              <span>
                <b>{i.name}</b>：{i.detail}
                {i.hint && <span className="dim"> —— {i.hint}</span>}
              </span>
            </div>
          ))}
          {target && (
            <Button
              size="small"
              type="primary"
              ghost
              className="guide-btn"
              onClick={() => useExm.getState().requestNav(target.view)}
            >
              {t(target.labelKey)}
            </Button>
          )}
        </Space>
      }
    />
  );
}
