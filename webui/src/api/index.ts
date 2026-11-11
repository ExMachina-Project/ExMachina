//! api/index.ts —— 桶（codebase-refactor 7.1 拆分）：合并各域对象为单一 `api`，
//! 再导出 core 类型与工具——`import { api } from "../api"` 调用点零改动（D6）。
//! 方法合并为 spread 展开零运行时差异；同名冲突 TS 编译期即报。

import { apiApprovals } from "./approvals";
import { apiAuth } from "./auth";
import { apiChannels } from "./channels";
import { apiConfig } from "./config";
import { apiCron } from "./cron";
import { apiGroups } from "./groups";
import { apiMemory } from "./memory";
import { apiModels } from "./models";
import { apiNexus } from "./nexus";
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
  ...apiSkills,
  ...apiPlugins,
  ...apiCron,
  ...apiApprovals,
  ...apiModels,
};
