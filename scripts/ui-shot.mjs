#!/usr/bin/env node
/**
 * UI 视觉验证（ui-visual-language / webui-ux-overhaul 6.1）：
 * puppeteer 全页截图 + 关键 DOM 断言（只锚结构性选择器，不锚文案与像素——主题改版不脆断）。
 *
 * 用法：node scripts/ui-shot.mjs [--base http://localhost:5173] [--out docs/审计/截图]
 * 前置：webui dev server（vite，代理 /api → 生产网关）或任一承载 WebUI 的地址。
 * 输出：<out>/<YYYY-MM-DD>/<view>.png + assert.json（断言结果清单）。
 */
import { createRequire } from "node:module";
import fs from "node:fs";
import path from "node:path";

/** puppeteer-core 解析：项目内优先，回落工具目录（脚本独立于 webui 依赖树） */
function loadPuppeteer() {
  const require = createRequire(import.meta.url);
  for (const base of [process.cwd(), "/tmp/opencode"]) {
    try {
      return require(path.join(base, "node_modules", "puppeteer-core"));
    } catch { /* 尝试下一处 */ }
  }
  // 兜底：NODE_PATH 级解析（require 本身会查 node_modules）
  return require("puppeteer-core");
}
const puppeteer = loadPuppeteer();

const args = process.argv.slice(2);
const argOf = (name, def) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : def;
};
const BASE = argOf("--base", "http://localhost:5173");
const OUT_ROOT = argOf("--out", path.join("docs", "审计", "截图"));
const KEY = process.env.EXM_KEY ?? "exmachina";

/** 页面清单：hash → 结构断言（选择器存在性；不锚文案） */
const VIEWS = [
  // wait: 首屏数据量差异——chat 页拉会话列表+编排目标+遥测，冷启动常超 1.8s（实测 3~4s）
  { name: "chat", hash: "#/chat", wait: 5000, selectors: [".chat-rail", ".session-list"] },
  { name: "sessions", hash: "#/sessions", selectors: [".sessions-toolbar", ".sessions-table", ".sessions-th"] },
  { name: "models", hash: "#/models", selectors: [".model-status-row", ".cap-badge"] },
  { name: "nexus", hash: "#/nexus", selectors: [".nexus-topo-canvas", ".ant-segmented"] },
  { name: "channels", hash: "#/channels", selectors: [".page-head"] },
  { name: "groups", hash: "#/groups", selectors: [".page-head"] },
  { name: "memory", hash: "#/memory", selectors: [".page-head"] },
  { name: "activity", hash: "#/activity", selectors: [".page-head"] },
  { name: "settings", hash: "#/settings", selectors: [".page-head"] },
];

const day = new Date().toISOString().slice(0, 10);
const outDir = path.join(OUT_ROOT, day);
fs.mkdirSync(outDir, { recursive: true });

const browser = await puppeteer.launch({
  executablePath: process.env.CHROMIUM ?? "/usr/bin/chromium",
  args: ["--no-sandbox", "--disable-dev-shm-usage"],
  defaultViewport: { width: 1440, height: 900 },
});
const results = [];
try {
  const page = await browser.newPage();
  await page.evaluateOnNewDocument((k) => localStorage.setItem("exm.key", k), KEY);
  page.on("pageerror", (e) => results.push({ view: "(global)", ok: false, error: String(e).slice(0, 200) }));
  for (const v of VIEWS) {
    const entry = { view: v.name, ok: true, missing: [], shot: null };
    try {
      await page.goto(`${BASE}/${v.hash}`, { waitUntil: "domcontentloaded", timeout: 20000 });
      // 选择器轮询等待（最长 v.wait ?? 6000ms，200ms 间隔）：比固定 sleep 稳——
      // 冷启动（vite 按需编译/接口慢）时自动等到出现，快时不白等
      const waitMs = v.wait ?? 6000;
      const deadline = Date.now() + waitMs;
      for (const sel of v.selectors) {
        while (Date.now() < deadline) {
          if (await page.$(sel)) break;
          await new Promise((r) => setTimeout(r, 200));
        }
      }
      // 断言前的最终检查（轮询超时的选择器在此落账）
      for (const sel of v.selectors) {
        const hit = await page.$(sel);
        if (!hit) entry.missing.push(sel);
      }
      entry.ok = entry.missing.length === 0;
      const file = path.join(outDir, `${v.name}.png`);
      await page.screenshot({ path: file, fullPage: false });
      entry.shot = path.relative(process.cwd(), file);
    } catch (e) {
      entry.ok = false;
      entry.error = String(e).slice(0, 200);
    }
    results.push(entry);
    console.log(`${entry.ok ? "✓" : "✗"} ${v.name}${entry.missing.length ? " 缺:" + entry.missing.join(",") : ""}${entry.error ? " err:" + entry.error : ""}`);
  }
} finally {
  await browser.close();
}
fs.writeFileSync(path.join(outDir, "assert.json"), JSON.stringify(results, null, 2));
const fail = results.filter((r) => !r.ok).length;
console.log(`\n${results.length - fail}/${results.length} 页通过；截图与断言在 ${outDir}/`);
process.exit(fail > 0 ? 1 : 0);
