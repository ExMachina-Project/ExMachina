/**
 * 模型能力徽章（model-settings-ux）：视觉 / 语音 / 工具 / 嵌入能力的可视化标注。
 *
 * 口径：能力来源分「探测」（连通测试 / 模型清单拉取时标记）与「手工」（用户手选）——
 * 探测失败与不支持必须区分展示（失败 = 探测不了，不代表不支持）。
 * 未探测的模型不渲染空徽章（渲染没有信息量的空徽章只有噪音）。
 */
import React from "react";
import { EyeOutlined, SoundOutlined, ToolOutlined, ClusterOutlined } from "@ant-design/icons";
import { Tooltip } from "antd";
import { useT } from "../i18n/core";
import type { ProfileModel } from "../api";

export type BadgeSource = "detected" | "manual";

export interface BadgeItem {
  kind: "vision" | "audio" | "tool" | "embed";
  source: BadgeSource;
}

/** ProfileModel（vision/audio 布尔）→ 徽章项（当前来源均为清单标记=探测口径） */
export function badgesOfModel(m: ProfileModel): BadgeItem[] {
  const out: BadgeItem[] = [];
  if (m.vision) out.push({ kind: "vision", source: "detected" });
  if (m.audio) out.push({ kind: "audio", source: "detected" });
  return out;
}

const KIND_ICON: Record<BadgeItem["kind"], React.ReactNode> = {
  vision: <EyeOutlined />,
  audio: <SoundOutlined />,
  tool: <ToolOutlined />,
  embed: <ClusterOutlined />,
};

/** 徽章组：有徽章才渲染容器；来源 tooltip 区分探测/手工 */
export function CapabilityBadges({ items, size = "small" }: { items: BadgeItem[]; size?: "small" | "large" }): React.ReactElement | null {
  const t = useT();
  if (items.length === 0) return null;
  return (
    <span className={`cap-badges ${size}`}>
      {items.map((b) => (
        <Tooltip
          key={b.kind}
          title={t(`models.capBadge.${b.kind}.${b.source}` as never)}
        >
          <span className={`cap-badge cap-badge-${b.kind} cap-badge-${b.source}`}>
            {KIND_ICON[b.kind]}
          </span>
        </Tooltip>
      ))}
    </span>
  );
}
