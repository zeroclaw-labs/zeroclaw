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
    let text = if policy.reasoning {
        strip_think_tags(text)
    } else {
        text.to_owned()
    };
    strip_trailing_terminal_markers(&text)
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
    use super::*;

    #[test]
    fn extraction_precedes_empty_classification_and_respects_disabled_policy() {
        let mut response = ChatResponse {
            text: Some("<think>internal</think><eom>".into()),
            tool_calls: Vec::new(),
            usage: None,
            reasoning_content: None,
        };
        normalize_response(&mut response, TextExtraction::default());
        assert!(!response.is_semantically_empty_terminal());
        normalize_response(
            &mut response,
            TextExtraction {
                reasoning: true,
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
                    reasoning: true,
                    tool_calls: true,
                },
            ),
            2
        );
        assert_eq!(
            count_text_tool_calls(
                text,
                TextExtraction {
                    reasoning: true,
                    tool_calls: false,
                },
            ),
            0
        );
    }
}
