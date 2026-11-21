//! OpenAPI 契约（静态清单生成，agent-parity-quality-2 §2.3）。
//!
//! axum 0.7 无路由反射：本模块以「路由注册处为事实源」的**静态清单**描述端点，
//! 用对账测试（`tests` 模块）守护清单与 `build_router` 实际路由的偏差——
//! 幽灵端点（清单有路由无）与漏项（路由有清单无）在测试期即红，不靠人肉纪律。
//!
//! schema 精度三档（design A3）：
//! 1. 路径/方法/摘要/鉴权/错误族（必填——全部端点覆盖）
//! 2. 请求体 schema（新端点必填；旧端点渐进补）
//! 3. 响应 schema（informational 标记，不承诺稳定性）

use serde_json::{json, Value};

mod llm;
mod model_routing;

/// 单端点描述（清单条目）
pub struct Endpoint {
    /// 路径（axum 形态，:id 参数）
    pub path: &'static str,
    /// 小写 HTTP 方法
    pub method: &'static str,
    /// 一句话摘要
    pub summary: &'static str,
    /// 是否需要鉴权（全站 authKey 启用时；公共面端点为 false）
    pub auth: bool,
    /// 请求体 schema（档 2；None = 无请求体或未补）
    pub request: Option<Value>,
    /// 响应 schema（档 3 informational；None = 不承诺）
    pub response: Option<Value>,
}

impl Endpoint {
    /// 档 1 精度的快捷构造（绝大多数端点）
    const fn new(method: &'static str, path: &'static str, summary: &'static str) -> Self {
        Endpoint {
            method,
            path,
            summary,
            auth: true,
            request: None,
            response: None,
        }
    }

    /// 无鉴权端点（契约文档/首启鉴权入口）
    const fn public(method: &'static str, path: &'static str, summary: &'static str) -> Self {
        Endpoint {
            method,
            path,
            summary,
            auth: false,
            request: None,
            response: None,
        }
    }

    fn json_request(mut self, schema: &str, example: Value) -> Self {
        self.request = Some(json!({
            "required": true,
            "content": { "application/json": {
                "schema": { "$ref": format!("#/components/schemas/{schema}") },
                "example": example,
            } },
        }));
        self
    }

    fn json_response(mut self, schema: &str, example: Value) -> Self {
        self.response = Some(json!({ "application/json": {
            "schema": { "$ref": format!("#/components/schemas/{schema}") },
            "example": example,
        } }));
        self
    }

    /// operationId：路径转驼峰 + 方法（/api/sessions/:id get → ApiSessionsIdGet）
    fn operation_id(&self) -> String {
        let mut id = String::new();
        for seg in self.path.trim_matches('/').split(&['/', ':', '-', '_'][..]) {
            if seg.is_empty() {
                continue;
            }
            let mut chars = seg.chars();
            if let Some(c) = chars.next() {
                id.extend(c.to_uppercase());
                id.push_str(chars.as_str());
            }
        }
        let mut m = self.method.to_string();
        if let Some(c) = m.get_mut(0..1) {
            c.make_ascii_uppercase();
        }
        format!("{id}{m}")
    }
}

/// 全量端点清单（手工维护的事实源镜像；对账测试守护与 build_router 的一致性）。
///
/// 排除项（不计入对账）：`/ws` 与 `/worker`（WebSocket 升级面）、MCP HTTP 挂载
/// （配置态路径）、WebUI 静态托管（fallback_service）。
pub fn endpoints() -> Vec<Endpoint> {
    vec![
        // ---------------- OpenAPI 自身（公共面：契约文档无需鉴权即可发现）
        Endpoint::public("get", "/api/openapi.json", "OpenAPI 3.1 契约文档"),
        Endpoint::new(
            "get",
            "/api/commands",
            "按 HTTP 身份与作用域查询版本化命令目录",
        ),
        Endpoint::new("get", "/api/capabilities", "查询当前 HTTP 身份的能力快照"),
        // ---------------- 系统（authKey 启用时与其他 API 一致）
        Endpoint::new("get", "/api/health", "存活探测"),
        Endpoint::new("get", "/api/version", "版本信息"),
        // ---------------- 会话
        Endpoint::new("get", "/api/sessions", "会话列表（含末条预览）"),
        Endpoint::new("get", "/api/collaborations", "列出当前项目的协作群聊"),
        Endpoint::new("post", "/api/collaborations", "创建同实例、多项目协作群聊"),
        Endpoint::new(
            "get",
            "/api/collaborations/:id",
            "协作群聊记录、成员状态与待审批",
        ),
        Endpoint::new(
            "post",
            "/api/collaborations/:id/turns",
            "向明确选中的成员发送任务（支持 requestId 幂等重试）",
        ),
        Endpoint::new(
            "post",
            "/api/collaborations/:id/turns/:turn_id/stop",
            "停止指定协作轮次（可指定单个成员）",
        ),
        Endpoint::new("post", "/api/sessions", "新建会话"),
        Endpoint::new("get", "/api/sessions/search", "会话内容检索（标题+消息）"),
        Endpoint::new("post", "/api/sessions/batch", "会话批量操作"),
        Endpoint::new("post", "/api/sessions/flow-archive", "外部通道会话流归档"),
        Endpoint::new("get", "/api/sessions/:id", "会话详情"),
        Endpoint::new("delete", "/api/sessions/:id", "删除会话（连带派生数据）"),
        Endpoint::new("get", "/api/sessions/:id/messages", "消息列表"),
        Endpoint::new(
            "get",
            "/api/sessions/:id/events",
            "事件溯源（最近 limit 条）",
        ),
        Endpoint::new("get", "/api/sessions/:id/export", "导出会话"),
        Endpoint::new("get", "/api/sessions/:id/trajectory", "导出训练轨迹"),
        Endpoint::new("get", "/api/sessions/:id/tokens", "token 用量统计"),
        Endpoint::new("get", "/api/sessions/:id/graph", "编排图（任务 DAG）"),
        Endpoint::new("get", "/api/sessions/:id/evidence", "证据链视图"),
        Endpoint::new("get", "/api/sessions/:id/snapshots", "轮次快照列表"),
        Endpoint::new("get", "/api/sessions/:id/archive", "归档视图"),
        Endpoint::new("post", "/api/sessions/:id/chat", "发送一轮对话"),
        Endpoint::new("post", "/api/sessions/:id/steer", "运行中转向指令"),
        Endpoint::new("post", "/api/sessions/:id/stop", "停止运行中轮次"),
        Endpoint::new("post", "/api/sessions/:id/undo", "撤销上一轮"),
        Endpoint::new("post", "/api/sessions/:id/edit", "编辑重发"),
        Endpoint::new("post", "/api/sessions/:id/fork", "分支派生"),
        Endpoint::new("put", "/api/sessions/:id/title", "会话重命名"),
        Endpoint::new("put", "/api/sessions/:id/pin", "会话置顶切换"),
        Endpoint::new("get", "/api/sessions/:id/plan", "规划模式：待决清单"),
        Endpoint::new(
            "post",
            "/api/sessions/:id/plan",
            "规划模式：决策（批准/修改/拒绝）",
        ),
        // ---------------- 智能体与组
        Endpoint::new("get", "/api/agents", "智能体列表"),
        Endpoint::new("post", "/api/agents", "创建智能体"),
        Endpoint::new("get", "/api/agents/:identifier", "智能体详情"),
        Endpoint::new("put", "/api/agents/:identifier", "更新智能体"),
        Endpoint::new("delete", "/api/agents/:identifier", "删除智能体"),
        Endpoint::new("put", "/api/agents/:identifier/model", "设置智能体模型"),
        Endpoint::new("get", "/api/agents/:identifier/persona", "人设查看"),
        Endpoint::new("put", "/api/agents/:identifier/persona", "人设修改"),
        Endpoint::new("delete", "/api/agents/:identifier/persona", "人设重置"),
        Endpoint::new("post", "/api/agents/:id/optimize", "智能体优化建议"),
        Endpoint::new("get", "/api/agents/:id/adaptation", "经验改进要点查看"),
        Endpoint::new("delete", "/api/agents/:id/adaptation", "经验改进要点重置"),
        Endpoint::new("get", "/api/groups", "组列表"),
        Endpoint::new("post", "/api/groups", "创建组"),
        Endpoint::new("get", "/api/groups/active", "当前活跃组"),
        Endpoint::new("put", "/api/groups/active", "切换活跃组"),
        Endpoint::new("put", "/api/groups/:id", "更新组"),
        Endpoint::new("delete", "/api/groups/:id", "删除组"),
        Endpoint::new("post", "/api/groups/:id/activate", "激活组"),
        Endpoint::new("get", "/api/groups/:id/agents", "组内智能体列表"),
        Endpoint::new("post", "/api/groups/:id/primary", "设置组主智能体"),
        Endpoint::new("put", "/api/groups/:id/model", "设置组模型"),
        Endpoint::new("put", "/api/groups/:id/capabilities", "设置组能力开关"),
        Endpoint::new("put", "/api/groups/:id/workspace", "设置组工作目录"),
        Endpoint::new("get", "/api/groups/:id/overview", "组总览"),
        Endpoint::new("put", "/api/groups/:id/self-optimize", "组自优化开关"),
        Endpoint::new(
            "delete",
            "/api/groups/:gid/agents/:identifier",
            "从组移除智能体",
        ),
        Endpoint::new(
            "get",
            "/api/groups/:gid/agents/:identifier/prompt",
            "智能体提示词查看",
        ),
        Endpoint::new(
            "put",
            "/api/groups/:gid/agents/:identifier/prompt",
            "智能体提示词修改",
        ),
        Endpoint::new("get", "/api/templates", "智能体模板列表"),
        // ---------------- 通道
        Endpoint::new("get", "/api/channels", "通道列表"),
        Endpoint::new("post", "/api/channels", "创建通道"),
        Endpoint::new("put", "/api/channels/:id", "更新通道"),
        Endpoint::new("delete", "/api/channels/:id", "删除通道"),
        Endpoint::new("post", "/api/channels/:id/test", "通道连通测试"),
        Endpoint::new("get", "/api/channels/status", "通道运行状态"),
        Endpoint::new("get", "/api/channels/flows", "外部通道会话流列表"),
        Endpoint::new("post", "/api/channels/flows/:id/read", "标记会话流已读"),
        Endpoint::new("post", "/api/channels/bili-qr", "B 站扫码登录发起"),
        Endpoint::new("get", "/api/channels/bili-qr/:key", "B 站扫码状态轮询"),
        Endpoint::new("post", "/api/channels/qqbot-bind", "QQ 官方机器人配对发起"),
        Endpoint::new(
            "get",
            "/api/channels/qqbot-bind/:task_id",
            "QQ 官方机器人配对轮询",
        ),
        Endpoint::new("post", "/api/channels/:id/inbound", "入站消息注入"),
        // ---------------- LLM 与模型
        Endpoint::new("get", "/api/llm/profiles", "模型档案列表"),
        Endpoint::new("post", "/api/llm/profiles", "保存模型档案"),
        Endpoint::new("delete", "/api/llm/profiles/:id", "删除模型档案"),
        Endpoint::new("put", "/api/llm/active", "激活模型档案"),
        Endpoint::new("post", "/api/llm/test", "模型通道测试"),
        Endpoint::new("post", "/api/llm/identity", "预览客户端生效协议与标识"),
        Endpoint::new("post", "/api/llm/models", "拉取提供商模型列表"),
        Endpoint::new("get", "/api/llm/effort", "思考强度查看"),
        Endpoint::new("put", "/api/llm/effort", "思考强度设置"),
        Endpoint::new("get", "/api/llm/capabilities", "能力模型槽位查看"),
        Endpoint::new("put", "/api/llm/capabilities", "能力模型槽位设置"),
        Endpoint::new("get", "/api/llm/catalog", "模型用途与成本目录"),
        Endpoint::new(
            "get",
            "/api/singles/:id/model-routing",
            "独立智能体模型分配与生效来源",
        ),
        Endpoint::new(
            "put",
            "/api/singles/:id/model-routing",
            "保存独立智能体模型分配",
        ),
        Endpoint::new("put", "/api/groups/:id/model-routing", "保存组默认模型分配"),
        Endpoint::new(
            "get",
            "/api/groups/:gid/agents/:identifier/model-routing",
            "组内成员模型分配与生效来源",
        ),
        Endpoint::new(
            "put",
            "/api/groups/:gid/agents/:identifier/model-routing",
            "保存组内成员模型分配",
        ),
        // ---------------- 记忆
        Endpoint::new("get", "/api/memory", "记忆列表"),
        Endpoint::new("post", "/api/memory", "新增记忆"),
        Endpoint::new(
            "get",
            "/api/memory/export",
            "记忆全量导出（JSONL；?agent= 过滤）",
        ),
        Endpoint::new("post", "/api/memory/search", "记忆检索"),
        Endpoint::new("get", "/api/memory/stats", "记忆统计"),
        Endpoint::new("post", "/api/memory/reindex", "重建记忆索引"),
        Endpoint::new("post", "/api/memory/decay", "触发记忆衰减"),
        Endpoint::new("post", "/api/memory/render", "渲染浅层快照"),
        Endpoint::new("delete", "/api/memory/:id", "删除记忆"),
        Endpoint::new("put", "/api/memory/:id", "编辑记忆条目"),
        Endpoint::new("post", "/api/memory/:id/pin", "记忆置顶切换"),
        Endpoint::new("get", "/api/memory/md", "浅层快照查看"),
        Endpoint::new("put", "/api/memory/md", "浅层快照编辑"),
        // ---------------- 知识库
        Endpoint::new("get", "/api/knowledge", "知识列表（q= 检索）"),
        Endpoint::new("post", "/api/knowledge", "新增知识"),
        Endpoint::new("delete", "/api/knowledge/:id", "删除知识"),
        // ---------------- 定时任务
        Endpoint::new("get", "/api/cron", "定时任务列表"),
        Endpoint::new("post", "/api/cron", "创建定时任务"),
        Endpoint::new("get", "/api/cron/runs", "定时任务执行历史"),
        Endpoint::new("put", "/api/cron/:id", "更新定时任务"),
        Endpoint::new("delete", "/api/cron/:id", "删除定时任务"),
        Endpoint::new("post", "/api/cron/:id/run", "立即执行定时任务"),
        // ---------------- 审批
        Endpoint::new("get", "/api/approvals", "待审批列表"),
        Endpoint::new("post", "/api/approvals/:id/approve", "批准"),
        Endpoint::new("post", "/api/approvals/:id/deny", "拒绝"),
        // ---------------- 审计与限额
        Endpoint::new("get", "/api/audit", "工具审计聚合"),
        Endpoint::new("get", "/api/limits/stats", "限额统计"),
        // ---------------- 配置
        Endpoint::new("get", "/api/config", "配置查看"),
        Endpoint::new("put", "/api/config", "配置修改"),
        Endpoint::new("get", "/api/config/schema", "配置 schema"),
        Endpoint::new("get", "/api/config/sanitize-stats", "净化统计"),
        // ---------------- 工作台与资产
        Endpoint::new("get", "/api/workbench/tree", "工作台文件树"),
        Endpoint::new("get", "/api/workbench/file", "工作台文件内容"),
        Endpoint::new("get", "/api/asset", "资产内联预览"),
        Endpoint::new("get", "/api/asset/download", "资产下载"),
        // ---------------- 检查点
        Endpoint::new("get", "/api/checkpoints", "检查点列表"),
        Endpoint::new("post", "/api/checkpoints/restore", "回滚到检查点"),
        // ---------------- 技能包
        Endpoint::new("get", "/api/skills", "技能包列表"),
        Endpoint::new("post", "/api/skills", "创建技能包"),
        Endpoint::new("post", "/api/skills/import", "导入技能包"),
        Endpoint::new("get", "/api/skills/:id/export", "导出技能包"),
        Endpoint::new("delete", "/api/skills/:id", "删除技能包"),
        // ---------------- 插件
        Endpoint::new("get", "/api/plugins", "插件列表"),
        Endpoint::new("post", "/api/plugins/install", "安装插件"),
        Endpoint::new("post", "/api/plugins/:id/update", "更新插件"),
        Endpoint::new("get", "/api/plugins/index", "插件索引"),
        Endpoint::new("get", "/api/plugins/:id", "插件详情"),
        Endpoint::new("put", "/api/plugins/:id", "插件配置修改"),
        Endpoint::new("delete", "/api/plugins/:id", "卸载插件"),
        Endpoint::new("get", "/api/plugins/:id/credentials", "插件凭据查看"),
        Endpoint::new("put", "/api/plugins/:id/credentials", "插件凭据设置"),
        // ---------------- MCP
        Endpoint::new("get", "/api/mcp", "MCP 服务端列表"),
        Endpoint::new("post", "/api/mcp/refresh", "刷新 MCP 连接"),
        // ---------------- 语音
        Endpoint::new("post", "/api/voice/transcribe", "语音转文字"),
        Endpoint::new("post", "/api/voice/speak", "文字转语音"),
        // ---------------- 身份与鉴权
        Endpoint::new("get", "/api/identity/list", "身份绑定列表"),
        Endpoint::new("post", "/api/identity/code", "签发配对码"),
        Endpoint::new("delete", "/api/identity/code/:code", "吊销配对码"),
        Endpoint::new("post", "/api/identity/codes/purge", "清理过期配对码"),
        Endpoint::new("post", "/api/identity/role", "设置身份角色"),
        Endpoint::new("delete", "/api/identity/:id", "解绑身份"),
        Endpoint::public("post", "/api/auth/verify", "校验鉴权密钥"),
        Endpoint::public("get", "/api/auth/status", "鉴权设置状态"),
        Endpoint::public("post", "/api/auth/snooze", "鉴权提醒延后"),
        Endpoint::public("post", "/api/auth/dismiss", "鉴权提醒关闭"),
        // ---------------- 版本升级
        Endpoint::new("get", "/api/version/check", "检查更新"),
        Endpoint::new("post", "/api/version/apply", "应用更新"),
        Endpoint::new("get", "/api/version/apply/status", "更新进度"),
        // ---------------- 事件触发
        Endpoint::new("post", "/api/events/webhook", "外部事件 webhook"),
        // ---------------- 单轮智能体与交互目标
        Endpoint::new("get", "/api/target", "当前交互目标"),
        Endpoint::new("put", "/api/target", "切换交互目标"),
        Endpoint::new("get", "/api/singles", "单轮智能体列表"),
        Endpoint::new("post", "/api/singles", "创建单轮智能体"),
        Endpoint::new("put", "/api/singles/:id", "更新单轮智能体"),
        Endpoint::new("delete", "/api/singles/:id", "删除单轮智能体"),
        Endpoint::new("get", "/api/singles/:id/persona", "单轮智能体人设查看"),
        Endpoint::new("put", "/api/singles/:id/persona", "单轮智能体人设修改"),
        Endpoint::new("delete", "/api/singles/:id/persona", "单轮智能体人设重置"),
        Endpoint::new("get", "/api/singles/:id/prompt", "单轮智能体提示词查看"),
        Endpoint::new("put", "/api/singles/:id/prompt", "单轮智能体提示词修改"),
        // ---------------- Nexus 拓扑
        Endpoint::new("get", "/api/nexus/links", "拓扑连边列表"),
        Endpoint::new("post", "/api/nexus/links", "保存拓扑连边"),
        Endpoint::new("delete", "/api/nexus/links/:id", "删除拓扑连边"),
        Endpoint::new("post", "/api/nexus/links/:id/probe", "连边连通探测"),
        Endpoint::new("get", "/api/nexus/graph", "拓扑图数据"),
        Endpoint::new("put", "/api/nexus/edges", "编辑拓扑边"),
        // ---------------- 剧本与自检
        Endpoint::new("get", "/api/playbooks", "剧本列表"),
        Endpoint::new("get", "/api/selfcheck", "系统自检"),
        // ---------------- 工作目录
        Endpoint::new("get", "/api/workspace", "工作目录查看"),
        Endpoint::new("put", "/api/workspace", "工作目录设置"),
        Endpoint::new("get", "/api/fs/list", "桌面工作区目录"),
        Endpoint::new("get", "/api/fs/file", "桌面文本预览"),
        Endpoint::new("put", "/api/fs/file", "桌面文件保存与检查点"),
        Endpoint::new("get", "/api/workspace/changes", "工作区变更"),
        Endpoint::new("get", "/api/git/overview", "Git 分支与历史"),
        Endpoint::new("post", "/api/git/op", "Git 操作"),
    ]
    .into_iter()
    .map(llm::describe)
    .map(model_routing::describe)
    .collect()
}

fn document_path(path: &str) -> String {
    path.split('/')
        .map(|segment| match segment.strip_prefix(':') {
            Some(name) => format!("{{{name}}}"),
            None => segment.to_string(),
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn error_response(description: &str) -> Value {
    json!({
        "description": description,
        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Error" } } },
    })
}

/// 生成 OpenAPI 3.1 文档（JSON Value）
pub fn generate() -> Value {
    let mut schemas = llm::schemas();
    schemas
        .as_object_mut()
        .unwrap()
        .extend(model_routing::schemas().as_object().unwrap().clone());
    let mut paths = serde_json::Map::new();
    for ep in endpoints() {
        let entry = paths
            .entry(document_path(ep.path))
            .or_insert_with(|| json!({}));
        let parameters: Vec<Value> = ep
            .path
            .split('/')
            .filter_map(|segment| {
                segment.strip_prefix(':').map(|name| {
                    json!({
                        "name": name, "in": "path", "required": true,
                        "schema": { "type": "string" },
                    })
                })
            })
            .collect();
        if !parameters.is_empty() {
            entry["parameters"] = json!(parameters);
        }
        let security = if ep.auth {
            json!([{ "authKey": [] }, { "bearerAuth": [] }])
        } else {
            json!([])
        };
        let mut op = json!({
            "summary": ep.summary,
            "operationId": ep.operation_id(),
            "security": security,
            "responses": {
                "200": { "description": "成功" },
                "401": error_response("鉴权失败（访问密钥启用时，或校验接口收到错误密钥）"),
            },
        });
        if matches!(ep.method, "post" | "put" | "patch")
            || (ep.path == "/api/llm/profiles/:id" && ep.method == "delete")
        {
            op["responses"]["400"] = error_response("请求参数或请求体无效");
        }
        if ep.path.split('/').any(|segment| segment.starts_with(':'))
            || matches!(
                ep.path,
                "/api/llm/active" | "/api/llm/test" | "/api/llm/models" | "/api/llm/identity"
            )
        {
            op["responses"]["404"] = error_response("指定资源不存在");
        }
        if matches!(
            (ep.method, ep.path),
            ("post", "/api/llm/profiles")
                | ("delete", "/api/llm/profiles/:id")
                | ("put", "/api/llm/active" | "/api/llm/capabilities")
        ) {
            op["responses"]["500"] = error_response("配置保存或运行时更新失败");
        }
        if let Some(req) = ep.request {
            op["requestBody"] = req;
        }
        if let Some(resp) = ep.response {
            op["responses"]["200"]["content"] = resp;
        }
        if matches!(ep.path, "/api/commands" | "/api/capabilities") {
            op["parameters"] = json!([
                {
                    "name": "protocolVersion", "in": "query", "required": false,
                    "schema": {"type": "integer", "minimum": 1},
                    "description": "命令协议主版本；省略时使用服务端当前版本"
                },
                {
                    "name": "sessionId", "in": "query", "required": false,
                    "schema": {"type": "string"},
                    "description": "绑定会话；项目固定会话还必须提交 X-Exm-Workspace"
                },
                {
                    "name": "scope", "in": "query", "required": false,
                    "schema": {"type": "string", "enum": ["global", "project", "session", "flow"]}
                },
                {
                    "name": "includeDeprecated", "in": "query", "required": false,
                    "schema": {"type": "boolean", "default": false}
                },
                {
                    "name": "X-Exm-Workspace", "in": "header", "required": false,
                    "schema": {"type": "string"},
                    "description": "项目固定会话绑定的绝对项目目录"
                }
            ]);
            op["responses"]["400"] = error_response("协议版本或查询参数无效");
            op["responses"]["403"] = error_response("鉴权失败或会话需要项目连接");
            op["responses"]["404"] = error_response("会话不存在或不属于当前项目");
        }
        if ep.path.starts_with("/api/collaborations") {
            let mut parameters = vec![
                json!({"name": "X-Exm-Workspace", "in": "header", "required": true,
                "schema": {"type": "string"}, "description": "百分号编码后的群聊归属项目绝对目录"}),
            ];
            if (ep.method, ep.path) == ("get", "/api/collaborations/:id") {
                parameters.push(
                    json!({"name": "before", "in": "query", "schema": {"type": "string"},
                    "description": "上一页返回的 nextBefore（独占游标）"}),
                );
            }
            op["parameters"] = json!(parameters);
            op["responses"]["400"] = error_response("项目请求头或参数无效");
            op["responses"]["403"] = error_response("实例未启用访问密钥");
            let status = match (ep.method, ep.path) {
                ("post", "/api/collaborations") => Some("201"),
                ("post", "/api/collaborations/:id/turns") => Some("202"),
                _ => None,
            };
            if let Some(status) = status {
                let responses = op["responses"].as_object_mut().unwrap();
                let success = responses.remove("200").unwrap();
                responses.insert(status.into(), success);
            }
            if ep.method == "post" && ep.path.contains("/turns") {
                op["responses"]["409"] = error_response("轮次或成员状态冲突");
            }
        }
        entry[ep.method] = op;
    }
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "EXMACHINA Gateway API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "多智能体系统网关：会话/智能体/组/通道/记忆/审批/审计 REST 面。\
                            人读协议见 docs/协议与契约.md。路径/方法/鉴权全覆盖；请求体渐进补；响应 schema informational。\
                            安全要求在实例启用访问密钥时生效；事件 webhook、通道入站与资产的额外鉴权方式见人读协议。",
        },
        "components": {
            "securitySchemes": {
                "authKey": {
                    "type": "apiKey",
                    "in": "header",
                    "name": "X-Auth-Key",
                    "description": "启用访问密钥时填写实例访问密钥；也可使用 bearerAuth。",
                },
                "bearerAuth": {
                    "type": "http",
                    "scheme": "bearer",
                    "description": "Authorization: Bearer <实例访问密钥>；与 X-Auth-Key 等价。",
                }
            },
            "schemas": schemas,
        },
        "paths": Value::Object(paths),
    })
}

// ---------------------------------------------------------------- HTTP handler

/// `GET /api/openapi.json`：契约文档端点（挂在 build_router；公共面豁免鉴权）
pub async fn serve_openapi() -> axum::Json<Value> {
    axum::Json(generate())
}

// ---------------------------------------------------------------- 路由提取（对账用）

/// 从 Router 的 Debug 输出提取「路径 → 方法集」（axum 0.7.9 Debug 格式解析）。
///
/// 事实源是 `build_router`；Debug 先输出 path_router（`paths: {RouteId(1): "/api/a"}`），
/// 再输出方法注册（`routes: {RouteId(1): MethodRouter({ get: BoxedHandler, ... })}`），
/// 最后是 fallback_router（其 paths 值为 "/" 与 catch-all，跳过）。
pub fn router_paths(router: &axum::Router) -> std::collections::BTreeMap<String, Vec<String>> {
    let dbg = format!("{router:?}");
    let id_to_path = parse_route_id_paths(&dbg);
    let mut out: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for (id, body) in parse_method_routers(&dbg) {
        let Some(path) = id_to_path.get(&id) else {
            continue;
        };
        let mut methods = Vec::new();
        for m in [
            "get", "post", "put", "delete", "patch", "head", "options", "trace",
        ] {
            // 已注册方法：`get: Route` 或 `get: BoxedHandler`（boxed 路由形态）；None 跳过
            if body.contains(&format!("{m}: Route")) || body.contains(&format!("{m}: BoxedHandler"))
            {
                methods.push(m.to_uppercase());
            }
        }
        out.entry(path.clone()).or_default().extend(methods);
    }
    out
}

/// `RouteId(N): "/path"` 提取（仅扫 path_router 的 paths 段——Debug 首个 `paths: {`）。
/// fallback_router 的 paths 值为 "/" 与 catch-all，不是真实端点，跳过。
fn parse_route_id_paths(dbg: &str) -> std::collections::BTreeMap<String, String> {
    let mut map: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    // 截取首个 paths: { ... } 段（path_router 的 node；终止于 `} }`（Node 结束））
    let Some(pstart) = dbg.find("paths: {") else {
        return map;
    };
    let seg = &dbg[pstart + "paths: {".len()..];
    // Node 的 Debug：`paths: {RouteId(1): "/a", RouteId(2): "/b"} }`——首个 `}` 前（嵌套无 braces）
    let Some(peend) = seg.find('}') else {
        return map;
    };
    let seg = &seg[..peend];
    let mut rest = seg;
    while let Some(start) = rest.find("RouteId(") {
        rest = &rest[start + "RouteId(".len()..];
        let Some(close) = rest.find(')') else { break };
        let id = rest[..close].to_string();
        let after = &rest[close + 1..];
        let Some(colon_quote) = after.find(": \"") else {
            continue;
        };
        let tail = &after[colon_quote + 3..];
        let Some(endq) = tail.find('"') else { break };
        map.insert(id, tail[..endq].to_string());
        rest = tail;
    }
    map
}

/// `RouteId(N): MethodRouter(MethodRouter { ... })` 提取 → (id, 方法体)
fn parse_method_routers(dbg: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = dbg;
    while let Some(start) = rest.find("RouteId(") {
        rest = &rest[start + "RouteId(".len()..];
        let Some(close) = rest.find(')') else { break };
        let id = rest[..close].to_string();
        let after = &rest[close + 1..];
        let Some(mr) = after.find("MethodRouter { ") else {
            continue;
        };
        let body_start = &after[mr + "MethodRouter { ".len()..];
        // 方法体以 `})` 或 `}` 收尾（allow_header 在最后一个字段）
        let Some(end) = body_start.find("}") else {
            break;
        };
        out.push((id, body_start[..end].to_string()));
        rest = body_start;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_core() -> std::sync::Arc<exm_core::Core> {
        let dir = std::env::temp_dir().join(format!("exm-openapi-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::sync::Arc::new(exm_core::Core::create(&dir).unwrap())
    }

    /// 对账测试：清单 ↔ build_router 双向 diff，多漏即红（幽灵端点/漏项/方法集偏差）
    #[test]
    fn 路由对账_清单与实路由一致() {
        let router = crate::build_router(test_core());
        let actual = router_paths(&router);
        let listed: std::collections::BTreeMap<String, Vec<String>> =
            endpoints()
                .into_iter()
                .fold(std::collections::BTreeMap::new(), |mut acc, ep| {
                    acc.entry(ep.path.to_string())
                        .or_default()
                        .push(ep.method.to_uppercase());
                    acc
                });
        // 幽灵端点（清单有路由无）
        let ghosts: Vec<&String> = listed.keys().filter(|k| !actual.contains_key(*k)).collect();
        assert!(ghosts.is_empty(), "清单中存在未注册路由：{ghosts:?}");
        // 漏项（路由有清单无；排除 /ws /worker——WebSocket 面）
        let missing: Vec<&String> = actual
            .keys()
            .filter(|k| !listed.contains_key(*k) && k.as_str() != "/ws" && k.as_str() != "/worker")
            .collect();
        assert!(missing.is_empty(), "路由未纳入清单：{missing:?}");
        // 方法集对齐
        for (path, methods) in &listed {
            if let Some(actual_methods) = actual.get(path) {
                let want: std::collections::BTreeSet<&String> = methods.iter().collect();
                let got: std::collections::BTreeSet<&String> = actual_methods.iter().collect();
                assert_eq!(
                    want, got,
                    "方法集不一致：{path} 清单 {methods:?} 实际 {actual_methods:?}"
                );
            }
        }
    }

    /// 生成文档的基本结构
    #[test]
    fn 生成文档_结构合法() {
        let doc = generate();
        assert_eq!(doc["openapi"], "3.1.0");
        assert!(doc["paths"].is_object());
        assert_eq!(
            doc["components"]["securitySchemes"]["authKey"]["name"],
            "X-Auth-Key"
        );
        // 每个操作有 summary 与 401 响应（错误族必填档）
        for (_, entry) in doc["paths"].as_object().unwrap() {
            for (method, op) in entry.as_object().unwrap() {
                if method == "parameters" {
                    continue;
                }
                assert!(op["summary"].is_string(), "缺 summary：{method} {entry}");
                assert!(
                    op["responses"]["401"].is_object(),
                    "缺 401：{method} {entry}"
                );
            }
        }
        for path in ["/api/commands", "/api/capabilities"] {
            let operation = &doc["paths"][path]["get"];
            let parameters = operation["parameters"].as_array().unwrap();
            for name in [
                "protocolVersion",
                "sessionId",
                "scope",
                "includeDeprecated",
                "X-Exm-Workspace",
            ] {
                assert!(
                    parameters.iter().any(|parameter| parameter["name"] == name),
                    "{path} OpenAPI omitted {name}"
                );
            }
            assert!(operation["responses"]["400"].is_object());
            assert!(operation["responses"]["403"].is_object());
        }
    }

    /// 提取器冒烟：固定 Router 的路径与方法
    #[test]
    fn 提取器_路径与方法() {
        let router: axum::Router = axum::Router::new()
            .route(
                "/api/a",
                axum::routing::get(|| async { "a" }).post(|| async { "b" }),
            )
            .route("/api/b/:id", axum::routing::put(|| async { "c" }));
        let paths = router_paths(&router);
        assert_eq!(
            paths.get("/api/a").map(|v| v.as_slice()),
            Some(&["GET".to_string(), "POST".to_string()][..])
        );
        assert_eq!(
            paths.get("/api/b/:id").map(|v| v.as_slice()),
            Some(&["PUT".to_string()][..])
        );
    }
}
