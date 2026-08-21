//! Transport-neutral gateway turn runner.
//!
//! Owns the turn lifecycle shared by the WebSocket and HTTP chat
//! transports: attribution, cost-tracking context, `agent_start` broadcast,
//! session-state transitions, cancellation-token registration, the streaming
//! event channel, cooperative timeout, and the exactly-once finalizer that
//! persists the conversation and updates session state. The transport supplies
//! a `forward` closure that consumes streaming events and returns usage
//! observations; the runner never talks to a socket or a wire format.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use crate::AppState;
use serde_json::json;
use zeroclaw_infra::session_backend::SessionBackend;
use zeroclaw_memory::consolidation::consolidate_turn;
use zeroclaw_providers::{ChatMessage, ConversationMessage};
use zeroclaw_runtime::agent::cost::{
    TOOL_LOOP_COST_TRACKING_CONTEXT, TOOL_LOOP_TURN_USAGE, ToolLoopCostTrackingContext, TurnUsage,
    build_model_provider_pricing,
};
use zeroclaw_runtime::agent::loop_::{is_tool_loop_cancelled, scope_session_key};
use zeroclaw_runtime::agent::{Agent, TurnEvent};

// ── Terminal-state types ────────────────────────────────────────────────────

/// Terminal state of a gateway turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnStatus {
    /// The agent returned a full response.
    Success,
    /// The turn was cancelled (client disconnect, abort endpoint, steering
    /// closed). Partial streamed text may exist.
    Cancelled,
    /// Cooperative timeout fired and the turn did not finish in time.
    TimedOut,
    /// The turn failed for a non-cancel, non-timeout reason.
    Error,
}

/// Error-path information for the transport's terminal frame.
pub struct TurnFailure {
    /// Raw (unsanitized) `error.to_string()`; the transport sanitizes it for
    /// the WARN record and the failure frame (single-sanitize semantics).
    pub diagnostic: String,
    /// Localized terminal-completion message. `Some` also signals a terminal
    /// provider failure (`is_terminal_provider_failure == user_message.is_some()`).
    pub user_message: Option<String>,
}

/// Result of a gateway turn, carrying everything the transport needs to render
/// its terminal frame. The conversation itself is already persisted by the
/// runner; the transport only presents `response_text` and the error frame.
pub struct TurnOutcome {
    pub status: TurnStatus,
    /// Success: full response; Cancelled/TimedOut: partial + interruption
    /// marker; Error: empty or partial streamed text.
    pub response_text: String,
    /// Only `Some` when `status == Error`.
    pub error: Option<TurnFailure>,
    /// Sum of `TurnEvent::Usage.input_tokens` across LLM calls in this turn.
    pub total_input_tokens: Option<u64>,
    /// Sum of `TurnEvent::Usage.output_tokens` across LLM calls in this turn.
    pub total_output_tokens: Option<u64>,
    /// `total_input_tokens` + `total_output_tokens` (saturating), computed by
    /// the runner.
    pub total_tokens: Option<u64>,
    /// Cost tracked via the task-local usage accumulator; `None` when no cost
    /// tracker is wired up or the turn accumulated no usage.
    pub cost_usd: Option<f64>,
    /// Most recent absolute provider-reported prompt size (replaced on each
    /// usage event, not accumulated) for the client's context bar.
    pub last_input_tokens: Option<u64>,
    pub turn_id: String,
    pub provider: String,
    pub model: String,
    pub max_context_tokens: u64,
}

/// Handle handed to the transport's `forward` closure: the streaming event
/// source and the turn's cancellation token (client disconnect or timeout can
/// trigger it).
pub struct TurnRunnerHandle {
    pub event_rx: tokio::sync::mpsc::Receiver<TurnEvent>,
    pub cancel_token: tokio_util::sync::CancellationToken,
}

/// Usage observations and accumulated streamed text returned by the
/// transport's `forward` closure.
#[derive(Debug, Default)]
pub struct TurnForwardResult {
    /// Accumulated `TurnEvent::Usage.input_tokens` (sum, not replaced).
    pub total_input_tokens: Option<u64>,
    /// Accumulated `TurnEvent::Usage.output_tokens` (sum, not replaced).
    pub total_output_tokens: Option<u64>,
    /// Most recent absolute provider-reported prompt size (replaced on each
    /// usage event, not accumulated).
    pub last_input_tokens: Option<u64>,
    /// Concatenated `Chunk` deltas; the runner reconstructs the partial
    /// response on cancellation/timeout from this.
    pub accumulated_text: String,
}

// ── Cancellation-token RAII guard ───────────────────────────────────────────

/// Registers a cancellation token in `state.cancel_tokens` on construction and
/// removes it on drop, so a panic or early return in the turn can never leave a
/// stale token behind. Drop only removes the map entry — it never cancels;
/// cancellation is triggered solely by the transport (disconnect/abort) or the
/// cooperative timeout.
struct CancelTokenGuard {
    cancel_tokens: Arc<
        std::sync::Mutex<std::collections::HashMap<String, tokio_util::sync::CancellationToken>>,
    >,
    session_key: String,
    token: tokio_util::sync::CancellationToken,
}

impl CancelTokenGuard {
    fn register(state: &AppState, session_key: &str) -> Self {
        let token = tokio_util::sync::CancellationToken::new();
        state
            .cancel_tokens
            .lock()
            .expect("cancel_tokens lock poisoned")
            .insert(session_key.to_string(), token.clone());
        Self {
            cancel_tokens: Arc::clone(&state.cancel_tokens),
            session_key: session_key.to_string(),
            token,
        }
    }

    fn token(&self) -> &tokio_util::sync::CancellationToken {
        &self.token
    }
}

impl Drop for CancelTokenGuard {
    fn drop(&mut self) {
        self.cancel_tokens
            .lock()
            .expect("cancel_tokens lock poisoned")
            .remove(&self.session_key);
    }
}

// ── Persistence helpers (shared with the transports) ────────────────────────

/// Append persisted chat messages for a session, refusing to resurrect a
/// session the user deleted while the turn was in flight. System-role and
/// non-chat message variants are skipped.
pub(crate) fn persist_conversation_messages(
    backend: &dyn SessionBackend,
    session_key: &str,
    messages: &[ConversationMessage],
) {
    if !backend.session_exists(session_key) {
        return;
    }
    for message in messages {
        let ConversationMessage::Chat(message) = message else {
            continue;
        };
        if message.role == "system" {
            continue;
        }
        let _ = backend.append(session_key, message);
    }
}

/// Whether the given messages already contain an assistant chat message —
/// used to decide whether the cancellation marker should be appended so an
/// interrupted turn still records the partial assistant turn.
pub(crate) fn has_assistant_chat_message(messages: &[ConversationMessage]) -> bool {
    messages.iter().any(|message| {
        matches!(
            message,
            ConversationMessage::Chat(message) if message.role == "assistant"
        )
    })
}

// ── Turn runner ─────────────────────────────────────────────────────────────

/// Run one agent turn with a transport-supplied `forward` closure.
///
/// The runner owns every state-level side effect: attribution, cost-tracking
/// context, `agent_start`, session-state transitions, cancellation-token
/// registration, the streaming event channel, cooperative timeout, the
/// exactly-once finalizer (persistence, consolidation, `agent_end`, tracing),
/// and the assembled [`TurnOutcome`]. The `forward` closure owns the
/// presentation: consuming `TurnEvent`s, handling transport-specific input
/// (steering/approvals/ping for WebSocket), and detecting disconnect by
/// cancelling the handle's token.
///
/// `steering_rx` is `Some` for the WebSocket transport (caller creates the
/// steering channel and captures the sender side in its `forward` closure) and
/// `None` for HTTP, which never enters steering-drain mode. `timeout` is
/// `None` for WebSocket (current behavior, no deadline) and `Some` for HTTP,
/// where the cooperative deadline produces a `TimedOut` outcome instead of a
/// middleware timeout.
#[allow(clippy::too_many_arguments)]
pub async fn run_gateway_turn<F, Fut>(
    state: &AppState,
    agent: &mut Agent,
    user_message: &str,
    session_key: &str,
    ws_memory: &Option<Arc<dyn zeroclaw_memory::Memory>>,
    steering_rx: Option<&mut tokio::sync::mpsc::Receiver<String>>,
    channel_name: &str,
    timeout: Option<Duration>,
    forward: F,
) -> TurnOutcome
where
    F: FnOnce(TurnRunnerHandle) -> Fut,
    Fut: Future<Output = TurnForwardResult>,
{
    use ::zeroclaw_log::Instrument as _;

    // ── Pre-turn: attribution, cost context, session state ─────────────
    let (turn_alias, turn_provider, turn_model) = agent.attribution_fields();
    let provider_label = turn_provider.clone();

    let cost_tracking_context = state.cost_tracker.as_ref().map(|tracker| {
        let config = state.config.read();
        let pricing = build_model_provider_pricing(&config);
        ToolLoopCostTrackingContext::new(tracker.clone(), Arc::new(pricing))
            .with_agent_alias(&turn_alias)
    });
    let turn_usage = state
        .cost_tracker
        .as_ref()
        .map(|_| Arc::new(parking_lot::Mutex::new(TurnUsage::default())));

    // Resolve the context budget for this agent — the runtime-profile budget
    // (same source the context meter uses), not the provider model-window
    // helper which falls back to 32_000 when unset.
    let max_context_tokens = {
        let cfg = state.config.read();
        cfg.effective_max_context_tokens(&turn_alias) as u64
    };

    // Broadcast agent_start event.
    let _ = state.event_tx.send(json!({
        "type": "agent_start",
        "model_provider": provider_label,
        "model": turn_model,
    }));

    // Set session state to running and generate the turn id.
    let turn_id = uuid::Uuid::new_v4().to_string();
    if let Some(ref backend) = state.session_backend {
        let _ = backend.set_session_state(session_key, "running", Some(&turn_id));
    }

    // Create the token before the turn starts so the abort endpoint can cancel
    // it; the guard removes it after the turn completes regardless of outcome.
    let guard = CancelTokenGuard::register(state, session_key);

    // Channel for streaming turn events from the agent to the transport.
    let (event_tx, event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);

    let content_owned = user_message.to_string();
    let session_key_owned = session_key.to_string();
    let turn_fut = async {
        let span = ::zeroclaw_log::info_span!(
            target: "zeroclaw_log_internal_scope",
            "zeroclaw_scope",
            session_key = %session_key_owned,
            agent_alias = %turn_alias,
            model_provider = %turn_provider,
            model = %turn_model,
            channel = channel_name,
        );
        scope_session_key(
            Some(session_key_owned.clone()),
            TOOL_LOOP_TURN_USAGE.scope(
                turn_usage.clone(),
                TOOL_LOOP_COST_TRACKING_CONTEXT.scope(
                    cost_tracking_context.clone(),
                    agent
                        .turn_streamed_with_steering_state(
                            &content_owned,
                            event_tx,
                            Some(guard.token().clone()),
                            steering_rx,
                        )
                        .instrument(span),
                ),
            ),
        )
        .await
    };

    let handle = TurnRunnerHandle {
        event_rx,
        cancel_token: guard.token().clone(),
    };
    let forward_fut = forward(handle);

    // Drive both concurrently with 64-cap backpressure. `Box::pin` makes the
    // combined future `Unpin` so the timeout `select!` can poll it by ref.
    let work = Box::pin(async { tokio::join!(turn_fut, forward_fut) });

    let mut timed_out = false;
    let (result, forward_result) = if let Some(duration) = timeout {
        // Cooperative timeout: fire exactly once, cancel, then await the turn
        // collapsing via `ToolLoopCancelled` so the finalizer runs on the real
        // partial output.
        let deadline = tokio::time::sleep(duration);
        tokio::pin!(deadline);
        let mut work = work;
        loop {
            tokio::select! {
                biased;
                _ = &mut deadline, if !timed_out => {
                    timed_out = true;
                    guard.token().cancel();
                }
                (result, forward_result) = &mut work => break (result, forward_result),
            }
        }
    } else {
        let (result, forward_result) = work.await;
        (result, forward_result)
    };

    // Cancel token no longer needed once the turn has finished.
    drop(guard);

    let TurnForwardResult {
        total_input_tokens,
        total_output_tokens,
        last_input_tokens,
        accumulated_text,
    } = forward_result;

    // Usage aggregates shared by every terminal branch.
    let total_tokens = match (total_input_tokens, total_output_tokens) {
        (Some(i), Some(o)) => Some(i.saturating_add(o)),
        (Some(i), None) => Some(i),
        (None, Some(o)) => Some(o),
        (None, None) => None,
    };
    let cost_usd = turn_usage
        .as_ref()
        .map(|usage| *usage.lock())
        .filter(|usage| usage.input_tokens > 0 || usage.output_tokens > 0)
        .map(|usage| usage.cost_usd);

    // Tracing message identity: the doctor/replay tool filters on this exact
    // string, so it must be derived from the channel name.
    let trace_message = match channel_name {
        "wss" => "gateway_ws_turn",
        "http" => "gateway_http_turn",
        _ => "gateway_chat_turn",
    };

    // State mapping: `Ok` wins even if the deadline fired concurrently (the
    // turn already finished); otherwise cancelled-beats-error, timeout wins
    // over any non-cancel error.
    let status = match &result {
        Ok(_) => TurnStatus::Success,
        Err(e) if is_tool_loop_cancelled(&e.error) && timed_out => TurnStatus::TimedOut,
        Err(e) if is_tool_loop_cancelled(&e.error) => TurnStatus::Cancelled,
        Err(_) if timed_out => TurnStatus::TimedOut,
        Err(_) => TurnStatus::Error,
    };

    match (&result, status) {
        (Ok(outcome), TurnStatus::Success) => {
            if let Some(ref backend) = state.session_backend {
                persist_conversation_messages(backend.as_ref(), session_key, &outcome.new_messages);
            }

            // Fire-and-forget memory consolidation so facts from gateway
            // sessions are extracted to long-term memory. Uses the gateway
            // default model, not the routed turn model.
            if state.auto_save {
                if let Some(mem) = ws_memory.clone() {
                    let model_provider = state.model_provider.clone();
                    let model = state.model.clone();
                    let temperature = state.temperature;
                    let memory_config = state.config.read().memory.clone();
                    let user_msg = user_message.to_string();
                    let assistant_resp = outcome.response.clone();
                    zeroclaw_spawn::spawn!(async move {
                        if let Err(e) = consolidate_turn(
                            model_provider.as_ref(),
                            &model,
                            temperature,
                            mem.as_ref(),
                            &memory_config,
                            &user_msg,
                            &assistant_resp,
                        )
                        .await
                        {
                            ::zeroclaw_log::record!(
                                DEBUG,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_attrs(json!({"error": format!("{}", e)})),
                                "Gateway memory consolidation skipped"
                            );
                        }
                    });
                } else {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        "Gateway memory consolidation skipped"
                    );
                }
            }

            if let Some(ref backend) = state.session_backend {
                let _ = backend.set_session_state(session_key, "idle", None);
            }

            // Broadcast agent_end event.
            let _ = state.event_tx.send(json!({
                "type": "agent_end",
                "model_provider": provider_label,
                "model": turn_model,
            }));

            // Append a runtime-trace record so the doctor sweep sees gateway
            // turns alongside channel and CLI turns.
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)
                    .with_outcome(::zeroclaw_log::EventOutcome::Success)
                    .with_attrs(json!({
                        "model_provider": provider_label,
                        "model": turn_model,
                        "session_key": session_key,
                        "input_tokens": total_input_tokens,
                        "output_tokens": total_output_tokens,
                        "tokens_used": total_tokens,
                        "cost_usd": cost_usd,
                        "last_input_tokens": last_input_tokens,
                        "trace_id": turn_id,
                    })),
                trace_message
            );

            TurnOutcome {
                status: TurnStatus::Success,
                response_text: outcome.response.clone(),
                error: None,
                total_input_tokens,
                total_output_tokens,
                total_tokens,
                cost_usd,
                last_input_tokens,
                turn_id,
                provider: turn_provider,
                model: turn_model,
                max_context_tokens,
            }
        }
        (Err(error), TurnStatus::Cancelled | TurnStatus::TimedOut) => {
            let marker =
                zeroclaw_runtime::i18n::get_required_cli_string("turn-interrupted-by-user");
            let truncated = if accumulated_text.is_empty() {
                marker
            } else {
                format!("{accumulated_text}\n\n{marker}")
            };

            if let Some(ref backend) = state.session_backend {
                let still_exists = backend.session_exists(session_key);
                if still_exists {
                    if !error.new_messages.is_empty() {
                        persist_conversation_messages(
                            backend.as_ref(),
                            session_key,
                            &error.new_messages,
                        );
                        // Only append the interruption marker when the partial
                        // output didn't already produce an assistant message.
                        if !has_assistant_chat_message(&error.new_messages) {
                            let assistant_msg = ChatMessage::assistant(&truncated);
                            // Re-check before the raw append — the user can
                            // delete the session between the outer check and
                            // here; `persist_conversation_messages` already
                            // re-checks internally.
                            if backend.session_exists(session_key) {
                                let _ = backend.append(session_key, &assistant_msg);
                            }
                        }
                    } else {
                        let assistant_msg = ChatMessage::assistant(&truncated);
                        if backend.session_exists(session_key) {
                            let _ = backend.append(session_key, &assistant_msg);
                        }
                    }
                }
            }

            if let Some(ref backend) = state.session_backend
                && backend.session_exists(session_key)
            {
                let _ = backend.set_session_state(session_key, "idle", None);
            }

            // Broadcast agent_end event.
            let _ = state.event_tx.send(json!({
                "type": "agent_end",
                "model_provider": provider_label,
                "model": turn_model,
            }));

            let reason = match status {
                TurnStatus::TimedOut => "timed out",
                _ => "interrupted by user",
            };
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Cancel)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(json!({
                        "model_provider": provider_label,
                        "model": turn_model,
                        "session_key": session_key,
                        "reason": reason,
                        "cancelled": true,
                        "trace_id": turn_id,
                    })),
                trace_message
            );

            TurnOutcome {
                status,
                response_text: truncated,
                error: None,
                total_input_tokens,
                total_output_tokens,
                total_tokens,
                cost_usd,
                last_input_tokens,
                turn_id,
                provider: turn_provider,
                model: turn_model,
                max_context_tokens,
            }
        }
        (Err(error), TurnStatus::Error) => {
            if let Some(ref backend) = state.session_backend
                && !error.new_messages.is_empty()
            {
                persist_conversation_messages(backend.as_ref(), session_key, &error.new_messages);
            }

            // Set session state to error.
            if let Some(ref backend) = state.session_backend {
                let _ = backend.set_session_state(session_key, "error", Some(&turn_id));
            }

            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(json!({"error": format!("{}", error.error)})),
                "Agent turn failed"
            );

            let user_message =
                zeroclaw_runtime::agent::terminal_completion_error_message(&error.error, None);

            TurnOutcome {
                status: TurnStatus::Error,
                response_text: accumulated_text,
                error: Some(TurnFailure {
                    diagnostic: format!("{}", error.error),
                    user_message,
                }),
                total_input_tokens,
                total_output_tokens,
                total_tokens,
                cost_usd,
                last_input_tokens,
                turn_id,
                provider: turn_provider,
                model: turn_model,
                max_context_tokens,
            }
        }
        (_, _) => unreachable!("turn status and result are consistent by construction"),
    }
}
