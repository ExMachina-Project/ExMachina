/** Browser composition root. Other clients create their own isolated stores. */
import { createExmStore } from "../../client/src/store";
import { api, serverBase, accessKey } from "./api";
import { tr } from "./i18n/core";
import { onConsoleEvent } from "./notifications";
export { loadNotifyPrefs, saveNotifyPrefs } from "./notifications";
export type { NotifyPrefs } from "./notifications";
export type { TimelineItem, LivePart, ExmState } from "../../client/src/store";
export const useExm = createExmStore({ api, serverBase, accessKey, tr, onEvent: onConsoleEvent });
