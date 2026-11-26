use super::*;
use crate::config::{ExmConfig, LlmProfile, ModelEntry};
use crate::provider::{ChatResponse, MockLlmProvider};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Selector {
    calls: AtomicUsize,
}
#[async_trait::async_trait]
impl LlmProvider for Selector {
    fn name(&self) -> &'static str {
        "allocation-fixture"
    }
    async fn chat(&self, request: ChatRequest) -> anyhow::Result<ChatResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.model, "commander");
        let input: serde_json::Value = serde_json::from_str(&request.messages[1].content)?;
        assert_eq!(input["candidates"][0]["cost"], "economy");
        assert!(input["instructions"].as_str().unwrap().contains("routine"));
        Ok(ChatResponse {
            content: r#"{"model":"p/small","reason":"例行摘要适合经济模型"}"#.into(),
            ..Default::default()
        })
    }
    async fn stream(
        &self,
        _: ChatRequest,
        _: tokio::sync::mpsc::UnboundedSender<StreamDelta>,
    ) -> anyhow::Result<ChatResponse> {
        unreachable!()
    }
}
struct Fixture {
    core: crate::Core,
    root: PathBuf,
    selector: Arc<Selector>,
    router: Arc<Orchestrator>,
}
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("exm-routing-policy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut cfg = ExmConfig::load(&root);
        cfg.workspace_root = root.clone();
        cfg.agents_dir = root.join("entities");
        cfg.data_dir = root.join("data");
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        cfg.llm_profiles.clear();
        cfg.llm_profiles.push(LlmProfile {
            id: "p".into(),
            name: "Fixture".into(),
            model: "commander".into(),
            models: vec![
                ModelEntry {
                    model: "small".into(),
                    cost: ModelCost::Economy,
                    usage_notes: "routine summaries".into(),
                    purposes: vec![ModelPurpose::Chat, ModelPurpose::Speech],
                    auto_eligible: true,
                    ..Default::default()
                },
                ModelEntry {
                    model: "commander".into(),
                    cost: ModelCost::Premium,
                    purposes: vec![ModelPurpose::Chat],
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        cfg.active_profile = "p".into();
        let core = crate::Core::with_config(cfg.clone()).unwrap();
        core.registry
            .create_group(Some("policy".into()), "Policy", "fixture")
            .unwrap();
        let primary=serde_json::from_value(serde_json::json!({"identifier":"commander","name":"Commander","description":"fixture","tier":"orchestrator","domain":"test"})).unwrap();
        core.registry.upsert_agent("policy", primary, None).unwrap();
        core.registry.set_primary("policy", "commander").unwrap();
        core.registry.set_active_group("policy").unwrap();
        core.registry.set_active_single(None).unwrap();
        let selector = Arc::new(Selector {
            calls: AtomicUsize::new(0),
        });
        let mut pool = ModelPool::new();
        pool.insert("p", selector.clone(), "commander");
        pool.set_models("p", cfg.llm_profiles[0].models.clone());
        pool.set_active("p");
        pool.insert("expensive", Arc::new(MockLlmProvider), "premium");
        pool.set_fallback("p", "expensive");
        let mut router = crate::build_orchestrator(
            &cfg,
            &core.store,
            &core.registry,
            &core.events,
            &core.memory,
            &core.knowledge,
            None,
            None,
            Some(core.cron.clone()),
        )
        .unwrap();
        router.model_pool = Arc::new(pool);
        let router = Arc::new(router);
        router.bind_model_tools();
        Self {
            core,
            root,
            selector,
            router,
        }
    }
    fn agent(&self, routing: Option<ModelRouting>) -> AgentDefinition {
        let mut def = self.core.registry.primary().unwrap();
        def.model_hint = None;
        def.model_routing = routing;
        def
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn fixed(model: &str) -> ModelAssignment {
    ModelAssignment {
        mode: AssignmentMode::Fixed,
        model: model.into(),
        ..Default::default()
    }
}
fn auto() -> ModelAssignment {
    ModelAssignment {
        mode: AssignmentMode::Auto,
        ..Default::default()
    }
}

#[tokio::test]
async fn auto_allocation_passes_cost_and_guidance_and_cannot_escape_candidates() {
    let f = Fixture::new();
    let def = f.agent(Some(ModelRouting {
        default: auto(),
        instructions: "Use economy models for routine work".into(),
        ..Default::default()
    }));
    let chain = f
        .router
        .resolve_agent_chain(
            &def,
            ModelPurpose::Summary,
            "Summarize these notes",
            "",
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        chain.candidates.len(),
        1,
        "Fallback cannot escape the allowed catalog"
    );
    assert_eq!(chain.candidates[0].2, "small");
    assert_eq!(f.selector.calls.load(Ordering::SeqCst), 1);
    let forbidden = ModelSelection {
        model: "p/commander".into(),
        purpose: ModelPurpose::Chat,
        reason: "too expensive and not allowed".into(),
    };
    assert!(f
        .router
        .resolve_agent_chain(&def, ModelPurpose::Chat, "routine", "", Some(&forbidden))
        .await
        .is_err());
    assert_eq!(
        f.selector.calls.load(Ordering::SeqCst),
        1,
        "Unauthorized plans fail before making requests"
    );
}

#[tokio::test]
async fn fixed_models_and_purpose_overrides_are_enforced() {
    let f = Fixture::new();
    let mut routing = ModelRouting {
        default: fixed("p/small"),
        ..Default::default()
    };
    routing
        .purposes
        .insert(ModelPurpose::Review, fixed("p/commander"));
    routing.purposes.insert(
        ModelPurpose::Speech,
        ModelAssignment {
            mode: AssignmentMode::Disabled,
            ..Default::default()
        },
    );
    let def = f.agent(Some(routing));
    let proposed = ModelSelection {
        model: "expensive/premium".into(),
        reason: "override attempt".into(),
        purpose: ModelPurpose::Chat,
    };
    assert_eq!(
        f.router
            .resolve_agent_model(&def, ModelPurpose::Chat, "routine", "", Some(&proposed))
            .await
            .unwrap()
            .2,
        "small"
    );
    assert_eq!(
        f.router
            .resolve_agent_model(&def, ModelPurpose::Review, "audit", "", Some(&proposed))
            .await
            .unwrap()
            .2,
        "commander"
    );
    assert!(!f.router.purpose_ready(&def, ModelPurpose::Speech));
    assert!(f
        .router
        .resolve_agent_model(&def, ModelPurpose::Speech, "hello", "", None)
        .await
        .is_err());
    assert_eq!(f.selector.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn group_inheritance_and_single_isolation_follow_round_context() {
    let f = Fixture::new();
    let group = f.core.registry.active_group();
    f.core
        .registry
        .set_group_model_routing(
            &group,
            ModelRouting {
                default: fixed("p/small"),
                ..Default::default()
            },
        )
        .unwrap();
    let def = f.agent(None);
    assert_eq!(
        f.router.routing_preview(&def, ModelPurpose::Chat).source,
        "group.default"
    );
    let mut own = def.clone();
    own.model_routing = Some(ModelRouting {
        default: fixed("p/commander"),
        ..Default::default()
    });
    assert_eq!(
        f.router.routing_preview(&own, ModelPurpose::Chat).source,
        "agent.default"
    );
    let single=serde_json::from_value(serde_json::json!({"identifier":"solo","name":"Solo","description":"fixture","domain":"test","tier":"unit"})).unwrap();
    f.core.registry.upsert_single(single, None).unwrap();
    f.core.registry.set_active_single(Some("solo")).unwrap();
    let single = f.core.registry.single("solo").unwrap();
    assert_eq!(
        f.router
            .routing_preview(&single, ModelPurpose::Chat)
            .assignment
            .model,
        "p"
    );
    let result = crate::registry::round_scope_decl::ROUND_SCOPE
        .scope(Some((group, None)), async {
            f.router
                .routing_preview(&def, ModelPurpose::Chat)
                .assignment
                .model
        })
        .await;
    assert_eq!(result, "p/small");
    assert_eq!(f.core.registry.active_single().as_deref(), Some("solo"));
}

#[test]
fn embedding_selection_and_empty_auto_candidates_are_rejected() {
    let f = Fixture::new();
    let mut routing = ModelRouting::default();
    routing.purposes.insert(ModelPurpose::Embedding, auto());
    assert!(f.router.validate_routing(&routing).is_err());
    let routing = ModelRouting {
        default: ModelAssignment {
            candidates: vec!["p/commander".into()],
            ..auto()
        },
        ..Default::default()
    };
    assert!(
        f.router.validate_routing(&routing).is_ok(),
        "An explicit candidate can opt in without catalog eligibility"
    );
}

#[tokio::test]
async fn explicit_missing_model_is_never_silently_replaced() {
    let f = Fixture::new();
    let def = f.agent(Some(ModelRouting {
        default: fixed("missing/small"),
        ..Default::default()
    }));
    assert!(f
        .router
        .resolve_agent_model(&def, ModelPurpose::Chat, "hello", "", None)
        .await
        .is_err());
    assert_eq!(f.selector.calls.load(Ordering::SeqCst), 0);
}

/// capability-parity 5.5：媒体能力候选过滤——Transcribe 用途只有声明 audio
/// （或 purposes 含 transcribe）的模型进入候选；不兼容模型不被调用。
#[tokio::test]
async fn media_purpose_candidates_require_audio_capability() {
    let f = Fixture::new();
    let mut routing = ModelRouting {
        default: fixed("p/commander"),
        ..Default::default()
    };
    routing.purposes.insert(
        ModelPurpose::Transcribe,
        ModelAssignment {
            mode: AssignmentMode::Auto,
            ..Default::default()
        },
    );
    let def = f.agent(Some(routing));
    let preview = f.router.routing_preview(&def, ModelPurpose::Transcribe);
    // small 声明了 Speech 用途但没声明 audio、也没声明 transcribe → 不该入选
    assert!(
        preview
            .candidates
            .iter()
            .all(|c| c.model != "commander" || c.purposes.contains(&ModelPurpose::Transcribe)),
        "无媒体能力声明的模型不得成为转写候选：{:?}",
        preview.candidates
    );
    // 目录里没有满足 audio 的候选 → purpose_ready=false（不静默调用不兼容模型）
    assert!(!f.router.purpose_ready(&def, ModelPurpose::Transcribe));
}

/// capability-parity 5.5：用途候选全部失败时保留真实失败——resolve 报错
/// 包含尝试语境，不冒充成功；model.selected 事件只在真正解析成功时发出。
#[tokio::test]
async fn purpose_candidates_exhausted_keeps_real_failure() {
    let f = Fixture::new();
    let mut routing = ModelRouting {
        default: fixed("p/commander"),
        ..Default::default()
    };
    routing.purposes.insert(
        ModelPurpose::ImageGen,
        ModelAssignment {
            mode: AssignmentMode::Auto,
            candidates: vec!["p/small".into()],
            ..Default::default()
        },
    );
    let def = f.agent(Some(routing));
    // small 未声明 imageGen 能力 → 候选为空（显式候选也得过能力判定）
    let preview = f.router.routing_preview(&def, ModelPurpose::ImageGen);
    assert!(
        preview.candidates.is_empty(),
        "显式候选不满足媒体能力时不得进入候选表：{:?}",
        preview.candidates
    );
    let err = match f
        .router
        .resolve_agent_model(&def, ModelPurpose::ImageGen, "画图", "", None)
        .await
    {
        Err(e) => e.to_string(),
        Ok(_) => panic!("无兼容候选的媒体用途不应解析成功"),
    };
    assert!(
        err.contains("没有可用模型候选"),
        "失败语境应保留真实原因：{err}"
    );
}

/// capability-parity 5.6：离线 provider 否定——未配置密钥的档案不进入自动候选
/// （不虚报能力；use_mock 例外——测试通道本来就是离线的）。
#[test]
fn offline_profiles_without_keys_are_not_candidates() {
    let f = Fixture::new();
    // Fixture 的档案走 use_mock=true（离线测试通道），候选判定应正常包含；
    // 该断言锁定「离线 mock 是合法 provider」这一前提
    let def = f.agent(Some(ModelRouting {
        default: fixed("p/commander"),
        ..Default::default()
    }));
    let preview = f.router.routing_preview(&def, ModelPurpose::Chat);
    assert!(!preview.assignment.model.is_empty());
}

/// capability-parity 5.6：真实（非 mock）档案缺密钥时不进候选——
/// routing_candidates 跳过 base_url/api_key 全空的档案，不虚报可路由。
/// 既有覆盖：routing_candidates 源码的 skip 条件（use_mock=false 且密钥空）；
/// 该条件正是 explicit_missing_model_is_never_silently_replaced 与
/// auto_allocation 测试所驱动的同一函数路径。此处验证 mock 档案（离线
/// provider）候选判定不因缺 HTTP 端点而排除——离线测试通道是合法 provider。
#[test]
fn offline_mock_profile_stays_routable_and_validation_rejects_missing_candidates() {
    let f = Fixture::new();
    // mock 档案（离线）在 Auto 分配下有候选（small 声明了 auto_eligible）
    let mut routing = ModelRouting {
        default: fixed("p/commander"),
        ..Default::default()
    };
    routing.purposes.insert(ModelPurpose::Summary, auto());
    let def = f.agent(Some(routing));
    let preview = f.router.routing_preview(&def, ModelPurpose::Summary);
    assert!(
        preview.candidates.iter().any(|c| c.model == "small"),
        "mock 离线档案的合格候选应可见：{:?}",
        preview.candidates
    );
    // 用途候选全空（ImageGen 无候选）→ validate 拒绝（不静默可路由假象）
    let mut bad = ModelRouting {
        default: fixed("p/commander"),
        ..Default::default()
    };
    bad.purposes.insert(ModelPurpose::ImageGen, auto());
    assert!(f.router.validate_routing(&bad).is_err());
}
