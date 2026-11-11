//! provider 域分文件（自 provider.rs 按职责拆出，纯搬移零行为变更）。

use super::*;
pub struct OpenAiCompatibleProvider {
    base_url: String,
    /// API 协议：openai（默认，/chat/completions）| anthropic（/v1/messages）
    pub(crate) api_format: String,
    /// Key 池：多把 Key 负载均衡——按发起方粘性选键，仅限额时前进（保留端点侧 KV 缓存）
    keys: Vec<String>,
    sticky: Mutex<std::collections::HashMap<String, usize>>,
    client: reqwest::Client,
}

impl OpenAiCompatibleProvider {
    pub fn new(base_url: impl Into<String>, keys: Vec<String>) -> Self {
        Self::with_format(base_url, keys, "openai")
    }

    pub fn with_format(base_url: impl Into<String>, keys: Vec<String>, api_format: &str) -> Self {
        let fmt = match api_format {
            "anthropic" | "azure" | "gemini" => api_format,
            _ => "openai",
        };
        OpenAiCompatibleProvider {
            base_url: base_url.into(),
            api_format: fmt.into(),
            keys: keys.into_iter().filter(|k| !k.trim().is_empty()).collect(),
            sticky: Mutex::new(std::collections::HashMap::new()),
            // 全局请求超时（审计 错误处理 P1-1）：端点 TCP 建连后僵死（代理半开/服务商挂起）
            // 时 reqwest 默认无总超时 → chat/embed 永久阻塞 → run 永不收束、run_lock 永久持有。
            // 300s = 非流式调用的宽上限（对比 transcribe/speak 已有的 120s）；
            // 流式不能套总超时（长回答合法超 5 分钟），改在收流处加帧间 deadline
            // （call_orch_stream 的 idle 超时）+ 建连/首响超时（connect_timeout 30s）。
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .connect_timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    fn url(&self, model: &str, stream: bool) -> String {
        let base = self.base_url.trim_end_matches('/');
        match self.api_format.as_str() {
            "anthropic" => format!("{base}/v1/messages"),
            // Azure：部署名入 URL，鉴权用 api-key 头
            "azure" => {
                format!("{base}/openai/deployments/{model}/chat/completions?api-version=2024-10-21")
            }
            // Gemini 原生：generateContent / streamGenerateContent(alt=sse)，模型入 URL
            "gemini" if stream => {
                format!("{base}/v1beta/models/{model}:streamGenerateContent?alt=sse")
            }
            "gemini" => format!("{base}/v1beta/models/{model}:generateContent"),
            _ => format!("{base}/chat/completions"),
        }
    }

    fn auth_header(
        &self,
        key: &str,
    ) -> (reqwest::header::HeaderName, reqwest::header::HeaderValue) {
        let val = reqwest::header::HeaderValue::from_str(key)
            .unwrap_or_else(|_| reqwest::header::HeaderValue::from_static(""));
        match self.api_format.as_str() {
            "anthropic" => (reqwest::header::HeaderName::from_static("x-api-key"), val),
            "azure" => (reqwest::header::HeaderName::from_static("api-key"), val),
            "gemini" => (
                reqwest::header::HeaderName::from_static("x-goog-api-key"),
                val,
            ),
            _ => (
                reqwest::header::AUTHORIZATION,
                reqwest::header::HeaderValue::from_str(&format!("Bearer {key}"))
                    .unwrap_or_else(|_| reqwest::header::HeaderValue::from_static("")),
            ),
        }
    }

    fn build_request(
        &self,
        key: &str,
        model: &str,
        stream: bool,
        body: serde_json::Value,
    ) -> reqwest::RequestBuilder {
        let (name, value) = self.auth_header(key);
        let rb = self
            .client
            .post(self.url(model, stream))
            .header(name, value);
        // 流式请求覆盖总超时为 600s 宽上限：SSE 长回答合法超过 client 的 300s
        // 非流式上限；帧间卡死由收流侧 idle deadline 兜底（见 call_orch_stream）
        let rb = if stream {
            rb.timeout(std::time::Duration::from_secs(600))
        } else {
            rb
        };
        match self.api_format.as_str() {
            "anthropic" => rb.header("anthropic-version", "2023-06-01").json(&body),
            _ => rb.json(&body),
        }
    }

    /// 嵌入用键：直接取第一把（无发起方粘性语义）
    fn key_hint_texts(&self) -> String {
        self.keys.first().cloned().unwrap_or_default()
    }

    /// 粘性选键：同 hint 稳定命中同一把；首次按 hint 散列分布（不同个体自然错开不同 Key）
    fn pick_index(&self, hint: &str) -> usize {
        let mut m = self.sticky.lock();
        *m.entry(hint.to_string()).or_insert_with(|| {
            let seed: u32 = hint
                .bytes()
                .fold(5381u32, |a, b| a.wrapping_mul(33).wrapping_add(b as u32));
            seed as usize % self.keys.len().max(1)
        })
    }

    /// 限额切换：前进到下一把并粘住
    fn rotate_index(&self, hint: &str) -> usize {
        let mut m = self.sticky.lock();
        let next = (*m.entry(hint.to_string()).or_insert(0) + 1) % self.keys.len().max(1);
        m.insert(hint.to_string(), next);
        next
    }

    /// 限额类失败判定（429/402/403 或配额、限流文案）
    fn is_quota_failure(status: u16, body: &str) -> bool {
        let low = body.to_lowercase();
        [429, 402].contains(&status)
            || status == 403
            || low.contains("quota")
            || low.contains("rate limit")
            || low.contains("insufficient")
            || low.contains("too many requests")
            || low.contains("resource_exhausted")
    }

    /// 限额/配额错误 → 固定话术（IM 可读性）。各家上游 429 载荷格式不一
    /// （聚合端点嵌套 JSON / 官方端点纯文本 / 字段名各异），不做内容提取——
    /// 只按状态码给固定建议；原始响应体走 stderr 留排障线索，不进用户回复。
    pub(crate) fn quota_error_summary(status: u16, body: &str) -> String {
        // 排障线索进日志（截 400 字）；用户侧只见通用话术
        eprintln!(
            "[llm-provider] 配额失败 HTTP {status}: {}",
            body.chars().take(400).collect::<String>()
        );
        match status {
            402 => "模型服务额度已用尽（余额不足，HTTP 402）。请充值或更换 API Key 后重试".into(),
            403 => "模型服务拒绝访问（HTTP 403）。请检查 API Key 权限或该模型的开通状态".into(),
            // 429 两种可能：瞬时限流（稍等即恢复）或日/月配额用尽（要等重置或换
            // Key）——载荷格式不一无法可靠区分，话术并列提示，不预设哪一种
            _ => "模型服务额度受限（HTTP 429）：可能是短时限流，也可能是当前周期配额已用尽。请稍等几分钟再试一次；若反复出现，检查 API Key 配额余量或补充备用 Key".into(),
        }
    }

    fn key_for(&self, hint: &str) -> String {
        if self.keys.is_empty() {
            return String::new();
        }
        self.keys[self.pick_index(hint)].clone()
    }

    pub(crate) fn body(&self, req: &ChatRequest, stream: bool) -> serde_json::Value {
        let mut base = match self.api_format.as_str() {
            "anthropic" => {
                let system: String = req
                    .messages
                    .iter()
                    .filter(|m| m.role == "system")
                    .map(|m| m.content.clone())
                    .collect::<Vec<_>>()
                    .join(
                        "
",
                    );
                let messages: Vec<serde_json::Value> = req
                    .messages
                    .iter()
                    .filter(|m| m.role != "system")
                    .map(|m| self.map_message(m))
                    .collect();
                let mut body = serde_json::json!({
                    "model": req.model,
                    "messages": messages,
                    "max_tokens": req.max_tokens.unwrap_or(1024), "stream": stream,
                });
                if !system.trim().is_empty() {
                    // prompt caching：系统提示作为可缓存前缀（长会话显著省 token）
                    body["system"] = serde_json::json!([
                        { "type": "text", "text": system, "cache_control": { "type": "ephemeral" } }
                    ]);
                }
                body
            }
            "gemini" => {
                let system: String = req
                    .messages
                    .iter()
                    .filter(|m| m.role == "system")
                    .map(|m| m.content.clone())
                    .collect::<Vec<_>>()
                    .join("\n");
                let contents: Vec<serde_json::Value> = req
                    .messages
                    .iter()
                    .filter(|m| m.role != "system")
                    .map(|m| self.map_message(m))
                    .collect();
                let mut body = serde_json::json!({
                    "contents": contents,
                    "generationConfig": { "temperature": req.temperature },
                });
                if !system.is_empty() {
                    body["systemInstruction"] =
                        serde_json::json!({ "parts": [{ "text": system }] });
                }
                if let Some(mt) = req.max_tokens {
                    body["generationConfig"]["maxOutputTokens"] = serde_json::json!(mt);
                }
                body
            }
            _ => {
                let messages: Vec<serde_json::Value> =
                    req.messages.iter().map(|m| self.map_message(m)).collect();
                let mut body = serde_json::json!({
                    "model": req.model, "messages": messages,
                    "temperature": req.temperature, "stream": stream,
                });
                if stream {
                    // 流式用量统计：末块返回 usage（OpenAI 兼容端点）
                    body["stream_options"] = serde_json::json!({ "include_usage": true });
                }
                if let Some(mt) = req.max_tokens {
                    body["max_tokens"] = serde_json::json!(mt);
                }
                body
            }
        };
        // 推理预算硬上限（anti-runaway-thinking）：OpenAI 兼容端点的 max_tokens
        // 是 reasoning+content 共享预算——对推理模型是唯一有效的「思考时长」硬闸。
        // 部署实证：规划调用不设上限时模型心算大数分解，单轮 thinking 烧到 20 万
        // 字符（≈943s 延迟 + 触发上游 429 限流）。
        // 参数选择（端点实测）：本端点忽略 max_completion_tokens（设 1500 照样
        // 烧满 3000），尊重 max_tokens（finish=length 正确截断）——写旧参数。
        // 请求级显式值优先，否则回落进程级设置；默认安全网 16384（不拦正常
        // 长回答，只拦 9 万 token 级的失控心算）。
        let openai_compat = self.api_format.is_empty() || self.api_format == "openai";
        if openai_compat {
            // 下限 1024 只作用于自动值（安全网/进程级），不抬显式请求值
            let cap = req.max_tokens.or_else(|| {
                reasoning_token_cap()
                    .or(Some(16_384))
                    .map(|v| v.max(1024))
            });
            base["max_tokens"] = serde_json::json!(cap);
        }
        // 模型思考强度：请求级显式值优先，否则回落进程级设置；None = 端点默认不下发
        let effort = req.reasoning.clone().or_else(reasoning_effort);
        if let Some(effort) = effort {
            let effort = normalize_effort(&effort);
            match self.api_format.as_str() {
                "anthropic" => {
                    // extended thinking：budget 需 >1024 且 < max_tokens，故 max_tokens 相应抬升
                    let budget: u32 = match effort.as_str() {
                        "low" => 2048,
                        "high" => 16384,
                        _ => 8192,
                    };
                    base["thinking"] =
                        serde_json::json!({ "type": "enabled", "budget_tokens": budget });
                    let mt = base["max_tokens"]
                        .as_u64()
                        .unwrap_or(1024)
                        .max((budget + 1024) as u64);
                    base["max_tokens"] = serde_json::json!(mt);
                }
                "gemini" => {
                    // Gemini 2.5 thinkingConfig：0=关闭，-1=动态；此处按档位给固定预算
                    let budget: i64 = match effort.as_str() {
                        "low" => 2048,
                        "high" => 24576,
                        _ => 8192,
                    };
                    base["generationConfig"]["thinkingConfig"] =
                        serde_json::json!({ "thinkingBudget": budget });
                }
                _ => {
                    // OpenAI 兼容：reasoning_effort（GPT-5 / o 系；不识别该字段的兼容端点通常忽略）
                    base["reasoning_effort"] = serde_json::json!(effort);
                }
            }
        }
        // 原生 function calling：按协议注入工具定义
        if !req.tools.is_empty() {
            match self.api_format.as_str() {
                "anthropic" => {
                    let mut tools: Vec<serde_json::Value> = req.tools.iter().map(|t| serde_json::json!({
                        "name": t.name, "description": t.description, "input_schema": t.parameters,
                    })).collect();
                    // prompt caching：工具定义为静态前缀，末项打缓存断点
                    if let Some(last) = tools.last_mut() {
                        last["cache_control"] = serde_json::json!({ "type": "ephemeral" });
                    }
                    base["tools"] = serde_json::json!(tools);
                }
                "gemini" => {
                    base["tools"] = serde_json::json!([{ "functionDeclarations": req.tools.iter().map(|t| serde_json::json!({
                        "name": t.name, "description": t.description, "parameters": t.parameters,
                    })).collect::<Vec<_>>() }]);
                }
                _ => {
                    base["tools"] = serde_json::json!(req.tools.iter().map(|t| serde_json::json!({
                        "type": "function",
                        "function": { "name": t.name, "description": t.description, "parameters": t.parameters },
                    })).collect::<Vec<_>>());
                    base["tool_choice"] = serde_json::json!("auto");
                }
            }
        }
        base
    }

    /// 单条消息 → 各协议请求形态（含原生工具调用线程）
    fn map_message(&self, m: &ChatMessage) -> serde_json::Value {
        match self.api_format.as_str() {
            "anthropic" => {
                if !m.tool_calls.is_empty() {
                    let mut blocks: Vec<serde_json::Value> = Vec::new();
                    if !m.content.is_empty() {
                        blocks.push(serde_json::json!({ "type": "text", "text": m.content }));
                    }
                    for c in &m.tool_calls {
                        blocks.push(serde_json::json!({ "type": "tool_use", "id": c.id, "name": c.name, "input": c.arguments }));
                    }
                    serde_json::json!({ "role": "assistant", "content": blocks })
                } else if m.role == "tool" {
                    serde_json::json!({
                        "role": "user",
                        "content": [{ "type": "tool_result", "tool_use_id": m.tool_call_id.clone().unwrap_or_default(), "content": m.content }],
                    })
                } else if !m.images.is_empty() {
                    let mut blocks: Vec<serde_json::Value> = Vec::new();
                    if !m.content.is_empty() {
                        blocks.push(serde_json::json!({ "type": "text", "text": m.content }));
                    }
                    for img in &m.images {
                        if let Some((mime, data)) = ChatMessage::split_data_url(img) {
                            blocks.push(serde_json::json!({ "type": "image", "source": { "type": "base64", "media_type": mime, "data": data } }));
                        }
                    }
                    serde_json::json!({ "role": m.role, "content": blocks })
                } else {
                    serde_json::json!({ "role": m.role, "content": m.content })
                }
            }
            "gemini" => {
                if !m.tool_calls.is_empty() {
                    let parts: Vec<serde_json::Value> = m.tool_calls.iter()
                        .map(|c| serde_json::json!({ "functionCall": { "name": c.name, "args": c.arguments } }))
                        .collect();
                    serde_json::json!({ "role": "model", "parts": parts })
                } else if m.role == "tool" {
                    let name = m
                        .name
                        .clone()
                        .or_else(|| m.tool_call_id.clone())
                        .unwrap_or_default();
                    serde_json::json!({
                        "role": "user",
                        "parts": [{ "functionResponse": { "name": name, "response": { "result": m.content } } }],
                    })
                } else if !m.images.is_empty() {
                    let mut parts: Vec<serde_json::Value> = Vec::new();
                    if !m.content.is_empty() {
                        parts.push(serde_json::json!({ "text": m.content }));
                    }
                    for img in &m.images {
                        if let Some((mime, data)) = ChatMessage::split_data_url(img) {
                            parts.push(serde_json::json!({ "inline_data": { "mime_type": mime, "data": data } }));
                        }
                    }
                    let role = if m.role == "assistant" {
                        "model"
                    } else {
                        m.role.as_str()
                    };
                    serde_json::json!({ "role": role, "parts": parts })
                } else {
                    let role = if m.role == "assistant" {
                        "model"
                    } else {
                        m.role.as_str()
                    };
                    serde_json::json!({ "role": role, "parts": [{ "text": m.content }] })
                }
            }
            _ => {
                if !m.tool_calls.is_empty() {
                    let calls: Vec<serde_json::Value> = m.tool_calls.iter().map(|c| serde_json::json!({
                        "id": c.id, "type": "function",
                        "function": { "name": c.name, "arguments": serde_json::to_string(&c.arguments).unwrap_or_default() },
                    })).collect();
                    let content = if m.content.is_empty() {
                        serde_json::Value::Null
                    } else {
                        serde_json::json!(m.content)
                    };
                    serde_json::json!({ "role": "assistant", "content": content, "tool_calls": calls })
                } else if m.role == "tool" {
                    serde_json::json!({
                        "role": "tool",
                        "tool_call_id": m.tool_call_id.clone().unwrap_or_default(),
                        "content": m.content,
                    })
                } else if !m.images.is_empty() {
                    let mut parts: Vec<serde_json::Value> = Vec::new();
                    if !m.content.is_empty() {
                        parts.push(serde_json::json!({ "type": "text", "text": m.content }));
                    }
                    for img in &m.images {
                        parts.push(
                            serde_json::json!({ "type": "image_url", "image_url": { "url": img } }),
                        );
                    }
                    serde_json::json!({ "role": m.role, "content": parts })
                } else {
                    serde_json::json!({ "role": m.role, "content": m.content })
                }
            }
        }
    }

    /// 非流式响应中的工具调用（各协议）
    fn parse_tool_calls(&self, v: &serde_json::Value) -> Vec<ToolCall> {
        match self.api_format.as_str() {
            "anthropic" => v
                .get("content")
                .and_then(|c| c.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
                        .enumerate()
                        .map(|(i, b)| ToolCall {
                            id: b
                                .get("id")
                                .and_then(|x| x.as_str())
                                .map(String::from)
                                .unwrap_or_else(|| format!("call_{i}")),
                            name: b
                                .get("name")
                                .and_then(|x| x.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            arguments: b
                                .get("input")
                                .cloned()
                                .unwrap_or_else(|| serde_json::json!({})),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            "gemini" => v
                .pointer("/candidates/0/content/parts")
                .and_then(|p| p.as_array())
                .map(|arr| {
                    arr.iter()
                        .enumerate()
                        .filter_map(|(i, part)| {
                            part.get("functionCall").map(|fc| ToolCall {
                                id: format!("call_{i}"),
                                name: fc
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                                arguments: fc
                                    .get("args")
                                    .cloned()
                                    .unwrap_or_else(|| serde_json::json!({})),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            _ => v
                .pointer("/choices/0/message/tool_calls")
                .and_then(|c| c.as_array())
                .map(|arr| {
                    arr.iter()
                        .enumerate()
                        .filter_map(|(i, tc)| {
                            let name = tc.pointer("/function/name").and_then(|n| n.as_str())?;
                            let raw = tc
                                .pointer("/function/arguments")
                                .and_then(|a| a.as_str())
                                .unwrap_or("{}");
                            Some(ToolCall {
                                id: tc
                                    .get("id")
                                    .and_then(|x| x.as_str())
                                    .map(String::from)
                                    .unwrap_or_else(|| format!("call_{i}")),
                                name: name.to_string(),
                                arguments: serde_json::from_str(raw)
                                    .unwrap_or_else(|_| serde_json::json!({})),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// 非流式响应 → 文本（多协议；Gemini 的 thought 部分不算正文）
    fn parse_response(&self, v: &serde_json::Value) -> Option<String> {
        match self.api_format.as_str() {
            "anthropic" => v.get("content").and_then(|c| c.as_array()).map(|arr| {
                arr.iter()
                    .filter(|b| {
                        b.get("type").and_then(|t| t.as_str()) != Some("thinking")
                            && b.get("type").and_then(|t| t.as_str()) != Some("redacted_thinking")
                    })
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("")
            }),
            "gemini" => Self::gemini_parts(v, false),
            _ => v
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.pointer("/message/content"))
                .and_then(|t| t.as_str())
                .map(String::from),
        }
    }

    /// 非流式响应 → 推理内容（多协议；与 parse_response 同源异轨）
    fn parse_reasoning(&self, v: &serde_json::Value) -> Option<String> {
        match self.api_format.as_str() {
            "anthropic" => v.get("content").and_then(|c| c.as_array()).map(|arr| {
                arr.iter()
                    .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("thinking"))
                    .filter_map(|b| b.get("thinking").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("")
            }),
            "gemini" => Self::gemini_parts(v, true),
            // DeepSeek 系 reasoning_content / OpenRouter reasoning / GLM reasoning_content
            _ => v
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| {
                    c.pointer("/message/reasoning_content")
                        .or_else(|| c.pointer("/message/reasoning"))
                })
                .and_then(|t| t.as_str())
                .map(String::from),
        }
    }

    /// 流式增量提取（多协议）→（正文增量, 思维链增量）
    pub(crate) fn parse_delta(&self, v: &serde_json::Value) -> (Option<String>, Option<String>) {
        match self.api_format.as_str() {
            // Anthropic thinking 块：content_block_delta 且 delta.type == thinking_delta
            "anthropic" => {
                if v.pointer("/delta/type").and_then(|t| t.as_str()) == Some("thinking_delta") {
                    (
                        None,
                        v.pointer("/delta/thinking")
                            .and_then(|t| t.as_str())
                            .map(String::from),
                    )
                } else {
                    (
                        v.pointer("/delta/text")
                            .and_then(|t| t.as_str())
                            .map(String::from),
                        None,
                    )
                }
            }
            // Gemini：part.thought == true 的是思维链
            "gemini" => {
                let parts = v
                    .pointer("/candidates/0/content/parts")
                    .and_then(|p| p.as_array());
                let mut text = String::new();
                let mut think = String::new();
                if let Some(arr) = parts {
                    for b in arr {
                        if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                            if b.get("thought").and_then(|x| x.as_bool()).unwrap_or(false) {
                                think.push_str(t);
                            } else {
                                text.push_str(t);
                            }
                        }
                    }
                }
                (
                    if text.is_empty() { None } else { Some(text) },
                    if think.is_empty() { None } else { Some(think) },
                )
            }
            _ => {
                // DeepSeek 系：delta.reasoning_content；OpenRouter：delta.reasoning
                let reasoning = v
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| {
                        c.pointer("/delta/reasoning_content")
                            .or_else(|| c.pointer("/delta/reasoning"))
                    })
                    .and_then(|t| t.as_str())
                    .map(String::from);
                let text = v
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.pointer("/delta/content"))
                    .and_then(|t| t.as_str())
                    .map(String::from);
                (text, reasoning)
            }
        }
    }

    /// Gemini：candidates[0].content.parts[*].text 拼接（非流式与流式分片同构）。
    /// `thought` = true 时取思维链部分，否则取正文部分。
    fn gemini_parts(v: &serde_json::Value, thought: bool) -> Option<String> {
        v.get("candidates")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(|p| p.as_array())
            .map(|arr| {
                arr.iter()
                    .filter(|b| {
                        b.get("thought").and_then(|x| x.as_bool()).unwrap_or(false) == thought
                    })
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("")
            })
    }
}

#[async_trait]
impl LlmProvider for OpenAiCompatibleProvider {
    /// 文本嵌入：openai 协议走 /embeddings；azure 走 deployments/{model}/embeddings；其余不支持
    async fn embed(&self, model: &str, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        if self.keys.is_empty() {
            anyhow::bail!("未配置 API Key（嵌入不可用）");
        }
        let fmt = self.api_format.as_str();
        if fmt != "openai" && fmt != "azure" {
            anyhow::bail!("协议 {fmt} 暂不支持 /embeddings 端点");
        }
        let base = self.base_url.trim_end_matches('/');
        let url = if fmt == "azure" {
            format!("{base}/openai/deployments/{model}/embeddings?api-version=2024-10-21")
        } else {
            format!("{base}/embeddings")
        };
        let key = self.key_hint_texts();
        let (name, value) = self.auth_header(&key);
        let resp = self
            .client
            .post(&url)
            .header(name, value)
            .json(&serde_json::json!({ "model": model, "input": texts }))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("嵌入请求失败: {e}"))?;
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        if !(200..300).contains(&status) {
            anyhow::bail!(
                "嵌入失败 {status}: {}",
                body.to_string().chars().take(200).collect::<String>()
            );
        }
        let mut out: Vec<(usize, Vec<f32>)> = body
            .get("data")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|d| {
                        let idx = d.get("index").and_then(|i| i.as_u64())? as usize;
                        let vec = d.get("embedding")?.as_array()?;
                        let v: Vec<f32> = vec
                            .iter()
                            .filter_map(|x| x.as_f64().map(|f| f as f32))
                            .collect();
                        Some((idx, v))
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by_key(|(i, _)| *i);
        let vectors: Vec<Vec<f32>> = out.into_iter().map(|(_, v)| v).collect();
        if vectors.len() != texts.len() {
            anyhow::bail!("嵌入返回数量不符（{} / {}）", vectors.len(), texts.len());
        }
        Ok(vectors)
    }

    /// 语音转写：openai 协议 /audio/transcriptions；azure deployments/{model}/audio/transcriptions
    async fn transcribe(
        &self,
        model: &str,
        audio: &[u8],
        filename: &str,
    ) -> anyhow::Result<String> {
        if self.keys.is_empty() {
            anyhow::bail!("未配置 API Key（转写不可用）");
        }
        let fmt = self.api_format.as_str();
        if fmt != "openai" && fmt != "azure" {
            anyhow::bail!("协议 {fmt} 暂不支持语音转写");
        }
        let base = self.base_url.trim_end_matches('/');
        let url = if fmt == "azure" {
            format!("{base}/openai/deployments/{model}/audio/transcriptions?api-version=2024-10-21")
        } else {
            format!("{base}/audio/transcriptions")
        };
        let key = self.key_hint_texts();
        let (name, value) = self.auth_header(&key);
        let part = reqwest::multipart::Part::bytes(audio.to_vec())
            .file_name(filename.to_string())
            .mime_str(if filename.ends_with(".mp3") {
                "audio/mpeg"
            } else if filename.ends_with(".wav") {
                "audio/wav"
            } else {
                "audio/webm"
            })?;
        let form = reqwest::multipart::Form::new()
            .text("model", model.to_string())
            .part("file", part);
        let resp = self
            .client
            .post(&url)
            .header(name, value)
            .multipart(form)
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("转写请求失败: {e}"))?;
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        if !(200..300).contains(&status) {
            anyhow::bail!(
                "转写失败 {status}: {}",
                body.to_string().chars().take(200).collect::<String>()
            );
        }
        Ok(body
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string())
    }

    /// 语音合成：openai /audio/speech；azure deployments/{model}/audio/speech（mp3）
    async fn speak(&self, model: &str, text: &str) -> anyhow::Result<Vec<u8>> {
        if self.keys.is_empty() {
            anyhow::bail!("未配置 API Key（合成不可用）");
        }
        let fmt = self.api_format.as_str();
        if fmt != "openai" && fmt != "azure" {
            anyhow::bail!("协议 {fmt} 暂不支持语音合成");
        }
        let base = self.base_url.trim_end_matches('/');
        let url = if fmt == "azure" {
            format!("{base}/openai/deployments/{model}/audio/speech?api-version=2024-10-21")
        } else {
            format!("{base}/audio/speech")
        };
        let key = self.key_hint_texts();
        let (name, value) = self.auth_header(&key);
        let resp = self
            .client
            .post(&url)
            .header(name, value)
            .json(&serde_json::json!({
                "model": model, "input": text, "voice": "alloy", "response_format": "mp3",
            }))
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("合成请求失败: {e}"))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "合成失败 {status}: {}",
                body.chars().take(200).collect::<String>()
            );
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| anyhow::anyhow!("读取音频失败: {e}"))?;
        Ok(bytes.to_vec())
    }

    fn name(&self) -> &'static str {
        match self.api_format.as_str() {
            "anthropic" => "anthropic",
            "azure" => "azure-openai",
            "gemini" => "google-gemini",
            _ => "openai-compatible",
        }
    }

    async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
        let hint = req.key_hint.clone().unwrap_or_default();
        if self.keys.is_empty() {
            anyhow::bail!("未配置 API Key（Mock 通道不应使用真实 Provider）");
        }
        let mut last_err = String::new();
        for _ in 0..self.keys.len() {
            let key = self.key_for(&hint);
            let resp = self
                .build_request(&key, &req.model, false, self.body(&req, false))
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("LLM 请求失败: {e}"))?;
            let status = resp.status().as_u16();
            if !(200..300).contains(&status) {
                let text = resp.text().await.unwrap_or_default();
                if Self::is_quota_failure(status, &text) && self.keys.len() > 1 {
                    last_err = format!("Key#{p} 限额（{status}）", p = self.pick_index(&hint));
                    self.rotate_index(&hint);
                    continue; // 限额 → 切换下一把 Key 并粘住
                }
                if Self::is_quota_failure(status, &text) {
                    // 单 Key 也限额：摘要化（IM 直投可读），不泼原始 JSON
                    anyhow::bail!("模型额度不足：{}", Self::quota_error_summary(status, &text));
                }
                anyhow::bail!(
                    "LLM 请求失败 {status}: {}",
                    text.chars().take(300).collect::<String>()
                );
            }
            let parsed: serde_json::Value = resp.json().await?;
            let content = self.parse_response(&parsed).unwrap_or_default();
            let reasoning = self.parse_reasoning(&parsed).unwrap_or_default();
            let tool_calls = self.parse_tool_calls(&parsed);
            let usage = parsed.get("usage");
            let pt = usage
                .and_then(|u| {
                    u.get("input_tokens")
                        .or_else(|| u.get("prompt_tokens"))
                        .or_else(|| u.get("promptTokenCount"))
                })
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let ct = usage
                .and_then(|u| {
                    u.get("output_tokens")
                        .or_else(|| u.get("completion_tokens"))
                        .or_else(|| u.get("candidatesTokenCount"))
                })
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            return Ok(ChatResponse {
                content,
                reasoning,
                prompt_tokens: pt,
                completion_tokens: ct,
                tool_calls,
            });
        }
        anyhow::bail!("全部 API Key 均不可用：{last_err}")
    }

    async fn stream(
        &self,
        req: ChatRequest,
        tx: UnboundedSender<StreamDelta>,
    ) -> anyhow::Result<ChatResponse> {
        let hint = req.key_hint.clone().unwrap_or_default();
        if self.keys.is_empty() {
            anyhow::bail!("未配置 API Key（Mock 通道不应使用真实 Provider）");
        }
        // 限额轮转限次（与 chat() 的 for 上界对齐）：最多每把 Key 尝试一次。
        // 此前是无限 `loop`——全部 Key 都限额时 rotate 永久轮转，调用方（节点执行 /
        // 会话轮次）被无限挂死，编排并发槽随之耗尽。这里改为有界轮转并把最后状态报出来。
        let resp = {
            let mut last_quota: Option<u16> = None;
            let mut ok: Option<reqwest::Response> = None;
            for _ in 0..self.keys.len().max(1) {
                let key = self.key_for(&hint);
                let r = self
                    .build_request(&key, &req.model, true, self.body(&req, true))
                    .send()
                    .await
                    .map_err(|e| anyhow::anyhow!("LLM 流式请求失败: {e}"))?;
                let status = r.status().as_u16();
                if (200..300).contains(&status) {
                    ok = Some(r);
                    break;
                }
                let text = r.text().await.unwrap_or_default();
                if Self::is_quota_failure(status, &text) && self.keys.len() > 1 {
                    last_quota = Some(status);
                    self.rotate_index(&hint);
                    continue; // 限额 → 切换下一把 Key 并粘住
                }
                if Self::is_quota_failure(status, &text) {
                    anyhow::bail!("模型额度不足：{}", Self::quota_error_summary(status, &text));
                }
                anyhow::bail!(
                    "LLM 流式请求失败 {status}: {}",
                    text.chars().take(300).collect::<String>()
                );
            }
            match ok {
                Some(r) => r,
                None => anyhow::bail!(
                    "全部 {} 把 API Key 均限额（最后状态 HTTP {}）：流式请求不可用，请稍后重试或补充 Key",
                    self.keys.len(),
                    last_quota.unwrap_or(0)
                ),
            }
        };

        // 流组装：逐帧解析状态收拢在 StreamAssembler，主循环只负责「读字节 → 出完整行」
        let mut asm = StreamAssembler::new(self);
        let mut sse = SseLineDecoder::new();
        let mut done = false;
        let mut stream = resp.bytes_stream();
        while !done {
            let Some(chunk) = stream.next().await else {
                break;
            };
            let bytes = chunk?;
            for line in sse.feed(&bytes) {
                if !line.starts_with("data:") {
                    continue;
                }
                let payload = line.trim_start_matches("data:").trim();
                if payload == "[DONE]" {
                    // 显式结束帧：停止读流。部分端点 [DONE] 后仍会发送杂散帧/保活注释，
                    // 继续读只会拖长收尾（且可能重复计帧）。
                    done = true;
                    break;
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) {
                    asm.frame(&v, &tx);
                }
                // 异常帧（非 JSON）静默跳过：SSE 流中允许注释/心跳，解析失败不应中断整条流
            }
        }
        // 对端未发换行即断流：冲刷残留半行（可能恰好是最后一帧，丢了会缺尾部内容）
        if !done {
            if let Some(line) = sse.flush() {
                if line.starts_with("data:") {
                    let payload = line.trim_start_matches("data:").trim();
                    if payload != "[DONE]" {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) {
                            asm.frame(&v, &tx);
                        }
                    }
                }
            }
        }
        Ok(asm.finish(&tx))
    }
}

