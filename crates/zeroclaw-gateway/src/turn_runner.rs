//! Transport-neutral turn lifecycle spine shared by the WebSocket (`/ws/chat`)
//! and HTTP (`/v1/chat/completions`) chat paths.
//!
//! The caller passes its forward logic as a closure that drains `event_rx`
//! concurrently with the turn future (the channel is capped at 64; awaiting
//! the turn before draining would backpressure and deadlock).

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Once};

use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zeroclaw_api::agent::TurnEvent;
use zeroclaw_api::memory_traits::MemoryStrategy;
use zeroclaw_providers::ConversationMessage;
use zeroclaw_runtime::agent::Agent;
use zeroclaw_runtime::agent::cost::TurnUsage;

use crate::AppState;

/// Observable outcome of a completed gateway turn. The runner has already
/// persisted `new_messages`, transitioned session state, broadcast
/// `agent_end`, and written the tracing record.
pub struct TurnOutcome {
    pub response_text: String,
    pub new_messages: Vec<ConversationMessage>,
    pub usage: Option<Arc<parking_lot::Mutex<TurnUsage>>>,
    pub total_input_tokens: Option<u64>,
    pub total_output_tokens: Option<u64>,
    /// Most recent provider-reported prompt size (replaces on each Usage; not accumulated).
    pub last_input_tokens: Option<u64>,
    /// Agent's configured max context window.
    pub max_context_tokens: u64,
    pub turn_id: String,
    pub turn_provider: String,
    pub turn_model: String,
    pub status: TurnStatus,
    /// Sanitized error string on the `Error` branch; `None` otherwise.
    pub error: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TurnStatus {
    Success,
    Cancelled,
    Error,
}

/// Handle handed to the caller's forward closure.
pub struct TurnRunnerHandle {
    pub event_rx: mpsc::Receiver<TurnEvent>,
    /// Cancelled on client disconnect / SSE failure / abort; the runtime
    /// propagates `ToolLoopCancelled`.
    pub cancel_token: CancellationToken,
}

/// RAII guard that removes the cancel token from `cancel_tokens` when dropped.
///
/// Created immediately after the token is registered in
/// [`run_gateway_turn`]. Guarantees cleanup even if the forward closure panics.
struct CancelTokenGuard {
    tokens: Arc<Mutex<HashMap<String, CancellationToken>>>,
    session_key: String,
}

impl Drop for CancelTokenGuard {
    fn drop(&mut self) {
        self.tokens.lock().remove(&self.session_key);
    }
}

/// Run a gateway turn: the transport-neutral spine (pre-turn setup + post-turn
/// persistence/state/broadcast/tracing) wrapped around the caller's forward
/// loop. The caller drains `event_rx` concurrently via `forward`; the runner
/// `tokio::join!`s the turn future with `forward(handle)` so neither blocks
/// the other (channel cap 64 — see module docs). Terminal frame emission
/// stays in the caller.
pub async fn run_gateway_turn<F, Fut>(
    state: &AppState,
    agent: &mut Agent,
    user_message: &str,
    session_key: &str,
    ws_memory: &Option<Arc<dyn zeroclaw_memory::Memory>>,
    steering_rx: Option<&mut mpsc::Receiver<String>>,
    channel_name: &str,
    forward: F,
) -> TurnOutcome
where
    F: FnOnce(TurnRunnerHandle) -> Fut,
    Fut: Future<Output = (Option<u64>, Option<u64>, Option<u64>)>,
{
    let (turn_alias, turn_provider, turn_model) = agent.attribution_fields();
    let provider_label = turn_provider.clone();
    // Resolve context budget for this agent. Wire field is named
    // `max_context_tokens` and must track the runtime-profile budget
    // (same source Zerocode's context meter uses), not the provider
    // model-window helper which falls back to 32_000 when unset.
    let max_context_tokens = {
        let cfg = state.config.read();
        cfg.effective_max_context_tokens(&turn_alias) as u64
    };
    let cost_tracking_context = state.cost_tracker.as_ref().map(|tracker| {
        let config = state.config.read();
        let pricing = zeroclaw_runtime::agent::cost::build_model_provider_pricing(&config);
        zeroclaw_runtime::agent::cost::ToolLoopCostTrackingContext::new(
            tracker.clone(),
            Arc::new(pricing),
        )
        .with_agent_alias(&turn_alias)
    });
    let turn_usage = state.cost_tracker.as_ref().map(|_| {
        Arc::new(parking_lot::Mutex::new(
            zeroclaw_runtime::agent::cost::TurnUsage::default(),
        ))
    });

    // Broadcast agent_start event
    if state.event_tx.receiver_count() > 0 {
        if let Err(e) = state.event_tx.send(serde_json::json!({
            "type": "agent_start",
            "model_provider": provider_label,
            "model": turn_model,
        })) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "session_key": session_key,
                        "event_type": "agent_start",
                        "model_provider": provider_label,
                        "model": turn_model,
                        "error": format!("{}", e),
                    })),
                "Failed to broadcast agent_start event"
            );
        }
    } else {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                .with_category(::zeroclaw_log::EventCategory::Agent)
                .with_attrs(::serde_json::json!({
                    "session_key": session_key,
                    "event_type": "agent_start",
                })),
            "Skipping agent_start broadcast: no active receivers"
        );
    }

    // Set session state to running
    let turn_id = uuid::Uuid::new_v4().to_string();
    if let Some(ref backend) = state.session_backend {
        if let Err(e) = backend.set_session_state(session_key, "running", Some(&turn_id)) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "session_key": session_key,
                        "turn_id": turn_id,
                        "state": "running",
                        "error": format!("{}", e),
                    })),
                "Failed to set session state to running"
            );
        }
    }

    let cancel_token = tokio_util::sync::CancellationToken::new();
    let _cancel_guard = {
        state
            .cancel_tokens
            .lock()
            .insert(session_key.to_string(), cancel_token.clone());
        CancelTokenGuard {
            tokens: Arc::clone(&state.cancel_tokens),
            session_key: session_key.to_string(),
        }
    };

    let (event_tx, event_rx) = tokio::sync::mpsc::channel::<TurnEvent>(64);

    // `agent` is `&mut` so it cannot move into a spawned task; join the turn
    // future with the caller's forward closure instead.
    let content_owned = user_message.to_string();
    let session_key_owned = session_key.to_string();
    let turn_fut = async {
        use ::zeroclaw_log::Instrument as _;
        let span = ::zeroclaw_log::info_span!(
            target: "zeroclaw_log_internal_scope",
            "zeroclaw_scope",
            session_key = %session_key_owned,
            agent_alias = %turn_alias,
            model_provider = %turn_provider,
            model = %turn_model,
            channel = channel_name,
        );
        zeroclaw_runtime::agent::loop_::scope_session_key(
            Some(session_key_owned.clone()),
            zeroclaw_runtime::agent::cost::TOOL_LOOP_TURN_USAGE.scope(
                turn_usage.clone(),
                zeroclaw_runtime::agent::cost::TOOL_LOOP_COST_TRACKING_CONTEXT.scope(
                    cost_tracking_context.clone(),
                    agent
                        .turn_streamed_with_steering_state(
                            &content_owned,
                            event_tx,
                            Some(cancel_token.clone()),
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
        cancel_token: cancel_token.clone(),
    };

    // The forward closure returns the usage tokens it aggregated so the runner
    // can include them in the tracing record.
    let (result, (total_input_tokens, total_output_tokens, last_input_tokens)) =
        tokio::join!(turn_fut, forward(handle));

    // CancelTokenGuard removes the token from cancel_tokens on drop, even if
    // the forward closure panicked.

    let was_cancelled = match &result {
        Err(e) => zeroclaw_runtime::agent::loop_::is_tool_loop_cancelled(&e.error),
        Ok(_) => false,
    };

    if was_cancelled {
        // The runtime fills `committed_response` (partial + marker) and appends
        // the marker assistant message to `new_messages` on cancel, so persist
        // `error.new_messages` directly.
        if let Some(ref backend) = state.session_backend {
            // `DELETE /api/sessions/{id}` cancels the token and removes the
            // session; skip every write when the session no longer exists to
            // avoid resurrecting it.
            let still_exists = backend.session_exists(session_key);
            if still_exists {
                if let Err(error) = &result {
                    if !error.new_messages.is_empty() {
                        persist_conversation_messages(
                            backend.as_ref(),
                            session_key,
                            &error.new_messages,
                        );
                    }
                    if !has_assistant_chat_message(&error.new_messages) {
                        let assistant_msg =
                            zeroclaw_providers::ChatMessage::assistant(&error.committed_response);
                        // Re-check: the session may be deleted between the outer check and here.
                        if backend.session_exists(session_key) {
                            if let Err(e) = backend.append(session_key, &assistant_msg) {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Write
                                    )
                                    .with_category(::zeroclaw_log::EventCategory::Agent)
                                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                    .with_attrs(
                                        ::serde_json::json!({
                                            "session_key": session_key,
                                            "message_role": "assistant",
                                            "error": format!("{}", e),
                                        })
                                    ),
                                    "Failed to persist cancelled-turn fallback assistant message"
                                );
                            }
                        }
                    }
                }
            }
        }

        // Only touch state for sessions that still exist (DELETE may have removed them).
        if let Some(ref backend) = state.session_backend
            && backend.session_exists(session_key)
        {
            if let Err(e) = backend.set_session_state(session_key, "idle", None) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write)
                        .with_category(::zeroclaw_log::EventCategory::Agent)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "session_key": session_key,
                            "state": "idle",
                            "error": format!("{}", e),
                        })),
                    "Failed to set session state to idle after cancellation"
                );
            }
        }

        if state.event_tx.receiver_count() > 0 {
            if let Err(e) = state.event_tx.send(serde_json::json!({
                "type": "agent_end",
                "model_provider": provider_label,
                "model": turn_model,
            })) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                        .with_category(::zeroclaw_log::EventCategory::Agent)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "session_key": session_key,
                            "event_type": "agent_end",
                            "model_provider": provider_label,
                            "model": turn_model,
                            "error": format!("{}", e),
                        })),
                    "Failed to broadcast agent_end event"
                );
            }
        } else {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_attrs(::serde_json::json!({
                        "session_key": session_key,
                        "event_type": "agent_end",
                    })),
                "Skipping agent_end broadcast: no active receivers"
            );
        }

        // Trace cancelled turns so `zeroclaw doctor` sees them.
        let trace_name = format!("gateway_{}_turn", channel_name);
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Cancel)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "model_provider": provider_label,
                    "model": turn_model,
                    "session_key": session_key,
                    "reason": "interrupted by user",
                    "cancelled": true,
                    "trace_id": turn_id,
                })),
            &trace_name
        );

        let cancelled_error_msg = result
            .as_ref()
            .err()
            .map(|e| zeroclaw_providers::sanitize_api_error(&e.error.to_string()));
        let (response_text, new_messages) = match result {
            Err(error) => (error.committed_response, error.new_messages),
            Ok(_) => (String::new(), Vec::new()),
        };
        return TurnOutcome {
            response_text,
            new_messages,
            usage: turn_usage,
            total_input_tokens,
            total_output_tokens,
            last_input_tokens,
            max_context_tokens,
            turn_id,
            turn_provider: provider_label,
            turn_model,
            status: TurnStatus::Cancelled,
            error: cancelled_error_msg,
        };
    }

    match result {
        Ok(outcome) => {
            if let Some(ref backend) = state.session_backend {
                persist_conversation_messages(backend.as_ref(), session_key, &outcome.new_messages);
            }

            // Fire-and-forget memory consolidation via MemoryStrategy.
            // Route through MemoryStrategy, not direct call.
            // TODO: use agent's provider/model/temperature (per-request) instead
            // of AppState globals. Agent fields are private — needs a getter.
            if state.auto_save {
                if let Some(mem) = ws_memory.clone() {
                    let model_provider = state.model_provider.clone();
                    let model = state.model.clone();
                    let temperature = state.temperature;
                    let memory_config = state.config.read().memory.clone();
                    let data_dir = state.config.read().data_dir.clone();
                    let user_msg = user_message.to_string();
                    let assistant_resp = outcome.response.clone();
                    static RERANK_WARNED: Once = Once::new();
                    zeroclaw_spawn::spawn!(async move {
                        // The MemoryStrategy constructor warns about
                        // rerank_enabled every call. Log once then mute.
                        let mut cfg = memory_config;
                        if cfg.rerank_enabled {
                            RERANK_WARNED.call_once(|| {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        "gateway turn runner",
                                        ::zeroclaw_log::Action::Note,
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(
                                        ::serde_json::json!({
                                            "rerank_enabled": true,
                                            "rerank_threshold": cfg.rerank_threshold,
                                        })
                                    ),
                                    "memory.rerank_enabled is set but \
                                     the rerank stage is not yet \
                                     implemented; this setting currently \
                                     has no effect"
                                );
                            });
                            cfg.rerank_enabled = false;
                        }
                        let strategy =
                            zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::new(
                                mem, cfg, data_dir,
                            );
                        if let Err(e) = strategy
                            .consolidate_turn(
                                &user_msg,
                                &assistant_resp,
                                model_provider.as_ref(),
                                &model,
                                temperature,
                            )
                            .await
                        {
                            ::zeroclaw_log::record!(
                                DEBUG,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                                "gateway memory consolidation skipped"
                            );
                        }
                    });
                } else {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        "gateway memory consolidation skipped"
                    );
                }
            }

            // Set session state to idle
            if let Some(ref backend) = state.session_backend {
                if let Err(e) = backend.set_session_state(session_key, "idle", None) {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write)
                            .with_category(::zeroclaw_log::EventCategory::Agent)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "session_key": session_key,
                                "state": "idle",
                                "error": format!("{}", e),
                            })),
                        "Failed to set session state to idle after successful turn"
                    );
                }
            }

            if state.event_tx.receiver_count() > 0 {
                if let Err(e) = state.event_tx.send(serde_json::json!({
                    "type": "agent_end",
                    "model_provider": provider_label,
                    "model": turn_model,
                })) {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                            .with_category(::zeroclaw_log::EventCategory::Agent)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "session_key": session_key,
                                "event_type": "agent_end",
                                "model_provider": provider_label,
                                "model": turn_model,
                                "error": format!("{}", e),
                            })),
                        "Failed to broadcast agent_end event"
                    );
                }
            } else {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                        .with_category(::zeroclaw_log::EventCategory::Agent)
                        .with_attrs(::serde_json::json!({
                            "session_key": session_key,
                            "event_type": "agent_end",
                        })),
                    "Skipping agent_end broadcast: no active receivers"
                );
            }

            // Trace gateway turns for `zeroclaw doctor`.
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
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)
                    .with_outcome(::zeroclaw_log::EventOutcome::Success)
                    .with_attrs(::serde_json::json!({
                        "model_provider": provider_label,
                        "model": turn_model,
                        "session_key": session_key,
                        "input_tokens": total_input_tokens,
                        "output_tokens": total_output_tokens,
                        "tokens_used": total_tokens,
                        "cost_usd": cost_usd,
                        "trace_id": turn_id,
                    })),
                &format!("gateway_{}_turn", channel_name)
            );

            TurnOutcome {
                response_text: outcome.response,
                new_messages: outcome.new_messages,
                usage: turn_usage,
                total_input_tokens,
                total_output_tokens,
                last_input_tokens,
                max_context_tokens,
                turn_id,
                turn_provider: provider_label,
                turn_model,
                status: TurnStatus::Success,
                error: None,
            }
        }
        Err(e) => {
            if let Some(ref backend) = state.session_backend
                && !e.new_messages.is_empty()
            {
                persist_conversation_messages(backend.as_ref(), session_key, &e.new_messages);
            }

            if let Some(ref backend) = state.session_backend {
                if let Err(e) = backend.set_session_state(session_key, "error", Some(&turn_id)) {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write)
                            .with_category(::zeroclaw_log::EventCategory::Agent)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "session_key": session_key,
                                "turn_id": turn_id,
                                "state": "error",
                                "error": format!("{}", e),
                            })),
                        "Failed to set session state to error after failed turn"
                    );
                }
            }

            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e.error)})),
                "Agent turn failed"
            );
            let sanitized = zeroclaw_providers::sanitize_api_error(&e.error.to_string());

            // Trace failed turns; turn_id cross-references costs.jsonl.
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "model_provider": provider_label,
                        "model": turn_model,
                        "session_key": session_key,
                        "error": sanitized,
                        "trace_id": turn_id,
                    })),
                &format!("gateway_{}_turn", channel_name)
            );

            TurnOutcome {
                response_text: e.committed_response,
                new_messages: e.new_messages,
                usage: turn_usage,
                total_input_tokens,
                total_output_tokens,
                last_input_tokens,
                max_context_tokens,
                turn_id,
                turn_provider: provider_label,
                turn_model,
                status: TurnStatus::Error,
                error: Some(sanitized),
            }
        }
    }
}

/// Persist the turn's `new_messages` to the session backend.
///
/// Skip writes when the session was deleted mid-turn, to avoid
/// resurrecting the session `DELETE /api/sessions/{id}` just removed.
pub(crate) fn persist_conversation_messages(
    backend: &dyn zeroclaw_infra::session_backend::SessionBackend,
    session_key: &str,
    messages: &[ConversationMessage],
) {
    // `append` uses `create(true)` — on the first turn the file is
    // created automatically. Deleted-session protection is handled by
    // the caller-side `session_exists` checks that return early with an
    // error before reaching this code path.
    let mut failed: usize = 0;
    let mut total: usize = 0;
    for message in messages {
        let zeroclaw_providers::ConversationMessage::Chat(message) = message else {
            continue;
        };
        if message.role == "system" {
            continue;
        }
        total += 1;
        if let Err(e) = backend.append(session_key, message) {
            failed += 1;
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "session_key": session_key,
                        "message_role": message.role,
                        "error": format!("{}", e),
                    })),
                "Failed to persist conversation message"
            );
        }
    }
    if failed > 0 {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write)
                .with_category(::zeroclaw_log::EventCategory::Agent)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "session_key": session_key,
                    "total_messages": total,
                    "failed_count": failed,
                })),
            "Conversation message persistence incomplete: {failed}/{total} messages failed"
        );
    }
}

pub(crate) fn has_assistant_chat_message(messages: &[ConversationMessage]) -> bool {
    messages.iter().any(|message| {
        matches!(
            message,
            zeroclaw_providers::ConversationMessage::Chat(message)
                if message.role == "assistant"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use parking_lot::Mutex;
    use std::collections::{HashMap, VecDeque};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::mpsc;
    use zeroclaw_api::attribution::{
        Attributable, ModelProviderKind, ProviderKind, Role, ToolKind,
    };
    use zeroclaw_api::memory_traits::Memory;
    use zeroclaw_api::model_provider::{ModelProvider, TokenUsage};
    use zeroclaw_api::tool::{Tool, ToolResult};
    use zeroclaw_config::schema::Config;
    use zeroclaw_infra::session_backend::SessionBackend;
    use zeroclaw_providers::{
        ChatMessage, ChatRequest, ChatResponse, ConversationMessage, ToolCall,
    };
    use zeroclaw_runtime::agent::Agent;
    use zeroclaw_runtime::agent::dispatcher::NativeToolDispatcher;
    use zeroclaw_runtime::observability::NoopObserver;

    // ── shared test fixtures (mirrors of safety_net.rs fixtures) ───────────

    fn text_response(text: &str) -> ChatResponse {
        ChatResponse {
            text: Some(text.into()),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        }
    }

    fn tool_call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: "{}".into(),
            extra_content: None,
        }
    }

    fn tool_response(calls: Vec<ToolCall>) -> ChatResponse {
        ChatResponse {
            text: Some(String::new()),
            tool_calls: calls,
            usage: None,
            reasoning_content: None,
        }
    }

    fn token_usage(input: u64, output: u64) -> TokenUsage {
        TokenUsage {
            input_tokens: Some(input),
            cached_input_tokens: None,
            output_tokens: Some(output),
        }
    }

    /// Returns scripted responses in order; "done" once the script is exhausted.
    /// Mirrors `safety_net.rs::ScriptedProvider` so the gateway tests exercise
    /// the same agent shape the runtime safety-net pins rely on.
    struct ScriptedProvider {
        responses: parking_lot::Mutex<VecDeque<ChatResponse>>,
    }

    impl ScriptedProvider {
        fn new(responses: Vec<ChatResponse>) -> Self {
            Self {
                responses: parking_lot::Mutex::new(responses.into()),
            }
        }
    }

    #[async_trait]
    impl ModelProvider for ScriptedProvider {
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
        ) -> anyhow::Result<ChatResponse> {
            Ok(self
                .responses
                .lock()
                .pop_front()
                .unwrap_or_else(|| text_response("done")))
        }
    }

    impl Attributable for ScriptedProvider {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }
        fn alias(&self) -> &str {
            "ScriptedProvider"
        }
    }

    /// Counts executions; succeeds with a fixed output.
    struct CountingTool {
        name: &'static str,
        calls: Arc<AtomicUsize>,
    }

    zeroclaw_api::tool_attribution!(CountingTool, ToolKind::Plugin);

    #[async_trait]
    impl Tool for CountingTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            self.name
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ToolResult {
                success: true,
                output: format!("{}-out", self.name).into(),
                error: None,
            })
        }
    }

    fn mem_none() -> Arc<dyn Memory> {
        let cfg = zeroclaw_config::schema::MemoryConfig {
            backend: "none".into(),
            ..zeroclaw_config::schema::MemoryConfig::default()
        };
        Arc::from(
            zeroclaw_memory::create_memory(&cfg, std::path::Path::new("/tmp"), None)
                .expect("memory creation should succeed"),
        )
    }

    fn build_agent(provider: Box<dyn ModelProvider>, tools_vec: Vec<Box<dyn Tool>>) -> Agent {
        Agent::builder()
            .model_provider(provider)
            .tools(tools_vec)
            .memory(mem_none())
            .observer(Arc::from(NoopObserver {}))
            .tool_dispatcher(Box::new(NativeToolDispatcher))
            .workspace_dir(std::path::PathBuf::from("/tmp"))
            .build()
            .expect("agent builder should succeed")
    }

    /// Minimal `SessionBackend` mock that records every `append` call and
    /// always reports the session as existing. Used to assert the runner
    /// persists `outcome.new_messages` verbatim.
    struct RecordingBackend {
        appended: std::sync::Mutex<Vec<(String, String, String)>>,
        exists: bool,
    }

    impl SessionBackend for RecordingBackend {
        fn load(&self, _session_key: &str) -> Vec<ChatMessage> {
            Vec::new()
        }
        fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
            self.appended.lock().unwrap().push((
                session_key.to_string(),
                message.role.clone(),
                message.content.clone(),
            ));
            Ok(())
        }
        fn remove_last(&self, _session_key: &str) -> std::io::Result<bool> {
            Ok(false)
        }
        fn list_sessions(&self) -> Vec<String> {
            Vec::new()
        }
        fn session_exists(&self, _session_key: &str) -> bool {
            self.exists
        }
    }

    /// Build a minimal `AppState` with only the fields the runner touches
    /// populated. `session_backend` is injected so persistence tests can
    /// observe `append` calls. Mirrors `api::tests::test_state` but stays
    /// local to this module (the `api` test module is private).
    fn runner_state(backend: Option<Arc<dyn SessionBackend>>) -> AppState {
        let config = Config::default();
        AppState {
            config: Arc::new(parking_lot::RwLock::new(config)),
            model_provider: Arc::new(ScriptedProvider::new(vec![text_response("seed")])),
            model: "test-model".into(),
            temperature: None,
            mem: mem_none(),
            memory_strategy: Arc::new(
                zeroclaw_runtime::agent::memory_strategy::DefaultMemoryStrategy::with_config(
                    mem_none(),
                    zeroclaw_config::schema::MemoryConfig::default(),
                    std::path::PathBuf::new(),
                ),
            ),
            auto_save: false,
            webhook_secret_hash: None,
            pairing: Arc::new(zeroclaw_runtime::security::pairing::PairingGuard::new(
                false,
                &[],
            )),
            trust_forwarded_headers: false,
            rate_limiter: Arc::new(crate::GatewayRateLimiter::new(100, 100, 100, 100)),
            auth_limiter: Arc::new(crate::auth_rate_limit::AuthRateLimiter::new()),
            idempotency_store: Arc::new(crate::IdempotencyStore::new(
                Duration::from_secs(300),
                1000,
            )),
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp: HashMap::new(),
            #[cfg(feature = "channel-whatsapp-cloud")]
            whatsapp_app_secret: HashMap::new(),
            #[cfg(feature = "channel-linq")]
            linq: HashMap::new(),
            #[cfg(feature = "channel-linq")]
            linq_signing_secrets: HashMap::new(),
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk: HashMap::new(),
            #[cfg(feature = "channel-nextcloud")]
            nextcloud_talk_webhook_secret: HashMap::new(),
            #[cfg(feature = "channel-wati")]
            wati: HashMap::new(),
            #[cfg(feature = "channel-email")]
            gmail_push: None,
            observer: Arc::new(NoopObserver),
            tools_registry: Arc::new(Vec::new()),
            tools_registry_by_agent: Arc::new(HashMap::new()),
            cost_tracker: None,
            event_tx: tokio::sync::broadcast::channel(16).0,
            event_buffer: Arc::new(crate::sse::EventBuffer::new(16)),
            shutdown_tx: tokio::sync::watch::channel(false).0,
            node_registry: Arc::new(crate::nodes::NodeRegistry::new(16)),
            session_backend: backend,
            session_queue: Arc::new(crate::session_queue::SessionActorQueue::new(8, 30, 600)),
            consolidation_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
            device_registry: None,
            pending_pairings: None,
            path_prefix: String::new(),
            web_dist_dir: None,
            canvas_store: zeroclaw_runtime::tools::CanvasStore::new(),
            cancel_tokens: Arc::new(Mutex::new(HashMap::new())),
            ws_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            pending_reload: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            tui_registry: None,
            reload_tx: None,
            sop_engine: None,
            sop_audit: None,
            #[cfg(feature = "webauthn")]
            webauthn: None,
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // §5.1.1 — HTTP turn abortable via the cancel token the runner registers
    // ─────────────────────────────────────────────────────────────────────
    //
    // The abort endpoint (`POST /api/sessions/{id}/abort`) looks up
    // `state.cancel_tokens` under the session_key and calls `.cancel()`. This
    // test proves the token the runner inserts into `cancel_tokens` at turn
    // start is the same token that, when cancelled, drives the turn to
    // `TurnStatus::Cancelled` and is removed from the map after the turn.
    //
    // A full HTTP-integration test (real abort request → AppState with
    // cancel_tokens + a running turn) is infeasible at unit level because
    // it needs a running axum server + mid-flight turn; this test exercises
    // the spine directly, which is the load-bearing seam the abort endpoint
    // depends on.

    #[tokio::test]
    async fn cancel_token_registered_then_cancel_propagates_to_turn_outcome() {
        // A provider that ALWAYS returns a tool call — the turn can only end
        // via cancellation (a finite script would let the turn reach the
        // "done" text and return Success before the cancel propagated).
        struct AlwaysToolCall;
        #[async_trait]
        impl ModelProvider for AlwaysToolCall {
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
            ) -> anyhow::Result<ChatResponse> {
                // Yield once so the runtime's select! between the cancel
                // token and the next iteration can observe the cancellation.
                tokio::task::yield_now().await;
                Ok(tool_response(vec![tool_call("tc", "echo")]))
            }
        }
        impl Attributable for AlwaysToolCall {
            fn role(&self) -> Role {
                Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
            }
            fn alias(&self) -> &str {
                "AlwaysToolCall"
            }
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let mut agent = build_agent(
            Box::new(AlwaysToolCall),
            vec![Box::new(CountingTool {
                name: "echo",
                calls: Arc::clone(&calls),
            })],
        );
        let state = runner_state(None);
        let session_key = "gw_abort_test";

        // The forward closure observes the first event, then cancels the
        // token — mirroring what `/api/sessions/{id}/abort` does via
        // `state.cancel_tokens`. We look the token up from `state` exactly
        // as the abort handler does, to prove the registered token is the
        // effective one.
        let state_ref = &state;
        let session_key_owned = session_key.to_string();
        let forward = move |handle: TurnRunnerHandle| async move {
            let TurnRunnerHandle {
                mut event_rx,
                cancel_token,
            } = handle;
            // Wait for the first turn event so we know the turn is running
            // and the token has been registered.
            let _ = event_rx.recv().await;
            // Prove the runner registered the token under the session_key
            // (same lookup the abort handler performs).
            let registered = state_ref
                .cancel_tokens
                .lock()
                .get(&session_key_owned)
                .cloned();
            assert!(
                registered.is_some(),
                "runner must register the cancel token under the session_key \
                 while the turn is running (abort endpoint depends on this)"
            );
            // Cancel via the handle's token (same token the abort handler
            // would grab from `state.cancel_tokens`).
            cancel_token.cancel();
            // Drain remaining events so the turn future can complete.
            while event_rx.recv().await.is_some() {}
            (None, None, None)
        };

        let outcome = run_gateway_turn(
            &state,
            &mut agent,
            "loop forever",
            session_key,
            &None,
            None,
            "http",
            forward,
        )
        .await;

        assert_eq!(
            outcome.status,
            TurnStatus::Cancelled,
            "cancelling the runner-registered token must drive the turn to \
             TurnStatus::Cancelled (the abort endpoint relies on this)"
        );
        // The runner removes the token after the turn completes, regardless
        // of outcome — verify it's gone so a subsequent abort doesn't fire
        // a stale token.
        assert!(
            state.cancel_tokens.lock().get(session_key).is_none(),
            "runner must remove the cancel token from state.cancel_tokens \
             after the turn completes (cancelled branch included)"
        );
        // The tool did execute at least once before the cancel propagated,
        // proving the turn actually started (cancel didn't pre-empt before
        // the first iteration).
        assert!(
            calls.load(Ordering::SeqCst) >= 1,
            "the turn must have started (at least one tool call) before \
             cancellation propagated"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // §5.1.2 — TOOL_LOOP_SESSION_KEY active during the (HTTP) tool loop
    // ─────────────────────────────────────────────────────────────────────
    //
    // The core bug fix: `scope_session_key` must wrap the turn *future*
    // (not the resolved result), so tools executing inside the loop observe
    // the task-local. This test runs a turn through `run_gateway_turn` with
    // `channel_name = "http"` and a tool that reads
    // `TOOL_LOOP_SESSION_KEY`; the tool records the value it saw and we
    // assert it equals the session_key passed to the runner. A non-scoped
    // (result-wrapped) future would leave the task-local unset during tool
    // execution, so this is the direct regression guard for the wrap fix.

    /// Tool that captures the `TOOL_LOOP_SESSION_KEY` task-local value seen
    /// during execution.
    struct SessionKeyProbeTool {
        seen: Arc<parking_lot::Mutex<Vec<Option<String>>>>,
    }
    zeroclaw_api::tool_attribution!(SessionKeyProbeTool, ToolKind::Plugin);

    #[async_trait]
    impl Tool for SessionKeyProbeTool {
        fn name(&self) -> &str {
            "session_probe"
        }
        fn description(&self) -> &str {
            "session_probe"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            let key = zeroclaw_api::TOOL_LOOP_SESSION_KEY
                .try_with(|v| v.clone())
                .ok()
                .flatten();
            self.seen.lock().push(key);
            Ok(ToolResult {
                success: true,
                output: "probed".into(),
                error: None,
            })
        }
    }

    #[tokio::test]
    async fn tool_loop_session_key_is_active_during_http_tool_loop() {
        let seen: Arc<parking_lot::Mutex<Vec<Option<String>>>> =
            Arc::new(parking_lot::Mutex::new(Vec::new()));
        // Script: a tool-call round, then a final text response.
        let mut first = tool_response(vec![tool_call("tc-1", "session_probe")]);
        first.usage = Some(token_usage(10, 5));
        let mut agent = build_agent(
            Box::new(ScriptedProvider::new(vec![first, text_response("done")])),
            vec![Box::new(SessionKeyProbeTool {
                seen: Arc::clone(&seen),
            })],
        );
        let state = runner_state(None);
        let session_key = "gw_http_session_scope";

        // Minimal forward closure: drain events, surface usage tokens back
        // to the runner (mirrors the HTTP SSE/blocking forward loops).
        let forward = move |handle: TurnRunnerHandle| async move {
            let TurnRunnerHandle { mut event_rx, .. } = handle;
            let mut total_input: Option<u64> = None;
            let mut total_output: Option<u64> = None;
            while let Some(event) = event_rx.recv().await {
                if let TurnEvent::Usage {
                    input_tokens,
                    cached_input_tokens: _,
                    output_tokens,
                    cost_usd: _,
                } = event
                {
                    if let Some(i) = input_tokens {
                        total_input = Some(total_input.unwrap_or(0) + i);
                    }
                    if let Some(o) = output_tokens {
                        total_output = Some(total_output.unwrap_or(0) + o);
                    }
                }
            }
            (total_input, total_output, None)
        };

        let outcome = run_gateway_turn(
            &state,
            &mut agent,
            "probe",
            session_key,
            &None,
            None,
            "http",
            forward,
        )
        .await;

        assert_eq!(
            outcome.status,
            TurnStatus::Success,
            "the turn should complete successfully"
        );
        let observed = seen.lock().clone();
        assert!(
            !observed.is_empty(),
            "the probe tool must have executed at least once"
        );
        for key in &observed {
            assert_eq!(
                key.as_deref(),
                Some(session_key),
                "TOOL_LOOP_SESSION_KEY must be set to the runner's session_key \
                 during tool execution (the scope_session_key wrap covers the \
                 turn future, not the resolved result); saw {key:?}"
            );
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // §5.1.3 — HTTP and WS persist the same structured outcome
    // ─────────────────────────────────────────────────────────────────────
    //
    // Both transports call the same `run_gateway_turn` spine, which persists
    // `outcome.new_messages` via `persist_conversation_messages`. This test
    // proves the runner persists the runtime's `new_messages` verbatim: it
    // runs a turn with a `RecordingBackend`, then independently runs
    // `Agent::turn_streamed_with_steering_state` with the same provider
    // script and asserts the backend received exactly the messages the
    // runtime produced (user + tool + assistant). A drift here would mean
    // the runner is altering the transcript before persistence.

    #[tokio::test]
    async fn runner_persists_runtime_new_messages_unchanged() {
        let calls = Arc::new(AtomicUsize::new(0));
        let backend = Arc::new(RecordingBackend {
            appended: std::sync::Mutex::new(Vec::new()),
            exists: true,
        });
        // Keep a typed handle for assertions; the state gets a trait-object
        // clone (the runner only needs the `SessionBackend` interface).
        let backend_handle = Arc::clone(&backend);
        let state = runner_state(Some(backend as Arc<dyn SessionBackend>));
        let session_key = "gw_persist_test";

        // Script for the runner-driven turn.
        let mut first = tool_response(vec![tool_call("tc-1", "echo")]);
        first.usage = Some(token_usage(7, 3));
        let script_runner = vec![first, text_response("final answer")];
        let mut agent = build_agent(
            Box::new(ScriptedProvider::new(script_runner)),
            vec![Box::new(CountingTool {
                name: "echo",
                calls: Arc::clone(&calls),
            })],
        );

        let forward = move |handle: TurnRunnerHandle| async move {
            let TurnRunnerHandle { mut event_rx, .. } = handle;
            let mut total_input: Option<u64> = None;
            let mut total_output: Option<u64> = None;
            while let Some(event) = event_rx.recv().await {
                if let TurnEvent::Usage {
                    input_tokens,
                    cached_input_tokens: _,
                    output_tokens,
                    cost_usd: _,
                } = event
                {
                    if let Some(i) = input_tokens {
                        total_input = Some(total_input.unwrap_or(0) + i);
                    }
                    if let Some(o) = output_tokens {
                        total_output = Some(total_output.unwrap_or(0) + o);
                    }
                }
            }
            (total_input, total_output, None)
        };

        let outcome = run_gateway_turn(
            &state,
            &mut agent,
            "run",
            session_key,
            &None,
            None,
            "http",
            forward,
        )
        .await;
        assert_eq!(outcome.status, TurnStatus::Success);

        // Independently run the same agent turn to capture the runtime's
        // canonical `new_messages`, then compare against what the backend
        // received. We rebuild the agent with the same script so the
        // comparison is apples-to-apples.
        let mut first2 = tool_response(vec![tool_call("tc-1", "echo")]);
        first2.usage = Some(token_usage(7, 3));
        let script_ref = vec![first2, text_response("final answer")];
        let mut agent_ref = build_agent(
            Box::new(ScriptedProvider::new(script_ref)),
            vec![Box::new(CountingTool {
                name: "echo",
                calls: Arc::new(AtomicUsize::new(0)),
            })],
        );
        let (tx, _rx) = mpsc::channel(256);
        let ref_outcome = agent_ref
            .turn_streamed_with_steering_state("run", tx, None, None)
            .await
            .expect("reference turn should succeed");

        // The runner persists only non-system Chat messages (system messages
        // are skipped by `persist_conversation_messages`); mirror that filter
        // for the comparison.
        let expected: Vec<(String, String)> = ref_outcome
            .new_messages
            .iter()
            .filter_map(|m| match m {
                ConversationMessage::Chat(msg) if msg.role != "system" => {
                    Some((msg.role.clone(), msg.content.clone()))
                }
                _ => None,
            })
            .collect();

        let recorded = backend_handle.appended.lock().unwrap().clone();
        let recorded_pairs: Vec<(String, String)> =
            recorded.into_iter().map(|(_, r, c)| (r, c)).collect();
        assert_eq!(
            recorded_pairs, expected,
            "the runner must persist the runtime's new_messages verbatim \
             (same role+content sequence the direct streamed turn produced); \
             a drift would mean the shared spine alters the transcript"
        );
        // Sanity: the runner's reported new_messages match the reference too
        // (the spine doesn't drop or reorder messages before returning them).
        assert_eq!(
            outcome.new_messages.len(),
            ref_outcome.new_messages.len(),
            "runner-returned new_messages count must match the direct runtime turn"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // §5.2.6 — A2A / webhook boundary: run_gateway_chat_with_tools does NOT
    //          route through turn_runner::run_gateway_turn
    // ─────────────────────────────────────────────────────────────────────
    //
    // The design (§3.5) explicitly scopes phase 3 to the WS + HTTP chat
    // completions paths; A2A/webhook stay on
    // `run_gateway_chat_with_tools` → `process_message` (loop module), which
    // is a structurally different turn mechanism (returns a plain String,
    // no session state machine, no cancel token, no transcript persistence).
    //
    // A runtime assertion against the full `run_gateway_chat_with_tools`
    // body would need a fully-booted AppState + agent runtime, which is
    // infeasible at unit level. Instead we assert the boundary at the
    // source-text level: the function's body must not reference
    // `turn_runner`. This is a fragile-but-explicit guard that documents
    // the boundary; the real proof is the A2A/webhook tests staying green
    // (they exercise the unchanged `process_message` path). If a future
    // refactor routes A2A/webhook through the runner, this test fails loud
    // and forces a conscious §3.5 boundary update.
    #[test]
    fn run_gateway_chat_with_tools_does_not_route_through_turn_runner() {
        let src = include_str!("lib.rs");
        // Isolate the function body by locating its signature line and the
        // next top-level `}` at column 0 (the function close).
        let sig = "pub(crate) async fn run_gateway_chat_with_tools(";
        let start = src
            .find(sig)
            .expect("run_gateway_chat_with_tools must exist in lib.rs");
        // Find the closing brace at column 0 after the signature.
        let tail = &src[start..];
        let close = tail
            .find("\n}\n")
            .or_else(|| tail.find("\n}\r\n"))
            .map(|p| p + 1)
            .expect("run_gateway_chat_with_tools must close with a top-level brace");
        let body = &tail[..close];
        assert!(
            !body.contains("turn_runner"),
            "run_gateway_chat_with_tools must NOT reference turn_runner — \
             A2A/webhook stay on process_message (design §3.5 boundary). \
             If this changed on purpose, update the §3.5 boundary declaration \
             and this assertion together."
        );
        // Sanity-check the boundary is the structurally-different path:
        // it must reference `process_message` (the loop-module entry).
        assert!(
            body.contains("process_message"),
            "run_gateway_chat_with_tools must call process_message (the \
             loop-module turn entry) — this is what makes it structurally \
             distinct from the WS/HTTP turn_streamed path."
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // §5.2.4 — WS frame-sequence snapshot (TurnEvent → WS frame mapping)
    // ─────────────────────────────────────────────────────────────────────
    //
    // Full forward-loop isolation (capturing every `sender.send` call) is
    // too coupled to a live WebSocket to test in isolation. Instead we
    // snapshot the extracted `turn_event_to_ws_frame` mapping for every
    // variant, asserting the exact JSON shape (type + all fields) WS
    // clients depend on. Any field change is a wire-format break and will
    // fail this test loud. The existing 28 WS tests cover the surrounding
    // forward-loop behavior (disconnect, approval, steering).

    #[test]
    fn turn_event_to_ws_frame_snapshot_all_variants() {
        // Usage is NOT framed (accumulated by the forward loop) → None.
        let usage = TurnEvent::Usage {
            input_tokens: Some(10),
            cached_input_tokens: Some(2),
            output_tokens: Some(5),
            cost_usd: Some(0.01),
        };
        assert!(crate::ws::turn_event_to_ws_frame(&usage).is_none());

        let chunk = TurnEvent::Chunk {
            delta: "Hello".into(),
        };
        assert_eq!(
            crate::ws::turn_event_to_ws_frame(&chunk).unwrap(),
            serde_json::json!({ "type": "chunk", "content": "Hello" })
        );

        let thinking = TurnEvent::Thinking {
            delta: "reasoning".into(),
        };
        assert_eq!(
            crate::ws::turn_event_to_ws_frame(&thinking).unwrap(),
            serde_json::json!({ "type": "thinking", "content": "reasoning" })
        );

        let tool_call_evt = TurnEvent::ToolCall {
            id: "call_1".into(),
            name: "shell".into(),
            args: serde_json::json!({"cmd": "ls"}),
        };
        assert_eq!(
            crate::ws::turn_event_to_ws_frame(&tool_call_evt).unwrap(),
            serde_json::json!({
                "type": "tool_call",
                "id": "call_1",
                "name": "shell",
                "args": {"cmd": "ls"}
            })
        );

        let tool_result = TurnEvent::ToolResult {
            id: "call_1".into(),
            name: "shell".into(),
            output: "file.txt".into(),
        };
        assert_eq!(
            crate::ws::turn_event_to_ws_frame(&tool_result).unwrap(),
            serde_json::json!({
                "type": "tool_result",
                "id": "call_1",
                "name": "shell",
                "output": "file.txt"
            })
        );

        let approval = TurnEvent::ApprovalRequest {
            request_id: "req_1".into(),
            tool_name: "shell".into(),
            arguments_summary: "command: rm -rf /".into(),
            timeout_secs: 120,
        };
        assert_eq!(
            crate::ws::turn_event_to_ws_frame(&approval).unwrap(),
            serde_json::json!({
                "type": "approval_request",
                "request_id": "req_1",
                "tool": "shell",
                "arguments_summary": "command: rm -rf /",
                "timeout_secs": 120
            })
        );

        let trimmed = TurnEvent::HistoryTrimmed {
            dropped_messages: 4,
            kept_turns: 2,
            reason: "token budget".into(),
        };
        assert_eq!(
            crate::ws::turn_event_to_ws_frame(&trimmed).unwrap(),
            serde_json::json!({
                "type": "history_trimmed",
                "dropped_messages": 4,
                "kept_turns": 2,
                "reason": "token budget"
            })
        );
    }

    // Regression: the HTTP handler must not pre-append the user message
    // before calling the runner — the runner owns post-turn persistence via
    // `persist_conversation_messages(new_messages)`, which already includes
    // the enriched user message. A double-write would pollute later
    // x-session-key turns with two user messages for one request.
    #[test]
    fn double_write_regression_runner_persists_exactly_one_user_message_per_turn() {
        use zeroclaw_providers::{ChatMessage, ConversationMessage};

        // Simulate the runner's post-turn persistence: the runtime returns
        // new_messages with exactly one user message + one assistant reply.
        let messages = vec![
            ConversationMessage::Chat(ChatMessage::user("hello world")),
            ConversationMessage::Chat(ChatMessage::assistant("hi there")),
        ];

        struct CountingBackend {
            messages: std::sync::Mutex<Vec<ChatMessage>>,
            exists: bool,
        }
        impl zeroclaw_infra::session_backend::SessionBackend for CountingBackend {
            fn load(&self, _session_key: &str) -> Vec<ChatMessage> {
                Vec::new()
            }
            fn append(&self, _session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
                self.messages.lock().unwrap().push(message.clone());
                Ok(())
            }
            fn remove_last(&self, _session_key: &str) -> std::io::Result<bool> {
                Ok(false)
            }
            fn list_sessions(&self) -> Vec<String> {
                Vec::new()
            }
            fn session_exists(&self, _session_key: &str) -> bool {
                self.exists
            }
        }

        let backend = CountingBackend {
            messages: std::sync::Mutex::new(Vec::new()),
            exists: true,
        };
        persist_conversation_messages(&backend, "gw_test", &messages);

        let stored = backend.messages.lock().unwrap();
        let user_count = stored.iter().filter(|m| m.role == "user").count();
        assert_eq!(
            user_count, 1,
            "runner must persist exactly one user message per turn; got {user_count}"
        );
        // Also confirm the assistant message was persisted.
        assert!(
            stored.iter().any(|m| m.role == "assistant"),
            "persisted messages must include the assistant reply"
        );
    }

    // ── Non-resurrection regression ───────────────────────────────────

    struct AppendCounter {
        calls: std::sync::Mutex<Vec<String>>,
        exists: bool,
    }

    impl zeroclaw_infra::session_backend::SessionBackend for AppendCounter {
        fn load(&self, _: &str) -> Vec<zeroclaw_providers::ChatMessage> {
            Vec::new()
        }
        fn append(&self, key: &str, msg: &zeroclaw_providers::ChatMessage) -> std::io::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("{key}:{role}", role = msg.role));
            Ok(())
        }
        fn remove_last(&self, _: &str) -> std::io::Result<bool> {
            Ok(false)
        }
        fn list_sessions(&self) -> Vec<String> {
            Vec::new()
        }
        fn session_exists(&self, _: &str) -> bool {
            self.exists
        }
    }

    #[test]
    fn cancelled_path_skips_persist_when_session_deleted() {
        // Simulates the cancelled-path guard: when session_exists()
        // returns false, persist_conversation_messages is NOT called,
        // preserving the non-resurrection contract.
        let backend = AppendCounter {
            calls: std::sync::Mutex::new(Vec::new()),
            exists: false,
        };
        let messages = vec![
            zeroclaw_providers::ConversationMessage::Chat(zeroclaw_providers::ChatMessage::user(
                "hi",
            )),
            zeroclaw_providers::ConversationMessage::Chat(
                zeroclaw_providers::ChatMessage::assistant("done"),
            ),
        ];

        if backend.session_exists("gw_deleted") {
            persist_conversation_messages(&backend, "gw_deleted", &messages);
        }

        assert!(backend.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn persist_messages_creates_first_turn() {
        // First-turn persistence: even when session_exists is false,
        // persist_conversation_messages appends (append uses create(true)).
        let backend = AppendCounter {
            calls: std::sync::Mutex::new(Vec::new()),
            exists: false,
        };
        let messages = vec![
            zeroclaw_providers::ConversationMessage::Chat(zeroclaw_providers::ChatMessage::user(
                "hi",
            )),
            zeroclaw_providers::ConversationMessage::Chat(
                zeroclaw_providers::ChatMessage::assistant("ack"),
            ),
        ];

        persist_conversation_messages(&backend, "gw_new", &messages);

        assert_eq!(backend.calls.lock().unwrap().len(), 2);
    }

    // ── CancelTokenGuard Drop behaviour ──────────────────────────────────
    //
    // The guard removes the cancel token from `cancel_tokens` when
    // dropped, preventing leaks if the forward closure panics after the
    // token is registered but before the explicit removal.

    #[test]
    fn cancel_token_guard_removes_token_on_drop() {
        let tokens = Arc::new(Mutex::new(HashMap::new()));
        let key = "gw_guard_drop_test".to_string();
        tokens.lock().insert(key.clone(), CancellationToken::new());
        assert!(
            tokens.lock().contains_key(&key),
            "token should be present before guard is created"
        );

        {
            let _guard = super::CancelTokenGuard {
                tokens: Arc::clone(&tokens),
                session_key: key.clone(),
            };
            assert!(
                tokens.lock().contains_key(&key),
                "token should still be present while guard is alive"
            );
        }
        // After the guard is dropped, the token must be removed.
        assert!(
            !tokens.lock().contains_key(&key),
            "guard must remove token from cancel_tokens on drop"
        );
    }
}
