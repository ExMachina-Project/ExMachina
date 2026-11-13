//! LLM Provider 抽象 —— docs/04 §5
//! 一套代码接所有 OpenAI 兼容端点；无密钥时使用确定性 Mock 通道。

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use parking_lot::Mutex;
use tokio::sync::mpsc::UnboundedSender;

/// 原生 function calling：暴露给模型的工具定义（parameters = JSON Schema 对象）
#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// 内联 `<think>` 分流状态机（provider 不经手，正文增量的前置过滤）。
/// 某些端点（MiniMax-M1 / DeepSeek R1 文本形态）把思考链以 `<think>…</think>`
/// 内联在正文里——本机把它切到思维链轨道，正文只留答案。
/// state：0 = 未判定（前缀不足以判定是否思考开头）1 = 思考中 2 = 正文。
pub mod inline_think {
    const OPEN: &str = "<think>";
    const CLOSE: &str = "</think>";

    /// 输入一段正文增量，返回 (是否思考, 文本) 段列表（可能同时含两轨）。
    pub fn route(state: &mut u8, buf: &mut String, delta: &str) -> Vec<(bool, String)> {
        let mut out: Vec<(bool, String)> = Vec::new();
        match *state {
            2 => {
                if !delta.is_empty() {
                    out.push((false, delta.to_string()));
                }
            }
            0 => {
                buf.push_str(delta);
                if buf.starts_with(OPEN) {
                    *state = 1;
                    let rest = buf[OPEN.len()..].to_string();
                    buf.clear();
                    out.extend(route(state, buf, &rest));
                } else if buf.len() >= OPEN.len() {
                    // 前缀不是 "<think>"：整段正文直通（后续增量不再判）
                    *state = 2;
                    let all = std::mem::take(buf);
                    out.push((false, all));
                }
                // else：不足判定长度，继续攒（流结束时的收尾统一冲刷）
            }
            _ => {
                // 1 = 思考中：找闭合标签；找不到则放出安全前缀（保留可能的半截 "</…"）。
                // 注意 drain 的截断点必须落在字符边界上——按字节回退到最近的合法边界，
                // 否则中文等多字节内容会触发 is_char_boundary 断言（线上已踩）。
                buf.push_str(delta);
                if let Some(pos) = buf.find(CLOSE) {
                    let th = buf[..pos].to_string();
                    let rest = buf[pos + CLOSE.len()..].to_string();
                    buf.clear();
                    *state = 2;
                    if !th.is_empty() {
                        out.push((true, th));
                    }
                    out.extend(route(state, buf, &rest));
                } else {
                    let keep = CLOSE.len().saturating_sub(1).min(buf.len());
                    let mut emit = buf.len() - keep;
                    while emit > 0 && !buf.is_char_boundary(emit) {
                        emit -= 1;
                    }
                    if emit > 0 {
                        let th: String = buf.drain(..emit).collect();
                        out.push((true, th));
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod inline_think_tests {
    use super::inline_think;

    /// 思考中含中文：小步增量喂入，截断点必须落在字符边界（回归：is_char_boundary 断言恐慌）
    #[test]
    fn 思考中多字节字符不恐慌且内容完整() {
        let mut state: u8 = 1; // 已进入思考段
        let mut buf = String::new();
        let mut thinking = String::new();
        let mut text = String::new();
        let payload = "思考中包含中文与 emoji 🎉 以及Mixed内容，用来压边界。";
        // 逐字符小步喂入（模拟 SSE 小增量），反复走 drain 截断路径
        for ch in payload.chars() {
            for (is_think, part) in inline_think::route(&mut state, &mut buf, &ch.to_string()) {
                if is_think {
                    thinking.push_str(&part);
                } else {
                    text.push_str(&part);
                }
            }
        }
        // 收尾冲刷
        if !buf.is_empty() {
            let t = std::mem::take(&mut buf);
            if state == 1 {
                thinking.push_str(&t);
            } else {
                text.push_str(&t);
            }
        }
        assert_eq!(thinking, payload, "思考内容应逐段完整重组");
        assert_eq!(text, "", "未出现闭合标签前不应有正文");
    }

    /// 完整闭环：<think>中文</think>正文 → 思考轨 + 正文轨分离
    #[test]
    fn 完整思考段分离() {
        let mut state: u8 = 0;
        let mut buf = String::new();
        let mut thinking = String::new();
        let mut text = String::new();
        for (is_think, part) in inline_think::route(
            &mut state,
            &mut buf,
            "<think>用户在问\n我是谁？</think>我是 Machina。",
        ) {
            if is_think {
                thinking.push_str(&part);
            } else {
                text.push_str(&part);
            }
        }
        assert_eq!(thinking, "用户在问\n我是谁？");
        assert_eq!(text, "我是 Machina。");
        assert_eq!(state, 2);
    }

    /// 非思考正文直通（前缀判定）
    #[test]
    fn 正文前缀判定直通() {
        let mut state: u8 = 0;
        let mut buf = String::new();
        let mut text = String::new();
        for piece in ["<", "p", ">", "正文"] {
            for (_, part) in inline_think::route(&mut state, &mut buf, piece) {
                text.push_str(&part);
            }
        }
        assert_eq!(text, "<p>正文");
        assert_eq!(state, 2);
    }
}

// ---------------------------------------------------------------- SSE 流解析

/// SSE 字节流 → 完整行的增量解码器。
/// 为什么按字节攒、只在完整行上解码：网络 chunk 边界可能落在多字节 UTF-8 字符中间
/// （中文 / emoji 常见），逐 chunk `from_utf8_lossy` 会把被切断的字符替换成 U+FFFD，
/// 流式正文出现乱码。行边界（`\n`）不可能出现在多字节字符内部，因此「完整行」
/// 一定是合法字符边界，此时解码才安全；残缺行（半包）留在缓冲等下一块拼齐。
pub(crate) struct SseLineDecoder {
    buf: Vec<u8>,
}

impl SseLineDecoder {
    pub(crate) fn new() -> Self {
        SseLineDecoder { buf: Vec::new() }
    }

    /// 喂入一段原始字节，返回其中所有完整行（trim 后非空；不含行尾 `\n` / `\r`）
    pub(crate) fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut lines = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            // 到 \n 为止是完整行：多字节字符不会被行尾截断，此处解码不产生替换符
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line[..line.len() - 1]);
            let line = line.trim_end_matches('\r').trim();
            if !line.is_empty() {
                lines.push(line.to_string());
            }
        }
        lines
    }

    /// 流结束冲刷：残留半行（对端未发换行即断流）按 lossy 解码兜底返回，
    /// 让尾部帧也有机会参与解析（宁可尾帧降级也不静默丢失）
    pub(crate) fn flush(&mut self) -> Option<String> {
        if self.buf.is_empty() {
            return None;
        }
        let rest = std::mem::take(&mut self.buf);
        let line = String::from_utf8_lossy(&rest);
        let line = line.trim_end_matches('\r').trim();
        (!line.is_empty()).then(|| line.to_string())
    }
}

/// 模型发起的一次工具调用
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    /// assistant 消息携带的工具调用（回传线程用）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// tool 结果消息：对应的调用 id
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// tool 结果消息：工具名（gemini functionResponse 需要）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 图片附件（多模态输入）：data URL（data:image/png;base64,…）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        ChatMessage {
            role: "system".into(),
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
            images: Vec::new(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        ChatMessage {
            role: "user".into(),
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
            images: Vec::new(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        ChatMessage {
            role: "assistant".into(),
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
            images: Vec::new(),
        }
    }
    /// 工具结果消息（openai 形态 role=tool；anthropic/gemini 在请求映射时转换）
    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        ChatMessage {
            role: "tool".into(),
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
            name: None,
            images: Vec::new(),
        }
    }

    /// data URL 拆解：(mime, base64)；非 data URL 返回 None
    fn split_data_url(url: &str) -> Option<(String, String)> {
        let rest = url.strip_prefix("data:")?;
        let (meta, data) = rest.split_once(',')?;
        let mime = meta.split(';').next()?.to_string();
        Some((mime, data.to_string()))
    }
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: f32,
    pub max_tokens: Option<u32>,
    /// 发起方标识（个体/指挥体）：多 Key 时按此粘性选键——同发起方不换 Key，
    /// 仅当前 Key 限额（429/402/配额类错误）时前进到下一把并继续粘住。
    pub key_hint: Option<String>,
    /// 原生 function calling：非空即随请求下发
    pub tools: Vec<ToolSpec>,
    /// 模型思考强度（reasoning effort）：low | medium | high；None = 端点默认（不下发）。
    /// 缺省回落进程级 `reasoning_effort()`（网关 /api/llm/effort 设置），请求级显式值优先。
    pub reasoning: Option<String>,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, messages: Vec<ChatMessage>) -> Self {
        ChatRequest {
            model: model.into(),
            messages,
            temperature: 0.2,
            max_tokens: None,
            key_hint: None,
            tools: Vec::new(),
            reasoning: None,
        }
    }

    pub fn with_key_hint(mut self, hint: impl Into<String>) -> Self {
        self.key_hint = Some(hint.into());
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct ChatResponse {
    pub content: String,
    /// 推理内容（思维链）：DeepSeek 系 reasoning_content / Anthropic thinking / Gemini thought
    /// —— 能取到就随响应带回，供渠道单独呈现；不可用时为空串（不承诺所有端点都分离思维链）。
    pub reasoning: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// 模型发起的工具调用（原生 function calling）
    pub tool_calls: Vec<ToolCall>,
}

/// 流式增量：正文与思维链分轨传输——渠道据此分别渲染「回答」与「思考」。
#[derive(Debug, Clone)]
pub enum StreamDelta {
    Text(String),
    Thinking(String),
}

impl StreamDelta {
    pub fn text(&self) -> &str {
        match self {
            StreamDelta::Text(s) => s,
            StreamDelta::Thinking(s) => s,
        }
    }
}


/// 回退候选链：按顺序尝试，失败（未发出内容前）切换下一候选并冷却失败者
pub struct ModelChain {
    pub candidates: Vec<(String, std::sync::Arc<dyn LlmProvider>, String)>,
    pub failover: std::sync::Arc<FailoverState>,
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse>;
    /// 文本嵌入（混合记忆检索）；默认不支持，openai/azure 协议实现
    async fn embed(&self, _model: &str, _texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        anyhow::bail!("该通道不支持嵌入")
    }
    /// 语音转写（音频字节 → 文本）；默认不支持，openai/azure 协议实现
    async fn transcribe(
        &self,
        _model: &str,
        _audio: &[u8],
        _filename: &str,
    ) -> anyhow::Result<String> {
        anyhow::bail!("该通道不支持语音转写")
    }
    /// 语音合成（文本 → mp3 字节）；默认不支持，openai/azure 协议实现
    async fn speak(&self, _model: &str, _text: &str) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!("该通道不支持语音合成")
    }
    /// 图像生成（prompt → 图片字节）；默认不支持，openai 协议实现。
    /// 返回 PNG/JPEG/WebP 任一格式的原始字节（b64_json 优先，URL 回落下载）
    async fn image_generate(&self, _model: &str, _prompt: &str, _size: &str) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!("该通道不支持图像生成")
    }
    /// 流式：增量通过 tx 发出，最终返回完整响应
    async fn stream(
        &self,
        req: ChatRequest,
        tx: UnboundedSender<StreamDelta>,
    ) -> anyhow::Result<ChatResponse>;
}

// ---------------------------------------------------------------- 域分文件

mod compat;
mod effort;
mod mock;
mod pool;
mod stream;

pub use compat::*;
pub use effort::*;
pub use mock::*;
pub use pool::*;
pub(crate) use stream::*;

#[cfg(test)]
mod tests;
