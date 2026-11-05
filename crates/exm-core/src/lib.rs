//! EXMACHINA 核心运行时（Rust）
//!
//! 组成：Store(SQLite) → MessageBus(全连结) → LocalRegistry(个体注册表)
//!      → LlmProvider(OpenAI 兼容 / Mock) → ToolGateway → AgentRuntime → Orchestrator
//!
//! 集群兼容：Bus / Registry / Scheduler 均为接口或可替换实现；
//! 单体实现先行，替换为分布式实现时上层业务零改动。

pub mod browser;
pub mod budget;
pub mod bus;
pub mod autonomy;
pub mod compaction;
pub mod computer;
pub mod config;
pub mod cron;
pub mod fsdb;
pub mod image_stash;
pub mod mcp;
pub mod mcp_server;
pub mod memory;
pub mod meta;
pub mod nexus;

pub mod orchestrator;
pub mod parse;
pub mod patch;
pub mod plan;
pub mod plugin;
pub mod provider;
pub mod registry;
pub mod remote;
pub mod round_trace;
pub mod runtime;
pub mod session_origin;
pub mod store;
pub mod store_sqlite;
pub mod task;
pub mod todo;
pub mod tools;
pub mod trash;
pub mod types;

use crate::bus::{InProcessBus, MessageBus};
use crate::config::ExmConfig;
use crate::cron::CronStore;
use crate::memory::{MemoryDraft, MemoryEntry, MemoryKind, MemoryStore, RecallHit};
use crate::orchestrator::{Orchestrator, ORCHESTRATOR_ID};
use crate::provider::{
    FailoverState, LlmProvider, MockLlmProvider, ModelPool, OpenAiCompatibleProvider,
    UnconfiguredProvider,
};
use crate::registry::LocalRegistry;
use crate::runtime::AgentRuntime;
use crate::store::Store;
use crate::tools::ToolGateway;
use crate::types::*;
use parking_lot::RwLock;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Auto Agent 运行态（实验性）：空转连续次数（退避用）+ 上次 Eval 的分钟键
#[derive(Debug, Default)]
pub struct AutoAgentState {
    idle_streak: u32,
    last_tick_minute: i64,
}

/// Meta Agent 巡检态（实验性 experiments.metaAgent）：独立于 Auto Agent 的
/// tick 状态（周期互不干扰）
pub struct MetaAgentState {
    pub last_tick_minute: i64,
}

/// 核心句柄：所有渠道（CLI / Gateway / 未来桌面端）共用同一实例
pub struct Core {
    pub bus: Arc<dyn MessageBus>,
    pub registry: Arc<LocalRegistry>,
    pub store: Arc<Store>,
    pub memory: Arc<MemoryStore>,
    pub events: broadcast::Sender<CoreEvent>,
    pub cron: Arc<CronStore>,
    /// Auto Agent 运行态（实验性 experiments.autoAgent）：空转计数 + 上次 tick 分钟
    auto_state: parking_lot::RwLock<AutoAgentState>,
    meta_state: parking_lot::RwLock<MetaAgentState>,
    /// 会话级运行串行：同一会话的并发输入排队（followup），不互踩任务图
    run_locks: parking_lot::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// 聊天流未读计数（流粒度，内存态）：智能体在流绑定会话产出回复且用户未打开
    /// 该会话时累积；打开即清零。未读是瞬时 UI 状态，重启归零合理（D5 单一事实源）。
    flow_unread: parking_lot::Mutex<std::collections::HashMap<String, u32>>,
    /// 一次性标记（agent-budget-audit 内联提示去重等）：key → 已置位。
    /// 进程内存态；key 由调用方自带语义前缀与生命周期（会话删除不清理——
    /// 预算提示「每进程每会话一次」符合预期）。
    once_flags: parking_lot::Mutex<std::collections::HashSet<String>>,
    /// 审批决定互斥（竞态 P1-1）：审批 id → 执行锁。decide 全程 try_lock 占位，
    /// 双击/多端并发时第二击等先到者收束后返回最新单据（先到者代执行语义），
    /// 命令只执行一次。锁条目不清理（审批单量有限，内存开销可忽略）。
    approval_locks: parking_lot::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// 会话级审批授权（approval-ergonomics）：会话 → {命令前缀 → ()}。
    /// 「本会话允许」档批准时写入；内存态（重启即清）、随会话删除清理。
    /// 红线：[root] 提权单与破坏性命令永不入表（写入侧校验）。
    session_grants: parking_lot::Mutex<std::collections::HashMap<String, std::collections::HashMap<String, ()>>>,
    /// 自引用弱句柄（approval-ergonomics）：attach_session_grant_probe 首次注入后
    /// 缓存；orchestrator 重建路径（rebind_plugin_tools）用它重挂授权探针。
    self_weak: parking_lot::RwLock<Option<std::sync::Weak<Core>>>,
    /// MCP 服务器池（与 Orchestrator 内工具网关共用同一实例）
    mcp: Arc<crate::mcp::McpRegistry>,
    /// 远程执行器（工作者池；网关在 serve 时注入）
    remote: parking_lot::RwLock<Option<Arc<dyn crate::remote::RemoteExecutor>>>,
    /// 插件管理器（plugins/ 目录装载 + 注册面）
    plugins: parking_lot::RwLock<Arc<crate::plugin::PluginManager>>,
    /// 视频动作代发执行器（video__act 审批批准后回调；网关侧注入）
    video_act_executor: parking_lot::RwLock<Option<crate::types::VideoActExecutor>>,
    orchestrator: RwLock<Arc<Orchestrator>>,
    config: Arc<RwLock<Arc<ExmConfig>>>,
}

/// 会话归属键 → 轮次作用域元组：`"single:<id>"` → (单智能体原组回落, Some(id))；
/// 组名 → (组名, None)。归属目标已被删除时返回 None（轮次回落全局激活态，
/// 与旧 align_to_session_group 的「目标不存在保持现状」语义一致）。
fn parse_session_scope(
    registry: &LocalRegistry,
    group_id: &str,
) -> Option<(String, Option<String>)> {
    if let Some(id) = group_id.strip_prefix("single:") {
        if registry.single_exists(id) {
            return Some((registry.active_group_global(), Some(id.to_string())));
        }
        return None;
    }
    let g = group_id.trim();
    if !g.is_empty() && registry.group_exists(g) {
        return Some((g.to_string(), None));
    }
    None
}

impl Core {
    pub fn create(workspace_root: impl AsRef<std::path::Path>) -> anyhow::Result<Self> {
        Self::with_config(ExmConfig::load(workspace_root))
    }

    pub fn with_config(cfg: ExmConfig) -> anyhow::Result<Self> {
        let store = Arc::new(Store::open(&cfg.data_dir)?);
        let memory = Arc::new(MemoryStore::open(&cfg.data_dir)?);
        let cron = Arc::new(CronStore::open(&cfg.data_dir)?);
        let auto_state = parking_lot::RwLock::new(AutoAgentState::default());
        let registry = Arc::new(LocalRegistry::new(&cfg.agents_dir)?);
        let bus: Arc<dyn MessageBus> = Arc::new(InProcessBus::new());
        let (events, _rx) = broadcast::channel(8192);
        let mcp_handle = crate::mcp::McpRegistry::shared();
        mcp_handle.configure(&cfg.mcp_servers);
        // 插件装载：plugins/ 目录扫描 + builtin 静态注册（exm-gateway 在 create 后补 video）
        let plugins = Arc::new(crate::plugin::PluginManager::new(
            cfg.workspace_root.join("plugins"),
        ));
        plugins.load_all(&cfg.plugins.state);
        // 收编既有机制为虚拟插件（管理面呈现；执行面零变化 = 零迁移）
        plugins.register_virtual("custom-tools", !cfg.tools.custom.is_empty());
        plugins.register_virtual(
            "builtin-resources",
            cfg.workspace_root.join("entities/skills").is_dir(),
        );
        let orchestrator = Arc::new(build_orchestrator(
            &cfg,
            &store,
            &registry,
            &events,
            &memory,
            Some(mcp_handle.clone()),
            None,
            Some(cron.clone()),
        )?);
        // 回接：插件声明式工具并入工具网关（custom 执行链全套复用）。
        // ToolGateway 构建在 build_orchestrator 内，此处经 orchestrator.tools 追加。
        {
            let plugin_tools: Vec<_> = plugins
                .declarative_tools()
                .into_iter()
                .map(|(_, _, t)| t)
                .collect();
            if !plugin_tools.is_empty() {
                let orch = orchestrator.clone();
                let gateway = orch.tools.clone();
                gateway.append_plugin_tools(plugin_tools);
            }
        }
        // 插件凭据解析面回接（T1 `${credentials.<key>}` 变量：查 config.plugins.credentials）。
        // config 真身 = 共享句柄：Core.config 字段与 gateway 闭包同源——
        // apply_config 换 config 时两者同步看到新凭据（热更新即时生效，无循环引用）。
        let config_shared: Arc<RwLock<Arc<ExmConfig>>> =
            Arc::new(RwLock::new(Arc::new(cfg.clone())));
        {
            let orch = orchestrator.clone();
            let gateway = orch.tools.clone();
            let snapshot = config_shared.clone();
            gateway.set_plugin_credentials_resolver(Box::new(move |pid, key| {
                snapshot
                    .read()
                    .plugins
                    .credentials
                    .get(pid)
                    .and_then(|m| m.get(key))
                    .cloned()
            }));
        }

        Ok(Core {
            bus,
            registry,
            store,
            memory,
            events,
            cron,
            auto_state,
            meta_state: parking_lot::RwLock::new(MetaAgentState { last_tick_minute: 0 }),
            run_locks: parking_lot::Mutex::new(std::collections::HashMap::new()),
            flow_unread: parking_lot::Mutex::new(std::collections::HashMap::new()),
            once_flags: parking_lot::Mutex::new(std::collections::HashSet::new()),
            approval_locks: parking_lot::Mutex::new(std::collections::HashMap::new()),
            session_grants: parking_lot::Mutex::new(std::collections::HashMap::new()),
            self_weak: parking_lot::RwLock::new(None),
            mcp: mcp_handle.clone(),
            remote: parking_lot::RwLock::new(None),
            plugins: parking_lot::RwLock::new(plugins),
            video_act_executor: parking_lot::RwLock::new(None),
            orchestrator: RwLock::new(orchestrator),
            config: config_shared,
        })
    }

    /// 装配会话授权查询（approval-ergonomics）：把 session_grants 查询面回接到
    /// 工具网关（gate_command 落单前短路）。必须经 Arc<Core> 调用——闭包持 Weak
    /// 引用避免与 orchestrator 成环（tools 由 orchestrator 持有,强引用即环）。
    /// orchestrator 重建路径（apply_config / set_remote 等）由 rebind_plugin_tools
    /// 尾部统一重挂。
    pub fn attach_session_grant_probe(self: &std::sync::Arc<Self>) {
        let weak = std::sync::Arc::downgrade(self);
        *self.self_weak.write() = Some(weak.clone());
        let orch = self.orchestrator.read().clone();
        orch.tools.set_session_grant_probe(std::sync::Arc::new(
            move |sid: &str, cmd: &str| {
                weak.upgrade().is_some_and(|c| c.session_granted(sid, cmd).is_some())
            },
        ));
    }

    /// 配置热更新（设置页）：重建 Provider / Runtime / Orchestrator
    pub fn apply_config(&self, cfg: ExmConfig) -> anyhow::Result<()> {
        cfg.save()?;
        self.mcp.configure(&cfg.mcp_servers);
        // 插件状态热更新：重扫目录并应用启停覆盖（进程插件重启在桥接层处理）
        self.plugins.read().load_all(&cfg.plugins.state);
        // 虚拟插件跟随内容出现/消失（CustomTool 增删、skills 目录有无）
        {
            let pm = self.plugins.read().clone();
            if cfg.tools.custom.is_empty() {
                pm.remove_entry("custom-tools");
            } else {
                pm.register_virtual("custom-tools", true);
            }
            if cfg.workspace_root.join("entities/skills").is_dir() {
                pm.register_virtual("builtin-resources", true);
            } else {
                pm.remove_entry("builtin-resources");
            }
        }
        let orch = build_orchestrator(
            &cfg,
            &self.store,
            &self.registry,
            &self.events,
            &self.memory,
            Some(self.mcp.clone()),
            self.remote(),
            Some(self.cron.clone()),
        )?;
        // 回接：插件工具面（T1 声明式 + T2 进程运行时）挂到新 orchestrator
        self.rebind_plugin_tools(&orch);
        *self.orchestrator.write() = Arc::new(orch);
        *self.config.write() = Arc::new(cfg);
        Ok(())
    }

    /// 插件管理器（注册面/管理端点共用）
    pub fn plugins(&self) -> Arc<crate::plugin::PluginManager> {
        self.plugins.read().clone()
    }

    // ---------------- 插件安装/更新（plugin-ecosystem；CLI 与 REST 共用面） ----------------

    /// 安装插件：staging 校验 → 落盘 → 热装载 → installation 来源持久化 →
    /// 声明式工具回接 ToolGateway。幂等冲突返回 Err（overwrite=true 走更新语义）。
    pub async fn plugin_install(
        &self,
        source: &crate::plugin::install::InstallSource,
        overwrite: bool,
    ) -> anyhow::Result<crate::plugin::install::InstallOutcome> {
        let plugins_dir = self.config().workspace_root.join("plugins");
        let pm = self.plugins();
        let host = crate::plugin::host_version_pub();
        let out = crate::plugin::install::install_from_source(
            &pm,
            source,
            &plugins_dir,
            &host,
            overwrite,
        )
        .await
        .map_err(anyhow::Error::msg)?;
        // 来源记录 + 声明式工具回接
        let mut cfg = (*self.config()).clone();
        cfg.plugins
            .installations
            .insert(out.id.clone(), source.to_installation());
        self.apply_config(cfg)?;
        Ok(out)
    }

    /// 更新插件：来源取 installation 记录（缺记录报无法更新）；已是最新不落盘；
    /// 更新保 data/ 与启用状态。返回 (是否更新, 消息)。
    pub async fn plugin_update(&self, id: &str) -> anyhow::Result<(bool, String)> {
        let cfg = self.config();
        let Some(inst) = cfg.plugins.installations.get(id).cloned() else {
            anyhow::bail!("插件 {id} 无安装来源记录（手工拷入的插件无法自动更新）");
        };
        let source = crate::plugin::install::installation_to_source(&inst);
        let plugins_dir = cfg.workspace_root.join("plugins");
        let pm = self.plugins();
        let host = crate::plugin::host_version_pub();
        // 本地版本
        let local_v = pm
            .get(id)
            .map(|e| e.manifest.version.clone())
            .unwrap_or_default();
        // 来源版本探测（staging 只读，不落盘）
        let remote_m = crate::plugin::install::probe_source_version(&source, &host)
            .await
            .map_err(anyhow::Error::msg)?;
        let remote_v = remote_m.version.trim().to_string();
        if !remote_v.is_empty()
            && !local_v.trim().is_empty()
            && !crate::plugin::version_newer(&remote_v, &local_v)
        {
            return Ok((false, format!("{id} 已是最新（{local_v}）")));
        }
        // 记录更新前启用状态（更新后保持）
        let was_enabled = pm.get(id).map(|e| e.enabled).unwrap_or(true);
        crate::plugin::install::update_from_source(&pm, &source, &plugins_dir, &host)
            .await
            .map_err(anyhow::Error::msg)?;
        // 启用状态保持 + 持久化
        pm.set_enabled(id, was_enabled);
        let mut cfg = (*self.config()).clone();
        cfg.plugins.state.insert(id.to_string(), was_enabled);
        cfg.plugins.installations.insert(id.to_string(), inst);
        self.apply_config(cfg)?;
        Ok((true, format!("{id} 已更新到 {remote_v}（原 {local_v}）")))
    }

    /// 卸载插件（与 REST DELETE 同语义，CLI 用）：禁用 → 终止进程（provider 摘除）→
    /// 删目录 → 清 config 状态/来源/凭据。
    pub fn plugin_uninstall(&self, id: &str) -> anyhow::Result<bool> {
        let pm = self.plugins();
        let Some(e) = pm.get(id) else {
            anyhow::bail!("插件不存在：{id}");
        };
        if e.form == "builtin" || e.form == "virtual" {
            anyhow::bail!("{} 插件随宿主发布或收编，不可卸载（可禁用）", e.form);
        }
        let Some(dir) = e.dir.clone() else {
            anyhow::bail!("插件目录未知，无法卸载");
        };
        let plugins_root = self.config().workspace_root.join("plugins");
        if !dir.starts_with(&plugins_root) || dir == plugins_root {
            anyhow::bail!("插件目录越界，拒绝卸载");
        }
        pm.set_enabled(id, false);
        pm.remove_provider(id);
        pm.remove_entry(id);
        std::fs::remove_dir_all(&dir)?;
        let mut cfg = (*self.config()).clone();
        cfg.plugins.state.remove(id);
        cfg.plugins.installations.remove(id);
        cfg.plugins.credentials.remove(id);
        self.apply_config(cfg)?;
        Ok(true)
    }

    // ---------------- 记忆系统便捷方法（组感知，docs/09 §6） ----------------

    /// 会话的记忆分区（外部通道「记忆隔离」）：None = 与同组共享。
    pub fn memory_partition_for_session(&self, session_id: &str) -> Option<String> {
        self.store
            .get_session(session_id)
            .ok()
            .flatten()
            .and_then(|s| s.memory_partition)
    }

    /// 会话的有效记忆命名空间：有分区用分区，否则用当前组。
    /// 注意：这里返回的是**记忆维度**的组键，不改变 registry 的激活组。
    pub fn memory_group_for_session(&self, session_id: &str) -> String {
        self.memory_partition_for_session(session_id)
            .unwrap_or_else(|| self.active_group())
    }

    pub fn memory_recall(
        &self,
        query: &str,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<RecallHit>> {
        let cfg = self.config();
        let gid = self.registry.active_group();
        self.memory.recall(
            query,
            limit.unwrap_or(cfg.memory_recall_limit),
            None,
            Some(&gid),
            None,
        )
    }

    /// 全量检索（管理视角：群体 + 所有个体私有；范围 = 激活组 + 全局条目）
    pub fn memory_recall_all(
        &self,
        query: &str,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<RecallHit>> {
        let gid = self.registry.active_group();
        self.memory.recall_all(
            query,
            limit.unwrap_or(self.config().memory_recall_limit),
            Some(&gid),
            None,
        )
    }

    /// 个体检索：该智能体私有 + 群体共享（范围 = 激活组 + 全局条目）
    pub fn memory_recall_for_agent(
        &self,
        agent_id: &str,
        query: &str,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<RecallHit>> {
        let cfg = self.config();
        let gid = self.registry.active_group();
        self.memory.recall_for_agent(
            agent_id,
            query,
            limit.unwrap_or(cfg.memory_recall_limit),
            Some(&gid),
            None,
        )
    }

    /// 写入记忆：自动归属激活组（组内可见，其他组不可见）
    pub fn memory_remember(&self, mut draft: MemoryDraft) -> anyhow::Result<MemoryEntry> {
        if draft.group_id.is_none() {
            draft.group_id = Some(self.registry.active_group());
        }
        let entry = self.memory.remember(&draft)?;
        self.render_memory_md()?;
        Ok(entry)
    }

    /// 写入全局记忆（跨组可见）
    pub fn memory_remember_global(&self, draft: MemoryDraft) -> anyhow::Result<MemoryEntry> {
        let mut draft = draft;
        draft.group_id = None;
        let entry = self.memory.remember(&draft)?;
        self.render_memory_md()?;
        Ok(entry)
    }

    pub fn memory_list(
        &self,
        kind: Option<MemoryKind>,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.memory.list(kind, limit)
    }

    /// 按归属过滤列表：Some(id) => 该个体私有 + 群体共享；组范围 = 激活组 + 全局条目
    pub fn memory_list_filtered(
        &self,
        kind: Option<MemoryKind>,
        agent: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let gid = self.registry.active_group();
        self.memory.list_filtered(kind, agent, limit, Some(&gid))
    }

    pub fn memory_stats(&self) -> anyhow::Result<serde_json::Value> {
        self.memory.stats()
    }

    pub fn memory_pin(&self, id: &str, pinned: bool) -> anyhow::Result<()> {
        self.memory.pin(id, pinned)?;
        self.render_memory_md()?;
        Ok(())
    }

    pub fn memory_forget(&self, id: &str) -> anyhow::Result<()> {
        self.memory.forget(id)?;
        self.render_memory_md()?;
        Ok(())
    }

    pub fn memory_reindex(&self) -> anyhow::Result<usize> {
        self.memory.reindex()
    }

    /// 记忆整理：时间衰减 + 重新渲染 memory.md
    pub fn memory_decay(&self, floor: f64) -> anyhow::Result<usize> {
        let cfg = self.config();
        let changed = self.memory.decay(cfg.memory_half_life_days, floor)?;
        self.render_memory_md()?;
        Ok(changed)
    }

    /// 重新生成基础记忆文件（memory.md，全局浅层视图）；
    /// 同时刷新：每组一份 `groups/<gid>/MEMORY.md`（组浅层记忆）、每智能体一份 `entities/agents/<id>/MEMORY.md`。
    /// 深层记忆始终在 MemoryStore（数据库检索/衰减/压缩）——MEMORY.md 只是容量受限的人读快照。
    pub fn render_memory_md(&self) -> anyhow::Result<usize> {
        let cfg = self.config();
        let n = self.memory.render_memory_md(&cfg.memory_md_path)?;
        let gid = self.registry.active_group();
        for meta in self.registry.list_groups() {
            if let Some(dir) = self.registry.group_dir(&meta.id) {
                let _ = self
                    .memory
                    .render_group_memory_md(&meta.id, dir.join("MEMORY.md"));
            }
        }
        for s in self.registry.list_singles() {
            let dir = self.registry.singles_dir_pub().join(&s.identifier);
            let _ = self.memory.render_agent_memory_md(
                &s.identifier,
                Some(&gid),
                dir.join("MEMORY.md"),
            );
        }
        Ok(n)
    }

    // ---------------- 人设（说话风格） ----------------

    pub fn persona(&self, identifier: &str) -> anyhow::Result<String> {
        self.registry.persona(identifier)
    }

    pub fn persona_is_custom(&self, identifier: &str) -> anyhow::Result<bool> {
        self.registry.persona_is_custom(identifier)
    }

    pub fn set_persona(&self, identifier: &str, text: &str) -> anyhow::Result<()> {
        self.registry.set_persona(identifier, text)
    }

    pub fn reset_persona(&self, identifier: &str) -> anyhow::Result<bool> {
        self.registry.reset_persona(identifier)
    }

    /// 统一对话入口：会话级串行（运行中的后续输入排队为 followup，收束后依次处理；
    /// 排队窗内到达的多条输入由 handle_user_message 的 collect 合并为一轮）
    /// 语音转写（全局生效档案，whisper 系模型；音频字节 → 文本）
    pub async fn transcribe(&self, audio: &[u8], filename: &str) -> anyhow::Result<String> {
        let orch = self.orchestrator();
        orch.transcribe_audio(audio, filename).await
    }

    /// 语音合成（全局生效档案，tts 系模型；文本 → mp3 字节）
    pub async fn speak(&self, text: &str) -> anyhow::Result<Vec<u8>> {
        let orch = self.orchestrator();
        orch.speak_audio(text).await
    }

    /// memory.md 超限自主压缩（LLM 简略 + 旧文归档）；返回 (压缩前, 压缩后) 字数
    pub async fn compact_memory_md(&self) -> anyhow::Result<(usize, usize)> {
        let max = self.config().memory_md_max_chars;
        let orch = self.orchestrator();
        orch.compact_memory_md(max).await
    }

    /// 会话 token 用量：优先真实用量（provider 上报），无记录时退回字符估算（全部消息陈述字符 / 4）
    pub fn session_tokens_estimate(&self, session_id: &str) -> u64 {
        let (prompt, completion, calls) = self.store.usage_total(session_id);
        if calls > 0 {
            return prompt + completion;
        }
        self.store
            .list_messages(session_id, 500)
            .unwrap_or_default()
            .iter()
            .map(|m| {
                m.statements
                    .iter()
                    .map(|s| s.text.chars().count() as u64)
                    .sum::<u64>()
                    / 4
            })
            .sum()
    }

    /// 会话用量明细：(prompt, completion, 调用次数)
    pub fn session_usage(&self, session_id: &str) -> (u64, u64, usize) {
        self.store.usage_total(session_id)
    }

    // ---------------- 文件检查点（写前快照的回看与回滚） ----------------

    fn checkpoints_dir(&self) -> std::path::PathBuf {
        self.config()
            .workspace_root
            .join(".exmachina")
            .join("checkpoints")
    }

    /// 检查点清单：(日期, 相对路径, 字节数)，按日期倒序
    pub fn list_checkpoints(&self) -> Vec<(String, String, u64)> {
        let root = self.checkpoints_dir();
        let mut out: Vec<(String, String, u64)> = Vec::new();
        let Ok(days) = std::fs::read_dir(&root) else {
            return out;
        };
        for day in days.flatten() {
            if !day.path().is_dir() {
                continue;
            }
            let date = day.file_name().to_string_lossy().to_string();
            let mut stack = vec![day.path()];
            while let Some(dir) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for e in entries.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else if let Ok(rel) = p.strip_prefix(&root).map(|r| r.to_path_buf()) {
                        // 去掉前缀的 <日期>/ 段
                        let rel_str = rel
                            .components()
                            .skip(1)
                            .map(|c| c.as_os_str().to_string_lossy().to_string())
                            .collect::<Vec<_>>()
                            .join("/");
                        let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                        out.push((date.clone(), rel_str, size));
                    }
                }
            }
        }
        out.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        out
    }

    /// 工作区文件保存（WebUI 编辑器写入口径）：路径防 `..` 兜底、写前检查点、工具审计。
    /// 与写类工具同规则（当日首次改动前的版本落 `.exmachina/checkpoints/`，供回滚）。
    /// 返回结果摘要；越界/非法路径报错。网关侧已做工作区规范化校验，此处兜底拒绝。
    pub fn save_workspace_file(&self, rel: &str, content: &str) -> anyhow::Result<String> {
        let norm = rel.replace('\\', "/").trim_matches('/').to_string();
        if norm.is_empty()
            || std::path::Path::new(rel.trim()).is_absolute()
            || norm.split('/').any(|seg| seg == "..")
        {
            anyhow::bail!("路径越界（仅允许工作区内的相对路径）: {rel}");
        }
        let root = self.config().workspace_root.clone();
        let target = root.join(&norm);
        if target.is_file() {
            let day: String = crate::types::now_iso().chars().take(10).collect();
            let dst = root
                .join(".exmachina")
                .join("checkpoints")
                .join(&day)
                .join(&norm);
            if !dst.exists() {
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(&target, &dst)?;
            }
        } else if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, content)?;
        // 审计：agent_id = webui，与智能体审计同账本、来源可区分
        let _ = self.store.audit_tool(
            "webui",
            "filesystem",
            &serde_json::json!({ "op": "write", "path": norm, "via": "editor" }),
            "编辑器保存",
            0,
        );
        Ok(format!(
            "已保存 {norm}（{} 字节，写前检查点已记录）",
            content.len()
        ))
    }

    // ---------------- 通道身份与配对码（gateway 身份闸门共用） ----------------

    pub fn identity_of(&self, channel: &str, external_id: &str) -> Option<UserIdentity> {
        self.store.get_identity(channel, external_id).ok().flatten()
    }

    pub fn identity_of_id(&self, id: &str) -> Option<UserIdentity> {
        self.store.get_identity_by_id(id).ok().flatten()
    }

    pub fn identities(&self) -> Vec<UserIdentity> {
        self.store.list_identities().unwrap_or_default()
    }

    pub fn save_identity(&self, identity: &UserIdentity) -> anyhow::Result<()> {
        self.store.put_identity(identity)
    }

    pub fn drop_identity(&self, id: &str) -> anyhow::Result<()> {
        self.store.delete_identity(id)
    }

    /// 撤销配对码（控制台删除；幂等：不存在也成功）
    pub fn drop_pairing_code(&self, code: &str) -> anyhow::Result<()> {
        self.store.delete_pairing_code(code)
    }

    /// 清理全部过期配对码，返回清理数（周期任务与控制台「一键清理」共用）
    pub fn purge_expired_pairing_codes(&self) -> usize {
        let now = now_ms();
        let mut n = 0usize;
        for c in self.pairing_codes() {
            if now > c.expires_at_ms && self.drop_pairing_code(&c.code).is_ok() {
                n += 1;
            }
        }
        n
    }

    // ---------------- 聊天流（chat flow）：外部通道的对话单元与会话绑定 ----------------

    /// 流 id 口径：`{channelId}:{chatKey}`
    pub fn flow_id(channel_id: &str, chat_key: &str) -> String {
        format!("{channel_id}:{chat_key}")
    }

    /// 取流（不存在返回 None）
    pub fn channel_flow(&self, channel_id: &str, chat_key: &str) -> Option<ChannelFlow> {
        self.store
            .get_channel_flow(&Self::flow_id(channel_id, chat_key))
            .ok()
            .flatten()
    }

    pub fn channel_flows(&self, channel_id: Option<&str>) -> Vec<ChannelFlow> {
        self.store.list_channel_flows(channel_id).unwrap_or_default()
    }

    /// 保存流（upsert）
    pub fn save_channel_flow(&self, flow: &ChannelFlow) -> anyhow::Result<()> {
        self.store.put_channel_flow(flow)
    }

    pub fn drop_channel_flow(&self, id: &str) -> anyhow::Result<()> {
        self.store.delete_channel_flow(id)
    }

    /// 流未读 +1（智能体在流绑定会话产出回复时调用；spawn_reply 收束点）。
    /// 流不存在时忽略（未绑定流的会话无未读语义）。
    /// 一次性标记消费（agent-budget-audit 内联提示去重）：首次返回 true 并置位，
    /// 此后 false。进程内存态。
    pub fn consume_once_flag(&self, key: &str) -> bool {
        self.once_flags.lock().insert(key.to_string())
    }

    pub fn bump_flow_unread(&self, channel_id: &str, chat_key: &str) {
        let id = Self::flow_id(channel_id, chat_key);
        if self.store.get_channel_flow(&id).ok().flatten().is_none() {
            return;
        }
        let mut m = self.flow_unread.lock();
        let c = m.entry(id).or_insert(0);
        *c = c.saturating_add(1);
    }

    /// 流未读清零（用户打开该流绑定会话时调用；REST /flows/:id/read）
    pub fn clear_flow_unread(&self, channel_id: &str, chat_key: &str) {
        self.flow_unread
            .lock()
            .remove(&Self::flow_id(channel_id, chat_key));
    }

    /// 单条流未读数（列表输出用）
    pub fn flow_unread_count(&self, channel_id: &str, chat_key: &str) -> u32 {
        self.flow_unread
            .lock()
            .get(&Self::flow_id(channel_id, chat_key))
            .copied()
            .unwrap_or(0)
    }

    /// 流绑定的活跃会话；不存在或已删 → None（调用方开新会话后回写绑定）
    pub fn flow_active_session(&self, channel_id: &str, chat_key: &str) -> Option<String> {
        let flow = self.channel_flow(channel_id, chat_key)?;
        let sid = flow.active_session?;
        // 会话可能已被用户在控制台删除：失效绑定顺带清理，下次重建
        if self.store.get_session(&sid).ok().flatten().is_none() {
            let _ = self.drop_channel_flow(&flow.id);
            return None;
        }
        Some(sid)
    }

    /// 把流绑定到会话（流不存在则创建；顺带刷最后活跃）
    pub fn bind_flow_session(
        &self,
        channel_id: &str,
        chat_key: &str,
        session_id: &str,
    ) -> anyhow::Result<()> {
        self.bind_flow_session_named(channel_id, chat_key, session_id, None, None)
    }

    /// 流名维护（会话管理可用性修复 2026-11）：流的展示名（群名 / 对端昵称）
    /// 只在为空时回填——不覆盖既有值（可能是用户改过的自定义名，或更早来源
    /// 的更好名称）。适配器每次入站都带最新名调用，首次到达即落名。
    pub fn ensure_flow_named(
        &self,
        channel_id: &str,
        chat_key: &str,
        display_name: &str,
        is_group: bool,
    ) {
        if display_name.trim().is_empty() {
            return;
        }
        let Some(mut flow) = self.channel_flow(channel_id, chat_key) else {
            return;
        };
        if flow.display_name.trim().is_empty() {
            flow.display_name = display_name.trim().to_string();
            flow.is_group = flow.is_group || is_group;
            let _ = self.store.put_channel_flow(&flow);
        }
    }

    /// bind_flow_session 带名变体：建流时直接落展示名（避免先建空名流再补名
    /// 的窗口期——控制台流列表显示裸 chat_key 不可辨认）
    pub fn bind_flow_session_named(
        &self,
        channel_id: &str,
        chat_key: &str,
        session_id: &str,
        display_name: Option<&str>,
        is_group: Option<bool>,
    ) -> anyhow::Result<()> {
        let id = Self::flow_id(channel_id, chat_key);
        let mut flow = self
            .channel_flow(channel_id, chat_key)
            .unwrap_or(ChannelFlow {
                id: id.clone(),
                channel_id: channel_id.to_string(),
                chat_key: chat_key.to_string(),
                display_name: display_name.unwrap_or("").trim().to_string(),
                is_group: is_group.unwrap_or(false),
                active_session: None,
                last_message: String::new(),
                last_active_at: String::new(),
                created_at: now_iso(),
            });
        // 既有流只补空名（不覆盖用户自定义）
        if flow.display_name.trim().is_empty() {
            if let Some(n) = display_name {
                flow.display_name = n.trim().to_string();
            }
        }
        if let Some(g) = is_group {
            flow.is_group = flow.is_group || g;
        }
        flow.active_session = Some(session_id.to_string());
        flow.last_active_at = now_iso();
        self.store.put_channel_flow(&flow)
    }

    /// 流的消息预览回写（入站消息落库后调用；不建流——只有 begin 才建）
    pub fn touch_flow_message(
        &self,
        channel_id: &str,
        chat_key: &str,
        preview: &str,
    ) -> anyhow::Result<()> {
        if let Some(mut flow) = self.channel_flow(channel_id, chat_key) {
            flow.last_message = preview.chars().take(120).collect();
            flow.last_active_at = now_iso();
            self.store.put_channel_flow(&flow)?;
        }
        Ok(())
    }

    /// 签发一次性配对码（8 位短码，TTL 由调用方给）
    pub fn issue_pairing_code(
        &self,
        note: &str,
        platform: Option<&str>,
        ttl_secs: u64,
    ) -> anyhow::Result<PairingCode> {
        self.issue_pairing_code_role(note, platform, ttl_secs, "member")
    }

    /// 带预设角色签发（admin = 管理者配对码；兑换即落 admin）
    pub fn issue_pairing_code_role(
        &self,
        note: &str,
        platform: Option<&str>,
        ttl_secs: u64,
        role: &str,
    ) -> anyhow::Result<PairingCode> {
        let code = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let c = PairingCode {
            code: code.clone(),
            note: note.to_string(),
            channel_platform: platform.map(|s| s.to_string()),
            created_by: "console".into(),
            created_at: now_iso(),
            expires_at_ms: now_ms() + ttl_secs * 1000,
            role: if role == "admin" {
                "admin".into()
            } else {
                "member".into()
            },
        };
        self.store.put_pairing_code(&c)?;
        Ok(c)
    }

    pub fn pairing_codes(&self) -> Vec<PairingCode> {
        self.store.list_pairing_codes().unwrap_or_default()
    }

    /// 直接落一张配对码（迁移 / 测试 / 控制台定向写入）
    pub fn save_pairing_code(&self, code: &PairingCode) -> anyhow::Result<()> {
        self.store.put_pairing_code(code)
    }

    /// 兑换配对码 → 绑定身份（单次有效 + 过期校验；channel 为通道账号 id）
    pub fn redeem_pairing(
        &self,
        code: &str,
        channel: &str,
        external_id: &str,
        display_name: &str,
        default_role: &str,
    ) -> anyhow::Result<UserIdentity> {
        let c = self
            .store
            .consume_pairing_code(code.trim())
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .ok_or_else(|| anyhow::anyhow!("配对码无效或已被使用"))?;
        if now_ms() > c.expires_at_ms {
            anyhow::bail!("配对码已过期，请重新签发");
        }
        if let Some(p) = &c.channel_platform {
            if !channel.starts_with(p.as_str()) {
                anyhow::bail!("配对码限定平台 {p}，当前通道不可用");
            }
        }
        let identity = UserIdentity {
            id: format!("{channel}:{external_id}"),
            channel: channel.to_string(),
            external_id: external_id.to_string(),
            display_name: display_name.to_string(),
            // 预设角色优先（签发时指定）；存量码无角色落 default_role（member）
            role: if c.role.trim().is_empty() {
                default_role.to_string()
            } else {
                c.role.clone()
            },
            paired_at: now_iso(),
            note: c.note.clone(),
        };
        self.store.put_identity(&identity)?;
        Ok(identity)
    }

    /// 供网关在执行前拒绝时发运行错误事件（配额 / 治理类短路，走既有回帖通路）
    pub fn emit_run_error(&self, session_id: &str, message: &str) {
        let _ = self.events.send(crate::types::CoreEvent {
            kind: "run.error".into(),
            session_id: session_id.to_string(),
            payload: serde_json::json!({ "message": message }),
        });
    }

    /// 入站消毒审计事件（inbound-sanitization）：广播 + 落事件库（事件页可查）。
    /// payload 只记模式摘要（通道/账号/模式/处置），MUST NOT 含原文全文。
    pub fn emit_sanitize_finding(&self, payload: serde_json::Value) {
        self.orchestrator.read().emit("", "sanitize.finding", payload);
    }

    /// 回滚检查点：`date` 为空取最近一份；返回被覆盖的文件相对路径
    pub fn restore_checkpoint(&self, date: Option<&str>, rel: &str) -> anyhow::Result<String> {
        let target = self
            .config()
            .workspace_root
            .join(".exmachina")
            .join("checkpoints");
        let day = match date {
            Some(d) if !d.trim().is_empty() => d.trim().to_string(),
            _ => self
                .list_checkpoints()
                .first()
                .map(|(d, _, _)| d.clone())
                .ok_or_else(|| anyhow::anyhow!("没有可用的检查点"))?,
        };
        let src = target.join(&day).join(rel);
        if !src.is_file() {
            anyhow::bail!("检查点不存在：{day}/{rel}");
        }
        let dst = self.config().workspace_root.join(rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&src, &dst)?;
        Ok(format!(
            "已回滚 {} → {rel}（来自检查点 {day}）",
            dst.display()
        ))
    }

    // ---------------- 会话历史管控（undo / 编辑重发 / fork，组 9） ----------------

    /// 会话运行锁句柄（外部历史操作先 try_lock：处理中一律拒绝）
    fn run_lock(&self, session_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.run_locks
            .lock()
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// 快照定位：turn 的消息游标（turn <= 0 = 空，即回退到会话起点）
    fn cursor_at_turn(&self, session_id: &str, turn: usize) -> anyhow::Result<usize> {
        if turn == 0 {
            return Ok(0);
        }
        let snaps = self.store.list_turn_snapshots(session_id)?;
        snaps
            .iter()
            .find(|s| s.turn == turn)
            .map(|s| s.message_count)
            .ok_or_else(|| anyhow::anyhow!("轮次快照不存在: turn {turn}（该轮未落快照或已被裁剪）"))
    }

    /// 消息撤销（9.2）：空闲强校验 → 游标回退（截断部分归档可查）→ 上下文重建
    /// → 可选检查点联动回滚 → 审计事件。返回 {archived, restored, cursor}。
    pub async fn undo_session(
        &self,
        session_id: &str,
        upto_turn: usize,
        restore_files: bool,
    ) -> anyhow::Result<serde_json::Value> {
        let lock = self.run_lock(session_id);
        // 空闲校验：处理中拒绝（spec 硬性要求）
        let _guard = lock
            .try_lock()
            .map_err(|_| anyhow::anyhow!("会话正在处理中，请等待当前任务完成后再撤销"))?;
        self.store
            .get_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))?;
        let cursor = self.cursor_at_turn(session_id, upto_turn)?;

        // 游标回退 + 归档（spec：被截断内容保留归档视图）
        let archived = self.store.truncate_messages(session_id, cursor)?;

        // 上下文重建：截断后滚动摘要失效（摘要覆盖越过游标即重置，按截断后历史重建）
        let session = self.store.get_session(session_id)?.unwrap();
        if session.summary_upto.map(|u| u > cursor).unwrap_or(false) {
            let _ = self.store.update_compaction(session_id, "", 0);
        }

        // 检查点联动回滚：该轮之后各快照登记的文件变更，按轮次倒序恢复
        let mut restored: Vec<String> = Vec::new();
        if restore_files {
            let mut later: Vec<TurnSnapshot> = self
                .store
                .list_turn_snapshots(session_id)?
                .into_iter()
                .filter(|s| s.turn > upto_turn && !s.checkpoint_ids.is_empty())
                .collect();
            later.sort_by_key(|s| std::cmp::Reverse(s.turn)); // 后轮先回滚（后者覆盖前者的场景）
            for snap in later {
                for cp in snap.checkpoint_ids.iter().rev() {
                    let Some((date, rel)) = cp.split_once('/') else {
                        continue;
                    };
                    match self.restore_checkpoint(Some(date), rel) {
                        Ok(_) => restored.push(cp.clone()),
                        Err(e) => eprintln!("[undo] 检查点恢复失败 {cp}: {e}"),
                    }
                }
            }
        }

        // 孤儿快照清理 + 审计 + 事件
        let _ = self.store.prune_turn_snapshots_after(session_id, upto_turn);
        let _ = self.store.audit_tool(
            "webui",
            "session_undo",
            &serde_json::json!({ "session": session_id, "uptoTurn": upto_turn, "restoreFiles": restore_files }),
            &format!("撤销至第 {upto_turn} 轮：归档 {archived} 条，联动回滚 {} 个文件", restored.len()),
            0,
        );
        let payload = serde_json::json!({
            "session": session_id, "uptoTurn": upto_turn, "cursor": cursor,
            "archived": archived, "restored": restored,
        });
        let _ = self.events.send(CoreEvent {
            kind: "session.undo".into(),
            session_id: session_id.to_string(),
            payload: payload.clone(),
        });
        Ok(payload)
    }

    /// 编辑重发（9.3）：撤销至该轮之前（原内容归档可查）后以新内容触发新一轮处理。
    /// `turn` 为被编辑的用户消息所在轮次。
    pub async fn edit_resend(
        &self,
        session_id: &str,
        turn: usize,
        new_text: &str,
    ) -> anyhow::Result<serde_json::Value> {
        if new_text.trim().is_empty() {
            anyhow::bail!("新内容不能为空");
        }
        let upto = turn.saturating_sub(1);
        let info = self.undo_session(session_id, upto, true).await?;
        // 以新内容重新执行（原内容已在归档中可查）
        self.chat(session_id, new_text).await?;
        Ok(serde_json::json!({ "undone": info, "resent": true, "turn": turn }))
    }

    /// 会话分支派生（9.4）：新会话携带截至该轮的完整历史副本与派生来源标注；
    /// 组归属（= 权限与成员上下文）继承，原会话不变。返回新会话。
    pub fn fork_session(&self, session_id: &str, upto_turn: usize) -> anyhow::Result<Session> {
        let src = self
            .store
            .get_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))?;
        let cursor = self.cursor_at_turn(session_id, upto_turn)?;
        let now = now_iso();
        let fork = Session {
            id: new_id(),
            title: format!("{}（分支·至第 {upto_turn} 轮）", src.title),
            status: "active".into(),
            origin: None,
            origin_label: None,
            group_id: src.group_id.clone(), // 权限与组继承
            memory_partition: src.memory_partition.clone(), // 记忆分区随会话继承
            rolling_summary: None,          // 摘要按副本历史重建
            summary_upto: None,
            parent_session: Some(src.id.clone()),
            parent_upto: Some(upto_turn),
            last_message_preview: None,
            last_active_at: None,
            pinned: false, // 置顶不继承分支
            archived_at: None, // 归档态不继承分支（分支从活跃态开始）
            message_count: None,
            ledger: src.ledger.clone(),
            created_at: now.clone(),
            updated_at: now,
        };
        self.store.insert_session(&fork)?;
        let copied = self
            .store
            .copy_messages_prefix(session_id, &fork.id, cursor)?;
        let _ = self.store.audit_tool(
            "webui",
            "session_fork",
            &serde_json::json!({ "source": session_id, "uptoTurn": upto_turn, "fork": fork.id }),
            &format!("派生分支会话 {}（复制 {copied} 条消息）", fork.id),
            0,
        );
        let _ = self.events.send(CoreEvent {
            kind: "session.fork".into(),
            session_id: fork.id.clone(),
            payload: serde_json::json!({ "source": session_id, "uptoTurn": upto_turn, "fork": fork.id, "copied": copied }),
        });
        Ok(fork)
    }

    pub async fn chat(&self, session_id: &str, text: &str) -> anyhow::Result<()> {
        // 默认能力边界：完整工具面 + Computer（WebUI / CLI / 定时任务的既有行为）
        self.chat_scoped(session_id, text, crate::types::ToolScope::Computer)
            .await
    }

    /// 带能力边界执行一轮：外部通道据此限制「只聊天 / 工具调用 / 电脑操控」。
    pub async fn chat_scoped(
        &self,
        session_id: &str,
        text: &str,
        scope: crate::types::ToolScope,
    ) -> anyhow::Result<()> {
        self.chat_scoped_origin(session_id, text, scope, None).await
    }

    /// 带来源标记的一轮执行：origin 落库进消息 agent_id，语义按前缀区分——
    /// "auto:xxx" 系统自动内容（视频观看/定时/webhook），展示层区分于真人消息；
    /// "im:名字(id)" 外部群聊发送者，展示层消息头署名、模型上下文渲染「用户·名字」；
    /// None = 真人输入（WebUI / 私聊）。
    pub async fn chat_scoped_origin(
        &self,
        session_id: &str,
        text: &str,
        scope: crate::types::ToolScope,
        origin: Option<&str>,
    ) -> anyhow::Result<()> {
        // 预算闸门：估算口径超限即拒绝新轮次（0 = 不限）
        let budget = self.config().max_session_tokens;
        if budget > 0 {
            let used = self.session_tokens_estimate(session_id);
            if used >= budget {
                anyhow::bail!("会话 token 预算已耗尽（估算 {used} / {budget}）；请新建会话或上调 maxSessionTokens");
            }
        }
        let lock = {
            let mut locks = self.run_locks.lock();
            locks
                .entry(session_id.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _guard = lock.lock().await;
        // 轮次生命周期：开始（登记取消令牌）→ 执行 → 结束（移除）
        crate::round_trace::begin(session_id);
        let session = self
            .store
            .get_session(session_id)
            .ok()
            .flatten()
            .ok_or_else(|| anyhow::anyhow!("会话不存在: {session_id}"))?;
        // 交互上下文对齐会话归属（路由唯一事实源）：会话 group_id 形如
        // "single:<id>"（单智能体）或组名。WebUI 切换目标 / 通道绑定切换 /
        // 子代理会话并发都可能翻转全局激活态，若不对齐，旧会话会被错误的
        // 人格/路由接管（部署实测：组会话在切换到单智能体后仍答指挥体人格）。
        // 实现为 task-local 轮次作用域：归属只在本轮任务内生效，全局激活态
        // 零污染——REST / 通道绑定 / 并发轮次读到的仍是各自显式设置的上下文
        // （旧实现写全局态再还原，run.finished 事件先于还原到达时会泄漏临时态）。
        // 归属目标已删除 → scope None → 回落全局激活态（轮次不中断）。
        let ctx = parse_session_scope(&self.registry, &session.group_id);
        let result = crate::registry::round_scope_decl::ROUND_SCOPE
            .scope(ctx, async {
                self.orchestrator()
                    .handle_user_message_scoped(session_id, text, scope, origin)
                    .await
            })
            .await;
        crate::round_trace::end(session_id);
        result
    }

    /// 停止会话运行中的轮次：置位取消令牌（工具循环检查点终止），返回已执行的工具次数。
    /// 空闲会话（无运行中轮次）返回错误。
    pub fn stop_session(&self, session_id: &str) -> anyhow::Result<usize> {
        if !crate::round_trace::is_running(session_id) {
            anyhow::bail!("该会话当前没有运行中的任务");
        }
        crate::round_trace::request_cancel(session_id);
        let n = crate::round_trace::tool_count(session_id);
        let _ = self.store.audit_tool(
            "system",
            "session_stop",
            &serde_json::json!({ "session": session_id }),
            &format!("用户请求停止（当时已执行 {n} 次工具调用）"),
            0,
        );
        Ok(n)
    }

    pub fn mcp(&self) -> Arc<crate::mcp::McpRegistry> {
        self.mcp.clone()
    }

    /// 注入远程执行器（工作者池）并热重建 Orchestrator 启用远程路由
    pub fn set_remote(&self, remote: Arc<dyn crate::remote::RemoteExecutor>) -> anyhow::Result<()> {
        *self.remote.write() = Some(remote);
        self.rebuild_orchestrator()
    }

    fn remote(&self) -> Option<Arc<dyn crate::remote::RemoteExecutor>> {
        self.remote.read().clone()
    }

    /// 断点续跑：网关重启后，把中断会话（图非终态）的剩余节点重新调度收束。
    /// 已完成节点的回流从持久层回填；dispatched/running 重置为 ready 重跑（幂等重执行）。
    pub async fn resume_interrupted(&self) -> usize {
        let sessions = match self.list_sessions() {
            Ok(s) => s,
            Err(_) => return 0,
        };
        let mut resumed = 0usize;
        for s in sessions {
            let Some(mut graph) = self.store.latest_graph(&s.id).unwrap_or(None) else {
                continue;
            };
            if graph.status != crate::types::GraphStatus::Executing {
                continue;
            }
            let has_pending = graph.nodes.iter().any(|n| !n.status.is_terminal());
            if !has_pending {
                continue;
            }
            match self.resume_session_graph(&s.id, &mut graph).await {
                Ok(()) => resumed += 1,
                Err(e) => eprintln!("[resume] 会话 {} 续跑失败：{e}", s.id),
            }
        }
        resumed
    }

    async fn resume_session_graph(
        &self,
        session_id: &str,
        graph: &mut crate::types::TaskGraph,
    ) -> anyhow::Result<()> {
        use crate::task::{NodeOutcome, Scheduler, TaskGraphModel};
        use futures_util::future::BoxFuture;
        use std::sync::Arc;
        use tokio::sync::Mutex;

        // 1) 非终态节点重置为可执行；已完成节点回流回填
        let mut reports: crate::task::Reports = Default::default();
        for n in graph.nodes.iter_mut() {
            if n.status.is_terminal() {
                if n.status == crate::types::TaskStatus::Done {
                    if let Ok(Some(r)) = self.store.sync_report_of_node(&n.id) {
                        reports.insert(n.id.clone(), r);
                    }
                }
            } else {
                n.status = crate::types::TaskStatus::Ready;
                n.retry_count = n.retry_count.saturating_sub(1);
            }
        }
        self.store.save_graph(graph)?;

        // 2) 重建 plan 骨架（收束与账本需要）
        let ledger = self.store.ledger_of(session_id)?;
        let plan = crate::types::OrchestratorPlan {
            route_level: crate::types::RouteLevel::L2,
            playbook: None,
            goal: ledger.task.goal.clone(),
            boundary: crate::types::DispatchBoundary {
                in_scope: ledger.task.constraints.clone(),
                forbidden: ledger.task.forbidden.clone(),
            },
            acceptance: ledger.task.acceptance.clone(),
            nodes: graph
                .nodes
                .iter()
                .map(|n| crate::types::PlanNode {
                    id: n.id.clone(),
                    title: n.title.clone(),
                    agent_identifier: n.agent_identifier.clone(),
                    objective: n.objective.clone(),
                    acceptance: n.acceptance.clone(),
                    depends_on: n.depends_on.clone(),
                    priority: n.priority,
                })
                .collect(),
            final_answer: None,
        };

        // 3) 重跑调度器（执行器 = Orchestrator 的节点执行路径）；已完成节点状态回放
        let orch = self.orchestrator();
        let mut model = TaskGraphModel::from_plan(&plan, session_id);
        for n in graph.nodes.iter() {
            if n.status == crate::types::TaskStatus::Done {
                model.set_status(&n.id, crate::types::TaskStatus::Done);
                model.set_report_id(&n.id, n.sync_report_id.clone());
            }
        }
        let reports_shared: Arc<Mutex<crate::task::Reports>> = Arc::new(Mutex::new(reports));
        let exec = crate::orchestrator::ExecCtx {
            orchestrator: orch.clone(),
            session_id: session_id.to_string(),
            plan: plan.clone(),
            reports: reports_shared.clone(),
            // 恢复执行按完整能力档：工具面已持久化在计划/派发单里
            scope: crate::types::ToolScope::Computer,
        };
        let exec = Arc::new(exec);
        let scheduler = Scheduler::new(self.config().max_concurrency);
        let store = self.store.clone();
        let events = self.events.clone();
        let sid = session_id.to_string();
        {
            let exec = exec.clone();
            scheduler
                .run(
                    &mut model,
                    move |node| -> BoxFuture<'static, NodeOutcome> {
                        let exec = exec.clone();
                        Box::pin(async move { exec.run_node(node).await })
                    },
                    move |g| {
                        let _ = store.save_graph(&g.to_graph());
                        let evt = crate::types::CoreEvent {
                            kind: "graph.updated".into(),
                            session_id: sid.clone(),
                            payload: serde_json::to_value(g.to_graph())
                                .unwrap_or(serde_json::Value::Null),
                        };
                        let _ = events.send(evt);
                    },
                )
                .await;
        }

        // 4) 收束（复用指挥体收束路径）
        let final_reports = reports_shared.lock().await.clone();
        let final_text = orch
            .converge(session_id, &plan, &final_reports, &model)
            .await?;
        let statements = crate::orchestrator::text_to_statements(&final_text);
        self.store.add_message(
            session_id,
            crate::types::MessageRole::Orchestrator,
            Some(orch.orch_id().as_str()),
            statements.clone(),
        )?;
        let _ = self.events.send(crate::types::CoreEvent {
            kind: "run.finished".into(),
            session_id: session_id.to_string(),
            payload: serde_json::json!({
                "routeLevel": "L2",
                "agentId": orch.orch_id(),
                "graph": model.to_graph(),
                "statements": statements,
                "resumed": true,
            }),
        });
        Ok(())
    }

    fn rebuild_orchestrator(&self) -> anyhow::Result<()> {
        let cfg = self.config();
        let orch = build_orchestrator(
            &cfg,
            &self.store,
            &self.registry,
            &self.events,
            &self.memory,
            Some(self.mcp.clone()),
            self.remote(),
            Some(self.cron.clone()),
        )?;
        self.rebind_plugin_tools(&orch);
        *self.orchestrator.write() = Arc::new(orch);
        Ok(())
    }

    /// 把插件工具面（T1 声明式 + T2 进程运行时）重新挂到新 orchestrator。
    /// 所有换 orchestrator 的路径（apply_config / set_remote 等）重建后必调——
    /// 否则进程工具的会话面注册随旧 ToolGateway 一起丢失。
    fn rebind_plugin_tools(&self, orch: &Orchestrator) {
        let pm = self.plugins.read().clone();
        // T1：声明式内联工具
        let plugin_tools: Vec<_> = pm
            .declarative_tools()
            .into_iter()
            .map(|(_, _, t)| t)
            .collect();
        if !plugin_tools.is_empty() {
            orch.tools.append_plugin_tools(plugin_tools);
        }
        // T2：进程运行时工具（provider 持 supervisor 句柄，重挂即恢复会话面）
        for e in pm.list() {
            if e.form != "process" || !e.enabled {
                continue;
            }
            if let Some(p) = pm.provider_of(&e.manifest.id) {
                if let Some(tp) = p.tool_provider() {
                    orch.tools.register_plugin_runtime_tools(&e.manifest.id, tp);
                }
            }
        }
        // 会话授权探针重挂（approval-ergonomics）：orchestrator 重建后 ToolGateway
        // 是新实例,旧探针闭包指向旧网关——用缓存的 Weak<Core> 重新注入。
        if let Some(weak) = self.self_weak.read().clone() {
            orch.tools.set_session_grant_probe(std::sync::Arc::new(
                move |sid: &str, cmd: &str| {
                    weak.upgrade().is_some_and(|c| c.session_granted(sid, cmd).is_some())
                },
            ));
        }
    }

    /// 暂存图片附件（随该会话下一轮对话注入规划；每轮取走即清）
    pub fn stage_images(&self, session_id: &str, images: Vec<String>) {
        crate::image_stash::stage(session_id, images);
    }

    pub fn orchestrator(&self) -> Arc<Orchestrator> {
        self.orchestrator.read().clone()
    }

    pub fn config(&self) -> Arc<ExmConfig> {
        self.config.read().clone()
    }

    pub fn is_mock(&self) -> bool {
        self.config.read().use_mock
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CoreEvent> {
        self.events.subscribe()
    }

    // ---------------- 会话与编成便捷方法 ----------------

    pub fn list_sessions_in_group(&self, gid: &str) -> anyhow::Result<Vec<Session>> {
        self.store.list_sessions_in_group(gid)
    }

    /// 当前交互范围（组或智能体）的会话列表
    pub fn list_sessions_in_scope(&self) -> anyhow::Result<Vec<Session>> {
        self.store
            .list_sessions_in_group(&self.registry.active_scope())
    }

    pub fn delete_session(&self, id: &str) -> anyhow::Result<()> {
        crate::orchestrator::unmark_im_session(id);
        // 会话删除连带清理审批授权与工具预算计数（生命周期 = 会话生命周期）
        self.session_grants.lock().remove(id);
        self.orchestrator().tools.clear_tool_budget(id);
        self.store.delete_session(id)
    }

    /// 会话审批授权写入（approval-ergonomics「本会话允许」档）：
    /// 前缀口径见 allowlist::command_head。红线校验在 approval_decide 调用侧——
    /// [root] 提权单与破坏性命令不会走到这里。
    pub fn grant_session(&self, session_id: &str, prefix: &str) {
        if prefix.is_empty() {
            return;
        }
        self.session_grants
            .lock()
            .entry(session_id.to_string())
            .or_default()
            .insert(prefix.to_string(), ());
    }

    /// 会话授权命中判定：命令前缀在授权表内即视为已批准。
    /// 返回命中的前缀（调试/审计可见），未命中返回 None。
    pub fn session_granted(&self, session_id: &str, cmd: &str) -> Option<String> {
        let head = crate::tools::command_head(cmd);
        if head.is_empty() {
            return None;
        }
        self.session_grants
            .lock()
            .get(session_id)?
            .contains_key(&head)
            .then_some(head)
    }

    /// 会话置顶切换（列表置顶分组数据源）
    pub fn toggle_session_pin(&self, id: &str, pinned: bool) -> anyhow::Result<()> {
        self.store.toggle_session_pin(id, pinned)
    }

    /// 会话归档 / 恢复（可逆状态：归档移出主列表，恢复回原分组）
    pub fn set_session_archived(&self, id: &str, archived: bool) -> anyhow::Result<()> {
        self.store.set_session_archived(id, archived)
    }

    /// 归档区列表（只含归档态，按归档时间倒序；预览口径与主列表一致）
    pub fn list_archived_sessions(&self) -> anyhow::Result<Vec<Session>> {
        let mut list = self.store.list_archived_sessions()?;
        for s in list.iter_mut() {
            s.last_message_preview = self.store.last_message_preview(&s.id, 80);
            s.last_active_at = Some(s.updated_at.clone());
            s.message_count = Some(self.store.message_count(&s.id));
        }
        self.attach_session_origins(&mut list);
        Ok(list)
    }

    /// 管理视角全量会话（含消息数计数；主列表口径 = 未归档）
    pub fn list_sessions_admin(&self) -> anyhow::Result<Vec<Session>> {
        let mut list = self.list_sessions()?;
        for s in list.iter_mut() {
            s.message_count = Some(self.store.message_count(&s.id));
        }
        self.attach_session_origins(&mut list);
        Ok(list)
    }

    /// 会话来源派生补齐（session-management）：title 约定解析 + flow 表 join。
    /// 通道会话 origin = `channel:{id}:{key}`、origin_label = `流显示名·标签`；
    /// 控制台会话 origin = `console`、label 缺省（title 即展示名）。
    fn attach_session_origins(&self, list: &mut [Session]) {
        // flow 表一次读全（量级 = 流数，远小于会话数），join 在内存完成：
        // flow_id（channelId:chatKey）→ 流显示名（缺省 chat_key）
        let flow_names: std::collections::HashMap<String, String> = self
            .channel_flows(None)
            .into_iter()
            .map(|f| {
                let name = if f.display_name.trim().is_empty() {
                    f.chat_key.clone()
                } else {
                    f.display_name.clone()
                };
                (f.id, name)
            })
            .collect();
        for s in list.iter_mut() {
            let origin = crate::session_origin::parse_session_origin(&s.title);
            s.origin = Some(origin.key());
            if let crate::session_origin::SessionOrigin::Channel {
                channel_id,
                chat_key,
                label,
            } = origin
            {
                let flow_id = format!("{channel_id}:{chat_key}");
                let flow_name = flow_names.get(&flow_id).cloned().unwrap_or(chat_key);
                s.origin_label = Some(match label {
                    Some(l) if !l.trim().is_empty() => format!("{flow_name}·{l}"),
                    _ => flow_name,
                });
            } else {
                s.origin_label = None;
            }
        }
    }

    /// 闲置流会话归档扫描（session-management 生命周期治理）。
    /// 口径：通道会话（title 约定解析）· 未归档 · `updated_at` 距 `now` 超过
    /// `idle_days` 天 · 非所在流的活跃绑定会话（flow.active_session 豁免）。
    /// 返回 (归档数, 豁免数)。批量归档复用同一豁免口径（archive_flow_sessions）。
    pub fn archive_idle_flow_sessions(
        &self,
        idle_days: u32,
        now: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<(usize, usize)> {
        if idle_days == 0 {
            return Ok((0, 0));
        }
        let horizon = now - chrono::Duration::days(idle_days as i64);
        let horizon_iso = horizon.to_rfc3339();
        let mut targets: Vec<String> = Vec::new();
        let mut skipped = 0usize;
        for s in self.store.list_sessions()? {
            if s.archived_at.is_some() {
                continue;
            }
            let crate::session_origin::SessionOrigin::Channel { channel_id, chat_key, .. } =
                crate::session_origin::parse_session_origin(&s.title)
            else {
                continue;
            };
            // 活跃绑定豁免：下条消息将继续复用该会话
            if self
                .flow_active_session(&channel_id, &chat_key)
                .as_deref()
                == Some(s.id.as_str())
            {
                skipped += 1;
                continue;
            }
            if s.updated_at < horizon_iso {
                targets.push(s.id.clone());
            }
        }
        for id in &targets {
            self.set_session_archived(id, true)?;
        }
        if !targets.is_empty() {
            let _ = self.events.send(CoreEvent {
                kind: "session.idle_archived".into(),
                session_id: String::new(),
                payload: serde_json::json!({ "count": targets.len(), "idleDays": idle_days }),
            });
        }
        Ok((targets.len(), skipped))
    }

    /// 按流/按通道归档旧会话（session-management 批量工具）：
    /// 圈定范围内未归档会话全部归档，各流活跃绑定会话豁免。
    /// 返回 (归档数, 豁免数)。
    pub fn archive_flow_sessions(
        &self,
        scope: crate::session_origin::ArchiveScope,
    ) -> anyhow::Result<(usize, usize)> {
        let mut targets: Vec<String> = Vec::new();
        let mut skipped = 0usize;
        for s in self.store.list_sessions()? {
            if s.archived_at.is_some() {
                continue;
            }
            let origin = crate::session_origin::parse_session_origin(&s.title);
            if !scope.matches(&origin) {
                continue;
            }
            if let crate::session_origin::SessionOrigin::Channel { channel_id, chat_key, .. } =
                &origin
            {
                if self
                    .flow_active_session(channel_id, chat_key)
                    .as_deref()
                    == Some(s.id.as_str())
                {
                    skipped += 1;
                    continue;
                }
            }
            targets.push(s.id.clone());
        }
        for id in &targets {
            self.set_session_archived(id, true)?;
        }
        Ok((targets.len(), skipped))
    }

    pub fn create_session(&self, title: &str) -> anyhow::Result<Session> {
        let gid = self.registry.active_scope();
        self.store.create_session(title, &gid)
    }

    pub fn list_sessions(&self) -> anyhow::Result<Vec<Session>> {
        let mut list = self.store.list_sessions()?;
        // 末条消息预览（chat-ergonomics）：会话量级 ≤200，末行微读成本可控
        for s in list.iter_mut() {
            s.last_message_preview = self.store.last_message_preview(&s.id, 80);
            s.last_active_at = Some(s.updated_at.clone());
        }
        Ok(list)
    }

    /// 会话内容检索（标题 + 最近 200 条消息）：REST / 速查共用，
    /// 范围与消息读取窗口一致（命中片段可被聊天页加载并定位）。
    pub fn search_sessions(&self, query: &str, limit: usize) -> anyhow::Result<Vec<SessionSearchHit>> {
        self.store.search_sessions(query, limit)
    }

    pub fn agents(&self) -> Vec<AgentDefinition> {
        self.registry.list()
    }

    pub fn agent(&self, identifier: &str) -> Option<AgentDefinition> {
        self.registry.get(identifier)
    }

    pub fn playbooks(&self) -> anyhow::Result<Vec<Playbook>> {
        self.registry.load_playbooks()
    }

    // ---------------- 智能体组（docs/09） ----------------

    pub fn list_groups(&self) -> Vec<GroupMeta> {
        self.registry.list_groups()
    }

    pub fn active_group(&self) -> String {
        self.registry.active_group()
    }

    pub fn active_group_meta(&self) -> Option<GroupMeta> {
        self.registry.active_group_meta()
    }

    pub fn switch_group(&self, gid: &str) -> anyhow::Result<()> {
        self.registry.set_active_group(gid)
    }

    pub fn create_group(
        &self,
        id: Option<String>,
        name: &str,
        description: &str,
    ) -> anyhow::Result<GroupMeta> {
        self.registry.create_group(id, name, description)
    }

    pub fn delete_group(&self, gid: &str) -> anyhow::Result<()> {
        self.registry.delete_group(gid)
    }

    pub fn group_meta(&self, gid: &str) -> Option<GroupMeta> {
        self.registry.group_meta(gid)
    }

    /// 在激活组内新增/替换个体（用户操作或主智能体经 agent_manage 工具）
    pub fn upsert_agent(
        &self,
        def: AgentDefinition,
        prompt: Option<String>,
    ) -> anyhow::Result<AgentDefinition> {
        let gid = self.registry.active_group();
        self.registry.upsert_agent(&gid, def, prompt)
    }

    pub fn remove_agent(&self, identifier: &str) -> anyhow::Result<()> {
        let gid = self.registry.active_group();
        self.registry.remove_agent(&gid, identifier)
    }

    pub fn set_primary(&self, identifier: &str) -> anyhow::Result<()> {
        let gid = self.registry.active_group();
        self.registry.set_primary(&gid, identifier)
    }

    pub fn registry(&self) -> Arc<LocalRegistry> {
        self.registry.clone()
    }

    // ---------------- 平台能力：技能包 / 定时任务 / 执行审批（docs/10） ----------------

    pub fn skills(&self) -> anyhow::Result<Vec<SkillDef>> {
        self.registry.load_skills()
    }

    pub fn add_skill(&self, skill: SkillDef) -> anyhow::Result<()> {
        self.registry.write_skill(&skill)
    }

    pub fn remove_skill(&self, id: &str) -> anyhow::Result<bool> {
        self.registry.remove_skill(id)
    }

    pub fn approval_list(
        &self,
        status: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<ApprovalRequest>> {
        self.store.list_approvals(status, limit)
    }

    pub fn approval_get(&self, id: &str) -> anyhow::Result<Option<ApprovalRequest>> {
        self.store.get_approval(id)
    }

    /// 审批决定：批准即由系统代执行并记录输出；拒绝即关闭审批单
    pub async fn approval_decide(
        &self,
        id: &str,
        approve: bool,
    ) -> anyhow::Result<ApprovalRequest> {
        self.approval_decide_scoped(id, ApprovalScope::Once { approve }).await
    }

    /// 审批决定（approval-ergonomics 三档粒度）：
    /// - Once：本次执行（旧行为，缺省档）
    /// - Session：本次执行 + 同命令前缀本会话后续自动放行（内存态）
    /// - Allowlist：本次执行 + 前缀写入 security.execAllowlist（持久）
    ///
    /// 红线：[root] 等特殊前缀单与破坏性命令不支持 Session/Allowlist 档——
    /// 越权档位自动降级 Once。
    pub async fn approval_decide_scoped(
        &self,
        id: &str,
        decision: ApprovalScope,
    ) -> anyhow::Result<ApprovalRequest> {
        // 决定互斥（竞态 P1-1）：双击批准/WebUI+通道并发时，同一审批单只有一个
        // 决定流能走下去——否则双执行命令（执行窗口可达数分钟）。占不到锁 =
        // 另一个决定在途：等它收束后返回最新单据（语义：「你的点击已生效，
        // 由先到者代执行」而非报错）。
        let lock = {
            let mut locks = self.approval_locks.lock();
            locks
                .entry(id.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let Ok(_guard) = lock.try_lock() else {
            // 另一个决定在途：轮询等最终态（上限 60s，防永挂）
            for _ in 0..120 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if let Some(req) = self.store.get_approval(id)? {
                    if req.status != "pending" {
                        return Ok(req);
                    }
                }
            }
            anyhow::bail!("审批决定超时：另一决定长时间未收束");
        };
        let mut req = self
            .store
            .get_approval(id)?
            .ok_or_else(|| anyhow::anyhow!("审批单不存在: {id}"))?;
        if req.status != "pending" {
            anyhow::bail!("审批单已处理: {}", req.status);
        }
        // 红线校验：特殊前缀单与破坏性命令不允许升档（拒绝类决定不受限）
        let effective = if decision.is_approve() {
            let special = req.command.starts_with('[');
            let destructive =
                crate::tools::is_destructive_command(&req.command.to_lowercase());
            if matches!(
                decision,
                ApprovalScope::Session | ApprovalScope::Allowlist
            ) && (special || destructive)
            {
                ApprovalScope::Once { approve: true }
            } else {
                decision
            }
        } else {
            ApprovalScope::Once { approve: false }
        };
        let approve = effective.is_approve();
        // 视频动作审批单（[video] 前缀）：批准 = 回调代发执行器，拒绝 = 记录跳过
        if req.command.starts_with("[video]") {
            let payload = req.command.trim_start_matches("[video]").trim().to_string();
            req.decided_at = Some(now_iso());
            if !approve {
                req.status = "denied".into();
                req.result = Some("视频互动被拒绝（未发送）".into());
                self.store.update_approval(&req)?;
                self.emit_approval_resolved(&req);
                return Ok(req);
            }
            match self.execute_video_act_approval(&payload).await {
                Ok(out) => {
                    req.status = "executed".into();
                    req.result = Some(out.chars().take(4000).collect());
                }
                Err(e) => {
                    req.status = "failed".into();
                    req.result = Some(e.to_string());
                }
            }
            self.store.update_approval(&req)?;
            self.emit_approval_resolved(&req);
            return Ok(req);
        }
        // 计划审批单（[plan] 前缀）：转发计划通道——批准 = 回放清单，拒绝 = 保留清单
        if req.command.starts_with("[plan]") {
            let sid = req.session_id.clone();
            if approve {
                let results = self.plan_approve(&sid).await;
                req.status = match results {
                    Ok(rs) if rs.iter().all(|(_, r)| r.ok) => "executed".into(),
                    Ok(_) => "failed".into(),
                    Err(e) => {
                        req.status = "failed".into();
                        req.result = Some(e.to_string());
                        self.store.update_approval(&req)?;
                        self.emit_approval_resolved(&req);
                        return Ok(req);
                    }
                };
                req.result = Some("计划已回放执行".into());
            } else {
                self.plan_reject(&sid);
                req.status = "denied".into();
                req.result = Some("计划被拒绝（清单保留可继续修改）".into());
            }
            req.decided_at = Some(now_iso());
            self.store.update_approval(&req)?;
            self.emit_approval_resolved(&req);
            return Ok(req);
        }
        // 元智能体提议单（[meta] 前缀，meta-agent）：批准 = 按 kind 代为应用
        // （prompt 补段/adaptation 版本化/roster 两阶段/build 注册）;拒绝 = 受阻回流。
        // 内置组保护与主智能体校验沿 agent_manage 同路径,提议不绕过任何闸门。
        if req.command.starts_with("[meta]") {
            req.decided_at = Some(now_iso());
            if !approve {
                req.status = "denied".into();
                req.result = Some("元智能体提议被拒绝".into());
                self.store.update_approval(&req)?;
                self.emit_approval_resolved(&req);
                return Ok(req);
            }
            let outcome = self.apply_meta_proposal(&req).await;
            let (status, text) = match outcome {
                Ok(msg) => ("executed".to_string(), msg),
                Err(e) => ("failed".to_string(), e.to_string()),
            };
            req.status = status;
            req.result = Some(text.chars().take(4000).collect());
            self.store.update_approval(&req)?;
            self.emit_approval_resolved(&req);
            let _ = self.events.send(CoreEvent {
                kind: "meta.applied".into(),
                session_id: req.session_id.clone(),
                payload: serde_json::json!({
                    "approvalId": req.id,
                    "ok": req.status == "executed",
                    "command": req.command,
                }),
            });
            return Ok(req);
        }
        // root 提权审批单（[root] 前缀，agent-exec-confinement）：批准 = 以 root 身份
        // 代执行（剥冗余 sudo 壳、保留 su 切换语义、不走 execUser 降权）；拒绝 = 受阻回流
        if req.command.starts_with("[root]") {
            let raw_cmd = req.command.trim_start_matches("[root]").trim().to_string();
            req.decided_at = Some(now_iso());
            if !approve {
                req.status = "denied".into();
                req.result = Some("root 提权被拒绝".into());
                self.store.update_approval(&req)?;
                self.emit_approval_resolved(&req);
                return Ok(req);
            }
            // 剥离 sudo/doas/run0 冗余壳（root 下多余）；su/sudo -u/runuser 保留语义
            let eff_cmd = crate::tools::strip_redundant_elevation(&raw_cmd);
            let workspace = self.config().workspace_root.clone();
            // root 代执行：ShellOpts::plain（不降权——审批的就是提权本身）
            let opts = crate::tools::ShellOpts::plain();
            let out = crate::tools::execute_shell_with(&workspace, &eff_cmd, 300, &opts).await;
            req.status = if out.ok { "executed".into() } else { "failed".into() };
            let text = if out.ok { out.output } else { out.error.unwrap_or_default() };
            // 红线降级注明（approval-ergonomics）：[root] 提权单不允许升档,
            // 越权档位自动降级 once——审批结果注明让裁决者知情
            let downgrade_note = if matches!(
                decision,
                ApprovalScope::Session | ApprovalScope::Allowlist
            ) {
                "\n[红线] 提权单不支持会话授权/白名单档，已按本次执行"
            } else {
                ""
            };
            req.result = Some(
                format!("[root 执行] {eff_cmd}\n{}{downgrade_note}", text)
                    .chars()
                    .take(4000)
                    .collect(),
            );
            self.store.update_approval(&req)?;
            self.emit_approval_resolved(&req);
            return Ok(req);
        }
        req.decided_at = Some(now_iso());
        if !approve {
            req.status = "denied".into();
            self.store.update_approval(&req)?;
            self.emit_approval_resolved(&req);
            return Ok(req);
        }
        // 普通审批单代执行收口（agent-exec-confinement）：与直通执行同一加固路径——
        // 审批放行的是命令内容,不是特权;execUser 降权/沙箱净化照常生效。
        // [红线] 前缀（agent-budget-audit 3.3 分档闸门标记）不是 shell 语法，
        // 代执行前剥掉——审批的是命令本身，标记只用于档位溯源
        let eff_cmd = req
            .command
            .strip_prefix("[红线]")
            .map(|c| c.trim().to_string())
            .unwrap_or_else(|| req.command.clone());
        let workspace = self.config().workspace_root.clone();
        let opts = self.orchestrator().tools.shell_opts(&eff_cmd);
        let out = crate::tools::execute_shell_with(&workspace, &eff_cmd, 300, &opts).await;
        req.status = if out.ok {
            "executed".into()
        } else {
            "failed".into()
        };
        let text = if out.ok {
            out.output
        } else {
            out.error.unwrap_or_default()
        };
        req.result = Some(text.chars().take(4000).collect());
        // 档位副作用（仅普通命令单走到这里;特殊前缀单已在上方分流 return）。
        // 执行失败也建授权/白名单?不——失败说明命令本身有问题,放行同类无意义,
        // 且用户批的是「这类命令可以跑」的意图以成功执行为前提更符合直觉。
        if out.ok {
            match effective {
                ApprovalScope::Session => {
                    let prefix = crate::tools::command_head(&req.command);
                    self.grant_session(&req.session_id, &prefix);
                    req.result = Some(format!(
                        "{}\n[会话授权] 同前缀命令「{prefix}」本会话内自动放行",
                        req.result.as_deref().unwrap_or("")
                    ));
                }
                ApprovalScope::Allowlist => {
                    let prefix = crate::tools::command_head(&req.command);
                    if !prefix.is_empty() {
                        let mut cfg = self.config().as_ref().clone();
                        if !cfg.security.exec_allowlist.iter().any(|p| p == &prefix) {
                            cfg.security.exec_allowlist.push(prefix.clone());
                            self.apply_config(cfg)?;
                        }
                        req.result = Some(format!(
                            "{}\n[白名单] 前缀「{prefix}」已写入全局白名单",
                            req.result.as_deref().unwrap_or("")
                        ));
                    }
                }
                _ => {}
            }
        }
        self.store.update_approval(&req)?;
        self.emit_approval_resolved(&req);
        Ok(req)
    }

    /// 登记会话的外部来源（闸门放行时由适配器调用；审批单创建时据此打升级标）
    pub fn stamp_session_origin(&self, session_id: &str, external_id: &str, role: &str) {
        let _ = self.store.put_session_origin(session_id, external_id, role);
    }

    // ---------------- 视频互动审批（video__act 批准代发） ----------------

    pub fn register_video_act_executor(&self, exec: crate::types::VideoActExecutor) {
        *self.video_act_executor.write() = Some(exec);
    }

    async fn execute_video_act_approval(&self, payload: &str) -> anyhow::Result<String> {
        let exec = self.video_act_executor.read().clone();
        let Some(exec) = exec else {
            anyhow::bail!("视频动作代发执行器未注册（网关未启动？）");
        };
        exec(payload.to_string()).await
    }

    // ---------------- 计划模式（spec: plan-mode） ----------------

    /// 工作台文件树（spec: webui-code-workbench）：.gitignore 口径过滤 + 越权拒绝
    pub fn workbench_tree(&self, rel: &str, depth: usize) -> anyhow::Result<serde_json::Value> {
        self.orchestrator.read().tools.workbench_tree(rel, depth)
    }

    /// 工作台文件读取（read 工具同口径分页）
    pub fn workbench_file(
        &self,
        path: &str,
        offset: u64,
        limit: u64,
    ) -> anyhow::Result<serde_json::Value> {
        self.orchestrator
            .read()
            .tools
            .workbench_file(path, offset, limit)
    }

    /// 设置/清除会话计划模式（true = 只读规划，写操作转计划条目）
    pub fn set_plan_mode(&self, session_id: &str, plan: bool) {
        crate::plan::set_plan_mode(session_id, plan);
        self.orchestrator.read().emit(
            session_id,
            if plan {
                "plan.enabled"
            } else {
                "plan.disabled"
            },
            serde_json::json!({}),
        );
    }

    /// 会话计划清单快照
    pub fn plan_list(&self, session_id: &str) -> Vec<crate::plan::PlanItem> {
        crate::plan::list_items(session_id)
    }

    /// 拒绝计划：保留清单（可继续修改），不切模式，广播事件；
    /// 若 approvals 通道有关联待审批单则一并了结
    pub fn plan_reject(&self, session_id: &str) -> Vec<crate::plan::PlanItem> {
        let items = crate::plan::reject(session_id);
        if let Some(ap_id) = crate::plan::take_pending_approval(session_id) {
            if let Ok(Some(mut ap)) = self.store.get_approval(&ap_id) {
                if ap.status == "pending" {
                    ap.status = "denied".into();
                    ap.decided_at = Some(now_iso());
                    ap.result = Some("计划被拒绝（清单保留可继续修改）".into());
                    let _ = self.store.update_approval(&ap);
                    self.emit_approval_resolved(&ap);
                }
            }
        }
        self.orchestrator.read().emit(
            session_id,
            "plan.rejected",
            serde_json::json!({ "count": items.len() }),
        );
        items
    }

    /// 批准计划：切 execute 并逐条回放执行（terminal 走 shell 执行器，
    /// 其余工具走网关 execute——此时已非 plan 模式，不再拦截），
    /// 每条执行结果广播 plan.applied 事件
    pub async fn plan_approve(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<(crate::plan::PlanItem, crate::tools::ToolResult)>> {
        let items = crate::plan::approve(session_id);
        if items.is_empty() {
            anyhow::bail!("计划清单为空（无可批准条目）");
        }
        // approvals 通道关联单了结（executed）
        if let Some(ap_id) = crate::plan::take_pending_approval(session_id) {
            if let Ok(Some(mut ap)) = self.store.get_approval(&ap_id) {
                if ap.status == "pending" {
                    ap.status = "executed".into();
                    ap.decided_at = Some(now_iso());
                    ap.result = Some("计划已批准回放".into());
                    let _ = self.store.update_approval(&ap);
                    self.emit_approval_resolved(&ap);
                }
            }
        }
        self.orchestrator.read().emit(
            session_id,
            "plan.approved",
            serde_json::json!({ "count": items.len() }),
        );
        let orch = self.orchestrator.read().clone();
        let mut out = Vec::new();
        for item in items {
            let tool = crate::types::ToolName::parse(&item.tool);
            let result = match tool {
                Some(t) => {
                    orch.tools
                        .execute("plan-replay", session_id, &[t], t, &item.args)
                        .await
                }
                None => crate::tools::ToolResult::err(format!("未知工具：{}", item.tool)),
            };
            self.orchestrator.read().emit(
                session_id,
                "plan.applied",
                serde_json::json!({
                    "itemId": item.id, "tool": item.tool, "target": item.target,
                    "ok": result.ok,
                }),
            );
            out.push((item, result));
        }
        Ok(out)
    }

    pub fn session_origin(&self, session_id: &str) -> Option<(String, String)> {
        self.store.get_session_origin(session_id).ok().flatten()
    }

    /// 创建 WebUI 发起的 git 破坏性操作审批单：command 为精确 git 命令，
    /// 批准后由 approval_decide 代执行（与工具审批同一执行与审计路径）
    pub async fn request_git_approval(&self, command: &str) -> anyhow::Result<ApprovalRequest> {
        let req = ApprovalRequest {
            id: crate::types::new_id(),
            session_id: String::new(),
            node_id: None,
            agent_id: "webui".into(),
            command: command.to_string(),
            status: "pending".into(),
            result: None,
            created_at: now_iso(),
            decided_at: None,
            origin_user: None,
            origin_role: None,
        
            meta_json: None,
        };
        self.store.add_approval(&req)?;
        let _ = self.events.send(CoreEvent {
            kind: "approval.required".into(),
            session_id: String::new(),
            payload: serde_json::json!({ "approvalId": req.id, "agentId": "webui", "command": command }),
        });
        Ok(req)
    }

    fn emit_approval_resolved(&self, req: &ApprovalRequest) {
        let _ = self.events.send(CoreEvent {
            kind: "approval.resolved".into(),
            session_id: req.session_id.clone(),
            payload: serde_json::json!({ "approvalId": req.id, "status": req.status, "command": req.command }),
        });
    }

    /// 注入一条事件提示词到目标组会话（事件触发器共用通路，design D6：
    /// 组切换 + 会话复用 + prompt 注入；与 cron 同路但独立记账，不进 cron 运行日志）。
    /// 返回执行的会话 id。标题缺省 = `event-<group>`（同组事件复用同一会话）。
    pub async fn run_event_prompt(
        &self,
        title: &str,
        group: Option<&str>,
        prompt: &str,
    ) -> anyhow::Result<String> {
        let prev_group = self.registry.active_group();
        let mut switched = false;
        if let Some(g) = group {
            if *g != prev_group && self.group_meta(g).is_some() {
                switched = self.registry.set_active_group(g).is_ok();
            }
        }
        let gid = self.registry.active_group();
        let title = if title.trim().is_empty() {
            format!("event-{gid}")
        } else {
            title.to_string()
        };
        let result = async {
            let existing = self
                .store
                .list_sessions_in_group(&gid)
                .ok()
                .and_then(|list| list.into_iter().find(|s| s.title == title));
            let session = match existing {
                Some(s) => s,
                None => self.create_session(&title)?,
            };
            self.chat(&session.id, prompt).await?;
            Ok::<String, anyhow::Error>(session.id)
        }
        .await;
        if switched {
            let _ = self.registry.set_active_group(&prev_group);
        }
        result
    }

    /// 执行一条定时任务：复用同名会话（缺省 job-<id>），注入提示词并等待收束
    pub async fn run_cron_job(&self, job: &CronJob) -> CronRun {
        let started = now_iso();
        let title = job
            .session_title
            .clone()
            .unwrap_or_else(|| format!("job-{}", job.id));
        // 组切换先行：任务声明的组决定会话归属与执行上下文（执行完恢复）
        let prev_group = self.registry.active_group();
        let mut switched = false;
        if let Some(g) = &job.group {
            if *g != prev_group && self.group_meta(g).is_some() {
                switched = self.registry.set_active_group(g).is_ok();
            }
        }
        // 会话复用（限定本组）：按标题匹配已有会话，避免每次运行新建。
        // 创建失败（磁盘满/库忙）不 panic——落 failed 记录 + 事件，调度循环继续（审计 P1-3）
        let gid = self.registry.active_group();
        let existing = self
            .store
            .list_sessions_in_group(&gid)
            .ok()
            .and_then(|list| list.into_iter().find(|s| s.title == title));
        let session = match existing {
            Some(s) => s,
            None => match self.create_session(&title) {
                Ok(s) => s,
                Err(e) => {
                    if switched {
                        let _ = self.registry.set_active_group(&prev_group);
                    }
                    let err = format!("会话创建失败：{e}");
                    let _ = self.events.send(CoreEvent {
                        kind: "cron.finished".into(),
                        session_id: String::new(),
                        payload: serde_json::json!({
                            "jobId": job.id,
                            "jobName": job.name,
                            "status": "failed",
                            "error": err,
                        }),
                    });
                    return CronRun {
                        id: new_id()[..8].to_string(),
                        job_id: job.id.clone(),
                        job_name: job.name.clone(),
                        session_id: String::new(),
                        started_at: started,
                        finished_at: now_iso(),
                        status: "failed".into(),
                        summary: err,
                    };
                }
            },
        };
        let result = self.chat(&session.id, &job.prompt).await;
        if switched {
            let _ = self.registry.set_active_group(&prev_group);
        }
        let status = if result.is_ok() { "done" } else { "failed" };
        let summary = match result {
            Ok(_) => "任务执行完成（详见会话与任务图）".to_string(),
            Err(e) => format!("执行失败：{e}"),
        };
        let run = CronRun {
            id: new_id()[..8].to_string(),
            job_id: job.id.clone(),
            job_name: job.name.clone(),
            session_id: session.id.clone(),
            started_at: started,
            finished_at: now_iso(),
            status: status.into(),
            summary,
        };
        let _ = self.cron.record_run(&run);
        let _ = self.events.send(CoreEvent {
            kind: "cron.finished".into(),
            session_id: session.id,
            payload: serde_json::json!({
                "jobId": run.job_id, "jobName": run.job_name,
                "status": run.status, "summary": run.summary,
            }),
        });
        run
    }

    // ---------------------------------------------------------------- Auto Agent（experiments.autoAgent）

    /// Auto Agent 状态（进程内：空转计数 + 上次 tick 分钟）。 Eval 通过次数与
    /// 执行结果落事件流（auto.eval / auto.act），重启归零不影响正确性。
    fn auto_agent_state(&self) -> (u32, i64) {
        let st = self.auto_state.read();
        (st.idle_streak, st.last_tick_minute)
    }

    /// Auto Agent 一次 tick：周期/退避判定 → Eval 自评 →（通过）自主行动。
    /// 返回 Some(任务) 表示本轮触发了行动；None = 未到周期 / Eval 无任务。
    /// 调度器每 20s 调用；内部按分钟粒度去重（同一分钟只评一次）。
    pub async fn auto_agent_tick(&self, now_minute: i64) -> Option<(String, String)> {
        let cfg = self.config();
        if !cfg.experiments.auto_agent {
            return None;
        }
        let (idle_streak, last_tick) = self.auto_agent_state();
        // 周期判定：距上次 tick 的分钟数 ≥ 退避间隔才评（首 tick last=0 直评）
        let due = crate::autonomy::auto_backoff_minutes(&cfg, idle_streak) as i64;
        if last_tick != 0 && now_minute - last_tick < due {
            return None;
        }
        self.auto_state.write().last_tick_minute = now_minute;
        // Eval：非流式一次调用（工具不可用——Eval 只读判断，行动轮才有工具）
        let orch = self.orchestrator();
        let req = crate::provider::ChatRequest::new(
            orch.orch_model.clone(),
            vec![
                crate::provider::ChatMessage::system(crate::autonomy::AUTO_EVAL_PROMPT),
                crate::provider::ChatMessage::user("执行一次自评。"),
            ],
        );
        let output = match orch.orch_provider.chat(req).await {
            Ok(r) => r.content,
            Err(e) => {
                let _ = self.events.send(CoreEvent {
                    kind: "auto.eval".into(),
                    session_id: String::new(),
                    payload: serde_json::json!({ "ok": false, "error": e.to_string() }),
                });
                return None;
            }
        };
        let Some((task, why)) = crate::autonomy::parse_auto_eval(&output) else {
            // 无任务：空转计数 +1（退避拉长），发审计事件
            self.auto_state.write().idle_streak += 1;
            let _ = self.events.send(CoreEvent {
                kind: "auto.eval".into(),
                session_id: String::new(),
                payload: serde_json::json!({ "ok": true, "act": false }),
            });
            return None;
        };
        // Eval 通过：空传统计复位，开自主行动会话执行
        self.auto_state.write().idle_streak = 0;
        let _ = self.events.send(CoreEvent {
            kind: "auto.eval".into(),
            session_id: String::new(),
            payload: serde_json::json!({ "ok": true, "act": true, "task": task, "why": why }),
        });
        let title = format!("auto-agent:{}", task.chars().take(30).collect::<String>());
        let session = self.create_session(&title).ok()?;
        let prompt = format!(
            "{}\n\n## 任务输入\n目标：{task}\n选定理由：{why}\n步数预算：{}",
            crate::autonomy::AUTO_ACT_DISCIPLINE,
            crate::autonomy::auto_act_max_steps(&cfg),
        );
        let result = self.chat(&session.id, &prompt).await;
        let _ = self.events.send(CoreEvent {
            kind: "auto.act".into(),
            session_id: session.id.clone(),
            payload: serde_json::json!({
                "task": task,
                "status": if result.is_ok() { "done" } else { "failed" },
                "error": result.err().map(|e| e.to_string()),
            }),
        });
        Some((task, why))
    }

    /// Meta Agent 一次巡检 tick（实验性 experiments.metaAgent）：周期判定 →
    /// 数据快照注入的 Eval 自评 →（通过）开自主会话（装 meta 工具组）分析落提议。
    /// 调度器每 20s 调;内部分钟去重。0 = Some((focus, why)) 表示触发了巡检行动。
    pub async fn meta_agent_tick(&self, now_minute: i64) -> Option<(String, String)> {
        let cfg = self.config();
        if !cfg.experiments.meta_agent {
            return None;
        }
        let interval = cfg.automation.meta_interval_min.max(1) as i64;
        if interval == 0 {
            return None; // 0 = 仅会话召唤(被 max(1) 归一后由显式判断拦截——实际不可达,防御式)
        }
        let last = self.meta_state.read().last_tick_minute;
        if last != 0 && now_minute - last < interval {
            return None;
        }
        self.meta_state.write().last_tick_minute = now_minute;
        // 数据快照（Eval 输入）
        let orch = self.orchestrator();
        let obs = crate::meta::system_observation(&self.store, &orch.registry);
        let snapshot = serde_json::to_string_pretty(&obs).unwrap_or_default();
        // Eval 自评（非流式无工具）
        let req = crate::provider::ChatRequest::new(
            orch.orch_model.clone(),
            vec![
                crate::provider::ChatMessage::system(crate::meta::META_EVAL_PROMPT),
                crate::provider::ChatMessage::user(format!(
                    "运行数据快照（7 天窗口）:\n{snapshot}\n\n执行一次巡评。"
                )),
            ],
        );
        let output = match orch.orch_provider.chat(req).await {
            Ok(r) => r.content,
            Err(e) => {
                let _ = self.events.send(CoreEvent {
                    kind: "meta.eval".into(),
                    session_id: String::new(),
                    payload: serde_json::json!({ "ok": false, "error": e.to_string() }),
                });
                return None;
            }
        };
        let Some((focus, why)) = crate::meta::parse_meta_eval(&output) else {
            let _ = self.events.send(CoreEvent {
                kind: "meta.eval".into(),
                session_id: String::new(),
                payload: serde_json::json!({ "ok": true, "act": false }),
            });
            return None;
        };
        let _ = self.events.send(CoreEvent {
            kind: "meta.eval".into(),
            session_id: String::new(),
            payload: serde_json::json!({ "ok": true, "act": true, "focus": focus, "why": why }),
        });
        // 巡检行动会话:主智能体 + meta 工具组（runtime_allowlist 追加 meta 三件）
        let title = format!("meta-agent:{}", focus.chars().take(30).collect::<String>());
        let session = self.create_session(&title).ok()?;
        let prompt = format!(
            "{}\n\n## 巡检输入\n关注点：{focus}\n依据：{why}\n数据快照已可经 meta_observe 获取。",
            crate::meta::META_ACT_PROMPT,
        );
        let result = self.chat(&session.id, &prompt).await;
        let _ = self.events.send(CoreEvent {
            kind: "meta.act".into(),
            session_id: session.id.clone(),
            payload: serde_json::json!({
                "focus": focus,
                "status": if result.is_ok() { "done" } else { "failed" },
                "error": result.err().map(|e| e.to_string()),
            }),
        });
        Some((focus, why))
    }

    /// 指挥体身份 = 激活组主智能体 identifier（自定义组随组切换）
    /// 应用 [meta] 提议（批准后）：按 kind 分发到既有写路径。
    /// - prompt：组内 prompts/ 追加补段（带锚点注释）+ 旧版备份 prompts/history/
    /// - adaptation：save_adaptation（自动版本留痕）
    /// - roster：两阶段（全量校验 → 逐 op 应用,失败逆序回滚）
    /// - build：注册新个体（agent_manage create 同校验）
    async fn apply_meta_proposal(&self, req: &crate::types::ApprovalRequest) -> anyhow::Result<String> {
        let raw = req
            .meta_json
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("提议单缺 meta_json 载荷"))?;
        let p: crate::tools::MetaProposal = serde_json::from_str(raw)
            .map_err(|e| anyhow::anyhow!("提议载荷解析失败: {e}"))?;
        let orch = self.orchestrator.read().clone();
        match p.kind.as_str() {
            "prompt" => {
                let patch = p
                    .change
                    .get("promptPatch")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("change.promptPatch 缺失"))?;
                append_prompt_patch(&orch, &p.target, patch)
            }
            "adaptation" => {
                let content = p
                    .change
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("change.content 缺失"))?;
                let old = orch.registry.load_adaptation(&p.target)?;
                let adapt = crate::types::AgentAdaptation {
                    identifier: p.target.clone(),
                    revision: old.as_ref().map(|a| a.revision + 1).unwrap_or(1),
                    content: content.to_string(),
                    lessons_seen: old.as_ref().map(|a| a.lessons_seen).unwrap_or(0),
                    updated_at: String::new(),
                    basis: old.as_ref().and_then(|a| a.basis.clone()),
                    previous: Vec::new(),
                };
                let saved = orch.registry.save_adaptation(adapt)?;
                Ok(format!(
                    "经验要点已沉淀: {} rev={}（历史版本自动保留）",
                    saved.identifier, saved.revision
                ))
            }
            "roster" => {
                let ops = p
                    .change
                    .get("ops")
                    .and_then(|v| v.as_array())
                    .ok_or_else(|| anyhow::anyhow!("change.ops 缺失"))?;
                apply_roster_ops(self, &p.target, ops).await
            }
            "build" => {
                let def = p.change.get("definition").cloned().unwrap_or_default();
                let prompt = p
                    .change
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                register_new_agent(self, &def, prompt).await
            }
            other => Err(anyhow::anyhow!("未知提议类型: {other}")),
        }
    }

    pub fn orchestrator_id(&self) -> String {

        self.registry
            .primary()
            .map(|p| p.identifier)
            .unwrap_or_else(|| ORCHESTRATOR_ID.to_string())
    }
}

// 依赖装配函数：八个入参即 Orchestrator 的全部协作者，收拢成 struct 反而造一个只用一次的壳
#[allow(clippy::too_many_arguments)]
pub fn build_orchestrator(
    cfg: &ExmConfig,
    store: &Arc<Store>,
    registry: &Arc<LocalRegistry>,
    events: &broadcast::Sender<CoreEvent>,
    memory: &Arc<MemoryStore>,
    mcp: Option<Arc<crate::mcp::McpRegistry>>,
    remote: Option<Arc<dyn crate::remote::RemoteExecutor>>,
    cron: Option<Arc<CronStore>>,
) -> anyhow::Result<Orchestrator> {
    // 全局通道：有端点与密钥即真实推理；未配置时——测试替身模式给替身，产品模式给「未配置」指引通道
    let provider: Arc<dyn LlmProvider> = {
        let mut keys = cfg.llm.api_keys.clone();
        if keys.is_empty() && !cfg.llm.api_key.trim().is_empty() {
            keys.push(cfg.llm.api_key.clone());
        }
        if keys.is_empty() || cfg.llm.base_url.trim().is_empty() {
            if cfg.use_mock {
                Arc::new(MockLlmProvider)
            } else {
                Arc::new(UnconfiguredProvider::new("全局通道"))
            }
        } else {
            Arc::new(OpenAiCompatibleProvider::with_format(
                cfg.llm.base_url.clone(),
                keys,
                &cfg.llm.api_format,
            ))
        }
    };
    let mcp = mcp.unwrap_or_else(crate::mcp::McpRegistry::shared);
    mcp.configure(&cfg.mcp_servers);
    let mut gateway = ToolGateway::new(
        &cfg.workspace_root,
        store.clone(),
        registry.clone(),
        events.clone(),
        cfg.security.clone(),
    )
    .with_search(cfg.search.clone())
    .with_hooks(cfg.hooks.clone())
    .with_sandbox(cfg.sandbox.clone())
    .with_browser(cfg.browser.clone())
    .with_computer(cfg.computer.clone())
    .with_custom_tools(cfg.tools.custom.clone())
    .with_mcp(mcp);
    if let Some(c) = &cron {
        gateway = gateway.with_cron(c.clone());
    }
    // 记忆工具（memory_read/write/link）的受控入口：组隔离在工具层强制
    gateway = gateway.with_memory(memory.clone());
    // 记忆模式对齐（深层记忆关闭 = 文件记忆模式）：工具面读 md、拒写库，与规划注入同源
    gateway = gateway.with_memory_mode(cfg.memory_enabled, cfg.memory_md_path.clone());
    let tools = Arc::new(gateway);
    let unit_runtime = Arc::new(
        AgentRuntime::new(
            registry.clone(),
            provider.clone(),
            tools.clone(),
            cfg.llm.model.clone(),
        )
        .with_experiments(cfg.experiments.clone()),
    );
    // 动态上下文压缩（实验性）：开关开启才装配压缩器——schema 下发与执行体同门。
    // 滑动窗口模式（实验性，互斥）开启时不装配：窗口语义替代模型主动压缩。
    if cfg.experiments.dynamic_compaction && !cfg.experiments.sliding_context_window {
        tools.set_compactor(provider.clone(), cfg.llm.model.clone());
    }
    // 模型档案运行池：每个档案独立 Provider。
    // 规则：**显式配置优先**——有端点与密钥即真实推理；未配置时，测试替身模式给替身，产品模式给「未配置」指引通道。
    let mut pool = ModelPool::new();
    for p in &cfg.llm_profiles {
        let mut keys = p.api_keys.clone();
        if keys.is_empty() && !p.api_key.trim().is_empty() {
            keys.push(p.api_key.clone());
        }
        let profile_provider: Arc<dyn LlmProvider> =
            if keys.is_empty() || p.base_url.trim().is_empty() {
                if cfg.use_mock {
                    Arc::new(MockLlmProvider)
                } else {
                    Arc::new(UnconfiguredProvider::new(format!("档案 {}", p.id)))
                }
            } else {
                Arc::new(OpenAiCompatibleProvider::with_format(
                    p.base_url.clone(),
                    keys,
                    &p.api_format,
                ))
            };
        pool.insert(p.id.clone(), profile_provider, p.model.clone());
        pool.set_models(p.id.clone(), p.models.clone());
        if let Some(fb) = &p.fallback {
            pool.set_fallback(&p.id, fb);
        }
    }
    pool.set_active(cfg.active_profile.clone());
    let pool = Arc::new(pool);
    // 子智能体派生执行器（spawn 工具，spec: subagent-spawn）：
    // 构造临时个体（自包含任务书为系统提示）+ 空历史跑 chat_execute。
    // 独立上下文 = 不继承会话历史；子级工具白名单在网关层已剔除 spawn/agent_manage（单层限制）。
    {
        let runtime = unit_runtime.clone();
        let default_model = cfg.llm.model.clone();
        let spawn_exec: crate::tools::SpawnExec = Arc::new(
            move |agent_id: &str,
                  session_id: &str,
                  task: &str,
                  tools: Vec<crate::types::ToolName>| {
                let runtime = runtime.clone();
                let default_model = default_model.clone();
                let def = crate::types::AgentDefinition {
                    name: "派生子智能体".into(),
                    identifier: format!("spawn-of-{agent_id}"),
                    domain: "派生".into(),
                    tier: crate::types::Tier::Unit,
                    description: "封闭子任务执行（独立上下文）".into(),
                    capabilities: vec![],
                    tools,
                    when_to_call: String::new(),
                    dependencies: vec![],
                    composable_with: vec![],
                    input_schema: Default::default(),
                    output_schema: Default::default(),
                    prompt_file: String::new(),
                    model_hint: None,
                    link: None,
                    max_steps: None,
                };
                let system = format!(
                "# 派生子智能体\n\n你是被派生出来执行封闭子任务的智能体。看不到主会话历史，\
                 以下任务书即全部背景；自主使用工具完成并输出结论摘要（含关键产出引用）。\n\n## 任务书\n{task}"
            );
                let chain = crate::provider::ModelChain {
                    candidates: vec![(String::new(), runtime.provider_clone(), default_model)],
                    failover: Arc::new(crate::provider::FailoverState::default()),
                };
                let session = session_id.to_string();
                Box::pin(async move {
                    runtime
                        .chat_execute(
                            &def,
                            system,
                            vec![],
                            &session,
                            &format!("{session}:spawn"),
                            chain,
                            false,
                            crate::types::ToolScope::Tools,
                            |_| {},
                        )
                        .await
                })
            },
        );
        tools.set_spawn_executor(spawn_exec);
    }
    Ok(Orchestrator {
        registry: registry.clone(),
        orch_provider: provider,
        model_pool: pool,
        failover: Arc::new(FailoverState::default()),
        remote_enabled: remote.is_some(),
        remote,
        unit_runtime,
        tools,
        store: store.clone(),
        memory: memory.clone(),
        memory_enabled: cfg.memory_enabled,
        memory_recall_limit: cfg.memory_recall_limit,
        memory_md_path: cfg.memory_md_path.clone(),
        events: events.clone(),
        orch_model: cfg.llm.model.clone(),
        memory_semantic_model: cfg.memory_semantic_model.clone(),
        speech_target: cfg.speech_model.clone(),
        stt_target: cfg.stt_model.clone(),
        vision_relay_target: cfg.vision_relay_model.clone(),
        max_concurrency: cfg.max_concurrency,
        max_unit_steps: cfg.automation.unit_max_steps,
        auto_adapt: cfg.automation.auto_adapt,
        config_snapshot: Arc::new(parking_lot::RwLock::new(cfg.clone())),
    })
}

// ---------------------------------------------------------------- [meta] 提议应用辅助（meta-agent）

/// prompt 类应用：组内提示词追加补段（锚点注释定位）+ 旧版备份 history/
fn append_prompt_patch(
    orch: &Orchestrator,
    target: &str,
    patch: &str,
) -> anyhow::Result<String> {
    let Some(gdir) = orch.registry.group_dir_of_agent(target) else {
        anyhow::bail!("个体不在激活组内（提示词补段只作用于组内成员）: {target}");
    };
    let prompt_file = gdir.join("prompts").join(format!("{target}.md"));
    let old = std::fs::read_to_string(&prompt_file).unwrap_or_default();
    // 备份旧版（history/ 按时间戳命名,可手工回滚）
    let hist_dir = gdir.join("prompts").join("history");
    std::fs::create_dir_all(&hist_dir)?;
    let stamp = crate::types::now_iso().replace(':', "-");
    std::fs::write(
        hist_dir.join(format!("{target}-{stamp}.md")),
        &old,
    )?;
    // 追加补段（锚点注释 = 生成时间,人工可 grep 定位全部元智能体改动）
    let anchor = format!("<!-- meta: {} -->", crate::types::now_iso());
    let new = format!("{old}\n\n{anchor}\n{patch}\n");
    std::fs::write(&prompt_file, new)?;
    Ok(format!(
        "提示词补段已应用: {target}（锚点 {anchor};旧版已备份 prompts/history/）"
    ))
}

/// roster 类应用：两阶段——先全量校验（agent_manage_op 干跑）,再逐 op 应用。
/// 应用中断时逆序回滚已应用项。
async fn apply_roster_ops(
    core: &Core,
    _target: &str,
    ops: &[serde_json::Value],
) -> anyhow::Result<String> {
    let orch = core.orchestrator.read().clone();
    let Some(meta) = orch.registry.active_group_meta() else {
        anyhow::bail!("无激活组");
    };
    if meta.builtin {
        anyhow::bail!("内置组的定义受保护：提议被拒绝");
    }
    // 阶段一：全量校验（对 add 校验可解析,对 remove/update 校验存在性）
    let mut checked: Vec<(String, serde_json::Value)> = Vec::new();
    for op in ops {
        let opk = op.get("op").and_then(|v| v.as_str()).unwrap_or("");
        let ident = op.get("identifier").and_then(|v| v.as_str()).unwrap_or("");
        match opk {
            "add" => {
                let name = op.get("name").and_then(|v| v.as_str()).unwrap_or("");
                if ident.is_empty() || name.is_empty() {
                    anyhow::bail!("add op 需要 identifier/name");
                }
                if orch.registry.get(ident).is_some() {
                    anyhow::bail!("个体已存在: {ident}");
                }
            }
            "remove" | "update" => {
                if orch.registry.get(ident).is_none() {
                    anyhow::bail!("个体不存在: {ident}");
                }
            }
            other => anyhow::bail!("未知 roster op: {other}"),
        }
        checked.push((opk.to_string(), op.clone()));
    }
    // 阶段二：逐 op 应用;记录已应用的反操作用于回滚。
    // roster 语义 → agent_manage 动作面映射:add=create / remove=delete
    let managed_op = |k: &str| -> String {
        match k {
            "add" => "create".to_string(),
            "remove" => "delete".to_string(),
            other => other.to_string(),
        }
    };
    let mut applied: Vec<(String, String)> = Vec::new(); // (op, identifier)
    for (opk, op) in &checked {
        // 每次应用前取最新组快照:前一个 op 可能改 primary/builtin 态,
        // 陈旧 meta 会误判(primary.is_none() 导致误设主智能体)
        let live_meta = orch
            .registry
            .active_group_meta()
            .unwrap_or_else(|| meta.clone());
        let res = orch.tools.agent_manage_op(&live_meta, &managed_op(opk), op);
        match res {
            Ok(_) => applied.push((opk.clone(), op.get("identifier").and_then(|v| v.as_str()).unwrap_or("").to_string())),
            Err(e) => {
                // 逆序回滚
                for (aop, aid) in applied.iter().rev() {
                    let undo = if aop == "add" {
                        serde_json::json!({ "op": "delete", "identifier": aid })
                    } else {
                        serde_json::json!({ "op": "create", "identifier": aid, "name": aid, "description": "回滚占位（roster 中断恢复）" })
                    };
                    let lm = orch.registry.active_group_meta().unwrap_or_else(|| meta.clone());
                    let _ = orch.tools.agent_manage_op(&lm, "delete", &undo);
                }
                anyhow::bail!("roster 应用中断（{e}）;已回滚 {} 项", applied.len());
            }
        }
    }
    Ok(format!("roster 提议已应用: {} 项操作全部成功", checked.len()))
}

/// build 类应用：注册新个体（走 agent_manage create 同校验路径）
async fn register_new_agent(
    core: &Core,
    def: &serde_json::Value,
    prompt: &str,
) -> anyhow::Result<String> {
    let orch = core.orchestrator.read().clone();
    let Some(meta) = orch.registry.active_group_meta() else {
        anyhow::bail!("无激活组");
    };
    if meta.builtin {
        anyhow::bail!("内置组的定义受保护：不能在内置组建新个体");
    }
    let create_arg = serde_json::json!({
        "op": "create",
        "identifier": def.get("name").and_then(|v| v.as_str()).unwrap_or(""),
        "name": def.get("name").and_then(|v| v.as_str()).unwrap_or(""),
        "description": def
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("元智能体构建"),
        "domain": def.get("domain").and_then(|v| v.as_str()).unwrap_or(""),
        "tier": def.get("tier").and_then(|v| v.as_str()).unwrap_or("unit"),
        "prompt": prompt,
    });
    let out = orch.tools.agent_manage_op(&meta, "create", &create_arg)?;
    Ok(format!(
        "新个体已注册: {}（{}）",
        def.get("name").and_then(|v| v.as_str()).unwrap_or("?"),
        serde_json::to_string(&out).unwrap_or_default()
    ))
}

// ---------------------------------------------------------------- 会话历史管控（组 9 单测）

#[cfg(test)]
mod im_discipline_tests {
    /// 会话归档状态机（spec session-management）：归档移出主列表、
    /// 归档区可见、恢复回主列表原分组、组内列表同口径过滤。
    #[test]
    fn 会话归档_过滤恢复与隔离() {
        let dir = std::env::temp_dir().join(format!("exm-arch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        let core = crate::Core::create(&dir).expect("创建 Core 失败");
        let a = core.create_session("arch-a").unwrap();
        let b = core.create_session("arch-b").unwrap();
        // 归档 a：主列表（组内 + 全量）只剩 b
        core.set_session_archived(&a.id, true).unwrap();
        let group_ids: Vec<String> = core
            .list_sessions_in_group(&a.group_id)
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert!(!group_ids.contains(&a.id), "归档态不进组内主列表");
        assert!(group_ids.contains(&b.id));
        let all_ids: Vec<String> = core.list_sessions().unwrap().into_iter().map(|s| s.id).collect();
        assert!(!all_ids.contains(&a.id), "归档态不进全量主列表");
        // 归档区：a 可见且带 archived_at 时间戳
        let archived = core.list_archived_sessions().unwrap();
        assert_eq!(archived.len(), 1, "归档区恰一条");
        assert_eq!(archived[0].id, a.id);
        assert!(archived[0].archived_at.is_some(), "归档时间落档");
        // 恢复：主列表回归（同组原位），归档区清空
        core.set_session_archived(&a.id, false).unwrap();
        let group_ids: Vec<String> = core
            .list_sessions_in_group(&a.group_id)
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert!(group_ids.contains(&a.id), "恢复后回主列表");
        assert!(core.list_archived_sessions().unwrap().is_empty(), "归档区清空");
        // 不存在的会话：报错不 panic
        assert!(core.set_session_archived("no-such", true).is_err());
    }
    /// 流名维护（会话管理可用性修复）：空填不覆盖——既有名（用户自定义或
    /// 更早来源）不被新名冲掉；空名流首名即落。
    #[test]
    fn 流名维护_空填不覆盖() {
        let dir = std::env::temp_dir().join(format!("exm-flowname-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        let core = crate::Core::create(&dir).expect("创建 Core 失败");
        let sid = core.create_session("fn-test").unwrap().id;
        // 建流带名
        core.bind_flow_session_named("ch1", "12345", &sid, Some("萝莉领主二群"), Some(true))
            .unwrap();
        let f = core.channel_flow("ch1", "12345").unwrap();
        assert_eq!(f.display_name, "萝莉领主二群");
        assert!(f.is_group, "群聊标记应落");
        // 再补名不覆盖（群改名事件到达也不冲掉——展示名以首次为准，
        // 用户在控制台改名后更不能被冲）
        core.ensure_flow_named("ch1", "12345", "新群名", true);
        let f = core.channel_flow("ch1", "12345").unwrap();
        assert_eq!(f.display_name, "萝莉领主二群", "已有名不得被覆盖");
        // 空名流首名即落（ensure_flow_named 空填路径）
        let sid2 = core.create_session("fn-test-2").unwrap().id;
        core.bind_flow_session_named("ch1", "67890", &sid2, None, None)
            .unwrap();
        assert_eq!(
            core.channel_flow("ch1", "67890").unwrap().display_name,
            "",
            "无名建流保持空"
        );
        core.ensure_flow_named("ch1", "67890", "终夜", false);
        assert_eq!(core.channel_flow("ch1", "67890").unwrap().display_name, "终夜");
        // 空串补名是 no-op（解析失败的适配器不该写入空名）
        core.ensure_flow_named("ch1", "67890", "", false);
        assert_eq!(core.channel_flow("ch1", "67890").unwrap().display_name, "终夜");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 流未读计数（spec experience-iteration 未读徽标服务端口径）：
    /// 有流才计数、累积不溢出、清零即除名、无流 bump 静默忽略。
    #[test]
    fn 流未读_累积与清零() {
        let dir = std::env::temp_dir().join(format!("exm-unread-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        let core = crate::Core::create(&dir).expect("创建 Core 失败");
        // 建流（bind_flow_session 顺带建流文档）
        let s = core.create_session("unread-test").unwrap();
        core.bind_flow_session("ch1", "g100", &s.id).unwrap();
        assert_eq!(core.flow_unread_count("ch1", "g100"), 0, "初始零未读");
        core.bump_flow_unread("ch1", "g100");
        core.bump_flow_unread("ch1", "g100");
        assert_eq!(core.flow_unread_count("ch1", "g100"), 2, "两次回复累积 2");
        core.clear_flow_unread("ch1", "g100");
        assert_eq!(core.flow_unread_count("ch1", "g100"), 0, "打开会话清零");
        // 无流 bump：静默忽略不 panic、不产生幽灵计数
        core.bump_flow_unread("ch-none", "g-none");
        assert_eq!(core.flow_unread_count("ch-none", "g-none"), 0);
    }
    /// IM 会话标记状态机：登记幂等、除名后不再命中、独立于轮次消费（不 take）。
    /// 注入路径（single_direct_round / converge 读 is_im_session 拼 IM_DISCIPLINE）
    /// 由编译器保证走同一分支，此处锚定标记语义与纪律文本的关键约束项。
    #[test]
    fn im会话标记_登记幂等与除名() {
        let sid = format!("im-test-{}", crate::types::new_id());
        assert!(!crate::orchestrator::is_im_session(&sid));
        crate::orchestrator::mark_im_session(&sid);
        crate::orchestrator::mark_im_session(&sid); // 幂等
        assert!(crate::orchestrator::is_im_session(&sid));
        crate::orchestrator::unmark_im_session(&sid);
        assert!(!crate::orchestrator::is_im_session(&sid));
    }

    #[test]
    fn im纪律文本_含关键约束() {
        let d = crate::orchestrator::IM_DISCIPLINE;
        assert!(d.contains("先给结论"), "结论先行：{d}");
        assert!(d.contains("1~5 句"), "句数上限：{d}");
        assert!(d.contains("不逐条播报"), "过程静默：{d}");
        assert!(d.contains("噪音"), "禁播报前缀：{d}");
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;
    use crate::types::{AgentDefinition, Tier};

    fn mock_core() -> Core {
        let dir = std::env::temp_dir().join(format!("exm-hist-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false; // 历史管控测试不关心记忆写入
        let core = Core::with_config(cfg).expect("创建 Core 失败");
        core.registry()
            .create_group(Some("t".into()), "历史测试组", "")
            .unwrap();
        let def = AgentDefinition {
            name: "问答体".into(),
            identifier: "hist-orch".into(),
            domain: "测试".into(),
            tier: Tier::Orchestrator,
            description: "测试主智能体".into(),
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
        };
        core.registry().upsert_agent("t", def, None).unwrap();
        core.registry().set_primary("t", "hist-orch").unwrap();
        core.registry().set_active_group("t").unwrap();
        core
    }

    async fn three_rounds(core: &Core, sid: &str) {
        core.chat(sid, "第一轮问题").await.unwrap();
        core.chat(sid, "第二轮问题").await.unwrap();
        core.chat(sid, "第三轮问题").await.unwrap();
    }

    #[tokio::test]
    async fn 轮次快照_多轮对话后快照链完整() {
        let core = mock_core();
        let s = core.create_session("快照链").unwrap();
        three_rounds(&core, &s.id).await;
        let snaps = core.store.list_turn_snapshots(&s.id).unwrap();
        assert_eq!(snaps.len(), 3, "每轮一条快照");
        for (i, snap) in snaps.iter().enumerate() {
            assert_eq!(snap.turn, i + 1, "轮次从 1 递增");
            assert_eq!(snap.status, "done");
        }
        // 游标单调不减且等于当前消息总数
        let total = core.store.list_messages(&s.id, 0).unwrap().len();
        assert_eq!(snaps.last().unwrap().message_count, total);
        assert!(
            snaps
                .windows(2)
                .all(|w| w[0].message_count <= w[1].message_count),
            "游标单调"
        );
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    #[tokio::test]
    async fn undo_空闲回退_归档可查_快照裁剪() {
        let core = mock_core();
        let s = core.create_session("撤销").unwrap();
        three_rounds(&core, &s.id).await;
        let total = core.store.list_messages(&s.id, 0).unwrap().len();

        let info = core.undo_session(&s.id, 2, false).await.unwrap();
        assert_eq!(
            info["archived"].as_u64().unwrap() as usize,
            total - snaps_at(&core, &s.id, 2),
            "归档数 = 第 2 轮之后的消息数"
        );
        // 活跃流截断到第 2 轮游标
        let cursor = core
            .store
            .list_turn_snapshots(&s.id)
            .unwrap()
            .last()
            .unwrap()
            .message_count;
        let now_len = core.store.list_messages(&s.id, 0).unwrap().len();
        assert_eq!(now_len, cursor, "截断后游标一致");
        assert!(now_len < total, "确实发生了截断");
        // 归档可查：被截断内容保留
        let archive = core.store.list_archived_messages(&s.id).unwrap();
        assert_eq!(archive.len(), total - now_len);
        assert!(
            archive
                .iter()
                .any(|m| m.statements.iter().any(|st| st.text.contains("第三轮"))),
            "第三轮内容在归档中"
        );
        // 孤儿快照已裁剪
        assert_eq!(snaps_len(&core, &s.id), 2);
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    fn snaps_len(core: &Core, sid: &str) -> usize {
        core.store.list_turn_snapshots(sid).unwrap().len()
    }

    fn snaps_at(core: &Core, sid: &str, turn: usize) -> usize {
        core.store
            .list_turn_snapshots(sid)
            .unwrap()
            .iter()
            .find(|s| s.turn == turn)
            .map(|s| s.message_count)
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn undo_处理中拒绝() {
        let core = mock_core();
        let s = core.create_session("忙碌").unwrap();
        core.chat(&s.id, "先来一轮").await.unwrap();
        // 占住运行锁（模拟处理中）
        let lock = core.run_lock(&s.id);
        let guard = lock.lock().await;
        let err = core.undo_session(&s.id, 1, false).await.unwrap_err();
        assert!(err.to_string().contains("处理中"), "应拒绝：{err}");
        drop(guard);
        // 空闲后可撤销
        assert!(core.undo_session(&s.id, 1, false).await.is_ok());
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    #[tokio::test]
    async fn undo_检查点联动回滚文件() {
        let core = mock_core();
        let s = core.create_session("文件联动").unwrap();
        // 构造两轮：第 1 轮登记检查点（文件 v1 → v2），第 2 轮普通
        let file = core.config().workspace_root.join("a.txt");
        std::fs::write(&file, "v1").unwrap();
        let day: String = now_iso().chars().take(10).collect();
        // 检查点文件（写类工具写前快照的同款落点）+ 轮次链登记
        let cp = core
            .config()
            .workspace_root
            .join(".exmachina")
            .join("checkpoints")
            .join(&day)
            .join("a.txt");
        std::fs::create_dir_all(cp.parent().unwrap()).unwrap();
        std::fs::write(&cp, "v1").unwrap();
        core.store
            .record_checkpoint_ref(&s.id, &day, "a.txt")
            .unwrap();
        std::fs::write(&file, "v2").unwrap();
        core.store
            .put_turn_snapshot(&TurnSnapshot {
                id: format!("{}:1", s.id),
                session_id: s.id.clone(),
                turn: 1,
                message_count: 2,
                checkpoint_ids: vec![format!("{day}/a.txt")],
                prompt_tokens: 0,
                completion_tokens: 0,
                status: "done".into(),
                created_at: now_iso(),
            })
            .unwrap();
        core.store
            .add_message(
                &s.id,
                MessageRole::User,
                None,
                vec![Statement::new(SpeechTag::要求, "改文件")],
            )
            .unwrap();
        core.store
            .put_turn_snapshot(&TurnSnapshot {
                id: format!("{}:2", s.id),
                session_id: s.id.clone(),
                turn: 2,
                message_count: 3,
                checkpoint_ids: vec![],
                prompt_tokens: 0,
                completion_tokens: 0,
                status: "done".into(),
                created_at: now_iso(),
            })
            .unwrap();

        // 撤销到第 1 轮（保留）→ 第 2 轮无检查点；撤销到第 0 轮 → 第 1 轮的检查点回滚文件
        let info = core.undo_session(&s.id, 0, true).await.unwrap();
        let restored: Vec<String> = serde_json::from_value(info["restored"].clone()).unwrap();
        assert_eq!(
            restored,
            vec![format!("{day}/a.txt")],
            "检查点引用被联动回滚"
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "v1",
            "文件内容回到检查点版本"
        );
        assert_eq!(
            core.store.list_messages(&s.id, 0).unwrap().len(),
            0,
            "游标回退到 0"
        );
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    #[tokio::test]
    async fn 编辑重发_原内容归档_新内容重执行() {
        let core = mock_core();
        let s = core.create_session("改写").unwrap();
        three_rounds(&core, &s.id).await;
        let before = core.store.list_messages(&s.id, 0).unwrap().len();

        core.edit_resend(&s.id, 2, "第二轮问题（已修改）")
            .await
            .unwrap();
        // 原内容归档可查
        let archive_text: String = core
            .store
            .list_archived_messages(&s.id)
            .unwrap()
            .iter()
            .flat_map(|m| m.statements.iter().map(|st| st.text.clone()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            archive_text.contains("第二轮问题"),
            "原第 2 轮内容在归档中：{archive_text}"
        );
        assert!(archive_text.contains("第三轮问题"), "第 3 轮一并被回退归档");
        // 新内容重执行：活跃流 = 第 1 轮 + 新一轮，最后一条用户消息为新文本
        let msgs = core.store.list_messages(&s.id, 0).unwrap();
        assert!(msgs.len() < before + 2, "重发后活跃流短于原全程");
        let last_user = msgs
            .iter()
            .rev()
            .find(|m| matches!(m.role, MessageRole::User))
            .unwrap();
        assert!(
            last_user.statements[0].text.contains("已修改"),
            "以编辑后的内容重执行"
        );
        // 新一轮也落了快照
        let snaps = core.store.list_turn_snapshots(&s.id).unwrap();
        assert_eq!(snaps.len(), 2, "第 1 轮保留 + 重发轮新快照");
        assert_eq!(snaps.last().unwrap().turn, 2);
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    #[tokio::test]
    async fn fork_派生独立演进_原会话不变() {
        let core = mock_core();
        let s = core.create_session("分叉源").unwrap();
        three_rounds(&core, &s.id).await;
        let src_msgs = core.store.list_messages(&s.id, 0).unwrap().len();
        let src_snaps = snaps_len(&core, &s.id);

        let fork = core.fork_session(&s.id, 2).unwrap();
        assert_ne!(fork.id, s.id);
        assert_eq!(
            fork.parent_session.as_deref(),
            Some(s.id.as_str()),
            "派生来源标注"
        );
        assert_eq!(fork.parent_upto, Some(2));
        assert_eq!(fork.group_id, "t", "组归属继承");
        // 历史副本 = 截至第 2 轮
        let copied = core.store.list_messages(&fork.id, 0).unwrap();
        let cursor = core
            .store
            .list_turn_snapshots(&s.id)
            .unwrap()
            .iter()
            .find(|x| x.turn == 2)
            .unwrap()
            .message_count;
        assert_eq!(copied.len(), cursor, "副本 = 前缀游标");
        assert!(copied.len() < src_msgs);
        // 原会话不变
        assert_eq!(core.store.list_messages(&s.id, 0).unwrap().len(), src_msgs);
        assert_eq!(snaps_len(&core, &s.id), src_snaps);
        // 分支独立演进：新消息只进分支
        core.chat(&fork.id, "分支上的新问题").await.unwrap();
        assert!(core.store.list_messages(&fork.id, 0).unwrap().len() > copied.len());
        assert_eq!(
            core.store.list_messages(&s.id, 0).unwrap().len(),
            src_msgs,
            "原会话不受分支影响"
        );
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    #[tokio::test]
    async fn undo_不存在的轮次与会话报错() {
        let core = mock_core();
        let s = core.create_session("边界").unwrap();
        assert!(
            core.undo_session(&s.id, 5, false).await.is_err(),
            "无快照轮次拒绝"
        );
        assert!(
            core.undo_session("no-such-session", 1, false)
                .await
                .is_err(),
            "不存在会话拒绝"
        );
        assert!(core.fork_session("no-such-session", 1).is_err());
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }
}

// ---------------------------------------------------------------- 计划模式集成（组 4）

#[cfg(test)]
mod plan_tests {
    use super::*;

    fn plan_core() -> Core {
        let dir = std::env::temp_dir().join(format!("exm-plan-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        Core::with_config(cfg).expect("创建 Core 失败")
    }

    /// 计划模式全链路：开启 → 拦截写转条目 + 落审批单 → 批准（approvals 通道）→ 回放落地
    #[tokio::test]
    async fn 计划审批_批准经approvals通道回放落地() {
        let core = plan_core();
        let s = core.create_session("计划").unwrap();
        core.set_plan_mode(&s.id, true);
        assert!(crate::plan::is_plan_mode(&s.id));
        // 拦截一次写（filesystem write）→ 1 条计划 + 关联审批单
        let orch = core.orchestrator.read().clone();
        let r = orch
            .tools
            .execute(
                "a1",
                &s.id,
                &[crate::types::ToolName::Filesystem],
                crate::types::ToolName::Filesystem,
                &serde_json::json!({ "op": "write", "path": "p.txt", "content": "v" }),
            )
            .await;
        assert!(r.ok && r.output.contains("计划模式"));
        assert_eq!(core.plan_list(&s.id).len(), 1);
        let pending = core.approval_list(Some("pending"), 10).unwrap();
        assert_eq!(pending.len(), 1, "应落一张计划审批单");
        assert!(
            pending[0].command.starts_with("[plan]"),
            "计划审批单带前缀标"
        );
        let ap_id = pending[0].id.clone();
        // 批准（经 approvals 通道）→ 回放执行
        let decided = core.approval_decide(&ap_id, true).await.unwrap();
        assert_eq!(decided.status, "executed");
        assert!(!crate::plan::is_plan_mode(&s.id), "批准后切 execute");
        assert!(core.plan_list(&s.id).is_empty(), "清单已消费");
        assert!(
            core.config().workspace_root.join("p.txt").exists(),
            "回放应真正写文件"
        );
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// 拒绝（approvals 通道）：清单保留、模式不变、文件不落
    #[tokio::test]
    async fn 计划审批_拒绝保留清单() {
        let core = plan_core();
        let s = core.create_session("计划拒绝").unwrap();
        core.set_plan_mode(&s.id, true);
        let orch = core.orchestrator.read().clone();
        let _ = orch
            .tools
            .execute(
                "a1",
                &s.id,
                &[crate::types::ToolName::Filesystem],
                crate::types::ToolName::Filesystem,
                &serde_json::json!({ "op": "write", "path": "q.txt", "content": "v" }),
            )
            .await;
        let ap_id = core.approval_list(Some("pending"), 10).unwrap()[0]
            .id
            .clone();
        core.approval_decide(&ap_id, false).await.unwrap();
        assert!(crate::plan::is_plan_mode(&s.id), "拒绝后仍在 plan 模式");
        assert_eq!(core.plan_list(&s.id).len(), 1, "清单保留");
        assert!(!core.config().workspace_root.join("q.txt").exists());
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// 空清单批准报错；模式位不影响读操作
    #[tokio::test]
    async fn 计划审批_空清单与读取放行() {
        let core = plan_core();
        let s = core.create_session("边界").unwrap();
        core.set_plan_mode(&s.id, true);
        assert!(core.plan_approve(&s.id).await.is_err(), "空清单应拒绝");
        let orch = core.orchestrator.read().clone();
        let r = orch
            .tools
            .execute(
                "a1",
                &s.id,
                &[crate::types::ToolName::Grep],
                crate::types::ToolName::Grep,
                &serde_json::json!({ "pattern": "fn " }),
            )
            .await;
        assert!(r.ok, "读操作放行：{:?}", r.error);
        // 清理：关模式并丢弃清单
        crate::plan::approve(&s.id);
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// 视频动作审批往返（[video] 前缀）：批准 = 回调代发执行器；拒绝 = 记录跳过
    #[tokio::test]
    async fn 视频动作审批_批准回调与拒绝跳过() {
        let core = plan_core();
        // 注入代发执行器（模拟网关侧注册；记录被调载荷）
        let called = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let sink = called.clone();
        core.register_video_act_executor(Arc::new(move |payload| {
            let sink = sink.clone();
            Box::pin(async move {
                sink.lock().push(payload);
                Ok("已代发：测试文本".into())
            })
        }));
        // 产一张 [video] 审批单（模拟 tool_act 的 create_act_approval）
        let req = crate::types::ApprovalRequest {
            id: format!("apr{}", &crate::types::new_id()[..14]),
            session_id: "s-video".into(),
            node_id: None,
            agent_id: "video".into(),
            command: "[video] {\"channelId\":\"bili\",\"videoId\":\"BV1\",\"text\":\"测试文本\"}"
                .into(),
            status: "pending".into(),
            result: None,
            created_at: crate::types::now_iso(),
            decided_at: None,
            origin_user: None,
            origin_role: None,
        
            meta_json: None,
        };
        core.store.add_approval(&req).unwrap();
        // 批准 → 执行器被调、审批单 executed
        let decided = core.approval_decide(&req.id, true).await.unwrap();
        assert_eq!(decided.status, "executed");
        assert!(decided.result.as_deref().unwrap_or("").contains("已代发"));
        assert_eq!(called.lock().len(), 1, "代发执行器应被调用一次");
        assert!(called.lock()[0].contains("BV1"));
        // 拒绝路径：再产一张 → denied，不触执行器
        let mut req2 = req.clone();
        req2.id = format!("apr{}", &crate::types::new_id()[..14]);
        req2.status = "pending".into();
        req2.decided_at = None;
        core.store.add_approval(&req2).unwrap();
        let denied = core.approval_decide(&req2.id, false).await.unwrap();
        assert_eq!(denied.status, "denied");
        assert_eq!(called.lock().len(), 1, "拒绝不应触代发");
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }
}

// ---------------- root 提权审批（agent-exec-confinement） ----------------

#[cfg(test)]
mod root_approval_tests {
    use super::*;

    fn root_core() -> Core {
        let dir = std::env::temp_dir().join(format!("exm-root-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        cfg.security.exec_user = "0".into(); // 数字 uid:约束开启(测试以 uid 0 近似)
        Core::with_config(cfg).expect("创建 Core 失败")
    }

    /// 提权命令无视白名单直达审批,审批单带 [root] 前缀
    #[tokio::test]
    async fn 提权命令_落root前缀审批单() {
        let core = root_core();
        let s = core.create_session("提权").unwrap();
        let orch = core.orchestrator.read().clone();
        // approval_wait_secs 默认 120,测试不等:落单后立即校验
        let _ = orch
            .tools
            .execute(
                "a1",
                &s.id,
                &[crate::types::ToolName::Terminal],
                crate::types::ToolName::Terminal,
                &serde_json::json!({ "command": "echo hello" }),
            )
            .await;
        // 直接走 gate 验证前缀语义(不经白名单拦截路径的等待)
        let ap = crate::types::ApprovalRequest {
            id: crate::types::new_id()[..8].to_string(),
            session_id: s.id.clone(),
            node_id: None,
            agent_id: "a1".into(),
            command: "[root] sudo id".into(),
            status: "pending".into(),
            result: None,
            created_at: crate::types::now_iso(),
            decided_at: None,
            origin_user: None,
            origin_role: None,
        
            meta_json: None,
        };
        core.store.add_approval(&ap).unwrap();
        // 批准 → 剥 sudo 壳以 root 执行 id
        let decided = core.approval_decide(&ap.id, true).await.unwrap();
        assert_eq!(decided.status, "executed");
        assert!(decided.result.as_deref().unwrap_or("").contains("uid="), "应执行 id 并带输出");
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// [root] 审批拒绝:受阻回流语义
    #[tokio::test]
    async fn 提权审批_拒绝回流() {
        let core = root_core();
        let ap = crate::types::ApprovalRequest {
            id: crate::types::new_id()[..8].to_string(),
            session_id: core.create_session("拒绝").unwrap().id,
            node_id: None,
            agent_id: "a1".into(),
            command: "[root] sudo rm -rf /".into(),
            status: "pending".into(),
            result: None,
            created_at: crate::types::now_iso(),
            decided_at: None,
            origin_user: None,
            origin_role: None,
        
            meta_json: None,
        };
        core.store.add_approval(&ap).unwrap();
        let decided = core.approval_decide(&ap.id, false).await.unwrap();
        assert_eq!(decided.status, "denied");
        assert_eq!(decided.result.as_deref(), Some("root 提权被拒绝"));
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// su 切换语义保留(不剥壳)
    #[tokio::test]
    async fn 提权审批_su语义保留() {
        let core = root_core();
        let ap = crate::types::ApprovalRequest {
            id: crate::types::new_id()[..8].to_string(),
            session_id: core.create_session("su").unwrap().id,
            node_id: None,
            agent_id: "a1".into(),
            command: "[root] su - nonexistentuser -c id".into(),
            status: "pending".into(),
            result: None,
            created_at: crate::types::now_iso(),
            decided_at: None,
            origin_user: None,
            origin_role: None,
        
            meta_json: None,
        };
        core.store.add_approval(&ap).unwrap();
        let decided = core.approval_decide(&ap.id, true).await.unwrap();
        // su 原样执行:用户不存在 → failed(但结果里应见 [root 执行] su 原文,证明未剥壳)
        let text = decided.result.as_deref().unwrap_or("");
        assert!(text.contains("[root 执行] su - nonexistentuser"), "su 语义保留: {text}");
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// approval_decide 代执行 cwd 回归:无身份约束时 cwd=workspace_root,
    /// 加固路径(shell_opts/bwrap)下 git 必须能识别仓库(/tmp 独立挂载桥接回归)
    #[tokio::test]
    async fn 审批代执行_cwd回归() {
        // 无 exec_user(约束关)——约束开启时 cwd 切 execHome 是另一语义,不在此测
        let dir = std::env::temp_dir().join(format!("exm-cwd-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        let core = Core::with_config(cfg).expect("创建 Core 失败");
        let ws = core.config().workspace_root.clone();
        std::process::Command::new("git").arg("init").current_dir(&ws).output().unwrap();
        // 直通参照
        let orch = core.orchestrator.read().clone();
        let eff = orch.tools.effective_root();
        eprintln!("ws={} eff={}", ws.display(), eff.display());
        let ap = crate::types::ApprovalRequest {
            id: crate::types::new_id()[..8].to_string(),
            session_id: core.create_session("cwd").unwrap().id,
            node_id: None,
            agent_id: "a1".into(),
            command: "git rev-parse --is-inside-work-tree".into(),
            status: "pending".into(),
            result: None,
            created_at: crate::types::now_iso(),
            decided_at: None,
            origin_user: None,
            origin_role: None,
        
            meta_json: None,
        };
        core.store.add_approval(&ap).unwrap();
        let decided = core.approval_decide(&ap.id, true).await.unwrap();
        eprintln!("decided={} result={:?}", decided.status, decided.result);
        assert_eq!(decided.status, "executed", "cwd 回归: {:?}", decided.result);
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// 普通审批单批准后走 shell_opts 加固路径(不降权环境下也不绕过沙箱)
    #[tokio::test]
    async fn 普通审批_批准后代执行仍受约束() {
        let core = root_core();
        let ap = crate::types::ApprovalRequest {
            id: crate::types::new_id()[..8].to_string(),
            session_id: core.create_session("普通").unwrap().id,
            node_id: None,
            agent_id: "a1".into(),
            command: "echo confined".into(),
            status: "pending".into(),
            result: None,
            created_at: crate::types::now_iso(),
            decided_at: None,
            origin_user: None,
            origin_role: None,
        
            meta_json: None,
        };
        core.store.add_approval(&ap).unwrap();
        let decided = core.approval_decide(&ap.id, true).await.unwrap();
        assert_eq!(decided.status, "executed");
        assert!(decided.result.as_deref().unwrap_or("").contains("confined"));
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }
}

/// 会话级审批授权（approval-ergonomics）：
/// 三档粒度 / 红线（[root]、破坏性永不入表）/ 会话删除清理 / gate 短路。
#[cfg(test)]
mod session_grant_tests {
    use super::*;
    use crate::types::ApprovalScope;

    fn grant_core() -> Core {
        let dir = std::env::temp_dir().join(format!("exm-grant-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        Core::with_config(cfg).expect("创建 Core 失败")
    }

    fn pending_ap(core: &Core, sid: &str, cmd: &str) -> crate::types::ApprovalRequest {
        let ap = crate::types::ApprovalRequest {
            id: crate::types::new_id()[..8].to_string(),
            session_id: sid.into(),
            node_id: None,
            agent_id: "a1".into(),
            command: cmd.into(),
            status: "pending".into(),
            result: None,
            created_at: crate::types::now_iso(),
            decided_at: None,
            origin_user: None,
            origin_role: None,
            meta_json: None,
        };
        core.store.add_approval(&ap).unwrap();
        ap
    }

    #[tokio::test]
    async fn session档_建立授权并命中() {
        let core = grant_core();
        let sid = core.create_session("授权").unwrap().id;
        let ap = pending_ap(&core, &sid, "echo hello");
        let decided = core
            .approval_decide_scoped(&ap.id, ApprovalScope::Session)
            .await
            .unwrap();
        assert_eq!(decided.status, "executed");
        assert!(decided.result.as_deref().unwrap_or("").contains("会话授权"));
        // 命中判定：前缀（echo hello）后接任意参数命中
        assert!(core.session_granted(&sid, "echo hello world").is_some());
        // 次 token 不同即不同类别（口径与白名单「命令+子命令」对齐）
        assert!(core.session_granted(&sid, "echo world").is_none());
        // 完全不同的命令不命中
        assert!(core.session_granted(&sid, "mkdir /tmp/x").is_none());
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    #[tokio::test]
    async fn once档_不建立授权() {
        let core = grant_core();
        let sid = core.create_session("单次").unwrap().id;
        let ap = pending_ap(&core, &sid, "mkdir -p /tmp/x");
        core.approval_decide_scoped(&ap.id, ApprovalScope::Once { approve: true })
            .await
            .unwrap();
        assert!(core.session_granted(&sid, "mkdir /tmp/y").is_none(), "once 档不建授权");
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    #[tokio::test]
    async fn root单_session档被降级() {
        let core = grant_core();
        let sid = core.create_session("红线").unwrap().id;
        let ap = pending_ap(&core, &sid, "[root] sudo id");
        let decided = core
            .approval_decide_scoped(&ap.id, ApprovalScope::Session)
            .await
            .unwrap();
        assert_eq!(decided.status, "executed");
        // 红线：[root] 单不允许升档——结果无 [会话授权] 注记（有 [红线] 降级说明）,也不建授权
        assert!(!decided.result.as_deref().unwrap_or("").contains("[会话授权]"));
        assert!(decided.result.as_deref().unwrap_or("").contains("[红线]"), "应注明降级");
        assert!(core.session_granted(&sid, "id").is_none());
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    #[tokio::test]
    async fn 破坏性命令_session档被降级() {
        let core = grant_core();
        let sid = core.create_session("破坏").unwrap().id;
        let ap = pending_ap(&core, &sid, "git push --force origin main");
        let decided = core
            .approval_decide_scoped(&ap.id, ApprovalScope::Session)
            .await
            .unwrap();
        // 执行本身按 once 发生(可能失败——force push 无远端;关键是档位被降)
        assert!(!decided.result.as_deref().unwrap_or("").contains("[会话授权]"));
        assert!(core.session_granted(&sid, "git push").is_none());
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    #[tokio::test]
    async fn allowlist档_写入配置() {
        let core = grant_core();
        let sid = core.create_session("白名单").unwrap().id;
        let ap = pending_ap(&core, &sid, "echo allowlist-test");
        let decided = core
            .approval_decide_scoped(&ap.id, ApprovalScope::Allowlist)
            .await
            .unwrap();
        assert!(decided.result.as_deref().unwrap_or("").contains("白名单"));
        // 前缀口径 = 命令名 + 首 token（echo allowlist-test）,与 command_head 一致
        assert!(core
            .config()
            .security
            .exec_allowlist
            .iter()
            .any(|p| p == "echo allowlist-test"), "前缀应写入 execAllowlist");
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    #[tokio::test]
    async fn 会话删除后授权失效() {
        let core = grant_core();
        let sid = core.create_session("删除").unwrap().id;
        core.grant_session(&sid, "cargo build");
        assert!(core.session_granted(&sid, "cargo build --release").is_some());
        core.delete_session(&sid).unwrap();
        assert!(core.session_granted(&sid, "cargo build --release").is_none());
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// gate_command 短路：探针注入 + 授权命中 → 不落审批单直接执行
    #[tokio::test]
    async fn gate短路_授权命中不落单() {
        let core = grant_core();
        let sid = core.create_session("短路").unwrap().id;
        // 授权前缀与 command_head 口径一致（echo + 首参数 token）
        core.grant_session(&sid, "echo granted-ok");
        // 探针注入（生产由 attach_session_grant_probe 完成;测试手动）
        let core = std::sync::Arc::new(core);
        core.attach_session_grant_probe();
        let orch = core.orchestrator.read().clone();
        let r = orch.tools.gate_command("a1", &sid, "echo granted-ok").await;
        assert!(r.ok, "授权直通应执行成功: {:?}", r.output);
        assert!(r.output.contains("granted-ok"));
        // 无新审批单产生
        let pendings = core.approval_list(Some("pending"), 10).unwrap();
        assert!(pendings.is_empty(), "授权命中不应落审批单");
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }
}

/// [meta] 提议单全链路（meta-agent）：落单 → 批准应用 / 拒绝回流 → 各 kind 语义
#[cfg(test)]
mod meta_approval_tests {
    use super::*;

    /// 自定义组场景（[meta] 的 prompt/roster/build 只对非内置组生效）：
    /// 创组 → upsert 成员（含提示词）→ 切激活
    fn meta_core() -> Core {
        let dir = std::env::temp_dir().join(format!("exm-meta-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = ExmConfig::load(&dir);
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        let core = Core::with_config(cfg).expect("创建 Core 失败");
        {
            let reg = &core.orchestrator.read().registry;
            reg.create_group(Some("wg-test".into()), "测试组", "meta 测试用")
                .unwrap();
            reg.upsert_agent(
                "wg-test",
                crate::types::AgentDefinition {
                    identifier: "worker".into(),
                    name: "worker".into(),
                    domain: "通用".into(),
                    tier: crate::types::Tier::Unit,
                    description: "测试成员".into(),
                    capabilities: vec![],
                    tools: vec![],
                    when_to_call: String::new(),
                    dependencies: vec![],
                    composable_with: vec![],
                    input_schema: crate::types::AgentIoSchema::default(),
                    output_schema: crate::types::AgentIoSchema::default(),
                    prompt_file: String::new(),
                    model_hint: None,
                    link: None,
                    max_steps: None,
                },
                Some("# worker\n\n测试提示词。".into()),
            )
            .unwrap();
            reg.set_active_group("wg-test").unwrap();
        }
        core
    }

    fn meta_ap(
        _core: &Core,
        session: &str,
        kind: &str,
        target: &str,
        change: serde_json::Value,
        rationale: &str,
    ) -> crate::types::ApprovalRequest {
        let p = crate::tools::MetaProposal {
            kind: kind.into(),
            target: target.into(),
            change,
            rationale: rationale.into(),
        };
        crate::types::ApprovalRequest {
            id: crate::types::new_id()[..8].to_string(),
            session_id: session.into(),
            node_id: None,
            agent_id: "meta".into(),
            command: format!("[meta] {kind} {target}: {rationale}"),
            status: "pending".into(),
            result: None,
            created_at: crate::types::now_iso(),
            decided_at: None,
            origin_user: None,
            origin_role: None,
            meta_json: Some(serde_json::to_string(&p).unwrap()),
        }
    }

    /// adaptation 类：批准 → save_adaptation 版本化（rev 递增）
    #[tokio::test]
    async fn meta提议_adaptation_批准后版本化沉淀() {
        let core = meta_core();
        let s = core.create_session("meta-adapt").unwrap();
        let ap = meta_ap(
            &core,
            &s.id,
            "adaptation",
            "worker",
            serde_json::json!({ "content": "- 高失败时优先降步验证,一次只改一个变量" }),
            "orchestrator 连续 3 次 edit 失败,失败模式重复",
        );
        core.store.add_approval(&ap).unwrap();
        let decided = core.approval_decide(&ap.id, true).await.unwrap();
        assert_eq!(decided.status, "executed");
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// 拒绝 → denied 回流,不落任何改动
    #[tokio::test]
    async fn meta提议_拒绝回流() {
        let core = meta_core();
        let s = core.create_session("meta-deny").unwrap();
        let ap = meta_ap(
            &core,
            &s.id,
            "adaptation",
            "worker",
            serde_json::json!({ "content": "- 不应被写入的内容" }),
            "测试拒绝路径",
        );
        core.store.add_approval(&ap).unwrap();
        let decided = core.approval_decide(&ap.id, false).await.unwrap();
        assert_eq!(decided.status, "denied");
        assert!(core
            .orchestrator()
            .registry
            .load_adaptation("worker")
            .unwrap()
            .is_none());
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// prompt 类:批准 → 组内提示词追加锚点补段 + history 备份
    #[tokio::test]
    async fn meta提议_prompt_补段与备份() {
        let core = meta_core();
        let s = core.create_session("meta-prompt").unwrap();
        // 主智能体在激活组内(prompts/ 是组目录约定)
        let ap = meta_ap(
            &core,
            &s.id,
            "prompt",
            "worker",
            serde_json::json!({ "promptPatch": "编码任务先读相邻文件再动手,失败两次必须换路径" }),
            "工具统计显示 edit 失败集中于未读上下文",
        );
        core.store.add_approval(&ap).unwrap();
        let decided = core.approval_decide(&ap.id, true).await.unwrap();
        assert_eq!(decided.status, "executed");
        assert!(decided.result.as_deref().unwrap_or("").contains("锚点"));
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// 内置组保护:roster/build 类对内置组一律 bail(提议单应用被拒)
    #[tokio::test]
    async fn meta提议_内置组保护() {
        let core = meta_core();
        core.orchestrator.read()
            .registry
            .set_active_group("exmachina")
            .unwrap();
        let s = core.create_session("meta-builtin").unwrap();
        let ap = meta_ap(
            &core,
            &s.id,
            "roster",
            "orchestrator",
            serde_json::json!({ "ops": [{ "op": "remove", "identifier": "orchestrator" }] }),
            "测试内置组保护",
        );
        core.store.add_approval(&ap).unwrap();
        let decided = core.approval_decide(&ap.id, true).await.unwrap();
        assert_eq!(decided.status, "failed");
        assert!(decided
            .result
            .as_deref()
            .unwrap_or("")
            .contains("内置组"));
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// build 类:批准 → 注册新个体(组内可见)
    #[tokio::test]
    async fn meta提议_build_注册新个体() {
        let core = meta_core();
        let s = core.create_session("meta-build").unwrap();
        let ap = meta_ap(
            &core,
            &s.id,
            "build",
            "builder",
            serde_json::json!({
                "asGroup": false,
                "definition": { "name": "builder", "domain": "通用", "tier": "unit", "description": "构建出的新个体" },
                "prompt": "你是构建出的新个体,负责数据整理与汇报。"
            }),
            "数据整理任务无适配个体,重复阻塞 4 次",
        );
        core.store.add_approval(&ap).unwrap();
        let decided = core.approval_decide(&ap.id, true).await.unwrap();
        assert_eq!(decided.status, "executed");
        assert!(core
            .orchestrator()
            .registry
            .get("builder")
            .is_some(), "新个体应注册进激活组");
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }

    /// roster 类:批准 → 两阶段应用(add 后 update)
    #[tokio::test]
    async fn meta提议_roster_两阶段应用() {
        let core = meta_core();
        let s = core.create_session("meta-roster").unwrap();
        let ap = meta_ap(
            &core,
            &s.id,
            "roster",
            "worker",
            serde_json::json!({ "ops": [
                { "op": "add", "identifier": "scout", "name": "scout", "description": "roster 新增" },
                { "op": "update", "identifier": "worker", "name": "worker", "description": "职责更新:数据整理" }
            ]}),
            "能力缺口:侦察类任务无个体承接",
        );
        core.store.add_approval(&ap).unwrap();
        let decided = core.approval_decide(&ap.id, true).await.unwrap();
        assert_eq!(decided.status, "executed");
        let reg = &core.orchestrator().registry;
        assert!(reg.get("scout").is_some(), "scout 应被加入");
        assert!(reg
            .get("worker")
            .map(|d| d.description.contains("数据整理"))
            .unwrap_or(false), "worker 描述应被更新");
        let _ = std::fs::remove_dir_all(&core.config().workspace_root);
    }
}

/// 会话来源派生（session-management）：title 解析 + flow join 口径。
#[cfg(test)]
mod session_origin_tests {
    use super::*;
    use crate::types::ChannelFlow;

    fn core() -> Core {
        let dir = std::env::temp_dir().join(format!("exm-origin-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = ExmConfig::load(&dir);
        Core::with_config(cfg).expect("创建 Core 失败")
    }

    fn flow(id: &str, key: &str, name: &str) -> ChannelFlow {
        ChannelFlow {
            id: format!("{id}:{key}"),
            channel_id: id.into(),
            chat_key: key.into(),
            display_name: name.into(),
            is_group: true,
            active_session: None,
            last_message: String::new(),
            last_active_at: String::new(),
            created_at: String::new(),
        }
    }

    #[test]
    fn 列表补齐_通道会话带来源与流名() {
        let c = core();
        // 流存在（display_name 提供友好名）
        c.save_channel_flow(&flow("c-tg", "123", "技术群")).unwrap();
        c.create_session("channel:c-tg:123").unwrap();
        c.create_session("channel:c-tg:123:20251101").unwrap();
        // 流不在 flow 表（已删流）：回落 chat_key
        c.create_session("channel:c-qq:g9").unwrap();
        c.create_session("控制台随手记").unwrap();

        let list = c.list_sessions_admin().unwrap();
        let by_title: std::collections::HashMap<_, _> =
            list.iter().map(|s| (s.title.clone(), s.clone())).collect();

        let bare = &by_title["channel:c-tg:123"];
        assert_eq!(bare.origin.as_deref(), Some("channel:c-tg:123"));
        assert_eq!(bare.origin_label.as_deref(), Some("技术群"));

        let labeled = &by_title["channel:c-tg:123:20251101"];
        assert_eq!(labeled.origin.as_deref(), Some("channel:c-tg:123"));
        assert_eq!(labeled.origin_label.as_deref(), Some("技术群·20251101"));

        let orphan = &by_title["channel:c-qq:g9"];
        assert_eq!(orphan.origin_label.as_deref(), Some("g9"), "无流记录回落 chat_key");

        let console = &by_title["控制台随手记"];
        assert_eq!(console.origin.as_deref(), Some("console"));
        assert!(console.origin_label.is_none(), "控制台会话 label 缺省");
        let _ = std::fs::remove_dir_all(&c.config().workspace_root);
    }

    #[test]
    fn 归档列表_同样补齐() {
        let c = core();
        c.save_channel_flow(&flow("c-x", "k1", "测试流")).unwrap();
        let s = c.create_session("channel:c-x:k1").unwrap();
        c.set_session_archived(&s.id, true).unwrap();
        let archived = c.list_archived_sessions().unwrap();
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].origin.as_deref(), Some("channel:c-x:k1"));
        assert_eq!(archived[0].origin_label.as_deref(), Some("测试流"));
        let _ = std::fs::remove_dir_all(&c.config().workspace_root);
    }
}

/// 闲置归档与按流归档（session-management 生命周期治理）。
#[cfg(test)]
mod flow_archive_tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn core() -> Core {
        let dir = std::env::temp_dir().join(format!("exm-idle-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        Core::with_config(ExmConfig::load(&dir)).expect("创建 Core 失败")
    }

    fn at(days_ago: i64) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 4, 4, 0, 0).unwrap() - chrono::Duration::days(days_ago)
    }

    /// 把会话 updated_at 拨回 N 天前（直接改存储行）
    fn age(c: &Core, id: &str, days: i64) {
        let iso = (Utc.with_ymd_and_hms(2026, 11, 4, 4, 0, 0).unwrap()
            - chrono::Duration::days(days))
            .to_rfc3339();
        c.store.set_session_active_at(id, &iso).unwrap();
    }

    #[test]
    fn 闲置归档_阈值与豁免() {
        let c = core();
        let s_old = c.create_session("channel:c-tg:g1").unwrap();
        let s_fresh = c.create_session("channel:c-tg:g2").unwrap();
        let s_bound = c.create_session("channel:c-tg:g3").unwrap();
        let s_console = c.create_session("控制台会话").unwrap();
        c.bind_flow_session("c-tg", "g3", &s_bound.id).unwrap();
        age(&c, &s_old.id, 10);
        age(&c, &s_bound.id, 10); // 绑定会话同样老旧 → 仍须豁免
        age(&c, &s_console.id, 10);

        let now = at(0);
        let (archived, skipped) = c.archive_idle_flow_sessions(7, now).unwrap();
        assert_eq!(archived, 1, "只有闲置非绑定流会话归档");
        assert_eq!(skipped, 1, "绑定会话豁免计数");
        let archived_list = c.list_archived_sessions().unwrap();
        assert_eq!(archived_list.len(), 1);
        assert_eq!(archived_list[0].id, s_old.id);
        // 新鲜流会话与控制台会话不受影响
        assert!(c.store.get_session(&s_fresh.id).unwrap().unwrap().archived_at.is_none());
        assert!(c.store.get_session(&s_console.id).unwrap().unwrap().archived_at.is_none());
        let _ = std::fs::remove_dir_all(&c.config().workspace_root);
    }

    #[test]
    fn 闲置归档_零天关闭() {
        let c = core();
        let s = c.create_session("channel:c-x:g").unwrap();
        age(&c, &s.id, 365);
        let (archived, _) = c.archive_idle_flow_sessions(0, at(0)).unwrap();
        assert_eq!(archived, 0, "0 = 关闭零行为");
        assert!(c.store.get_session(&s.id).unwrap().unwrap().archived_at.is_none());
        let _ = std::fs::remove_dir_all(&c.config().workspace_root);
    }

    #[test]
    fn 按流归档_绑定豁免与范围圈定() {
        let c = core();
        let f1_active = c.create_session("channel:c-a:g1").unwrap();
        let f1_old = c.create_session("channel:c-a:g1:旧话题").unwrap();
        let f2 = c.create_session("channel:c-a:g2").unwrap();
        let other = c.create_session("channel:c-b:g1").unwrap();
        c.bind_flow_session("c-a", "g1", &f1_active.id).unwrap();

        let (archived, skipped) = c
            .archive_flow_sessions(crate::session_origin::ArchiveScope::Flow {
                channel_id: "c-a".into(),
                chat_key: "g1".into(),
            })
            .unwrap();
        assert_eq!((archived, skipped), (1, 1), "流内旧会话归档、绑定豁免");
        assert!(c.store.get_session(&f1_old.id).unwrap().unwrap().archived_at.is_some());
        assert!(c.store.get_session(&f1_active.id).unwrap().unwrap().archived_at.is_none());
        // 范围外不受影响
        assert!(c.store.get_session(&f2.id).unwrap().unwrap().archived_at.is_none());
        assert!(c.store.get_session(&other.id).unwrap().unwrap().archived_at.is_none());
        let _ = std::fs::remove_dir_all(&c.config().workspace_root);
    }

    #[test]
    fn 按通道归档_全流旧会话() {
        let c = core();
        let a1 = c.create_session("channel:c-a:g1:old1").unwrap();
        let a2 = c.create_session("channel:c-a:g2:old2").unwrap();
        let bound = c.create_session("channel:c-a:g2").unwrap();
        let b = c.create_session("channel:c-b:g1").unwrap();
        c.bind_flow_session("c-a", "g2", &bound.id).unwrap();

        let (archived, skipped) = c
            .archive_flow_sessions(crate::session_origin::ArchiveScope::Channel {
                channel_id: "c-a".into(),
            })
            .unwrap();
        assert_eq!((archived, skipped), (2, 1));
        assert!(c.store.get_session(&a1.id).unwrap().unwrap().archived_at.is_some());
        assert!(c.store.get_session(&a2.id).unwrap().unwrap().archived_at.is_some());
        assert!(c.store.get_session(&bound.id).unwrap().unwrap().archived_at.is_none());
        assert!(c.store.get_session(&b.id).unwrap().unwrap().archived_at.is_none());
        let _ = std::fs::remove_dir_all(&c.config().workspace_root);
    }
}

/// 一次性标记（once_flags）：预算内联提示去重等
#[cfg(test)]
mod once_flag_tests {
    use super::*;

    #[test]
    fn 一次性标记_首次置位后续拒绝() {
        let dir = std::env::temp_dir().join(format!("exm-once-{}", crate::types::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = Core::with_config(ExmConfig::load(&dir)).expect("创建 Core 失败");
        let key = "budget-notified:s1";
        assert!(c.consume_once_flag(key), "首次返回 true");
        assert!(!c.consume_once_flag(key), "后续返回 false");
        assert!(!c.consume_once_flag(key));
        assert!(c.consume_once_flag("budget-notified:s2"), "不同 key 互不影响");
        let _ = std::fs::remove_dir_all(&c.config().workspace_root);
    }
}
