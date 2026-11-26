//! core/skills.rs —— Core 技能域（codebase-refactor 拆分）：技能清单/新增/
//! SKILL.md 导入导出/使用统计/删除。
//!
//! capability-parity 6.5/6.6：REST、CLI 与学习候选转正的**内容写入全部走同一版本链**
//! （`write_skill_versioned`：同一套校验 + 旧版本归档 + 版本号单调递增）。没有旁路——
//! 旧的非版本化 `write_skill` 不再被 Core 暴露，否则「三入口同一版本事实源」不成立。

use super::super::Core;
use crate::types::SkillDef;

impl Core {
    pub fn skills(&self) -> anyhow::Result<Vec<SkillDef>> {
        self.registry.load_skills()
    }

    /// 新增/更新技能（REST `POST /api/skills`、CLI `skill add`）。版本化写入：
    /// 新技能 v1，既有技能 v+1 且旧版本归档；校验不过则原技能不变。
    pub fn add_skill(&self, skill: SkillDef) -> anyhow::Result<SkillDef> {
        self.skill_write_versioned(&skill)
    }

    /// 从 SKILL.md 原文导入技能（REST / CLI 共用；同 id → 新版本，旧版本归档）
    pub fn import_skill_md(&self, raw: &str) -> anyhow::Result<SkillDef> {
        let skill = self.registry.import_skill_md_dry_run(raw)?;
        self.skill_write_versioned(&skill)
    }

    /// 导出技能为 SKILL.md 原文（可分享/迁移）
    pub fn export_skill_md(&self, id: &str) -> anyhow::Result<Option<String>> {
        self.registry.export_skill_md(id)
    }

    /// 技能使用统计回填（派发注入即计一次；节点终态回填成功标记）
    pub fn record_skill_use(&self, id: &str, ok: bool) {
        self.registry.record_skill_use(id, ok);
    }

    /// 删除技能：留痕可恢复——当前版本归档、`SKILL.md` 移除（立即停止注入）、
    /// `.deleted` 标记；事件归治理流。返回是否存在并删除。
    pub fn remove_skill(&self, id: &str) -> anyhow::Result<bool> {
        let removed = self.registry.remove_skill_versioned(id)?;
        if removed {
            self.emit_session_event(
                Self::GOVERNANCE_EVENTS,
                "skill.deleted",
                serde_json::json!({ "id": id }),
            );
        }
        Ok(removed)
    }
}
