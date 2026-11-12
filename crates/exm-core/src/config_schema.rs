//! config_schema.rs —— 配置 schema 声明（codebase-refactor 6.3 拆分）：
//! `config_schema()` 字段描述：CLI 安装向导与 WebUI 设置页共用，
//! 新增配置项零 UI 改动。纯搬移：签名与实现零改动。

use crate::config::CONFIG_VERSION;

/// 字段描述：CLI 安装向导与 WebUI 设置页共用，新增配置项零 UI 改动
pub fn config_schema() -> serde_json::Value {
    serde_json::json!({
        "configVersion": CONFIG_VERSION,
        "groups": [
            {
                "key": "llm",
                "label": "模型接入",
                "fields": [
                    { "key": "llm.baseUrl", "label": "LLM Base URL（OpenAI 兼容）", "kind": "string",
                      "default": "https://api.openai.com/v1", "required": true,
                      "help": "支持官方 OpenAI、DeepSeek、通义、vLLM、Ollama 等任意兼容端点" },
                    { "key": "llm.apiKey", "label": "API Key", "kind": "password", "default": "",
                      "required": false, "help": "未配置时不接受推理请求，请在「提供商」页或此填写" },
                    { "key": "llm.model", "label": "默认模型", "kind": "string",
                      "default": "", "required": true, "help": "该端点的模型名；个体与组可各自覆盖" }
                ]
            },
            {
                "key": "runtime",
                "label": "运行时",
                "fields": [
                    { "key": "maxConcurrency", "label": "子个体并发数", "kind": "number",
                      "default": "4", "required": true, "min": 1, "max": 32,
                      "help": "调度器同时执行的最大子个体数量" },
                    { "key": "maxSessionTokens", "label": "会话 token 预算", "kind": "number",
                      "default": "0", "required": false, "min": 0, "max": 100000000,
                      "help": "估算口径（字符/4）；超出后该会话拒绝新轮次；0 = 不限" },
                    { "key": "compaction.triggerRatio", "label": "上下文压缩触发占比", "kind": "number",
                      "default": "0.6", "required": false, "min": 0, "max": 0.95,
                      "help": "会话历史估算 token 超过 模型窗口 × 上下文预算占比 × 该占比 即触发滚动压缩（早于预算硬顶截断介入）；0 = 关闭占比判据，仅按条数阈值（40 条）兜底" }
                ]
            },
            {
                "key": "memory",
                "label": "记忆系统",
                "fields": [
                    { "key": "memory.enabled", "label": "深层记忆（数据库检索 + 自动写入）", "kind": "boolean",
                      "default": "true", "required": false,
                      "help": "关闭后仅使用 memory.md 文件记忆（文件记忆模式）：规划时注入该文件内容，可在记忆面板直接编辑；不写数据库、不做语义检索" },
                    { "key": "memory.mdMaxChars", "label": "memory.md 字数上限", "kind": "number",
                      "default": "5000", "required": false, "min": 500, "max": 100000,
                      "help": "超过上限触发 AI 自主压缩简略；被精简的原文自动归档（深层开 = 存数据库，关 = 存工作区归档文件）" },
                    { "key": "memory.recallLimit", "label": "单轮召回条数", "kind": "number",
                      "default": "5", "required": false, "min": 0, "max": 20,
                      "help": "注入指挥体上下文的历史记忆条数" },
                    { "key": "memory.halfLifeDays", "label": "记忆衰减半衰期（天）", "kind": "number",
                      "default": "30", "required": false, "min": 1, "max": 365,
                      "help": "未被使用的记忆重要性按该半衰期衰减" }
                ]
            },
            {
                "key": "security",
                "label": "安全与审批",
                "fields": [
                    { "key": "security.execApproval", "label": "终端命令审批", "kind": "string",                      "default": "off", "required": false,
                      "help": "off=不拦截；risky=拦截高危命令（删除/强推/关机等）；always=全部终端命令需审批。白名单前缀可跳过" },
                    { "key": "security.execAllowlist", "label": "审批白名单前缀", "kind": "string",
                      "default": "", "required": false,
                      "help": "逗号分隔的命令前缀，命中即免审批，如：git status,cargo test" },
                    { "key": "security.sanitizeRules.fakeSystemPrompt", "label": "消毒：伪装系统指令", "kind": "boolean",
                      "default": "true", "required": false,
                      "help": "入站消息行首「SYSTEM:/【系统】」等伪装权威角色的文本改写为引用形式（语义保留、权威结构消除）" },
                    { "key": "security.sanitizeRules.hijackDirective", "label": "消毒：指令劫持短语", "kind": "boolean",
                      "default": "true", "required": false,
                      "help": "「ignore previous instructions/忽略之前所有指令」等劫持短语标记为用户引用" },
                    { "key": "security.sanitizeRules.fakeToolResult", "label": "消毒：工具结果伪装", "kind": "boolean",
                      "default": "true", "required": false,
                      "help": "「[tool_result]/【工具返回】」等伪装工具调用结构的标记中性化" },
                    { "key": "security.sanitizeRules.hiddenUnicode", "label": "消毒：隐藏字符剥离", "kind": "boolean",
                      "default": "true", "required": false,
                      "help": "零宽/双向覆写/不可见格式字符剥离（隐藏指令载体；建议常开）" },
                    { "key": "security.authKey", "label": "后台访问密钥", "kind": "password",
                      "default": "", "required": false,
                      "help": "非空时 WebUI 与全部 /api 请求需鉴权（浏览器出现登录门；程序调用带 X-Auth-Key 头）；留空 = 免鉴权。掩码表示沿用已配置密钥，清空保存 = 关闭鉴权" },
                    { "key": "security.terminalTimeoutSecs", "label": "终端命令超时（秒）", "kind": "number",
                      "default": "120", "required": false, "min": 10, "max": 1800,
                      "help": "前台命令的最长执行时间；超时强杀进程树。长任务请让 AI 用后台模式（不受此限）" },
                    { "key": "security.toolOutputSpillChars", "label": "工具结果落盘阈值（字符）", "kind": "number",
                      "default": "12000", "required": false, "min": 1000, "max": 1000000,
                      "help": "结果超过该长度即全文落盘（.exmachina/tool-output/），回填摘要与文件路径供按需回读；0 = 仅截断" },
                    { "key": "security.approvalWaitSecs", "label": "审批等待（秒）", "kind": "number",
                      "default": "120", "required": false, "min": 0, "max": 3600,
                      "help": "命令被审批拦截后，运行时原地等待人工决定的最长时长：期间在聊天里批准 → 执行结果直接回灌模型继续干活；超时按受阻回流。0 = 不等待（直接受阻）" },
                    { "key": "security.execUser", "label": "执行身份约束（Linux）", "kind": "string",
                      "default": "", "required": false,
                      "help": "（实验性 agent 身份约束）终端子进程降权到此用户（用户名或数字 uid；留空 = 不约束）。开启后命令以该身份执行、工作目录圈禁在其主目录；root 操作（sudo/su 等）自动转人工审批" },
                    { "key": "security.execHome", "label": "圈禁根目录", "kind": "string",
                      "default": "", "required": false,
                      "help": "（配合执行身份约束）子进程工作目录与 HOME 边界，须绝对路径；留空 = 自动推导（/home/<用户名> 或 passwd 记录）" }
                ]
            },
            {
                "key": "computerUse",
                "label": "Computer Use",
                "fields": [
                    { "key": "computer.enabled", "label": "启用 Computer Use", "kind": "boolean",
                      "default": "false", "required": false, "primary": true,
                      "help": "开启后个体获得 computer 工具：截屏 + 鼠标 / 键盘控制本机桌面。默认关闭——这是把整机输入权交给模型的显式决定" },
                    { "key": "computer.approval", "label": "输入动作审批", "kind": "boolean",
                      "default": "true", "required": false,
                      "showIf": { "field": "computer.enabled", "equals": true },
                      "help": "点击 / 键入 / 滚动 / 拖拽 / 热键一律先过审批闸门（截屏只读放行）；关闭后模型可直接操作桌面" },
                    { "key": "computer.maxEdge", "label": "截图最长边（像素）", "kind": "number",
                      "default": "1568", "required": false, "min": 0, "max": 4096,
                      "showIf": { "field": "computer.enabled", "equals": true },
                      "help": "截图回灌视觉模型前等比缩小到该边长以内，控制 token 消耗；0 = 原图不缩放" },
                    { "key": "computer.actionIntervalMs", "label": "动作最小间隔（毫秒）", "kind": "number",
                      "default": "120", "required": false, "min": 0, "max": 5000,
                      "showIf": { "field": "computer.enabled", "equals": true },
                      "help": "两次输入动作之间的强制间隔，防失控连点" }
                ]
            },
            {
                "key": "automation",
                "label": "自动化与心跳",
                "fields": [
                    { "key": "automation.heartbeatEnabled", "label": "启用心跳巡检", "kind": "boolean",
                      "default": "false", "required": false, "primary": true,
                      "help": "网关调度器按周期向心跳会话发送巡检提示（需 exm gateway 常驻）" },
                    { "key": "automation.heartbeatIntervalMinutes", "label": "心跳间隔（分钟）", "kind": "number",
                      "default": "30", "required": false, "min": 5, "max": 1440,
                      "showIf": { "field": "automation.heartbeatEnabled", "equals": true },
                      "help": "心跳巡检的执行周期" },
                    { "key": "automation.autoAdapt", "label": "自动经验优化", "kind": "boolean",
                      "default": "true", "required": false,
                      "help": "子个体新教训累计达 3 条时自动提炼经验改进要点并注入其派发（不创建个体）" },
                    { "key": "automation.heartbeatPrompt", "label": "心跳提示词", "kind": "string",
                      "default": "系统心跳巡检：检查未决任务、受阻节点与风险账，无事项则简短报告正常。", "required": false,
                      "showIf": { "field": "automation.heartbeatEnabled", "equals": true },
                      "help": "每次心跳注入给指挥体的提示词" },
                    { "key": "automation.heartbeatAgent", "label": "心跳目标个体", "kind": "string",
                      "default": "", "required": false,
                      "showIf": { "field": "automation.heartbeatEnabled", "equals": true },
                      "help": "个体 identifier（per-agent 粒度）：以其所属组身份巡检，产出进其组记忆；留空 = 全局巡检（激活组）" },
                    { "key": "automation.unitMaxSteps", "label": "子个体单次派发步数上限", "kind": "number",
                      "default": "16", "required": false, "min": 3, "max": 60,
                      "help": "一次派发内允许的「思考-调工具」轮次；复杂任务（多文件改动）需要更大的步数预算" },
                    { "key": "automation.timezone", "label": "cron 调度时区", "kind": "string",
                      "default": "", "required": false,
                      "help": "定时任务的 cron 表达式按该 IANA 时区（如 Asia/Shanghai）的墙上时钟解释——「每天 8 点」在配置时区的 8 点触发；留空 = UTC" }
                ]
            },
            {
                "key": "search",
                "label": "联网搜索",
                "fields": [
                    { "key": "search.provider", "label": "搜索后端", "kind": "string",
                      "default": "", "required": false, "primary": true,
                      "help": "tavily | brave | searxng | custom；留空 = 未配置 —— 此时 web_search 工具不下发给模型（不承诺不存在的能力）" },
                    { "key": "search.endpoint", "label": "搜索端点", "kind": "string",
                      "default": "", "required": false,
                      "showIf": { "field": "search.provider", "in": ["searxng", "custom"] },
                      "help": "searxng 填实例地址（如 https://searx.example.com）；custom 填你自己的 JSON 搜索接口" },
                    { "key": "search.apiKey", "label": "搜索 API Key", "kind": "password",
                      "default": "", "required": false,
                      "showIf": { "field": "search.provider", "in": ["tavily", "brave", "custom"] },
                      "help": "tavily / brave 需要；searxng 与自建通常留空" },
                    { "key": "search.maxResults", "label": "返回条数上限", "kind": "number",
                      "default": "5", "required": false, "min": 1, "max": 20,
                      "help": "单次搜索注入上下文的结果条数" },
                    { "key": "search.contentFetchN", "label": "正文抓取条数", "kind": "number",
                      "default": "3", "required": false, "min": 0, "max": 5,
                      "help": "includeContent 开启时对排名前 N 条并发抓取正文关键段（0 = 关闭抓取）；多后端聚合经 search.backends 配置（JSON 数组形态，REST /api/config 透传）" },
                    { "key": "search.proactive", "label": "主动搜索纪律", "kind": "boolean",
                      "default": "true", "required": false,
                      "help": "开启后系统提示注入「何时该主动搜」行为准则（时效/核验/未知概念先搜再答）；关闭 = 模型仅被动响应用户的搜索请求" }
                ]
            },
            {
                "key": "sandbox",
                "label": "沙箱执行",
                "fields": [
                    { "key": "sandbox.mode", "label": "沙箱级别", "kind": "string",
                      "default": "workspace", "required": false, "primary": true,
                      "help": "off=仅白名单+审批；workspace=子进程环境净化（敏感环境变量不外泄，HOME/TMP 重定向到 .exmachina/sandbox）；strict=净化 + 联网类命令走审批 + Linux/macOS 检测到 bubblewrap 时做系统级隔离" },
                    { "key": "sandbox.allowNetwork", "label": "strict 下允许联网安装", "kind": "boolean",
                      "default": "false", "required": false,
                      "showIf": { "field": "sandbox.mode", "in": ["strict"] },
                      "help": "开启后 pip/npm install、curl/wget 等在 strict 模式无需审批" },
                    { "key": "sandbox.useBwrap", "label": "使用 bubblewrap（Linux/macOS）", "kind": "boolean",
                      "default": "true", "required": false,
                      "showIf": { "field": "sandbox.mode", "in": ["strict"] },
                      "help": "检测到 bwrap 时用其做文件系统隔离：根只读、工作区可写、按需断网" },
                    { "key": "sandbox.memoryMb", "label": "子进程内存上限（MB）", "kind": "number",
                      "default": "0", "required": false, "min": 0, "max": 1048576,
                      "showIf": { "field": "sandbox.mode", "in": ["workspace", "strict"] },
                      "help": "Linux 经 ulimit、Windows 经作业对象生效；0 = 不限" },
                    { "key": "sandbox.maxProcesses", "label": "子进程数上限（Windows）", "kind": "number",
                      "default": "0", "required": false, "min": 0, "max": 4096,
                      "showIf": { "field": "sandbox.mode", "in": ["workspace", "strict"] },
                      "help": "Windows 作业对象的活动进程上限，防 fork 炸弹；0 = 不限" }
                ]
            },
            {
                "key": "browser",
                "label": "浏览器自动化",
                "fields": [
                    { "key": "browser.executable", "label": "浏览器可执行文件", "kind": "string",
                      "default": "", "required": false,
                      "help": "留空自动探测 Chrome/Chromium/Edge 常见安装位置（含 Playwright 缓存）" },
                    { "key": "browser.headless", "label": "无头模式", "kind": "boolean",
                      "default": "true", "required": false,
                      "help": "默认无头；调试时可关掉看真实窗口" },
                    { "key": "browser.timeoutSecs", "label": "单次操作超时（秒）", "kind": "number",
                      "default": "30", "required": false, "min": 5, "max": 300,
                      "help": "导航与脚本执行的等待上限" },
                    { "key": "browser.maxChars", "label": "正文提取上限（字符）", "kind": "number",
                      "default": "8000", "required": false, "min": 500, "max": 100000,
                      "help": "browser text 单次返回的正文长度" },
                    { "key": "browser.searchEngine", "label": "浏览器搜索引擎", "kind": "string",
                      "default": "bing", "required": false,
                      "help": "browser op=search 用的引擎：bing | duckduckgo | baidu。无需 API Key——直接导航搜索页取渲染结果；与联网搜索后端（tavily 等）互补" }
                ]
            },
            {
                "key": "tools",
                "label": "自定义工具",
                "fields": [
                    { "key": "tools.custom", "label": "声明式工具清单（JSON 数组）", "kind": "string",
                      "default": "", "required": false,
                      "help": "把 HTTP 接口或命令模板包成模型可调用的工具，免写 MCP 服务器。JSON 数组，元素字段：name（字母开头）、description、kind（http|shell）、target（{参数名} 占位）、method（GET|POST，默认 POST）、headers、parameters（JSON Schema）、agents（可见个体，空=全部）。例：[{\"name\":\"weather\",\"description\":\"查天气\",\"kind\":\"http\",\"method\":\"GET\",\"target\":\"https://api.example.com/now?city={city}\",\"parameters\":{\"type\":\"object\",\"properties\":{\"city\":{\"type\":\"string\"}},\"required\":[\"city\"]}}]" }
                ]
            },
            {
                "key": "trash",
                "label": "回收站",
                "fields": [
                    { "key": "trash.retentionDays", "label": "保留期（天）", "kind": "number",
                      "default": "7", "required": false, "min": 1, "max": 365,
                      "help": "文件删除进回收站后的保留天数；超期目录在每小时巡检时自动清理" },
                    { "key": "trash.maxBytes", "label": "容量上限（字节）", "kind": "number",
                      "default": "536870912", "required": false, "min": 10485760, "max": 1073741824,
                      "help": "回收站总大小上限（默认 512MB）；超限按最旧日期目录优先淘汰" }
                ]
            },
            {
                "key": "hooks",
                "label": "生命周期钩子",
                "fields": [
                    { "key": "hooks.preTool", "label": "工具调用前钩子", "kind": "string",
                      "default": "", "required": false,
                      "help": "逗号分隔的命令（每行一条亦可）；环境变量 EXM_TOOL / EXM_AGENT / EXM_ARGS / EXM_WORKSPACE 可用。**非零退出 = 拦截该次工具调用**并回报原因" },
                    { "key": "hooks.postTool", "label": "工具调用后钩子", "kind": "string",
                      "default": "", "required": false,
                      "help": "同上；额外有 EXM_TOOL_OK（true/false）。用于留痕、通知、指标采集" },
                    { "key": "hooks.onRunEnd", "label": "运行收束钩子", "kind": "string",
                      "default": "", "required": false,
                      "help": "一轮 run 结束（成功或失败）时触发；EXM_RUN_STATUS 给出终态" }
                ]
            },
            {
                "key": "identity",
                "label": "通道身份",
                "fields": [
                    { "key": "identity.identityRequired", "label": "通道身份管控", "kind": "boolean",
                      "default": "false", "required": false, "primary": true,
                      "help": "开启后白名单内用户也须配对方可执行（未绑定仅能收到配对指引）；关闭 = 维持白名单即全权" },
                    { "key": "identity.pairingTtlSecs", "label": "配对码有效期（秒）", "kind": "number",
                      "default": "600", "required": false, "min": 60, "max": 86400,
                      "showIf": { "field": "identity.identityRequired", "equals": true },
                      "help": "控制台签发的配对码有效时长；单次有效" },
                    { "key": "identity.pairingMaxFailures", "label": "配对码防爆破阈值（次）", "kind": "number",
                      "default": "5", "required": false, "min": 0, "max": 100,
                      "showIf": { "field": "identity.identityRequired", "equals": true },
                      "help": "锁定窗口内连续失败达该次数即锁定（0 = 关闭防爆破）；锁定期内正确码也不受理" },
                    { "key": "identity.pairingLockoutSecs", "label": "配对码锁定时长（秒）", "kind": "number",
                      "default": "900", "required": false, "min": 60, "max": 86400,
                      "showIf": { "field": "identity.identityRequired", "equals": true },
                      "help": "防爆破锁定窗口时长（默认 15 分钟）；锁定有审计事件" },
                    { "key": "identity.groupGateDefault", "label": "新通道群聊唤醒门控默认值", "kind": "boolean",
                      "default": "true", "required": false,
                      "help": "开启 = 群内仅 @提及 / 回复智能体时触发（私聊不受限）；逐通道可单独覆盖" }
                ]
            },
            {
                "key": "limits",
                "label": "用量限制",
                "fields": [
                    { "key": "limits.enabled", "label": "启用限流与配额", "kind": "boolean",
                      "default": "false", "required": false, "primary": true,
                      "help": "按主体（通道账号 / 绑定用户 / API key）独立计数；仅作用于外部入口，组内派发不受限" },
                    { "key": "limits.windowSecs", "label": "限流窗口（秒）", "kind": "number",
                      "default": "60", "required": false, "min": 5, "max": 3600,
                      "showIf": { "field": "limits.enabled", "equals": true },
                      "help": "滑动窗口时长" },
                    { "key": "limits.maxRequests", "label": "窗口内最大请求数", "kind": "number",
                      "default": "0", "required": false, "min": 0, "max": 100000,
                      "showIf": { "field": "limits.enabled", "equals": true },
                      "help": "单主体在窗口内的请求上限；0 = 不限" },
                    { "key": "limits.quotaPeriod", "label": "配额周期", "kind": "string",
                      "default": "daily", "required": false,
                      "showIf": { "field": "limits.enabled", "equals": true },
                      "help": "daily | weekly | monthly" },
                    { "key": "limits.quotaTokens", "label": "周期 token 配额", "kind": "number",
                      "default": "0", "required": false, "min": 0, "max": 1000000000,
                      "showIf": { "field": "limits.enabled", "equals": true },
                      "help": "单主体周期内 token 用量上限（口径同 token 台账）；0 = 不限" },
                    { "key": "limits.quotaRequests", "label": "周期请求数配额", "kind": "number",
                      "default": "0", "required": false, "min": 0, "max": 1000000,
                      "showIf": { "field": "limits.enabled", "equals": true },
                      "help": "单主体周期内请求次数上限；0 = 不限" },
                    { "key": "limits.quotaRunningPolicy", "label": "配额耗尽时进行中任务", "kind": "string",
                      "default": "finish", "required": false,
                      "showIf": { "field": "limits.enabled", "equals": true },
                      "help": "finish = 完成当前再停；interrupt = 立即中断" }
                ]
            },
            {
                "key": "triggers",
                "label": "事件触发器",
                "fields": [
                    { "key": "triggers.fileWatch.enabled", "label": "启用文件监听", "kind": "boolean",
                      "default": "false", "required": false, "primary": true,
                      "help": "工作区内匹配路径发生创建/修改/删除时注入会话事件" },
                    { "key": "triggers.fileWatch.debounceMs", "label": "去抖窗口（毫秒）", "kind": "number",
                      "default": "1000", "required": false, "min": 100, "max": 30000,
                      "showIf": { "field": "triggers.fileWatch.enabled", "equals": true },
                      "help": "窗口内同路径连续变更合并为一次注入" },
                    { "key": "triggers.fileWatch.exclude", "label": "排除路径前缀", "kind": "string",
                      "default": ".exmachina/,target/,node_modules/", "required": false,
                      "showIf": { "field": "triggers.fileWatch.enabled", "equals": true },
                      "help": "逗号分隔；命中前缀的路径不监听" },
                    { "key": "triggers.eventWebhook.enabled", "label": "启用事件 webhook", "kind": "boolean",
                      "default": "false", "required": false, "primary": true,
                      "help": "通用事件入口（独立于通道 webhook 的路径与密钥）" },
                    { "key": "triggers.eventWebhook.secret", "label": "事件 webhook 密钥", "kind": "password",
                      "default": "", "required": false,
                      "showIf": { "field": "triggers.eventWebhook.enabled", "equals": true },
                      "help": "HMAC 签名密钥；空 = 入口关闭" }
                ]
            },
            {
                "key": "mcpServe",
                "label": "MCP 服务端",
                "fields": [
                    { "key": "mcpServe.enabled", "label": "启用工具对外暴露", "kind": "boolean",
                      "default": "false", "required": false, "primary": true,
                      "help": "把启用的内置工具以 MCP 协议暴露给外部客户端（stdio / HTTP）；调用与内部执行同过审批、沙箱、审计、限流" },
                    { "key": "mcpServe.httpPath", "label": "HTTP 挂载路径", "kind": "string",
                      "default": "/mcp", "required": false,
                      "showIf": { "field": "mcpServe.enabled", "equals": true },
                      "help": "网关侧 MCP 端点路径；stdio 模式不受此限" },
                    { "key": "mcpServe.allowedGroups", "label": "允许的组", "kind": "string",
                      "default": "", "required": false,
                      "showIf": { "field": "mcpServe.enabled", "equals": true },
                      "help": "逗号分隔的组 ID；空 = 全部组" },
                    { "key": "mcpServe.allowedTools", "label": "允许的工具", "kind": "string",
                      "default": "", "required": false,
                      "showIf": { "field": "mcpServe.enabled", "equals": true },
                      "help": "逗号分隔的工具名；空 = 全部启用工具" }
                ]
            },
            {
                "key": "experiments",
                "label": "实验性功能",
                "fields": [
                    { "key": "experiments.dynamicCompaction", "label": "动态上下文压缩", "kind": "boolean",
                      "default": "false", "required": false,
                      "help": "（实验性）模型获得 context_compact 工具，自行判断压缩时机：上下文冗长、早期细节不再关键时主动把旧历史压成滚动摘要。关闭时按条数阈值自动压缩" },
                    { "key": "experiments.groupSharedContext", "label": "智能体组共享上下文", "kind": "boolean",
                      "default": "false", "required": false,
                      "help": "（实验性）派发子个体时自动携带组主会话的滚动摘要与最近对话窗口，子个体不再只有 objective 孤岛视角；会放大派发 token 开销" },
                    { "key": "experiments.slidingContextWindow", "label": "滑动上下文窗口", "kind": "boolean",
                      "default": "false", "required": false,
                      "help": "（实验性）关闭条数阈值触发的定时压缩：模型持续持有最近对话的完整原文，窗口外历史不再新增压缩调用（已压过的沿用旧摘要）。与动态上下文压缩互斥" },
                    { "key": "experiments.llmDrivenMemory", "label": "模型自主记忆（LLM Driven Memory）", "kind": "boolean",
                      "default": "false", "required": false,
                      "help": "（实验性）关闭程序每轮自动写入的会话摘要/决策/证据/教训，由模型经 memory_write 工具自主决定哪些内容值得长期记忆；治理记忆库被无关内容灌满" },
                    { "key": "experiments.autoMemoryDigest", "label": "长会话记忆归纳（每日低峰摘要）", "kind": "boolean",
                      "default": "false", "required": false,
                      "help": "（实验性）低峰期（每日 04:30）把长会话历史归纳为记忆条目：LLM 单次摘要 + 内容哈希去重 + token 预算钳制；治理「会话信息随窗口滑走而丢失」" },
                    { "key": "automation.memoryDigestMinTurns", "label": "长会话记忆归纳触发轮数下限", "kind": "number",
                      "default": "20", "required": false, "min": 5, "max": 500,
                      "showIf": { "field": "experiments.autoMemoryDigest", "equals": true },
                      "help": "会话消息数 ≥ 该值才进入低峰归纳；过短会话不值得摘要" },
                    { "key": "experiments.superAgent", "label": "Super Agent 深度委托", "kind": "boolean",
                      "default": "false", "required": false,
                      "help": "（实验性）新会话进入深度委托模式：agent 静默长链路推进（工具循环+并行子代理），只在需要决策/交付结果/无法继续时产出一条消息——尽可能少占注意力，尽可能多交付价值" },
                    { "key": "experiments.autoAgent", "label": "Auto Agent 完全自主", "kind": "boolean",
                      "default": "false", "required": false,
                      "help": "（实验性）无人值守：agent 按周期自评「有没有值得做的事」，有则自主开会话执行，只在有人必须行动时经通知通道说话。配套参数见下方" },
                    { "key": "automation.autoAgentIntervalMinutes", "label": "Auto Agent 自评周期（分钟）", "kind": "number",
                      "default": "30", "required": false, "min": 5, "max": 1440,
                      "showIf": { "field": "experiments.autoAgent", "equals": true },
                      "help": "agent 每周期自评一次「有没有值得做的事」；不是每次都执行任务" },
                    { "key": "automation.autoAgentMaxSteps", "label": "Auto Agent 单次行动步数预算", "kind": "number",
                      "default": "24", "required": false, "min": 3, "max": 120,
                      "showIf": { "field": "experiments.autoAgent", "equals": true },
                      "help": "自主行动单次的工具步数上限（防失控）；0 = 沿用子个体派发步数上限" },
                    { "key": "automation.autoAgentBackoffMax", "label": "Auto Agent 空转退避上限（倍）", "kind": "number",
                      "default": "8", "required": false, "min": 1, "max": 30,
                      "showIf": { "field": "experiments.autoAgent", "equals": true },
                      "help": "连续「无事可做」次数 N 后自评间隔 × min(N, 上限)；有任务即复位（防空转耗 token）" },
                    { "key": "experiments.metaAgent", "label": "Meta Agent 元智能体", "kind": "boolean",
                      "default": "false", "required": false,
                      "help": "（实验性）元智能体：周期巡检运行数据，对高失败率/重复失败/能力缺口提出改进提议（提示词补段/经验要点/成员编成/新个体构建），全部落 [meta] 审批单人工批准后应用；另含轮内元认知反思（连续失败时注入策略调整提示）" },
                    { "key": "automation.metaIntervalMin", "label": "Meta Agent 巡检周期（分钟）", "kind": "number",
                      "default": "60", "required": false, "min": 5, "max": 1440,
                      "showIf": { "field": "experiments.metaAgent", "equals": true },
                      "help": "元智能体每次巡检的间隔；巡检 = 数据聚合 → 自评 → 有改进点才开会话落提议单" }
                ]
            },
            {
                "key": "notify",
                "label": "结果推送",
                "fields": [
                    { "key": "notify.enabled", "label": "启用定时任务结果推送", "kind": "boolean",
                      "default": "true", "required": false, "primary": true,
                      "help": "总开关；逐任务仍须在任务上配置订阅通道，关闭 = 全部不推" },
                    { "key": "notify.maxChars", "label": "推送摘要长度上限（字符）", "kind": "number",
                      "default": "500", "required": false, "min": 100, "max": 8000,
                      "showIf": { "field": "notify.enabled", "equals": true },
                      "help": "推送至通道的结果摘要截断长度" }
                ]
            }
        ]
    })
}
