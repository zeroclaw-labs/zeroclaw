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
use crate::agent::loop_::capture_llm_messages;
use crate::observability::ObserverEvent;
use std::time::Instant;
use zeroclaw_api::agent::TurnEvent;
use zeroclaw_providers::{ChatMessage, ChatResponse, ToolCall};
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
            let mut call = serde_json::Map::from_iter([
                ("id".to_string(), serde_json::json!(tc.id)),
                ("name".to_string(), serde_json::json!(tc.name)),
                ("arguments".to_string(), serde_json::json!(tc.arguments)),
            ]);
            if let Some(extra_content) = &tc.extra_content {
                call.insert("extra_content".to_string(), extra_content.clone());
            }
            serde_json::Value::Object(call)
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

pub(crate) fn unforwarded_narration<'a>(
    display_text: &'a str,
    streamed_visible_text: &str,
) -> &'a str {
    if let Some(rest) = display_text.strip_prefix(streamed_visible_text) {
        return rest;
    }
    let stream_lead_normalized = streamed_visible_text.trim_start();
    if !stream_lead_normalized.is_empty()
        && let Some(rest) = display_text.strip_prefix(stream_lead_normalized)
    {
        return rest;
    }
    if streamed_visible_text.trim() == display_text {
        return "";
    }
    display_text
}

/// The interpreted Ok-arm of one provider call.
pub(crate) struct InterpretedResponse {
    pub(crate) response_text: String,
    pub(crate) parsed_text: String,
    pub(crate) tool_calls: Vec<ParsedToolCall>,
    pub(crate) assistant_history_content: String,
    pub(crate) native_tool_calls: Vec<ToolCall>,
    pub(crate) parse_issue_detected: bool,
    pub(crate) input_tokens: Option<u64>,
}

/// Interpret a successful chat response. Takes the response by value and
/// holds no borrows of `ctx` past the call (RUN_SHEET `turn.parse_response`).
pub(crate) async fn interpret_chat_response(
    ctx: &TurnCtx<'_>,
    resp: ChatResponse,
    history: &[ChatMessage],
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
        channel: Some(ctx.channel_name.to_string()),
        agent_alias: ctx.agent_alias.map(|s| s.to_string()),
        parent_agent_alias: ctx.parent_agent_alias.map(|s| s.to_string()),
        turn_id: Some(ctx.turn_id.to_string()),
        // Credential-scrubbed prompt/completion content for OTel GenAI export;
        // `None` unless the `observability-otel` feature is active.
        messages: capture_llm_messages(history, Some(resp.text_or_empty()), &resp.tool_calls),
    });

    let call_cost_usd = resp
        .usage
        .as_ref()
        .and_then(|usage| record_tool_loop_cost_usage(ctx.provider_name, ctx.model, usage))
        .map(|(_total_tokens, cost_usd)| cost_usd);

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
                cost_usd: call_cost_usd,
            })
            .await;
    }

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
                .with_category(::zeroclaw_log::EventCategory::Tool)
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
            .with_category(::zeroclaw_log::EventCategory::Provider)
            .with_outcome(::zeroclaw_log::EventOutcome::Success)
            .with_duration(u64::try_from(llm_started_at.elapsed().as_millis()).unwrap_or(u64::MAX))
            .with_attrs(::serde_json::json!({
                "model": ctx.model,
                "iteration": iteration + 1,
                "input_tokens": resp_input_tokens,
                "output_tokens": resp_output_tokens,
                "cost_usd": call_cost_usd,
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
        input_tokens: resp_input_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::{build_native_assistant_history, unforwarded_narration};
    use zeroclaw_providers::ToolCall;

    #[test]
    fn native_assistant_history_preserves_tool_call_extra_content() {
        let history = build_native_assistant_history(
            "",
            &[ToolCall {
                id: "call_1".to_string(),
                name: "search".to_string(),
                arguments: "{}".to_string(),
                extra_content: Some(serde_json::json!({
                    "google": {
                        "thought_signature": "sig_1"
                    }
                })),
            }],
            None,
        );

        let value = serde_json::from_str::<serde_json::Value>(&history).unwrap();
        assert_eq!(
            value["tool_calls"][0]["extra_content"],
            serde_json::json!({"google": {"thought_signature": "sig_1"}})
        );
    }

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

    #[test]
    fn returns_empty_when_streamed_text_has_trailing_whitespace() {
        assert_eq!(
            unforwarded_narration("Checking the data.", "Checking the data.\n\n"),
            ""
        );
    }

    #[test]
    fn returns_empty_when_streamed_text_has_leading_whitespace() {
        assert_eq!(
            unforwarded_narration("Checking the data.", "\n\nChecking the data."),
            ""
        );
    }

    #[test]
    fn returns_empty_when_streamed_text_has_whitespace_on_both_ends() {
        assert_eq!(
            unforwarded_narration("Checking the data.", "\n\nChecking the data.\n"),
            ""
        );
    }

    #[test]
    fn returns_suffix_past_the_whitespace_trimmed_streamed_prefix() {
        assert_eq!(
            unforwarded_narration("About to check.", "\nAbout to"),
            " check."
        );
    }

    #[test]
    fn preserves_trailing_prefix_space_when_only_the_leading_edge_diverges() {
        assert_eq!(
            unforwarded_narration("About to check.", "\nAbout to "),
            "check."
        );
    }
}

#[cfg(test)]
mod cost_usd_regression_tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;
    use zeroclaw_providers::traits::TokenUsage;

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn cost_usd_flows_to_usage_event_and_llm_response() {
        // Exact-f64 inputs.
        let input_tokens: u64 = 2_000_000;
        let output_tokens: u64 = 1_000_000;
        let input_rate: f64 = 1.5;
        let output_rate: f64 = 3.0;
        // cached_input_tokens = 0, so the full input is billed at input_rate.
        let expected: f64 = (input_tokens as f64) * input_rate / 1_000_000.0
            + (output_tokens as f64) * output_rate / 1_000_000.0;

        let provider = "testprov";
        let model = "testmodel";

        // Cost scope: real CostTracker + a pricing map keyed by provider, with
        // per-1M-token input/output/cached_input rates for `model`.
        let tmpdir = tempfile::TempDir::new().unwrap();
        let tracker = Arc::new(
            crate::cost::CostTracker::new(
                zeroclaw_config::schema::CostConfig::default(),
                tmpdir.path(),
            )
            .unwrap(),
        );
        let pricing: HashMap<String, f64> = HashMap::from([
            (format!("{model}.input"), input_rate),
            (format!("{model}.output"), output_rate),
            (format!("{model}.cached_input"), 0.0_f64),
        ]);
        let cost_ctx = crate::agent::cost::ToolLoopCostTrackingContext::new(
            Arc::clone(&tracker),
            Arc::new(HashMap::from([(provider.to_string(), pricing)])),
        );

        // TurnCtx with event_tx wired; everything else empty/None.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TurnEvent>(4);
        let pacing = zeroclaw_config::schema::PacingConfig::default();
        let dedup_exempt_tools: Vec<String> = Vec::new();
        let ctx = TurnCtx {
            parent_agent_alias: None,
            observer: &crate::observability::NoopObserver,
            provider_name: provider,
            model,
            temperature: None,
            approval: None,
            channel_name: "",
            channel_reply_target: None,
            cancellation_token: None,
            on_delta: None,
            event_tx: Some(&tx),
            hooks: None,
            dedup_exempt_tools: &dedup_exempt_tools,
            pacing: &pacing,
            strict_tool_parsing: false,
            channel: None,
            agent_alias: None,
            turn_id: "turn-cost-regression",
        };

        let specs = IterationToolSpecs {
            tool_specs: vec![],
            known_tool_names: HashSet::new(),
            use_native_tools: false,
        };

        let resp = ChatResponse {
            text: Some("hello".to_string()),
            tool_calls: vec![],
            usage: Some(TokenUsage {
                input_tokens: Some(input_tokens),
                output_tokens: Some(output_tokens),
                cached_input_tokens: Some(0),
            }),
            reasoning_content: None,
        };

        // Capture the `llm_response` broadcast record. Hold the public
        // writer + hook locks so a parallel `clear_broadcast_hook` (or a
        // peer crate's writer test) doesn't drop this test's event.
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut log_rx = zeroclaw_log::subscribe_or_install();
        while log_rx.try_recv().is_ok() {}

        // Run interpret_chat_response inside the cost scope so
        // record_tool_loop_cost_usage sees the pricing map.
        let now = std::time::Instant::now();
        crate::agent::cost::TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(Some(cost_ctx), async {
                interpret_chat_response(&ctx, resp, &[], &specs, false, now, 0, false).await;
            })
            .await;

        // (a) The Usage event must carry the cost.
        let event = rx
            .try_recv()
            .expect("interpret_chat_response should emit a TurnEvent::Usage");
        match event {
            TurnEvent::Usage { cost_usd, .. } => {
                let c = cost_usd.expect("Usage event must carry cost_usd, got None");
                assert!(
                    (c - expected).abs() < 1e-9,
                    "Usage event cost_usd {c} != expected {expected}"
                );
            }
            other => panic!("expected TurnEvent::Usage, got {other:?}"),
        }

        // (b) The llm_response log record must carry the cost.
        let mut found_cost: Option<f64> = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while found_cost.is_none() && std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let step = remaining.min(std::time::Duration::from_millis(50));
            match tokio::time::timeout(step, log_rx.recv()).await {
                Ok(Ok(value)) => {
                    if value
                        .get("message")
                        .and_then(|v| v.as_str())
                        .map(|s| s == "llm_response")
                        .unwrap_or(false)
                        && value.get("trace_id").and_then(|v| v.as_str())
                            == Some("turn-cost-regression")
                    {
                        found_cost = Some(
                            value
                                .get("attributes")
                                .and_then(|a| a.get("cost_usd"))
                                .and_then(serde_json::Value::as_f64)
                                .expect("llm_response record must carry attributes.cost_usd"),
                        );
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                Err(_elapsed) => {}
            }
        }
        let logged = found_cost.expect("did not observe an llm_response log record with cost_usd");
        assert!(
            (logged - expected).abs() < 1e-9,
            "llm_response cost_usd {logged} != expected {expected}"
        );

        zeroclaw_log::clear_broadcast_hook();
    }
}
