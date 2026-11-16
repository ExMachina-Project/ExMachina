#!/usr/bin/env -S npx tsx
/** Browser regressions for two independent clients against isolated REST/WS fixtures. */
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import type { AddressInfo } from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { chromium, type Page, type Route, type WebSocketRoute } from "playwright-core";
import { createServer } from "vite";
import type {} from "../webui/src/desktop";
import type { ChatMessage, Session } from "../webui/src/types";

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
const makeSession = (id: string): Session => ({ id, title: `Conversation ${id}`, groupId: "single:machina", status: "active", running: false, ledger, createdAt: "2026-11-16T00:00:00Z", updatedAt: "2026-11-16T00:00:00Z" });
const message = (id: string, text: string): ChatMessage => ({ id: `msg-${text}`, sessionId: id, role: "orchestrator", statements: [{ tag: "报告", text }], createdAt: "2026-11-16T00:00:00Z" });
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
    sessions: [makeSession("a"), makeSession("b")], messages: { a: [], b: [] } as Record<string, ChatMessage[]>,
    approvals: [] as { id: string; sessionId: string; agentId: string; command: string; status?: string }[],
    sockets: [] as WebSocketRoute[], requests: [] as { path: string; method: string; body: unknown }[],
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
    state.requests.push({ path: p, method: route.request().method(), body: route.request().postData() ? route.request().postDataJSON() as unknown : null });
    const fulfill = async (body: unknown, status = 200) => { await route.fulfill({ status, contentType: "application/json", body: JSON.stringify(body) }); };
    const override = state.overrides.get(p);
    if (override) return override(route, fulfill);
    if (/\/sessions\/[^/]+\/chat$/.test(p)) return state.chat ? state.chat(route, fulfill) : fulfill({ accepted: true }, 202);
    if (p === "/sessions" && route.request().method() === "GET") return fulfill(state.sessions);
    if (p === "/sessions" && route.request().method() === "POST") {
      const created = { ...makeSession(`new-${state.sessions.length}`), title: (route.request().postDataJSON() as { title: string }).title };
      state.sessions.unshift(created); state.messages[created.id] = []; return fulfill(created);
    }
    if (p === "/workspace" && route.request().method() === "PUT") { state.workspace = (route.request().postDataJSON() as { path: string }).path; return fulfill({ workspace: state.workspace }); }
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
      if (!part) return fulfill(state.sessions.find((item) => item.id === id) ?? makeSession(id));
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
      "/workspace": { workspace: state.workspace }, "/health": { ok: true, mock: true, agents: 1 },
      "/version": { version: "0.7.0", gitHash: "fixture" }, "/audit": { items: [] },
      "/approvals": state.approvals,
      "/workbench/tree": { root: "/fixture", count: 0, nodes: [] },
      "/selfcheck": { ok: true, overall: "ok", items: [], errors: 0, warns: 0 },
      "/plugins": { plugins: [] }, "/skills": [], "/cron": [], "/cron/runs": [], "/memory/stats": { memory: { total: 0, pinned: 0, byKind: {} }, agentStats: [] }, "/memory": [], "/knowledge": { items: [], total: 0 }, "/channels": [], "/channels/flows": [],
      "/plugins/index": { entries: [], indexUrl: "" },
      "/nexus/links": { links: [] }, "/nexus/edges": { edges: [] }, "/nexus/status": { links: [], edges: [] },
      "/nexus/graph": { nodes: [], edges: [] },
      "/identity/list": { identities: [], codes: [], identityRequired: false },
    };
    return fulfill(responses[p] ?? []);
  });
  await page.routeWebSocket(/\/ws\?/, (socket) => {
    state.sockets.push(socket);
    socket.onMessage((text) => { if (text === "ping") { state.pings += 1; if (state.pong) socket.send("pong"); } });
  });
  await page.goto(desktop ? desktopBase : base, { waitUntil: "domcontentloaded" });
  if (desktop) {
    await page.waitForSelector("#btn-send", { state: "visible" });
    await page.waitForFunction(async (url) => { const { useExm } = await import(url) as typeof import("../webui/src/store"); return useExm.getState().sessionId === "a"; }, `${desktopBase}/@fs${root}/webui/src/store.ts`);
  } else {
    await page.waitForSelector(".chat-composer textarea", { state: "attached" });
    if (viewport.width <= 860) await page.locator(".session-row").filter({ hasText: "Conversation a" }).click();
    await page.waitForSelector(".chat-composer textarea", { state: "visible" });
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
const storeUrl = (page: Page) => `${new URL(page.url()).origin}${page.url().startsWith(desktopBase) ? `/@fs${root}/webui` : ""}/src/store.ts`;
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
  if (screenshotDir) await page.screenshot({ path: path.join(screenshotDir, `${name}.png`) });
}

try {
  for (const desktop of [false, true]) {
    await test(`${desktop ? "Desktop" : "WebUI"} client identity saves, reloads, validates and resets`, async () => {
      const f = await fixture(desktop);
      try {
        let saved = { ...profile, clientIdentity: { preset: "default", userAgent: "", headers: {} } };
        f.state.overrides.set("/llm/profiles", (route, fulfill) => {
          if (route.request().method() === "POST") {
            saved = { ...saved, ...route.request().postDataJSON() };
            return fulfill({ ok: true, id: profile.id });
          }
          return fulfill({ active: profile.id, mock: true, profiles: [saved] });
        });
        if (desktop) {
          await f.page.getByRole("button", { name: "设置", exact: true }).click();
          await f.page.getByRole("tab", { name: "模型", exact: true }).click();
        } else {
          await f.page.evaluate(() => { location.hash = "#/models"; });
        }
        await f.page.locator(".model-card").first().waitFor();
        await f.page.locator(".model-card .anticon-edit").first().click();
        const modal = f.page.locator(".ant-modal:not(.desk-settings)").last();
        await modal.getByText("客户端伪装", { exact: true }).first().click();
        await modal.getByTestId("client-identity-preset").click();
        await f.page.getByText("Claude Code", { exact: true }).last().click();
        await modal.getByTestId("client-identity-user-agent").fill("fixture-cli/1.2");
        const headers = modal.getByTestId("client-identity-headers");
        await headers.fill('{"Authorization":"forbidden"}');
        await f.page.locator(".ant-modal-footer .ant-btn-primary").last().click();
        await modal.getByText(/请输入最多 32 项的 JSON 对象/).waitFor();
        assert.equal(f.state.requests.filter((r) => r.path === "/llm/profiles" && r.method === "POST").length, 0);
        await headers.fill('{"x-client-name":"fixture"}');
        await f.page.locator(".ant-modal-footer .ant-btn-primary").last().click();
        await eventually(() => saved.clientIdentity.preset === "claude-code", "Client identity was not saved");
        assert.deepEqual(saved.clientIdentity, { preset: "claude-code", userAgent: "fixture-cli/1.2", headers: { "x-client-name": "fixture" } });
        await modal.waitFor({ state: "hidden" });
        await f.page.locator(".model-card .anticon-edit").first().click();
        await modal.getByText("客户端伪装", { exact: true }).first().click();
        assert.equal(await modal.getByTestId("client-identity-user-agent").inputValue(), "fixture-cli/1.2");
        assert.ok((await headers.inputValue()).includes("x-client-name"));
        await screenshot(f.page, `${desktop ? "desktop" : "webui"}-client-identity`);
        await modal.getByRole("button", { name: "恢复默认身份", exact: true }).click();
        await f.page.locator(".ant-modal-footer .ant-btn-primary").last().click();
        await eventually(() => saved.clientIdentity.preset === "default", "Client identity was not disabled");
        assert.deepEqual(saved.clientIdentity, { preset: "default" });
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
    await test(`${name} stale approval refresh cannot resurrect a resolved request`, async () => {
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
        f.emit("approval.required", { approvalId: "ap", agentId: "agent", command: "echo pending" });
        f.state.approvals = [{ id: "ap", sessionId: "a", agentId: "agent", command: "echo pending" }];
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
  await test("Desktop workspace changes protect unsaved files and clear old file tabs", async () => {
    const f = await fixture(true);
    try {
      await f.page.getByRole("button", { name: "查看代码", exact: true }).click(); await f.page.getByText("main.rs", { exact: true }).click();
      await f.page.locator(".cm-content").click(); await f.page.keyboard.type("// draft");
      await f.page.getByRole("button", { name: "打开项目", exact: true }).click(); await f.page.getByRole("textbox", { name: "项目目录" }).fill("/fixture/other");
      await f.page.getByRole("dialog").getByRole("button", { name: "打开", exact: true }).click(); await f.page.getByRole("button", { name: "继续编辑", exact: true }).click();
      assert.equal(f.state.workspace, "/fixture/project");
      await f.page.getByRole("dialog").getByRole("button", { name: "打开", exact: true }).click(); await f.page.getByRole("button", { name: "丢弃修改", exact: true }).click();
      await eventually(() => f.state.workspace === "/fixture/other", "Workspace did not change");
      await f.page.getByRole("button", { name: "关闭 src/main.rs", exact: true }).waitFor({ state: "detached" });
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
  await test("Desktop settings keep management inside the workspace dialog", async () => {
    const f = await fixture(true);
    try {
      f.state.overrides.set("/memory/md", (_, fulfill) => fulfill({ content: "fixture memory" }));
      await f.page.getByRole("button", { name: "设置", exact: true }).click();
      for (const label of ["模型", "智能体", "桌面", "技能", "插件", "会话", "审批", "记忆", "知识", "自动化", "连结", "高级", "审计"]) {
        await f.page.getByRole("tab", { name: label, exact: true }).click();
        await f.page.getByRole("tabpanel").waitFor({ state: "visible" });
        await f.page.waitForFunction(() => !document.querySelector(".desk-settings .ant-spin-spinning"));
        await f.page.waitForLoadState("networkidle");
        assert.ok(await f.page.locator(".desk-statusbar").count());
      }
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
      await f.page.locator(".cfg2-foot .ant-btn-primary").click();
      await eventually(() => f.state.requests.some((request) => request.path === "/config" && request.method === "PUT"), "Settings update missing");
      const update = f.state.requests.findLast((request) => request.path === "/config" && request.method === "PUT")!.body as { compaction: { triggerRatio: number } };
      assert.equal(update.compaction.triggerRatio, 0.72); assert.ok(!Object.hasOwn(update, "compaction.triggerRatio"));
      await screenshot(f.page, "webui-settings");
    } finally { await f.close(); }
  });
  console.log(`${passed} browser regressions passed`);
} finally {
  await browser.close();
  await stopServers();
}
