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
pub mod inbox_stash;
pub mod mcp;
pub mod mcp_server;
pub mod memory;
pub mod meta;
pub mod nexus;

pub(crate) mod core;
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
pub mod task_queue;
pub mod task;
pub mod todo;
pub mod tools;
pub mod trajectory;
pub mod trash;
pub mod types;

use crate::bus::{InProcessBus, MessageBus};
use crate::config::ExmConfig;
use crate::cron::CronStore;
use crate::memory::MemoryStore;
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
use parking_lot::{Mutex, RwLock};
use std::sync::Arc;
use tokio::sync::broadcast;

/// 自主记忆摘要提示词（experiments.autoMemoryDigest，9.1）：单次非流式归纳。
/// 输出契约：第一行一句话主题，随后 ≤300 字要点（目标/关键结论/未决事项）。
const DIGEST_SYSTEM_PROMPT: &str = "你是会话记忆归纳器【自主记忆摘要】。把给定会话转录归纳为一条可长期检索的记忆：第一行一句话主题（≤40 字），随后不超过 300 字的要点（目标、关键结论、决策、未决事项）。只输出记忆正文，不要任何前后缀与解释。";

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

/// 核心句柄：所有渠道（CLI / Gateway / 未来桌面端）共用同一实例。
///
/// # 方法分布图（Core 领域拆分，codebase-refactor）
///
/// `impl Core` 按域分布在多个文件（同 crate 同 struct 多 impl 块），组合根留本文件：
/// - `core/sessions.rs`：会话域——create/list/search/delete/undo/edit_resend/fork/
///   stop/pin/archive/checkpoints/snapshots/tokens/usage/turn 快照/session_scope
/// - `core/approvals.rs`：审批域——approval_list/get/decide(_scoped)/grant_session/
///   session_grants/request_git_approval/approval_locks
/// - `core/memory.rs`：记忆域——memory_*（recall/remember/list/stats/pin/forget/
///   reindex/decay/digest）、render_memory_md/compact_memory_md、persona_*
/// - `core/identity.rs`：通道身份与流绑定——identity_*/identities/pairing_*/
///   issue_pairing/redeem/flow_*/channel_flow*/bind_flow/ensure_flow/touch_flow
/// - `core/skills.rs`：技能域——skills/add_skill/import_skill_md/export_skill_md/
///   record_skill_use/remove_skill
/// - `core/plugins.rs`：插件域——plugins()/plugin_install/update/uninstall
/// - `core/autonomy.rs`：实验体调度——auto_agent_tick/meta_agent_tick/
///   memory_digest_session/run_cron_job/run_event_prompt
/// - `lib.rs`（本文件）：组合根——struct 定义、create/with_config/apply_config、
///   chat/chat_scoped(_origin)（跨域粘合）、emit_*、build_orchestrator、
///   session_send_exec、attach_* 注入点、orchestrator()/config()/subscribe()
/// - 注册表转发（agents/groups/playbooks 等对 registry 的薄转发）留本文件
///   （一行转发的聚合视图，拆散反而破坏「Core 是唯一门面」契约）
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
    /// 会话级运行串行：同一会话的并发输入排队（followup），不互踩编排图
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
    /// 流级互斥（竞态 P1-3）：flow_id → 锁。同 chat_key 并发消息的
    /// 「查绑定→建会话→回写绑定」与「touch 预览」各自临界区——否则双建会话
    /// （孤儿+绑定互抹）或 touch 读改写覆盖他方 bind（上下文永久分裂）。
    flow_locks: Mutex<std::collections::HashMap<String, Arc<Mutex<()>>>>,
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


/// 跨会话投递执行器工厂：闭包持 Weak<Core>，避免 tools ↔ orchestrator ↔ Core 成环。
fn session_send_exec(weak: std::sync::Weak<Core>) -> crate::tools::SessionSendExec {
    std::sync::Arc::new(move |target, text, origin, wait| {
        let weak = weak.clone();
        Box::pin(async move {
            let Some(core) = weak.upgrade() else {
                anyhow::bail!("Core 已释放");
            };
            let run = async move {
                core.chat_scoped_origin(
                    &target,
                    &text,
                    crate::types::ToolScope::Tools,
                    Some(&origin),
                )
                .await
            };
            if wait {
                run.await.map(|_| "目标会话已完成本轮".to_string())
            } else {
                tokio::spawn(run);
                Ok("已投递（目标会话异步执行）".to_string())
            }
        })
    })
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
            flow_locks: parking_lot::Mutex::new(std::collections::HashMap::new()),
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

    /// 装配跨会话投递执行器（sessions 工具 op=send）：必须经 Arc<Core> 调用。
    /// 闭包持 Weak 引用（tools 由 orchestrator 持有，强引用即环）；orchestrator
    /// 重建路径由 rebind_plugin_tools 尾部统一重挂。
    pub fn attach_session_send_executor(self: &std::sync::Arc<Self>) {
        let weak = std::sync::Arc::downgrade(self);
        *self.self_weak.write() = Some(weak.clone());
        let orch = self.orchestrator.read().clone();
        orch.tools.set_session_send_executor(session_send_exec(weak));
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


    // ---------------- 插件安装/更新（plugin-ecosystem；CLI 与 REST 共用面） ----------------




    // ---------------- 记忆系统便捷方法（组感知，docs/09 §6） ----------------

















    // ---------------- 人设（说话风格） ----------------










    // ---------------- 文件检查点（写前快照的回看与回滚） ----------------



    /// 工作区文件保存（WebUI 编辑器写入口径）：路径防 `..` 兜底、写前检查点、工具审计。
    /// 与写类工具同规则（当日首次改动前的版本落 `.exmachina/checkpoints/`，供回滚）。
    /// 返回结果摘要；越界/非法路径报错。网关侧已做工作区规范化校验，此处兜底拒绝。
    /// （当前无调用点——预留接口；pub(crate) 化待接入方出现再升 pub，codebase-refactor D2）
    #[allow(dead_code)]
    pub(crate) fn save_workspace_file(&self, rel: &str, content: &str) -> anyhow::Result<String> {
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
        self.store.audit_tool_best_effort(
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








    // ---------------- 聊天流（chat flow）：外部通道的对话单元与会话绑定 ----------------


    /// 流级锁的 Arc（竞态 P1-3）：同 flow 的「查绑定→建会话→回写绑定」/
    /// 「touch 预览」临界区互斥。锁粒度 = flow_id（不同流互不影响）；
    /// parking_lot::Mutex（锁迁移 5.18：持锁 panic 不中毒）——临界区内全是同步
    /// fs 操作，持锁不跨 await。
    /// 用法：`let lock = self.flow_lock_arc(ch, key); let _g = lock.lock();`
    /// （先 clone Arc 再 lock，guard 借局部 Arc，安全）
    fn flow_lock_arc(&self, channel_id: &str, chat_key: &str) -> Arc<Mutex<()>> {
        let id = Self::flow_id(channel_id, chat_key);
        let mut locks = self.flow_locks.lock();
        locks.entry(id).or_insert_with(|| Arc::new(Mutex::new(()))).clone()
    }




















    /// 供网关在执行前拒绝时发运行错误事件（配额 / 治理类短路，走既有回帖通路）
    pub fn emit_run_error(&self, session_id: &str, message: &str) {
        let _ = self.events.send(crate::types::CoreEvent {
            kind: "run.error".into(),
            session_id: session_id.to_string(),
            payload: serde_json::json!({ "message": message }),
        });
    }

    /// 通用会话事件广播 + 落事件库（task_queue.updated 等运行时状态面；
    /// 与 ToolGateway::emit_and_persist 同口径——广播供 WS 实时、落库供事件页/刷新回放）
    pub fn emit_session_event(&self, session_id: &str, kind: &str, payload: serde_json::Value) {
        let evt = crate::types::CoreEvent {
            kind: kind.to_string(),
            session_id: session_id.to_string(),
            payload,
        };
        let _ = self.events.send(evt.clone());
        let env = crate::types::Envelope::new(
            format!("session.{session_id}.events"),
            "core",
            "broadcast",
            "event",
            session_id.to_string(),
            serde_json::to_value(&evt).unwrap_or(serde_json::Value::Null),
        );
        let _ = self.store.append_event(&env);
    }

    /// 入站消毒审计事件（inbound-sanitization）：广播 + 落事件库（事件页可查）。
    /// payload 只记模式摘要（通道/账号/模式/处置），MUST NOT 含原文全文。
    pub fn emit_sanitize_finding(&self, payload: serde_json::Value) {
        self.orchestrator.read().emit("", "sanitize.finding", payload);
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
        let ctx = crate::core::sessions::parse_session_scope(&self.registry, &session.group_id);
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
        // 跨会话投递执行器重挂（sessions 工具）：与授权探针同生命周期。
        if let Some(weak) = self.self_weak.read().clone() {
            orch.tools.set_session_send_executor(session_send_exec(weak));
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

    /// 组信息更新（group-card-menu）：改名 / 改 id 一步完成。id 变更时迁移全部
    /// 外部引用（会话归属、记忆条目、定时任务、事件触发器），激活组拒绝改 id。
    pub fn update_group_meta(
        &self,
        gid: &str,
        name: &str,
        new_id: Option<&str>,
    ) -> anyhow::Result<GroupMeta> {
        // 改 id 前置守卫：激活组不可改（active_group 落盘引用与运行中轮次按 id 寻址）
        if let Some(nid) = new_id {
            let nid = nid.trim();
            if nid != gid && self.registry.active_group() == gid {
                anyhow::bail!("激活组不可改 id：请先切换到其他组再操作");
            }
        }
        // 1) 名称（registry 落盘 group.json）
        let meta = self.registry.rename_group(gid, name)?;
        // 2) id（目录迁移 + 内存键迁移），随后迁移外部引用
        match new_id.map(str::trim).filter(|s| !s.is_empty() && *s != gid) {
            Some(nid) => {
                let meta = self.registry.change_group_id(gid, nid)?;
                self.rename_group_refs(gid, nid)?;
                Ok(meta)
            }
            None => Ok(meta),
        }
    }

    /// 组 id 迁移的外部引用清扫（change_group_id 成功后调用）：
    /// 会话归属（sessions/group_id）、记忆条目（memory/entries/group_id）、
    /// 定时任务（cron jobs group）、事件触发器（event_triggers group）。
    /// 全部为文档存储的纯字段改写：逐条读-改-写，失败即中断（调用方已先落盘组目录，
    /// 此处失败留下悬空引用——量级小、重跑同 id 迁移可自愈，不做分布式事务）。
    fn rename_group_refs(&self, old_id: &str, new_id: &str) -> anyhow::Result<()> {
        // 会话（含归档）
        for sid in [
            self.store.list_sessions()?,
            self.store.list_archived_sessions()?,
        ]
        .concat()
        {
            if sid.group_id == old_id {
                let mut s = sid;
                s.group_id = new_id.to_string();
                self.store.update_session_group(&s.id, new_id)?;
            }
        }
        // 记忆条目（组维度归属）
        let entries = self.memory.entries_public()?;
        for e in entries {
            if e.group_id.as_deref() == Some(old_id) {
                self.memory.retag_group(&e.id, new_id)?;
            }
        }
        // 定时任务
        let mut jobs = self.cron.list()?;
        let mut dirty = false;
        for j in jobs.iter_mut() {
            if j.group.as_deref() == Some(old_id) {
                j.group = Some(new_id.to_string());
                dirty = true;
            }
        }
        if dirty {
            self.cron.save(&jobs)?;
        }
        // 事件触发器
        for t in self.store.list_event_triggers(None)? {
            if t.group.as_deref() == Some(old_id) {
                let mut t2 = t.clone();
                t2.group = Some(new_id.to_string());
                self.store.put_event_trigger(&t2)?;
            }
        }
        Ok(())
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
                    // P2-7：审批了结落库失败必须可见——静默会造内存态/落库态分叉
                    if let Err(e) = self.store.update_approval(&ap) {
                        eprintln!("[approval] 计划拒绝落库失败（{}）：{e}", ap.id);
                    }
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
                    // P2-7：同上——落库失败留日志防状态分叉
                    if let Err(e) = self.store.update_approval(&ap) {
                        eprintln!("[approval] 计划执行落库失败（{}）：{e}", ap.id);
                    }
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
                // 抢占校验（竞态 P1-2）：切换后立即回读——被并发通道抢走则抢回一次
                if switched && self.registry.active_group() != *g {
                    let _ = self.registry.set_active_group(g);
                }
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
                // 抢占校验（竞态 P1-2）：切换后立即回读——被并发通道抢走则抢回一次
                if switched && self.registry.active_group() != *g {
                    let _ = self.registry.set_active_group(g);
                }
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
            Ok(_) => "任务执行完成（详见会话与编排图）".to_string(),
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
                orch.emit(
                    "",
                    "auto.eval",
                    serde_json::json!({ "ok": false, "error": e.to_string() }),
                );
                return None;
            }
        };
        let Some((task, why)) = crate::autonomy::parse_auto_eval(&output) else {
            // 无任务：空转计数 +1（退避拉长），发审计事件（广播 + 落库，事件页可查）
            self.auto_state.write().idle_streak += 1;
            orch.emit("", "auto.eval", serde_json::json!({ "ok": true, "act": false }));
            return None;
        };
        // Eval 通过：空传统计复位，开自主行动会话执行
        self.auto_state.write().idle_streak = 0;
        orch.emit(
            "",
            "auto.eval",
            serde_json::json!({ "ok": true, "act": true, "task": task, "why": why }),
        );
        let title = format!("auto-agent:{}", task.chars().take(30).collect::<String>());
        let session = self.create_session(&title).ok()?;
        let prompt = format!(
            "{}\n\n## 任务输入\n目标：{task}\n选定理由：{why}\n步数预算：{}",
            crate::autonomy::AUTO_ACT_DISCIPLINE,
            crate::autonomy::auto_act_max_steps(&cfg),
        );
        let result = self.chat(&session.id, &prompt).await;
        orch.emit(
            &session.id,
            "auto.act",
            serde_json::json!({
                "task": task,
                "status": if result.is_ok() { "done" } else { "failed" },
                "error": result.err().map(|e| e.to_string()),
            }),
        );
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
    // 启动期推理治理（anti-runaway-thinking 部署固化）：进程级 effort / token cap
    // 经环境变量 EXM_REASONING_EFFORT / EXM_REASONING_TOKEN_CAP 注入——否则容器
    // 重启即回端点默认，「响应太慢」随每次部署复发（effort 是进程内存态）。
    // 运行时仍可经 /api/llm/effort 覆盖（进程内存优先于启动环境）。
    if let Ok(v) = std::env::var("EXM_REASONING_EFFORT") {
        if !v.trim().is_empty() {
            crate::provider::set_reasoning_effort(Some(v.trim()));
        }
    }
    if let Ok(v) = std::env::var("EXM_REASONING_TOKEN_CAP") {
        if let Ok(n) = v.trim().parse::<u32>() {
            crate::provider::set_reasoning_token_cap(Some(n));
        }
    }
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
        queue_draining: parking_lot::Mutex::new(std::collections::HashSet::new()),
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

// 原 lib.rs 内联测试整体迁出（纯搬移）→ core/tests.rs（Core 领域拆分第一批前置）；
// 拆分后经 core/mod.rs 挂载（tests 在 core/ 内部声明），此处不再重复挂载