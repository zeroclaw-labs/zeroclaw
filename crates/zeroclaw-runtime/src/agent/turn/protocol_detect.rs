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
        .find(|&len| pattern.is_char_boundary(len) && text.ends_with(&pattern[..len]))
        .unwrap_or(0)
}

fn ends_with_partial_marker(text: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    let mut best: Option<usize> = None;
    for marker in ["<|dsml|", "<|tool_call|", "<｜dsml｜", "<｜tool_call｜"] {
        let matched = longest_suffix_matching_prefix(&lower, marker);
        if matched >= 2 {
            best = Some(best.map_or(matched, |current| current.max(matched)));
        }
    }
    best.map(|matched| text.len() - matched)
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
        "<|dsml",
        "<|tool_call",
        "<｜dsml",
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
        "<|dsml",
        "<|tool_call",
        "<｜dsml",
        "```tool",
        "```invoke",
        "```json",
    ] {
        if let Some(idx) = lower.rfind(pattern) {
            earliest = Some(earliest.map_or(idx, |current| current.min(idx)));
        }
    }

    if let Some(partial_start) = ends_with_partial_marker(text) {
        earliest = Some(earliest.map_or(partial_start, |current| current.min(partial_start)));
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
        || lower.starts_with("<|dsml")
        || lower.starts_with("<|tool_call")
        || lower.starts_with("<｜dsml")
        || lower.starts_with("```tool")
        || lower.starts_with("```invoke")
        || lower.starts_with("```json")
}

pub(crate) fn starts_suspicious_tag_or_fence_prefix(text: &str) -> bool {
    let lower = text.trim_start().to_ascii_lowercase();
    lower.starts_with("<tool")
        || lower.starts_with("<invoke")
        || lower.starts_with("<function")
        || lower.starts_with("<|dsml")
        || lower.starts_with("<|tool_call")
        || lower.starts_with("<｜dsml")
        || lower.starts_with("```tool")
        || lower.starts_with("```invoke")
        || lower.starts_with("```json")
        || lower.starts_with("[tool_call]")
}

pub(crate) fn contains_close_tag_marker(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "</tool_call",
        "</toolcall",
        "</tool-call",
        "</invoke",
        "</function",
        "</|dsml|",
        "</|tool_call|",
        "</｜dsml｜",
        "</｜dsml",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

/// Lowercase close-marker prefixes for tool-protocol envelopes. Shared by the
/// envelope-boundary and continuation-fragment helpers below.
const SUPPRESSED_CLOSE_MARKERS: [&str; 11] = [
    "</|dsml|>",
    "</｜dsml｜",
    "</|tool_call|>",
    "</tool_call>",
    "</tool_calls>",
    "</toolcall>",
    "</tool-call>",
    "</invoke>",
    "</function",
    "</minimax:tool_call>",
    "</minimax:toolcall>",
];

fn is_plain_tag_name(seg: &str) -> bool {
    !seg.is_empty()
        && seg
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | ':'))
}

fn first_json_value_end(input: &str) -> Option<usize> {
    for (byte_idx, ch) in input.char_indices() {
        if ch != '{' && ch != '[' {
            continue;
        }
        let slice = &input[byte_idx..];
        let mut stream = serde_json::Deserializer::from_str(slice).into_iter::<serde_json::Value>();
        if let Some(Ok(_)) = stream.next() {
            let consumed = stream.byte_offset();
            if consumed > 0 {
                return Some(byte_idx + consumed);
            }
        }
    }
    None
}

pub(crate) fn protocol_envelope_end(text: &str, start: usize) -> Option<usize> {
    let rest = text.get(start..)?;
    let lower = rest.to_ascii_lowercase();

    if let Some(body) = rest.strip_prefix("```") {
        if let Some(close) = body.find("```") {
            return Some(start + 3 + close + 3);
        }
        return None;
    }

    let mut best: Option<usize> = None;
    for marker in SUPPRESSED_CLOSE_MARKERS {
        let Some(rel) = lower.rfind(marker) else {
            continue;
        };
        let tail = &rest[rel + marker.len()..];
        let end = match tail.find('>') {
            Some(idx)
                if tail[..idx].trim_start().is_empty()
                    || is_plain_tag_name(tail[..idx].trim_start()) =>
            {
                rel + marker.len() + idx + 1
            }
            _ => rel + marker.len(),
        };
        best = Some(best.map_or(end, |current: usize| current.max(end)));
    }
    if let Some(end) = best {
        return Some(start + end);
    }

    if rest.starts_with('{') || rest.starts_with('[') {
        return first_json_value_end(rest).map(|end| start + end);
    }
    for open in ["<|dsml|>", "<｜dsml｜", "<|tool_call|>"] {
        if !lower.starts_with(open) {
            continue;
        }
        let mut body = &rest[open.len()..];
        let mut body_offset = open.len();
        if let Some(idx) = body.find('>')
            && is_plain_tag_name(body[..idx].trim_start())
        {
            body = &body[idx + 1..];
            body_offset += idx + 1;
        }
        if let Some(end) = first_json_value_end(body) {
            return Some(start + body_offset + end);
        }
        break;
    }

    None
}

pub(crate) fn suppressed_continuation_trailing(chunk: &str) -> Option<&str> {
    let lead = chunk.len() - chunk.trim_start().len();
    let body = &chunk[lead..];
    let lower = body.to_ascii_lowercase();

    let mut first: Option<usize> = None;
    for marker in SUPPRESSED_CLOSE_MARKERS {
        if let Some(rel) = lower.find(marker)
            && first.is_none_or(|current| rel < current)
        {
            first = Some(rel);
        }
    }
    let Some(first_idx) = first else {
        return close_run_trailing(chunk, lead, body);
    };
    if !body[..first_idx].trim().is_empty() {
        return None;
    }

    let mut best: Option<usize> = None;
    for marker in SUPPRESSED_CLOSE_MARKERS {
        let Some(rel) = lower.rfind(marker) else {
            continue;
        };
        let tail = &body[rel + marker.len()..];
        let end = match tail.find('>') {
            Some(idx)
                if tail[..idx].trim_start().is_empty()
                    || is_plain_tag_name(tail[..idx].trim_start()) =>
            {
                rel + marker.len() + idx + 1
            }
            _ => rel + marker.len(),
        };
        best = Some(best.map_or(end, |current: usize| current.max(end)));
    }
    if let Some(end) = best {
        return trailing_after_fragment(chunk, lead + end);
    }

    close_run_trailing(chunk, lead, body)
}

fn close_run_trailing<'a>(chunk: &'a str, lead: usize, body: &str) -> Option<&'a str> {
    let mut end = lead;
    for ch in body.chars() {
        if matches!(ch, '}' | ']' | '`') {
            end += ch.len_utf8();
        } else {
            break;
        }
    }
    if end > lead {
        return trailing_after_fragment(chunk, end);
    }
    None
}

fn trailing_after_fragment(chunk: &str, end: usize) -> Option<&str> {
    let trailing = chunk[end..].trim_start();
    if trailing.is_empty() {
        return None;
    }
    let lower = trailing.to_ascii_lowercase();
    if starts_suspicious_protocol_prefix(trailing) || lower.starts_with("</") {
        return None;
    }
    Some(trailing)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_suffix_matching_prefix_is_character_boundary_safe() {
        // The fullwidth DSML token is multi-byte UTF-8; slicing must not panic.
        assert_eq!(longest_suffix_matching_prefix("ab｜DS", "｜DSML｜"), 5);
        assert_eq!(longest_suffix_matching_prefix("ab", "｜DSML｜"), 0);
        assert_eq!(longest_suffix_matching_prefix("", "｜DSML｜"), 0);
        assert_eq!(longest_suffix_matching_prefix("hello", "hello"), 0);
        // The text ends in a full `｜` char, matching the 3-byte pattern prefix.
        assert_eq!(longest_suffix_matching_prefix("ab｜", "｜DSML｜"), 3);
    }

    #[test]
    fn detects_fullwidth_dsml_candidates() {
        let embedded = r#"Here it is: <｜DSML｜tool_calls>"#;
        assert_eq!(
            find_embedded_protocol_candidate_start(embedded),
            Some(embedded.find("<｜DSML｜tool_calls>").unwrap())
        );
        assert!(starts_suspicious_protocol_prefix(
            "<｜dsml｜invoke name=\"shell\">"
        ));
        assert!(starts_suspicious_tag_or_fence_prefix(
            "<｜dsml｜tool_calls>"
        ));
        assert!(find_incomplete_protocol_candidate_start("x<｜dsml").is_some());
    }

    #[test]
    fn detects_marker_split_across_chunk_boundary() {
        assert!(find_incomplete_protocol_candidate_start("I will <|").is_some());
        assert!(find_incomplete_protocol_candidate_start("I will <|D").is_some());
        assert!(find_incomplete_protocol_candidate_start("I will <|DSM").is_some());
        assert!(find_incomplete_protocol_candidate_start("I will <｜").is_some());
        assert!(find_incomplete_protocol_candidate_start("I will <｜D").is_some());
        assert!(find_incomplete_protocol_candidate_start("I will <｜DSM").is_some());
        assert!(find_incomplete_protocol_candidate_start("x<|tool_").is_some());
        assert!(find_incomplete_protocol_candidate_start("x<｜tool_").is_some());
        assert!(
            find_incomplete_protocol_candidate_start("just text <").is_none(),
            "a bare trailing '<' is not a protocol candidate"
        );
        assert!(
            find_incomplete_protocol_candidate_start("wrote 5 < 10").is_none(),
            "'<' inside plain text is not a protocol candidate"
        );
    }
}
