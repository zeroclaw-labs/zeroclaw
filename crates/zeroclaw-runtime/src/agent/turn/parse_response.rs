//! Interpretation of a successful provider chat response: observer/cost
//! recording, native and text-fallback tool-call parsing, parse-issue
//! detection, and assistant-history construction.

use super::context::TurnCtx;
use super::protocol_detect::{
    detect_internal_protocol_without_tools, detect_tool_call_parse_issue_for_known_tools,
};
use super::redact::scrub_credentials;
use super::tool_specs::IterationToolSpecs;
use crate::agent::cost::record_tool_loop_cost_usage;
use crate::observability::ObserverEvent;
use std::time::Instant;
use zeroclaw_api::agent::TurnEvent;
use zeroclaw_providers::{ChatResponse, ToolCall};
use zeroclaw_tool_call_parser::{
    ParsedToolCall, build_native_assistant_history_from_parsed_calls,
    looks_like_tool_protocol_example, parse_tool_calls, strip_think_tags,
};

/// Build assistant history entry in JSON format for native tool-call APIs.
/// `convert_messages` in the OpenRouter model_provider parses this JSON to reconstruct
/// the proper `NativeMessage` with structured `tool_calls`.
pub(crate) fn build_native_assistant_history(
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

pub(crate) fn resolve_display_text(
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

/// Narration to relay after the live stream, given what was already forwarded.
/// Returns the suffix of `display_text` past `streamed_visible_text` when the
/// latter is a genuine prefix. On any divergence the whole `display_text` is
/// relayed: duplicate output is recoverable noise, a dropped tail is permanent
/// loss, so the total function never truncates.
pub(crate) fn unforwarded_narration<'a>(
    display_text: &'a str,
    streamed_visible_text: &str,
) -> &'a str {
    display_text
        .strip_prefix(streamed_visible_text)
        .unwrap_or(display_text)
}

/// The interpreted Ok-arm of one provider call.
pub(crate) struct InterpretedResponse {
    pub(crate) response_text: String,
    pub(crate) parsed_text: String,
    pub(crate) tool_calls: Vec<ParsedToolCall>,
    pub(crate) assistant_history_content: String,
    pub(crate) native_tool_calls: Vec<ToolCall>,
    pub(crate) parse_issue_detected: bool,
}

/// Interpret a successful chat response. Takes the response by value and
/// holds no borrows of `ctx` past the call (RUN_SHEET `turn.parse_response`).
pub(crate) async fn interpret_chat_response(
    ctx: &TurnCtx<'_>,
    resp: ChatResponse,
    specs: &IterationToolSpecs,
    streamed_protocol_suppressed: bool,
    llm_started_at: Instant,
    iteration: usize,
    detect_protocol_without_tools: bool,
) -> InterpretedResponse {
    let (resp_input_tokens, resp_output_tokens) = resp
        .usage
        .as_ref()
        .map(|u| (u.input_tokens, u.output_tokens))
        .unwrap_or((None, None));

    ctx.observer.record_event(&ObserverEvent::LlmResponse {
        model_provider: ctx.provider_name.to_string(),
        model: ctx.model.to_string(),
        duration: llm_started_at.elapsed(),
        success: true,
        error_message: None,
        input_tokens: resp_input_tokens,
        output_tokens: resp_output_tokens,
        channel: None,
        agent_alias: None,
        turn_id: None,
    });

    // Per-LLM-call usage event, right after the observer success event
    // (upstream E2 parity, agent.rs Usage emission).
    if let Some(tx) = ctx.event_tx
        && let Some(ref usage) = resp.usage
    {
        let _ = tx
            .send(TurnEvent::Usage {
                input_tokens: usage.input_tokens,
                cached_input_tokens: usage.cached_input_tokens,
                output_tokens: usage.output_tokens,
                cost_usd: None,
            })
            .await;
    }

    // Record cost via task-local tracker (no-op when not scoped)
    let _ = resp
        .usage
        .as_ref()
        .and_then(|usage| record_tool_loop_cost_usage(ctx.provider_name, ctx.model, usage));

    let response_text = strip_think_tags(resp.text_or_empty());
    // First try native structured tool calls (OpenAI-format).
    // Fall back to text-based parsing (XML tags, markdown blocks,
    // GLM format) only if the model_provider returned no native calls —
    // this ensures we support both native and prompt-guided models.
    let mut calls: Vec<ParsedToolCall> = if specs.tool_specs.is_empty() {
        Vec::new()
    } else {
        resp.tool_calls
            .iter()
            .map(|call| ParsedToolCall {
                name: call.name.clone(),
                arguments: serde_json::from_str::<serde_json::Value>(&call.arguments)
                    .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new())),
                tool_call_id: Some(call.id.clone()),
            })
            .collect()
    };
    let mut parsed_text = String::new();

    if calls.is_empty()
        && !specs.tool_specs.is_empty()
        && !ctx.strict_tool_parsing
        && !looks_like_tool_protocol_example(&response_text)
    {
        let (fallback_text, fallback_calls) = parse_tool_calls(&response_text);
        let filtered_calls: Vec<ParsedToolCall> = fallback_calls
            .into_iter()
            .filter(|call| {
                specs
                    .known_tool_names
                    .contains(&call.name.to_ascii_lowercase())
            })
            .collect();
        if !fallback_text.is_empty() && !filtered_calls.is_empty() {
            parsed_text = fallback_text;
        }
        calls = filtered_calls;
    }

    let parse_issue = if ctx.strict_tool_parsing {
        None
    } else if specs.tool_specs.is_empty() {
        // Knob-gated (embedders return model text verbatim); a live stream
        // suppression already altered the visible text, so it is always
        // reported regardless of the knob.
        detect_protocol_without_tools
            .then(|| detect_internal_protocol_without_tools(&response_text))
            .flatten()
            .or_else(|| {
                streamed_protocol_suppressed.then(|| {
                    "streaming text guard suppressed an internal tool protocol envelope".to_string()
                })
            })
    } else {
        detect_tool_call_parse_issue_for_known_tools(
            &response_text,
            &calls,
            &specs.known_tool_names,
        )
        .or_else(|| {
            streamed_protocol_suppressed.then(|| {
                "streaming text guard suppressed an internal tool protocol envelope".to_string()
            })
        })
    };
    if let Some(ref issue) = parse_issue {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({
                    "model": ctx.model,
                    "iteration": iteration + 1,
                    "issue": issue.as_str(),
                    "response": scrub_credentials(&response_text),
                    "trace_id": ctx.turn_id,
                })),
            "tool_call_parse_issue"
        );
    }

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Receive)
            .with_outcome(::zeroclaw_log::EventOutcome::Success)
            .with_duration(u64::try_from(llm_started_at.elapsed().as_millis()).unwrap_or(u64::MAX))
            .with_attrs(::serde_json::json!({
                "model": ctx.model,
                "iteration": iteration + 1,
                "input_tokens": resp_input_tokens,
                "output_tokens": resp_output_tokens,
                "raw_response": scrub_credentials(&response_text),
                "native_tool_calls": resp.tool_calls.len(),
                "parsed_tool_calls": calls.len(),
                "trace_id": ctx.turn_id,
            })),
        "llm_response"
    );

    // Preserve native tool call IDs in assistant history so role=tool
    // follow-up messages can reference the exact call id.
    let reasoning_content = resp.reasoning_content.clone();
    let assistant_history_content = if resp.tool_calls.is_empty() {
        if specs.use_native_tools {
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
    InterpretedResponse {
        response_text,
        parsed_text,
        tool_calls: calls,
        assistant_history_content,
        native_tool_calls: native_calls,
        parse_issue_detected: parse_issue.is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::unforwarded_narration;

    #[test]
    fn returns_suffix_when_streamed_text_is_a_prefix() {
        assert_eq!(
            unforwarded_narration("About to check the count.", "About to "),
            "check the count."
        );
    }

    #[test]
    fn returns_empty_when_everything_was_streamed() {
        assert_eq!(
            unforwarded_narration("fully streamed", "fully streamed"),
            ""
        );
    }

    #[test]
    fn returns_whole_text_when_nothing_was_streamed() {
        assert_eq!(
            unforwarded_narration("never streamed", ""),
            "never streamed"
        );
    }

    #[test]
    fn relays_whole_text_on_prefix_divergence_rather_than_truncating() {
        assert_eq!(
            unforwarded_narration("final visible text", "diverged live text"),
            "final visible text"
        );
    }
}
