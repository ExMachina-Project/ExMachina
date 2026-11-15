// models 域文件：预设与 Key 池表单类型（自 ModelsView.tsx 拆出）。
import type { TKey } from "../../i18n/core";

export interface Preset {
  key: string;
  name: string;
  baseUrl: string;
  model: string;
  apiFormat: string;
}

export const PRESETS: Preset[] = [
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
export const PRESET_LABEL: Partial<Record<string, TKey>> = {
  qwen: "models.preset.qwen",
  zhipu: "models.preset.zhipu",
  ollama: "models.preset.ollama",
  minimax: "models.preset.minimax",
};

export const CUSTOM = "__custom__";

/** 页内分页：提供商 ↔ 功能分配（能力模型拆开，两件事不再混排一页） */
export type Tab = "providers" | "caps";

/** API 协议选项的显示名走 i18n（值为协议标识，与后端契约） */
export const API_FORMAT_KEYS: { value: string; key: TKey }[] = [
  { value: "openai", key: "models.fmt.openai" },
  { value: "anthropic", key: "models.fmt.anthropic" },
  { value: "gemini", key: "models.fmt.gemini" },
  { value: "azure", key: "models.fmt.azure" },
];

/** 密钥掩码哨兵：KEY_MASK 从 api.ts 引入（与后端契约一致：提交掩码位 = 服务端沿用旧值） */

/** 协议短标签（卡片上的 Tag 用），显示名走 i18n */
export const FORMAT_LABEL_KEY: Record<string, TKey> = {
  openai: "models.fmtShort.openai",
  anthropic: "models.fmtShort.anthropic",
  gemini: "models.fmtShort.gemini",
  azure: "models.fmtShort.azure",
};

/** 表单字段：密钥为可增删的动态行；掩码行携带 keep = 旧池索引（提交结构化沿用，展示仍为掩码） */
export interface KeyForm {
  keys?: { value?: string; keep?: number }[];
}

/** 取有效密钥行（去空白），保序返回行对象 */
export function liveRows(rows: KeyForm["keys"]): { value: string; keep?: number }[] {
  return (rows ?? [])
    .filter((r) => (r?.value ?? "").trim() !== "")
    .map((r) => ({ value: (r?.value ?? "").trim(), keep: r?.keep }));
}

