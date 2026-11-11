//! core/skills.rs —— Core 技能域（codebase-refactor 拆分）：技能清单/新增/
//! SKILL.md 导入导出/使用统计/删除。纯搬移：方法签名与实现零改动。

use super::super::Core;
use crate::types::SkillDef;

impl Core {
    pub fn skills(&self) -> anyhow::Result<Vec<SkillDef>> {
        self.registry.load_skills()
    }
    pub fn add_skill(&self, skill: SkillDef) -> anyhow::Result<()> {
        self.registry.write_skill(&skill)
    }
    /// 从 SKILL.md 原文导入技能（REST / CLI 共用；同 id 覆盖）
    pub fn import_skill_md(&self, raw: &str) -> anyhow::Result<SkillDef> {
        self.registry.import_skill_md(raw)
    }
    /// 导出技能为 SKILL.md 原文（可分享/迁移）
    pub fn export_skill_md(&self, id: &str) -> anyhow::Result<Option<String>> {
        self.registry.export_skill_md(id)
    }
    /// 技能使用统计回填（派发注入即计一次；节点终态回填成功标记）
    pub fn record_skill_use(&self, id: &str, ok: bool) {
        self.registry.record_skill_use(id, ok);
    }
    pub fn remove_skill(&self, id: &str) -> anyhow::Result<bool> {
        self.registry.remove_skill(id)
    }

}
