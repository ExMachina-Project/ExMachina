/** WebUI UI-only defaults. Never switches the server's active target. */
import { useStore } from "zustand";
import { createOperationContext, type OperationContextState, type OperationTarget } from "../../client/src/operationContext";
import { api, serverBase } from "./api";
import { useExm } from "./store";

let storage: Storage | undefined;
try { storage = sessionStorage; } catch { /* Browser storage is optional. */ }
export const operationContext = createOperationContext({ instance: serverBase() || location.origin, storage });
export const useOperationContext = <T,>(selector: (state: OperationContextState) => T): T => useStore(operationContext, selector);

export function suggestBoundTarget(): void {
  const binding = useExm.getState().selectedSession?.groupId;
  if (!binding) return;
  const kind = binding.startsWith("single:") ? "single" : "group";
  const id = kind === "single" ? binding.slice(7) : binding;
  const item = operationContext.getState().directory.find((target) => target.kind === kind && target.id === id);
  if (item) operationContext.getState().suggest(item);
}

export async function refreshOperationDirectory(): Promise<void> {
  const request = operationContext.getState().beginDirectory();
  try {
    const [groups, singles] = await Promise.all([api.groups(), api.listSingles()]);
    if (!Array.isArray(groups.groups) || !Array.isArray(singles.singles)) throw new Error("Invalid object directory");
    const targets: OperationTarget[] = [
      ...singles.singles.map((agent) => ({ kind: "single" as const, id: agent.identifier, name: agent.name })),
      ...groups.groups.map((group) => ({ kind: "group" as const, id: group.id, name: group.name })),
    ];
    operationContext.getState().receiveDirectory(request, targets);
    suggestBoundTarget();
  } catch (error) {
    operationContext.getState().failDirectory(request, String(error));
  }
}
