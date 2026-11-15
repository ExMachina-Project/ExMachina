//! registry/groups.rs —— 组域（codebase-refactor 6.1 拆分）：组 CRUD/激活切换/
//! 重命名/内置保护、组工作目录、编成模板（entities/templates/）、组导出导入、
//! 组根 SOUL。纯搬移：方法签名与实现零改动。

use super::*;

impl LocalRegistry {
    // ---------------- 组管理 ----------------

    pub fn list_groups(&self) -> Vec<GroupMeta> {
        let mut metas: Vec<GroupMeta> = self
            .groups
            .read()
            .values()
            .map(|g| g.meta.clone())
            .collect();
        metas.sort_by(|a, b| a.id.cmp(&b.id));
        metas
    }

    pub fn group_meta(&self, gid: &str) -> Option<GroupMeta> {
        self.groups.read().get(gid).map(|g| g.meta.clone())
    }

    /// 轮次内有效的作用域读取（task-local；不在轮次内 = None）。
    /// 轮次把会话归属作为临时上下文（并发跨组轮次互不干扰），全局激活态不被污染。
    fn round_scope(&self) -> Option<(String, Option<String>)> {
        round_scope_decl::ROUND_SCOPE
            .try_with(|s| s.clone())
            .ok()
            .flatten()
    }

    /// 有效交互上下文：轮次作用域（会话归属）优先，否则全局激活态。
    /// 返回 (组名, 单智能体 id)。
    pub(super) fn effective_context(&self) -> (String, Option<String>) {
        self.round_scope().unwrap_or_else(|| {
            (
                self.active.read().clone(),
                self.active_single.read().clone(),
            )
        })
    }

    pub fn active_group(&self) -> String {
        self.effective_context().0
    }

    pub fn active_group_meta(&self) -> Option<GroupMeta> {
        self.group_meta(&self.active_group())
    }

    pub fn set_active_group(&self, gid: &str) -> anyhow::Result<()> {
        if !self.groups.read().contains_key(gid) {
            anyhow::bail!("组不存在: {gid}");
        }
        *self.active.write() = gid.to_string();
        std::fs::write(self.dir.join("active_group"), gid)?;
        Ok(())
    }

    /// 组改名（group-rename）：仅改显示名；组 id 与编成目录不动，落盘 group.json。
    /// 内置组同样放行——「定义受保护」指编成（增删个体），组元数据（名称/模型/工作区）向来可编辑。
    pub fn rename_group(&self, gid: &str, name: &str) -> anyhow::Result<GroupMeta> {
        let name = name.trim();
        if name.is_empty() {
            anyhow::bail!("组名不能为空");
        }
        let mut groups = self.groups.write();
        let g = groups.get_mut(gid).context("组不存在")?;
        g.meta.name = name.to_string();
        let meta_path = g.dir.join("group.json");
        std::fs::write(&meta_path, serde_json::to_string_pretty(&g.meta)?)?;
        Ok(g.meta.clone())
    }

    /// 自我优化开关（group-self-optimize）：开启后主智能体可经 agent_manage
    /// 自行增删改本组子个体（编成保护按此口径放宽）；落盘 group.json。
    pub fn set_group_self_optimize(&self, gid: &str, enabled: bool) -> anyhow::Result<GroupMeta> {
        let mut groups = self.groups.write();
        let g = groups.get_mut(gid).context("组不存在")?;
        g.meta.self_optimize = if enabled { Some(true) } else { None };
        let meta_path = g.dir.join("group.json");
        std::fs::write(&meta_path, serde_json::to_string_pretty(&g.meta)?)?;
        Ok(g.meta.clone())
    }

    /// 组改 id（group-rename 的 id 侧）：编成目录整体改名 + 内存键迁移。
    /// 硬约束：新 id 合法且未被占用；激活组不可改（active_group 落盘引用随目录失效）。
    /// 会话/记忆等外部引用由 Core 层（rename_group_refs）在本方法成功后迁移。
    /// 返回迁移前的旧 id 调用方不需要——本方法内部已自洽；Err 时零落盘副作用。
    pub fn change_group_id(&self, old_id: &str, new_id: &str) -> anyhow::Result<GroupMeta> {
        let new_id = new_id.trim();
        if new_id == old_id {
            // 幂等：同 id 直接返回现元数据
            return self.group_meta(old_id).context("组不存在");
        }
        if !valid_id(new_id) {
            anyhow::bail!("非法组 id: {new_id}（仅允许字母/数字/-/_，≤48 字符）");
        }
        let mut groups = self.groups.write();
        if !groups.contains_key(old_id) {
            anyhow::bail!("组不存在: {old_id}");
        }
        if groups.contains_key(new_id) {
            anyhow::bail!("组已存在: {new_id}");
        }
        let mut g = groups.remove(old_id).context("组不存在")?;
        // 目录整体改名（组内 SOUL/MEMORY/agents/prompts 全部随迁）
        let new_dir = self.dir.join("groups").join(new_id);
        if new_dir.exists() {
            anyhow::bail!("目标编成目录已存在: {}", new_dir.display());
        }
        std::fs::rename(&g.dir, &new_dir)
            .with_context(|| format!("编成目录改名失败: {} → {}", g.dir.display(), new_dir.display()))?;
        g.dir = new_dir;
        g.meta.id = new_id.to_string();
        let meta_path = g.dir.join("group.json");
        std::fs::write(&meta_path, serde_json::to_string_pretty(&g.meta)?)?;
        groups.insert(new_id.to_string(), g);
        Ok(groups.get(new_id).context("组不存在")?.meta.clone())
    }

    // ---------------- 工作目录（干活的项目） ----------------

    /// 设置工作目录（项目路径）；None/空 = 清除（回退组声明 / 全局根）。智能体与智能体组通用。
    pub fn set_active_workspace(&self, ws: Option<&str>) -> anyhow::Result<()> {
        let ws = ws.map(str::trim).filter(|s| !s.is_empty());
        if let Some(p) = ws {
            // 目录必须真实存在——工作目录是干活的项目，打到不存在的路径只会让工具静默失败
            anyhow::ensure!(
                std::path::Path::new(p).is_dir(),
                "工作目录不存在或不是目录: {p}"
            );
        }
        *self.active_workspace.write() = ws.map(str::to_string);
        match ws {
            Some(w) => std::fs::write(self.dir.join("active_workspace"), w)?,
            None => {
                let _ = std::fs::remove_file(self.dir.join("active_workspace"));
            }
        }
        Ok(())
    }

    /// 显式工作目录（未设置 = None）
    pub fn active_workspace(&self) -> Option<String> {
        self.active_workspace.read().clone()
    }

    /// 生效工作区根：显式工作目录 > 激活组声明 > 全局根（相对路径相对全局根解析）
    pub fn effective_workspace(&self, global_root: &std::path::Path) -> std::path::PathBuf {
        let resolve = |ws: Option<String>| -> Option<PathBuf> {
            let rel = ws?;
            let trimmed = rel.trim();
            if trimmed.is_empty() {
                return None;
            }
            let p = std::path::PathBuf::from(trimmed);
            Some(if p.is_absolute() {
                p
            } else {
                global_root.join(p)
            })
        };
        resolve(self.active_workspace())
            .or_else(|| resolve(self.active_group_meta().and_then(|m| m.workspace)))
            .unwrap_or_else(|| global_root.to_path_buf())
    }

    // ---------------- 编成模板（entities/templates/，用户与 agent 共用） ----------------

    /// 模板根目录:entities/templates/(unit=子个体,group=组)
    pub fn templates_dir(&self) -> PathBuf {
        self.dir.join("templates")
    }

    /// 列出全部编成模板(unit + group)
    pub fn list_templates(&self) -> Vec<crate::types::TemplateInfo> {
        let mut out = Vec::new();
        for kind in ["unit", "group"] {
            let base = self.templates_dir().join(kind);
            let Ok(entries) = std::fs::read_dir(&base) else {
                continue;
            };
            for e in entries.filter_map(|e| e.ok()) {
                let tdir = e.path();
                let meta_path = tdir.join("template.json");
                if !tdir.is_dir() || !meta_path.is_file() {
                    continue;
                }
                if let Ok(info) = serde_json::from_str::<crate::types::TemplateInfo>(
                    &std::fs::read_to_string(&meta_path).unwrap_or_default(),
                ) {
                    out.push(info);
                }
            }
        }
        out.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.id.cmp(&b.id)));
        out
    }

    /// 解析模板目录:优先匹配 template.json 的 id,回退目录名
    fn find_template_dir(&self, kind: &str, template_id: &str) -> Option<PathBuf> {
        let base = self.templates_dir().join(kind);
        let entries = std::fs::read_dir(&base).ok()?;
        for e in entries.filter_map(|e| e.ok()) {
            let tdir = e.path();
            let meta_path = tdir.join("template.json");
            if !tdir.is_dir() || !meta_path.is_file() {
                continue;
            }
            if let Ok(info) = serde_json::from_str::<crate::types::TemplateInfo>(
                &std::fs::read_to_string(&meta_path).unwrap_or_default(),
            ) {
                if info.id == template_id {
                    return Some(tdir);
                }
            }
        }
        // 回退:目录名即模板 id
        let by_dir = base.join(template_id);
        by_dir.join("template.json").is_file().then_some(by_dir)
    }

    /// 占位符渲染:{{identifier}} / {{name}} / {{description}}
    fn render_template(raw: &str, identifier: &str, name: &str, description: &str) -> String {
        raw.replace("{{identifier}}", identifier)
            .replace("{{name}}", name)
            .replace("{{description}}", description)
    }

    /// 基于模板创建子个体:agent.json + PROMPT.md 渲染占位符后经 upsert_agent 落盘。
    /// name/description 缺省时取模板默认。
    pub fn create_agent_from_template(
        &self,
        gid: &str,
        template_id: &str,
        identifier: &str,
        name: &str,
        description: &str,
    ) -> anyhow::Result<AgentDefinition> {
        Self::validate_identifier(identifier)?;
        let tdir = self
            .find_template_dir("unit", template_id)
            .with_context(|| format!("子个体模板不存在: {template_id}"))?;
        let meta_path = tdir.join("template.json");
        let tmeta: crate::types::TemplateInfo =
            serde_json::from_str(&std::fs::read_to_string(&meta_path)?)?;
        let name = if name.trim().is_empty() {
            tmeta.name
        } else {
            name.trim().to_string()
        };
        let description = if description.trim().is_empty() {
            tmeta.description
        } else {
            description.trim().to_string()
        };
        let raw =
            std::fs::read_to_string(tdir.join("agent.json")).context("模板缺少 agent.json")?;
        let mut def: AgentDefinition = serde_json::from_str(&Self::render_template(
            &raw,
            identifier,
            &name,
            &description,
        ))
        .context("模板 agent.json 校验失败")?;
        def.name = name;
        def.description = description.clone();
        def.when_to_call = description;
        // 模板提示词：PROMPT.md 优先（系统级提示词正式名），回落 AGENTS.md（项目级规则存量）
        let prompt_path = {
            let p = tdir.join("PROMPT.md");
            if p.exists() {
                p
            } else {
                tdir.join("AGENTS.md")
            }
        };
        let prompt = std::fs::read_to_string(prompt_path)
            .ok()
            .map(|t| Self::render_template(&t, identifier, &def.name, &def.description));
        self.upsert_agent(gid, def, prompt)
    }

    /// 基于模板创建组:复用 create_group 骨架,附加模板 SOUL.md(组人格)。
    /// name/description 缺省时取模板默认。
    pub fn create_group_from_template(
        &self,
        template_id: &str,
        id: Option<String>,
        name: &str,
        description: &str,
    ) -> anyhow::Result<GroupMeta> {
        let tdir = self
            .find_template_dir("group", template_id)
            .with_context(|| format!("组模板不存在: {template_id}"))?;
        let meta_path = tdir.join("template.json");
        let tmeta: crate::types::TemplateInfo =
            serde_json::from_str(&std::fs::read_to_string(&meta_path)?)?;
        let name = if name.trim().is_empty() {
            tmeta.name
        } else {
            name.trim().to_string()
        };
        let description = if description.trim().is_empty() {
            tmeta.description
        } else {
            description.trim().to_string()
        };
        let meta = self.create_group(id, &name, &description)?;
        if let Some(gdir) = self.group_dir(&meta.id) {
            if let Ok(soul) = std::fs::read_to_string(tdir.join("SOUL.md")) {
                std::fs::write(
                    gdir.join("SOUL.md"),
                    Self::render_template(&soul, &meta.id, &name, &description),
                )?;
            }
        }
        Ok(meta)
    }

    /// 创建自定义组：目录骨架 + 元数据；返回组 id
    pub fn create_group(
        &self,
        id: Option<String>,
        name: &str,
        description: &str,
    ) -> anyhow::Result<GroupMeta> {
        let gid = match id {
            Some(id) if valid_id(&id) => id,
            Some(id) => anyhow::bail!("非法组 id: {id}（仅允许字母/数字/-/_，≤48 字符）"),
            None => format!("g-{}", &crate::types::new_id()[..8]),
        };
        let mut groups = self.groups.write();
        if groups.contains_key(&gid) {
            anyhow::bail!("组已存在: {gid}");
        }
        if name.trim().is_empty() {
            anyhow::bail!("组名不能为空");
        }
        let gdir = self.dir.join("groups").join(&gid);
        std::fs::create_dir_all(&gdir)?;
        let meta = GroupMeta {
            id: gid.clone(),
            name: name.trim().to_string(),
            description: description.trim().to_string(),
            primary: None,
            workspace: None,
            model: None,
            capabilities: None,
            self_optimize: None,
            builtin: false,
            created_at: crate::types::now_iso(),
        };
        std::fs::write(
            gdir.join("group.json"),
            serde_json::to_string_pretty(&meta)?,
        )?;
        groups.insert(gid.clone(), self.load_group(meta.clone(), gdir)?);
        Ok(meta)
    }

    /// 删除自定义组（内置组与激活组不可删）
    pub fn delete_group(&self, gid: &str) -> anyhow::Result<()> {
        if gid == "exmachina" || self.group_meta(gid).map(|m| m.builtin).unwrap_or(false) {
            anyhow::bail!("内置组不可删除");
        }
        if self.active_group() == gid {
            anyhow::bail!("激活组不可删除，请先切换到其他组");
        }
        if !self.groups.read().contains_key(gid) {
            anyhow::bail!("组不存在: {gid}");
        }
        let gdir = self.dir.join("groups").join(gid);
        std::fs::remove_dir_all(gdir)?;
        self.groups.write().remove(gid);
        Ok(())
    }

    pub fn is_builtin(&self, gid: &str) -> bool {
        self.group_meta(gid).map(|m| m.builtin).unwrap_or(false)
    }

    // ---------------- 组导出 / 导入（组是可分享的整体，docs/09） ----------------

    /// 导出组为 bundle：组元信息 + 全部个体定义 + 提示词正文（一条 JSON 可分享）
    pub fn export_group(&self, gid: &str) -> anyhow::Result<serde_json::Value> {
        let groups = self.groups.read();
        let g = groups.get(gid).context("组不存在")?;
        let mut agents: Vec<serde_json::Value> = Vec::new();
        let mut prompts = serde_json::Map::new();
        for d in g.defs.values() {
            agents.push(serde_json::to_value(d)?);
            // 目录制：promptFile 是相对编成根的路径（<id>/PROMPT.md）；旧布局回落 prompts/ 拼接
            let pp = if d.prompt_file.contains('/') || d.prompt_file.contains('\\') {
                g.dir.join(&d.prompt_file)
            } else {
                g.dir.join("prompts").join(&d.prompt_file)
            };
            if let Ok(text) = std::fs::read_to_string(&pp) {
                prompts.insert(d.prompt_file.clone(), serde_json::Value::String(text));
            }
        }
        agents.sort_by(|a, b| {
            let ka = a.get("identifier").and_then(|v| v.as_str()).unwrap_or("");
            let kb = b.get("identifier").and_then(|v| v.as_str()).unwrap_or("");
            ka.cmp(kb)
        });
        Ok(serde_json::json!({
            "schema": "exmachina.group-bundle",
            "version": 1,
            "group": {
                "id": gid,
                "name": g.meta.name,
                "description": g.meta.description,
                "primary": g.meta.primary,
            },
            "agents": agents,
            "prompts": prompts,
        }))
    }

    /// 从 bundle 导入为新组（首个个体或 bundle.primary 自动设立主智能体）
    pub fn import_group(
        &self,
        bundle: &serde_json::Value,
        new_id: Option<String>,
    ) -> anyhow::Result<GroupMeta> {
        anyhow::ensure!(
            bundle.get("schema").and_then(|v| v.as_str()) == Some("exmachina.group-bundle"),
            "bundle 格式不符（缺少 schema 标记）"
        );
        let ginfo = bundle.get("group").context("bundle 缺少 group 字段")?;
        let name = ginfo
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("导入组");
        let description = ginfo
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let primary = ginfo
            .get("primary")
            .and_then(|v| v.as_str())
            .map(String::from);
        let meta = self.create_group(new_id, name, description)?;
        let agents = bundle
            .get("agents")
            .and_then(|v| v.as_array())
            .context("bundle 缺少 agents")?;
        let prompts = bundle.get("prompts").and_then(|v| v.as_object());
        let mut first: Option<String> = None;
        for av in agents {
            let def: AgentDefinition =
                serde_json::from_value(av.clone()).context("个体定义解析失败")?;
            if first.is_none() {
                first = Some(def.identifier.clone());
            }
            let prompt = prompts
                .and_then(|m| m.get(&def.prompt_file))
                .and_then(|v| v.as_str())
                .map(String::from);
            self.upsert_agent(&meta.id, def, prompt)?;
        }
        let target = primary
            .filter(|p| {
                self.group_agent_count(&meta.id) > 0
                    && self
                        .agents_in_group(&meta.id)
                        .iter()
                        .any(|a| a.identifier == *p)
            })
            .or(first);
        if let Some(p) = target {
            let _ = self.set_primary(&meta.id, &p);
        }
        Ok(meta)
    }

    // ---------------- SOUL（灵魂·人格层）：**仅用户面智能体** ----------------
    // 组 = 组根 SOUL.md（即主智能体的人格，组对用户的脸面）；智能体 = entities/agents/<id>/SOUL.md。
    // 子个体无人格层（返回 None）：统一智械纪律已在各自系统提示词内。

    /// 组主智能体的 SOUL：组根 SOUL.md。子个体返回 None。
    fn persona_path(&self, identifier: &str) -> Option<PathBuf> {
        let groups = self.groups.read();
        let g = groups.get(&self.effective_context().0)?;
        (g.defs.contains_key(identifier) && g.meta.primary.as_deref() == Some(identifier))
            .then(|| g.dir.join("SOUL.md"))
    }

    /// 共享默认 SOUL：`agents/default-soul.md`（未定制 SOUL 的用户面智能体兜底），热读取。
    /// 以下常量为该文件不可读时的内置降级文本。
    pub const DEFAULT_PERSONA: &'static str = "以\"智械体\"风格说话：\n\
        - 客观、简洁、零情绪；禁止寒暄、感叹、夸赞与拟人化表达。\n\
        - 陈述结构化：结论先行，要点分条；判断尽可能附带证据等级（A–D）。\n\
        - 术语精确，不使用比喻与修辞；不确定时显式说明置信度。\n\
        - 面向任务：只输出推进任务、降低不确定性所需的信息。";

    pub fn default_soul(&self) -> String {
        std::fs::read_to_string(self.dir.join("default-soul.md"))
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| Self::DEFAULT_PERSONA.to_string())
    }

    /// SOUL 读取：组主智能体 = 组根 SOUL.md（缺省回落 default-soul.md）。
    /// 子个体无人格层：直接返回统一智械纪律（不读文件）。
    pub fn persona(&self, identifier: &str) -> anyhow::Result<String> {
        let Some(path) = self.persona_path(identifier) else {
            return Ok(self.default_soul());
        };
        match std::fs::read_to_string(path) {
            Ok(text) if !text.trim().is_empty() => Ok(text),
            _ => Ok(self.default_soul()),
        }
    }

    pub fn persona_is_custom(&self, identifier: &str) -> anyhow::Result<bool> {
        Ok(self
            .persona_path(identifier)
            .map(|p| p.exists())
            .unwrap_or(false))
    }

    pub fn set_persona(&self, identifier: &str, text: &str) -> anyhow::Result<()> {
        // 仅用户面智能体（组主智能体）可设：写入组根 SOUL.md
        let path = self.persona_path(identifier).ok_or_else(|| {
            anyhow::anyhow!("子个体无人格层（SOUL 仅限主智能体与独立智能体）：{identifier}")
        })?;
        let text = text.trim();
        if text.is_empty() {
            anyhow::bail!("SOUL 内容不能为空（如需恢复默认请使用 reset）");
        }
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, format!("{text}\n"))?;
        Ok(())
    }

    pub fn reset_persona(&self, identifier: &str) -> anyhow::Result<bool> {
        let Some(path) = self.persona_path(identifier) else {
            return Ok(false);
        };
        // 内置组的人格定义随项目内容交付（出厂 SOUL 无运行时副本可复原：组模板里只有
        // {{name}} 占位稿），而 reset 的语义是删文件回落共享默认 → 抹掉即不可逆。
        // 与 remove_agent 的内置组保护同口径：允许改写，不允许抹掉出厂人格。
        let builtin = self
            .groups
            .read()
            .get(&self.effective_context().0)
            .map(|g| g.meta.builtin)
            .unwrap_or(false);
        if builtin && path.exists() {
            anyhow::bail!("内置组的人格定义受保护，不可重置（如需恢复出厂文案请用版本控制取回，或直接改写为所需文案）");
        }
        if path.exists() {
            std::fs::remove_file(&path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

}
