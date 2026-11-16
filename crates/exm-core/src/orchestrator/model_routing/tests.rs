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
