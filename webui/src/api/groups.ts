import { req } from "./core";
import { createGroupsApi } from "../../../client/src/api/groups";
export const apiGroups = createGroupsApi(req);
