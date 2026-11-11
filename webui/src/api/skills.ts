//! api/skills.ts —— 技能域：技能包清单与创建删除（codebase-refactor 7.1 拆分）。纯搬移。

import { req } from "./core";
import type { SkillDef } from "./core";
export const apiSkills = {
  skills: () => req<SkillDef[]>("/skills"),

  createSkill: (body: {
    id: string;
    name: string;
    description?: string;
    triggers?: string[];
    instructions: string;
    agents?: string[];
  }) => req<SkillDef>("/skills", { method: "POST", body: JSON.stringify(body) }),

  deleteSkill: (id: string) => req<{ ok: boolean }>(`/skills/${id}`, { method: "DELETE" }),

  // ---------------- 插件系统（/api/plugins）,
};
