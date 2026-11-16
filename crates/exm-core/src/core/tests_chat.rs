use crate::config::ExmConfig;
use crate::provider::{ChatRequest, ChatResponse, LlmProvider, ModelPool, StreamDelta};
use crate::{Core, build_orchestrator, round_trace};
use std::sync::Arc;
use tokio::sync::{Semaphore, mpsc};

struct ControlledProvider {
    entered: mpsc::UnboundedSender<()>,
    release: Arc<Semaphore>,
    fail: bool,
}

#[async_trait::async_trait]
impl LlmProvider for ControlledProvider {
    fn name(&self) -> &'static str { "controlled" }
    async fn chat(&self, _: ChatRequest) -> anyhow::Result<ChatResponse> {
        unreachable!("the fixture always returns nonempty stream content")
    }
    async fn stream(&self, _: ChatRequest, tx: mpsc::UnboundedSender<StreamDelta>) -> anyhow::Result<ChatResponse> {
        self.entered.send(()).unwrap();
        tx.send(StreamDelta::Thinking("in flight".into())).unwrap();
        self.release.acquire().await.unwrap().forget();
        if self.fail { anyhow::bail!("fixture failure"); }
        Ok(ChatResponse { content: "done".into(), reasoning: String::new(), prompt_tokens: 10, completion_tokens: 5, tool_calls: vec![] })
    }
}

struct Fixture {
    core: Arc<Core>,
    root: std::path::PathBuf,
    entered: mpsc::UnboundedReceiver<()>,
    release: Arc<Semaphore>,
}
impl Fixture {
    fn new(fail: bool) -> Self {
        let root = std::env::temp_dir().join(format!("exm-direct-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut cfg = ExmConfig::load(&root);
        cfg.workspace_root = root.clone();
        cfg.agents_dir = root.join("entities");
        cfg.data_dir = root.join("data");
        cfg.use_mock = true;
        cfg.memory_enabled = false;
        cfg.llm.api_key.clear();
        cfg.llm.base_url.clear();
        cfg.llm_profiles.clear();
        let core = Core::with_config(cfg.clone()).unwrap();
        let def = serde_json::from_value(serde_json::json!({
            "name": "Unit", "identifier": "unit", "description": "test", "domain": "test", "tier": "unit"
        })).unwrap();
        core.registry.upsert_single(def, Some("answer briefly".into())).unwrap();
        core.registry.set_active_single(Some("unit")).unwrap();
        let (tx, entered) = mpsc::unbounded_channel();
        let release = Arc::new(Semaphore::new(0));
        let provider = Arc::new(ControlledProvider { entered: tx, release: release.clone(), fail });
        let mut pool = ModelPool::new();
        pool.insert("fixture", provider, "fixture");
        pool.set_active("fixture");
        let mut orch = build_orchestrator(&cfg, &core.store, &core.registry, &core.events, &core.memory, &core.knowledge, None, None, Some(core.cron.clone())).unwrap();
        orch.model_pool = Arc::new(pool);
        *core.orchestrator.write() = Arc::new(orch);
        Self { core: Arc::new(core), root, entered, release }
    }
    async fn started(&mut self) {
        tokio::time::timeout(std::time::Duration::from_secs(3), self.entered.recv()).await.unwrap().unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.root); }
}

#[tokio::test]
async fn direct_chat_serializes_with_normal_chat_and_records_history() {
    let mut f = Fixture::new(false);
    let id = f.core.create_session("direct").unwrap().id;
    let core = f.core.clone(); let sid = id.clone();
    let first = tokio::spawn(async move { core.chat_with_agent(&sid, "unit", "first").await });
    f.started().await;
    assert!(round_trace::is_running(&id));
    assert!(f.core.undo_session(&id, 0, false).await.is_err());
    let core = f.core.clone(); let sid = id.clone();
    let second = tokio::spawn(async move { core.chat(&sid, "second").await });
    assert!(tokio::time::timeout(std::time::Duration::from_millis(50), f.entered.recv()).await.is_err());
    f.release.add_permits(1);
    first.await.unwrap().unwrap();
    f.started().await;
    f.release.add_permits(1);
    second.await.unwrap().unwrap();
    assert!(!round_trace::is_running(&id));
    assert_eq!(f.core.store.list_turn_snapshots(&id).unwrap().len(), 2);
    assert_eq!(f.core.store.list_messages(&id, 0).unwrap().len(), 4);
}

#[tokio::test]
async fn direct_chat_errors_and_aborted_futures_release_running_state() {
    for fail in [false, true] {
        let mut f = Fixture::new(fail);
        let id = f.core.create_session("cleanup").unwrap().id;
        let core = f.core.clone(); let sid = id.clone();
        let task = tokio::spawn(async move { core.chat_with_agent(&sid, "unit", "test").await });
        f.started().await;
        assert!(f.core.stop_session(&id).is_ok());
        if fail {
            f.release.add_permits(1);
            assert!(task.await.unwrap().is_err());
        } else {
            task.abort();
            let _ = task.await;
            f.release.add_permits(1);
        }
        assert!(!round_trace::is_running(&id));
        assert!(!round_trace::is_cancelled(&id));
        assert!(f.core.undo_session(&id, 0, false).await.is_ok());
    }
}

#[tokio::test]
async fn missing_sessions_never_leave_a_running_marker() {
    let f = Fixture::new(false);
    assert!(f.core.chat("missing", "hi").await.is_err());
    assert!(!round_trace::is_running("missing"));
    assert!(f.core.chat_with_agent("missing", "unit", "hi").await.is_err());
    assert!(!round_trace::is_running("missing"));
}

#[tokio::test]
async fn im_greetings_require_a_real_provider_response_and_propagate_errors() {
    for fail in [false, true] {
        let mut f = Fixture::new(fail);
        for text in ["在吗", "你好！", "谢谢", "晚安", "OK", "test", "@2511705734 在吗"] {
            let id = f.core.create_session("im-greeting").unwrap().id;
            crate::orchestrator::mark_im_session(&id);
            let mut events = f.core.subscribe();
            let core = f.core.clone();
            let sid = id.clone();
            let task = tokio::spawn(async move { core.chat(&sid, text).await });
            // The old keyword reply completed without ever entering the provider.
            f.started().await;
            f.release.add_permits(1);
            let result = task.await.unwrap();
            assert_eq!(result.is_err(), fail);
            let messages = f.core.store.list_messages(&id, 0).unwrap();
            let replies: Vec<_> = messages.iter().filter(|m| m.role == crate::types::MessageRole::Orchestrator).collect();
            if fail {
                assert!(replies.is_empty(), "A failed model cannot create a successful greeting");
                assert!(messages.iter().any(|m| m.role == crate::types::MessageRole::System && m.statements.iter().any(|s| s.text.contains("fixture failure"))));
            } else {
                assert_eq!(replies.len(), 1);
                assert_eq!(replies[0].statements[0].text, "done");
            }
            let emitted: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
            assert_eq!(emitted.iter().any(|event| event.kind == "run.error"), fail);
            assert!(!emitted.iter().any(|event| event.payload.get("shortCircuit").is_some()));
            assert!(!round_trace::is_running(&id));
            assert_eq!(f.core.store.list_turn_snapshots(&id).unwrap().last().unwrap().status, if fail { "failed" } else { "done" });
            crate::orchestrator::unmark_im_session(&id);
        }
    }
}
