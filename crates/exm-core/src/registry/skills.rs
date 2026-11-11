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
        let Some(dir) = self.skills_dir() else {
            return Ok(vec![]);
        };
        if !dir.exists() {
            return Ok(vec![]);
        }
        // 两类来源：skills/<id>/SKILL.md（新标准）与 skills/<id>.json（旧格式兼容）
        let mut md: Vec<PathBuf> = Vec::new();
        let mut json: Vec<PathBuf> = Vec::new();
        for p in std::fs::read_dir(&dir)?.filter_map(|e| e.ok()).map(|e| e.path()) {
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
        let md_ids: std::collections::HashSet<String> =
            out.iter().map(|s| s.id.clone()).collect();
        for f in json {
            let raw = std::fs::read_to_string(&f)?;
            let s: crate::types::SkillDef = serde_json::from_str(&raw)
                .with_context(|| format!("技能包校验失败: {}", f.display()))?;
            if !md_ids.contains(&s.id) {
                out.push(s); // 同 id 的 SKILL.md 已收，旧 json 跳过
            }
        }
        // 运行期统计（侧车聚合；SKILL.md 保持纯内容，统计不污染可分享的技能文件）
        let usage = self.skill_usage_map();
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
        let root = std::env::temp_dir().join(format!("exm-skills-{tag}-{}", crate::types::new_id()));
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
        assert!(list.iter().any(|s| s.id == "legacy"), "迁移后可装载：{list:?}");
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
        assert!(dir.join("rt-skill/SKILL.md").exists(), "写入应为 SKILL.md 格式");
        let list = reg.load_skills().unwrap();
        let got = list.iter().find(|x| x.id == "rt-skill").expect("应可读回");
        assert_eq!(got.name, "往返技能");
        assert_eq!(got.triggers, vec!["往返".to_string(), "roundtrip".to_string()]);
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
        assert!(!skills.join("dual.json").exists(), "write 应清除同 id 旧 json");
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
        reg.upsert_single(crate::types::AgentDefinition {
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
        }, None)
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
