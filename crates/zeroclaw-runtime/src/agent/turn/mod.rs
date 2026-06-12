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
pub(crate) use context::TurnCtx;
pub(crate) use context_recovery::{record_llm_failure, try_recover_context_overflow};
#[cfg(test)]
pub(crate) use delivery_defaults::maybe_inject_channel_delivery_defaults;
pub use events::{DraftEvent, PROGRESS_MIN_INTERVAL_MS, StreamDelta};
pub(crate) use history_append::append_tool_round_to_history;
pub(crate) use history_window::preflight_history_maintenance;
pub use knobs::{LoopKnobs, MaxIterationBehavior};
pub(crate) use max_iter::finish_after_max_iterations;
pub use outcome::{
    ModelSwitchCallback, ModelSwitchRequested, ToolLoopCancelled, is_model_switch_requested,
    is_tool_loop_cancelled,
};
#[cfg(test)]
pub(crate) use parse_response::build_native_assistant_history;
pub(crate) use parse_response::{interpret_chat_response, resolve_display_text};
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
pub(crate) use stream_consume::StreamedChatOutcome;
#[cfg(test)]
pub(crate) use stream_consume::consume_provider_streaming_response;
pub(crate) use tool_specs::{IterationToolSpecs, build_iteration_tool_specs};
pub(crate) use vision_route::{prepare_messages_for_iteration, resolve_vision_provider};

use crate::agent::tool_execution::{
    execute_tools_parallel, execute_tools_sequential, should_execute_tools_in_parallel,
};
use crate::approval::ApprovalManager;
use crate::observability::Observer;
use crate::tools::Tool;
use crate::util::truncate_with_ellipsis;
use anyhow::Result;
use std::collections::HashSet;
use std::io::Write as _;
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zeroclaw_api::agent::TurnEvent;
use zeroclaw_api::channel::Channel;
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

/// Append a receipt footer to the response text if any receipts were collected.
/// Execute a single turn of the agent loop: send messages, parse tool calls,
/// execute tools, and loop until the LLM produces a final text response.
#[allow(clippy::too_many_arguments)]
pub async fn run_tool_call_loop(
    model_provider: &dyn ModelProvider,
    history: &mut Vec<ChatMessage>,
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    provider_name: &str,
    model: &str,
    temperature: Option<f64>,
    silent: bool,
    approval: Option<&ApprovalManager>,
    channel_name: &str,
    channel_reply_target: Option<&str>,
    multimodal_config: &zeroclaw_config::schema::MultimodalConfig,
    max_tool_iterations: usize,
    cancellation_token: Option<CancellationToken>,
    on_delta: Option<tokio::sync::mpsc::Sender<DraftEvent>>,
    hooks: Option<&crate::hooks::HookRunner>,
    excluded_tools: &[String],
    dedup_exempt_tools: &[String],
    activated_tools: Option<&std::sync::Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
    model_switch_callback: Option<ModelSwitchCallback>,
    pacing: &zeroclaw_config::schema::PacingConfig,
    strict_tool_parsing: bool,
    parallel_tools: bool,
    max_tool_result_chars: usize,
    context_token_budget: usize,
    shared_budget: Option<Arc<std::sync::atomic::AtomicUsize>>,
    channel: Option<&dyn Channel>,
    receipt_generator: Option<&crate::agent::tool_receipts::ReceiptGenerator>,
    collected_receipts: Option<&std::sync::Mutex<Vec<String>>>,
    event_tx: Option<tokio::sync::mpsc::Sender<TurnEvent>>,
    mut steering: Option<&mut tokio::sync::mpsc::Receiver<String>>,
    mut new_messages_out: Option<&mut Vec<ChatMessage>>,
    knobs: &LoopKnobs,
) -> Result<String> {
    let max_iterations = if max_tool_iterations == 0 {
        DEFAULT_MAX_TOOL_ITERATIONS
    } else {
        max_tool_iterations
    };

    let turn_id = Uuid::new_v4().to_string();
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
    let initial_history_len = history.len();

    // Shared-ref context for the turn step functions. Every `&mut` the loop
    // owns stays a loop local passed as an explicit argument (RUN_SHEET
    // `turn.context.TurnCtx`).
    let ctx = TurnCtx {
        model_provider,
        tools_registry,
        observer,
        provider_name,
        model,
        temperature,
        silent,
        approval,
        channel_name,
        channel_reply_target,
        multimodal_config,
        cancellation_token: cancellation_token.as_ref(),
        on_delta: on_delta.as_ref(),
        event_tx: event_tx.as_ref(),
        hooks,
        excluded_tools,
        dedup_exempt_tools,
        activated_tools,
        pacing,
        strict_tool_parsing,
        parallel_tools,
        max_tool_result_chars,
        channel,
        receipt_generator,
        collected_receipts,
        turn_id: &turn_id,
    };

    for iteration in 0..max_iterations {
        // Steering: fold caller-pushed mid-turn messages into history before
        // this iteration's provider request.
        for steering_message in drain_steering_messages(&mut steering) {
            history.push(ChatMessage::user(steering_message));
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
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"iteration": iteration})),
                    "Shared iteration budget exhausted at iteration "
                );
                break;
            }
            budget.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }

        preflight_history_maintenance(history, context_token_budget, iteration);

        // Check if model switch was requested via model_switch tool
        if let Some(ref callback) = model_switch_callback
            && let Ok(guard) = callback.lock()
            && let Some((new_model_provider, new_model)) = guard.as_ref()
            && (new_model_provider != provider_name || new_model != model)
        {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
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

        let iteration_tool_specs = build_iteration_tool_specs(
            model_provider,
            tools_registry,
            excluded_tools,
            activated_tools,
        );
        let IterationToolSpecs {
            ref tool_specs,
            use_native_tools,
            ..
        } = iteration_tool_specs;

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

        let prepared_messages =
            prepare_messages_for_iteration(history, multimodal_config, degrade_strip_images)
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
        let should_consume_provider_stream = (on_delta.is_some() || event_tx.is_some())
            && model_provider.supports_streaming()
            && (request_tools.is_none() || model_provider.supports_streaming_tool_events());
        ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"has_on_delta": on_delta.is_some(), "has_event_tx": event_tx.is_some(), "supports_streaming": model_provider.supports_streaming(), "should_consume_provider_stream": should_consume_provider_stream})), &format!("Streaming decision for iteration {}", iteration + 1));

        let ProviderCallOutcome {
            chat_result,
            streamed_live_deltas,
            streamed_protocol_suppressed,
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
        ) = match chat_result {
            Ok(resp) => {
                let interpreted = interpret_chat_response(
                    &ctx,
                    resp,
                    &iteration_tool_specs,
                    streamed_protocol_suppressed,
                    llm_started_at,
                    iteration,
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
                )
            }
            Err(e) => {
                record_llm_failure(
                    observer,
                    provider_name,
                    model,
                    llm_started_at,
                    iteration,
                    &turn_id,
                    &e,
                );
                let recovered = try_recover_context_overflow(history, &e, iteration);
                if recovered {
                    continue;
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
                history.push(ChatMessage::user(
                    "[Tool call parse error]\n\
                     Your previous response looked like an internal tool-call protocol payload, \
                     but ZeroClaw could not parse it into a valid tool call. Use the supported \
                     tool-call schema, or answer in natural language if no tool is needed."
                        .to_string(),
                ));
                continue;
            }

            let fallback =
                crate::i18n::get_required_cli_string("channel-runtime-malformed-tool-output");
            accumulated_display_text.push_str(&fallback);
            if let Some(ref tx) = on_delta {
                let _ = tx.send(StreamDelta::Text(fallback.to_string())).await;
            }
            history.push(ChatMessage::assistant(fallback.to_string()));
            if let Some(out) = new_messages_out.as_deref_mut() {
                *out = history[initial_history_len..].to_vec();
            }
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

            history.push(ChatMessage::assistant(response_text.clone()));
            if let Some(out) = new_messages_out.as_deref_mut() {
                *out = history[initial_history_len..].to_vec();
            }
            return Ok(accumulated_display_text);
        }

        // Do not accumulate intermediate-turn display text into the final
        // channel response. Native tool-call providers may emit narration or
        // scratchpad-like text alongside tool calls; draft-capable channels
        // can still see it live through `on_delta` below, but the final
        // delivered response must only contain the final assistant turn.

        // Native tool-call model_providers can return assistant text separately from
        // the structured call payload; relay it to draft-capable channels.
        if !display_text.is_empty() {
            if !native_tool_calls.is_empty()
                && let Some(ref tx) = on_delta
            {
                let mut narration = display_text.clone();
                if !narration.ends_with('\n') {
                    narration.push('\n');
                }
                let _ = tx.send(StreamDelta::Text(narration)).await;
            }
            if !silent {
                print!("{display_text}");
                let _ = std::io::stdout().flush();
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
            iteration,
            knobs.dedup_enabled,
        )
        .await;

        let executed_outcomes = if allow_parallel_execution && executable_calls.len() > 1 {
            execute_tools_parallel(
                &executable_calls,
                tools_registry,
                activated_tools,
                observer,
                cancellation_token.as_ref(),
                receipt_generator,
            )
            .await?
        } else {
            execute_tools_sequential(
                &executable_calls,
                tools_registry,
                activated_tools,
                observer,
                cancellation_token.as_ref(),
                receipt_generator,
            )
            .await?
        };

        record_executed_outcomes(
            &ctx,
            &executable_indices,
            &executable_calls,
            executed_outcomes,
            &mut ordered_results,
            iteration,
        )
        .await;

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
            &turn_id,
        )?;

        check_identical_output_abort(
            &detection_relevant_output,
            loop_started_at,
            pacing,
            &mut consecutive_identical_outputs,
            &mut last_tool_output_hash,
            model,
            iteration,
            &turn_id,
        )?;

        append_tool_round_to_history(
            history,
            assistant_history_content,
            &native_tool_calls,
            &individual_results,
            &tool_results,
            use_native_tools,
        );
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
        &turn_id,
        knobs,
        new_messages_out,
        initial_history_len,
    )
    .await
}
