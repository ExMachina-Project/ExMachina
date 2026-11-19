import type { ExmStore } from "../../client/src/store";

function currentStore(): ExmStore {
  const id = window.__EXM_DESKTOP_ACTIVE_PROJECT__;
  const store = window.__EXM_DESKTOP_TEST_STORES__?.find((entry) => entry.projectId === id)?.store;
  if (!store) throw new Error("Desktop project store is not ready");
  return store;
}

/** Development-only test adapter; not imported by the desktop application bundle. */
export const useExm = Object.assign(
  (() => currentStore()()) as ExmStore,
  {
    getState: () => currentStore().getState(),
    setState: (...args: Parameters<ExmStore["setState"]>) => currentStore().setState(...args),
  },
);
