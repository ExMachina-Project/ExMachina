import assert from "node:assert/strict";
import { test } from "node:test";
import { createOperationContext, operationStorageKey, targetAvailability, type OperationTarget } from "../client/src/operationContext";
import { resolveOperationScope } from "../client/src/operationScope";
import { createSessionsApi } from "../client/src/api/sessions";
import type { RequestJson } from "../client/src/api/core";

const agent: OperationTarget = { kind: "single", id: "a", name: "Same name" };
const group: OperationTarget = { kind: "group", id: "a", name: "Same name" };
function storage() {
  const values = new Map<string, string>();
  return { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); } };
}
function ready(store: ReturnType<typeof createOperationContext>, targets = [agent, group]) {
  store.getState().receiveDirectory(store.getState().beginDirectory(), targets);
}

test("operation selection has no execution/network dependencies and restores only after validation", () => {
  const sessionStorage = storage();
  const options = { instance: "https://example.test/api/", storage: sessionStorage };
  const store = createOperationContext(options);
  assert.equal(store.getState().select(agent), false);
  ready(store);
  assert.equal(store.getState().select(agent), true);
  assert.deepEqual(store.getState().target, agent);
  const restored = createOperationContext(options);
  assert.deepEqual(restored.getState().target, agent);
  assert.equal(targetAvailability(restored.getState()), "checking");
  ready(restored);
  assert.equal(targetAvailability(restored.getState()), "available");
  assert.equal(restored.getState().select(group), true);
  assert.equal(restored.getState().target?.kind, "group");
});

test("missing/deleted entries remain invalid, preserving their name without a fallback", () => {
  const store = createOperationContext({ instance: "https://example.test", storage: storage() });
  ready(store);
  store.getState().select(agent);
  ready(store, [group]);
  assert.deepEqual(store.getState().target, agent);
  assert.equal(targetAvailability(store.getState()), "unavailable");
  store.getState().suggest(group);
  assert.deepEqual(store.getState().target, agent);
});

test("connections, projects and tabs do not share mutable selection", () => {
  const tab = storage();
  const first = createOperationContext({ instance: "https://a.test", project: "/one", storage: tab });
  ready(first); first.getState().select(agent);
  for (const options of [{ instance: "https://b.test", project: "/one", storage: tab },
    { instance: "https://a.test", project: "/two", storage: tab },
    { instance: "https://a.test", project: "/one", storage: storage() }]) {
    const next = createOperationContext(options);
    assert.equal(next.getState().target, null);
  }
  const independent = createOperationContext({ instance: "https://a.test", project: "/one", storage: tab });
  ready(independent); independent.getState().select(group);
  assert.deepEqual(first.getState().target, agent, "another store does not live-update this tab");
  assert.equal(operationStorageKey("https://user:password@a.test/api/?key=secret#hash", "/one"), operationStorageKey("https://a.test/api", "/one"));
});

test("stale directory responses and read failures cannot change the selected target", () => {
  const store = createOperationContext({ instance: "https://example.test" });
  ready(store); store.getState().select(agent);
  const old = store.getState().beginDirectory();
  const current = store.getState().beginDirectory();
  store.getState().receiveDirectory(current, [agent, group]);
  store.getState().select(group);
  store.getState().receiveDirectory(old, [agent]);
  store.getState().failDirectory(old, "old failure");
  assert.equal(store.getState().status, "ready");
  assert.deepEqual(store.getState().target, group);
  const failed = store.getState().beginDirectory();
  store.getState().failDirectory(failed, "unavailable");
  assert.equal(store.getState().select(agent), false);
  assert.deepEqual(store.getState().target, group);
});

test("an explicit none survives initialization and malformed/unavailable storage is safe", () => {
  const tab = storage();
  const options = { instance: "https://example.test", storage: tab };
  const store = createOperationContext(options); ready(store);
  store.getState().select(null); store.getState().suggest(agent);
  assert.equal(store.getState().target, null);
  const restored = createOperationContext(options); ready(restored); restored.getState().suggest(agent);
  assert.equal(restored.getState().target, null);
  for (const badStorage of [{ getItem: () => "{", setItem: () => {} },
    { getItem: () => { throw new Error("denied"); }, setItem: () => { throw new Error("denied"); } }]) {
    const next = createOperationContext({ instance: options.instance, storage: badStorage }); ready(next);
    next.getState().select(agent);
    assert.deepEqual(next.getState().target, agent);
  }
});

test("scope precedence is bound > explicit override > compatible default; it never infers a primary", () => {
  const state = { target: group, directory: [agent, group], status: "ready" as const };
  const cases = [
    { input: { requirement: "target" as const, bound: agent, override: group }, source: "bound", target: agent, issue: null },
    { input: { requirement: "target" as const, override: agent }, source: "override", target: agent, issue: null },
    { input: { requirement: "target" as const }, source: "default", target: group, issue: null },
    { input: { requirement: "agent" as const }, source: "default", target: group, issue: "member-required" },
    { input: { requirement: "group" as const, override: agent }, source: "override", target: agent, issue: "group-required" },
    { input: { requirement: "instance" as const, bound: agent }, source: "instance", target: null, issue: null },
    { input: { requirement: "target" as const, bound: null }, source: "bound", target: null, issue: "none" },
    { input: { requirement: "target" as const, override: null, allowNone: true }, source: "override", target: null, issue: null },
  ];
  for (const entry of cases) {
    const result = resolveOperationScope({ ...entry.input, state });
    assert.equal(result.source, entry.source);
    assert.deepEqual(result.target, entry.target);
    assert.equal(result.issue, entry.issue);
    assert.equal(result.available, entry.issue === null);
  }
});

test("an invalid/stale default cannot fall back, and a directory failure cannot change a fixed resource", () => {
  const state = { target: agent, directory: [group], status: "ready" as const };
  assert.equal(resolveOperationScope({ requirement: "target", state }).issue, "target-unavailable");
  assert.equal(resolveOperationScope({ requirement: "target", state: { ...state, status: "error" } }).issue, "directory-unavailable");
  const fixed = resolveOperationScope({ requirement: "target", bound: agent, state: { ...state, status: "error" } });
  assert.equal(fixed.source, "bound");
  assert.deepEqual(fixed.target, agent);
  assert.equal(fixed.available, true, "Stopping/inspecting a bound resource must not depend on a UI default directory");
});

test("explicit-target session APIs refuse old gateways before querying or writing", async () => {
  const requests: string[] = [];
  const req: RequestJson = async <T,>(path: string) => { requests.push(path); return { features: ["session-workspace-v1"] } as T; };
  const api = createSessionsApi({ req, serverBase: () => "", authHeaders: () => ({}) }, (key) => key);
  await assert.rejects(api.createSessionForTarget("new", "single:a"), /operation.unsupported/);
  await assert.rejects(api.listSessionsForTarget("single:a"), /operation.unsupported/);
  assert.deepEqual(requests, ["/health", "/health"]);
});

test("explicit-target APIs carry the project and validate returned binding instead of trusting global active", async () => {
  const calls: { path: string; body?: unknown }[] = [];
  let returnedTarget = "single:a";
  let returnedWorkspace = "/project";
  const req: RequestJson = async <T,>(path: string, init?: RequestInit) => {
    calls.push({ path, body: init?.body ? JSON.parse(String(init.body)) : undefined });
    if (path === "/health") return { features: ["session-target-v1"] } as T;
    const session = { id: "fixed", groupId: returnedTarget, workspace: returnedWorkspace };
    return (init?.method === "POST" ? session : [session]) as T;
  };
  const api = createSessionsApi({ req, serverBase: () => "", authHeaders: () => ({}), workspace: "/project" }, (key) => key);
  assert.equal((await api.createSessionForTarget("new", "single:a")).id, "fixed");
  assert.deepEqual(calls.at(-1)?.body, { title: "new", target: "single:a", workspace: "/project" });
  await api.listSessionsForTarget("single:a");
  assert.equal(calls.at(-1)?.path, "/sessions?target=single%3Aa&workspace=%2Fproject");
  returnedTarget = "other";
  await assert.rejects(api.listSessionsForTarget("single:a"), /operation.scopeMismatch/);
  returnedTarget = "single:a"; returnedWorkspace = "/other-project";
  await assert.rejects(api.createSessionForTarget("new", "single:a"), /operation.scopeMismatch/);
  assert.equal(calls.some(({ path }) => path === "/target"), false);
});
