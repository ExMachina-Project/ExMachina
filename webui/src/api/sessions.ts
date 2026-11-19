import { req, serverBase, authHeaders } from "./core";
import { createSessionsApi } from "../../../client/src/api/sessions";
import { tr } from "../i18n/core";
export const apiSessions = createSessionsApi({ req, serverBase, authHeaders }, tr);
