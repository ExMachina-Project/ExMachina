//! tools/research.rs —— 深度研究编排（deep-research 4.1/4.2/4.3）
//!
//! 单工具长任务：topic → 研究计划（LLM 拆解子问题）→ spawn 并行检索
//! （每子问题一个，只读白名单 web_search/web_fetch/read）→ 引用聚合
//! （第二次 LLM：URL 规范化建序号表、正文 [n] 内联、无法溯源标综合推断）
//! → 四节 Markdown 报告落 `.exmachina/outbox/` 登记 file 产物。
//!
//! 护栏：无可用搜索后端明确报错（不产出空报告）；工具级总超时 10min；
//! 伪造引用编号在聚合后校验（超出序号表的 [n] 改标综合推断）。

use super::*;

/// depth 三档扇出参数（子问题数上限）
fn depth_questions(depth: &str) -> (usize, u64) {
    match depth {
        "quick" => (2, 240),
        "deep" => (6, 600),
        _ => (4, 360), // standard 缺省
    }
}

/// 工具级总超时上限（10min）：单档超时的硬顶（防 depth=deep + 慢后端拖死会话）
const RESEARCH_TOTAL_TIMEOUT_SECS: u64 = 600;

/// URL 规范化（引用合并键）：去 fragment、去尾斜杠、http→https 不合并（语义可不同）。
/// 够用即可——完整 canonicalization 需要的解析面超出本工具职责。
fn normalize_url(u: &str) -> String {
    let mut s = u.trim().to_string();
    if let Some(i) = s.find('#') {
        s.truncate(i);
    }
    while s.ends_with('/') {
        s.pop();
    }
    s.to_lowercase()
}

/// 从聚合产出正文中提取引用编号（[n]），校验是否都在序号表内。
/// 超出的编号 = 模型伪造 → 替换为「综合推断」标注（不得编造引用编号）。
fn clamp_citations(body: &str, max_n: usize) -> String {
    let mut out = body.to_string();
    // 逐个 [n] 检查（倒序替换避免位置漂移）
    let mut refs: Vec<(usize, usize, usize)> = Vec::new(); // (start, end, n)
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            if let Some(close) = body[i + 1..].find(']') {
                let inner = &body[i + 1..i + 1 + close];
                if let Ok(n) = inner.trim().parse::<usize>() {
                    refs.push((i, i + 1 + close + 1, n));
                }
                i = i + 1 + close + 1;
                continue;
            }
        }
        i += 1;
    }
    for (start, end, n) in refs.iter().rev() {
        if *n == 0 || *n > max_n {
            out.replace_range(start..end, "（综合推断）");
        }
    }
    out
}

/// slug 化主题（文件名安全）：非字母数字（含 CJK 保留）转 -
fn slugify(topic: &str) -> String {
    let mut s: String = topic
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let s = s.trim_matches('-').to_string();
    let s = if s.is_empty() { "research".to_string() } else { s };
    s.chars().take(40).collect()
}

impl ToolGateway {
    pub(crate) async fn tool_research_report(
        &self,
        agent_id: &str,
        session_id: &str,
        args: &serde_json::Value,
    ) -> ToolResult {
        let topic = args
            .get("topic")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if topic.is_empty() {
            return ToolResult::err("topic 不能为空（研究主题）");
        }
        let depth = args
            .get("depth")
            .and_then(|v| v.as_str())
            .unwrap_or("standard")
            .trim()
            .to_string();
        let depth = match depth.as_str() {
            "quick" | "standard" | "deep" => depth,
            other => {
                return ToolResult::err(format!(
                    "不支持的 depth「{other}」（可选 quick / standard / deep）"
                ))
            }
        };
        // 护栏：无可用搜索后端明确报错（不产出空报告）
        if !self.search_ready() {
            return ToolResult::err(
                "检索后端不可用：research_report 需要至少一个可用的联网搜索后端（在「模型与搜索」设置里配置）",
            );
        }
        let Some((provider, model)) = self.research_llm.read().clone() else {
            return ToolResult::err("研究通道未装配（research_llm 未注入）");
        };
        let Some(exec) = self.spawn_executor.read().clone() else {
            return ToolResult::err("spawn 执行器未装配（运行时未就绪）");
        };
        let (max_questions, per_stage_timeout) = depth_questions(&depth);
        let total_timeout = RESEARCH_TOTAL_TIMEOUT_SECS.min(
            per_stage_timeout * 2 + 60, // 两阶段 LLM + 检索窗口
        );

        // ---- 阶段 1：研究计划（LLM 单次调用产出子问题 JSON） ----
        self.emit_stage(session_id, "planning", &topic, &depth);
        let plan_prompt = format!(
            "你是研究规划师。把研究主题拆解为最多 {max_questions} 个互相独立、可并行检索的子问题。\
只输出 JSON 数组（字符串元素），不要任何其他文字。\n研究主题：{topic}"
        );
        let plan_resp = match provider
            .chat(crate::provider::ChatRequest::new(
                model.clone(),
                vec![crate::provider::ChatMessage::user(plan_prompt)],
            ))
            .await
        {
            Ok(r) => r,
            Err(e) => return ToolResult::err(format!("研究计划生成失败：{e}")),
        };
        let questions: Vec<String> = match parse_json_array(&plan_resp.content) {
            Some(qs) if !qs.is_empty() => qs,
            _ => {
                return ToolResult::err(format!(
                    "研究计划解析失败（非 JSON 数组）：{}",
                    plan_resp.content.chars().take(120).collect::<String>()
                ))
            }
        };
        let questions: Vec<String> = questions
            .into_iter()
            .map(|q| q.trim().to_string())
            .filter(|q| !q.is_empty())
            .take(max_questions)
            .collect();

        // ---- 阶段 2：并行检索（每子问题一个 spawn，只读白名单） ----
        self.emit_stage(session_id, "searching", &topic, &depth);
        let mut futures = Vec::new();
        for q in &questions {
            let exec = exec.clone();
            let agent = agent_id.to_string();
            let session = session_id.to_string();
            let task = format!(
                "联网检索并回捞正文要点。研究子问题：{q}\n\
要求：用 web_search 检索（必要时 web_fetch 抓取关键页面正文）；\
产出要点清单，每个要点后附来源 URL（格式：`要点文字 [来源: URL]`）；\
只输出有来源支撑的要点，没有可靠来源就写「（本子问题未找到可靠来源）」。"
            );
            let tools = vec![
                ToolName::WebSearch,
                ToolName::WebFetch,
                ToolName::Read,
            ];
            futures.push(async move {
                let fut = exec(&agent, &session, &task, tools);
                match tokio::time::timeout(
                    std::time::Duration::from_secs(per_stage_timeout),
                    fut,
                )
                .await
                {
                    Ok(Ok(text)) => (q.clone(), text),
                    Ok(Err(e)) => (q.clone(), format!("（子任务失败：{e}）")),
                    Err(_) => (q.clone(), "（子任务超时）".into()),
                }
            });
        }
        let results = match tokio::time::timeout(
            std::time::Duration::from_secs(total_timeout),
            futures_util::future::join_all(futures),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => {
                return ToolResult::err(format!(
                    "研究总超时（{total_timeout} 秒）：检索阶段过慢，可降低 depth 或稍后重试"
                ))
            }
        };

        // ---- 阶段 3：引用聚合（第二次 LLM：URL 规范化建序号表 + [n] 内联） ----
        self.emit_stage(session_id, "synthesizing", &topic, &depth);
        // 先从子任务产出提取全部 URL 建规范序号表（同 URL 合并单编号）
        let mut url_table: Vec<(String, String)> = Vec::new(); // (原始 URL, 规范化)
        for (_q, text) in &results {
            for u in extract_urls(text) {
                let norm = normalize_url(&u);
                if !url_table.iter().any(|(_, n)| *n == norm) {
                    url_table.push((u, norm));
                }
            }
        }
        let numbered_sources: Vec<String> = url_table
            .iter()
            .enumerate()
            .map(|(i, (u, _))| format!("[{}] {}", i + 1, u))
            .collect();
        let sources_block = if numbered_sources.is_empty() {
            "（无）".to_string()
        } else {
            numbered_sources.join("\n")
        };
        let research_block = results
            .iter()
            .map(|(q, text)| format!("## 子问题：{q}\n{text}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let synth_prompt = format!(
            "你是研究综合分析师。基于下列子任务检索产出，撰写主题研究报告。\n\n\
规则：\n1. 正文事实性论断用内联引用标记 [n]，n 对应来源清单编号（清单在下方）；\n\
2. 只引用清单内的编号，不得编造编号；无法溯源到具体来源的结论标注（综合推断）；\n\
3. 输出 Markdown，含四节，节标题固定：`## 结论摘要`、`## 分主题详述`、`## 来源清单`、`## 研究方法`；\n\
4. 来源清单节原样输出下方「来源编号表」（保持编号与 URL 不变）；\n\
5. 结论摘要 ≤ 5 句。\n\n研究主题：{topic}\n\n来源编号表：\n{sources_block}\n\n\
子任务检索产出：\n{research_block}"
        );
        let synth_resp = match provider
            .chat(crate::provider::ChatRequest::new(
                model.clone(),
                vec![crate::provider::ChatMessage::user(synth_prompt)],
            ))
            .await
        {
            Ok(r) => r,
            Err(e) => return ToolResult::err(format!("报告聚合失败：{e}")),
        };
        // 伪造引用校验：超出序号表的 [n] 改标综合推断
        let body = clamp_citations(&synth_resp.content, url_table.len());
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let report = format!(
            "# 深度研究报告：{topic}\n\n> depth: {depth} · 生成: {today} · 子问题 {} 个 · 来源 {} 条\n\n\
{body}\n\n---\n*本报告由 research_report 工具自动编排生成（计划 → 并行检索 → 引用聚合）。*\n",
            questions.len(),
            url_table.len()
        );

        // ---- 阶段 4：产物落盘 + outbox 登记 ----
        self.emit_stage(session_id, "writing", &topic, &depth);
        let dir = self.workspace_root.join(".exmachina/outbox");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return ToolResult::err(format!("创建发件箱目录失败：{e}"));
        }
        let fname = format!("research-{}-{}.md", slugify(&topic), today);
        let abs = dir.join(&fname);
        if let Err(e) = std::fs::write(&abs, &report) {
            return ToolResult::err(format!("报告落盘失败：{e}"));
        }
        let rel = format!(".exmachina/outbox/{fname}");
        let caption = format!("深度研究报告：{topic}（{depth}）");
        if let Err(e) = self.store.outbox_push(session_id, "file", &rel, &caption) {
            return ToolResult::err(format!("发件箱登记失败：{e}"));
        }
        // 结论摘要随工具结果回流（收束 statements 摘要可见）
        let summary = body
            .split("## 结论摘要")
            .nth(1)
            .and_then(|s| s.split("## ").next())
            .unwrap_or("")
            .trim()
            .to_string();
        self.emit_and_persist(CoreEvent {
            kind: "research.finished".into(),
            session_id: session_id.to_string(),
            payload: serde_json::json!({
                "topic": topic, "depth": depth,
                "questions": questions.len(), "sources": url_table.len(),
                "file": rel,
            }),
        });
        ToolResult::ok(format!(
            "研究报告已生成并登记投递：{fname}\n主题：{topic}（depth={depth}）\n\
子问题 {} 个 · 来源 {} 条\n\n## 结论摘要\n{summary}",
            questions.len(),
            url_table.len()
        ))
    }

    /// 阶段切换事件（research.stage：WebUI/CLI 呈现研究进行态）
    fn emit_stage(&self, session_id: &str, stage: &str, topic: &str, depth: &str) {
        self.emit_and_persist(CoreEvent {
            kind: "research.stage".into(),
            session_id: session_id.to_string(),
            payload: serde_json::json!({
                "stage": stage, "topic": topic, "depth": depth,
            }),
        });
    }
}

/// 从文本提取 URL（子任务产出里的 [来源: URL] 与裸 URL 都要抓）
fn extract_urls(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for tok in text.split_whitespace() {
        let tok = tok.trim_matches(|c: char| c == '[' || c == ']' || c == '`' || c == '(' || c == ')');
        if (tok.starts_with("http://") || tok.starts_with("https://"))
            && !out.contains(&tok.to_string())
        {
            out.push(tok.to_string());
        }
    }
    out
}

/// 宽松 JSON 数组解析（LLM 输出可能裹 ```json 围栏或前后缀文字）
fn parse_json_array(text: &str) -> Option<Vec<String>> {
    let start = text.find('[')?;
    let end = text.rfind(']')?;
    let parsed: serde_json::Value =
        serde_json::from_str(&text[start..=end]).ok()?;
    parsed
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
}

#[cfg(test)]
mod research_tests {
    use super::*;

    /// 计划解析：裸数组 / 围栏包裹 / 前后缀文字都能解出；非数组 None
    #[test]
    fn 研究计划_宽松json解析() {
        assert_eq!(
            parse_json_array(r#"["子问题1","子问题2"]"#),
            Some(vec!["子问题1".into(), "子问题2".into()])
        );
        assert_eq!(
            parse_json_array("```json\n[\"a\",\"b\"]\n```"),
            Some(vec!["a".into(), "b".into()])
        );
        assert_eq!(
            parse_json_array("好的，以下是子问题：\n[\"x\"]\n希望有帮助"),
            Some(vec!["x".into()])
        );
        assert_eq!(parse_json_array("没有任何数组"), None);
    }

    /// 引用合并：同 URL（含 fragment/尾斜杠差异）规范化后单编号；
    /// URL 提取覆盖 [来源: URL] 与裸 URL 两种形态
    #[test]
    fn 引用管理_同url合并与提取() {
        let text = "要点A [来源: https://example.com/page] 继续 要点B [来源: https://example.com/page/#sec] 要点C (https://other.com/x/)";
        let urls = extract_urls(text);
        assert_eq!(urls.len(), 3, "提取三个原始 URL");
        // 规范化：example 两形态合并，other 保留
        let mut table: Vec<String> = Vec::new();
        for u in &urls {
            let n = normalize_url(u);
            if !table.contains(&n) {
                table.push(n);
            }
        }
        assert_eq!(table.len(), 2, "同 URL 合并单编号");
        assert_eq!(table[0], "https://example.com/page");
        assert_eq!(table[1], "https://other.com/x");
    }

    /// 伪造引用拦截：超出序号表的 [n] 改标综合推断；表内编号保留
    #[test]
    fn 引用管理_伪造编号拦截() {
        let body = "论断一[1] 论断二[2] 论断三[99] 论断四[0]";
        let out = clamp_citations(body, 2);
        assert!(out.contains("论断一[1]"), "表内编号保留");
        assert!(out.contains("论断二[2]"), "表内编号保留");
        assert!(out.contains("论断三（综合推断）"), "超表编号拦截：{out}");
        assert!(out.contains("论断四（综合推断）"), "编号 0 拦截：{out}");
    }

    /// slug：中文主题保留、特殊字符转 -、空主题回落
    #[test]
    fn 文件名_slug() {
        assert_eq!(slugify("深度研究：AI 智能体"), "深度研究-AI-智能体");
        assert_eq!(slugify("!!!"), "research");
        assert_eq!(slugify(&"很".repeat(50)).chars().count(), 40, "40 字符截断");
    }

    /// depth 三档扇出参数
    #[test]
    fn depth_三档参数() {
        assert_eq!(depth_questions("quick"), (2, 240));
        assert_eq!(depth_questions("standard"), (4, 360));
        assert_eq!(depth_questions("deep"), (6, 600));
        // 缺省与未知走 standard
        assert_eq!(depth_questions(""), (4, 360));
    }
}

#[cfg(test)]
mod research_tool_tests {
    use super::super::*;
    use crate::store::Store;
    use crate::types::Tier;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TestRig {
        gw: ToolGateway,
        store: Arc<Store>,
        dir: std::path::PathBuf,
    }

    fn rig() -> TestRig {
        rig_with_search(false)
    }

    /// rig_with_search(true) = 配 tavily 后端（ready 门控通过；检索走 spawn
    /// 替身不真发请求）；false = 无后端（报错路径）
    fn rig_with_search(with_backend: bool) -> TestRig {
        let dir = std::env::temp_dir().join(format!("exm-rs-{}", crate::types::new_id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let store = Arc::new(Store::open(dir.join("data")).unwrap());
        let registry = Arc::new(LocalRegistry::new(dir.join("agents")).unwrap());
        registry
            .create_group(Some("t".into()), "测试组", "")
            .unwrap();
        let _ = registry.upsert_agent(
            "t",
            crate::types::AgentDefinition {
                name: "orch-1".into(),
                identifier: "orch-1".into(),
                domain: "测试".into(),
                tier: Tier::Orchestrator,
                description: "测试主智能体".into(),
                capabilities: vec![],
                tools: vec![],
                when_to_call: String::new(),
                link: None,
                max_steps: None,
                dependencies: vec![],
                composable_with: vec![],
                input_schema: Default::default(),
                output_schema: Default::default(),
                prompt_file: String::new(),
                model_hint: None,
            },
            None,
        );
        registry.set_primary("t", "orch-1").unwrap();
        registry.set_active_group("t").unwrap();
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let mut gw = ToolGateway::new(&dir, store.clone(), registry, events, SecurityConfig::default());
        if with_backend {
            gw = gw.with_search(crate::config::SearchConfig {
                backends: Vec::new(),
                provider: "tavily".into(),
                endpoint: String::new(),
                api_key: "test-key".into(),
                max_results: 5,
                content_fetch_n: 3,
                proactive: false,
            });
        }
        TestRig { gw, store, dir }
    }

    /// 可编排 mock LLM：按调用序返回预置响应（第一次 = 计划，第二次 = 聚合）
    struct ScriptedProvider {
        responses: Vec<&'static str>,
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl crate::provider::LlmProvider for ScriptedProvider {
        fn name(&self) -> &'static str {
            "scripted"
        }
        async fn chat(
            &self,
            _req: crate::provider::ChatRequest,
        ) -> anyhow::Result<crate::provider::ChatResponse> {
            let i = self.calls.fetch_add(1, Ordering::SeqCst);
            let content = self
                .responses
                .get(i.min(self.responses.len() - 1))
                .unwrap_or(&"");
            Ok(crate::provider::ChatResponse {
                content: content.to_string(),
                reasoning: String::new(),
                prompt_tokens: 10,
                completion_tokens: 20,
                tool_calls: Vec::new(),
            })
        }
        async fn stream(
            &self,
            _req: crate::provider::ChatRequest,
            _tx: tokio::sync::mpsc::UnboundedSender<crate::provider::StreamDelta>,
        ) -> anyhow::Result<crate::provider::ChatResponse> {
            anyhow::bail!("scripted 不支持流式")
        }
    }

    /// 全链路：mock 计划（2 子问题）→ spawn 并行回流（带 URL）→ 聚合产出四节报告
    /// → 落盘登记 file 产物 → 工具结果含结论摘要
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 研究工具_mock全链路() {
        let rig = rig_with_search(true);
        // 计划：两个子问题；聚合：四节报告（含表内 [1] 引用 + 一个伪造 [7]）
        rig.gw.set_research_llm(
            Arc::new(ScriptedProvider {
                responses: vec![
                    "[\"子问题A：市场规模\",\"子问题B：竞争格局\"]",
                    "## 结论摘要\n市场快速增长。\n\n## 分主题详述\n市场规模达百亿[1]。竞争集中[7]。\n\n## 来源清单\n[1] https://example.com/market\n\n## 研究方法\n并行检索后聚合。",
                ],
                calls: AtomicUsize::new(0),
            }),
            "test-model".into(),
        );
        // spawn 替身：记录白名单与任务数，回流带来源 URL
        let spawned = Arc::new(AtomicUsize::new(0));
        let s2 = spawned.clone();
        rig.gw.set_spawn_executor(Arc::new(move |_a, _s, task, tools| {
            let spawned = s2.clone();
            Box::pin(async move {
                spawned.fetch_add(1, Ordering::SeqCst);
                // 白名单断言素材：只读三件套
                assert!(
                    tools.contains(&ToolName::WebSearch)
                        && tools.contains(&ToolName::WebFetch)
                        && !tools.contains(&ToolName::Terminal),
                    "spawn 白名单必须是只读检索面"
                );
                let _ = task;
                Ok("要点：相关数据 [来源: https://example.com/market]".to_string())
            })
        }));
        let r = rig
            .gw
            .execute(
                "t",
                "s-rs",
                &[ToolName::ResearchReport],
                ToolName::ResearchReport,
                &serde_json::json!({ "topic": "AI 智能体市场", "depth": "quick" }),
            )
            .await;
        assert!(r.ok, "全链路成功：{}", r.error.unwrap_or_default());
        assert_eq!(spawned.load(Ordering::SeqCst), 2, "两个子问题各派发一次");
        assert!(r.output.contains("结论摘要"), "结果含结论摘要");
        assert!(r.output.contains("research-"), "回显报告文件名");
        // 产物登记
        let outbox = rig.store.outbox_drain("s-rs").unwrap();
        assert_eq!(outbox.len(), 1);
        assert_eq!(outbox[0]["kind"], "file");
        assert!(
            outbox[0]["path"].as_str().unwrap_or("").ends_with(".md"),
            "报告为 .md：{}",
            outbox[0]["path"]
        );
        // 报告内容：伪造 [7] 被拦截改标综合推断（path 相对 workspace root）
        let path = outbox[0]["path"].as_str().unwrap();
        let report = std::fs::read_to_string(rig.dir.join(path)).unwrap_or_default();
        assert!(report.contains("（综合推断）"), "伪造引用拦截：报告含标注");
        assert!(report.contains("[1] https://example.com/market"), "来源清单保留");
        // 事件：四阶段 + finished（list_events 回 Envelope JSON；CoreEvent 的
        // kind 字段 serde rename 为 "type"，嵌在 Envelope.payload 下）
        let events = rig.store.list_events("s-rs").unwrap_or_default();
        let stages: Vec<String> = events
            .iter()
            .filter(|env| env["payload"]["type"] == "research.stage")
            .filter_map(|env| env["payload"]["payload"]["stage"].as_str().map(String::from))
            .collect();
        assert_eq!(
            stages,
            vec!["planning", "searching", "synthesizing", "writing"]
        );
        assert!(
            events
                .iter()
                .any(|env| env["payload"]["type"] == "research.finished"),
            "完成事件"
        );
    }

    /// 无搜索后端：明确报错不产出空报告
    #[tokio::test]
    async fn 研究工具_无后端明确报错() {
        let rig = rig();
        let r = rig
            .gw
            .execute(
                "t",
                "s-rs2",
                &[ToolName::ResearchReport],
                ToolName::ResearchReport,
                &serde_json::json!({ "topic": "x" }),
            )
            .await;
        assert!(!r.ok);
        assert!(
            r.error.unwrap().contains("检索后端不可用"),
            "无后端明确报错"
        );
        assert!(rig.store.outbox_drain("s-rs2").unwrap().is_empty());
    }

    /// 入参护栏：空 topic / 非法 depth 拒绝
    #[tokio::test]
    async fn 研究工具_入参护栏() {
        let rig = rig();
        let r = rig
            .gw
            .execute(
                "t",
                "s-rs3",
                &[ToolName::ResearchReport],
                ToolName::ResearchReport,
                &serde_json::json!({ "topic": " " }),
            )
            .await;
        assert!(!r.ok && r.error.unwrap().contains("topic"));
        let r = rig
            .gw
            .execute(
                "t",
                "s-rs3",
                &[ToolName::ResearchReport],
                ToolName::ResearchReport,
                &serde_json::json!({ "topic": "x", "depth": "ultra" }),
            )
            .await;
        assert!(!r.ok && r.error.unwrap().contains("depth"));
    }
}
