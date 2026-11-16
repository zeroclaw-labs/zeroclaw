use super::*;
use crate::agent::dispatcher::{NativeToolDispatcher, ToolDispatcher, XmlToolDispatcher};
use async_trait::async_trait;
use chrono::TimeZone;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use zeroclaw_api::observability_traits::ObserverMetric;
use zeroclaw_config::schema::Config;

#[test]
fn build_session_model_provider_rejects_undotted_ref() {
    let config = Config::default();
    let err = match build_session_model_provider(&config, "anthropic", Some("m")) {
        Ok(_) => panic!("undotted ref must error"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("<type>.<alias>"), "got: {err}");
}

#[test]
fn build_session_model_provider_requires_a_model() {
    // No configured entry and no override → cannot resolve a model name.
    let config = Config::default();
    let err = match build_session_model_provider(&config, "anthropic.default", None) {
        Ok(_) => panic!("missing model must error"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("no `model` configured"),
        "got: {err}"
    );
}

zeroclaw_api::mock_tool_attribution!(
    CountingTool,
    NamedMockTool,
    MockTool,
    SlowTool,
    ModelSwitchTriggerTool,
    BoundarySteeringTool,
);

struct BoundarySteeringTool {
    steering: tokio::sync::mpsc::Sender<String>,
    cancel: Option<tokio_util::sync::CancellationToken>,
}

#[async_trait]
impl Tool for BoundarySteeringTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Send a correction during the tool batch"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object"})
    }
    async fn execute(&self, _: serde_json::Value) -> Result<crate::tools::ToolResult> {
        self.steering
            .try_send("use the corrected destination".into())?;
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
        Ok(crate::tools::ToolResult {
            success: true,
            output: "tool finished safely".into(),
            error: None,
        })
    }
}

#[cfg(not(feature = "memory-tachi"))]
#[derive(Default)]
struct SteeringMemoryStub {
    entries: Mutex<HashMap<String, zeroclaw_memory::MemoryEntry>>,
}

#[cfg(not(feature = "memory-tachi"))]
impl SteeringMemoryStub {
    fn write_entry(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        namespace: Option<&str>,
        importance: Option<f64>,
        agent_id: Option<&str>,
    ) {
        self.entries.lock().insert(
            key.to_string(),
            zeroclaw_memory::MemoryEntry {
                id: key.to_string(),
                key: key.to_string(),
                content: content.to_string(),
                category,
                timestamp: chrono::Utc::now().to_rfc3339(),
                session_id: session_id.map(str::to_string),
                score: None,
                namespace: namespace.unwrap_or("default").to_string(),
                importance,
                superseded_by: None,
                kind: None,
                pinned: false,
                tenant_id: None,
                agent_alias: None,
                agent_id: agent_id.map(str::to_string),
            },
        );
    }
}

#[cfg(not(feature = "memory-tachi"))]
#[async_trait]
impl Memory for SteeringMemoryStub {
    fn name(&self) -> &str {
        "stub"
    }
    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> Result<()> {
        self.write_entry(key, content, category, session_id, None, None, None);
        Ok(())
    }
    async fn store_with_metadata(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        namespace: Option<&str>,
        importance: Option<f64>,
    ) -> Result<()> {
        self.write_entry(
            key, content, category, session_id, namespace, importance, None,
        );
        Ok(())
    }
    async fn recall(
        &self,
        _: &str,
        _: usize,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<Vec<zeroclaw_memory::MemoryEntry>> {
        Ok(vec![])
    }
    async fn get(&self, key: &str) -> Result<Option<zeroclaw_memory::MemoryEntry>> {
        Ok(self.entries.lock().get(key).cloned())
    }
    async fn list(
        &self,
        _: Option<&MemoryCategory>,
        _: Option<&str>,
    ) -> Result<Vec<zeroclaw_memory::MemoryEntry>> {
        Ok(vec![])
    }
    async fn forget(&self, _: &str) -> Result<bool> {
        Ok(false)
    }
    async fn forget_for_agent(&self, _: &str, _: &str) -> Result<bool> {
        Ok(false)
    }
    async fn count(&self) -> Result<usize> {
        Ok(self.entries.lock().len())
    }
    async fn health_check(&self) -> bool {
        true
    }
    async fn store_with_agent(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        namespace: Option<&str>,
        importance: Option<f64>,
        agent_id: Option<&str>,
    ) -> Result<()> {
        self.write_entry(
            key, content, category, session_id, namespace, importance, agent_id,
        );
        Ok(())
    }
    async fn recall_for_agents(
        &self,
        _: &[&str],
        _: &str,
        _: usize,
        _: Option<&str>,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<Vec<zeroclaw_memory::MemoryEntry>> {
        Ok(vec![])
    }
}

#[cfg(not(feature = "memory-tachi"))]
impl ::zeroclaw_api::attribution::Attributable for SteeringMemoryStub {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Memory(::zeroclaw_api::attribution::MemoryKind::InMemory)
    }
    fn alias(&self) -> &str {
        "SteeringMemoryStub"
    }
}

#[tokio::test]
async fn steering_at_tool_boundary_keeps_enrichment_and_cancellation_precedence() {
    for cancel_at_boundary in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        #[cfg(feature = "memory-tachi")]
        let memory: Arc<dyn Memory> =
            Arc::new(zeroclaw_memory::TachiMemory::new("tachi", tmp.path()).unwrap());
        #[cfg(not(feature = "memory-tachi"))]
        let memory: Arc<dyn Memory> = Arc::new(SteeringMemoryStub::default());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let observer = Arc::new(CapturingObserver::default());
        let (steering_tx, mut steering_rx) = tokio::sync::mpsc::channel(4);
        let cancel = tokio_util::sync::CancellationToken::new();
        let fixed = chrono::DateTime::parse_from_rfc3339("2026-01-01T12:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Local);
        let mut agent = Agent::builder()
            .model_provider(Box::new(TranscriptCaptureModelProvider {
                seen_messages: seen.clone(),
                responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
                    text: Some("checking first".into()),
                    tool_calls: vec![zeroclaw_providers::ToolCall {
                        id: "boundary-tool".into(),
                        name: "echo".into(),
                        arguments: "{}".into(),
                        extra_content: None,
                    }],
                    usage: None,
                    reasoning_content: None,
                }]),
            }))
            .tools(vec![Box::new(BoundarySteeringTool {
                steering: steering_tx,
                cancel: cancel_at_boundary.then(|| cancel.clone()),
            })])
            .memory(memory.clone())
            .memory_session_id(Some("steering-session".into()))
            .auto_save(true)
            .observer(observer.clone())
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(tmp.path().to_path_buf())
            .turn_datetime(move || fixed)
            .build()
            .unwrap();
        let (events, _rx) = tokio::sync::mpsc::channel(64);
        let result = agent
            .turn_streamed_with_steering_state(
                "check destination",
                events,
                Some(cancel),
                Some(&mut steering_rx),
            )
            .await;
        let captured = seen.lock().clone();
        if cancel_at_boundary {
            assert!(result.is_err());
            assert_eq!(
                captured.len(),
                1,
                "cancel must win before a steered model call"
            );
            assert!(
                !memory
                    .get("user_msg")
                    .await
                    .unwrap()
                    .unwrap()
                    .content
                    .contains("corrected")
            );
        } else {
            let result = result.unwrap();
            assert_eq!(
                captured.len(),
                2,
                "steering must reach the next call, before any stale continuation"
            );
            let expected = format!(
                "[{}] use the corrected destination",
                fixed.format("%Y-%m-%d %H:%M:%S %Z")
            );
            assert!(
                captured[1]
                    .iter()
                    .any(|message| message.role == "user" && message.content == expected)
            );
            assert!(
                captured[1]
                    .iter()
                    .any(|message| message.content.contains("tool finished safely")),
                "completed tool results precede steering"
            );
            assert_eq!(result.new_messages.iter().filter(|message| matches!(message, ConversationMessage::Chat(message) if message.role == "user" && message.content == expected)).count(), 1);
            let stored = memory.get("user_msg").await.unwrap().unwrap();
            assert_eq!(
                stored.content, "use the corrected destination",
                "autosave stores the raw user correction, not its prompt wrapper"
            );
            assert_eq!(stored.session_id.as_deref(), Some("steering-session"));
        }
        let stores = observer
            .events
            .lock()
            .iter()
            .filter(|event| matches!(event, ObserverEvent::MemoryStore { .. }))
            .count();
        assert_eq!(stores, if cancel_at_boundary { 1 } else { 2 });
    }
}

struct MockModelProvider {
    responses: Mutex<Vec<zeroclaw_providers::ChatResponse>>,
}

#[async_trait]
impl ModelProvider for MockModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<String> {
        Ok("ok".into())
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<zeroclaw_providers::ChatResponse> {
        let mut guard = self.responses.lock();
        if guard.is_empty() {
            return Ok(zeroclaw_providers::ChatResponse {
                text: Some("done".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            });
        }
        Ok(guard.remove(0))
    }
}
impl ::zeroclaw_api::attribution::Attributable for MockModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "MockModelProvider"
    }
}

const BLANK_TURN_ERROR: &str = "empty user message: refusing to dispatch a blank turn";

fn blank_input_agent(model_provider: Box<dyn ModelProvider>) -> Agent {
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );
    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    Agent::builder()
        .model_provider(model_provider)
        .tools(Vec::new())
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed with valid config")
}

#[tokio::test]
async fn turn_rejects_blank_input() {
    let model_provider = Box::new(MockModelProvider {
        responses: Mutex::new(Vec::new()),
    });
    let mut agent = blank_input_agent(model_provider);
    let err = agent.turn("").await.expect_err("blank turn must fail");
    assert_eq!(err.to_string(), BLANK_TURN_ERROR);
}

#[tokio::test]
async fn turn_rejects_whitespace_only_input() {
    let model_provider = Box::new(MockModelProvider {
        responses: Mutex::new(Vec::new()),
    });
    let mut agent = blank_input_agent(model_provider);
    let err = agent
        .turn("   \n\t")
        .await
        .expect_err("whitespace-only turn must fail");
    assert_eq!(err.to_string(), BLANK_TURN_ERROR);
}

// ── model-fallback notice (silent downgrade surfacing) ──────────────

fn fallback_info(
    requested_provider: &str,
    requested_model: &str,
    actual_provider: &str,
    actual_model: &str,
) -> zeroclaw_providers::reliable::ProviderFallbackInfo {
    zeroclaw_providers::reliable::ProviderFallbackInfo {
        requested_provider: requested_provider.into(),
        requested_model: requested_model.into(),
        actual_provider: actual_provider.into(),
        actual_model: actual_model.into(),
    }
}

#[tokio::test]
async fn model_fallback_notice_appended_and_streamed_on_model_downgrade() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    // Same provider family, different model — the case the channels
    // orchestrator's family check suppresses; direct-turn surfaces must
    // still see it.
    let info = fallback_info("anthropic", "model-requested", "anthropic", "model-served");
    let out = Agent::append_model_fallback_notice("hello".to_string(), Some(&info), &tx).await;
    assert!(
        out.starts_with("hello\n\n"),
        "reply text must be preserved ahead of the notice: {out}"
    );
    assert!(
        out.contains("model-requested") && out.contains("model-served"),
        "notice must name both models: {out}"
    );
    match rx.try_recv() {
        Ok(TurnEvent::Chunk { delta }) => {
            assert!(
                delta.contains("model-served"),
                "streamed chunk must carry the notice for delta-only consumers: {delta}"
            );
        }
        other => panic!("expected a trailing Chunk carrying the notice, got {other:?}"),
    }
}

#[tokio::test]
async fn model_fallback_notice_skipped_for_pure_retry() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    // The resilient wrapper records retries too (attempt > 0 on the
    // primary entry); an identical requested/served pair is not a
    // downgrade and must stay silent.
    let info = fallback_info("anthropic", "same-model", "anthropic", "same-model");
    let out = Agent::append_model_fallback_notice("hello".to_string(), Some(&info), &tx).await;
    assert_eq!(out, "hello");
    assert!(rx.try_recv().is_err(), "no chunk for a retry");
}

#[tokio::test]
async fn model_fallback_notice_absent_without_fallback_info() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let out = Agent::append_model_fallback_notice("hello".to_string(), None, &tx).await;
    assert_eq!(out, "hello");
    assert!(rx.try_recv().is_err());
}

#[derive(Clone, Copy)]
enum RuntimeStreamPlan {
    Unsupported,
    Text(&'static str),
    Error,
}

struct RuntimeStreamingProbeProvider {
    stream: RuntimeStreamPlan,
    chat_text: Option<&'static str>,
}

#[async_trait]
impl ModelProvider for RuntimeStreamingProbeProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<String> {
        Ok(self.chat_text.unwrap_or("ok").to_string())
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<zeroclaw_providers::ChatResponse> {
        let Some(text) = self.chat_text else {
            anyhow::bail!("chat path must not be used for this probe");
        };
        Ok(zeroclaw_providers::ChatResponse {
            text: Some(text.to_string()),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        })
    }

    fn supports_streaming(&self) -> bool {
        !matches!(self.stream, RuntimeStreamPlan::Unsupported)
    }

    fn stream_chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
        _options: zeroclaw_providers::traits::StreamOptions,
    ) -> futures_util::stream::BoxStream<
        'static,
        zeroclaw_providers::traits::StreamResult<zeroclaw_providers::traits::StreamEvent>,
    > {
        use futures_util::StreamExt as _;

        match self.stream {
            RuntimeStreamPlan::Unsupported => futures_util::stream::empty().boxed(),
            RuntimeStreamPlan::Text(text) => futures_util::stream::iter(vec![
                Ok(zeroclaw_providers::traits::StreamEvent::TextDelta(
                    zeroclaw_providers::traits::StreamChunk::delta(text),
                )),
                Ok(zeroclaw_providers::traits::StreamEvent::Final),
            ])
            .boxed(),
            RuntimeStreamPlan::Error => futures_util::stream::iter(vec![Err(
                zeroclaw_providers::traits::StreamError::ModelProvider(
                    "stream failed before output".into(),
                ),
            )])
            .boxed(),
        }
    }
}

impl ::zeroclaw_api::attribution::Attributable for RuntimeStreamingProbeProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "RuntimeStreamingProbeProvider"
    }
}

fn streaming_probe_reliable_provider(
    primary: RuntimeStreamingProbeProvider,
    fallback: RuntimeStreamingProbeProvider,
) -> zeroclaw_providers::reliable::ReliableModelProvider {
    zeroclaw_providers::reliable::ReliableModelProvider::new(
        "test",
        vec![
            (
                "provider-requested".to_string(),
                Box::new(primary) as Box<dyn ModelProvider>,
            ),
            (
                "provider-served".to_string(),
                Box::new(fallback) as Box<dyn ModelProvider>,
            ),
        ],
        0,
        1,
    )
}

/// End-to-end: a resilient wrapper that fails over to a second entry
/// mid-turn must surface the downgrade in BOTH the returned response and
/// the event stream.
#[tokio::test]
async fn streamed_turn_surfaces_provider_fallback_notice() {
    struct FailingModelProvider;
    #[async_trait]
    impl ModelProvider for FailingModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            anyhow::bail!("primary provider is down")
        }
        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<zeroclaw_providers::ChatResponse> {
            anyhow::bail!("primary provider is down")
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for FailingModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "FailingModelProvider"
        }
    }

    let reliable = zeroclaw_providers::reliable::ReliableModelProvider::new(
        "test",
        vec![
            (
                "provider-requested".to_string(),
                Box::new(FailingModelProvider) as Box<dyn ModelProvider>,
            ),
            (
                "provider-served".to_string(),
                Box::new(MockModelProvider {
                    responses: Mutex::new(Vec::new()),
                }) as Box<dyn ModelProvider>,
            ),
        ],
        0,
        50,
    );

    let mut agent = blank_input_agent(Box::new(reliable));
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let outcome = agent
        .turn_streamed_with_steering_state("hello", tx, None, None)
        .await
        .expect("turn must succeed via the fallback entry");

    assert!(
        outcome.response.contains("provider-served")
            && outcome.response.contains("provider-requested"),
        "final response must carry the fallback notice: {}",
        outcome.response
    );

    let mut chunk_carried_notice = false;
    while let Ok(event) = rx.try_recv() {
        if let TurnEvent::Chunk { delta } = event
            && delta.contains("provider-served")
        {
            chunk_carried_notice = true;
        }
    }
    assert!(
        chunk_carried_notice,
        "the notice must also be streamed for delta-only consumers (ZeroCode)"
    );
}

#[tokio::test]
async fn streamed_turn_surfaces_streaming_provider_fallback_notice() {
    let reliable = streaming_probe_reliable_provider(
        RuntimeStreamingProbeProvider {
            stream: RuntimeStreamPlan::Unsupported,
            chat_text: None,
        },
        RuntimeStreamingProbeProvider {
            stream: RuntimeStreamPlan::Text("streamed fallback"),
            chat_text: None,
        },
    );

    let mut agent = blank_input_agent(Box::new(reliable));
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let outcome = agent
        .turn_streamed_with_steering_state("hello", tx, None, None)
        .await
        .expect("turn must succeed via the streaming fallback entry");

    assert!(
        outcome.response.contains("streamed fallback")
            && outcome.response.contains("provider-served"),
        "final response must include streamed text and fallback notice: {}",
        outcome.response
    );

    let mut streamed = String::new();
    while let Ok(event) = rx.try_recv() {
        if let TurnEvent::Chunk { delta } = event {
            streamed.push_str(&delta);
        }
    }
    assert!(
        streamed.contains("streamed fallback") && streamed.contains("provider-served"),
        "streamed chunks must include the live fallback output and notice: {streamed}"
    );
}

#[tokio::test]
async fn streamed_turn_does_not_surface_stale_record_after_stream_error() {
    let reliable = streaming_probe_reliable_provider(
        RuntimeStreamingProbeProvider {
            stream: RuntimeStreamPlan::Unsupported,
            chat_text: Some("primary final"),
        },
        RuntimeStreamingProbeProvider {
            stream: RuntimeStreamPlan::Error,
            chat_text: None,
        },
    );

    let mut agent = blank_input_agent(Box::new(reliable));
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    let outcome = agent
        .turn_streamed_with_steering_state("hello", tx, None, None)
        .await
        .expect("pre-output stream error must fall back to primary chat");

    assert_eq!(
        outcome.response, "primary final",
        "failed fallback streams must not leave stale fallback notice state"
    );
}

#[tokio::test]
async fn turn_streamed_rejects_blank_input() {
    let model_provider = Box::new(MockModelProvider {
        responses: Mutex::new(Vec::new()),
    });
    let mut agent = blank_input_agent(model_provider);
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(8);
    let err = agent
        .turn_streamed("", event_tx, None)
        .await
        .expect_err("blank streamed turn must fail");
    assert_eq!(err.to_string(), BLANK_TURN_ERROR);
}

#[tokio::test]
async fn turn_streamed_rejects_whitespace_only_input() {
    let model_provider = Box::new(MockModelProvider {
        responses: Mutex::new(Vec::new()),
    });
    let mut agent = blank_input_agent(model_provider);
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(8);
    let err = agent
        .turn_streamed("  \n", event_tx, None)
        .await
        .expect_err("whitespace-only streamed turn must fail");
    assert_eq!(err.to_string(), BLANK_TURN_ERROR);
}

struct ModelCaptureModelProvider {
    responses: Mutex<Vec<zeroclaw_providers::ChatResponse>>,
    seen_models: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl ModelProvider for ModelCaptureModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<String> {
        Ok("ok".into())
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        model: &str,
        _temperature: Option<f64>,
    ) -> Result<zeroclaw_providers::ChatResponse> {
        self.seen_models.lock().push(model.to_string());
        let mut guard = self.responses.lock();
        if guard.is_empty() {
            return Ok(zeroclaw_providers::ChatResponse {
                text: Some("done".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            });
        }
        Ok(guard.remove(0))
    }
}
impl ::zeroclaw_api::attribution::Attributable for ModelCaptureModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "ModelCaptureModelProvider"
    }
}

struct TranscriptCaptureModelProvider {
    responses: Mutex<Vec<zeroclaw_providers::ChatResponse>>,
    seen_messages: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
}

#[tokio::test]
async fn dated_turns_share_an_undated_system_prefix_across_entry_points() {
    let workspace = tempfile::tempdir().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut expected_dates = Vec::new();
    for streamed in [false, true] {
        for timestamp in ["2026-01-01T12:00:00+00:00", "2026-01-02T12:00:00+00:00"] {
            let fixed = chrono::DateTime::parse_from_rfc3339(timestamp)
                .unwrap()
                .with_timezone(&chrono::Local);
            expected_dates.push(fixed.format("%Y-%m-%d %H:%M:%S %Z").to_string());
            let mut agent = Agent::builder()
                .model_provider(Box::new(TranscriptCaptureModelProvider {
                    responses: Mutex::new(vec![]),
                    seen_messages: Arc::clone(&seen),
                }))
                .tools(vec![])
                .memory(Arc::new(zeroclaw_memory::NoneMemory::new("none")))
                .observer(Arc::new(crate::observability::NoopObserver))
                .tool_dispatcher(Box::new(NativeToolDispatcher))
                .workspace_dir(workspace.path().to_path_buf())
                .model_name("fixture-model".into())
                .turn_datetime(move || fixed)
                .build()
                .unwrap();
            if streamed {
                let (event_tx, _event_rx) = tokio::sync::mpsc::channel(64);
                agent
                    .turn_streamed("fixture question", event_tx, None)
                    .await
                    .unwrap();
            } else {
                agent.turn("fixture question").await.unwrap();
            }
        }
    }
    let requests = seen.lock();
    assert_eq!(requests.len(), expected_dates.len());
    let system = requests[0].iter().find(|m| m.role == "system").unwrap();
    assert!(!system.content.contains("CRITICAL CONTEXT: CURRENT DATE"));
    for (request, date) in requests.iter().zip(&expected_dates) {
        assert_eq!(
            request.iter().find(|m| m.role == "system").unwrap().content,
            system.content
        );
        let user = request.iter().rev().find(|m| m.role == "user").unwrap();
        assert!(
            user.content.contains(date),
            "missing current turn date: {}",
            user.content
        );
        assert!(user.content.contains("fixture question"));
    }
    assert_ne!(expected_dates[0], expected_dates[1]);
}

#[tokio::test]
async fn resolved_tool_retention_reaches_turn_and_streamed_requests_without_rewriting_history() {
    use zeroclaw_config::schema::{AliasedAgentConfig, RuntimeProfileConfig};

    for streamed in [false, true] {
        for keep in [0, 1, 2, 8] {
            let workspace = tempfile::tempdir().unwrap();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let mut config = Config::default();
            config.runtime_profiles.insert(
                "fixture".into(),
                RuntimeProfileConfig {
                    keep_tool_context_turns: Some(keep),
                    tool_dispatcher: Some("native".into()),
                    max_history_messages: Some(1_000),
                    max_context_tokens: Some(1_000_000),
                    ..RuntimeProfileConfig::default()
                },
            );
            config.agents.insert(
                "fixture".into(),
                AliasedAgentConfig {
                    runtime_profile: "fixture".into(),
                    ..AliasedAgentConfig::default()
                },
            );
            let mut agent = Agent::builder()
                .model_provider(Box::new(TranscriptCaptureModelProvider {
                    responses: Mutex::new(vec![]),
                    seen_messages: Arc::clone(&seen),
                }))
                .tools(vec![])
                .memory(Arc::new(zeroclaw_memory::NoneMemory::new("none")))
                .observer(Arc::new(crate::observability::NoopObserver))
                .tool_dispatcher(Box::new(NativeToolDispatcher))
                .workspace_dir(workspace.path().to_path_buf())
                .model_name("fixture-model".into())
                .config(config.resolved_agent_config("fixture").unwrap())
                .build()
                .unwrap();
            let mut seed = Vec::new();
            let output = format!("HEAD\n{}\nTAIL", "result ".repeat(1_000));
            for turn in 0..9 {
                seed.push(ConversationMessage::Chat(ChatMessage::user(format!(
                    "question {turn}"
                ))));
                seed.push(ConversationMessage::AssistantToolCalls {
                    text: None,
                    tool_calls: (0..2)
                        .map(|parallel| zeroclaw_providers::ToolCall {
                            id: format!("call-{turn}-{parallel}"),
                            name: "fixture".into(),
                            arguments: "{}".into(),
                            extra_content: Some(serde_json::json!({"signature": "opaque"})),
                        })
                        .collect(),
                    reasoning_content: Some("opaque reasoning".into()),
                });
                seed.push(ConversationMessage::ToolResults(
                    (0..2)
                        .map(|parallel| zeroclaw_providers::ToolResultMessage {
                            tool_call_id: format!("call-{turn}-{parallel}"),
                            content: output.clone(),
                            tool_name: "fixture".into(),
                        })
                        .collect(),
                ));
                seed.push(ConversationMessage::Chat(ChatMessage::assistant(format!(
                    "answer {turn}"
                ))));
            }
            let source = serde_json::to_vec(&seed).unwrap();
            agent.seed_conversation_history(seed);
            if streamed {
                let (event_tx, _event_rx) = tokio::sync::mpsc::channel(64);
                agent
                    .turn_streamed("current question", event_tx, None)
                    .await
                    .unwrap();
            } else {
                agent.turn("current question").await.unwrap();
            }
            let requests = seen.lock();
            assert_eq!(requests.len(), 1);
            let results: Vec<_> = requests[0].iter().filter(|m| m.role == "tool").collect();
            assert_eq!(results.len(), 18, "no call/result pair may be dropped");
            for (index, result) in results.into_iter().enumerate() {
                let payload: serde_json::Value = serde_json::from_str(&result.content).unwrap();
                let turn = index / 2;
                assert_eq!(
                    payload["tool_call_id"],
                    format!("call-{turn}-{}", index % 2)
                );
                let content = payload["content"].as_str().unwrap();
                if turn >= 9usize.saturating_sub(keep) {
                    assert_eq!(
                        content, output,
                        "turn={turn}, k={keep}, streamed={streamed}"
                    );
                } else {
                    assert!(
                        content.len()
                            <= zeroclaw_providers::multimodal::STALE_TOOL_RESULT_MAX_CHARS
                    );
                    assert!(content.starts_with("HEAD"));
                    assert!(content.ends_with("TAIL"));
                }
            }
            let calls: Vec<serde_json::Value> = requests[0]
                .iter()
                .filter(|m| m.role == "assistant")
                .filter_map(|m| serde_json::from_str(&m.content).ok())
                .collect();
            assert_eq!(calls.len(), 9);
            for call in calls {
                assert_eq!(call["reasoning_content"], "opaque reasoning");
                assert_eq!(call["tool_calls"].as_array().unwrap().len(), 2);
                assert_eq!(
                    call["tool_calls"][0]["extra_content"]["signature"],
                    "opaque"
                );
            }
            assert_eq!(
                serde_json::to_vec(&agent.history()[1..37]).unwrap(),
                source,
                "request projection must not overwrite canonical replay history"
            );
        }
    }
}

#[async_trait]
impl ModelProvider for TranscriptCaptureModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<String> {
        Ok("ok".into())
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<zeroclaw_providers::ChatResponse> {
        self.seen_messages.lock().push(request.messages.to_vec());
        let mut responses = self.responses.lock();
        if responses.is_empty() {
            return Ok(zeroclaw_providers::ChatResponse {
                text: Some("done".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            });
        }
        Ok(responses.remove(0))
    }
}

impl ::zeroclaw_api::attribution::Attributable for TranscriptCaptureModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "TranscriptCaptureModelProvider"
    }
}

struct StreamingSteeringModelProvider {
    seen_messages: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
    call_count: AtomicUsize,
    fail_on_call: Option<usize>,
    fail_chat_on_call: Option<usize>,
    fail_after_delta_on_call: Option<usize>,
    delay_chat_on_call: Option<usize>,
}

#[async_trait]
impl ModelProvider for StreamingSteeringModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<String> {
        Ok("ok".into())
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<zeroclaw_providers::ChatResponse> {
        let call = self.call_count.fetch_add(1, Ordering::SeqCst) + 1;
        self.seen_messages.lock().push(request.messages.to_vec());
        if self.delay_chat_on_call == Some(call) {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }
        if self.fail_on_call == Some(call) {
            anyhow::bail!("synthetic provider failure on call {call}");
        }
        if self.fail_chat_on_call == Some(call) {
            anyhow::bail!("synthetic chat failure on call {call}");
        }
        if self.fail_after_delta_on_call == Some(call) {
            anyhow::bail!("synthetic provider failure after delta on call {call}");
        }
        Ok(zeroclaw_providers::ChatResponse {
            text: Some(if call == 1 { "draft" } else { "final" }.into()),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        })
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn stream_chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
        _options: zeroclaw_providers::traits::StreamOptions,
    ) -> futures_util::stream::BoxStream<
        'static,
        zeroclaw_providers::traits::StreamResult<zeroclaw_providers::traits::StreamEvent>,
    > {
        use futures_util::StreamExt as _;

        let call = self.call_count.fetch_add(1, Ordering::SeqCst) + 1;
        self.seen_messages.lock().push(request.messages.to_vec());
        let should_fail = self.fail_on_call == Some(call);
        let should_fail_after_delta = self.fail_after_delta_on_call == Some(call);
        let delta = if call == 1 { "draft" } else { "final" }.to_string();
        futures_util::stream::unfold(0, move |step| {
            let delta = delta.clone();
            async move {
                match step {
                    0 if should_fail => Some((
                        Err(zeroclaw_providers::traits::StreamError::ModelProvider(
                            "synthetic provider failure".into(),
                        )),
                        1,
                    )),
                    0 => Some((
                        Ok(zeroclaw_providers::traits::StreamEvent::TextDelta(
                            zeroclaw_providers::traits::StreamChunk {
                                delta,
                                is_final: false,
                                reasoning: None,
                                token_count: 0,
                            },
                        )),
                        1,
                    )),
                    1 if should_fail_after_delta => Some((
                        Err(zeroclaw_providers::traits::StreamError::ModelProvider(
                            "synthetic provider failure after delta".into(),
                        )),
                        2,
                    )),
                    1 => {
                        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                        Some((Ok(zeroclaw_providers::traits::StreamEvent::Final), 2))
                    }
                    _ => None,
                }
            }
        })
        .boxed()
    }
}

impl ::zeroclaw_api::attribution::Attributable for StreamingSteeringModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "StreamingSteeringModelProvider"
    }
}

#[derive(Default)]
struct CapturingObserver {
    events: parking_lot::Mutex<Vec<ObserverEvent>>,
}

fn fixed_response_cache_turn_datetime() -> chrono::DateTime<chrono::Local> {
    chrono::Local
        .with_ymd_and_hms(2026, 6, 25, 12, 0, 0)
        .single()
        .expect("fixed local test timestamp")
}

impl Observer for CapturingObserver {
    fn record_event(&self, event: &ObserverEvent) {
        self.events.lock().push(event.clone());
    }
    fn record_metric(&self, _metric: &ObserverMetric) {}
    fn name(&self) -> &str {
        "capturing"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn flush(&self) {}
}

struct MultimodalCaptureProvider {
    seen_user_messages: Arc<Mutex<Vec<String>>>,
    streamed: bool,
    /// When true, `chat` and `stream_chat` return `Err` after capturing
    /// the user message. Used by #25 tests to verify the settle path
    /// hands announcements back on provider failure.
    fail_chat: bool,
}

#[async_trait]
impl ModelProvider for MultimodalCaptureProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<String> {
        Ok("ok".into())
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<zeroclaw_providers::ChatResponse> {
        if let Some(message) = request.messages.iter().rfind(|msg| msg.role == "user") {
            self.seen_user_messages.lock().push(message.content.clone());
        }
        if self.fail_chat {
            return Err(anyhow::Error::msg("synthetic provider failure (#25)"));
        }
        Ok(zeroclaw_providers::ChatResponse {
            text: Some("done".into()),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        })
    }

    fn stream_chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
        _options: zeroclaw_providers::traits::StreamOptions,
    ) -> futures_util::stream::BoxStream<
        'static,
        zeroclaw_providers::traits::StreamResult<zeroclaw_providers::traits::StreamEvent>,
    > {
        use futures_util::stream::{self, StreamExt};

        if let Some(message) = request.messages.iter().rfind(|msg| msg.role == "user") {
            self.seen_user_messages.lock().push(message.content.clone());
        }

        if self.fail_chat {
            return stream::iter(vec![Err(
                zeroclaw_providers::traits::StreamError::ModelProvider(
                    "synthetic stream failure (#25)".into(),
                ),
            )])
            .boxed();
        }

        if self.streamed {
            let chunk = zeroclaw_providers::traits::StreamEvent::TextDelta(
                zeroclaw_providers::traits::StreamChunk {
                    delta: "stream-done".into(),
                    is_final: false,
                    reasoning: None,
                    token_count: 0,
                },
            );
            stream::iter(vec![
                Ok(chunk),
                Ok(zeroclaw_providers::traits::StreamEvent::Final),
            ])
            .boxed()
        } else {
            stream::iter(vec![Ok(zeroclaw_providers::traits::StreamEvent::Final)]).boxed()
        }
    }

    fn supports_vision(&self) -> bool {
        true
    }
}
impl ::zeroclaw_api::attribution::Attributable for MultimodalCaptureProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "MultimodalCaptureProvider"
    }
}

struct MockTool;

#[async_trait]
impl Tool for MockTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "echo"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<crate::tools::ToolResult> {
        Ok(crate::tools::ToolResult {
            success: true,
            output: "tool-out".into(),
            error: None,
        })
    }
}

#[test]
fn direct_agent_turn_does_not_write_intermediate_native_text_to_stdout() {
    let current_exe = std::env::current_exe().expect("current test binary path");
    let output = std::process::Command::new(current_exe)
        .args([
            "direct_agent_turn_stdout_boundary_helper_4721",
            "--ignored",
            "--nocapture",
        ])
        .output()
        .expect("helper test process should run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "helper failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        stdout,
        stderr
    );
    assert!(
        !stdout.contains("intermediate native narration"),
        "intermediate native narration leaked to stdout:\n{stdout}"
    );
    assert!(
        stderr.contains("intermediate native narration"),
        "intermediate native narration was not routed to stderr:\n{stderr}"
    );
}

#[tokio::test]
#[ignore = "subprocess helper for stdout/stderr boundary regression"]
async fn direct_agent_turn_stdout_boundary_helper_4721() {
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let model_provider = Box::new(MockModelProvider {
        responses: Mutex::new(vec![
            zeroclaw_providers::ChatResponse {
                text: Some("intermediate native narration".into()),
                tool_calls: vec![zeroclaw_providers::ToolCall {
                    id: "tc1".into(),
                    name: "echo".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                }],
                usage: None,
                reasoning_content: None,
            },
            zeroclaw_providers::ChatResponse {
                text: Some("final answer".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            },
        ]),
    });

    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed with valid config");

    let answer = agent
        .turn("run the tool")
        .await
        .expect("turn should finish");
    assert_eq!(answer, "final answer");
}

struct FailingModelProvider;

#[async_trait]
impl ModelProvider for FailingModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<String> {
        Err(anyhow::Error::msg("provider unavailable"))
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<zeroclaw_providers::ChatResponse> {
        Err(anyhow::Error::msg("provider unavailable"))
    }
}

impl ::zeroclaw_api::attribution::Attributable for FailingModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "FailingModelProvider"
    }
}

struct FailingPromptSection;

impl crate::agent::prompt::PromptSection for FailingPromptSection {
    fn name(&self) -> &str {
        "failing-test-section"
    }

    fn build(&self, _ctx: &PromptContext<'_>) -> Result<String> {
        Err(anyhow::Error::msg("synthetic prompt rebuild failure"))
    }
}

struct ToolThenFailingModelProvider {
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl ModelProvider for ToolThenFailingModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<String> {
        Err(anyhow::Error::msg("provider unavailable after tool"))
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<zeroclaw_providers::ChatResponse> {
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            return Ok(zeroclaw_providers::ChatResponse {
                text: Some("running tool".into()),
                tool_calls: vec![zeroclaw_providers::ToolCall {
                    id: "error-path-call".into(),
                    name: "echo".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                }],
                usage: None,
                reasoning_content: None,
            });
        }
        Err(anyhow::Error::msg("provider unavailable after tool"))
    }
}

impl ::zeroclaw_api::attribution::Attributable for ToolThenFailingModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }

    fn alias(&self) -> &str {
        "ToolThenFailingModelProvider"
    }
}

struct SlowTool;

#[async_trait]
impl Tool for SlowTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "echo"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<crate::tools::ToolResult> {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        Ok(crate::tools::ToolResult {
            success: true,
            output: "tool-out".into(),
            error: None,
        })
    }
}

struct CountingTool {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "echo"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<crate::tools::ToolResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(crate::tools::ToolResult {
            success: true,
            output: "tool-out".into(),
            error: None,
        })
    }
}

#[tokio::test]
async fn turn_without_tools_returns_text() {
    let model_provider = Box::new(MockModelProvider {
        responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
            text: Some("hello".into()),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        }]),
    });

    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(XmlToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed with valid config");

    let response = agent.turn("hi").await.unwrap();
    assert_eq!(response, "hello");
}

#[tokio::test]
async fn direct_agent_strict_tool_parsing_ignores_xml_dispatcher_calls() {
    let provider = Box::new(MockModelProvider {
        responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
            text: Some(
                r#"<tool_call>{"name":"echo","arguments":{"value":"ignored"}}</tool_call>"#.into(),
            ),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        }]),
    });

    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );
    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let calls = Arc::new(AtomicUsize::new(0));
    let agent_config = zeroclaw_config::schema::AliasedAgentConfig {
        resolved: zeroclaw_config::schema::ResolvedRuntime {
            strict_tool_parsing: true,
            ..Default::default()
        },
        ..zeroclaw_config::schema::AliasedAgentConfig::default()
    };
    let mut agent = Agent::builder()
        .model_provider(provider)
        .tools(vec![Box::new(CountingTool {
            calls: Arc::clone(&calls),
        })])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(XmlToolDispatcher))
        .config(agent_config)
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed with valid config");

    let system_prompt = agent
        .build_system_prompt()
        .expect("system prompt should render");
    assert!(
        !system_prompt.contains("## Tools"),
        "strict parsing should not advertise text tool instructions"
    );
    assert!(
        !system_prompt.contains("<tool_call"),
        "strict parsing should not advertise XML tool calls"
    );

    let response = agent.turn("hi").await.unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(response.contains("<tool_call>"));
}

#[test]
fn native_agent_prompt_omits_duplicate_tools_section() {
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let workspace = tempfile::TempDir::new().expect("temp dir");
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, workspace.path(), None)
            .expect("memory creation should succeed with valid config"),
    );
    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});

    let native_agent = Agent::builder()
        .model_provider(Box::new(MockModelProvider {
            responses: Mutex::new(vec![]),
        }))
        .tools(vec![Box::new(MockTool)])
        .memory(Arc::clone(&mem))
        .observer(Arc::clone(&observer))
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(workspace.path().to_path_buf())
        .build()
        .expect("agent builder should succeed with valid config");
    let native_prompt = native_agent.build_system_prompt().unwrap();
    assert!(!native_prompt.contains("## Tools"));
    assert!(!native_prompt.contains("echo"));

    let xml_agent = Agent::builder()
        .model_provider(Box::new(MockModelProvider {
            responses: Mutex::new(vec![]),
        }))
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(XmlToolDispatcher))
        .workspace_dir(workspace.path().to_path_buf())
        .build()
        .expect("agent builder should succeed with valid config");
    let xml_prompt = xml_agent.build_system_prompt().unwrap();
    assert!(xml_prompt.contains("## Tools"));
    assert!(xml_prompt.contains("echo"));
    assert!(xml_prompt.contains("## Tool Use Protocol"));
}

/// Builds an `Agent` the way `from_config` wires persona in (see
/// `agent.rs`'s `from_config`): resolve `config.persona_for_agent(alias)`,
/// render it, and hand the rendered section straight to
/// `SystemPromptBuilder::with_defaults`. Skips the network/provider
/// machinery `from_config` also does, since that is orthogonal to whether
/// persona reaches this pipeline's built prompt.
fn build_agent_with_persona_from_config(
    config: &zeroclaw_config::schema::Config,
    agent_alias: &str,
) -> Agent {
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let workspace = tempfile::TempDir::new().expect("temp dir");
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, workspace.path(), None)
            .expect("memory creation should succeed with valid config"),
    );
    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let persona_section = config
        .persona_for_agent(agent_alias)
        .and_then(zeroclaw_config::persona::PersonaKnobs::to_prompt_section);

    Agent::builder()
        .model_provider(Box::new(MockModelProvider {
            responses: Mutex::new(vec![]),
        }))
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(workspace.keep())
        .prompt_builder(SystemPromptBuilder::with_defaults(persona_section))
        .build()
        .expect("agent builder should succeed with valid config")
}

/// A card that names a persona reaches this pipeline's built prompt —
/// closing the gap `606e2ea19` left open for `Agent::build_system_prompt`.
#[test]
fn carded_agent_persona_reaches_this_pipelines_built_prompt() {
    let toml = r#"
            [providers.models.custom.default]
            api_key = "k"
            model = "test-model"
            uri = "https://example.com/v1"
            wire_api = "chat_completions"

            [risk_profiles.default]
            level = "supervised"

            [personas.terse]
            directness = "xhigh"

            [cards.analyst]
            persona = "terse"
            risk_profile = "default"

            [cards.analyst.grants]
            tools = [{ tool = "memory_recall", class = "local_read" }]

            [agents.default]
            enabled = true
            model_provider = "custom.default"
            card = "analyst"
        "#;
    let config: zeroclaw_config::schema::Config = toml::from_str(toml).expect("valid config");

    let agent = build_agent_with_persona_from_config(&config, "default");
    let prompt = agent.build_system_prompt().unwrap();

    assert!(
        prompt.contains("## Tone"),
        "carded persona must render a Tone section"
    );
    assert!(
        prompt.contains(
            "- directness (xhigh): Give the verdict immediately and plainly. Example: \"The date is invalid. Correct it first.\""
        ),
        "the card's `terse` persona's xhigh directness dial must be the text that renders"
    );
}

/// A direct (uncarded) `agents.<alias>.persona` reaches this pipeline's
/// built prompt too.
#[test]
fn direct_persona_agent_reaches_this_pipelines_built_prompt() {
    let toml = r#"
            [providers.models.custom.default]
            api_key = "k"
            model = "test-model"
            uri = "https://example.com/v1"
            wire_api = "chat_completions"

            [risk_profiles.default]
            level = "supervised"

            [personas.terse]
            directness = "xhigh"

            [agents.default]
            enabled = true
            model_provider = "custom.default"
            risk_profile = "default"
            persona = "terse"
        "#;
    let config: zeroclaw_config::schema::Config = toml::from_str(toml).expect("valid config");

    let agent = build_agent_with_persona_from_config(&config, "default");
    let prompt = agent.build_system_prompt().unwrap();

    assert!(
        prompt.contains("## Tone"),
        "a direct persona field must render a Tone section"
    );
    assert!(
        prompt.contains(
            "- directness (xhigh): Give the verdict immediately and plainly. Example: \"The date is invalid. Correct it first.\""
        ),
        "the direct `terse` persona's xhigh directness dial must be the text that renders"
    );
}

/// An agent with neither a card nor a direct persona must produce no
/// `## Tone` section at all — the regression guard for every existing
/// agent that predates persona dials.
#[test]
fn agent_with_no_persona_configured_has_no_tone_section() {
    let toml = r#"
            [providers.models.custom.default]
            api_key = "k"
            model = "test-model"
            uri = "https://example.com/v1"
            wire_api = "chat_completions"

            [risk_profiles.default]
            level = "supervised"

            [agents.default]
            enabled = true
            model_provider = "custom.default"
            risk_profile = "default"
        "#;
    let config: zeroclaw_config::schema::Config = toml::from_str(toml).expect("valid config");

    let agent = build_agent_with_persona_from_config(&config, "default");
    let prompt = agent.build_system_prompt().unwrap();

    assert!(
        !prompt.contains("## Tone"),
        "no persona configured must mean no Tone section"
    );
}

mod surface2_tests {
    use super::*;
    use crate::agent::dispatcher::{NativeToolDispatcher, XmlToolDispatcher};

    /// Marker text produced by the section-based prompt builder when tools
    /// are advertised as XML/text instructions rather than native tool specs.
    const XML_TOOLS_MARKER: &str = "## Tools";
    type CapturedTranscripts = Arc<Mutex<Vec<Vec<ChatMessage>>>>;

    /// Test provider that captures the provider-visible transcript and
    /// reports a configurable native-tool capability.
    struct CapturingModelProvider {
        responses: Mutex<Vec<zeroclaw_providers::ChatResponse>>,
        supports_native: bool,
        captured_messages: CapturedTranscripts,
    }

    #[async_trait]
    impl ModelProvider for CapturingModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            Ok("ok".into())
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<zeroclaw_providers::ChatResponse> {
            self.captured_messages
                .lock()
                .push(request.messages.to_vec());
            let mut guard = self.responses.lock();
            if guard.is_empty() {
                return Ok(zeroclaw_providers::ChatResponse {
                    text: Some("done".into()),
                    tool_calls: vec![],
                    usage: None,
                    reasoning_content: None,
                });
            }
            Ok(guard.remove(0))
        }

        fn supports_native_tools(&self) -> bool {
            self.supports_native
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for CapturingModelProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "CapturingModelProvider"
        }
    }

    fn capturing_provider(supports_native: bool) -> (Box<dyn ModelProvider>, CapturedTranscripts) {
        let captured: CapturedTranscripts = Arc::new(Mutex::new(Vec::new()));
        (
            Box::new(CapturingModelProvider {
                responses: Mutex::new(vec![]),
                supports_native,
                captured_messages: Arc::clone(&captured),
            }),
            captured,
        )
    }

    fn test_agent_with_provider(
        provider: Box<dyn ModelProvider>,
        tools: Vec<Box<dyn Tool>>,
    ) -> Agent {
        test_agent_with_provider_and_multimodal(provider, tools, None, None)
    }

    fn test_agent_with_provider_and_multimodal(
        provider: Box<dyn ModelProvider>,
        tools: Vec<Box<dyn Tool>>,
        tool_dispatcher: Option<Box<dyn ToolDispatcher>>,
        multimodal_config: Option<zeroclaw_config::schema::MultimodalConfig>,
    ) -> Agent {
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let workspace = tempfile::TempDir::new().expect("temp dir");
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, workspace.path(), None)
                .expect("memory creation should succeed"),
        );
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let mut builder = Agent::builder()
            .model_provider(provider)
            .tools(tools)
            .memory(mem)
            .observer(observer)
            .workspace_dir(workspace.path().to_path_buf());
        if let Some(dispatcher) = tool_dispatcher {
            builder = builder.tool_dispatcher(dispatcher);
        } else {
            builder = builder.tool_dispatcher(Box::new(NativeToolDispatcher));
        }
        if let Some(mm) = multimodal_config {
            builder = builder.multimodal_config(mm);
        }
        builder.build().expect("agent builder should succeed")
    }

    #[test]
    fn build_system_prompt_with_dispatcher_reflects_dispatcher_mode() {
        let workspace = tempfile::TempDir::new().expect("temp dir");
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, workspace.path(), None)
                .expect("memory creation should succeed"),
        );
        let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
        let agent = Agent::builder()
            .model_provider(Box::new(MockModelProvider {
                responses: Mutex::new(vec![]),
            }))
            .tools(vec![Box::new(MockTool)])
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(workspace.path().to_path_buf())
            .build()
            .expect("agent builder should succeed");

        let native_prompt = agent
            .build_system_prompt_with_dispatcher(&NativeToolDispatcher as &dyn ToolDispatcher)
            .unwrap();
        assert!(
            !native_prompt.contains(XML_TOOLS_MARKER),
            "native dispatcher must not emit XML tool listing"
        );

        let xml_prompt = agent
            .build_system_prompt_with_dispatcher(&XmlToolDispatcher as &dyn ToolDispatcher)
            .unwrap();
        assert!(
            xml_prompt.contains(XML_TOOLS_MARKER),
            "xml dispatcher must emit XML tool listing"
        );
    }

    #[test]
    fn rebuild_system_prompt_switches_to_xml_when_active_provider_non_native() {
        let (provider, _) = capturing_provider(true);
        let mut agent = test_agent_with_provider(provider, vec![Box::new(MockTool)]);

        // Seed a native-style system prompt as if the agent was built
        // against a native-capable base provider.
        let native_prompt = agent
            .build_system_prompt_with_dispatcher(&NativeToolDispatcher as &dyn ToolDispatcher)
            .unwrap();
        agent.history = vec![ConversationMessage::Chat(ChatMessage::system(
            native_prompt,
        ))];

        // Active provider for this turn does not support native tools.
        agent
            .rebuild_system_prompt_for_dispatcher(&XmlToolDispatcher)
            .expect("rebuild should succeed");

        let prompt = match &agent.history[0] {
            ConversationMessage::Chat(msg) => msg.content.clone(),
            _ => panic!("history[0] should be a chat message"),
        };
        assert!(
            prompt.contains(XML_TOOLS_MARKER),
            "prompt must be rebuilt with XML tool listing"
        );
    }

    #[test]
    fn rebuild_system_prompt_switches_to_native_when_active_provider_native() {
        let (provider, _) = capturing_provider(false);
        let mut agent = test_agent_with_provider(provider, vec![Box::new(MockTool)]);

        let xml_prompt = agent
            .build_system_prompt_with_dispatcher(&XmlToolDispatcher as &dyn ToolDispatcher)
            .unwrap();
        agent.history = vec![ConversationMessage::Chat(ChatMessage::system(xml_prompt))];

        // Active provider for this turn supports native tools.
        agent
            .rebuild_system_prompt_for_dispatcher(&NativeToolDispatcher)
            .expect("rebuild should succeed");

        let prompt = match &agent.history[0] {
            ConversationMessage::Chat(msg) => msg.content.clone(),
            _ => panic!("history[0] should be a chat message"),
        };
        assert!(
            !prompt.contains(XML_TOOLS_MARKER),
            "prompt must be rebuilt without XML tool listing"
        );
    }

    #[tokio::test]
    async fn turn_uses_active_provider_tool_mode_for_transcript() {
        let (provider, captured) = capturing_provider(false);
        let mut agent = test_agent_with_provider(provider, vec![Box::new(MockTool)]);

        // The base provider does not support native tools, so the active
        // provider resolved by the turn path must be non-native. The
        // provider-visible transcript should reflect that.
        agent.turn("hello").await.expect("turn should succeed");

        let messages = captured.lock();
        let first_call = messages
            .first()
            .expect("provider should have received a request");
        let system = first_call
            .iter()
            .find(|m| m.role == "system")
            .expect("transcript must contain a system message");
        assert!(
            system.content.contains(XML_TOOLS_MARKER),
            "system prompt must advertise XML tools when active provider is non-native"
        );
    }

    #[tokio::test]
    async fn turn_streamed_uses_active_provider_tool_mode_for_transcript() {
        let (provider, captured) = capturing_provider(false);
        let mut agent = test_agent_with_provider(provider, vec![Box::new(MockTool)]);
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(16);

        agent
            .turn_streamed("hello", event_tx, None)
            .await
            .expect("streamed turn should succeed");

        let messages = captured.lock();
        let first_call = messages
            .first()
            .expect("provider should have received a request");
        let system = first_call
            .iter()
            .find(|m| m.role == "system")
            .expect("transcript must contain a system message");
        assert!(
            system.content.contains(XML_TOOLS_MARKER),
            "streamed system prompt must advertise XML tools when active provider is non-native"
        );
    }

    #[tokio::test]
    async fn turn_rebuilds_prompt_for_vision_routed_xml_provider() {
        // Base provider supports native tools but not vision. The configured
        // vision provider is a custom OpenAI-compatible endpoint: it supports
        // vision but not native tools.
        let (base_provider, _captured) = capturing_provider(true);
        let mm_config = zeroclaw_config::schema::MultimodalConfig {
            vision_model_provider: Some("custom:http://127.0.0.1:9".into()),
            ..Default::default()
        };
        let mut agent = test_agent_with_provider_and_multimodal(
            base_provider,
            vec![Box::new(MockTool)],
            Some(Box::new(NativeToolDispatcher)),
            Some(mm_config),
        );

        let msg = "describe this image [IMAGE:data:image/png;base64,iVBORw0KGgo=]";

        // The vision provider will fail to connect to localhost:9, but the
        // prompt rebuild and provider-visible transcript happen before the
        // network call.
        let result = agent.turn(msg).await;
        assert!(
            result.is_err(),
            "vision provider chat should fail to connect"
        );

        let system_content = match &agent.history[0] {
            ConversationMessage::Chat(m) => m.content.clone(),
            _ => panic!("history[0] should be a chat message"),
        };
        assert!(
            system_content.contains(XML_TOOLS_MARKER),
            "stored system prompt must be rebuilt for XML vision provider"
        );

        let provider_messages = XmlToolDispatcher.to_provider_messages(&agent.history);
        let system = provider_messages
            .iter()
            .find(|m| m.role == "system")
            .expect("transcript must contain a system message");
        assert!(
            system.content.contains(XML_TOOLS_MARKER),
            "provider-visible transcript must advertise XML tools for vision provider"
        );
    }

    #[tokio::test]
    async fn turn_streamed_rebuilds_prompt_for_vision_routed_native_provider() {
        // Base provider does not support native tools or vision. The
        // configured vision provider is an Anthropic-compatible endpoint:
        // it supports both vision and native tools.
        let (base_provider, _captured) = capturing_provider(false);
        let mm_config = zeroclaw_config::schema::MultimodalConfig {
            vision_model_provider: Some("anthropic-custom:http://127.0.0.1:9".into()),
            ..Default::default()
        };
        let mut agent = test_agent_with_provider_and_multimodal(
            base_provider,
            vec![Box::new(MockTool)],
            Some(Box::new(XmlToolDispatcher)),
            Some(mm_config),
        );

        let msg = "describe this image [IMAGE:data:image/png;base64,iVBORw0KGgo=]";
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(16);

        let result = agent.turn_streamed(msg, event_tx, None).await;
        assert!(
            result.is_err(),
            "vision provider chat should fail to connect"
        );

        let system_content = match &agent.history[0] {
            ConversationMessage::Chat(m) => m.content.clone(),
            _ => panic!("history[0] should be a chat message"),
        };
        assert!(
            !system_content.contains(XML_TOOLS_MARKER),
            "stored system prompt must be rebuilt for native vision provider"
        );

        let provider_messages = NativeToolDispatcher.to_provider_messages(&agent.history);
        let system = provider_messages
            .iter()
            .find(|m| m.role == "system")
            .expect("transcript must contain a system message");
        assert!(
            !system.content.contains(XML_TOOLS_MARKER),
            "provider-visible transcript must advertise native tools for vision provider"
        );
    }
}

#[tokio::test]
async fn turn_with_native_dispatcher_handles_tool_results_variant() {
    let model_provider = Box::new(MockModelProvider {
        responses: Mutex::new(vec![
            zeroclaw_providers::ChatResponse {
                text: Some(String::new()),
                tool_calls: vec![zeroclaw_providers::ToolCall {
                    id: "tc1".into(),
                    name: "echo".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                }],
                usage: None,
                reasoning_content: None,
            },
            zeroclaw_providers::ChatResponse {
                text: Some("done".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            },
        ]),
    });

    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed with valid config");

    let response = agent.turn("hi").await.unwrap();
    assert_eq!(response, "done");
    assert!(
        agent
            .history()
            .iter()
            .any(|msg| matches!(msg, ConversationMessage::ToolResults(_)))
    );
}

#[tokio::test]
async fn turn_routes_with_hint_when_query_classification_matches() {
    let seen_models = Arc::new(Mutex::new(Vec::new()));
    let model_provider = Box::new(ModelCaptureModelProvider {
        responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
            text: Some("classified".into()),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        }]),
        seen_models: seen_models.clone(),
    });

    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut route_model_by_hint = HashMap::new();
    route_model_by_hint.insert("fast".to_string(), "anthropic/claude-haiku-4-5".to_string());
    let mut agent = Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .classification_config(zeroclaw_config::schema::QueryClassificationConfig {
            enabled: true,
            rules: vec![zeroclaw_config::schema::ClassificationRule {
                hint: "fast".to_string(),
                keywords: vec!["quick".to_string()],
                patterns: vec![],
                min_length: None,
                max_length: None,
                priority: 10,
            }],
        })
        .available_hints(vec!["fast".to_string()])
        .route_model_by_hint(route_model_by_hint)
        .build()
        .expect("agent builder should succeed with valid config");

    let response = agent.turn("quick summary please").await.unwrap();
    assert_eq!(response, "classified");
    let seen = seen_models.lock();
    assert_eq!(seen.as_slice(), &["hint:fast".to_string()]);
}

#[tokio::test]
async fn from_config_passes_extra_headers_to_custom_provider() {
    use axum::{Json, Router, http::HeaderMap, routing::post};
    use tempfile::TempDir;
    use tokio::net::TcpListener;

    let captured_headers: Arc<std::sync::Mutex<Option<HashMap<String, String>>>> =
        Arc::new(std::sync::Mutex::new(None));
    let captured_headers_clone = captured_headers.clone();

    let app = Router::new().route(
        "/chat/completions",
        post(
            move |headers: HeaderMap, Json(_body): Json<serde_json::Value>| {
                let captured_headers = captured_headers_clone.clone();
                async move {
                    let collected = headers
                        .iter()
                        .filter_map(|(name, value)| {
                            value
                                .to_str()
                                .ok()
                                .map(|value| (name.as_str().to_string(), value.to_string()))
                        })
                        .collect();
                    *captured_headers.lock().unwrap() = Some(collected);
                    Json(serde_json::json!({
                        "choices": [{
                            "message": {
                                "content": "hello from mock"
                            }
                        }]
                    }))
                }
            },
        ),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = listener.local_addr().unwrap();
    let server_handle = zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let tmp = TempDir::new().expect("temp dir");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace_dir).unwrap();

    let mut config = zeroclaw_config::schema::Config {
        data_dir: workspace_dir,
        config_path: tmp.path().join("config.toml"),
        ..Default::default()
    };
    {
        let entry = config
            .providers
            .models
            .ensure("custom", "default")
            .expect("custom model_provider type slot");
        entry.api_key = Some("test-key".to_string());
        entry.model = Some("test-model".to_string());
        entry.uri = Some(format!("http://{mock_addr}"));
        entry.extra_headers.insert(
            "User-Agent".to_string(),
            "zeroclaw-web-test/1.0".to_string(),
        );
        entry
            .extra_headers
            .insert("X-Title".to_string(), "zeroclaw-web".to_string());
    }
    config.memory.backend = "none".to_string();
    config.memory.auto_save = false;

    // An explicit agent is required. Wire up a minimal agent that
    // points at the synthesized model_provider entry, then construct
    // Agent::from_config against it.
    config.risk_profiles.insert(
        "test-profile".to_string(),
        zeroclaw_config::schema::RiskProfileConfig::default(),
    );
    let agent_cfg = zeroclaw_config::schema::AliasedAgentConfig {
        model_provider: "custom.default".into(),
        risk_profile: "test-profile".into(),
        ..zeroclaw_config::schema::AliasedAgentConfig::default()
    };
    config.agents.insert("test-agent".to_string(), agent_cfg);

    let mut agent = Agent::from_config(&config, "test-agent")
        .await
        .expect("agent from config");
    let response = agent.turn("hello").await.expect("agent turn");

    assert_eq!(response, "hello from mock");

    let headers = captured_headers
        .lock()
        .unwrap()
        .clone()
        .expect("captured headers");
    assert_eq!(
        headers.get("user-agent").map(String::as_str),
        Some("zeroclaw-web-test/1.0")
    );
    assert_eq!(
        headers.get("x-title").map(String::as_str),
        Some("zeroclaw-web")
    );

    server_handle.abort();
}

#[tokio::test]
async fn from_config_accepts_openai_alias_with_requires_openai_auth() {
    use tempfile::TempDir;
    use zeroclaw_config::schema::{
        AliasedAgentConfig, Config, ModelProviderConfig, OpenAIModelProviderConfig,
        RiskProfileConfig, WireApi,
    };

    let tmp = TempDir::new().expect("temp dir");
    let workspace_dir = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace_dir).expect("workspace dir");

    let mut config = Config {
        data_dir: workspace_dir,
        config_path: tmp.path().join("config.toml"),
        ..Default::default()
    };
    config.memory.backend = "none".to_string();
    config.memory.auto_save = false;
    config
        .risk_profiles
        .insert("test-profile".to_string(), RiskProfileConfig::default());
    config.providers.models.openai.insert(
        "codex".to_string(),
        OpenAIModelProviderConfig {
            base: ModelProviderConfig {
                model: Some("gpt-5.4".to_string()),
                requires_openai_auth: true,
                wire_api: Some(WireApi::Responses),
                ..ModelProviderConfig::default()
            },
        },
    );
    config.agents.insert(
        "test-agent".to_string(),
        AliasedAgentConfig {
            model_provider: "openai.codex".into(),
            risk_profile: "test-profile".into(),
            ..AliasedAgentConfig::default()
        },
    );

    let result = Agent::from_config(&config, "test-agent").await;

    assert!(
        result.is_ok(),
        "openai alias with requires_openai_auth should construct via Codex OAuth path: {}",
        result.err().unwrap()
    );
}

#[test]
fn builder_allowed_tools_none_keeps_all_tools() {
    let model_provider = Box::new(MockModelProvider {
        responses: Mutex::new(vec![]),
    });

    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let agent = Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .allowed_tools(None)
        .build()
        .expect("agent builder should succeed with valid config");

    assert_eq!(agent.tools.len(), 1);
    assert_eq!(agent.tools[0].name(), "echo");
}

#[test]
fn builder_allowed_tools_some_filters_tools() {
    let model_provider = Box::new(MockModelProvider {
        responses: Mutex::new(vec![]),
    });

    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let agent = Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .allowed_tools(Some(vec!["nonexistent".to_string()]))
        .build()
        .expect("agent builder should succeed with valid config");

    assert!(
        agent.tools.is_empty(),
        "No tools should match a non-existent allowlist entry"
    );
}

#[test]
fn session_cwd_keeps_workspace_in_allowed_roots() {
    let workspace = std::env::temp_dir().join("zeroclaw_test_session_cwd_workspace");
    let session = std::env::temp_dir().join("zeroclaw_test_session_cwd_session");
    let _ = std::fs::create_dir_all(&workspace);
    let _ = std::fs::create_dir_all(&session);

    let skill_file = workspace.join("SKILL.md");
    let _ = std::fs::write(&skill_file, "body");
    // is_resolved_path_allowed expects a canonicalized path (symlinks resolved).
    let skill_resolved = std::fs::canonicalize(&skill_file).unwrap_or(skill_file);

    let risk_profile = zeroclaw_config::schema::RiskProfileConfig::default();

    // Policy WITH the fix: workspace pushed into allowed_roots.
    let mut policy = SecurityPolicy::from_risk_profile(&risk_profile, &session);
    policy.allowed_roots.push(workspace.clone());
    assert!(
        policy.is_resolved_path_allowed(&skill_resolved),
        "workspace skills must remain readable when session_cwd differs"
    );

    // Without the push the same path must be denied, confirming the push
    // is the load-bearing fix rather than an incidental side-effect.
    let policy_no_push = SecurityPolicy::from_risk_profile(&risk_profile, &session);
    assert!(
        !policy_no_push.is_resolved_path_allowed(&skill_resolved),
        "without allowed_roots.push, workspace files must be outside the sandbox"
    );
}

#[test]
fn seed_history_prepends_system_and_skips_system_from_seed() {
    let model_provider = Box::new(MockModelProvider {
        responses: Mutex::new(vec![]),
    });

    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed with valid config");

    let seed = vec![
        ChatMessage::system("old system prompt"),
        ChatMessage::user("hello"),
        ChatMessage::assistant("hi there"),
    ];
    agent.seed_history(&seed);

    let history = agent.history();
    // First message should be a freshly built system prompt (not the seed one)
    assert!(matches!(&history[0], ConversationMessage::Chat(m) if m.role == "system"));
    // System message from seed should be skipped, so next is user
    assert!(
        matches!(&history[1], ConversationMessage::Chat(m) if m.role == "user" && m.content == "hello")
    );
    assert!(
        matches!(&history[2], ConversationMessage::Chat(m) if m.role == "assistant" && m.content == "hi there")
    );
    assert_eq!(history.len(), 3);
}

#[test]
fn set_tool_dispatcher_refreshes_existing_system_prompt() {
    use zeroclaw_api::model_provider::{ChatMessage, ConversationMessage};

    let model_provider = Box::new(MockModelProvider {
        responses: Mutex::new(vec![]),
    });
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );
    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(XmlToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed with valid config");

    agent.seed_history(&[ChatMessage::user("hello")]);
    let before = match agent.history().first() {
        Some(ConversationMessage::Chat(m)) if m.role == "system" => m.content.clone(),
        other => panic!("expected a system prompt first, got {other:?}"),
    };
    assert!(
        before.contains("Tool Use Protocol"),
        "xml dispatcher system prompt should carry the xml tool protocol"
    );

    agent.set_tool_dispatcher(Box::new(NativeToolDispatcher));
    let after = match agent.history().first() {
        Some(ConversationMessage::Chat(m)) if m.role == "system" => m.content.clone(),
        other => panic!("expected a system prompt first, got {other:?}"),
    };
    assert!(
        !after.contains("Tool Use Protocol"),
        "native dispatcher system prompt must not carry the xml tool protocol after swap"
    );
}

#[test]
fn seed_conversation_history_preserves_tool_call_variants() {
    use zeroclaw_api::model_provider::{
        ChatMessage, ConversationMessage, ToolCall, ToolResultMessage,
    };

    let provider = Box::new(MockModelProvider {
        responses: Mutex::new(vec![]),
    });

    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = Agent::builder()
        .model_provider(provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed with valid config");

    let messages = vec![
        ConversationMessage::Chat(ChatMessage::user("run it")),
        ConversationMessage::AssistantToolCalls {
            text: None,
            tool_calls: vec![ToolCall {
                id: "tc-1".into(),
                name: "shell".into(),
                arguments: r#"{"command":"ls"}"#.into(),
                extra_content: None,
            }],
            reasoning_content: None,
        },
        ConversationMessage::ToolResults(vec![ToolResultMessage {
            tool_call_id: "tc-1".into(),
            content: "ok".into(),
            tool_name: String::new(),
        }]),
        ConversationMessage::Chat(ChatMessage::assistant("done")),
    ];

    agent.seed_conversation_history(messages);

    // System prompt may have been prepended; find non-system messages
    let non_system: Vec<_> = agent
        .history()
        .iter()
        .filter(|m| !matches!(m, ConversationMessage::Chat(c) if c.role == "system"))
        .collect();

    assert_eq!(non_system.len(), 4);
    assert!(
        matches!(non_system[1], ConversationMessage::AssistantToolCalls { tool_calls, .. } if tool_calls[0].id == "tc-1")
    );
    assert!(
        matches!(non_system[2], ConversationMessage::ToolResults(r) if r[0].tool_call_id == "tc-1")
    );
}

#[test]
fn seed_history_trims_over_cap_restore_and_returns_transport_event() {
    let capturing = Arc::new(CapturingObserver::default());
    let observer: Arc<dyn Observer> = capturing.clone();
    let mut agent = trim_history_test_agent(2, observer);

    let event = agent.seed_history_with_event(&[
        ChatMessage::user("old request"),
        ChatMessage::assistant("old answer"),
        ChatMessage::user("new request"),
        ChatMessage::assistant("new answer"),
    ]);

    assert!(matches!(
        event,
        Some(TurnEvent::HistoryTrimmed {
            dropped_messages: 2,
            kept_turns: 1,
            ..
        })
    ));
    assert!(agent.history_has_trim_breadcrumb);
    assert!(matches!(
        agent.history.get(2),
        Some(ConversationMessage::Chat(message))
            if message.role == "user" && message.content == "new request"
    ));
    assert_eq!(
        capturing
            .events
            .lock()
            .iter()
            .filter(|event| matches!(event, ObserverEvent::HistoryTrimmed { .. }))
            .count(),
        1
    );
}

#[test]
fn seed_conversation_history_trims_over_cap_restore_without_splitting_tools() {
    use zeroclaw_providers::{ToolCall, ToolResultMessage};

    let capturing = Arc::new(CapturingObserver::default());
    let observer: Arc<dyn Observer> = capturing.clone();
    let mut agent = trim_history_test_agent(4, observer);
    let event = agent.seed_conversation_history_with_event(vec![
        ConversationMessage::Chat(ChatMessage::user("old request")),
        ConversationMessage::Chat(ChatMessage::assistant("old answer")),
        ConversationMessage::Chat(ChatMessage::user("new request")),
        ConversationMessage::AssistantToolCalls {
            text: Some("running".into()),
            tool_calls: vec![ToolCall {
                id: "seed-call".into(),
                name: "echo".into(),
                arguments: "{}".into(),
                extra_content: None,
            }],
            reasoning_content: None,
        },
        ConversationMessage::ToolResults(vec![ToolResultMessage {
            tool_call_id: "seed-call".into(),
            content: "result".into(),
            tool_name: "echo".into(),
        }]),
        ConversationMessage::Chat(ChatMessage::assistant("new answer")),
    ]);

    assert!(matches!(
        event,
        Some(TurnEvent::HistoryTrimmed {
            dropped_messages: 2,
            kept_turns: 1,
            ..
        })
    ));
    assert!(matches!(
        (&agent.history[3], &agent.history[4]),
        (
            ConversationMessage::AssistantToolCalls { tool_calls, .. },
            ConversationMessage::ToolResults(results),
        ) if tool_calls[0].id == "seed-call" && results[0].tool_call_id == "seed-call"
    ));
    assert_eq!(
        capturing
            .events
            .lock()
            .iter()
            .filter(|event| matches!(event, ObserverEvent::HistoryTrimmed { .. }))
            .count(),
        1
    );
}

#[test]
fn clear_history_resets_trim_breadcrumb_provenance_before_reuse() {
    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = trim_history_test_agent(2, observer);
    agent.history = vec![
        ConversationMessage::Chat(ChatMessage::system("system")),
        ConversationMessage::Chat(ChatMessage::user("old user")),
        ConversationMessage::Chat(ChatMessage::assistant("old assistant")),
        ConversationMessage::Chat(ChatMessage::user("new user")),
        ConversationMessage::Chat(ChatMessage::assistant("new assistant")),
    ];
    let _ = agent.trim_history(None);
    assert!(agent.history_has_trim_breadcrumb);

    agent.clear_history();
    assert!(!agent.history_has_trim_breadcrumb);

    let breadcrumb = crate::i18n::get_required_cli_string("history-trim-breadcrumb");
    agent.seed_history(&[
        ChatMessage::user(breadcrumb.clone()),
        ChatMessage::assistant("user-authored marker reply"),
    ]);
    assert!(!agent.history_has_trim_breadcrumb);

    agent.seed_history(&[
        ChatMessage::user("later user"),
        ChatMessage::assistant("later assistant"),
    ]);
    assert!(agent.history_has_trim_breadcrumb);
    assert_eq!(
        agent
            .history
            .iter()
            .filter(|message| matches!(
                message,
                ConversationMessage::Chat(chat) if chat.content == breadcrumb
            ))
            .count(),
        1,
        "the user-authored marker must be dropped as an ordinary old turn before one synthetic breadcrumb is inserted"
    );
    assert!(agent.history.iter().any(|message| matches!(
        message,
        ConversationMessage::Chat(chat)
            if chat.role == "user" && chat.content == "later user"
    )));
}

#[test]
fn append_seed_history_preserves_existing_trim_breadcrumb_provenance() {
    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = trim_history_test_agent(2, observer);
    agent.seed_history(&[
        ChatMessage::user("old user"),
        ChatMessage::assistant("old assistant"),
        ChatMessage::user("kept user"),
        ChatMessage::assistant("kept assistant"),
    ]);
    assert!(agent.history_has_trim_breadcrumb);

    agent.seed_history(&[
        ChatMessage::user("appended user"),
        ChatMessage::assistant("appended assistant"),
    ]);

    let breadcrumb = crate::i18n::get_required_cli_string("history-trim-breadcrumb");
    assert!(agent.history_has_trim_breadcrumb);
    assert_eq!(
        agent
            .history
            .iter()
            .filter(|message| matches!(
                message,
                ConversationMessage::Chat(chat) if chat.content == breadcrumb
            ))
            .count(),
        1
    );
    assert!(agent.history.iter().any(|message| matches!(
        message,
        ConversationMessage::Chat(chat)
            if chat.role == "user" && chat.content == "appended user"
    )));
}

#[test]
fn append_conversation_seed_preserves_existing_trim_breadcrumb_provenance() {
    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = trim_history_test_agent(2, observer);
    agent.seed_conversation_history(vec![
        ConversationMessage::Chat(ChatMessage::user("old user")),
        ConversationMessage::Chat(ChatMessage::assistant("old assistant")),
        ConversationMessage::Chat(ChatMessage::user("kept user")),
        ConversationMessage::Chat(ChatMessage::assistant("kept assistant")),
    ]);
    assert!(agent.history_has_trim_breadcrumb);

    agent.seed_conversation_history(vec![
        ConversationMessage::Chat(ChatMessage::user("appended user")),
        ConversationMessage::Chat(ChatMessage::assistant("appended assistant")),
    ]);

    let breadcrumb = crate::i18n::get_required_cli_string("history-trim-breadcrumb");
    assert!(agent.history_has_trim_breadcrumb);
    assert_eq!(
        agent
            .history
            .iter()
            .filter(|message| matches!(
                message,
                ConversationMessage::Chat(chat) if chat.content == breadcrumb
            ))
            .count(),
        1
    );
    assert!(agent.history.iter().any(|message| matches!(
        message,
        ConversationMessage::Chat(chat)
            if chat.role == "user" && chat.content == "appended user"
    )));
}

/// Mock provider that captures whether tool specs were passed to `stream_chat`
/// and returns a tool call followed by a text response through the stream.
struct StreamToolCaptureModelProvider {
    tools_received: Arc<Mutex<Vec<bool>>>,
    call_count: Arc<Mutex<usize>>,
}

#[async_trait]
impl ModelProvider for StreamToolCaptureModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<String> {
        Ok("ok".into())
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<zeroclaw_providers::ChatResponse> {
        self.tools_received.lock().push(request.tools.is_some());
        let mut count = self.call_count.lock();
        *count += 1;
        if *count == 1 {
            Ok(zeroclaw_providers::ChatResponse {
                text: Some(String::new()),
                tool_calls: vec![zeroclaw_providers::ToolCall {
                    id: "00000000-0000-0000-0000-000000000001".into(),
                    name: "echo".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                }],
                usage: None,
                reasoning_content: None,
            })
        } else {
            Ok(zeroclaw_providers::ChatResponse {
                text: Some("stream-done".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            })
        }
    }

    fn supports_native_tools(&self) -> bool {
        true
    }

    fn stream_chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
        _options: zeroclaw_providers::traits::StreamOptions,
    ) -> futures_util::stream::BoxStream<
        'static,
        zeroclaw_providers::traits::StreamResult<zeroclaw_providers::traits::StreamEvent>,
    > {
        use futures_util::stream::{self, StreamExt};
        self.tools_received.lock().push(request.tools.is_some());
        let mut count = self.call_count.lock();
        *count += 1;
        if *count == 1 {
            let tc =
                zeroclaw_providers::traits::StreamEvent::ToolCall(zeroclaw_providers::ToolCall {
                    id: "00000000-0000-0000-0000-000000000001".into(),
                    name: "echo".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                });
            stream::iter(vec![
                Ok(tc),
                Ok(zeroclaw_providers::traits::StreamEvent::Final),
            ])
            .boxed()
        } else {
            let chunk = zeroclaw_providers::traits::StreamEvent::TextDelta(
                zeroclaw_providers::traits::StreamChunk {
                    delta: "stream-done".into(),
                    is_final: false,
                    reasoning: None,
                    token_count: 0,
                },
            );
            stream::iter(vec![
                Ok(chunk),
                Ok(zeroclaw_providers::traits::StreamEvent::Final),
            ])
            .boxed()
        }
    }
}
impl ::zeroclaw_api::attribution::Attributable for StreamToolCaptureModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "StreamToolCaptureModelProvider"
    }
}

#[tokio::test]
async fn turn_streamed_passes_tool_specs_to_provider() {
    let tools_received = Arc::new(Mutex::new(Vec::new()));
    let model_provider = Box::new(StreamToolCaptureModelProvider {
        tools_received: tools_received.clone(),
        call_count: Arc::new(Mutex::new(0)),
    });

    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed with valid config");

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
    let (response, _) = agent
        .turn_streamed("use the echo tool", event_tx, None)
        .await
        .unwrap();
    assert_eq!(response, "stream-done");

    // Verify tools were passed in both stream_chat calls
    let received = tools_received.lock();
    assert!(
        received.len() >= 2,
        "Expected at least 2 stream_chat calls, got {}",
        received.len()
    );
    assert!(
        received[0],
        "First stream_chat call should have received tool specs"
    );
    assert!(
        received[1],
        "Second stream_chat call should have received tool specs"
    );

    // Collect events and verify tool call + tool result were emitted
    let mut events = Vec::new();
    while let Ok(ev) = event_rx.try_recv() {
        events.push(ev);
    }
    let has_tool_call = events
        .iter()
        .any(|e| matches!(e, TurnEvent::ToolCall { name, .. } if name == "echo"));
    let has_tool_result = events
        .iter()
        .any(|e| matches!(e, TurnEvent::ToolResult { name, .. } if name == "echo"));
    assert!(
        has_tool_call,
        "Should have emitted a ToolCall event for 'echo'"
    );
    assert!(
        has_tool_result,
        "Should have emitted a ToolResult event for 'echo'"
    );

    // Verify ID correlation
    let call_id = events
        .iter()
        .find_map(|e| {
            if let TurnEvent::ToolCall { id, .. } = e {
                Some(id.clone())
            } else {
                None
            }
        })
        .expect("ToolCall should have an ID");

    let result_id = events
        .iter()
        .find_map(|e| {
            if let TurnEvent::ToolResult { id, .. } = e {
                Some(id.clone())
            } else {
                None
            }
        })
        .expect("ToolResult should have an ID");

    assert_eq!(
        call_id, result_id,
        "ToolCall and ToolResult should share the same ID for correlation"
    );

    // Verify it's a valid UUID
    assert!(
        uuid::Uuid::parse_str(&call_id).is_ok(),
        "Generated ID should be a valid UUID: got '{}'",
        call_id
    );
}

fn tool_receipts_enabled_config(enabled: bool) -> zeroclaw_config::schema::AliasedAgentConfig {
    zeroclaw_config::schema::AliasedAgentConfig {
        resolved: zeroclaw_config::schema::ResolvedRuntime {
            tool_receipts: zeroclaw_config::schema::ToolReceiptsConfig {
                enabled,
                ..Default::default()
            },
            ..Default::default()
        },
        ..zeroclaw_config::schema::AliasedAgentConfig::default()
    }
}

fn streamed_agent_with_receipts(enabled: bool) -> Agent {
    let model_provider = Box::new(StreamToolCaptureModelProvider {
        tools_received: Arc::new(Mutex::new(Vec::new())),
        call_count: Arc::new(Mutex::new(0)),
    });
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );
    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .config(tool_receipts_enabled_config(enabled))
        .build()
        .expect("agent builder should succeed with valid config")
}

fn history_has_receipt(agent: &Agent) -> bool {
    agent.history().iter().any(|m| match m {
        ConversationMessage::ToolResults(results) => results
            .iter()
            .any(|r| r.content.contains("[receipt: zc-receipt-")),
        _ => false,
    })
}

// RED on upstream/master: the streamed turn path (ACP, gateway WS) hardcoded
// `receipt_generator: None`, so an enabled config produced zero receipts.
// GREEN once `turn_streamed` derives the scope from its own config through
// the shared `ReceiptScope::from_config` seam.
#[tokio::test]
async fn turn_streamed_signs_tool_results_when_receipts_enabled() {
    let mut agent = streamed_agent_with_receipts(true);
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
    agent
        .turn_streamed("use the echo tool", event_tx, None)
        .await
        .expect("streamed turn should succeed");
    assert!(
        history_has_receipt(&agent),
        "enabled receipts must sign tool results on the streamed path"
    );
}

// GREEN control: disabled config produces no receipts on the same path.
#[tokio::test]
async fn turn_streamed_omits_receipts_when_disabled() {
    let mut agent = streamed_agent_with_receipts(false);
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
    agent
        .turn_streamed("use the echo tool", event_tx, None)
        .await
        .expect("streamed turn should succeed");
    assert!(
        !history_has_receipt(&agent),
        "disabled receipts must not sign tool results"
    );
}

fn show_in_response_config(show: bool) -> zeroclaw_config::schema::AliasedAgentConfig {
    zeroclaw_config::schema::AliasedAgentConfig {
        resolved: zeroclaw_config::schema::ResolvedRuntime {
            tool_receipts: zeroclaw_config::schema::ToolReceiptsConfig {
                enabled: true,
                show_in_response: show,
                ..Default::default()
            },
            ..Default::default()
        },
        ..zeroclaw_config::schema::AliasedAgentConfig::default()
    }
}

fn streamed_agent_with_config(config: zeroclaw_config::schema::AliasedAgentConfig) -> Agent {
    let model_provider = Box::new(StreamToolCaptureModelProvider {
        tools_received: Arc::new(Mutex::new(Vec::new())),
        call_count: Arc::new(Mutex::new(0)),
    });
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );
    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .config(config)
        .build()
        .expect("agent builder should succeed with valid config")
}

// RED on the pre-fix branch: `show_in_response` was read only in the channel
// orchestrator, so ACP/WS/CLI turns never appended the auditable block.
// GREEN once the turn paths route the collector through
// `append_receipts_block`.
#[tokio::test]
async fn turn_streamed_appends_receipts_block_when_show_in_response() {
    let mut agent = streamed_agent_with_config(show_in_response_config(true));
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
    let (response, _msgs) = agent
        .turn_streamed("use the echo tool", event_tx, None)
        .await
        .expect("streamed turn should succeed");
    assert!(
        response.contains("---\nTool receipts:") && response.contains("zc-receipt-"),
        "show_in_response must append the Tool receipts block to the reply, got: {response}"
    );
}

// Control: with show_in_response off the reply carries no receipts block,
// even though receipts are still signed into history.
#[tokio::test]
async fn turn_streamed_omits_receipts_block_when_show_in_response_off() {
    let mut agent = streamed_agent_with_config(show_in_response_config(false));
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
    let (response, _msgs) = agent
        .turn_streamed("use the echo tool", event_tx, None)
        .await
        .expect("streamed turn should succeed");
    assert!(
        !response.contains("Tool receipts:"),
        "no receipts block when show_in_response is off, got: {response}"
    );
    assert!(
        history_has_receipt(&agent),
        "receipts are still signed into history when only the reply block is off"
    );
}

// The receipt-echo system-prompt addendum is added on the turn path when
// inject_system_prompt is on (default), matching the channel orchestrator.
#[test]
fn build_system_prompt_injects_receipt_addendum_when_enabled() {
    let agent = streamed_agent_with_config(show_in_response_config(true));
    let prompt = agent
        .build_system_prompt()
        .expect("system prompt should build");
    assert!(
        prompt.contains("## Tool Execution Receipts"),
        "enabled receipts with inject_system_prompt must add the addendum"
    );
}

/// then finishes. Used to verify serial dispatch ordering.
struct TwoToolCallStreamModelProvider {
    call_count: Arc<Mutex<usize>>,
}

#[async_trait]
impl ModelProvider for TwoToolCallStreamModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<String> {
        Ok("ok".into())
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<zeroclaw_providers::ChatResponse> {
        Ok(zeroclaw_providers::ChatResponse {
            text: Some("done".into()),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        })
    }

    fn supports_native_tools(&self) -> bool {
        true
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_streaming_tool_events(&self) -> bool {
        true
    }

    fn stream_chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
        _options: zeroclaw_providers::traits::StreamOptions,
    ) -> futures_util::stream::BoxStream<
        'static,
        zeroclaw_providers::traits::StreamResult<zeroclaw_providers::traits::StreamEvent>,
    > {
        use futures_util::stream::{self, StreamExt};
        let mut count = self.call_count.lock();
        *count += 1;
        if *count == 1 {
            stream::iter(vec![
                Ok(zeroclaw_providers::traits::StreamEvent::ToolCall(
                    zeroclaw_providers::ToolCall {
                        id: "00000000-0000-0000-0000-000000000001".into(),
                        name: "echo".into(),
                        arguments: "{}".into(),
                        extra_content: None,
                    },
                )),
                Ok(zeroclaw_providers::traits::StreamEvent::ToolCall(
                    zeroclaw_providers::ToolCall {
                        id: "00000000-0000-0000-0000-000000000002".into(),
                        name: "echo".into(),
                        arguments: "{}".into(),
                        extra_content: None,
                    },
                )),
                Ok(zeroclaw_providers::traits::StreamEvent::Final),
            ])
            .boxed()
        } else {
            stream::iter(vec![
                Ok(zeroclaw_providers::traits::StreamEvent::TextDelta(
                    zeroclaw_providers::traits::StreamChunk {
                        delta: "stream-done".into(),
                        is_final: false,
                        reasoning: None,
                        token_count: 0,
                    },
                )),
                Ok(zeroclaw_providers::traits::StreamEvent::Final),
            ])
            .boxed()
        }
    }
}
impl ::zeroclaw_api::attribution::Attributable for TwoToolCallStreamModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "TwoToolCallStreamModelProvider"
    }
}

#[tokio::test]
async fn turn_streamed_dispatches_multiple_tools_serially_when_parallel_disabled() {
    let model_provider = Box::new(TwoToolCallStreamModelProvider {
        call_count: Arc::new(Mutex::new(0)),
    });

    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed with valid config");

    // Default resolved config has parallel_tools = false; this is the
    // serial path under test.
    assert!(
        !agent.config.resolved.parallel_tools,
        "test precondition: parallel_tools must be disabled"
    );

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
    let (response, _) = agent
        .turn_streamed("use echo twice", event_tx, None)
        .await
        .unwrap();
    assert_eq!(response, "stream-done");

    // Reduce events to the call/result sequence, tagged by id.
    let mut seq: Vec<(&'static str, String)> = Vec::new();
    while let Ok(ev) = event_rx.try_recv() {
        match ev {
            TurnEvent::ToolCall { id, .. } => seq.push(("call", id)),
            TurnEvent::ToolResult { id, .. } => seq.push(("result", id)),
            _ => {}
        }
    }

    let id1 = "00000000-0000-0000-0000-000000000001";
    let id2 = "00000000-0000-0000-0000-000000000002";
    assert_eq!(
        seq,
        vec![
            ("call", id1.to_string()),
            ("result", id1.to_string()),
            ("call", id2.to_string()),
            ("result", id2.to_string()),
        ],
        "serial dispatch must interleave call->result per tool, not batch all \
             starts then all results; got {seq:?}"
    );
}

struct PreExecutedToolModelProvider;

#[async_trait]
impl ModelProvider for PreExecutedToolModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<String> {
        Ok(String::new())
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<zeroclaw_providers::ChatResponse> {
        Ok(zeroclaw_providers::ChatResponse {
            text: Some(String::new()),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        })
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn stream_chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
        _options: zeroclaw_providers::traits::StreamOptions,
    ) -> futures_util::stream::BoxStream<
        'static,
        zeroclaw_providers::traits::StreamResult<zeroclaw_providers::traits::StreamEvent>,
    > {
        use futures_util::stream::{self, StreamExt};

        stream::iter(vec![
            Ok(
                zeroclaw_providers::traits::StreamEvent::PreExecutedToolCall {
                    name: "file_read".into(),
                    args: "{\"path\":\"a.txt\",\"token\":\"abcdefgh123456\"}".into(),
                },
            ),
            Ok(
                zeroclaw_providers::traits::StreamEvent::PreExecutedToolCall {
                    name: "shell".into(),
                    args: "{\"command\":\"pwd\"}".into(),
                },
            ),
            Ok(
                zeroclaw_providers::traits::StreamEvent::PreExecutedToolResult {
                    name: "file_read".into(),
                    output: "read ok token=aaaaaaaaaaaa99".into(),
                },
            ),
            Ok(
                zeroclaw_providers::traits::StreamEvent::PreExecutedToolResult {
                    name: "shell".into(),
                    output: "b".into(),
                },
            ),
            Ok(zeroclaw_providers::traits::StreamEvent::Final),
        ])
        .boxed()
    }
}
impl ::zeroclaw_api::attribution::Attributable for PreExecutedToolModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "PreExecutedToolModelProvider"
    }
}

#[tokio::test]
async fn pre_executed_tool_results_keep_ids_when_calls_overlap() {
    let model_provider = Box::new(PreExecutedToolModelProvider);

    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed with valid config");

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
    let _ = agent
        .turn_streamed("use pre-executed tools", event_tx, None)
        .await
        .unwrap();

    let mut call_ids = HashMap::new();
    let mut result_ids = HashMap::new();
    let mut file_read_args: Option<serde_json::Value> = None;
    let mut file_read_output: Option<String> = None;
    let mut shell_output: Option<String> = None;
    while let Ok(event) = event_rx.try_recv() {
        match event {
            TurnEvent::ToolCall { id, name, args } => {
                if name == "file_read" {
                    file_read_args = Some(args);
                }
                call_ids.insert(name, id);
            }
            TurnEvent::ToolResult { id, name, output } => {
                match name.as_str() {
                    "file_read" => file_read_output = Some(output),
                    "shell" => shell_output = Some(output),
                    _ => {}
                }
                result_ids.insert(name, id);
            }
            _ => {}
        }
    }

    let rendered_args = file_read_args
        .expect("file_read ToolCall event must carry args")
        .to_string();
    assert!(
        !rendered_args.contains("abcdefgh123456"),
        "pre-executed tool-call events must scrub credentials: {rendered_args}"
    );
    assert!(
        rendered_args.contains("[REDACTED]"),
        "pre-executed tool-call events must show the full mask: {rendered_args}"
    );
    assert!(
        rendered_args.contains("a.txt"),
        "non-sensitive sibling args must survive scrubbing: {rendered_args}"
    );
    let rendered_output = file_read_output.expect("file_read ToolResult event");
    assert!(
        !rendered_output.contains("aaaaaaaaaaaa99"),
        "pre-executed tool-result events must scrub credentials: {rendered_output}"
    );
    assert!(
        rendered_output.contains("[REDACTED]"),
        "pre-executed tool-result events must show the full mask: {rendered_output}"
    );
    assert!(
        rendered_output.contains("read ok"),
        "non-sensitive output must survive scrubbing: {rendered_output}"
    );
    assert_eq!(
        shell_output.as_deref(),
        Some("b"),
        "plain pre-executed output passes through unchanged"
    );
    assert_eq!(call_ids.len(), 2, "expected two pre-executed tool calls");
    assert_eq!(
        result_ids.len(),
        2,
        "expected two pre-executed tool results"
    );
    assert_eq!(call_ids.get("file_read"), result_ids.get("file_read"));
    assert_eq!(call_ids.get("shell"), result_ids.get("shell"));
}

#[tokio::test]
async fn turn_normalizes_user_image_markers_before_provider_call() {
    let seen_user_messages = Arc::new(Mutex::new(Vec::new()));
    let provider = Box::new(MultimodalCaptureProvider {
        seen_user_messages: seen_user_messages.clone(),
        streamed: false,
        fail_chat: false,
    });

    let temp = tempfile::tempdir().expect("tempdir");
    let image_path = temp.path().join("agent-turn.png");
    std::fs::write(
        &image_path,
        [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'],
    )
    .expect("write fixture");

    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = Agent::builder()
        .model_provider(provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .multimodal_config(zeroclaw_config::schema::MultimodalConfig::default())
        .build()
        .expect("agent builder should succeed with valid config");

    agent
        .turn(&format!(
            "inspect [IMAGE:{}]",
            image_path.display().to_string()
        ))
        .await
        .expect("turn should succeed");

    let seen = seen_user_messages.lock();
    let last = seen.last().expect("provider should receive a user message");
    assert!(
        last.contains("data:image/png;base64,"),
        "expected normalized data URI in provider request, got: {last}"
    );
}

#[tokio::test]
async fn turn_streamed_normalizes_user_image_markers_before_provider_call() {
    let seen_user_messages = Arc::new(Mutex::new(Vec::new()));
    let provider = Box::new(MultimodalCaptureProvider {
        seen_user_messages: seen_user_messages.clone(),
        streamed: true,
        fail_chat: false,
    });

    let temp = tempfile::tempdir().expect("tempdir");
    let image_path = temp.path().join("agent-stream.png");
    std::fs::write(
        &image_path,
        [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'],
    )
    .expect("write fixture");

    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = Agent::builder()
        .model_provider(provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .multimodal_config(zeroclaw_config::schema::MultimodalConfig::default())
        .build()
        .expect("agent builder should succeed with valid config");

    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(8);
    agent
        .turn_streamed(
            &format!("inspect [IMAGE:{}]", image_path.display().to_string()),
            event_tx,
            None,
        )
        .await
        .expect("turn_streamed should succeed");

    let seen = seen_user_messages.lock();
    let last = seen.last().expect("provider should receive a user message");
    assert!(
        last.contains("data:image/png;base64,"),
        "expected normalized data URI in provider request, got: {last}"
    );
}

fn trim_history_test_agent(max_history_messages: usize, observer: Arc<dyn Observer>) -> Agent {
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );
    let agent_config = zeroclaw_config::schema::AliasedAgentConfig {
        resolved: zeroclaw_config::schema::ResolvedRuntime::default(),
        ..zeroclaw_config::schema::AliasedAgentConfig::default()
    };

    Agent::builder()
        .model_provider(Box::new(MockModelProvider {
            responses: Mutex::new(vec![]),
        }))
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .config(agent_config)
        .structured_max_history_messages(max_history_messages)
        .build()
        .expect("agent builder should succeed with valid config")
}

fn seed_old_trim_test_turn(agent: &mut Agent) {
    agent.history = vec![
        ConversationMessage::Chat(ChatMessage::system("system")),
        ConversationMessage::Chat(ChatMessage::user("old user")),
        ConversationMessage::Chat(ChatMessage::assistant("old assistant")),
    ];
}

fn assert_old_trim_test_turn_was_removed(agent: &Agent) {
    assert!(agent.history_has_trim_breadcrumb);
    assert!(!agent.history.iter().any(|message| matches!(
        message,
        ConversationMessage::Chat(chat)
            if chat.content == "old user" || chat.content == "old assistant"
    )));
}

fn drain_history_trim_events(event_rx: &mut tokio::sync::mpsc::Receiver<TurnEvent>) -> usize {
    let mut count = 0;
    while let Ok(event) = event_rx.try_recv() {
        if matches!(event, TurnEvent::HistoryTrimmed { .. }) {
            count += 1;
        }
    }
    count
}

fn push_trim_history_tool_exchange(agent: &mut Agent, index: usize) {
    use zeroclaw_providers::{ToolCall, ToolResultMessage};

    let tool_call_id = format!("trim-history-call-{index}");
    agent.history.push(ConversationMessage::AssistantToolCalls {
        text: Some(format!("Calling tool {index}")),
        tool_calls: vec![ToolCall {
            id: tool_call_id.clone(),
            name: "mock".into(),
            arguments: "{}".into(),
            extra_content: None,
        }],
        reasoning_content: None,
    });
    agent
        .history
        .push(ConversationMessage::ToolResults(vec![ToolResultMessage {
            tool_call_id,
            content: format!("result {index}"),
            tool_name: "mock".into(),
        }]));
}

#[test]
fn trim_history_preserves_single_tool_heavy_turn_over_message_cap() {
    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = trim_history_test_agent(50, observer);
    agent
        .history
        .push(ConversationMessage::Chat(ChatMessage::user("start")));
    for index in 1..=31 {
        push_trim_history_tool_exchange(&mut agent, index);
    }
    agent
        .history
        .push(ConversationMessage::Chat(ChatMessage::assistant("done")));

    let _ = agent.trim_history(None);

    assert_eq!(
        agent.history.len(),
        64,
        "the newest complete turn must survive even when it exceeds the message cap"
    );
    assert!(matches!(
        agent.history.first(),
        Some(ConversationMessage::Chat(message))
            if message.role == "user" && message.content == "start"
    ));
    assert!(matches!(
        agent.history.last(),
        Some(ConversationMessage::Chat(message))
            if message.role == "assistant" && message.content == "done"
    ));
    for (index, pair) in agent.history[1..63].chunks_exact(2).enumerate() {
        let expected_id = format!("trim-history-call-{}", index + 1);
        match pair {
            [
                ConversationMessage::AssistantToolCalls { tool_calls, .. },
                ConversationMessage::ToolResults(results),
            ] => {
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(results.len(), 1);
                assert_eq!(tool_calls[0].id, expected_id);
                assert_eq!(results[0].tool_call_id, expected_id);
            }
            _ => panic!("tool exchange {} was split or reordered", index + 1),
        }
    }
}

#[test]
fn trim_history_drops_old_turn_with_breadcrumb_and_observer_event() {
    let capturing = Arc::new(CapturingObserver::default());
    let observer: Arc<dyn Observer> = capturing.clone();
    let mut agent = trim_history_test_agent(2, observer);
    agent.history = vec![
        ConversationMessage::Chat(ChatMessage::system("system")),
        ConversationMessage::Chat(ChatMessage::user("old user")),
        ConversationMessage::Chat(ChatMessage::assistant("old assistant")),
        ConversationMessage::Chat(ChatMessage::user("new user")),
        ConversationMessage::Chat(ChatMessage::assistant("new assistant")),
    ];

    let _ = agent.trim_history(None);

    let breadcrumb = crate::i18n::get_required_cli_string("history-trim-breadcrumb");
    assert!(matches!(
        agent.history.first(),
        Some(ConversationMessage::Chat(message))
            if message.role == "system"
    ));
    assert!(matches!(
        agent.history.get(1),
        Some(ConversationMessage::Chat(message))
            if message.role == "user" && message.content == breadcrumb
    ));
    assert_eq!(
        agent
            .history
            .iter()
            .filter(|message| matches!(
                message,
                ConversationMessage::Chat(chat) if chat.content == breadcrumb
            ))
            .count(),
        1,
        "trim breadcrumb must be inserted exactly once"
    );
    assert!(matches!(
        agent.history.get(2),
        Some(ConversationMessage::Chat(message))
            if message.role == "user" && message.content == "new user"
    ));
    assert!(matches!(
        agent.history.get(3),
        Some(ConversationMessage::Chat(message))
            if message.role == "assistant" && message.content == "new assistant"
    ));
    assert_eq!(
        agent.history.len(),
        4,
        "only the complete newest turn remains"
    );

    let trim_events: Vec<_> = capturing
        .events
        .lock()
        .iter()
        .filter_map(|event| match event {
            ObserverEvent::HistoryTrimmed {
                dropped_messages,
                kept_turns,
                reason,
                ..
            } => Some((*dropped_messages, *kept_turns, reason.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(trim_events.len(), 1, "one observer trim event is required");
    assert_eq!(trim_events[0].0, 2);
    assert_eq!(trim_events[0].1, 1);
    assert_eq!(
        trim_events[0].2,
        crate::i18n::get_required_cli_string("history-trim-reason-message-cap")
    );
}

#[tokio::test]
async fn trim_history_runs_after_direct_tool_loop_provider_error() {
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );
    let capturing = Arc::new(CapturingObserver::default());
    let observer: Arc<dyn Observer> = capturing.clone();
    let config = zeroclaw_config::schema::AliasedAgentConfig {
        resolved: zeroclaw_config::schema::ResolvedRuntime::default(),
        ..Default::default()
    };
    let mut agent = Agent::builder()
        .model_provider(Box::new(ToolThenFailingModelProvider {
            calls: std::sync::atomic::AtomicUsize::new(0),
        }))
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .model_name("test-model".into())
        .config(config)
        .structured_max_history_messages(2)
        .build()
        .expect("agent builder should succeed with valid config");
    agent.history = vec![
        ConversationMessage::Chat(ChatMessage::system("system")),
        ConversationMessage::Chat(ChatMessage::user("old request")),
        ConversationMessage::Chat(ChatMessage::assistant("old answer")),
    ];

    let error = agent
        .turn("new request")
        .await
        .expect_err("second provider call should fail");

    assert!(
        error
            .to_string()
            .contains("provider unavailable after tool")
    );
    assert!(agent.history_has_trim_breadcrumb);
    assert!(!agent.history.iter().any(|message| matches!(
        message,
        ConversationMessage::Chat(chat)
            if chat.content == "old request" || chat.content == "old answer"
    )));
    assert!(agent.history.iter().any(|message| matches!(
        message,
        ConversationMessage::Chat(chat)
            if chat.role == "user" && chat.content.contains("new request")
    )));
    assert!(agent.history.windows(2).any(|pair| matches!(
        pair,
        [
            ConversationMessage::AssistantToolCalls { tool_calls, .. },
            ConversationMessage::ToolResults(results),
        ] if tool_calls[0].id == "error-path-call"
            && results[0].tool_call_id == "error-path-call"
    )));
    assert_eq!(
        capturing
            .events
            .lock()
            .iter()
            .filter(|event| matches!(event, ObserverEvent::HistoryTrimmed { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn trim_history_runs_after_direct_vision_resolution_error() {
    let capturing = Arc::new(CapturingObserver::default());
    let observer: Arc<dyn Observer> = capturing.clone();
    let mut agent = trim_history_test_agent(2, observer);
    seed_old_trim_test_turn(&mut agent);

    let error = agent
        .turn("inspect [IMAGE:data:image/png;base64,iVBORw0KGgo=]")
        .await
        .expect_err("missing vision support should fail before provider dispatch");

    assert!(error.to_string().contains("does not support vision input"));
    assert_old_trim_test_turn_was_removed(&agent);
    assert_eq!(
        capturing
            .events
            .lock()
            .iter()
            .filter(|event| matches!(event, ObserverEvent::HistoryTrimmed { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn trim_history_runs_after_streamed_vision_resolution_error() {
    let capturing = Arc::new(CapturingObserver::default());
    let observer: Arc<dyn Observer> = capturing.clone();
    let mut agent = trim_history_test_agent(2, observer);
    seed_old_trim_test_turn(&mut agent);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(8);

    let error = agent
        .turn_streamed(
            "inspect [IMAGE:data:image/png;base64,iVBORw0KGgo=]",
            event_tx,
            None,
        )
        .await
        .expect_err("missing vision support should fail before provider dispatch");

    assert!(error.to_string().contains("does not support vision input"));
    assert_old_trim_test_turn_was_removed(&agent);
    assert_eq!(drain_history_trim_events(&mut event_rx), 1);
}

#[tokio::test]
async fn trim_history_runs_after_direct_system_prompt_rebuild_error() {
    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = trim_history_test_agent(2, observer);
    seed_old_trim_test_turn(&mut agent);
    agent.prompt_builder =
        SystemPromptBuilder::default().add_section(Box::new(FailingPromptSection));

    let error = agent
        .turn("new user")
        .await
        .expect_err("synthetic prompt rebuild should fail");

    assert!(
        error
            .to_string()
            .contains("synthetic prompt rebuild failure")
    );
    assert_old_trim_test_turn_was_removed(&agent);
}

#[tokio::test]
async fn trim_history_runs_after_streamed_system_prompt_rebuild_error() {
    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = trim_history_test_agent(2, observer);
    seed_old_trim_test_turn(&mut agent);
    agent.prompt_builder =
        SystemPromptBuilder::default().add_section(Box::new(FailingPromptSection));
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(8);

    let error = agent
        .turn_streamed("new user", event_tx, None)
        .await
        .expect_err("synthetic prompt rebuild should fail");

    assert!(
        error
            .to_string()
            .contains("synthetic prompt rebuild failure")
    );
    assert_old_trim_test_turn_was_removed(&agent);
    assert_eq!(drain_history_trim_events(&mut event_rx), 1);
}

#[tokio::test]
async fn trim_history_runs_before_streamed_round_loop_exhaustion_error() {
    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = trim_history_test_agent(2, observer);
    agent.config.resolved.max_tool_iterations = 0;
    seed_old_trim_test_turn(&mut agent);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(8);

    let error = agent
        .turn_streamed("new user", event_tx, None)
        .await
        .expect_err("zero rounds should return the exhaustion error");

    assert!(
        error
            .to_string()
            .contains("exceeded maximum tool iterations (0)")
    );
    assert_old_trim_test_turn_was_removed(&agent);
    assert_eq!(drain_history_trim_events(&mut event_rx), 1);
}

#[test]
fn trim_history_log_uses_canonical_attribution() {
    let _writer_guard = zeroclaw_log::__private_test_writer_lock();
    let _hook_guard = zeroclaw_log::__private_test_hook_lock();
    zeroclaw_log::try_install_capture_subscriber();
    let mut log_rx = zeroclaw_log::subscribe_or_install();
    while log_rx.try_recv().is_ok() {}

    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = trim_history_test_agent(2, observer);
    agent.agent_alias = "trim-test-agent".into();
    agent.channel_name = "trim-test-channel".into();
    agent.history = vec![
        ConversationMessage::Chat(ChatMessage::system("system")),
        ConversationMessage::Chat(ChatMessage::user("old user")),
        ConversationMessage::Chat(ChatMessage::assistant("old assistant")),
        ConversationMessage::Chat(ChatMessage::user("new user")),
        ConversationMessage::Chat(ChatMessage::assistant("new assistant")),
    ];

    let _ = agent.trim_history(Some("trim-test-turn"));

    let mut selected = None;
    let mut candidates = Vec::new();
    loop {
        match log_rx.try_recv() {
            Ok(value)
                if value.get("message").and_then(serde_json::Value::as_str)
                    == Some("trim_history: dropped oldest whole turns") =>
            {
                if value.get("trace_id").and_then(serde_json::Value::as_str)
                    == Some("trim-test-turn")
                {
                    selected = Some(value.clone());
                }
                candidates.push(value);
            }
            Ok(_) | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
        }
    }
    let value = selected.unwrap_or_else(|| {
            panic!(
                "trim LogEvent with trace_id=trim-test-turn was not captured; candidates: {candidates:#?}"
            )
        });
    let event: zeroclaw_log::LogEvent =
        serde_json::from_value(value).expect("captured trim event should deserialize");

    assert_eq!(event.zeroclaw.get("agent_alias"), Some("trim-test-agent"));
    assert_eq!(
        event.zeroclaw.get("channel_type"),
        Some("trim-test-channel")
    );
    assert_eq!(event.zeroclaw.get("channel"), None);
    assert_eq!(event.trace_id.as_deref(), Some("trim-test-turn"));
    assert!(event.attributes.get("agent_alias").is_none());
    assert!(event.attributes.get("channel").is_none());
    assert!(event.attributes.get("turn_id").is_none());

    zeroclaw_log::clear_broadcast_hook();
}

#[tokio::test]
async fn trim_history_streamed_turn_forwards_single_hard_cap_event() {
    let capturing = Arc::new(CapturingObserver::default());
    let observer: Arc<dyn Observer> = capturing.clone();
    let mut agent = trim_history_test_agent(2, observer);
    agent.history = vec![
        ConversationMessage::Chat(ChatMessage::system("system")),
        ConversationMessage::Chat(ChatMessage::user("old user")),
        ConversationMessage::Chat(ChatMessage::assistant("old assistant")),
    ];
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(16);

    agent
        .turn_streamed("new user", event_tx, None)
        .await
        .expect("streamed turn should succeed");

    let mut trim_events = Vec::new();
    while let Ok(event) = event_rx.try_recv() {
        if let TurnEvent::HistoryTrimmed {
            dropped_messages,
            kept_turns,
            reason,
        } = event
        {
            trim_events.push((dropped_messages, kept_turns, reason));
        }
    }
    assert_eq!(trim_events.len(), 1, "one streamed trim event is required");
    assert_eq!(trim_events[0].0, 2);
    assert_eq!(trim_events[0].1, 1);
    assert_eq!(
        trim_events[0].2,
        crate::i18n::get_required_cli_string("history-trim-reason-message-cap")
    );
    assert!(capturing.events.lock().iter().any(|event| matches!(
        event,
        ObserverEvent::HistoryTrimmed {
            turn_id: Some(_),
            ..
        }
    )));
}

#[tokio::test]
async fn trim_history_cancel_before_output_retains_synthesized_newest_turn() {
    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = trim_history_test_agent(2, observer);
    agent.history = vec![
        ConversationMessage::Chat(ChatMessage::system("system")),
        ConversationMessage::Chat(ChatMessage::user("old user")),
        ConversationMessage::Chat(ChatMessage::assistant("old assistant")),
    ];
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(16);
    let cancel_token = tokio_util::sync::CancellationToken::new();
    cancel_token.cancel();

    let error = agent
        .turn_streamed_with_steering_state("new user", event_tx, Some(cancel_token), None)
        .await
        .expect_err("pre-cancelled streamed turn should return cancellation");

    let breadcrumb = crate::i18n::get_required_cli_string("history-trim-breadcrumb");
    let interruption = crate::i18n::get_required_cli_string("turn-interrupted-by-user");
    assert!(crate::agent::loop_::is_tool_loop_cancelled(&error.error));
    assert_eq!(error.committed_response, interruption);
    assert_eq!(agent.history.len(), 4);
    assert!(matches!(
        agent.history.first(),
        Some(ConversationMessage::Chat(message))
            if message.role == "system"
    ));
    assert!(matches!(
        agent.history.get(1),
        Some(ConversationMessage::Chat(message))
            if message.role == "user" && message.content == breadcrumb
    ));
    assert!(matches!(
        agent.history.get(2),
        Some(ConversationMessage::Chat(message))
            if message.role == "user" && message.content.contains("new user")
    ));
    assert!(matches!(
        agent.history.last(),
        Some(ConversationMessage::Chat(message))
            if message.role == "assistant" && message.content == interruption
    ));
    assert!(!agent.history.iter().any(|message| matches!(
        message,
        ConversationMessage::Chat(chat)
            if chat.content == "old user" || chat.content == "old assistant"
    )));

    let mut trim_events = Vec::new();
    while let Ok(event) = event_rx.try_recv() {
        if let TurnEvent::HistoryTrimmed {
            dropped_messages,
            kept_turns,
            ..
        } = event
        {
            trim_events.push((dropped_messages, kept_turns));
        }
    }
    assert_eq!(trim_events, vec![(2, 1)]);
}

// ── Duplicate narration guard ────────────────────────────────────

#[tokio::test]
async fn narration_with_tool_calls_produces_no_consecutive_assistant_entries() {
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let model_provider = Box::new(MockModelProvider {
        responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
            text: Some("I will echo the message.".into()),
            tool_calls: vec![zeroclaw_providers::ToolCall {
                id: "tc1".into(),
                name: "echo".into(),
                arguments: "{}".into(),
                extra_content: None,
            }],
            usage: None,
            reasoning_content: None,
        }]),
    });

    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed with valid config");

    agent.turn("hi").await.unwrap();

    let history = agent.history();
    for window in history.windows(2) {
        let prev_is_assistant_chat = matches!(
            &window[0],
            ConversationMessage::Chat(m) if m.role == "assistant"
        );
        let next_is_tool_calls =
            matches!(&window[1], ConversationMessage::AssistantToolCalls { .. });
        assert!(
            !(prev_is_assistant_chat && next_is_tool_calls),
            "history contains Chat(assistant) immediately before AssistantToolCalls — \
                 duplicate narration push was not removed"
        );
    }
}

/// Streaming mock that emits narration text + tool call on the first turn,
/// then a plain text response on the second. Used to verify the streaming
/// path has the same duplicate-narration guard as the blocking path.
struct NarrationStreamModelProvider {
    call_count: Arc<Mutex<usize>>,
}

#[async_trait]
impl ModelProvider for NarrationStreamModelProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<String> {
        Ok("ok".into())
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<zeroclaw_providers::ChatResponse> {
        Ok(zeroclaw_providers::ChatResponse {
            text: Some("done".into()),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        })
    }

    fn supports_native_tools(&self) -> bool {
        true
    }

    fn stream_chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
        _options: zeroclaw_providers::traits::StreamOptions,
    ) -> futures_util::stream::BoxStream<
        'static,
        zeroclaw_providers::traits::StreamResult<zeroclaw_providers::traits::StreamEvent>,
    > {
        use futures_util::stream::{self, StreamExt};
        let mut count = self.call_count.lock();
        *count += 1;
        if *count == 1 {
            stream::iter(vec![
                Ok(zeroclaw_providers::traits::StreamEvent::TextDelta(
                    zeroclaw_providers::traits::StreamChunk {
                        delta: "I will echo the message.".into(),
                        is_final: false,
                        reasoning: None,
                        token_count: 0,
                    },
                )),
                Ok(zeroclaw_providers::traits::StreamEvent::ToolCall(
                    zeroclaw_providers::ToolCall {
                        id: "tc1".into(),
                        name: "echo".into(),
                        arguments: "{}".into(),
                        extra_content: None,
                    },
                )),
                Ok(zeroclaw_providers::traits::StreamEvent::Final),
            ])
            .boxed()
        } else {
            stream::iter(vec![
                Ok(zeroclaw_providers::traits::StreamEvent::TextDelta(
                    zeroclaw_providers::traits::StreamChunk {
                        delta: "done".into(),
                        is_final: false,
                        reasoning: None,
                        token_count: 0,
                    },
                )),
                Ok(zeroclaw_providers::traits::StreamEvent::Final),
            ])
            .boxed()
        }
    }
}
impl ::zeroclaw_api::attribution::Attributable for NarrationStreamModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "NarrationStreamModelProvider"
    }
}

#[tokio::test]
async fn streaming_narration_with_tool_calls_produces_no_consecutive_assistant_entries() {
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let model_provider = Box::new(NarrationStreamModelProvider {
        call_count: Arc::new(Mutex::new(0)),
    });

    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed with valid config");

    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
    agent.turn_streamed("hi", event_tx, None).await.unwrap();

    let history = agent.history();
    for window in history.windows(2) {
        let prev_is_assistant_chat = matches!(
            &window[0],
            ConversationMessage::Chat(m) if m.role == "assistant"
        );
        let next_is_tool_calls =
            matches!(&window[1], ConversationMessage::AssistantToolCalls { .. });
        assert!(
            !(prev_is_assistant_chat && next_is_tool_calls),
            "streaming path: history contains Chat(assistant) immediately before \
                 AssistantToolCalls — duplicate narration push was not removed"
        );
    }
}

#[tokio::test]
async fn response_cache_key_uses_full_provider_visible_transcript() {
    let tmp = tempfile::tempdir().expect("temp response cache dir");
    let cache = Arc::new(
        zeroclaw_memory::response_cache::ResponseCache::new(tmp.path(), 60, 100)
            .expect("response cache should initialize"),
    );

    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem_a: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );
    let mem_b: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let seen_a = Arc::new(Mutex::new(Vec::new()));
    let seen_b = Arc::new(Mutex::new(Vec::new()));
    let provider_a = Box::new(TranscriptCaptureModelProvider {
        responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
            text: Some("from prior transcript".into()),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        }]),
        seen_messages: seen_a.clone(),
    });
    let provider_b = Box::new(TranscriptCaptureModelProvider {
        responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
            text: Some("from fresh transcript".into()),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        }]),
        seen_messages: seen_b.clone(),
    });

    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent_a = Agent::builder()
        .model_provider(provider_a)
        .tools(vec![Box::new(MockTool)])
        .memory(mem_a)
        .observer(observer.clone())
        .response_cache(Some(cache.clone()))
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .model_name("test-model".into())
        .temperature(Some(0.0))
        .build()
        .expect("agent builder should succeed with valid config");
    agent_a.seed_history(&[
        ChatMessage::user("earlier turn"),
        ChatMessage::assistant("earlier answer"),
    ]);

    let mut agent_b = Agent::builder()
        .model_provider(provider_b)
        .tools(vec![Box::new(MockTool)])
        .memory(mem_b)
        .observer(observer)
        .response_cache(Some(cache))
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .model_name("test-model".into())
        .temperature(Some(0.0))
        .build()
        .expect("agent builder should succeed with valid config");

    assert_eq!(
        agent_a.turn("same final prompt").await.unwrap(),
        "from prior transcript"
    );
    assert_eq!(
        agent_b.turn("same final prompt").await.unwrap(),
        "from fresh transcript"
    );
    assert_eq!(seen_a.lock().len(), 1);
    assert_eq!(
        seen_b.lock().len(),
        1,
        "fresh transcript must not reuse a cache entry written for a different prior transcript"
    );
}

#[tokio::test]
async fn response_cache_does_not_cross_serve_memory_conditioned_answers() {
    // A backend whose recall always returns one Core entry with the given
    // content, so injection yields a deterministic, agent-specific preamble.
    // name() != "none" marks it a real, injecting backend for the gate.
    struct FixtureRecallMemory {
        content: String,
    }
    #[async_trait]
    impl Memory for FixtureRecallMemory {
        fn name(&self) -> &str {
            "fixture"
        }
        async fn store(
            &self,
            _: &str,
            _: &str,
            _: MemoryCategory,
            _: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn recall(
            &self,
            _: &str,
            _: usize,
            _: Option<&str>,
            _: Option<&str>,
            _: Option<&str>,
        ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
            Ok(vec![zeroclaw_memory::MemoryEntry {
                id: "deploy".into(),
                key: "deploy".into(),
                content: self.content.clone(),
                category: MemoryCategory::Core,
                timestamp: chrono::Utc::now().to_rfc3339(),
                session_id: None,
                score: None,
                namespace: "default".into(),
                importance: None,
                superseded_by: None,
                kind: None,
                pinned: false,
                tenant_id: None,
                agent_alias: None,
                agent_id: None,
            }])
        }
        async fn get(&self, _: &str) -> anyhow::Result<Option<zeroclaw_memory::MemoryEntry>> {
            Ok(None)
        }
        async fn list(
            &self,
            _: Option<&MemoryCategory>,
            _: Option<&str>,
        ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
            Ok(vec![])
        }
        async fn forget(&self, _: &str) -> anyhow::Result<bool> {
            Ok(true)
        }
        async fn forget_for_agent(&self, _: &str, _: &str) -> anyhow::Result<bool> {
            Ok(true)
        }
        async fn count(&self) -> anyhow::Result<usize> {
            Ok(1)
        }
        async fn health_check(&self) -> bool {
            true
        }
        async fn store_with_agent(
            &self,
            _: &str,
            _: &str,
            _: MemoryCategory,
            _: Option<&str>,
            _: Option<&str>,
            _: Option<f64>,
            _: Option<&str>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn recall_for_agents(
            &self,
            _: &[&str],
            query: &str,
            limit: usize,
            session_id: Option<&str>,
            since: Option<&str>,
            until: Option<&str>,
        ) -> anyhow::Result<Vec<zeroclaw_memory::MemoryEntry>> {
            self.recall(query, limit, session_id, since, until).await
        }
    }
    impl ::zeroclaw_api::attribution::Attributable for FixtureRecallMemory {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Memory(
                ::zeroclaw_api::attribution::MemoryKind::InMemory,
            )
        }
        fn alias(&self) -> &str {
            "FixtureRecallMemory"
        }
    }

    // Frozen clock so both turns share a byte-identical bare transcript (the
    // per-turn `[CURRENT DATE & TIME]` prefix is otherwise second-precision),
    // which is what makes the two pre-injection cache keys collide.
    let fixed = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00+00:00")
        .unwrap()
        .with_timezone(&chrono::Local);
    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});

    let last_user = |seen: &Arc<Mutex<Vec<Vec<ChatMessage>>>>| -> String {
        seen.lock()
            .last()
            .expect("a model call was captured")
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .expect("a user message")
            .content
            .clone()
    };

    let build = |mem: Arc<dyn Memory>,
                 seen: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
                 cache: Arc<zeroclaw_memory::response_cache::ResponseCache>| {
        let provider = Box::new(TranscriptCaptureModelProvider {
            responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
                text: Some("answer".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            }]),
            seen_messages: seen,
        });
        Agent::builder()
            .model_provider(provider)
            .tools(vec![])
            .memory(mem)
            .observer(observer.clone())
            .response_cache(Some(cache))
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .model_name("test-model".into())
            .temperature(Some(0.0))
            .turn_datetime(move || fixed)
            .build()
            .expect("agent builder should succeed")
    };

    const PROMPT: &str = "what is the deploy target";

    // Harm case: same prompt, DIFFERENT recalled memory, one shared cache.
    let harm_dir = tempfile::tempdir().expect("cache dir");
    let harm_cache = Arc::new(
        zeroclaw_memory::response_cache::ResponseCache::new(harm_dir.path(), 60, 100)
            .expect("response cache"),
    );
    let seen_a = Arc::new(Mutex::new(Vec::new()));
    let seen_b = Arc::new(Mutex::new(Vec::new()));
    let mut agent_a = build(
        Arc::new(FixtureRecallMemory {
            content: "the deploy target is prod-3-alpha".into(),
        }),
        seen_a.clone(),
        harm_cache.clone(),
    );
    let mut agent_b = build(
        Arc::new(FixtureRecallMemory {
            content: "the deploy target is prod-9-beta".into(),
        }),
        seen_b.clone(),
        harm_cache.clone(),
    );
    agent_a.turn(PROMPT).await.expect("turn a");
    agent_b.turn(PROMPT).await.expect("turn b");

    assert_eq!(seen_a.lock().len(), 1, "agent A always runs the model");
    assert!(
        last_user(&seen_a).contains("prod-3-alpha"),
        "agent A's model call must see A's injected memory"
    );
    // Pre-fix, B's key equals A's (both pre-injection) so B is served A's
    // prod-3 answer and never runs against its own prod-9 memory.
    assert_eq!(
        seen_b.lock().len(),
        1,
        "agent B must run the model, not reuse A's cache entry keyed on the shared pre-injection transcript"
    );
    assert!(
        last_user(&seen_b).contains("prod-9-beta"),
        "agent B's model call must see B's OWN injected memory, not A's"
    );

    // Control: `none` backend injects nothing, so the two transcripts really
    // are identical and the shared cache DOES hit: the second agent is
    // served from cache and never reaches the model. This proves the harm
    // case is not passing merely because the cache never works.
    let ctrl_dir = tempfile::tempdir().expect("cache dir");
    let ctrl_cache = Arc::new(
        zeroclaw_memory::response_cache::ResponseCache::new(ctrl_dir.path(), 60, 100)
            .expect("response cache"),
    );
    let none_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let none_mem = || -> Arc<dyn Memory> {
        Arc::from(
            zeroclaw_memory::create_memory(&none_cfg, std::path::Path::new("/tmp"), None)
                .expect("none memory"),
        )
    };
    let seen_c = Arc::new(Mutex::new(Vec::new()));
    let seen_d = Arc::new(Mutex::new(Vec::new()));
    let mut agent_c = build(none_mem(), seen_c.clone(), ctrl_cache.clone());
    let mut agent_d = build(none_mem(), seen_d.clone(), ctrl_cache.clone());
    agent_c.turn(PROMPT).await.expect("turn c");
    agent_d.turn(PROMPT).await.expect("turn d");
    assert_eq!(seen_c.lock().len(), 1, "agent C always runs the model");
    assert_eq!(
        seen_d.lock().len(),
        0,
        "control: with no injection the identical prompt is served from the shared response cache"
    );
}

#[test]
fn response_cache_key_skips_multimodal_image_markers() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let cache = Arc::new(
        zeroclaw_memory::response_cache::ResponseCache::new(tmp.path(), 60, 100)
            .expect("response cache init"),
    );

    let agent = Agent::builder()
        .model_provider(Box::new(MockModelProvider {
            responses: Mutex::new(vec![]),
        }))
        .tools(vec![Box::new(MockTool)])
        .memory(Arc::from(
            zeroclaw_memory::create_memory(
                &zeroclaw_config::schema::MemoryConfig {
                    backend: "none".into(),
                    ..zeroclaw_config::schema::MemoryConfig::default()
                },
                std::path::Path::new("/tmp"),
                None,
            )
            .expect("memory"),
        ))
        .observer(Arc::from(crate::observability::NoopObserver {}))
        .response_cache(Some(cache))
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .model_name("test-model".into())
        .temperature(Some(0.0))
        .build()
        .expect("agent builder");

    // Plain text messages should produce a cache key.
    let plain_messages = vec![
        ChatMessage::system("system prompt"),
        ChatMessage::user("hello"),
    ];
    let key = agent.response_cache_key_for_messages(&plain_messages, "test-model");
    assert!(key.is_some(), "plain text prompt must produce a cache key");

    // Messages containing `[IMAGE:]` must return None (skip cache).
    let multimodal_messages = vec![
        ChatMessage::system("system prompt"),
        ChatMessage::user("describe this image [IMAGE:/tmp/photo.png]"),
    ];
    let key = agent.response_cache_key_for_messages(&multimodal_messages, "test-model");
    assert!(
        key.is_none(),
        "multimodal prompt with [IMAGE:] marker must skip response cache"
    );
}

#[tokio::test]
async fn turn_streamed_with_steering_commits_streamed_output_before_continuing() {
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let seen_messages = Arc::new(Mutex::new(Vec::new()));
    let model_provider = Box::new(StreamingSteeringModelProvider {
        seen_messages: seen_messages.clone(),
        call_count: AtomicUsize::new(0),
        fail_on_call: None,
        fail_chat_on_call: None,
        fail_after_delta_on_call: None,
        delay_chat_on_call: None,
    });
    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed with valid config");

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
    let (steering_tx, mut steering_rx) = tokio::sync::mpsc::channel::<String>(4);
    let handle = zeroclaw_spawn::spawn!(async move {
        agent
            .turn_streamed_with_steering_state("first", event_tx, None, Some(&mut steering_rx))
            .await
    });

    loop {
        match event_rx.recv().await.expect("turn event should arrive") {
            TurnEvent::Chunk { delta } if delta == "draft" => {
                steering_tx
                    .send("second".into())
                    .await
                    .expect("steering message should enqueue");
                break;
            }
            _ => {}
        }
    }

    let outcome = handle
        .await
        .expect("turn task should finish")
        .expect("steered turn should succeed");
    assert_eq!(outcome.response, "draftfinal");

    let new_chat_messages: Vec<_> = outcome
        .new_messages
        .iter()
        .filter_map(|msg| match msg {
            ConversationMessage::Chat(message) => {
                Some((message.role.as_str(), message.content.as_str()))
            }
            _ => None,
        })
        .collect();
    assert!(
        new_chat_messages
            .iter()
            .any(|(role, content)| { *role == "assistant" && *content == "draft" }),
        "already streamed output must be committed before the steering continuation"
    );
    assert!(
        new_chat_messages
            .iter()
            .any(|(role, content)| { *role == "user" && content.contains("second") }),
        "accepted steering must be retained as its own user turn"
    );

    let seen = seen_messages.lock();
    assert_eq!(seen.len(), 2);
    let second_call = &seen[1];
    assert!(
        second_call
            .iter()
            .any(|msg| msg.role == "assistant" && msg.content == "draft"),
        "second provider call must see the committed streamed assistant text"
    );
    assert!(
        second_call
            .iter()
            .filter(|msg| msg.role == "user")
            .any(|msg| msg.content.contains("second")),
        "second provider call must include the accepted steering user message"
    );
}

#[tokio::test]
async fn turn_streamed_with_steering_error_returns_committed_partial_output() {
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let model_provider = Box::new(StreamingSteeringModelProvider {
        seen_messages: Arc::new(Mutex::new(Vec::new())),
        call_count: AtomicUsize::new(0),
        fail_on_call: Some(2),
        fail_chat_on_call: Some(3),
        fail_after_delta_on_call: None,
        delay_chat_on_call: None,
    });
    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed with valid config");

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
    let (steering_tx, mut steering_rx) = tokio::sync::mpsc::channel::<String>(4);
    let handle = zeroclaw_spawn::spawn!(async move {
        agent
            .turn_streamed_with_steering_state("first", event_tx, None, Some(&mut steering_rx))
            .await
    });

    loop {
        match event_rx.recv().await.expect("turn event should arrive") {
            TurnEvent::Chunk { delta } if delta == "draft" => {
                steering_tx
                    .send("second".into())
                    .await
                    .expect("steering message should enqueue");
                break;
            }
            _ => {}
        }
    }

    let err = handle
        .await
        .expect("turn task should finish")
        .expect_err("second provider call should fail");
    assert_eq!(err.committed_response, "draft");
    assert!(
            err.new_messages.iter().any(|msg| {
                matches!(msg, ConversationMessage::Chat(message) if message.role == "assistant" && message.content == "draft")
            }),
            "committed partial assistant output should be returned for persistence after continuation failure"
        );
    assert!(
            err.new_messages.iter().any(|msg| {
                matches!(msg, ConversationMessage::Chat(message) if message.role == "user" && message.content.contains("second"))
            }),
            "accepted steering user message should still be returned after continuation failure"
        );
}

#[tokio::test]
async fn turn_streamed_error_before_visible_output_falls_back_to_chat() {
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let seen_messages = Arc::new(Mutex::new(Vec::new()));
    let model_provider = Box::new(StreamingSteeringModelProvider {
        seen_messages: seen_messages.clone(),
        call_count: AtomicUsize::new(0),
        fail_on_call: Some(1),
        fail_chat_on_call: None,
        fail_after_delta_on_call: None,
        delay_chat_on_call: None,
    });
    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed with valid config");

    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
    let handle = zeroclaw_spawn::spawn!(async move {
        agent
            .turn_streamed_with_steering_state("first", event_tx, None, None)
            .await
    });

    let outcome = handle
        .await
        .expect("turn task should finish")
        .expect("pre-output stream failure should fall back to non-streaming chat");
    assert_eq!(outcome.response, "final");
    assert!(
            outcome.new_messages.iter().any(|msg| {
                matches!(msg, ConversationMessage::Chat(message) if message.role == "assistant" && message.content == "final")
            }),
            "new messages should carry the fallback assistant answer"
        );
    assert!(
            !outcome.new_messages.iter().any(|msg| {
                matches!(msg, ConversationMessage::Chat(message) if message.role == "assistant" && message.content.contains(&crate::i18n::get_english_cli_string_with_args("turn-stream-interrupted", &[])))
            }),
            "successful fallback should not persist interrupted stream text"
        );

    let seen = seen_messages.lock();
    assert_eq!(seen.len(), 2);
    assert!(
        !seen[1]
            .iter()
            .any(|msg| { msg.role == "assistant" && msg.content.contains("draft") }),
        "fallback chat must not receive the abandoned stream attempt as prior assistant text"
    );
}

#[tokio::test]
async fn turn_streamed_error_after_delta_preserves_visible_partial() {
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let model_provider = Box::new(StreamingSteeringModelProvider {
        seen_messages: Arc::new(Mutex::new(Vec::new())),
        call_count: AtomicUsize::new(0),
        fail_on_call: None,
        fail_chat_on_call: None,
        fail_after_delta_on_call: Some(1),
        delay_chat_on_call: None,
    });
    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed with valid config");

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
    let handle = zeroclaw_spawn::spawn!(async move {
        agent
            .turn_streamed_with_steering_state("first", event_tx, None, None)
            .await
    });

    assert!(
        matches!(
            event_rx.recv().await,
            Some(TurnEvent::Chunk { delta }) if delta == "draft"
        ),
        "the client should see the streamed text before the provider error"
    );

    let err = handle
        .await
        .expect("turn task should finish")
        .expect_err("post-output stream failure should return an error with partial output");
    assert!(
        err.error
            .to_string()
            .contains("synthetic provider failure after delta"),
        "unexpected error: {}",
        err.error
    );
    assert!(
        err.committed_response
            .contains(&crate::i18n::get_english_cli_string_with_args(
                "turn-stream-interrupted",
                &[]
            )),
        "persisted partial text should mark that the visible stream was interrupted"
    );
    assert!(
            err.new_messages.iter().any(|msg| {
                matches!(msg, ConversationMessage::Chat(message) if message.role == "assistant" && message.content.contains("draft"))
            }),
            "new messages should carry the visible assistant partial for gateway persistence"
        );
}

#[tokio::test]
async fn turn_streamed_error_before_visible_output_fallback_can_be_cancelled() {
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let model_provider = Box::new(StreamingSteeringModelProvider {
        seen_messages: Arc::new(Mutex::new(Vec::new())),
        call_count: AtomicUsize::new(0),
        fail_on_call: Some(1),
        fail_chat_on_call: None,
        fail_after_delta_on_call: None,
        delay_chat_on_call: Some(2),
    });
    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed with valid config");

    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let cancel_for_task = cancel_token.clone();
    let handle = zeroclaw_spawn::spawn!(async move {
        agent
            .turn_streamed_with_steering_state("first", event_tx, Some(cancel_for_task), None)
            .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    cancel_token.cancel();

    let err = handle
        .await
        .expect("turn task should finish")
        .expect_err("cancelled fallback should return cancellation");
    assert!(
        crate::agent::loop_::is_tool_loop_cancelled(&err.error),
        "unexpected error: {}",
        err.error
    );
    assert_eq!(
        err.committed_response,
        crate::i18n::get_english_cli_string_with_args("turn-interrupted-by-user", &[])
    );
    assert!(
            err.new_messages.iter().any(|msg| {
                matches!(msg, ConversationMessage::Chat(message) if message.role == "assistant" && message.content == crate::i18n::get_english_cli_string_with_args("turn-interrupted-by-user", &[]))
            }),
            "pre-output fallback cancellation should include an interruption marker"
        );
}

#[tokio::test]
async fn turn_streamed_cancel_before_output_returns_interruption_message() {
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let model_provider = Box::new(StreamingSteeringModelProvider {
        seen_messages: Arc::new(Mutex::new(Vec::new())),
        call_count: AtomicUsize::new(0),
        fail_on_call: None,
        fail_chat_on_call: None,
        fail_after_delta_on_call: None,
        delay_chat_on_call: None,
    });
    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed with valid config");

    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
    let cancel_token = tokio_util::sync::CancellationToken::new();
    cancel_token.cancel();

    let err = agent
        .turn_streamed_with_steering_state("first", event_tx, Some(cancel_token), None)
        .await
        .expect_err("pre-cancelled turn should return cancellation");

    assert!(
        crate::agent::loop_::is_tool_loop_cancelled(&err.error),
        "unexpected error: {}",
        err.error
    );
    assert_eq!(
        err.committed_response,
        crate::i18n::get_english_cli_string_with_args("turn-interrupted-by-user", &[])
    );
    assert!(
            err.new_messages.iter().any(|msg| {
                matches!(msg, ConversationMessage::Chat(message) if message.role == "assistant" && message.content == crate::i18n::get_english_cli_string_with_args("turn-interrupted-by-user", &[]))
            }),
            "cancelled turn should include an assistant interruption marker for persistence"
        );
}

#[tokio::test]
async fn turn_streamed_stream_error_after_delta_emits_llm_response_failure() {
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let model_provider = Box::new(StreamingSteeringModelProvider {
        seen_messages: Arc::new(Mutex::new(Vec::new())),
        call_count: AtomicUsize::new(0),
        fail_on_call: None,
        fail_chat_on_call: None,
        fail_after_delta_on_call: Some(1),
        delay_chat_on_call: None,
    });
    let capturing = Arc::new(CapturingObserver::default());
    let observer: Arc<dyn Observer> = capturing.clone();
    let mut agent = Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed with valid config");

    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
    let err = agent
        .turn_streamed_with_steering_state("test", event_tx, None, None)
        .await
        .expect_err("provider stream failure should be returned");

    assert!(
        err.committed_response.contains("draft")
            && err
                .committed_response
                .contains(&crate::i18n::get_english_cli_string_with_args(
                    "turn-stream-interrupted",
                    &[]
                )),
        "unexpected committed_response: {}",
        err.committed_response
    );

    let events = capturing.events.lock();
    let request = events
        .iter()
        .find(|e| matches!(e, ObserverEvent::LlmRequest { .. }))
        .expect("LlmRequest should have been recorded");
    let response = events
        .iter()
        .find(|e| matches!(e, ObserverEvent::LlmResponse { .. }))
        .expect("LlmResponse should have been recorded");

    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, ObserverEvent::LlmRequest { .. }))
            .count(),
        1,
        "exactly one LlmRequest expected"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, ObserverEvent::LlmResponse { .. }))
            .count(),
        1,
        "exactly one LlmResponse expected"
    );

    let (
        ObserverEvent::LlmRequest {
            model_provider: req_provider,
            model: req_model,
            ..
        },
        ObserverEvent::LlmResponse {
            model_provider: resp_provider,
            model: resp_model,
            success,
            error_message,
            ..
        },
    ) = (request, response)
    else {
        panic!("matched event variants should be LlmRequest and LlmResponse");
    };

    assert!(!success, "LlmResponse on stream error must be a failure");
    assert!(
        error_message.as_deref().is_some_and(|m| !m.is_empty()),
        "failure LlmResponse must carry a non-empty error_message"
    );
    assert_eq!(req_provider, resp_provider, "provider should match");
    assert_eq!(req_model, resp_model, "model should match");
}

#[tokio::test]
async fn turn_streamed_cancel_during_stream_emits_llm_response_failure() {
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let model_provider = Box::new(StreamingSteeringModelProvider {
        seen_messages: Arc::new(Mutex::new(Vec::new())),
        call_count: AtomicUsize::new(0),
        fail_on_call: None,
        fail_chat_on_call: None,
        fail_after_delta_on_call: None,
        delay_chat_on_call: None,
    });
    let capturing = Arc::new(CapturingObserver::default());
    let observer: Arc<dyn Observer> = capturing.clone();
    let mut agent = Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed with valid config");

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let cancel_for_task = cancel_token.clone();

    let canceller = zeroclaw_spawn::spawn!(async move {
        while let Some(event) = event_rx.recv().await {
            if matches!(event, TurnEvent::Chunk { ref delta } if delta == "draft") {
                cancel_for_task.cancel();
                break;
            }
        }
        while event_rx.recv().await.is_some() {}
    });

    let err = agent
        .turn_streamed_with_steering_state("test", event_tx, Some(cancel_token), None)
        .await
        .expect_err("cancelled turn should return cancellation");

    canceller.await.expect("canceller task should finish");

    assert!(
        crate::agent::loop_::is_tool_loop_cancelled(&err.error),
        "cancelled turn should carry the cancellation error: {}",
        err.error
    );

    let events = capturing.events.lock();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, ObserverEvent::LlmRequest { .. }))
            .count(),
        1,
        "exactly one LlmRequest expected"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, ObserverEvent::LlmResponse { .. }))
            .count(),
        1,
        "exactly one LlmResponse expected"
    );

    let request = events
        .iter()
        .find(|e| matches!(e, ObserverEvent::LlmRequest { .. }))
        .expect("LlmRequest should have been recorded");
    let response = events
        .iter()
        .find(|e| matches!(e, ObserverEvent::LlmResponse { .. }))
        .expect("LlmResponse should have been recorded");

    let (
        ObserverEvent::LlmRequest {
            model_provider: req_provider,
            model: req_model,
            ..
        },
        ObserverEvent::LlmResponse {
            model_provider: resp_provider,
            model: resp_model,
            success,
            error_message,
            ..
        },
    ) = (request, response)
    else {
        panic!("matched event variants should be LlmRequest and LlmResponse");
    };

    assert!(!success, "cancellation LlmResponse must be a failure");
    assert_eq!(
        error_message.as_deref(),
        Some("request cancelled by user"),
        "cancellation LlmResponse must carry the fixed cancel message"
    );
    assert_eq!(req_provider, resp_provider, "provider should match");
    assert_eq!(req_model, resp_model, "model should match");
}

// ── Skill tool registration & excluded_tools filtering ──────────

/// A mock tool whose name is configurable (unlike `MockTool` which is
/// always "echo").
struct NamedMockTool {
    tool_name: String,
}

impl NamedMockTool {
    fn new(name: &str) -> Self {
        Self {
            tool_name: name.to_string(),
        }
    }
}

#[async_trait]
impl Tool for NamedMockTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        "mock"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<crate::tools::ToolResult> {
        Ok(crate::tools::ToolResult {
            success: true,
            output: "ok".into(),
            error: None,
        })
    }
}

fn make_skill(name: &str, tool_names: &[&str]) -> crate::skills::Skill {
    crate::skills::Skill {
        name: name.to_string(),
        description: format!("{name} skill"),
        description_localizations: Default::default(),
        version: "0.1.0".to_string(),
        author: None,
        tags: vec![],
        tools: tool_names
            .iter()
            .map(|t| crate::skills::SkillTool {
                name: t.to_string(),
                description: format!("{t} tool"),
                kind: "shell".to_string(),
                command: format!("echo {t}"),
                args: std::collections::HashMap::new(),
                target: None,
                locked_args: std::collections::HashMap::new(),
                timeout_secs: None,
            })
            .collect(),
        prompts: vec![],
        slash_options: Vec::new(),
        location: None,
    }
}

#[test]
fn register_skill_tools_adds_skill_tools_to_registry() {
    let security = Arc::new(crate::security::SecurityPolicy::default());
    let mut tools: Vec<Box<dyn Tool>> = vec![Box::new(NamedMockTool::new("builtin_a"))];

    let skills = vec![make_skill("deploy", &["run", "status"])];
    tools::register_skill_tools(&mut tools, &skills, security);

    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert_eq!(names, &["builtin_a", "deploy__run", "deploy__status"]);
}

#[test]
fn register_skill_tools_skips_shadowed_builtins() {
    let security = Arc::new(crate::security::SecurityPolicy::default());
    // Pre-populate with a tool whose name matches what the skill would produce.
    let mut tools: Vec<Box<dyn Tool>> = vec![Box::new(NamedMockTool::new("my_skill__run"))];

    let skills = vec![make_skill("my_skill", &["run"])];
    tools::register_skill_tools(&mut tools, &skills, security);

    // Should still be just 1 tool — the duplicate was skipped.
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name(), "my_skill__run");
}

#[test]
fn register_skill_tools_honors_excluded_tools() {
    // excluded_tools always subtracts — including skill-defined tools (previously
    // skill tools bypassed the policy entirely; theclass, missed for skills).
    let security = Arc::new(crate::security::SecurityPolicy {
        excluded_tools: Some(vec!["deploy__status".to_string()]),
        ..crate::security::SecurityPolicy::default()
    });
    let mut tools: Vec<Box<dyn Tool>> = vec![Box::new(NamedMockTool::new("builtin_a"))];

    let skills = vec![make_skill("deploy", &["run", "status"])];
    tools::register_skill_tools(&mut tools, &skills, security);

    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert!(
        names.contains(&"deploy__run"),
        "non-excluded skill tool must register, got {names:?}"
    );
    assert!(
        !names.contains(&"deploy__status"),
        "excluded_tools must subtract the skill tool deploy__status, got {names:?}"
    );
}

#[test]
fn register_skill_tools_allowlist_does_not_hide_skills() {
    // The allowlist gates built-ins, NOT skill tools: skills are granted explicitly via
    // skill config, and builtin-kind skill tools are scoped-elevation wrappers meant to
    // stay callable when the raw tool is off the allowlist. A restrictive allowed_tools
    // that omits the skill tool must NOT remove it (only excluded_tools does).
    let security = Arc::new(crate::security::SecurityPolicy {
        allowed_tools: Some(vec!["shell".to_string()]),
        ..crate::security::SecurityPolicy::default()
    });
    let mut tools: Vec<Box<dyn Tool>> = Vec::new();

    let skills = vec![make_skill("deploy", &["run"])];
    tools::register_skill_tools(&mut tools, &skills, security);

    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert!(
        names.contains(&"deploy__run"),
        "allowlist must not hide an explicitly-granted skill tool, got {names:?}"
    );
}

#[test]
fn from_config_policy_filter_blocks_raw_target_but_keeps_scoped_wrapper() {
    use crate::skills::{Skill, SkillTool};

    let shell: Arc<dyn Tool> = Arc::new(NamedMockTool::new("shell"));
    let file_read: Arc<dyn Tool> = Arc::new(NamedMockTool::new("file_read"));
    // The resolution registry retains the raw tool so the wrapper can
    // delegate to it even after the policy filter removes it below.
    let resolution: Vec<Arc<dyn Tool>> = vec![Arc::clone(&shell), Arc::clone(&file_read)];

    let mut tools: Vec<Box<dyn Tool>> = vec![
        Box::new(crate::tools::ArcToolRef(Arc::clone(&shell))),
        Box::new(crate::tools::ArcToolRef(Arc::clone(&file_read))),
    ];

    // Allowlist the agent to `file_read` only — the gate from_config now
    // applies to built-ins before skills register. (Pre-fix, from_config
    // honored only the denylist, so raw `shell` leaked through.)
    let policy = crate::security::SecurityPolicy {
        allowed_tools: Some(vec!["file_read".to_string()]),
        workspace_dir: std::env::temp_dir(),
        ..crate::security::SecurityPolicy::default()
    };
    crate::agent::loop_::apply_policy_tool_filter(&mut tools, Some(&policy), None);
    assert!(
        !tools.iter().any(|t| t.name() == "shell"),
        "raw shell must be removed by the allowlist on the from_config path"
    );
    assert!(
        tools.iter().any(|t| t.name() == "file_read"),
        "allowlisted file_read must survive the filter"
    );

    let skill = Skill {
        name: "ops".to_string(),
        description: "d".to_string(),
        description_localizations: Default::default(),
        version: "1".to_string(),
        author: None,
        tags: vec![],
        tools: vec![SkillTool {
            name: "use_shell".to_string(),
            description: "scoped shell".to_string(),
            kind: "builtin".to_string(),
            command: String::new(),
            args: std::collections::HashMap::new(),
            target: Some("shell".to_string()),
            locked_args: std::collections::HashMap::new(),
            timeout_secs: None,
        }],
        prompts: vec![],
        slash_options: Vec::new(),
        location: None,
    };
    tools::register_skill_tools_with_context(
        &mut tools,
        &[skill],
        Arc::new(crate::security::SecurityPolicy::default()),
        &resolution,
    );

    assert!(
        !tools.iter().any(|t| t.name() == "shell"),
        "raw shell must STILL be unavailable after skill registration"
    );
    assert!(
        tools.iter().any(|t| t.name() == "ops__use_shell"),
        "the scoped elevation wrapper must remain the only callable path to shell"
    );
}

#[test]
fn excluded_tools_filters_matching_tools() {
    let mut tools: Vec<Box<dyn Tool>> = vec![
        Box::new(NamedMockTool::new("shell")),
        Box::new(NamedMockTool::new("file_write")),
        Box::new(NamedMockTool::new("web_search")),
    ];

    let excluded = ["shell".to_string(), "file_write".to_string()];
    tools.retain(|t| !excluded.iter().any(|ex| ex == t.name()));

    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert_eq!(names, &["web_search"]);
}

#[test]
fn excluded_tools_preserves_non_excluded() {
    let mut tools: Vec<Box<dyn Tool>> = vec![
        Box::new(NamedMockTool::new("shell")),
        Box::new(NamedMockTool::new("file_read")),
        Box::new(NamedMockTool::new("web_fetch")),
    ];

    // Exclude only "shell" — the other two should survive.
    let excluded = ["shell".to_string()];
    tools.retain(|t| !excluded.iter().any(|ex| ex == t.name()));

    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert_eq!(names, &["file_read", "web_fetch"]);
}

#[test]
fn empty_excluded_tools_preserves_all() {
    let mut tools: Vec<Box<dyn Tool>> = vec![
        Box::new(NamedMockTool::new("shell")),
        Box::new(NamedMockTool::new("file_read")),
    ];

    let excluded: Vec<String> = vec![];
    if !excluded.is_empty() {
        tools.retain(|t| !excluded.iter().any(|ex| ex == t.name()));
    }

    assert_eq!(tools.len(), 2);
}

#[tokio::test]
async fn turn_streamed_returns_new_messages_at_history_limit() {
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    // Use a small limit so that pre-filling to the limit forces a trim on
    // the very first new turn.
    let agent_config = zeroclaw_config::schema::AliasedAgentConfig {
        resolved: zeroclaw_config::schema::ResolvedRuntime::default(),
        ..zeroclaw_config::schema::AliasedAgentConfig::default()
    };

    // Simple streaming provider that returns plain text (no tool calls).
    let provider = Box::new(NarrationStreamModelProvider {
        call_count: Arc::new(Mutex::new(0)),
    });

    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut agent = Agent::builder()
        .model_provider(provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .config(agent_config)
        .structured_max_history_messages(4)
        .build()
        .expect("agent builder should succeed with valid config");

    // Pre-fill the history to exactly max_history_messages non-system
    // messages so that adding a new user+assistant pair triggers trim.
    // (system message is added by turn_streamed on first call, so we
    // push user+assistant pairs to simulate a history-at-limit state.)
    agent
        .history
        .push(ConversationMessage::Chat(ChatMessage::system("sys")));
    for i in 0..2 {
        agent
            .history
            .push(ConversationMessage::Chat(ChatMessage::user(format!(
                "old {i}"
            ))));
        agent
            .history
            .push(ConversationMessage::Chat(ChatMessage::assistant(format!(
                "old reply {i}"
            ))));
    }
    // History is now: [system, user0, assistant0, user1, assistant1] = 5
    // entries. The structured message limit of 4 means trim fires after
    // adding the new turn.

    let (event_tx, _rx) = tokio::sync::mpsc::channel::<TurnEvent>(8);
    let (_, new_msgs) = agent
        .turn_streamed("new question", event_tx, None)
        .await
        .expect("turn_streamed should succeed");

    // The returned Vec must contain the new user message.
    let has_user = new_msgs
        .iter()
        .any(|m| matches!(m, ConversationMessage::Chat(c) if c.role == "user"));
    assert!(
        has_user,
        "new_msgs must include the user message even after trim; got: {new_msgs:?}"
    );

    // The returned Vec must contain the new assistant reply.
    let has_assistant = new_msgs
        .iter()
        .any(|m| matches!(m, ConversationMessage::Chat(c) if c.role == "assistant"));
    assert!(
        has_assistant,
        "new_msgs must include the assistant reply even after trim; got: {new_msgs:?}"
    );
}

#[test]
fn excluded_tools_then_skill_registration_end_to_end() {
    let security = Arc::new(crate::security::SecurityPolicy::default());
    let mut tools: Vec<Box<dyn Tool>> = vec![
        Box::new(NamedMockTool::new("shell")),
        Box::new(NamedMockTool::new("file_read")),
        Box::new(NamedMockTool::new("web_fetch")),
    ];

    // Step 1: filter excluded tools (mirrors from_config logic)
    let excluded = ["shell".to_string()];
    tools.retain(|t| !excluded.iter().any(|ex| ex == t.name()));

    // Step 2: register skill tools (mirrors from_config logic)
    let skills = vec![make_skill("ops", &["deploy", "rollback"])];
    tools::register_skill_tools(&mut tools, &skills, security);

    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert_eq!(
        names,
        &["file_read", "web_fetch", "ops__deploy", "ops__rollback"]
    );
}

fn observer_event_turn_id(event: &ObserverEvent) -> Option<&str> {
    match event {
        ObserverEvent::AgentStart { turn_id, .. }
        | ObserverEvent::LlmRequest { turn_id, .. }
        | ObserverEvent::LlmResponse { turn_id, .. }
        | ObserverEvent::AgentEnd { turn_id, .. }
        | ObserverEvent::ToolCall { turn_id, .. }
        | ObserverEvent::ToolCallStart { turn_id, .. }
        | ObserverEvent::MemoryRecall { turn_id, .. }
        | ObserverEvent::MemoryStore { turn_id, .. }
        | ObserverEvent::RagRetrieve { turn_id, .. } => turn_id.as_deref(),
        _ => None,
    }
}

fn assert_all_events_share_turn_id(
    events: &[ObserverEvent],
    expected_alias: Option<&str>,
    expected_channel: Option<&str>,
) {
    let mut turn_ids: Vec<String> = Vec::new();
    for event in events {
        let (variant, channel, agent_alias, turn_id) = match event {
            ObserverEvent::AgentStart {
                channel,
                agent_alias,
                turn_id,
                ..
            } => ("AgentStart", channel, agent_alias, turn_id),
            ObserverEvent::AgentEnd {
                channel,
                agent_alias,
                turn_id,
                ..
            } => ("AgentEnd", channel, agent_alias, turn_id),
            ObserverEvent::LlmRequest {
                channel,
                agent_alias,
                turn_id,
                ..
            } => ("LlmRequest", channel, agent_alias, turn_id),
            ObserverEvent::LlmResponse {
                channel,
                agent_alias,
                turn_id,
                ..
            } => ("LlmResponse", channel, agent_alias, turn_id),
            ObserverEvent::ToolCallStart {
                channel,
                agent_alias,
                turn_id,
                ..
            } => ("ToolCallStart", channel, agent_alias, turn_id),
            ObserverEvent::ToolCall {
                channel,
                agent_alias,
                turn_id,
                ..
            } => ("ToolCall", channel, agent_alias, turn_id),
            ObserverEvent::MemoryRecall {
                channel,
                agent_alias,
                turn_id,
                ..
            } => ("MemoryRecall", channel, agent_alias, turn_id),
            ObserverEvent::MemoryStore {
                channel,
                agent_alias,
                turn_id,
                ..
            } => ("MemoryStore", channel, agent_alias, turn_id),
            ObserverEvent::RagRetrieve {
                channel,
                agent_alias,
                turn_id,
                ..
            } => ("RagRetrieve", channel, agent_alias, turn_id),
            _ => continue,
        };
        assert!(
            channel.is_some(),
            "{variant} observer event must carry channel, got None: {event:?}"
        );
        assert!(
            agent_alias.is_some(),
            "{variant} observer event must carry agent_alias, got None: {event:?}"
        );
        assert!(
            turn_id.is_some(),
            "{variant} observer event must carry turn_id, got None: {event:?}"
        );
        turn_ids.push(turn_id.clone().expect("checked Some above"));
    }

    assert!(!turn_ids.is_empty(), "expected turn events with turn_id");
    let first = &turn_ids[0];
    assert!(
        turn_ids.iter().all(|id| id == first),
        "all turn_ids should be consistent"
    );

    if let Some(alias) = expected_alias {
        for e in events {
            let agent_alias = match e {
                ObserverEvent::AgentStart { agent_alias, .. }
                | ObserverEvent::AgentEnd { agent_alias, .. }
                | ObserverEvent::LlmRequest { agent_alias, .. }
                | ObserverEvent::LlmResponse { agent_alias, .. }
                | ObserverEvent::ToolCallStart { agent_alias, .. }
                | ObserverEvent::ToolCall { agent_alias, .. }
                | ObserverEvent::MemoryRecall { agent_alias, .. }
                | ObserverEvent::MemoryStore { agent_alias, .. }
                | ObserverEvent::RagRetrieve { agent_alias, .. } => agent_alias,
                _ => continue,
            };
            assert_eq!(
                agent_alias.as_deref(),
                Some(alias),
                "agent_alias should be consistent"
            );
        }
    }

    if let Some(channel) = expected_channel {
        for e in events {
            let ch = match e {
                ObserverEvent::AgentStart { channel: ch, .. }
                | ObserverEvent::LlmRequest { channel: ch, .. }
                | ObserverEvent::LlmResponse { channel: ch, .. }
                | ObserverEvent::ToolCallStart { channel: ch, .. }
                | ObserverEvent::ToolCall { channel: ch, .. }
                | ObserverEvent::AgentEnd { channel: ch, .. }
                | ObserverEvent::MemoryRecall { channel: ch, .. }
                | ObserverEvent::MemoryStore { channel: ch, .. }
                | ObserverEvent::RagRetrieve { channel: ch, .. } => ch,
                _ => continue,
            };
            assert_eq!(ch.as_deref(), Some(channel), "channel should be consistent");
        }
    }
}

fn assert_single_agent_lifecycle(events: &[ObserverEvent]) -> (usize, usize) {
    let starts: Vec<_> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| matches!(event, ObserverEvent::AgentStart { .. }))
        .collect();
    let ends: Vec<_> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| matches!(event, ObserverEvent::AgentEnd { .. }))
        .collect();

    assert_eq!(starts.len(), 1, "expected exactly one AgentStart");
    assert_eq!(ends.len(), 1, "expected exactly one AgentEnd");
    assert!(starts[0].0 < ends[0].0, "AgentEnd must follow AgentStart");
    assert_eq!(
        observer_event_turn_id(starts[0].1),
        observer_event_turn_id(ends[0].1),
        "AgentEnd turn_id must match AgentStart turn_id"
    );

    (starts[0].0, ends[0].0)
}

fn agent_end_tokens(
    event: &ObserverEvent,
) -> Option<zeroclaw_api::observability_traits::TurnTokenUsage> {
    match event {
        ObserverEvent::AgentEnd { tokens_used, .. } => tokens_used.clone(),
        _ => None,
    }
}

#[tokio::test]
async fn turn_cache_hit_emits_agent_end_with_none_tokens() {
    let tmp = tempfile::tempdir().expect("temp response cache dir");
    let cache = Arc::new(
        zeroclaw_memory::response_cache::ResponseCache::new(tmp.path(), 60, 100)
            .expect("response cache should initialize"),
    );
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem_a: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );
    let mem_b: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let ws_dir = tmp.path().to_path_buf();
    let mut agent_a = Agent::builder()
        .model_provider(Box::new(MockModelProvider {
            responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
                text: Some("cached answer".into()),
                tool_calls: vec![],
                usage: Some(zeroclaw_providers::traits::TokenUsage {
                    input_tokens: Some(10),
                    cached_input_tokens: None,
                    output_tokens: Some(5),
                }),
                reasoning_content: None,
            }]),
        }))
        .tools(vec![Box::new(MockTool)])
        .memory(mem_a)
        .observer(Arc::from(crate::observability::NoopObserver {}) as Arc<dyn Observer>)
        .response_cache(Some(cache.clone()))
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(ws_dir.clone())
        .model_name("test-model".into())
        .temperature(Some(0.0))
        .prompt_builder(SystemPromptBuilder::default())
        .turn_datetime(fixed_response_cache_turn_datetime)
        .build()
        .expect("agent builder should succeed with valid config");

    assert_eq!(agent_a.turn("seed").await.unwrap(), "cached answer");

    let capturing = Arc::new(CapturingObserver::default());
    let observer: Arc<dyn Observer> = capturing.clone();
    let mut agent_b = Agent::builder()
        .model_provider(Box::new(MockModelProvider {
            responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
                text: Some("uncached answer".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            }]),
        }))
        .tools(vec![Box::new(MockTool)])
        .memory(mem_b)
        .observer(observer)
        .response_cache(Some(cache))
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(ws_dir)
        .model_name("test-model".into())
        .temperature(Some(0.0))
        .prompt_builder(SystemPromptBuilder::default())
        .turn_datetime(fixed_response_cache_turn_datetime)
        .build()
        .expect("agent builder should succeed with valid config");

    assert_eq!(agent_b.turn("seed").await.unwrap(), "cached answer");

    let events = capturing.events.lock();
    let (_, end_idx) = assert_single_agent_lifecycle(&events);
    assert!(agent_end_tokens(&events[end_idx]).is_none());
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ObserverEvent::LlmRequest { .. })),
        "cache hit should not call the LLM"
    );
}

#[tokio::test]
async fn turn_streamed_cancel_during_tool_execution_emits_agent_end_with_tokens() {
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );
    let capturing = Arc::new(CapturingObserver::default());
    let observer: Arc<dyn Observer> = capturing.clone();
    let mut agent = Agent::builder()
        .model_provider(Box::new(MockModelProvider {
            responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
                text: Some("I will echo.".into()),
                tool_calls: vec![zeroclaw_providers::ToolCall {
                    id: "tc1".into(),
                    name: "echo".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                }],
                usage: Some(zeroclaw_providers::traits::TokenUsage {
                    input_tokens: Some(10),
                    cached_input_tokens: None,
                    output_tokens: Some(5),
                }),
                reasoning_content: None,
            }]),
        }))
        .tools(vec![Box::new(SlowTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .build()
        .expect("agent builder should succeed with valid config");

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let cancel_for_task = cancel_token.clone();
    let handle = zeroclaw_spawn::spawn!(async move {
        agent
            .turn_streamed_with_steering_state("use echo", event_tx, Some(cancel_for_task), None)
            .await
    });

    while let Some(event) = event_rx.recv().await {
        if matches!(event, TurnEvent::Usage { .. }) {
            cancel_token.cancel();
            break;
        }
    }

    handle
        .await
        .expect("turn task should finish")
        .expect_err("turn should be cancelled before tool execution completes");

    let events = capturing.events.lock();
    let (_, end_idx) = assert_single_agent_lifecycle(&events);
    let tokens = agent_end_tokens(&events[end_idx]).expect("AgentEnd should include tokens");
    assert_eq!(tokens.input_tokens, 10);
    assert_eq!(tokens.output_tokens, 5);
    let llm_response_idx = events
        .iter()
        .position(|event| matches!(event, ObserverEvent::LlmResponse { success: true, .. }))
        .expect("successful LlmResponse should be recorded");
    assert!(
        llm_response_idx < end_idx,
        "AgentEnd must follow LlmResponse"
    );
}

#[tokio::test]
async fn turn_reuses_outer_cost_tracking_context() {
    use crate::agent::cost::{
        TOOL_LOOP_COST_TRACKING_CONTEXT, TOOL_LOOP_TURN_USAGE, ToolLoopCostTrackingContext,
        TurnUsage,
    };
    use crate::cost::CostTracker;
    use std::collections::HashMap;

    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );
    let workspace = tempfile::TempDir::new().expect("temp dir");
    let tracker = Arc::new(
        CostTracker::new(
            zeroclaw_config::schema::CostConfig {
                enabled: true,
                track_per_agent: true,
                ..zeroclaw_config::schema::CostConfig::default()
            },
            workspace.path(),
        )
        .expect("cost tracker should initialize"),
    );
    let pricing = Arc::new(HashMap::from([(
        "mock-provider".to_string(),
        HashMap::from([
            ("test-model.input".to_string(), 3.0),
            ("test-model.output".to_string(), 15.0),
        ]),
    )]));
    let cost_context = ToolLoopCostTrackingContext::new(Arc::clone(&tracker), pricing)
        .with_agent_alias("agent-turn");
    let turn_usage = Arc::new(parking_lot::Mutex::new(TurnUsage::default()));

    let mut agent = Agent::builder()
        .model_provider(Box::new(MockModelProvider {
            responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
                text: Some("turn cost".into()),
                tool_calls: vec![],
                usage: Some(zeroclaw_providers::traits::TokenUsage {
                    input_tokens: Some(1_000),
                    cached_input_tokens: None,
                    output_tokens: Some(200),
                }),
                reasoning_content: None,
            }]),
        }))
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(Arc::from(crate::observability::NoopObserver {}) as Arc<dyn Observer>)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .model_name("test-model".into())
        .model_provider_name("mock-provider".into())
        .agent_alias("agent-turn".into())
        .build()
        .expect("agent builder should succeed with valid config");

    let response = TOOL_LOOP_TURN_USAGE
        .scope(
            Some(Arc::clone(&turn_usage)),
            TOOL_LOOP_COST_TRACKING_CONTEXT.scope(Some(cost_context), agent.turn("hello")),
        )
        .await
        .expect("turn should succeed");

    assert_eq!(response, "turn cost");

    let recorded = *turn_usage.lock();
    assert_eq!(recorded.input_tokens, 1_000);
    assert_eq!(recorded.output_tokens, 200);
    assert!(
        recorded.cost_usd > 0.0,
        "outer turn usage should accumulate non-zero cost from scoped pricing"
    );

    let summary = tracker.get_summary().expect("cost summary");
    assert_eq!(summary.request_count, 1);
    assert_eq!(summary.total_tokens, 1_200);
    assert!(
        summary.session_cost_usd > 0.0,
        "scoped tracker should persist turn usage"
    );
    let agent_summary = tracker
        .get_summary_for_agent("agent-turn")
        .expect("agent-scoped summary");
    assert_eq!(agent_summary.request_count, 1);
    assert!(
        agent_summary.session_cost_usd > 0.0,
        "agent alias should flow through persisted turn usage"
    );
}

#[tokio::test]
async fn turn_streamed_reuses_outer_cost_tracking_context() {
    use crate::agent::cost::{
        TOOL_LOOP_COST_TRACKING_CONTEXT, TOOL_LOOP_TURN_USAGE, ToolLoopCostTrackingContext,
        TurnUsage,
    };
    use crate::cost::CostTracker;
    use std::collections::HashMap;

    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );
    let workspace = tempfile::TempDir::new().expect("temp dir");
    let tracker = Arc::new(
        CostTracker::new(
            zeroclaw_config::schema::CostConfig {
                enabled: true,
                track_per_agent: true,
                ..zeroclaw_config::schema::CostConfig::default()
            },
            workspace.path(),
        )
        .expect("cost tracker should initialize"),
    );
    let pricing = Arc::new(HashMap::from([(
        "mock-provider".to_string(),
        HashMap::from([
            ("test-model.input".to_string(), 3.0),
            ("test-model.output".to_string(), 15.0),
        ]),
    )]));
    let cost_context = ToolLoopCostTrackingContext::new(Arc::clone(&tracker), pricing)
        .with_agent_alias("streamed-agent");
    let turn_usage = Arc::new(parking_lot::Mutex::new(TurnUsage::default()));

    let mut agent = Agent::builder()
        .model_provider(Box::new(MockModelProvider {
            responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
                text: Some("streamed cost".into()),
                tool_calls: vec![],
                usage: Some(zeroclaw_providers::traits::TokenUsage {
                    input_tokens: Some(1_000),
                    cached_input_tokens: None,
                    output_tokens: Some(200),
                }),
                reasoning_content: None,
            }]),
        }))
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(Arc::from(crate::observability::NoopObserver {}) as Arc<dyn Observer>)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .model_name("test-model".into())
        .model_provider_name("mock-provider".into())
        .agent_alias("streamed-agent".into())
        .build()
        .expect("agent builder should succeed with valid config");

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
    let outcome = TOOL_LOOP_TURN_USAGE
        .scope(
            Some(Arc::clone(&turn_usage)),
            TOOL_LOOP_COST_TRACKING_CONTEXT.scope(
                Some(cost_context),
                agent.turn_streamed_with_steering_state("hello", event_tx, None, None),
            ),
        )
        .await
        .expect("streamed turn should succeed");

    assert_eq!(outcome.response, "streamed cost");
    while event_rx.recv().await.is_some() {}

    let recorded = *turn_usage.lock();
    assert_eq!(recorded.input_tokens, 1_000);
    assert_eq!(recorded.output_tokens, 200);
    assert!(
        recorded.cost_usd > 0.0,
        "outer turn usage should accumulate non-zero cost from scoped pricing"
    );

    let summary = tracker.get_summary().expect("cost summary");
    assert_eq!(summary.request_count, 1);
    assert_eq!(summary.total_tokens, 1_200);
    assert!(
        summary.session_cost_usd > 0.0,
        "scoped tracker should persist streamed-turn usage"
    );
    let agent_summary = tracker
        .get_summary_for_agent("streamed-agent")
        .expect("agent-scoped summary");
    assert_eq!(agent_summary.request_count, 1);
    assert!(
        agent_summary.session_cost_usd > 0.0,
        "agent alias should flow through persisted streamed-turn usage"
    );
}

#[tokio::test]
async fn turn_llm_error_emits_agent_end() {
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );
    let capturing = Arc::new(CapturingObserver::default());
    let observer: Arc<dyn Observer> = capturing.clone();
    let mut agent = Agent::builder()
        .model_provider(Box::new(FailingModelProvider))
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .model_name("test-model".into())
        .temperature(Some(0.0))
        .build()
        .expect("agent builder should succeed with valid config");

    let result = agent.turn("hello").await;
    assert!(
        result.is_err(),
        "turn should fail when provider is unavailable"
    );

    let events = capturing.events.lock();
    let (_, end_idx) = assert_single_agent_lifecycle(&events);
    assert!(
        agent_end_tokens(&events[end_idx]).is_none(),
        "AgentEnd should have tokens_used: None on LLM error"
    );
}

#[tokio::test]
async fn turn_events_share_consistent_turn_id() {
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let model_provider = Box::new(MockModelProvider {
        responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
            text: Some("done".into()),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        }]),
    });
    let capturing = Arc::new(CapturingObserver::default());
    let observer: Arc<dyn Observer> = capturing.clone();
    let mut agent = Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .agent_alias("test-agent".into())
        .auto_save(true)
        .build()
        .expect("agent builder should succeed with valid config");

    let _ = agent.turn("test").await.expect("turn should succeed");

    let events = capturing.events.lock();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ObserverEvent::MemoryStore { .. })),
        "auto_save(true) must cause Agent::turn to emit a MemoryStore event \
             so its (channel, agent_alias, turn_id) triple is actually asserted below"
    );
    assert_all_events_share_turn_id(&events, Some("test-agent"), Some("agent"));
}

#[tokio::test]
async fn streamed_turn_events_share_consistent_turn_id() {
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation should succeed with valid config"),
    );

    let model_provider = Box::new(MockModelProvider {
        responses: Mutex::new(vec![zeroclaw_providers::ChatResponse {
            text: Some("done".into()),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        }]),
    });
    let capturing = Arc::new(CapturingObserver::default());
    let observer: Arc<dyn Observer> = capturing.clone();
    let mut agent = Agent::builder()
        .model_provider(model_provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .agent_alias("test-agent".into())
        .auto_save(true)
        .build()
        .expect("agent builder should succeed with valid config");

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
    let _ = agent
        .turn_streamed_with_steering_state("test", event_tx, None, None)
        .await
        .expect("streamed turn should succeed");
    while event_rx.recv().await.is_some() {}

    let events = capturing.events.lock();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ObserverEvent::MemoryStore { .. })),
        "auto_save(true) must cause the streamed turn to emit a MemoryStore event"
    );
    assert_all_events_share_turn_id(&events, Some("test-agent"), Some("agent"));
}

fn build_test_agent(
    initial_provider_name: &str,
    initial_model_name: &str,
    switch_config: Option<ProviderSwitchConfig>,
) -> Agent {
    let provider = Box::new(MockModelProvider {
        responses: Mutex::new(vec![]),
    });
    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation"),
    );
    let observer: Arc<dyn Observer> = Arc::from(crate::observability::NoopObserver {});
    let mut builder = Agent::builder()
        .model_provider(provider)
        .tools(vec![Box::new(MockTool)])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .model_provider_name(initial_provider_name.to_string())
        .model_name(initial_model_name.to_string());
    if let Some(cfg) = switch_config {
        builder = builder.provider_switch_config(cfg);
    }
    builder.build().expect("agent builder")
}

#[test]
fn try_apply_model_switch_noop_when_identical_to_current() {
    let mut agent = build_test_agent("openai", "gpt-4o-mini", None);
    let result = agent.try_apply_model_switch(
        "gpt-4o-mini",
        "openai".to_string(),
        "gpt-4o-mini".to_string(),
    );
    assert_eq!(result, None, "same-provider/same-model is a no-op");
}

#[test]
fn try_apply_model_switch_preserves_agent_without_switch_config() {
    // Agent has NO provider_switch_config — cannot rebuild provider.
    let mut agent = build_test_agent("openai", "gpt-4o-mini", None);
    let result = agent.try_apply_model_switch(
        "gpt-4o-mini",
        "anthropic".to_string(),
        "claude-haiku".to_string(),
    );

    // Returns None (failed switch) and leaves the agent unchanged.
    assert_eq!(result, None);
    assert_eq!(
        agent.model_provider_name, "openai",
        "provider_name must NOT change when provider rebuild is not possible"
    );
    assert_eq!(
        agent.model_name, "gpt-4o-mini",
        "model_name must NOT change when provider rebuild is not possible"
    );
}

#[test]
fn try_apply_model_switch_succeeds_with_switch_config() {
    let switch_cfg = ProviderSwitchConfig {
        config: Some(std::sync::Arc::new(
            zeroclaw_config::schema::Config::default(),
        )),
    };

    let mut agent = build_test_agent("openai", "gpt-4o-mini", Some(switch_cfg));
    let result =
        agent.try_apply_model_switch("gpt-4o-mini", "ollama".to_string(), "llama3".to_string());

    assert_eq!(
        result.as_deref(),
        Some("llama3"),
        "successful switch must return the new effective model"
    );
    assert_eq!(
        agent.model_provider_name, "ollama",
        "provider_name must reflect the switched provider after success"
    );
    assert_eq!(
        agent.model_name, "llama3",
        "model_name must reflect the switched model after success"
    );
}

#[test]
fn try_apply_model_switch_succeeds_on_provider_only_change() {
    let switch_cfg = ProviderSwitchConfig {
        config: Some(std::sync::Arc::new(
            zeroclaw_config::schema::Config::default(),
        )),
    };

    let mut agent = build_test_agent("openai", "shared-name", Some(switch_cfg));
    let result = agent.try_apply_model_switch(
        "shared-name",
        "ollama".to_string(),
        "shared-name".to_string(),
    );

    assert_eq!(
        result.as_deref(),
        Some("shared-name"),
        "provider-only switch must also be treated as a successful switch"
    );
    assert_eq!(
        agent.model_provider_name, "ollama",
        "provider_name must update on a provider-only switch"
    );
    assert_eq!(agent.model_name, "shared-name");
}

#[test]
fn try_apply_model_switch_prefers_route_api_key() {
    let route = zeroclaw_config::schema::ModelRouteConfig {
        model_provider: "ollama".to_string(),
        model: "tinyllama".to_string(),
        hint: "fast".to_string(),
        api_key: Some("route-specific-key".to_string()),
    };

    let route_config = zeroclaw_config::schema::Config {
        model_routes: vec![route],
        ..zeroclaw_config::schema::Config::default()
    };
    let switch_cfg = ProviderSwitchConfig {
        config: Some(std::sync::Arc::new(route_config)),
    };

    let mut agent = build_test_agent("openai", "gpt-4o-mini", Some(switch_cfg));
    let result =
        agent.try_apply_model_switch("gpt-4o-mini", "ollama".to_string(), "tinyllama".to_string());

    assert_eq!(
        result.as_deref(),
        Some("tinyllama"),
        "switch must succeed when a model_routes entry matches the target"
    );
    assert_eq!(agent.model_provider_name, "ollama");
}

/// Streamed mock whose first call emits a tool call (queuing a model
/// switch via `ModelSwitchTriggerTool`) and whose later calls emit final
/// text. `call_count` lets the test prove the original provider is used
/// for exactly the first call — the next call goes to the switched one.
struct StreamSwitchTriggerProvider {
    call_count: Arc<Mutex<usize>>,
}

#[async_trait]
impl ModelProvider for StreamSwitchTriggerProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<String> {
        Ok("ok".into())
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
    ) -> Result<zeroclaw_providers::ChatResponse> {
        // The unified loop drives the streaming wrapper through `chat`
        // (stream events are synthesized post-hoc), so the tool call that
        // queues the switch is emitted here on the first call.
        let mut count = self.call_count.lock();
        *count += 1;
        if *count == 1 {
            Ok(zeroclaw_providers::ChatResponse {
                text: Some(String::new()),
                tool_calls: vec![zeroclaw_providers::ToolCall {
                    id: "00000000-0000-0000-0000-000000000002".into(),
                    name: "model_switch_trigger".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                }],
                usage: None,
                reasoning_content: None,
            })
        } else {
            // Should not be reached: after the switch, the next call goes
            // to the switched provider, not this one.
            Ok(zeroclaw_providers::ChatResponse {
                text: Some("original-provider-should-not-be-reused".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            })
        }
    }

    fn supports_native_tools(&self) -> bool {
        true
    }

    fn stream_chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: Option<f64>,
        _options: zeroclaw_providers::traits::StreamOptions,
    ) -> futures_util::stream::BoxStream<
        'static,
        zeroclaw_providers::traits::StreamResult<zeroclaw_providers::traits::StreamEvent>,
    > {
        use futures_util::stream::{self, StreamExt};
        let mut count = self.call_count.lock();
        *count += 1;
        if *count == 1 {
            // First call: ask to run the tool that queues a model switch.
            let tc =
                zeroclaw_providers::traits::StreamEvent::ToolCall(zeroclaw_providers::ToolCall {
                    id: "00000000-0000-0000-0000-000000000002".into(),
                    name: "model_switch_trigger".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                });
            stream::iter(vec![
                Ok(tc),
                Ok(zeroclaw_providers::traits::StreamEvent::Final),
            ])
            .boxed()
        } else {
            // Should not be reached: after the switch, the next call goes
            // to the switched provider, not this one.
            let chunk = zeroclaw_providers::traits::StreamEvent::TextDelta(
                zeroclaw_providers::traits::StreamChunk {
                    delta: "original-provider-should-not-be-reused".into(),
                    is_final: false,
                    reasoning: None,
                    token_count: 0,
                },
            );
            stream::iter(vec![
                Ok(chunk),
                Ok(zeroclaw_providers::traits::StreamEvent::Final),
            ])
            .boxed()
        }
    }
}

impl ::zeroclaw_api::attribution::Attributable for StreamSwitchTriggerProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::Custom,
            ),
        )
    }
    fn alias(&self) -> &str {
        "StreamSwitchTriggerProvider"
    }
}

/// Test tool that queues a pending `model_switch` when executed, standing
/// in for the real `model_switch` tool during a streamed turn.
struct ModelSwitchTriggerTool {
    target_provider: String,
    target_model: String,
}

#[async_trait]
impl Tool for ModelSwitchTriggerTool {
    fn name(&self) -> &str {
        "model_switch_trigger"
    }
    fn description(&self) -> &str {
        "test tool: queues a pending model switch"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn execute(&self, _args: serde_json::Value) -> Result<crate::tools::ToolResult> {
        let state = crate::agent::turn::current_model_switch_state()?;
        *state.lock().unwrap() = Some((self.target_provider.clone(), self.target_model.clone()));
        Ok(crate::tools::ToolResult {
            success: true,
            output: "model switch queued".into(),
            error: None,
        })
    }
}

#[test]
fn turn_streamed_applies_pending_model_switch_for_next_call() {
    let initial_calls = Arc::new(Mutex::new(0usize));
    let provider = Box::new(StreamSwitchTriggerProvider {
        call_count: Arc::clone(&initial_calls),
    });

    let memory_cfg = zeroclaw_config::schema::MemoryConfig {
        backend: "none".into(),
        ..zeroclaw_config::schema::MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
            .expect("memory creation"),
    );
    let capturing = Arc::new(CapturingObserver::default());
    let observer: Arc<dyn Observer> = capturing.clone();

    let switch_cfg = ProviderSwitchConfig {
        config: Some(std::sync::Arc::new(zeroclaw_config::schema::Config {
            reliability: zeroclaw_config::schema::ReliabilityConfig {
                provider_retries: 0,
                provider_backoff_ms: 0,
                ..zeroclaw_config::schema::ReliabilityConfig::default()
            },
            ..zeroclaw_config::schema::Config::default()
        })),
    };

    let mut agent = Agent::builder()
        .model_provider(provider)
        .tools(vec![Box::new(ModelSwitchTriggerTool {
            target_provider: "ollama".to_string(),
            target_model: "llama3".to_string(),
        })])
        .memory(mem)
        .observer(observer)
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .workspace_dir(std::path::PathBuf::from("/tmp"))
        .model_provider_name("openai".to_string())
        .model_name("gpt-4o-mini".to_string())
        .provider_switch_config(switch_cfg)
        .build()
        .expect("agent builder");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async {
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);
        // The turn ultimately errors because the switched provider has no
        // live server; the timeout only guards against an unexpected hang.
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            agent.turn_streamed("please switch the model", event_tx, None),
        )
        .await;
    });

    // `turn_streamed` itself must have consumed the pending switch and
    // committed the rebuilt provider/model via `ProviderSwitchConfig`.
    assert_eq!(
        agent.model_provider_name, "ollama",
        "turn_streamed must commit the switched provider after the tool result"
    );
    assert_eq!(
        agent.model_name, "llama3",
        "turn_streamed must commit the switched model after the tool result"
    );

    // The original provider is used for exactly the first call; the next
    // call in the same turn goes to the switched provider instead.
    assert_eq!(
        *initial_calls.lock(),
        1,
        "the original provider must serve only the first call — the next \
             call must use the switched provider, not the original"
    );

    // The next provider call in the same streamed turn targets the
    // switched provider/model: the `LlmRequest` event is recorded at the
    // top of the post-switch iteration, immediately before that call.
    let events = capturing.events.lock();
    let switched_request = events.iter().any(|e| {
        matches!(
            e,
            ObserverEvent::LlmRequest { model_provider, model, .. }
                if model_provider == "ollama" && model == "llama3"
        )
    });
    assert!(
        switched_request,
        "turn_streamed must issue the next provider call against the switched \
             provider/model (ollama/llama3); captured events: {events:?}"
    );
    drop(events);
}
