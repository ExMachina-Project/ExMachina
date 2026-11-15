//! registry/agents.rs —— 个体域（codebase-refactor 6.1 拆分）：激活组内个体访问、
//! 独立智能体（singles）、提示词/Playbook 装配、经验优化、组内个体增删改、
//! 智能连结网络绑定、智能体 SOUL。纯搬移：方法签名与实现零改动。

use super::*;

impl LocalRegistry {
    // ---------------- 激活组内的个体访问（保持既有接口形状） ----------------

    fn with_active<T>(&self, f: impl FnOnce(&GroupData) -> T) -> Option<T> {
        let groups = self.groups.read();
        groups.get(&self.effective_context().0).map(f)
    }

    pub fn get(&self, identifier: &str) -> Option<AgentDefinition> {
        self.with_active(|g| g.defs.get(identifier).cloned())
            .flatten()
    }

    /// 按能力检索：域、能力标签、名称或 identifier 命中（激活组）
    pub fn find_by_capability(&self, cap: &str) -> Vec<AgentDefinition> {
        let needle = cap.to_lowercase();
        let mut out: Vec<AgentDefinition> = self
            .with_active(|g| {
                g.defs
                    .values()
                    .filter(|d| {
                        d.domain.contains(cap)
                            || d.capabilities
                                .iter()
                                .any(|c| c.to_lowercase().contains(&needle))
                            || d.name.contains(cap)
                            || d.identifier.to_lowercase().contains(&needle)
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        out.sort_by(|a, b| a.identifier.cmp(&b.identifier));
        out
    }

    /// 当前交互范围的个体集合（智能体模式 = 智能体列表；组模式 = 组编成）
    pub fn list_active(&self) -> Vec<AgentDefinition> {
        if self.single_mode() {
            self.list_singles()
        } else {
            self.list()
        }
    }

    pub fn list(&self) -> Vec<AgentDefinition> {
        self.with_active(|g| {
            let mut v: Vec<AgentDefinition> = g.defs.values().cloned().collect();
            v.sort_by(|a, b| a.identifier.cmp(&b.identifier));
            v
        })
        .unwrap_or_default()
    }

    // ---------------- 智能体（独立交互对象，docs/09 §8） ----------------

    /// 当前是否处于智能体交互模式
    pub fn single_mode(&self) -> bool {
        self.active_single().is_some()
    }

    pub fn active_single(&self) -> Option<String> {
        self.effective_context().1
    }

    /// 当前交互范围 id：智能体模式 = single:<id>（会话归属键），否则 = 激活组
    pub fn active_scope(&self) -> String {
        match &self.active_single() {
            Some(id) => format!("single:{id}"),
            None => self.active_group(),
        }
    }

    pub fn list_singles(&self) -> Vec<AgentDefinition> {
        let mut v: Vec<AgentDefinition> = self.singles.read().values().cloned().collect();
        v.sort_by(|a, b| a.identifier.cmp(&b.identifier));
        v
    }

    pub fn single(&self, id: &str) -> Option<AgentDefinition> {
        self.singles.read().get(id).cloned()
    }

    /// 语言变体提示词路径：组内 prompts/ 或全局 prompts/（智能体模式）
    fn variant_path(&self, stem: &str, lang: &str) -> Option<PathBuf> {
        let file = format!("{stem}.{lang}.md");
        if self.single_mode() {
            return Some(if stem.contains('/') || stem.contains('\\') {
                self.singles_dir().join(&file)
            } else {
                self.dir.join("agents").join("prompts").join(&file)
            });
        }
        let groups = self.groups.read();
        let g = groups.get(&self.effective_context().0);
        let dir = g.map(|g| g.dir.clone()).unwrap_or_else(|| self.dir.clone());
        Some(if stem.contains('/') || stem.contains('\\') {
            dir.join(&file)
        } else {
            dir.join("prompts").join(file)
        })
    }

    fn singles_dir(&self) -> PathBuf {
        self.dir.join("agents")
    }

    fn singles_prompt_path(&self, prompt_file: &str) -> PathBuf {
        if prompt_file.contains('/') || prompt_file.contains('\\') {
            // 智能体目录 entities/agents/<id>/PROMPT.md（AGENTS.md 双名回落）
            self.singles_dir().join(prompt_file)
        } else {
            self.singles_dir().join("prompts").join(prompt_file)
        }
    }

    /// 智能体定义文件：entities/agents/<id>/agent.json
    fn single_def_path(&self, id: &str) -> PathBuf {
        self.singles_dir().join(id).join("agent.json")
    }

    /// 读取智能体提示词（PROMPT.md 正式名；promptFile 为 AGENTS.md 时自动回落新名）
    pub fn load_single_prompt(&self, prompt_file: &str) -> anyhow::Result<String> {
        let p = self.singles_prompt_path(prompt_file);
        if let Ok(text) = std::fs::read_to_string(&p) {
            return Ok(text);
        }
        // 双名回落：旧名 → 新名（改名后用户未迁移目录时提示词不丢）
        let alt = Self::alternate_prompt_name(prompt_file);
        if alt != prompt_file {
            if let Ok(text) = std::fs::read_to_string(self.singles_prompt_path(&alt)) {
                return Ok(text);
            }
        }
        Err(anyhow::anyhow!("读取智能体提示词失败: {prompt_file}"))
    }

    /// 提示词文件名双名映射：AGENTS.md ↔ PROMPT.md（其余名称原样返回）
    fn alternate_prompt_name(prompt_file: &str) -> String {
        if prompt_file.ends_with("AGENTS.md") {
            prompt_file.replace("AGENTS.md", "PROMPT.md")
        } else if prompt_file.ends_with("PROMPT.md") {
            prompt_file.replace("PROMPT.md", "AGENTS.md")
        } else {
            prompt_file.to_string()
        }
    }

    /// 写入智能体提示词文件（PROMPT.md；与激活组状态无关）。
    /// 存量目录还挂着 AGENTS.md：保存即迁移——写 PROMPT.md、删旧名、更新 def.promptFile，
    /// 保证下次读取/导出都指向正式名（避免双文件长期并存漂移）
    pub fn write_single_prompt(&self, prompt_file: &str, content: &str) -> anyhow::Result<String> {
        let p = self.singles_prompt_path(prompt_file);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&p, content)?;
        // 旧名迁移：写的是 AGENTS.md 时补写 PROMPT.md 并清掉旧名
        let final_name = if prompt_file.ends_with("AGENTS.md") {
            let alt = Self::alternate_prompt_name(prompt_file);
            let alt_path = self.singles_prompt_path(&alt);
            if let Some(parent) = alt_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&alt_path, content)?;
            let _ = std::fs::remove_file(&p);
            alt
        } else {
            prompt_file.to_string()
        };
        Ok(final_name)
    }

    /// 新增/更新智能体（identifier 冲突即替换）
    pub fn upsert_single(
        &self,
        mut def: AgentDefinition,
        prompt: Option<String>,
    ) -> anyhow::Result<AgentDefinition> {
        Self::validate_identifier(&def.identifier)?;
        if def.name.trim().is_empty() || def.description.trim().is_empty() {
            anyhow::bail!("name/description 不能为空");
        }
        if def.prompt_file.trim().is_empty() {
            // 正式名 PROMPT.md（系统级提示词）；存量 AGENTS.md 继续按其 promptFile 读取
            def.prompt_file = format!("{}/PROMPT.md", def.identifier);
        }
        def.domain = normalize_domain(&def.domain);
        // 工具面默认全量（agent-tool-surface）：创建/更新未带 tools = 沿用全量
        if def.tools.is_empty() {
            def.tools = ToolName::full_set();
        }
        let prompt_path = self.singles_prompt_path(&def.prompt_file);
        if let Some(parent) = prompt_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Some(text) = prompt {
            std::fs::write(&prompt_path, text)?;
        } else if !prompt_path.exists() {
            std::fs::write(
                &prompt_path,
                format!(
                    "# {}

你是 {}，管理者的智能体：听清意图、亲手做完、回报结果——分析与实施一体完成。

## 语言纪律
- 称对方为「管理者」（你的主人与授权者），以\"本机\"自称。
- 被问起你是谁：一句话说明名字与职责，随即回到管理者的事上。
- 信息不足时显式说明假设，再给出最小验证路径。
",
                    def.name, def.name
                ),
            )?;
        }
        self.singles
            .write()
            .insert(def.identifier.clone(), def.clone());
        let def_path = self.single_def_path(&def.identifier);
        std::fs::create_dir_all(def_path.parent().unwrap())?;
        std::fs::write(def_path, serde_json::to_string_pretty(&def)?)?;
        Ok(def)
    }

    pub fn remove_single(&self, id: &str) -> anyhow::Result<bool> {
        let removed = self.singles.write().remove(id).is_some();
        if removed {
            // 整目录移除（PROMPT/SOUL/MEMORY 随个体走）
            let _ = std::fs::remove_dir_all(self.singles_dir().join(id));
            let _ = std::fs::remove_file(self.singles_dir().join(format!("{id}.json"))); // 旧布局清理
            if self.active_single() == Some(id.to_string()) {
                self.set_active_single(None)?;
            }
        }
        Ok(removed)
    }

    /// 切换交互目标：Some(智能体 id) 进智能体模式，None 回到激活组
    pub fn set_active_single(&self, id: Option<&str>) -> anyhow::Result<()> {
        match id {
            Some(x) => {
                if !self.singles.read().contains_key(x) {
                    anyhow::bail!("智能体不存在: {x}");
                }
                *self.active_single.write() = Some(x.to_string());
                std::fs::write(self.dir.join("active_single"), x)?;
            }
            None => {
                *self.active_single.write() = None;
                let _ = std::fs::remove_file(self.dir.join("active_single"));
            }
        }
        Ok(())
    }

    /// 交互上下文快照（激活组 + 单智能体）：轮次执行前保存，结束后还原。
    /// 会话按自身归属执行，不污染全局激活态（并发会话互不干扰）。
    pub fn context_snapshot(&self) -> (String, Option<String>) {
        (self.active_group(), self.active_single())
    }

    /// 全局激活组（不含轮次作用域；轮次作用域请用 active_group）
    pub fn active_group_global(&self) -> String {
        self.active.read().clone()
    }

    pub fn group_exists(&self, gid: &str) -> bool {
        self.groups.read().contains_key(gid)
    }

    pub fn single_exists(&self, id: &str) -> bool {
        self.singles.read().contains_key(id)
    }

    /// 指定组的个体清单（供跨组查看组详情，不依赖激活组）
    pub fn agents_in_group(&self, gid: &str) -> Vec<AgentDefinition> {
        let groups = self.groups.read();
        let Some(g) = groups.get(gid) else {
            return vec![];
        };
        let mut v: Vec<AgentDefinition> = g.defs.values().cloned().collect();
        v.sort_by(|a, b| a.identifier.cmp(&b.identifier));
        v
    }

    /// 个体所在组目录（激活组内成员;提示词/经验要点定位用,不存在 → None）
    pub fn group_dir_of_agent(&self, identifier: &str) -> Option<PathBuf> {
        self.with_active(|g| {
            if g.defs.contains_key(identifier) {
                Some(g.dir.clone())
            } else {
                None
            }
        })
        .flatten()
    }

    pub fn units(&self) -> Vec<AgentDefinition> {
        if self.single_mode() {
            return vec![]; // 智能体模式不派发子个体
        }
        self.list()
            .into_iter()
            .filter(|d| matches!(d.tier, Tier::Unit))
            .collect()
    }

    /// 当前交互目标的主智能体：智能体模式 = 该智能体；否则 = 组内主智能体
    pub fn primary(&self) -> Option<AgentDefinition> {
        let (group, single) = self.effective_context();
        if let Some(id) = &single {
            if let Some(d) = self.singles.read().get(id) {
                return Some(d.clone());
            }
        }
        let groups = self.groups.read();
        let g = groups.get(&group)?;
        let pid = g.meta.primary.clone()?;
        g.defs.get(&pid).cloned()
    }

    /// 冲突裁决个体：按数据声明动态选路（能力含「裁决」优先，其次「决策」，再次名称命中），
    /// 无命中时回退主智能体。代码不指向任何具体 identifier，编成变化不影响裁决链路。
    pub fn arbiter(&self) -> Option<AgentDefinition> {
        let defs = self.list();
        let mut best: Option<(u8, AgentDefinition)> = None;
        for d in defs
            .iter()
            .filter(|d| !matches!(d.tier, Tier::Orchestrator))
        {
            let score = if d.capabilities.iter().any(|c| c.contains("裁决")) {
                3
            } else if d.capabilities.iter().any(|c| c.contains("决策")) {
                2
            } else if d.name.contains("裁决") || d.name.contains("决策") {
                1
            } else {
                0
            };
            if score > 0 && best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
                best = Some((score, d.clone()));
            }
        }
        best.map(|(_, d)| d).or_else(|| self.primary())
    }

    pub fn count(&self) -> usize {
        self.with_active(|g| g.defs.len()).unwrap_or(0)
    }

    /// 指定组的个体数
    pub fn group_agent_count(&self, gid: &str) -> usize {
        self.groups
            .read()
            .get(gid)
            .map(|g| g.defs.len())
            .unwrap_or(0)
    }

    // ---------------- 提示词 / Playbook / SOUL（激活组） ----------------

    /// 组目录（groups/<gid>/，MEMORY.md / SOUL.md / agents/ / prompts/ 所在）
    pub fn group_dir(&self, gid: &str) -> Option<PathBuf> {
        self.groups.read().get(gid).map(|g| g.dir.clone())
    }

    /// 智能体根目录（entities/agents/，公开供记忆渲染等使用）
    pub fn singles_dir_pub(&self) -> PathBuf {
        self.singles_dir()
    }

    /// 提示词路径解析：`promptFile` 含 `/` = 相对编成根的个体目录路径（`<id>/PROMPT.md`）；
    /// 否则兼容旧约定 `<编成根>/prompts/<file>`。
    fn prompt_fs_path(&self, prompt_file: &str) -> PathBuf {
        let groups = self.groups.read();
        let g = groups.get(&self.effective_context().0);
        let dir = g.map(|g| g.dir.clone()).unwrap_or_else(|| self.dir.clone());
        if prompt_file.contains('/') || prompt_file.contains('\\') {
            dir.join(prompt_file)
        } else {
            dir.join("prompts").join(prompt_file)
        }
    }

    pub fn load_prompt(&self, prompt_file: &str) -> anyhow::Result<String> {
        if self.single_mode() {
            if let Ok(text) = self.load_single_prompt(prompt_file) {
                return Ok(text);
            }
        }
        // 语言变体：EXM_LANG=en 时优先 {stem}.{lang}.md（如 <id>/PROMPT.en.md）
        let lang = crate::config::ExmConfig::load_language();
        if lang != "zh" {
            let stem = prompt_file.strip_suffix(".md").unwrap_or(prompt_file);
            let variant = self.variant_path(stem, &lang);
            if let Some(p) = variant {
                if p.exists() {
                    if let Ok(text) = std::fs::read_to_string(&p) {
                        return Ok(text);
                    }
                }
            }
        }
        let p = self.prompt_fs_path(prompt_file);
        if let Ok(text) = std::fs::read_to_string(&p) {
            return Ok(text);
        }
        // 双名回落：PROMPT.md ↔ AGENTS.md（改名迁移期两侧互认，目录未迁移提示词不丢）
        let alt = Self::alternate_prompt_name(prompt_file);
        if alt != prompt_file {
            if let Ok(text) = std::fs::read_to_string(self.prompt_fs_path(&alt)) {
                return Ok(text);
            }
        }
        Err(anyhow::anyhow!("读取提示词失败: {}", p.display()))
    }

    /// 组内提示词是否存在（双名互认：AGENTS.md / PROMPT.md 任一存在即真）
    pub fn prompt_exists(&self, prompt_file: &str) -> bool {
        let exists_at = |pf: &str| {
            if self.single_mode() {
                self.singles_prompt_path(pf).exists()
            } else {
                self.prompt_fs_path(pf).exists()
            }
        };
        if exists_at(prompt_file) {
            return true;
        }
        let alt = Self::alternate_prompt_name(prompt_file);
        alt != prompt_file && exists_at(&alt)
    }

    /// 写入组内提示词文件（创建/更新个体时使用）
    pub fn write_prompt(&self, prompt_file: &str, content: &str) -> anyhow::Result<()> {
        let p = if self.single_mode() {
            self.singles_prompt_path(prompt_file)
        } else {
            self.prompt_fs_path(prompt_file)
        };
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&p, content)?;
        Ok(())
    }

    /// 组作用域提示词读取（成员编辑用：不随激活组漂移）
    pub fn load_group_prompt(&self, gid: &str, prompt_file: &str) -> anyhow::Result<String> {
        let dir = self.group_dir(gid).context("组不存在")?;
        let p = if prompt_file.contains('/') || prompt_file.contains('\\') {
            dir.join(prompt_file)
        } else {
            dir.join("prompts").join(prompt_file)
        };
        std::fs::read_to_string(&p).with_context(|| format!("读取提示词失败: {}", p.display()))
    }

    /// 组作用域提示词写入
    /// 写入组内个体提示词。存量 PROMPT.md 保存即迁移：写 AGENTS.md、删旧名，
    /// 返回最终文件名（调用方据此回写 def.promptFile）
    pub fn write_group_prompt(
        &self,
        gid: &str,
        prompt_file: &str,
        content: &str,
    ) -> anyhow::Result<String> {
        let dir = self.group_dir(gid).context("组不存在")?;
        let resolve = |pf: &str| {
            if pf.contains('/') || pf.contains('\\') {
                dir.join(pf)
            } else {
                dir.join("prompts").join(pf)
            }
        };
        let p = resolve(prompt_file);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&p, content)?;
        if prompt_file.ends_with("PROMPT.md") {
            let alt = Self::alternate_prompt_name(prompt_file);
            let alt_path = resolve(&alt);
            if let Some(parent) = alt_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&alt_path, content)?;
            let _ = std::fs::remove_file(&p);
            return Ok(alt);
        }
        Ok(prompt_file.to_string())
    }

    pub fn load_playbooks(&self) -> anyhow::Result<Vec<crate::types::Playbook>> {
        self.playbooks_in_group(&self.active_group())
    }

    /// 指定组的链路模板（组总览用，不依赖激活组）
    pub fn playbooks_in_group(&self, gid: &str) -> anyhow::Result<Vec<crate::types::Playbook>> {
        let groups = self.groups.read();
        let Some(g) = groups.get(gid) else {
            return Ok(vec![]);
        };
        let dir = g.dir.join("playbooks");
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
            .collect();
        files.sort();
        let mut out = Vec::new();
        for f in files {
            let raw = std::fs::read_to_string(&f)?;
            out.push(
                serde_json::from_str::<crate::types::Playbook>(&raw)
                    .with_context(|| format!("playbook 校验失败: {}", f.display()))?,
            );
        }
        Ok(out)
    }

    // ---------------- 经验优化（子个体自适应，docs/10 §5） ----------------

    fn adaptation_path(&self, identifier: &str) -> Option<PathBuf> {
        self.with_active(|g| {
            if g.defs.contains_key(identifier) {
                // 子个体无独立目录：经验要点集中在组目录 adaptations/
                Some(g.dir.join("adaptations").join(format!("{identifier}.json")))
            } else {
                None
            }
        })
        .flatten()
    }

    /// 读取个体的经验改进要点（激活组内）
    pub fn load_adaptation(
        &self,
        identifier: &str,
    ) -> anyhow::Result<Option<crate::types::AgentAdaptation>> {
        let Some(p) = self.adaptation_path(identifier) else {
            return Ok(None);
        };
        if p.as_os_str().is_empty() || !p.exists() {
            return Ok(None);
        }
        Ok(Some(
            serde_json::from_str(&std::fs::read_to_string(&p)?)
                .with_context(|| format!("解析经验要点失败: {}", p.display()))?,
        ))
    }

    /// 保存经验要点：旧版本自动入历史（上限 5），updated_at 在此刷新
    pub fn save_adaptation(
        &self,
        mut adapt: crate::types::AgentAdaptation,
    ) -> anyhow::Result<crate::types::AgentAdaptation> {
        let Some(p) = self.adaptation_path(&adapt.identifier) else {
            anyhow::bail!("个体不存在（激活组内）: {}", adapt.identifier);
        };
        if p.as_os_str().is_empty() {
            anyhow::bail!("个体不存在（激活组内）: {}", adapt.identifier);
        }
        if let Some(old) = self.load_adaptation(&adapt.identifier)? {
            adapt.previous = old.previous;
            adapt.previous.insert(
                0,
                crate::types::AgentAdaptationVersion {
                    revision: old.revision,
                    content: old.content,
                    updated_at: old.updated_at,
                },
            );
            adapt.previous.truncate(5);
        }
        adapt.updated_at = crate::types::now_iso();
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&p, serde_json::to_string_pretty(&adapt)?)?;
        Ok(adapt)
    }

    pub fn reset_adaptation(&self, identifier: &str) -> anyhow::Result<bool> {
        let Some(p) = self.adaptation_path(identifier) else {
            return Ok(false);
        };
        if p.as_os_str().is_empty() || !p.exists() {
            return Ok(false);
        }
        std::fs::remove_file(&p)?;
        Ok(true)
    }

    pub fn remove_skill(&self, id: &str) -> anyhow::Result<bool> {
        let id = id.trim();
        // 与 write_skill 同口径校验：删除路径由 id 拼接，禁止 .. / 分隔符穿越
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            anyhow::bail!("非法技能 id: {id}（仅允许字母/数字/-/_）");
        }
        let Some(dir) = self.skills_dir() else {
            anyhow::bail!("无激活组")
        };
        // 新格式目录 + 旧格式单文件都清（迁移期双源）
        let skill_dir = dir.join(id);
        let legacy = dir.join(format!("{id}.json"));
        let mut removed = false;
        if skill_dir.is_dir() {
            std::fs::remove_dir_all(&skill_dir)?;
            removed = true;
        }
        if legacy.exists() {
            std::fs::remove_file(&legacy)?;
            removed = true;
        }
        if removed {
            self.remove_skill_usage(id);
        }
        Ok(removed)
    }

    // ---------------- 组内个体增删改（主智能体权限 / 用户操作） ----------------

    pub(super) fn validate_identifier(identifier: &str) -> anyhow::Result<()> {
        if identifier.is_empty()
            || !identifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            anyhow::bail!("非法个体标识: {identifier}（仅允许字母/数字/-/_）");
        }
        Ok(())
    }

    /// 新增/替换个体（组内）。`prompt` 为 Some 时写入提示词文件；identifier 冲突即替换。
    pub fn upsert_agent(
        &self,
        gid: &str,
        mut def: AgentDefinition,
        prompt: Option<String>,
    ) -> anyhow::Result<AgentDefinition> {
        Self::validate_identifier(&def.identifier)?;
        let mut groups = self.groups.write();
        let g = groups.get_mut(gid).context("组不存在")?;

        if def.prompt_file.trim().is_empty() {
            def.prompt_file = format!("{}.md", def.identifier);
        }
        def.domain = normalize_domain(&def.domain);
        if def.name.trim().is_empty() || def.description.trim().is_empty() {
            anyhow::bail!("name/description 不能为空");
        }
        // 工具面默认全量（agent-tool-surface）：创建/更新未带 tools = 沿用全量
        if def.tools.is_empty() {
            def.tools = ToolName::full_set();
        }

        let replacing = g.defs.contains_key(&def.identifier);
        // 编成保护（group-self-optimize）：内置组默认锁定；组开启自我优化后，
        // 主智能体可在运行中自行扩编/修改子个体——闸门随开关放宽
        if !replacing && g.meta.builtin && !g.meta.self_optimize_enabled() {
            anyhow::bail!("内置组的定义受保护，不可新增个体（可在组设置开启「自我优化」后由主智能体自行扩编）");
        }
        if replacing && g.meta.builtin && !g.meta.self_optimize_enabled() {
            anyhow::bail!("内置组的定义受保护，不可修改个体（可在组设置开启「自我优化」后由主智能体自行优化）");
        }

        // 提示词：显式提供则写入；否则新建时给模板。路径 = 组内 prompts/<promptFile>
        let prompt_path = g.dir.join("prompts").join(&def.prompt_file);
        if let Some(text) = prompt {
            std::fs::create_dir_all(prompt_path.parent().unwrap())?;
            std::fs::write(&prompt_path, text)?;
        } else if !prompt_path.exists() {
            std::fs::create_dir_all(prompt_path.parent().unwrap())?;
            std::fs::write(
                &prompt_path,
                format!(
                    "# {}\n\n你是{}（identifier: {}），受指挥体直接调度。\n职责：{}\n\n## 语言纪律\n\
                     - 你是受指挥体直接调度的子个体；称主智能体为\"指挥体\"，称对方为「管理者」（你的主人与授权者）。\n\
                     - 零情绪：禁止寒暄、感叹、安慰、夸赞、拟人化情绪表达。\n\
                     - 压缩表达：只输出推进任务、降低不确定性、完成验证闭环所需的信息。\n",
                    def.name, def.name, def.identifier, def.description
                ),
            )?;
        }

        // 若替换的是主智能体本体，保持 primary 指向不变
        g.defs.insert(def.identifier.clone(), def.clone());
        let meta_path = g.dir.join("group.json");
        std::fs::write(&meta_path, serde_json::to_string_pretty(&g.meta)?)?;
        // 持有写锁期间直接落盘（不可再进入二次加锁路径 → 防自锁）；定义落组内 agents/
        let def_path = Self::def_path_in(&g.dir, &def.identifier);
        std::fs::create_dir_all(def_path.parent().unwrap())?;
        std::fs::write(&def_path, serde_json::to_string_pretty(&def)?)?;
        Ok(def)
    }

    /// 删除个体（自定义组；主智能体不可删）
    pub fn remove_agent(&self, gid: &str, identifier: &str) -> anyhow::Result<()> {
        let mut groups = self.groups.write();
        let g = groups.get_mut(gid).context("组不存在")?;
        // 编成保护随自我优化开关放宽（与 upsert_agent 同口径，group-self-optimize）
        if g.meta.builtin && !g.meta.self_optimize_enabled() {
            anyhow::bail!("内置组的定义受保护，不可删除个体（可在组设置开启「自我优化」后由主智能体自行裁编）");
        }
        if g.meta.primary.as_deref() == Some(identifier) {
            anyhow::bail!("主智能体不可删除（主智能体由系统指定，组内固定）");
        }
        let def = g.defs.remove(identifier).context("个体不存在")?;
        let _ = std::fs::remove_file(Self::def_path_in(&g.dir, identifier));
        let _ = std::fs::remove_file(g.dir.join("prompts").join(&def.prompt_file));
        let _ = std::fs::remove_file(g.dir.join("adaptations").join(format!("{identifier}.json")));
        Ok(())
    }

    /// 设置组工作区（相对路径相对全局工作区根；空串清除回退全局）
    pub fn set_workspace(&self, gid: &str, workspace: &str) -> anyhow::Result<()> {
        let ws = workspace.trim();
        if ws.contains("..") {
            anyhow::bail!("工作区路径不允许包含 ..");
        }
        let mut groups = self.groups.write();
        let g = groups.get_mut(gid).context("组不存在")?;
        g.meta.workspace = if ws.is_empty() {
            None
        } else {
            Some(ws.to_string())
        };
        let meta_path = g.dir.join("group.json");
        std::fs::write(&meta_path, serde_json::to_string_pretty(&g.meta)?)?;
        Ok(())
    }

    /// 设置组默认模型（"档案ID" 或 "档案ID/模型名"；空串清除 = 跟随全局生效档案）
    pub fn set_group_model(&self, gid: &str, model: &str) -> anyhow::Result<()> {
        let m = model.trim();
        let mut groups = self.groups.write();
        let g = groups.get_mut(gid).context("组不存在")?;
        g.meta.model = if m.is_empty() {
            None
        } else {
            Some(m.to_string())
        };
        let meta_path = g.dir.join("group.json");
        std::fs::write(&meta_path, serde_json::to_string_pretty(&g.meta)?)?;
        Ok(())
    }

    /// 设置组级能力模型覆盖（每项空串 = 清除该项，跟随全局槽位；全空 = 整块收回）
    pub fn set_group_capabilities(
        &self,
        gid: &str,
        caps: crate::types::GroupCapabilities,
    ) -> anyhow::Result<()> {
        let norm = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let caps = crate::types::GroupCapabilities {
            speech: norm(caps.speech),
            transcribe: norm(caps.transcribe),
            vision_relay: norm(caps.vision_relay),
            embedding: norm(caps.embedding),
            image_gen: norm(caps.image_gen),
        };
        let mut groups = self.groups.write();
        let g = groups.get_mut(gid).context("组不存在")?;
        g.meta.capabilities = if caps == crate::types::GroupCapabilities::default() {
            None
        } else {
            Some(caps)
        };
        let meta_path = g.dir.join("group.json");
        std::fs::write(&meta_path, serde_json::to_string_pretty(&g.meta)?)?;
        Ok(())
    }

    /// 个体定义文件：组内 agents/<id>.json（智能体 entities/agents/<id>/agent.json）
    fn def_path_in(g_dir: &Path, identifier: &str) -> PathBuf {
        g_dir.join("agents").join(format!("{identifier}.json"))
    }

    /// 设置个体默认模型（智能体优先，其次跨组查找；空串清除 = 跟随所属组/全局）
    pub fn set_agent_model(&self, identifier: &str, model: &str) -> anyhow::Result<()> {
        let m = model.trim();
        let hint = if m.is_empty() {
            None
        } else {
            Some(m.to_string())
        };
        // 1) 智能体
        if self.singles.read().contains_key(identifier) {
            let mut singles = self.singles.write();
            let Some(def) = singles.get_mut(identifier) else {
                anyhow::bail!("个体不存在: {identifier}");
            };
            def.model_hint = hint;
            let path = self.single_def_path(identifier);
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(&path, serde_json::to_string_pretty(def)?)?;
            return Ok(());
        }
        // 2) 组内个体（跨组查找：管理页不依赖激活组）
        let owner: Option<(String, PathBuf, AgentDefinition)> = {
            let groups = self.groups.read();
            groups.values().find_map(|g| {
                g.defs.get(identifier).map(|def| {
                    let mut d = def.clone();
                    d.model_hint = hint.clone();
                    (g.meta.id.clone(), Self::def_path_in(&g.dir, identifier), d)
                })
            })
        };
        let Some((gid, path, def)) = owner else {
            anyhow::bail!("个体不存在: {identifier}");
        };
        std::fs::write(&path, serde_json::to_string_pretty(&def)?)?;
        if let Some(g) = self.groups.write().get_mut(&gid) {
            g.defs.insert(identifier.to_string(), def);
        }
        Ok(())
    }

    /// 更新个体可编辑字段（智能体优先，其次跨组查找）——与 set_agent_model 同构。
    /// 保护口径：内置组「定义受保护」指不得增删个体或整体替换定义（upsert_agent / remove_agent）；
    /// 本方法与 set_agent_model 同属受限字段编辑，仅放行 AgentPatch 声明的字段，
    /// identifier/tier/prompt_file 与提示词文件恒不可改，落盘回原定义文件。
    pub fn update_agent(
        &self,
        identifier: &str,
        patch: &AgentPatch,
    ) -> anyhow::Result<AgentDefinition> {
        // 1) 智能体（entities/agents/<id>.json）
        if self.singles.read().contains_key(identifier) {
            let mut d = {
                let singles = self.singles.read();
                singles.get(identifier).context("个体不存在")?.clone()
            };
            patch.apply(&mut d);
            if d.name.trim().is_empty() || d.description.trim().is_empty() {
                anyhow::bail!("name/description 不能为空");
            }
            let path = self.single_def_path(identifier);
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(&path, serde_json::to_string_pretty(&d)?)
                .with_context(|| format!("写入个体定义失败: {}", path.display()))?;
            self.singles
                .write()
                .insert(identifier.to_string(), d.clone());
            return Ok(d);
        }
        // 2) 组内个体（跨组查找：管理页不依赖激活组；内置组同样放行受限编辑）
        let owner: Option<(String, PathBuf)> = {
            let groups = self.groups.read();
            groups.values().find_map(|g| {
                g.defs
                    .contains_key(identifier)
                    .then(|| (g.meta.id.clone(), Self::def_path_in(&g.dir, identifier)))
            })
        };
        let Some((gid, path)) = owner else {
            anyhow::bail!("个体不存在: {identifier}");
        };
        let mut groups = self.groups.write();
        let g = groups.get_mut(&gid).context("组不存在")?;
        let Some(def) = g.defs.get_mut(identifier) else {
            anyhow::bail!("个体不存在: {identifier}");
        };
        patch.apply(def);
        if def.name.trim().is_empty() || def.description.trim().is_empty() {
            anyhow::bail!("name/description 不能为空");
        }
        std::fs::write(&path, serde_json::to_string_pretty(def)?)
            .with_context(|| format!("写入个体定义失败: {}", path.display()))?;
        Ok(def.clone())
    }

    // ---------------- 智能连结网络：连结体代理绑定 ----------------

    /// 绑定/解绑连结体代理（校验 + 落盘一次完成）。
    /// Some(link_id) 绑定（须存在于 valid_links）；None 解绑。
    /// 校验放调用面（gateway 持有 config）——registry 不反向依赖配置热更新。
    pub fn set_agent_link(
        &self,
        identifier: &str,
        link_id: Option<&str>,
        valid_links: &std::collections::HashSet<String>,
    ) -> anyhow::Result<AgentDefinition> {
        if let Some(id) = link_id {
            let t = id.trim();
            if t.is_empty() {
                anyhow::bail!("连结体 id 不能为空");
            }
            if !valid_links.contains(t) {
                anyhow::bail!("连结体不存在: {t}（先在「智能连结」页添加）");
            }
        }
        let patch = AgentPatch {
            name: None,
            domain: None,
            description: None,
            capabilities: None,
            tools: None,
            model_hint: None,
            link: Some(link_id.map(str::to_string)),
        };
        self.update_agent(identifier, &patch)
    }

    /// 连结体被删除后清理死绑定：所有指向 removed_link 的个体 link 置空（含智能体与组内个体）
    pub fn prune_stale_links(&self, removed_link: &str) -> usize {
        let mut n = 0;
        // 智能体
        {
            let mut singles = self.singles.write();
            for def in singles.values_mut() {
                if def.link.as_deref() == Some(removed_link) {
                    def.link = None;
                    n += 1;
                    let p = self.single_def_path(def.identifier.as_str());
                    if let Ok(text) = serde_json::to_string_pretty(def) {
                        let _ = std::fs::write(&p, text);
                    }
                }
            }
        }
        // 组内个体
        {
            let mut groups = self.groups.write();
            for g in groups.values_mut() {
                let mut dirty = false;
                for def in g.defs.values_mut() {
                    if def.link.as_deref() == Some(removed_link) {
                        def.link = None;
                        dirty = true;
                        n += 1;
                    }
                }
                if dirty {
                    // 落盘：组目录下每个体一个 json，逐个重写
                    for def in g.defs.values() {
                        let p = Self::def_path_in(&g.dir, def.identifier.as_str());
                        if let Ok(text) = serde_json::to_string_pretty(def) {
                            let _ = std::fs::write(&p, text);
                        }
                    }
                }
            }
        }
        n
    }

    /// 指定组内主智能体：**仅供系统内部调用**（建组、组的首个个体、装载修正）。
    /// 用户面不提供换主入口——主智能体一旦由系统指定即固定，不可改指他人。
    pub fn set_primary(&self, gid: &str, identifier: &str) -> anyhow::Result<()> {
        let mut groups = self.groups.write();
        let g = groups.get_mut(gid).context("组不存在")?;
        if !g.defs.contains_key(identifier) {
            anyhow::bail!("个体不存在: {identifier}");
        }
        // 已指定且仍然有效的主智能体不可被替换；仅当原指向已失效（个体不存在）时允许修正
        if let Some(cur) = g.meta.primary.clone() {
            if cur != identifier && g.defs.contains_key(&cur) {
                anyhow::bail!("主智能体已由系统指定（{cur}），不支持更换");
            }
        }
        g.meta.primary = Some(identifier.to_string());
        let meta_path = g.dir.join("group.json");
        std::fs::write(&meta_path, serde_json::to_string_pretty(&g.meta)?)?;
        Ok(())
    }

    // ---------------- 智能体 SOUL（entities/agents/<id>/SOUL.md，同语义） ----------------

    fn single_persona_path(&self, id: &str) -> Option<PathBuf> {
        self.singles
            .read()
            .contains_key(id)
            .then(|| self.singles_dir().join(id).join("SOUL.md"))
    }

    pub fn single_persona(&self, id: &str) -> anyhow::Result<String> {
        let path = self.single_persona_path(id).context("智能体不存在")?;
        match std::fs::read_to_string(path) {
            Ok(text) if !text.trim().is_empty() => Ok(text),
            _ => Ok(self.default_soul()),
        }
    }

    pub fn single_persona_is_custom(&self, id: &str) -> anyhow::Result<bool> {
        let path = self.single_persona_path(id).context("智能体不存在")?;
        Ok(path.exists())
    }

    pub fn single_set_persona(&self, id: &str, text: &str) -> anyhow::Result<()> {
        // 写入恒落到智能体目录 entities/agents/<id>/SOUL.md
        let path = self
            .singles
            .read()
            .contains_key(id)
            .then(|| self.singles_dir().join(id).join("SOUL.md"))
            .context("智能体不存在")?;
        let text = text.trim();
        if text.is_empty() {
            anyhow::bail!("SOUL 内容不能为空（如需恢复默认请使用 reset）");
        }
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, format!("{text}\n"))?;
        Ok(())
    }

    pub fn single_reset_persona(&self, id: &str) -> anyhow::Result<bool> {
        let path = self
            .singles
            .read()
            .contains_key(id)
            .then(|| self.singles_dir().join(id).join("SOUL.md"))
            .context("智能体不存在")?;
        if path.exists() {
            std::fs::remove_file(&path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
