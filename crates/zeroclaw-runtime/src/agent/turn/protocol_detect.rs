//! Heuristics for detecting tool-protocol fragments in streamed model text.

use std::collections::HashSet;
use zeroclaw_tool_call_parser::{
    ParsedToolCall, ToolProtocolEnvelopeKind, classify_tool_protocol_envelope,
    contains_tool_protocol_tag_call, looks_like_malformed_tool_protocol_envelope,
    looks_like_malformed_tool_protocol_envelope_for_known_tools, looks_like_tool_protocol_envelope,
    looks_like_tool_protocol_example, tool_protocol_envelope_mentions_known_tool,
};

pub(crate) fn longest_suffix_matching_prefix(text: &str, pattern: &str) -> usize {
    (1..pattern.len())
        .rev()
        .find(|&len| text.ends_with(&pattern[..len]))
        .unwrap_or(0)
}

pub(crate) fn find_embedded_protocol_candidate_start(text: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    let mut earliest: Option<usize> = None;

    for pattern in [
        "<tool_call",
        "<toolcall",
        "<tool-call",
        "<invoke",
        "<function",
        "```tool",
        "```invoke",
        "```json",
    ] {
        if let Some(idx) = lower.find(pattern) {
            earliest = Some(earliest.map_or(idx, |current| current.min(idx)));
        }
    }

    for key in ["\"tool_calls\"", "\"toolcalls\"", "\"function_call\""] {
        if let Some(key_idx) = lower.find(key)
            && let Some(json_start) = text[..key_idx].rfind(['{', '['])
        {
            earliest = Some(earliest.map_or(json_start, |current| current.min(json_start)));
        }
    }

    earliest
}

pub(crate) fn find_incomplete_protocol_candidate_start(text: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    let mut earliest: Option<usize> = None;

    for pattern in [
        "<tool",
        "<invoke",
        "<function",
        "```tool",
        "```invoke",
        "```json",
    ] {
        if let Some(idx) = lower.rfind(pattern) {
            earliest = Some(earliest.map_or(idx, |current| current.min(idx)));
        }
    }

    for delimiter in ['{', '['] {
        if let Some(idx) = text.rfind(delimiter) {
            let tail = &lower[idx..];
            if tail.contains("\"tool")
                || tail.contains("\"function")
                || tail.contains("\"call")
                || tail.len() <= 16
            {
                earliest = Some(earliest.map_or(idx, |current| current.min(idx)));
            }
        }
    }

    earliest
}

pub(crate) fn starts_suspicious_protocol_prefix(text: &str) -> bool {
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with('{')
        || lower.starts_with('[')
        || lower.starts_with("<tool")
        || lower.starts_with("<invoke")
        || lower.starts_with("<function")
        || lower.starts_with("```tool")
        || lower.starts_with("```invoke")
        || lower.starts_with("```json")
}

pub(crate) fn starts_suspicious_tag_or_fence_prefix(text: &str) -> bool {
    let lower = text.trim_start().to_ascii_lowercase();
    lower.starts_with("<tool")
        || lower.starts_with("<invoke")
        || lower.starts_with("<function")
        || lower.starts_with("```tool")
        || lower.starts_with("```invoke")
        || lower.starts_with("```json")
        || lower.starts_with("[tool_call]")
}

pub(crate) fn complete_non_protocol_json(text: &str, known_tool_names: &HashSet<String>) -> bool {
    let trimmed = text.trim();
    (trimmed.starts_with('{') || trimmed.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
        && (!looks_like_tool_protocol_envelope(trimmed)
            || !tool_protocol_envelope_mentions_known_tool(trimmed, known_tool_names))
}

pub(crate) fn complete_json_fence_protocol_state(
    text: &str,
    known_tool_names: &HashSet<String>,
) -> Option<bool> {
    let trimmed = text.trim();
    let body = json_fence_body(trimmed)?;
    Some(
        looks_like_tool_protocol_envelope(body)
            && tool_protocol_envelope_mentions_known_tool(body, known_tool_names),
    )
}

pub(crate) fn detect_internal_protocol_without_tools(response: &str) -> Option<String> {
    let trimmed = response.trim();
    if trimmed.is_empty() {
        return None;
    }
    if looks_like_tool_protocol_example(trimmed) {
        return None;
    }

    (looks_like_malformed_tool_protocol_envelope(trimmed)
        || contains_tool_protocol_tag_call(trimmed)
        || classify_tool_protocol_envelope(trimmed)
            .is_some_and(|kind| matches!(kind, ToolProtocolEnvelopeKind::TaggedToolCall))
        || (classify_tool_protocol_envelope(trimmed).is_none()
            && looks_like_tool_protocol_envelope(trimmed)))
    .then(|| {
        "response resembled an internal tool protocol envelope but no tools were enabled".into()
    })
}

pub(crate) fn detect_tool_call_parse_issue_for_known_tools(
    response: &str,
    parsed_calls: &[ParsedToolCall],
    known_tool_names: &HashSet<String>,
) -> Option<String> {
    if !parsed_calls.is_empty() {
        return None;
    }

    let trimmed = response.trim();
    if trimmed.is_empty() || looks_like_tool_protocol_example(trimmed) {
        return None;
    }

    let message = "response resembled an internal tool protocol envelope but no valid tool call could be parsed";

    if looks_like_malformed_tool_protocol_envelope_for_known_tools(trimmed, known_tool_names)
        || contains_tool_protocol_tag_call(trimmed)
    {
        return Some(message.into());
    }

    if let Some(kind) = classify_tool_protocol_envelope(trimmed) {
        return (matches!(
            kind,
            ToolProtocolEnvelopeKind::TaggedToolCall | ToolProtocolEnvelopeKind::ToolResult
        ) || tool_protocol_envelope_mentions_known_tool(trimmed, known_tool_names))
        .then(|| message.into());
    }

    looks_like_tool_protocol_envelope(trimmed).then(|| message.into())
}

pub(crate) fn json_fence_body(trimmed: &str) -> Option<&str> {
    let rest = trimmed.strip_prefix("```")?;
    let first_newline = rest.find('\n')?;
    let language = rest[..first_newline].trim().trim_end_matches('\r');
    if !language.eq_ignore_ascii_case("json") {
        return None;
    }

    let body_with_close = &rest[first_newline + 1..];
    let close_start = body_with_close.rfind("```")?;
    if !body_with_close[close_start + 3..].trim().is_empty() {
        return None;
    }
    Some(body_with_close[..close_start].trim())
}
