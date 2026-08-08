//! Shared turn execution. Single source of truth for spawn-drain-cancel.

use crate::agent::agent::{Agent, StreamedTurnError, StreamedTurnSuccess, TurnEvent};
use crate::agent::cost::{TOOL_LOOP_COST_TRACKING_CONTEXT, ToolLoopCostTrackingContext};
use crate::agent::loop_::is_tool_loop_cancelled;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;
use zeroclaw_api::model_provider::ConversationMessage;

pub enum TurnOutcome {
    Completed {
        text: String,
        messages: Vec<ConversationMessage>,
    },
    Cancelled {
        partial_text: String,
        messages: Vec<ConversationMessage>,
    },
}

#[derive(Debug)]
pub enum TurnError {
    Panicked(String),
    AgentError(String),
}

impl std::fmt::Display for TurnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Panicked(msg) => write!(f, "Turn task panicked: {msg}"),
            Self::AgentError(msg) => write!(f, "Agent turn failed: {msg}"),
        }
    }
}

impl std::error::Error for TurnError {}

/// Attribution fields attached to the tracing span for the duration of a turn.
/// All fields appear on every `record!()` emitted inside the turn.
///
/// `session_key` is the bare session identifier fed to the tracing span
/// and the `scope_session_key` task-local; in Chat mode the persistence
/// layer derives the `rpc_<sid>` storage key from it downstream.
/// `conversation_id` is the caller-owned cross-turn telemetry identity
/// stamped onto the agent so every attributed observer event carries it.
/// The two are semantically independent even when they happen to carry
/// the same bare `sid` - the `rpc_`-prefixed storage key must NEVER be
/// stored in `conversation_id`.
#[derive(Clone, Default)]
pub struct TurnAttribution {
    pub session_key: Option<String>,
    /// Caller-owned cross-turn conversation id. Stamped onto the agent in
    /// [`execute_turn`] so the streamed turn's `AgentTurnGuard` / `ToolLoop`
    /// attribute every observer event with it. `None` for non-RPC or
    /// test paths.
    pub conversation_id: Option<String>,
    pub agent_alias: String,
    pub model_provider: String,
    pub model: String,
    pub channel: &'static str,
}

pub async fn execute_turn<F, Fut>(
    agent: Arc<Mutex<Agent>>,
    prompt: String,
    cancel: CancellationToken,
    attribution: TurnAttribution,
    cost_context: Option<ToolLoopCostTrackingContext>,
    on_event: F,
) -> Result<TurnOutcome, TurnError>
where
    F: Fn(TurnEvent) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    let (event_tx, mut event_rx) = mpsc::channel::<TurnEvent>(64);
    let cancel_clone = cancel.clone();
    let session_key = attribution.session_key.clone();

    let mut turn_handle = zeroclaw_spawn::spawn!(async move {
        let mut guard = agent.lock().await;
        // Stamp the caller-owned conversation id onto the agent before the
        // streamed turn starts; the agent threads it through its
        // `AgentTurnGuard` / `ToolLoop` / `TurnCtx` / `TurnMeta` so every
        // attributed observer event carries it. `execute_turn` is the single
        // unified entry for ordinary prompt turns, resume, and reaper
        // rehydrate - all reach here via `handle_session_prompt` - so the id
        // does not need to be copied into `RpcSession`. `conversation_id` is
        // distinct from `session_key` (the history/storage key) and must never
        // carry the `rpc_`-prefixed form.
        guard.set_conversation_id(attribution.conversation_id.clone());
        let sk = attribution.session_key.clone();
        crate::agent::loop_::scope_session_key(attribution.session_key, async move {
            use ::zeroclaw_log::Instrument as _;
            let span = ::zeroclaw_log::info_span!(
                target: "zeroclaw_log_internal_scope",
                "zeroclaw_scope",
                session_key = %sk.as_deref().unwrap_or(""),
                agent_alias = %attribution.agent_alias,
                model_provider = %attribution.model_provider,
                model = %attribution.model,
                channel = %attribution.channel,
            );
            TOOL_LOOP_COST_TRACKING_CONTEXT
                .scope(
                    cost_context,
                    guard
                        .turn_streamed_with_steering_state(
                            &prompt,
                            event_tx,
                            Some(cancel_clone),
                            None,
                        )
                        .instrument(span),
                )
                .await
        })
        .await
    });

    let mut accumulated_text = String::new();

    let drain =
        drain_until_done_or_cancelled(&mut event_rx, &cancel, &mut accumulated_text, &on_event)
            .await;
    let _ = session_key; // consumed above

    match drain {
        DrainOutcome::Completed => {
            let joined = turn_handle
                .await
                .map_err(|e| TurnError::Panicked(format!("{e}")))?;
            outcome_from_task_result(joined, accumulated_text)
        }
        DrainOutcome::ExplicitCancel => {
            match tokio::time::timeout(CANCEL_GRACE, &mut turn_handle).await {
                Ok(joined) => outcome_from_task_result(
                    joined.map_err(|e| TurnError::Panicked(format!("cancelled turn join: {e}")))?,
                    accumulated_text,
                ),
                Err(_) => {
                    turn_handle.abort();
                    Ok(TurnOutcome::Cancelled {
                        partial_text: accumulated_text,
                        messages: Vec::new(),
                    })
                }
            }
        }
    }
}

/// Grace window allowing a cancelled turn task to commit its cooperative
/// unwind (synthesized tool results + `[interrupted]` message) into the agent
/// history before the dispatch path falls back to a hard abort.
const CANCEL_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Map a finished turn task into a [`TurnOutcome`]. A successful turn yields
/// `Completed`; a cooperative cancel yields `Cancelled` carrying the messages
/// the task committed so persistence never depends on the abort/commit race.
fn outcome_from_task_result(
    joined: Result<StreamedTurnSuccess, StreamedTurnError>,
    accumulated_text: String,
) -> Result<TurnOutcome, TurnError> {
    match joined {
        Ok(StreamedTurnSuccess {
            response,
            new_messages,
        }) => Ok(TurnOutcome::Completed {
            text: response,
            messages: new_messages,
        }),
        Err(StreamedTurnError {
            error,
            committed_response,
            new_messages,
        }) if is_tool_loop_cancelled(&error) => Ok(TurnOutcome::Cancelled {
            partial_text: if committed_response.is_empty() {
                accumulated_text
            } else {
                committed_response
            },
            messages: new_messages,
        }),
        Err(StreamedTurnError { error, .. }) => Err(TurnError::AgentError(format!("{error}"))),
    }
}

/// Why [`drain_until_done_or_cancelled`] returned. `ExplicitCancel` is an
/// outside fire (client RPC, reaper, session removal) that reached the drain.
/// There is no self-firing idle exit: a live turn falls silent for the whole
/// duration of a tool call, so silence is never treated as a stall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrainOutcome {
    Completed,
    ExplicitCancel,
}

async fn drain_until_done_or_cancelled<F, Fut>(
    event_rx: &mut mpsc::Receiver<TurnEvent>,
    cancel: &CancellationToken,
    accumulated: &mut String,
    on_event: &F,
) -> DrainOutcome
where
    F: Fn(TurnEvent) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    loop {
        if cancel.is_cancelled() {
            return DrainOutcome::ExplicitCancel;
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return DrainOutcome::ExplicitCancel,
            maybe_event = event_rx.recv() => {
                match maybe_event {
                    Some(event) => {
                        if let TurnEvent::Chunk { ref delta } = event {
                            accumulated.push_str(delta);
                        }
                        on_event(event).await;
                    }
                    None => return DrainOutcome::Completed,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn noop(_e: TurnEvent) -> std::future::Ready<()> {
        std::future::ready(())
    }

    #[tokio::test]
    async fn drain_must_not_idle_cancel_a_live_turn_across_a_long_tool_gap() {
        let (tx, mut rx) = mpsc::channel::<TurnEvent>(8);
        let cancel = CancellationToken::new();
        let mut acc = String::new();

        let sender = zeroclaw_spawn::spawn!(async move {
            let _ = tx
                .send(TurnEvent::ToolCall {
                    id: "c1".to_string(),
                    name: "shell".to_string(),
                    args: serde_json::json!({ "command": "cargo test" }),
                })
                .await;
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let _ = tx
                .send(TurnEvent::ToolResult {
                    id: "c1".to_string(),
                    name: "shell".to_string(),
                    output: "ok".to_string(),
                    artifact: None,
                })
                .await;
            let _ = tx
                .send(TurnEvent::Chunk {
                    delta: "done".to_string(),
                })
                .await;
        });

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            drain_until_done_or_cancelled(&mut rx, &cancel, &mut acc, &noop),
        )
        .await
        .expect("drain must terminate when the live turn task completes");

        sender.await.unwrap();
        assert_eq!(
            outcome,
            DrainOutcome::Completed,
            "a turn whose sender is alive but quiet during a long tool \
             execution is NOT stalled; silence during execute_tools is the \
             normal case. Killing it is the idle_stall regression that froze \
             the TUI mid-turn (sessions 102, 103)."
        );
        assert!(
            !cancel.is_cancelled(),
            "drain self-cancelled a healthy turn across a tool gap; the token \
             must stay clean so downstream records no cancel."
        );
        assert_eq!(
            acc, "done",
            "drain dropped the post-tool chunk after wrongly tripping an idle \
             bound mid-execution."
        );
    }

    #[tokio::test]
    async fn drain_must_still_accumulate_chunks_when_events_arrive_steadily() {
        let (tx, mut rx) = mpsc::channel::<TurnEvent>(8);
        let cancel = CancellationToken::new();
        let mut acc = String::new();

        let sender = zeroclaw_spawn::spawn!(async move {
            for delta in ["he", "llo", " ", "world"] {
                let _ = tx
                    .send(TurnEvent::Chunk {
                        delta: delta.to_string(),
                    })
                    .await;
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        });

        let cancelled = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            drain_until_done_or_cancelled(&mut rx, &cancel, &mut acc, &noop),
        )
        .await
        .expect("drain must terminate after the sender drops");

        sender.await.unwrap();
        assert_eq!(
            cancelled,
            DrainOutcome::Completed,
            "channel closure is not a cancel; drain returned the wrong verdict"
        );
        assert_eq!(
            acc, "hello world",
            "drain dropped chunks instead of accumulating them; a fix that \
             short-circuits with too-aggressive an idle window (e.g. <250ms) \
             would corrupt legitimate streaming turns. The production idle \
             window must sit comfortably between the inter-chunk gap of a \
             healthy stream (~hundreds of ms) and the user-perceptible hang \
             threshold (~seconds)."
        );
    }

    #[test]
    fn cancel_outcome_carries_committed_messages_not_just_partial_text() {
        let msgs = vec![ConversationMessage::Chat(
            zeroclaw_providers::ChatMessage::assistant("[interrupted by user]"),
        )];
        let err = StreamedTurnError {
            error: crate::agent::loop_::ToolLoopCancelled.into(),
            committed_response: "partial".to_string(),
            new_messages: msgs.clone(),
        };

        let outcome = outcome_from_task_result(Err(err), "accumulated".to_string())
            .expect("cooperative cancel maps to a Cancelled outcome, not an error");

        match outcome {
            TurnOutcome::Cancelled {
                partial_text,
                messages,
            } => {
                assert_eq!(
                    partial_text, "partial",
                    "committed_response from the task must win over the drain's \
                     accumulated text when present"
                );
                assert_eq!(
                    messages.len(),
                    msgs.len(),
                    "cancelled outcome dropped the messages the task committed"
                );
            }
            TurnOutcome::Completed { .. } => {
                panic!("a tool-loop cancel must not map to Completed")
            }
        }
    }

    #[test]
    fn non_cancel_agent_error_stays_an_error() {
        let err = StreamedTurnError {
            error: anyhow::Error::msg("provider exploded"),
            committed_response: String::new(),
            new_messages: Vec::new(),
        };
        let outcome = outcome_from_task_result(Err(err), String::new());
        assert!(
            matches!(outcome, Err(TurnError::AgentError(_))),
            "a genuine agent failure must surface as an error, not a silent \
             cancel"
        );
    }

    #[tokio::test]
    async fn execute_turn_scopes_cost_context_so_usage_is_persisted() {
        use crate::agent::agent::Agent;
        use crate::agent::dispatcher::NativeToolDispatcher;
        use crate::cost::CostTracker;
        use crate::observability::{NoopObserver, Observer};
        use async_trait::async_trait;
        use std::collections::HashMap;
        use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
        use zeroclaw_api::model_provider::ModelProvider;
        use zeroclaw_memory::Memory;
        use zeroclaw_providers::ChatRequest;

        // Minimal provider that returns a final answer carrying non-zero token
        // usage on the non-streaming `chat` path (the default the engine takes
        // when the provider does not advertise streaming).
        struct UsageProvider;

        #[async_trait]
        impl ModelProvider for UsageProvider {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<String> {
                Ok("ok".into())
            }

            async fn chat(
                &self,
                _request: ChatRequest<'_>,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<zeroclaw_providers::ChatResponse> {
                Ok(zeroclaw_providers::ChatResponse {
                    text: Some("done".into()),
                    tool_calls: vec![],
                    usage: Some(zeroclaw_providers::traits::TokenUsage {
                        input_tokens: Some(1_000),
                        cached_input_tokens: None,
                        output_tokens: Some(200),
                    }),
                    reasoning_content: None,
                })
            }
        }

        impl Attributable for UsageProvider {
            fn role(&self) -> Role {
                Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
            }
            fn alias(&self) -> &str {
                "mock-provider"
            }
        }

        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed"),
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
            .with_agent_alias("rpc-agent");

        let agent = Agent::builder()
            .model_provider(Box::new(UsageProvider))
            .tools(vec![])
            .memory(mem)
            .observer(Arc::from(NoopObserver {}) as Arc<dyn Observer>)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .model_name("test-model".into())
            .model_provider_name("mock-provider".into())
            .agent_alias("rpc-agent".into())
            .build()
            .expect("agent builder should succeed");

        let outcome = execute_turn(
            Arc::new(Mutex::new(agent)),
            "hello".to_string(),
            CancellationToken::new(),
            TurnAttribution {
                session_key: Some("s1".into()),
                conversation_id: None,
                agent_alias: "rpc-agent".into(),
                model_provider: "mock-provider".into(),
                model: "test-model".into(),
                channel: "rpc",
            },
            Some(cost_context),
            noop,
        )
        .await
        .expect("turn should complete");
        assert!(
            matches!(outcome, TurnOutcome::Completed { .. }),
            "turn should complete normally"
        );

        let summary = tracker.get_summary().expect("cost summary");
        assert_eq!(
            summary.request_count, 1,
            "execute_turn must scope the cost context so the turn's usage is \
             persisted (#5221)"
        );
        assert_eq!(summary.total_tokens, 1_200);
        let agent_summary = tracker
            .get_summary_for_agent("rpc-agent")
            .expect("agent-scoped summary");
        assert_eq!(
            agent_summary.request_count, 1,
            "the agent alias must flow through to the persisted cost record"
        );
    }

    /// Captures every `ObserverEvent` so the `conversation_id` stamped onto the
    /// agent by `execute_turn` can be asserted against the attributed events a
    /// real turn emits.
    #[derive(Default)]
    struct CapturingObserver {
        events: parking_lot::Mutex<Vec<crate::observability::ObserverEvent>>,
    }

    impl crate::observability::Observer for CapturingObserver {
        fn record_event(&self, event: &crate::observability::ObserverEvent) {
            self.events.lock().push(event.clone());
        }
        fn record_metric(&self, _metric: &zeroclaw_api::observability_traits::ObserverMetric) {}
        fn name(&self) -> &str {
            "rpc-capturing"
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn flush(&self) {}
    }

    /// The `conversation_id` carried by an attributed observer event, wrapped
    /// in a double `Option`: the outer `None` marks events without the field
    /// (so `filter_map` drops them), the inner `Option` is the event's own
    /// `conversation_id` (`None` when the field is present but unset).
    fn attributed_conversation_id(
        event: &crate::observability::ObserverEvent,
    ) -> Option<Option<&str>> {
        match event {
            crate::observability::ObserverEvent::AgentStart {
                conversation_id, ..
            }
            | crate::observability::ObserverEvent::AgentEnd {
                conversation_id, ..
            }
            | crate::observability::ObserverEvent::LlmRequest {
                conversation_id, ..
            }
            | crate::observability::ObserverEvent::LlmResponse {
                conversation_id, ..
            }
            | crate::observability::ObserverEvent::ToolCallStart {
                conversation_id, ..
            }
            | crate::observability::ObserverEvent::ToolCall {
                conversation_id, ..
            }
            | crate::observability::ObserverEvent::MemoryRecall {
                conversation_id, ..
            }
            | crate::observability::ObserverEvent::MemoryStore {
                conversation_id, ..
            }
            | crate::observability::ObserverEvent::RagRetrieve {
                conversation_id, ..
            } => Some(conversation_id.as_deref()),
            _ => None,
        }
    }

    /// A model provider that returns one canned final answer per `chat` call,
    /// so RPC turn tests can drive real `execute_turn` turns without a live
    /// model. The non-streaming `chat` path is the default the engine takes
    /// when the provider does not advertise streaming.
    struct CannedProvider;

    #[async_trait::async_trait]
    impl zeroclaw_api::model_provider::ModelProvider for CannedProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".into())
        }
        async fn chat(
            &self,
            _request: zeroclaw_providers::ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<zeroclaw_providers::ChatResponse> {
            Ok(zeroclaw_providers::ChatResponse {
                text: Some("done".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            })
        }
    }

    impl zeroclaw_api::attribution::Attributable for CannedProvider {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Provider(
                zeroclaw_api::attribution::ProviderKind::Model(
                    zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "mock-provider"
        }
    }

    /// Build an agent wired to `observer` whose model provider hands back a
    /// canned final answer, wrapped in the `Arc<Mutex<Agent>>` shape that
    /// `execute_turn` consumes.
    fn rpc_turn_agent(
        observer: Arc<dyn crate::observability::Observer>,
    ) -> Arc<tokio::sync::Mutex<Agent>> {
        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn zeroclaw_memory::Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed"),
        );
        let agent = Agent::builder()
            .model_provider(Box::new(CannedProvider))
            .tools(vec![])
            .memory(mem)
            .observer(observer)
            .tool_dispatcher(Box::new(crate::agent::dispatcher::NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .model_name("test-model".into())
            .model_provider_name("mock-provider".into())
            .agent_alias("rpc-agent".into())
            .build()
            .expect("agent builder should succeed");
        Arc::new(tokio::sync::Mutex::new(agent))
    }

    /// To prove the stamp uses `conversation_id` (not `session_key`), this
    /// test passes them as distinct values - modeling the `rpc_`-prefixed
    /// storage key in `session_key` to assert it never leaks into the
    /// conversation slot. In production both fields carry the same bare sid;
    /// the `rpc_` prefix is applied downstream in the persistence layer.
    #[tokio::test]
    async fn rpc_reuses_protocol_session_id_stamps_bare_sid_not_rpc_prefixed_key() {
        let capturing = Arc::new(CapturingObserver::default());
        let agent =
            rpc_turn_agent(Arc::clone(&capturing) as Arc<dyn crate::observability::Observer>);

        let outcome = execute_turn(
            agent.clone(),
            "hello".to_string(),
            CancellationToken::new(),
            TurnAttribution {
                // The history/storage key carries the `rpc_` prefix; this is the
                // value that must NOT surface as the conversation id.
                session_key: Some("rpc_abc-123".into()),
                conversation_id: Some("abc-123".into()),
                agent_alias: "rpc-agent".into(),
                model_provider: "mock-provider".into(),
                model: "test-model".into(),
                channel: "rpc",
            },
            None,
            noop,
        )
        .await
        .expect("turn should complete");
        assert!(
            matches!(outcome, TurnOutcome::Completed { .. }),
            "turn should complete normally"
        );

        // The agent is stamped with the BARE protocol sid, never the
        // `rpc_`-prefixed history key.
        let stamped = agent.lock().await.conversation_id().map(str::to_string);
        assert_eq!(
            stamped.as_deref(),
            Some("abc-123"),
            "execute_turn must stamp the bare conversation id, not the rpc_-prefixed session_key"
        );
        assert!(
            !stamped.as_deref().unwrap_or("").starts_with("rpc_"),
            "the rpc_-prefixed history key leaked into the conversation slot: {stamped:?}"
        );

        // Every attributed observer event carries the bare id; none carry the
        // `rpc_`-prefixed form.
        let events = capturing.events.lock();
        let conv_ids: Vec<Option<&str>> = events
            .iter()
            .filter_map(attributed_conversation_id)
            .collect();
        assert!(
            !conv_ids.is_empty(),
            "execute_turn should emit attributed observer events"
        );
        assert!(
            conv_ids.iter().all(|id| *id == Some("abc-123")),
            "every attributed event must carry the bare sid abc-123, got {conv_ids:?}"
        );
        assert!(
            !conv_ids
                .iter()
                .any(|id| id.map(|s| s.starts_with("rpc_")).unwrap_or(false)),
            "a rpc_-prefixed history key leaked onto an observer event: {conv_ids:?}"
        );
    }

    /// Two turns on the same RPC session reuse the same protocol `session_id`,
    /// so the conversation id stamped by `execute_turn` must be identical across
    /// both turns - the cross-turn telemetry grouping the id provides.
    #[tokio::test]
    async fn rpc_reuses_protocol_session_id_two_turns_share_same_id() {
        let capturing = Arc::new(CapturingObserver::default());
        let agent =
            rpc_turn_agent(Arc::clone(&capturing) as Arc<dyn crate::observability::Observer>);

        for prompt in ["first", "second"] {
            let outcome = execute_turn(
                agent.clone(),
                prompt.to_string(),
                CancellationToken::new(),
                TurnAttribution {
                    // Both turns carry the same bare sid; `session_key` keeps
                    // the `rpc_` prefix to model the real Chat split.
                    session_key: Some("rpc_shared-sid".into()),
                    conversation_id: Some("shared-sid".into()),
                    agent_alias: "rpc-agent".into(),
                    model_provider: "mock-provider".into(),
                    model: "test-model".into(),
                    channel: "rpc",
                },
                None,
                noop,
            )
            .await
            .expect("turn should complete");
            assert!(
                matches!(outcome, TurnOutcome::Completed { .. }),
                "each turn should complete normally"
            );
        }

        let stamped = agent.lock().await.conversation_id().map(str::to_string);
        assert_eq!(
            stamped.as_deref(),
            Some("shared-sid"),
            "the agent must retain the conversation id across turns"
        );

        let events = capturing.events.lock();
        let conv_ids: Vec<Option<&str>> = events
            .iter()
            .filter_map(attributed_conversation_id)
            .collect();
        assert!(
            conv_ids.iter().all(|id| *id == Some("shared-sid")),
            "both turns' events must carry the shared conversation id, got {conv_ids:?}"
        );
    }

    /// A fresh `session/new` mints a fresh protocol `session_id`. Two separate
    /// RPC sessions (distinct agents) must therefore carry distinct conversation
    /// ids, and each session's observer events carry only its own id - never the
    /// other session's.
    #[tokio::test]
    async fn rpc_reuses_protocol_session_id_fresh_session_yields_distinct_id() {
        let cap_a = Arc::new(CapturingObserver::default());
        let cap_b = Arc::new(CapturingObserver::default());
        let agent_a = rpc_turn_agent(Arc::clone(&cap_a) as Arc<dyn crate::observability::Observer>);
        let agent_b = rpc_turn_agent(Arc::clone(&cap_b) as Arc<dyn crate::observability::Observer>);

        for (agent, sid) in [(agent_a.clone(), "sid-a"), (agent_b.clone(), "sid-b")] {
            let outcome = execute_turn(
                agent.clone(),
                "hello".to_string(),
                CancellationToken::new(),
                TurnAttribution {
                    session_key: Some(format!("rpc_{sid}")),
                    conversation_id: Some(sid.to_string()),
                    agent_alias: "rpc-agent".into(),
                    model_provider: "mock-provider".into(),
                    model: "test-model".into(),
                    channel: "rpc",
                },
                None,
                noop,
            )
            .await
            .expect("turn should complete");
            assert!(
                matches!(outcome, TurnOutcome::Completed { .. }),
                "turn should complete normally"
            );
        }

        let stamped_a = agent_a.lock().await.conversation_id().map(str::to_string);
        let stamped_b = agent_b.lock().await.conversation_id().map(str::to_string);
        assert_eq!(
            stamped_a.as_deref(),
            Some("sid-a"),
            "session A is stamped with its own sid"
        );
        assert_eq!(
            stamped_b.as_deref(),
            Some("sid-b"),
            "session B is stamped with its own, distinct sid"
        );
        assert_ne!(
            stamped_a, stamped_b,
            "fresh sessions must carry distinct conversation ids"
        );

        // Each session's events carry only its own id.
        for (cap, expected) in [(cap_a.clone(), "sid-a"), (cap_b.clone(), "sid-b")] {
            let events = cap.events.lock();
            let conv_ids: Vec<Option<&str>> = events
                .iter()
                .filter_map(attributed_conversation_id)
                .collect();
            assert!(
                !conv_ids.is_empty(),
                "session {expected} should emit attributed events"
            );
            assert!(
                conv_ids.iter().all(|id| *id == Some(expected)),
                "session {expected} events must carry only {expected}, got {conv_ids:?}"
            );
            let other = if expected == "sid-a" {
                "sid-b"
            } else {
                "sid-a"
            };
            assert!(
                !conv_ids.contains(&Some(other)),
                "session {expected} leaked the other session's id {other}: {conv_ids:?}"
            );
        }
    }
}
