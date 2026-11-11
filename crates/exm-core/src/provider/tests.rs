//! provider 域测试（自 provider.rs 迁出，纯搬移）。

use super::*;
#[cfg(test)]
mod reasoning_effort_tests {
    use super::*;

    #[test]
    fn 思考强度按协议注入请求体() {
        let p = OpenAiCompatibleProvider::with_format("http://x", vec![], "openai");
        // 显式请求级值 → OpenAI 兼容 reasoning_effort
        let mut req = ChatRequest::new("m", vec![ChatMessage::user("hi")]);
        req.reasoning = Some("high".into());
        assert_eq!(p.body(&req, false)["reasoning_effort"], "high");
        // 未设置且无全局 → 不下发（端点默认，兼容不支持该字段的实现）
        let req = ChatRequest::new("m", vec![ChatMessage::user("hi")]);
        assert!(p.body(&req, false).get("reasoning_effort").is_none());
        // 全局回落：请求未显式设置时取进程级
        set_reasoning_effort(Some("low"));
        let req = ChatRequest::new("m", vec![ChatMessage::user("hi")]);
        assert_eq!(p.body(&req, false)["reasoning_effort"], "low");
        set_reasoning_effort(None);
        let req = ChatRequest::new("m", vec![ChatMessage::user("hi")]);
        assert!(p.body(&req, false).get("reasoning_effort").is_none());
    }

    /// 推理预算硬上限（anti-runaway-thinking）：OpenAI 兼容端点必写 max_tokens
    /// （端点实测：忽略 max_completion_tokens，尊重 max_tokens 的共享预算截断）。
    /// 优先级：请求级显式值 > 进程级 reasoning_token_cap > 默认安全网 16384。
    #[test]
    fn openai_推理预算硬上限_默认安全网与覆盖优先级() {
        let p = OpenAiCompatibleProvider::with_format("http://x", vec![], "openai");
        // 无任何设置 → 默认安全网 16384（拦 9 万 token 级失控心算）
        let req = ChatRequest::new("m", vec![ChatMessage::user("hi")]);
        assert_eq!(p.body(&req, false)["max_tokens"], 16384);
        // 请求级显式值优先
        let mut req = ChatRequest::new("m", vec![ChatMessage::user("hi")]);
        req.max_tokens = Some(500);
        assert_eq!(p.body(&req, false)["max_tokens"], 500);
        // 进程级 cap 覆盖默认安全网
        set_reasoning_token_cap(Some(4096));
        let req = ChatRequest::new("m", vec![ChatMessage::user("hi")]);
        assert_eq!(p.body(&req, false)["max_tokens"], 4096);
        set_reasoning_token_cap(None);
        // 清除后回落安全网
        let req = ChatRequest::new("m", vec![ChatMessage::user("hi")]);
        assert_eq!(p.body(&req, false)["max_tokens"], 16384);
        // anthropic 协议不受此默认网影响（自带 max_tokens=1024 历史默认与 thinking budget 机制）
        let pa = OpenAiCompatibleProvider::with_format("http://x", vec![], "anthropic");
        let req = ChatRequest::new("m", vec![ChatMessage::user("hi")]);
        assert_eq!(pa.body(&req, false)["max_tokens"], 1024);
    }

    #[test]
    fn anthropic_注入thinking预算并抬升max_tokens() {
        let p = OpenAiCompatibleProvider::with_format("http://x", vec![], "anthropic");
        let mut req = ChatRequest::new("m", vec![ChatMessage::user("hi")]);
        req.reasoning = Some("medium".into());
        let body = p.body(&req, false);
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 8192);
        assert!(
            body["max_tokens"].as_u64().unwrap() > 8192,
            "max_tokens 必须大于 thinking 预算"
        );
        // low 档预算
        req.reasoning = Some("low".into());
        assert_eq!(p.body(&req, false)["thinking"]["budget_tokens"], 2048);
    }

    #[test]
    fn gemini_注入thinkingconfig_非法值归一medium() {
        let p = OpenAiCompatibleProvider::with_format("http://x", vec![], "gemini");
        let mut req = ChatRequest::new("m", vec![ChatMessage::user("hi")]);
        req.reasoning = Some("high".into());
        assert_eq!(
            p.body(&req, false)["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            24576
        );
        // 非法值归一为 medium 而非透传（防不支持的自定义档位打 400）
        assert_eq!(normalize_effort("ultrathink"), "medium");
        assert_eq!(normalize_effort(" HIGH "), "high");
    }
}

// ---------------------------------------------------------------- 流解析与 Key 池测试

#[cfg(test)]
mod stream_tests {
    use super::*;

    /// 半包与多字节字符：SSE 帧按任意字节位置切块，内容必须完整无损（回归：逐 chunk
    /// from_utf8_lossy 在 chunk 边界切断中文时产生 U+FFFD）
    #[test]
    fn sse_解码器_多字节字符跨块不损坏() {
        // 正文含中文、emoji、四字节字符：覆盖 2/3/4 字节 UTF-8 序列
        let frame =
            "data: {\"choices\":[{\"delta\":{\"content\":\"中文🦀emoji𝄞与英文 mixed\"}}]}\n\n";
        let payload: String = frame
            .split("content\":\"")
            .nth(1)
            .and_then(|s| s.split("\"").next())
            .map(String::from)
            .unwrap_or_default();
        assert!(!payload.is_empty());

        // 逐字节切块喂入（最苛刻的半包序列），重组结果必须与原文一致
        let bytes = frame.as_bytes();
        let mut dec = SseLineDecoder::new();
        let mut content = String::new();
        for b in bytes {
            for line in dec.feed(std::slice::from_ref(b)) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(
                    line.trim_start_matches("data:").trim(),
                ) {
                    if let Some(t) = v
                        .pointer("/choices/0/delta/content")
                        .and_then(|t| t.as_str())
                    {
                        content.push_str(t);
                    }
                }
            }
        }
        assert_eq!(content, payload, "跨块多字节字符不得产生替换符");
        assert!(!content.contains('\u{FFFD}'), "不得出现 U+FFFD");
    }

    /// 模拟通道流式切块：按字符边界切，重组无损且无 U+FFFD
    /// （回归：此前 `as_bytes().chunks(48)` 会在多字节字符中间断开，
    ///   from_utf8_lossy 把残字替换成「���」，中文模拟输出每 ~48 字节必现）
    #[test]
    fn 模拟通道_流式切块不切碎多字节字符() {
        let text = "背景事实带证据等级、缺口显式列出；不可逆变更与越权调度。🦀 mixed 中英";
        let chunks = char_chunks(text, 48);
        assert!(chunks.len() > 1, "样本应被切成多块");
        for c in &chunks {
            assert!(!c.contains('\u{FFFD}'), "单块不得含替换符：{c}");
        }
        assert_eq!(chunks.concat(), text, "重组必须与原文逐字一致");
        // 每块不超过预算（单字符超预算除外）
        for c in &chunks {
            assert!(c.len() <= 48 + 4, "块过大：{}", c.len());
        }
        // 空串与纯 ASCII
        assert!(char_chunks("", 48).is_empty());
        assert_eq!(char_chunks("abc", 48), vec!["abc".to_string()]);
    }

    /// 半包攒齐才出行：不完整的行不出、拼齐后恰好出一行；残留半行由 flush 兜底
    #[test]
    fn sse_解码器_半包与流尾冲刷() {
        let mut dec = SseLineDecoder::new();
        assert!(dec.feed(b"data: {\"a\"").is_empty(), "半包不应产出行");
        let lines = dec.feed(b":1}\n\nkeep-alive\n");
        assert_eq!(lines, vec![r#"data: {"a":1}"#, "keep-alive"]);
        // 流尾残留（无换行）：flush 必须交出，避免尾部帧静默丢失
        assert!(dec.feed(b"data: tail").is_empty());
        assert_eq!(dec.flush().as_deref(), Some("data: tail"));
        assert_eq!(dec.flush(), None, "冲刷后缓冲应为空");
        // \r\n 行尾：\r 应剥掉
        let mut dec2 = SseLineDecoder::new();
        assert_eq!(dec2.feed(b"data: x\r\n"), vec!["data: x"]);
    }

    /// 流式 Key 池：全部 Key 限额时必须有限次尝试后报错（回归：无限 rotate 挂死调用方）
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 流式_全key限额_有限次轮转后报错() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc as StdArc;

        // 最小 429 服务器：恒定返回限额错误，并统计被请求次数
        let hits = StdArc::new(AtomicUsize::new(0));
        let h2 = hits.clone();
        let app = axum::Router::new().route(
            "/chat/completions",
            axum::routing::post(move || {
                let h = h2.clone();
                async move {
                    h.fetch_add(1, Ordering::SeqCst);
                    (
                        axum::http::StatusCode::TOO_MANY_REQUESTS,
                        axum::Json(serde_json::json!({
                            "error": { "message": "rate limit exceeded", "type": "quota" }
                        })),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        // 双 Key 池：两把都指向恒定 429 的端点
        let p =
            OpenAiCompatibleProvider::new(format!("http://{addr}"), vec!["k1".into(), "k2".into()]);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let started = std::time::Instant::now();
        let err = p
            .stream(ChatRequest::new("m", vec![ChatMessage::user("hi")]), tx)
            .await
            .expect_err("全部 Key 限额应报错");
        let elapsed = started.elapsed();

        assert_eq!(
            hits.load(Ordering::SeqCst),
            2,
            "应恰好尝试 2 次（每把 Key 一次）而非无限轮转"
        );
        assert!(
            err.to_string().contains("限额"),
            "错误应可诊断（说明全部 Key 限额）: {err}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "应快速失败而非挂死，实际 {elapsed:?}"
        );
    }

    /// 流式正常路径：SSE 帧解析 + 正文/思考分轨 + usage 统计（经字节级解码器）
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 流式_帧解析_分轨与用量() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"你好\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"，世界\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"想一想\"}}]}\n\n",
            "data: {\"choices\":[{}],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":7}}\n\n",
            "data: [DONE]\n\n",
        );
        let app = axum::Router::new().route(
            "/chat/completions",
            axum::routing::post(move || {
                let body = body.to_string();
                async move {
                    (
                        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                        body,
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let p = OpenAiCompatibleProvider::new(format!("http://{addr}"), vec!["k".into()]);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let resp = p
            .stream(ChatRequest::new("m", vec![ChatMessage::user("hi")]), tx)
            .await
            .expect("流式请求应成功");

        assert_eq!(resp.content, "你好，世界");
        assert_eq!(resp.reasoning, "想一想");
        assert_eq!(resp.prompt_tokens, 11);
        assert_eq!(resp.completion_tokens, 7);
        // 增量序列：正文与思考分轨（重组一致即可，不约束切块粒度）
        let mut text = String::new();
        let mut think = String::new();
        while let Ok(d) = rx.try_recv() {
            match d {
                StreamDelta::Text(t) => text.push_str(&t),
                StreamDelta::Thinking(t) => think.push_str(&t),
            }
        }
        assert_eq!(text, "你好，世界");
        assert_eq!(think, "想一想");
    }
}

#[cfg(test)]
mod quota_summary_tests {
    use super::*;

    /// 限额错误 → 固定话术：不解析上游内容（各家载荷格式不一），只按状态码
    /// 分叉；用户回复里不得出现任何上游原始文本。
    #[test]
    fn 限额摘要_固定话术不含上游内容() {
        // 真实聚合端点 429 载荷（部署实测）：原始 JSON 不得外发
        let body = r#"{"error":{"message":"[minimax-cn/MiniMax-M3.1-Flash] All minimax-cn accounts have exhausted their quota (reset after 5m)","retry_after":300}}"#;
        let s = OpenAiCompatibleProvider::quota_error_summary(429, body);
        assert!(!s.contains("minimax"), "不得夹带上游内容：{s}");
        assert!(!s.contains("{\"error\""), "不得外发原始 JSON：{s}");
        assert!(s.contains("429") && s.contains("额度"), "按状态码给话术：{s}");
        // 402 余额 / 403 权限各自分叉
        assert!(OpenAiCompatibleProvider::quota_error_summary(402, "x").contains("余额"));
        assert!(OpenAiCompatibleProvider::quota_error_summary(403, "x").contains("权限"));
    }
}
