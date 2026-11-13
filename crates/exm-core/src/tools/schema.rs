//! tools/schema.rs —— 工具 schema 声明域（codebase-refactor 6.2 拆分）：
//! 原生 function calling 的工具 schema 生成（tool_specs：按白名单 + 能力就绪
//! 门控）、记忆档裁剪（tool_specs_gated）、实验开关门控（tool_specs_experiments）。
//! 纯搬移：方法签名与实现零改动。

use super::gateway::ToolGateway;
use super::ToolName;

impl ToolGateway {
    /// 原生 function calling：按白名单生成工具 schema（docs/协议与契约.md）。
    /// `search_ready` / `browser_ready` 决定联网搜索与浏览器工具是否下发——
    /// 后端能力未就绪时不承诺该工具（避免模型调用必然失败的工具）。
    pub fn tool_specs(
        allowlist: &[ToolName],
        search_ready: bool,
        browser_ready: bool,
        computer_ready: bool,
        speech_ready: bool,
        image_gen_ready: bool,
        chart_ready: bool,
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
            if name == ToolName::Speech && !speech_ready {
                return; // TTS 未装配（无能力模型/未回接）：不下发，避免承诺不存在的能力
            }
            if name == ToolName::ImageGen && !image_gen_ready {
                return; // 生图槽位未配置：不下发（不承诺不存在的能力）
            }
            if name == ToolName::RenderChart && !chart_ready {
                return; // chart.endpoint 空 = 关闭：不下发
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
            "文件系统操作：write（整文件写入/新建）、mkdir、list（条目清单）、delete（移入回收站可恢复，不裸删）、rename（重命名/移动，不覆盖已存在目标）、send_file（把工作区内文件发送给用户——任意类型含二进制，本轮收束时经通道直发；要给用户交付文件时优先用这个。禁止在回复文本里编造下载链接或 sandbox:/file:// 之类的伪 URL——交付文件只走 send_file，链接由系统按通道能力生成）",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "op": { "type": "string", "enum": ["write", "mkdir", "list", "delete", "rename", "send_file"] },
                    "path": { "type": "string", "description": "相对工作区的路径" },
                    "content": { "type": "string", "description": "op=write 时的文件内容" },
                    "newPath": { "type": "string", "description": "op=rename 时的目标路径（相对工作区；目标存在则拒绝）" },
                    "caption": { "type": "string", "description": "op=send_file 时的文件说明（随文件一起展示，可选）" }
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
            "联网搜索并返回结果摘要（标题 + 链接 + 摘要）。主动使用：时效性问题（新闻/版本/价格）、待核验的具体事实（数字/日期/参数）、没把握的概念——先搜再答，不要凭训练记忆编造；搜索未果换关键词重试（最多 2 轮）后如实说明",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "maxResults": { "type": "integer", "description": "返回条数（默认取配置值）" },
                    "includeContent": { "type": "boolean", "description": "true = 对排名前几的结果并发抓取正文关键段（含查询词定位；较慢，仅在摘要不足以回答时开启）" }
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
                "cron": { "type": "string", "description": "五段 cron：分 时 日 月 周，如 0 9 * * *（与 at 二选一）。按本机配置时区解释；回显的下次执行时间即按该时区渲染" },
                "at": { "type": "string", "description": "一次性触发时间（ISO8601，如 2026-09-20T18:00:00+08:00）" },
                "notify_channels": { "type": "array", "items": { "type": "string" }, "description": "结果推送订阅的通道 id（如 qq:12345）；建任务时声明，完成后自动把结果摘要推到该通道" },
                "session_title": { "type": "string", "description": "会话标题（复用同名会话；缺省 job-<id>）" }
            },
            "required": ["op"]
        }));
        push(ToolName::Speech, "语音回复：把一段文本合成为语音条发给用户（走 TTS，随本轮回复送达）。用户说「用语音回我」「念出来」「发语音」时使用；文本应精炼（≤600 字符），正式内容仍以文字回复为主", serde_json::json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "要合成为语音的正文（≤600 字符）" }
            },
            "required": ["text"]
        }));
        push(ToolName::ImageGen, "生成一张图片并发给用户（文生图，随本轮回复送达）。用户明确想要图片时使用（插画/示意图/表情包风格化图等）；prompt 用英文效果通常更好、需完整描述画面主体与风格；生成耗时较长（最长 120 秒）不要连环重试", serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": { "type": "string", "description": "画面描述（主体、风格、构图；英文效果通常更好）" },
                "size": { "type": "string", "enum": ["1024x1024", "1024x1536", "1536x1024"], "description": "尺寸：方形/竖版/横版（默认 1024x1024）" }
            },
            "required": ["prompt"]
        }));
        push(ToolName::RenderChart, "把 mermaid 图表源码渲染成 SVG 图片（随本轮回复送达）。适合流程图/时序图/状态图等结构示意；mermaid 参数是完整图表源码（如 flowchart TD 开头，节点与连线语法见 mermaid 文档）；语法错误会带回服务端提示，修正后可重试", serde_json::json!({
            "type": "object",
            "properties": {
                "mermaid": { "type": "string", "description": "mermaid 图表源码（flowchart / sequenceDiagram / stateDiagram-v2 等，≤20000 字符）" }
            },
            "required": ["mermaid"]
        }));
        push(ToolName::KnowledgeWrite, "写入共享知识库：沉淀对全体智能体通用的客观知识（部署约束、平台特性、方法论、踩坑经验）。内容哈希去重——重复写入会提升既有条目热度而非新建。沉淀纪律：只存跨会话可复用的通用结论，不存会话私有细节、用户个人信息或一次性流水；与记忆的分工——记忆存本组经历，知识存实例级通用事实。正文超 8000 字符会被拒绝，超长内容请拆分为多条聚焦知识", serde_json::json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "一句话标题（概括这条知识）" },
                "body": { "type": "string", "description": "知识正文（精炼、可独立理解，≤8000 字符）" },
                "tags": { "type": "array", "items": { "type": "string" }, "description": "标签（可选，辅助检索）" }
            },
            "required": ["title", "body"]
        }));
        push(ToolName::KnowledgeRead, "检索共享知识库（全体智能体通用知识层，与记忆互补）：先查已有结论再探索能省大量重复工作。op=search 按关键词检索 top-k；op=get 按 id 取全文；op=list 列最近条目（含 hits 热度参考）。遇到部署/平台/环境类问题或可复用方法论时，先检索这里", serde_json::json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["search", "get", "list"], "description": "默认 search" },
                "query": { "type": "string", "description": "op=search：检索关键词" },
                "id": { "type": "string", "description": "op=get：条目 id" },
                "limit": { "type": "integer", "description": "返回条数（默认 5，上限 20）" }
            },
            "required": []
        }));
        push(ToolName::Browser, "浏览器自动化（Chrome + CDP）：搜索网页（无需搜索 API）、导航网页、取渲染后正文、执行 JS、截图。适合抓动态页面 / 走一遍网页流程 / 留存页面证据；搜索后端未配置时 op=search 是联网检索的主路径", serde_json::json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["open", "search", "text", "eval", "screenshot", "close"], "description": "默认 open" },
                "query": { "type": "string", "description": "op=search：搜索词（导航搜索引擎取结果，无需 API Key）" },
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
        push(ToolName::Sessions, "跨会话互通（同组范围内）：列示/检索同组会话、读取指定会话最近历史、向另一会话投递消息并触发它执行。用于延续另一条对话的上下文、把任务转交给绑定其他通道的会话、或汇总本组多会话进展。组隔离强制执行——跨组会话不可见、不可投递", serde_json::json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["list", "search", "history", "send"], "description": "动作：list 列会话 / search 检索 / history 读历史 / send 投递" },
                "query": { "type": "string", "description": "op=search：检索关键词" },
                "sessionId": { "type": "string", "description": "op=history/send：目标会话 id" },
                "limit": { "type": "integer", "description": "list/search/history 返回条数（默认 20，范围 1-100）" },
                "text": { "type": "string", "description": "op=send：投递文本（会作为目标会话的用户输入）" },
                "wait": { "type": "boolean", "description": "op=send：true = 等待目标会话跑完并返回结果；缺省 false = 投递后立即返回" }
            },
            "required": ["op"]
        }));
        push(ToolName::SkillManage, "技能包自管理：把可复用的流程沉淀为技能（SKILL.md 标准格式），或列示/更新/删除既有技能。技能在后续派发中按触发词自动注入相关个体。完成任务后若形成了可复用方法，应主动沉淀（id 稳定、触发词具体、指令可执行）；发现技能与实测不符时用 update 修正", serde_json::json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["list", "create", "update", "delete", "export", "import"], "description": "动作（缺省 list）" },
                "id": { "type": "string", "description": "技能 id（仅字母/数字/-/_）" },
                "name": { "type": "string", "description": "技能名" },
                "description": { "type": "string", "description": "一句话说明" },
                "triggers": { "type": "array", "items": { "type": "string" }, "description": "触发词：任务目标/节点标题包含任一词即注入" },
                "instructions": { "type": "string", "description": "作业指令正文（create/update 必填）：步骤、约束、验收口径" },
                "agents": { "type": "array", "items": { "type": "string" }, "description": "适用个体 identifier；空 = 全体" },
                "content": { "type": "string", "description": "import：SKILL.md 原文（含 frontmatter）" }
            },
            "required": ["op"]
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
        push(ToolName::MetaObserve, "（元智能体）读运行数据聚合：按 agent 维度的工具统计（次数/失败率/平均耗时/最近失败）与经验要点现状（7 天窗口）。改进提议前先观测,让依据落在数据上", serde_json::json!({
            "type": "object",
            "properties": {
                "agent": { "type": "string", "description": "目标个体 identifier（空 = 全体）" }
            },
            "required": []
        }));
        push(ToolName::MetaPropose, "（元智能体）提交结构化改进提议,落入人工审批（[meta] 前缀审批单）。批准后自动应用,拒绝则回流。kind=prompt（提示词补段）/adaptation（经验要点）/roster（成员增删调,≤5 op,两阶段校验）", serde_json::json!({
            "type": "object",
            "properties": {
                "kind": { "type": "string", "enum": ["prompt", "adaptation", "roster"] },
                "target": { "type": "string", "description": "目标个体 identifier" },
                "change": { "type": "object", "description": "prompt: {promptPatch}; adaptation: {content}; roster: {ops: [{op: add|remove|update, ...}]}" },
                "rationale": { "type": "string", "description": "提议理由（依据的数据与预期收益,展示给审批人）" }
            },
            "required": ["kind", "target", "change", "rationale"]
        }));
        push(ToolName::MetaBuild, "（元智能体）构建新智能体/组：定义草稿 + 提示词草稿落入 [meta] 审批单,批准后注册。工具本身不做生成——定义由你在参数里给出", serde_json::json!({
            "type": "object",
            "properties": {
                "description": { "type": "string", "description": "任务的自然语言描述（为何需要这个个体）" },
                "definition": { "type": "object", "description": "个体定义草稿: {name, domain, tier, description, tools: [...]}" },
                "prompt": { "type": "string", "description": "系统提示词草稿（≥16 字符）" },
                "asGroup": { "type": "boolean", "description": "true = 构建为组（默认 false = 个体）" }
            },
            "required": ["description", "definition", "prompt"]
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
        speech_ready: bool,
        image_gen_ready: bool,
        chart_ready: bool,
        memory_deep: bool,
    ) -> Vec<crate::provider::ToolSpec> {
        let mut specs = Self::tool_specs(
            allowlist,
            search_ready,
            browser_ready,
            computer_ready,
            speech_ready,
            image_gen_ready,
            chart_ready,
        );
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
        sliding_window: bool,
        meta_agent: bool,
    ) -> Vec<crate::provider::ToolSpec> {
        // 滑动窗口模式与动态压缩互斥：同为「何时压缩」的决策权归属，
        // 滑动窗口 = 交回程序窗口语义（不主动压）；动态压缩 = 交模型。
        // slidingContextWindow 开启时 context_compact 一律不下发。
        let mut specs = if dynamic_compaction && !sliding_window {
            specs
        } else {
            specs
                .into_iter()
                .filter(|s| s.name != ToolName::ContextCompact.key())
                .collect()
        };
        // meta 工具组（实验性 metaAgent）:关闭时从下发面剔除（allowlist 里有也不下发了
        // ——工具面承诺与执行一致性;开启时注入三件套,无需逐个编成授权）
        if !meta_agent {
            specs.retain(|s| {
                !matches!(
                    s.name.as_str(),
                    "meta_observe" | "meta_propose" | "meta_build"
                )
            });
        }
        specs
    }
}
