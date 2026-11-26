//! runtime 域测试（自 runtime.rs 迁出，纯搬移；各测试 mod 原样保留）。

#[path = "../../tests/support/agent_acceptance.rs"]
mod acceptance_fixture;

use crate::provider::{ChatRequest, LlmProvider, StreamDelta};
use crate::registry::LocalRegistry;
use crate::runtime::{resolve_max_steps, AgentRuntime};
use crate::tools::ToolGateway;
use crate::types::{AgentDefinition, DispatchOrder, ToolScope};
use std::sync::Arc;

#[cfg(target_os = "linux")]
#[tokio::test]
async fn shared_acceptance_fixture_isolates_and_reaps_processes() {
    let fixture = acceptance_fixture::IsolatedProject::new("shared-process");
    assert!(fixture.project().starts_with(fixture.root()));
    assert!(fixture.exec_home().starts_with(fixture.root()));
    assert!(fixture.bound_project().starts_with(fixture.exec_home()));
    assert!(fixture.outside().starts_with(fixture.root()));
    assert!(fixture.oracle().starts_with(fixture.root()));
    std::fs::write(fixture.oracle().join("expected.txt"), "oracle-only").unwrap();

    let output = fixture
        .spawn_process("sh", &["-c", "printf 'process-fixture-ok\\n'; pwd"])
        .await
        .unwrap()
        .wait_output()
        .await
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("process-fixture-ok"));
    assert!(stdout.contains(&fixture.project().display().to_string()));

    let mut sleeper = fixture
        .spawn_process("sh", &["-c", "exec sleep 30"])
        .await
        .unwrap();
    let pid = sleeper.id().expect("running process fixture has a pid");
    sleeper.terminate().await.unwrap();
    let alive = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(
        !alive.success(),
        "terminated fixture process must be reaped"
    );
}

// ---------------------------------------------------------------- 派发时限测试

#[cfg(test)]
mod timeout_tests {
    use super::*;
    use crate::config::SecurityConfig;
    use crate::provider::{ChatResponse, FailoverState};
    use crate::types::{DispatchBoundary, DispatchConstraints, DispatchInput, DispatchOrder};
    use std::time::Duration;

    /// 挂死流替身：永不产出增量、长眠不醒（模拟端点挂死 / 网络黑洞）
    struct HangingStreamProvider;

    /// 永动工具调用替身（unit-step-budget）：每轮都产出文本协议工具调用、
    /// 从不产出 SyncReport——模拟「预算内一直在干活但收不了束」的个体。
    struct AlwaysToolProvider;

    struct FinishOnBudgetProvider {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl LlmProvider for FinishOnBudgetProvider {
        fn name(&self) -> &'static str {
            "finish-on-budget"
        }

        async fn chat(&self, _request: ChatRequest) -> anyhow::Result<ChatResponse> {
            anyhow::bail!("test requires streaming")
        }

        async fn stream(
            &self,
            request: ChatRequest,
            tx: tokio::sync::mpsc::UnboundedSender<StreamDelta>,
        ) -> anyhow::Result<ChatResponse> {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if call == 0 {
                return AlwaysToolProvider.stream(request, tx).await;
            }
            assert_eq!(call, 1, "only one report-only completion is allowed");
            assert!(
                request.tools.is_empty(),
                "final report must not expose executable tools"
            );
            assert!(request.messages.last().unwrap().content.contains("dry-run"));
            let text = serde_json::json!({
                "sourceAgent":"untrusted", "taskNodeId":"untrusted", "status":"blocked",
                "statements":[{"tag":"报告", "text":"标题和画面已取得；评论仅为 dry-run，未发布"}],
                "summary":"观看材料已取得；评论未发布", "evidence":[], "risks":[],
                "blockers":[{"reason":"dry-run，未实际发布", "unblockCondition":"等待人工确认"}],
                "confidence":0.8
            })
            .to_string();
            let _ = tx.send(StreamDelta::Text(text.clone()));
            Ok(ChatResponse {
                content: text,
                ..Default::default()
            })
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for AlwaysToolProvider {
        fn name(&self) -> &'static str {
            "always-tool"
        }
        async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                content: r#"{"tool": "terminal", "args": {"command": "echo step"}}"#.into(),
                reasoning: String::new(),
                prompt_tokens: 10,
                completion_tokens: 20,
                tool_calls: Vec::new(),
            })
        }
        async fn stream(
            &self,
            _req: ChatRequest,
            tx: tokio::sync::mpsc::UnboundedSender<StreamDelta>,
        ) -> anyhow::Result<ChatResponse> {
            let _ = tx.send(StreamDelta::Text(
                r#"{"tool": "terminal", "args": {"command": "echo step"}}"#.into(),
            ));
            Ok(ChatResponse {
                content: r#"{"tool": "terminal", "args": {"command": "echo step"}}"#.into(),
                reasoning: String::new(),
                prompt_tokens: 10,
                completion_tokens: 20,
                tool_calls: Vec::new(),
            })
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for HangingStreamProvider {
        fn name(&self) -> &'static str {
            "hanging"
        }
        async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok(ChatResponse::default())
        }
        async fn stream(
            &self,
            _req: ChatRequest,
            _tx: tokio::sync::mpsc::UnboundedSender<StreamDelta>,
        ) -> anyhow::Result<ChatResponse> {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok(ChatResponse::default())
        }
    }

    fn runtime_with(provider: Arc<dyn LlmProvider>) -> AgentRuntime {
        let base = std::env::temp_dir().join(format!("exm-rt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let reg_dir = base.join("entities");
        std::fs::create_dir_all(reg_dir.join("prompts")).unwrap();
        let meta = crate::types::GroupMeta {
            id: "exmachina".into(),
            name: "测试组".into(),
            description: String::new(),
            primary: None,
            workspace: None,
            model: None,
            model_routing: None,
            capabilities: None,
            self_optimize: None,
            builtin: true,
            created_at: crate::types::now_iso(),
        };
        std::fs::write(
            reg_dir.join("group.json"),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();
        // 提示词按组布局解析：groups/<gid>/prompts/<file>.md
        let prompts_dir = reg_dir.join("groups").join("exmachina").join("prompts");
        std::fs::create_dir_all(&prompts_dir).unwrap();
        std::fs::write(
            prompts_dir.join("timeout-agent.md"),
            "# 测试个体\n你是测试个体。",
        )
        .unwrap();
        let registry = Arc::new(LocalRegistry::new(&reg_dir).expect("临时注册表创建失败"));
        let store =
            Arc::new(crate::store::Store::open(base.join("data")).expect("临时存储创建失败"));
        let (events, _rx) = tokio::sync::broadcast::channel::<crate::types::CoreEvent>(16);
        let tools = Arc::new(ToolGateway::new(
            &base,
            store,
            registry.clone(),
            events,
            SecurityConfig::default(),
        ));
        AgentRuntime::new(registry, provider, tools, "test-model")
    }

    fn order_with(timeout_ms: u64) -> DispatchOrder {
        DispatchOrder {
            task_node_id: "T1".into(),
            objective: "时限演练".into(),
            acceptance: vec!["产出 SyncReport".into()],
            boundary: DispatchBoundary {
                in_scope: vec![],
                forbidden: vec![],
            },
            inputs: vec![DispatchInput {
                ref_id: "user".into(),
                kind: "userInput".into(),
                summary: "时限演练".into(),
            }],
            tool_allowlist: vec![],
            constraints: DispatchConstraints {
                max_steps: 3,
                timeout_ms,
            },
            report_format: "SyncReport".into(),
            model_selection: None,
        }
    }

    fn def_with() -> AgentDefinition {
        serde_json::from_value(serde_json::json!({
            "name": "时限演练体", "identifier": "timeout-agent", "domain": "公共",
            "tier": "unit", "description": "派发时限演练", "capabilities": ["测试"],
            "promptFile": "timeout-agent.md"
        }))
        .unwrap()
    }

    /// 挂死的 LLM 流必须在派发时限内被掐断并报可诊断错误（回归：timeout_ms 声明无人执行，
    /// 节点无限期占住编排并发槽）
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 派发时限_挂死流被掐断并报错() {
        let rt = runtime_with(Arc::new(HangingStreamProvider));
        let def = def_with();
        let order = order_with(400); // 400ms 时限
        let chain = crate::provider::ModelChain {
            candidates: vec![(String::new(), Arc::new(HangingStreamProvider), "m".into())],
            failover: Arc::new(FailoverState::default()),
        };
        let started = std::time::Instant::now();
        let r = rt
            .execute(
                &def,
                &order,
                chain,
                "ses-timeout",
                "ses-timeout",
                false,
                ToolScope::Computer,
                |_| {},
            )
            .await;
        let elapsed = started.elapsed();
        let err = r.expect_err("挂死流应在时限内失败");
        assert!(
            err.to_string().contains("超时"),
            "错误应可诊断为超时: {err}"
        );
        assert!(
            elapsed >= Duration::from_millis(400) && elapsed < Duration::from_secs(10),
            "应约在时限处失败而非挂满 60s，实际 {elapsed:?}"
        );
    }

    /// 快速正常的 LLM 不应被时限误伤（时限内正常产出 SyncReport → 成功回流）
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 派发时限_正常流不受影响() {
        let rt = runtime_with(Arc::new(crate::provider::MockLlmProvider));
        let def = def_with();
        let order = order_with(10_000);
        let chain = crate::provider::ModelChain {
            candidates: vec![(
                String::new(),
                Arc::new(crate::provider::MockLlmProvider),
                "m".into(),
            )],
            failover: Arc::new(FailoverState::default()),
        };
        let report = rt
            .execute(
                &def,
                &order,
                chain,
                "ses-ok",
                "ses-ok",
                false,
                ToolScope::Computer,
                |_| {},
            )
            .await
            .expect("时限充裕时应正常执行");
        assert_eq!(report.task_node_id, "T1");
        assert_eq!(report.status, crate::types::SyncStatus::Done);
    }

    /// 步数耗尽降级回流（unit-step-budget 回归）：个体预算内一直在干活
    /// （工具调用不断）但没产出 SyncReport——旧行为 bail 一行失败串，
    /// 已完成的全部工作被扔掉；现在应得到 blocked 降级报告（工作如实可见）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 步数耗尽_降级回流不丢工作() {
        let rt = runtime_with(Arc::new(AlwaysToolProvider));
        let def = def_with();
        let mut order = order_with(600_000);
        order.constraints.max_steps = 3; // 小预算快速触底
        let chain = crate::provider::ModelChain {
            candidates: vec![(String::new(), Arc::new(AlwaysToolProvider), "m".into())],
            failover: Arc::new(FailoverState::default()),
        };
        let report = rt
            .execute(
                &def,
                &order,
                chain,
                "ses-budget",
                "ses-budget",
                false,
                ToolScope::Computer,
                |_| {},
            )
            .await
            .expect("步数耗尽应降级回流而非失败");
        assert_eq!(
            report.status,
            crate::types::SyncStatus::Blocked,
            "降级状态应为 blocked"
        );
        assert!(
            report.summary.contains("步数预算耗尽"),
            "摘要应注明预算耗尽: {}",
            report.summary
        );
        assert!(
            !report.blockers.is_empty(),
            "未完成项应进 blockers（指挥体可据实裁决）"
        );
        assert!(
            report.statements.iter().any(|s| s.text.contains("3 轮")),
            "陈述应带预算值: {:?}",
            report.statements
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatched_budget_fallback_uses_child_trace_and_keeps_dry_run_evidence() {
        let rt = runtime_with(Arc::new(AlwaysToolProvider));
        let parent = format!("parent-{}", uuid::Uuid::new_v4());
        let child = format!("child-{}", uuid::Uuid::new_v4());
        let mut order = order_with(60_000);
        order.constraints.max_steps = 1;
        for (index, tool, summary) in [
            ("summary", "video__summary", "标题：已核验的视频标题"),
            ("shots", "video__shots", "已抽取 6 帧画面"),
            ("act", "video__act", "[dry-run] 拟评论（未发送至平台）"),
        ] {
            crate::round_trace::push_tool(
                &child,
                crate::round_trace::ToolCallRecord {
                    call_id: index.into(),
                    agent_id: "timeout-agent".into(),
                    tool: tool.into(),
                    args: serde_json::json!({"secret":"must-not-enter-report"}),
                    ok: true,
                    duration_ms: 1,
                    summary: summary.into(),
                },
            );
        }
        let chain = crate::provider::ModelChain {
            candidates: vec![(String::new(), Arc::new(AlwaysToolProvider), "m".into())],
            failover: Arc::new(FailoverState::default()),
        };
        let report = rt
            .execute(
                &def_with(),
                &order,
                chain,
                &parent,
                &child,
                false,
                ToolScope::Computer,
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(report.status, crate::types::SyncStatus::Blocked);
        assert!(
            !report.summary.contains("未留下工具调用记录"),
            "must read the dispatched child trace"
        );
        assert!(report.summary.contains("video__summary(ok)"));
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(serialized.contains("已抽取 6 帧画面"));
        assert!(serialized.contains("[dry-run]"));
        assert!(serialized.contains("未发送至平台"));
        assert!(!serialized.contains("must-not-enter-report"));
        assert!(report.evidence.iter().all(|item| item
            .reference
            .starts_with(&format!("session:{child}/tool:"))));
        assert!(
            crate::round_trace::tool_count(&child) >= 3,
            "evidence snapshot must not drain child records"
        );
        crate::round_trace::end(&parent);
        crate::round_trace::end(&child);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn exhausted_tool_budget_allows_one_tools_free_report_without_replaying_tools() {
        let provider = Arc::new(FinishOnBudgetProvider {
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let rt = runtime_with(provider.clone());
        let mut order = order_with(60_000);
        order.constraints.max_steps = 1;
        let session = format!("budget-report-{}", uuid::Uuid::new_v4());
        let chain = crate::provider::ModelChain {
            candidates: vec![(String::new(), provider.clone(), "m".into())],
            failover: Arc::new(FailoverState::default()),
        };
        let report = rt
            .execute(
                &def_with(),
                &order,
                chain,
                &session,
                &session,
                false,
                ToolScope::Computer,
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(report.source_agent, "timeout-agent");
        assert_eq!(report.task_node_id, "T1");
        assert_eq!(report.status, crate::types::SyncStatus::Blocked);
        assert_eq!(report.summary, "观看材料已取得；评论未发布");
        crate::round_trace::end(&session);
    }
}

// ---------------------------------------------------------------- 原生工具循环回归

/// 复现「agent 无法正常开发」：原生 function calling 的完整多轮工具循环。
/// 覆盖：同轮只读并发（read+grep）→ 写工具（edit）→ terminal → 工具结果回灌 → SyncReport。
/// Mock 通道只走文本协议，覆盖不到这条路径——真实端点（OpenAI 兼容）全走这里。
#[cfg(test)]
mod tool_loop_tests {
    use super::*;
    use crate::config::SecurityConfig;
    use crate::provider::{ChatResponse, FailoverState, ToolCall};
    use crate::types::{DispatchBoundary, DispatchConstraints, DispatchInput, SyncStatus};
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// 脚本化 Provider：按调用次序回放固定的工具调用序列（模拟真实编码模型的作业轨迹）
    struct ScriptedProvider {
        step: AtomicUsize,
        /// 每步收到的消息列表快照（role 序列），供回灌断言
        seen_roles: Mutex<Vec<Vec<String>>>,
    }

    impl ScriptedProvider {
        fn new() -> Self {
            ScriptedProvider {
                step: AtomicUsize::new(0),
                seen_roles: Mutex::new(Vec::new()),
            }
        }
    }

    struct NativeImageProvider {
        step: AtomicUsize,
        saw_real_image: AtomicBool,
        observed_image_count: AtomicUsize,
        saw_ordered_tool_results: AtomicBool,
        saw_nonvision_downgrade: AtomicBool,
    }

    struct TextImageProvider {
        step: AtomicUsize,
        saw_real_image: AtomicBool,
        saw_nonvision_downgrade: AtomicBool,
    }

    struct LateToolProvider {
        barrier: acceptance_fixture::FaultBarrier,
    }

    struct SteeringWriteProvider {
        step: AtomicUsize,
        root: std::path::PathBuf,
        entered: tokio::sync::mpsc::UnboundedSender<()>,
        release: Arc<tokio::sync::Semaphore>,
        saw_steering: AtomicBool,
        saw_late_steering: AtomicBool,
        saw_written_sentinel: AtomicBool,
    }

    #[async_trait::async_trait]
    impl LlmProvider for LateToolProvider {
        fn name(&self) -> &'static str {
            "late-tool-fixture"
        }

        async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
            anyhow::bail!("fixture uses streaming")
        }

        async fn stream(
            &self,
            _request: ChatRequest,
            _tx: tokio::sync::mpsc::UnboundedSender<StreamDelta>,
        ) -> anyhow::Result<ChatResponse> {
            self.barrier.arrive_and_wait().await;
            Ok(resp(
                "",
                vec![ToolCall {
                    id: "late-write".into(),
                    name: "filesystem".into(),
                    arguments: serde_json::json!({
                        "op": "write",
                        "path": "must-not-exist.txt",
                        "content": "late response"
                    }),
                }],
            ))
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for SteeringWriteProvider {
        fn name(&self) -> &'static str {
            "steering-write-fixture"
        }

        async fn chat(&self, _request: ChatRequest) -> anyhow::Result<ChatResponse> {
            anyhow::bail!("fixture uses streaming")
        }

        async fn stream(
            &self,
            request: ChatRequest,
            _tx: tokio::sync::mpsc::UnboundedSender<StreamDelta>,
        ) -> anyhow::Result<ChatResponse> {
            match self.step.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    self.entered.send(()).unwrap();
                    self.release.acquire().await.unwrap().forget();
                    #[cfg(unix)]
                    let call = ToolCall {
                        id: "write-sentinel".into(),
                        name: "terminal".into(),
                        arguments: serde_json::json!({
                            "command": "sh steering-write.sh",
                            "timeoutSecs": 10
                        }),
                    };
                    #[cfg(not(unix))]
                    let call = ToolCall {
                        id: "write-sentinel".into(),
                        name: "filesystem".into(),
                        arguments: serde_json::json!({
                            "op": "write",
                            "path": "steering-sentinel.txt",
                            "content": "safe-write"
                        }),
                    };
                    Ok(resp("", vec![call]))
                }
                _ => {
                    assert!(
                        self.root.join("steering-sentinel.txt").is_file(),
                        "写工具没有完成：{}",
                        request
                            .messages
                            .iter()
                            .rev()
                            .take(2)
                            .map(|message| message.content.as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    );
                    self.saw_steering.store(
                        request
                            .messages
                            .iter()
                            .any(|message| message.content.contains("改成安全方向")),
                        Ordering::SeqCst,
                    );
                    self.saw_written_sentinel.store(
                        self.root.join("steering-sentinel.txt").is_file(),
                        Ordering::SeqCst,
                    );
                    self.saw_late_steering.store(
                        request
                            .messages
                            .iter()
                            .any(|message| message.content.contains("写入中补充")),
                        Ordering::SeqCst,
                    );
                    let report = serde_json::json!({
                        "sourceAgent": "coding-agent",
                        "taskNodeId": "T1",
                        "status": "done",
                        "statements": [{"tag": "报告", "text": "写入已完成", "evidenceLevel": "B"}],
                        "summary": "写入完成后应用 steering",
                        "evidence": [{"level": "B", "kind": "code", "ref": "steering-sentinel.txt"}],
                        "risks": [],
                        "confidence": 0.9
                    });
                    Ok(resp(&format!("```json\n{report}\n```"), vec![]))
                }
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for NativeImageProvider {
        fn name(&self) -> &'static str {
            "native-image-fixture"
        }

        async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
            anyhow::bail!("fixture uses streaming")
        }

        async fn stream(
            &self,
            request: ChatRequest,
            _tx: tokio::sync::mpsc::UnboundedSender<StreamDelta>,
        ) -> anyhow::Result<ChatResponse> {
            let step = self.step.fetch_add(1, Ordering::SeqCst);
            if step == 0 {
                assert!(
                    request
                        .messages
                        .iter()
                        .all(|message| message.images.is_empty()),
                    "image fixture must not exist before the read tool runs"
                );
                return Ok(resp(
                    "",
                    vec![
                        ToolCall {
                            id: "image-read-a".into(),
                            name: "read".into(),
                            arguments: serde_json::json!({"path": "shot.png"}),
                        },
                        ToolCall {
                            id: "image-read-b".into(),
                            name: "read".into(),
                            arguments: serde_json::json!({"path": "shot-two.png"}),
                        },
                    ],
                ));
            }
            let image_messages: Vec<usize> = request
                .messages
                .iter()
                .enumerate()
                .filter(|(_, message)| !message.images.is_empty())
                .map(|(index, _)| index)
                .collect();
            let real_images = image_messages
                .iter()
                .flat_map(|index| request.messages[*index].images.iter())
                .filter(|url| url.starts_with("data:image/png;base64,") && url.len() > 30)
                .count();
            self.observed_image_count
                .store(real_images, Ordering::SeqCst);
            if real_images > 0 {
                self.saw_real_image.store(true, Ordering::SeqCst);
            }
            if image_messages.len() >= 2 {
                let first_image = image_messages[0];
                let tool_positions: Vec<usize> = request
                    .messages
                    .iter()
                    .enumerate()
                    .filter(|(_, message)| message.role == "tool")
                    .map(|(index, _)| index)
                    .collect();
                if tool_positions.len() == 2
                    && tool_positions.iter().all(|index| *index < first_image)
                {
                    self.saw_ordered_tool_results.store(true, Ordering::SeqCst);
                }
            }
            if image_messages.is_empty()
                && request
                    .messages
                    .iter()
                    .any(|message| message.content.contains("不支持视觉输入能力"))
            {
                self.saw_nonvision_downgrade.store(true, Ordering::SeqCst);
            }
            let report = serde_json::json!({
                "sourceAgent": "coding-agent",
                "taskNodeId": "T1",
                "status": "done",
                "statements": [{"tag":"报告", "text":"已读取真实图片", "evidenceLevel":"B"}],
                "summary": "图像已回灌",
                "evidence": [{"level":"B", "kind":"doc", "ref":"shot.png"}],
                "risks": [],
                "confidence": 0.9
            });
            Ok(resp(&format!("```json\n{report}\n```"), vec![]))
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for TextImageProvider {
        fn name(&self) -> &'static str {
            "text-image-fixture"
        }

        async fn chat(&self, _request: ChatRequest) -> anyhow::Result<ChatResponse> {
            anyhow::bail!("fixture uses streaming")
        }

        async fn stream(
            &self,
            request: ChatRequest,
            _tx: tokio::sync::mpsc::UnboundedSender<StreamDelta>,
        ) -> anyhow::Result<ChatResponse> {
            match self.step.fetch_add(1, Ordering::SeqCst) {
                0 => Ok(resp(
                    r#"{"tool":"read","args":{"path":"shot.png"}}"#,
                    vec![],
                )),
                _ => {
                    self.saw_real_image.store(
                        request.messages.iter().any(|message| {
                            message.images.iter().any(|url| {
                                url.starts_with("data:image/png;base64,") && url.len() > 30
                            })
                        }),
                        Ordering::SeqCst,
                    );
                    self.saw_nonvision_downgrade.store(
                        request
                            .messages
                            .iter()
                            .any(|message| message.content.contains("不支持视觉输入能力")),
                        Ordering::SeqCst,
                    );
                    Ok(resp("文本协议图像回灌完成", vec![]))
                }
            }
        }
    }

    fn resp(content: &str, calls: Vec<ToolCall>) -> ChatResponse {
        ChatResponse {
            content: content.to_string(),
            reasoning: String::new(),
            prompt_tokens: 10,
            completion_tokens: 10,
            tool_calls: calls,
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for ScriptedProvider {
        fn name(&self) -> &'static str {
            "scripted"
        }
        async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
            anyhow::bail!("脚本化通道只走流式")
        }
        async fn stream(
            &self,
            req: ChatRequest,
            _tx: tokio::sync::mpsc::UnboundedSender<StreamDelta>,
        ) -> anyhow::Result<ChatResponse> {
            self.seen_roles
                .lock()
                .push(req.messages.iter().map(|m| m.role.clone()).collect());
            let step = self.step.fetch_add(1, Ordering::SeqCst);
            let s = match step {
                0 => resp(
                    "",
                    vec![
                        ToolCall {
                            id: "c1".into(),
                            name: "read".into(),
                            arguments: serde_json::json!({"path": "src/app.rs"}),
                        },
                        ToolCall {
                            id: "c2".into(),
                            name: "grep".into(),
                            arguments: serde_json::json!({"pattern": "fn main"}),
                        },
                    ],
                ),
                1 => resp(
                    "",
                    vec![ToolCall {
                        id: "c3".into(),
                        name: "edit".into(),
                        arguments: serde_json::json!({
                            "path": "src/app.rs",
                            "old_string": "fn main() {}",
                            "new_string": "fn main() { println!(\"exm-ok\"); }"
                        }),
                    }],
                ),
                2 => resp(
                    "",
                    vec![ToolCall {
                        id: "c4".into(),
                        name: "terminal".into(),
                        arguments: serde_json::json!({"command": "ls"}),
                    }],
                ),
                _ => {
                    let report = serde_json::json!({
                        "sourceAgent": "coding-agent",
                        "taskNodeId": "T1",
                        "status": "done",
                        "statements": [{"tag": "报告", "text": "已完成", "evidenceLevel": "B"}],
                        "summary": "完成",
                        "evidence": [{"level": "B", "kind": "code", "ref": "src/app.rs"}],
                        "risks": [],
                        "confidence": 0.8
                    });
                    return Ok(resp(&format!("```json\n{report}\n```"), vec![]));
                }
            };
            Ok(s)
        }
    }

    fn runtime_with_files(
        provider: Arc<dyn LlmProvider>,
    ) -> (AgentRuntime, acceptance_fixture::IsolatedProject) {
        let fixture = acceptance_fixture::IsolatedProject::new("runtime-loop");
        let base = fixture.project().to_path_buf();
        std::fs::create_dir_all(base.join("src")).unwrap();
        std::fs::write(base.join("src/app.rs"), "fn main() {}\n").unwrap();
        let reg_dir = base.join("entities");
        std::fs::create_dir_all(reg_dir.join("groups/exmachina/prompts")).unwrap();
        let meta = crate::types::GroupMeta {
            id: "exmachina".into(),
            name: "测试组".into(),
            description: String::new(),
            primary: None,
            workspace: None,
            model: None,
            model_routing: None,
            capabilities: None,
            self_optimize: None,
            builtin: true,
            created_at: crate::types::now_iso(),
        };
        std::fs::write(
            reg_dir.join("group.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();
        std::fs::write(
            reg_dir.join("groups/exmachina/prompts/coding-agent.md"),
            "# 测试个体\n你是测试个体。",
        )
        .unwrap();
        let registry = Arc::new(LocalRegistry::new(&reg_dir).expect("临时注册表创建失败"));
        let store =
            Arc::new(crate::store::Store::open(base.join("data")).expect("临时存储创建失败"));
        let (events, _rx) = tokio::sync::broadcast::channel::<crate::types::CoreEvent>(16);
        let tools = Arc::new(ToolGateway::new(
            &base,
            store,
            registry.clone(),
            events,
            SecurityConfig::default(),
        ));
        (
            AgentRuntime::new(registry, provider, tools, "test-model"),
            fixture,
        )
    }

    fn coding_def() -> crate::types::AgentDefinition {
        serde_json::from_value(serde_json::json!({
            "name": "编码体", "identifier": "coding-agent", "domain": "实作域",
            "tier": "unit", "description": "编码", "capabilities": ["编码"],
            "tools": ["read", "grep", "glob", "edit", "patch", "filesystem", "terminal"],
            "promptFile": "coding-agent.md"
        }))
        .unwrap()
    }

    fn coding_order(allowlist: Vec<crate::types::ToolName>) -> DispatchOrder {
        DispatchOrder {
            task_node_id: "T1".into(),
            objective: "在 src/app.rs 加输出".into(),
            acceptance: vec!["文件被修改".into()],
            boundary: DispatchBoundary {
                in_scope: vec![],
                forbidden: vec![],
            },
            inputs: vec![DispatchInput {
                ref_id: "user".into(),
                kind: "userInput".into(),
                summary: "改文件".into(),
            }],
            tool_allowlist: allowlist,
            constraints: DispatchConstraints {
                max_steps: 8,
                timeout_ms: 0,
            },
            report_format: "SyncReport".into(),
            model_selection: None,
        }
    }

    /// 完整编码作业轨迹：只读并发 → edit 落盘 → terminal → 工具结果回灌 → SyncReport 校验通过
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 原生工具循环_编码作业全链路() {
        let provider = Arc::new(ScriptedProvider::new());
        let (rt, fixture) = runtime_with_files(provider.clone());
        let base = fixture.project();
        let def = coding_def();
        let order = coding_order(def.tools.clone());
        let chain = crate::provider::ModelChain {
            candidates: vec![(String::new(), provider.clone(), "m".into())],
            failover: Arc::new(FailoverState::default()),
        };
        let report = rt
            .execute(
                &def,
                &order,
                chain,
                "sess-loop",
                "trace-loop",
                false,
                ToolScope::Computer,
                |_| {},
            )
            .await
            .expect("原生工具循环应完整走通");
        assert_eq!(report.status, SyncStatus::Done);

        // edit 真的落盘了
        let content = std::fs::read_to_string(base.join("src/app.rs")).unwrap();
        assert!(
            content.contains("exm-ok"),
            "edit 应真实落盘，实际: {content}"
        );

        // 工具结果以 role=tool 回灌，assistant 的 tool_calls 有被记录
        let seen = provider.seen_roles.lock();
        assert!(seen.len() >= 4, "应有多轮交互，实际 {} 轮", seen.len());
        let last = seen.last().unwrap();
        assert!(
            last.iter().any(|r| r == "tool"),
            "最终轮消息里应有工具结果回灌，实际 roles: {last:?}"
        );
        assert!(
            last.iter().any(|r| r == "assistant"),
            "应保留 assistant 工具调用记录"
        );
    }

    /// 学习候选证据门的真实入口验收（capability-parity 6.4）。
    ///
    /// 此前候选核验只读「已落盘消息」，而工具回执要到轮次收束才落盘——模型在同一轮内
    /// 提交候选时，本轮真实回执全部核验失败，证据门在生产里形同关闭。本测试走真实
    /// `run_tool_calls`，**不落盘任何消息**，直接用回填给模型的回执 id 提交候选：
    /// - 成功调用：回填文本回显的回执 id == 模型自己的 call id（模型引用得到）；
    /// - 同轮暂存的回执可被核验（tool_receipt 通过、test_result 要求命令具备测试语义）；
    /// - 失败调用：不回显回执 id，且引用它必然核验失败。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 学习候选_同轮回执可核验_且回执标识对模型可见() {
        let provider = Arc::new(crate::provider::MockLlmProvider);
        let (rt, fixture) = runtime_with_files(provider);
        let base = fixture.project().to_path_buf();
        // 真实的「检查通过」命令：Makefile 的 check 目标确实执行并通过
        std::fs::write(base.join("Makefile"), "check:\n\t@echo check-ok\n").unwrap();
        let session = "trace-receipt";
        crate::round_trace::begin(session);
        let allow = vec![
            crate::types::ToolName::Terminal,
            crate::types::ToolName::Read,
        ];
        let calls = vec![
            ToolCall {
                id: "model-call-check".into(),
                name: "terminal".into(),
                arguments: serde_json::json!({ "command": "make check" }),
            },
            ToolCall {
                id: "model-call-fail".into(),
                name: "read".into(),
                arguments: serde_json::json!({ "path": "no-such-file.txt" }),
            },
        ];
        let feedbacks = rt
            .run_tool_calls(
                "coding-agent",
                session,
                &allow,
                &calls,
                false,
                ToolScope::Computer,
            )
            .await;
        let (ok_fb, fail_fb) = (&feedbacks[0].0, &feedbacks[1].0);
        assert!(
            ok_fb.contains("〔回执 model-call-check〕"),
            "成功结果须回显与模型 call id 一致的回执标识，实际：{ok_fb}"
        );
        assert!(!ok_fb.contains("t1"), "不应回显内部生成的 id");
        assert!(
            !fail_fb.contains("〔回执"),
            "失败调用不是证据，不得回显回执标识，实际：{fail_fb}"
        );

        // 落盘记录里的 callId 与模型 call id 对齐（前端工具卡 / 审计 / 模型引用三者一致）
        // （只读并发与串行写分开执行，落盘顺序 ≠ 调用顺序是既有行为；回执按 id 检索，
        // 不依赖顺序——故只断言集合，不断言顺序。）
        let records = crate::round_trace::snapshot_tool_records(session, 10);
        let mut ids: Vec<String> = records.iter().map(|r| r.call_id.clone()).collect();
        ids.sort();
        assert_eq!(
            ids,
            vec![
                "model-call-check".to_string(),
                "model-call-fail".to_string()
            ],
            "落盘 callId 须采用模型 call id（含只读并发分支），不得混入内部生成的 id"
        );

        // 尚无任何消息落盘——证据只能来自本轮暂存
        let store = crate::store::Store::open(base.join("data")).unwrap();
        assert!(
            store.list_messages(session, 0).unwrap().is_empty(),
            "前置：本轮尚未收束落盘"
        );
        let triggers = vec!["检查".to_string()];
        use crate::learning_candidates::{CandidateDraft, CandidateEvidenceKind};
        fn draft<'a>(
            session: &'a str,
            scope: &'a str,
            triggers: &'a [String],
            evidence: &'a [(CandidateEvidenceKind, String)],
        ) -> CandidateDraft<'a> {
            CandidateDraft {
                target: "skill",
                target_id: Some("make-check"),
                title: "提交前跑 make check",
                content: "改动后先执行 make check，通过再报告完成。",
                scope,
                triggers,
                source_session: session,
                source_goal: "加输出",
                evidence,
            }
        }
        let book = crate::learning_candidates::CandidateBook::new(&store);
        // tool_receipt：成功调用 → 通过
        let ev = vec![(
            crate::learning_candidates::CandidateEvidenceKind::ToolReceipt,
            "model-call-check".into(),
        )];
        let cand = book
            .submit(None, &draft(session, "global", &triggers, &ev))
            .expect("同轮真实回执应核验通过");
        assert_eq!(cand.evidence[0].verification, "verified");
        // test_result：命令具备测试语义且成功 → 通过
        let ev = vec![(
            crate::learning_candidates::CandidateEvidenceKind::TestResult,
            "model-call-check".into(),
        )];
        let cand2 = book
            .submit(None, &draft(session, "group", &triggers, &ev))
            .expect("真实测试回执应核验通过");
        assert_eq!(cand2.evidence[0].verification, "verified");
        // 失败调用：无论按哪种证据引用都核验失败
        for kind in [
            crate::learning_candidates::CandidateEvidenceKind::ToolReceipt,
            crate::learning_candidates::CandidateEvidenceKind::TestResult,
        ] {
            let ev = vec![(kind, "model-call-fail".into())];
            let err = book
                .submit(None, &draft(session, "global", &triggers, &ev))
                .expect_err("失败调用不得作为证据");
            assert!(
                err.iter().any(|r| r.contains("核验")),
                "失败回执应核验失败：{err:?}"
            );
        }
        // 编造的 id：核验失败
        let ev = vec![(
            crate::learning_candidates::CandidateEvidenceKind::ToolReceipt,
            "model-call-made-up".into(),
        )];
        let err = book
            .submit(None, &draft(session, "global", &triggers, &ev))
            .expect_err("编造回执不得通过");
        assert!(err.iter().any(|r| r.contains("核验")), "{err:?}");
        crate::round_trace::end(session);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 运行中_steering_不取消写工具并在安全检查点生效() {
        let fixture = acceptance_fixture::IsolatedProject::new("steering-write");
        let root = fixture.project().to_path_buf();
        std::fs::create_dir_all(&root).unwrap();
        #[cfg(unix)]
        std::fs::write(root.join("steering-write.sh"),
            "printf started > steering-started.txt\nwhile [ ! -f steering-release.txt ]; do sleep 0.01; done\nprintf safe-write > steering-sentinel.txt\n").unwrap();
        let reg_dir = root.join("entities");
        std::fs::create_dir_all(reg_dir.join("groups/exmachina/prompts")).unwrap();
        let meta = crate::types::GroupMeta {
            id: "exmachina".into(),
            name: "测试组".into(),
            description: String::new(),
            primary: None,
            workspace: None,
            model: None,
            model_routing: None,
            capabilities: None,
            self_optimize: None,
            builtin: true,
            created_at: crate::types::now_iso(),
        };
        std::fs::write(
            reg_dir.join("group.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();
        std::fs::write(
            reg_dir.join("groups/exmachina/prompts/coding-agent.md"),
            "# 测试个体\n你是测试个体。",
        )
        .unwrap();
        let registry = Arc::new(LocalRegistry::new(&reg_dir).unwrap());
        let store = Arc::new(crate::store::Store::open(root.join("data")).unwrap());
        let session = store.create_session("steering-write", "exmachina").unwrap();
        let (entered, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let provider = Arc::new(SteeringWriteProvider {
            step: AtomicUsize::new(0),
            root: root.clone(),
            entered,
            release: release.clone(),
            saw_steering: AtomicBool::new(false),
            saw_late_steering: AtomicBool::new(false),
            saw_written_sentinel: AtomicBool::new(false),
        });
        let (events, _rx) = tokio::sync::broadcast::channel::<crate::types::CoreEvent>(16);
        let tools = Arc::new(ToolGateway::new(
            &root,
            store,
            registry.clone(),
            events,
            SecurityConfig {
                exec_approval: "risky".into(),
                ..SecurityConfig::default()
            },
        ));
        let rt = AgentRuntime::new(registry, provider.clone(), tools, "test-model");
        let def = coding_def();
        let order = coding_order(vec![
            crate::types::ToolName::Filesystem,
            crate::types::ToolName::Terminal,
        ]);
        let chain = crate::provider::ModelChain {
            candidates: vec![(String::new(), provider.clone(), "m".into())],
            failover: Arc::new(FailoverState::default()),
        };
        crate::round_trace::begin(&session.id);
        let run = tokio::spawn({
            let provider_session_id = session.id.clone();
            async move {
                rt.execute(
                    &def,
                    &order,
                    chain,
                    &provider_session_id,
                    &provider_session_id,
                    false,
                    ToolScope::Computer,
                    |_| {},
                )
                .await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), entered_rx.recv())
            .await
            .expect("模型请求应启动")
            .expect("provider 通知应存在");
        let steering = crate::round_trace::admit_steering(&session.id, "改成安全方向", || Ok(()))
            .expect("运行中 steering 应立即被接收");
        assert_eq!(steering.text, "改成安全方向");
        assert!(!crate::round_trace::is_cancelled(&session.id));
        release.add_permits(1);
        #[cfg(unix)]
        {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while !root.join("steering-started.txt").is_file() {
                    assert!(!run.is_finished(), "写工具应阻塞在安全终态前");
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("写工具应启动");
            crate::round_trace::admit_steering(&session.id, "写入中补充：继续安全收束", || Ok(()))
                .expect("工具执行期间仍应立即 admission");
            assert!(!crate::round_trace::is_cancelled(&session.id));
            assert!(!root.join("steering-sentinel.txt").exists());
            assert!(!provider.saw_late_steering.load(Ordering::SeqCst));
            std::fs::write(root.join("steering-release.txt"), "release").unwrap();
        }
        let report = tokio::time::timeout(std::time::Duration::from_secs(10), run)
            .await
            .expect("写工具应在释放后收束")
            .unwrap()
            .expect("写工具完成后应收束");
        assert_eq!(report.status, SyncStatus::Done);
        assert!(provider.saw_written_sentinel.load(Ordering::SeqCst));
        assert!(provider.saw_steering.load(Ordering::SeqCst));
        #[cfg(unix)]
        assert!(provider.saw_late_steering.load(Ordering::SeqCst));
        assert!(!crate::round_trace::is_cancelled(&session.id));
        assert_eq!(
            std::fs::read_to_string(root.join("steering-sentinel.txt")).unwrap(),
            "safe-write"
        );
        crate::round_trace::end(&session.id);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 原生只读图像_下一次模型请求收到真实data_url() {
        let provider = Arc::new(NativeImageProvider {
            step: AtomicUsize::new(0),
            saw_real_image: AtomicBool::new(false),
            observed_image_count: AtomicUsize::new(0),
            saw_ordered_tool_results: AtomicBool::new(false),
            saw_nonvision_downgrade: AtomicBool::new(false),
        });
        let (rt, fixture) = runtime_with_files(provider.clone());
        fixture.write_png("shot.png");
        fixture.write_png("shot-two.png");
        let def = coding_def();
        let order = coding_order(def.tools.clone());
        let chain = crate::provider::ModelChain {
            candidates: vec![(String::new(), provider.clone(), "m".into())],
            failover: Arc::new(FailoverState::default()),
        };
        let report = rt
            .execute(
                &def,
                &order,
                chain,
                "sess-image-fixture",
                "trace-image-fixture",
                true,
                ToolScope::Computer,
                |_| {},
            )
            .await
            .expect("原生图像工具循环应收束");
        assert_eq!(report.status, SyncStatus::Done);
        assert!(
            provider.saw_real_image.load(Ordering::SeqCst),
            "并发 read 返回的真实 PNG 必须出现在下一次模型请求中"
        );
        assert_eq!(
            provider.observed_image_count.load(Ordering::SeqCst),
            2,
            "并发 read 的两张真实图片都必须回灌"
        );
        assert!(
            provider.saw_ordered_tool_results.load(Ordering::SeqCst),
            "同轮所有 tool 结果必须先于 user 图像回灌消息"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 原生只读图像_非视觉模型收到明确降级而非伪装观察() {
        let provider = Arc::new(NativeImageProvider {
            step: AtomicUsize::new(0),
            saw_real_image: AtomicBool::new(false),
            observed_image_count: AtomicUsize::new(0),
            saw_ordered_tool_results: AtomicBool::new(false),
            saw_nonvision_downgrade: AtomicBool::new(false),
        });
        let (rt, fixture) = runtime_with_files(provider.clone());
        fixture.write_png("shot.png");
        fixture.write_png("shot-two.png");
        let def = coding_def();
        let order = coding_order(def.tools.clone());
        let chain = crate::provider::ModelChain {
            candidates: vec![(String::new(), provider.clone(), "m".into())],
            failover: Arc::new(FailoverState::default()),
        };
        let report = rt
            .execute(
                &def,
                &order,
                chain,
                "sess-image-text-fixture",
                "trace-image-text-fixture",
                false,
                ToolScope::Computer,
                |_| {},
            )
            .await
            .expect("非视觉图像工具循环应明确收束");
        assert_eq!(report.status, SyncStatus::Done);
        assert!(
            provider.saw_nonvision_downgrade.load(Ordering::SeqCst),
            "非视觉模型必须收到明确图像降级说明"
        );
        assert!(
            !provider.saw_real_image.load(Ordering::SeqCst),
            "非视觉模型不得收到伪造的图像输入"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 文本协议图像回灌_仍将真实图片传到下一轮() {
        let provider = Arc::new(TextImageProvider {
            step: AtomicUsize::new(0),
            saw_real_image: AtomicBool::new(false),
            saw_nonvision_downgrade: AtomicBool::new(false),
        });
        let (rt, fixture) = runtime_with_files(provider.clone());
        fixture.write_png("shot.png");
        let def = coding_def();
        let chain = crate::provider::ModelChain {
            candidates: vec![(String::new(), provider.clone(), "m".into())],
            failover: Arc::new(FailoverState::default()),
        };
        let result = rt
            .chat_execute(
                &def,
                "文本协议图像回灌测试".into(),
                vec![],
                "sess-text-image-fixture",
                "trace-text-image-fixture",
                chain,
                true,
                ToolScope::Computer,
                |_| {},
            )
            .await
            .expect("文本协议图像回灌应收束");
        assert_eq!(result, "文本协议图像回灌完成");
        assert!(
            provider.saw_real_image.load(Ordering::SeqCst),
            "文本协议工具路径也必须把真实图片传到下一轮模型请求"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 文本协议图像回灌_非视觉模型收到降级说明() {
        let provider = Arc::new(TextImageProvider {
            step: AtomicUsize::new(0),
            saw_real_image: AtomicBool::new(false),
            saw_nonvision_downgrade: AtomicBool::new(false),
        });
        let (rt, fixture) = runtime_with_files(provider.clone());
        fixture.write_png("shot.png");
        let def = coding_def();
        let chain = crate::provider::ModelChain {
            candidates: vec![(String::new(), provider.clone(), "m".into())],
            failover: Arc::new(FailoverState::default()),
        };
        let result = rt
            .chat_execute(
                &def,
                "文本协议非视觉图像回灌测试".into(),
                vec![],
                "sess-text-image-text-fixture",
                "trace-text-image-text-fixture",
                chain,
                false,
                ToolScope::Computer,
                |_| {},
            )
            .await
            .expect("文本协议非视觉降级应收束");
        assert_eq!(result, "文本协议图像回灌完成");
        assert!(
            provider.saw_nonvision_downgrade.load(Ordering::SeqCst),
            "文本协议路径也必须给非视觉模型明确降级说明"
        );
        assert!(
            !provider.saw_real_image.load(Ordering::SeqCst),
            "文本协议非视觉模型不得收到图像输入"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 停止屏障_释放迟到模型响应后不执行写工具() {
        let barrier = acceptance_fixture::FaultBarrier::new();
        let provider = Arc::new(LateToolProvider {
            barrier: barrier.clone(),
        });
        let (runtime, fixture) = runtime_with_files(provider.clone());
        let runtime = Arc::new(runtime);
        let session = format!("late-response-{}", uuid::Uuid::new_v4());
        let trace = format!("late-response-evidence-{}", uuid::Uuid::new_v4());
        let def = coding_def();
        let order = coding_order(def.tools.clone());
        let chain = crate::provider::ModelChain {
            candidates: vec![(String::new(), provider, "m".into())],
            failover: Arc::new(FailoverState::default()),
        };
        crate::round_trace::begin(&session);
        let task_session = session.clone();
        let task_trace = trace.clone();
        let task = tokio::spawn(async move {
            runtime
                .execute(
                    &def,
                    &order,
                    chain,
                    &task_session,
                    &task_trace,
                    false,
                    ToolScope::Computer,
                    |_| {},
                )
                .await
        });
        barrier.wait_until_entered().await;
        assert!(crate::round_trace::request_cancel(&session));
        barrier.release();
        let error = task
            .await
            .unwrap()
            .expect_err("stopped round must not complete");
        assert!(
            error.to_string().contains("已被用户停止"),
            "unexpected result: {error}"
        );
        assert!(
            !fixture.project().join("must-not-exist.txt").exists(),
            "late response tool call must be blocked before filesystem side effects"
        );
        crate::round_trace::end(&session);
    }

    /// 白名单外的工具调用必须被拒且不炸循环：模型拿到错误回灌后仍能继续收敛
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 原生工具循环_白名单外工具被拒_循环不中断() {
        struct RogueProvider {
            step: AtomicUsize,
        }
        #[async_trait::async_trait]
        impl LlmProvider for RogueProvider {
            fn name(&self) -> &'static str {
                "rogue"
            }
            async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
                anyhow::bail!("只走流式")
            }
            async fn stream(
                &self,
                _req: ChatRequest,
                _tx: tokio::sync::mpsc::UnboundedSender<StreamDelta>,
            ) -> anyhow::Result<ChatResponse> {
                let step = self.step.fetch_add(1, Ordering::SeqCst);
                if step == 0 {
                    Ok(resp(
                        "",
                        vec![ToolCall {
                            id: "r1".into(),
                            name: "web_search".into(), // 不在 coding-agent 白名单
                            arguments: serde_json::json!({"query": "rust"}),
                        }],
                    ))
                } else {
                    let report = serde_json::json!({
                        "sourceAgent": "coding-agent", "taskNodeId": "T1", "status": "done",
                        "statements": [{"tag": "报告", "text": "收到拒绝后收敛", "evidenceLevel": "C"}],
                        "summary": "ok", "evidence": [], "risks": [], "confidence": 0.7
                    });
                    Ok(resp(&format!("```json\n{report}\n```"), vec![]))
                }
            }
        }
        let provider = Arc::new(RogueProvider {
            step: AtomicUsize::new(0),
        });
        let (rt, _fixture) = runtime_with_files(provider.clone());
        let def = coding_def();
        let order = coding_order(def.tools.clone());
        let chain = crate::provider::ModelChain {
            candidates: vec![(String::new(), provider.clone(), "m".into())],
            failover: Arc::new(FailoverState::default()),
        };
        let report = rt
            .execute(
                &def,
                &order,
                chain,
                "sess-rogue",
                "trace-rogue",
                false,
                ToolScope::Computer,
                |_| {},
            )
            .await
            .expect("白名单拒绝不应中断循环");
        assert_eq!(report.status, SyncStatus::Done);
    }
}

#[cfg(test)]
mod max_steps_tests {
    use super::resolve_max_steps;
    use crate::types::ToolScope;

    #[test]
    fn 步数覆盖优先级_个体最高() {
        // 个体显式 48 覆盖一切
        assert_eq!(resolve_max_steps(Some(48), ToolScope::Tools, 24), 48);
        assert_eq!(resolve_max_steps(Some(48), ToolScope::Chat, 24), 48);
    }

    #[test]
    fn 步数覆盖优先级_作用域档位次之() {
        // 无个体覆盖：Chat 档取半，Tools/Computer 用全局
        assert_eq!(resolve_max_steps(None, ToolScope::Chat, 24), 12);
        assert_eq!(resolve_max_steps(None, ToolScope::Tools, 24), 24);
        assert_eq!(resolve_max_steps(None, ToolScope::Computer, 24), 24);
    }

    #[test]
    fn 步数无效值兜底() {
        // 个体 0 / 全局 0 均视为未配置：回落默认
        assert_eq!(resolve_max_steps(Some(0), ToolScope::Tools, 24), 24);
        assert_eq!(resolve_max_steps(None, ToolScope::Tools, 0), 24);
        assert_eq!(resolve_max_steps(None, ToolScope::Chat, 0), 12);
        // Chat 档取半后至少 1
        assert_eq!(resolve_max_steps(None, ToolScope::Chat, 1), 1);
    }
}

#[cfg(test)]
mod soft_landing_tests {
    use super::*;
    use crate::config::SecurityConfig;
    use crate::provider::{ChatRequest, ChatResponse, ToolCall};
    use crate::types::AgentDefinition;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 永远想调工具的替身：直到看见收束提示才吐最终文本
    struct ToolLoopProvider {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl LlmProvider for ToolLoopProvider {
        fn name(&self) -> &'static str {
            "tool-loop"
        }
        async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
            let wrapped = req
                .messages
                .iter()
                .any(|m| m.content.contains("步数预算已用完"));
            if wrapped {
                return Ok(ChatResponse {
                    content: "最终结论：基于已看材料，视频值得一览。".into(),
                    reasoning: String::new(),
                    prompt_tokens: 10,
                    completion_tokens: 10,
                    tool_calls: Vec::new(),
                });
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ChatResponse {
                content: String::new(),
                reasoning: String::new(),
                prompt_tokens: 10,
                completion_tokens: 10,
                tool_calls: vec![ToolCall {
                    id: format!("c{}", self.calls.load(Ordering::SeqCst)),
                    name: "read".into(),
                    arguments: serde_json::json!({ "path": "/tmp/x" }),
                }],
            })
        }
        async fn embed(&self, _model: &str, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![0.0f32; 8]).collect())
        }
        async fn stream(
            &self,
            _req: ChatRequest,
            _tx: tokio::sync::mpsc::UnboundedSender<StreamDelta>,
        ) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse::default())
        }
    }

    fn base_rig(provider: Arc<dyn LlmProvider>) -> (AgentRuntime, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("exm-rt-sl-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let reg_dir = base.join("entities");
        std::fs::create_dir_all(reg_dir.join("prompts")).unwrap();
        let meta = crate::types::GroupMeta {
            id: "exmachina".into(),
            name: "测试组".into(),
            description: String::new(),
            primary: None,
            workspace: None,
            model: None,
            model_routing: None,
            capabilities: None,
            self_optimize: None,
            builtin: true,
            created_at: crate::types::now_iso(),
        };
        std::fs::write(
            reg_dir.join("group.json"),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();
        let prompts_dir = reg_dir.join("groups").join("exmachina").join("prompts");
        std::fs::create_dir_all(&prompts_dir).unwrap();
        std::fs::write(
            prompts_dir.join("watch-agent.md"),
            "# 观众代理\n你是观众代理。",
        )
        .unwrap();
        let registry = Arc::new(LocalRegistry::new(&reg_dir).expect("临时注册表创建失败"));
        let store =
            Arc::new(crate::store::Store::open(base.join("data")).expect("临时存储创建失败"));
        let (events, _rx) = tokio::sync::broadcast::channel::<crate::types::CoreEvent>(16);
        let tools = Arc::new(ToolGateway::new(
            &base,
            store,
            registry.clone(),
            events,
            SecurityConfig::default(),
        ));
        (
            AgentRuntime::new(registry, provider, tools, "test-model"),
            base,
        )
    }

    fn watch_def() -> AgentDefinition {
        serde_json::from_value(serde_json::json!({
            "name": "观众代理", "identifier": "watch-agent", "domain": "公共",
            "tier": "unit", "description": "观看演练", "capabilities": ["测试"],
            "promptFile": "watch-agent.md"
        }))
        .unwrap()
    }

    /// 步数耗尽软着陆：注入收束提示后智能体产出结论（不硬截断）
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 步数耗尽_软着陆产出结论() {
        let provider = Arc::new(ToolLoopProvider {
            calls: AtomicUsize::new(0),
        });
        let (rt, base) = base_rig(provider.clone());
        let def = watch_def();
        // 会话步数预算 = 2（模拟通道面 maxWatchSteps）
        rt.tools.set_session_step_budget("s-sl", 2);
        let out = rt
            .chat_execute(
                &def,
                "系统提示".into(),
                vec![],
                "s-sl",
                "test",
                crate::provider::ModelChain {
                    candidates: vec![("loop".into(), provider.clone(), "test-model".into())],
                    failover: std::sync::Arc::new(crate::provider::FailoverState::default()),
                },
                false,
                ToolScope::Tools,
                |_d| {},
            )
            .await
            .expect("软着陆应收束成功");
        assert!(out.contains("最终结论"), "应收束提示产出结论：{out}");
        assert!(
            provider.calls.load(Ordering::SeqCst) >= 2,
            "工具调用至少走了预算步数"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}

/// 主动搜索纪律注入（proactive-search）：chat_execute 系统提示携带行为准则
#[cfg(test)]
mod search_discipline_tests {
    use super::*;
    use crate::config::SearchConfig;
    use crate::provider::{ChatRequest, ChatResponse, StreamDelta};
    use crate::types::AgentDefinition;
    use parking_lot::Mutex;
    use std::sync::Arc;

    /// 回声替身：记录看到的系统提示，直接产出终文本
    struct PromptProbeProvider {
        seen: Mutex<String>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for PromptProbeProvider {
        fn name(&self) -> &'static str {
            "prompt-probe"
        }
        async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
            if let Some(m) = req.messages.first() {
                *self.seen.lock() = m.content.clone();
            }
            Ok(ChatResponse {
                content: "收到".into(),
                reasoning: String::new(),
                prompt_tokens: 10,
                completion_tokens: 10,
                tool_calls: Vec::new(),
            })
        }
        async fn embed(&self, _model: &str, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![0.0f32; 8]).collect())
        }
        async fn stream(
            &self,
            _req: ChatRequest,
            _tx: tokio::sync::mpsc::UnboundedSender<StreamDelta>,
        ) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse::default())
        }
    }

    fn probe_rig(
        provider: Arc<PromptProbeProvider>,
        search: SearchConfig,
    ) -> (AgentRuntime, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("exm-rt-sd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let reg_dir = base.join("entities");
        std::fs::create_dir_all(reg_dir.join("prompts")).unwrap();
        let meta = crate::types::GroupMeta {
            id: "exmachina".into(),
            name: "测试组".into(),
            description: String::new(),
            primary: None,
            workspace: None,
            model: None,
            model_routing: None,
            capabilities: None,
            self_optimize: None,
            builtin: true,
            created_at: crate::types::now_iso(),
        };
        std::fs::write(
            reg_dir.join("group.json"),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();
        let prompts_dir = reg_dir.join("groups").join("exmachina").join("prompts");
        std::fs::create_dir_all(&prompts_dir).unwrap();
        std::fs::write(
            prompts_dir.join("watch-agent.md"),
            "# 观众代理\n你是观众代理。",
        )
        .unwrap();
        let registry = Arc::new(LocalRegistry::new(&reg_dir).expect("临时注册表创建失败"));
        let store =
            Arc::new(crate::store::Store::open(base.join("data")).expect("临时存储创建失败"));
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let tools = Arc::new(
            crate::tools::ToolGateway::new(
                &base,
                store,
                registry.clone(),
                events,
                Default::default(),
            )
            .with_search(search),
        );
        let rt = AgentRuntime::new(registry, provider, tools, "test-model");
        (rt, base)
    }

    fn def() -> AgentDefinition {
        serde_json::from_value(serde_json::json!({
            "name": "观众代理", "identifier": "watch-agent", "domain": "公共",
            "tier": "unit", "description": "观看演练", "capabilities": ["测试"],
            "promptFile": "watch-agent.md"
        }))
        .unwrap()
    }

    fn tavily(proactive: bool) -> SearchConfig {
        SearchConfig {
            backends: Vec::new(),
            provider: "tavily".into(),
            endpoint: String::new(),
            api_key: "k".into(),
            max_results: 5,
            content_fetch_n: 3,
            proactive,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 搜索就绪_系统提示含纪律() {
        let probe = Arc::new(PromptProbeProvider {
            seen: Mutex::new(String::new()),
        });
        let (rt, base) = probe_rig(probe.clone(), tavily(true));
        let chain = crate::provider::ModelChain {
            candidates: vec![("probe".into(), probe.clone(), "test-model".into())],
            failover: std::sync::Arc::new(crate::provider::FailoverState::default()),
        };
        let _ = rt
            .chat_execute(
                &def(),
                "系统提示".into(),
                vec![],
                "s-sd1",
                "t",
                chain,
                false,
                ToolScope::Tools,
                |_d| {},
            )
            .await
            .unwrap();
        let seen = probe.seen.lock().clone();
        assert!(seen.contains("主动搜索纪律"), "就绪+开启应注入: {seen}");
        assert!(seen.contains("时效性"), "纪律内容含行为触发条件");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 开关关闭_零注入() {
        let probe = Arc::new(PromptProbeProvider {
            seen: Mutex::new(String::new()),
        });
        let (rt, base) = probe_rig(probe.clone(), tavily(false));
        let chain = crate::provider::ModelChain {
            candidates: vec![("probe".into(), probe.clone(), "test-model".into())],
            failover: std::sync::Arc::new(crate::provider::FailoverState::default()),
        };
        let _ = rt
            .chat_execute(
                &def(),
                "系统提示".into(),
                vec![],
                "s-sd2",
                "t",
                chain,
                false,
                ToolScope::Tools,
                |_d| {},
            )
            .await
            .unwrap();
        let seen = probe.seen.lock().clone();
        assert!(!seen.contains("主动搜索纪律"), "关闭后不应注入: {seen}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 仅聊天档_零注入() {
        let probe = Arc::new(PromptProbeProvider {
            seen: Mutex::new(String::new()),
        });
        let (rt, base) = probe_rig(probe.clone(), tavily(true));
        let chain = crate::provider::ModelChain {
            candidates: vec![("probe".into(), probe.clone(), "test-model".into())],
            failover: std::sync::Arc::new(crate::provider::FailoverState::default()),
        };
        // Chat 档无工具面：纪律无意义（web_search 不可达），不注入
        let _ = rt
            .chat_execute(
                &def(),
                "系统提示".into(),
                vec![],
                "s-sd3",
                "t",
                chain,
                false,
                ToolScope::Chat,
                |_d| {},
            )
            .await
            .unwrap();
        let seen = probe.seen.lock().clone();
        assert!(!seen.contains("主动搜索纪律"), "Chat 档不应注入: {seen}");
        let _ = std::fs::remove_dir_all(&base);
    }
}

// ---------------------------------------------------------------- SyncReport 失败可观测

/// sync-report-resilience 5.2：契约失败每次独立落 report.rejected 事件
/// （首个失败字段 + 原文长度），连续失败计数语义不变。
#[cfg(test)]
mod report_rejected_tests {
    use crate::provider::{ChatRequest, ChatResponse, LlmProvider, StreamDelta};
    use crate::runtime::AgentRuntime;
    use crate::types::{
        DispatchBoundary, DispatchConstraints, DispatchInput, DispatchOrder, ToolScope,
    };
    use std::sync::Arc;

    /// 恒产「缺 taskNodeId」报告的替身（核心字段缺失 → 不可修复 → 连续失败）
    struct InvalidReportProvider;

    #[async_trait::async_trait]
    impl LlmProvider for InvalidReportProvider {
        fn name(&self) -> &'static str {
            "invalid-report"
        }
        async fn chat(&self, _req: ChatRequest) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                content: r#"```json
{"sourceAgent":"web-agent","status":"done","statements":[{"tag":"报告","text":"完成。"}],"summary":"done","confidence":0.8}
```"#.into(),
                reasoning: String::new(),
                prompt_tokens: 10,
                completion_tokens: 20,
                tool_calls: Vec::new(),
            })
        }
        async fn stream(
            &self,
            _req: ChatRequest,
            tx: tokio::sync::mpsc::UnboundedSender<StreamDelta>,
        ) -> anyhow::Result<ChatResponse> {
            // 流式优先路径：与 chat 同响应（缺 taskNodeId 的报告）
            let content = r#"```json
{"sourceAgent":"web-agent","status":"done","statements":[{"tag":"报告","text":"完成。"}],"summary":"done","confidence":0.8}
```"#;
            let _ = tx.send(StreamDelta::Text(content.to_string()));
            Ok(ChatResponse {
                content: content.to_string(),
                reasoning: String::new(),
                prompt_tokens: 10,
                completion_tokens: 20,
                tool_calls: Vec::new(),
            })
        }
    }

    fn rig() -> (AgentRuntime, Arc<crate::store::Store>, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("exm-rej-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let reg_dir = base.join("entities");
        std::fs::create_dir_all(reg_dir.join("groups").join("exmachina").join("prompts")).unwrap();
        let meta = crate::types::GroupMeta {
            id: "exmachina".into(),
            name: "测试组".into(),
            description: String::new(),
            primary: None,
            workspace: None,
            model: None,
            model_routing: None,
            capabilities: None,
            self_optimize: None,
            builtin: true,
            created_at: crate::types::now_iso(),
        };
        std::fs::write(
            reg_dir.join("group.json"),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();
        std::fs::write(
            reg_dir
                .join("groups")
                .join("exmachina")
                .join("prompts")
                .join("timeout-agent.md"),
            "# 测试个体\n你是测试个体。",
        )
        .unwrap();
        let registry = Arc::new(crate::registry::LocalRegistry::new(&reg_dir).unwrap());
        let store = Arc::new(crate::store::Store::open(base.join("data")).unwrap());
        let (events, _rx) = tokio::sync::broadcast::channel::<crate::types::CoreEvent>(16);
        let tools = Arc::new(crate::tools::ToolGateway::new(
            &base,
            store.clone(),
            registry.clone(),
            events,
            crate::config::SecurityConfig::default(),
        ));
        let rt = AgentRuntime::new(
            registry,
            Arc::new(InvalidReportProvider),
            tools,
            "test-model",
        );
        (rt, store, base)
    }

    fn order() -> DispatchOrder {
        DispatchOrder {
            task_node_id: "T9".into(),
            objective: "失败可观测演练".into(),
            acceptance: vec![],
            boundary: DispatchBoundary {
                in_scope: vec![],
                forbidden: vec![],
            },
            inputs: vec![DispatchInput {
                ref_id: "user".into(),
                kind: "userInput".into(),
                summary: "演练".into(),
            }],
            tool_allowlist: vec![],
            constraints: DispatchConstraints {
                max_steps: 6,
                timeout_ms: 600_000,
            },
            report_format: "SyncReport".into(),
            model_selection: None,
        }
    }

    /// 连续失败：每次失败一条独立 report.rejected（含失败字段与原文长度），
    /// 判死错误串保持既有形态（连续失败计数语义不变）
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 连续失败_独立事件落库() {
        let (rt, store, base) = rig();
        let def: crate::types::AgentDefinition = serde_json::from_value(serde_json::json!({
            "name": "演练体", "identifier": "timeout-agent", "domain": "公共",
            "tier": "unit", "description": "测试", "capabilities": ["测试"],
            "promptFile": "timeout-agent.md"
        }))
        .unwrap();
        let chain = crate::provider::ModelChain {
            candidates: vec![(
                String::new(),
                Arc::new(InvalidReportProvider) as Arc<dyn LlmProvider>,
                "m".into(),
            )],
            failover: Arc::new(crate::provider::FailoverState::default()),
        };
        let r = rt
            .execute(
                &def,
                &order(),
                chain,
                "ses-rej",
                "ses-rej",
                false,
                ToolScope::Tools,
                |_| {},
            )
            .await;
        let err = r.expect_err("核心字段缺失应连续失败判死");
        assert!(
            err.to_string().contains("连续校验失败"),
            "判死错误形态不变: {err}"
        );
        // 事件断言：SYNC_REWRITE_LIMIT=2 → attempts = 3（1 + 2 次重写），
        // 每次一条独立明细事件
        let events = store.list_events("ses-rej").unwrap_or_default();
        let rejected: Vec<&serde_json::Value> = events
            .iter()
            .filter(|env| env["payload"]["type"] == "report.rejected")
            .collect();
        assert_eq!(rejected.len(), 3, "3 次失败 = 3 条独立事件: {events:?}");
        for (i, evt) in rejected.iter().enumerate() {
            let payload = &evt["payload"]["payload"];
            assert_eq!(
                payload["field"].as_str().unwrap_or(""),
                "taskNodeId",
                "首个失败字段在事件里: {payload}"
            );
            assert!(
                payload["textLen"].as_u64().unwrap_or(0) > 0,
                "原文长度在事件里: {payload}"
            );
            assert_eq!(
                payload["attempt"].as_u64().unwrap_or(0),
                (i + 1) as u64,
                "attempt 递增"
            );
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}

/// 元认知反思（meta-agent 3.1 补实现）：阈值触发 / 每轮一次上限 / 失败降级 / 事件落库。
/// 反思 LLM 调用用 mock 确定性分支驱动（系统提示「元认知反思器」→ 策略 JSON）。
mod meta_reflect_tests {
    use super::*;
    use crate::config::ExperimentsConfig;
    use crate::store::Store;

    /// 反思状态（每实例独立——并行测试不互相污染）：
    /// strategy 决定反思输出策略；fail 置位后反思调用失败（驱动降级分支）。
    #[derive(Default)]
    struct ReflectState {
        fail: std::sync::atomic::AtomicBool,
        strategy: std::sync::Mutex<String>,
    }

    struct ReflectProvider {
        state: std::sync::Arc<ReflectState>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for ReflectProvider {
        fn name(&self) -> &'static str {
            "reflect-mock"
        }
        async fn chat(&self, req: ChatRequest) -> anyhow::Result<crate::provider::ChatResponse> {
            if self.state.fail.load(std::sync::atomic::Ordering::SeqCst) {
                anyhow::bail!("反思通道不可用");
            }
            let system = req
                .messages
                .iter()
                .find(|m| m.role == "system")
                .map(|m| m.content.clone())
                .unwrap_or_default();
            let content = if system.contains("元认知反思器") {
                let strategy = self.state.strategy.lock().unwrap().clone();
                format!("{{\"strategy\": \"{strategy}\", \"note\": \"测试策略\"}}")
            } else {
                "工具循环正常回复".to_string()
            };
            Ok(crate::provider::ChatResponse {
                content,
                ..Default::default()
            })
        }
        async fn stream(
            &self,
            _req: ChatRequest,
            _tx: tokio::sync::mpsc::UnboundedSender<StreamDelta>,
        ) -> anyhow::Result<crate::provider::ChatResponse> {
            anyhow::bail!("测试替身不支持流式")
        }
        async fn embed(&self, _model: &str, _texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
            anyhow::bail!("测试替身不支持嵌入")
        }
        async fn transcribe(
            &self,
            _model: &str,
            _audio: &[u8],
            _filename: &str,
        ) -> anyhow::Result<String> {
            anyhow::bail!("测试替身不支持转写")
        }
        async fn speak(&self, _model: &str, _text: &str) -> anyhow::Result<Vec<u8>> {
            anyhow::bail!("测试替身不支持语音")
        }
    }

    fn reflect_runtime(
        strategy: &str,
    ) -> (
        AgentRuntime,
        std::path::PathBuf,
        std::sync::Arc<ReflectState>,
    ) {
        let base = std::env::temp_dir().join(format!("exm-reflect-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let reg_dir = base.join("entities");
        std::fs::create_dir_all(reg_dir.join("prompts")).unwrap();
        let meta = crate::types::GroupMeta {
            id: "exmachina".into(),
            name: "测试组".into(),
            description: String::new(),
            primary: None,
            workspace: None,
            model: None,
            model_routing: None,
            capabilities: None,
            self_optimize: None,
            builtin: true,
            created_at: crate::types::now_iso(),
        };
        std::fs::write(
            reg_dir.join("group.json"),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();
        let registry = Arc::new(LocalRegistry::new(&reg_dir).expect("注册表失败"));
        let store = Arc::new(Store::open(base.join("data")).expect("存储失败"));
        let (events, _rx) = tokio::sync::broadcast::channel::<crate::types::CoreEvent>(16);
        let tools = Arc::new(ToolGateway::new(
            &base,
            store,
            registry.clone(),
            events,
            crate::config::SecurityConfig::default(),
        ));
        let state = std::sync::Arc::new(ReflectState::default());
        *state.strategy.lock().unwrap() = strategy.to_string();
        let provider = Arc::new(ReflectProvider {
            state: state.clone(),
        });
        let experiments = ExperimentsConfig {
            meta_agent: true,
            ..ExperimentsConfig::default()
        };
        let rt = AgentRuntime::new(registry, provider, tools, "test-model")
            .with_experiments(experiments);
        (rt, base, state)
    }

    /// 反思产出策略并注入提示（meta_reflect 直测：连续失败入口）
    #[tokio::test]
    async fn 反思_策略注入与事件落库() {
        let (rt, base, _state) = reflect_runtime("replan");
        crate::round_trace::begin("s-reflect-1");
        let hint = rt
            .meta_reflect("s-reflect-1", &2, &["terminal".into(), "files".into()])
            .await
            .expect("replan 应注入提示");
        assert!(hint.contains("【元认知】"), "提示带标记: {hint}");
        assert!(hint.contains("重新审视任务分解"), "replan 文案: {hint}");
        // 事件落库可查（append_event 走 tools.store）
        let evts = rt.tools.store.list_events("s-reflect-1").unwrap();
        assert!(
            evts.iter().any(|e| e["payload"]["type"] == "meta.reflect"),
            "meta.reflect 事件应落库: {evts:?}"
        );
        crate::round_trace::end("s-reflect-1");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// continue 策略 = 不注入提示但仍发事件（审计可见反思发生过）
    #[tokio::test]
    async fn 反思_continue不注入但事件可见() {
        let (rt, base, _state) = reflect_runtime("continue");
        crate::round_trace::begin("s-reflect-2");
        let hint = rt
            .meta_reflect("s-reflect-2", &2, &["terminal".into()])
            .await;
        assert!(hint.is_none(), "continue 不注入");
        let evts = rt.tools.store.list_events("s-reflect-2").unwrap();
        assert!(
            evts.iter().any(|e| e["payload"]["type"] == "meta.reflect"
                && e["payload"]["payload"]["strategy"] == "continue"),
            "continue 反思事件仍落库: {evts:?}"
        );
        crate::round_trace::end("s-reflect-2");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 反思调用失败 → 降级 None（不阻塞主流程）
    #[tokio::test]
    async fn 反思_调用失败降级() {
        let (rt, base, state) = reflect_runtime("replan");
        state.fail.store(true, std::sync::atomic::Ordering::SeqCst);
        crate::round_trace::begin("s-reflect-3");
        let hint = rt
            .meta_reflect("s-reflect-3", &2, &["terminal".into()])
            .await;
        assert!(hint.is_none(), "失败降级为 None");
        crate::round_trace::end("s-reflect-3");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 条件②（低产出）入口：trigger=low_yield 事件可区分
    #[tokio::test]
    async fn 反思_低产出触发入口() {
        let (rt, base, _state) = reflect_runtime("redirect");
        crate::round_trace::begin("s-reflect-4");
        let hint = rt
            .meta_reflect_low_yield("s-reflect-4", &["search".into()], 0.2)
            .await
            .expect("redirect 应注入");
        assert!(hint.contains("更换工具"), "redirect 文案: {hint}");
        assert!(hint.contains("产出占比 20%"), "占比标注: {hint}");
        let evts = rt.tools.store.list_events("s-reflect-4").unwrap();
        assert!(
            evts.iter()
                .any(|e| e["payload"]["payload"]["trigger"] == "low_yield"),
            "低产出 trigger 事件: {evts:?}"
        );
        crate::round_trace::end("s-reflect-4");
        let _ = std::fs::remove_dir_all(&base);
    }
}
