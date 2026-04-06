use crate::approval::{ApprovalManager, ApprovalRequest, ApprovalResponse};
use crate::cost::types::BudgetCheck;
use crate::i18n::ToolDescriptions;
use crate::multimodal;
use crate::observability::{Observer, ObserverEvent, runtime_trace};
use crate::providers::{
    self, ChatMessage, ChatRequest, Provider, ProviderCapabilityError, ToolCall,
};
use crate::tools::Tool;
use crate::util::truncate_with_ellipsis;
use anyhow::Result;
use std::collections::HashSet;
use std::fmt::Write;
use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

// Cost tracking moved to `super::cost`.
pub(crate) use super::cost::{
    TOOL_LOOP_COST_TRACKING_CONTEXT, ToolLoopCostTrackingContext, check_tool_loop_budget,
    record_tool_loop_cost_usage,
};

/// Default maximum agentic tool-use iterations per user message to prevent runaway loops.
const DEFAULT_MAX_TOOL_ITERATIONS: usize = 10;

// History management moved to `super::history`.
pub(crate) use super::history::{
    emergency_history_trim, estimate_history_tokens, fast_trim_tool_results, truncate_tool_result,
};

// Model switch state moved to `super::model_switch`.
pub(crate) use super::model_switch::{
    ModelSwitchCallback, ModelSwitchRequested, clear_model_switch_request, get_model_switch_state,
    is_model_switch_requested,
};

// Tool filtering moved to `super::tool_filter`.
pub(crate) use super::tool_filter::compute_excluded_mcp_tools;

// Credential scrubbing moved to `super::credentials`.
pub(crate) use super::credentials::scrub_credentials;

// Streaming moved to `super::streaming`.
pub(crate) use super::streaming::{
    DraftEvent, STREAM_CHUNK_MIN_CHARS, consume_provider_streaming_response,
};

// Tool execution moved to `super::tool_execution`.
pub(crate) use super::tool_execution::{
    ToolExecutionOutcome, execute_tools_parallel, execute_tools_sequential,
    should_execute_tools_in_parallel,
};

// Tool-call parsing moved to `super::tool_call_parser`.
pub(crate) use super::tool_call_parser::{
    ParsedToolCall, canonicalize_json_for_tool_signature, detect_tool_call_parse_issue,
    parse_tool_calls,
};

// Entrypoint functions (`run`, `process_message`) moved to `super::entrypoint`.
#[allow(unused_imports)]
pub use super::entrypoint::{process_message, run};

tokio::task_local! {
    /// Stable thread/conversation identifier from the incoming channel message.
    /// Used by [`PerSenderTracker`] to isolate rate-limit buckets per chat.
    /// Set from the channel's thread ID, topic ID, or message ID.
    pub static TOOL_LOOP_THREAD_ID: Option<String>;
}

/// Run a future with the thread ID set in task-local storage.
/// Rate-limiting reads this to assign per-sender buckets.
pub async fn scope_thread_id<F>(thread_id: Option<String>, future: F) -> F::Output
where
    F: std::future::Future,
{
    TOOL_LOOP_THREAD_ID.scope(thread_id, future).await
}

tokio::task_local! {
    pub(crate) static TOOL_CHOICE_OVERRIDE: Option<String>;
}

fn build_native_assistant_history(
    text: &str,
    tool_calls: &[ToolCall],
    reasoning_content: Option<&str>,
) -> String {
    let calls_json: Vec<serde_json::Value> = tool_calls
        .iter()
        .map(|tc| {
            serde_json::json!({
                "id": tc.id,
                "name": tc.name,
                "arguments": tc.arguments,
            })
        })
        .collect();

    let content = if text.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(text.trim().to_string())
    };

    let mut obj = serde_json::json!({
        "content": content,
        "tool_calls": calls_json,
    });

    if let Some(rc) = reasoning_content {
        obj.as_object_mut().unwrap().insert(
            "reasoning_content".to_string(),
            serde_json::Value::String(rc.to_string()),
        );
    }

    obj.to_string()
}

fn build_native_assistant_history_from_parsed_calls(
    text: &str,
    tool_calls: &[ParsedToolCall],
    reasoning_content: Option<&str>,
) -> Option<String> {
    let calls_json = tool_calls
        .iter()
        .map(|tc| {
            Some(serde_json::json!({
                "id": tc.tool_call_id.clone()?,
                "name": tc.name,
                "arguments": serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".to_string()),
            }))
        })
        .collect::<Option<Vec<_>>>()?;

    let content = if text.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(text.trim().to_string())
    };

    let mut obj = serde_json::json!({
        "content": content,
        "tool_calls": calls_json,
    });

    if let Some(rc) = reasoning_content {
        obj.as_object_mut().unwrap().insert(
            "reasoning_content".to_string(),
            serde_json::Value::String(rc.to_string()),
        );
    }

    Some(obj.to_string())
}

fn resolve_display_text(
    response_text: &str,
    parsed_text: &str,
    has_tool_calls: bool,
    has_native_tool_calls: bool,
) -> String {
    if has_tool_calls {
        if !parsed_text.is_empty() {
            return parsed_text.to_string();
        }
        if has_native_tool_calls {
            return response_text.to_string();
        }
        return String::new();
    }

    if parsed_text.is_empty() {
        response_text.to_string()
    } else {
        parsed_text.to_string()
    }
}

#[derive(Debug)]
pub(crate) struct ToolLoopCancelled;

impl std::fmt::Display for ToolLoopCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("tool loop cancelled")
    }
}

impl std::error::Error for ToolLoopCancelled {}

pub(crate) fn is_tool_loop_cancelled(err: &anyhow::Error) -> bool {
    err.chain().any(|source| source.is::<ToolLoopCancelled>())
}

/// Execute a single turn of the agent loop: send messages, parse tool calls,
/// execute tools, and loop until the LLM produces a final text response.
/// When `silent` is true, suppresses stdout (for channel use).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn agent_turn(
    provider: &dyn Provider,
    history: &mut Vec<ChatMessage>,
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    provider_name: &str,
    model: &str,
    temperature: f64,
    silent: bool,
    channel_name: &str,
    channel_reply_target: Option<&str>,
    multimodal_config: &crate::config::MultimodalConfig,
    max_tool_iterations: usize,
    approval: Option<&ApprovalManager>,
    excluded_tools: &[String],
    dedup_exempt_tools: &[String],
    activated_tools: Option<&std::sync::Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
    model_switch_callback: Option<ModelSwitchCallback>,
) -> Result<String> {
    run_tool_call_loop(
        provider,
        history,
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
        max_tool_iterations,
        None,
        None,
        None,
        excluded_tools,
        dedup_exempt_tools,
        activated_tools,
        model_switch_callback,
        &crate::config::PacingConfig::default(),
        0,    // max_tool_result_chars: 0 = disabled (legacy callers)
        0,    // context_token_budget: 0 = disabled (legacy callers)
        None, // shared_budget: no shared budget for legacy callers
    )
    .await
}

fn maybe_inject_channel_delivery_defaults(
    tool_name: &str,
    tool_args: &mut serde_json::Value,
    channel_name: &str,
    channel_reply_target: Option<&str>,
) {
    if tool_name != "cron_add" {
        return;
    }

    if !matches!(
        channel_name,
        "telegram" | "discord" | "slack" | "mattermost" | "matrix"
    ) {
        return;
    }

    let Some(reply_target) = channel_reply_target
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };

    let Some(args) = tool_args.as_object_mut() else {
        return;
    };

    let is_agent_job = args
        .get("job_type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|job_type| job_type.eq_ignore_ascii_case("agent"))
        || args
            .get("prompt")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|prompt| !prompt.trim().is_empty());
    if !is_agent_job {
        return;
    }

    let default_delivery = || {
        serde_json::json!({
            "mode": "announce",
            "channel": channel_name,
            "to": reply_target,
        })
    };

    match args.get_mut("delivery") {
        None => {
            args.insert("delivery".to_string(), default_delivery());
        }
        Some(serde_json::Value::Null) => {
            *args.get_mut("delivery").expect("delivery key exists") = default_delivery();
        }
        Some(serde_json::Value::Object(delivery)) => {
            if delivery
                .get("mode")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|mode| mode.eq_ignore_ascii_case("none"))
            {
                return;
            }

            delivery
                .entry("mode".to_string())
                .or_insert_with(|| serde_json::Value::String("announce".to_string()));

            let needs_channel = delivery
                .get("channel")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|value| value.trim().is_empty());
            if needs_channel {
                delivery.insert(
                    "channel".to_string(),
                    serde_json::Value::String(channel_name.to_string()),
                );
            }

            let needs_target = delivery
                .get("to")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|value| value.trim().is_empty());
            if needs_target {
                delivery.insert(
                    "to".to_string(),
                    serde_json::Value::String(reply_target.to_string()),
                );
            }
        }
        Some(_) => {}
    }
}

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
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_tool_call_loop(
    provider: &dyn Provider,
    history: &mut Vec<ChatMessage>,
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    provider_name: &str,
    model: &str,
    temperature: f64,
    silent: bool,
    approval: Option<&ApprovalManager>,
    channel_name: &str,
    channel_reply_target: Option<&str>,
    multimodal_config: &crate::config::MultimodalConfig,
    max_tool_iterations: usize,
    cancellation_token: Option<CancellationToken>,
    on_delta: Option<tokio::sync::mpsc::Sender<DraftEvent>>,
    hooks: Option<&crate::hooks::HookRunner>,
    excluded_tools: &[String],
    dedup_exempt_tools: &[String],
    activated_tools: Option<&std::sync::Arc<std::sync::Mutex<crate::tools::ActivatedToolSet>>>,
    model_switch_callback: Option<ModelSwitchCallback>,
    pacing: &crate::config::PacingConfig,
    max_tool_result_chars: usize,
    context_token_budget: usize,
    shared_budget: Option<Arc<std::sync::atomic::AtomicUsize>>,
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

    for iteration in 0..max_iterations {
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
                tracing::warn!("Shared iteration budget exhausted at iteration {iteration}");
                break;
            }
            budget.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }

        // Preemptive context management: trim history before it overflows
        if context_token_budget > 0 {
            let estimated = estimate_history_tokens(history);
            if estimated > context_token_budget {
                tracing::info!(
                    estimated,
                    budget = context_token_budget,
                    iteration = iteration + 1,
                    "Preemptive context trim: estimated tokens exceed budget"
                );
                let chars_saved = fast_trim_tool_results(history, 4);
                if chars_saved > 0 {
                    tracing::info!(chars_saved, "Preemptive fast-trim applied");
                }
                // If still over budget, use the history pruner for deeper cleanup
                let recheck = estimate_history_tokens(history);
                if recheck > context_token_budget {
                    let stats = crate::agent::history_pruner::prune_history(
                        history,
                        &crate::agent::history_pruner::HistoryPrunerConfig {
                            enabled: true,
                            max_tokens: context_token_budget,
                            keep_recent: 4,
                            collapse_tool_results: true,
                        },
                    );
                    if stats.dropped_messages > 0 || stats.collapsed_pairs > 0 {
                        tracing::info!(
                            collapsed = stats.collapsed_pairs,
                            dropped = stats.dropped_messages,
                            "Preemptive history prune applied"
                        );
                    }
                }
            }
        }

        // Check if model switch was requested via model_switch tool
        if let Some(ref callback) = model_switch_callback {
            if let Ok(guard) = callback.lock() {
                if let Some((new_provider, new_model)) = guard.as_ref() {
                    if new_provider != provider_name || new_model != model {
                        tracing::info!(
                            "Model switch detected: {} {} -> {} {}",
                            provider_name,
                            model,
                            new_provider,
                            new_model
                        );
                        return Err(ModelSwitchRequested {
                            provider: new_provider.clone(),
                            model: new_model.clone(),
                        }
                        .into());
                    }
                }
            }
        }

        // Rebuild tool_specs each iteration so newly activated deferred tools appear.
        let mut tool_specs: Vec<crate::tools::ToolSpec> = tools_registry
            .iter()
            .filter(|tool| !excluded_tools.iter().any(|ex| ex == tool.name()))
            .map(|tool| tool.spec())
            .collect();
        if let Some(at) = activated_tools {
            for spec in at.lock().unwrap().tool_specs() {
                if !excluded_tools.iter().any(|ex| ex == &spec.name) {
                    tool_specs.push(spec);
                }
            }
        }
        let use_native_tools = provider.supports_native_tools() && !tool_specs.is_empty();

        let image_marker_count = multimodal::count_image_markers(history);

        // ── Vision provider routing ──────────────────────────
        // When the default provider lacks vision support but a dedicated
        // vision_provider is configured, create it on demand and use it
        // for this iteration.  Otherwise, preserve the original error.
        let vision_provider_box: Option<Box<dyn Provider>> = if image_marker_count > 0
            && !provider.supports_vision()
        {
            if let Some(ref vp) = multimodal_config.vision_provider {
                let vp_instance = providers::create_provider(vp, None)
                    .map_err(|e| anyhow::anyhow!("failed to create vision provider '{vp}': {e}"))?;
                if !vp_instance.supports_vision() {
                    return Err(ProviderCapabilityError {
                        provider: vp.clone(),
                        capability: "vision".to_string(),
                        message: format!(
                            "configured vision_provider '{vp}' does not support vision input"
                        ),
                    }
                    .into());
                }
                Some(vp_instance)
            } else {
                return Err(ProviderCapabilityError {
                        provider: provider_name.to_string(),
                        capability: "vision".to_string(),
                        message: format!(
                            "received {image_marker_count} image marker(s), but this provider does not support vision input"
                        ),
                    }
                    .into());
            }
        } else {
            None
        };

        let (active_provider, active_provider_name, active_model): (&dyn Provider, &str, &str) =
            if let Some(ref vp_box) = vision_provider_box {
                let vp_name = multimodal_config
                    .vision_provider
                    .as_deref()
                    .unwrap_or(provider_name);
                let vm = multimodal_config.vision_model.as_deref().unwrap_or(model);
                (vp_box.as_ref(), vp_name, vm)
            } else {
                (provider, provider_name, model)
            };

        let prepared_messages =
            multimodal::prepare_messages_for_provider(history, multimodal_config).await?;

        // ── Progress: LLM thinking ────────────────────────────
        if let Some(ref tx) = on_delta {
            let phase = if iteration == 0 {
                "\u{1f914} Thinking...\n".to_string()
            } else {
                format!("\u{1f914} Thinking (round {})...\n", iteration + 1)
            };
            let _ = tx.send(DraftEvent::Progress(phase)).await;
        }

        observer.record_event(&ObserverEvent::LlmRequest {
            provider: active_provider_name.to_string(),
            model: active_model.to_string(),
            messages_count: history.len(),
        });
        runtime_trace::record_event(
            "llm_request",
            Some(channel_name),
            Some(active_provider_name),
            Some(active_model),
            Some(&turn_id),
            None,
            None,
            serde_json::json!({
                "iteration": iteration + 1,
                "messages_count": history.len(),
            }),
        );

        let llm_started_at = Instant::now();

        // Fire void hook before LLM call
        if let Some(hooks) = hooks {
            hooks.fire_llm_input(history, model).await;
        }

        // Budget enforcement — block if limit exceeded (no-op when not scoped)
        if let Some(BudgetCheck::Exceeded {
            current_usd,
            limit_usd,
            period,
        }) = check_tool_loop_budget()
        {
            return Err(anyhow::anyhow!(
                "Budget exceeded: ${:.4} of ${:.2} {:?} limit. Cannot make further API calls until the budget resets.",
                current_usd,
                limit_usd,
                period
            ));
        }

        // Unified path via Provider::chat so provider-specific native tool logic
        // (OpenAI/Anthropic/OpenRouter/compatible adapters) is honored.
        let request_tools = if use_native_tools {
            Some(tool_specs.as_slice())
        } else {
            None
        };
        let should_consume_provider_stream = on_delta.is_some()
            && provider.supports_streaming()
            && (request_tools.is_none() || provider.supports_streaming_tool_events());
        tracing::debug!(
            has_on_delta = on_delta.is_some(),
            supports_streaming = provider.supports_streaming(),
            should_consume_provider_stream,
            "Streaming decision for iteration {}",
            iteration + 1,
        );
        let mut streamed_live_deltas = false;

        let chat_result = if should_consume_provider_stream {
            match consume_provider_streaming_response(
                active_provider,
                &prepared_messages.messages,
                request_tools,
                active_model,
                temperature,
                cancellation_token.as_ref(),
                on_delta.as_ref(),
            )
            .await
            {
                Ok(streamed) => {
                    streamed_live_deltas = streamed.forwarded_live_deltas;
                    Ok(crate::providers::ChatResponse {
                        text: Some(streamed.response_text),
                        tool_calls: streamed.tool_calls,
                        usage: None,
                        reasoning_content: None,
                    })
                }
                Err(stream_err) => {
                    tracing::warn!(
                        provider = active_provider_name,
                        model = active_model,
                        iteration = iteration + 1,
                        "provider streaming failed, falling back to non-streaming chat: {stream_err}"
                    );
                    runtime_trace::record_event(
                        "llm_stream_fallback",
                        Some(channel_name),
                        Some(active_provider_name),
                        Some(active_model),
                        Some(&turn_id),
                        Some(false),
                        Some("provider stream failed; fallback to non-streaming chat"),
                        serde_json::json!({
                            "iteration": iteration + 1,
                            "error": scrub_credentials(&stream_err.to_string()),
                        }),
                    );
                    if let Some(ref tx) = on_delta {
                        let _ = tx.send(DraftEvent::Clear).await;
                    }
                    {
                        let chat_future = active_provider.chat(
                            ChatRequest {
                                messages: &prepared_messages.messages,
                                tools: request_tools,
                            },
                            active_model,
                            temperature,
                        );
                        if let Some(token) = cancellation_token.as_ref() {
                            tokio::select! {
                                () = token.cancelled() => Err(ToolLoopCancelled.into()),
                                result = chat_future => result,
                            }
                        } else {
                            chat_future.await
                        }
                    }
                }
            }
        } else {
            // Non-streaming path: wrap with optional per-step timeout from
            // pacing config to catch hung model responses.
            let chat_future = active_provider.chat(
                ChatRequest {
                    messages: &prepared_messages.messages,
                    tools: request_tools,
                },
                active_model,
                temperature,
            );

            match pacing.step_timeout_secs {
                Some(step_secs) if step_secs > 0 => {
                    let step_timeout = Duration::from_secs(step_secs);
                    if let Some(token) = cancellation_token.as_ref() {
                        tokio::select! {
                            () = token.cancelled() => return Err(ToolLoopCancelled.into()),
                            result = tokio::time::timeout(step_timeout, chat_future) => {
                                match result {
                                    Ok(inner) => inner,
                                    Err(_) => anyhow::bail!(
                                        "LLM inference step timed out after {step_secs}s (step_timeout_secs)"
                                    ),
                                }
                            },
                        }
                    } else {
                        match tokio::time::timeout(step_timeout, chat_future).await {
                            Ok(inner) => inner,
                            Err(_) => anyhow::bail!(
                                "LLM inference step timed out after {step_secs}s (step_timeout_secs)"
                            ),
                        }
                    }
                }
                _ => {
                    if let Some(token) = cancellation_token.as_ref() {
                        tokio::select! {
                            () = token.cancelled() => return Err(ToolLoopCancelled.into()),
                            result = chat_future => result,
                        }
                    } else {
                        chat_future.await
                    }
                }
            }
        };

        let (
            response_text,
            parsed_text,
            tool_calls,
            assistant_history_content,
            native_tool_calls,
            _parse_issue_detected,
            response_streamed_live,
        ) = match chat_result {
            Ok(resp) => {
                let (resp_input_tokens, resp_output_tokens) = resp
                    .usage
                    .as_ref()
                    .map(|u| (u.input_tokens, u.output_tokens))
                    .unwrap_or((None, None));

                observer.record_event(&ObserverEvent::LlmResponse {
                    provider: provider_name.to_string(),
                    model: model.to_string(),
                    duration: llm_started_at.elapsed(),
                    success: true,
                    error_message: None,
                    input_tokens: resp_input_tokens,
                    output_tokens: resp_output_tokens,
                });

                // Record cost via task-local tracker (no-op when not scoped)
                let _ = resp
                    .usage
                    .as_ref()
                    .and_then(|usage| record_tool_loop_cost_usage(provider_name, model, usage));

                let response_text = resp.text_or_empty().to_string();
                // First try native structured tool calls (OpenAI-format).
                // Fall back to text-based parsing (XML tags, markdown blocks,
                // GLM format) only if the provider returned no native calls —
                // this ensures we support both native and prompt-guided models.
                let mut calls: Vec<ParsedToolCall> = resp
                    .tool_calls
                    .iter()
                    .map(|call| ParsedToolCall {
                        name: call.name.clone(),
                        arguments: serde_json::from_str::<serde_json::Value>(&call.arguments)
                            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new())),
                        tool_call_id: Some(call.id.clone()),
                    })
                    .collect();
                let mut parsed_text = String::new();

                if calls.is_empty() {
                    let (fallback_text, fallback_calls) = parse_tool_calls(&response_text);
                    if !fallback_text.is_empty() {
                        parsed_text = fallback_text;
                    }
                    calls = fallback_calls;
                }

                let parse_issue = detect_tool_call_parse_issue(&response_text, &calls);
                if let Some(ref issue) = parse_issue {
                    runtime_trace::record_event(
                        "tool_call_parse_issue",
                        Some(channel_name),
                        Some(provider_name),
                        Some(model),
                        Some(&turn_id),
                        Some(false),
                        Some(issue.as_str()),
                        serde_json::json!({
                            "iteration": iteration + 1,
                            "response_excerpt": truncate_with_ellipsis(
                                &scrub_credentials(&response_text),
                                600
                            ),
                        }),
                    );
                }

                runtime_trace::record_event(
                    "llm_response",
                    Some(channel_name),
                    Some(provider_name),
                    Some(model),
                    Some(&turn_id),
                    Some(true),
                    None,
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "duration_ms": llm_started_at.elapsed().as_millis(),
                        "input_tokens": resp_input_tokens,
                        "output_tokens": resp_output_tokens,
                        "raw_response": scrub_credentials(&response_text),
                        "native_tool_calls": resp.tool_calls.len(),
                        "parsed_tool_calls": calls.len(),
                    }),
                );

                // Preserve native tool call IDs in assistant history so role=tool
                // follow-up messages can reference the exact call id.
                let reasoning_content = resp.reasoning_content.clone();
                let assistant_history_content = if resp.tool_calls.is_empty() {
                    if use_native_tools {
                        build_native_assistant_history_from_parsed_calls(
                            &response_text,
                            &calls,
                            reasoning_content.as_deref(),
                        )
                        .unwrap_or_else(|| response_text.clone())
                    } else {
                        response_text.clone()
                    }
                } else {
                    build_native_assistant_history(
                        &response_text,
                        &resp.tool_calls,
                        reasoning_content.as_deref(),
                    )
                };

                let native_calls = resp.tool_calls;
                (
                    response_text,
                    parsed_text,
                    calls,
                    assistant_history_content,
                    native_calls,
                    parse_issue.is_some(),
                    streamed_live_deltas,
                )
            }
            Err(e) => {
                let safe_error = crate::providers::sanitize_api_error(&e.to_string());
                observer.record_event(&ObserverEvent::LlmResponse {
                    provider: provider_name.to_string(),
                    model: model.to_string(),
                    duration: llm_started_at.elapsed(),
                    success: false,
                    error_message: Some(safe_error.clone()),
                    input_tokens: None,
                    output_tokens: None,
                });
                runtime_trace::record_event(
                    "llm_response",
                    Some(channel_name),
                    Some(provider_name),
                    Some(model),
                    Some(&turn_id),
                    Some(false),
                    Some(&safe_error),
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "duration_ms": llm_started_at.elapsed().as_millis(),
                    }),
                );

                // Context overflow recovery: trim history and retry
                if crate::providers::reliable::is_context_window_exceeded(&e) {
                    tracing::warn!(
                        iteration = iteration + 1,
                        "Context window exceeded, attempting in-loop recovery"
                    );

                    // Step 1: fast-trim old tool results (cheap)
                    let chars_saved = fast_trim_tool_results(history, 4);
                    if chars_saved > 0 {
                        tracing::info!(
                            chars_saved,
                            "Context recovery: trimmed old tool results, retrying"
                        );
                        continue;
                    }

                    // Step 2: emergency drop oldest non-system messages
                    let dropped = emergency_history_trim(history, 4);
                    if dropped > 0 {
                        tracing::info!(dropped, "Context recovery: dropped old messages, retrying");
                        continue;
                    }

                    // Nothing left to trim — truly unrecoverable
                    tracing::error!("Context overflow unrecoverable: no trimmable messages");
                }

                return Err(e);
            }
        };

        let display_text = if parsed_text.is_empty() {
            response_text.clone()
        } else {
            parsed_text
        };

        // ── Progress: LLM responded ─────────────────────────────
        if let Some(ref tx) = on_delta {
            let llm_secs = llm_started_at.elapsed().as_secs();
            if !tool_calls.is_empty() {
                let _ = tx
                    .send(DraftEvent::Progress(format!(
                        "\u{1f4ac} Got {} tool call(s) ({llm_secs}s)\n",
                        tool_calls.len()
                    )))
                    .await;
            }
        }

        if tool_calls.is_empty() {
            runtime_trace::record_event(
                "turn_final_response",
                Some(channel_name),
                Some(provider_name),
                Some(model),
                Some(&turn_id),
                Some(true),
                None,
                serde_json::json!({
                    "iteration": iteration + 1,
                    "text": scrub_credentials(&display_text),
                }),
            );
            // No tool calls — this is the final response.
            // If a streaming sender is provided, relay the text in small chunks
            // so the channel can progressively update the draft message.
            if let Some(ref tx) = on_delta {
                let should_emit_post_hoc_chunks =
                    !response_streamed_live || display_text != response_text;
                if !should_emit_post_hoc_chunks {
                    history.push(ChatMessage::assistant(response_text.clone()));
                    return Ok(display_text);
                }
                // Clear accumulated progress lines before streaming the final answer.
                let _ = tx.send(DraftEvent::Clear).await;
                // Split on whitespace boundaries, accumulating chunks of at least
                // STREAM_CHUNK_MIN_CHARS characters for progressive draft updates.
                let mut chunk = String::new();
                for word in display_text.split_inclusive(char::is_whitespace) {
                    if cancellation_token
                        .as_ref()
                        .is_some_and(CancellationToken::is_cancelled)
                    {
                        return Err(ToolLoopCancelled.into());
                    }
                    chunk.push_str(word);
                    if chunk.len() >= STREAM_CHUNK_MIN_CHARS
                        && tx
                            .send(DraftEvent::Content(std::mem::take(&mut chunk)))
                            .await
                            .is_err()
                    {
                        break; // receiver dropped
                    }
                }
                if !chunk.is_empty() {
                    let _ = tx.send(DraftEvent::Content(chunk)).await;
                }
            }
            history.push(ChatMessage::assistant(response_text.clone()));
            return Ok(display_text);
        }

        // Native tool-call providers can return assistant text separately from
        // the structured call payload; relay it to draft-capable channels.
        if !display_text.is_empty() {
            if !native_tool_calls.is_empty() {
                if let Some(ref tx) = on_delta {
                    let mut narration = display_text.clone();
                    if !narration.ends_with('\n') {
                        narration.push('\n');
                    }
                    let _ = tx.send(DraftEvent::Content(narration)).await;
                }
            }
            if !silent {
                print!("{display_text}");
                let _ = std::io::stdout().flush();
            }
        }

        // Execute tool calls and build results. `individual_results` tracks per-call output so
        // native-mode history can emit one role=tool message per tool call with the correct ID.
        //
        // When multiple tool calls are present and interactive CLI approval is not needed, run
        // tool executions concurrently for lower wall-clock latency.
        let mut tool_results = String::new();
        let mut individual_results: Vec<(Option<String>, String)> = Vec::new();
        let mut ordered_results: Vec<Option<(String, Option<String>, ToolExecutionOutcome)>> =
            (0..tool_calls.len()).map(|_| None).collect();
        let allow_parallel_execution = should_execute_tools_in_parallel(&tool_calls, approval);
        let mut executable_indices: Vec<usize> = Vec::new();
        let mut executable_calls: Vec<ParsedToolCall> = Vec::new();

        for (idx, call) in tool_calls.iter().enumerate() {
            // ── Hook: before_tool_call (modifying) ──────────
            let mut tool_name = call.name.clone();
            let mut tool_args = call.arguments.clone();
            if let Some(hooks) = hooks {
                match hooks
                    .run_before_tool_call(tool_name.clone(), tool_args.clone())
                    .await
                {
                    crate::hooks::HookResult::Cancel(reason) => {
                        tracing::info!(tool = %call.name, %reason, "tool call cancelled by hook");
                        let cancelled = format!("Cancelled by hook: {reason}");
                        runtime_trace::record_event(
                            "tool_call_result",
                            Some(channel_name),
                            Some(provider_name),
                            Some(model),
                            Some(&turn_id),
                            Some(false),
                            Some(&cancelled),
                            serde_json::json!({
                                "iteration": iteration + 1,
                                "tool": call.name,
                                "arguments": scrub_credentials(&tool_args.to_string()),
                            }),
                        );
                        if let Some(ref tx) = on_delta {
                            let _ = tx
                                .send(DraftEvent::Progress(format!(
                                    "\u{274c} {}: {}\n",
                                    call.name,
                                    truncate_with_ellipsis(&scrub_credentials(&cancelled), 200)
                                )))
                                .await;
                        }
                        ordered_results[idx] = Some((
                            call.name.clone(),
                            call.tool_call_id.clone(),
                            ToolExecutionOutcome {
                                output: cancelled,
                                success: false,
                                error_reason: Some(scrub_credentials(&reason)),
                                duration: Duration::ZERO,
                            },
                        ));
                        continue;
                    }
                    crate::hooks::HookResult::Continue((name, args)) => {
                        tool_name = name;
                        tool_args = args;
                    }
                }
            }

            maybe_inject_channel_delivery_defaults(
                &tool_name,
                &mut tool_args,
                channel_name,
                channel_reply_target,
            );

            // ── Approval hook ────────────────────────────────
            if let Some(mgr) = approval {
                if mgr.needs_approval(&tool_name) {
                    let request = ApprovalRequest {
                        tool_name: tool_name.clone(),
                        arguments: tool_args.clone(),
                    };

                    // Interactive CLI: prompt the operator.
                    // Non-interactive (channels): auto-deny since no operator
                    // is present to approve.
                    let decision = if mgr.is_non_interactive() {
                        ApprovalResponse::No
                    } else {
                        mgr.prompt_cli(&request)
                    };

                    mgr.record_decision(&tool_name, &tool_args, decision, channel_name);

                    if decision == ApprovalResponse::No {
                        let denied = "Denied by user.".to_string();
                        runtime_trace::record_event(
                            "tool_call_result",
                            Some(channel_name),
                            Some(provider_name),
                            Some(model),
                            Some(&turn_id),
                            Some(false),
                            Some(&denied),
                            serde_json::json!({
                                "iteration": iteration + 1,
                                "tool": tool_name.clone(),
                                "arguments": scrub_credentials(&tool_args.to_string()),
                            }),
                        );
                        if let Some(ref tx) = on_delta {
                            let _ = tx
                                .send(DraftEvent::Progress(format!(
                                    "\u{274c} {}: {}\n",
                                    tool_name, denied
                                )))
                                .await;
                        }
                        ordered_results[idx] = Some((
                            tool_name.clone(),
                            call.tool_call_id.clone(),
                            ToolExecutionOutcome {
                                output: denied.clone(),
                                success: false,
                                error_reason: Some(denied),
                                duration: Duration::ZERO,
                            },
                        ));
                        continue;
                    }
                }
            }

            let signature = {
                let canonical_args = canonicalize_json_for_tool_signature(&tool_args);
                let args_json =
                    serde_json::to_string(&canonical_args).unwrap_or_else(|_| "{}".to_string());
                (tool_name.trim().to_ascii_lowercase(), args_json)
            };
            let dedup_exempt = dedup_exempt_tools.iter().any(|e| e == &tool_name);
            if !dedup_exempt && !seen_tool_signatures.insert(signature) {
                let duplicate = format!(
                    "Skipped duplicate tool call '{tool_name}' with identical arguments in this turn."
                );
                runtime_trace::record_event(
                    "tool_call_result",
                    Some(channel_name),
                    Some(provider_name),
                    Some(model),
                    Some(&turn_id),
                    Some(false),
                    Some(&duplicate),
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "tool": tool_name.clone(),
                        "arguments": scrub_credentials(&tool_args.to_string()),
                        "deduplicated": true,
                    }),
                );
                if let Some(ref tx) = on_delta {
                    let _ = tx
                        .send(DraftEvent::Progress(format!(
                            "\u{274c} {}: {}\n",
                            tool_name, duplicate
                        )))
                        .await;
                }
                ordered_results[idx] = Some((
                    tool_name.clone(),
                    call.tool_call_id.clone(),
                    ToolExecutionOutcome {
                        output: duplicate.clone(),
                        success: false,
                        error_reason: Some(duplicate),
                        duration: Duration::ZERO,
                    },
                ));
                continue;
            }

            runtime_trace::record_event(
                "tool_call_start",
                Some(channel_name),
                Some(provider_name),
                Some(model),
                Some(&turn_id),
                None,
                None,
                serde_json::json!({
                    "iteration": iteration + 1,
                    "tool": tool_name.clone(),
                    "arguments": scrub_credentials(&tool_args.to_string()),
                }),
            );

            // ── Progress: tool start ────────────────────────────
            if let Some(ref tx) = on_delta {
                let hint = {
                    let raw = match tool_name.as_str() {
                        "shell" => tool_args.get("command").and_then(|v| v.as_str()),
                        "file_read" | "file_write" => {
                            tool_args.get("path").and_then(|v| v.as_str())
                        }
                        _ => tool_args
                            .get("action")
                            .and_then(|v| v.as_str())
                            .or_else(|| tool_args.get("query").and_then(|v| v.as_str())),
                    };
                    match raw {
                        Some(s) => truncate_with_ellipsis(s, 60),
                        None => String::new(),
                    }
                };
                let progress = if hint.is_empty() {
                    format!("\u{23f3} {}\n", tool_name)
                } else {
                    format!("\u{23f3} {}: {hint}\n", tool_name)
                };
                tracing::debug!(tool = %tool_name, "Sending progress start to draft");
                let _ = tx.send(DraftEvent::Progress(progress)).await;
            }

            executable_indices.push(idx);
            executable_calls.push(ParsedToolCall {
                name: tool_name,
                arguments: tool_args,
                tool_call_id: call.tool_call_id.clone(),
            });
        }

        let executed_outcomes = if allow_parallel_execution && executable_calls.len() > 1 {
            execute_tools_parallel(
                &executable_calls,
                tools_registry,
                activated_tools,
                observer,
                cancellation_token.as_ref(),
            )
            .await?
        } else {
            execute_tools_sequential(
                &executable_calls,
                tools_registry,
                activated_tools,
                observer,
                cancellation_token.as_ref(),
            )
            .await?
        };

        for ((idx, call), outcome) in executable_indices
            .iter()
            .zip(executable_calls.iter())
            .zip(executed_outcomes.into_iter())
        {
            runtime_trace::record_event(
                "tool_call_result",
                Some(channel_name),
                Some(provider_name),
                Some(model),
                Some(&turn_id),
                Some(outcome.success),
                outcome.error_reason.as_deref(),
                serde_json::json!({
                    "iteration": iteration + 1,
                    "tool": call.name.clone(),
                    "duration_ms": outcome.duration.as_millis(),
                    "output": scrub_credentials(&outcome.output),
                }),
            );

            // ── Hook: after_tool_call (void) ─────────────────
            if let Some(hooks) = hooks {
                let tool_result_obj = crate::tools::ToolResult {
                    success: outcome.success,
                    output: outcome.output.clone(),
                    error: None,
                };
                hooks
                    .fire_after_tool_call(&call.name, &tool_result_obj, outcome.duration)
                    .await;
            }

            // ── Progress: tool completion ───────────────────────
            if let Some(ref tx) = on_delta {
                let secs = outcome.duration.as_secs();
                let progress_msg = if outcome.success {
                    format!("\u{2705} {} ({secs}s)\n", call.name)
                } else if let Some(ref reason) = outcome.error_reason {
                    format!(
                        "\u{274c} {} ({secs}s): {}\n",
                        call.name,
                        truncate_with_ellipsis(reason, 200)
                    )
                } else {
                    format!("\u{274c} {} ({secs}s)\n", call.name)
                };
                tracing::debug!(tool = %call.name, secs, "Sending progress complete to draft");
                let _ = tx.send(DraftEvent::Progress(progress_msg)).await;
            }

            ordered_results[*idx] = Some((call.name.clone(), call.tool_call_id.clone(), outcome));
        }

        // Collect tool results and build per-tool output for loop detection.
        // Only non-ignored tool outputs contribute to the identical-output hash.
        let mut detection_relevant_output = String::new();
        // Use enumerate *before* filter_map so result_index stays aligned with
        // tool_calls even when some ordered_results entries are None.
        for (result_index, (tool_name, tool_call_id, outcome)) in ordered_results
            .into_iter()
            .enumerate()
            .filter_map(|(i, opt)| opt.map(|v| (i, v)))
        {
            if !loop_ignore_tools.contains(tool_name.as_str()) {
                detection_relevant_output.push_str(&outcome.output);

                // Feed the pattern-based loop detector with name + args + result.
                let args = tool_calls
                    .get(result_index)
                    .map(|c| &c.arguments)
                    .unwrap_or(&serde_json::Value::Null);
                let det_result = loop_detector.record(&tool_name, args, &outcome.output);
                match det_result {
                    crate::agent::loop_detector::LoopDetectionResult::Ok => {}
                    crate::agent::loop_detector::LoopDetectionResult::Warning(ref msg) => {
                        tracing::warn!(tool = %tool_name, %msg, "loop detector warning");
                        // Inject a system nudge so the LLM adjusts strategy.
                        history.push(ChatMessage::system(format!("[Loop Detection] {msg}")));
                    }
                    crate::agent::loop_detector::LoopDetectionResult::Block(ref msg) => {
                        tracing::warn!(tool = %tool_name, %msg, "loop detector blocked tool call");
                        // Replace the tool output with the block message.
                        // We still continue the loop so the LLM sees the block feedback.
                        history.push(ChatMessage::system(format!(
                            "[Loop Detection — BLOCKED] {msg}"
                        )));
                    }
                    crate::agent::loop_detector::LoopDetectionResult::Break(msg) => {
                        runtime_trace::record_event(
                            "loop_detector_circuit_breaker",
                            Some(channel_name),
                            Some(provider_name),
                            Some(model),
                            Some(&turn_id),
                            Some(false),
                            Some(&msg),
                            serde_json::json!({
                                "iteration": iteration + 1,
                                "tool": tool_name,
                            }),
                        );
                        anyhow::bail!("Agent loop aborted by loop detector: {msg}");
                    }
                }
            }
            let result_output = truncate_tool_result(&outcome.output, max_tool_result_chars);
            individual_results.push((tool_call_id, result_output.clone()));
            let _ = writeln!(
                tool_results,
                "<tool_result name=\"{}\">\n{}\n</tool_result>",
                tool_name, result_output
            );
        }

        // ── Time-gated loop detection ──────────────────────────
        // When pacing.loop_detection_min_elapsed_secs is set, identical-output
        // loop detection activates after the task has been running that long.
        // This avoids false-positive aborts on long-running browser/research
        // workflows while keeping aggressive protection for quick tasks.
        // When not configured, identical-output detection is disabled (preserving
        // existing behavior where only max_iterations prevents runaway loops).
        let loop_detection_active = match pacing.loop_detection_min_elapsed_secs {
            Some(min_secs) => loop_started_at.elapsed() >= Duration::from_secs(min_secs),
            None => false, // disabled when not configured (backwards compatible)
        };

        if loop_detection_active && !detection_relevant_output.is_empty() {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            detection_relevant_output.hash(&mut hasher);
            let current_hash = hasher.finish();

            if last_tool_output_hash == Some(current_hash) {
                consecutive_identical_outputs += 1;
            } else {
                consecutive_identical_outputs = 0;
                last_tool_output_hash = Some(current_hash);
            }

            // Bail if we see 3+ consecutive identical tool outputs (clear runaway).
            if consecutive_identical_outputs >= 3 {
                runtime_trace::record_event(
                    "tool_loop_identical_output_abort",
                    Some(channel_name),
                    Some(provider_name),
                    Some(model),
                    Some(&turn_id),
                    Some(false),
                    Some("identical tool output detected 3 consecutive times"),
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "consecutive_identical": consecutive_identical_outputs,
                    }),
                );
                anyhow::bail!(
                    "Agent loop aborted: identical tool output detected {} consecutive times",
                    consecutive_identical_outputs
                );
            }
        }

        // Add assistant message with tool calls + tool results to history.
        // Native mode: use JSON-structured messages so convert_messages() can
        // reconstruct proper OpenAI-format tool_calls and tool result messages.
        // Prompt mode: use XML-based text format as before.
        history.push(ChatMessage::assistant(assistant_history_content));
        if native_tool_calls.is_empty() {
            let all_results_have_ids = use_native_tools
                && !individual_results.is_empty()
                && individual_results
                    .iter()
                    .all(|(tool_call_id, _)| tool_call_id.is_some());
            if all_results_have_ids {
                for (tool_call_id, result) in &individual_results {
                    let tool_msg = serde_json::json!({
                        "tool_call_id": tool_call_id,
                        "content": result,
                    });
                    history.push(ChatMessage::tool(tool_msg.to_string()));
                }
            } else {
                history.push(ChatMessage::user(format!("[Tool results]\n{tool_results}")));
            }
        } else {
            for (native_call, (_, result)) in
                native_tool_calls.iter().zip(individual_results.iter())
            {
                let tool_msg = serde_json::json!({
                    "tool_call_id": native_call.id,
                    "content": result,
                });
                history.push(ChatMessage::tool(tool_msg.to_string()));
            }
        }
    }

    runtime_trace::record_event(
        "tool_loop_exhausted",
        Some(channel_name),
        Some(provider_name),
        Some(model),
        Some(&turn_id),
        Some(false),
        Some("agent exceeded maximum tool iterations"),
        serde_json::json!({
            "max_iterations": max_iterations,
        }),
    );

    // Graceful shutdown: ask the LLM for a final summary without tools
    tracing::warn!(
        max_iterations,
        "Max iterations reached, requesting final summary"
    );
    history.push(ChatMessage::user(
        "You have reached the maximum number of tool iterations. \
         Please provide your best answer based on the work completed so far. \
         Summarize what you accomplished and what remains to be done."
            .to_string(),
    ));

    let summary_request = crate::providers::ChatRequest {
        messages: history,
        tools: None, // No tools — force a text response
    };
    match provider.chat(summary_request, model, temperature).await {
        Ok(resp) => {
            let text = resp.text.unwrap_or_default();
            if text.is_empty() {
                anyhow::bail!("Agent exceeded maximum tool iterations ({max_iterations})")
            }
            Ok(text)
        }
        Err(e) => {
            tracing::warn!(error = %e, "Final summary LLM call failed, bailing");
            anyhow::bail!("Agent exceeded maximum tool iterations ({max_iterations})")
        }
    }
}

/// Build the tool instruction block for the system prompt so the LLM knows
/// how to invoke tools.
pub(crate) fn build_tool_instructions(
    tools_registry: &[Box<dyn Tool>],
    tool_descriptions: Option<&ToolDescriptions>,
) -> String {
    let mut instructions = String::new();
    instructions.push_str("\n## Tool Use Protocol\n\n");
    instructions.push_str("To use a tool, wrap a JSON object in <tool_call></tool_call> tags:\n\n");
    instructions.push_str("```\n<tool_call>\n{\"name\": \"tool_name\", \"arguments\": {\"param\": \"value\"}}\n</tool_call>\n```\n\n");
    instructions.push_str(
        "CRITICAL: Output actual <tool_call> tags—never describe steps or give examples.\n\n",
    );
    instructions.push_str("Example: User says \"what's the date?\". You MUST respond with:\n<tool_call>\n{\"name\":\"shell\",\"arguments\":{\"command\":\"date\"}}\n</tool_call>\n\n");
    instructions.push_str("You may use multiple tool calls in a single response. ");
    instructions.push_str("After tool execution, results appear in <tool_result> tags. ");
    instructions
        .push_str("Continue reasoning with the results until you can give a final answer.\n\n");
    instructions.push_str("### Available Tools\n\n");

    for tool in tools_registry {
        let desc = tool_descriptions
            .and_then(|td| td.get(tool.name()))
            .unwrap_or_else(|| tool.description());
        let _ = writeln!(
            instructions,
            "**{}**: {}\nParameters: `{}`\n",
            tool.name(),
            desc,
            tool.parameters_schema()
        );
    }

    instructions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tool_execution::execute_one_tool;
    use crate::observability::NoopObserver;
    use crate::providers::traits::{ProviderCapabilities, StreamChunk, StreamEvent, StreamOptions};
    use crate::providers::{ChatMessage, ChatRequest, ChatResponse, Provider, ToolCall};
    use crate::tools::{Tool, ToolResult};
    use async_trait::async_trait;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[tokio::test]
    async fn execute_one_tool_does_not_panic_on_utf8_boundary() {
        let call_arguments = (0..600)
            .map(|n| serde_json::json!({ "content": format!("{}：tail", "a".repeat(n)) }))
            .find(|args| {
                let raw = args.to_string();
                raw.len() > 300 && !raw.is_char_boundary(300)
            })
            .expect("should produce a sample whose byte index 300 is not a char boundary");

        let observer = NoopObserver;
        let result =
            execute_one_tool("unknown_tool", call_arguments, &[], None, &observer, None).await;
        assert!(result.is_ok(), "execute_one_tool should not panic or error");

        let outcome = result.unwrap();
        assert!(!outcome.success);
        assert!(outcome.output.contains("Unknown tool: unknown_tool"));
    }

    #[tokio::test]
    async fn execute_one_tool_resolves_unique_activated_tool_suffix() {
        let observer = NoopObserver;
        let invocations = Arc::new(AtomicUsize::new(0));
        let activated = Arc::new(std::sync::Mutex::new(crate::tools::ActivatedToolSet::new()));
        let activated_tool: Arc<dyn Tool> = Arc::new(CountingTool::new(
            "docker-mcp__extract_text",
            Arc::clone(&invocations),
        ));
        activated
            .lock()
            .unwrap()
            .activate("docker-mcp__extract_text".into(), activated_tool);

        let outcome = execute_one_tool(
            "extract_text",
            serde_json::json!({ "value": "ok" }),
            &[],
            Some(&activated),
            &observer,
            None,
        )
        .await
        .expect("suffix alias should execute the unique activated tool");

        assert!(outcome.success);
        assert_eq!(outcome.output, "counted:ok");
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
    }

    use crate::providers::router::{Route, RouterProvider};
    use tempfile::TempDir;

    struct NonVisionProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Provider for NonVisionProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok("ok".to_string())
        }
    }

    struct VisionProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Provider for VisionProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tool_calling: false,
                vision: true,
                prompt_caching: false,
            }
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok("ok".to_string())
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let marker_count = crate::multimodal::count_image_markers(request.messages);
            if marker_count == 0 {
                anyhow::bail!("expected image markers in request messages");
            }

            if request.tools.is_some() {
                anyhow::bail!("no tools should be attached for this test");
            }

            Ok(ChatResponse {
                text: Some("vision-ok".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }

    struct ScriptedProvider {
        responses: Arc<Mutex<VecDeque<ChatResponse>>>,
        capabilities: ProviderCapabilities,
    }

    impl ScriptedProvider {
        fn from_text_responses(responses: Vec<&str>) -> Self {
            let scripted = responses
                .into_iter()
                .map(|text| ChatResponse {
                    text: Some(text.to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                })
                .collect();
            Self {
                responses: Arc::new(Mutex::new(scripted)),
                capabilities: ProviderCapabilities::default(),
            }
        }

        fn with_native_tool_support(mut self) -> Self {
            self.capabilities.native_tool_calling = true;
            self
        }
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            self.capabilities.clone()
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            anyhow::bail!("chat_with_system should not be used in scripted provider tests");
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            let mut responses = self
                .responses
                .lock()
                .expect("responses lock should be valid");
            responses
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("scripted provider exhausted responses"))
        }
    }

    struct StreamingScriptedProvider {
        responses: Arc<Mutex<VecDeque<String>>>,
        stream_calls: Arc<AtomicUsize>,
        chat_calls: Arc<AtomicUsize>,
    }

    impl StreamingScriptedProvider {
        fn from_text_responses(responses: Vec<&str>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(
                    responses.into_iter().map(ToString::to_string).collect(),
                )),
                stream_calls: Arc::new(AtomicUsize::new(0)),
                chat_calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait]
    impl Provider for StreamingScriptedProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            anyhow::bail!(
                "chat_with_system should not be used in streaming scripted provider tests"
            );
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            self.chat_calls.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("chat should not be called when streaming succeeds")
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat_with_history(
            &self,
            _messages: &[ChatMessage],
            _model: &str,
            _temperature: f64,
            options: StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            crate::providers::traits::StreamResult<StreamChunk>,
        > {
            self.stream_calls.fetch_add(1, Ordering::SeqCst);
            if !options.enabled {
                return Box::pin(futures_util::stream::empty());
            }

            let response = self
                .responses
                .lock()
                .expect("responses lock should be valid")
                .pop_front()
                .unwrap_or_default();

            Box::pin(futures_util::stream::iter(vec![
                Ok(StreamChunk::delta(response)),
                Ok(StreamChunk::final_chunk()),
            ]))
        }
    }

    enum NativeStreamTurn {
        ToolCall(ToolCall),
        Text(String),
    }

    struct StreamingNativeToolEventProvider {
        turns: Arc<Mutex<VecDeque<NativeStreamTurn>>>,
        stream_calls: Arc<AtomicUsize>,
        stream_tool_requests: Arc<AtomicUsize>,
        chat_calls: Arc<AtomicUsize>,
    }

    impl StreamingNativeToolEventProvider {
        fn with_turns(turns: Vec<NativeStreamTurn>) -> Self {
            Self {
                turns: Arc::new(Mutex::new(turns.into())),
                stream_calls: Arc::new(AtomicUsize::new(0)),
                stream_tool_requests: Arc::new(AtomicUsize::new(0)),
                chat_calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait]
    impl Provider for StreamingNativeToolEventProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tool_calling: true,
                vision: false,
                prompt_caching: false,
            }
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            anyhow::bail!(
                "chat_with_system should not be used in streaming native tool event provider tests"
            );
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            self.chat_calls.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("chat should not be called when native streaming events succeed")
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn supports_streaming_tool_events(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
            options: StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            crate::providers::traits::StreamResult<StreamEvent>,
        > {
            self.stream_calls.fetch_add(1, Ordering::SeqCst);
            if request.tools.is_some_and(|tools| !tools.is_empty()) {
                self.stream_tool_requests.fetch_add(1, Ordering::SeqCst);
            }
            if !options.enabled {
                return Box::pin(futures_util::stream::empty());
            }

            let turn = self
                .turns
                .lock()
                .expect("turns lock should be valid")
                .pop_front()
                .expect("streaming turns should have scripted output");
            match turn {
                NativeStreamTurn::ToolCall(tool_call) => {
                    Box::pin(futures_util::stream::iter(vec![
                        Ok(StreamEvent::ToolCall(tool_call)),
                        Ok(StreamEvent::Final),
                    ]))
                }
                NativeStreamTurn::Text(text) => Box::pin(futures_util::stream::iter(vec![
                    Ok(StreamEvent::TextDelta(StreamChunk::delta(text))),
                    Ok(StreamEvent::Final),
                ])),
            }
        }
    }

    struct RouteAwareStreamingProvider {
        response: String,
        stream_calls: Arc<AtomicUsize>,
        chat_calls: Arc<AtomicUsize>,
        last_model: Arc<Mutex<String>>,
    }

    impl RouteAwareStreamingProvider {
        fn new(response: &str) -> Self {
            Self {
                response: response.to_string(),
                stream_calls: Arc::new(AtomicUsize::new(0)),
                chat_calls: Arc::new(AtomicUsize::new(0)),
                last_model: Arc::new(Mutex::new(String::new())),
            }
        }
    }

    #[async_trait]
    impl Provider for RouteAwareStreamingProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            anyhow::bail!("chat_with_system should not be used in route-aware stream tests");
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            self.chat_calls.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("chat should not be called when routed streaming succeeds")
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat_with_history(
            &self,
            _messages: &[ChatMessage],
            model: &str,
            _temperature: f64,
            options: StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            crate::providers::traits::StreamResult<StreamChunk>,
        > {
            self.stream_calls.fetch_add(1, Ordering::SeqCst);
            *self
                .last_model
                .lock()
                .expect("last_model lock should be valid") = model.to_string();
            if !options.enabled {
                return Box::pin(futures_util::stream::empty());
            }

            Box::pin(futures_util::stream::iter(vec![
                Ok(StreamChunk::delta(self.response.clone())),
                Ok(StreamChunk::final_chunk()),
            ]))
        }
    }

    struct CountingTool {
        name: String,
        invocations: Arc<AtomicUsize>,
    }

    impl CountingTool {
        fn new(name: &str, invocations: Arc<AtomicUsize>) -> Self {
            Self {
                name: name.to_string(),
                invocations,
            }
        }
    }

    #[async_trait]
    impl Tool for CountingTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "Counts executions for loop-stability tests"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string" }
                }
            })
        }

        async fn execute(
            &self,
            args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            let value = args
                .get("value")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            Ok(crate::tools::ToolResult {
                success: true,
                output: format!("counted:{value}"),
                error: None,
            })
        }
    }

    struct RecordingArgsTool {
        name: String,
        recorded_args: Arc<Mutex<Vec<serde_json::Value>>>,
    }

    impl RecordingArgsTool {
        fn new(name: &str, recorded_args: Arc<Mutex<Vec<serde_json::Value>>>) -> Self {
            Self {
                name: name.to_string(),
                recorded_args,
            }
        }
    }

    #[async_trait]
    impl Tool for RecordingArgsTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "Records tool arguments for regression tests"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string" },
                    "schedule": { "type": "object" },
                    "delivery": { "type": "object" }
                }
            })
        }

        async fn execute(
            &self,
            args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            self.recorded_args
                .lock()
                .expect("recorded args lock should be valid")
                .push(args.clone());
            Ok(crate::tools::ToolResult {
                success: true,
                output: args.to_string(),
                error: None,
            })
        }
    }

    struct DelayTool {
        name: String,
        delay_ms: u64,
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    impl DelayTool {
        fn new(
            name: &str,
            delay_ms: u64,
            active: Arc<AtomicUsize>,
            max_active: Arc<AtomicUsize>,
        ) -> Self {
            Self {
                name: name.to_string(),
                delay_ms,
                active,
                max_active,
            }
        }
    }

    #[async_trait]
    impl Tool for DelayTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "Delay tool for testing parallel tool execution"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string" }
                },
                "required": ["value"]
            })
        }

        async fn execute(
            &self,
            args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            let now_active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(now_active, Ordering::SeqCst);

            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;

            self.active.fetch_sub(1, Ordering::SeqCst);

            let value = args
                .get("value")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();

            Ok(crate::tools::ToolResult {
                success: true,
                output: format!("ok:{value}"),
                error: None,
            })
        }
    }

    /// A tool that always returns a failure with a given error reason.
    struct FailingTool {
        tool_name: String,
        error_reason: String,
    }

    impl FailingTool {
        fn new(name: &str, error_reason: &str) -> Self {
            Self {
                tool_name: name.to_string(),
                error_reason: error_reason.to_string(),
            }
        }
    }

    #[async_trait]
    impl Tool for FailingTool {
        fn name(&self) -> &str {
            &self.tool_name
        }

        fn description(&self) -> &str {
            "A tool that always fails for testing failure surfacing"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" }
                }
            })
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            Ok(crate::tools::ToolResult {
                success: false,
                output: String::new(),
                error: Some(self.error_reason.clone()),
            })
        }
    }

    #[tokio::test]
    async fn run_tool_call_loop_returns_structured_error_for_non_vision_provider() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = NonVisionProvider {
            calls: Arc::clone(&calls),
        };

        let mut history = vec![ChatMessage::user(
            "please inspect [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let err = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            None,
            &crate::config::MultimodalConfig::default(),
            3,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &crate::config::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect_err("provider without vision support should fail");

        assert!(err.to_string().contains("provider_capability_error"));
        assert!(err.to_string().contains("capability=vision"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn run_tool_call_loop_rejects_oversized_image_payload() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = VisionProvider {
            calls: Arc::clone(&calls),
        };

        let oversized_payload = STANDARD.encode(vec![0_u8; (1024 * 1024) + 1]);
        let mut history = vec![ChatMessage::user(format!(
            "[IMAGE:data:image/png;base64,{oversized_payload}]"
        ))];

        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;
        let multimodal = crate::config::MultimodalConfig {
            max_images: 4,
            max_image_size_mb: 1,
            allow_remote_fetch: false,
            ..Default::default()
        };

        let err = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            None,
            &multimodal,
            3,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &crate::config::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect_err("oversized payload must fail");

        assert!(
            err.to_string()
                .contains("multimodal image size limit exceeded")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn run_tool_call_loop_accepts_valid_multimodal_request_flow() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = VisionProvider {
            calls: Arc::clone(&calls),
        };

        let mut history = vec![ChatMessage::user(
            "Analyze this [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            None,
            &crate::config::MultimodalConfig::default(),
            3,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &crate::config::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("valid multimodal payload should pass");

        assert_eq!(result, "vision-ok");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// When `vision_provider` is not set and the default provider lacks vision
    /// support, the original `ProviderCapabilityError` should be returned.
    #[tokio::test]
    async fn run_tool_call_loop_no_vision_provider_config_preserves_error() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = NonVisionProvider {
            calls: Arc::clone(&calls),
        };

        let mut history = vec![ChatMessage::user(
            "check [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let err = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            None,
            &crate::config::MultimodalConfig::default(),
            3,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &crate::config::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect_err("should fail without vision_provider config");

        assert!(err.to_string().contains("capability=vision"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// When `vision_provider` is set but the provider factory cannot resolve
    /// the name, a descriptive error should be returned (not the generic
    /// capability error).
    #[tokio::test]
    async fn run_tool_call_loop_vision_provider_creation_failure() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = NonVisionProvider {
            calls: Arc::clone(&calls),
        };

        let mut history = vec![ChatMessage::user(
            "inspect [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let multimodal = crate::config::MultimodalConfig {
            vision_provider: Some("nonexistent-provider-xyz".to_string()),
            vision_model: Some("some-model".to_string()),
            ..Default::default()
        };

        let err = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            None,
            &multimodal,
            3,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &crate::config::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect_err("should fail when vision provider cannot be created");

        assert!(
            err.to_string().contains("failed to create vision provider"),
            "expected creation failure error, got: {}",
            err
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// Messages without image markers should use the default provider even
    /// when `vision_provider` is configured.
    #[tokio::test]
    async fn run_tool_call_loop_no_images_uses_default_provider() {
        let provider = ScriptedProvider::from_text_responses(vec!["hello world"]);

        let mut history = vec![ChatMessage::user("just text, no images".to_string())];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let multimodal = crate::config::MultimodalConfig {
            vision_provider: Some("nonexistent-provider-xyz".to_string()),
            vision_model: Some("some-model".to_string()),
            ..Default::default()
        };

        // Even though vision_provider points to a nonexistent provider, this
        // should succeed because there are no image markers to trigger routing.
        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "scripted",
            "scripted-model",
            0.0,
            true,
            None,
            "cli",
            None,
            &multimodal,
            3,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &crate::config::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("text-only messages should succeed with default provider");

        assert_eq!(result, "hello world");
    }

    /// When `vision_provider` is set but `vision_model` is not, the default
    /// model should be used as fallback for the vision provider.
    #[tokio::test]
    async fn run_tool_call_loop_vision_provider_without_model_falls_back() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = NonVisionProvider {
            calls: Arc::clone(&calls),
        };

        let mut history = vec![ChatMessage::user(
            "look [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        // vision_provider set but vision_model is None — the code should
        // fall back to the default model. Since the provider name is invalid,
        // we just verify the error path references the correct provider.
        let multimodal = crate::config::MultimodalConfig {
            vision_provider: Some("nonexistent-provider-xyz".to_string()),
            vision_model: None,
            ..Default::default()
        };

        let err = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            None,
            &multimodal,
            3,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &crate::config::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect_err("should fail due to nonexistent vision provider");

        // Verify the routing was attempted (not the generic capability error).
        assert!(
            err.to_string().contains("failed to create vision provider"),
            "expected creation failure, got: {}",
            err
        );
    }

    /// Empty `[IMAGE:]` markers (which are preserved as literal text by the
    /// parser) should not trigger vision provider routing.
    #[tokio::test]
    async fn run_tool_call_loop_empty_image_markers_use_default_provider() {
        let provider = ScriptedProvider::from_text_responses(vec!["handled"]);

        let mut history = vec![ChatMessage::user(
            "empty marker [IMAGE:] should be ignored".to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let multimodal = crate::config::MultimodalConfig {
            vision_provider: Some("nonexistent-provider-xyz".to_string()),
            ..Default::default()
        };

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "scripted",
            "scripted-model",
            0.0,
            true,
            None,
            "cli",
            None,
            &multimodal,
            3,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &crate::config::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("empty image markers should not trigger vision routing");

        assert_eq!(result, "handled");
    }

    /// Multiple image markers should still trigger vision routing when
    /// vision_provider is configured.
    #[tokio::test]
    async fn run_tool_call_loop_multiple_images_trigger_vision_routing() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = NonVisionProvider {
            calls: Arc::clone(&calls),
        };

        let mut history = vec![ChatMessage::user(
            "two images [IMAGE:data:image/png;base64,aQ==] and [IMAGE:data:image/png;base64,bQ==]"
                .to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let multimodal = crate::config::MultimodalConfig {
            vision_provider: Some("nonexistent-provider-xyz".to_string()),
            vision_model: Some("llava:7b".to_string()),
            ..Default::default()
        };

        let err = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            None,
            &multimodal,
            3,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &crate::config::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect_err("should attempt vision provider creation for multiple images");

        assert!(
            err.to_string().contains("failed to create vision provider"),
            "expected creation failure for multiple images, got: {}",
            err
        );
    }

    fn should_execute_tools_in_parallel_returns_false_for_single_call() {
        let calls = vec![ParsedToolCall {
            name: "file_read".to_string(),
            arguments: serde_json::json!({"path": "a.txt"}),
            tool_call_id: None,
        }];

        assert!(!should_execute_tools_in_parallel(&calls, None));
    }

    fn should_execute_tools_in_parallel_returns_false_when_approval_is_required() {
        let calls = vec![
            ParsedToolCall {
                name: "shell".to_string(),
                arguments: serde_json::json!({"command": "pwd"}),
                tool_call_id: None,
            },
            ParsedToolCall {
                name: "http_request".to_string(),
                arguments: serde_json::json!({"url": "https://example.com"}),
                tool_call_id: None,
            },
        ];
        let approval_cfg = crate::config::AutonomyConfig::default();
        let approval_mgr = ApprovalManager::from_config(&approval_cfg);

        assert!(!should_execute_tools_in_parallel(
            &calls,
            Some(&approval_mgr)
        ));
    }

    fn should_execute_tools_in_parallel_returns_true_when_cli_has_no_interactive_approvals() {
        let calls = vec![
            ParsedToolCall {
                name: "shell".to_string(),
                arguments: serde_json::json!({"command": "pwd"}),
                tool_call_id: None,
            },
            ParsedToolCall {
                name: "http_request".to_string(),
                arguments: serde_json::json!({"url": "https://example.com"}),
                tool_call_id: None,
            },
        ];
        let approval_cfg = crate::config::AutonomyConfig {
            level: crate::security::AutonomyLevel::Full,
            ..crate::config::AutonomyConfig::default()
        };
        let approval_mgr = ApprovalManager::from_config(&approval_cfg);

        assert!(should_execute_tools_in_parallel(
            &calls,
            Some(&approval_mgr)
        ));
    }

    #[tokio::test]
    async fn run_tool_call_loop_executes_multiple_tools_with_ordered_results() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"delay_a","arguments":{"value":"A"}}
</tool_call>
<tool_call>
{"name":"delay_b","arguments":{"value":"B"}}
</tool_call>"#,
            "done",
        ]);

        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![
            Box::new(DelayTool::new(
                "delay_a",
                200,
                Arc::clone(&active),
                Arc::clone(&max_active),
            )),
            Box::new(DelayTool::new(
                "delay_b",
                200,
                Arc::clone(&active),
                Arc::clone(&max_active),
            )),
        ];

        let approval_cfg = crate::config::AutonomyConfig {
            level: crate::security::AutonomyLevel::Full,
            ..crate::config::AutonomyConfig::default()
        };
        let approval_mgr = ApprovalManager::from_config(&approval_cfg);

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run tool calls"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            Some(&approval_mgr),
            "telegram",
            None,
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &crate::config::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("parallel execution should complete");

        assert_eq!(result, "done");
        assert!(
            max_active.load(Ordering::SeqCst) >= 1,
            "tools should execute successfully"
        );

        let tool_results_message = history
            .iter()
            .find(|msg| msg.role == "user" && msg.content.starts_with("[Tool results]"))
            .expect("tool results message should be present");
        let idx_a = tool_results_message
            .content
            .find("name=\"delay_a\"")
            .expect("delay_a result should be present");
        let idx_b = tool_results_message
            .content
            .find("name=\"delay_b\"")
            .expect("delay_b result should be present");
        assert!(
            idx_a < idx_b,
            "tool results should preserve input order for tool call mapping"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_injects_channel_delivery_defaults_for_cron_add() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"cron_add","arguments":{"job_type":"agent","prompt":"remind me later","schedule":{"kind":"every","every_ms":60000}}}
</tool_call>"#,
            "done",
        ]);

        let recorded_args = Arc::new(Mutex::new(Vec::new()));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(RecordingArgsTool::new(
            "cron_add",
            Arc::clone(&recorded_args),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("schedule a reminder"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "telegram",
            Some("chat-42"),
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &crate::config::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("cron_add delivery defaults should be injected");

        assert_eq!(result, "done");

        let recorded = recorded_args
            .lock()
            .expect("recorded args lock should be valid");
        let delivery = recorded[0]["delivery"].clone();
        assert_eq!(
            delivery,
            serde_json::json!({
                "mode": "announce",
                "channel": "telegram",
                "to": "chat-42",
            })
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_preserves_explicit_cron_delivery_none() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"cron_add","arguments":{"job_type":"agent","prompt":"run silently","schedule":{"kind":"every","every_ms":60000},"delivery":{"mode":"none"}}}
</tool_call>"#,
            "done",
        ]);

        let recorded_args = Arc::new(Mutex::new(Vec::new()));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(RecordingArgsTool::new(
            "cron_add",
            Arc::clone(&recorded_args),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("schedule a quiet cron job"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "telegram",
            Some("chat-42"),
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &crate::config::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("explicit delivery mode should be preserved");

        assert_eq!(result, "done");

        let recorded = recorded_args
            .lock()
            .expect("recorded args lock should be valid");
        assert_eq!(recorded[0]["delivery"], serde_json::json!({"mode": "none"}));
    }

    #[tokio::test]
    async fn run_tool_call_loop_deduplicates_repeated_tool_calls() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"count_tool","arguments":{"value":"A"}}
</tool_call>
<tool_call>
{"name":"count_tool","arguments":{"value":"A"}}
</tool_call>"#,
            "done",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run tool calls"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            None,
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &crate::config::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("loop should finish after deduplicating repeated calls");

        assert_eq!(result, "done");
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "duplicate tool call with same args should not execute twice"
        );

        let tool_results = history
            .iter()
            .find(|msg| msg.role == "user" && msg.content.starts_with("[Tool results]"))
            .expect("prompt-mode tool result payload should be present");
        assert!(tool_results.content.contains("counted:A"));
        assert!(tool_results.content.contains("Skipped duplicate tool call"));
    }

    #[tokio::test]
    async fn run_tool_call_loop_allows_low_risk_shell_in_non_interactive_mode() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"shell","arguments":{"command":"echo hello"}}
</tool_call>"#,
            "done",
        ]);

        let tmp = TempDir::new().expect("temp dir");
        let security = Arc::new(crate::security::SecurityPolicy {
            autonomy: crate::security::AutonomyLevel::Supervised,
            workspace_dir: tmp.path().to_path_buf(),
            ..crate::security::SecurityPolicy::default()
        });
        let runtime: Arc<dyn crate::runtime::RuntimeAdapter> =
            Arc::new(crate::runtime::NativeRuntime::new());
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(
            crate::tools::shell::ShellTool::new(security, runtime),
        )];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run shell"),
        ];
        let observer = NoopObserver;
        let approval_mgr =
            ApprovalManager::for_non_interactive(&crate::config::AutonomyConfig::default());

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            Some(&approval_mgr),
            "telegram",
            None,
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &crate::config::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("non-interactive shell should succeed for low-risk command");

        assert_eq!(result, "done");

        let tool_results = history
            .iter()
            .find(|msg| msg.role == "user" && msg.content.starts_with("[Tool results]"))
            .expect("tool results message should be present");
        assert!(tool_results.content.contains("hello"));
        assert!(!tool_results.content.contains("Denied by user."));
    }

    #[tokio::test]
    async fn run_tool_call_loop_dedup_exempt_allows_repeated_calls() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"count_tool","arguments":{"value":"A"}}
</tool_call>
<tool_call>
{"name":"count_tool","arguments":{"value":"A"}}
</tool_call>"#,
            "done",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run tool calls"),
        ];
        let observer = NoopObserver;
        let exempt = vec!["count_tool".to_string()];

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            None,
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
            &exempt,
            None,
            None,
            &crate::config::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("loop should finish with exempt tool executing twice");

        assert_eq!(result, "done");
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            2,
            "exempt tool should execute both duplicate calls"
        );

        let tool_results = history
            .iter()
            .find(|msg| msg.role == "user" && msg.content.starts_with("[Tool results]"))
            .expect("prompt-mode tool result payload should be present");
        assert!(
            !tool_results.content.contains("Skipped duplicate tool call"),
            "exempt tool calls should not be suppressed"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_dedup_exempt_only_affects_listed_tools() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"count_tool","arguments":{"value":"A"}}
</tool_call>
<tool_call>
{"name":"count_tool","arguments":{"value":"A"}}
</tool_call>
<tool_call>
{"name":"other_tool","arguments":{"value":"B"}}
</tool_call>
<tool_call>
{"name":"other_tool","arguments":{"value":"B"}}
</tool_call>"#,
            "done",
        ]);

        let count_invocations = Arc::new(AtomicUsize::new(0));
        let other_invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![
            Box::new(CountingTool::new(
                "count_tool",
                Arc::clone(&count_invocations),
            )),
            Box::new(CountingTool::new(
                "other_tool",
                Arc::clone(&other_invocations),
            )),
        ];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run tool calls"),
        ];
        let observer = NoopObserver;
        let exempt = vec!["count_tool".to_string()];

        let _result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            None,
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
            &exempt,
            None,
            None,
            &crate::config::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("loop should complete");

        assert_eq!(
            count_invocations.load(Ordering::SeqCst),
            2,
            "exempt tool should execute both calls"
        );
        assert_eq!(
            other_invocations.load(Ordering::SeqCst),
            1,
            "non-exempt tool should still be deduped"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_native_mode_preserves_fallback_tool_call_ids() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"{"content":"Need to call tool","tool_calls":[{"id":"call_abc","name":"count_tool","arguments":"{\"value\":\"X\"}"}]}"#,
            "done",
        ])
        .with_native_tool_support();

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run tool calls"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            None,
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            &crate::config::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("native fallback id flow should complete");

        assert_eq!(result, "done");
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
        assert!(
            history.iter().any(|msg| {
                msg.role == "tool" && msg.content.contains("\"tool_call_id\":\"call_abc\"")
            }),
            "tool result should preserve parsed fallback tool_call_id in native mode"
        );
        assert!(
            history
                .iter()
                .all(|msg| !(msg.role == "user" && msg.content.starts_with("[Tool results]"))),
            "native mode should use role=tool history instead of prompt fallback wrapper"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_relays_native_tool_call_text_via_on_delta() {
        let provider = ScriptedProvider {
            responses: Arc::new(Mutex::new(VecDeque::from(vec![
                ChatResponse {
                    text: Some("Task started. Waiting 30 seconds before checking status.".into()),
                    tool_calls: vec![ToolCall {
                        id: "call_wait".into(),
                        name: "count_tool".into(),
                        arguments: r#"{"value":"A"}"#.into(),
                    }],
                    usage: None,
                    reasoning_content: None,
                },
                ChatResponse {
                    text: Some("Final answer".into()),
                    tool_calls: Vec::new(),
                    usage: None,
                    reasoning_content: None,
                },
            ]))),
            capabilities: ProviderCapabilities {
                native_tool_calling: true,
                ..ProviderCapabilities::default()
            },
        };

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run tool calls"),
        ];
        let observer = NoopObserver;
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "telegram",
            None,
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            Some(tx),
            None,
            &[],
            &[],
            None,
            None,
            &crate::config::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("native tool-call text should be relayed through on_delta");

        let mut deltas: Vec<DraftEvent> = Vec::new();
        while let Some(delta) = rx.recv().await {
            deltas.push(delta);
        }

        let explanation_idx = deltas
            .iter()
            .position(|delta| matches!(delta, DraftEvent::Content(t) if t == "Task started. Waiting 30 seconds before checking status.\n"))
            .expect("native assistant text should be relayed to on_delta");
        let clear_idx = deltas
            .iter()
            .position(|delta| matches!(delta, DraftEvent::Clear))
            .expect("final answer streaming should clear prior draft state");

        assert!(
            deltas
                .iter()
                .any(|delta| matches!(delta, DraftEvent::Progress(t) if t.starts_with("\u{1f4ac} Got 1 tool call(s)"))),
            "tool-call progress line should still be relayed"
        );
        assert!(
            explanation_idx < clear_idx,
            "native assistant text should arrive before final-answer draft clearing"
        );
        assert_eq!(result, "Final answer");
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn run_tool_call_loop_consumes_provider_stream_for_final_response() {
        let provider =
            StreamingScriptedProvider::from_text_responses(vec!["streamed final answer"]);
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("say hi"),
        ];
        let observer = NoopObserver;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(32);

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "telegram",
            None,
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            Some(tx),
            None,
            &[],
            &[],
            None,
            None,
            &crate::config::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("streaming provider should complete");

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            match delta {
                DraftEvent::Clear => {
                    visible_deltas.clear();
                }
                DraftEvent::Progress(_) => {}
                DraftEvent::Content(text) => {
                    visible_deltas.push_str(&text);
                }
            }
        }

        assert_eq!(result, "streamed final answer");
        assert_eq!(
            visible_deltas, "streamed final answer",
            "draft should receive upstream deltas once without post-hoc duplication"
        );
        assert_eq!(provider.stream_calls.load(Ordering::SeqCst), 1);
        assert_eq!(provider.chat_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn run_tool_call_loop_streaming_path_preserves_tool_loop_semantics() {
        let provider = StreamingScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"count_tool","arguments":{"value":"A"}}
</tool_call>"#,
            "done",
        ]);
        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run tool calls"),
        ];
        let observer = NoopObserver;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(64);

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "telegram",
            None,
            &crate::config::MultimodalConfig::default(),
            5,
            None,
            Some(tx),
            None,
            &[],
            &[],
            None,
            None,
            &crate::config::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("streaming tool loop should execute tool and finish");

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            match delta {
                DraftEvent::Clear => {
                    visible_deltas.clear();
                }
                DraftEvent::Progress(_) => {}
                DraftEvent::Content(text) => {
                    visible_deltas.push_str(&text);
                }
            }
        }

        assert_eq!(result, "done");
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
        assert_eq!(provider.stream_calls.load(Ordering::SeqCst), 2);
        assert_eq!(provider.chat_calls.load(Ordering::SeqCst), 0);
        assert_eq!(visible_deltas, "done");
        assert!(
            !visible_deltas.contains("<tool_call"),
            "draft text should not leak streamed tool payload markers"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_streams_native_tool_events_without_chat_fallback() {
        let provider = StreamingNativeToolEventProvider::with_turns(vec![
            NativeStreamTurn::ToolCall(ToolCall {
                id: "call_native_1".to_string(),
                name: "count_tool".to_string(),
                arguments: r#"{"value":"A"}"#.to_string(),
            }),
            NativeStreamTurn::Text("done".to_string()),
        ]);
        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run native tools"),
        ];
        let observer = NoopObserver;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(64);

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "telegram",
            None,
            &crate::config::MultimodalConfig::default(),
            5,
            None,
            Some(tx),
            None,
            &[],
            &[],
            None,
            None,
            &crate::config::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("native streaming events should preserve tool loop semantics");

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            match delta {
                DraftEvent::Clear => {
                    visible_deltas.clear();
                }
                DraftEvent::Progress(_) => {}
                DraftEvent::Content(text) => {
                    visible_deltas.push_str(&text);
                }
            }
        }

        assert_eq!(result, "done");
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
        assert_eq!(provider.stream_calls.load(Ordering::SeqCst), 2);
        assert_eq!(provider.stream_tool_requests.load(Ordering::SeqCst), 2);
        assert_eq!(provider.chat_calls.load(Ordering::SeqCst), 0);
        assert_eq!(visible_deltas, "done");
    }

    #[tokio::test]
    async fn run_tool_call_loop_routed_streaming_uses_live_provider_deltas_once() {
        let default_provider = RouteAwareStreamingProvider::new("default answer");
        let default_stream_calls = Arc::clone(&default_provider.stream_calls);
        let default_chat_calls = Arc::clone(&default_provider.chat_calls);

        let routed_provider = RouteAwareStreamingProvider::new("routed streamed answer");
        let routed_stream_calls = Arc::clone(&routed_provider.stream_calls);
        let routed_chat_calls = Arc::clone(&routed_provider.chat_calls);
        let routed_last_model = Arc::clone(&routed_provider.last_model);

        let router = RouterProvider::new(
            vec![
                ("default".to_string(), Box::new(default_provider)),
                ("fast".to_string(), Box::new(routed_provider)),
            ],
            vec![(
                "fast".to_string(),
                Route {
                    provider_name: "fast".to_string(),
                    model: "routed-model".to_string(),
                },
            )],
            "default-model".to_string(),
        );

        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("say hi"),
        ];
        let observer = NoopObserver;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DraftEvent>(32);

        let result = run_tool_call_loop(
            &router,
            &mut history,
            &tools_registry,
            &observer,
            "router",
            "hint:fast",
            0.0,
            true,
            None,
            "telegram",
            None,
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            Some(tx),
            None,
            &[],
            &[],
            None,
            None,
            &crate::config::PacingConfig::default(),
            0,
            0,
            None,
        )
        .await
        .expect("routed streaming provider should complete");

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            match delta {
                DraftEvent::Clear => {
                    visible_deltas.clear();
                }
                DraftEvent::Progress(_) => {}
                DraftEvent::Content(text) => {
                    visible_deltas.push_str(&text);
                }
            }
        }

        assert_eq!(result, "routed streamed answer");
        assert_eq!(
            visible_deltas, "routed streamed answer",
            "routed draft should receive upstream deltas once without post-hoc duplication"
        );
        assert_eq!(default_stream_calls.load(Ordering::SeqCst), 0);
        assert_eq!(routed_stream_calls.load(Ordering::SeqCst), 1);
        assert_eq!(default_chat_calls.load(Ordering::SeqCst), 0);
        assert_eq!(routed_chat_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            routed_last_model
                .lock()
                .expect("routed_last_model lock should be valid")
                .as_str(),
            "routed-model"
        );
    }

    fn agent_turn_executes_activated_tool_from_wrapper() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime should initialize");

        runtime.block_on(async {
            let provider = ScriptedProvider::from_text_responses(vec![
                r#"<tool_call>
{"name":"pixel__get_api_health","arguments":{"value":"ok"}}
</tool_call>"#,
                "done",
            ]);

            let invocations = Arc::new(AtomicUsize::new(0));
            let activated = Arc::new(std::sync::Mutex::new(crate::tools::ActivatedToolSet::new()));
            let activated_tool: Arc<dyn Tool> = Arc::new(CountingTool::new(
                "pixel__get_api_health",
                Arc::clone(&invocations),
            ));
            activated
                .lock()
                .unwrap()
                .activate("pixel__get_api_health".into(), activated_tool);

            let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
            let mut history = vec![
                ChatMessage::system("test-system"),
                ChatMessage::user("use the activated MCP tool"),
            ];
            let observer = NoopObserver;

            let result = agent_turn(
                &provider,
                &mut history,
                &tools_registry,
                &observer,
                "mock-provider",
                "mock-model",
                0.0,
                true,
                "daemon",
                None,
                &crate::config::MultimodalConfig::default(),
                4,
                None,
                &[],
                &[],
                Some(&activated),
                None,
            )
            .await
            .expect("wrapper path should execute activated tools");

            assert_eq!(result, "done");
            assert_eq!(invocations.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn resolve_display_text_hides_raw_payload_for_tool_only_turns() {
        let display = resolve_display_text(
            "<tool_call>{\"name\":\"memory_store\"}</tool_call>",
            "",
            true,
            false,
        );
        assert!(display.is_empty());
    }

    #[test]
    fn resolve_display_text_keeps_plain_text_for_tool_turns() {
        let display = resolve_display_text(
            "<tool_call>{\"name\":\"shell\"}</tool_call>",
            "Let me check that.",
            true,
            false,
        );
        assert_eq!(display, "Let me check that.");
    }

    #[test]
    fn resolve_display_text_uses_response_text_for_native_tool_turns() {
        let display = resolve_display_text("Task started.", "", true, true);
        assert_eq!(display, "Task started.");
    }

    #[test]
    fn resolve_display_text_uses_response_text_for_final_turns() {
        let display = resolve_display_text("Final answer", "", false, false);
        assert_eq!(display, "Final answer");
    }
}
