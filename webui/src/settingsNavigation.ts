import { useEffect, useId } from "react";
import { Modal } from "antd";
import { tr } from "./i18n/core";

const pending = new Set<string>();

/** Guard shared WebUI navigation and the desktop settings window. Drafts stay in memory. */
export function useUnsavedChanges(dirty: boolean) {
  const id = useId();
  useEffect(() => {
    if (!dirty) return;
    pending.add(id);
    const unload = (event: BeforeUnloadEvent) => { event.preventDefault(); event.returnValue = ""; };
    window.addEventListener("beforeunload", unload);
    return () => { pending.delete(id); window.removeEventListener("beforeunload", unload); };
  }, [dirty, id]);
}

export function canLeaveSettings(): boolean {
  return !pending.size || window.confirm(tr("ux.discardBody"));
}

export function confirmDiscard(dirty: boolean, action: () => void) {
  if (!dirty) { action(); return; }
  Modal.confirm({
    title: tr("ux.discardTitle"), content: tr("ux.discardBody"),
    okText: tr("ux.discard"), cancelText: tr("ux.keepEditing"), onOk: action,
  });
}
