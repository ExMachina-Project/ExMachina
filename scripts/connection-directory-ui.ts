#!/usr/bin/env -S npx tsx
/** Isolated real-browser regressions for the Nexus connection directory. */
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import { createServer as createPortServer, type AddressInfo } from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { chromium, type Browser, type BrowserContext, type Page } from "playwright-core";
import { createServer } from "vite";
import type { Session } from "../webui/src/types";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const screenshotDir = process.env.UI_SCREENSHOT_DIR;
if (screenshotDir) await fs.mkdir(screenshotDir, { recursive: true });

const randomPort = await new Promise<number>((resolve, reject) => {
  const probe = createPortServer();
  probe.once("error", reject);
  probe.listen(0, "127.0.0.1", () => {
    const port = (probe.address() as AddressInfo).port;
    probe.close((error) => error ? reject(error) : resolve(port));
  });
});

const server = await createServer({
  root: path.join(root, "webui"),
  configFile: path.join(root, "webui", "vite.config.ts"),
  server: { host: "127.0.0.1", port: randomPort, strictPort: false },
  logLevel: "error",
});
await server.listen();
const base = `http://127.0.0.1:${(server.httpServer!.address() as AddressInfo).port}`;

const instanceId = "00000000-0000-4000-8000-000000000001";
const generation = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const relationId = "00000000-0000-4000-8000-000000000099";
const nextCursor = "00000000-0000-4000-8000-000000000201";
const secretCanary = "connection-directory-private-canary-5f0d31";

type ConnectionKind = "software" | "external_agent" | "device";
type ConnectionRecord = {
  kind: ConnectionKind;
  displayName: string;
  reference: { instanceId: string; connectionId: string; generation: string; kind: ConnectionKind };
  sourceRef: {
    kind: string;
    sourceId: string;
    scope: { instanceId: string; authorityId: string; accountId: string; projectId?: string };
    subresourceId?: string;
    lifecycle: { state: "known" | "legacy_unresolved"; generation?: string };
    revision: string;
  };
  sourceEditTarget: { surface: string; sourceId: string };
  registrationState: string;
  registrationRevision: number;
  readiness: {
    enabled: boolean;
    availability: string;
    handshake: string;
    freshness: string;
    reasons: string[];
  };
  capabilities: Array<{
    name: string;
    role: string;
    origin: string;
    effects: string[];
    verification: string;
    authorization: string;
  }>;
  capabilitiesTotal: number;
  capabilitiesTruncated: boolean;
  relations: Array<{
    id: string;
    kind: string;
    revision: number;
    from: { reference: NonNullable<ConnectionRecord["reference"]>; displayName: string };
    to: { reference: NonNullable<ConnectionRecord["reference"]>; displayName: string };
  }>;
  /** Deliberately unknown wire metadata; a view must not echo it. */
  privateFixtureMarker?: string;
};

type DirectoryResponse = {
  records: ConnectionRecord[];
  issues: Array<{ source: string; reason: string }>;
  total: number;
  limit: number;
  offset: number;
  truncated: boolean;
  nextOffset: number | null;
  nextCursor: string | null;
  managementOnly: true;
  executionAuthorization: "unknown";
};

type ApiReply = { status?: number; body: unknown; delayMs?: number };
type RequestRecord = { path: string; method: string; query: Record<string, string> };
type FixtureState = {
  requests: RequestRecord[];
  pageErrors: string[];
  consoleErrors: string[];
  blockedExternal: string[];
  websocketCount: number;
  directoryCalls: number;
  directoryResponder: (url: URL, call: number) => Promise<ApiReply> | ApiReply;
};

function makeRecord(
  n: number,
  kind: ConnectionKind,
  displayName: string,
  sourceKind = "software",
  sourceId = `source-${n}`,
  accountId = `account-${n}`,
): ConnectionRecord {
  const ref = { instanceId, connectionId: generation(n + 1), generation: generation(n + 101), kind };
  return {
    kind,
    displayName,
    reference: ref,
    sourceRef: {
      kind: sourceKind,
      sourceId,
      scope: { instanceId, authorityId: "fixture-authority", accountId },
      lifecycle: { state: "known", generation: `source-generation-${n}` },
      revision: `source-revision-${n}`,
    },
    sourceEditTarget: { surface: `${sourceKind}-settings`, sourceId },
    registrationState: "enabled",
    registrationRevision: 1,
    readiness: {
      enabled: true,
      availability: "available",
      handshake: "not_observed",
      freshness: "current",
      reasons: [],
    },
    capabilities: [{
      name: kind === "external_agent" ? "ask" : "read",
      role: kind === "external_agent" ? "task_executor" : "tool_provider",
      origin: "declared",
      effects: ["read"],
      verification: "unverified",
      authorization: "unknown",
    }],
    capabilitiesTotal: 1,
    capabilitiesTruncated: false,
    relations: [],
    privateFixtureMarker: secretCanary,
  };
}

const docsA = makeRecord(1, "software", "Same-name Docs", "software", "remote-docs", "account-alpha");
const docsB = makeRecord(2, "software", "Same-name Docs", "software", "remote-docs", "account-beta");
const embeddedAi = makeRecord(3, "external_agent", "Docs Assistant", "software", "remote-docs", "account-alpha");
embeddedAi.sourceRef.subresourceId = "assistant";
const relation = {
  id: relationId,
  kind: "embedded_in",
  revision: 1,
  from: { reference: embeddedAi.reference, displayName: embeddedAi.displayName },
  to: { reference: docsA.reference, displayName: docsA.displayName },
};
embeddedAi.relations = [relation];
docsA.relations = [relation];

const longRecordName = `Directory fixture with a deliberately long name ${"长名称 / external account ".repeat(9)}`;
const baseRecords: ConnectionRecord[] = [
  docsA,
  docsB,
  embeddedAi,
  makeRecord(4, "device", "Registered display only", "device", "device-4", "device-account"),
  ...Array.from({ length: 58 }, (_, index) => makeRecord(index + 10, "software", index === 0 ? longRecordName : `Software fixture ${index + 1}`, index % 3 === 0 ? "plugin" : "mcp")),
];

function pageFor(records: ConnectionRecord[], url: URL): DirectoryResponse {
  const search = (url.searchParams.get("search") ?? "").toLocaleLowerCase();
  const kind = url.searchParams.get("kind");
  const sourceKind = url.searchParams.get("sourceKind");
  const filtered = records.filter((record) =>
    (!kind || record.kind === kind)
    && (!sourceKind || record.sourceRef.kind === sourceKind)
    && (!search || record.displayName.toLocaleLowerCase().includes(search)
      || record.sourceRef.sourceId.toLocaleLowerCase().includes(search)
      || record.capabilities.some((capability) => capability.name.toLocaleLowerCase().includes(search))),
  );
  const limit = Number(url.searchParams.get("limit") ?? 50);
  const offset = Number(url.searchParams.get("offset") ?? 0);
  const pageRecords = filtered.slice(offset, offset + limit);
  const hasMore = offset + pageRecords.length < filtered.length;
  return {
    records: pageRecords,
    issues: [],
    total: filtered.length,
    limit,
    offset,
    truncated: hasMore,
    nextOffset: hasMore ? offset + pageRecords.length : null,
    nextCursor: hasMore ? nextCursor : null,
    managementOnly: true,
    executionAuthorization: "unknown",
  };
}

function defaultDirectoryReply(url: URL): ApiReply {
  const offset = Number(url.searchParams.get("offset") ?? 0);
  if (url.searchParams.has("cursor") && offset === 0) {
    return { status: 409, body: { error: "fixture offset required with cursor" } };
  }
  if (offset > 0 && url.searchParams.get("cursor") !== nextCursor) {
    return { status: 409, body: { error: "fixture cursor required" } };
  }
  return { body: pageFor(baseRecords, url) };
}

const testConfig = {
  mock: true,
  llm: { apiKey: "", baseUrl: "", model: "fixture" },
  memory: { enabled: false, recallLimit: 6, halfLifeDays: 30, semanticModel: "" },
  security: { execApproval: "risky", execAllowlist: "" },
  automation: {}, maxSessionTokens: 0, maxConcurrency: 2, tools: {}, trash: {},
  experiments: {}, identity: {}, limits: {}, triggers: { fileWatch: {}, eventWebhook: {} },
  mcpServe: {}, notify: {},
};

const fixtureSession: Session = {
  id: "directory-observed-session",
  title: "Existing observed session",
  groupId: "single:fixture",
  workspace: "/fixture",
  status: "active",
  running: false,
  ledger: {
    task: { goal: "fixture", acceptance: [], constraints: [], forbidden: [], priorities: [] },
    evidence: { confirmed: [], gaps: [] },
    risk: { openAssertions: [], impact: [], revertPaths: [], blockers: [] },
  },
  createdAt: "2026-11-26T00:00:00Z",
  updatedAt: "2026-11-26T00:00:00Z",
};

async function fixture(viewport = { width: 1440, height: 900 }) {
  assert.ok(browser, "Browser process must be launched before creating fixtures");
  const context: BrowserContext = await browser.newContext({ viewport, storageState: { cookies: [], origins: [] }, serviceWorkers: "block" });
  const page = await context.newPage();
  page.setDefaultTimeout(10_000);
  page.setDefaultNavigationTimeout(30_000);
  const state: FixtureState = {
    requests: [],
    pageErrors: [],
    consoleErrors: [],
    blockedExternal: [],
    websocketCount: 0,
    directoryCalls: 0,
    directoryResponder: (url) => defaultDirectoryReply(url),
  };
  page.on("pageerror", (error) => state.pageErrors.push(error.stack ?? error.message));
  page.on("console", (message) => {
    if (message.type() === "error") state.consoleErrors.push(message.text());
  });
  await page.addInitScript(() => {
    localStorage.setItem("exm.key", "isolated-directory-fixture");
    localStorage.setItem("exm.sfx", "0");
    window.__EXM_GATEWAY__ = { url: location.origin, key: "isolated-directory-fixture", remote: true };
  });

  // Every browser context has its own isolated storage and cannot reach external hosts.
  await context.route("**/*", async (route) => {
    const url = new URL(route.request().url());
    if (url.origin !== base) {
      state.blockedExternal.push(url.origin);
      await route.abort();
      return;
    }
    await route.continue();
  });

  await page.route("**/api/**", async (route) => {
    const requestUrl = new URL(route.request().url());
    if (!requestUrl.pathname.startsWith("/api/")) return route.continue();
    const apiPath = requestUrl.pathname.replace(/^\/api/, "");
    state.requests.push({
      path: apiPath,
      method: route.request().method(),
      query: Object.fromEntries(requestUrl.searchParams.entries()),
    });
    const fulfill = async (body: unknown, status = 200) => route.fulfill({
      status,
      contentType: "application/json",
      body: JSON.stringify(body),
    });
    if (apiPath === "/connections" && route.request().method() === "GET") {
      const call = ++state.directoryCalls;
      const reply = await state.directoryResponder(requestUrl, call);
      if (reply.delayMs) await new Promise((resolve) => setTimeout(resolve, reply.delayMs));
      return fulfill(reply.body, reply.status ?? 200);
    }
    if (apiPath === "/auth/verify" && route.request().method() === "POST") return fulfill({ ok: true });
    if (route.request().method() !== "GET") return fulfill({ error: "unexpected mutation in read-only browser fixture" }, 405);

    if (apiPath === "/sessions") return fulfill([fixtureSession]);
    if (apiPath === `/sessions/${fixtureSession.id}`) return fulfill(fixtureSession);
    if (apiPath === `/sessions/${fixtureSession.id}/graph`) return fulfill({ nodes: [] });
    if (apiPath === `/sessions/${fixtureSession.id}/plan`) return fulfill({ sessionId: fixtureSession.id, mode: "execute", items: [], count: 0 });
    if (apiPath === `/sessions/${fixtureSession.id}/tokens`) return fulfill({ estimate: 0, budget: 0, unlimited: true });

    const responses: Record<string, unknown> = {
      "/auth/status": { keyConfigured: true },
      "/auth/verify": { ok: true },
      "/config": testConfig,
      "/config/schema": { configVersion: 1, groups: [] },
      "/target": { mode: "single", id: "fixture", name: "Fixture", primary: "fixture" },
      "/groups": { active: "default", groups: [{ id: "default", name: "Fixture group", builtin: true, createdAt: "" }] },
      "/singles": { active: "fixture", singles: [{ identifier: "fixture", name: "Fixture", description: "", domain: "" }] },
      "/agents": [],
      "/llm/profiles": { active: "fixture", mock: true, profiles: [] },
      "/llm/effort": { effort: "default" },
      "/llm/capabilities": { speech: "", transcribe: "", embedding: "", visionRelay: "" },
      "/workspace": { workspace: "/fixture", explicit: null, scoped: false },
      "/health": { ok: true, mock: true, agents: 0, features: [] },
      "/version": { version: "fixture", gitHash: "fixture" },
      "/approvals": [],
      "/plugins": { plugins: [] },
      "/channels": [],
      "/channels/status": {},
      "/channels/flows": [],
      "/nexus/links": { links: [] },
      "/nexus/edges": { edges: [] },
      "/nexus/status": { links: [], edges: [] },
      "/nexus/graph": { nodes: [], edges: [] },
      "/identity/list": { identities: [], codes: [], identityRequired: false },
      "/templates": { templates: [] },
      "/plugins/index": { entries: [], indexUrl: "fixture" },
    };
    return fulfill(responses[apiPath] ?? []);
  });
  await page.routeWebSocket(/\/ws(?:\?|$)/, (socket) => {
    state.websocketCount += 1;
    socket.onMessage((message) => {
      if (message === "ping") socket.send("pong");
    });
  });

  try {
    // Match the existing UI regression fixture: boot the shell first, then use the
    // hash router so startup effects cannot race the initial view selection.
    await page.goto(base, { waitUntil: "domcontentloaded" });
    await page.waitForSelector(".app-shell", { state: "attached" });
    await page.waitForFunction(async (url) => {
      const { useExm } = await import(url) as typeof import("../webui/src/store");
      return useExm.getState().sessionId === "directory-observed-session";
    }, `${base}/src/store/index.ts`);
    await page.evaluate(() => { location.hash = "#/nexus"; });
    await page.waitForFunction(() => document.querySelector(".app-shell")?.getAttribute("data-view") === "nexus");
  } catch (error) {
    const body = (await page.locator("body").innerText().catch(() => "")).slice(0, 500);
    const html = (await page.content().catch(() => "")).slice(0, 1_000);
    await context.close();
    throw new Error(`WebUI fixture failed to boot: ${body}\nURL=${page.url()}\nHTML=${html}\nRequests=${JSON.stringify(state.requests)}\n${state.pageErrors.join("\n")}`, { cause: error });
  }

  return {
    page,
    context,
    state,
    async enterDirectory() {
      await page.locator(".pane-wrap .page-head").waitFor({ state: "visible" });
      const headerTabs = page.locator(".pane-wrap .page-head .ant-segmented-item");
      await headerTabs.nth(1).click();
      await page.locator(".pane-wrap .ant-segmented").nth(1).waitFor({ state: "visible" });
      const nexusTabs = page.locator(".pane-wrap .ant-segmented").nth(1).locator(".ant-segmented-item");
      await nexusTabs.nth(2).click();
      await page.getByRole("searchbox").first().waitFor({ state: "visible" });
      await page.locator(".connection-directory").waitFor({ state: "visible" });
      await page.locator(".connection-directory-records, .connection-directory .brand-empty-desc, .connection-directory-error").first().waitFor({ state: "visible" });
    },
    async close() {
      await context.close();
      assert.deepEqual(state.pageErrors, [], "No uncaught browser page errors");
    },
    async screenshot(name: string) {
      if (!screenshotDir) return;
      await page.screenshot({ path: path.join(screenshotDir, `${name}.png`), animations: "disabled" });
    },
  };
}

async function eventually(check: () => boolean | Promise<boolean>, note: string, timeoutMs = 5_000): Promise<void> {
  const until = Date.now() + timeoutMs;
  while (!(await check())) {
    assert.ok(Date.now() < until, note);
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}

function directoryRequests(state: FixtureState): RequestRecord[] {
  return state.requests.filter((request) => request.path === "/connections");
}

function assertReadOnlySince(state: FixtureState, start: number): void {
  const requests = state.requests.slice(start);
  assert.ok(requests.length > 0, "Expected a directory read request");
  assert.deepEqual(requests.filter((request) => request.method !== "GET"), [], "Directory browsing and filtering must remain GET-only");
  const prohibited = requests.filter((request) =>
    /(?:^|\/)(?:discover(?:y)?|probe|login|pair(?:ing)?|task(?:s)?|target|config|stop|credential(?:s)?|secret(?:s)?)(?:\/|$)/i.test(request.path)
    || request.path === "/auth/verify"
    || request.path.startsWith("/llm/"),
  );
  assert.deepEqual(prohibited, [], "Directory browsing must not probe, log in, pair, invoke tasks, switch target/config, stop, or call model APIs");
}

async function checkNoCanary(f: Awaited<ReturnType<typeof fixture>>): Promise<void> {
  const visibleText = await f.page.locator("body").innerText();
  assert.equal(visibleText.includes(secretCanary), false, "Unknown private wire metadata must not be rendered");
  assert.equal([...f.state.pageErrors, ...f.state.consoleErrors].join("\n").includes(secretCanary), false, "Private canary must not leak into browser errors or logs");
}

function directoryCards(page: Page) {
  return page.locator(".connection-directory-records > .connection-directory-card");
}

function cardNamed(page: Page, recordName: string) {
  const name = page.locator(".connection-directory-name").filter({ hasText: recordName });
  return directoryCards(page).filter({ has: name }).first();
}

async function openRecordDetails(page: Page, recordName: string): Promise<void> {
  const card = cardNamed(page, recordName);
  await card.waitFor({ state: "visible" });
  const summary = card.locator("summary");
  await summary.focus();
  await summary.press("Enter");
}

const failures: Array<{ name: string; error: unknown }> = [];
let passed = 0;
async function test(name: string, run: () => Promise<void>): Promise<void> {
  if (process.env.UI_TEST_FILTER && !name.includes(process.env.UI_TEST_FILTER)) return;
  try {
    await run();
    passed += 1;
    console.log(`✓ ${name}`);
  } catch (error) {
    failures.push({ name, error });
    console.error(`✗ ${name}\n${error instanceof Error ? error.stack ?? error.message : String(error)}`);
  }
}

let browser: Browser | undefined;
try {
  const chromiumPath = process.env.CHROMIUM || await fs.access("/usr/bin/chromium").then(() => "/usr/bin/chromium", () => chromium.executablePath());
  browser = await chromium.launch({ executablePath: chromiumPath, args: ["--no-sandbox", "--disable-dev-shm-usage"] });

  await test("directory loads only a local read-only snapshot and keeps same-name accounts and software AI distinct", async () => {
    const f = await fixture();
    try {
      const before = f.state.requests.length;
      const websocketBefore = f.state.websocketCount;
      await f.enterDirectory();
      await eventually(() => directoryRequests(f.state).length > 0, "Selecting the unified directory should request its snapshot");
      await eventually(async () => (await f.page.locator("body").innerText()).includes("Docs Assistant"), "Directory records did not render");
      assert.equal(f.state.requests.slice(before).some((request) => request.path === "/connections"), true);
      assert.equal(await directoryCards(f.page).locator(".connection-directory-name").filter({ hasText: "Same-name Docs" }).count(), 2,
        "Same source ID in distinct accounts must remain two records");
      assert.equal(await directoryCards(f.page).locator(".connection-directory-name").filter({ hasText: "Docs Assistant" }).count(), 1,
        "Embedded AI must remain an independently typed record");
      assert.ok(directoryRequests(f.state).every((request) => request.query.limit === "50" && Number(request.query.offset ?? "0") === 0),
        "Initial snapshot requests must be bounded to page one");
      assert.equal(f.state.websocketCount, websocketBefore, "Opening the directory must not start another WebSocket activity channel");
      assertReadOnlySince(f.state, before);
      assert.equal(f.state.blockedExternal.length, 0, "Fixture must not attempt external network access");
      await checkNoCanary(f);
    } finally {
      await f.close();
    }
  });

  await test("type/source filters and search issue bounded GET queries and restore defaults", async () => {
    const f = await fixture();
    try {
      await f.enterDirectory();
      await eventually(() => directoryRequests(f.state).length >= 1, "Initial directory request missing");
      const baseline = f.state.requests.length;
      const search = f.page.getByRole("searchbox").first();
      await search.fill("Docs Assistant");
      await search.press("Enter");
      await eventually(() => directoryRequests(f.state).some((request) => request.query.search === "Docs Assistant"), "Search request was not issued");
      await eventually(async () => await directoryCards(f.page).count() === 1, "Search results did not settle");
      const filters = f.page.locator(".connection-directory-filters").getByRole("combobox");
      await filters.nth(0).click();
      await f.page.locator(".ant-select-dropdown:visible .ant-select-item-option").filter({ hasText: /软件 \/ 服务|Software/ }).click();
      await eventually(() => directoryRequests(f.state).some((request) => request.query.kind === "software"), "Type filter request was not issued");
      await f.page.locator(".connection-directory .brand-empty-desc").waitFor({ state: "visible" });
      await filters.nth(1).click();
      await f.page.locator(".ant-select-dropdown:visible .ant-select-item-option").filter({ hasText: /软件登记|software/i }).click();
      await eventually(() => directoryRequests(f.state).some((request) => request.query.sourceKind === "software"), "Source filter request was not issued");
      const reset = f.page.getByRole("button", { name: /恢复默认|清除筛选|reset filters|clear filters/i });
      await reset.click();
      await eventually(() => {
        const query = directoryRequests(f.state).at(-1)?.query ?? {};
        return !query.search && !query.kind && !query.sourceKind && query.offset === "0";
      }, "Restoring defaults must clear filters and return to the first page");
      assertReadOnlySince(f.state, baseline);
      await checkNoCanary(f);
    } finally {
      await f.close();
    }
  });

  await test("details show typed identity and explicit software-AI relation without implying authorization", async () => {
    const f = await fixture();
    try {
      const baseline = f.state.requests.length;
      await f.enterDirectory();
      await eventually(async () => (await f.page.locator("body").innerText()).includes("Docs Assistant"), "AI record did not render");
      await openRecordDetails(f.page, "Docs Assistant");
      const detail = cardNamed(f.page, "Docs Assistant").locator(".connection-directory-detail");
      await detail.waitFor({ state: "visible" });
      await detail.getByText("Same-name Docs", { exact: false }).waitFor();
      const detailText = await detail.innerText();
      assert.match(detailText, /external_agent|software|外部|软件/i, "Details must distinguish the AI resource type and source");
      assert.match(detailText, /unknown|未知|not authorized|未授权/i, "No directory row or relation may imply execution authorization");
      assert.match(detailText, /embedded_in|embedded-in|内置|嵌入/i, "Relationship type should be visible in details");
      assertReadOnlySince(f.state, baseline);
      await checkNoCanary(f);
    } finally {
      await f.close();
    }
  });

  await test("unverified display-only records do not invent capabilities and legacy Nexus list stays available", async () => {
    const f = await fixture();
    try {
      f.state.directoryResponder = (url) => {
        const record = makeRecord(801, "software", "Unverified display-only service", "software", "unverified-service");
        record.capabilities = [];
        record.capabilitiesTotal = 0;
        return { body: pageFor([record], url) };
      };
      await f.enterDirectory();
      await eventually(async () => (await f.page.locator("body").innerText()).includes("Unverified display-only service"),
        "Display-only connection did not render");
      await openRecordDetails(f.page, "Unverified display-only service");
      const details = cardNamed(f.page, "Unverified display-only service").locator(".connection-directory-detail");
      await details.getByText(/没有可显示的能力声明|no capability declarations/i).waitFor();
      assert.equal(await details.locator(".connection-directory-capabilities li").count(), 0,
        "A product/source name cannot synthesize a capability");

      const beforeOpeningLegacyList = f.state.requests.length;
      const nexusTabs = f.page.locator(".pane-wrap .ant-segmented").nth(1).locator(".ant-segmented-item");
      assert.equal(await nexusTabs.count(), 3, "Legacy topology/list tabs must remain alongside the directory");
      await nexusTabs.nth(1).click();
      await f.page.locator(".pane-wrap .ant-segmented").nth(1).waitFor({ state: "visible" });
      await eventually(() => f.state.requests.some((request) => request.path === "/nexus/links" && request.method === "GET"),
        "The existing Nexus source list must remain reachable");
      assertReadOnlySince(f.state, beforeOpeningLegacyList);
    } finally {
      await f.close();
    }
  });

  await test("pagination follows the server cursor and returns to its previous page", async () => {
    const f = await fixture();
    try {
      f.state.directoryResponder = (url) => defaultDirectoryReply(url);
      await f.enterDirectory();
      await eventually(() => directoryRequests(f.state).length > 0, "First page was not loaded");
      const baseline = f.state.requests.length;
      const next = f.page.getByRole("button", { name: /下一页|next/i });
      await next.waitFor({ state: "visible" });
      await next.click();
      await eventually(() => directoryRequests(f.state).some((request) => request.query.offset === "50"), "Next page request missing");
      const second = directoryRequests(f.state).find((request) => request.query.offset === "50");
      assert.equal(second?.query.cursor, nextCursor, "Offset pagination must be bound to the server-issued cursor");
      await eventually(async () => (await f.page.locator("body").innerText()).includes("Software fixture 58"), "Second page contents did not render");
      const previous = f.page.getByRole("button", { name: /上一页|previous/i });
      await previous.click();
      await eventually(() => directoryRequests(f.state).some((request) => request.query.offset === "0"), "Previous page request missing");
      assertReadOnlySince(f.state, baseline);
    } finally {
      await f.close();
    }
  });

  await test("empty directory state is explicit and contains no phantom records", async () => {
    const f = await fixture();
    try {
      f.state.directoryResponder = (url) => ({ body: pageFor([], url) });
      await f.enterDirectory();
      await f.page.locator(".brand-empty-desc").waitFor({ state: "visible" });
      assert.equal(await directoryCards(f.page).count(), 0);
      assert.equal(await f.page.locator(".connection-directory-error").count(), 0);
    } finally {
      await f.close();
    }
  });

  await test("initial failure, stale refresh failure, and retry preserve truthful directory state", async () => {
    const f = await fixture();
    try {
      let mode: "fail-first" | "normal" | "fail-refresh" = "fail-first";
      f.state.directoryResponder = (url, call) => {
        if (mode === "fail-first") return { status: 503, body: { error: "fixture initial failure" } };
        if (mode === "fail-refresh") return { status: 503, body: { error: "fixture refresh failure" } };
        return { body: pageFor([docsA], url) };
      };
      await f.enterDirectory();
      await eventually(() => f.page.locator(".connection-directory-error").count().then((count) => count > 0), "Initial read failure should be visible");
      assert.equal(await directoryCards(f.page).count(), 0, "A first-load failure has no stale records to invent");
      mode = "normal";
      await f.page.locator(".connection-directory-error").getByRole("button").click();
      await eventually(async () => (await f.page.locator("body").innerText()).includes("Same-name Docs"), "Retry did not load current records");
      mode = "fail-refresh";
      const refresh = f.page.locator(".connection-directory-actions").getByRole("button").first();
      await refresh.click();
      await eventually(() => f.page.locator(".connection-directory-error").count().then((count) => count > 0), "Refresh failure should be visible");
      assert.equal(await directoryCards(f.page).filter({ hasText: "Same-name Docs" }).count(), 1, "Refresh failure must retain the last successful records");
      mode = "normal";
      await f.page.locator(".connection-directory-error").getByRole("button").click();
      await eventually(() => f.page.locator(".connection-directory-error").count().then((count) => count === 0), "Successful retry should clear the refresh error");
      assert.equal(await directoryCards(f.page).filter({ hasText: "Same-name Docs" }).count(), 1);
      await checkNoCanary(f);
    } finally {
      await f.close();
    }
  });

  await test("late search response A cannot replace the newer response B", async () => {
    const f = await fixture();
    try {
      f.state.directoryResponder = (url) => {
        const query = url.searchParams.get("search") ?? "";
        const record = makeRecord(query === "A" ? 701 : 702, "software", query === "A" ? "Result A" : "Result B");
        return { body: pageFor([record], url), delayMs: query === "A" ? 700 : 40 };
      };
      await f.enterDirectory();
      await eventually(() => directoryRequests(f.state).length > 0, "Initial request missing");
      const baseline = f.state.requests.length;
      const search = f.page.getByRole("searchbox").first();
      await search.fill("A");
      await search.press("Enter");
      await eventually(() => directoryRequests(f.state).some((request) => request.query.search === "A"), "A request missing");
      await search.fill("B");
      await search.press("Enter");
      await eventually(() => directoryRequests(f.state).some((request) => request.query.search === "B"), "B request missing");
      await f.page.getByText("Result B", { exact: true }).waitFor();
      await f.page.waitForTimeout(850);
      assert.equal(await f.page.getByText("Result B", { exact: true }).count(), 1);
      assert.equal(await f.page.getByText("Result A", { exact: true }).count(), 0, "Stale response must not overwrite the current filter result");
      assertReadOnlySince(f.state, baseline);
    } finally {
      await f.close();
    }
  });

  await test("expired pagination cursor keeps stale records until explicit first-page recheck", async () => {
    const f = await fixture();
    try {
      f.state.directoryResponder = (url) => Number(url.searchParams.get("offset") ?? 0) > 0
        ? { status: 409, body: { error: "fixture cursor expired" } }
        : { body: pageFor(baseRecords, url) };
      await f.enterDirectory();
      await eventually(() => directoryRequests(f.state).length > 0, "First page was not loaded");
      const firstPageNames = await directoryCards(f.page).locator(".connection-directory-name").allTextContents();
      await f.page.getByRole("button", { name: /下一页|next/i }).click();
      await eventually(() => f.page.getByText(/游标.*失效|cursor.*expired/i).count().then((count) => count > 0),
        "A stale cursor conflict should be visible without exposing its response body");
      assert.deepEqual(
        await directoryCards(f.page).locator(".connection-directory-name").allTextContents(),
        firstPageNames,
        "A cursor failure must preserve the last successful page",
      );
      assert.equal(await f.page.getByRole("button", { name: /重查当前筛选|recheck/i }).count() > 0, true,
        "A cursor conflict needs an explicit recheck action");
      const beforeRecheck = directoryRequests(f.state).length;
      await f.page.getByRole("button", { name: /重查当前筛选|recheck/i }).first().click();
      await eventually(() => directoryRequests(f.state).length > beforeRecheck
        && directoryRequests(f.state).at(-1)?.query.offset === "0"
        && directoryRequests(f.state).at(-1)?.query.cursor === undefined,
      "Explicit recheck must discard the old cursor and return to page one");
      assert.deepEqual(await directoryCards(f.page).locator(".connection-directory-name").allTextContents(), firstPageNames);
      assert.equal(await f.page.locator(".connection-directory-error").count(), 0);
    } finally {
      await f.close();
    }
  });

  await test("malformed directory payload fails closed instead of rendering partial records", async () => {
    const f = await fixture();
    try {
      f.state.directoryResponder = (url) => {
        const malformed = pageFor([docsA], url);
        malformed.records[0].capabilitiesTotal = 0;
        return { body: malformed };
      };
      await f.enterDirectory();
      await eventually(() => f.page.locator(".connection-directory-error").count().then((count) => count > 0),
        "Malformed record totals should be rejected");
      assert.equal(await directoryCards(f.page).count(), 0, "Invalid records must not be partially displayed");
      assert.equal(await f.page.locator(".brand-empty-desc").count(), 0, "Invalid data must not be presented as an empty directory");
      await checkNoCanary(f);
    } finally {
      await f.close();
    }
  });

  for (const viewport of [{ width: 1440, height: 900 }, { width: 390, height: 844 }, { width: 420, height: 480 }]) {
    await test(`directory keyboard, scroll, long-name and overflow ${viewport.width}x${viewport.height}`, async () => {
      const f = await fixture(viewport);
      try {
        await f.enterDirectory();
        await eventually(() => directoryRequests(f.state).length > 0, "Directory load missing");
        const start = f.state.requests.length;
        const search = f.page.getByRole("searchbox").first();
        await search.focus();
        const searchRequestCount = directoryRequests(f.state).length;
        await search.fill("fixture");
        await search.press("Enter");
        await eventually(() => directoryRequests(f.state).length > searchRequestCount
          && directoryRequests(f.state).at(-1)?.query.search === "fixture", "Keyboard search did not submit");
        await f.page.getByText(longRecordName, { exact: true }).waitFor({ state: "visible" });
        const clearRequestCount = directoryRequests(f.state).length;
        await search.fill("");
        await search.press("Enter");
        await eventually(() => directoryRequests(f.state).length > clearRequestCount
          && directoryRequests(f.state).at(-1)?.query.search === undefined, "Clearing search did not restore unfiltered records");
        await f.page.getByText(longRecordName, { exact: true }).waitFor({ state: "visible" });
        const geometry = await f.page.evaluate(() => {
          const content = document.querySelector<HTMLElement>(".app-content")!;
          const cards = Array.from(document.querySelectorAll<HTMLElement>(".connection-directory-card, [data-connection-record], .nexus-directory-record, .nexus-links > .nexus-link-card"));
          return {
            viewportWidth: innerWidth,
            documentWidth: document.documentElement.scrollWidth,
            contentWidth: content.scrollWidth,
            contentClientWidth: content.clientWidth,
            contentCanScroll: content.scrollHeight > content.clientHeight,
            cardOverflow: cards.some((card) => card.scrollWidth > card.clientWidth + 1),
          };
        });
        assert.ok(geometry.documentWidth <= geometry.viewportWidth, `Document horizontal overflow: ${JSON.stringify(geometry)}`);
        assert.ok(geometry.contentWidth <= geometry.contentClientWidth + 1, `Content horizontal overflow: ${JSON.stringify(geometry)}`);
        assert.equal(geometry.cardOverflow, false, `A connection card overflows horizontally: ${JSON.stringify(geometry)}`);
        assert.equal(geometry.contentCanScroll, true, `Directory must expose a reachable content scroll region: ${JSON.stringify(geometry)}`);
        await f.page.locator(".app-content").evaluate((element) => { element.scrollTop = element.scrollHeight; });
        assert.ok(await f.page.locator(".app-content").evaluate((element) => element.scrollTop > 0), "Directory content must be scroll reachable");
        await f.page.locator(".app-content").evaluate((element) => { element.scrollTop = 0; });
        await openRecordDetails(f.page, longRecordName);
        const detail = cardNamed(f.page, longRecordName).locator(".connection-directory-detail");
        await detail.waitFor({ state: "visible" });
        await detail.scrollIntoViewIfNeeded();
        const detailBounds = await detail.boundingBox();
        assert.ok(detailBounds && detailBounds.x >= 0 && detailBounds.width <= viewport.width + 1,
          `Keyboard-opened details must fit viewport width: ${JSON.stringify(detailBounds)}`);
        assertReadOnlySince(f.state, start);
        await checkNoCanary(f);
        await f.screenshot(`connection-directory-${viewport.width}x${viewport.height}`);
      } finally {
        await f.close();
      }
    });
  }

  console.log(`Connection directory UI regressions: ${passed} passed, ${failures.length} failed.`);
  if (failures.length) process.exitCode = 1;
} finally {
  if (browser) await browser.close();
  await server.close();
}
