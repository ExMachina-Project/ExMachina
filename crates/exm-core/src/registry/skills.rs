//! registry/skills.rs —— 技能域（codebase-refactor 6.1 拆分）：技能包 CRUD/
//! SKILL.md 解析导入导出、使用统计、协议层（protocol/*.md frontmatter 驱动）。
//! 纯搬移：方法签名与实现零改动。

use super::*;

impl LocalRegistry {
    // ---------------- 技能包（激活组，docs/10） ----------------

    /// 技能全局目录：`entities/skills/`，全体组共用（技能是动态加载的作业指令，不做组隔离）。
    /// 历史路径是项目根 `skills/`（重组遗留）：存在旧目录且无新目录时自动迁移，
    /// 旧目录与新目录并存时新目录优先（旧目录只读兼容）。
    pub(super) fn skills_dir(&self) -> Option<PathBuf> {
        let parent = self
            .dir
            .parent()
            .unwrap_or(self.dir.as_path())
            .to_path_buf();
        let new = parent.join("entities").join("skills");
        let old = parent.join("skills");
        if new.is_dir() {
            return Some(new);
        }
        if old.is_dir() {
            // 旧目录存量迁移：entities/skills 建不出来或写失败时退回旧目录（只读兼容）
            if std::fs::create_dir_all(&new).is_ok() {
                let moved = std::fs::read_dir(&old)
                    .map(|rd| {
                        rd.filter_map(|e| e.ok())
                            .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
                            .map(|e| e.path())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let mut ok = true;
                for p in moved {
                    let dst = new.join(p.file_name().unwrap_or_default());
                    if std::fs::rename(&p, &dst).is_err() {
                        ok = false;
                    }
                }
                if ok {
                    return Some(new);
                }
            }
            return Some(old);
        }
        Some(new)
    }

    /// 装载全局技能包（SKILL.md 优先 + 旧 `*.json` 兼容读，热装载，全体组共用）。
    /// 同 id 同时存在 SKILL.md 与 json 时 SKILL.md 优先（json 视为待迁移旧档）。
    pub fn load_skills(&self) -> anyhow::Result<Vec<crate::types::SkillDef>> {
        self.load_skills_from(self.skills_dir())
    }

    /// 命令目录查询不得触发旧技能目录迁移，也不更新使用统计。
    pub fn load_skills_readonly(&self) -> anyhow::Result<Vec<crate::types::SkillDef>> {
        let root = self.dir.parent().unwrap_or(&self.dir);
        let current = root.join("entities/skills");
        let legacy = root.join("skills");
        self.load_skills_from(Some(if current.is_dir() || !legacy.is_dir() {
            current
        } else {
            legacy
        }))
    }

    fn load_skills_from(
        &self,
        dir: Option<PathBuf>,
    ) -> anyhow::Result<Vec<crate::types::SkillDef>> {
        let Some(dir) = dir else {
            return Ok(vec![]);
        };
        if !dir.exists() {
            return Ok(vec![]);
        }
        // 两类来源：skills/<id>/SKILL.md（新标准）与 skills/<id>.json（旧格式兼容）
        let mut md: Vec<PathBuf> = Vec::new();
        let mut json: Vec<PathBuf> = Vec::new();
        for p in std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
        {
            if p.extension().map(|x| x == "json").unwrap_or(false) {
                json.push(p);
            } else if p.is_dir() && p.join("SKILL.md").exists() {
                md.push(p.join("SKILL.md"));
            }
        }
        md.sort();
        json.sort();
        let mut out: Vec<crate::types::SkillDef> = Vec::new();
        for f in md {
            let raw = std::fs::read_to_string(&f)?;
            match parse_skill_md(&raw) {
                Ok(Some(s)) => out.push(s),
                Ok(None) => anyhow::bail!("技能包缺少 frontmatter: {}", f.display()),
                Err(e) => anyhow::bail!("技能包校验失败: {}（{e}）", f.display()),
            }
        }
        let md_ids: std::collections::HashSet<String> = out.iter().map(|s| s.id.clone()).collect();
        for f in json {
            let raw = std::fs::read_to_string(&f)?;
            let s: crate::types::SkillDef = serde_json::from_str(&raw)
                .with_context(|| format!("技能包校验失败: {}", f.display()))?;
            if !md_ids.contains(&s.id) {
                out.push(s); // 同 id 的 SKILL.md 已收，旧 json 跳过
            }
        }
        // 运行期统计（侧车聚合；SKILL.md 保持纯内容，统计不污染可分享的技能文件）
        let usage = Self::skill_usage_map_at(&dir.join(".usage.jsonl"));
        for s in out.iter_mut() {
            if s.source.trim().is_empty() {
                s.source = "manual".into();
            }
            if let Some((count, success, last)) = usage.get(&s.id) {
                s.use_count = *count;
                s.success_count = *success;
                s.last_used_at = last.clone();
            }
        }
        Ok(out)
    }

    fn skill_usage_path(&self) -> Option<PathBuf> {
        self.skills_dir().map(|d| d.join(".usage.jsonl"))
    }

    /// 聚合使用统计：id → (使用次数, 成功次数, 最近使用时间)。
    /// 侧车按追加写，读时按行聚合——避免每次使用都重写 SKILL.md（手工编辑零冲突）。
    fn skill_usage_map(&self) -> std::collections::HashMap<String, (u64, u64, String)> {
        let Some(path) = self.skill_usage_path() else {
            return Default::default();
        };
        Self::skill_usage_map_at(&path)
    }

    fn skill_usage_map_at(path: &Path) -> std::collections::HashMap<String, (u64, u64, String)> {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Default::default();
        };
        let mut map: std::collections::HashMap<String, (u64, u64, String)> = Default::default();
        for line in raw.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let Some(id) = v.get("id").and_then(|x| x.as_str()) else {
                continue;
            };
            let ok = v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false);
            // 兼容两种行：逐次事件（无 count）与压缩后的聚合行（count/success）
            let count = v.get("count").and_then(|x| x.as_u64()).unwrap_or(1);
            let success = v
                .get("success")
                .and_then(|x| x.as_u64())
                .unwrap_or_else(|| u64::from(ok));
            let at = v
                .get("at")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string();
            let e = map.entry(id.to_string()).or_insert((0, 0, String::new()));
            e.0 += count;
            e.1 += success;
            if !at.is_empty() {
                e.2 = at;
            }
        }
        map
    }

    /// 记录一次技能使用（注入派发即计一次；节点完成/受阻回填成功标记）。
    /// 写失败静默（统计是增强面，不阻塞任务主链）。
    pub fn record_skill_use(&self, id: &str, ok: bool) {
        let Some(path) = self.skill_usage_path() else {
            return;
        };
        let line = serde_json::json!({
            "id": id,
            "at": crate::types::now_iso(),
            "ok": ok,
        });
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            use std::io::Write;
            let _ = writeln!(f, "{line}");
        }
        // 侧车封顶：超过 256KB 即聚合为「每技能一行」，避免长期运行无限增长
        if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > 256 * 1024 {
            self.compact_skill_usage(&path);
        }
    }

    /// 把逐次事件压缩为每技能一行（count/success/at），统计口径不变、文件有界。
    fn compact_skill_usage(&self, path: &Path) {
        let map = self.skill_usage_map();
        let mut ids: Vec<&String> = map.keys().collect();
        ids.sort();
        let body = ids
            .into_iter()
            .map(|id| {
                let (count, success, last) = &map[id];
                serde_json::json!({
                    "id": id,
                    "count": count,
                    "success": success,
                    "at": last,
                })
                .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let _ = std::fs::write(path, format!("{body}\n"));
    }

    /// 删除某技能的使用统计（技能删除时一并清理，避免同名重建继承旧统计）
    pub(super) fn remove_skill_usage(&self, id: &str) {
        let Some(path) = self.skill_usage_path() else {
            return;
        };
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return;
        };
        let keep: Vec<&str> = raw
            .lines()
            .filter(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .ok()
                    .and_then(|v| v.get("id").and_then(|x| x.as_str()).map(|s| s.to_string()))
                    .as_deref()
                    != Some(id)
            })
            .collect();
        let _ = std::fs::write(path, keep.join("\n") + "\n");
    }

    /// 导出单个技能为 SKILL.md 原文（可分享/迁移；不含运行期统计）
    pub fn export_skill_md(&self, id: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .load_skills()?
            .into_iter()
            .find(|s| s.id == id)
            .map(|s| render_skill_md(&s)))
    }

    /// 只解析不落盘（模型侧 import 入候选前的结构校验）。
    pub fn import_skill_md_dry_run(&self, raw: &str) -> anyhow::Result<crate::types::SkillDef> {
        let mut skill = parse_skill_md(raw)
            .map_err(|e| anyhow::anyhow!("SKILL.md 解析失败：{e}"))?
            .ok_or_else(|| anyhow::anyhow!("缺少 YAML frontmatter（需以 --- 开头）"))?;
        if skill.source.trim().is_empty() {
            skill.source = "import".into();
        }
        if skill.created_at.trim().is_empty() {
            skill.created_at = crate::types::now_iso();
        }
        Ok(skill)
    }

    /// 从 SKILL.md 原文导入技能（标准 frontmatter + Markdown 正文）。
    /// 来源标记 import；同 id 覆盖（与 write_skill 一致）。
    pub fn import_skill_md(&self, raw: &str) -> anyhow::Result<crate::types::SkillDef> {
        let mut skill = parse_skill_md(raw)
            .map_err(|e| anyhow::anyhow!("SKILL.md 解析失败：{e}"))?
            .ok_or_else(|| anyhow::anyhow!("缺少 YAML frontmatter（需以 --- 开头）"))?;
        if skill.source.trim().is_empty() {
            skill.source = "import".into();
        }
        if skill.created_at.trim().is_empty() {
            skill.created_at = crate::types::now_iso();
        }
        self.write_skill(&skill)?;
        Ok(skill)
    }

    /// 写入技能包到全局目录（SKILL.md 格式：frontmatter + 正文=instructions）
    pub fn write_skill(&self, skill: &crate::types::SkillDef) -> anyhow::Result<()> {
        let Some(dir) = self.skills_dir() else {
            anyhow::bail!("无激活组")
        };
        let id = skill.id.trim();
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            anyhow::bail!("非法技能 id: {id}（仅允许字母/数字/-/_）");
        }
        if skill.name.trim().is_empty() || skill.instructions.trim().is_empty() {
            anyhow::bail!("name/instructions 不能为空");
        }
        std::fs::create_dir_all(dir.join(id))?;
        let mut skill = skill.clone();
        skill.id = id.to_string();
        if skill.source.trim().is_empty() {
            skill.source = "manual".into();
        }
        std::fs::write(dir.join(id).join("SKILL.md"), render_skill_md(&skill))?;
        // 同 id 旧 json 一并清除（防双源分叉：load 已优先 SKILL.md，旧档留着徒增困惑）
        let legacy = dir.join(format!("{id}.json"));
        if legacy.exists() {
            let _ = std::fs::remove_file(&legacy);
        }
        Ok(())
    }

    // ---------------- 版本化写入（capability-parity 6.5，spec: agent-learning-loop） ----------------

    /// 校验新版本内容（格式/大小/敏感信息/范围）：拒绝时旧版本保持不变，
    /// 不留半成品。全部通过才可进入版本化写入。
    pub fn validate_skill_update(skill: &crate::types::SkillDef) -> Result<(), String> {
        let id = skill.id.trim();
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(format!("非法技能 id: {id}（仅允许字母/数字/-/_）"));
        }
        if skill.name.trim().is_empty() {
            return Err("name 不能为空".into());
        }
        if skill.instructions.trim().is_empty() {
            return Err("instructions 不能为空".into());
        }
        if skill.instructions.chars().count() > 30_000 {
            return Err("instructions 超过 30000 字符上限".into());
        }
        if crate::learning_candidates::is_sensitive_content(&skill.instructions)
            || crate::learning_candidates::is_sensitive_content(&skill.name)
        {
            return Err("内容含敏感信息形态（密钥/令牌/密码）——默认拒绝".into());
        }
        Ok(())
    }

    /// 版本化写入：校验通过后先归档当前版本到 `<id>/.history/v<N>.md`，再写
    /// 新版本（version+1、updatedAt=now）。旧版本在历史目录保持可回滚。
    /// 校验失败返回 Err——原技能不变（受控写入红线）。
    pub fn write_skill_versioned(
        &self,
        skill: &crate::types::SkillDef,
    ) -> anyhow::Result<crate::types::SkillDef> {
        Self::validate_skill_update(skill).map_err(|e| anyhow::anyhow!("技能更新被拒：{e}"))?;
        let Some(dir) = self.skills_dir() else {
            anyhow::bail!("无激活组");
        };
        let id = skill.id.trim().to_string();
        let current = self.load_skills()?.into_iter().find(|s| s.id == id);
        let mut next = skill.clone();
        next.id = id;
        if next.source.trim().is_empty() {
            next.source = current
                .as_ref()
                .map(|s| s.source.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "manual".into());
        }
        next.created_at = current
            .as_ref()
            .map(|s| s.created_at.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(crate::types::now_iso);
        next.version = current.as_ref().map(|s| s.version).unwrap_or(0) + 1;
        next.updated_at = crate::types::now_iso();
        // 恢复被删除的技能：清除删除标记（目录里只剩 .history/ 与标记，current 为 None → v1；
        // 若历史上已有版本，版本号接着最大历史版本走，避免 v1.md 被覆盖）。
        std::fs::create_dir_all(dir.join(&next.id).join(".history"))?;
        let _ = std::fs::remove_file(dir.join(&next.id).join(".deleted"));
        if next.version == 1 {
            if let Some(max_hist) = max_history_version(&dir.join(&next.id).join(".history")) {
                next.version = max_hist + 1;
            }
        }
        if let Some(old) = current.as_ref() {
            let hist = dir.join(&next.id).join(".history");
            std::fs::create_dir_all(&hist)?;
            std::fs::write(
                hist.join(format!("v{}.md", old.version)),
                render_skill_md(old),
            )?;
        }
        std::fs::create_dir_all(dir.join(&next.id))?;
        std::fs::write(dir.join(&next.id).join("SKILL.md"), render_skill_md(&next))?;
        let legacy = dir.join(format!("{}.json", next.id));
        if legacy.exists() {
            let _ = std::fs::remove_file(&legacy);
        }
        Ok(next)
    }

    /// 技能是否处于「已删除但历史保留」状态（有删除标记、无 SKILL.md）。
    pub fn skill_is_deleted(&self, id: &str) -> anyhow::Result<bool> {
        let Some(dir) = self.skills_dir() else {
            anyhow::bail!("无激活组");
        };
        Ok(dir.join(id).join(".deleted").exists() && !dir.join(id).join("SKILL.md").exists())
    }

    /// 已删除（历史保留、可回滚恢复）的技能清单，最近删除的在前。
    /// 删除标记内容 = 删除时间（ISO）；标记缺失/读不出时按空串处理并排在后面。
    /// 与 [`Self::skill_is_deleted`] 同一判据——治理面据此提供「回滚恢复」入口
    /// （否则技能从列表消失后用户无处可点）。
    pub fn list_deleted_skills(&self) -> anyhow::Result<Vec<DeletedSkill>> {
        let Some(dir) = self.skills_dir() else {
            anyhow::bail!("无激活组");
        };
        let mut out: Vec<DeletedSkill> = Vec::new();
        // 技能目录在写入第一个技能之前并不存在：此时「已删除清单」语义上就是空，
        // 不是错误（全新工作区一打开治理面就 400 是不可接受的）。
        if !dir.is_dir() {
            return Ok(out);
        }
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let skill_dir = entry.path();
            if !skill_dir.join(".deleted").exists() || skill_dir.join("SKILL.md").exists() {
                continue;
            }
            let deleted_at = std::fs::read_to_string(skill_dir.join(".deleted"))
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            out.push(DeletedSkill {
                id: entry.file_name().to_string_lossy().into_owned(),
                deleted_at,
            });
        }
        // id 作平局裁决：顺序确定，不随目录迭代序漂移
        out.sort_by(|a, b| {
            b.deleted_at
                .cmp(&a.deleted_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(out)
    }

    /// 版本历史（旧→新）：`<id>/.history/vN.md` 清单 + 当前版本。
    /// 技能被删除后历史仍可列出（末尾无「当前版本」行）——删除是可审计、可恢复的状态，
    /// 不是抹除。
    pub fn skill_history(&self, id: &str) -> anyhow::Result<Vec<(u64, String)>> {
        Ok(self
            .skill_history_detailed(id)?
            .into_iter()
            .map(|r| (r.version, r.name))
            .collect())
    }

    /// 技能版本历史（旧 → 新），每行带该版本的完整定义。
    /// 归档文件 `v{v}.md` 本就是完整的 SKILL.md——`skill_history` 此前每个版本都
    /// 读取并解析了它，只留下 name。治理面要让用户看得出「这一版写了什么」才谈得上
    /// 选回滚目标，所以这里把解析结果原样交出去（不增加 I/O）。
    /// 末行是当前生效版本（技能已删除时没有末行）。
    pub fn skill_history_detailed(&self, id: &str) -> anyhow::Result<Vec<SkillHistoryRow>> {
        let Some(dir) = self.skills_dir() else {
            anyhow::bail!("无激活组");
        };
        let mut out: Vec<SkillHistoryRow> = Vec::new();
        let hist = dir.join(id).join(".history");
        if hist.is_dir() {
            let mut vers: Vec<u64> = std::fs::read_dir(&hist)?
                .filter_map(|e| e.ok())
                .filter_map(|e| history_version_of(&e.file_name().to_string_lossy()))
                .collect();
            vers.sort_unstable();
            for v in vers {
                let raw =
                    std::fs::read_to_string(hist.join(format!("v{v}.md"))).unwrap_or_default();
                let def = parse_skill_md(&raw).ok().flatten();
                let name = def
                    .as_ref()
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| format!("v{v}"));
                out.push(SkillHistoryRow {
                    version: v,
                    name,
                    description: def
                        .as_ref()
                        .map(|s| s.description.clone())
                        .unwrap_or_default(),
                    instructions: def.map(|s| s.instructions).unwrap_or_default(),
                });
            }
        }
        if let Some(cur) = self.load_skills()?.into_iter().find(|s| s.id == id) {
            out.push(SkillHistoryRow {
                version: cur.version,
                name: cur.name,
                description: cur.description,
                instructions: cur.instructions,
            });
        }
        Ok(out)
    }

    /// 回滚到历史版本：历史版本经同一校验后成为新版本行（version 继续
    /// 单调递增——回滚不是倒退版本号，是「以旧内容写新版本」，审计链不断）。
    pub fn rollback_skill(
        &self,
        id: &str,
        to_version: u64,
    ) -> anyhow::Result<crate::types::SkillDef> {
        let Some(dir) = self.skills_dir() else {
            anyhow::bail!("无激活组");
        };
        let hist_path = dir
            .join(id)
            .join(".history")
            .join(format!("v{to_version}.md"));
        let raw = std::fs::read_to_string(&hist_path)
            .map_err(|_| anyhow::anyhow!("历史版本不存在：{id} v{to_version}"))?;
        let old = parse_skill_md(&raw)
            .map_err(|e| anyhow::anyhow!("历史版本解析失败：{e}"))?
            .ok_or_else(|| anyhow::anyhow!("历史版本缺少 frontmatter"))?;
        self.write_skill_versioned(&old)
    }

    /// 删除技能——留痕可恢复（spec「删除状态在后续会话生效并可审计」）：
    /// 当前版本先归档到 `<id>/.history/v<N>.md`，再移除 `SKILL.md` 与旧 json，
    /// 留 `.deleted` 标记（内容 = 删除时间）。`load_skills` 只认带 `SKILL.md` 的目录，
    /// 所以删除后技能立即停止注入；`.history/` 保留，`rollback_skill` 可恢复任一版本。
    /// 不存在返回 Ok(false)；标记已存在（已删除）也返回 Ok(false)。
    pub fn remove_skill_versioned(&self, id: &str) -> anyhow::Result<bool> {
        let id = id.trim();
        Self::validate_skill_id(id)?;
        let Some(dir) = self.skills_dir() else {
            anyhow::bail!("无激活组");
        };
        let skill_dir = dir.join(id);
        let live = skill_dir.join("SKILL.md").exists();
        let legacy = dir.join(format!("{id}.json"));
        if !live && !legacy.exists() {
            return Ok(false);
        }
        if live {
            let current = self
                .load_skills()?
                .into_iter()
                .find(|s| s.id == id)
                .ok_or_else(|| anyhow::anyhow!("技能校验失败，拒绝删除：{id}"))?;
            let hist = skill_dir.join(".history");
            std::fs::create_dir_all(&hist)?;
            // 已有同名归档（旧实现把 legacy v0 存成 v1.md 的遗留）不覆盖
            let path = hist.join(format!("v{}.md", current.version));
            if !path.exists() {
                std::fs::write(&path, render_skill_md(&current))?;
            }
            std::fs::remove_file(skill_dir.join("SKILL.md"))?;
        } else {
            // 旧格式单文件：原样搬进历史（按 v0 记——legacy 技能本就没有版本号）
            let raw = std::fs::read_to_string(&legacy)?;
            let hist = skill_dir.join(".history");
            std::fs::create_dir_all(&hist)?;
            let path = hist.join("v0.md");
            if !path.exists() {
                std::fs::write(&path, &raw)?;
            }
            std::fs::remove_file(&legacy)?;
        }
        std::fs::create_dir_all(&skill_dir)?;
        std::fs::write(skill_dir.join(".deleted"), crate::types::now_iso())?;
        self.remove_skill_usage(id);
        Ok(true)
    }

    fn validate_skill_id(id: &str) -> anyhow::Result<()> {
        if id.is_empty()
            || id.len() > 64
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            anyhow::bail!("非法技能 id: {id}（仅允许字母/数字/-/_）");
        }
        Ok(())
    }

    // ---------------- 协议层（`<组目录>/protocol/*.md`，frontmatter 驱动按需注入） ----------------

    /// 装载激活组的协议文档（热装载；无 frontmatter 的 .md 跳过，坏 frontmatter 跳过并告警）
    pub fn load_protocols(&self) -> anyhow::Result<Vec<crate::types::ProtocolDef>> {
        self.protocols_in_group(&self.active_group())
    }

    /// 装载指定组的协议文档（目录不存在返回空表；注入点与触发词见文件 frontmatter）
    pub fn protocols_in_group(&self, gid: &str) -> anyhow::Result<Vec<crate::types::ProtocolDef>> {
        let dir = {
            let groups = self.groups.read();
            let Some(g) = groups.get(gid) else {
                return Ok(vec![]);
            };
            g.dir.join("protocol")
        };
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "md").unwrap_or(false))
            .collect();
        files.sort();
        let mut out = Vec::new();
        for f in files {
            let raw = std::fs::read_to_string(&f)?;
            match parse_protocol_doc(&raw) {
                Ok(Some(p)) => out.push(p),
                Ok(None) => {} // 无 frontmatter：普通文档，不参与注入（如 README）
                Err(why) => eprintln!("协议文档跳过（{why}）：{}", f.display()),
            }
        }
        Ok(out)
    }
}

/// 归档文件名 `v<N>.md` → N（其它文件名返回 None）。历史清单与版本号续算共用，
/// 两处口径不会漂移。
fn history_version_of(name: &str) -> Option<u64> {
    name.strip_prefix('v')?.strip_suffix(".md")?.parse().ok()
}

/// `.history/` 里最大的版本号（无历史返回 None）。
fn max_history_version(hist: &Path) -> Option<u64> {
    std::fs::read_dir(hist)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| history_version_of(&e.file_name().to_string_lossy()))
        .max()
}

/// SKILL.md 渲染：frontmatter（内联列表）+ 空行 + Markdown 正文
fn render_skill_md(s: &crate::types::SkillDef) -> String {
    let mut fm = String::from("---\n");
    fm.push_str(&format!("id: {}\n", s.id));
    fm.push_str(&format!("name: {}\n", s.name));
    if !s.description.trim().is_empty() {
        fm.push_str(&format!("description: {}\n", s.description));
    }
    if !s.triggers.is_empty() {
        fm.push_str(&format!("triggers: [{}]\n", s.triggers.join(", ")));
    }
    if !s.agents.is_empty() {
        fm.push_str(&format!("agents: [{}]\n", s.agents.join(", ")));
    }
    if !s.created_at.trim().is_empty() {
        fm.push_str(&format!("createdAt: {}\n", s.created_at));
    }
    if !s.source.trim().is_empty() {
        fm.push_str(&format!("source: {}\n", s.source));
    }
    if s.version > 0 {
        fm.push_str(&format!("version: {}\n", s.version));
    }
    if !s.updated_at.trim().is_empty() {
        fm.push_str(&format!("updatedAt: {}\n", s.updated_at));
    }
    fm.push_str("---\n\n");
    fm.push_str(s.instructions.trim());
    fm.push('\n');
    fm
}

// ---------------------------------------------------------------- 协议文档 frontmatter 解析

/// 解析协议文档：`---` 包裹的 frontmatter（`key: value`；列表用 `[a, b]`）+ 正文。
/// 无 frontmatter 返回 Ok(None)；有 frontmatter 但未闭合或缺 id/name 返回 Err。
fn parse_protocol_doc(raw: &str) -> Result<Option<crate::types::ProtocolDef>, String> {
    let raw = raw.trim_start_matches('\u{feff}');
    let mut lines = raw.lines();
    if lines.next().map(|l| l.trim()) != Some("---") {
        return Ok(None);
    }
    let mut id = String::new();
    let mut name = String::new();
    let mut description = String::new();
    let mut triggers: Vec<String> = Vec::new();
    let mut agents: Vec<String> = Vec::new();
    let mut scope = String::new();
    let mut closed = false;
    for line in &mut lines {
        if line.trim() == "---" {
            closed = true;
            break;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "id" => id = v.to_string(),
            "name" => name = v.to_string(),
            "description" => description = v.to_string(),
            "triggers" => triggers = parse_inline_list(v),
            "agents" => agents = parse_inline_list(v),
            "scope" => {
                scope = match v {
                    "plan" | "unit" => v.to_string(),
                    _ => "both".to_string(),
                }
            }
            _ => {}
        }
    }
    if !closed {
        return Err("frontmatter 未闭合".into());
    }
    if id.trim().is_empty() || name.trim().is_empty() {
        return Err("缺少 id 或 name".into());
    }
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    Ok(Some(crate::types::ProtocolDef {
        id: id.trim().to_string(),
        name: name.trim().to_string(),
        description: description.trim().to_string(),
        triggers,
        scope,
        agents,
        body,
    }))
}

/// 行内列表：`[a, b, c]`（逗号 / 顿号分隔；空 `[]` = 空列表）
fn parse_inline_list(v: &str) -> Vec<String> {
    let inner = v.trim().trim_start_matches('[').trim_end_matches(']');
    inner
        .split([',', '，', '、'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// SKILL.md frontmatter 解析：`---` 包裹的元数据头 + Markdown 正文 = instructions。
/// 行级解析（与 protocol 文档同风格，零 YAML 依赖）；支持 triggers/agents 的
/// 内联列表（`[a, b]`）与逐行 `- item` 两种写法。无 frontmatter 返回 None。
fn parse_skill_md(raw: &str) -> Result<Option<crate::types::SkillDef>, String> {
    let raw = raw.trim_start_matches('\u{feff}');
    let mut lines = raw.lines();
    if lines.next().map(|l| l.trim()) != Some("---") {
        return Ok(None);
    }
    let mut id = String::new();
    let mut name = String::new();
    let mut description = String::new();
    let mut triggers: Vec<String> = Vec::new();
    let mut agents: Vec<String> = Vec::new();
    let mut created_at = String::new();
    let mut source = String::new();
    let mut version: u64 = 0;
    let mut updated_at = String::new();
    let mut closed = false;
    // 逐行列表项的归属：跟随最近声明的列表字段（triggers → agents）
    let mut list_field: u8 = 0; // 0=无 1=triggers 2=agents
    for line in &mut lines {
        if line.trim() == "---" {
            closed = true;
            break;
        }
        if let Some(item) = line.trim().strip_prefix("- ") {
            let item = item.trim().to_string();
            if item.is_empty() {
                continue;
            }
            match list_field {
                2 => agents.push(item),
                1 => triggers.push(item),
                _ => {}
            }
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "id" => id = v.to_string(),
            "name" => name = v.to_string(),
            "description" => description = v.to_string(),
            "triggers" => {
                triggers = parse_inline_list(v);
                list_field = 1;
            }
            "agents" => {
                agents = parse_inline_list(v);
                list_field = 2;
            }
            "createdAt" | "created_at" => created_at = v.to_string(),
            "source" => source = v.to_string(),
            "version" => version = v.parse::<u64>().unwrap_or(0),
            "updatedAt" | "updated_at" => updated_at = v.to_string(),
            _ => {}
        }
    }
    if !closed {
        return Err("frontmatter 未闭合".into());
    }
    if id.trim().is_empty() || name.trim().is_empty() {
        return Err("id/name 不能为空".into());
    }
    let instructions: String = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    if instructions.is_empty() {
        return Err("正文（instructions）不能为空".into());
    }
    Ok(Some(crate::types::SkillDef {
        id,
        name,
        description,
        triggers,
        instructions,
        agents,
        created_at,
        use_count: 0,
        success_count: 0,
        last_used_at: String::new(),
        source,
        version,
        updated_at,
    }))
}

#[cfg(test)]
mod protocol_parse_tests {
    use super::*;

    #[test]
    fn 协议frontmatter解析_元数据与正文分离() {
        let raw = "---\nid: debug\nname: 调试协议\ndescription: 定位\nscope: unit\ntriggers: [调试, 报错]\nagents: [review-agent]\n---\n# 调试协议\n\n正文第一行\n";
        let p = parse_protocol_doc(raw).unwrap().unwrap();
        assert_eq!(p.id, "debug");
        assert_eq!(p.name, "调试协议");
        assert_eq!(p.scope, "unit");
        assert_eq!(p.triggers, vec!["调试", "报错"]);
        assert_eq!(p.agents, vec!["review-agent"]);
        assert!(p.body.starts_with("# 调试协议"));
        assert!(!p.injects_plan() && p.injects_unit());
    }

    #[test]
    fn 无frontmatter跳过_坏frontmatter报错() {
        assert!(parse_protocol_doc("# 普通文档\n无元数据")
            .unwrap()
            .is_none());
        assert!(parse_protocol_doc("---\nname: 缺 id\n---\n正文").is_err());
        assert!(parse_protocol_doc("---\nid: x\nname: y\n正文未闭合").is_err());
    }

    #[test]
    fn 缺省范围both_未知范围归both() {
        let raw = "---\nid: a\nname: A\nscope: 火星\n---\n正文";
        let p = parse_protocol_doc(raw).unwrap().unwrap();
        assert_eq!(p.scope, "both");
        assert!(p.injects_plan() && p.injects_unit());
    }
}

#[cfg(test)]
mod skills_dir_tests {
    use super::*;

    /// 生产同构布局：<workspace>/entities 作为 agents_dir（config.rs 口径）
    fn mk_registry(tag: &str) -> (LocalRegistry, std::path::PathBuf) {
        let root =
            std::env::temp_dir().join(format!("exm-skills-{tag}-{}", crate::types::new_id()));
        let reg = LocalRegistry::new(root.join("entities")).unwrap();
        (reg, root)
    }

    fn mk_skill(dir: &std::path::Path, id: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let s = serde_json::json!({
            "id": id, "name": id, "description": "测试技能",
            "triggers": [id], "instructions": "测试指令"
        });
        std::fs::write(dir.join(format!("{id}.json")), s.to_string()).unwrap();
    }

    /// 技能目录修复回归：读取路径必须是 entities/skills（技能实体的真实存放地；
    /// 此前解析到项目根 skills/——重组遗留路径，容器内恒为空导致技能面全空）
    #[test]
    fn 技能目录_新路径优先() {
        let (reg, root) = mk_registry("new");
        let dir = reg.skills_dir().unwrap();
        assert_eq!(
            dir,
            root.join("entities").join("skills"),
            "技能目录应解析到 entities/skills"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 旧目录存量自动迁移：老部署技能在根 skills/ → 迁到 entities/skills 后可读
    #[test]
    fn 技能目录_旧目录迁移() {
        let (reg, root) = mk_registry("mig");
        mk_skill(&root.join("skills"), "legacy");
        // skills_dir 解析即迁移：legacy 应已搬到新目录
        let dir = reg.skills_dir().unwrap();
        assert!(dir.join("legacy.json").exists(), "旧技能应已迁移到新目录");
        let list = reg.load_skills().unwrap();
        assert!(
            list.iter().any(|s| s.id == "legacy"),
            "迁移后可装载：{list:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 新旧并存：新目录优先，旧目录不再吞写
    #[test]
    fn 技能目录_并存时新目录优先() {
        let (reg, root) = mk_registry("both");
        mk_skill(&root.join("entities").join("skills"), "fresh");
        mk_skill(&root.join("skills"), "stale");
        let list = reg.load_skills().unwrap();
        assert!(list.iter().any(|s| s.id == "fresh"));
        assert!(!list.iter().any(|s| s.id == "stale"), "并存时只读新目录");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// SKILL.md 往返：write 落 SKILL.md 格式，load 原样读回（通用标准格式的主链路）
    #[test]
    fn 技能_skillmd往返() {
        let (reg, root) = mk_registry("md");
        let s = crate::types::SkillDef {
            id: "rt-skill".into(),
            name: "往返技能".into(),
            description: "验证 SKILL.md 读写".into(),
            triggers: vec!["往返".into(), "roundtrip".into()],
            instructions: "1) 约束一\n2) 约束二".into(),
            agents: vec![],
            created_at: "2026-10-31T00:00:00.000Z".into(),
            ..Default::default()
        };
        reg.write_skill(&s).unwrap();
        let dir = reg.skills_dir().unwrap();
        assert!(
            dir.join("rt-skill/SKILL.md").exists(),
            "写入应为 SKILL.md 格式"
        );
        let list = reg.load_skills().unwrap();
        let got = list.iter().find(|x| x.id == "rt-skill").expect("应可读回");
        assert_eq!(got.name, "往返技能");
        assert_eq!(
            got.triggers,
            vec!["往返".to_string(), "roundtrip".to_string()]
        );
        assert!(got.instructions.contains("约束一"));
        assert!(got.agents.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 同 id 双源：SKILL.md 优先，旧 json 不分叉；write 后旧 json 被清
    #[test]
    fn 技能_同id双源skillmd优先() {
        let (reg, root) = mk_registry("dup");
        let skills = root.join("entities").join("skills");
        // 旧 json 版本
        mk_skill(&skills, "dual");
        // SKILL.md 版本（内容不同）
        std::fs::create_dir_all(skills.join("dual")).unwrap();
        std::fs::write(
            skills.join("dual/SKILL.md"),
            "---\nid: dual\nname: MD版\n---\n\nMD 正文\n",
        )
        .unwrap();
        let list = reg.load_skills().unwrap();
        let got = list.iter().find(|x| x.id == "dual").unwrap();
        assert_eq!(got.name, "MD版", "SKILL.md 应优先于旧 json");
        // write 清旧 json
        reg.write_skill(&crate::types::SkillDef {
            id: "dual".into(),
            name: "新版".into(),
            description: String::new(),
            triggers: vec![],
            instructions: "新正文".into(),
            agents: vec![],
            created_at: String::new(),
            ..Default::default()
        })
        .unwrap();
        assert!(
            !skills.join("dual.json").exists(),
            "write 应清除同 id 旧 json"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// SKILL.md 解析：逐行列表项写法 + frontmatter 异常路径
    #[test]
    fn 技能_skillmd解析_逐行列表与异常() {
        // 逐行列表
        let md = "---\nid: line-list\nname: 逐行\ntriggers:\n  - 甲\n  - 乙\nagents:\n  - machina\n---\n\n正文\n";
        let s = parse_skill_md(md).unwrap().unwrap();
        assert_eq!(s.triggers, vec!["甲".to_string(), "乙".to_string()]);
        assert_eq!(s.agents, vec!["machina".to_string()]);
        // 无 frontmatter
        assert!(parse_skill_md("没有头").unwrap().is_none());
        // 未闭合
        assert!(parse_skill_md("---\nid: x\nname: y\n").is_err());
        // 缺正文
        assert!(parse_skill_md("---\nid: x\nname: y\n---\n").is_err());
    }

    /// 技能使用统计侧车 + 导入导出往返：统计不污染 SKILL.md，删除时一并清理
    #[test]
    fn 技能_使用统计与导入导出() {
        let (reg, root) = mk_registry("usage");
        let md = "---\nid: use-me\nname: 统计往返\ntriggers: [统计]\n---\n\n按步骤执行\n";
        let s = reg.import_skill_md(md).unwrap();
        assert_eq!(s.id, "use-me");
        assert_eq!(s.source, "import");
        reg.record_skill_use("use-me", true);
        reg.record_skill_use("use-me", false);
        let got = reg
            .load_skills()
            .unwrap()
            .into_iter()
            .find(|x| x.id == "use-me")
            .expect("导入后可读回");
        assert_eq!(got.use_count, 2);
        assert_eq!(got.success_count, 1);
        assert!(!got.last_used_at.is_empty());
        let exported = reg.export_skill_md("use-me").unwrap().unwrap();
        assert!(exported.contains("id: use-me"));
        assert!(exported.contains("按步骤执行"));
        assert!(!exported.contains("useCount"), "统计不应写入 SKILL.md");
        assert!(reg.remove_skill("use-me").unwrap());
        // 路径穿越防护：非法 id 直接拒绝（删除路径由 id 拼接）
        assert!(reg.remove_skill("../../evil").is_err());
        // 同名重建：旧统计已被清理，不继承
        reg.import_skill_md(md).unwrap();
        let again = reg
            .load_skills()
            .unwrap()
            .into_iter()
            .find(|x| x.id == "use-me")
            .unwrap();
        assert_eq!(again.use_count, 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 会话归属路由（切换人设无效的回归锚点）：轮次作用域（task-local）按
    /// 会话 group_id（single:<id> / 组名）对齐交互上下文；全局激活态零污染——
    /// 并发轮次 / REST / 通道绑定读到的始终是各自显式设置的上下文。
    #[tokio::test]
    async fn 交互上下文_轮次作用域与全局零污染() {
        let dir = std::env::temp_dir().join(format!("exm-ctx-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = crate::config::ExmConfig::load(&dir);
        cfg.use_mock = true;
        let core = crate::Core::with_config(cfg).expect("创建 Core 失败");
        let reg = core.registry();

        // 造两个组 + 一个单智能体
        reg.create_group(Some("g1".into()), "组一", "").unwrap();
        reg.create_group(Some("g2".into()), "组二", "").unwrap();
        reg.set_active_group("g1").unwrap();
        reg.upsert_single(
            crate::types::AgentDefinition {
                name: "单体".into(),
                identifier: "solo-x".into(),
                domain: "测试".into(),
                tier: crate::types::Tier::Orchestrator,
                description: "测试单智能体".into(),
                capabilities: vec![],
                tools: vec![],
                when_to_call: String::new(),
                link: None,
                max_steps: None,
                dependencies: vec![],
                composable_with: vec![],
                input_schema: Default::default(),
                output_schema: Default::default(),
                prompt_file: String::new(),
                model_hint: None,
                model_routing: None,
            },
            None,
        )
        .unwrap();

        // 全局态：g1 组模式
        assert_eq!(reg.active_group(), "g1");
        assert!(reg.active_single().is_none());

        // 轮次作用域内：对齐到单智能体会话（single:solo-x）→ 单智能体模式生效
        round_scope_decl::ROUND_SCOPE
            .scope(Some(("g1".into(), Some("solo-x".into()))), async {
                assert_eq!(reg.active_single().as_deref(), Some("solo-x"));
                assert!(reg.single_mode());
                assert_eq!(reg.active_scope(), "single:solo-x");
            })
            .await;

        // 作用域外：全局态未被污染（仍 g1 组模式）
        assert_eq!(reg.active_group(), "g1");
        assert!(reg.active_single().is_none());

        // 轮次作用域内：对齐到组会话（g2）→ 组模式生效
        round_scope_decl::ROUND_SCOPE
            .scope(Some(("g2".into(), None)), async {
                assert_eq!(reg.active_group(), "g2");
                assert!(!reg.single_mode());
            })
            .await;

        // 作用域外：仍是 g1
        assert_eq!(reg.active_group(), "g1");

        // 无作用域（不在轮次内）：回落全局激活态
        // （已在上面断言）

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// capability-parity 6.5 版本化写入：格式错误/敏感内容拒绝时旧版本不变；
/// 成功更新归档旧版本；回滚以旧内容写新版本（版本号单调递增）。
#[cfg(test)]
mod versioned_write_tests {
    use super::super::*;
    use crate::types::SkillDef;

    fn mk_registry(tag: &str) -> (LocalRegistry, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("exm-skv-{tag}-{}", crate::types::new_id()));
        let reg = LocalRegistry::new(root.join("entities")).unwrap();
        (reg, root)
    }

    fn skill(id: &str, instructions: &str) -> SkillDef {
        SkillDef {
            id: id.into(),
            name: format!("技能-{id}"),
            description: String::new(),
            triggers: vec!["触发".into()],
            instructions: instructions.into(),
            agents: vec![],
            created_at: String::new(),
            use_count: 0,
            success_count: 0,
            last_used_at: String::new(),
            source: String::new(),
            version: 0,
            updated_at: String::new(),
        }
    }

    #[test]
    fn 版本链_创建更新回滚_单调递增() {
        let (reg, _root) = mk_registry("chain");
        // 创建 = v1
        let v1 = reg
            .write_skill_versioned(&skill("deploy", "第一版指令"))
            .unwrap();
        assert_eq!(v1.version, 1);
        // 更新 = v2（v1 归档）
        let v2 = reg
            .write_skill_versioned(&skill("deploy", "第二版指令（金丝雀）"))
            .unwrap();
        assert_eq!(v2.version, 2);
        let hist = reg.skill_history("deploy").unwrap();
        assert_eq!(hist.len(), 2, "历史+当前：{hist:?}");
        // 回滚到 v1：内容恢复、版本号继续递增（v3）
        let v3 = reg.rollback_skill("deploy", 1).unwrap();
        assert_eq!(v3.version, 3);
        let current = reg
            .load_skills()
            .unwrap()
            .into_iter()
            .find(|s| s.id == "deploy")
            .unwrap();
        assert!(
            current.instructions.contains("第一版指令"),
            "回滚后内容恢复旧版"
        );
        assert_eq!(current.version, 3);
    }

    #[test]
    fn 格式错误与敏感内容_旧版本不变() {
        let (reg, _root) = mk_registry("reject");
        let v1 = reg
            .write_skill_versioned(&skill("safe", "正常指令"))
            .unwrap();
        assert_eq!(v1.version, 1);
        // 空 instructions：拒
        let err = reg.write_skill_versioned(&skill("safe", "  ")).unwrap_err();
        assert!(err.to_string().contains("被拒"), "{err}");
        // 敏感内容：拒
        let err = reg
            .write_skill_versioned(&skill("safe", "用 password=hunter2 直连"))
            .unwrap_err();
        assert!(err.to_string().contains("敏感"), "{err}");
        // 非法 id：拒
        let err = reg
            .write_skill_versioned(&skill("bad id!", "内容"))
            .unwrap_err();
        assert!(err.to_string().contains("id"), "{err}");
        // 旧版本原样：v1 内容未动、无 v2
        let current = reg
            .load_skills()
            .unwrap()
            .into_iter()
            .find(|s| s.id == "safe")
            .unwrap();
        assert_eq!(current.version, 1);
        assert!(current.instructions.contains("正常指令"));
        assert!(
            reg.skill_history("safe").unwrap().len() == 1,
            "无半成品历史"
        );
    }

    #[test]
    fn 回滚不存在版本_具体拒绝原因() {
        let (reg, _root) = mk_registry("norollback");
        reg.write_skill_versioned(&skill("x", "内容")).unwrap();
        let err = reg.rollback_skill("x", 9).unwrap_err();
        assert!(err.to_string().contains("不存在"), "{err}");
    }

    /// legacy 技能（无 `version` frontmatter → 读作 0）连续更新两次：legacy **原件**必须
    /// 仍可回滚到。旧实现把它归档成 `v1.md`，第二次更新时归档「当前 v1」又写 `v1.md`，
    /// 原件被覆盖丢失。
    #[test]
    fn legacy技能_v0原件_连续更新不丢失() {
        let (reg, root) = mk_registry("legacy");
        let dir = reg.skills_dir().unwrap();
        // 直接摆一个 legacy 技能（无 version / updatedAt frontmatter）——不经过版本化写入
        std::fs::create_dir_all(dir.join("old")).unwrap();
        std::fs::write(
            dir.join("old/SKILL.md"),
            "---\nid: old\nname: 旧技能\ntriggers: [触发]\n---\nlegacy 原件指令\n",
        )
        .unwrap();
        assert!(root.join("entities").exists());
        let before = reg.load_skills().unwrap().remove(0);
        assert_eq!(before.version, 0, "前置：legacy 技能读作 v0");

        let v1 = reg
            .write_skill_versioned(&skill("old", "第一次更新"))
            .unwrap();
        assert_eq!(v1.version, 1);
        let v2 = reg
            .write_skill_versioned(&skill("old", "第二次更新"))
            .unwrap();
        assert_eq!(v2.version, 2);

        let hist = dir.join("old/.history");
        assert!(
            std::fs::read_to_string(hist.join("v0.md"))
                .unwrap()
                .contains("legacy 原件指令"),
            "legacy 原件应归档为 v0.md 且内容不变"
        );
        assert!(
            std::fs::read_to_string(hist.join("v1.md"))
                .unwrap()
                .contains("第一次更新"),
            "v1 应完整保留"
        );
        // 历史清单把 v0 如实列出，且 v0 可回滚
        let vers: Vec<u64> = reg
            .skill_history("old")
            .unwrap()
            .into_iter()
            .map(|(v, _)| v)
            .collect();
        assert_eq!(vers, vec![0, 1, 2]);
        // 详细版：每行带该版本写时的完整指令——治理面据此看出「这一版写了什么」。
        // 归档文件本就是完整 SKILL.md，这里只是不再把解析结果丢掉。
        let detail = reg.skill_history_detailed("old").unwrap();
        assert_eq!(
            detail.iter().map(|r| r.version).collect::<Vec<_>>(),
            vec![0, 1, 2],
            "与精简版同一版本序列"
        );
        let bodies: Vec<&str> = detail.iter().map(|r| r.instructions.as_str()).collect();
        assert_eq!(
            bodies,
            vec!["legacy 原件指令", "第一次更新", "第二次更新"],
            "各版内容与写入时一致"
        );
        // legacy 原件没有 version/updatedAt frontmatter，但 name 照常解析出来
        assert_eq!(detail[0].name, "旧技能", "legacy 归档行的名称可解析");
        let back = reg.rollback_skill("old", 0).unwrap();
        assert_eq!(back.version, 3, "回滚以新版本行呈现");
        assert_eq!(back.instructions, "legacy 原件指令");
        assert_eq!(back.name, "旧技能", "回滚恢复的是 legacy 原件的完整内容");
    }

    /// 删除是可审计、可恢复的状态，不是抹除：删除后停止加载（不再注入）、历史完整
    /// 可读、`.deleted` 标记在；恢复后续版本号接续而不是从 v1 重来。
    #[test]
    fn 删除留痕_停止加载_历史可读_可恢复且版本续接() {
        let (reg, _root) = mk_registry("delete");
        reg.write_skill_versioned(&skill("d", "第一版")).unwrap();
        reg.write_skill_versioned(&skill("d", "第二版")).unwrap();
        assert!(!reg.skill_is_deleted("d").unwrap());

        assert!(reg.remove_skill_versioned("d").unwrap());
        assert!(
            !reg.remove_skill_versioned("d").unwrap(),
            "重复删除返回 false（幂等）"
        );
        assert!(
            !reg.remove_skill_versioned("never-existed").unwrap(),
            "不存在 → false"
        );
        // 停止加载
        assert!(
            reg.load_skills().unwrap().iter().all(|s| s.id != "d"),
            "删除后不得再被加载/注入"
        );
        assert!(reg.skill_is_deleted("d").unwrap());
        // 历史完整可读：v1、v2 都在，没有「当前版本」行
        let hist = reg.skill_history("d").unwrap();
        let vers: Vec<u64> = hist.iter().map(|(v, _)| *v).collect();
        assert_eq!(vers, vec![1, 2], "删除后历史仍应可审计：{hist:?}");
        // 回滚 = 恢复：以 v1 内容写成新版本，版本号续接（3），不再处于删除态
        let restored = reg.rollback_skill("d", 1).unwrap();
        assert_eq!(
            restored.version, 3,
            "恢复后版本号续接，不得回到 v1 覆盖历史"
        );
        assert_eq!(restored.instructions, "第一版");
        assert!(!reg.skill_is_deleted("d").unwrap());
        let live = reg.load_skills().unwrap().remove(0);
        assert_eq!(live.id, "d");
        assert_eq!(live.version, 3);
        // 历史里 v1/v2 原样保留（恢复没有覆盖它们）
        let vers: Vec<u64> = reg
            .skill_history("d")
            .unwrap()
            .iter()
            .map(|(v, _)| *v)
            .collect();
        assert_eq!(vers, vec![1, 2, 3]);
    }

    /// 删除 legacy 单文件技能（`<id>.json`）：原样搬进历史，同样可恢复。
    #[test]
    fn 删除legacy单文件技能_原件进历史() {
        let (reg, _root) = mk_registry("del-legacy");
        let dir = reg.skills_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = dir.join("lj.json");
        std::fs::write(
            &legacy,
            serde_json::json!({
                "id": "lj", "name": "旧档", "description": "", "triggers": ["触发"],
                "instructions": "旧档指令", "agents": []
            })
            .to_string(),
        )
        .unwrap();
        assert!(
            reg.load_skills().unwrap().iter().any(|s| s.id == "lj"),
            "前置：旧档可加载"
        );
        assert!(reg.remove_skill_versioned("lj").unwrap());
        assert!(!legacy.exists());
        assert!(reg.load_skills().unwrap().iter().all(|s| s.id != "lj"));
        let raw = std::fs::read_to_string(dir.join("lj/.history/v0.md")).unwrap();
        assert!(raw.contains("旧档指令"), "旧档原件应原样进历史：{raw}");
        assert!(reg.skill_is_deleted("lj").unwrap());
    }

    /// 校验不过（格式/敏感/越界 id）→ Err 且**原技能一个字节不变**，无半成品归档。
    #[test]
    fn 校验失败_原技能不变_无半成品() {
        let (reg, _root) = mk_registry("atomic");
        reg.write_skill_versioned(&skill("a", "好内容")).unwrap();
        let dir = reg.skills_dir().unwrap();
        let before = std::fs::read_to_string(dir.join("a/SKILL.md")).unwrap();
        // 前置：首次写入没有旧版本，不应产生任何归档
        assert!(
            !dir.join("a/.history/v0.md").exists(),
            "首次写入无旧版本可归档"
        );
        let err = reg
            .write_skill_versioned(&skill("a", "令牌 sk-abcdefghijklmnopqrstuvwx"))
            .unwrap_err();
        assert!(err.to_string().contains("敏感"), "{err}");
        let err = reg.write_skill_versioned(&skill("a", "")).unwrap_err();
        assert!(err.to_string().contains("instructions"), "{err}");
        let after = std::fs::read_to_string(dir.join("a/SKILL.md")).unwrap();
        assert_eq!(before, after, "被拒的更新不得改动原技能");
        assert_eq!(
            reg.skill_history("a").unwrap().len(),
            1,
            "不得留下半成品历史"
        );
    }
}
