//! 个体注册表 —— 多组实现（docs/09）
//!
//! 组（Group）是隔离与切换的基本单位，统一布局 `agents/groups/<gid>/`：
//!   - SOUL.md（主智能体人格，用户面）+ MEMORY.md（组浅层记忆快照）
//!   - agents/*.json（成员定义，含主智能体）+ prompts/*.md（成员提示词）+ adaptations/（经验要点）
//!   - 内置默认组 = groups/default（builtin=true，定义受保护）；主智能体与独立智能体持 SOUL，子个体无人格层
//!   - 集群级共享：protocol/（协议）、skills/（技能）、default-soul.md（兜底人格）
//!
//! 切换组即热切换：指挥体的规划/派发/提示词/SOUL 全部落在激活组内。
//! 分布式实现可替换本文件（接口一致）。

use crate::types::{normalize_domain, AgentDefinition, GroupMeta, Tier, ToolName};
use anyhow::Context;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// task-local 声明：轮次作用域（会话归属键 → (组名, 单智能体 id)）。
/// lib.rs 的轮次入口在此设值；registry 读口（active_group / active_single 等）
/// 先查本值再回落全局激活态。
pub(crate) mod round_scope_decl {
    tokio::task_local! {
        pub(crate) static ROUND_SCOPE: Option<(String, Option<String>)>;
    }
}

pub struct GroupData {
    pub meta: GroupMeta,
    pub defs: HashMap<String, AgentDefinition>,
    pub dir: PathBuf,
}

/// 个体可编辑字段补丁（None = 沿用旧值）。identifier/tier/prompt_file 不可经此修改。
/// model_hint 语义与 set_agent_model 一致：Some("") = 清除（跟随所属组/全局）。
#[derive(Debug, Clone, Default)]
pub struct AgentPatch {
    pub name: Option<String>,
    pub domain: Option<String>,
    pub description: Option<String>,
    pub capabilities: Option<Vec<String>>,
    pub tools: Option<Vec<ToolName>>,
    pub model_hint: Option<String>,
    /// 连结体代理绑定（智能连结网络）：None = 不动；Some(None) = 解绑；
    /// Some(Some(link_id)) = 绑定（校验由调用面承担——gateway 持有 config）
    pub link: Option<Option<String>>,
}

impl AgentPatch {
    fn apply(&self, def: &mut AgentDefinition) {
        if let Some(v) = self.name.as_deref() {
            let t = v.trim();
            if !t.is_empty() {
                def.name = t.to_string();
            }
        }
        if let Some(v) = self.domain.as_deref() {
            def.domain = normalize_domain(v);
        }
        if let Some(v) = self.description.as_deref() {
            let t = v.trim();
            if !t.is_empty() {
                def.description = t.to_string();
            }
        }
        if let Some(list) = &self.capabilities {
            def.capabilities = list
                .iter()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
        }
        if let Some(list) = &self.tools {
            def.tools = list.clone();
        }
        if let Some(v) = self.model_hint.as_deref() {
            let t = v.trim();
            def.model_hint = if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            };
        }
        if let Some(v) = &self.link {
            def.link = v.clone();
        }
    }
}

pub struct LocalRegistry {
    dir: PathBuf,
    groups: RwLock<HashMap<String, GroupData>>,
    active: RwLock<String>,
    /// 智能体（独立于任何组，直接作为交互对象；entities/agents/）
    singles: RwLock<HashMap<String, AgentDefinition>>,
    /// 当前交互目标：None = 激活组；Some(id) = 智能体
    active_single: RwLock<Option<String>>,
    /// 工作目录（干活的项目）：显式设置后优先于组声明与全局根，智能体与智能体组通用
    active_workspace: RwLock<Option<String>>,
}

fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && s.len() <= 48
}

impl LocalRegistry {
    pub fn new(agents_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        let dir = agents_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(dir.join("agents"))?;
        let reg = LocalRegistry {
            dir,
            groups: RwLock::new(HashMap::new()),
            active: RwLock::new("exmachina".to_string()),
            singles: RwLock::new(HashMap::new()),
            active_single: RwLock::new(None),
            active_workspace: RwLock::new(None),
        };
        reg.reload()?;
        Ok(reg)
    }

    pub fn reload(&self) -> anyhow::Result<()> {
        let mut groups = HashMap::new();

        // 组统一放 agents/groups/<gid>/；内置默认组 = groups/default（builtin=true，定义受保护）
        let groups_dir = self.dir.join("groups");
        std::fs::create_dir_all(&groups_dir)?;
        let default_dir = groups_dir.join("exmachina");
        let default_meta_path = default_dir.join("group.json");
        let mut default_meta = if default_meta_path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&default_meta_path)?)
                .context("解析 groups/default/group.json 失败")?
        } else {
            let meta = GroupMeta {
                id: "exmachina".into(),
                name: "default".into(),
                description: "内置默认组（连结体）：指挥体 + 子个体集群（定义受保护）".into(),
                primary: Some("orchestrator".into()),
                workspace: None,
                model: None,
                capabilities: None,
                self_optimize: None,
                builtin: true,
                created_at: crate::types::now_iso(),
            };
            std::fs::create_dir_all(&default_dir)?;
            std::fs::write(&default_meta_path, serde_json::to_string_pretty(&meta)?)?;
            meta
        };
        // 显示名迁移：内置默认组显示名统一为 default（历史安装落盘为「智能连结」）
        if default_meta.name == "智能连结" {
            default_meta.name = "default".into();
            let _ = std::fs::write(
                &default_meta_path,
                serde_json::to_string_pretty(&default_meta)?,
            );
        }
        groups.insert(
            default_meta.id.clone(),
            self.load_group(default_meta, default_dir)?,
        );

        // 自定义组：groups/<gid>/（跳过默认组）
        if groups_dir.exists() {
            for entry in std::fs::read_dir(&groups_dir)?.filter_map(|e| e.ok()) {
                let gdir = entry.path();
                let meta_path = gdir.join("group.json");
                if !gdir.is_dir() || !meta_path.exists() {
                    continue;
                }
                let meta: GroupMeta =
                    serde_json::from_str(&std::fs::read_to_string(&meta_path)?)
                        .with_context(|| format!("解析组元数据失败: {}", meta_path.display()))?;
                if !valid_id(&meta.id) {
                    anyhow::bail!("非法组 id: {}", meta.id);
                }
                if meta.id == "exmachina" {
                    continue; // 默认组已在上方装载
                }
                groups.insert(meta.id.clone(), self.load_group(meta, gdir)?);
            }
        }
        if groups.is_empty() {
            anyhow::bail!("未装载到任何智能体组");
        }
        if !groups.contains_key("exmachina") {
            anyhow::bail!("缺少内置默认组");
        }

        // 智能体：entities/agents/*.json（独立交互对象，不属于任何组）
        let mut singles = HashMap::new();
        let singles_dir = self.dir.join("agents");
        if singles_dir.exists() {
            // 目录制：entities/agents/<id>/agent.json
            let mut def_files: Vec<PathBuf> = std::fs::read_dir(&singles_dir)?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_dir() && p.join("agent.json").is_file())
                .map(|p| p.join("agent.json"))
                .collect();
            // 兼容旧布局：<id>.json 平铺定义
            if let Ok(entries) = std::fs::read_dir(&singles_dir) {
                for e in entries.filter_map(|e| e.ok()).map(|e| e.path()) {
                    if e.is_file() && e.extension().map(|x| x == "json").unwrap_or(false) {
                        def_files.push(e);
                    }
                }
            }
            def_files.sort();
            for p in def_files {
                let def: AgentDefinition = serde_json::from_str(&std::fs::read_to_string(&p)?)
                    .with_context(|| format!("智能体定义校验失败: {}", p.display()))?;
                // 工具面默认全量（agent-tool-surface）：未声明 = 全量，显式声明才裁剪
                let mut def = def;
                if def.tools.is_empty() {
                    def.tools = ToolName::full_set();
                }
                singles.insert(def.identifier.clone(), def);
            }
        }
        // 显示名迁移：内置默认智能体显示名统一为 Machina（历史落盘曾为 default）
        if let Some(def) = singles.get_mut("machina") {
            if def.name == "default" {
                def.name = "Machina".into();
                let p = self.dir.join("agents").join("machina").join("agent.json");
                if p.is_file() {
                    let _ = std::fs::write(&p, serde_json::to_string_pretty(def)?);
                }
            }
        }
        *self.singles.write() = singles;
        let active_single =
            std::fs::read_to_string(self.dir.join("active_single")).unwrap_or_default();
        let active_single = active_single.trim();
        *self.active_single.write() = if active_single.is_empty() {
            None
        } else {
            Some(active_single.to_string())
        };

        // 工作目录持久化：agents/active_workspace（干活的项目，智能体与智能体组通用）
        let active_ws =
            std::fs::read_to_string(self.dir.join("active_workspace")).unwrap_or_default();
        let active_ws = active_ws.trim();
        *self.active_workspace.write() = if active_ws.is_empty() {
            None
        } else {
            Some(active_ws.to_string())
        };

        // 激活组持久化：agents/active_group
        let active_file = self.dir.join("active_group");
        let saved = std::fs::read_to_string(&active_file).unwrap_or_default();
        let active = saved.trim().to_string();
        let active = if groups.contains_key(&active) {
            active
        } else {
            "exmachina".to_string()
        };

        *self.groups.write() = groups;
        *self.active.write() = active;
        Ok(())
    }

    fn load_group(&self, meta: GroupMeta, dir: PathBuf) -> anyhow::Result<GroupData> {
        // 组布局：SOUL.md（主智能体人格）+ agents/（成员定义）+ prompts/（成员提示词）
        let mut defs = HashMap::new();
        let def_dir = dir.join("agents");
        if def_dir.exists() {
            let mut files: Vec<PathBuf> = std::fs::read_dir(&def_dir)?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
                .collect();
            files.sort();
            for path in files {
                let raw = std::fs::read_to_string(&path)
                    .with_context(|| format!("读取定义失败: {}", path.display()))?;
                let mut def: AgentDefinition = serde_json::from_str(&raw)
                    .with_context(|| format!("定义 schema 校验失败: {}", path.display()))?;
                def.domain = normalize_domain(&def.domain);
                // 工具面默认全量（agent-tool-surface）：未声明 tools = 全量
                // （serde 缺省空表）——显式声明才裁剪
                if def.tools.is_empty() {
                    def.tools = ToolName::full_set();
                }
                defs.insert(def.identifier.clone(), def);
            }
        }
        Ok(GroupData { meta, defs, dir })
    }

    #[allow(dead_code)]
    fn save_meta(&self, gid: &str) -> anyhow::Result<()> {
        let groups = self.groups.read();
        let g = groups.get(gid).context("组不存在")?;
        let path = g.dir.join("group.json");
        std::fs::write(&path, serde_json::to_string_pretty(&g.meta)?)?;
        Ok(())
    }
}

// ---------------------------------------------------------------- 领域分文件（6.1）
// groups（组/工作目录/模板/导入导出/组 SOUL）、agents（个体/提示词/增删改/
// 连结网络/智能体 SOUL/经验优化）、skills（技能包/协议层）按域拆出；
// 本文件保留 struct 定义与装载/持久化内核（new/reload/load_group/save_meta）。
mod agents;
mod groups;
mod skills;
