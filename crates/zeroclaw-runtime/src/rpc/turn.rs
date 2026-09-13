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
    TerminalCompletion {
        diagnostic: String,
        user_message: String,
    },
}

impl std::fmt::Display for TurnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Panicked(msg) => write!(f, "Turn task panicked: {msg}"),
            Self::AgentError(msg) => write!(f, "Agent turn failed: {msg}"),
            Self::TerminalCompletion { diagnostic, .. } => {
                write!(f, "Agent turn failed: {diagnostic}")
            }
        }
    }
}

impl std::error::Error for TurnError {}

impl TurnError {
    /// Localized text is carried only for a client-delivery boundary. Display
    /// remains the stable diagnostic form used by logs and durable audit rows.
    pub fn user_message(&self) -> Option<&str> {
        match self {
            Self::TerminalCompletion { user_message, .. } => Some(user_message),
            Self::Panicked(_) | Self::AgentError(_) => None,
        }
    }
}

/// Attribution fields attached to the tracing span for the duration of a turn.
/// All fields appear on every `record!()` emitted inside the turn.
#[derive(Clone, Default)]
pub struct TurnAttribution {
    pub session_key: Option<String>,
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
    connection_activity: Option<crate::rpc::ConnectionActivity>,
    on_event: F,
) -> Result<TurnOutcome, TurnError>
where
    F: Fn(TurnEvent) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    let (event_tx, mut event_rx) = mpsc::channel::<TurnEvent>(64);
    let cancel_clone = cancel.clone();
    let session_key = attribution.session_key.clone();

    let turn_handle = zeroclaw_spawn::spawn!(async move {
        // Held inside the task body so the connection stays counted until this
        // task's future is actually dropped. An abort requested by the caller
        // only schedules that drop; provider and tool cleanup still runs after
        // it, and the reload drain must not read zero while it does.
        let _connection_activity = connection_activity;
        let mut guard = agent.lock().await;
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

    let mut turn_handle_guard = TurnHandleGuard(Some(turn_handle));

    let mut accumulated_text = String::new();

    let drain =
        drain_until_done_or_cancelled(&mut event_rx, &cancel, &mut accumulated_text, &on_event)
            .await;
    let _ = session_key; // consumed above

    match drain {
        DrainOutcome::Completed => {
            let joined = {
                let handle = turn_handle_guard.handle()?;
                handle
                    .await
                    .map_err(|e| TurnError::Panicked(format!("{e}")))?
            };
            outcome_from_task_result(joined, accumulated_text)
        }
        DrainOutcome::ExplicitCancel => {
            let graced = {
                let handle = turn_handle_guard.handle()?;
                tokio::time::timeout(CANCEL_GRACE, &mut *handle).await
            };
            match graced {
                Ok(joined) => outcome_from_task_result(
                    joined.map_err(|e| TurnError::Panicked(format!("cancelled turn join: {e}")))?,
                    accumulated_text,
                ),
                Err(_) => {
                    let handle = turn_handle_guard.handle()?;
                    handle.abort();
                    // Joined through the guard rather than a moved-out handle:
                    // if this future is dropped while the abort is still being
                    // processed, the guard aborts what it still owns instead of
                    // leaving a detached task behind.
                    let _ = handle.await;
                    Ok(TurnOutcome::Cancelled {
                        partial_text: accumulated_text,
                        messages: Vec::new(),
                    })
                }
            }
        }
    }
}

type TurnJoinHandle = tokio::task::JoinHandle<
    std::result::Result<
        crate::agent::agent::StreamedTurnSuccess,
        crate::agent::agent::StreamedTurnError,
    >,
>;

/// Owner of the spawned turn task for the whole lifetime of
/// [`execute_turn`].
///
/// The handle is never moved out. Awaiting a moved-out handle detaches the
/// turn task when the awaiting future is dropped, and a dropped prompt is
/// exactly what a forced listener teardown produces, so the task would keep
/// running with nobody holding it. Borrowing the handle out of the guard keeps
/// [`Drop`] able to abort it on every exit path.
struct TurnHandleGuard(Option<TurnJoinHandle>);

impl TurnHandleGuard {
    fn handle(&mut self) -> Result<&mut TurnJoinHandle, TurnError> {
        self.0.as_mut().ok_or_else(|| {
            TurnError::Panicked("turn task handle missing from its owner".to_string())
        })
    }
}

impl Drop for TurnHandleGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
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
        Err(StreamedTurnError { error, .. }) => {
            if let Some(user_message) =
                crate::agent::terminal_completion_error_message(&error, None)
            {
                return Err(TurnError::TerminalCompletion {
                    diagnostic: error.to_string(),
                    user_message,
                });
            }
            Err(TurnError::AgentError(error.to_string()))
        }
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

    #[test]
    fn terminal_completion_keeps_diagnostic_and_delivery_text_separate() {
        let expected = crate::agent::semantic_empty_terminal_completion_message(None);
        let err = StreamedTurnError {
            error: anyhow::Error::new(
                zeroclaw_api::model_provider::SemanticEmptyTerminalCompletion,
            ),
            committed_response: String::new(),
            new_messages: Vec::new(),
        };

        let outcome = match outcome_from_task_result(Err(err), String::new()) {
            Err(error) => error,
            Ok(_) => panic!("semantic-empty terminal completion must fail"),
        };
        assert_eq!(
            outcome.to_string(),
            "Agent turn failed: provider completed without final text or tool calls"
        );
        assert_eq!(outcome.user_message(), Some(expected.as_str()));
    }

    #[test]
    fn provider_terminal_completion_keeps_retry_diagnostic_out_of_rpc_delivery() {
        use zeroclaw_providers::{
            ReliableProviderTerminalFailure, ReliableProviderTerminalFailureKind,
        };

        let error = anyhow::Error::new(ReliableProviderTerminalFailure::new(
            ReliableProviderTerminalFailureKind::Connection,
            Some("http://localhost:11434/v1/chat/completions".to_string()),
            "All model providers/models failed after 3 failure event(s). Events: \
                 event 1 (retry 1/3): retryable"
                .to_string(),
        ));
        let expected = crate::agent::terminal_completion_error_message(&error, None)
            .expect("provider failures have a canonical localized projection");
        let err = StreamedTurnError {
            error,
            committed_response: String::new(),
            new_messages: Vec::new(),
        };

        let outcome = match outcome_from_task_result(Err(err), String::new()) {
            Err(error) => error,
            Ok(_) => panic!("provider terminal completion must fail"),
        };
        let user_message = outcome
            .user_message()
            .expect("terminal completion supplies a user message");
        assert!(
            outcome
                .to_string()
                .contains("All model providers/models failed")
        );
        assert_eq!(user_message, expected);
        assert!(user_message.contains("http://localhost:11434/v1/chat/completions"));
        assert!(!user_message.contains("retry 1/3"));
        assert!(!user_message.contains("All model providers/models failed"));
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
                        cache_creation_input_tokens: None,
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
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![],
            ))
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
                agent_alias: "rpc-agent".into(),
                model_provider: "mock-provider".into(),
                model: "test-model".into(),
                channel: "rpc",
            },
            Some(cost_context),
            None,
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

    /// A forced listener teardown drops the prompt future while it is joining
    /// the turn task. The turn task must stay owned across that drop, and the
    /// connection it belongs to must stay counted until the task's cleanup has
    /// actually returned.
    ///
    /// Without the owning guard the handle is moved out before the join, so the
    /// drop detaches the task: cleanup never starts and the count never falls.
    /// Requesting an abort is not enough either, which is why the assertion is
    /// on cleanup having ended rather than on the abort having been issued.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn dropped_prompt_keeps_the_turn_task_owned_until_its_cleanup_ends() {
        use crate::agent::agent::Agent;
        use crate::agent::dispatcher::NativeToolDispatcher;
        use crate::observability::{NoopObserver, Observer};
        use async_trait::async_trait;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
        use zeroclaw_api::model_provider::ModelProvider;
        use zeroclaw_memory::Memory;

        /// Finite cleanup that outlives the forced deadline: it starts when the
        /// provider future is dropped and returns only when the test releases
        /// it, the same shape as the listeners' `HoldOnDrop` fixture.
        struct HoldOnDrop {
            unwind_started: Arc<AtomicBool>,
            unwind_ended: Arc<AtomicBool>,
            release: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
        }

        impl Drop for HoldOnDrop {
            fn drop(&mut self) {
                self.unwind_started.store(true, Ordering::SeqCst);
                // Bounded so a failing assertion elsewhere cannot park this
                // worker thread for the life of the test binary.
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                let (lock, cvar) = &*self.release;
                let mut released = lock.lock().unwrap();
                while !*released {
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    if remaining.is_zero() {
                        return;
                    }
                    released = cvar.wait_timeout(released, remaining).unwrap().0;
                }
                self.unwind_ended.store(true, Ordering::SeqCst);
            }
        }

        struct HeldProvider {
            started: Arc<AtomicBool>,
            unwind_started: Arc<AtomicBool>,
            unwind_ended: Arc<AtomicBool>,
            release: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
        }

        #[async_trait]
        impl ModelProvider for HeldProvider {
            async fn chat_with_system(
                &self,
                _system_prompt: Option<&str>,
                _message: &str,
                _model: &str,
                _temperature: Option<f64>,
            ) -> anyhow::Result<String> {
                let _cleanup = HoldOnDrop {
                    unwind_started: Arc::clone(&self.unwind_started),
                    unwind_ended: Arc::clone(&self.unwind_ended),
                    release: Arc::clone(&self.release),
                };
                self.started.store(true, Ordering::SeqCst);
                std::future::pending::<()>().await;
                Ok("unreachable".to_string())
            }
        }

        impl Attributable for HeldProvider {
            fn role(&self) -> Role {
                Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
            }
            fn alias(&self) -> &str {
                "held-provider"
            }
        }

        async fn wait_for(label: &str, condition: impl Fn() -> bool) {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while !condition() {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("{label}"));
        }

        let started = Arc::new(AtomicBool::new(false));
        let unwind_started = Arc::new(AtomicBool::new(false));
        let unwind_ended = Arc::new(AtomicBool::new(false));
        let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));

        let memory_cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(&memory_cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed"),
        );
        let agent = Agent::builder()
            .model_provider(Box::new(HeldProvider {
                started: Arc::clone(&started),
                unwind_started: Arc::clone(&unwind_started),
                unwind_ended: Arc::clone(&unwind_ended),
                release: Arc::clone(&release),
            }))
            .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
                vec![],
            ))
            .memory(mem)
            .observer(Arc::from(NoopObserver {}) as Arc<dyn Observer>)
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .model_name("test-model".into())
            .model_provider_name("held-provider".into())
            .agent_alias("rpc-agent".into())
            .build()
            .expect("agent builder should succeed");

        let connections = Arc::new(AtomicUsize::new(0));
        let activity = crate::rpc::ConnectionActivity::new(Arc::clone(&connections));
        assert_eq!(
            connections.load(Ordering::Relaxed),
            1,
            "the accepted connection must be counted before any prompt runs"
        );

        let cancel = CancellationToken::new();
        let turn_cancel = cancel.clone();
        let prompt_task = zeroclaw_spawn::spawn!(async move {
            let _ = execute_turn(
                Arc::new(Mutex::new(agent)),
                "hold".to_string(),
                turn_cancel,
                TurnAttribution {
                    session_key: Some("forced-drop".into()),
                    agent_alias: "rpc-agent".into(),
                    model_provider: "held-provider".into(),
                    model: "test-model".into(),
                    channel: "rpc",
                },
                None,
                Some(activity),
                noop,
            )
            .await;
        });

        wait_for("the prompt must reach the provider", || {
            started.load(Ordering::SeqCst)
        })
        .await;

        // The connection generation ends, then the listener's forced deadline
        // drops the prompt while it is still joining the turn task.
        cancel.cancel();
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        prompt_task.abort();
        let _ = prompt_task.await;

        wait_for(
            "a dropped prompt must abort the turn task it owns, not detach it",
            || unwind_started.load(Ordering::SeqCst),
        )
        .await;
        assert!(
            !unwind_ended.load(Ordering::SeqCst),
            "the fixture must still be holding cleanup at this point"
        );
        assert_eq!(
            connections.load(Ordering::Relaxed),
            1,
            "the connection must stay counted while its turn task unwinds"
        );

        {
            let (lock, cvar) = &*release;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
        }

        wait_for(
            "the connection must be released once cleanup returns",
            || connections.load(Ordering::Relaxed) == 0,
        )
        .await;
        assert!(
            unwind_ended.load(Ordering::SeqCst),
            "the count may only reach zero after the turn task's cleanup ended"
        );
    }
}
