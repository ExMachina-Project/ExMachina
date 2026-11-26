#!/usr/bin/env node
/** Capture current WebUI routes and assert stable structural selectors. */
import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { fileURLToPath } from "node:url";

const args = process.argv.slice(2);
const argOf = (name, def) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : def;
};
const BASE = argOf("--base", "http://localhost:5173");
const OUT_ROOT = argOf("--out", process.env.EXM_ARTIFACT_DIR ?? path.join(os.tmpdir(), "exmachina-ui"));
const docsRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../docs");
const outputRelativeToDocs = path.relative(docsRoot, path.resolve(OUT_ROOT));
if (!outputRelativeToDocs || (!outputRelativeToDocs.startsWith(`..${path.sep}`) && outputRelativeToDocs !== ".." && !path.isAbsolute(outputRelativeToDocs))) {
  throw new Error("UI artifacts must be written outside docs/; use a temporary directory.");
}
const KEY = process.env.EXM_KEY ?? "exmachina";

/** 页面清单：hash → 结构断言（选择器存在性；不锚文案） */
const VIEWS = [
  { name: "chat", hash: "#/chat", wait: 5000, selectors: [".app-shell[data-view=chat]", ".chat-rail", ".session-list"] },
  { name: "flows", hash: "#/flows", selectors: [".app-shell[data-view=flows]", ".flows-rail"] },
  { name: "tasks", hash: "#/tasks", selectors: [".app-shell[data-view=tasks]", ".flows-rail"] },
  { name: "graph", hash: "#/graph", selectors: [".app-shell[data-view=graph]", ".page-head"] },
  { name: "agents", hash: "#/agent", selectors: [".app-shell[data-view=agent]", ".page-head"] },
  { name: "nexus", hash: "#/nexus", selectors: [".app-shell[data-view=nexus]", ".nexus-topo-canvas"] },
  { name: "skills", hash: "#/skills", selectors: [".app-shell[data-view=skills]", ".page-head"] },
  { name: "plugins", hash: "#/plugins", selectors: [".app-shell[data-view=plugins]", ".page-head"] },
  { name: "models", hash: "#/models", selectors: [".app-shell[data-view=models]", ".page-head"] },
  { name: "automations", hash: "#/automations", selectors: [".app-shell[data-view=automations]", ".page-head"] },
  { name: "approvals", hash: "#/approvals", selectors: [".app-shell[data-view=approvals]", ".page-head"] },
  { name: "ledger", hash: "#/ledger", selectors: [".app-shell[data-view=ledger]", ".page-head"] },
  { name: "memory", hash: "#/memory", selectors: [".app-shell[data-view=memory]", ".page-head"] },
  { name: "knowledge", hash: "#/knowledge", selectors: [".app-shell[data-view=knowledge]", ".page-head"] },
  { name: "activity", hash: "#/activity", selectors: [".app-shell[data-view=activity]", ".page-head"] },
  { name: "settings", hash: "#/settings", selectors: [".app-shell[data-view=settings]", ".page-head"] },
];

const day = new Date().toISOString().slice(0, 10);
const outDir = path.join(OUT_ROOT, day);
fs.mkdirSync(outDir, { recursive: true });

// Import only after the output guard: refusing docs output must not load browser dependencies.
const { chromium } = await import("playwright-core");
const browser = await chromium.launch({
  executablePath: process.env.CHROMIUM ?? "/usr/bin/chromium",
  args: ["--no-sandbox", "--disable-dev-shm-usage"],
});
const results = [];
try {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.addInitScript((k) => localStorage.setItem("exm.key", k), KEY);
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
