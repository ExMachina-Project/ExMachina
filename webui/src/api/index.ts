//! Combines domain APIs into the WebUI's shared `api` object.

import { apiApprovals } from "./approvals";
import { apiAuth } from "./auth";
import { apiChannels } from "./channels";
import { apiConfig } from "./config";
import { apiCron } from "./cron";
import { apiGroups } from "./groups";
import { apiKnowledge } from "./knowledge";
import { apiMemory } from "./memory";
import { apiModels } from "./models";
import { apiNexus } from "./nexus";
export type { ConnectionDirectoryPage, ConnectionDirectoryRecord } from "./nexus";
import { apiPlugins } from "./plugins";
import { apiSessions } from "./sessions";
import { apiSkills } from "./skills";

export * from "./core";
export * from "./identity";
export * from "./limits";
export * from "./workbench";

export const api = {
  ...apiSessions,
  ...apiNexus,
  ...apiAuth,
  ...apiGroups,
  ...apiChannels,
  ...apiConfig,
  ...apiMemory,
  ...apiKnowledge,
  ...apiSkills,
  ...apiPlugins,
  ...apiCron,
  ...apiApprovals,
  ...apiModels,
};
