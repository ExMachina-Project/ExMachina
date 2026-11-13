//! tools/gateway.rs —— 自 tools.rs 按领域拆出（纯搬移，零行为变更）

use super::*;

/// 子智能体派生执行器闭包：(主个体 id, 会话 id, 任务描述, 工具白名单) → 执行产出
pub type SpawnExec = Arc<
    dyn Fn(
            &str,
            &str,
            &str,
            Vec<ToolName>,
        ) -> futures_util::future::BoxFuture<'static, anyhow::Result<String>>
        + Send
        + Sync,
>;

/// 插件凭据解析闭包：(插件 id, 键) → 真值（None = 未配置）
pub type PluginCredResolver = Box<dyn Fn(&str, &str) -> Option<String> + Send + Sync>;

/// 跨会话投递执行器：(目标会话 id, 文本, 来源标记, 是否等待本轮完成) → 结果说明。
/// Core 装配后回接（闭包持 Weak 引用，避免与 orchestrator 成环）。
pub type SessionSendExec = Arc<
    dyn Fn(
            String,
            String,
            String,
            bool,
        ) -> futures_util::future::BoxFuture<'static, anyhow::Result<String>>
        + Send
        + Sync,
>;

/// 会话授权查询闭包（approval-ergonomics）：(会话 id, 命令全文) → 是否命中会话授权
pub type SessionGrantProbe =
    std::sync::Arc<dyn Fn(&str, &str) -> bool + Send + Sync>;

/// 语音合成执行器（speech_reply 工具）：文本 → mp3 字节。
/// Core 装配后回接 orchestrator speak_audio（能力槽位链）。
pub type SpeechSynth = Arc<
    dyn Fn(String) -> futures_util::future::BoxFuture<'static, anyhow::Result<Vec<u8>>>
        + Send
        + Sync,
>;

/// 图像生成执行器（image_gen 工具）：(prompt, size) → 图片字节。
/// Core 装配后回接 orchestrator image_gen_bytes（能力槽位链）。
pub type ImageGen = Arc<
    dyn Fn(String, String) -> futures_util::future::BoxFuture<'static, anyhow::Result<Vec<u8>>>
        + Send
        + Sync,
>;

/// 会话工具调用计数器（agent-budget-audit 三值预算）
#[derive(Debug, Default, Clone)]
pub struct ToolCallCounter {
    /// 本轮调用数（chat_execute 进入时清零）
    pub turn: usize,
    /// 会话累计（进程生命周期）
    pub session_total: usize,
    /// 时间窗内的调用时刻戳（毫秒；滑动窗口裁剪）
    pub window_stamps: std::collections::VecDeque<u64>,
}

impl ToolCallCounter {
    /// 裁剪时间窗外旧戳并返回窗内计数
    fn window_count(&mut self, now_ms: u64, window_secs: u64) -> usize {
        if window_secs == 0 {
            return 0;
        }
        let horizon = now_ms.saturating_sub(window_secs * 1000);
        while let Some(&t) = self.window_stamps.front() {
            if t < horizon {
                self.window_stamps.pop_front();
            } else {
                break;
            }
        }
        self.window_stamps.len()
    }
}

/// capability 档位预算表（agent-budget-audit）：chat < tools < computer。
/// config 的 ToolCallBudget 是 computer 档全量值；低档位按比例收严：
/// chat 取 1/4（对话优先,工具只做轻量补充）、tools 取 1/2。
/// 0 值单项在低档位同样收严为 0 内保留（只在全量档关闭才关闭）。
pub fn scope_budget(full: &crate::config::ToolCallBudget, scope: crate::types::ToolScope) -> crate::config::ToolCallBudget {
    let (t, s, w) = match scope {
        crate::types::ToolScope::Chat => (full.per_turn / 4, full.per_session / 4, full.per_window / 4),
        crate::types::ToolScope::Tools => (full.per_turn / 2, full.per_session / 2, full.per_window / 2),
        crate::types::ToolScope::Computer => (full.per_turn, full.per_session, full.per_window),
    };
    crate::config::ToolCallBudget {
        per_turn: t,
        per_session: s,
        per_window: w,
        window_secs: full.window_secs,
    }
}

pub struct ToolGateway {
    pub(crate) workspace_root: PathBuf,
    pub(crate) store: Arc<Store>,
    pub(crate) registry: Arc<LocalRegistry>,
    pub(crate) events: tokio::sync::broadcast::Sender<CoreEvent>,
    pub(crate) security: SecurityConfig,
    /// 联网搜索后端（未配置 = web_search 不下发，避免「承诺不存在的能力」）
    pub(crate) search: SearchConfig,
    /// 沙箱执行策略（环境净化 / bwrap / strict 闸门）
    pub(crate) sandbox: SandboxConfig,
    /// 生命周期钩子（pre/post tool、run 收束）
    pub(crate) hooks: crate::config::HooksConfig,
    /// 定时任务存储（schedule 工具：AI 自主排程）
    pub(crate) cron: Option<Arc<crate::cron::CronStore>>,
    /// cron 调度时区（automation.timezone；空 = UTC）
    pub(crate) cron_timezone: String,
    /// 子智能体派生执行器（spawn 工具；未注入 = spawn 返回未装配错误）。
    /// RwLock 槽：Core 装配后回接（tools 已 Arc 共享给 runtime，无法再拿可变引用）
    pub(crate) spawn_executor: parking_lot::RwLock<Option<SpawnExec>>,
    /// 跨会话投递执行器（sessions 工具 op=send；未注入 = 返回未装配说明）
    pub(crate) session_send_executor: parking_lot::RwLock<Option<SessionSendExec>>,
    /// 会话授权查询（approval-ergonomics）：Core 装配后回接——gate_command
    /// 落审批单前先查会话授权表，命中直通。槽位语义同 spawn_executor。
    pub(crate) session_grant_probe: parking_lot::RwLock<Option<SessionGrantProbe>>,
    /// 上下文压缩执行器（context_compact 工具，实验性；未注入 = 工具返回未装配说明）。
    /// 槽位语义同 spawn_executor：Core 装配后回接 (provider, model)。
    pub(crate) compactor: parking_lot::RwLock<Option<(Arc<dyn LlmProvider>, String)>>,
    /// 语音合成执行器（speech_reply 工具，assistant-parity T2）：文本 → mp3 字节。
    /// 槽位语义同 spawn_executor：Core 装配后回接 orchestrator speak_audio
    /// （能力槽位链：组覆盖 → 全局 speech 模型 → tts-1 兜底）。
    pub(crate) speech_synth: parking_lot::RwLock<Option<SpeechSynth>>,
    /// 图像生成执行器（image_gen 工具；未装配时工具不下发、不执行）
    pub(crate) image_gen: parking_lot::RwLock<Option<ImageGen>>,
    /// spawn 并发闸（实例级信号量；ToolGateway 装配时按上限创建）
    pub(crate) spawn_semaphore: Arc<tokio::sync::Semaphore>,
    /// spawn 子任务超时（秒；默认 300）
    pub(crate) spawn_timeout_secs: u64,
    /// 后台任务注册表（长命令：启动后返回 id，可轮询/终止）
    pub(crate) background: Arc<parking_lot::Mutex<std::collections::HashMap<String, BgTask>>>,
    /// 后台 spawn 子任务注册表（spec: subagent-spawn 异步并行）：
    /// background=true 派生后立即返回，终态暂存供 op=status/list 回收
    pub(crate) spawns_bg: Arc<parking_lot::Mutex<std::collections::HashMap<String, SpawnBgTask>>>,
    /// 浏览器配置与会话（懒启动常驻；串行化访问）
    pub(crate) browser_cfg: crate::config::BrowserConfig,
    pub(crate) browser: Arc<tokio::sync::Mutex<Option<crate::browser::BrowserSession>>>,
    /// Computer Use 配置与机械执行器（懒初始化；串行化访问）
    pub(crate) computer_cfg: crate::config::ComputerUseConfig,
    pub(crate) computer: Arc<tokio::sync::Mutex<Option<crate::computer::ComputerSession>>>,
    /// MCP 服务器池（第三方工具：mcp:server:tool 全名空间）
    pub(crate) mcp: Arc<crate::mcp::McpRegistry>,
    /// 声明式自定义工具（配置驱动，免写 MCP 服务器；按 agents 可见性放行）
    pub(crate) custom: Vec<crate::config::CustomTool>,
    /// 插件凭据解析面（T1 声明式工具 `${credentials.<key>}` 变量；Core 装配后回接——
    /// ToolGateway 构建期 config 尚未定稿，运行期经此闭包查 `plugins.credentials.<id>`）
    pub(crate) plugin_credentials: parking_lot::RwLock<Option<PluginCredResolver>>,
    /// 插件声明式工具（T1 清单内联 HTTP 工具，命名空间 `<plugin>__<tool>`；
    /// 与 custom 共用执行链——run_custom 全套（http/shell、审计、钩子、落盘））
    pub(crate) plugin_tools: parking_lot::RwLock<Vec<crate::config::CustomTool>>,
    /// 进程插件运行时工具面（T2：握手后发现，无法静态声明）。
    /// key = 插件 id；specs/执行转发到 provider（supervisor 持进程句柄）。
    /// 注册时机：spawn_process_plugin 拉起成功后（Core 回接）。
    pub(crate) plugin_runtime_tools: parking_lot::RwLock<
        std::collections::BTreeMap<String, std::sync::Arc<dyn crate::plugin::ToolProvider>>,
    >,
    /// 动态工具执行器（T3 内置插件用：命名空间工具注册 schema + 自定义 async 执行面）。
    /// key = 工具全名；执行优先级：动态表 > plugin_tools/custom（可覆盖声明式同名）。
    pub(crate) dynamic_tools: parking_lot::RwLock<
        std::collections::BTreeMap<String, (crate::config::CustomTool, DynamicToolExec)>,
    >,
    /// 会话级工具注入（视频观看会话等「按会话来源收窄」的插件工具）：
    /// key = session_id；schema 下发与执行都只对本会话生效，普通会话不可见。
    pub(crate) session_scoped_tools: parking_lot::RwLock<
        std::collections::HashMap<String, Vec<(crate::config::CustomTool, DynamicToolExec)>>,
    >,
    /// 会话级步数预算覆盖（自动任务「防失控」面；None = 走三级默认解析）
    pub(crate) session_step_budgets: parking_lot::RwLock<std::collections::HashMap<String, usize>>,
    /// 工具调用预算计数（agent-budget-audit）：会话 → 计数器。
    /// 三值口径：单轮（chat_execute 进入时清零）/ 会话累计 / 时间窗（滑动）。
    /// 内存态：进程重启清零（防失控是运行时关注,不是持久治理）。
    pub(crate) tool_call_counters:
        parking_lot::Mutex<std::collections::HashMap<String, ToolCallCounter>>,
    /// 会话作用域快照（agent-budget-audit 预算档位判定）：通道会话由
    /// ChannelRun::begin 写入 capability 档位;缺省 Computer（控制台全量）。
    pub(crate) session_scopes:
        parking_lot::RwLock<std::collections::HashMap<String, crate::types::ToolScope>>,
    /// 记忆库（memory_read/write/link 三工具的受控入口；组隔离在工具层强制）
    pub(crate) memory: Option<Arc<crate::memory::MemoryStore>>,
    /// 知识库（knowledge_write/read 两工具的受控入口）。与记忆不同：全局共享
    /// 不分区，恒下发（不受记忆模式/分区排除影响）；None = 未装配，两工具
    /// 执行返回明确错误（Core 装配后必有值）
    pub(crate) knowledge: Option<Arc<crate::knowledge::KnowledgeStore>>,
    /// 深层记忆开关（config.memory.enabled）。false = 文件记忆模式：
    /// memory_read 改读 memory.md（与规划注入同源），memory_write/link 拒绝并引导。
    /// 为什么写工具要拒绝而非照写数据库：文件记忆模式下规划面只注入 memory.md，
    /// 写进数据库的知识永远不会被召回（写入黑洞）——静默成功比失败更伤。
    pub(crate) memory_deep: bool,
    /// memory.md 路径（文件记忆模式的读取源；与 orchestrator 注入用同一路径与同一 8000 字符上限）
    pub(crate) memory_md_path: Option<PathBuf>,
}

/// computer 输入动作：在既有会话上执行一次鼠标/键盘操作
pub(crate) type InputAction =
    Box<dyn FnOnce(&mut crate::computer::ComputerSession) -> anyhow::Result<()> + Send>;
impl ToolGateway {
    pub fn new(
        workspace_root: impl AsRef<Path>,
        store: Arc<Store>,
        registry: Arc<LocalRegistry>,
        events: tokio::sync::broadcast::Sender<CoreEvent>,
        security: SecurityConfig,
    ) -> Self {
        ToolGateway {
            workspace_root: workspace_root.as_ref().to_path_buf(),
            store,
            registry,
            events,
            security,
            search: SearchConfig::default(),
            sandbox: SandboxConfig::default(),
            hooks: crate::config::HooksConfig::default(),
            cron: None,
            cron_timezone: String::new(),
            spawn_executor: parking_lot::RwLock::new(None),
            session_send_executor: parking_lot::RwLock::new(None),
            session_grant_probe: parking_lot::RwLock::new(None),
            compactor: parking_lot::RwLock::new(None),
            speech_synth: parking_lot::RwLock::new(None),
            image_gen: parking_lot::RwLock::new(None),
            spawn_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
            spawn_timeout_secs: 300,
            background: Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new())),
            spawns_bg: Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new())),
            browser_cfg: crate::config::BrowserConfig::default(),
            browser: Arc::new(tokio::sync::Mutex::new(None)),
            computer_cfg: crate::config::ComputerUseConfig::default(),
            computer: Arc::new(tokio::sync::Mutex::new(None)),
            mcp: crate::mcp::McpRegistry::shared(),
            custom: Vec::new(),
            plugin_credentials: parking_lot::RwLock::new(None),
            plugin_tools: parking_lot::RwLock::new(Vec::new()),
            plugin_runtime_tools: parking_lot::RwLock::new(std::collections::BTreeMap::new()),
            dynamic_tools: parking_lot::RwLock::new(std::collections::BTreeMap::new()),
            session_scoped_tools: parking_lot::RwLock::new(std::collections::HashMap::new()),
            session_step_budgets: parking_lot::RwLock::new(std::collections::HashMap::new()),
            tool_call_counters: parking_lot::Mutex::new(std::collections::HashMap::new()),
            session_scopes: parking_lot::RwLock::new(std::collections::HashMap::new()),
            memory: None,
            knowledge: None,
            // 默认深层记忆开启 = 历史行为（未接线 memory 模式的调用方零感知）
            memory_deep: true,
            memory_md_path: None,
        }
    }

    /// 注入记忆库（memory_read/write/link 三工具；未注入时三工具执行返回明确错误）
    pub fn with_memory(mut self, memory: Arc<crate::memory::MemoryStore>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// 注入知识库（knowledge_write/read 两工具；未注入时两工具执行返回明确错误）。
    /// 知识全局共享不分区——不需要记忆那类模式开关，装配即全量语义。
    pub fn with_knowledge(mut self, knowledge: Arc<crate::knowledge::KnowledgeStore>) -> Self {
        self.knowledge = Some(knowledge);
        self
    }

    /// 注入子智能体派生执行器（spawn 工具）：Core 装配后回接 AgentRuntime。
    /// 闭包签名：(主个体 id, 会话 id, 任务描述, 工具白名单) → 执行产出（结论文本）。
    /// 并发/超时控制由网关层实现（spec: subagent-spawn）。
    pub fn set_spawn_executor(&self, exec: SpawnExec) {
        *self.spawn_executor.write() = Some(exec);
    }

    /// 注入跨会话投递执行器（sessions 工具 op=send）：Core 装配后回接。
    pub fn set_session_send_executor(&self, exec: SessionSendExec) {
        *self.session_send_executor.write() = Some(exec);
    }

    /// 注入会话授权查询（approval-ergonomics）：Core 装配后回接。
    /// 闭包签名：(会话 id, 命令全文) → 是否命中会话授权（前缀口径见 command_head）。
    pub fn set_session_grant_probe(&self, f: SessionGrantProbe) {
        *self.session_grant_probe.write() = Some(f);
    }

    /// 注入上下文压缩执行器（context_compact 工具，实验性开关开启时由 Core 装配后回接）
    pub fn set_compactor(&self, provider: Arc<dyn LlmProvider>, model: String) {
        *self.compactor.write() = Some((provider, model));
    }

    /// 注入语音合成执行器（speech_reply 工具）：Core 装配后回接。
    pub fn set_speech_synth(&self, synth: SpeechSynth) {
        *self.speech_synth.write() = Some(synth);
    }

    /// 注入图像生成执行器（image_gen 工具）：Core 装配后回接。
    pub fn set_image_gen(&self, gen: ImageGen) {
        *self.image_gen.write() = Some(gen);
    }

    /// 图像生成是否装配（决定 image_gen 是否下发：能力未回接不承诺）
    pub fn image_gen_ready(&self) -> bool {
        self.image_gen.read().is_some()
    }

    /// 注入记忆模式（config.memory.enabled + memory_md_path）。
    /// `deep=false` 进入文件记忆模式：三工具口径与规划注入对齐（read 读 md、write/link 拒绝并引导）。
    pub fn with_memory_mode(mut self, deep: bool, md_path: PathBuf) -> Self {
        self.memory_deep = deep;
        self.memory_md_path = Some(md_path);
        self
    }

    /// 深层记忆是否开启（工具 schema 下发口径用）
    pub fn memory_deep(&self) -> bool {
        self.memory_deep
    }

    /// 注入联网搜索后端配置（未配置时 web_search 不下发给模型）
    pub fn with_search(mut self, search: SearchConfig) -> Self {
        self.search = search;
        self
    }

    /// 注入声明式自定义工具（配置驱动）
    pub fn with_custom_tools(mut self, tools: Vec<crate::config::CustomTool>) -> Self {
        self.custom = tools;
        self
    }

    /// 追加插件声明式工具（T1 零代码形态：构建期注入用；Core 回接走 append_plugin_tools）
    pub fn with_plugin_tools(self, tools: Vec<crate::config::CustomTool>) -> Self {
        self.append_plugin_tools(tools);
        self
    }

    /// 就地追加插件工具（Core 装配后回接用：ToolGateway 已包在 Arc 里不可再构建）。
    /// 重复调用幂等（同名先注册者胜，custom 与本表都算已注册）。
    pub fn append_plugin_tools(&self, tools: Vec<crate::config::CustomTool>) {
        let mut guard = self.plugin_tools.write();
        for t in tools {
            if guard.iter().any(|c| c.name == t.name) {
                continue; // 先注册者胜
            }
            if self.custom.iter().any(|c| c.name == t.name) {
                continue; // 与 config CustomTool 冲突：config 优先
            }
            guard.push(t);
        }
    }

    /// 注册进程插件运行时工具面（T2：拉起握手成功后由 spawn_process_plugin 回接）。
    /// 幂等：同 id 重复注册覆盖（重启场景新 supervisor 顶旧）。
    pub fn register_plugin_runtime_tools(
        &self,
        plugin_id: &str,
        provider: std::sync::Arc<dyn crate::plugin::ToolProvider>,
    ) {
        self.plugin_runtime_tools
            .write()
            .insert(plugin_id.to_string(), provider);
    }

    /// 摘除进程插件运行时工具面（禁用/卸载用）
    pub fn unregister_plugin_runtime_tools(&self, plugin_id: &str) {
        self.plugin_runtime_tools.write().remove(plugin_id);
    }

    /// 当前注册的全部插件工具（诊断/管理端点用）
    pub fn plugin_tools(&self) -> Vec<crate::config::CustomTool> {
        self.plugin_tools.read().clone()
    }

    /// 注入插件凭据解析面（Core 装配后回接：查 `plugins.credentials.<插件id>.<键>`）
    pub fn set_plugin_credentials_resolver(&self, f: PluginCredResolver) {
        *self.plugin_credentials.write() = Some(f);
    }

    /// 解析文本中的 `${credentials.<key>}` 变量（插件工具执行期）。
    /// 返回 (解析后文本, 缺失键列表)——缺失时不发部分鉴权请求，由调用方拒绝。
    pub(crate) fn resolve_credential_vars(&self, plugin_id: &str, text: &str) -> (String, Vec<String>) {
        let mut missing = Vec::new();
        // 形如 ${credentials.api_token}（键名：字母/数字/_/-）
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(i) = rest.find("${credentials.") {
            out.push_str(&rest[..i]);
            let after = &rest[i + "${credentials.".len()..];
            match after.find('}') {
                Some(j) => {
                    let key = &after[..j];
                    if key.is_empty()
                        || !key
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                    {
                        // 非法键名：原样保留（不视为变量）
                        out.push_str("${credentials.");
                        rest = after;
                        continue;
                    }
                    let resolver = self.plugin_credentials.read();
                    let hit = resolver.as_ref().and_then(|f| f(plugin_id, key));
                    match hit {
                        Some(v) => out.push_str(&v),
                        None => {
                            // 缺键：保留原文（调用方按 missing 拒绝；纯函数面可测）
                            missing.push(key.to_string());
                            out.push_str(&rest[i..i + "${credentials.".len() + key.len() + 1]);
                        }
                    }
                    rest = &after[j + 1..];
                }
                None => {
                    out.push_str("${credentials.");
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        (out, missing)
    }

    /// 注册动态命名空间工具（T3 内置插件：schema + 自定义 async 执行面）。
    /// 名字须含 `<plugin>__<tool>` 命名空间；同名重复注册 = 覆盖（热更新用）。
    pub fn register_dynamic_tool(&self, spec: crate::config::CustomTool, exec: DynamicToolExec) {
        self.dynamic_tools
            .write()
            .insert(spec.name.clone(), (spec, exec));
    }

    /// 注销动态命名空间工具（插件停用/通道下线回收工具面）
    pub fn unregister_dynamic_tool(&self, name: &str) {
        self.dynamic_tools.write().remove(name);
    }

    /// 按名查动态工具（执行面用）
    pub fn dynamic_tool(&self, name: &str) -> Option<(crate::config::CustomTool, DynamicToolExec)> {
        self.dynamic_tools.read().get(name).cloned()
    }

    /// 当前注册的动态工具 schema 清单（诊断用）
    pub fn dynamic_tool_names(&self) -> Vec<String> {
        self.dynamic_tools.read().keys().cloned().collect()
    }

    /// 会话级工具注入：把一组工具绑定到指定会话（视频观看会话等「按会话来源
    /// 收窄」的场景）。同名重复注入 = 覆盖；轮次结束须调用 clear_session_tools 清理。
    pub fn inject_session_tools(
        &self,
        session_id: &str,
        tools: Vec<(crate::config::CustomTool, DynamicToolExec)>,
    ) {
        self.session_scoped_tools
            .write()
            .insert(session_id.to_string(), tools);
    }

    /// 清理会话级注入（观看轮次收束时调用；防泄漏）
    pub fn clear_session_tools(&self, session_id: &str) {
        self.session_scoped_tools.write().remove(session_id);
    }

    /// 按会话查注入工具（执行面用）
    pub(crate) fn session_tool(
        &self,
        session_id: &str,
        name: &str,
    ) -> Option<(crate::config::CustomTool, DynamicToolExec)> {
        self.session_scoped_tools
            .read()
            .get(session_id)
            .and_then(|v| v.iter().find(|(c, _)| c.name == name).cloned())
    }

    /// 会话级注入工具的 schema（runtime 下发用：session_id 变体）
    pub fn session_tool_specs(&self, session_id: &str) -> Vec<crate::provider::ToolSpec> {
        let to_spec = |c: &crate::config::CustomTool| crate::provider::ToolSpec {
            name: c.name.trim().to_string(),
            description: if c.description.trim().is_empty() {
                format!("插件会话工具 {}", c.name)
            } else {
                c.description.clone()
            },
            parameters: c.param_schema(),
        };
        self.session_scoped_tools
            .read()
            .get(session_id)
            .map(|v| v.iter().map(|(c, _)| to_spec(c)).collect())
            .unwrap_or_default()
    }

    /// 设置会话级步数预算（视频观看轮次等自动任务的「防失控」通道面配置）：
    /// n=0 清除覆盖（回落个体 maxSteps / 档位 / 全局默认）。
    pub fn set_session_step_budget(&self, session_id: &str, steps: usize) {
        let mut map = self.session_step_budgets.write();
        if steps == 0 {
            map.remove(session_id);
        } else {
            map.insert(session_id.to_string(), steps);
        }
    }

    /// 查会话级步数覆盖（None = 未设置，走默认三级解析）
    pub fn session_step_budget(&self, session_id: &str) -> Option<usize> {
        self.session_step_budgets.read().get(session_id).copied()
    }

    /// 单轮预算清零（agent-budget-audit）：chat_execute 每轮进入时调用——
    /// 单轮预算只约束「一轮用户输入」内的工具循环,跨轮不累计。
    pub fn reset_turn_tool_budget(&self, session_id: &str) {
        if let Some(c) = self.tool_call_counters.lock().get_mut(session_id) {
            c.turn = 0;
        }
    }

    /// 会话删除时清理预算计数（与步数覆盖/授权表同生命周期）
    pub fn clear_tool_budget(&self, session_id: &str) {
        self.tool_call_counters.lock().remove(session_id);
        self.session_scopes.write().remove(session_id);
    }

    /// 记录会话作用域（agent-budget-audit）：ChannelRun::begin 快照通道 capability。
    /// 控制台/CLI 会话不写（缺省 Computer 全量档）。
    pub fn set_session_scope(&self, session_id: &str, scope: crate::types::ToolScope) {
        self.session_scopes
            .write()
            .insert(session_id.to_string(), scope);
    }

    /// 查会话作用域（缺省 Computer）
    pub(crate) fn session_scope_of(&self, session_id: &str) -> crate::types::ToolScope {
        self.session_scopes
            .read()
            .get(session_id)
            .copied()
            .unwrap_or_default()
    }

    /// 工具调用预算预检 + 计数（agent-budget-audit）。
    /// 超限返回余量说明（调用方以受限结果回灌模型收束）;未超限计数并放行。
    /// scope 取会话快照（通道 capability;缺省 Computer）,档位表 scope_budget。
    pub fn consume_tool_budget(&self, session_id: &str) -> Result<(), String> {
        let scope = self.session_scope_of(session_id);
        let full = self.security.tool_call_budget;
        if full.disabled() {
            return Ok(());
        }
        let b = scope_budget(&full, scope);
        let now = crate::types::now_ms();
        let mut map = self.tool_call_counters.lock();
        let c = map.entry(session_id.to_string()).or_default();
        if b.per_turn > 0 && c.turn >= b.per_turn {
            return Err(format!(
                "单轮工具调用预算已耗尽（上限 {}，本轮已用 {}）。请基于已有结果收束本轮回复；下轮输入后预算恢复。",
                b.per_turn, c.turn
            ));
        }
        if b.per_session > 0 && c.session_total >= b.per_session {
            return Err(format!(
                "本会话工具调用累计预算已耗尽（上限 {}）。继续操作请开新会话或让管理员调整 security.toolCallBudget。",
                b.per_session
            ));
        }
        if b.per_window > 0 && c.window_count(now, b.window_secs) >= b.per_window {
            return Err(format!(
                "时间窗工具调用预算已耗尽（{} 秒内上限 {}）。请稍候重试或减少连续调用。",
                b.window_secs, b.per_window
            ));
        }
        c.turn += 1;
        c.session_total += 1;
        if b.per_window > 0 {
            c.window_stamps.push_back(now);
        }
        Ok(())
    }

    /// 预算余量快照（事件载荷/调试用）
    pub fn tool_budget_snapshot(&self, session_id: &str) -> serde_json::Value {
        let scope = self.session_scope_of(session_id);
        let full = self.security.tool_call_budget;
        if full.disabled() {
            return serde_json::json!({ "enabled": false });
        }
        let b = scope_budget(&full, scope);
        let map = self.tool_call_counters.lock();
        let c = map.get(session_id);
        let turn_used = c.map(|c| c.turn).unwrap_or(0);
        let session_used = c.map(|c| c.session_total).unwrap_or(0);
        serde_json::json!({
            "enabled": true,
            "scope": format!("{scope:?}"),
            "perTurn": b.per_turn, "turnUsed": turn_used,
            "perSession": b.per_session, "sessionUsed": session_used,
            "perWindow": b.per_window, "windowSecs": b.window_secs,
        })
    }

    /// 该个体可见的自定义工具 schema（与内置工具一起随 function calling 下发）。
    /// 插件工具（`<plugin>__<tool>` 命名空间）一并发下——清单未配 agents = 全体可见。
    pub fn custom_specs_for(&self, agent_id: &str) -> Vec<crate::provider::ToolSpec> {
        let to_spec = |c: &crate::config::CustomTool| crate::provider::ToolSpec {
            name: c.name.trim().to_string(),
            description: if c.description.trim().is_empty() {
                format!("自定义工具 {}（{}）", c.name, c.kind_norm())
            } else {
                c.description.clone()
            },
            parameters: c.param_schema(),
        };
        self.custom
            .iter()
            .filter(|c| c.visible_to(agent_id))
            .map(to_spec)
            .chain(
                self.plugin_tools
                    .read()
                    .iter()
                    .filter(|c| c.visible_to(agent_id))
                    .map(to_spec),
            )
            .chain(
                self.dynamic_tools
                    .read()
                    .values()
                    .filter(|(c, _)| c.visible_to(agent_id))
                    .map(|(c, _)| to_spec(c)),
            )
            // 进程插件运行时工具（T2 握手发现；清单未配 agents = 全体可见）
            .chain(
                self.plugin_runtime_tools
                    .read()
                    .values()
                    .flat_map(|p| p.tool_specs().into_iter())
                    .filter(|c| c.visible_to(agent_id))
                    .map(|c| to_spec(&c)),
            )
            .collect()
    }

    /// 自定义工具是否已声明（诊断 / doctor 用）
    pub fn has_custom_tool(&self, name: &str) -> bool {
        self.custom.iter().any(|c| c.name == name)
    }

    /// 注入生命周期钩子配置
    pub fn with_hooks(mut self, hooks: crate::config::HooksConfig) -> Self {
        self.hooks = hooks;
        self
    }

    /// 注入沙箱执行策略
    pub fn with_sandbox(mut self, sandbox: SandboxConfig) -> Self {
        self.sandbox = sandbox;
        self
    }

    /// 注入浏览器配置（browser 工具）
    pub fn with_browser(mut self, browser: crate::config::BrowserConfig) -> Self {
        self.browser_cfg = browser;
        self
    }

    /// 注入 Computer Use 配置（computer 工具；enabled=false 时不下发）
    pub fn with_computer(mut self, computer: crate::config::ComputerUseConfig) -> Self {
        self.computer_cfg = computer;
        self
    }

    /// Computer Use 是否可用（总开关开启即下发——无交互桌面时调用会得到明确报错，
    /// 而「承诺存在但配置关闭」与「不存在」是两回事）
    pub fn computer_ready(&self) -> bool {
        self.computer_cfg.enabled
    }

    /// 浏览器是否可用（探测可执行文件；不可用时不下发 browser 工具，避免「承诺不存在的能力」）
    pub fn browser_ready(&self) -> bool {
        crate::browser::find_browser(&self.browser_cfg.executable).is_some()
    }

    /// 注入定时任务存储（schedule 工具）
    pub fn with_cron(mut self, cron: Arc<crate::cron::CronStore>) -> Self {
        self.cron = Some(cron);
        self
    }

    /// cron 调度时区（schedule 工具回显/创建说明按该时区解释 cron；
    /// 空 = UTC 既有行为）。来源 automation.timezone。
    pub fn with_cron_timezone(mut self, tz: String) -> Self {
        self.cron_timezone = tz;
        self
    }

    /// 搜索后端是否就绪（决定 web_search 是否出现在下发给模型的工具表里）
    pub fn search_ready(&self) -> bool {
        self.search.ready()
    }

    /// 语音合成是否装配（决定 speech_reply 是否下发：能力未回接不承诺）
    pub fn speech_ready(&self) -> bool {
        self.speech_synth.read().is_some()
    }

    /// 主动搜索纪律注入门控（proactive-search）：任一搜索通路可用
    /// （API 后端就绪 / 浏览器可探测）且开关开启。全部不可用或显式关闭时
    /// 返回 false——提示层零注入（零回归语义）。
    pub fn search_discipline_ready(&self) -> bool {
        self.search.proactive && (self.search.ready() || self.browser_ready())
    }

    /// 测试可见：审批单断言用
    #[cfg(test)]
    pub(crate) fn store_for_test(&self) -> &Store {
        &self.store
    }

    /// 单智能体对话循环全局步数上限（security.chatMaxSteps；0 值配置视为默认）
    pub fn chat_max_steps(&self) -> usize {
        let v = self.security.chat_max_steps;
        if v == 0 {
            24
        } else {
            v
        }
    }

    /// 上下文预算安全线（security.contextBudgetRatio；越界值回落 0.8）
    pub fn context_budget_ratio(&self) -> f64 {
        let r = self.security.context_budget_ratio;
        if (0.5..=1.0).contains(&r) {
            r
        } else {
            0.8
        }
    }

    /// 记录一次 LLM 调用的真实用量（provider 返回的 usage；session 空 = 归入 unattributed）
    pub fn record_usage(
        &self,
        session_id: &str,
        prompt_tokens: u64,
        completion_tokens: u64,
        model: &str,
    ) {
        let scope = if session_id.trim().is_empty() {
            "unattributed"
        } else {
            session_id.trim()
        };
        let _ = self
            .store
            .add_usage(scope, prompt_tokens, completion_tokens, model);
    }

    /// 运行收束钩子（onRunEnd）：由指挥体在 run 结束时调用
    pub fn run_end_hooks(&self, status: &str, session_id: &str, agent_id: &str) -> Vec<String> {
        if self.hooks.on_run_end.is_empty() {
            return Vec::new();
        }
        let hooks = self.hooks.on_run_end.clone();
        let mut notes = Vec::new();
        for h in hooks {
            let (ok, out) = run_hook_command(
                &h,
                &self.effective_root(),
                &[
                    ("EXM_HOOK", "run_end"),
                    ("EXM_RUN_STATUS", status),
                    ("EXM_SESSION", session_id),
                    ("EXM_AGENT", agent_id),
                ],
            );
            if !ok {
                notes.push(format!("钩子失败（{h}）：{out}"));
            }
        }
        notes
    }

    /// 注入 MCP 服务器池（build_orchestrator 配置后传入同一实例）
    pub fn with_mcp(mut self, mcp: Arc<crate::mcp::McpRegistry>) -> Self {
        self.mcp = mcp;
        self
    }

    pub fn mcp(&self) -> Arc<crate::mcp::McpRegistry> {
        self.mcp.clone()
    }

    /// 执行 MCP 工具（全名 mcp:server:tool）；审计与内置工具一致落库，事件流对用户可见
    pub async fn execute_mcp(
        &self,
        agent_id: &str,
        session_id: &str,
        full_name: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        let started = Instant::now();
        let call_id = format!("t{}", &crate::types::new_id()[..12]);
        let _ = self.events.send(CoreEvent {
            kind: "tool.call".into(),
            session_id: session_id.to_string(),
            payload: serde_json::json!({
                "callId": call_id, "agentId": agent_id, "tool": full_name, "args": args,
            }),
        });
        let result = match self.mcp.call(full_name, agent_id, args).await {
            Some(r) => ToolResult {
                ok: r.ok,
                output: r.text.chars().take(8000).collect(),
                error: if r.ok {
                    None
                } else {
                    Some(r.text.chars().take(2000).collect())
                },
                images: Vec::new(),
            },
            None => ToolResult::err(format!("MCP 工具不存在或未开放: {full_name}")),
        };
        let summary: String = if result.ok {
            result.output.chars().take(200).collect()
        } else {
            result
                .error
                .clone()
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect()
        };
        if let Err(e) = self.store.audit_tool_ex(
            agent_id,
            full_name,
            args,
            &summary,
            started.elapsed().as_millis() as u64,
            result.ok,
        ) {
            eprintln!("[audit] 审计落库失败：{}", e);
        }
        // 轮次过程轨迹：随收束落盘到最终消息（过程收起栏数据源）
        crate::round_trace::push_tool(
            session_id,
            crate::round_trace::ToolCallRecord {
                call_id: call_id.clone(),
                agent_id: agent_id.to_string(),
                tool: full_name.to_string(),
                args: args.clone(),
                ok: result.ok,
                duration_ms: started.elapsed().as_millis() as u64,
                summary,
            },
        );
        self.emit_tool_result(session_id, &call_id, agent_id, full_name, &result, &started);
        result
    }

    /// 生效工作区根：显式工作目录（干活的项目）> 激活组声明 > 全局根（统一走注册表口径）
    pub(crate) fn effective_root(&self) -> PathBuf {
        self.registry.effective_workspace(&self.workspace_root)
    }





    /// 工具层事件发双层：实时广播（WS 推流）+ 事件库落盘（历史可回放）。
    /// 此前 tool.call/tool.result 只广播不落库——历史会话的事件页看不到任何工具过程，
    /// 「过程透明」只对在线连接生效。落库量与工具次数同阶（每次调用 2 条），
    /// 与 token 流水账（一次运行数千条）不同量级，不会刷屏。
    pub(crate) fn emit_and_persist(&self, evt: CoreEvent) {
        let _ = self.events.send(evt.clone());
        let env = crate::types::Envelope::new(
            format!("session.{}.events", evt.session_id),
            "tools",
            "broadcast",
            "event",
            evt.session_id.clone(),
            serde_json::to_value(&evt).unwrap_or(serde_json::Value::Null),
        );
        let _ = self.store.append_event(&env);
    }

    /// 执行内置工具（按枚举）：白名单外拒绝 → preTool 钩子（可拦截）→ 执行 → 结果落盘 → postTool 钩子 → 审计
    pub async fn execute(
        &self,
        agent_id: &str,
        session_id: &str,
        allowlist: &[ToolName],
        tool: ToolName,
        args: &serde_json::Value,
    ) -> ToolResult {
        self.execute_named(agent_id, session_id, allowlist, tool.key(), args)
            .await
    }

    /// 执行任意工具名：内置（按 `allowlist` 放行）或**声明式自定义工具**（按 `agents` 可见性放行）。
    /// 模型给出的工具名统一走这里，钩子 / 结果落盘 / 审计对两类工具一视同仁。
    /// `session_id` 用于：工具执行事件（tool.call / tool.result）与审批单归属——渠道可按会话过滤呈现。
    pub async fn execute_named(
        &self,
        agent_id: &str,
        session_id: &str,
        allowlist: &[ToolName],
        name: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        let started = Instant::now();
        let call_id = format!("t{}", &crate::types::new_id()[..12]);
        // 工具执行事件（call）：文件操作 / 终端命令 / 截屏对用户全程可见——编程软件式的过程透明
        // 插件工具归因：`<plugin>__<tool>` 名字解析出 plugin id 写入事件（审计可检索）
        let plugin_attr = crate::plugin::parse_namespaced(name)
            .map(|(p, _)| serde_json::json!(p))
            .unwrap_or(serde_json::Value::Null);
        // 载荷脱敏（agent-budget-audit 3.5）：密钥类参数（token/sk-/password…）
        // 进事件与审计前打码——事件库与广播都是明文面，原文只留在工具执行现场。
        let args_redacted = super::redact::redact_json(args);
        self.emit_and_persist(CoreEvent {
            kind: "tool.call".into(),
            session_id: session_id.to_string(),
            payload: serde_json::json!({
                "callId": call_id, "agentId": agent_id, "tool": name, "args": args_redacted,
                "plugin": plugin_attr,
            }),
        });
        // 工具调用预算（agent-budget-audit）：超限即熔断——不执行本次调用,
        // 受限结果回灌模型（说明预算状态）,模型自然收束本轮;事件页可见熔断条目。
        if let Err(reason) = self.consume_tool_budget(session_id) {
            self.emit_and_persist(CoreEvent {
                kind: "tool.budget".into(),
                session_id: session_id.to_string(),
                payload: serde_json::json!({
                    "callId": call_id, "tool": name, "reason": reason,
                    "budget": self.tool_budget_snapshot(session_id),
                }),
            });
            return ToolResult::err(format!("【预算受限】{reason}"));
        }
        // 计划模式拦截（spec: plan-mode）：写类工具不执行，转为计划条目累积清单。
        // 读取类工具放行（read/grep/glob/web/截屏/git 只读等）
        if let Some(tool) = ToolName::parse(name) {
            if crate::plan::is_plan_mode(session_id) && crate::plan::is_write_tool(tool, args) {
                let item = crate::plan::to_plan_item(tool, args);
                crate::plan::push_item(session_id, item.clone());
                let pending_count = crate::plan::list_items(session_id).len();
                self.emit_and_persist(CoreEvent {
                    kind: "plan.item".into(),
                    session_id: session_id.to_string(),
                    payload: serde_json::json!({
                        "itemId": item.id, "tool": item.tool, "target": item.target,
                        "action": item.action, "pendingCount": pending_count,
                    }),
                });
                // 首条出现 = 待审批状态生效：广播 + 复用 approvals 通道落计划审批单
                // （批准/拒绝经 approval_decide 的 [plan] 分支或 REST 计划端点处理）
                if pending_count == 1 {
                    let req = crate::types::ApprovalRequest {
                        id: crate::types::new_id()[..8].to_string(),
                        session_id: session_id.to_string(),
                        node_id: None,
                        agent_id: agent_id.to_string(),
                        command: format!("[plan] 会话计划清单（{} 条待执行）", pending_count),
                        status: "pending".into(),
                        result: None,
                        created_at: crate::types::now_iso(),
                        decided_at: None,
                        origin_user: None,
                        origin_role: None,
                    
                        meta_json: None,
                    };
                    let _ = self.store.add_approval(&req);
                    crate::plan::set_pending_approval(session_id, &req.id);
                    self.emit_and_persist(CoreEvent {
                        kind: "plan.pending".into(),
                        session_id: session_id.to_string(),
                        payload: serde_json::json!({
                            "count": 1,
                            "approvalId": req.id,
                        }),
                    });
                }
                let blocked = ToolResult::ok(format!(
                    "【计划模式】该写操作已记录为计划条目（{} {} {}），未执行；\
                     继续规划或等待用户批准计划后统一执行。",
                    item.action, item.tool, item.target
                ));
                self.emit_tool_result(session_id, &call_id, agent_id, name, &blocked, &started);
                return blocked;
            }
        }
        // preTool 钩子：非零退出即拦截（返回原因给模型，计入审计）
        if !self.hooks.pre_tool.is_empty() {
            let args_json = serde_json::to_string(args).unwrap_or_default();
            let short_args: String = args_json.chars().take(4000).collect();
            let root = self.effective_root();
            let root_s = root.display().to_string();
            for h in self.hooks.pre_tool.clone() {
                let (ok, out) = run_hook_command(
                    &h,
                    &root,
                    &[
                        ("EXM_HOOK", "pre_tool"),
                        ("EXM_TOOL", name),
                        ("EXM_AGENT", agent_id),
                        ("EXM_ARGS", &short_args),
                        ("EXM_WORKSPACE", &root_s),
                    ],
                );
                if !ok {
                    let why = if out.trim().is_empty() {
                        h.clone()
                    } else {
                        out.clone()
                    };
                    let blocked = ToolResult::err(format!("preTool 钩子拦截：{why}"));
                    if let Err(e) = self.store.audit_tool_ex(
                        agent_id,
                        name,
                        args,
                        "preTool 钩子拦截",
                        started.elapsed().as_millis() as u64,
                        false,
                    ) {
            eprintln!("[audit] 审计落库失败：{}", e);
        }
                    self.emit_tool_result(session_id, &call_id, agent_id, name, &blocked, &started);
                    return blocked;
                }
            }
        }

        let result = match ToolName::parse(name) {
            Some(tool) => {
                if !allowlist.contains(&tool) && tool != ToolName::ContextCompact {
                    ToolResult::err(format!("工具 {} 不在本节点放行白名单内", tool.key()))
                } else if tool == ToolName::AgentManage
                    && self
                        .registry
                        .active_group_meta()
                        .and_then(|m| m.primary)
                        .as_deref()
                        != Some(agent_id)
                {
                    // 纵深防御：allowlist 注入之外再校验调用者身份
                    ToolResult::err("agent_manage 仅限激活组主智能体使用")
                } else if tool == ToolName::Computer {
                    self.tool_computer(agent_id, session_id, args).await
                } else if tool == ToolName::ContextCompact {
                    // 实验性动态压缩：豁免 allowlist（能力面由 schema 下发门控控制——
                    // 未开实验开关时模型根本看不到该工具 schema，不会有调用进来）
                    self.tool_context_compact(agent_id, session_id, args).await
                } else {
                    self.dispatch(agent_id, session_id, tool, args).await
                }
            }
            // 非内置名：会话级注入（视频观看会话）→ 动态命名空间工具（T3 内置插件）→ 声明式自定义工具
            None => {
                // 会话级注入优先（按会话收窄的插件工具：video__* 等）
                if let Some((spec, exec)) = self.session_tool(session_id, name) {
                    if spec.visible_to(agent_id) {
                        let out = exec(agent_id, session_id, args.clone()).await;
                        if out.ok {
                            ToolResult::ok_with_images(out.text, out.images)
                        } else {
                            ToolResult::err(out.text)
                        }
                    } else {
                        ToolResult::err(format!(
                            "工具 {name} 未对本个体开放（插件工具的 agents 未包含 {agent_id}）"
                        ))
                    }
                } else if let Some((spec, exec)) = self.dynamic_tool(name) {
                    // 动态表（T3 内置插件全局注册面）
                    if spec.visible_to(agent_id) {
                        let out = exec(agent_id, session_id, args.clone()).await;
                        if out.ok {
                            ToolResult::ok_with_images(out.text, out.images)
                        } else {
                            ToolResult::err(out.text)
                        }
                    } else {
                        ToolResult::err(format!(
                            "工具 {name} 未对本个体开放（插件工具的 agents 未包含 {agent_id}）"
                        ))
                    }
                } else {
                    // 查找顺序：config CustomTool 先于插件工具（先注册者胜）；
                    // 进程插件运行时工具最后（T2 握手发现面）
                    let found = self
                        .custom
                        .iter()
                        .find(|c| c.name == name)
                        .cloned()
                        .or_else(|| {
                            self.plugin_tools
                                .read()
                                .iter()
                                .find(|c| c.name == name)
                                .cloned()
                        });
                    match found {
                        Some(ct) if ct.visible_to(agent_id) => {
                            self.run_custom(agent_id, session_id, &ct, args).await
                        }
                        Some(_) => ToolResult::err(format!(
                            "工具 {name} 未对本个体开放（自定义工具的 agents 未包含 {agent_id}）"
                        )),
                        None => {
                            // 进程插件运行时工具（T2）：转发到 provider（supervisor → 子进程）
                            let provider = self
                                .plugin_runtime_tools
                                .read()
                                .values()
                                .find(|p| p.tool_specs().iter().any(|c| c.name == name))
                                .cloned();
                            match provider {
                                Some(p) => match p.call_tool(name, args) {
                                    Ok(text) => ToolResult::ok(text),
                                    Err(e) => ToolResult::err(format!("插件工具执行失败：{e}")),
                                },
                                None => ToolResult::err(format!("未知工具：{name}")),
                            }
                        }
                    }
                }
            }
        };
        let result = self.spill_if_large(name, result);
        // postTool 钩子：仅留痕（失败不改变工具结果，但会追加到输出末尾提示）
        let mut result = result;
        if !self.hooks.post_tool.is_empty() {
            let root = self.effective_root();
            let root_s = root.display().to_string();
            let ok_s = if result.ok { "true" } else { "false" };
            let preview: String = if result.ok {
                result.output.chars().take(4000).collect()
            } else {
                result
                    .error
                    .clone()
                    .unwrap_or_default()
                    .chars()
                    .take(4000)
                    .collect()
            };
            for h in self.hooks.post_tool.clone() {
                let (hook_ok, out) = run_hook_command(
                    &h,
                    &root,
                    &[
                        ("EXM_HOOK", "post_tool"),
                        ("EXM_TOOL", name),
                        ("EXM_AGENT", agent_id),
                        ("EXM_TOOL_OK", ok_s),
                        ("EXM_OUTPUT", &preview),
                        ("EXM_WORKSPACE", &root_s),
                    ],
                );
                if !hook_ok {
                    let tail = format!(
                        "\n[postTool 钩子失败] {}",
                        out.chars().take(300).collect::<String>()
                    );
                    if result.ok {
                        result.output.push_str(&tail);
                    } else if let Some(e) = result.error.as_mut() {
                        e.push_str(&tail);
                    }
                }
            }
        }
        let summary = if result.ok {
            result.output.chars().take(200).collect::<String>()
        } else {
            result
                .error
                .clone()
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect()
        };
        if let Err(e) = self.store.audit_tool_ex(
            agent_id,
            name,
            args,
            &summary,
            started.elapsed().as_millis() as u64,
            result.ok,
        ) {
            eprintln!("[audit] 审计落库失败：{}", e);
        }
        // 轮次过程轨迹：与 execute_mcp 一致,内置/自定义工具执行同样随收束落盘到最终消息
        crate::round_trace::push_tool(
            session_id,
            crate::round_trace::ToolCallRecord {
                call_id: call_id.clone(),
                agent_id: agent_id.to_string(),
                tool: name.to_string(),
                args: args.clone(),
                ok: result.ok,
                duration_ms: started.elapsed().as_millis() as u64,
                summary,
            },
        );
        self.emit_tool_result(session_id, &call_id, agent_id, name, &result, &started);
        result
    }

    /// 工具执行事件（result）：状态 / 耗时 / 摘要 / 图像清单——渠道据此渲染工具卡与截图预览
    pub(crate) fn emit_tool_result(
        &self,
        session_id: &str,
        call_id: &str,
        agent_id: &str,
        name: &str,
        result: &ToolResult,
        started: &Instant,
    ) {
        let text = if result.ok {
            result.output.clone()
        } else {
            result.error.clone().unwrap_or_default()
        };
        // 插件归因（与 tool.call 对称；非插件工具为 null 不占字段语义）
        let plugin_attr = crate::plugin::parse_namespaced(name)
            .map(|p| serde_json::json!(p.0))
            .unwrap_or(serde_json::Value::Null);
        self.emit_and_persist(CoreEvent {
            kind: "tool.result".into(),
            session_id: session_id.to_string(),
            payload: serde_json::json!({
                "callId": call_id, "agentId": agent_id, "tool": name,
                "ok": result.ok,
                "durationMs": started.elapsed().as_millis() as u64,
                "summary": text.chars().take(400).collect::<String>(),
                "images": result.images,
                "plugin": plugin_attr,
            }),
        });
        // 历史回放可见工具的成败与耗时摘要（不止「开始了」）——落库在 emit_and_persist 内完成
    }

    /// 结果超阈值时全文落盘并回填「摘要 + 路径 + 行数」，供模型按需用 read 取回。
    /// 阈值 0 = 关闭（仅按工具自身的截断）。
    pub(crate) fn spill_if_large(&self, name: &str, result: ToolResult) -> ToolResult {
        let limit = self.security.tool_output_spill_chars;
        if limit == 0 {
            return result;
        }
        let (text, is_err) = if result.ok {
            (result.output.clone(), false)
        } else {
            (result.error.clone().unwrap_or_default(), true)
        };
        if text.chars().count() <= limit {
            return result;
        }
        let dir = self.workspace_root.join(".exmachina").join("tool-output");
        if std::fs::create_dir_all(&dir).is_err() {
            return result;
        }
        let stamp = crate::types::now_iso().replace([':', '-'], "");
        let file = dir.join(format!("{stamp}-{name}.txt"));
        if std::fs::write(&file, &text).is_err() {
            return result;
        }
        let lines = text.lines().count();
        let head: String = text.chars().take(4000).collect();
        let rel = file
            .strip_prefix(&self.workspace_root)
            .map(|p| p.display().to_string().replace('\\', "/"))
            .unwrap_or_else(|_| file.display().to_string());
        let note = format!(
            "{head}\n\n…（结果共 {total} 字符 / {lines} 行，已截断）\n完整内容已落盘：{rel}\n可用 read 工具按 offset/limit 分段读取。",
            total = text.chars().count()
        );
        if is_err {
            ToolResult::err(note)
        } else {
            ToolResult::ok(note)
        }
    }

    pub(crate) async fn dispatch(
        &self,
        agent_id: &str,
        session_id: &str,
        tool: ToolName,
        args: &serde_json::Value,
    ) -> ToolResult {
        match tool {
            ToolName::Read => self.tool_read(args),
            ToolName::Edit => self.tool_edit(session_id, args),
            ToolName::Patch => self.tool_patch(session_id, args),
            ToolName::Grep => self.tool_grep(args),
            ToolName::Glob => self.tool_glob(args),
            ToolName::Filesystem => self.tool_fs(session_id, args),
            ToolName::Terminal => self.tool_terminal(agent_id, session_id, args).await,
            ToolName::WebSearch => self.tool_web_search(args).await,
            ToolName::WebFetch => self.tool_web_fetch(args).await,
            ToolName::AgentManage => self.tool_agent_manage(args),
            ToolName::Schedule => self.tool_schedule(args),
            ToolName::Browser => self.tool_browser(session_id, args).await,
            ToolName::Computer => self.tool_computer(agent_id, session_id, args).await,
            ToolName::MemoryRead => self.tool_memory_read(agent_id, session_id, args),
            ToolName::MemoryWrite => self.tool_memory_write(agent_id, session_id, args),
            ToolName::MemoryLink => self.tool_memory_link(agent_id, session_id, args),
            ToolName::Git => self.tool_git(agent_id, session_id, args).await,
            ToolName::Spawn => self.tool_spawn(agent_id, session_id, args).await,
            ToolName::Sessions => self.tool_sessions(agent_id, session_id, args).await,
            ToolName::SkillManage => self.tool_skill_manage(args),
            ToolName::Todo => self.tool_todo(session_id, args),
            ToolName::Speech => self.tool_speech(session_id, args).await,
            ToolName::ImageGen => self.tool_image_gen(session_id, args).await,
            ToolName::KnowledgeWrite => {
                self.tool_knowledge_write(agent_id, session_id, args).await
            }
            ToolName::KnowledgeRead => {
                self.tool_knowledge_read(agent_id, session_id, args).await
            }
            ToolName::MetaObserve | ToolName::MetaPropose | ToolName::MetaBuild => {
                self.tool_meta(agent_id, session_id, tool, args).await
            }
            ToolName::ContextCompact => unreachable!("context_compact 在 execute_named 预分发"),
        }
    }

    /// 审批闸门是否开启（risky/always）——白名单外命令转人工审批；off 档直接拒绝
    pub fn is_approval_gated(&self) -> bool {
        matches!(self.security.exec_approval.as_str(), "risky" | "always")
    }
}
