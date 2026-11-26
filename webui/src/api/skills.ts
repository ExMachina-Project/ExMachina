//! Skill listing and management APIs.

import { req } from "./core";
import type {
  DeletedSkill,
  SkillDef,
  SkillRollbackReceipt,
  SkillVersionsResponse,
} from "./core";
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

  // ---------------- 版本链（capability-parity 6.5）----------------
  // 与 CLI（exm skill versions/rollback）、面板（/skills versions…）共用同一版本事实源。

  /** 版本历史（旧 → 新）。已删除的技能：current=null、deleted=true。 */
  skillVersions: (id: string) => req<SkillVersionsResponse>(`/skills/${id}/versions`),

  /** 已删除（历史保留）的技能，删除时间倒序——「回滚恢复」入口的清单。 */
  deletedSkills: () => req<{ skills: DeletedSkill[] }>("/skills/deleted"),

  /** 回滚到历史版本：以旧内容写新版本行（经同一校验，审计链不断）。 */
  rollbackSkill: (id: string, toVersion: number) =>
    req<SkillRollbackReceipt>(`/skills/${id}/rollback`, {
      method: "POST",
      body: JSON.stringify({ toVersion }),
    }),

  // ---------------- 插件系统（/api/plugins）,
};
