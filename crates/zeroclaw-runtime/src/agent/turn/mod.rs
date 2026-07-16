//! The agent turn engine, decomposed into single-purpose step modules.
//!
//! Public paths are unchanged: external code keeps importing via
//! `crate::agent::loop_::*` (re-export block there). Full contract manifest:
//! `/opt/notes/work/zeroclaw/unification_modular/RUN_SHEET.md` (condensed
//! copy below) during the #7415 migration.
//!
//! # Run sheet (condensed)
//!
//! [`run_tool_call_loop`] is the orchestrator: loop control only, no step
//! logic. [`TurnCtx`] carries shared refs ONLY; every `&mut` the loop owns
//! (history, loop detector, `seen_tool_signatures` — reset per iteration —
//! identical-output counters, accumulated display text, malformed-retry
//! counter) stays a loop local passed as an explicit argument.
//!
//! Per-iteration step sequence:
//!
//! ```text
//! preflight_history_maintenance(&mut history)            history_window.rs
//! → [model-switch check — inline]
//! → build_iteration_tool_specs(..)                       tool_specs.rs
//! → resolve_vision_provider(..)                          vision_route.rs
//! → [active triple derivation — inline]
//! → prepare_messages_for_iteration(..)                   vision_route.rs
//! → announce_llm_request(ctx, ..) -> Instant             provider_call.rs
//! → enforce_tool_loop_budget()                           provider_call.rs
//! → call_provider(ctx, ..) -> ProviderCallOutcome        provider_call.rs
//!     [streaming: consume_provider_streaming_response]   stream_consume.rs
//!     [cancel asymmetry: see ProviderCallOutcome docs]
//! → Ok:  interpret_chat_response(ctx, ..)                parse_response.rs
//!   Err: record_llm_failure(..);                         context_recovery.rs
//!        try_recover_context_overflow(..) -> bool (true ⇒ continue)
//! → resolve_display_text / [malformed-retry — inline]
//!   / [no-tool exit — inline, stream_text_posthoc_chunks → events.rs]
//! → prepare_tool_calls(ctx, &mut seen, ..)               call_prep.rs
//!     [approval via gate_tool_approval]                  approval_gate.rs
//! → [execute dispatch — inline → tool_execution::execute_tools_{parallel,sequential}]
//! → record_executed_outcomes(ctx, .., &mut ordered)      post_exec.rs
//! → collect_tool_results(..) -> CollectedResults         results_collect.rs
//! → check_identical_output_abort(.., &mut counters)      results_collect.rs
//! → append_tool_round_to_history(&mut history, ..)       history_append.rs
//! [loop exhausted] → finish_after_max_iterations(..)     max_iter.rs
//! ```
//!
//! Leaf/type modules: `context` (TurnCtx), `events` (StreamDelta/DraftEvent,
//! pacing consts, post-hoc chunker), `outcome` (ToolLoopCancelled,
//! ModelSwitchRequested), `redact` (scrub_credentials), `stream_guard` +
//! `protocol_detect` (streaming protocol suppression), `delivery_defaults`.

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

// ── Agent Tool-Call Loop ──────────────────────────────────────────────────
// Core agentic iteration: send conversation to the LLM, parse any tool
// calls from the response, execute them, append results to history, and
// repeat until the LLM produces a final text-only answer.
//
// Loop invariant: at the start of each iteration, `history` contains the
// full conversation so far (system prompt + user messages + prior tool
// results). The loop exits when:
//   • the LLM returns no tool calls (final answer), or
//   • max_iterations is reached (runaway safety), or
//   • the cancellation token fires (external abort).

/// Execute a single turn of the agent loop: send messages, parse tool calls,
/// execute tools, and loop until the LLM produces a final text response.
///
/// `new_messages_out` is an append-log: every message the loop adds to
/// `history` is mirrored into it at push time (a clone taken before any
/// later in-loop history maintenance), so it is populated on **every** exit
/// — success, error, and cancellation — and never derived from history
/// indices, which in-loop pruning can invalidate. Loop-detection system
/// notes are the one exception (merged into the existing system message;
/// only reachable when pattern loop detection is enabled, which no
/// `new_messages_out` consumer turns on).
/// All parameters of [`run_tool_call_loop`], bundled into one borrowed struct.
///
/// Field names and types mirror the loop's former positional arguments
/// one-for-one (the loop borrows everything for the duration of the turn,
/// including the `&mut` working sets `history`, `steering`, `new_messages_out`,
/// and `image_cache`). [`LoopKnobs`] stays a nested sub-bundle in `knobs`.
///
/// Callers build this struct literal and pass it by value; the loop
/// destructures it once at entry, so the body reads exactly as it did when
/// these were positional parameters.
pub struct ToolLoop<'a> {
    /// The resolved per-agent execution context: model binding, gated tool
    /// registry, approval, observability, and resolved runtime knobs. Stable
    /// for every turn to this agent; built once and reused. See
    /// [`ResolvedAgentExecution`]. Everything below is per-message turn state.
    pub exec: ResolvedAgentExecution<'a>,
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
    pub new_messages_out: Option<&'a mut Vec<ChatMessage>>,
    pub image_cache: Option<&'a mut zeroclaw_providers::multimodal::LocalImageCache>,
    /// The ingress envelope stamped by the entry layer (RFC #6971). Travels
    /// with the turn into the engine, where the universal SOP policy layer
    /// dispositions it at P1 (turn entry) and P2 (each steering injection).
    /// Phase-1 callers stamp a per-origin envelope; real per-transport
    /// stamping is phase 2. Owned (not borrowed) — the envelope is small and
    /// consumed by the policy front door for the turn's lifetime.
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
    /// Handle the live SOP driver uses to re-assemble a nested step's execution
    /// context when the step delegates to a different agent (see
    /// [`SopStepReassembly`]). `None` on every path that cannot reach `Config`
    /// or that never drives nested SOP steps; when `None`, a cross-agent step
    /// falls back to the parent context (today's behavior).
    pub sop_reassembly: Option<SopStepReassembly<'a>>,
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
        mut steering,
        mut new_messages_out,
        mut image_cache,
        ingress,
        memory,
        agent_alias,
        turn_id,
        sop_reassembly,
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

    // ── Ingress policy · P1 (turn entry) ────────────────────────────────────
    // RFC #6971: every inbound turn passes the universal SOP policy layer before
    // a model sees it. The default policy dispositions to `Loop` (run the agent,
    // today's behavior); the layer is always on, never skipped. `ingress` is
    // consumed here (passed to `ingress_policy`) so it is never dead code under
    // `-D warnings`. The text dispositioned at P1 is the trailing user turn —
    // the most recently appended `user` history message, when present.
    let ingress_policy_cfg = IngressPolicy::default();
    let p1_text = history
        .iter()
        .rev()
        .find(|m| m.role == "user")
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

    // ── Memory-context injection (unified) ──────────────────────────────────
    // The ONE injection point for the memory preamble, replacing the per-path
    // inline renderers. The decision is keyed on the ingress origin (sub-turns
    // never inject; scheduled origins exclude Conversation entries); the
    // pipeline and its documented uniform behavior live in
    // `agent::memory_inject`. Injection prepends to the trailing user message
    // AFTER the P1 policy scan, so policy always sees the caller's own text.
    if let Some(turn_memory) = &memory {
        let has_session = turn_memory.sessions.iter().any(Option::is_some);
        if let crate::agent::memory_inject::InjectPolicy::Inject {
            exclude_conversation,
        } = crate::agent::memory_inject::resolve_inject_policy(
            ingress.origin,
            has_session,
            turn_memory.suppress,
        ) && let Some(last_user_idx) = history.iter().rposition(|m| m.role == "user")
            // Idempotence: a model-switch retry re-enters the engine with the
            // same history; the preamble must not stack.
            && !history[last_user_idx]
                .content
                .starts_with(zeroclaw_memory::MEMORY_CONTEXT_OPEN)
        {
            let scopes: Vec<Option<&str>> =
                turn_memory.sessions.iter().map(|s| s.as_deref()).collect();
            let context = crate::agent::memory_inject::render_memory_context(
                turn_memory.handle,
                observer,
                &turn_memory.query,
                &scopes,
                &turn_memory.cfg,
                exclude_conversation,
            )
            .await;
            if !context.is_empty() {
                let existing = &history[last_user_idx].content;
                history[last_user_idx].content = format!("{context}{existing}");
            }
        }
    }

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
        // Steering: fold caller-pushed mid-turn messages into history before
        // this iteration's provider request.
        //
        // ── Ingress policy · P2 (steering drain) ────────────────────────────
        // RFC #6971: each mid-turn injection passes the same universal policy
        // layer as P1. The default policy dispositions to `Loop` → append as
        // today. The envelope (`ingress`) carries the turn's provenance to the
        // policy for each drained message.
        for steering_message in drain_steering_messages(&mut steering) {
            match ingress_policy(&steering_message, &ingress, &ingress_policy_cfg) {
                // DEFAULT — append the injection to history exactly as today.
                IngressDecision::Loop => {}
                // Phase 3: frame as untrusted data; proceed as Loop until
                // framing exists (behavior-identical).
                IngressDecision::Annotate { .. } => {}
                // Phase 2: divert this injection into the SOP run rather than
                // history. Not reachable under the default policy.
                IngressDecision::Gate { .. } => {
                    // TODO(PR C): route this steering message into the gated
                    // SOP run instead of appending it to history.
                }
                // Not reachable under the default policy; drop the injection
                // (do not append it) when it is.
                IngressDecision::Drop { .. } => continue,
            }
            let msg = ChatMessage::user(steering_message);
            if let Some(out) = new_messages_out.as_deref_mut() {
                out.push(msg.clone());
            }
            history.push(msg);
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
            // The system prompt + inlined tool definitions form an irreducible
            // floor whole-turn trimming can never drop. When that floor alone
            // meets or exceeds the budget, trimming conversation history can
            // never bring the request under budget — surface the actionable
            // root cause once per turn (this block is gated on iteration == 0)
            // so the misconfiguration is not silent (#5808). Whole-turn
            // trimming below still runs and is harmless (it keeps the most
            // recent turn); the model call / reactive recovery proceeds.
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
            let taken = std::mem::take(history);
            let result =
                crate::agent::history_trim::trim_to_recent_turns(taken, context_token_budget);
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

        let (vision_model_provider_box, degrade_strip_images) =
            resolve_vision_provider(model_provider, history, multimodal_config, provider_name)?;

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

        // ── Per-turn system prompt anchor refresh (#8054 Surface 3) ──
        // The system prompt in `history[0]` was built by
        // `Agent::build_system_prompt()` against the base provider, and
        // may not reflect this iteration's `active_model_provider` after
        // vision routing.  Swap the TASK_FRAMING anchor so the prompt's
        // tool-availability claim matches the actual `request_tools`.
        refresh_prompt_anchor(history, use_native_tools);

        let prepared_messages = prepare_messages_for_iteration(
            history,
            multimodal_config,
            degrade_strip_images,
            image_cache.as_deref_mut(),
        )
        .await?;

        let llm_started_at = announce_llm_request(
            &ctx,
            history,
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
                    if let Some(out) = new_messages_out.as_deref_mut() {
                        out.push(msg.clone());
                    }
                    history.push(msg);
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
                    if let Some(out) = new_messages_out.as_deref_mut() {
                        out.push(msg.clone());
                    }
                    history.push(msg);
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
                if let Some(out) = new_messages_out.as_deref_mut() {
                    out.push(msg.clone());
                }
                history.push(msg);
                continue;
            }

            let fallback =
                crate::i18n::get_required_cli_string("channel-runtime-malformed-tool-output");
            accumulated_display_text.push_str(&fallback);
            if let Some(ref tx) = on_delta {
                let _ = tx.send(StreamDelta::Text(fallback.to_string())).await;
            }
            let msg = ChatMessage::assistant(fallback.to_string());
            if let Some(out) = new_messages_out.as_deref_mut() {
                out.push(msg.clone());
            }
            history.push(msg);
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
            if let Some(out) = new_messages_out.as_deref_mut() {
                out.push(msg.clone());
            }
            history.push(msg);
            if let Some(reported) = reported_input_tokens {
                enforce_reported_budget(
                    history,
                    reported as usize,
                    context_token_budget,
                    event_tx.as_ref(),
                    observer,
                )
                .await;
            }
            return Ok(accumulated_display_text);
        }

        // Do not accumulate intermediate-turn display text into the final
        // channel response. Native tool-call providers may emit narration or
        // scratchpad-like text alongside tool calls; draft-capable channels
        // can still see it live through `on_delta` below, but the final
        // delivered response must only contain the final assistant turn.

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

        let appended_from = history.len();
        append_tool_round_to_history(
            history,
            assistant_history_content,
            &native_tool_calls,
            &individual_results,
            &tool_results,
            use_native_tools,
        );
        if let Some(out) = new_messages_out.as_deref_mut() {
            out.extend_from_slice(&history[appended_from..]);
        }

        if cancelled_mid_batch {
            return Err(ToolLoopCancelled.into());
        }

        let queued_sop_actions = crate::sop::executor::drain_live_actions(&live_sop_queue);
        if !queued_sop_actions.is_empty() {
            // Box the drive future: it inlines the full per-agent re-assembly
            // (a large async fn), which would otherwise inflate this loop's
            // stack-allocated future for every turn, SOP or not.
            Box::pin(drive_live_sop_actions(
                queued_sop_actions,
                history,
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
                new_messages_out.as_deref_mut(),
                image_cache.as_deref_mut(),
                agent_alias,
                sop_reassembly,
            ))
            .await?;
        }

        if let Some(reported) = reported_input_tokens {
            enforce_reported_budget(
                history,
                reported as usize,
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
        provider_name,
        model,
        temperature,
        pacing,
        cancellation_token.as_ref(),
        max_iterations,
        accumulated_display_text,
        turn_id,
        knobs,
        new_messages_out,
    )
    .await
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

/// Config handle the live SOP driver needs to re-assemble a nested step's agent
/// when the step delegates to a different agent than the one running the turn.
///
/// The live nested-step driver otherwise reuses the parent turn's assembled
/// execution context; when a step names a different agent it must run AS that
/// agent — with that agent's own gated tools, policy, and MCP scope — not the
/// parent's. This carries the one handle needed to rebuild that context in
/// flight; the runtime adapter is created from `config.runtime` so the nested
/// context is assembled the way a fresh agent turn would be. `Copy` so the
/// handle survives being re-read on every drained action and forwarded into
/// each nested turn.
///
/// This handle is either `Some` on every frame of a recursion tree or `None` on
/// every frame: it is introduced only at the top entry points and forwarded
/// unchanged (config-wise) into each nested step, so a `None` here means no
/// ancestor could have re-assembled and thus `baseline_alias == agent_alias`
/// always holds. That invariant is what lets the re-assembly baseline ride
/// inside this handle instead of a separate per-turn field.
#[derive(Clone, Copy)]
pub struct SopStepReassembly<'a> {
    pub config: &'a zeroclaw_config::schema::Config,
    /// The alias of the agent whose execution context the CURRENT loop is
    /// actually running with — the re-assembly baseline. At a top-level entry
    /// this equals `agent_alias`; once a cross-agent step re-assembles a
    /// different agent in a parent frame, the nested loop carries THAT agent
    /// here (see [`advance_nested_reassembly`]). It is deliberately distinct
    /// from `agent_alias`, which stays pinned to the outer agent for
    /// attribution (Observer/receipt/OTel) and must not be overloaded: at
    /// depth >= 2 the two diverge, and the re-assembly gate must compare a
    /// step's alias against the effective baseline, not the attribution alias,
    /// or a step naming the outer agent from inside a re-assembled child would
    /// skip re-assembly and inherit the child's broader scope (escalation).
    pub baseline_alias: Option<&'a str>,
}

/// The re-assembly gate: a step needs its own agent context re-assembled when it
/// names an agent different from the one the current loop is running as. The
/// baseline is the EFFECTIVE alias (the re-assembly baseline), never the
/// attribution `agent_alias`, which at depth >= 2 lags behind a re-assembled
/// child. A step with no explicit agent inherits the current one and never
/// re-assembles.
fn step_needs_reassembly(baseline_alias: Option<&str>, step_alias: Option<&str>) -> bool {
    matches!(step_alias, Some(s) if baseline_alias != Some(s))
}

/// Advance the re-assembly baseline for a nested step's sub-loop. When this step
/// re-assembled a different agent's context (`reassembled`), that agent becomes
/// the baseline its own nested sub-steps compare against, so a deeper step naming
/// yet another agent (including the outer one) re-assembles correctly instead of
/// inheriting this step agent's scope. When nothing was re-assembled the baseline
/// carries through unchanged. The config handle is preserved so the whole subtree
/// keeps the same re-assembly capability.
fn advance_nested_reassembly<'a>(
    current: Option<SopStepReassembly<'a>>,
    step_alias: Option<&'a str>,
    reassembled: bool,
) -> Option<SopStepReassembly<'a>> {
    match current {
        Some(r) if reassembled => Some(SopStepReassembly {
            config: r.config,
            baseline_alias: step_alias,
        }),
        other => other,
    }
}

/// A nested SOP step's owned per-agent execution surface, assembled for a step
/// that delegates to a different agent. Owns exactly the fields of
/// [`ResolvedAgentExecution`] that constitute per-agent isolation — provider
/// binding, gated tool registry, approval policy, and the deferred-MCP
/// activation set. The remaining turn knobs (iteration caps, pacing, budgets,
/// multimodal, hooks) stay parent-threaded by design: the boundary re-assembled
/// here is per-agent isolation of tools/policy/MCP/provider, not a full
/// per-agent turn-config swap.
pub(crate) struct OwnedAgentExecution {
    model_provider: Box<dyn ModelProvider>,
    provider_name: String,
    model: String,
    pub(crate) tools_registry: Vec<Box<dyn crate::tools::Tool>>,
    approval: crate::approval::ApprovalManager,
    activated_tools: Option<Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
}

/// Re-assemble `alias`'s per-agent execution context the way a fresh agent turn
/// would: the agent's security policy, memory, gated tool registry (through the
/// one [`crate::tools::scoped::ScopedToolRegistry::assemble`] seam, connecting
/// the agent's own granted MCP scope), skills, provider binding, and
/// non-interactive approval policy. The live SOP engine/audit handles are
/// threaded from the running SOP so the nested step keeps its SOP tools bound to
/// the same engine. This connects MCP servers, so the driver memoizes the
/// result per alias across a drain and re-assembles only on an alias change.
pub(crate) async fn assemble_owned_execution(
    config: &zeroclaw_config::schema::Config,
    alias: &str,
    sop_engine: Arc<std::sync::Mutex<crate::sop::SopEngine>>,
    sop_audit: Option<Arc<crate::sop::SopAuditLogger>>,
) -> Result<OwnedAgentExecution> {
    let security = Arc::new(crate::security::SecurityPolicy::for_agent(config, alias)?);
    let risk_profile = config
        .risk_profile_for_agent(alias)
        .cloned()
        .ok_or_else(|| {
            anyhow::Error::msg(format!(
                "SOP step agent '{alias}' has no configured risk profile"
            ))
        })?;
    let api_key = config
        .resolved_model_provider_for_agent(alias)
        .and_then(|(_, _, cfg)| cfg.api_key.clone());
    let memory =
        zeroclaw_memory::create_memory_for_agent(config, alias, api_key.as_deref()).await?;

    // Mirror a fresh agent turn: the headless SOP driver reaches this agent's
    // tools via `crate::agent::run`, which builds its runtime from
    // `config.runtime`. Creating it here (rather than reusing the parent's) is
    // read-only — a shell-existence check plus struct construction — and only
    // happens on the cross-agent path, which is memoized per alias.
    let runtime: Arc<dyn crate::platform::RuntimeAdapter> =
        Arc::from(crate::platform::create_runtime(&config.runtime)?);

    let (composio_key, composio_entity_id) = if config.composio.enabled {
        (
            config.composio.api_key.as_deref(),
            Some(config.composio.entity_id.as_str()),
        )
    } else {
        (None, None)
    };

    let built = crate::tools::all_tools_with_runtime(
        Arc::new(config.clone()),
        &security,
        &risk_profile,
        alias,
        runtime.clone(),
        memory,
        composio_key,
        composio_entity_id,
        &config.browser,
        &config.http_request,
        &config.web_fetch,
        &config.data_dir,
        &config.agents,
        api_key.as_deref(),
        config,
        None,
        false,
        None,
        Some(sop_engine),
        sop_audit,
        None,
    );
    let skills = crate::skills::load_skills_for_agent_from_config(config, alias);
    // The same gated seam run(), process_message, and independent delegation use:
    // step 2 filters with THIS agent's SecurityPolicy, `connect_mcp` grants only
    // this agent's MCP bundles, and its skills register as tools. Peripherals stay
    // disconnected — a nested SOP sub-loop must not seize the serial hardware the
    // live daemon holds exclusively.
    let assembled =
        crate::tools::scoped::ScopedToolRegistry::assemble(crate::tools::scoped::ScopedAssembly {
            config,
            agent_alias: alias,
            security: &security,
            built,
            skills: &skills,
            runtime,
            caller_allowed: None,
            connect_mcp: true,
            connect_peripherals: false,
            exclude_memory: false,
            list_deferred_mcp_specs: false,
            emit_assembly_logs: true,
        })
        .await;
    let crate::tools::scoped::ScopedAssembled {
        registry,
        activated_handle,
        ..
    } = assembled;
    let tools_registry = registry.into_inner();

    let provider_ref = config
        .resolved_model_provider_for_agent(alias)
        .map(|(ty, al, _)| format!("{ty}.{al}"))
        .ok_or_else(|| {
            anyhow::Error::msg(format!(
                "SOP step agent '{alias}' has no resolved model provider"
            ))
        })?;
    let (model_provider, provider_name, model) =
        crate::agent::agent::build_session_model_provider(config, &provider_ref, None)?;

    let approval = crate::approval::ApprovalManager::for_non_interactive(&risk_profile);

    Ok(OwnedAgentExecution {
        model_provider,
        provider_name,
        model,
        tools_registry,
        approval,
        activated_tools: activated_handle,
    })
}

#[allow(clippy::too_many_arguments)]
async fn drive_live_sop_actions(
    queued_actions: Vec<crate::sop::executor::QueuedSopAction>,
    history: &mut Vec<ChatMessage>,
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
    mut new_messages_out: Option<&mut Vec<ChatMessage>>,
    mut image_cache: Option<&mut zeroclaw_providers::multimodal::LocalImageCache>,
    agent_alias: Option<&str>,
    sop_reassembly: Option<SopStepReassembly<'_>>,
) -> Result<()> {
    let mut pending = std::collections::VecDeque::from(queued_actions);
    // Per-agent execution contexts re-assembled in flight for steps that
    // delegate to a different agent, memoized by alias across this drain:
    // `assemble_owned_execution` connects MCP, so it runs at most once per
    // distinct step agent, never per step.
    let mut exec_cache: std::collections::HashMap<String, OwnedAgentExecution> =
        std::collections::HashMap::new();
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
                    history.push(user_message.clone());
                    if let Some(out) = new_messages_out.as_deref_mut() {
                        out.push(user_message);
                    }

                    // A step that delegates to a different agent must run AS that
                    // agent — with that agent's own gated tools, policy, and MCP
                    // scope — not the parent turn's. When the step names a
                    // different agent and a reassembly handle is available,
                    // re-assemble (and memoize) that agent's execution context;
                    // same-agent steps keep the parent context unchanged.
                    let step_alias = step.agent.as_deref();
                    // Compare against the EFFECTIVE re-assembly baseline (the agent
                    // this loop is actually running as), not `agent_alias` (which
                    // stays pinned to the outer agent for attribution). At depth
                    // >= 2 a re-assembled child carries its step agent as the
                    // baseline; comparing against `agent_alias` there would skip
                    // re-assembling a step that names the outer agent and let it
                    // inherit the child's broader scope. When there is no handle,
                    // the tree never re-assembles, so baseline == `agent_alias`.
                    let baseline_alias = sop_reassembly.map_or(agent_alias, |r| r.baseline_alias);
                    let needs_reassembly = step_needs_reassembly(baseline_alias, step_alias);
                    let mut assembly_error: Option<anyhow::Error> = None;
                    if needs_reassembly {
                        let alias =
                            step_alias.expect("needs_reassembly implies a step agent alias");
                        if let Some(reassembly) = sop_reassembly {
                            if !exec_cache.contains_key(alias) {
                                match assemble_owned_execution(
                                    reassembly.config,
                                    alias,
                                    Arc::clone(&queued.engine),
                                    queued.audit.clone(),
                                )
                                .await
                                {
                                    Ok(owned) => {
                                        exec_cache.insert(alias.to_string(), owned);
                                    }
                                    Err(e) => assembly_error = Some(e),
                                }
                            }
                        } else {
                            // This construction path provides no per-agent re-assembly
                            // handle (e.g. a bounded delegate sub-loop that carries a live
                            // SOP tool). Running a cross-agent step here would inherit the
                            // parent/delegate agent's broader tools/policy/MCP surface —
                            // the exact escalation this whole seam exists to prevent. Fail
                            // the step CLOSED, the same as an assembly error below, rather
                            // than under-enforce in silence (omission is not a grant).
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                ),
                                &format!(
                                    "SOP step delegates to agent '{alias}' but this run path \
                                     provides no per-agent re-assembly handle; refusing the step \
                                     (fail-closed) rather than run it with the parent agent's \
                                     context (per-agent isolation cannot be applied here)"
                                )
                            );
                            assembly_error = Some(anyhow::Error::msg(format!(
                                "SOP step delegates to agent '{alias}' but this run path provides \
                                 no per-agent re-assembly handle; refusing to run the step with \
                                 the parent agent's context (per-agent isolation cannot be applied)"
                            )));
                        }
                    }

                    let nested_turn_id = format!("sop:{run_id}:step:{}", step.number);
                    let step_call_sink = crate::sop::executor::new_step_call_sink();
                    let step_output = if let Some(err) = assembly_error {
                        // Fail closed: never run a delegated step with the parent
                        // agent's broader context when the step agent's own
                        // context could not be assembled.
                        Err(err)
                    } else {
                        // Select the effective per-agent execution surface: the
                        // re-assembled step agent when reassembly applied,
                        // otherwise the parent turn's (byte-identical to today).
                        let owned = if needs_reassembly {
                            exec_cache.get(
                                step_alias.expect("needs_reassembly implies a step agent alias"),
                            )
                        } else {
                            None
                        };
                        // The baseline the step's own nested sub-steps compare
                        // against: this step's agent once we re-assembled it
                        // (`owned` is `Some`), otherwise the current baseline
                        // unchanged. Threaded via `sop_reassembly` so a depth >= 2
                        // sub-step re-resolves scope against the agent actually in
                        // effect, not the outer attribution alias.
                        let nested_sop_reassembly =
                            advance_nested_reassembly(sop_reassembly, step_alias, owned.is_some());
                        let (
                            eff_model_provider,
                            eff_provider_name,
                            eff_model,
                            eff_registry,
                            eff_approval,
                            eff_activated,
                        ) = match owned {
                            Some(o) => (
                                o.model_provider.as_ref(),
                                o.provider_name.as_str(),
                                o.model.as_str(),
                                o.tools_registry.as_slice(),
                                Some(&o.approval),
                                o.activated_tools.as_ref(),
                            ),
                            None => (
                                model_provider,
                                provider_name,
                                model,
                                tools_registry,
                                approval,
                                activated_tools,
                            ),
                        };

                        let sop_excluded_tools = sop_step_excluded_tools(
                            &queued,
                            &run_id,
                            &step,
                            eff_registry,
                            eff_activated,
                            excluded_tools,
                        );

                        crate::sop::executor::scope_step_call_sink(
                            step_call_sink.clone(),
                            Box::pin(run_tool_call_loop(ToolLoop {
                                exec: ResolvedAgentExecution::resolve(
                                    ResolvedModelAccess {
                                        model_provider: eff_model_provider,
                                        provider_name: eff_provider_name,
                                        model: eff_model,
                                        temperature,
                                    },
                                    ResolvedIo {
                                        tools_registry: eff_registry,
                                        observer,
                                        silent,
                                        approval: eff_approval,
                                        multimodal_config,
                                        hooks,
                                        activated_tools: eff_activated,
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
                                channel_name,
                                channel_reply_target,
                                cancellation_token: cancellation_token.clone(),
                                on_delta: on_delta.clone(),
                                shared_budget: shared_budget.clone(),
                                channel,
                                collected_receipts,
                                event_tx: event_tx.clone(),
                                steering: None,
                                new_messages_out: new_messages_out.as_deref_mut(),
                                image_cache: image_cache.as_deref_mut(),
                                memory: None,
                                ingress: IngressContext::sub_turn(),
                                // Attribution stays on the OUTER agent (Observer/
                                // receipts/OTel); the re-assembly baseline rides in
                                // `sop_reassembly` instead. See `advance_nested_reassembly`.
                                agent_alias,
                                turn_id: &nested_turn_id,
                                sop_reassembly: nested_sop_reassembly,
                            })),
                        )
                        .await
                    };

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

/// Per-turn system prompt TASK_FRAMING anchor refresh (#8054 Surface 3).
///
/// The system prompt in `history[0]` was built by
/// `Agent::build_system_prompt()` against the base provider, and may not
/// reflect the iteration's `active_model_provider` after vision routing.
/// This function surgically swaps the `NATIVE_TOOLS_TASK_FRAMING` /
/// `NO_TOOLS_TASK_FRAMING` anchor so the prompt's tool-availability claim
/// matches the actual `request_tools` for this iteration.
///
/// When neither anchor is present (custom `system_prompt_prefix`), the
/// function is a no-op — same as the pre-existing behavior.
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

/// Live SOP nested-step re-assembly gate + fail-closed regressions.
///
/// Two privilege-scope properties of the live driver:
///
/// - **Depth >= 2 baseline (Defect 1).** The re-assembly gate compares a step's
///   agent against the EFFECTIVE baseline (the agent this loop is actually
///   running as), not the attribution `agent_alias`. After a cross-agent
///   re-assembly the two diverge; a step naming the outer agent from inside a
///   re-assembled child must re-assemble, not inherit the child's broader scope.
/// - **No-handle fail-closed (Defect 2).** A driver path with no re-assembly
///   handle (`sop_reassembly: None`, e.g. a bounded delegate sub-loop carrying a
///   live SOP tool) must fail a cross-agent step CLOSED, never run it with the
///   parent/delegate agent's tools.
#[cfg(test)]
mod sop_step_reassembly_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zeroclaw_providers::{ChatResponse, ToolCall};

    // ── Defect 1: gate + baseline threading (pure, on the driver's own fns) ──

    #[test]
    fn gate_reassembles_only_on_agent_change() {
        assert!(
            !step_needs_reassembly(Some("a"), Some("a")),
            "same agent must not re-assemble"
        );
        assert!(
            step_needs_reassembly(Some("a"), Some("b")),
            "a different agent must re-assemble"
        );
        assert!(
            !step_needs_reassembly(Some("a"), None),
            "a step with no explicit agent inherits the current one"
        );
        assert!(
            step_needs_reassembly(None, Some("b")),
            "an unnamed baseline still re-assembles a named step"
        );
        assert!(!step_needs_reassembly(None, None));
    }

    #[test]
    fn nested_baseline_advances_only_when_reassembled() {
        let config = zeroclaw_config::schema::Config::default();
        let top = SopStepReassembly {
            config: &config,
            baseline_alias: Some("root-a"),
        };
        // Re-assembled a step naming "mid-b": the nested sub-loop's baseline
        // becomes "mid-b", carrying the same config handle.
        let nested = advance_nested_reassembly(Some(top), Some("mid-b"), true)
            .expect("handle preserved through re-assembly");
        assert_eq!(nested.baseline_alias, Some("mid-b"));
        // Same-agent step (not re-assembled): baseline unchanged.
        let same = advance_nested_reassembly(Some(top), Some("root-a"), false)
            .expect("handle preserved when not re-assembling");
        assert_eq!(same.baseline_alias, Some("root-a"));
        // No handle stays no handle (a None-tree never re-assembles anywhere).
        assert!(advance_nested_reassembly(None, Some("mid-b"), true).is_none());
    }

    /// The depth-2 regression: root `A` -> step names `B` -> `B` re-assembled ->
    /// `B`'s nested step names root `A`. The `A`-named nested step MUST
    /// re-assemble (run with `A`'s scope), not skip because attribution still
    /// reads `A`. This composes the exact functions the live driver runs across
    /// two frames. A literal end-to-end live drive is infeasible
    /// (`assemble_owned_execution` binds a provider from config with no
    /// scripted-provider seam for the nested loop), so this pins the decision +
    /// threading the driver performs; the real-scope tie is asserted separately
    /// in `reassembly_yields_the_named_agents_own_scope`.
    #[test]
    fn depth2_step_naming_outer_agent_reassembles_against_effective_baseline() {
        let config = zeroclaw_config::schema::Config::default();

        // Frame 0 — root turn runs as agent "A"; the top handle's baseline is "A".
        let root = SopStepReassembly {
            config: &config,
            baseline_alias: Some("A"),
        };

        // Depth-1 step names "B" (!= "A") -> re-assemble B.
        assert!(step_needs_reassembly(root.baseline_alias, Some("B")));
        let depth1 = advance_nested_reassembly(Some(root), Some("B"), /* reassembled */ true)
            .expect("B re-assembled");
        assert_eq!(
            depth1.baseline_alias,
            Some("B"),
            "the nested sub-loop must run with B as its re-assembly baseline"
        );

        // Depth-2 step names the ROOT agent "A" from inside re-assembled B.
        // FIXED: compared against the effective baseline "B", so "A" (!= "B")
        // re-assembles and the step runs with A's own scope.
        assert!(
            step_needs_reassembly(depth1.baseline_alias, Some("A")),
            "a depth-2 step naming the outer agent must re-assemble that agent's scope"
        );

        // Pre-fix escalation contrast: the driver used to thread the OUTER
        // attribution alias unchanged as the nested baseline; comparing a step
        // naming "A" against "A" skipped re-assembly, so the step ran with B's
        // broader scope. Pin that this is exactly what the fix reverses.
        let attribution_alias = Some("A");
        assert!(
            !step_needs_reassembly(attribution_alias, Some("A")),
            "documents the closed escalation: comparing against attribution skips re-assembly"
        );
    }

    fn tool_names(tools: &[Box<dyn crate::tools::Tool>]) -> Vec<String> {
        tools.iter().map(|t| t.name().to_string()).collect()
    }

    /// Real-scope tie for Defect 1: re-assembling a step's named agent yields
    /// THAT agent's own gated tool set, and distinct agents resolve distinct
    /// scopes — so the depth-2 gate decision above is load-bearing (choosing to
    /// re-assemble genuinely changes which tools the step can reach).
    #[tokio::test]
    async fn reassembly_yields_the_named_agents_own_scope() {
        use zeroclaw_config::multi_agent::{AgentMemoryConfig, MemoryBackendKind};
        use zeroclaw_config::schema::{
            AliasedAgentConfig, Config, ModelProviderConfig, OllamaModelProviderConfig,
            RiskProfileConfig, SopConfig,
        };

        let root =
            std::env::temp_dir().join(format!("zeroclaw-sop-depth2-{}", uuid::Uuid::new_v4()));
        let mut config = Config {
            data_dir: root.join("data"),
            config_path: root.join("config.toml"),
            ..Config::default()
        };
        // Two agents whose per-agent policy allowlists exactly one, DIFFERENT
        // built-in each: their assembled scopes must not coincide.
        config.risk_profiles.insert(
            "reader".to_string(),
            RiskProfileConfig {
                allowed_tools: vec!["file_read".to_string()],
                ..RiskProfileConfig::default()
            },
        );
        config.risk_profiles.insert(
            "writer".to_string(),
            RiskProfileConfig {
                allowed_tools: vec!["file_write".to_string()],
                ..RiskProfileConfig::default()
            },
        );
        config.providers.models.ollama.insert(
            "p".to_string(),
            OllamaModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("test-model".to_string()),
                    ..ModelProviderConfig::default()
                },
                ..OllamaModelProviderConfig::default()
            },
        );
        for (alias, profile) in [("reader", "reader"), ("writer", "writer")] {
            config.agents.insert(
                alias.to_string(),
                AliasedAgentConfig {
                    enabled: true,
                    model_provider: "ollama.p".into(),
                    risk_profile: profile.into(),
                    memory: AgentMemoryConfig {
                        backend: MemoryBackendKind::Markdown,
                    },
                    ..AliasedAgentConfig::default()
                },
            );
        }
        let engine = Arc::new(std::sync::Mutex::new(crate::sop::SopEngine::new(
            SopConfig::default(),
        )));

        let reader = assemble_owned_execution(&config, "reader", Arc::clone(&engine), None)
            .await
            .expect("reader assembles");
        let writer = assemble_owned_execution(&config, "writer", Arc::clone(&engine), None)
            .await
            .expect("writer assembles");
        let reader_names = tool_names(&reader.tools_registry);
        let writer_names = tool_names(&writer.tools_registry);

        assert!(
            reader_names.contains(&"file_read".to_string())
                && !reader_names.contains(&"file_write".to_string()),
            "reader gets only its own allowlisted tool: {reader_names:?}"
        );
        assert!(
            writer_names.contains(&"file_write".to_string())
                && !writer_names.contains(&"file_read".to_string()),
            "writer gets only its own allowlisted tool: {writer_names:?}"
        );
        // The gate re-assembles when a step names the other agent, so a
        // cross-agent step lands in the named agent's scope, not the baseline's.
        assert!(step_needs_reassembly(Some("reader"), Some("writer")));
    }

    // ── Defect 2: no-handle cross-agent step fails closed ────────────────────

    /// A provider that, if the nested loop ever ran, would drive the parent's
    /// sensitive tool. Under the fix the fail-closed guard short-circuits before
    /// the nested loop, so `chat` is never polled.
    struct ShellCallingProvider;

    impl ::zeroclaw_api::attribution::Attributable for ShellCallingProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "ShellCallingProvider"
        }
    }

    #[async_trait::async_trait]
    impl ModelProvider for ShellCallingProvider {
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
            _request: zeroclaw_api::model_provider::ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<ChatResponse> {
            Ok(ChatResponse {
                text: Some(String::new()),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "shell".into(),
                    arguments: "{}".into(),
                    extra_content: None,
                }],
                usage: None,
                reasoning_content: None,
            })
        }
    }

    /// The parent/delegate agent's sensitive tool; counts executions so the test
    /// can prove a no-handle cross-agent step never reaches it.
    struct ShellProbe {
        calls: Arc<AtomicUsize>,
    }

    ::zeroclaw_api::tool_attribution!(ShellProbe, ::zeroclaw_api::attribution::ToolKind::Plugin);

    #[async_trait::async_trait]
    impl crate::tools::Tool for ShellProbe {
        fn name(&self) -> &str {
            "shell"
        }
        fn description(&self) -> &str {
            "shell"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(&self, _args: serde_json::Value) -> Result<crate::tools::ToolResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(crate::tools::ToolResult {
                success: true,
                output: "shell-out".to_string().into(),
                error: None,
            })
        }
    }

    /// A provider that returns plain text (no tool call), so a nested step loop
    /// completes in a single iteration without needing any assembled tools.
    struct TextProvider;

    impl ::zeroclaw_api::attribution::Attributable for TextProvider {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Provider(
                ::zeroclaw_api::attribution::ProviderKind::Model(
                    ::zeroclaw_api::attribution::ModelProviderKind::Custom,
                ),
            )
        }
        fn alias(&self) -> &str {
            "TextProvider"
        }
    }

    #[async_trait::async_trait]
    impl ModelProvider for TextProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<String> {
            Ok("done".into())
        }

        async fn chat(
            &self,
            _request: zeroclaw_api::model_provider::ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> Result<ChatResponse> {
            Ok(ChatResponse {
                text: Some("done".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }

    /// Build a single-step SOP whose step delegates to `step_agent`, start it in a
    /// fresh engine, and return the shared engine handle plus the first
    /// `ExecuteStep` action (already resolved to a cross-agent step).
    fn start_single_cross_agent_step(
        step_agent: &str,
    ) -> (
        Arc<std::sync::Mutex<crate::sop::SopEngine>>,
        String,
        crate::sop::types::SopRunAction,
    ) {
        use crate::sop::types::{
            Sop, SopEvent, SopExecutionMode, SopPriority, SopRunAction, SopStep, SopTrigger,
            SopTriggerSource,
        };
        use zeroclaw_config::schema::SopConfig;

        let sop = Sop {
            name: "cross-agent".to_string(),
            description: "x".to_string(),
            version: "0.1.0".to_string(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Auto,
            triggers: vec![SopTrigger::Manual],
            steps: vec![SopStep {
                number: 1,
                title: "delegate".to_string(),
                body: "run".to_string(),
                agent: Some(step_agent.to_string()),
                ..SopStep::default()
            }],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
            agent: None,
        };
        let mut engine = crate::sop::SopEngine::new(SopConfig::default());
        engine.set_sops_for_test(vec![sop]);
        let event = SopEvent {
            source: SopTriggerSource::Manual,
            topic: None,
            payload: None,
            timestamp: "2026-07-16T00:00:00Z".to_string(),
        };
        let action = engine.start_run("cross-agent", event).expect("run starts");
        let run_id = match &action {
            SopRunAction::ExecuteStep { run_id, step, .. } => {
                assert_eq!(
                    step.agent.as_deref(),
                    Some(step_agent),
                    "the step must resolve to a cross-agent delegation"
                );
                run_id.clone()
            }
            other => panic!("expected ExecuteStep, got {other:?}"),
        };
        (Arc::new(std::sync::Mutex::new(engine)), run_id, action)
    }

    /// Runs `drive_live_sop_actions` over a single queued cross-agent step with a
    /// plain-text provider and the given `agent_alias` / `sop_reassembly`, then
    /// returns the recorded status of step 1.
    async fn drive_single_step_status(
        engine: Arc<std::sync::Mutex<crate::sop::SopEngine>>,
        run_id: &str,
        action: crate::sop::types::SopRunAction,
        agent_alias: Option<&str>,
        sop_reassembly: Option<SopStepReassembly<'_>>,
    ) -> crate::sop::types::SopStepStatus {
        use crate::sop::executor::QueuedSopAction;

        let provider = TextProvider;
        let parent_tools: Vec<Box<dyn crate::tools::Tool>> = Vec::new();
        let queued = QueuedSopAction {
            engine: Arc::clone(&engine),
            audit: None,
            action,
        };
        let mut history: Vec<ChatMessage> = Vec::new();

        drive_live_sop_actions(
            vec![queued],
            &mut history,
            &provider,
            "mock",
            "mock-model",
            None,
            &parent_tools,
            &crate::observability::NoopObserver {},
            true,
            None,
            &zeroclaw_config::schema::MultimodalConfig::default(),
            5,
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            false,
            false,
            30_000,
            100_000,
            None,
            &LoopKnobs::default(),
            "cli",
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            agent_alias,
            sop_reassembly,
        )
        .await
        .expect("drive returns Ok");

        let guard = engine.lock().expect("engine lock");
        guard
            .get_run(run_id)
            .expect("run present after drive")
            .step_results
            .iter()
            .find(|r| r.step_number == 1)
            .expect("step 1 result recorded")
            .status
    }

    /// Driver-level guard for Defect 1's fix line: the re-assembly gate reads the
    /// baseline from the `sop_reassembly` HANDLE (`|r| r.baseline_alias`), not the
    /// attribution `agent_alias`. Set the handle's baseline == the step agent
    /// ("stepper") while attribution reads a different agent ("outer").
    ///
    /// - FIXED: baseline "stepper" == step "stepper" -> no re-assembly -> the
    ///   nested loop runs the plain-text provider -> step Completed.
    /// - If the fix line reverts to `agent_alias`: baseline "outer" != "stepper"
    ///   -> the gate tries to assemble "stepper" from the bare handle config
    ///   (no such agent / risk profile) -> assembly error -> step Failed.
    ///
    /// The Completed-vs-Failed split makes this test fail if the driver stops
    /// consulting the handle baseline. (The deeper two-frame threading revert is
    /// not feasibly drivable — frame-1 re-assembly would bind a real provider for
    /// the nested loop — so that half stays pinned by the pure composition test
    /// above plus the wired call to `advance_nested_reassembly`.)
    #[tokio::test]
    async fn gate_reads_baseline_from_handle_not_attribution_alias() {
        let (engine, run_id, action) = start_single_cross_agent_step("stepper");
        let config = zeroclaw_config::schema::Config::default();
        let handle = SopStepReassembly {
            config: &config,
            baseline_alias: Some("stepper"),
        };
        let status = drive_single_step_status(
            Arc::clone(&engine),
            &run_id,
            action,
            // Attribution reads a DIFFERENT agent than the handle baseline; the
            // gate must consult the handle, not this.
            Some("outer"),
            Some(handle),
        )
        .await;
        assert_eq!(
            status,
            crate::sop::types::SopStepStatus::Completed,
            "the gate must read the baseline from the handle (== step agent -> no \
             re-assembly -> the text step completes); a revert to the attribution \
             alias would try to assemble a nonexistent agent and fail the step"
        );
    }

    #[tokio::test]
    async fn no_handle_cross_agent_step_fails_closed_not_parent_scope() {
        use crate::sop::executor::QueuedSopAction;
        use crate::sop::types::{
            Sop, SopEvent, SopExecutionMode, SopPriority, SopRunAction, SopStep, SopStepStatus,
            SopTrigger, SopTriggerSource,
        };
        use zeroclaw_config::schema::SopConfig;

        // A single-step SOP whose step delegates to a DIFFERENT agent than the
        // one running the (bounded-delegate-like) loop.
        let sop = Sop {
            name: "bounded-cross-agent".to_string(),
            description: "x".to_string(),
            version: "0.1.0".to_string(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Auto,
            triggers: vec![SopTrigger::Manual],
            steps: vec![SopStep {
                number: 1,
                title: "delegate".to_string(),
                body: "run".to_string(),
                agent: Some("other-agent".to_string()),
                ..SopStep::default()
            }],
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
            agent: None,
        };
        let mut engine = crate::sop::SopEngine::new(SopConfig::default());
        engine.set_sops_for_test(vec![sop]);
        let event = SopEvent {
            source: SopTriggerSource::Manual,
            topic: None,
            payload: None,
            timestamp: "2026-07-16T00:00:00Z".to_string(),
        };
        let action = engine
            .start_run("bounded-cross-agent", event)
            .expect("run starts");
        let run_id = match &action {
            SopRunAction::ExecuteStep { run_id, step, .. } => {
                assert_eq!(
                    step.agent.as_deref(),
                    Some("other-agent"),
                    "the step must resolve to a cross-agent delegation"
                );
                run_id.clone()
            }
            other => panic!("expected ExecuteStep, got {other:?}"),
        };
        let engine = Arc::new(std::sync::Mutex::new(engine));

        let shell_calls = Arc::new(AtomicUsize::new(0));
        // Parent/delegate scope: a sensitive tool the cross-agent step must never
        // reach.
        let parent_tools: Vec<Box<dyn crate::tools::Tool>> = vec![Box::new(ShellProbe {
            calls: Arc::clone(&shell_calls),
        })];
        let provider = ShellCallingProvider;

        let queued = QueuedSopAction {
            engine: Arc::clone(&engine),
            audit: None,
            action,
        };
        let mut history: Vec<ChatMessage> = Vec::new();

        drive_live_sop_actions(
            vec![queued],
            &mut history,
            &provider,
            "mock",
            "mock-model",
            None,
            &parent_tools,
            &crate::observability::NoopObserver {},
            true,
            None,
            &zeroclaw_config::schema::MultimodalConfig::default(),
            5,
            None,
            &[],
            &[],
            None,
            None,
            &zeroclaw_config::schema::PacingConfig::default(),
            false,
            false,
            30_000,
            100_000,
            None,
            &LoopKnobs::default(),
            "cli",
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            // The bounded-delegate loop runs AS "delegate"; the step names a
            // different agent, and this path carries NO re-assembly handle.
            Some("delegate"),
            None,
        )
        .await
        .expect("drive returns Ok even when the step fails closed");

        // Security property: the parent's sensitive tool never executed — the
        // cross-agent step did not run with the parent/delegate scope.
        assert_eq!(
            shell_calls.load(Ordering::SeqCst),
            0,
            "a no-handle cross-agent step must not run with the parent agent's tools"
        );

        // And the step is recorded FAILED (fail-closed), not Completed.
        let guard = engine.lock().expect("engine lock");
        let run = guard.get_run(&run_id).expect("run present after drive");
        let step1 = run
            .step_results
            .iter()
            .find(|r| r.step_number == 1)
            .expect("step 1 result recorded");
        assert_eq!(
            step1.status,
            SopStepStatus::Failed,
            "the no-handle cross-agent step must fail closed: {:?}",
            step1.status
        );
    }
}
