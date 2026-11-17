//! OpenAI Responses wire format used by Codex and compatible gateways.
use super::*;
use serde_json::{json, Value};

pub(super) fn body(request: &ChatRequest, stream: bool) -> Value {
    let mut input = Vec::new();
    let mut instructions = Vec::new();
    for message in &request.messages {
        if message.role == "system" {
            instructions.push(message.content.clone());
            continue;
        }
        if message.role == "tool" {
            input.push(json!({"type":"function_call_output","call_id":message.tool_call_id.as_deref().unwrap_or(""),"output":message.content}));
            continue;
        }
        if !message.content.is_empty() || !message.images.is_empty() {
            let mut content = Vec::new();
            if !message.content.is_empty() {
                content.push(json!({"type":if message.role=="assistant" {"output_text"} else {"input_text"},"text":message.content}));
            }
            content.extend(
                message
                    .images
                    .iter()
                    .map(|image| json!({"type":"input_image","image_url":image})),
            );
            input.push(json!({"type":"message","role":message.role,"content":content}));
        }
        input.extend(message.tool_calls.iter().map(|call|json!({"type":"function_call","call_id":call.id,"name":call.name,"arguments":call.arguments.to_string()})));
    }
    let mut result = json!({"model":request.model,"instructions":instructions.join("\n"),"input":input,"stream":stream,"store":false});
    result["max_output_tokens"] = json!(request
        .max_tokens
        .or_else(reasoning_token_cap)
        .unwrap_or(16384));
    if let Some(effort) = request.reasoning.clone().or_else(reasoning_effort) {
        result["reasoning"] = json!({"effort":normalize_effort(&effort),"summary":"auto"});
    }
    if !request.tools.is_empty() {
        result["tools"] = json!(request.tools.iter().map(|tool|json!({"type":"function","name":tool.name,"description":tool.description,"parameters":tool.parameters,"strict":false})).collect::<Vec<_>>());
        result["tool_choice"] = json!("auto");
        result["parallel_tool_calls"] = json!(true);
    }
    result
}

pub(super) fn parse(value: &Value) -> anyhow::Result<ChatResponse> {
    anyhow::ensure!(
        value.get("error").is_none_or(Value::is_null),
        "Responses 上游返回错误"
    );
    anyhow::ensure!(
        value["status"].as_str().is_none_or(|s| s == "completed"),
        "Responses 上游未完成响应"
    );
    anyhow::ensure!(
        value["output"].is_array() || value["output_text"].is_string(),
        "Responses 响应缺少 output"
    );
    let mut result = ChatResponse::default();
    let direct = value["output_text"].as_str();
    if let Some(text) = direct {
        result.content = text.into();
    }
    for item in value["output"].as_array().into_iter().flatten() {
        match item["type"].as_str().unwrap_or("") {
            "message" if direct.is_none() => {
                result.content.push_str(
                    &item["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|part| {
                            part["text"].as_str().or_else(|| part["refusal"].as_str())
                        })
                        .collect::<String>(),
                );
            }
            "reasoning" => result.reasoning.push_str(
                &item["summary"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|part| part["text"].as_str())
                    .collect::<String>(),
            ),
            "function_call" => result.tool_calls.push(parse_call(item)?),
            _ => {}
        }
    }
    result.prompt_tokens = value["usage"]["input_tokens"].as_u64().unwrap_or(0);
    result.completion_tokens = value["usage"]["output_tokens"].as_u64().unwrap_or(0);
    Ok(result)
}

fn parse_call(item: &Value) -> anyhow::Result<ToolCall> {
    let id = item["call_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Responses 工具调用缺少 call_id"))?;
    let name = item["name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Responses 工具调用缺少 name"))?;
    let arguments = serde_json::from_str(item["arguments"].as_str().unwrap_or(""))
        .map_err(|_| anyhow::anyhow!("Responses 工具参数不是有效 JSON"))?;
    Ok(ToolCall {
        id: id.into(),
        name: name.into(),
        arguments,
    })
}

fn complete_text(
    current: &mut String,
    final_text: String,
    tx: &UnboundedSender<StreamDelta>,
    thinking: bool,
) -> anyhow::Result<()> {
    if final_text.is_empty() {
        return Ok(());
    }
    anyhow::ensure!(
        final_text.starts_with(current.as_str()),
        "Responses 完成内容与已接收增量不一致"
    );
    let suffix = &final_text[current.len()..];
    if !suffix.is_empty() {
        let _ = tx.send(if thinking {
            StreamDelta::Thinking(suffix.into())
        } else {
            StreamDelta::Text(suffix.into())
        });
    }
    *current = final_text;
    Ok(())
}

#[derive(Default)]
pub(super) struct ResponsesStream {
    result: ChatResponse,
    calls: std::collections::BTreeMap<u64, (String, String, String)>,
    completed: bool,
}

impl ResponsesStream {
    pub(super) fn frame(
        &mut self,
        value: &Value,
        tx: &UnboundedSender<StreamDelta>,
    ) -> anyhow::Result<()> {
        let kind = value["type"].as_str().unwrap_or("");
        let index = value["output_index"].as_u64().unwrap_or(0);
        match kind {
            "response.output_text.delta" | "response.refusal.delta" => {
                let delta = value["delta"].as_str().unwrap_or("");
                self.result.content.push_str(delta);
                let _ = tx.send(StreamDelta::Text(delta.into()));
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let delta = value["delta"].as_str().unwrap_or("");
                self.result.reasoning.push_str(delta);
                let _ = tx.send(StreamDelta::Thinking(delta.into()));
            }
            "response.output_item.added" if value["item"]["type"] == "function_call" => {
                let item = &value["item"];
                self.calls.insert(
                    index,
                    (
                        item["call_id"].as_str().unwrap_or("").into(),
                        item["name"].as_str().unwrap_or("").into(),
                        item["arguments"].as_str().unwrap_or("").into(),
                    ),
                );
            }
            "response.function_call_arguments.delta" => self
                .calls
                .entry(index)
                .or_default()
                .2
                .push_str(value["delta"].as_str().unwrap_or("")),
            "response.output_item.done" if value["item"]["type"] == "function_call" => {
                let item = &value["item"];
                self.calls.insert(
                    index,
                    (
                        item["call_id"].as_str().unwrap_or("").into(),
                        item["name"].as_str().unwrap_or("").into(),
                        item["arguments"].as_str().unwrap_or("{}").into(),
                    ),
                );
            }
            "response.function_call_arguments.done" => {
                if let Some(arguments) = value["arguments"].as_str() {
                    self.calls.entry(index).or_default().2 = arguments.into();
                }
            }
            "response.completed" | "response.done" => {
                anyhow::ensure!(
                    value["response"]["status"]
                        .as_str()
                        .is_none_or(|s| s == "completed"),
                    "Responses 上游未完成响应"
                );
                let parsed = parse(&value["response"])?;
                complete_text(&mut self.result.content, parsed.content, tx, false)?;
                complete_text(&mut self.result.reasoning, parsed.reasoning, tx, true)?;
                self.result.tool_calls = parsed.tool_calls;
                self.result.prompt_tokens = parsed.prompt_tokens;
                self.result.completion_tokens = parsed.completion_tokens;
                self.completed = true;
            }
            "error" | "response.failed" | "response.incomplete" => {
                anyhow::bail!("Responses 请求失败（{kind}）：上游未完成响应");
            }
            _ => {}
        }
        Ok(())
    }
    pub(super) fn finish(mut self) -> anyhow::Result<ChatResponse> {
        anyhow::ensure!(
            self.completed,
            "Responses 流未收到 response.completed，响应不完整"
        );
        if self.result.tool_calls.is_empty() {
            self.result.tool_calls = self
                .calls
                .into_values()
                .map(|(id, name, args)| {
                    parse_call(&json!({"call_id":id,"name":name,"arguments":args}))
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
        }
        anyhow::ensure!(
            !self.result.content.trim().is_empty() || !self.result.tool_calls.is_empty(),
            "Responses 上游返回空响应"
        );
        Ok(self.result)
    }
}
