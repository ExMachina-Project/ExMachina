//! runtime 域测试（自 runtime.rs 迁出，纯搬移；各测试 mod 原样保留）。

use crate::provider::{ChatRequest, LlmProvider, StreamDelta};
use crate::runtime::{resolve_max_steps, AgentRuntime};
use crate::registry::LocalRegistry;
use crate::tools::ToolGateway;
use crate::types::{AgentDefinition, DispatchOrder, ToolScope};
use std::sync::Arc;

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
        assert_eq!(report.status, crate::types::SyncStatus::Blocked, "降级状态应为 blocked");
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
}

// ---------------------------------------------------------------- 原生工具循环回归

/// 复现「agent 无法正常开发」：原生 function calling 的完整多轮工具循环。
/// 覆盖：同轮只读并发（read+grep）→ 写工具（edit）→ terminal → 工具结果回灌 → SyncReport。
/// Mock 通道只走文本协议，覆盖不到这条路径——真实端点（OpenAI 兼容）全走这里。
#[cfg(test)]
mod tool_loop_tests {
    use super::*;
    use parking_lot::Mutex;
    use crate::config::SecurityConfig;
    use crate::provider::{ChatResponse, FailoverState, ToolCall};
    use crate::types::{DispatchBoundary, DispatchConstraints, DispatchInput, SyncStatus};
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    fn runtime_with_files(provider: Arc<dyn LlmProvider>) -> (AgentRuntime, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("exm-loop-{}", uuid::Uuid::new_v4()));
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
            base,
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
        }
    }

    /// 完整编码作业轨迹：只读并发 → edit 落盘 → terminal → 工具结果回灌 → SyncReport 校验通过
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 原生工具循环_编码作业全链路() {
        let provider = Arc::new(ScriptedProvider::new());
        let (rt, base) = runtime_with_files(provider.clone());
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
        let (rt, _base) = runtime_with_files(provider.clone());
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
    use parking_lot::Mutex;
    use crate::config::SearchConfig;
    use crate::provider::{ChatRequest, ChatResponse, StreamDelta};
    use crate::types::AgentDefinition;
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
        std::fs::write(prompts_dir.join("watch-agent.md"), "# 观众代理\n你是观众代理。").unwrap();
        let registry = Arc::new(LocalRegistry::new(&reg_dir).expect("临时注册表创建失败"));
        let store = Arc::new(crate::store::Store::open(base.join("data")).expect("临时存储创建失败"));
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let tools = Arc::new(
            crate::tools::ToolGateway::new(&base, store, registry.clone(), events, Default::default())
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
            provider: "tavily".into(),
            endpoint: String::new(),
            api_key: "k".into(),
            max_results: 5,
            proactive,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 搜索就绪_系统提示含纪律() {
        let probe = Arc::new(PromptProbeProvider { seen: Mutex::new(String::new()) });
        let (rt, base) = probe_rig(probe.clone(), tavily(true));
        let chain = crate::provider::ModelChain {
            candidates: vec![("probe".into(), probe.clone(), "test-model".into())],
            failover: std::sync::Arc::new(crate::provider::FailoverState::default()),
        };
        let _ = rt
            .chat_execute(&def(), "系统提示".into(), vec![], "s-sd1", "t", chain, false, ToolScope::Tools, |_d| {})
            .await
            .unwrap();
        let seen = probe.seen.lock().clone();
        assert!(seen.contains("主动搜索纪律"), "就绪+开启应注入: {seen}");
        assert!(seen.contains("时效性"), "纪律内容含行为触发条件");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 开关关闭_零注入() {
        let probe = Arc::new(PromptProbeProvider { seen: Mutex::new(String::new()) });
        let (rt, base) = probe_rig(probe.clone(), tavily(false));
        let chain = crate::provider::ModelChain {
            candidates: vec![("probe".into(), probe.clone(), "test-model".into())],
            failover: std::sync::Arc::new(crate::provider::FailoverState::default()),
        };
        let _ = rt
            .chat_execute(&def(), "系统提示".into(), vec![], "s-sd2", "t", chain, false, ToolScope::Tools, |_d| {})
            .await
            .unwrap();
        let seen = probe.seen.lock().clone();
        assert!(!seen.contains("主动搜索纪律"), "关闭后不应注入: {seen}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn 仅聊天档_零注入() {
        let probe = Arc::new(PromptProbeProvider { seen: Mutex::new(String::new()) });
        let (rt, base) = probe_rig(probe.clone(), tavily(true));
        let chain = crate::provider::ModelChain {
            candidates: vec![("probe".into(), probe.clone(), "test-model".into())],
            failover: std::sync::Arc::new(crate::provider::FailoverState::default()),
        };
        // Chat 档无工具面：纪律无意义（web_search 不可达），不注入
        let _ = rt
            .chat_execute(&def(), "系统提示".into(), vec![], "s-sd3", "t", chain, false, ToolScope::Chat, |_d| {})
            .await
            .unwrap();
        let seen = probe.seen.lock().clone();
        assert!(!seen.contains("主动搜索纪律"), "Chat 档不应注入: {seen}");
        let _ = std::fs::remove_dir_all(&base);
    }
}
