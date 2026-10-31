import React from "react";
import { createRoot } from "react-dom/client";
import { ConfigProvider, theme as antdTheme } from "antd";
import zhCN from "antd/locale/zh_CN";
import App from "./App";
import { accentColor, themeOf, useTheme, type ThemeKey } from "./theme";
import "./styles.css";

/** antd 原生 token 表：与 styles.css 的色阶同源，避免 antd 件与自绘面板两种底色。
 *  antd token 无法直接读 CSS 变量，因此这里按主题硬编码一份（与 CSS 里的色阶保持一致）。 */
const ANTD_TOKENS: Record<ThemeKey, Record<string, string>> = {
  minimal: {
    colorBgBase: "#fafafa",
    colorBgContainer: "#ffffff",
    colorBgElevated: "#ffffff",
    colorBgLayout: "#fafafa",
    colorBorder: "#d4d4d8",
    colorBorderSecondary: "#e4e4e7",
    colorText: "#3f3f46",
    colorTextSecondary: "#6b6b73",
    colorTextTertiary: "#6b6b74",
    colorTextQuaternary: "#6b6b74",
    colorTextPlaceholder: "#6b6b74",
    colorSuccess: "#047857",
    colorWarning: "#92400e",
    colorError: "#b91c1c",
    // 浅底浅标签：antd 派生的 *Bg 太深，文字对比只有 2.5–4.3
    colorSuccessBg: "#ecfdf5",
    colorWarningBg: "#fdf6e8",
    colorErrorBg: "#fef2f2",
    colorInfoBg: "#eff6ff",
  },
  graphite: {
    colorBgBase: "#18181b",
    colorBgContainer: "#1f1f23",
    colorBgElevated: "#27272a",
    colorBgLayout: "#18181b",
    colorBorder: "#3f3f46",
    colorBorderSecondary: "#2e2e33",
    colorText: "#d4d4d8",
    colorTextSecondary: "#a1a1aa",
    colorTextTertiary: "#94949f",
    colorTextQuaternary: "#94949f",
    colorTextPlaceholder: "#94949f",
    colorSuccessBg: "#11301f",
    colorWarningBg: "#302308",
    colorErrorBg: "#321518",
    colorInfoBg: "#161d33",
    colorSuccess: "#4ade80",
    colorWarning: "#fbbf24",
    colorError: "#f87171",
  },
};

function Root(): React.ReactElement {
  const preset = useTheme((s) => s.preset);
  const accent = useTheme((s) => s.accent);
  const mode = themeOf(preset).mode;
  const color = accentColor(preset, accent);
  const tokens = ANTD_TOKENS[preset];
  return (
    <ConfigProvider
      locale={zhCN}
      theme={{
        algorithm: mode === "light" ? antdTheme.defaultAlgorithm : antdTheme.darkAlgorithm,
        token: {
          // 高级极简：克制的纯色强调 + 中性色阶 + 统一圆角/阴影/动效节奏
          colorPrimary: color,
          colorInfo: color,
          colorLink: color,
          ...tokens,
          // 圆角纪律：智械直角风——结构圆角全部归零（LED/头像等圆形走组件内定制）
          borderRadius: 0,
          borderRadiusLG: 0,
          borderRadiusSM: 0,
          borderRadiusXS: 0,
          borderRadiusOuter: 0,
          // 动效令牌：快进慢出，全局统一节奏
          motionDurationFast: "0.12s",
          motionDurationMid: "0.2s",
          motionDurationSlow: "0.3s",
          motionEaseOut: "cubic-bezier(0.22, 1, 0.36, 1)",
          motionEaseInOut: "cubic-bezier(0.65, 0, 0.35, 1)",
          fontFamily:
            '"Inter", "Segoe UI", "PingFang SC", "Microsoft YaHei", system-ui, -apple-system, sans-serif',
        },
      }}
    >
      <App />
    </ConfigProvider>
  );
}

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <Root />
  </React.StrictMode>,
);

// PWA：Service Worker（离线壳缓存；API/WS 永远走网络）
if ("serviceWorker" in navigator && location.protocol !== "devtools:") {
  window.addEventListener("load", () => {
    navigator.serviceWorker.register("/sw.js").catch(() => {
      /* SW 不可用（如 http:// 局域网 IP 下部分浏览器限制）时静默降级为普通网页 */
    });
  });
}
