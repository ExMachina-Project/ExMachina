/** 轨道环背景 —— profile/assets hero.svg 的同心虚线轨道 + 卫星点复刻。
 *
 *  资产几何（1000 口径）：三个虚线同心环（stroke 1px、dasharray "1 6"、白 6%），
 *  最内环上均布 8 颗卫星点（白 30%，其中 135° 一颗品牌紫 65%）——「集群在轨」构图。
 *  登录壳背景层：与字节雨同层（z-index 1），整环 240s 极慢自转给出在轨生命感；
 *  reduced-motion 由全局降级规则接管（动画走完一拍即停在初始相位）。
 *  颜色走 currentColor：宿主给 color 即可适配明暗主题（light 主题由 CSS 翻成深色线），
 *  品牌紫点直接引 var(--accent)，跟随强调色设置。
 */
import React from "react";

const C = 500;
/** 三层轨道半径（hero: 210/278/352 @1000×320 → 全屏壳等比放大取 250/330/420） */
const RINGS = [250, 330, 420];
/** 卫星点落位（度，0° = 正右，逆时针）：均布最内环；135° 为品牌紫点（hero 同位） */
const SATS = [0, 45, 90, 135, 180, 225, 270, 315];
const SAT_R = 250;

function pt(deg: number, r: number): [number, number] {
  const a = (deg * Math.PI) / 180;
  return [C + r * Math.cos(a), C - r * Math.sin(a)];
}

export function OrbitRings(): React.ReactElement {
  return (
    <svg
      className="orbit-rings"
      viewBox="0 0 1000 1000"
      preserveAspectRatio="xMidYMid slice"
      aria-hidden="true"
      focusable="false"
    >
      <g fill="none" stroke="currentColor" strokeWidth={1} strokeOpacity={0.06} strokeDasharray="1 6">
        {RINGS.map((r) => (
          <circle key={r} cx={C} cy={C} r={r} />
        ))}
      </g>
      {SATS.map((at) => {
        const [x, y] = pt(at, SAT_R);
        const accent = at === 135;
        return (
          <circle
            key={at}
            cx={x.toFixed(1)}
            cy={y.toFixed(1)}
            r={1.8}
            fill={accent ? "var(--accent)" : "currentColor"}
            fillOpacity={accent ? 0.65 : 0.3}
          />
        );
      })}
    </svg>
  );
}
