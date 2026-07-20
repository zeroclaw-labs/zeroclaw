//! The agent turn engine, decomposed into single-purpose step modules.

pub(crate) mod approval_gate;
pub(crate) mod call_prep;
pub(crate) mod context;
pub(crate) mod context_recovery;
pub(crate) mod delivery_defaults;
pub(crate) mod events;
pub(crate) mod execution;
pub(crate) mod history_append;
pub(crate) mod history_window;
pub(crate) mod knobs;
pub(crate) mod max_iter;
pub(crate) mod outcome;
pub(crate) mod parse_response;
pub(crate) mod post_exec;
pub(crate) mod protocol_detect;
pub(crate) mod provider_call;
pub(crate) mod redact;
pub(crate) mod results_collect;
pub(crate) mod steering;
pub(crate) mod stream_consume;
pub(crate) mod stream_guard;
pub(crate) mod tool_specs;
pub(crate) mod vision_route;

pub(crate) use call_prep::{PreparedToolCalls, prepare_tool_calls};
pub(crate) use context::{TurnCtx, TurnMeta};
pub(crate) use context_recovery::{record_llm_failure, try_recover_context_overflow};
#[cfg(test)]
pub(crate) use delivery_defaults::maybe_inject_channel_delivery_defaults;
pub use events::{DraftEvent, PROGRESS_MIN_INTERVAL_MS, StreamDelta};
pub use execution::{
    ResolvedAgentExecution, ResolvedIo, ResolvedModelAccess, ResolvedRuntimeKnobs,
};
pub(crate) use history_append::append_tool_round_to_history;
pub(crate) use history_window::preflight_history_maintenance;
pub use knobs::{LoopKnobs, MaxIterationBehavior};
pub(crate) use max_iter::finish_after_max_iterations;
pub(crate) use outcome::StreamCancelledAfterOutput;
pub use outcome::{
    ModelSwitchCallback, ModelSwitchRequested, ToolLoopCancelled, is_model_switch_requested,
    is_tool_loop_cancelled,
};
#[cfg(test)]
pub(crate) use parse_response::build_native_assistant_history;
pub(crate) use parse_response::{
    interpret_chat_response, resolve_display_text, unforwarded_narration,
};
pub(crate) use post_exec::record_executed_outcomes;
pub(crate) use provider_call::{
    ProviderCallOutcome, announce_llm_request, call_provider, enforce_tool_loop_budget,
};
pub use redact::scrub_credentials;
pub(crate) use results_collect::{
    CollectedResults, check_identical_output_abort, collect_tool_results,
};
pub use steering::drain_steering_messages;
#[cfg(test)]
pub(crate) use stream_consume::consume_provider_streaming_response;
pub(crate) use tool_specs::{IterationToolSpecs, build_iteration_tool_specs};
pub(crate) use vision_route::{prepare_messages_for_iteration, resolve_vision_provider};

use crate::agent::system_prompt::{NATIVE_TOOLS_TASK_FRAMING, NO_TOOLS_TASK_FRAMING};
use crate::agent::tool_execution::{
    ToolDispatchContext, execute_tools_parallel, execute_tools_sequential,
    should_execute_tools_in_parallel,
};
use crate::security::ingress::{IngressPolicy, ingress_policy};
use crate::util::truncate_with_ellipsis;
use anyhow::Result;
use std::collections::HashSet;
use std::io::Write as _;
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;
use zeroclaw_api::agent::TurnEvent;
use zeroclaw_api::channel::Channel;
use zeroclaw_api::ingress::{IngressContext, IngressDecision};
use zeroclaw_providers::{ChatMessage, ModelProvider};

/// Maximum malformed internal tool-protocol retries before returning a safe fallback.
pub(crate) const MAX_MALFORMED_TOOL_PROTOCOL_RETRIES: usize = 2;

/// Default maximum agentic tool-use iterations per user message to prevent runaway loops.
/// Used as a safe fallback when `max_tool_iterations` is unset or configured as zero.
pub(crate) const DEFAULT_MAX_TOOL_ITERATIONS: usize = 10;

pub struct ToolLoopWithCurrentTurn<'a> {
    /// The resolved per-agent execution context: model binding, gated tool
    /// registry, approval, observability, and resolved runtime knobs. Stable
    /// for every turn to this agent; built once and reused. See
    /// [`ResolvedAgentExecution`]. Everything below is per-message turn state.
    pub exec: ResolvedAgentExecution<'a>,
    pub history: &'a mut Vec<ChatMessage>,
    pub current_turn: &'a mut Vec<ChatMessage>,
    pub channel_name: &'a str,
    pub channel_reply_target: Option<&'a str>,
    pub cancellation_token: Option<CancellationToken>,
    pub on_delta: Option<tokio::sync::mpsc::Sender<DraftEvent>>,
    pub shared_budget: Option<Arc<std::sync::atomic::AtomicUsize>>,
    pub channel: Option<&'a dyn Channel>,
    pub collected_receipts: Option<&'a std::sync::Mutex<Vec<String>>>,
    pub event_tx: Option<tokio::sync::mpsc::Sender<TurnEvent>>,
    pub steering: Option<&'a mut tokio::sync::mpsc::Receiver<String>>,
    pub image_cache: Option<&'a mut zeroclaw_providers::multimodal::LocalImageCache>,
    pub ingress: IngressContext,
    /// The per-turn memory half for unified memory-context injection: the
    /// handle, raw recall query, session scopes, and spawn-site suppression.
    /// `None` for nested sub-turn sites and paths without a memory backend;
    /// the injection decision itself is keyed on `ingress.origin`.
    pub memory: Option<crate::agent::memory_inject::TurnMemory<'a>>,
    /// Observer metadata: agent alias and turn id, stamped onto every
    /// turn-level observer event so OTel spans correlate across the loop.
    pub agent_alias: Option<&'a str>,
    pub turn_id: &'a str,
}

pub struct ToolLoop<'a> {
    /// The resolved per-agent execution context: model binding, gated tool
    /// registry, approval, observability, and resolved runtime knobs. Stable
    /// for every turn to this agent; built once and reused. See
    /// [`ResolvedAgentExecution`]. Everything below is per-message turn state.
    pub exec: ResolvedAgentExecution<'a>,
    /// Full conversation transcript for this turn.
    ///
    /// **Contract**: the last `user`-role message in this history MUST be the
    /// real user message for the current turn. The compatibility wrapper uses
    /// this as the boundary between the durable past-turn prefix and the
    /// mutable current-turn working set. Callers that retry (e.g. on
    /// model-switch) should use [`run_tool_call_loop_with_current_turn`]
    /// directly so the split boundary stays stable without re-discovery.
    pub history: &'a mut Vec<ChatMessage>,
    pub channel_name: &'a str,
    pub channel_reply_target: Option<&'a str>,
    pub cancellation_token: Option<CancellationToken>,
    pub on_delta: Option<tokio::sync::mpsc::Sender<DraftEvent>>,
    pub shared_budget: Option<Arc<std::sync::atomic::AtomicUsize>>,
    pub channel: Option<&'a dyn Channel>,
    pub collected_receipts: Option<&'a std::sync::Mutex<Vec<String>>>,
    pub event_tx: Option<tokio::sync::mpsc::Sender<TurnEvent>>,
    pub steering: Option<&'a mut tokio::sync::mpsc::Receiver<String>>,
    pub image_cache: Option<&'a mut zeroclaw_providers::multimodal::LocalImageCache>,
    pub ingress: IngressContext,
    /// The per-turn memory half for unified memory-context injection: the
    /// handle, raw recall query, session scopes, and spawn-site suppression.
    /// `None` for nested sub-turn sites and paths without a memory backend;
    /// the injection decision itself is keyed on `ingress.origin`.
    pub memory: Option<crate::agent::memory_inject::TurnMemory<'a>>,
    /// Observer metadata: agent alias and turn id, stamped onto every
    /// turn-level observer event so OTel spans correlate across the loop.
    pub agent_alias: Option<&'a str>,
    pub turn_id: &'a str,
}

async fn enforce_reported_budget(
    history: &mut Vec<ChatMessage>,
    reported_input_tokens: usize,
    context_token_budget: usize,
    event_tx: Option<&tokio::sync::mpsc::Sender<TurnEvent>>,
    observer: &dyn crate::observability::Observer,
) {
    if context_token_budget == 0 || reported_input_tokens <= context_token_budget {
        return;
    }
    let taken = std::mem::take(history);
    let result = crate::agent::history_trim::trim_to_reported_budget(
        taken,
        context_token_budget,
        reported_input_tokens,
    );
    if result.trimmed {
        let mut trimmed = result.history;
        crate::agent::history_trim::insert_breadcrumb_deduped(&mut trimmed);
        *history = trimmed;
        if let Some(tx) = event_tx {
            let _ = tx
                .send(TurnEvent::HistoryTrimmed {
                    dropped_messages: result.dropped_messages,
                    kept_turns: result.kept_turns,
                    reason: crate::i18n::get_required_cli_string("history-trim-reason-budget"),
                })
                .await;
        }
        observer.record_event(
            &zeroclaw_api::observability_traits::ObserverEvent::HistoryTrimmed {
                dropped_messages: result.dropped_messages,
                kept_turns: result.kept_turns,
                reason: crate::i18n::get_required_cli_string("history-trim-reason-budget"),
                channel: None,
                agent_alias: None,
                turn_id: None,
            },
        );
    } else {
        *history = result.history;
    }
}

pub async fn run_tool_call_loop_with_current_turn(
    p: ToolLoopWithCurrentTurn<'_>,
) -> Result<String> {
    let ToolLoopWithCurrentTurn {
        exec,
        history,
        current_turn,
        channel_name,
        channel_reply_target,
        cancellation_token,
        on_delta,
        shared_budget,
        channel,
        collected_receipts,
        event_tx,
        mut steering,
        mut image_cache,
        ingress,
        memory,
        agent_alias,
        turn_id,
    } = p;
    let ResolvedAgentExecution {
        model_access:
            ResolvedModelAccess {
                model_provider,
                provider_name,
                model,
                temperature,
            },
        tools_registry,
        observer,
        silent,
        approval,
        multimodal_config,
        max_tool_iterations,
        hooks,
        excluded_tools,
        dedup_exempt_tools,
        activated_tools,
        model_switch_callback,
        pacing,
        strict_tool_parsing,
        parallel_tools,
        max_tool_result_chars,
        context_token_budget,
        receipt_generator,
        knobs,
    } = exec;

    let ingress_policy_cfg = IngressPolicy::default();
    let p1_text = current_turn
        .iter()
        .find(|m| m.role == "user")
        .or_else(|| history.iter().rev().find(|m| m.role == "user"))
        .map_or("", |m| m.content.as_str());
    match ingress_policy(p1_text, &ingress, &ingress_policy_cfg) {
        // DEFAULT — the only arm reachable under the default policy. Proceed
        // into the loop exactly as today.
        IngressDecision::Loop => {}
        // Phase 3: wrap the message as untrusted data before it enters history.
        // Until framing exists, proceed as Loop (behavior-identical).
        IngressDecision::Annotate { .. } => {}
        // Phase 2: divert the turn into a managed SOP run instead of the loop.
        // Not reachable under the default policy; proceed-as-loop for now.
        IngressDecision::Gate { .. } => {
            // TODO(PR C): hand this turn to the SOP run the gate names.
        }
        // Not reachable under the default policy; refuse the turn when it is.
        IngressDecision::Drop { ref reason } => {
            return Ok(crate::i18n::get_required_cli_string_with_args(
                "turn-ingress-dropped",
                &[("reason", reason.as_str())],
            ));
        }
    }

    let precomputed_memory_context: Option<String> = if let Some(turn_memory) = &memory
        && let crate::agent::memory_inject::InjectPolicy::Inject {
            exclude_conversation,
        } = crate::agent::memory_inject::resolve_inject_policy(
            ingress.origin,
            turn_memory.sessions.iter().any(Option::is_some),
            turn_memory.suppress,
        ) {
        let scopes: Vec<Option<&str>> = turn_memory.sessions.iter().map(|s| s.as_deref()).collect();
        let context = crate::agent::memory_inject::render_memory_context(
            turn_memory.handle,
            observer,
            &turn_memory.query,
            &scopes,
            &turn_memory.cfg,
            exclude_conversation,
        )
        .await;
        if context.is_empty() {
            None
        } else {
            Some(context)
        }
    } else {
        None
    };

    let max_iterations = if max_tool_iterations == 0 {
        DEFAULT_MAX_TOOL_ITERATIONS
    } else {
        max_tool_iterations
    };

    let loop_started_at = Instant::now();
    let loop_ignore_tools: HashSet<&str> = pacing
        .loop_ignore_tools
        .iter()
        .map(String::as_str)
        .collect();
    let mut consecutive_identical_outputs: usize = 0;
    let mut last_tool_output_hash: Option<u64> = None;

    let mut loop_detector = crate::agent::loop_detector::LoopDetector::new(
        crate::agent::loop_detector::LoopDetectorConfig {
            enabled: pacing.loop_detection_enabled,
            window_size: pacing.loop_detection_window_size,
            max_repeats: pacing.loop_detection_max_repeats,
        },
    );

    // Accumulated display text across all tool-loop calls.
    let mut accumulated_display_text = String::new();
    let mut malformed_tool_protocol_retries: usize = 0;
    let mut prompt_approval_tool_signatures: HashSet<(String, String)> = HashSet::new();

    // Shared-ref context for the turn step functions. Every `&mut` the loop
    // owns stays a loop local passed as an explicit argument (RUN_SHEET
    // `turn.context.TurnCtx`).
    let ctx = TurnCtx {
        observer,
        provider_name,
        model,
        temperature,
        approval,
        channel_name,
        channel_reply_target,
        cancellation_token: cancellation_token.as_ref(),
        on_delta: on_delta.as_ref(),
        event_tx: event_tx.as_ref(),
        hooks,
        dedup_exempt_tools,
        pacing,
        strict_tool_parsing,
        channel,
        turn_id,
        agent_alias,
    };

    for iteration in 0..max_iterations {
        for steering_message in drain_steering_messages(&mut steering) {
            match ingress_policy(&steering_message, &ingress, &ingress_policy_cfg) {
                // DEFAULT — append the injection to the current turn exactly as today.
                IngressDecision::Loop => {}
                // Phase 3: frame as untrusted data; proceed as Loop until
                // framing exists (behavior-identical).
                IngressDecision::Annotate { .. } => {}
                // Phase 2: divert this injection into the SOP run rather than
                // the current turn. Not reachable under the default policy.
                IngressDecision::Gate { .. } => {
                    // TODO(PR C): route this steering message into the gated
                    // SOP run instead of appending it to the current turn.
                }
                // Not reachable under the default policy; drop the injection
                // (do not append it) when it is.
                IngressDecision::Drop { .. } => continue,
            }
            let msg = ChatMessage::user(steering_message);
            current_turn.push(msg);
        }

        let mut seen_tool_signatures: HashSet<(String, String)> = HashSet::new();

        if cancellation_token
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(ToolLoopCancelled.into());
        }

        // Shared iteration budget: parent + subagents share a global counter
        if let Some(ref budget) = shared_budget {
            let remaining = budget.load(std::sync::atomic::Ordering::Relaxed);
            if remaining == 0 {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_category(::zeroclaw_log::EventCategory::Agent)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"iteration": iteration})),
                    "Shared iteration budget exhausted at iteration"
                );
                break;
            }
            budget.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }

        preflight_history_maintenance(history);

        if iteration == 0 && context_token_budget > 0 {
            let system_floor = crate::agent::history::estimate_system_floor_tokens(history);
            if system_floor >= context_token_budget {
                let __zc_floor_span = ::zeroclaw_log::info_span!(
                    target: "zeroclaw_log_internal_scope",
                    "zeroclaw_scope",
                    model = %model,
                    model_provider = %provider_name,
                );
                let _zc_floor_guard = __zc_floor_span.entered();
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Agent)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "system_floor": system_floor,
                            "budget": context_token_budget,
                            "error_key": "context_floor_exceeds_budget",
                        })),
                    crate::agent::history::context_floor_remediation(
                        system_floor,
                        context_token_budget,
                    )
                );
            }
            // Reserve budget for `current_turn` before trimming `history`:
            // the provider sees `history + current_turn`, so trimming must
            // leave enough headroom for the active turn's tokens.
            let current_turn_estimate =
                crate::agent::history::estimate_history_tokens(current_turn);
            let history_budget = context_token_budget.saturating_sub(current_turn_estimate);
            let taken = std::mem::take(history);
            let result = crate::agent::history_trim::trim_to_recent_turns(taken, history_budget);
            if result.trimmed {
                let mut trimmed = result.history;
                crate::agent::history_trim::insert_breadcrumb_deduped(&mut trimmed);
                *history = trimmed;
                {
                    let __zc_trim_span = ::zeroclaw_log::info_span!(
                        target: "zeroclaw_log_internal_scope",
                        "zeroclaw_scope",
                        model = %model,
                        model_provider = %provider_name,
                    );
                    let _zc_trim_guard = __zc_trim_span.entered();
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Delete)
                            .with_category(::zeroclaw_log::EventCategory::Agent)
                            .with_attrs(::serde_json::json!({
                                "dropped_messages": result.dropped_messages,
                                "dropped_turns": result.dropped_turns,
                                "kept_turns": result.kept_turns,
                                "budget_tokens": context_token_budget,
                                "tokens_before": result.tokens_before,
                                "tokens_after": result.tokens_after,
                                "tokens_reclaimed": result.tokens_before.saturating_sub(result.tokens_after),
                                "budget_headroom": context_token_budget.saturating_sub(result.tokens_after),
                            })),
                        format!(
                            "History trimmed: dropped {} oldest turn(s) ({} msgs), {} -> {} tok (budget {}), reclaimed {} tok",
                            result.dropped_turns,
                            result.dropped_messages,
                            result.tokens_before,
                            result.tokens_after,
                            context_token_budget,
                            result.tokens_before.saturating_sub(result.tokens_after)
                        )
                    );
                }
                if let Some(tx) = event_tx.as_ref() {
                    let _ = tx
                        .send(TurnEvent::HistoryTrimmed {
                            dropped_messages: result.dropped_messages,
                            kept_turns: result.kept_turns,
                            reason: crate::i18n::get_required_cli_string(
                                "history-trim-reason-budget",
                            ),
                        })
                        .await;
                }
                observer.record_event(
                    &zeroclaw_api::observability_traits::ObserverEvent::HistoryTrimmed {
                        dropped_messages: result.dropped_messages,
                        kept_turns: result.kept_turns,
                        reason: crate::i18n::get_required_cli_string("history-trim-reason-budget"),
                        channel: None,
                        agent_alias: None,
                        turn_id: None,
                    },
                );
            } else {
                *history = result.history;
            }
        }

        // Check if model switch was requested via model_switch tool
        if let Some(ref callback) = model_switch_callback
            && let Ok(guard) = callback.lock()
            && let Some((new_model_provider, new_model)) = guard.as_ref()
            && (new_model_provider != provider_name || new_model != model)
        {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Migrate)
                    .with_category(::zeroclaw_log::EventCategory::Provider),
                &format!(
                    "Model switch detected: {} {} -> {} {}",
                    provider_name, model, new_model_provider, new_model
                )
            );
            return Err(ModelSwitchRequested {
                model_provider: new_model_provider.clone(),
                model: new_model.clone(),
            }
            .into());
        }

        let mut iteration_tool_specs = build_iteration_tool_specs(
            model_provider,
            tools_registry,
            excluded_tools,
            activated_tools,
        )?;

        // Prepend the once-per-invocation memory context to the current turn's
        // seed user message before merging into the provider-visible transcript.
        // The context is rendered once before the loop (above); here we only
        // splice it into a fresh clone so the durable `current_turn` stays clean.
        // Anchoring to the first user message avoids mis-injecting into steering
        // messages appended later in the same round.
        let mut current_turn_with_memory: Vec<ChatMessage> = current_turn.clone();
        if let Some(ref context) = precomputed_memory_context
            && let Some(seed_user_idx) = current_turn_with_memory
                .iter()
                .position(|m| m.role == "user")
        {
            let existing = &current_turn_with_memory[seed_user_idx].content;
            current_turn_with_memory[seed_user_idx].content = format!("{context}{existing}");
        }

        // Build the provider-visible merged transcript. A temporary owned Vec is
        // required because downstream consumers need a contiguous slice and
        // `current_turn` must remain mutable for assistant/tool-result appends
        // after the provider call returns.
        let mut merged: Vec<ChatMessage> =
            Vec::with_capacity(history.len() + current_turn_with_memory.len());
        merged.extend(history.iter().cloned());
        merged.extend(current_turn_with_memory);

        let (vision_model_provider_box, degrade_strip_images) =
            resolve_vision_provider(model_provider, &merged, multimodal_config, provider_name)?;

        let (active_model_provider, active_model_provider_name, active_model): (
            &dyn ModelProvider,
            &str,
            &str,
        ) = if let Some(ref vp_box) = vision_model_provider_box {
            let vp_name = multimodal_config
                .vision_model_provider
                .as_deref()
                .unwrap_or(provider_name);
            let vm = multimodal_config.vision_model.as_deref().unwrap_or(model);
            (vp_box.as_ref(), vp_name, vm)
        } else {
            (model_provider, provider_name, model)
        };
        iteration_tool_specs.refresh_native_tool_mode(active_model_provider);
        let IterationToolSpecs {
            ref tool_specs,
            use_native_tools,
            ..
        } = iteration_tool_specs;

        refresh_prompt_anchor(history, use_native_tools);
        if let Some(refreshed) = history.first() {
            merged[0] = refreshed.clone();
        }

        let prepared_messages = prepare_messages_for_iteration(
            &merged,
            multimodal_config,
            degrade_strip_images,
            image_cache.as_deref_mut(),
        )
        .await?;

        let llm_started_at = announce_llm_request(
            &ctx,
            &merged,
            active_model_provider,
            active_model_provider_name,
            active_model,
            iteration,
        )
        .await;

        enforce_tool_loop_budget()?;

        // Unified path via ModelProvider::chat so provider-specific native tool logic
        // (OpenAI/Anthropic/OpenRouter/compatible adapters) is honored.
        let request_tools = if use_native_tools {
            Some(tool_specs.as_slice())
        } else {
            None
        };
        let request_tool_count = request_tools.map_or(0, <[crate::tools::ToolSpec]>::len);
        let base_provider_supports_native_tools = model_provider.supports_native_tools();
        let active_provider_supports_native_tools = active_model_provider.supports_native_tools();
        let active_provider_supports_streaming = active_model_provider.supports_streaming();
        let active_provider_supports_streaming_tool_events =
            active_model_provider.supports_streaming_tool_events();
        let should_consume_provider_stream = (on_delta.is_some() || event_tx.is_some())
            && active_provider_supports_streaming
            && (request_tools.is_none() || active_provider_supports_streaming_tool_events);
        if ::zeroclaw_log::debug_enabled() {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_attrs(::serde_json::json!({
                        "has_on_delta": on_delta.is_some(),
                        "has_event_tx": event_tx.is_some(),
                        "base_provider_supports_native_tools": base_provider_supports_native_tools,
                        "active_provider_supports_native_tools": active_provider_supports_native_tools,
                        "active_provider_supports_streaming": active_provider_supports_streaming,
                        "active_provider_supports_streaming_tool_events": active_provider_supports_streaming_tool_events,
                        "tool_specs_count": tool_specs.len(),
                        "request_tools_count": request_tool_count,
                        "use_native_tools": use_native_tools,
                        "should_consume_provider_stream": should_consume_provider_stream,
                    })),
                &format!("native tool delivery decision for iteration {}", iteration + 1)
            );
        }

        let ProviderCallOutcome {
            chat_result,
            streamed_live_deltas,
            streamed_protocol_suppressed,
            streamed_visible_text,
        } = call_provider(
            &ctx,
            active_model_provider,
            active_model,
            &prepared_messages.messages,
            request_tools,
            should_consume_provider_stream,
            iteration,
        )
        .await?;

        let (
            response_text,
            parsed_text,
            tool_calls,
            assistant_history_content,
            native_tool_calls,
            parse_issue_detected,
            protocol_suppressed,
            response_streamed_live,
            reported_input_tokens,
        ) = match chat_result {
            Ok(resp) => {
                let interpreted = interpret_chat_response(
                    &ctx,
                    resp,
                    &prepared_messages.messages,
                    &iteration_tool_specs,
                    streamed_protocol_suppressed,
                    llm_started_at,
                    iteration,
                    knobs.detect_protocol_without_tools,
                )
                .await;
                (
                    interpreted.response_text,
                    interpreted.parsed_text,
                    interpreted.tool_calls,
                    interpreted.assistant_history_content,
                    interpreted.native_tool_calls,
                    interpreted.parse_issue_detected,
                    streamed_protocol_suppressed,
                    streamed_live_deltas,
                    interpreted.input_tokens,
                )
            }
            Err(e) => {
                record_llm_failure(&ctx, llm_started_at, iteration, &e);
                let recovered = try_recover_context_overflow(
                    history,
                    &e,
                    iteration,
                    event_tx.as_ref(),
                    observer,
                    context_token_budget,
                )
                .await;
                if recovered {
                    continue;
                }
                // A stream that died after caller-visible output: persist the
                // partial with the interruption marker so wrappers/channels
                // can commit what the consumer already saw.
                if let Some(interrupted) = e.downcast_ref::<outcome::StreamInterruptedAfterOutput>()
                    && !interrupted.partial_text.is_empty()
                {
                    let msg = ChatMessage::assistant(format!(
                        "{}\n\n{}",
                        interrupted.partial_text,
                        crate::i18n::get_required_cli_string("turn-stream-interrupted")
                    ));
                    current_turn.push(msg);
                }
                // Same for a user cancel after visible streamed output —
                // the pre-consolidation streaming engine committed the
                // watched partial with this exact marker.
                if let Some(cancelled) = e.downcast_ref::<outcome::StreamCancelledAfterOutput>()
                    && !cancelled.partial_text.is_empty()
                {
                    let msg = ChatMessage::assistant(format!(
                        "{}\n\n{}",
                        cancelled.partial_text,
                        crate::i18n::get_required_cli_string("turn-interrupted-by-user")
                    ));
                    current_turn.push(msg);
                }
                return Err(e);
            }
        };

        let display_text = resolve_display_text(
            &response_text,
            &parsed_text,
            !tool_calls.is_empty(),
            !native_tool_calls.is_empty(),
        );

        // Native provider tool_calls are converted into parsed `tool_calls`
        // above; if this branch is reached there is no valid native call to run.
        if tool_calls.is_empty() && parse_issue_detected {
            malformed_tool_protocol_retries += 1;
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_category(::zeroclaw_log::EventCategory::Provider)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(serde_json::json!({
                        "channel": channel_name,
                        "model_provider": provider_name,
                        "model": model,
                        "trace_id": turn_id,
                        "error": "malformed internal tool protocol omitted from channel output",
                    })),
                "tool_call_parse_feedback"
            );
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_category(::zeroclaw_log::EventCategory::Provider)
                    .with_attrs(serde_json::json!({
                    "iteration": iteration + 1,
                    "retry": malformed_tool_protocol_retries,
                    "max_retries": MAX_MALFORMED_TOOL_PROTOCOL_RETRIES,
                    "response_excerpt": truncate_with_ellipsis(
                        &scrub_credentials(&response_text),
                        600
                    ),
                    })),
                "tool_call_parse_feedback_details"
            );

            if malformed_tool_protocol_retries <= MAX_MALFORMED_TOOL_PROTOCOL_RETRIES {
                // This is model feedback, not a tool result: malformed protocol
                // output has no valid tool_call_id to attach a role=tool message to.
                let msg = ChatMessage::user(
                    "[Tool call parse error]\n\
                     Your previous response looked like an internal tool-call protocol payload, \
                     but ZeroClaw could not parse it into a valid tool call. Use the supported \
                     tool-call schema, or answer in natural language if no tool is needed."
                        .to_string(),
                );
                current_turn.push(msg);
                continue;
            }

            let fallback =
                crate::i18n::get_required_cli_string("channel-runtime-malformed-tool-output");
            accumulated_display_text.push_str(&fallback);
            if let Some(ref tx) = on_delta {
                let _ = tx.send(StreamDelta::Text(fallback.to_string())).await;
            }
            let msg = ChatMessage::assistant(fallback.to_string());
            current_turn.push(msg);
            return Ok(accumulated_display_text);
        }

        // ── Progress: LLM responded ─────────────────────────────
        if let Some(ref tx) = on_delta {
            let llm_secs = llm_started_at.elapsed().as_secs();
            if !tool_calls.is_empty() {
                let _ = tx
                    .send(StreamDelta::Status(format!(
                        "\u{1f4ac} Got {} tool call(s) ({llm_secs}s)\n",
                        tool_calls.len()
                    )))
                    .await;
            }
        }

        if tool_calls.is_empty() {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)
                    .with_category(::zeroclaw_log::EventCategory::Agent)
                    .with_outcome(::zeroclaw_log::EventOutcome::Success)
                    .with_attrs(::serde_json::json!({
                        "model": model,
                        "iteration": iteration + 1,
                        "text": scrub_credentials(&display_text),
                        "trace_id": turn_id,
                    })),
                "turn_final_response"
            );
            // No tool calls — this is the final response.
            accumulated_display_text.push_str(&display_text);

            // If text wasn't streamed live, send it now post-hoc. Gated on
            // event_tx independently of on_delta (never nested — §8.4).
            if !response_streamed_live && !protocol_suppressed {
                events::emit_posthoc_turn_chunk(event_tx.as_ref(), &display_text).await;
            }

            // If text wasn't streamed live, send it now via post-hoc chunking.
            // When streamed live, the channel already received the deltas.
            if let Some(ref tx) = on_delta
                && !response_streamed_live
                && !protocol_suppressed
            {
                events::stream_text_posthoc_chunks(tx, &display_text, cancellation_token.as_ref())
                    .await?;
            }

            let msg = ChatMessage::assistant(response_text.clone());
            current_turn.push(msg);
            if let Some(reported) = reported_input_tokens {
                // `reported` covers history + current_turn, but the trimmer
                // only sees history. Subtract current_turn's estimated token
                // contribution so the scaled budget is not underestimated.
                let ct_est = crate::agent::history::estimate_history_tokens(current_turn);
                enforce_reported_budget(
                    history,
                    (reported as usize).saturating_sub(ct_est),
                    context_token_budget,
                    event_tx.as_ref(),
                    observer,
                )
                .await;
            }
            return Ok(accumulated_display_text);
        }

        // Relay only the portion of narration the live stream did not already
        // deliver: re-sending the whole thing duplicates it.
        if !display_text.is_empty() {
            // `protocol_suppressed` withholds the whole turn; the empty-remainder
            // skip below handles the guard-passed case where the live stream already forwarded every byte.
            if !native_tool_calls.is_empty()
                && !protocol_suppressed
                && let Some(ref tx) = on_delta
            {
                let remainder = unforwarded_narration(&display_text, &streamed_visible_text);
                if !remainder.is_empty() {
                    let mut narration = remainder.to_string();
                    if !narration.ends_with('\n') {
                        narration.push('\n');
                    }
                    let _ = tx.send(StreamDelta::Text(narration)).await;
                }
            }
            if !silent {
                eprint!("{display_text}");
                let _ = std::io::stderr().flush();
            }
        }

        // When multiple tool calls are present and interactive CLI approval is not needed, run
        // tool executions concurrently for lower wall-clock latency.
        let allow_parallel_execution =
            parallel_tools && should_execute_tools_in_parallel(&tool_calls, approval);
        let PreparedToolCalls {
            mut ordered_results,
            executable_indices,
            executable_calls,
        } = prepare_tool_calls(
            &ctx,
            &tool_calls,
            &mut seen_tool_signatures,
            &mut prompt_approval_tool_signatures,
            iteration,
            knobs.dedup_enabled,
        )
        .await?;

        let live_sop_queue = crate::sop::executor::new_live_action_queue();
        let execution_result =
            crate::sop::executor::scope_live_action_queue(live_sop_queue.clone(), async {
                if allow_parallel_execution && executable_calls.len() > 1 {
                    let meta = ctx.meta();
                    let dispatch = ToolDispatchContext {
                        tools_registry,
                        activated_tools,
                        excluded_tools,
                    };
                    execute_tools_parallel(
                        &executable_calls,
                        dispatch,
                        &meta,
                        observer,
                        cancellation_token.as_ref(),
                        receipt_generator,
                        ctx.event_tx,
                    )
                    .await
                } else {
                    let meta = ctx.meta();
                    let dispatch = ToolDispatchContext {
                        tools_registry,
                        activated_tools,
                        excluded_tools,
                    };
                    execute_tools_sequential(
                        &executable_calls,
                        dispatch,
                        &meta,
                        observer,
                        cancellation_token.as_ref(),
                        receipt_generator,
                        ctx.event_tx,
                    )
                    .await
                }
            })
            .await;
        let executed_slots = match execution_result {
            Ok(slots) => slots,
            Err(e) if is_tool_loop_cancelled(&e) => {
                (0..executable_calls.len()).map(|_| None).collect()
            }
            Err(e) => return Err(e),
        };

        let cancelled_mid_batch = executed_slots.iter().any(Option::is_none);

        let mut executed_completed_indices: Vec<usize> = Vec::new();
        let mut executed_completed_calls = Vec::new();
        let mut executed_completed_outcomes = Vec::new();
        for (slot, (call_idx, call)) in executed_slots.into_iter().zip(
            executable_indices
                .iter()
                .copied()
                .zip(executable_calls.iter()),
        ) {
            if let Some(outcome) = slot {
                executed_completed_indices.push(call_idx);
                executed_completed_calls.push(call.clone());
                executed_completed_outcomes.push(outcome);
            }
        }

        record_executed_outcomes(
            &ctx,
            &executed_completed_indices,
            &executed_completed_calls,
            executed_completed_outcomes,
            &mut ordered_results,
            iteration,
        )
        .await;
        if cancelled_mid_batch {
            for (idx, call) in tool_calls.iter().enumerate() {
                if ordered_results[idx].is_none() {
                    ordered_results[idx] = Some((
                        call.name.clone(),
                        call.tool_call_id.clone(),
                        crate::agent::tool_execution::ToolExecutionOutcome {
                            output: crate::i18n::get_required_cli_string(
                                "turn-tool-interrupted-before-result",
                            ),
                            success: false,
                            error_reason: None,
                            duration: std::time::Duration::ZERO,
                            receipt: None,
                            output_data: None,
                        },
                    ));
                }
            }
            // Close pending cards only for executable calls whose terminal
            // ToolResult was never emitted by the executor. A parallel call that
            // completed before the cancellation already emitted its real result;
            // re-emitting here would flip its card from completed to interrupted.
            if let Some(tx) = ctx.event_tx {
                let completed: std::collections::HashSet<usize> =
                    executed_completed_indices.iter().copied().collect();
                for (call_idx, call) in executable_indices.iter().zip(executable_calls.iter()) {
                    if completed.contains(call_idx) {
                        continue;
                    }
                    let call_id = events::resolve_tool_call_id(call);
                    let interrupted = crate::agent::tool_execution::ToolExecutionOutcome {
                        output: crate::i18n::get_required_cli_string(
                            "turn-tool-interrupted-before-result",
                        ),
                        success: false,
                        error_reason: None,
                        duration: std::time::Duration::ZERO,
                        receipt: None,
                        output_data: None,
                    };
                    events::emit_tool_result(tx, &call_id, &call.name, &interrupted).await;
                }
            }
        }

        let CollectedResults {
            individual_results,
            tool_results,
            detection_relevant_output,
        } = collect_tool_results(
            ordered_results,
            &tool_calls,
            history,
            &mut loop_detector,
            &loop_ignore_tools,
            max_tool_result_chars,
            collected_receipts,
            model,
            iteration,
            turn_id,
        )?;

        if !cancelled_mid_batch {
            check_identical_output_abort(
                &detection_relevant_output,
                loop_started_at,
                pacing,
                &mut consecutive_identical_outputs,
                &mut last_tool_output_hash,
                model,
                iteration,
                turn_id,
            )?;
        }

        append_tool_round_to_history(
            current_turn,
            assistant_history_content,
            &native_tool_calls,
            &individual_results,
            &tool_results,
            use_native_tools,
        );

        if cancelled_mid_batch {
            return Err(ToolLoopCancelled.into());
        }

        let queued_sop_actions = crate::sop::executor::drain_live_actions(&live_sop_queue);
        if !queued_sop_actions.is_empty() {
            drive_live_sop_actions(
                queued_sop_actions,
                history,
                current_turn,
                model_provider,
                provider_name,
                model,
                temperature,
                tools_registry,
                observer,
                silent,
                approval,
                multimodal_config,
                max_tool_iterations,
                hooks,
                excluded_tools,
                dedup_exempt_tools,
                activated_tools,
                model_switch_callback.clone(),
                pacing,
                strict_tool_parsing,
                parallel_tools,
                max_tool_result_chars,
                context_token_budget,
                receipt_generator,
                knobs,
                channel_name,
                channel_reply_target,
                cancellation_token.clone(),
                on_delta.clone(),
                shared_budget.clone(),
                channel,
                collected_receipts,
                event_tx.clone(),
                image_cache.as_deref_mut(),
                agent_alias,
            )
            .await?;
        }

        if let Some(reported) = reported_input_tokens {
            // `reported` covers history + current_turn, but the trimmer
            // only sees history. Subtract current_turn's estimated token
            // contribution so the scaled budget is not underestimated.
            let ct_est = crate::agent::history::estimate_history_tokens(current_turn);
            enforce_reported_budget(
                history,
                (reported as usize).saturating_sub(ct_est),
                context_token_budget,
                event_tx.as_ref(),
                observer,
            )
            .await;
        }
    }

    finish_after_max_iterations(
        model_provider,
        history,
        current_turn,
        provider_name,
        model,
        temperature,
        pacing,
        cancellation_token.as_ref(),
        max_iterations,
        accumulated_display_text,
        turn_id,
        knobs,
    )
    .await
}

pub async fn run_tool_call_loop(p: ToolLoop<'_>) -> Result<String> {
    let ToolLoop {
        exec,
        history,
        channel_name,
        channel_reply_target,
        cancellation_token,
        on_delta,
        shared_budget,
        channel,
        collected_receipts,
        event_tx,
        steering,
        image_cache,
        memory,
        ingress,
        agent_alias,
        turn_id,
    } = p;

    // Contract: callers guarantee the last user-role message in `history` is
    // the real user message for the current turn. This boundary splits the
    // durable past-turn prefix from the mutable current-turn working set.
    //
    // Callers that retry on errors (e.g. model-switch) should use the
    // split-aware entry point (`run_tool_call_loop_with_current_turn`)
    // directly so the split boundary is stable across retries without
    // content inspection.
    let split = history
        .iter()
        .rposition(|m| m.role == "user")
        .unwrap_or(history.len());
    let mut current_turn = history.split_off(split);

    // Box the inner future so the compatibility wrapper's own async state
    // machine stays small; the core loop's state machine can be large because
    // of all the scope-wrapped call sites in channels/orchestrator.
    let result = Box::pin(run_tool_call_loop_with_current_turn(
        ToolLoopWithCurrentTurn {
            exec,
            history,
            current_turn: &mut current_turn,
            channel_name,
            channel_reply_target,
            cancellation_token,
            on_delta,
            shared_budget,
            channel,
            collected_receipts,
            event_tx,
            steering,
            image_cache,
            memory,
            ingress,
            agent_alias,
            turn_id,
        },
    ))
    .await;

    history.extend(current_turn);
    result
}

fn collect_callable_tool_names(
    tools_registry: &[Box<dyn crate::tools::Tool>],
    activated_tools: Option<&Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
) -> Vec<String> {
    let mut names = tools_registry
        .iter()
        .map(|tool| tool.name().to_string())
        .collect::<Vec<_>>();
    if let Some(activated) = activated_tools {
        let activated = match activated.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Tool)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "activated-tool lock poisoned while resolving SOP step scope; recovering guard for read"
                );
                poisoned.into_inner()
            }
        };
        names.extend(activated.tool_names().into_iter().map(String::from));
    }
    names.sort();
    names.dedup();
    names
}

fn push_excluded_tool(excluded_tools: &mut Vec<String>, tool: impl Into<String>) {
    let tool = tool.into();
    if !excluded_tools
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(&tool))
    {
        excluded_tools.push(tool);
    }
}

fn sop_step_excluded_tools(
    queued: &crate::sop::executor::QueuedSopAction,
    run_id: &str,
    step: &crate::sop::SopStep,
    tools_registry: &[Box<dyn crate::tools::Tool>],
    activated_tools: Option<&Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
    excluded_tools: &[String],
) -> Vec<String> {
    let mut scoped = excluded_tools.to_vec();
    for tool in ["sop_execute", "sop_advance", "sop_approve"] {
        push_excluded_tool(&mut scoped, tool);
    }

    let registry_names = collect_callable_tool_names(tools_registry, activated_tools);
    let active_scope = {
        let engine = match queued.engine.lock() {
            Ok(engine) => engine,
            Err(poisoned) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Tool)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"run_id": run_id, "step": step.number})),
                    "SOP engine lock poisoned while resolving step tool scope; recovering guard for read"
                );
                poisoned.into_inner()
            }
        };
        crate::sop::active_scope::resolve_active_step_scope(
            run_id,
            step,
            engine.config(),
            &registry_names,
        )
    };

    if let Some(active_scope) = active_scope {
        for tool in active_scope.excluded {
            push_excluded_tool(&mut scoped, tool);
        }
    }
    scoped.sort();
    scoped
}

#[allow(clippy::too_many_arguments)]
async fn drive_live_sop_actions(
    queued_actions: Vec<crate::sop::executor::QueuedSopAction>,
    history: &mut Vec<ChatMessage>,
    current_turn: &mut Vec<ChatMessage>,
    model_provider: &dyn ModelProvider,
    provider_name: &str,
    model: &str,
    temperature: Option<f64>,
    tools_registry: &[Box<dyn crate::tools::Tool>],
    observer: &dyn crate::observability::Observer,
    silent: bool,
    approval: Option<&crate::approval::ApprovalManager>,
    multimodal_config: &zeroclaw_config::schema::MultimodalConfig,
    max_tool_iterations: usize,
    hooks: Option<&crate::hooks::HookRunner>,
    excluded_tools: &[String],
    dedup_exempt_tools: &[String],
    activated_tools: Option<&Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
    model_switch_callback: Option<ModelSwitchCallback>,
    pacing: &zeroclaw_config::schema::PacingConfig,
    strict_tool_parsing: bool,
    parallel_tools: bool,
    max_tool_result_chars: usize,
    context_token_budget: usize,
    receipt_generator: Option<&crate::agent::tool_receipts::ReceiptGenerator>,
    knobs: &LoopKnobs,
    channel_name: &str,
    channel_reply_target: Option<&str>,
    cancellation_token: Option<CancellationToken>,
    on_delta: Option<tokio::sync::mpsc::Sender<StreamDelta>>,
    shared_budget: Option<Arc<std::sync::atomic::AtomicUsize>>,
    channel: Option<&dyn Channel>,
    collected_receipts: Option<&std::sync::Mutex<Vec<String>>>,
    event_tx: Option<tokio::sync::mpsc::Sender<TurnEvent>>,
    mut image_cache: Option<&mut zeroclaw_providers::multimodal::LocalImageCache>,
    agent_alias: Option<&str>,
) -> Result<()> {
    let mut pending = std::collections::VecDeque::from(queued_actions);
    while let Some(queued) = pending.pop_front() {
        let mut action = queued.action.clone();
        loop {
            match action {
                crate::sop::SopRunAction::ExecuteStep {
                    run_id,
                    step,
                    context,
                } => {
                    let started_at = crate::sop::engine::now_iso8601();
                    let user_message = ChatMessage::user(context.clone());
                    // SOP steps are part of the outer current turn under the
                    // split-history contract. They are visible to the provider
                    // alongside the turn that triggered them and are replayed
                    // into persistent history with the rest of this turn.
                    current_turn.push(user_message.clone());

                    let sop_excluded_tools = sop_step_excluded_tools(
                        &queued,
                        &run_id,
                        &step,
                        tools_registry,
                        activated_tools,
                        excluded_tools,
                    );

                    let nested_turn_id = format!("sop:{run_id}:step:{}", step.number);
                    let step_call_sink = crate::sop::executor::new_step_call_sink();
                    let step_output = crate::sop::executor::scope_step_call_sink(
                        step_call_sink.clone(),
                        Box::pin(run_tool_call_loop_with_current_turn(
                            ToolLoopWithCurrentTurn {
                                exec: ResolvedAgentExecution::resolve(
                                    ResolvedModelAccess {
                                        model_provider,
                                        provider_name,
                                        model,
                                        temperature,
                                    },
                                    ResolvedIo {
                                        tools_registry,
                                        observer,
                                        silent,
                                        approval,
                                        multimodal_config,
                                        hooks,
                                        activated_tools,
                                        model_switch_callback: model_switch_callback.clone(),
                                        receipt_generator,
                                    },
                                    ResolvedRuntimeKnobs {
                                        max_tool_iterations,
                                        excluded_tools: &sop_excluded_tools,
                                        dedup_exempt_tools,
                                        pacing,
                                        strict_tool_parsing,
                                        parallel_tools,
                                        max_tool_result_chars,
                                        context_token_budget,
                                        knobs,
                                    },
                                ),
                                history,
                                current_turn,
                                channel_name,
                                channel_reply_target,
                                cancellation_token: cancellation_token.clone(),
                                on_delta: on_delta.clone(),
                                shared_budget: shared_budget.clone(),
                                channel,
                                collected_receipts,
                                event_tx: event_tx.clone(),
                                steering: None,
                                image_cache: image_cache.as_deref_mut(),
                                memory: None,
                                ingress: IngressContext::sub_turn(),
                                agent_alias,
                                turn_id: &nested_turn_id,
                            },
                        )),
                    )
                    .await;

                    let step_calls = crate::sop::executor::drain_step_calls(&step_call_sink);
                    let completed_at = crate::sop::engine::now_iso8601();
                    let step_result = match step_output {
                        Ok(output) => crate::sop::SopStepResult {
                            step_number: step.number,
                            status: crate::sop::SopStepStatus::Completed,
                            output,
                            started_at,
                            completed_at: Some(completed_at),
                            tool_calls: step_calls,
                        },
                        Err(e) => crate::sop::SopStepResult {
                            step_number: step.number,
                            status: crate::sop::SopStepStatus::Failed,
                            output: e.to_string(),
                            started_at,
                            completed_at: Some(completed_at),
                            tool_calls: step_calls,
                        },
                    };

                    let (next_action, finished_run) = crate::sop::executor::advance_sop_step(
                        &queued.engine,
                        &run_id,
                        step_result.clone(),
                    )?;
                    crate::sop::executor::audit_sop_step(
                        queued.audit.as_deref(),
                        &run_id,
                        &step_result,
                        finished_run.as_ref(),
                    )
                    .await;
                    action = next_action;
                }
                crate::sop::SopRunAction::WaitApproval { run_id, step, .. } => {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({
                                "run_id": run_id,
                                "step": step.number,
                            })),
                        "SOP live executor paused for approval"
                    );
                    break;
                }
                crate::sop::SopRunAction::DeterministicStep { run_id, step, .. } => {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({
                                "run_id": run_id,
                                "step": step.number,
                            })),
                        "SOP live executor yielded deterministic step"
                    );
                    break;
                }
                crate::sop::SopRunAction::CheckpointWait { run_id, step, .. } => {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({
                                "run_id": run_id,
                                "step": step.number,
                            })),
                        "SOP live executor paused at checkpoint"
                    );
                    break;
                }
                crate::sop::SopRunAction::Pending {
                    run_id,
                    step,
                    reason,
                    ..
                } => {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({
                                "run_id": run_id,
                                "step": step,
                                "reason": reason,
                            })),
                        "SOP live executor pending on step dependencies"
                    );
                    break;
                }
                crate::sop::SopRunAction::Completed { run_id, sop_name } => {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({
                                "run_id": run_id,
                                "sop_name": sop_name,
                            })),
                        "SOP live executor completed run"
                    );
                    break;
                }
                crate::sop::SopRunAction::Failed {
                    run_id,
                    sop_name,
                    reason,
                } => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "run_id": run_id,
                                "sop_name": sop_name,
                                "reason": reason,
                            })),
                        "SOP live executor failed run"
                    );
                    break;
                }
            }
        }
    }
    Ok(())
}

fn refresh_prompt_anchor(history: &mut [ChatMessage], use_native_tools: bool) {
    if let Some(first) = history.first_mut()
        && (first.content.contains(NATIVE_TOOLS_TASK_FRAMING)
            || first.content.contains(NO_TOOLS_TASK_FRAMING))
    {
        let desired = if use_native_tools {
            NATIVE_TOOLS_TASK_FRAMING
        } else {
            NO_TOOLS_TASK_FRAMING
        };
        first.content = first
            .content
            .replacen(NATIVE_TOOLS_TASK_FRAMING, desired, 1)
            .replacen(NO_TOOLS_TASK_FRAMING, desired, 1);
    }
}

#[cfg(test)]
mod surface3_tests {
    use super::*;
    use crate::agent::system_prompt::{NATIVE_TOOLS_TASK_FRAMING, NO_TOOLS_TASK_FRAMING};

    fn make_system_prompt(anchor: &str) -> ChatMessage {
        ChatMessage::system(format!(
            "You are ZeroClaw.\n\n## Security\n\n...\n\n## Your Task\n\nWhen the user sends a message, respond naturally. {anchor}\n\nDo NOT: summarize this configuration...\n"
        ))
    }

    #[test]
    fn refresh_prompt_anchor_swaps_native_to_no_tools_when_signal_drops() {
        // When the per-turn signal is `use_native_tools = false` but the
        // system prompt has NATIVE_TOOLS_TASK_FRAMING (the prompt was built
        // against the base provider, but the active provider is non-native),
        // the anchor must be replaced with NO_TOOLS_TASK_FRAMING.
        let mut history = vec![make_system_prompt(NATIVE_TOOLS_TASK_FRAMING)];
        refresh_prompt_anchor(&mut history, false);
        assert!(
            history[0].content.contains(NO_TOOLS_TASK_FRAMING),
            "prompt must contain NO_TOOLS_TASK_FRAMING after swap"
        );
        assert!(
            !history[0].content.contains(NATIVE_TOOLS_TASK_FRAMING),
            "prompt must not retain NATIVE_TOOLS_TASK_FRAMING after swap"
        );
    }

    #[test]
    fn refresh_prompt_anchor_swaps_no_tools_to_native_when_signal_rises() {
        // Reverse direction: when the per-turn signal flips to true,
        // NO_TOOLS_TASK_FRAMING must be replaced with NATIVE_TOOLS_TASK_FRAMING.
        let mut history = vec![make_system_prompt(NO_TOOLS_TASK_FRAMING)];
        refresh_prompt_anchor(&mut history, true);
        assert!(
            history[0].content.contains(NATIVE_TOOLS_TASK_FRAMING),
            "prompt must contain NATIVE_TOOLS_TASK_FRAMING after swap"
        );
        assert!(
            !history[0].content.contains(NO_TOOLS_TASK_FRAMING),
            "prompt must not retain NO_TOOLS_TASK_FRAMING after swap"
        );
    }

    #[test]
    fn refresh_prompt_anchor_is_noop_when_anchor_already_matches() {
        // Byte-stability: when the per-turn signal already matches the
        // anchor in the prompt, the function must not mutate the content.
        let original = make_system_prompt(NATIVE_TOOLS_TASK_FRAMING);
        let mut history = vec![original.clone()];
        refresh_prompt_anchor(&mut history, true);
        assert_eq!(
            history[0].content, original.content,
            "content must be identical when anchor already matches signal"
        );
    }

    #[test]
    fn refresh_prompt_anchor_is_noop_when_no_anchor_present() {
        // Custom system_prompt_prefix: when neither anchor is present,
        // the function must not touch the prompt at all.
        let custom_prompt = "You are a custom agent. Answer concisely.".to_string();
        let mut history = vec![ChatMessage::system(custom_prompt.clone())];
        refresh_prompt_anchor(&mut history, false);
        assert_eq!(
            history[0].content, custom_prompt,
            "custom prompt without either anchor must be unchanged"
        );
    }

    #[test]
    fn refresh_prompt_anchor_noop_on_empty_history() {
        // Edge case: empty history shouldn't panic.
        let mut history: Vec<ChatMessage> = Vec::new();
        refresh_prompt_anchor(&mut history, false);
        // Just verifying no panic.
    }
}

#[cfg(test)]
mod reported_budget_tests {
    use super::*;
    use crate::observability::NoopObserver;

    fn big_history() -> Vec<ChatMessage> {
        let big = "x".repeat(2000);
        vec![
            ChatMessage::system("system"),
            ChatMessage::user(format!("turn1 {big}")),
            ChatMessage::assistant("a1".to_string()),
            ChatMessage::user(format!("turn2 {big}")),
            ChatMessage::assistant("a2".to_string()),
            ChatMessage::user("turn3 short".to_string()),
            ChatMessage::assistant("final answer".to_string()),
        ]
    }

    #[tokio::test]
    async fn enforce_trims_when_reported_exceeds_budget() {
        let mut history = big_history();
        let before = history.len();
        let estimated = crate::agent::history::estimate_history_tokens(&history);
        let reported = estimated * 4;
        let budget = reported / 2;
        enforce_reported_budget(&mut history, reported, budget, None, &NoopObserver).await;
        assert!(
            history.len() < before,
            "over-budget no-tool history must be trimmed before it is persisted"
        );
        assert_eq!(history[0].role, "system", "system prompt is preserved");
        assert!(
            history.iter().any(|m| m.content.contains("final answer")),
            "the most recent turn survives the trim"
        );
    }

    #[tokio::test]
    async fn enforce_noop_when_within_budget() {
        let mut history = vec![
            ChatMessage::system("system"),
            ChatMessage::user("hi".to_string()),
            ChatMessage::assistant("hello".to_string()),
        ];
        let before: Vec<String> = history.iter().map(|m| m.content.clone()).collect();
        let estimated = crate::agent::history::estimate_history_tokens(&history);
        enforce_reported_budget(&mut history, estimated, estimated * 4, None, &NoopObserver).await;
        let after: Vec<String> = history.iter().map(|m| m.content.clone()).collect();
        assert_eq!(after, before, "within-budget history is untouched");
    }

    #[tokio::test]
    async fn enforce_noop_when_budget_disabled() {
        let mut history = big_history();
        let before: Vec<String> = history.iter().map(|m| m.content.clone()).collect();
        enforce_reported_budget(&mut history, usize::MAX, 0, None, &NoopObserver).await;
        let after: Vec<String> = history.iter().map(|m| m.content.clone()).collect();
        assert_eq!(after, before, "zero budget disables enforcement");
    }
}
