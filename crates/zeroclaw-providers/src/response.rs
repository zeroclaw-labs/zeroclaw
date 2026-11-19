//! Format interpretation shared by reliable providers and injected consumers.
//! Tool authorization, schema validation and execution remain downstream.

pub use zeroclaw_tool_call_parser::strip_think_tags;

use zeroclaw_api::{TextExtraction, model_provider::ChatResponse};
use zeroclaw_tool_call_parser::{
    ParsedToolCall, looks_like_tool_protocol_example, parse_tool_calls_from_text,
    strip_trailing_terminal_markers,
};

/// Normalize wire text before semantic-empty classification. The admitted
/// policy is provided by the caller, never retained in a provider snapshot.
pub fn normalize_response(response: &mut ChatResponse, policy: TextExtraction) {
    if let Some(text) = response.text.as_mut() {
        *text = normalize_text(text, policy);
    }
}

pub fn normalize_text(text: &str, policy: TextExtraction) -> String {
    // Omitted policy preserves the historical extraction behavior. Only an
    // explicit `Some(false)` opts out, so terminal classification remains
    // compatible with the pre-registry providers.
    let text = if policy.reasoning != Some(false) {
        strip_think_tags(text)
    } else {
        text.to_owned()
    };
    strip_trailing_terminal_markers(&text)
}

/// Structured output owns its string contents, including literal protocol
/// tags. Preserve a complete JSON value before interpreting an outer reasoning
/// envelope. Schema validation remains the consumer's responsibility.
pub fn normalize_json_text(text: &str, policy: TextExtraction) -> String {
    let mut candidate = text.to_owned();
    loop {
        // Terminal markers are outside the structured value. Remove them
        // before checking JSON so a marker cannot send us through the generic
        // text path, where reasoning tags inside JSON strings would be
        // interpreted as outer provider metadata.
        let normalized = strip_trailing_terminal_markers(candidate.trim());
        let trimmed = normalized.trim();
        let json = trimmed
            .strip_prefix("```json")
            .or_else(|| trimmed.strip_prefix("```"))
            .and_then(|body| body.strip_suffix("```"))
            .unwrap_or(trimmed);
        if serde_json::from_str::<serde_json::Value>(json).is_ok() {
            return trimmed.to_owned();
        }
        if policy.reasoning != Some(false)
            && let Some(reasoning) = trimmed.strip_prefix("<think>")
            && let Some((_, rest)) = reasoning.split_once("</think>")
        {
            candidate = rest.to_owned();
            continue;
        }
        return normalize_text(text, policy);
    }
}

/// Count text-encoded tool calls after applying the same response-boundary
/// normalization used by provider adapters. Runtime may use the count for
/// turn policy decisions, but it does not need to know the wire parser or
/// terminal-marker rules.
pub fn count_text_tool_calls(text: &str, policy: TextExtraction) -> usize {
    if !policy.tool_calls {
        return 0;
    }
    let text = normalize_text(text, policy);
    if looks_like_tool_protocol_example(&text) {
        return 0;
    }
    parse_tool_calls_from_text(&text).1.len()
}

pub struct InterpretedText {
    pub text: String,
    pub parsed_text: String,
    pub calls: Vec<ParsedToolCall>,
}

/// Interpret native calls first, and text envelopes only when explicitly
/// enabled. Unknown tool names are left to the runtime protocol diagnostics;
/// this helper never authorizes or executes a call.
pub fn interpret_response(
    response: &ChatResponse,
    policy: TextExtraction,
    known_tool_names: &std::collections::HashSet<String>,
) -> InterpretedText {
    let text = normalize_text(response.text_or_empty(), policy);
    let mut calls = if known_tool_names.is_empty() {
        Vec::new()
    } else {
        response
            .tool_calls
            .iter()
            .map(|call| ParsedToolCall {
                name: call.name.clone(),
                arguments: serde_json::from_str(&call.arguments)
                    .unwrap_or_else(|_| serde_json::json!({})),
                tool_call_id: Some(call.id.clone()),
            })
            .collect()
    };
    let mut parsed_text = String::new();
    if calls.is_empty()
        && !known_tool_names.is_empty()
        && policy.tool_calls
        && !looks_like_tool_protocol_example(&text)
    {
        let (candidate_text, candidate_calls) = parse_tool_calls_from_text(&text);
        calls = candidate_calls
            .into_iter()
            .filter(|call| known_tool_names.contains(&call.name.to_ascii_lowercase()))
            .collect();
        if !calls.is_empty() {
            parsed_text = candidate_text;
        }
    }
    InterpretedText {
        text,
        parsed_text,
        calls,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn json_contents_are_preserved_before_reasoning_extraction() {
        let policy = zeroclaw_api::TextExtraction {
            reasoning: Some(true),
            tool_calls: false,
        };
        for text in [
            r#"{"text":"<think>keep me</think>"}"#,
            r#"{"text":"<think>keep me"}"#,
            "```json\n{\"text\":\"<think>keep me</think>\"}\n```",
        ] {
            assert_eq!(super::normalize_json_text(text, policy), text);
        }
        assert_eq!(
            super::normalize_json_text(r#"{"text":"<think>keep me</think>"}<eom>"#, policy),
            r#"{"text":"<think>keep me</think>"}"#
        );
        assert_eq!(
            super::normalize_json_text("<think>reasoning</think>{\"answer\":true}", policy),
            r#"{"answer":true}"#
        );
        assert_eq!(
            super::normalize_json_text(
                r#"<think>reasoning</think>{"text":"<think>keep</think>"}"#,
                policy,
            ),
            r#"{"text":"<think>keep</think>"}"#
        );
        assert_eq!(
            super::normalize_json_text(
                "<think>reasoning</think>{\"answer\":true}",
                zeroclaw_api::TextExtraction {
                    reasoning: Some(false),
                    tool_calls: false
                },
            ),
            "<think>reasoning</think>{\"answer\":true}"
        );
    }
    use super::*;

    #[test]
    fn extraction_precedes_empty_classification_and_respects_disabled_policy() {
        let mut response = ChatResponse {
            text: Some("<think>internal</think><eom>".into()),
            tool_calls: Vec::new(),
            usage: None,
            reasoning_content: None,
        };
        let original = response.clone();
        normalize_response(&mut response, TextExtraction::default());
        assert!(response.is_semantically_empty_terminal());
        let mut disabled = original;
        normalize_response(
            &mut disabled,
            TextExtraction {
                reasoning: Some(false),
                tool_calls: false,
            },
        );
        assert!(!disabled.is_semantically_empty_terminal());
        assert_eq!(disabled.text.as_deref(), Some("<think>internal</think>"));
        normalize_response(
            &mut response,
            TextExtraction {
                reasoning: Some(true),
                tool_calls: false,
            },
        );
        assert!(response.is_semantically_empty_terminal());
    }

    #[test]
    fn text_tool_call_count_owns_wire_parser_policy() {
        let text = r#"<think>internal</think><tool_call>{"name":"one","arguments":{}}</tool_call><tool_call>{"name":"two","arguments":{}}</tool_call>"#;
        assert_eq!(
            count_text_tool_calls(
                text,
                TextExtraction {
                    reasoning: Some(true),
                    tool_calls: true,
                },
            ),
            2
        );
        assert_eq!(
            count_text_tool_calls(
                text,
                TextExtraction {
                    reasoning: Some(true),
                    tool_calls: false,
                },
            ),
            0
        );
    }
}
