import { req } from "./core";
import { createApprovalsApi } from "../../../client/src/api/approvals";
export const apiApprovals = createApprovalsApi(req);
