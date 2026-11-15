//! runtime 域分文件（自 runtime.rs 按域拆出，纯搬移零行为变更）。

use super::*;

impl AgentRuntime {
    /// `deadline` = 派发时限（Some = execute 路径按 DispatchOrder 预算收口；None = 不限时）。
    /// 挂死的 SSE 连接在剩余预算内等不到收尾时：abort 流任务并报可诊断错误，节点不被无限拖住。
    pub(crate) async fn call_llm<F>(
        &self,
        messages: &[ChatMessage],
        hint: &Option<String>,
        chain: &crate::provider::ModelChain,
        tools: &[crate::provider::ToolSpec],
        deadline: Option<std::time::Instant>,
        on_delta: &mut F,
    ) -> anyhow::Result<(String, Vec<ToolCall>, (u64, u64))>
    where
        F: FnMut(StreamDelta),
    {
        // 候选链为空 → 运行时默认（全局生效档案）
        let mut candidates: Vec<(String, Arc<dyn LlmProvider>, String)> = chain.candidates.clone();
        if candidates.is_empty() {
            let (p, m) = self.default_target();
            candidates.push((String::new(), p, m));
        }
        let mut last_err: Option<anyhow::Error> = None;
        for (pid, provider, model) in candidates {
            let mut req = ChatRequest::new(model.clone(), messages.to_vec());
            req.key_hint = hint.clone();
            if !tools.is_empty() {
                req.tools = tools.to_vec();
            }
            let (tx, mut rx) = unbounded_channel::<StreamDelta>();
            let req_clone = req.clone();
            let stream_provider = provider.clone();

            // 注意：tx 必须被 move 进任务（不能保留原 sender），否则 rx 永不关闭 → 自锁
            let handle = tokio::spawn(async move { stream_provider.stream(req_clone, tx).await });

            let mut emitted = false;
            // 有界收增量：到点（deadline）或流自然关闭即出循环；超时路径 abort 流任务，
            // 连接随任务取消而释放（不残留读端拖住运行时收尾）
            let drain = async {
                while let Some(delta) = rx.recv().await {
                    emitted = true;
                    on_delta(delta);
                }
            };
            match deadline {
                Some(dl) => {
                    if tokio::time::timeout_at(tokio::time::Instant::from_std(dl), drain)
                        .await
                        .is_err()
                    {
                        handle.abort();
                        return Err(anyhow::anyhow!(
                            "LLM 调用超时（候选 {pid}/{model}）：时限内未完成流式响应，已中止"
                        ));
                    }
                }
                None => drain.await,
            }
            match handle.await {
                Ok(Ok(resp)) => {
                    if !pid.is_empty() {
                        chain.failover.clear(&pid);
                    }
                    if resp.content.trim().is_empty() && resp.tool_calls.is_empty() {
                        // 流式空响应退化非流式
                        let fallback = provider.chat(req).await?;
                        if !fallback.reasoning.is_empty() {
                            on_delta(StreamDelta::Thinking(fallback.reasoning.clone()));
                        }
                        on_delta(StreamDelta::Text(fallback.content.clone()));
                        let usage = (fallback.prompt_tokens, fallback.completion_tokens);
                        return Ok((fallback.content, fallback.tool_calls, usage));
                    }
                    let usage = (resp.prompt_tokens, resp.completion_tokens);
                    return Ok((resp.content, resp.tool_calls, usage));
                }
                Ok(Err(e)) => {
                    // 已发出 token 的流不再重试（避免重复输出）；Mock 守卫错误直抛
                    if emitted || e.to_string().contains("未配置 API Key") {
                        return Err(e);
                    }
                    if !pid.is_empty() {
                        chain
                            .failover
                            .cool(&pid, crate::provider::FAILOVER_COOLDOWN_SECS);
                    }
                    last_err = Some(e);
                }
                Err(e) => return Err(anyhow::anyhow!("流式任务失败: {e}")),
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("无可用模型候选")))
    }
}
