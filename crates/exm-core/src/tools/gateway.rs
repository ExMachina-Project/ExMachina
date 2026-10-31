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
    /// 子智能体派生执行器（spawn 工具；未注入 = spawn 返回未装配错误）。
    /// RwLock 槽：Core 装配后回接（tools 已 Arc 共享给 runtime，无法再拿可变引用）
    pub(crate) spawn_executor: parking_lot::RwLock<Option<SpawnExec>>,
    /// 上下文压缩执行器（context_compact 工具，实验性；未注入 = 工具返回未装配说明）。
    /// 槽位语义同 spawn_executor：Core 装配后回接 (provider, model)。
    pub(crate) compactor: parking_lot::RwLock<Option<(Arc<dyn LlmProvider>, String)>>,
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
    /// 记忆库（memory_read/write/link 三工具的受控入口；组隔离在工具层强制）
    pub(crate) memory: Option<Arc<crate::memory::MemoryStore>>,
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
            spawn_executor: parking_lot::RwLock::new(None),
            compactor: parking_lot::RwLock::new(None),
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
            memory: None,
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

    /// 注入子智能体派生执行器（spawn 工具）：Core 装配后回接 AgentRuntime。
    /// 闭包签名：(主个体 id, 会话 id, 任务描述, 工具白名单) → 执行产出（结论文本）。
    /// 并发/超时控制由网关层实现（spec: subagent-spawn）。
    pub fn set_spawn_executor(&self, exec: SpawnExec) {
        *self.spawn_executor.write() = Some(exec);
    }

    /// 注入上下文压缩执行器（context_compact 工具，实验性开关开启时由 Core 装配后回接）
    pub fn set_compactor(&self, provider: Arc<dyn LlmProvider>, model: String) {
        *self.compactor.write() = Some((provider, model));
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

    /// 搜索后端是否就绪（决定 web_search 是否出现在下发给模型的工具表里）
    pub fn search_ready(&self) -> bool {
        self.search.ready()
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
        let _ = self.store.audit_tool(
            agent_id,
            full_name,
            args,
            &summary,
            started.elapsed().as_millis() as u64,
        );
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

    /// 派发时的运行时工具白名单（8.2 口径）：
    /// - 个体 = 编成 JSON 的 tools 清单（未授权工具不在其工具面，不可见亦不可调用）
    /// - 组主智能体 = 额外获得组内个体管理工具（自定义组）与记忆三工具（默认授权）
    /// - 智能体（tier=orchestrator 且不在组内）按编成清单，可经 tools 显式授权记忆工具
    pub fn runtime_allowlist(
        def: &AgentDefinition,
        group_meta: Option<&crate::types::GroupMeta>,
    ) -> Vec<ToolName> {
        let mut allowlist = def.tools.clone();
        let is_primary = group_meta
            .map(|m| m.primary.as_deref() == Some(def.identifier.as_str()))
            .unwrap_or(false);
        if is_primary {
            if !group_meta.map(|m| m.builtin).unwrap_or(true)
                && !allowlist.contains(&ToolName::AgentManage)
            {
                allowlist.push(ToolName::AgentManage);
            }
            for t in [
                ToolName::MemoryRead,
                ToolName::MemoryWrite,
                ToolName::MemoryLink,
            ] {
                if !allowlist.contains(&t) {
                    allowlist.push(t);
                }
            }
        }
        allowlist
    }

    /// 原生 function calling：按白名单生成工具 schema（docs/协议与契约.md）。
    /// `search_ready` / `browser_ready` 决定联网搜索与浏览器工具是否下发——
    /// 后端能力未就绪时不承诺该工具（避免模型调用必然失败的工具）。
    pub fn tool_specs(
        allowlist: &[ToolName],
        search_ready: bool,
        browser_ready: bool,
        computer_ready: bool,
    ) -> Vec<crate::provider::ToolSpec> {
        let mut specs: Vec<crate::provider::ToolSpec> = Vec::new();
        let mut push = |name: ToolName, description: &str, parameters: serde_json::Value| {
            if !allowlist.contains(&name) {
                return;
            }
            if name == ToolName::WebSearch && !search_ready {
                return;
            }
            if name == ToolName::Browser && !browser_ready {
                return; // 未探测到浏览器：不下发
            }
            if name == ToolName::Computer && !computer_ready {
                return; // Computer Use 未启用：不下发（整机输入权须显式交出）
            }
            specs.push(crate::provider::ToolSpec {
                name: name.key().to_string(),
                description: description.to_string(),
                parameters,
            });
        };
        push(ToolName::Read, "读取工作区内文件或目录。文件按行返回并带行号，可用 offset/limit 分页续读；目录返回条目清单", serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "相对工作区的文件或目录路径" },
                "offset": { "type": "integer", "description": "起始行号（1 起，默认 1）" },
                "limit": { "type": "integer", "description": "最多返回行数（默认 400）" },
                "maxChars": { "type": "integer", "description": "返回字符上限（默认 24000）" }
            },
            "required": ["path"]
        }));
        push(ToolName::Edit, "精确编辑文件：把 old_string 替换为 new_string（old_string 必须唯一命中；整段替换请用 filesystem write）", serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "相对工作区的文件路径" },
                "old_string": { "type": "string", "description": "被替换的原文（含缩进与换行，必须与文件内容完全一致且唯一）" },
                "new_string": { "type": "string", "description": "替换后的内容；空串 = 删除该段" },
                "replace_all": { "type": "boolean", "description": "true = 替换全部命中（默认 false，多命中会报错并列出位置）" }
            },
            "required": ["path", "old_string", "new_string"]
        }));
        push(ToolName::Patch, "应用统一 diff 补丁到工作区文件：支持多文件段与新建/删除；任一文件的上下文校验失败则整体拒绝（不写入任何文件），全部通过才原子应用并写检查点", serde_json::json!({
            "type": "object",
            "properties": {
                "patch": { "type": "string", "description": "统一 diff 格式补丁全文（含 ---/+++ 路径行与 @@ hunk 头；路径相对工作区）" }
            },
            "required": ["patch"]
        }));
        push(
            ToolName::Grep,
            "在工作区内按正则搜索文件内容（尊重 .gitignore），返回 文件:行号:内容",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "正则表达式（Rust regex 语法）" },
                    "path": { "type": "string", "description": "搜索起始路径（默认工作区根）" },
                    "glob": { "type": "string", "description": "文件过滤，如 **/*.rs" },
                    "ignoreCase": { "type": "boolean", "description": "忽略大小写（默认 false）" },
                    "contextLines": { "type": "integer", "description": "命中行前后各带 N 行上下文（0-5，默认 0 = 单行输出）" },
                    "maxResults": { "type": "integer", "description": "最多返回条数（默认 100，上限 500）" }
                },
                "required": ["pattern"]
            }),
        );
        push(
            ToolName::Glob,
            "按 glob 模式列出工作区内的文件路径（尊重 .gitignore），用于定位文件",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "glob 模式，如 src/**/*.ts 或 **/*.md" },
                    "path": { "type": "string", "description": "起始目录（默认工作区根）" }
                },
                "required": ["pattern"]
            }),
        );
        push(
            ToolName::Filesystem,
            "文件系统操作：write（整文件写入/新建）、mkdir、list（条目清单）、delete（移入回收站可恢复，不裸删）、rename（重命名/移动，不覆盖已存在目标）",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "op": { "type": "string", "enum": ["write", "mkdir", "list", "delete", "rename"] },
                    "path": { "type": "string", "description": "相对工作区的路径" },
                    "content": { "type": "string", "description": "op=write 时的文件内容" },
                    "newPath": { "type": "string", "description": "op=rename 时的目标路径（相对工作区；目标存在则拒绝）" }
                },
                "required": ["op", "path"]
            }),
        );
        push(ToolName::Terminal, "在工作区内执行终端命令（白名单 + 审批闸门）。长任务用 background=true 启动后轮询 taskId", serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "op=run 时必填；支持 | && || ; 链式命令（每段须都在白名单）；` $( > < & 等逃逸/重定向符不支持" },
                "op": { "type": "string", "enum": ["run", "poll", "kill", "list"], "description": "默认 run；list 列出本会话后台任务（无需 taskId）" },
                "taskId": { "type": "string", "description": "op=poll/kill 时的后台任务 id" },
                "background": { "type": "boolean", "description": "true = 后台执行并立即返回任务 id（适合构建/测试等长任务）" },
                "timeoutSecs": { "type": "integer", "description": "前台命令超时（秒），上限由安全设置决定" }
            },
            "required": []
        }));
        push(
            ToolName::WebSearch,
            "联网搜索并返回结果摘要（标题 + 链接 + 摘要）",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "maxResults": { "type": "integer", "description": "返回条数（默认取配置值）" }
                },
                "required": ["query"]
            }),
        );
        push(
            ToolName::WebFetch,
            "抓取网页 URL 并提取正文文本（自动剥离标签与脚本）",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "http(s) 地址" },
                    "maxChars": { "type": "integer", "description": "最多返回字符数，默认 8000" }
                },
                "required": ["url"]
            }),
        );
        push(ToolName::Schedule, "自主排程：创建/查看/删除定时或一次性任务（到期由调度器唤醒本组执行）。用于「每天巡检」「明天提醒跟进」这类自驱工作", serde_json::json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["create", "list", "remove", "enable", "disable"] },
                "id": { "type": "string", "description": "op=list 时可过滤；remove/enable/disable 必填" },
                "name": { "type": "string", "description": "任务名（create 必填）" },
                "prompt": { "type": "string", "description": "到期时注入的提示词（create 必填）" },
                "cron": { "type": "string", "description": "五段 cron：分 时 日 月 周，如 0 9 * * *（与 at 二选一）" },
                "at": { "type": "string", "description": "一次性触发时间（ISO8601，如 2026-09-20T18:00:00+08:00）" }
            },
            "required": ["op"]
        }));
        push(ToolName::Browser, "浏览器自动化（headless Chrome + CDP）：导航网页、取渲染后正文、执行 JS、截图。适合抓动态页面 / 走一遍网页流程 / 留存页面证据", serde_json::json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["open", "text", "eval", "screenshot", "close"], "description": "默认 open" },
                "url": { "type": "string", "description": "op=open：http(s):// 或 file:// 地址" },
                "expression": { "type": "string", "description": "op=eval：页面内执行的 JS 表达式（可点击/填表：document.querySelector('#x').click()）" },
                "name": { "type": "string", "description": "op=screenshot：截图文件名（不含扩展名；缺省用时间戳）" },
                "maxChars": { "type": "integer", "description": "正文返回上限（默认取配置值）" }
            },
            "required": ["op"]
        }));
        push(ToolName::Computer, "Computer Use：控制本机桌面。screenshot 截屏（返回截图，视觉模型可直接看到屏幕画面）；click/move/scroll/type/key 操作鼠标键盘。坐标为截屏上的物理像素坐标。输入类动作可能需要人工审批", serde_json::json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["screenshot", "click", "move", "scroll", "type", "key"], "description": "screenshot=截屏并回看画面；click=点击；move=移动鼠标；scroll=滚动；type=键入文本；key=按键/热键" },
                "x": { "type": "integer", "description": "click/move：目标 X 坐标（截屏物理像素）" },
                "y": { "type": "integer", "description": "click/move：目标 Y 坐标（截屏物理像素）" },
                "button": { "type": "string", "enum": ["left", "right", "middle"], "description": "click：鼠标键，默认 left" },
                "double": { "type": "boolean", "description": "click：true = 双击" },
                "dx": { "type": "integer", "description": "scroll：横向格数（正右负左）" },
                "dy": { "type": "integer", "description": "scroll：纵向格数（正上负下）" },
                "text": { "type": "string", "description": "type：要键入的文本" },
                "combo": { "type": "string", "description": "key：按键组合，如 enter / ctrl+c / ctrl+shift+t / f5" },
                "monitor": { "type": "integer", "description": "screenshot：显示器序号（默认 0 主屏）" }
            },
            "required": ["op"]
        }));
        push(ToolName::AgentManage, "组内个体管理（仅主智能体）：templates/list/create/update/remove；create 支持基于编成模板", serde_json::json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["templates", "list", "create", "update", "remove"] },
                "template": { "type": "string", "description": "create 可选:模板 id(unit-*),先 templates 查询;提供后仅 identifier 必填" },
                "identifier": { "type": "string" },
                "name": { "type": "string" },
                "description": { "type": "string" },
                "domain": { "type": "string" },
                "tier": { "type": "string", "enum": ["unit", "orchestrator"] },
                "prompt": { "type": "string" },
                "capabilities": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["op"]
        }));
        push(ToolName::MemoryRead, "检索本组记忆（含你的个体私有层）：按关键词返回相关度排序的条目（内容/类型/置信度/重要性/来源）。执行中需要历史结论、约定或教训时先查再断言", serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "检索关键词（标题/正文/标签分词匹配）" },
                "limit": { "type": "integer", "description": "返回条数（默认 5，上限 20）" }
            },
            "required": ["query"]
        }));
        push(ToolName::MemoryWrite, "写入一条本组记忆：六类之一（fact/decision/preference/evidence/digest/lesson），内容哈希去重（重复写入提升既有条目重要性而非新建）。仅固化跨会话有价值的知识，不写过程性流水", serde_json::json!({
            "type": "object",
            "properties": {
                "kind": { "type": "string", "enum": ["fact", "decision", "preference", "evidence", "digest", "lesson"], "description": "fact=基础事实 decision=决策取舍 preference=用户偏好 evidence=证据 digest=摘要 lesson=教训" },
                "title": { "type": "string", "description": "一句话标题（去重按标题+内容判定）" },
                "body": { "type": "string", "description": "条目正文（精炼、可独立理解）" },
                "tags": { "type": "array", "items": { "type": "string" }, "description": "标签（可选，辅助检索）" },
                "importance": { "type": "number", "description": "重要性 0-1（默认 0.5）" }
            },
            "required": ["kind", "title", "body"]
        }));
        push(ToolName::MemoryLink, "记忆关联与主题列示：op=link 把两条记忆标注关联（如 derives-from/contradicts/supports，后续检索可见）；op=list 列示某主题下的条目及其关联链", serde_json::json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["link", "list"], "description": "默认 link" },
                "from": { "type": "string", "description": "op=link：起始条目 id" },
                "to": { "type": "string", "description": "op=link：目标条目 id" },
                "relation": { "type": "string", "description": "op=link：关系（derives-from / contradicts / supports / related）" },
                "query": { "type": "string", "description": "op=list：主题关键词" },
                "limit": { "type": "integer", "description": "op=list：返回条数（默认 5）" }
            },
            "required": []
        }));
        push(ToolName::Git, "工作区仓库的 Git 版本控制。只读动作直接执行：status/diff/log/branch/show/stash-list；写动作走审批闸门：add/commit/restore/stash-push/stash-pop/checkout-b。push/pull 等远程操作不在动作面（如需请用 terminal 工具走完整审批链）", serde_json::json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["status", "diff", "log", "branch", "show", "stash-list", "add", "commit", "restore", "stash-push", "stash-pop", "checkout-b"], "description": "git 动作" },
                "pathSpec": { "type": "string", "description": "add/restore/diff：目标路径（相对工作区，可多行）" },
                "message": { "type": "string", "description": "commit：提交信息" },
                "branch": { "type": "string", "description": "checkout-b：新分支名；branch：留空列分支" },
                "maxCount": { "type": "integer", "description": "log：返回条数（默认 20）" },
                "object": { "type": "string", "description": "show：对象引用（commit/分支/标签）" }
            },
            "required": ["action"]
        }));
        push(ToolName::Spawn, "派生子智能体执行封闭子任务：子智能体在独立上下文（不继承本会话历史）中执行任务描述，完成后返回结论摘要。适合全库检索、批量调研等可独立完成的工作。并发上限与超时受控；子智能体不可再派生（单层）。background=true 立即返回 spawnId 后台执行（期间可继续对话/派生其它子任务），用 op=status/list 回收结果", serde_json::json!({
            "type": "object",
            "properties": {
                "task": { "type": "string", "description": "自包含的任务书：目标、范围、期望产出（子智能体看不到本会话，必须写全背景）" },
                "tools": { "type": "array", "items": { "type": "string", "enum": ["read", "edit", "patch", "grep", "glob", "filesystem", "terminal", "git", "web_search", "web_fetch"] }, "description": "子智能体工具白名单（缺省只读：read/grep/glob/web_fetch）" },
                "timeoutSecs": { "type": "integer", "description": "子任务超时（秒；默认 300，范围 5-1800）" },
                "background": { "type": "boolean", "description": "true = 后台执行：立即返回 spawnId，结果经 op=status 查询（长任务并行时用）" },
                "op": { "type": "string", "enum": ["status", "list"], "description": "后台子任务回收：status 需配 spawnId；list 列当前会话全部子任务" },
                "spawnId": { "type": "string", "description": "op=status 时指定子任务" }
            },
            "required": []
        }));
        push(ToolName::Todo, "维护会话任务清单（长任务自组织）：op=write 一次性全量重写整个清单（不是追加——每次都写全所有条目与最新状态）；清单会注入后续每轮上下文，让你随时掌握进度全景。任务开始时建清单、每完成一项立即更新状态、全部完成后收尾", serde_json::json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["write", "list"], "description": "默认 write" },
                "items": { "type": "array", "items": { "type": "object", "properties": { "content": { "type": "string" }, "status": { "type": "string", "enum": ["pending", "in_progress", "done"] } }, "required": ["content"] }, "description": "全量条目（write 时必填；空数组 = 清空清单）" }
            },
            "required": []
        }));
        push(ToolName::ContextCompact, "主动压缩会话上下文：把最近窗口之外的更早历史压成滚动摘要（原文保留在会话记录中，仅上下文视图替换）。何时调用由你判断：上下文已冗长、早期细节不再关键、即将进入需要大量新鲜上下文的长任务阶段。压缩是增量的，可多次调用", serde_json::json!({
            "type": "object",
            "properties": {
                "keepRecent": { "type": "integer", "description": "保留最近 N 条原文不压缩（默认 12，范围 2-100）" },
                "note": { "type": "string", "description": "本次压缩时机判断的简要说明（为什么现在压、想保住什么）" }
            },
            "required": []
        }));
        specs
    }

    /// 同 [`Self::tool_specs`]，额外按记忆模式收敛 schema：文件记忆模式下
    /// memory_write / memory_link 必然失败，不下发（与 web_search/browser 的
    /// 「后端未就绪不承诺」同一口径）；memory_read 保留——读 memory.md 仍有意义。
    /// 记忆模式门控下发：memory_deep=false（文件记忆模式）时记忆写类工具不下发
    pub fn tool_specs_gated(
        allowlist: &[ToolName],
        search_ready: bool,
        browser_ready: bool,
        computer_ready: bool,
        memory_deep: bool,
    ) -> Vec<crate::provider::ToolSpec> {
        let mut specs = Self::tool_specs(allowlist, search_ready, browser_ready, computer_ready);
        if !memory_deep {
            let drop: Vec<&str> = [ToolName::MemoryWrite, ToolName::MemoryLink]
                .iter()
                .map(|t| t.key())
                .collect();
            specs.retain(|s| !drop.contains(&s.name.as_str()));
        }
        specs
    }

    /// 实验性开关门控：dynamic_compaction=false 时 context_compact 不下发
    /// （不开实验开关就不给模型这个能力面，避免误用未稳定行为）
    pub fn tool_specs_experiments(
        specs: Vec<crate::provider::ToolSpec>,
        dynamic_compaction: bool,
    ) -> Vec<crate::provider::ToolSpec> {
        if dynamic_compaction {
            specs
        } else {
            specs
                .into_iter()
                .filter(|s| s.name != ToolName::ContextCompact.key())
                .collect()
        }
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
        self.emit_and_persist(CoreEvent {
            kind: "tool.call".into(),
            session_id: session_id.to_string(),
            payload: serde_json::json!({
                "callId": call_id, "agentId": agent_id, "tool": name, "args": args,
                "plugin": plugin_attr,
            }),
        });
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
                    let _ = self.store.audit_tool(
                        agent_id,
                        name,
                        args,
                        "preTool 钩子拦截",
                        started.elapsed().as_millis() as u64,
                    );
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
        let _ = self.store.audit_tool(
            agent_id,
            name,
            args,
            &summary,
            started.elapsed().as_millis() as u64,
        );
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
            ToolName::Todo => self.tool_todo(session_id, args),
            ToolName::ContextCompact => unreachable!("context_compact 在 execute_named 预分发"),
        }
    }

    /// 审批闸门是否开启（risky/always）——白名单外命令转人工审批；off 档直接拒绝
    pub fn is_approval_gated(&self) -> bool {
        matches!(self.security.exec_approval.as_str(), "risky" | "always")
    }
}
