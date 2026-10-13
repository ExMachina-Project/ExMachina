/** 主题与界面偏好注册表 —— EXMACHINA 控制台
 *
 *  两个正交的轴：
 *  1) 主题预设 preset（决定明暗与中性色阶）—— 写入 <html data-theme>，由 CSS 接管色阶。
 *     · midnight  精炼暗色（默认）—— 蓝调中性、圆角、柔和阴影
 *     · minimal   极简浅色 —— 近白底、锌灰阶，主流极简主义
 *     · graphite  极简暗色 —— 中性石墨灰，无蓝调
 *  2) 强调色 accent（5 套纯色）—— 由 JS 内联写入 CSS 变量（--accent / --accent-rgb / --accent-fg）。
 *     同一 accent 键在不同主题下取不同明度（浅色主题需要更深的色以保证对比度）。
 *
 *  为什么 accent 走内联变量而不是 [data-theme] 选择器：预设与强调色是两个正交维度，
 *  用选择器要写 N×M 条规则；内联变量一次写入、全局生效，且方便同步给 antd token。
 *
 *  字符数据流：界面级开关（exm.stream），写入 <html data-stream> 供 CSS 隐藏画布。
 *  以上都是本地偏好，不进后端 config（设置页的 schema 面板只管网关配置）。
 *  主题/强调色的显示名走 i18n（theme.preset.<key> / theme.accent.<key>）。 */
import { create } from "zustand";

export type ThemeKey = "midnight" | "minimal" | "graphite";
export type ThemeMode = "dark" | "light";

export interface ThemePreset {
  key: ThemeKey;
  mode: ThemeMode;
  /** 切到该主题时的默认强调色键 */
  defaultAccent: string;
}

/** 主题预设清单（顺序即切换器顺序，midnight 置顶为默认） */
export const THEMES: ThemePreset[] = [
  { key: "midnight", mode: "dark", defaultAccent: "blue" },
  { key: "minimal", mode: "light", defaultAccent: "blue" },
  { key: "graphite", mode: "dark", defaultAccent: "cyan" },
];

export interface AccentTheme {
  key: string;
  /** 各主题下的色值（浅色主题取更深的一档，保证正文/描边对比度） */
  colors: Record<ThemeKey, string>;
}

/** 强调色矩阵：键名固定（cyan/blue/green/red/yellow），与 i18n 文案一一对应 */
export const ACCENTS: AccentTheme[] = [
  { key: "cyan", colors: { midnight: "#22d3ee", minimal: "#0891b2", graphite: "#22d3ee" } },
  { key: "blue", colors: { midnight: "#7c8cff", minimal: "#2563eb", graphite: "#818cf8" } },
  { key: "green", colors: { midnight: "#34d399", minimal: "#059669", graphite: "#4ade80" } },
  { key: "red", colors: { midnight: "#f87171", minimal: "#dc2626", graphite: "#f87171" } },
  { key: "yellow", colors: { midnight: "#fbbf24", minimal: "#b45309", graphite: "#fbbf24" } },
];

export type StreamPref = "on" | "off";

const savedPreset = localStorage.getItem("exm.preset") ?? "midnight";
const savedAccent = localStorage.getItem("exm.accent") ?? "";
/** 启动即应用数据流偏好（模块加载先于首帧渲染，避免闪烁） */
const savedStream: StreamPref = localStorage.getItem("exm.stream") === "off" ? "off" : "on";
document.documentElement.dataset.stream = savedStream;

export function themeOf(key: string): ThemePreset {
  return THEMES.find((t) => t.key === key) ?? THEMES[0];
}

export function accentOf(key: string): AccentTheme {
  return ACCENTS.find((a) => a.key === key) ?? ACCENTS[0];
}

/** 取某主题下某强调色的色值（十六进制） */
export function accentColor(preset: ThemeKey, accent: string): string {
  return accentOf(accent).colors[preset];
}

function hexToRgb(hex: string): string {
  const h = hex.replace("#", "");
  const n = parseInt(h.length === 3 ? h.replace(/./g, "$&$&") : h, 16);
  return `${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}`;
}

/** 把主题预设与强调色落到 DOM：data-theme 走 CSS 色阶，accent 走内联变量 */
export function applyTheme(presetKey: string, accentKey: string): void {
  const preset = themeOf(presetKey);
  const root = document.documentElement;
  root.dataset.theme = preset.key;
  root.dataset.mode = preset.mode;
  const color = accentColor(preset.key, accentKey);
  root.style.setProperty("--accent", color);
  root.style.setProperty("--accent-rgb", hexToRgb(color));
  // 亮色强调色配白字，暗色强调色配深字 —— 保证按钮/选中态上的文字可读
  root.style.setProperty("--accent-fg", preset.mode === "light" ? "#ffffff" : "#0a0b0f");
  // antd 弹层/滚动条等原生控件跟随主题明暗
  root.style.colorScheme = preset.mode;
}

/** 初始值校验：非法 preset 回落 midnight；accent 缺失时取该主题默认 */
function initialPreset(): ThemeKey {
  return THEMES.some((t) => t.key === savedPreset) ? (savedPreset as ThemeKey) : "midnight";
}
function initialAccent(preset: ThemeKey): string {
  if (accentOf(savedAccent) && savedAccent && ACCENTS.some((a) => a.key === savedAccent)) return savedAccent;
  return themeOf(preset).defaultAccent;
}

const bootPreset = initialPreset();
const bootAccent = initialAccent(bootPreset);
applyTheme(bootPreset, bootAccent);

interface ThemeState {
  preset: ThemeKey;
  setPreset: (key: ThemeKey) => void;
  accent: string;
  setAccent: (key: string) => void;
  stream: StreamPref;
  setStream: (v: StreamPref) => void;
}

export const useTheme = create<ThemeState>((set, get) => ({
  preset: bootPreset,
  setPreset: (key) => {
    if (!THEMES.some((t) => t.key === key)) return;
    localStorage.setItem("exm.preset", key);
    // 预设切换时若当前强调色在新主题下对比度不佳，仍保留用户选择（矩阵已按主题分档）
    applyTheme(key, get().accent);
    set({ preset: key });
  },
  accent: bootAccent,
  setAccent: (key) => {
    if (!ACCENTS.some((a) => a.key === key)) return;
    localStorage.setItem("exm.accent", key);
    applyTheme(get().preset, key);
    set({ accent: key });
  },
  stream: savedStream,
  setStream: (v) => {
    localStorage.setItem("exm.stream", v);
    document.documentElement.dataset.stream = v;
    set({ stream: v });
  },
}));
