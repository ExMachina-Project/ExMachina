/** UI defaults, not an execution target or a source of authorization. */
import { createStore } from "zustand/vanilla";

export type OperationTarget = { kind: "single" | "group"; id: string; name: string };
export type DirectoryState = "idle" | "loading" | "ready" | "error";
export type TargetStorage = Pick<Storage, "getItem" | "setItem">;

export const targetKey = (target: Pick<OperationTarget, "kind" | "id">): string => `${target.kind}:${target.id}`;
export const sameTarget = (a: OperationTarget | null, b: OperationTarget | null): boolean =>
  a === null || b === null ? a === b : targetKey(a) === targetKey(b);

/** The caller supplies the approved project root; URL credentials never enter storage. */
export function operationStorageKey(instance: string, project?: string): string {
  const url = new URL(instance);
  return `exm.operation.v1:${JSON.stringify([url.origin + url.pathname.replace(/\/+$/, ""), project ?? ""])}`;
}

function parseTarget(value: unknown): OperationTarget | null {
  if (!value || typeof value !== "object") return null;
  const target = value as Partial<OperationTarget>;
  if ((target.kind !== "single" && target.kind !== "group") || typeof target.id !== "string"
      || !target.id.trim() || target.id.length > 256 || typeof target.name !== "string") return null;
  return { kind: target.kind, id: target.id, name: target.name.slice(0, 256) };
}

export interface OperationContextState {
  target: OperationTarget | null;
  directory: OperationTarget[];
  status: DirectoryState;
  error: string | null;
  revision: number;
  /** A remembered choice (including explicit none) must not be replaced by initialization. */
  chosen: boolean;
  select: (target: OperationTarget | null) => boolean;
  suggest: (target: OperationTarget) => void;
  beginDirectory: () => number;
  receiveDirectory: (request: number, targets: OperationTarget[]) => void;
  failDirectory: (request: number, error: string) => void;
}

export function targetAvailability(state: Pick<OperationContextState, "target" | "directory" | "status">): "none" | "checking" | "available" | "unavailable" {
  if (!state.target) return "none";
  if (state.status !== "ready") return "checking";
  return state.directory.some((item) => sameTarget(item, state.target)) ? "available" : "unavailable";
}

export function createOperationContext(options: { instance: string; project?: string; storage?: TargetStorage }) {
  const key = operationStorageKey(options.instance, options.project);
  let restored: OperationTarget | null = null;
  let chosen = false;
  try {
    const raw: unknown = JSON.parse(options.storage?.getItem(key) ?? "null");
    if (raw && typeof raw === "object" && (raw as { version?: unknown }).version === 1) {
      const candidate = (raw as { target?: unknown }).target;
      restored = parseTarget(candidate);
      chosen = candidate === null || restored !== null;
    }
  } catch { /* Storage can be unavailable or contain obsolete data. */ }
  let request = 0;
  const persist = (target: OperationTarget | null) => {
    try { options.storage?.setItem(key, JSON.stringify({ version: 1, target })); } catch { /* Session-local state remains usable. */ }
  };
  return createStore<OperationContextState>((set, get) => ({
    target: restored, directory: [], status: "idle", error: null, revision: 0, chosen,
    select: (target) => {
      const state = get();
      // Do not accept an arbitrary ID while the directory is stale or unavailable.
      const canonical = target && state.status === "ready"
        ? state.directory.find((item) => sameTarget(item, target)) : null;
      if (target && !canonical) return false;
      const next = canonical ?? null;
      set({ target: next, chosen: true, revision: state.revision + 1 });
      persist(next);
      return true;
    },
    suggest: (target) => {
      if (get().chosen || get().status !== "ready") return;
      const canonical = get().directory.find((item) => sameTarget(item, target));
      if (canonical) {
        set({ target: canonical, chosen: true, revision: get().revision + 1 });
        persist(canonical);
      }
    },
    beginDirectory: () => {
      set({ status: "loading", error: null });
      return ++request;
    },
    receiveDirectory: (version, targets) => {
      if (version !== request) return;
      const directory = [...new Map(targets.flatMap((item) => {
        const parsed = parseTarget(item);
        return parsed ? [[targetKey(parsed), parsed] as const] : [];
      })).values()];
      const current = get().target;
      const canonical = current && directory.find((item) => sameTarget(item, current));
      set({ directory, status: "ready", error: null, ...(canonical ? { target: canonical } : {}) });
      // Missing entries stay visibly invalid; never fall back to a different Agent.
    },
    failDirectory: (version, error) => {
      if (version === request) set({ status: "error", error });
    },
  }));
}
