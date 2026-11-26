#!/usr/bin/env -S npx tsx
/** Browser regressions for two independent clients against isolated REST/WS fixtures. */
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import type { AddressInfo } from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { chromium, type Page, type Route, type WebSocketRoute } from "playwright-core";
import { createServer } from "vite";
import type {} from "../desktop/src/native";
import type { ChatMessage, Session, SessionSearchHit, SessionSnapshot } from "../webui/src/types";
import type { ClientIdentity, ClientIdentityPreview } from "../webui/src/api/core";
import type { ModelRouting } from "../webui/src/modelRouting";
import type { CollaborationPage, CollaborationRoom, CollaborationTurnInput } from "../client/src/api/collaborations";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const screenshotDir = process.env.UI_SCREENSHOT_DIR;
if (screenshotDir) await fs.mkdir(screenshotDir, { recursive: true });
const servers = await Promise.all(["webui", "desktop"].map((client) => createServer({
  root: path.join(root, client),
  configFile: path.join(root, client, "vite.config.ts"),
  server: { host: "127.0.0.1", port: 0, strictPort: false },
  logLevel: "error",
})));
async function stopServers(): Promise<void> {
  await Promise.all(servers.map((server) => server.close()));
}
await Promise.all(servers.map((server) => server.listen()));
const [base, desktopBase] = servers.map((server) => `http://127.0.0.1:${(server.httpServer!.address() as AddressInfo).port}`);

const ledger = { task: { goal: "fixture", acceptance: ["works"], constraints: [], forbidden: [], priorities: [] }, evidence: { confirmed: [], gaps: [] }, risk: { openAssertions: [], impact: [], revertPaths: [], blockers: [] } };
const makeSession = (id: string, workspace = "/fixture/project"): Session => ({ id, title: `Conversation ${id}`, groupId: "single:machina", workspace, status: "active", running: false, ledger, createdAt: "2026-11-16T00:00:00Z", updatedAt: "2026-11-16T00:00:00Z" });
const message = (id: string, text: string): ChatMessage => ({ id: `msg-${text}`, sessionId: id, role: "orchestrator", statements: [{ tag: "报告", text }], createdAt: "2026-11-16T00:00:00Z" });
const makeSearchHit = (sessionId: string, title: string, messageId: string, text: string): SessionSearchHit => ({
  sessionId, title, titleMatch: false, messageMatches: 1,
  snippets: [{ messageId, role: "user", text, createdAt: "2026-11-16T00:00:00Z" }],
  lastActiveAt: "2026-11-16T00:00:00Z",
});
const makeSnapshot = (turn: number, overrides: Partial<SessionSnapshot> = {}): SessionSnapshot => ({
  turn, messageCount: turn * 2, status: "done", checkpointIds: [], promptTokens: 120, completionTokens: 40,
  createdAt: `2026-11-${String(14 + turn).padStart(2, "0")}T00:00:00Z`, preview: `User input for turn ${turn}`,
  ...overrides,
});
const png = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/l9sAAAAASUVORK5CYII=", "base64");
const config = { mock: true, llm: { apiKey: "", baseUrl: "", model: "mock" }, memory: { enabled: false, recallLimit: 6, halfLifeDays: 30, semanticModel: "" }, security: { execApproval: "risky", execAllowlist: "" }, automation: {}, maxSessionTokens: 0, maxConcurrency: 2, tools: {}, trash: {}, experiments: {}, identity: {}, limits: {}, triggers: { fileWatch: {}, eventWebhook: {} }, mcpServe: {}, notify: {} };
const profile = { id: "test", name: "Fixture", model: "test", models: [], apiKey: "", baseUrl: "http://fixture.invalid" };
const schema = { configVersion: 1, groups: [{ key: "runtime", label: "运行时", fields: [
  { key: "maxConcurrency", label: "并发", kind: "number", default: "2", min: 1, max: 32 },
  { key: "compaction.triggerRatio", label: "压缩阈值", kind: "number", default: "0.6", min: 0, max: 0.95 },
  { key: "memory.enabled", label: "记忆", kind: "boolean", default: "false" },
] }] };

type Fulfill = (body: unknown, status?: number) => Promise<void>;
type Handler = (route: Route, fulfill: Fulfill) => Promise<void> | void;
async function fixture(desktop = false, viewport = { width: 1440, height: 900 }, virtualClock = false) {
  const context = await browser.newContext({ viewport });
  const page = await context.newPage();
  page.setDefaultTimeout(10_000);
  page.setDefaultNavigationTimeout(30_000);
  const errors: string[] = [];
  const state = {
    sessions: [makeSession("a"), makeSession("b")], archivedSessions: [] as Session[], messages: { a: [], b: [] } as Record<string, ChatMessage[]>,
    approvals: [] as { id: string; sessionId: string; agentId: string; command: string; status?: string }[],
    sockets: [] as WebSocketRoute[], requests: [] as { path: string; method: string; body: unknown; workspace?: string; query: Record<string, string> }[],
    chat: null as Handler | null, overrides: new Map<string, Handler>(),
    pong: true, pings: 0, fileContent: "fn main() {}\n", saved: "", workspace: "/fixture/project",
    config: structuredClone(config), target: { mode: "single", id: "machina", name: "Fixture Agent", primary: "machina" },
  };
  page.on("pageerror", (error) => errors.push(error.stack ?? error.message));
  if (virtualClock) await page.clock.install();
  await page.addInitScript(() => {
    localStorage.setItem("exm.key", "fixture");
    localStorage.setItem("exm.sfx", "0");
    window.__EXM_GATEWAY__ = { url: location.origin, key: "fixture", remote: true };
  });
  await page.route("**/api/**", async (route) => {
    const url = new URL(route.request().url());
    if (!url.pathname.startsWith("/api/")) return route.continue();
    const p = url.pathname.slice(4);
    state.requests.push({ path: p, method: route.request().method(), body: route.request().postData() ? route.request().postDataJSON() as unknown : null, workspace: route.request().headers()["x-exm-workspace"], query: Object.fromEntries(url.searchParams.entries()) });
    const fulfill = async (body: unknown, status = 200) => { await route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) }); };
    const override = state.overrides.get(p);
    if (override) return override(route, fulfill);
    if (/\/sessions\/[^/]+\/chat$/.test(p)) return state.chat ? state.chat(route, fulfill) : fulfill({ accepted: true }, 202);
    if (p === "/sessions" && route.request().method() === "GET") {
      const source = url.searchParams.has("archived") ? state.archivedSessions : state.sessions;
      const workspace = url.searchParams.get("workspace");
      const target = url.searchParams.get("target");
      return fulfill(source.filter((session) => (!workspace || session.workspace === workspace) && (!target || session.groupId === target)));
    }
    if (p === "/sessions" && route.request().method() === "POST") {
      const body = route.request().postDataJSON() as { title: string; workspace?: string; target?: string };
      const created = { ...makeSession(`new-${state.sessions.length}`, body.workspace ?? "/fixture/project"), title: body.title, groupId: body.target ?? "single:machina" };
      state.sessions.unshift(created); state.messages[created.id] = []; return fulfill(created);
    }
    if (p === "/workspace" && route.request().method() === "PUT") { state.workspace = (route.request().postDataJSON() as { path: string }).path; return fulfill({ workspace: state.workspace }); }
    if (p === "/sessions/search") return fulfill({ query: url.searchParams.get("q"), hits: [] });
    if (p === "/sessions/batch" && route.request().method() === "POST") return fulfill({ ok: true, affected: (route.request().postDataJSON() as { ids: string[] }).ids.length, failed: [] });
    if (p === "/target" && route.request().method() === "PUT") { Object.assign(state.target, route.request().postDataJSON() as object); return fulfill({ ok: true }); }
    if (p === "/config" && route.request().method() === "PUT") { Object.assign(state.config, route.request().postDataJSON() as object); return fulfill({ ok: true }); }
    const approval = p.match(/^\/approvals\/([^/]+)\/(approve|deny)$/);
    if (approval) { state.approvals = state.approvals.filter((item) => item.id !== approval[1]); return fulfill({ id: approval[1], status: approval[2] === "approve" ? "executed" : "denied" }); }
    if (p === "/fs/list") return fulfill({ path: "", entries: [
      { name: "src", path: "src", dir: true, size: 0 }, { name: "main.rs", path: "src/main.rs", dir: false, size: state.fileContent.length },
    ] });
    if (p === "/fs/file" && route.request().method() === "GET") return fulfill({ path: url.searchParams.get("path"), content: state.fileContent, size: state.fileContent.length, truncated: false, binary: false });
    if (p === "/fs/file" && route.request().method() === "PUT") { const body = route.request().postDataJSON(); state.saved = body.content; state.fileContent = body.content; return fulfill({ ok: true }); }
    if (p === "/workspace/changes") return fulfill({ source: "git", files: [{ path: "src/main.rs", status: "M", staged: false, diff: { text: "@@\n-old\n+new", truncated: false } }] });
    if (p === "/git/overview") return fulfill({ repo: true, branch: "dev", branches: ["dev"], log: ["abc fixture"] });
    if (p === "/git/op") {
      const body = route.request().postDataJSON();
      if (body.op === "discard") { state.approvals = [{ id: "git-1", sessionId: "", agentId: "desktop", command: "[git] git restore -- \"src/main.rs\"", status: "pending" }]; return fulfill({ pending: true, approvalId: "git-1", command: "git restore" }); }
      return fulfill({ ok: true, output: "" });
    }
    const session = p.match(/^\/sessions\/([^/]+)(?:\/(\w+))?$/);
    if (session) {
      const [, id, part] = session;
      if (!part) return fulfill(state.sessions.find((item) => item.id === id) ?? state.archivedSessions.find((item) => item.id === id) ?? makeSession(id));
      if (part === "messages") return fulfill(state.messages[id] ?? []);
      if (part === "graph") return fulfill({ nodes: [] });
      if (["events", "snapshots", "archive"].includes(part)) return fulfill([]);
      if (part === "tokens") return fulfill({ estimate: 12, budget: 0, unlimited: true });
      if (part === "plan") return fulfill({ sessionId: id, mode: "execute", items: [], count: 0 });
      if (part === "stop") { const item = state.sessions.find((session) => session.id === id); if (item) item.running = false; return fulfill({ ok: true, toolsExecuted: 0 }); }
      if (part === "title" || part === "pin") return fulfill({ ok: true });
    }
    const responses: Record<string, unknown> = {
      "/auth/status": { keyConfigured: true }, "/auth/verify": { ok: true },
      "/config": state.config, "/config/schema": schema,
      "/target": state.target,
      "/groups": { active: "default", groups: [{ id: "default", name: "Default", builtin: true, description: "", createdAt: "" }] },
      "/singles": { active: "machina", singles: [{ identifier: "machina", name: "Fixture Agent", description: "", domain: "" }] },
      "/agents": [{ name: "Fixture Agent", identifier: "machina", domain: "test", tier: "unit", description: "fixture", capabilities: [], tools: [] }],
      "/llm/profiles": { active: "test", mock: true, profiles: [profile] },
      "/llm/effort": { effort: "default" }, "/llm/capabilities": { speech: "", transcribe: "", embedding: "", visionRelay: "" },
      "/workspace": (() => {
        const header = route.request().headers()["x-exm-workspace"];
        return { workspace: header ? decodeURIComponent(header) : state.workspace, explicit: null, scoped: Boolean(header) };
      })(), "/health": { ok: true, mock: true, agents: 1, features: ["session-workspace-v1", "session-target-v1"] },
      "/version": { version: "0.7.0", gitHash: "fixture" }, "/audit": { items: [] },
      "/approvals": (() => {
        const workspace = url.searchParams.get("workspace");
        return workspace ? state.approvals.filter((approval) => state.sessions.find((session) => session.id === approval.sessionId)?.workspace === workspace) : state.approvals;
      })(),
      "/workbench/tree": { root: "/fixture", count: 0, nodes: [] },
      "/selfcheck": { ok: true, overall: "ok", items: [], errors: 0, warns: 0 },
      "/plugins": { plugins: [] }, "/skills": [], "/cron": [], "/cron/runs": [], "/memory/stats": { memory: { total: 0, pinned: 0, byKind: {} }, agentStats: [] }, "/memory": [], "/knowledge": { items: [], total: 0 }, "/channels": [], "/channels/flows": [],
      // These APIs return named envelopes (not bare arrays); keep empty-state fixtures on contract.
      "/skills/deleted": { skills: [] }, "/memory/forgotten": { entries: [] },
      "/plugins/index": { entries: [], indexUrl: "" },
      "/nexus/links": { links: [] }, "/nexus/edges": { edges: [] }, "/nexus/status": { links: [], edges: [] },
      "/templates": { templates: [] },
      "/nexus/graph": { nodes: [], edges: [] },
      "/identity/list": { identities: [], codes: [], identityRequired: false },
    };
    return fulfill(responses[p] ?? []);
  });
  await page.routeWebSocket(/\/ws\?/, (socket) => {
    state.sockets.push(socket);
    socket.onMessage((text) => { if (text === "ping") { state.pings += 1; if (state.pong) socket.send("pong"); } });
  });
  try {
    await page.goto(desktop ? desktopBase : base, { waitUntil: "domcontentloaded" });
    if (desktop) {
      await page.waitForSelector("#btn-send", { state: "visible", timeout: 30_000 });
      await page.waitForFunction(async (url) => { const { useExm } = await import(url) as typeof import("../webui/src/store"); return useExm.getState().sessionId === "a"; }, `${desktopBase}/src/testing.ts`);
    } else {
      await page.waitForSelector(".chat-composer textarea", { state: "attached", timeout: 30_000 });
      if (viewport.width <= 860) await page.locator(".session-row").filter({ hasText: "Conversation a" }).click();
      await page.waitForSelector(".chat-composer textarea", { state: "visible" });
    }
  } catch (error) {
    const main = (await page.locator("body").innerText({ timeout: 1_000 }).catch(() => "")).slice(0, 600);
    await context.close();
    throw new Error(`${desktop ? "Desktop" : "WebUI"} fixture did not boot. Page: ${main}\nErrors: ${errors.join("\n")}`, { cause: error });
  }
  return {
    page, state, errors,
    emit: (type: string, payload: Record<string, unknown> = {}, sessionId = "a") => state.sockets.at(-1)!.send(JSON.stringify({ type, payload, sessionId })),
    close: async () => { await context.close(); assert.deepEqual(errors, [], "No uncaught browser errors"); },
    screenshot: async (name: string) => { if (screenshotDir) await page.screenshot({ path: path.join(screenshotDir, `${name}.png`) }); },
  };
}

let browser: Awaited<ReturnType<typeof chromium.launch>>;
const chromiumPath = process.env.CHROMIUM || await fs.access("/usr/bin/chromium").then(() => "/usr/bin/chromium", () => chromium.executablePath());
try { browser = await chromium.launch({ executablePath: chromiumPath, args: ["--no-sandbox", "--disable-dev-shm-usage"] }); }
catch (error) { await stopServers(); throw error; }

let passed = 0;
async function test(name: string, run: () => Promise<void>): Promise<void> {
  if (process.env.UI_TEST_FILTER && !name.includes(process.env.UI_TEST_FILTER)) return;
  const timeout = setTimeout(() => { console.error(`Timed out: ${name}`); void browser.close(); }, 90_000);
  try { await run(); passed += 1; console.log(`✓ ${name}`); }
  finally { clearTimeout(timeout); }
}
const storeUrl = (page: Page) => `${new URL(page.url()).origin}/src/${page.url().startsWith(desktopBase) ? "testing" : "store"}.ts`;
const webState = (page: Page) => page.evaluate(async (url) => {
  const { useExm } = await import(url) as typeof import("../webui/src/store"); const state = useExm.getState();
  return { messages: state.messages, running: state.running, drafts: state.drafts, approvals: state.approvals, sessionId: state.sessionId, liveOrch: state.liveOrch };
}, storeUrl(page));
async function eventually(check: () => boolean | Promise<boolean>, note: string): Promise<void> {
  const until = Date.now() + 5_000;
  while (!(await check())) { assert.ok(Date.now() < until, note); await new Promise((resolve) => setTimeout(resolve, 20)); }
}
const stop = (desktop: boolean) => desktop ? "#btn-stop" : ".composer-stop";
const inputSelector = (desktop: boolean) => desktop ? "#input" : ".chat-composer textarea";
const sendSelector = (desktop: boolean) => desktop ? "#btn-send" : ".composer-send";
const fileSelector = (desktop: boolean) => desktop ? "#file-pick" : '.chat-composer input[type="file"]';
const selectSession = (page: Page, id: string) => page.evaluate(async ({ url, id }) => {
  const { useExm } = await import(url) as typeof import("../webui/src/store"); await useExm.getState().selectSession(id);
}, { url: storeUrl(page), id });
async function screenshot(page: Page, name: string): Promise<void> {
  if (!screenshotDir) return;
  if (name.includes("settings") || name.includes("allocation")) await page.waitForFunction(() =>
    !Array.from(document.querySelectorAll(".settings-dialog .ant-btn-loading, .settings-page .ant-btn-loading, .settings-page .ant-spin-spinning"))
      .some((element) => element.getClientRects().length));
  await page.screenshot({ path: path.join(screenshotDir, `${name}.png`), animations: "disabled" });
}
async function openSettings(page: Page, desktop: boolean, section: "模型" | "智能体" | "高级") {
  if (desktop) {
    await page.getByRole("button", { name: "设置", exact: true }).click();
    await page.getByRole("tab", { name: section, exact: true }).click();
  } else await page.evaluate((hash) => { location.hash = hash; }, `#/${section === "模型" ? "models" : section === "智能体" ? "agent" : "settings"}`);
}
async function settingsSection(page: Page, name: string) {
  await page.locator(".settings-dialog-nav button").filter({ hasText: name }).click();
}
async function selectRoutingMode(page: Page, purpose: string, mode: string) {
  await page.locator(`[data-routing-purpose="${purpose}"]:visible .routing-mode`).click();
  await page.locator(".ant-select-dropdown:visible").getByText(mode, { exact: true }).click();
}

try {
  await test("WebUI operation context preserves memory and knowledge routes without claiming graph migration", async () => {
    const f = await fixture();
    try {
      f.state.overrides.set("/memory/md", (_, fulfill) => fulfill({ content: "Existing source", path: "memory.md" }));
      const baseline = f.state.requests.length;
      for (const [route, title] of [["memory", "记忆图"], ["knowledge", "知识库"], ["graph", "编排图"]]) {
        await f.page.evaluate((route) => { location.hash = `#/${route}`; }, route);
        await f.page.locator(".page-title").filter({ hasText: title }).waitFor();
        assert.equal(new URL(f.page.url()).hash, `#/${route}`);
        if (route === "memory") await f.page.getByRole("note").filter({ hasText: "实体与事实图谱尚未实现" }).waitFor();
      }
      await f.page.locator(".operation-target-trigger").click();
      await f.page.getByRole("dialog", { name: "当前操作对象", exact: true }).getByText(/不创建会话或执行模型/).waitFor();
      assert.equal(f.state.requests.slice(baseline).some((request) => request.method !== "GET"), false);
    } finally { await f.close(); }
  });

  await test("WebUI operation context selects typed targets without switching resources or writing configuration", async () => {
    const f = await fixture();
    try {
      f.state.overrides.set("/singles", (_, fulfill) => fulfill({ singles: [
        { identifier: "machina", name: "Fixture Agent", description: "", domain: "" },
        { identifier: "unused", name: "Unused Agent", description: "", domain: "" },
      ] }));
      f.state.overrides.set("/groups", (_, fulfill) => fulfill({ active: "default", groups: [{ id: "machina", name: "Fixture Agent", builtin: false, createdAt: "" }] }));
      await f.page.locator(".operation-target-trigger").click();
      const popover = f.page.getByRole("dialog", { name: "当前操作对象", exact: true });
      await popover.locator('[data-target-key="group:machina"]').waitFor();
      assert.equal(await popover.locator('[data-target-key="single:machina"]').innerText(), "Fixture Agent\n独立智能体 · machina");
      assert.equal(await popover.locator('[data-target-key="group:machina"]').innerText(), "Fixture Agent\n智能体组 · machina");
      const baseline = f.state.requests.length;
      const state = await webState(f.page);
      const initialUrl = f.page.url();
      await popover.locator('[data-target-key="group:machina"]').click();
      assert.equal((await webState(f.page)).sessionId, state.sessionId);
      assert.equal(f.page.url(), initialUrl, "Selection must preserve the route (including an empty initial hash)");
      assert.equal(f.state.requests.slice(baseline).some((request) => request.method !== "GET"), false, "Selection must never create/switch/write/stop");
      await f.page.locator(".operation-target-trigger").click();
      await popover.getByRole("combobox").fill("Unused");
      await popover.locator('[data-target-key="single:unused"]').click();
      await f.page.reload();
      await f.page.locator(".operation-target-trigger").getByText("Unused Agent", { exact: true }).waitFor();
      await f.page.locator('.operation-selector[data-availability="available"]').waitFor();
      await f.page.waitForSelector(".chat-composer textarea", { state: "attached" });
      await eventually(async () => (await webState(f.page)).sessionId !== null, "Session initialization must finish after reload");
      assert.equal((await webState(f.page)).sessionId, state.sessionId, "Refreshing a UI default must not open a different conversation");
      assert.equal(f.state.requests.some((request) => request.path === "/target" && request.method === "PUT"), false);
    } finally { await f.close(); }
  });

  await test("WebUI operation context retains unavailable targets and exposes empty, search and retry states", async () => {
    const f = await fixture();
    try {
      await f.page.locator('.operation-selector[data-availability="available"]').waitFor();
      f.state.overrides.set("/singles", (_, fulfill) => fulfill({ error: "directory failed" }, 503));
      await f.page.locator(".operation-target-trigger").click();
      const popover = f.page.getByRole("dialog", { name: "当前操作对象", exact: true });
      await popover.getByRole("alert").filter({ hasText: "目录读取失败" }).waitFor();
      assert.equal(await f.page.locator(".operation-target-trigger strong").innerText(), "Fixture Agent");
      f.state.overrides.set("/singles", (_, fulfill) => fulfill({ singles: [] }));
      f.state.overrides.set("/groups", (_, fulfill) => fulfill({ active: "default", groups: [] }));
      await popover.getByRole("button", { name: "刷新目录", exact: true }).click();
      await popover.getByText("没有可用对象。", { exact: true }).waitFor();
      await popover.getByRole("alert").filter({ hasText: "当前对象已失效，请重新选择。" }).waitFor();
      assert.equal(await f.page.locator(".operation-target-trigger strong").innerText(), "Fixture Agent");
      await popover.getByRole("button", { name: "不设默认对象", exact: true }).click();
      await f.page.locator(".operation-target-trigger strong").getByText("不设默认对象", { exact: true }).waitFor();
      await f.page.locator(".operation-target-trigger").click();
      await popover.getByRole("combobox").fill("unknown");
      await popover.getByText("没有匹配的对象。", { exact: true }).waitFor();
    } finally { await f.close(); }
  });

  for (const viewport of [{ width: 1440, height: 900 }, { width: 390, height: 844 }, { width: 420, height: 480 }]) {
    await test(`WebUI operation context sidebar keyboard and geometry ${viewport.width}x${viewport.height}`, async () => {
      const f = await fixture(false, viewport);
      try {
        const longName = `Default ${"非常长的对象名称".repeat(12)}`;
        f.state.overrides.set("/groups", (_, fulfill) => fulfill({ active: "default", groups: [{ id: "default", name: longName, builtin: true, createdAt: "" }] }));
        if (viewport.width <= 860) await f.page.getByRole("button", { name: "menu", exact: true }).click();
        assert.equal(await f.page.locator(".brand-sub").count(), 0);
        assert.ok((await f.page.locator(".brand").boundingBox())!.height < 50, "Removed subtitle must not leave an empty row");
        const menu = await f.page.locator(".nav-menu").getByRole("menuitem").allTextContents();
        const index = menu.findIndex((item) => item.includes("自动化"));
        assert.ok(index >= 0 && menu[index + 1].includes("记忆图") && menu[index + 2].includes("知识库"), "Three-character navigation items must stay adjacent and ordered");
        await f.page.locator('.operation-selector[data-availability="available"]').waitFor();
        const trigger = f.page.locator(".operation-target-trigger");
        await trigger.focus(); await trigger.press("Enter");
        const popover = f.page.getByRole("dialog", { name: "当前操作对象", exact: true });
        const search = popover.getByRole("combobox");
        await search.waitFor();
        await search.fill("Default");
        await popover.locator('.operation-option:enabled').first().waitFor();
        await search.press("ArrowDown"); await search.press("Enter");
        await trigger.getByText(longName, { exact: true }).waitFor();
        await trigger.press("Enter");
        await search.waitFor(); await search.press("Escape");
        await eventually(() => trigger.evaluate((element) => element === document.activeElement), "Esc must restore trigger focus");
        await trigger.click(); await search.waitFor();
        const bounds = await popover.boundingBox();
        assert.ok(bounds && bounds.x >= 0 && bounds.y >= 0 && bounds.x + bounds.width <= viewport.width && bounds.y + bounds.height <= viewport.height, `Target picker must fit viewport: ${JSON.stringify(bounds)}`);
        assert.equal(await f.page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
        await screenshot(f.page, `operation-context-${viewport.width}x${viewport.height}`);
        if (viewport.width <= 860) {
          await search.press("Escape");
          await f.page.getByRole("button", { name: "menu", exact: true }).click();
          await f.page.locator(".mobile-operation-target").click();
          await popover.waitFor();
          await search.press("Escape");
        }
      } finally { await f.close(); }
    });
  }

  await test("WebUI stays on its own instance despite a legacy remote address", async () => {
    const f = await fixture();
    try {
      const apiOrigins: string[] = [];
      f.page.on("request", (request) => {
        const url = new URL(request.url());
        if (url.pathname.startsWith("/api/")) apiOrigins.push(url.origin);
      });
      await f.page.evaluate(() => localStorage.setItem("exm.server", "https://legacy-remote.invalid"));
      await f.page.reload({ waitUntil: "domcontentloaded" });
      await f.page.locator(".chat-composer textarea").waitFor();
      assert.ok(apiOrigins.length > 0);
      assert.ok(apiOrigins.every((origin) => origin === base), "WebUI must never send instance credentials to a saved remote address");
      f.state.overrides.set("/auth/verify", (_, fulfill) => fulfill({ ok: false }));
      await f.page.reload({ waitUntil: "domcontentloaded" });
      await f.page.locator(".login-box input[type=password]").waitFor();
      assert.equal(await f.page.locator(".login-remote-toggle").count(), 0);
      assert.equal(await f.page.locator(".login-box input").count(), 1);
    } finally { await f.close(); }
  });
  // Desktop intentionally keeps instance administration out of the coding shell;
  // the WebUI suite remains the owner of model/agent/provider settings coverage.
  for (const desktop of [false]) {
    const client = desktop ? "Desktop" : "WebUI";
    await test(`${client} settings UX preserves multi-section agent drafts and retries failed saves`, async () => {
      const f = await fixture(desktop);
      try {
        const agent = { identifier: "machina", name: "Fixture Agent", description: "Handles product questions", domain: "support", modelRouting: null as ModelRouting | null };
        let prompt = "Follow the project instructions.";
        let persona = "Clear, thoughtful, and precise.";
        let rejectPrompt = true;
        f.state.overrides.set("/singles", (_, fulfill) => fulfill({ singles: [agent] }));
        f.state.overrides.set("/singles/machina", (route, fulfill) => { Object.assign(agent, route.request().postDataJSON()); return fulfill(agent); });
        f.state.overrides.set("/singles/machina/model-routing", (route, fulfill) => {
          if (route.request().method() === "PUT") agent.modelRouting = route.request().postDataJSON() as ModelRouting;
          return fulfill({ ok: true, policies: [] });
        });
        f.state.overrides.set("/singles/machina/prompt", (route, fulfill) => {
          if (route.request().method() === "PUT") {
            if (rejectPrompt) return fulfill({ error: "fixture prompt temporarily unavailable" }, 503);
            prompt = (route.request().postDataJSON() as { prompt: string }).prompt;
          }
          return fulfill({ prompt });
        });
        f.state.overrides.set("/singles/machina/persona", (route, fulfill) => {
          if (route.request().method() === "PUT") persona = (route.request().postDataJSON() as { persona: string }).persona;
          return fulfill({ persona, custom: true });
        });
        f.state.overrides.set("/llm/profiles", (_, fulfill) => fulfill({ active: "test", profiles: [{ ...profile, models: [{ model: "small", vision: false, audio: false, cost: "economy" }] }] }));
        await openSettings(f.page, desktop, "智能体");
        await f.page.locator(".agent-directory-card").filter({ hasText: "Fixture Agent" }).getByRole("button", { name: /配置智能体/ }).click();
        const dialog = f.page.locator(".settings-dialog");
        await dialog.getByRole("textbox", { name: "名称", exact: true }).fill("Product Assistant");
        await settingsSection(f.page, "系统提示词");
        await dialog.getByRole("textbox", { name: "系统提示词", exact: true }).fill("Updated instructions kept between sections.");
        await settingsSection(f.page, "模型与能力");
        await selectRoutingMode(f.page, "default", "手动指定");
        await dialog.getByRole("combobox", { name: "默认对话与任务 模型", exact: true }).click();
        await f.page.locator(".ant-select-dropdown:visible .ant-select-item-option").filter({ hasText: "Fixture / small" }).click();
        await settingsSection(f.page, "基本资料");
        assert.equal(await dialog.getByRole("textbox", { name: "名称", exact: true }).inputValue(), "Product Assistant");
        await dialog.locator(".ant-modal-close").click();
        await f.page.getByRole("button", { name: "继续编辑", exact: true }).click();
        await dialog.getByRole("button", { name: "保存", exact: true }).click();
        await dialog.getByRole("alert").filter({ hasText: "fixture prompt temporarily unavailable" }).waitFor();
        assert.equal(agent.name, "Product Assistant");
        assert.equal(agent.modelRouting?.default?.model, "test/small");
        assert.equal(prompt, "Follow the project instructions.");
        await settingsSection(f.page, "系统提示词");
        assert.equal(await dialog.getByRole("textbox", { name: "系统提示词", exact: true }).inputValue(), "Updated instructions kept between sections.");
        rejectPrompt = false;
        await dialog.getByRole("button", { name: "保存", exact: true }).click();
        await eventually(() => prompt.startsWith("Updated instructions"), "Prompt retry was not saved");
        await eventually(async () => dialog.getByRole("button", { name: "保存", exact: true }).isDisabled(), "Successful save did not clear dirty state");
        assert.equal(f.state.requests.filter((request) => request.path === "/singles/machina" && request.method === "PUT").length, 1, "Retry must not repeat completed sections");
        assert.equal(f.state.requests.filter((request) => request.path === "/singles/machina/model-routing" && request.method === "PUT").length, 1);
        await settingsSection(f.page, "人设与语气");
        await dialog.getByRole("textbox", { name: "人设与语气", exact: true }).fill("Warm and concise.");
        await dialog.getByRole("button", { name: "保存", exact: true }).click();
        await eventually(() => persona === "Warm and concise.", "Persona was not saved");
        await screenshot(f.page, `${desktop ? "desktop" : "webui"}-settings-agent`);
        assert.equal(f.state.requests.filter((request) => request.path === "/target" && request.method === "PUT").length, 0);
      } finally { await f.close(); }
    });
    await test(`${client} model page stays instance-scoped and saves only global capabilities`, async () => {
      const f = await fixture(desktop);
      try {
        f.state.overrides.set("/llm/profiles", (_, fulfill) => fulfill({ active: "test", profiles: [{ ...profile, models: [{ model: "small", purposes: ["speech"], vision: false, audio: true }] }] }));
        let capabilities = { speech: "", transcribe: "", visionRelay: "", embedding: "" };
        f.state.overrides.set("/llm/capabilities", (route, fulfill) => {
          if (route.request().method() === "PUT") capabilities = route.request().postDataJSON() as typeof capabilities;
          return fulfill(capabilities);
        });
        await openSettings(f.page, desktop, "模型");
        assert.equal(await f.page.getByRole("tab", { name: "智能体分配", exact: true }).count(), 0);
        assert.equal(await f.page.locator(".allocation-workspace").count(), 0);
        await f.page.getByRole("tab", { name: "全局能力", exact: true }).click();
        await f.page.getByRole("combobox", { name: "语音合成（TTS）", exact: true }).click();
        await f.page.locator(".ant-select-dropdown:visible .ant-select-item-option").filter({ hasText: "Fixture / small" }).click();
        await f.page.locator(".global-cap-workspace").getByRole("button", { name: "保存", exact: true }).click();
        await eventually(() => capabilities.speech === "test/small", "Global capability did not save");
        assert.equal(f.state.requests.filter((request) => request.path.includes("model-routing") && request.method === "PUT").length, 0);
        await screenshot(f.page, `${desktop ? "desktop" : "webui"}-settings-global-capabilities`);
      } finally { await f.close(); }
    });
    await test(`${client} settings UX preserves key indexes and catalog metadata when editing a provider`, async () => {
      const f = await fixture(desktop);
      try {
        const mask = "***已配置***";
        let saved = { ...profile, apiKey: mask, apiKeys: [mask, mask, mask],
          models: [{ model: "small", vision: true, audio: false, cost: "economy", autoEligible: true, purposes: ["chat"], usageNotes: "Fast everyday tasks", contextWindow: 128000 }] };
        const originalModels = structuredClone(saved.models);
        let posted: { apiKey: string; apiKeys: (string | { keep: number })[]; clearKeys: boolean; models: typeof saved.models } | undefined;
        f.state.overrides.set("/llm/profiles", (route, fulfill) => {
          if (route.request().method() === "POST") {
            posted = route.request().postDataJSON() as typeof posted;
            saved = { ...saved, ...posted, apiKey: mask, apiKeys: [mask, mask] };
            return fulfill({ ok: true, id: "test" });
          }
          return fulfill({ active: "test", profiles: [saved] });
        });
        await openSettings(f.page, desktop, "模型");
        await f.page.locator(".provider-workspace .settings-detail-actions").getByRole("button", { name: "编辑", exact: true }).click();
        let dialog = f.page.locator(".settings-dialog");
        assert.ok(await dialog.getByRole("button", { name: "保存", exact: true }).isDisabled(), "Opening a saved profile must not mark it dirty");
        await dialog.locator(".key-row").nth(2).getByRole("button", { name: "移除这把密钥", exact: true }).click();
        await dialog.getByRole("button", { name: /添加密钥/ }).click();
        await dialog.locator(".key-row").last().getByRole("textbox").fill("fixture-new-key");
        await settingsSection(f.page, "模型清单");
        await dialog.getByRole("textbox", { name: "模型 1", exact: true }).fill("small");
        await settingsSection(f.page, "连接与密钥");
        await dialog.getByRole("button", { name: "保存", exact: true }).click();
        await dialog.waitFor({ state: "hidden" });
        assert.ok(posted);
        assert.equal(posted.apiKey, mask);
        assert.deepEqual(posted.apiKeys, [{ keep: 0 }, "__CLEAR__", { keep: 2 }, "fixture-new-key"]);
        assert.equal(posted.clearKeys, false);
        assert.deepEqual(posted.models, originalModels, "Collapsed metadata must survive edits");
        await f.page.locator(".provider-workspace .settings-detail-actions").getByRole("button", { name: "编辑", exact: true }).click();
        dialog = f.page.locator(".settings-dialog");
        while (await dialog.locator(".key-row").count()) await dialog.locator(".key-row").first().getByRole("button", { name: "移除这把密钥", exact: true }).click();
        await dialog.getByRole("button", { name: "保存", exact: true }).click();
        await dialog.waitFor({ state: "hidden" });
        assert.equal(posted.clearKeys, true);
        await screenshot(f.page, `${desktop ? "desktop" : "webui"}-settings-provider`);
      } finally { await f.close(); }
    });
    await test(`${client} settings UX keeps group defaults and member capabilities in their own scope`, async () => {
      const f = await fixture(desktop);
      try {
        const group = { id: "default", name: "Engineering", builtin: true, primary: "chief", description: "Review and implementation", createdAt: "", workspace: "", selfOptimize: true, modelRouting: null as ModelRouting | null };
        const member = { identifier: "reviewer", name: "Reviewer", description: "Review code changes", domain: "review", tier: "unit", tools: ["read", "grep"], capabilities: [], modelRouting: null as ModelRouting | null };
        const chief = { ...member, identifier: "chief", name: "Lead", description: "Coordinate the engineering group" };
        f.state.overrides.set("/groups", (_, fulfill) => fulfill({ active: "default", groups: [group] }));
        f.state.overrides.set("/groups/default", (route, fulfill) => { Object.assign(group, route.request().postDataJSON()); return fulfill(group); });
        f.state.overrides.set("/groups/default/agents", (_, fulfill) => fulfill([chief, member]));
        f.state.overrides.set("/groups/default/overview", (_, fulfill) => fulfill({ agents: 2, sessions: 3, playbooks: 1, memory: { group: 2, shared: 1 }, agentStats: [] }));
        f.state.overrides.set("/groups/default/workspace", (route, fulfill) => { group.workspace = (route.request().postDataJSON() as { workspace: string }).workspace; return fulfill({ ok: true }); });
        f.state.overrides.set("/groups/default/model-routing", (route, fulfill) => { group.modelRouting = route.request().postDataJSON() as ModelRouting; return fulfill({ ok: true }); });
        f.state.overrides.set("/groups/default/self-optimize", (route, fulfill) => { group.selfOptimize = (route.request().postDataJSON() as { enabled: boolean }).enabled; return fulfill({ ok: true }); });
        f.state.overrides.set("/groups/default/agents/reviewer/model-routing", (route, fulfill) => {
          if (route.request().method() === "PUT") member.modelRouting = route.request().postDataJSON() as ModelRouting;
          return fulfill({ ok: true, policies: [] });
        });
        f.state.overrides.set("/groups/default/agents/reviewer/prompt", (_, fulfill) => fulfill({ prompt: "Review the code carefully." }));
        f.state.overrides.set("/agents/reviewer/persona", (_, fulfill) => fulfill({ persona: "Precise reviewer", custom: true }));
        f.state.overrides.set("/llm/profiles", (_, fulfill) => fulfill({ active: "test", profiles: [{ ...profile, models: [{ model: "small", vision: true, audio: false, cost: "economy" }] }] }));
        await openSettings(f.page, desktop, "智能体");
        await f.page.locator(".group-card").getByRole("button", { name: /组设置/ }).click();
        let dialog = f.page.locator(".settings-dialog");
        await dialog.getByRole("textbox", { name: "组名", exact: true }).fill("Engineering Studio");
        await dialog.getByRole("textbox", { name: "工作区", exact: true }).fill("/fixture/engineering");
        assert.ok(await dialog.getByRole("textbox", { name: "组 ID", exact: true }).isDisabled());
        await settingsSection(f.page, "模型与能力");
        await selectRoutingMode(f.page, "default", "手动指定");
        await dialog.getByRole("combobox", { name: "默认对话与任务 模型", exact: true }).click();
        await f.page.locator(".ant-select-dropdown:visible .ant-select-item-option").filter({ hasText: "Fixture / small" }).click();
        await settingsSection(f.page, "协作与自我优化");
        await dialog.getByRole("switch", { name: "自我优化", exact: true }).click();
        await dialog.getByRole("button", { name: "保存", exact: true }).click();
        await eventually(async () => dialog.getByRole("button", { name: "保存", exact: true }).isDisabled(), "Group save did not finish");
        assert.equal(group.selfOptimize, false); assert.equal(group.workspace, "/fixture/engineering"); assert.equal(group.modelRouting?.default?.model, "test/small");
        await dialog.locator(".ant-modal-close").click();
        await f.page.locator(".group-card").getByRole("button", { name: /管理成员/ }).click();
        await f.page.locator(".group-member-row").filter({ hasText: "Reviewer" }).getByRole("button", { name: /配置智能体/ }).click();
        dialog = f.page.locator(".settings-dialog");
        await settingsSection(f.page, "模型与能力");
        await dialog.getByRole("tab", { name: /多模态与检索/ }).click();
        await selectRoutingMode(f.page, "speech", "关闭此能力");
        await dialog.getByRole("button", { name: "保存", exact: true }).click();
        await eventually(async () => dialog.getByRole("button", { name: "保存", exact: true }).isDisabled(), "Member save did not finish");
        assert.equal(member.modelRouting?.purposes?.speech?.mode, "disabled");
        assert.equal(group.modelRouting?.purposes?.speech, undefined);
        await settingsSection(f.page, "人设与语气");
        assert.ok(await dialog.getByRole("button", { name: /恢复默认/ }).isDisabled(), "Factory persona protection must remain visible");
        assert.equal(f.state.requests.filter((request) => request.path === "/target" && request.method === "PUT").length, 0);
        await screenshot(f.page, `${desktop ? "desktop" : "webui"}-settings-group-member`);
      } finally { await f.close(); }
    });
  }
  await test("WebUI settings UX searches across sections and protects unsaved navigation", async () => {
    const f = await fixture();
    try {
      await openSettings(f.page, false, "高级");
      const search = f.page.getByRole("textbox", { name: "搜索所有设置…", exact: true });
      await search.fill("子个体并发数");
      await f.page.locator(".settings-search-result").filter({ hasText: "子个体并发数" }).click();
      const concurrency = f.page.locator('[data-config-field="maxConcurrency"]').getByRole("spinbutton");
      await concurrency.fill("4");
      f.page.once("dialog", (dialog) => { void dialog.dismiss(); });
      await f.page.locator(".nav-menu").getByText("模型", { exact: true }).click();
      assert.ok(f.page.url().endsWith("#/settings"));
      assert.equal(await concurrency.inputValue(), "4");
      await f.page.locator(".settings-savebar-global").getByRole("button", { name: "保存设置", exact: true }).click();
      await f.page.locator(".settings-savebar-global").waitFor({ state: "hidden" });
      const update = f.state.requests.findLast((request) => request.path === "/config" && request.method === "PUT")!.body;
      assert.deepEqual(update, { maxConcurrency: 4 });
      await f.page.locator(".nav-menu").getByText("模型", { exact: true }).click();
      await f.page.locator(".provider-workspace").waitFor();
    } finally { await f.close(); }
  });
  await test("WebUI settings directory searches model catalogs and agent profiles", async () => {
    const f = await fixture();
    try {
      const models = [
        { model: "gpt-6.1", vision: true, audio: false, cost: "premium", purposes: ["planning", "coding", "review"], usageNotes: "复杂规划、代码实施与高风险审查", autoEligible: true },
        { model: "gpt-6-mini", vision: true, audio: false, cost: "economy", purposes: ["chat", "summary"], usageNotes: "日常对话、摘要与轻量整理", autoEligible: true },
        { model: "text-embedding-3-small", vision: false, audio: false, cost: "economy", purposes: ["embedding"], usageNotes: "记忆向量与语义检索", autoEligible: false },
      ];
      f.state.overrides.set("/llm/profiles", (_, fulfill) => fulfill({ active: "primary", profiles: [
        { ...profile, id: "primary", name: "OpenAI", baseUrl: "https://api.example.com/v1", apiKey: "***已配置***", model: "gpt-6.1", models },
        { ...profile, id: "local", name: "本地推理", baseUrl: "http://localhost:11434/v1", models: [{ ...models[1], model: "local-model" }] },
      ] }));
      await openSettings(f.page, false, "模型");
      const search = f.page.getByRole("textbox", { name: "搜索提供商或模型…", exact: true });
      await search.fill("gpt-6-mini");
      assert.equal(await f.page.locator(".provider-workspace .settings-object-list button").count(), 1);
      await search.fill("");
      await f.page.locator(".provider-workspace .settings-detail-head h3").getByText("OpenAI", { exact: true }).waitFor();
      await screenshot(f.page, "webui-settings-provider-catalog");
      await f.page.locator(".provider-workspace .settings-detail-actions").getByRole("button", { name: "编辑", exact: true }).click();
      await settingsSection(f.page, "模型清单");
      await f.page.locator(".provider-model-field").first().locator(".ant-collapse-header").click();
      await screenshot(f.page, "webui-settings-model-catalog-editor");
      await f.page.locator(".settings-dialog .ant-modal-close").click();
      f.state.overrides.set("/singles", (_, fulfill) => fulfill({ singles: [
        { identifier: "assistant", name: "工作助理", description: "日常问答、资料整理与内容总结", domain: "general", modelHint: "primary/gpt-6-mini" },
        { identifier: "reviewer", name: "代码审查", description: "审查代码质量、验证改动与分析风险", domain: "review", modelHint: "primary/gpt-6.1" },
      ] }));
      await openSettings(f.page, false, "智能体");
      await f.page.getByRole("textbox", { name: "搜索智能体、组或标识…", exact: true }).fill("审查");
      assert.equal(await f.page.locator(".agent-directory-card").count(), 1);
      await f.page.getByRole("textbox", { name: "搜索智能体、组或标识…", exact: true }).fill("");
      await screenshot(f.page, "webui-settings-agent-directory");
    } finally { await f.close(); }
  });
  for (const viewport of [{ width: 1916, height: 867 }, { width: 1440, height: 900 }]) {
    await test(`WebUI model workspace fills available height and scrolls its catalog ${viewport.width}x${viewport.height}`, async () => {
      const f = await fixture(false, viewport);
      try {
        await f.page.evaluate(async (url) => {
          const { useTheme } = await import(url) as typeof import("../webui/src/theme");
          useTheme.getState().setPreset("minimal");
        }, `${base}/src/theme.ts`);
        let models = [{ model: "small", vision: true, audio: false }];
        f.state.overrides.set("/llm/profiles", (_, fulfill) => fulfill({ active: "test", profiles: [{ ...profile, apiKey: "***已配置***", models }] }));
        await openSettings(f.page, false, "模型");
        await f.page.locator(".provider-model-entry").first().waitFor();
        const geometry = await f.page.evaluate(() => {
          const content = document.querySelector(".app-content")!.getBoundingClientRect();
          const workspace = document.querySelector(".provider-workspace")!.getBoundingClientRect();
          const list = document.querySelector(".provider-workspace .settings-list-panel")!.getBoundingClientRect();
          const detail = document.querySelector(".provider-workspace .settings-detail-panel")!.getBoundingClientRect();
          const footer = document.querySelector(".provider-owner-hint")!.getBoundingClientRect();
          return { bottomGap: content.bottom - workspace.bottom, panelGap: Math.abs(list.bottom - detail.bottom), footerGap: workspace.bottom - footer.bottom };
        });
        assert.ok(geometry.bottomGap >= 0 && geometry.bottomGap <= 16, `Dead space below model workspace: ${JSON.stringify(geometry)}`);
        assert.ok(geometry.panelGap <= 1 && geometry.footerGap <= 20, `Provider panels/actions do not fill their allocated workspace: ${JSON.stringify(geometry)}`);
        await screenshot(f.page, `webui-model-height-${viewport.width}x${viewport.height}`);
        models = Array.from({ length: 40 }, (_, index) => ({ model: `catalog-${index}`, vision: true, audio: false }));
        await f.page.getByRole("button", { name: "刷新", exact: true }).click();
        await f.page.locator(".provider-model-entry").filter({ hasText: "catalog-39" }).waitFor({ state: "attached" });
        const scroll = await f.page.locator(".provider-model-list").evaluate((element) => {
          element.scrollTop = element.scrollHeight;
          const bounds = element.getBoundingClientRect();
          const workspace = element.closest(".provider-workspace")!.getBoundingClientRect();
          return { scrolls: element.scrollHeight > element.clientHeight && element.scrollTop > 0, contained: bounds.bottom <= workspace.bottom, height: element.clientHeight };
        });
        assert.ok(scroll.scrolls && scroll.contained && scroll.height >= 80, `Catalog should scroll inside the workspace: ${JSON.stringify(scroll)}`);
        assert.ok(await f.page.locator(".provider-owner-hint").isVisible());
      } finally { await f.close(); }
    });
  }
  for (const viewport of [{ width: 1440, height: 900 }, { width: 420, height: 820 }, { width: 420, height: 480 }]) {
    await test(`WebUI console keeps every route inside a compact viewport ${viewport.width}x${viewport.height}`, async () => {
      const f = await fixture(false, viewport);
      try {
        f.state.sessions.push(...Array.from({ length: 18 }, (_, index) => makeSession(`layout-${index}`)),
          { ...makeSession("layout-job"), title: "job-layout" });
        f.state.overrides.set("/singles", (_, fulfill) => fulfill({ active: "machina", singles: Array.from({ length: 12 }, (_, index) => ({
          identifier: `agent-${index}`, name: `Fixture Agent ${index}`, description: "Compact layout fixture",
          domain: "test", modelHint: "test/small", modelRouting: null,
        })) }));
        f.state.overrides.set("/groups", (_, fulfill) => fulfill({ active: "default", groups: [{ id: "default", name: "Default", builtin: true, description: "Fixture group" }] }));
        f.state.overrides.set("/llm/profiles", (_, fulfill) => fulfill({ active: "test", mock: true, profiles: [{ ...profile, models: [{ model: "small", vision: true, audio: false, cost: "economy" }] }] }));
        f.state.overrides.set("/memory/md", (_, fulfill) => fulfill({ content: "fixture memory", path: "/fixture/memory.md" }));
        f.state.overrides.set("/memory", (_, fulfill) => fulfill(Array.from({ length: 12 }, (_, index) => ({
          id: `memory-${index}`, kind: index % 2 ? "fact" : "decision", scope: "group", title: `Long fixture memory ${index} — compact display check`,
          body: "A dense but readable memory entry with enough text to verify that content wraps within its table cell instead of widening the console. ".repeat(2),
          tags: ["fixture", "layout"], importance: 0.8, confidence: 0.9, pinned: index === 0, createdAt: "2026-11-18T00:00:00Z", updatedAt: "2026-11-18T00:00:00Z", accessCount: index,
        }))));
        f.state.overrides.set("/memory/stats", (_, fulfill) => fulfill({ memory: { total: 24, pinned: 3, individual: 14, shared: 10, terms: 30, byKind: {}, dbPath: "/fixture/memory.sqlite" }, agentStats: [{ agentId: "fixture-agent-with-a-long-name", runs: 18, done: 13, blocked: 2, failed: 3, avgConfidence: 0.83, updatedAt: "2026-11-18T00:00:00Z" }] }));
        f.state.overrides.set("/knowledge", (_, fulfill) => fulfill({ total: 12, items: Array.from({ length: 8 }, (_, index) => ({
          id: `knowledge-${index}`, title: `Long fixture knowledge item ${index}`, body: "Shared knowledge content that should wrap within a compact list row. ".repeat(2), tags: ["fixture", "reference"], sourceAgent: "fixture-agent-with-a-long-name", hits: 8, createdAt: "2026-11-18T00:00:00Z", updatedAt: "2026-11-18T00:00:00Z",
        })) }));
        f.state.overrides.set("/skills", (_, fulfill) => fulfill(Array.from({ length: 9 }, (_, index) => ({
          id: `skill-${index}`, name: `Long fixture skill ${index}`, description: "A short skill description for checking card density.", triggers: ["review", "verify"], instructions: "Detailed skill steps remain collapsed until requested.\n".repeat(8), agents: [], createdAt: "2026-11-18T00:00:00Z", useCount: 14, successCount: 12,
        }))));
        f.state.overrides.set("/plugins", (_, fulfill) => fulfill({ plugins: Array.from({ length: 8 }, (_, index) => ({ id: `plugin-${index}`, name: `Long fixture plugin ${index}`, version: "1.0.0", description: "A plugin summary that should stay compact in a responsive card grid.", form: "declarative", enabled: true, capabilities: { tools: 2, watch: false, resources: 0, process: false } })) }));
        f.state.overrides.set("/plugins/index", (_, fulfill) => fulfill({ entries: Array.from({ length: 8 }, (_, index) => ({ id: `catalog-${index}`, name: `Catalog fixture ${index}`, version: "1.0.0", description: "Plugin catalog description wraps inside its table cell.", status: "available" })), indexUrl: "fixture" }));
        f.state.overrides.set("/cron", (_, fulfill) => fulfill(Array.from({ length: 8 }, (_, index) => ({ id: `cron-${index}`, name: `Long scheduled task ${index}`, prompt: "Fixture task prompt", kind: "cron", cron: "0 9 * * *", group: "default", enabled: true, createdAt: "2026-11-18T00:00:00Z", nextRunAt: "2026-11-19T09:00:00Z", lastStatus: "done" }))));
        f.state.overrides.set("/audit", (_, fulfill) => fulfill({ items: Array.from({ length: 12 }, (_, index) => ({ id: `audit-${index}`, callId: `call-${index}`, tool: `tool-${index}`, agentId: "fixture-agent-with-a-long-name", ok: index % 4 !== 0, summary: "Fixture audit summary containing enough information to wrap safely on narrow displays.", args: { path: "/fixture/a-very-long-file-name.rs" }, createdAt: "2026-11-18T00:00:00Z", durationMs: 240 })) }));
        f.state.overrides.set("/nexus/links", (_, fulfill) => fulfill({ links: Array.from({ length: 6 }, (_, index) => ({ id: `nexus-${index}`, name: `External connection ${index}`, kind: "custom", command: "external-agent --run", enabled: true })) }));
        f.state.overrides.set("/nexus/graph", (_, fulfill) => fulfill({ nodes: Array.from({ length: 8 }, (_, index) => ({ id: `nexus-${index}`, name: `External agent ${index}`, kind: "custom", enabled: true })), edges: Array.from({ length: 8 }, (_, index) => ({ agentId: `agent-${index}`, agentName: `Long fixture agent name ${index}`, linkId: `nexus-${index}` })) }));
        f.state.overrides.set("/nexus/status", (_, fulfill) => fulfill({ links: [], edges: [] }));
        f.state.overrides.set("/identity/list", (_, fulfill) => fulfill({ identityRequired: true, identities: Array.from({ length: 6 }, (_, index) => ({ id: `identity-${index}`, channel: `channel-${index}`, externalId: `external-id-${index}`, displayName: `Long paired user name ${index}`, role: "member", pairedAt: "2026-11-18T00:00:00Z", note: "fixture" })), codes: Array.from({ length: 6 }, (_, index) => ({ code: `pair-${index}`, note: `Long fixture pairing note ${index}`, platform: "napcat", createdAt: "2026-11-18T00:00:00Z", expiresAtMs: Date.now() + 60_000, expired: false, role: "member" })) }));
        f.state.approvals = Array.from({ length: 8 }, (_, index) => ({ id: `approval-${index}`, sessionId: "a", agentId: `fixture-agent-${index}`, command: `cargo test --package fixture-crate --test a-very-long-test-name-${index} -- --nocapture`, status: "pending" }));
        f.state.overrides.set("/channels", (_, fulfill) => fulfill(Array.from({ length: 5 }, (_, index) => ({ id: `channel-${index}`, type: "napcat", enabled: true, account: `Fixture account ${index}`, group: "default", capability: "chat", createdAt: "2026-11-18T00:00:00Z" }))));
        f.state.overrides.set("/channels/status", (_, fulfill) => fulfill(Object.fromEntries(Array.from({ length: 5 }, (_, index) => [`channel-${index}`, { state: "ok", detail: "Fixture channel online", at: "2026-11-18T00:00:00Z" }]))));
        f.state.overrides.set("/channels/flows", (_, fulfill) => fulfill(Array.from({ length: 8 }, (_, index) => ({ id: `flow-${index}`, channelId: `channel-${index % 5}`, chatKey: `group-${index}`, displayName: `Long external conversation ${index}`, isGroup: true, activeSession: `layout-${index}`, unread: index, lastMessage: "A compact preview of a long fixture conversation.", lastActiveAt: "2026-11-18T00:00:00Z", createdAt: "2026-11-18T00:00:00Z" }))));
        await f.page.evaluate(async (url) => {
          const { useExm } = await import(url) as typeof import("../webui/src/store");
          const current = useExm.getState().config;
          if (current) useExm.setState({ config: { ...current, memory: { ...current.memory, enabled: true } } });
        }, storeUrl(f.page));
        const routes = ["chat", "flows", "tasks", "agent", "nexus", "graph", "automations", "skills", "plugins", "models", "approvals", "ledger", "memory", "knowledge", "activity", "settings"];
        const contentSelectors: Record<string, string> = {
          chat: ".chat-shell", flows: ".flows-rail .flow-item", tasks: ".flows-rail .flow-item", agent: ".agent-directory-card",
          nexus: ".ant-table", graph: ".graph-empty", automations: ".ant-table-tbody > .ant-table-row",
          skills: ".skill-card", plugins: ".card-grid .ant-card", models: ".provider-model-entry", approvals: ".ant-table-tbody > .ant-table-row",
          ledger: ".ledger-wrap .ant-card", memory: ".memory-stats", knowledge: ".memory-hits .ant-list-item", activity: ".event-stream", settings: ".cfg-panel",
        };
        for (const route of routes) {
          await f.page.evaluate((next) => { location.hash = `#/${next}`; }, route);
          await f.page.waitForFunction((expected) => document.querySelector(".app-shell")?.getAttribute("data-view") === expected, route);
          await f.page.waitForFunction(() => !document.querySelector(".view-loading"));
          await f.page.locator(contentSelectors[route]).first().waitFor({ state: route === "flows" ? "attached" : "visible", timeout: 10_000 });
          const layout = await f.page.evaluate(() => {
            const shell = document.querySelector<HTMLElement>(".app-shell")!;
            const content = document.querySelector<HTMLElement>(".app-content")!;
            const main = content.getBoundingClientRect();
            return {
              shellOverflow: shell.scrollWidth > innerWidth + 1,
              contentOverflow: content.scrollWidth > content.clientWidth + 1,
              mainOutsideViewport: main.left < -1 || main.right > innerWidth + 1 || main.bottom > innerHeight + 1,
              hiddenByFooter: !!document.querySelector(".telemetry-bar") && getComputedStyle(document.querySelector(".telemetry-bar")!).display !== "none" && main.bottom > document.querySelector(".telemetry-bar")!.getBoundingClientRect().top + 1,
            };
          });
          assert.equal(layout.shellOverflow, false, `${route} overflows shell: ${JSON.stringify(layout)}`);
          assert.equal(layout.contentOverflow, false, `${route} overflows content: ${JSON.stringify(layout)}`);
          assert.equal(layout.mainOutsideViewport, false, `${route} escapes viewport: ${JSON.stringify(layout)}`);
          assert.equal(layout.hiddenByFooter, false, `${route} is obscured by telemetry: ${JSON.stringify(layout)}`);
        }
      } finally { await f.close(); }
    });
  }
  for (const desktop of [false]) for (const viewport of [{ width: 1440, height: 900 }, { width: 420, height: 820 }, { width: 420, height: 480 }]) {
    await test(`${desktop ? "Desktop" : "WebUI"} settings layout keeps fields and save actions reachable ${viewport.width}x${viewport.height}`, async () => {
      const f = await fixture(desktop, viewport);
      try {
        f.state.overrides.set("/llm/profiles", (_, fulfill) => fulfill({ active: "test", profiles: [{ ...profile, apiKey: "***已配置***", models: [{ model: "small", vision: true, audio: false, cost: "economy", autoEligible: true, purposes: ["chat", "coding"], usageNotes: "Fast routine tasks" }] }] }));
        await openSettings(f.page, desktop, "模型");
        await f.page.locator(".provider-workspace .settings-detail-actions").getByRole("button", { name: "编辑", exact: true }).click();
        const dialog = f.page.locator(".settings-dialog");
        for (const section of ["连接与密钥", "模型清单", "客户端伪装", "高级（回退与覆盖）"]) {
          await settingsSection(f.page, section);
          const save = await dialog.getByRole("button", { name: "保存", exact: true }).boundingBox();
          assert.ok(save && save.x >= 0 && save.y >= 0 && save.x + save.width <= viewport.width + 1 && save.y + save.height <= viewport.height + 1, `${section} save action outside viewport: ${JSON.stringify(save)}`);
          const fits = await dialog.evaluate((element) => element.scrollWidth <= element.clientWidth + 1);
          assert.ok(fits, `${section} has horizontal overflow`);
        }
        await settingsSection(f.page, "模型清单");
        const catalogDetails = dialog.locator(".provider-model-field .ant-collapse-header").first();
        await catalogDetails.scrollIntoViewIfNeeded();
        await catalogDetails.click();
        const switches = dialog.locator(".provider-capability-switches .ant-form-item");
        await switches.first().waitFor();
        assert.equal(await switches.count(), 3);
        const heights = await switches.evaluateAll((items) => items.map((item) => item.getBoundingClientRect().height));
        assert.ok(heights.every((height) => height <= 48), `Model capability options remain compact: ${heights.join(", ")}`);
        if (!desktop) assert.ok(!(await f.page.locator(".telemetry-bar").innerText()).includes("MODEL"));
        await settingsSection(f.page, "客户端伪装");
        await dialog.getByTestId("client-identity-preset").click();
        await f.page.locator(".ant-select-dropdown:visible").getByText("Codex CLI", { exact: true }).click();
        await dialog.getByTestId("client-identity-headers").fill('{"x-client-name":"fixture"}');
        await dialog.getByTestId("client-identity-preview-button").scrollIntoViewIfNeeded();
        const content = await dialog.locator(".settings-dialog-content").evaluate((element) => ({ scrollHeight: element.scrollHeight, clientHeight: element.clientHeight, scrollTop: element.scrollTop }));
        assert.ok(content.clientHeight > 50, JSON.stringify(content));
        await screenshot(f.page, `${desktop ? "desktop" : "webui"}-settings-${viewport.width}x${viewport.height}`);
      } finally { await f.close(); }
    });
  }
  for (const desktop of [false]) {
    await test(`${desktop ? "Desktop" : "WebUI"} model rules are configured inside the agent's own settings`, async () => {
      const f = await fixture(desktop);
      try {
        const agent = { identifier: "machina", name: "Fixture Agent", description: "fixture", domain: "test", modelRouting: null as ModelRouting | null };
        const models = [{ model: "small", vision: false, audio: false, purposes: ["chat", "coding"], cost: "economy", autoEligible: true, usageNotes: "Routine coding and summaries" }];
        f.state.overrides.set("/llm/profiles", (_, fulfill) => fulfill({ active: "test", mock: true, profiles: [{ ...profile, models }] }));
        f.state.overrides.set("/singles", (_, fulfill) => fulfill({ active: "machina", singles: [agent] }));
        f.state.overrides.set("/singles/machina/model-routing", (route, fulfill) => {
          if (route.request().method() === "PUT") agent.modelRouting = route.request().postDataJSON() as ModelRouting;
          return fulfill({ ...(route.request().method() === "PUT" ? { ok: true } : {}), policies: [] });
        });
        f.state.overrides.set("/singles/machina/prompt", (_, fulfill) => fulfill({ prompt: "Help the user." }));
        f.state.overrides.set("/singles/machina/persona", (_, fulfill) => fulfill({ persona: "Friendly", custom: false }));
        if (desktop) {
          await f.page.getByRole("button", { name: "设置", exact: true }).click();
          await f.page.getByRole("tab", { name: "智能体", exact: true }).click();
        } else await f.page.evaluate(() => { location.hash = "#/agent"; });
        await f.page.locator(".agent-directory-card").filter({ hasText: "Fixture Agent" }).getByRole("button", { name: /配置智能体/ }).click();
        const dialog = f.page.locator(".settings-dialog");
        await settingsSection(f.page, "模型与能力");
        const defaultRow = dialog.locator('[data-routing-purpose="default"]');
        await defaultRow.locator(".routing-mode").click(); await f.page.getByText("由指挥体分配", { exact: true }).last().click();
        await dialog.getByRole("tab", { name: /多模态与检索/ }).click();
        const speechRow = dialog.locator('[data-routing-purpose="speech"]');
        await speechRow.locator(".routing-mode").click(); await f.page.getByText("关闭此能力", { exact: true }).last().click();
        await dialog.locator(".routing-purpose-tabs .ant-tabs-tab").nth(2).click();
        await dialog.getByRole("textbox", { name: "给指挥体的分配说明", exact: true }).fill("Routine tasks use the economy model.");
        await dialog.getByRole("button", { name: "保存", exact: true }).click();
        await eventually(() => f.state.requests.some((r) => r.path === "/singles/machina/model-routing" && r.method === "PUT"), "Agent allocation was not saved");
        const saved = f.state.requests.findLast((r) => r.path === "/singles/machina/model-routing" && r.method === "PUT")!.body as { default: { mode: string }; purposes: { speech: { mode: string } }; instructions: string };
        assert.equal(saved.default.mode, "auto"); assert.equal(saved.purposes.speech.mode, "disabled"); assert.ok(saved.instructions.includes("economy"));
        assert.equal(f.state.requests.filter((r) => r.path === "/llm/capabilities" && r.method === "PUT").length, 0);
        assert.equal(f.state.requests.filter((r) => r.path === "/target" && r.method === "PUT").length, 0);
        await screenshot(f.page, `${desktop ? "desktop" : "webui"}-agent-owned-model-settings`);
      } finally { await f.close(); }
    });
    await test(`${desktop ? "Desktop" : "WebUI"} client identity saves, reloads, validates and resets`, async () => {
      const f = await fixture(desktop);
      try {
        let saved = { ...profile, clientIdentity: { preset: "default", userAgent: "", headers: {} } as ClientIdentity };
        f.state.overrides.set("/llm/profiles", (route, fulfill) => {
          if (route.request().method() === "POST") {
            saved = { ...saved, ...route.request().postDataJSON() };
            return fulfill({ ok: true, id: profile.id });
          }
          return fulfill({ active: profile.id, mock: true, profiles: [saved] });
        });
        const preview = (body: { clientIdentity: ClientIdentity; apiFormat?: string }): ClientIdentityPreview => ({
          preset: body.clientIdentity.preset, version: body.clientIdentity.version ?? "0.169.0", requestMode: body.clientIdentity.requestMode ?? "native",
          configuredFormat: body.apiFormat ?? "openai", effectiveFormat: body.clientIdentity.requestMode === "headers" ? "openai" : body.clientIdentity.preset === "codex" ? "responses" : "anthropic",
          path: body.clientIdentity.requestMode === "headers" ? "/chat/completions" : body.clientIdentity.preset === "codex" ? "/responses" : "/v1/messages",
          headers: { "user-agent": `${body.clientIdentity.preset === "codex" ? "codex_cli_rs" : "claude-cli"}/${body.clientIdentity.version || "0.169.0"}`, originator: "codex_cli_rs" },
          metadata: body.clientIdentity.preset === "claude-code", nativeCompatible: body.clientIdentity.requestMode !== "headers", upstreamVerified: false,
        });
        f.state.overrides.set("/llm/identity", (route, fulfill) => fulfill(preview(route.request().postDataJSON())));
        f.state.overrides.set("/llm/test", (route, fulfill) => fulfill({ ok: true, status: 200, identity: preview(route.request().postDataJSON()) }));
        if (desktop) {
          await f.page.getByRole("button", { name: "设置", exact: true }).click();
          await f.page.getByRole("tab", { name: "模型", exact: true }).click();
        } else {
          await f.page.evaluate(() => { location.hash = "#/models"; });
        }
        await f.page.locator(".model-card").first().waitFor();
        await f.page.locator(".provider-workspace .settings-detail-actions").getByRole("button", { name: "编辑", exact: true }).click();
        const modal = f.page.locator(".ant-modal:not(.desk-settings)").last();
        await modal.getByText("客户端伪装", { exact: true }).first().click();
        await modal.getByTestId("client-identity-preset").click();
        await f.page.getByText("Claude Code", { exact: true }).last().click();
        await modal.getByTestId("client-identity-version").fill("2.1.326");
        await modal.getByTestId("client-identity-user-agent").fill("fixture-cli/1.2");
        const headers = modal.getByTestId("client-identity-headers");
        await headers.fill('{"Authorization":"forbidden"}');
        await f.page.locator(".ant-modal-footer .ant-btn-primary").last().click();
        await modal.getByText(/请输入最多 32 项的 JSON 对象/).waitFor();
        assert.equal(f.state.requests.filter((r) => r.path === "/llm/profiles" && r.method === "POST").length, 0);
        await headers.fill('{"x-client-name":"fixture","version":"2.1.326","X-OpenRouter-Title":"Fixture"}');
        await f.page.locator(".ant-modal-footer .ant-btn-primary").last().click();
        await eventually(() => saved.clientIdentity.preset === "claude-code", "Client identity was not saved");
        assert.deepEqual(saved.clientIdentity, { preset: "claude-code", version: "2.1.326", requestMode: "native", userAgent: "fixture-cli/1.2", headers: { "x-client-name": "fixture", version: "2.1.326", "X-OpenRouter-Title": "Fixture" } });
        await modal.waitFor({ state: "hidden" });
        await f.page.locator(".provider-workspace .settings-detail-actions").getByRole("button", { name: "编辑", exact: true }).click();
        await modal.getByText("客户端伪装", { exact: true }).first().click();
        assert.equal(await modal.getByTestId("client-identity-user-agent").inputValue(), "fixture-cli/1.2");
        assert.ok((await headers.inputValue()).includes("x-client-name"));
        assert.equal(await modal.getByTestId("client-identity-version").inputValue(), "2.1.326");
        await modal.getByTestId("client-identity-preset").click();
        await f.page.getByText("Codex CLI", { exact: true }).last().click();
        assert.equal(await modal.getByTestId("client-identity-user-agent").inputValue(), "");
        assert.equal(await headers.inputValue(), "");
        assert.equal(await modal.getByTestId("client-identity-version").inputValue(), "");
        await modal.getByTestId("client-identity-version").fill("0.169.0");
        await modal.getByTestId("client-identity-preview-button").click();
        await modal.getByTestId("client-identity-preview").getByText("/responses", { exact: true }).waitFor();
        const previewText = await modal.getByTestId("client-identity-preview").textContent();
        assert.ok(previewText?.includes("codex_cli_rs/0.169.0"), JSON.stringify({previewText, request: f.state.requests.findLast((r) => r.path === "/llm/identity")}));
        await modal.getByTestId("client-identity-version").fill("0.170.0");
        await modal.getByTestId("client-identity-preview").waitFor({ state: "hidden" });
        await modal.getByTestId("client-identity-test-button").click();
        await modal.getByTestId("client-identity-preview").getByText("/responses", { exact: true }).waitFor();
        const tested = f.state.requests.findLast((r) => r.path === "/llm/test")!.body as { id: string; clientIdentity: ClientIdentity };
        assert.equal(tested.id, "test"); assert.equal(tested.clientIdentity.version, "0.170.0");
        assert.equal(saved.clientIdentity.preset, "claude-code", "Draft probes must not save changes");
        await modal.getByTestId("client-identity-mode").click();
        await f.page.getByText("仅覆盖请求头", { exact: true }).last().click();
        await modal.getByTestId("client-identity-preview-button").click();
        await modal.getByTestId("client-identity-preview").getByText("/chat/completions", { exact: true }).waitFor();
        assert.ok((await modal.getByTestId("client-identity-preview").innerText()).includes("当前协议与所选客户端不同"));
        await screenshot(f.page, `${desktop ? "desktop" : "webui"}-client-identity`);
        await modal.getByRole("button", { name: "恢复默认身份", exact: true }).click();
        await f.page.locator(".ant-modal-footer .ant-btn-primary").last().click();
        await eventually(() => saved.clientIdentity.preset === "default", "Client identity was not disabled");
        assert.deepEqual(saved.clientIdentity, { preset: "default" });
      } catch (error) {
        await screenshot(f.page, `${desktop ? "desktop" : "webui"}-client-identity-failure`);
        if (screenshotDir) await fs.writeFile(path.join(screenshotDir, `${desktop ? "desktop" : "webui"}-client-identity-failure.txt`), await f.page.locator("body").innerText());
        throw error;
      } finally { await f.close(); }
    });
  }
  await test("WebUI rejected send retains editable text and images", async () => {
    const f = await fixture();
    try {
      f.state.chat = (_, fulfill) => fulfill({ error: "fixture rejection" }, 400);
      const input = f.page.locator(".chat-composer textarea");
      await input.fill("retry this");
      await f.page.locator('.chat-composer input[type="file"]').setInputFiles({ name: "test.png", mimeType: "image/png", buffer: png });
      await f.page.locator(".composer-send").click();
      await f.page.waitForFunction(() => document.body.textContent?.includes("fixture rejection"));
      assert.equal(await input.inputValue(), "retry this");
      assert.equal(await f.page.locator(".attach-thumb").count(), 1);
      const state = await webState(f.page); assert.equal(state.running, false); assert.equal(state.messages.length, 0);
    } finally { await f.close(); }
  });
  await test("WebUI pending send preserves a newer draft", async () => {
    const f = await fixture();
    try {
      let finish!: () => void;
      f.state.chat = async (_, fulfill) => { await new Promise<void>((resolve) => { finish = resolve; }); await fulfill({ accepted: true }, 202); };
      const input = f.page.locator(".chat-composer textarea"); await input.fill("first"); await f.page.locator(".composer-send").click(); await input.fill("next draft"); await eventually(() => Boolean(finish), "Chat request did not start"); finish();
      await f.page.waitForFunction(() => document.querySelector<HTMLTextAreaElement>(".chat-composer textarea")?.value === "next draft");
      assert.equal(f.state.requests.filter((r) => r.path.endsWith("/chat")).length, 1);
    } finally { await f.close(); }
  });
  await test("Desktop opens a project, edits and saves a file", async () => {
    const f = await fixture(true);
    try {
      await f.page.getByRole("button", { name: "查看代码", exact: true }).click();
      await f.page.getByText("main.rs").click();
      await f.page.waitForSelector(".cm-content");
      await f.page.locator(".cm-content").click(); await f.page.keyboard.press("Control+End"); await f.page.keyboard.type("// changed");
      await f.page.getByRole("button", { name: "保存文件", exact: true }).click();
      await eventually(() => f.state.saved.includes("// changed"), "Editor save did not reach API");
      await screenshot(f.page, "desktop-editor");
    } finally { await f.close(); }
  });
  await test("Desktop Git discard creates an approval without executing it", async () => {
    const f = await fixture(true);
    try {
      await f.page.getByRole("button", { name: "查看变更", exact: true }).click();
      await f.page.locator(".desk-change summary").click();
      await f.page.getByRole("button", { name: "丢弃修改", exact: true }).click();
      await f.page.getByRole("button", { name: "创建审批" }).click();
      await eventually(() => f.state.requests.some((r) => r.path === "/git/op"), "Git action was not sent");
      const operation = f.state.requests.filter((r) => r.path === "/git/op"); assert.equal(operation.length, 1);
    } finally { await f.close(); }
  });
  for (const desktop of [false, true]) {
    const name = desktop ? "Desktop" : "WebUI";
    await test(`${name} isolates drafts and attachments between sessions`, async () => {
      const f = await fixture(desktop);
      try {
        const input = f.page.locator(inputSelector(desktop));
        await input.fill("draft A"); await f.page.locator(fileSelector(desktop)).setInputFiles({ name: "a.png", mimeType: "image/png", buffer: png });
        await f.page.waitForSelector(".attach-thumb");
        await selectSession(f.page, "b");
        assert.equal(await input.inputValue(), ""); assert.equal(await f.page.locator(".attach-thumb").count(), 0);
        await input.fill("draft B"); await selectSession(f.page, "a");
        await eventually(async () => await input.inputValue() === "draft A", "Session A draft was not restored");
        assert.equal(await f.page.locator(".attach-thumb").count(), 1);
        await selectSession(f.page, "b"); assert.equal(await input.inputValue(), "draft B");
      } finally { await f.close(); }
    });
    await test(`${name} restores running direct chat without graph nodes and stops it`, async () => {
      const f = await fixture(desktop);
      try {
        f.state.sessions[1].running = true; await selectSession(f.page, "b");
        await f.page.locator(stop(desktop)).waitFor({ state: "visible" });
        await f.page.locator(stop(desktop)).click();
        if (!desktop) await f.page.locator(".ant-popconfirm-buttons .ant-btn-primary").click();
        await eventually(() => f.state.requests.some((request) => request.path === "/sessions/b/stop"), "Stop request missing");
        f.emit("unit.finished", { direct: true, agentId: "machina" }, "b");
        await f.page.locator(stop(desktop)).waitFor({ state: "detached" });
      } finally { await f.close(); }
    });
    await test(`${name} old message requests cannot replace the selected session`, async () => {
      const f = await fixture(desktop);
      let release!: () => void;
      try {
        f.state.overrides.set("/sessions/b/messages", async (_, fulfill) => { await new Promise<void>((resolve) => { release = resolve; }); await fulfill([message("b", "stale B result")]); });
        const pending = selectSession(f.page, "b");
        await eventually(() => Boolean(release), "Delayed request did not start");
        f.state.messages.a = [message("a", "latest A result")]; await selectSession(f.page, "a"); release(); await pending;
        const current = await webState(f.page); assert.equal(current.sessionId, "a"); assert.equal(current.messages[0].statements[0].text, "latest A result");
      } finally { release?.(); await f.close(); }
    });
    if (!desktop) await test(`${name} stale approval refresh cannot resurrect a resolved request`, async () => {
      const f = await fixture(desktop);
      let release!: () => void;
      try {
        const held = new Promise<void>((resolve) => { release = resolve; });
        let intercepted = false;
        f.state.overrides.set("/approvals", async (_, fulfill) => {
          intercepted = true; f.state.overrides.delete("/approvals");
          await held; await fulfill([{ id: "late", sessionId: "a", agentId: "agent", command: "echo stale" }]);
        });
        const pending = f.page.evaluate(async (url) => { const { useExm } = await import(url) as typeof import("../webui/src/store"); await useExm.getState().refreshPendingApprovals(); }, storeUrl(f.page));
        await eventually(() => intercepted, "Approval request did not start");
        f.emit("approval.required", { approvalId: "late", agentId: "agent", command: "echo stale" }); await f.page.waitForSelector(".approval-card");
        f.state.overrides.delete("/approvals"); f.emit("approval.resolved", { approvalId: "late" });
        await f.page.waitForSelector(".approval-card", { state: "detached" }); release(); await pending;
        assert.equal((await webState(f.page)).approvals.length, 0);
      } finally { release?.(); await f.close(); }
    });
    await test(`${name} restores missed messages and clears resolved approvals`, async () => {
      const f = await fixture(desktop);
      try {
        f.state.approvals = [{ id: "ap", sessionId: "a", agentId: "agent", command: "echo pending" }];
        f.emit("approval.required", { approvalId: "ap", agentId: "agent", command: "echo pending" });
        await f.page.waitForSelector(".approval-card");
        f.state.messages.a = [message("a", "completed while disconnected")]; f.state.approvals = [];
        await f.state.sockets.at(-1)!.close({ code: 1012, reason: "fixture restart" });
        await f.page.waitForFunction(() => document.body.textContent?.includes("completed while disconnected"));
        await f.page.waitForSelector(".approval-card", { state: "detached" });
      } finally { await f.close(); }
    });
    await test(`${name} heartbeat replaces a silent socket`, async () => {
      const f = await fixture(desktop, undefined, true);
      try {
        const before = f.state.sockets.length;
        for (let i = 0; i < 3; i += 1) { const pings = f.state.pings; await f.page.clock.runFor(21_000); await eventually(() => f.state.pings > pings, "Ping not sent"); await new Promise((resolve) => setTimeout(resolve, 50)); }
        assert.equal(f.state.sockets.length, before);
        f.state.pong = false; await f.page.clock.runFor(61_000);
        await eventually(() => f.state.sockets.length > before, "Silent socket not replaced");
      } finally { await f.close(); }
    });
  }
  await test("WebUI file mention is visible above the narrow composer", async () => {
    const f = await fixture(false, { width: 420, height: 820 });
    try {
      f.state.overrides.set("/workbench/tree", (_, fulfill) => fulfill({ root: "/fixture", nodes: [{ path: "src/main.rs", type: "file", name: "main.rs" }] }));
      const input = f.page.locator(".chat-composer textarea"); await input.fill("read @src"); await f.page.locator(".mention-pop-item.on").click();
      assert.equal(await input.inputValue(), "read src/main.rs ");
    } finally { await f.close(); }
  });
  await test("Desktop rejected sends preserve text and image attachments", async () => {
    const f = await fixture(true);
    try {
      f.state.chat = (_, fulfill) => fulfill({ error: "desktop rejection" }, 400);
      await f.page.locator("#input").fill("retry desktop"); await f.page.locator("#file-pick").setInputFiles({ name: "a.png", mimeType: "image/png", buffer: png });
      await f.page.waitForSelector(".attach-thumb"); await f.page.locator("#btn-send").click();
      await eventually(async () => !(await webState(f.page)).running, "Rejected send remained running");
      assert.equal(await f.page.locator("#input").inputValue(), "retry desktop"); assert.equal(await f.page.locator(".attach-thumb").count(), 1);
    } finally { await f.close(); }
  });
  await test("Desktop pending send preserves a newer draft and prevents duplicate requests", async () => {
    const f = await fixture(true); let release!: () => void;
    try {
      f.state.chat = async (_, fulfill) => { await new Promise<void>((resolve) => { release = resolve; }); await fulfill({ accepted: true }, 202); };
      await f.page.locator("#input").fill("first"); await f.page.locator("#btn-send").click();
      await eventually(() => Boolean(release), "Chat request did not start"); await f.page.locator("#input").fill("next draft"); await f.page.locator("#input").press("Enter");
      assert.equal(f.state.requests.filter((request) => request.path.endsWith("/chat")).length, 1); release();
      await eventually(async () => (await webState(f.page)).drafts.a === "next draft", "New draft lost");
      assert.equal(await f.page.locator("#input").inputValue(), "next draft");
    } finally { release?.(); await f.close(); }
  });
  await test("Desktop code navigation retains unsent images and text", async () => {
    const f = await fixture(true);
    try {
      await f.page.locator("#input").fill("unsent task"); await f.page.locator("#file-pick").setInputFiles({ name: "a.png", mimeType: "image/png", buffer: png }); await f.page.waitForSelector(".attach-thumb");
      await f.page.getByRole("button", { name: "查看代码", exact: true }).click(); await f.page.getByRole("button", { name: "查看对话", exact: true }).click();
      assert.equal(await f.page.locator("#input").inputValue(), "unsent task"); assert.equal(await f.page.locator(".attach-thumb").count(), 1);
    } finally { await f.close(); }
  });
  await test("Desktop save conflict retains local edits until an explicit reload", async () => {
    const f = await fixture(true);
    try {
      await f.page.getByRole("button", { name: "查看代码", exact: true }).click(); await f.page.getByText("main.rs", { exact: true }).click();
      await f.page.locator(".cm-content").click(); await f.page.keyboard.press("Control+End"); await f.page.keyboard.type("// local edit");
      f.state.overrides.set("/fs/file", (route, fulfill) => route.request().method() === "PUT" ? fulfill({ error: "文件已被其他操作修改" }, 409) : fulfill({ path: "src/main.rs", content: "// external edit", size: 16, binary: false, truncated: false }));
      await f.page.getByRole("button", { name: "保存文件", exact: true }).click(); await f.page.getByText("文件已被其他操作修改", { exact: false }).waitFor();
      assert.ok((await f.page.locator(".cm-content").innerText()).includes("// local edit"));
      await f.page.getByRole("button", { name: "重新加载文件", exact: true }).click(); await f.page.getByRole("button", { name: "继续编辑", exact: true }).click();
      assert.ok((await f.page.locator(".cm-content").innerText()).includes("// local edit"));
      await f.page.getByRole("button", { name: "重新加载文件", exact: true }).click(); await f.page.getByRole("button", { name: "丢弃修改", exact: true }).click();
      await eventually(async () => (await f.page.locator(".cm-content").innerText()).includes("// external edit"), "External file not loaded");
    } finally { await f.close(); }
  });
  await test("Desktop keeps project conversations and drafts isolated while switching tabs", async () => {
    const f = await fixture(true);
    try {
      const input = f.page.locator("#input");
      await input.fill("draft in project one");
      await f.page.getByRole("button", { name: "项目与连接", exact: true }).click();
      const dialog = f.page.getByRole("dialog", { name: "项目与连接" });
      await dialog.getByRole("textbox", { name: "项目名称" }).fill("Project Two");
      await dialog.getByRole("textbox", { name: "项目目录" }).fill("/fixture/second");
      await dialog.getByRole("button", { name: "打开项目", exact: true }).click();
      await f.page.locator("#input").waitFor({ state: "visible" });
      const second = f.page.locator("#input");
      await eventually(async () => await f.page.locator(".desk-project-tabs").getByRole("button", { name: /Project Two/ }).count() === 1,
        `Second project tab did not appear: ${await f.page.locator("body").innerText()}`);
      await second.fill("draft in project two");
      const secondList = f.state.requests.findLast((request) => request.path === "/sessions" && request.method === "GET" && request.workspace === encodeURIComponent("/fixture/second"));
      assert.ok(secondList, `Project Two did not query its scoped session list: ${JSON.stringify(f.state.requests.filter((request) => request.path === "/sessions" && request.method === "GET"))}`);
      const secondCreate = f.state.requests.find((request) => request.path === "/sessions" && request.method === "POST" && (request.body as { workspace?: string } | null)?.workspace === "/fixture/second");
      assert.ok(secondCreate, "Project Two's new session did not persist its project directory");
      await f.page.locator(".desk-project-tabs").getByRole("button", { name: /project$/i }).click();
      await eventually(async () => (await f.page.locator("#input").inputValue()) === "draft in project one", "Project One draft changed after switching");
      await f.page.locator(".desk-project-tabs").getByRole("button", { name: /Project Two/ }).click();
      await eventually(async () => (await f.page.locator("#input").inputValue()) === "draft in project two", "Project Two draft changed after switching");
    } finally { await f.close(); }
  });
  await test("Desktop preserves unsaved editor changes when switching projects", async () => {
    const f = await fixture(true);
    try {
      await f.page.getByRole("button", { name: "查看代码", exact: true }).click();
      await f.page.getByText("main.rs", { exact: true }).click();
      await f.page.waitForSelector(".cm-content");
      await f.page.locator(".cm-content").click();
      await f.page.keyboard.press("Control+End");
      await f.page.keyboard.type("// unsaved project-one edit");
      await f.page.locator(".desk-editor header").getByText("src/main.rs · 未保存", { exact: true }).waitFor();

      await f.page.getByRole("button", { name: "项目与连接", exact: true }).click();
      const dialog = f.page.getByRole("dialog", { name: "项目与连接" });
      await dialog.getByRole("textbox", { name: "项目名称" }).fill("Project Two");
      await dialog.getByRole("textbox", { name: "项目目录" }).fill("/fixture/second");
      await dialog.getByRole("button", { name: "打开项目", exact: true }).click();

      const projects = f.page.locator(".desk-project-tabs");
      await projects.getByRole("button", { name: "Project Two", exact: true }).waitFor();
      await projects.getByRole("button", { name: "project", exact: true }).click();
      await f.page.locator(".cm-content").getByText("// unsaved project-one edit", { exact: true }).waitFor();
      await f.page.locator(".desk-editor header").getByText("src/main.rs · 未保存", { exact: true }).waitFor();
      assert.equal(f.state.requests.filter((request) => request.path === "/fs/file" && request.method === "PUT").length, 0);
    } finally { await f.close(); }
  });
  await test("Desktop collaboration routes selected work, recovers a rejected send and uses the member approval scope", async () => {
    const f = await fixture(true);
    try {
      const home = "/fixture/project", second = "/fixture/second";
      const updatedAt = "2026-11-19T00:00:00Z";
      const room: CollaborationRoom = { id: "mixed", title: "Release coordination", workspace: home, updatedAt, members: [
        { sessionId: "solo-alpha", target: "single:machina", label: "Fixture Agent", workspace: home },
        { sessionId: "group-beta", target: "default", label: "Default", workspace: second },
      ] };
      let created = false;
      let attempts = 0;
      const snapshot: CollaborationPage = { room, turns: [], activeTurnId: null, approvals: [], nextBefore: null };
      f.state.overrides.set("/health", (_, fulfill) => fulfill({ ok: true, features: ["session-workspace-v1", "collaboration-v1"] }));
      f.state.overrides.set("/collaborations", (route, fulfill) => {
        if (route.request().method() === "POST") {
          created = true;
          return fulfill(room, 201);
        }
        return fulfill(created && route.request().headers()["x-exm-workspace"] === encodeURIComponent(home) ? [room] : []);
      });
      f.state.overrides.set("/collaborations/mixed", (_, fulfill) => fulfill(snapshot));
      f.state.overrides.set("/collaborations/mixed/turns", (route, fulfill) => {
        if (++attempts === 1) return fulfill({ error: "temporary interruption" }, 503);
        const body = route.request().postDataJSON() as CollaborationTurnInput;
        snapshot.activeTurnId = body.requestId;
        snapshot.turns = [{ id: body.requestId, text: body.text, createdAt: updatedAt, deliveries: [
          { ...body.recipients[0], status: "done", messages: [message("solo-alpha", "Alpha result")] },
          { ...body.recipients[1], status: "running" },
        ] }];
        snapshot.approvals = [{ memberSessionId: "group-beta", id: "beta-approval", agentId: "unit", command: "echo review" }];
        f.state.overrides.set(`/collaborations/mixed/turns/${body.requestId}/stop`, (_, stopped) => {
          snapshot.turns[0].deliveries[1].status = "stopped"; snapshot.activeTurnId = null;
          return stopped({ stopped: 1 });
        });
        return fulfill({ id: body.requestId }, 202);
      });
      f.state.overrides.set("/approvals/beta-approval/approve", (_, fulfill) => {
        snapshot.approvals = [];
        return fulfill({ id: "beta-approval", status: "executed" });
      });

      await f.page.getByRole("button", { name: "项目与连接", exact: true }).click();
      const projectDialog = f.page.getByRole("dialog", { name: "项目与连接" });
      await projectDialog.getByRole("textbox", { name: "项目名称" }).fill("Project Two");
      await projectDialog.getByRole("textbox", { name: "项目目录" }).fill(second);
      await projectDialog.getByRole("button", { name: "打开项目", exact: true }).click();
      const projects = f.page.getByRole("navigation", { name: "项目切换" });
      await projects.getByRole("button", { name: "Project Two", exact: true }).waitFor();
      await projects.getByRole("button", { name: "project", exact: true }).click();
      await f.page.getByRole("button", { name: "协作群聊", exact: true }).click();
      await f.page.getByRole("button", { name: "新建协作群聊", exact: true }).click();
      const dialog = f.page.getByRole("dialog", { name: "新建协作群聊" });
      await dialog.getByRole("textbox", { name: "群聊名称" }).fill(room.title);
      await dialog.getByRole("combobox", { name: "成员 1 对象" }).click();
      await f.page.getByText("智能体 · Fixture Agent（machina）", { exact: true }).last().click();
      await dialog.getByRole("combobox", { name: "成员 2 对象" }).click();
      await f.page.getByText("组 · Default（default）", { exact: true }).last().click();
      await dialog.getByRole("button", { name: "创建群聊", exact: true }).click();
      const collab = f.page.getByRole("region", { name: "项目协作群聊" });
      const input = collab.getByRole("textbox", { name: "群聊消息" });
      const send = collab.getByRole("button", { name: "发送给所选成员" });
      await input.fill("Prepare release");
      assert.ok(await send.isDisabled(), "No default broadcast");
      await collab.getByRole("checkbox", { name: /Fixture Agent/ }).check();
      await collab.getByRole("checkbox", { name: /Project Two.*Default/ }).check();
      await collab.getByRole("textbox", { name: /Fixture Agent 的分工/ }).fill("check docs");
      await collab.getByRole("textbox", { name: /Default 的分工/ }).fill("verify tests");
      await send.click();
      await f.page.getByText(/temporary interruption/).waitFor();
      assert.equal(await input.inputValue(), "Prepare release");
      await send.click();
      await collab.getByText("Alpha result", { exact: true }).waitFor();
      const creation = f.state.requests.find((request) => request.path === "/collaborations" && request.method === "POST")!;
      assert.equal(creation.workspace, encodeURIComponent(home));
      assert.deepEqual(creation.body, { title: room.title, members: [{ workspace: home, target: "single:machina" }, { workspace: second, target: "default" }] });
      const sends = f.state.requests.filter((request) => request.path === "/collaborations/mixed/turns");
      assert.equal(sends.length, 2);
      assert.deepEqual(sends[0].body, sends[1].body, "Retry must keep the same request ID and payload");
      assert.deepEqual((sends[0].body as CollaborationTurnInput).recipients, [{ sessionId: "solo-alpha", instruction: "check docs" }, { sessionId: "group-beta", instruction: "verify tests" }]);
      await collab.getByRole("button", { name: "批准一次", exact: true }).click();
      assert.equal(f.state.requests.find((request) => request.path === "/approvals/beta-approval/approve")?.workspace, encodeURIComponent(second));
      await collab.getByRole("button", { name: /停止 Project Two/ }).click();
      await collab.getByText("已停止", { exact: true }).waitFor();
      const cancelled = f.state.requests.find((request) => request.path.startsWith("/collaborations/mixed/turns/") && request.path.endsWith("/stop"));
      assert.deepEqual(cancelled?.body, { sessionId: "group-beta" });
      await collab.getByRole("button", { name: "引用到草稿", exact: true }).click();
      assert.ok((await input.inputValue()).includes("> Alpha result"));
      assert.ok(await send.isDisabled());
      await projects.getByRole("button", { name: "Project Two", exact: true }).click();
      await projects.getByRole("button", { name: "project", exact: true }).click();
      assert.ok((await input.inputValue()).includes("> Alpha result"));
      assert.ok(!f.state.requests.some((request) => request.method === "PUT" && ["/target", "/workspace"].includes(request.path)));
      await screenshot(f.page, "desktop-collaboration");
    } finally { await f.close(); }
  });
  await test("Desktop collaboration leaves ordinary chat usable on an old gateway", async () => {
    const f = await fixture(true);
    try {
      await f.page.getByRole("button", { name: "协作群聊", exact: true }).click();
      await f.page.getByText("该网关不支持协作群聊", { exact: true }).waitFor();
      assert.ok(!f.state.requests.some((request) => request.path.startsWith("/collaborations")));
      await f.page.getByRole("button", { name: "对话", exact: true }).click();
      await f.page.locator("#input").fill("old gateway remains usable");
      assert.ok(await f.page.locator("#btn-send").isEnabled());
    } finally { await f.close(); }
  });
  await test("Desktop cross-session approvals navigate to their source", async () => {
    const f = await fixture(true);
    try {
      f.state.approvals = [{ id: "cross", sessionId: "b", agentId: "agent", command: "echo cross" }];
      f.emit("approval.required", { approvalId: "cross", agentId: "agent", command: "echo cross" }, "b");
      await f.page.locator(".approval-session").click(); await eventually(async () => (await webState(f.page)).sessionId === "b", "Source session not selected");
      await f.page.getByRole("button", { name: "批准", exact: true }).click(); await f.page.waitForSelector(".approval-card", { state: "detached" });
    } finally { await f.close(); }
  });
  for (const viewport of [{ width: 1440, height: 900 }, { width: 420, height: 820 }, { width: 420, height: 480 }, { width: 680, height: 420 }]) {
    await test(`Desktop composer and session navigation fit ${viewport.width}x${viewport.height}`, async () => {
      const f = await fixture(true, viewport);
      try {
        await f.page.locator("#input").fill("compact task");
        const box = await f.page.locator("#btn-send").boundingBox(); assert.ok(box && box.x >= 0 && box.y >= 0 && box.x + box.width <= viewport.width + 1 && box.y + box.height <= viewport.height + 1);
        if (viewport.width >= 960) await f.page.getByRole("button", { name: "隐藏会话", exact: true }).click();
        await f.page.getByRole("button", { name: "显示会话", exact: true }).click();
        await f.page.locator(".desk-session-select").filter({ hasText: "Conversation b" }).click();
        await eventually(async () => (await webState(f.page)).sessionId === "b", "Session navigation did not finish");
        await screenshot(f.page, `desktop-${viewport.width}x${viewport.height}`);
      } finally { await f.close(); }
    });
  }
  for (const desktop of [false, true]) {
    await test(`${desktop ? "Desktop" : "WebUI"} file mention keyboard selection never sends the task`, async () => {
      const f = await fixture(desktop);
      try {
        f.state.overrides.set("/workbench/tree", (_, fulfill) => fulfill({ root: "/fixture", nodes: [{ path: "src/main.rs", type: "file", name: "main.rs" }] }));
        const input = f.page.locator(inputSelector(desktop)); await input.fill("read @src"); await f.page.locator(".mention-pop-item.on").waitFor(); await input.press("Enter");
        assert.equal(await input.inputValue(), "read src/main.rs "); assert.equal(f.state.requests.filter((request) => request.path.endsWith("/chat")).length, 0);
      } finally { await f.close(); }
    });
    await test(`${desktop ? "Desktop" : "WebUI"} streaming follows the bottom and preserves manual reading`, async () => {
      const f = await fixture(desktop);
      try {
        f.emit("orchestrator.token", { delta: "Long output\n\n".repeat(100) });
        const stream = f.page.locator(".chat-scroll");
        await eventually(async () => stream.evaluate((element) => element.scrollHeight - element.scrollTop - element.clientHeight < 100), "Streaming did not follow the bottom");
        await stream.evaluate((element) => { element.scrollTop = 0; element.dispatchEvent(new Event("scroll")); });
        f.emit("orchestrator.token", { delta: "more output\n\n".repeat(40) });
        await eventually(async () => (await webState(f.page)).liveOrch.includes("more output"), "Incremental output missing");
        assert.ok(await stream.evaluate((element) => element.scrollTop < 100));
      } finally { await f.close(); }
    });
    await test(`${desktop ? "Desktop" : "WebUI"} empty file mentions are cached and email addresses do not open the picker`, async () => {
      const f = await fixture(desktop);
      try {
        const input = f.page.locator(inputSelector(desktop)); await input.fill("email@example.com");
        assert.equal(await f.page.locator(".mention-pop").count(), 0);
        await input.fill("read @"); await f.page.locator(".mention-pop").waitFor();
        await eventually(() => f.state.requests.some((request) => request.path === "/workbench/tree"), "File tree request missing");
        await input.fill("read @src"); await input.press("Escape"); await input.fill("read @again");
        assert.equal(f.state.requests.filter((request) => request.path === "/workbench/tree").length, 1);
      } finally { await f.close(); }
    });
  }
  await test("Desktop direct agent tokens remain visible through completion", async () => {
    const f = await fixture(true);
    try {
      f.emit("unit.token", { direct: true, agentId: "machina", delta: "Direct answer" });
      await f.page.getByText("Direct answer", { exact: true }).waitFor(); await f.page.locator("#btn-stop").waitFor();
      f.state.messages.a = [message("a", "Saved direct answer")]; f.emit("unit.finished", { direct: true, agentId: "machina" });
      await f.page.getByText("Saved direct answer", { exact: true }).waitFor(); await f.page.locator("#btn-stop").waitFor({ state: "detached" });
    } finally { await f.close(); }
  });
  await test("Desktop message editing calls the history endpoint and refreshes the selected conversation", async () => {
    const f = await fixture(true);
    try {
      f.state.messages.a = [{ ...message("a", "old request"), role: "user", turn: 1 }];
      await selectSession(f.page, "a");
      f.state.overrides.set("/sessions/a/edit", (route, fulfill) => {
        const { text } = route.request().postDataJSON() as { text: string };
        f.state.messages.a = [{ ...message("a", text), role: "user", turn: 1 }]; return fulfill({ resent: true });
      });
      await f.page.getByRole("button", { name: "编辑并重发", exact: true }).click(); await f.page.getByRole("textbox", { name: "编辑消息", exact: true }).fill("revised request");
      await f.page.locator(".ant-modal-footer .ant-btn-primary").click();
      await f.page.getByText("revised request", { exact: true }).waitFor();
      assert.equal(f.state.requests.filter((request) => request.path === "/sessions/a/edit").length, 1);
    } finally { await f.close(); }
  });
  await test("Desktop settings keep project and instance controls inside the workspace dialog", async () => {
    const f = await fixture(true);
    try {
      await f.page.getByRole("button", { name: "设置", exact: true }).click();
      for (const label of ["桌面", "手机连接", "实例配置"]) {
        await f.page.getByRole("tab", { name: label, exact: true }).click();
        await f.page.getByRole("tabpanel", { name: label, exact: true }).waitFor({ state: "visible", timeout: 30_000 });
        assert.ok(await f.page.locator(".desk-statusbar").count());
      }
      assert.equal(await f.page.getByRole("tab", { name: "模型", exact: true }).count(), 0);
      assert.equal(await f.page.getByRole("tab", { name: "智能体", exact: true }).count(), 0);
      await screenshot(f.page, "desktop-settings");
    } finally { await f.close(); }
  });
  for (const view of ["flows", "tasks"]) {
    await test(`WebUI ${view} ignores stale history and retains messages after refresh failures`, async () => {
      const f = await fixture();
      let release!: () => void;
      try {
        f.state.sessions.push({ ...makeSession("task-a"), title: "job-a" }, { ...makeSession("task-b"), title: "job-b" });
        f.state.messages["task-a"] = [message("task-a", "older task A")]; f.state.messages["task-b"] = [message("task-b", "new task B")];
        if (view === "flows") {
          f.state.overrides.set("/channels/flows", (_, fulfill) => fulfill([
            { id: "flow-a", channelId: "fixture", chatKey: "a", displayName: "flow A", activeSession: "task-a" },
            { id: "flow-b", channelId: "fixture", chatKey: "b", displayName: "flow B", activeSession: "task-b" },
          ]));
        }
        const held = new Promise<void>((resolve) => { release = resolve; });
        let intercepted = false;
        f.state.overrides.set("/sessions/task-a/messages", async (_, fulfill) => { intercepted = true; await held; await fulfill([message("task-a", "stale task A")]); });
        await f.page.evaluate((view) => { location.hash = `#/${view}`; }, view);
        await f.page.locator(".flow-item").filter({ hasText: view === "flows" ? "flow A" : "job-a" }).click();
        await eventually(() => intercepted, "History request did not start");
        await f.page.locator(".flow-item").filter({ hasText: view === "flows" ? "flow B" : "job-b" }).click();
        await f.page.locator(".flows-messages").getByText("new task B").waitFor(); release();
        assert.ok(!(await f.page.locator(".flows-messages").innerText()).includes("stale task A"));
        f.state.overrides.set("/sessions/task-b/messages", (_, fulfill) => fulfill({ error: "offline refresh" }, 503));
        await f.page.evaluate(() => window.dispatchEvent(new CustomEvent("exm:bound-refresh", { detail: "task-b" })));
        await eventually(() => f.state.requests.filter((request) => request.path === "/sessions/task-b/messages").length >= 3, "History refresh did not run");
        assert.ok((await f.page.locator(".flows-messages").innerText()).includes("new task B"));
        assert.equal(await f.page.locator(".flows-thread textarea").count(), 0);
        await screenshot(f.page, `webui-${view}`);
      } finally { release?.(); await f.close(); }
    });
  }
  await test("WebUI nested settings serialize paths as objects", async () => {
    const f = await fixture();
    try {
      await f.page.evaluate(() => { location.hash = "#/settings"; });
      await f.page.locator(".cfg-nav-item").filter({ hasText: "运行" }).first().click();
      const row = f.page.locator('[data-config-field="compaction.triggerRatio"]');
      await row.getByRole("spinbutton").fill("0.72");
      await f.page.locator(".settings-savebar-global .ant-btn-primary").click();
      await eventually(() => f.state.requests.some((request) => request.path === "/config" && request.method === "PUT"), "Settings update missing");
      const update = f.state.requests.findLast((request) => request.path === "/config" && request.method === "PUT")!.body as { compaction: { triggerRatio: number } };
      assert.equal(update.compaction.triggerRatio, 0.72); assert.ok(!Object.hasOwn(update, "compaction.triggerRatio"));
      await screenshot(f.page, "webui-settings");
    } finally { await f.close(); }
  });
  await test("WebUI keeps session management in chat and exposes Agent execution details in context", async () => {
    const f = await fixture();
    try {
      const store = storeUrl(f.page);
      await f.page.evaluate(async (url) => {
        const { useExm } = await import(url) as typeof import("../webui/src/store");
        useExm.setState({
          running: true,
          liveParts: { machina: [{ kind: "text", text: "正在检查工作区文件" }] },
          runToolCalls: [{ callId: "work-call", agentId: "machina", tool: "read", args: { path: "src/main.rs" }, status: "running" }],
          graph: { id: "graph-a", sessionId: "a", status: "running", createdAt: "2026-11-18T00:00:00Z", nodes: [{
            id: "node-a", sessionId: "a", graphId: "graph-a", agentIdentifier: "machina", title: "检查入口代码",
            objective: "确认当前工作区的主要执行路径", acceptance: ["找到入口"], priority: "normal", dependsOn: [], status: "running", retryCount: 0, createdAt: "2026-11-18T00:00:00Z",
          }] },
        });
      }, store);
      const menuItems = await f.page.locator(".nav-menu .ant-menu-item").allInnerTexts();
      assert.ok(!menuItems.some((item) => item.includes("会话管理")), "Session management should not occupy a navigation page");
      await f.page.getByRole("button", { name: "Agent 工作态势", exact: true }).click();
      const workDrawer = f.page.locator(".agent-work-drawer");
      await workDrawer.getByText("检查入口代码", { exact: true }).last().waitFor();
      await workDrawer.getByText("正在检查工作区文件", { exact: true }).waitFor();
      await workDrawer.getByText("src/main.rs", { exact: true }).waitFor();
      assert.equal(await workDrawer.locator(".agent-work-metrics").locator("strong").nth(1).innerText(), "1");
      await f.page.keyboard.press("Escape");

      await f.page.getByRole("button", { name: "管理会话", exact: true }).click();
      const manager = f.page.locator(".session-manager-drawer");
      await manager.getByText("会话管理", { exact: true }).waitFor();
      await manager.locator(".sessions-row").first().waitFor();
      assert.equal(await manager.locator(".sessions-row").count(), 2);
      await manager.locator(".sessions-search input").fill("does-not-match");
      await manager.getByText("没有匹配的会话（标题或最近 200 条消息）", { exact: true }).waitFor();
      await manager.locator(".sessions-search input").clear();
      await manager.locator(".sessions-row").first().getByRole("checkbox").check();
      await manager.getByText("已选 1 项", { exact: true }).waitFor();
      await manager.getByRole("button", { name: "取消选择", exact: true }).click();
      await manager.getByText("归档区", { exact: true }).click();
      await manager.getByText("归档区为空", { exact: true }).waitFor();
      await f.page.keyboard.press("Escape");
      assert.deepEqual(f.errors, []);
    } finally { await f.close(); }
  });
  await test("WebUI session management uses the console theme and straight edges", async () => {
    const f = await fixture(false, { width: 1916, height: 867 });
    try {
      f.state.sessions = f.state.sessions.map((session) => ({ ...session, origin: "console" }));
      f.state.sessions.push({ ...makeSession("style-subtask"), title: "sub:crawler-agent:fixture task", origin: "console" });
      await f.page.getByRole("button", { name: "管理会话", exact: true }).click();
      const drawer = f.page.locator(".session-manager-drawer");
      await drawer.locator(".sessions-row").first().waitFor();
      const subtask = drawer.locator('[data-session-id="style-subtask"]');
      await subtask.getByText("fixture task", { exact: true }).waitFor();
      assert.equal(await subtask.getAttribute("data-raw-title"), "sub:crawler-agent:fixture task");
      assert.equal(await subtask.getAttribute("data-session-kind"), "subtask");
      assert.equal(await subtask.locator(".sessions-title-text").getAttribute("title"), "sub:crawler-agent:fixture task");
      for (const preset of ["minimal", "graphite"] as const) {
        await f.page.evaluate(async ({ url, preset }) => {
          const { useTheme } = await import(url) as typeof import("../webui/src/theme");
          useTheme.getState().setPreset(preset);
        }, { url: `${base}/src/theme.ts`, preset });
        const theme = await drawer.evaluate((element) => {
          const header = element.querySelector<HTMLElement>(".sessions-th")!;
          const table = element.querySelector<HTMLElement>(".sessions-table")!;
          const tag = element.querySelector<HTMLElement>(".sessions-kind-tag")!;
          const probe = document.createElement("span");
          probe.style.backgroundColor = "var(--bg-raise)";
          element.append(probe);
          const expected = getComputedStyle(probe).backgroundColor;
          probe.remove();
          return { background: getComputedStyle(header).backgroundColor, expected,
            tableRadius: getComputedStyle(table).borderRadius, tagRadius: getComputedStyle(tag).borderRadius };
        });
        assert.equal(theme.background, theme.expected, `Session header ignores ${preset} theme: ${JSON.stringify(theme)}`);
        assert.equal(theme.tableRadius, "0px", "Table must follow console straight-edge styling");
        assert.equal(theme.tagRadius, "0px", "Type labels must not use unrelated pill styling");
        await screenshot(f.page, `webui-session-manager-${preset}`);
      }
      await drawer.locator(".sessions-row").first().getByRole("checkbox").check();
      assert.equal(await drawer.locator(".sessions-batchbar").evaluate((element) => getComputedStyle(element).borderRadius), "0px");
      await drawer.getByRole("button", { name: "取消选择", exact: true }).click();
      await drawer.getByRole("combobox", { name: "视图", exact: true }).click();
      await f.page.locator(".ant-select-dropdown:visible").getByText("按来源分组", { exact: true }).click();
      await drawer.locator(".sessions-group").first().waitFor();
      assert.equal(await drawer.locator(".sessions-group").first().evaluate((element) => getComputedStyle(element).borderRadius), "0px");
      await drawer.getByText("归档区", { exact: true }).click();
      await drawer.getByText("归档区为空", { exact: true }).waitFor();
    } finally { await f.close(); }
  });
  for (const viewport of [{ width: 1440, height: 900 }, { width: 420, height: 820 }]) {
    await test(`WebUI session manager preserves raw search and reachable controls ${viewport.width}x${viewport.height}`, async () => {
      const f = await fixture(false, viewport);
      try {
        f.state.sessions = f.state.sessions.map((session) => ({ ...session, origin: "console" }));
        f.state.sessions.push(
          { ...makeSession("child-view"), title: "sub:crawler-agent:Check video evidence·T1", origin: "console" },
          { ...makeSession("channel-view"), title: "channel:fixture:long-external-id", origin: "channel:fixture:long-external-id", originLabel: "Engineering room" },
          { ...makeSession("auto-view"), title: "job-fixture", origin: "console" },
          ...Array.from({ length: 35 }, (_, i) => ({ ...makeSession(`scroll-${i}`), origin: "console" })),
        );
        f.state.overrides.set("/channels", (_, fulfill) => fulfill([{ id: "fixture", type: "qqbot", enabled: true, config: {}, group: "default", capability: "tools", createdAt: "" }]));
        if (viewport.width <= 860) await f.page.locator(".rail-back").click();
        await f.page.getByRole("button", { name: "管理会话", exact: true }).click();
        const drawer = f.page.locator(".session-manager-drawer");
        const kind = drawer.locator(".sessions-kind-filter");
        await drawer.locator('[data-session-id="child-view"] .sessions-title-text').getByText("Check video evidence", { exact: true }).waitFor();
        await drawer.locator('[data-session-id="channel-view"] .sessions-title-text').getByText("Engineering room", { exact: true }).waitFor();
        await kind.getByText("子任务", { exact: true }).click();
        assert.equal(await drawer.locator(".sessions-row").count(), 1);
        assert.equal(await drawer.locator(".sessions-row").getAttribute("data-session-kind"), "subtask");
        await drawer.locator(".sessions-search input").fill("sub:crawler-agent");
        await drawer.locator('.search-result[data-session-id="child-view"]').waitFor();
        assert.equal(await drawer.locator(".search-result").count(), 1);
        assert.equal(await drawer.locator(".search-result").getAttribute("data-raw-title"), "sub:crawler-agent:Check video evidence·T1");
        await drawer.locator(".sessions-search input").clear();
        await kind.getByText("系统任务", { exact: true }).click();
        await drawer.locator('[data-session-id="auto-view"]').waitFor();
        assert.equal(await drawer.locator(".sessions-row").count(), 1);
        await kind.getByText("全部", { exact: true }).click();
        await drawer.locator(".sessions-row").first().getByRole("checkbox").check();
        const before = await drawer.getByTestId("sessions-controls").boundingBox();
        const scroll = await drawer.getByTestId("sessions-list-scroll").evaluate((element) => {
          element.scrollTop = element.scrollHeight;
          return { scrollHeight: element.scrollHeight, clientHeight: element.clientHeight, scrollTop: element.scrollTop };
        });
        assert.ok(scroll.scrollTop > 0 && scroll.scrollHeight > scroll.clientHeight, "Only the session list should scroll");
        const after = await drawer.getByTestId("sessions-controls").boundingBox();
        assert.equal(after?.y, before?.y, "Search and filters must stay in place while browsing sessions");
        const footer = await drawer.getByTestId("sessions-footer").boundingBox();
        assert.ok(footer && footer.y >= 0 && footer.y + footer.height <= viewport.height + 1, "Batch actions must remain inside the viewport");
        assert.ok(await drawer.getByRole("button", { name: "取消选择", exact: true }).isVisible());
        await drawer.getByRole("button", { name: "取消选择", exact: true }).click();
        await drawer.getByRole("combobox", { name: "视图", exact: true }).click();
        await f.page.locator(".ant-select-dropdown:visible").getByText("按来源分组", { exact: true }).click();
        await drawer.locator(".sessions-group").first().waitFor();
        assert.equal(await drawer.locator('[data-session-id="channel-view"]').getAttribute("data-session-kind"), "channel");
        assert.equal(f.state.requests.filter((request) => ["POST", "PUT", "DELETE"].includes(request.method) && !request.path.startsWith("/auth/")).length, 0, "Browsing and filtering must not mutate sessions");
        await screenshot(f.page, `webui-session-controls-${viewport.width}x${viewport.height}`);
      } finally { await f.close(); }
    });
  }
  await test("WebUI session content search debounces and renders matching snippets", async () => {
    const f = await fixture();
    try {
      const targetMessage = message("b", "Needle appears in the matching historical message");
      let searches = 0;
      let releaseSlow: (() => void) | undefined;
      const slowResponse = new Promise<void>((resolve) => { releaseSlow = resolve; });
      f.state.overrides.set("/sessions/search", async (route, fulfill) => {
        const query = new URL(route.request().url()).searchParams.get("q") ?? "";
        searches += 1;
        if (query === "slow") await slowResponse;
        return fulfill({ query, hits: query === "needle" ? [makeSearchHit("b", "Conversation b", targetMessage.id, targetMessage.statements[0].text)] : [] });
      });
      await f.page.getByRole("button", { name: "管理会话", exact: true }).click();
      const manager = f.page.locator(".session-manager-drawer");
      const input = manager.locator(".sessions-search input");
      await input.fill("slow");
      await eventually(() => f.state.requests.some((request) => request.path === "/sessions/search" && request.query.q === "slow"), "Slow search request should be sent after debounce");
      assert.equal(await manager.locator(".sessions-list-scroll").getAttribute("aria-busy"), "true");
      await manager.locator(".sessions-loading").waitFor();
      releaseSlow?.();
      await manager.getByText("没有匹配的会话（标题或最近 200 条消息）", { exact: true }).waitFor();
      searches = 0;
      for (const partial of ["n", "ne", "nee", "need", "needle"]) await input.fill(partial);
      const result = manager.locator('.search-result[data-session-id="b"]');
      await result.waitFor();
      await result.locator(".search-result-snippet mark").getByText("Needle", { exact: true }).waitFor();
      await eventually(async () => (await result.locator(".search-result-meta").innerText()).includes("1 条消息命中"), "Result should display the matching-message count");
      await f.page.waitForTimeout(350);
      assert.equal(searches, 1, "Rapid typing must produce one debounced request with the final query");
      assert.equal(f.state.requests.find((request) => request.path === "/sessions/search" && request.query.q === "needle")?.query.q, "needle");
      await input.fill("absent");
      await manager.getByText("没有匹配的会话（标题或最近 200 条消息）", { exact: true }).waitFor();
      await input.clear();
      await eventually(async () => (await manager.locator(".sessions-row").count()) === 2, "Clearing search should restore the normal session list");
    } finally { await f.close(); }
  });
  await test("WebUI session content search falls back locally on errors and keeps archived filtering local", async () => {
    const f = await fixture();
    try {
      f.state.sessions.push({ ...makeSession("c"), title: "Offline fallback phrase" });
      f.state.archivedSessions = [{ ...makeSession("archived-needle"), title: "Archived needle session", archivedAt: "2026-11-18T00:00:00Z" }];
      f.state.overrides.set("/sessions/search", (route, fulfill) => {
        const query = new URL(route.request().url()).searchParams.get("q") ?? "";
        return query === "offline" ? fulfill({ error: "fixture offline" }, 503) : fulfill({ query, hits: [] });
      });
      await f.page.getByRole("button", { name: "管理会话", exact: true }).click();
      const reopened = f.page.locator(".session-manager-drawer");
      const offlineInput = reopened.locator(".sessions-search input");
      await offlineInput.fill("offline");
      await reopened.getByRole("status").getByText("内容检索不可用，已回退为标题/预览过滤", { exact: true }).waitFor();
      await reopened.locator('[data-session-id="c"].sessions-row').waitFor();
      await offlineInput.clear();
      await eventually(async () => (await reopened.locator(".sessions-row").count()) === 3, "Clearing search should restore the full active session list");
      const beforeArchiveSearch = f.state.requests.filter((request) => request.path === "/sessions/search").length;
      await reopened.getByText("归档区", { exact: true }).click();
      await reopened.locator(".sessions-search input").fill("archived needle");
      await reopened.locator('[data-session-id="archived-needle"].sessions-row').waitFor();
      assert.equal(f.state.requests.filter((request) => request.path === "/sessions/search").length, beforeArchiveSearch, "Archived view must retain local filtering without content-search requests");
    } finally { await f.close(); }
  });
  await test("WebUI session search results remain usable without horizontal overflow at 390x844", async () => {
    const f = await fixture(false, { width: 390, height: 844 });
    try {
      if (390 <= 860) await f.page.locator(".rail-back").click();
      const snippet = `narrow ${"long matching context ".repeat(18)}`;
      f.state.overrides.set("/sessions/search", (route, fulfill) => {
        const query = new URL(route.request().url()).searchParams.get("q") ?? "";
        const hit = makeSearchHit("a", "Narrow result", "narrow-message", snippet);
        hit.titleMatch = true;
        return fulfill({ query, hits: [hit] });
      });
      await f.page.getByRole("button", { name: "管理会话", exact: true }).click();
      const drawer = f.page.locator(".session-manager-drawer");
      const input = drawer.locator(".sessions-search input");
      await input.fill("narrow");
      const result = drawer.locator('.search-result[data-session-id="a"]');
      await result.waitFor();
      assert.equal(await drawer.locator(".search-results").evaluate((element) => getComputedStyle(element).flexDirection), "column");
      const geometry = await f.page.evaluate(() => ({ width: innerWidth, documentWidth: document.documentElement.scrollWidth }));
      assert.ok(geometry.documentWidth <= geometry.width, `Session search must not cause horizontal overflow: ${JSON.stringify(geometry)}`);
      const resultBox = await result.boundingBox();
      assert.ok(resultBox && resultBox.x >= 0 && resultBox.x + resultBox.width <= 390, `Search result must fit narrow viewport: ${JSON.stringify(resultBox)}`);
      assert.ok(await result.locator(".search-result-snippet mark").count() > 0);
      await screenshot(f.page, "webui-session-search-390x844");
      await input.clear();
      await drawer.getByRole("combobox", { name: "排序", exact: true }).click();
      await f.page.locator(".ant-select-dropdown:visible").getByText("创建时间", { exact: true }).click();
      assert.ok(await drawer.getByRole("combobox", { name: "排序", exact: true }).isVisible(), "Sort control should remain reachable on narrow layouts");
    } finally { await f.close(); }
  });
  await test("WebUI command palette supports shortcut, empty state, result cap, and keyboard selection", async () => {
    const f = await fixture();
    try {
      const paletteMessages = Array.from({ length: 14 }, (_, index) => message(`palette-${index}`, `needle body ${index}`));
      paletteMessages[1].id = "msg-needle-1";
      f.state.messages["palette-1"] = [paletteMessages[1]];
      f.state.overrides.set("/sessions/search", (route, fulfill) => {
        const url = new URL(route.request().url());
        const query = url.searchParams.get("q") ?? "";
        const hits = query === "needle"
          ? Array.from({ length: 14 }, (_, index) => makeSearchHit(`palette-${index}`, `Palette session ${index}`, `msg-needle-${index}`, `needle body ${index}`))
          : [];
        return fulfill({ query, hits: hits.slice(0, Number(url.searchParams.get("limit") ?? 20)) });
      });

      await f.page.keyboard.press("Control+k");
      const dialog = f.page.locator(".cmd-palette");
      await dialog.waitFor();
      const roleDialog = f.page.getByRole("dialog");
      await roleDialog.waitFor();
      assert.equal(await roleDialog.getAttribute("aria-modal"), "true");
      const input = dialog.getByRole("textbox", { name: "会话速查", exact: true });
      await eventually(() => input.evaluate((element) => document.activeElement === element), "Command palette input did not receive focus");
      assert.equal(await dialog.locator(".cmd-status").innerText(), "↑↓ 选择 · Enter 打开 · Esc 关闭 · / 唤起");
      await screenshot(f.page, "webui-command-palette-1440x900");
      await f.page.setViewportSize({ width: 390, height: 844 });
      const paletteBox = await dialog.boundingBox();
      assert.ok(paletteBox && paletteBox.x >= 0 && paletteBox.x + paletteBox.width <= 390, `Palette should fit narrow viewport: ${JSON.stringify(paletteBox)}`);
      await screenshot(f.page, "webui-command-palette-390x844");
      await input.fill("none");
      await dialog.getByText("无匹配会话", { exact: true }).waitFor();
      await f.page.keyboard.press("Escape");
      await dialog.waitFor({ state: "hidden" });
      assert.equal(await f.page.locator(".app-shell").getAttribute("data-view"), "chat", "Esc should close without navigating");

      await f.page.keyboard.press("Control+k");
      const reopened = f.page.locator(".cmd-palette");
      const search = reopened.getByRole("textbox", { name: "会话速查", exact: true });
      await search.fill("needle");
      await reopened.locator(".cmd-item").first().waitFor();
      assert.equal(await reopened.locator(".cmd-item").count(), 12, "Palette must cap visible results at twelve");
      const paletteSearch = f.state.requests.findLast((request) => request.path === "/sessions/search");
      assert.deepEqual(paletteSearch?.query, { q: "needle", limit: "12" }, "Palette should request the configured query and twelve-result cap");
      await search.press("ArrowDown");
      assert.ok(await reopened.locator(".cmd-item").nth(1).evaluate((element) => element.classList.contains("on")), "Down arrow should select the next hit");
      await search.press("ArrowUp");
      assert.ok(await reopened.locator(".cmd-item").first().evaluate((element) => element.classList.contains("on")), "Up arrow should move selection to the previous hit");
      await search.press("Escape");
      await reopened.waitFor({ state: "hidden" });
      assert.equal(await f.page.locator(".app-shell").getAttribute("data-view"), "chat", "Closing the palette should not navigate");
      assert.ok(paletteSearch, "Palette search endpoint should be called");
    } finally { await f.close(); }
  });
  await test("WebUI session history menu shows ordered snapshots and performs undo and fork", async () => {
    const f = await fixture();
    try {
      f.state.overrides.set("/sessions/a/snapshots", (_, fulfill) => fulfill([
        makeSnapshot(2, { checkpointIds: ["file-a"] }),
        makeSnapshot(1, { status: "failed" }),
        makeSnapshot(3, { checkpointIds: ["file-a", "file-b"] }),
      ]));
      f.state.overrides.set("/sessions/a/undo", (route, fulfill) => {
        const body = route.request().postDataJSON() as { turn: number; restoreFiles: boolean };
        assert.deepEqual(body, { turn: 2, restoreFiles: true });
        return fulfill({ uptoTurn: 2, archived: 4, restored: ["src/main.rs"] });
      });
      f.state.overrides.set("/sessions/a/fork", (route, fulfill) => {
        assert.deepEqual(route.request().postDataJSON(), { turn: 1 });
        return fulfill({ id: "forked-a", title: "Conversation a · 分支·至第 1 轮" });
      });
      await f.page.getByRole("button", { name: "管理会话", exact: true }).click();
      const manager = f.page.locator(".session-manager-drawer");
      const row = manager.locator('[data-session-id="a"].sessions-row');
      await row.locator(".sessions-td-ops .ant-btn").nth(1).click();
      await f.page.getByText("历史检查点", { exact: true }).last().click();
      const modal = f.page.locator(".history-modal");
      await modal.waitFor();
      const items = modal.locator(".history-item");
      await items.first().waitFor();
      assert.deepEqual(await items.locator(".history-item-head b").allInnerTexts(), ["第 3 轮", "第 2 轮", "第 1 轮"]);
      await items.nth(0).getByText("2 个文件检查点", { exact: true }).waitFor();
      await items.nth(0).getByText("User input for turn 3", { exact: true }).waitFor();
      await items.nth(1).getByText("4 条消息", { exact: true }).waitFor();
      await items.nth(2).getByText("失败", { exact: true }).waitFor();

      await items.nth(1).getByRole("button", { name: "回到此轮", exact: true }).click();
      await f.page.locator(".ant-popconfirm-buttons .ant-btn-primary").last().click();
      await f.page.getByText("已回到第 2 轮（归档 4 条消息）", { exact: true }).waitFor();
      assert.deepEqual(f.state.requests.findLast((request) => request.path === "/sessions/a/undo")?.body, { turn: 2, restoreFiles: true });

      await items.nth(2).getByRole("button", { name: "从此轮分叉", exact: true }).click();
      await f.page.waitForFunction(async (url) => {
        const { useExm } = await import(url) as typeof import("../webui/src/store");
        return useExm.getState().sessionId === "forked-a";
      }, storeUrl(f.page));
      assert.deepEqual(f.state.requests.findLast((request) => request.path === "/sessions/a/fork")?.body, { turn: 1 });
      await f.page.getByText("已创建分支会话", { exact: true }).waitFor();
    } finally { await f.close(); }
  });
  await test("WebUI session history distinguishes empty, loading, and failed snapshot states", async () => {
    const f = await fixture();
    try {
      f.state.sessions.push(makeSession("history-failed"));
      f.state.overrides.set("/sessions/b/snapshots", (_, fulfill) => fulfill([]));
      let loadingStarted = false;
      let releaseFailure: (() => void) | undefined;
      const blocked = new Promise<void>((resolve) => { releaseFailure = resolve; });
      f.state.overrides.set("/sessions/history-failed/snapshots", async (_, fulfill) => {
        loadingStarted = true;
        await blocked;
        await fulfill({ error: "fixture unavailable" }, 503);
      });
      await f.page.getByRole("button", { name: "管理会话", exact: true }).click();
      const manager = f.page.locator(".session-manager-drawer");
      const openHistory = async (id: string) => {
        const row = manager.locator(`[data-session-id="${id}"].sessions-row`);
        await row.locator(".sessions-td-ops .ant-btn").nth(1).click();
        await f.page.getByText("历史检查点", { exact: true }).last().click();
        return f.page.locator(".history-modal");
      };
      let modal = await openHistory("b");
      await modal.getByText("该会话暂无轮次快照（快照自该功能上线后记录）", { exact: true }).waitFor();
      await modal.locator(".ant-modal-close").click();
      modal = await openHistory("history-failed");
      await eventually(() => loadingStarted, "Snapshot request should enter the loading state");
      await modal.locator(".history-status .ant-spin-spinning").waitFor();
      releaseFailure?.();
      await modal.getByText(/加载快照失败：/, { exact: false }).waitFor();
    } finally { await f.close(); }
  });
  await test("WebUI session history reports rejected undo and fork actions without closing", async () => {
    const f = await fixture();
    try {
      f.state.overrides.set("/sessions/a/snapshots", (_, fulfill) => fulfill([makeSnapshot(2)]));
      f.state.overrides.set("/sessions/a/undo", (_, fulfill) => fulfill({ error: "会话正在处理中" }, 409));
      f.state.overrides.set("/sessions/a/fork", (_, fulfill) => fulfill({ error: "轮次不存在" }, 400));
      await f.page.getByRole("button", { name: "管理会话", exact: true }).click();
      const manager = f.page.locator(".session-manager-drawer");
      const row = manager.locator('[data-session-id="a"].sessions-row');
      await row.locator(".sessions-td-ops .ant-btn").nth(1).click();
      await f.page.getByText("历史检查点", { exact: true }).last().click();
      const modal = f.page.locator(".history-modal");
      const item = modal.locator(".history-item");
      await item.waitFor();
      await item.getByRole("button", { name: "回到此轮", exact: true }).click();
      await f.page.locator(".ant-popconfirm-buttons .ant-btn-primary").last().click();
      await f.page.getByText("会话正在处理中", { exact: true }).waitFor();
      assert.ok(await modal.isVisible(), "Rejected undo should leave the history modal open");
      assert.deepEqual((await webState(f.page)).messages, [], "Rejected undo must not mutate local messages");

      await item.getByRole("button", { name: "从此轮分叉", exact: true }).click();
      await f.page.locator(".ant-message-notice-error").waitFor();
      assert.ok(await modal.isVisible(), "Rejected fork should leave the history modal open");
      assert.equal(f.state.requests.filter((request) => ["/sessions/a/undo", "/sessions/a/fork"].includes(request.path)).length, 2);
    } finally { await f.close(); }
  });
  await test("WebUI session history modal fits a 390x844 viewport and keeps its snapshot list scrollable", async () => {
    const f = await fixture(false, { width: 390, height: 844 });
    try {
      if (390 <= 860) await f.page.locator(".rail-back").click();
      f.state.overrides.set("/sessions/a/snapshots", (_, fulfill) => fulfill(Array.from({ length: 12 }, (_, index) => makeSnapshot(index + 1, {
        preview: `Narrow viewport checkpoint ${index + 1}: ${"long user input ".repeat(8)}`,
        checkpointIds: ["src/main.rs", "src/lib.rs"],
      }))));
      await f.page.getByRole("button", { name: "管理会话", exact: true }).click();
      const manager = f.page.locator(".session-manager-drawer");
      const row = manager.locator('[data-session-id="a"].sessions-row');
      await row.locator(".sessions-td-ops .ant-btn").nth(1).click();
      await f.page.getByText("历史检查点", { exact: true }).last().click();
      const modal = f.page.locator(".history-modal");
      await modal.locator(".history-item").last().waitFor();
      const bounds = await modal.boundingBox();
      assert.ok(bounds && bounds.x >= 0 && bounds.x + bounds.width <= 390, `History modal should fit the narrow viewport: ${JSON.stringify(bounds)}`);
      const list = modal.locator(".history-list");
      const scroll = await list.evaluate((element) => ({ clientHeight: element.clientHeight, scrollHeight: element.scrollHeight }));
      assert.ok(scroll.scrollHeight > scroll.clientHeight, `Long checkpoint lists should scroll inside the modal: ${JSON.stringify(scroll)}`);
      await screenshot(f.page, "webui-session-history-390x844");
    } finally { await f.close(); }
  });
  await test("WebUI flows auto-select their session and render scoped live run activity without duplicate final output", async () => {
    const f = await fixture();
    try {
      const flowA = makeSession("flow-a");
      const flowB = makeSession("flow-b");
      f.state.sessions.push(flowA, flowB);
      f.state.messages[flowA.id] = [];
      f.state.messages[flowB.id] = [];
      f.state.overrides.set("/channels/flows", (_, fulfill) => fulfill([
        { id: "flow-b", channelId: "fixture", chatKey: "b", displayName: "Flow B", activeSession: flowB.id },
        { id: "flow-a", channelId: "fixture", chatKey: "a", displayName: "Flow A", activeSession: flowA.id },
      ]));
      const terminal = {
        taskId: "bg-flow", command: "cargo test", status: "running" as const, startedAtMs: 1,
        outputPreview: "", outputBytes: 0, outputTruncated: false,
      };
      const activityEvents = [
        { type: "run.progress", sessionId: flowB.id, payload: { stage: "preparing", message: "Hidden long-task preamble", announce: false } },
        { type: "orchestrator.thinking", sessionId: flowB.id, payload: { delta: "Model-provided thought" } },
        { type: "orchestrator.token", sessionId: flowB.id, payload: { delta: "Snapshot body" } },
        { type: "tool.call", sessionId: flowB.id, payload: { callId: "flow-tool", agentId: "machina", tool: "terminal", args: { command: "echo live-tool" } } },
        { type: "spawn.started", sessionId: flowB.id, payload: { spawnId: "flow-spawn", taskPreview: "Background child task", background: true } },
        { type: "unit.thinking", sessionId: flowB.id, payload: { agentId: "child-agent", delta: "Finished child should disappear" } },
        { type: "unit.finished", sessionId: flowB.id, payload: { agentId: "child-agent", direct: false } },
        { type: "terminal.started", sessionId: flowB.id, payload: terminal },
      ];
      f.state.overrides.set("/sessions/flow-b", (_, fulfill) => fulfill({ ...flowB, running: true, activity: { events: activityEvents }, terminalJobs: [terminal] }));
      await f.page.evaluate(() => { location.hash = "#/flows"; });
      await f.page.waitForFunction(async (url) => {
        const { useExm } = await import(url) as typeof import("../webui/src/store");
        return useExm.getState().sessionId === "flow-b";
      }, storeUrl(f.page));
      await eventually(() => new URL(f.state.sockets.at(-1)!.url()).searchParams.get("sessionId") === flowB.id, "Initial, unclicked flow must own the shared websocket");
      assert.equal(new URL(f.state.sockets.at(-1)!.url()).searchParams.get("workspace"), flowB.workspace, "Flow websocket must use the selected session's persisted workspace");
      await f.page.locator(".flows-messages .run-progress-message").getByText("执行中", { exact: true }).waitFor();
      assert.ok(!(await f.page.locator(".flows-messages").innerText()).includes("Hidden long-task preamble"), "Preparing text with announce=false must stay hidden");
      assert.ok(!(await f.page.locator(".flows-messages").innerText()).includes("Finished child should disappear"), "Replayed unit.finished must remove the child's live card");
      const replayed = await f.page.evaluate(async (url) => {
        const { useExm } = await import(url) as typeof import("../webui/src/store");
        const state = useExm.getState();
        return { liveThinking: state.liveThinking, liveOrch: state.liveOrch, running: state.running };
      }, storeUrl(f.page));
      assert.equal(replayed.liveThinking, "Model-provided thought");
      assert.equal(replayed.liveOrch, "Snapshot body");
      assert.equal(replayed.running, true);
      await f.page.locator(".flows-messages .live-card .usage-bar-head").filter({ hasText: "思考" }).click();
      await f.page.locator(".flows-messages").getByText("Model-provided thought", { exact: true }).waitFor();
      await f.page.locator(".terminal-job-command").getByText("cargo test", { exact: true }).waitFor();
      await f.page.locator(".spawn-preview").getByText("Background child task", { exact: true }).waitFor();
      await f.page.locator(".flows-messages .live-card .usage-bar-head").filter({ hasText: "工具" }).click();
      await f.page.locator(".tool-title").getByText("echo live-tool", { exact: true }).waitFor();
      f.emit("run.progress", { stage: "preparing", message: "Visible long-task preamble", announce: true }, flowB.id);
      await f.page.locator(".run-progress-message").getByText("Visible long-task preamble", { exact: true }).waitFor();
      f.emit("orchestrator.thinking", { delta: " · streamed thinking" }, flowB.id);
      await f.page.locator(".flows-messages .usage-think").getByText("Model-provided thought · streamed thinking", { exact: true }).waitFor();
      f.emit("tool.result", { callId: "flow-tool", ok: true, durationMs: 12, summary: "Live tool result summary" }, flowB.id);
      await f.page.locator(".tool-card.tool-ok").click();
      await f.page.locator(".tool-summary").getByText("Live tool result summary", { exact: true }).waitFor();
      f.emit("terminal.updated", { ...terminal, outputPreview: "Live terminal output", outputBytes: 20 }, flowB.id);
      await f.page.locator(".terminal-job-head").click();
      await f.page.locator(".terminal-job-detail").getByText("Live terminal output", { exact: true }).waitFor();
      f.emit("terminal.finished", { ...terminal, status: "exited", outputPreview: "Terminal complete", outputBytes: 17, exitCode: 0, finishedAtMs: 3 }, flowB.id);
      await f.page.locator(".terminal-job-exited .terminal-job-detail").getByText("Terminal complete", { exact: true }).waitFor();
      f.emit("spawn.finished", { spawnId: "flow-spawn", status: "done", background: true, summaryPreview: "Live child result" }, flowB.id);
      await f.page.locator(".spawn-card.spawn-done .spawn-card-head").click();
      await f.page.locator(".spawn-result").getByText("Live child result", { exact: true }).waitFor();
      assert.equal((await webState(f.page)).running, true, "Thinking/tools/terminal/spawn activity must render before run.finished");
      await screenshot(f.page, "webui-flow-live-activity");

      f.emit("orchestrator.token", { delta: "Other-flow-only body" }, flowA.id);
      await f.page.waitForTimeout(60);
      assert.ok(!(await f.page.locator(".flows-messages").innerText()).includes("Other-flow-only body"), "Events from a different flow must not leak into this view");
      await f.page.locator(".flow-item").filter({ hasText: "Flow A" }).click();
      await f.page.waitForFunction(async (url) => {
        const { useExm } = await import(url) as typeof import("../webui/src/store");
        return useExm.getState().sessionId === "flow-a";
      }, storeUrl(f.page));
      assert.ok(!(await f.page.locator(".flows-messages").innerText()).includes("Snapshot body"), "Switching flows must clear the previous live body");
      assert.equal(await f.page.locator(".terminal-job").count(), 0, "Terminal jobs must remain scoped to their owning session");

      await f.page.locator(".flow-item").filter({ hasText: "Flow B" }).click();
      await f.page.waitForFunction(async (url) => {
        const { useExm } = await import(url) as typeof import("../webui/src/store");
        return useExm.getState().sessionId === "flow-b";
      }, storeUrl(f.page));
      f.emit("orchestrator.token", { delta: "Live body before finish" }, flowB.id);
      await f.page.locator(".flows-messages").getByText("Snapshot bodyLive body before finish", { exact: true }).waitFor();
      f.state.messages[flowB.id] = [message(flowB.id, "Final flow response")];
      f.emit("run.finished", { statements: [{ tag: "报告", text: "Final flow response" }] }, flowB.id);
      await f.page.locator(".flows-messages").getByText("Final flow response", { exact: true }).waitFor();
      await eventually(async () => (await f.page.locator(".flows-messages").getByText("Final flow response", { exact: true }).count()) === 1, "Final history should replace rather than duplicate live output");
      assert.ok(!(await f.page.locator(".flows-messages").innerText()).includes("Live body before finish"));
    } finally { await f.close(); }
  });
  await test("WebUI flow reconnect replays the current activity snapshot and terminal status", async () => {
    const f = await fixture();
    try {
      const session = makeSession("flow-reconnect");
      f.state.sessions.push(session);
      f.state.overrides.set("/channels/flows", (_, fulfill) => fulfill([
        { id: "reconnect-flow", channelId: "fixture", chatKey: "reconnect", displayName: "Reconnect flow", activeSession: session.id },
      ]));
      const job = {
        taskId: "reconnect-job", command: "npm test", status: "running" as const, startedAtMs: 2,
        outputPreview: "", outputBytes: 0, outputTruncated: false,
      };
      let snapshot: { type: string; sessionId: string; payload: Record<string, unknown> }[] = [{ type: "unit.thinking", sessionId: session.id, payload: { agentId: "child", delta: "Child pre-finish state" } },
        { type: "unit.finished", sessionId: session.id, payload: { agentId: "child", direct: false } }];
      f.state.overrides.set("/sessions/flow-reconnect", (_, fulfill) => fulfill({ ...session, running: true, activity: { events: snapshot }, terminalJobs: [job] }));
      await f.page.evaluate(() => { location.hash = "#/flows"; });
      await f.page.waitForFunction(async (url) => {
        const { useExm } = await import(url) as typeof import("../webui/src/store");
        return useExm.getState().sessionId === "flow-reconnect";
      }, storeUrl(f.page));
      await f.page.locator(".terminal-job-command").getByText("npm test", { exact: true }).waitFor();
      assert.ok(!(await f.page.locator(".flows-messages").innerText()).includes("Child pre-finish state"), "Completed child output must not survive snapshot replay");

      const previousSockets = f.state.sockets.length;
      snapshot = [
        { type: "run.progress", sessionId: session.id, payload: { stage: "planning", message: "internal planning details" } },
        { type: "orchestrator.token", sessionId: session.id, payload: { delta: "Recovered after reconnect" } },
      ];
      await f.state.sockets.at(-1)!.close({ code: 1012, reason: "fixture reconnect" });
      await eventually(() => f.state.sockets.length > previousSockets, "The flow websocket should reconnect");
      await f.page.locator(".flows-messages").getByText("Recovered after reconnect", { exact: true }).waitFor();
      await f.page.locator(".run-progress-message").getByText("正在规划", { exact: true }).waitFor();
      const bodyCount = await f.page.locator(".flows-messages").getByText("Recovered after reconnect", { exact: true }).count();
      assert.equal(bodyCount, 1, "Activity snapshot deltas must not duplicate after reconnect replay");
      const current = await f.page.evaluate(async (url) => {
        const { useExm } = await import(url) as typeof import("../webui/src/store");
        const state = useExm.getState();
        return { running: state.running, liveOrch: state.liveOrch, runProgress: state.runProgress, terminalJobs: state.terminalJobs };
      }, storeUrl(f.page));
      assert.equal(current.running, true);
      assert.equal(current.liveOrch, "Recovered after reconnect");
      assert.equal(current.runProgress?.stage, "planning");
      assert.equal(current.terminalJobs[0]?.taskId, "reconnect-job");
    } finally { await f.close(); }
  });
  for (const desktop of [false, true]) {
    await test(`${desktop ? "Desktop" : "WebUI"} shared run activity separates public progress from thinking and scopes terminal jobs`, async () => {
      const f = await fixture(desktop);
      try {
        const currentSession = f.state.sessions[0];
        currentSession.running = true;
        currentSession.activity = { events: [
          { type: "run.progress", sessionId: "a", payload: { stage: "preparing", message: "Do not announce this short-chat preamble", announce: false } },
          { type: "orchestrator.thinking", sessionId: "a", payload: { delta: "Original model-only thinking" } },
          { type: "orchestrator.token", sessionId: "a", payload: { delta: "Active answer body" } },
          { type: "unit.token", sessionId: "a", payload: { agentId: "finished-child", delta: "Finished child should not stay live" } },
          { type: "unit.finished", sessionId: "a", payload: { agentId: "finished-child", direct: false } },
          { type: "dispatch.sent", sessionId: "a", payload: { agentIdentifier: "finished-child", nodeId: "child-node", childSessionId: "child-session-not-listed" } },
        ] };
        currentSession.terminalJobs = [{ taskId: "shared-bg", command: "Shared background command", status: "running", startedAtMs: 10, outputPreview: "Recovered terminal output", outputBytes: 25, outputTruncated: false }];
        const sessionListsBefore = f.state.requests.filter((request) => request.path === "/sessions" && request.method === "GET").length;
        await selectSession(f.page, "a");
        await f.page.locator(".run-progress-message").getByText("执行中", { exact: true }).waitFor();
        assert.ok(!(await f.page.locator("body").innerText()).includes("Do not announce this short-chat preamble"), "Non-announced preparing message must not become a public preamble");
        assert.ok(!(await f.page.locator("body").innerText()).includes("Finished child should not stay live"), "unit.finished replay must clear a child's stream");
        assert.equal(f.state.requests.filter((request) => request.path === "/sessions" && request.method === "GET").length, sessionListsBefore, "Snapshot replay must not fetch child sessions");
        const think = f.page.locator(desktop ? ".desk-thinking summary" : ".usage-bar-head").filter({ hasText: "思考" });
        await think.click();
        await f.page.getByText("Original model-only thinking", { exact: true }).waitFor();
        f.emit("run.progress", { stage: "preparing", message: "Announced public task preamble", announce: true });
        await f.page.locator(".run-progress-message").getByText("Announced public task preamble", { exact: true }).waitFor();
        f.emit("run.progress", { stage: "planning", message: "Private planning message" });
        await f.page.locator(".run-progress-message").getByText("正在规划", { exact: true }).waitFor();
        assert.ok(!(await f.page.locator("body").innerText()).includes("Private planning message"), "Planning status should remain generic rather than masquerade as an announced preamble");
        f.emit("run.progress", { stage: "working", message: "Tools: read, terminal" });
        await f.page.locator(".run-progress-message").getByText("Tools: read, terminal", { exact: true }).waitFor();
        const terminal = f.page.locator(".terminal-job");
        await terminal.locator(".terminal-job-head").click();
        await terminal.getByText("Recovered terminal output", { exact: true }).waitFor();
        f.emit("terminal.finished", { ...currentSession.terminalJobs[0], status: "exited", exitCode: 0, outputPreview: "Shared terminal complete", finishedAtMs: 20 });
        await f.page.locator(".terminal-job-exited").getByText("Shared terminal complete", { exact: true }).waitFor();
        await screenshot(f.page, `${desktop ? "desktop" : "webui"}-shared-run-activity`);
        await selectSession(f.page, "b");
        assert.equal(await f.page.locator(".terminal-job").count(), 0, "Background jobs must not leak to another session");
        assert.equal(await f.page.locator(".run-progress-message").count(), 0, "Progress must not leak to another session");
      } finally { await f.close(); }
    });
  }
  await test("WebUI late reconnect activity snapshots cannot resurrect a completed run", async () => {
    const f = await fixture();
    let release: (() => void) | undefined;
    try {
      const staleSession: Session = {
        ...makeSession("a"), running: true,
        activity: { events: [{ type: "orchestrator.token", sessionId: "a", payload: { delta: "Stale reconnect body" } }] },
        terminalJobs: [{ taskId: "stale-job", command: "Stale terminal command", status: "running", startedAtMs: 1, outputPreview: "", outputBytes: 0, outputTruncated: false }],
      };
      let intercepted = false;
      const held = new Promise<void>((resolve) => { release = resolve; });
      f.state.overrides.set("/sessions/a", async (_, fulfill) => { intercepted = true; await held; await fulfill(staleSession); });
      await f.state.sockets.at(-1)!.close({ code: 1012, reason: "late snapshot fixture" });
      await eventually(() => intercepted, "Reconnect should request a session-detail snapshot");
      f.state.messages.a = [message("a", "Completed before snapshot returned")];
      f.emit("run.finished", { statements: [{ tag: "报告", text: "Completed before snapshot returned" }] });
      await f.page.locator(".chat-scroll").getByText("Completed before snapshot returned", { exact: true }).waitFor();
      release?.();
      await f.page.waitForTimeout(100);
      const state = await f.page.evaluate(async (url) => {
        const { useExm } = await import(url) as typeof import("../webui/src/store");
        const current = useExm.getState();
        return { running: current.running, liveOrch: current.liveOrch, progress: current.runProgress, jobs: current.terminalJobs };
      }, storeUrl(f.page));
      assert.equal(state.running, false, "stateRevision must reject an older active-run snapshot after run.finished");
      assert.equal(state.liveOrch, "");
      assert.equal(state.progress, null);
      assert.deepEqual(state.jobs, []);
      assert.ok(!(await f.page.locator("body").innerText()).includes("Stale reconnect body"));
      assert.equal(await f.page.locator(".chat-scroll").getByText("Completed before snapshot returned", { exact: true }).count(), 1);
    } finally { release?.(); await f.close(); }
  });
  await test("WebUI narrow flows keep list navigation explicit during live updates and refreshes", async () => {
    const f = await fixture(false, { width: 390, height: 844 });
    try {
      const flow = { ...makeSession("narrow-flow"), title: "channel:fixture:narrow-room" };
      f.state.sessions.push(flow);
      f.state.overrides.set("/channels/flows", (_, fulfill) => fulfill([
        { id: "narrow-flow", channelId: "fixture", chatKey: "room", displayName: "Narrow flow", activeSession: flow.id },
      ]));
      await f.page.evaluate(() => { location.hash = "#/flows"; });
      await f.page.locator(".flows-back").waitFor();
      await f.page.waitForFunction(async (url) => {
        const { useExm } = await import(url) as typeof import("../webui/src/store");
        return useExm.getState().sessionId === "narrow-flow";
      }, storeUrl(f.page));
      await f.page.locator(".flows-back").click();
      const row = f.page.locator(".flow-item").filter({ hasText: "Narrow flow" });
      await row.waitFor({ state: "visible" });
      f.emit("run.progress", { stage: "preparing", message: "Narrow visible preparation", announce: true }, flow.id);
      await f.page.evaluate(() => window.dispatchEvent(new Event("exm:flows-refresh")));
      await f.page.waitForTimeout(100);
      assert.equal(await f.page.locator(".flows-shell").evaluate((element) => element.classList.contains("thread")), false, "Refreshing flows must not undo the explicit narrow back navigation");
      await row.click();
      await f.page.locator(".run-progress-message").getByText("Narrow visible preparation", { exact: true }).waitFor();
      f.emit("terminal.started", { taskId: "narrow-terminal", command: "Long narrow background command for the active flow", status: "running", startedAtMs: 2, outputPreview: "Narrow output", outputBytes: 13, outputTruncated: false }, flow.id);
      await f.page.locator(".terminal-job-head").click();
      await f.page.locator(".terminal-job-detail").getByText("Narrow output", { exact: true }).waitFor();
      const geometry = await f.page.evaluate(() => ({ width: innerWidth, documentWidth: document.documentElement.scrollWidth }));
      assert.ok(geometry.documentWidth <= geometry.width, `Live flow activity must fit the narrow viewport: ${JSON.stringify(geometry)}`);
      await screenshot(f.page, "webui-flow-live-activity-390x844");
    } finally { await f.close(); }
  });
  await test("WebUI chat-flow management can reopen archived flow history in the flow stream", async () => {
    const f = await fixture();
    try {
      const archived = { ...makeSession("archived-flow"), title: "channel:fixture:old-room", origin: "channel:fixture:old-room", originLabel: "Old room", archivedAt: "2026-11-18T00:00:00Z" };
      f.state.archivedSessions.push(archived);
      f.state.sessions.push({ ...makeSession("current-flow"), title: "channel:fixture:new-room" });
      f.state.overrides.set("/channels/flows", (_, fulfill) => fulfill([
        { id: "current-flow", channelId: "fixture", chatKey: "new-room", displayName: "Current room", activeSession: "current-flow" },
      ]));
      f.state.messages[archived.id] = [message(archived.id, "Archived flow transcript")];
      await f.page.evaluate(() => { location.hash = "#/flows"; });
      await f.page.locator(".flow-session-manager-trigger").waitFor();
      await f.page.locator(".flow-session-manager-trigger").click();
      const manager = f.page.locator(".session-manager-drawer");
      await manager.getByText("归档区", { exact: true }).click();
      const row = manager.locator('[data-session-id="archived-flow"]');
      await row.waitFor();
      await row.locator(".sessions-td-ops .ant-btn").last().click();
      await f.page.getByText("打开会话", { exact: true }).last().click();
      await f.page.waitForFunction(async (url) => {
        const { useExm } = await import(url) as typeof import("../webui/src/store");
        return useExm.getState().sessionId === "archived-flow";
      }, storeUrl(f.page));
      await f.page.waitForFunction(() => document.querySelector(".app-shell")?.getAttribute("data-view") === "flows");
      await f.page.getByText("历史通道会话 · 只读", { exact: true }).waitFor();
      await f.page.locator(".flows-messages").getByText("Archived flow transcript", { exact: true }).waitFor();
      await f.page.evaluate(() => window.dispatchEvent(new Event("exm:flows-refresh")));
      await f.page.waitForTimeout(100);
      assert.equal((await webState(f.page)).sessionId, archived.id, "Flow-list refresh must not replace an explicit archived-session selection");
      await f.page.locator(".flows-messages").getByText("Archived flow transcript", { exact: true }).waitFor();
      await f.page.locator(".flow-item").filter({ hasText: "Current room" }).click();
      await f.page.waitForFunction(async (url) => {
        const { useExm } = await import(url) as typeof import("../webui/src/store");
        return useExm.getState().sessionId === "current-flow";
      }, storeUrl(f.page));
      assert.ok(!(await f.page.locator(".flows-messages").innerText()).includes("Archived flow transcript"), "Opening the active flow should explicitly leave archived history");
      assert.deepEqual(f.errors, []);
    } finally { await f.close(); }
  });
  await test("WebUI clicking a content-search hit opens and flashes its matching message", async () => {
    const f = await fixture();
    try {
      const matching = message("b", "Needle is in this message");
      f.state.messages.b = [matching];
      f.state.overrides.set("/sessions/search", (route, fulfill) => {
        const query = new URL(route.request().url()).searchParams.get("q") ?? "";
        return fulfill({ query, hits: [makeSearchHit("b", "Conversation b", matching.id, "Needle is in this message")] });
      });
      await f.page.getByRole("button", { name: "管理会话", exact: true }).click();
      const manager = f.page.locator(".session-manager-drawer");
      await manager.locator(".sessions-search input").fill("needle");
      await manager.locator('.search-result[data-session-id="b"]').click();
      await f.page.waitForFunction(async (url) => {
        const { useExm } = await import(url) as typeof import("../webui/src/store");
        return useExm.getState().sessionId === "b";
      }, storeUrl(f.page));
      const target = f.page.locator(`[data-message-id="${matching.id}"]`);
      await target.waitFor();
      await eventually(() => target.evaluate((element) => element.classList.contains("msg-flash")), "Search navigation must flash the matching message");
      const bounds = await target.boundingBox();
      assert.ok(bounds && bounds.y + bounds.height > 0 && bounds.y < 900, "Target message should be in the visible chat area");
      await eventually(() => target.evaluate((element) => !element.classList.contains("msg-flash")), "The matching-message flash should clear");
    } finally { await f.close(); }
  });
  await test("WebUI command palette Enter opens the selected session and focuses its matching message", async () => {
    const f = await fixture();
    try {
      const matching = message("palette-1", "Needle palette message");
      matching.id = "needle-palette-message";
      f.state.messages["palette-1"] = [matching];
      f.state.overrides.set("/sessions/search", (route, fulfill) => {
        const query = new URL(route.request().url()).searchParams.get("q") ?? "";
        return fulfill({ query, hits: [makeSearchHit("palette-1", "Palette session", matching.id, "Needle palette message")] });
      });
      await f.page.keyboard.press("Control+k");
      const dialog = f.page.locator(".cmd-palette");
      const input = dialog.getByRole("textbox", { name: "会话速查", exact: true });
      await input.fill("needle");
      await dialog.locator(".cmd-item").waitFor();
      await input.press("Enter");
      await f.page.waitForFunction(async (url) => {
        const { useExm } = await import(url) as typeof import("../webui/src/store");
        return useExm.getState().sessionId === "palette-1";
      }, storeUrl(f.page));
      const target = f.page.locator(`[data-message-id="${matching.id}"]`);
      await target.waitFor();
      await eventually(() => target.evaluate((element) => element.classList.contains("msg-flash")), "Palette navigation must flash the matching message");
    } finally { await f.close(); }
  });
  await test("WebUI command palette clicking a result opens its session and focuses the matching message", async () => {
    const f = await fixture();
    try {
      const matching = message("palette-click", "Needle clicked result message");
      f.state.messages[matching.sessionId] = [matching];
      f.state.overrides.set("/sessions/search", (route, fulfill) => {
        const query = new URL(route.request().url()).searchParams.get("q") ?? "";
        return fulfill({ query, hits: [makeSearchHit(matching.sessionId, "Clicked session", matching.id, "Needle clicked result message")] });
      });
      await f.page.keyboard.press("Control+k");
      const dialog = f.page.locator(".cmd-palette");
      await dialog.getByRole("textbox", { name: "会话速查", exact: true }).fill("needle");
      await dialog.locator(".cmd-item").click();
      await f.page.waitForFunction(async (url) => {
        const { useExm } = await import(url) as typeof import("../webui/src/store");
        return useExm.getState().sessionId === "palette-click";
      }, storeUrl(f.page));
      const target = f.page.locator(`[data-message-id="${matching.id}"]`);
      await target.waitFor();
      await eventually(() => target.evaluate((element) => element.classList.contains("msg-flash")), "Clicking a palette result must flash the matching message");
    } finally { await f.close(); }
  });
  console.log(`${passed} browser regressions passed`);
} finally {
  await browser.close();
  await stopServers();
}
