//! provider 域分文件（自 provider.rs 按职责拆出，纯搬移零行为变更）。

use super::*;
/// 流式响应组装器：正文 / 思维链累积、内联 `<think>` 分流、用量统计、工具调用分片拼装。
/// 把原本散在 `stream()` 主循环里的十余个可变局部收拢成状态机——
/// 主循环只管「字节 → 完整行」，帧处理全部收口在 [`StreamAssembler::frame`]。
pub(crate) struct StreamAssembler<'a> {
    provider: &'a OpenAiCompatibleProvider,
    /// 正文累积（已过内联 <think> 分流）
    acc: String,
    /// 思维链累积（原生 reasoning 轨 + 内联 think 轨）
    reasoning_acc: String,
    /// 内联思维链分流状态（0=未判定 1=思考中 2=正文）
    think_state: u8,
    think_buf: String,
    usage_prompt: u64,
    usage_completion: u64,
    /// 工具调用流式分片组装：openai/anthropic 按索引拼增量，gemini 为整块
    call_frags: Vec<(String, String, String)>, // (id, name, arguments-json)
    gemini_calls: Vec<ToolCall>,
}

impl<'a> StreamAssembler<'a> {
    pub(crate) fn new(provider: &'a OpenAiCompatibleProvider) -> Self {
        StreamAssembler {
            provider,
            acc: String::new(),
            reasoning_acc: String::new(),
            think_state: 0,
            think_buf: String::new(),
            usage_prompt: 0,
            usage_completion: 0,
            call_frags: Vec::new(),
            gemini_calls: Vec::new(),
        }
    }

    /// 处理一帧 SSE data JSON：分轨（正文/思考）、攒工具调用分片、累计用量
    pub(crate) fn frame(&mut self, v: &serde_json::Value, tx: &UnboundedSender<StreamDelta>) {
        match self.provider.api_format.as_str() {
            "anthropic" => {
                let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if ty == "content_block_start" {
                    if let Some(block) = v.pointer("/content_block") {
                        if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                            let idx = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                            while self.call_frags.len() <= idx {
                                self.call_frags
                                    .push((String::new(), String::new(), String::new()));
                            }
                            self.call_frags[idx].0 = block
                                .get("id")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string();
                            self.call_frags[idx].1 = block
                                .get("name")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string();
                        }
                    }
                } else if ty == "content_block_delta" {
                    if let Some(d) = v.get("delta") {
                        if d.get("type").and_then(|t| t.as_str()) == Some("input_json_delta") {
                            let idx = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                            while self.call_frags.len() <= idx {
                                self.call_frags
                                    .push((String::new(), String::new(), String::new()));
                            }
                            let partial =
                                d.get("partial_json").and_then(|p| p.as_str()).unwrap_or("");
                            self.call_frags[idx].2.push_str(partial);
                        }
                    }
                }
            }
            "gemini" => {
                if let Some(parts) = v
                    .pointer("/candidates/0/content/parts")
                    .and_then(|p| p.as_array())
                {
                    for part in parts {
                        if let Some(fc) = part.get("functionCall") {
                            self.gemini_calls.push(ToolCall {
                                id: format!("gem_{}", self.gemini_calls.len()),
                                name: fc
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
                                arguments: fc
                                    .get("args")
                                    .cloned()
                                    .unwrap_or_else(|| serde_json::json!({})),
                            });
                        }
                    }
                }
            }
            _ => {
                if let Some(fcs) = v
                    .pointer("/choices/0/delta/tool_calls")
                    .and_then(|c| c.as_array())
                {
                    for tc in fcs {
                        let idx = tc
                            .get("index")
                            .and_then(|i| i.as_u64())
                            .unwrap_or(self.call_frags.len() as u64)
                            as usize;
                        while self.call_frags.len() <= idx {
                            self.call_frags
                                .push((String::new(), String::new(), String::new()));
                        }
                        if let Some(id) = tc.get("id").and_then(|x| x.as_str()) {
                            self.call_frags[idx].0 = id.to_string();
                        }
                        if let Some(nm) = tc.pointer("/function/name").and_then(|x| x.as_str()) {
                            self.call_frags[idx].1 = nm.to_string();
                        }
                        if let Some(a) = tc.pointer("/function/arguments").and_then(|x| x.as_str())
                        {
                            self.call_frags[idx].2.push_str(a);
                        }
                    }
                }
            }
        }
        let (text_delta, think_delta) = self.provider.parse_delta(v);
        if let Some(t) = think_delta {
            self.reasoning_acc.push_str(&t);
            let _ = tx.send(StreamDelta::Thinking(t));
        }
        if let Some(d) = text_delta {
            // 正文增量先过内联 <think> 分流，再分别入轨
            for (is_think, part) in
                inline_think::route(&mut self.think_state, &mut self.think_buf, &d)
            {
                if is_think {
                    self.reasoning_acc.push_str(&part);
                    let _ = tx.send(StreamDelta::Thinking(part));
                } else {
                    self.acc.push_str(&part);
                    let _ = tx.send(StreamDelta::Text(part));
                }
            }
        }
        // 用量：openai（末块 usage）/ anthropic（message_start 输入）/ gemini
        if let Some(u) = v.get("usage") {
            let p = u
                .get("prompt_tokens")
                .or_else(|| u.get("input_tokens"))
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            let c = u
                .get("completion_tokens")
                .or_else(|| u.get("output_tokens"))
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            if p > 0 {
                self.usage_prompt = p;
            }
            if c > 0 {
                self.usage_completion = c;
            }
        }
        if let Some(p) = v
            .pointer("/message/usage/input_tokens")
            .and_then(|x| x.as_u64())
        {
            self.usage_prompt = p;
        }
        if let Some(u) = v.get("usageMetadata") {
            if let Some(p) = u.get("promptTokenCount").and_then(|x| x.as_u64()) {
                self.usage_prompt = p;
            }
            if let Some(c) = u.get("candidatesTokenCount").and_then(|x| x.as_u64()) {
                self.usage_completion = c;
            }
        }
    }

    /// 流结束：未判定的前缀缓冲（不足 "<think>" 长度即结束）或未闭合的思考尾段冲刷，
    /// 再拼装工具调用与用量，产出最终响应
    pub(crate) fn finish(mut self, tx: &UnboundedSender<StreamDelta>) -> ChatResponse {
        if !self.think_buf.is_empty() {
            let text = std::mem::take(&mut self.think_buf);
            if self.think_state == 1 {
                self.reasoning_acc.push_str(&text);
                let _ = tx.send(StreamDelta::Thinking(text));
            } else {
                self.acc.push_str(&text);
                let _ = tx.send(StreamDelta::Text(text));
            }
        }
        let mut tool_calls: Vec<ToolCall> = self
            .call_frags
            .into_iter()
            .enumerate()
            .filter(|(_, (_, name, _))| !name.is_empty())
            .map(|(i, (id, name, args))| ToolCall {
                id: if id.is_empty() {
                    format!("call_{i}")
                } else {
                    id
                },
                name,
                // 分片 JSON 拼装失败（模型产出残缺参数）不应让整次响应报废：
                // 回退空对象，调用方按「参数不全」走工具侧报错，循环可继续自纠
                arguments: serde_json::from_str(&args).unwrap_or_else(|_| serde_json::json!({})),
            })
            .collect();
        tool_calls.extend(self.gemini_calls);
        ChatResponse {
            content: self.acc,
            reasoning: self.reasoning_acc,
            prompt_tokens: self.usage_prompt,
            completion_tokens: self.usage_completion,
            tool_calls,
        }
    }
}

// ---------------------------------------------------------------- 未配置的模型通道

