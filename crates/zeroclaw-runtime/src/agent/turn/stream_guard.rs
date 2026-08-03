//! Streaming-text guards: protocol-fragment buffering and `<think>` tag stripping.

use super::protocol_detect::{
    complete_json_fence_protocol_state, complete_non_protocol_json,
    find_embedded_protocol_candidate_start, find_incomplete_protocol_candidate_start,
    longest_suffix_matching_prefix, starts_suspicious_protocol_prefix,
    starts_suspicious_tag_or_fence_prefix,
};
use std::collections::HashSet;
use zeroclaw_tool_call_parser::{
    ToolProtocolEnvelopeKind, classify_tool_protocol_envelope, contains_tool_protocol_tag_call,
    looks_like_malformed_tool_protocol_envelope_for_known_tools, looks_like_tool_protocol_envelope,
    looks_like_tool_protocol_example, tool_protocol_envelope_mentions_known_tool,
};

#[derive(Debug, Default)]
pub(crate) struct StreamTextGuard {
    // Suspicious leading chunks can split `"toolcalls"` / `<tool_call>` across
    // deltas. Buffer just that prefix until it is clearly protocol or normal JSON.
    pending: String,
    pending_candidate_start: Option<usize>,
    known_tool_names: HashSet<String>,
    has_active_tools: bool,
    pub(crate) suppress_forwarding: bool,
    pub(crate) suppressed_protocol: bool,
}

impl StreamTextGuard {
    pub(crate) fn new(available_tools: Option<&[crate::tools::ToolSpec]>) -> Self {
        let available_tools = available_tools.unwrap_or(&[]);
        let known_tool_names = available_tools
            .iter()
            .map(|tool| tool.name.to_ascii_lowercase())
            .collect();
        Self {
            known_tool_names,
            has_active_tools: !available_tools.is_empty(),
            ..Self::default()
        }
    }

    pub(crate) fn push(&mut self, chunk: &str) -> Option<String> {
        if self.suppress_forwarding || chunk.is_empty() {
            return None;
        }

        if self.pending.is_empty() && !starts_suspicious_protocol_prefix(chunk) {
            if let Some(start) = find_embedded_protocol_candidate_start(chunk) {
                self.pending_candidate_start = Some(start);
                self.pending.push_str(&chunk[start..]);
                return if self.should_suppress_protocol_candidate(&self.pending) {
                    self.suppress_protocol();
                    None
                } else {
                    self.pending.insert_str(0, &chunk[..start]);
                    self.evaluate_pending(false)
                };
            }
            if let Some(start) = find_incomplete_protocol_candidate_start(chunk) {
                self.pending_candidate_start = Some(start);
                self.pending.push_str(chunk);
                return None;
            }
            return Some(chunk.to_string());
        }

        self.pending.push_str(chunk);
        self.evaluate_pending(false)
    }

    pub(crate) fn finish(&mut self) -> Option<String> {
        if self.suppress_forwarding || self.pending.is_empty() {
            return None;
        }
        if let Some(release) = self.evaluate_pending(true) {
            return Some(release);
        }
        if self.suppressed_protocol || self.pending.is_empty() {
            return None;
        }
        if looks_like_malformed_tool_protocol_envelope_for_known_tools(
            &self.pending,
            &self.known_tool_names,
        ) {
            self.suppress_protocol();
            return None;
        }
        Some(std::mem::take(&mut self.pending))
    }

    fn evaluate_pending(&mut self, finalizing: bool) -> Option<String> {
        let candidate = self
            .pending_candidate_start
            .and_then(|start| self.pending.get(start..))
            .unwrap_or(&self.pending);

        if !finalizing && starts_suspicious_tag_or_fence_prefix(candidate) {
            return None;
        }

        if self.should_suppress_protocol_candidate(candidate) {
            self.suppress_protocol();
            return None;
        }

        if let Some(is_protocol) =
            complete_json_fence_protocol_state(candidate, &self.known_tool_names)
        {
            if is_protocol && self.has_active_tools {
                self.suppress_protocol();
                return None;
            }
            self.pending_candidate_start = None;
            return Some(std::mem::take(&mut self.pending));
        }

        if complete_non_protocol_json(candidate, &self.known_tool_names) {
            self.pending_candidate_start = None;
            return Some(std::mem::take(&mut self.pending));
        }

        None
    }

    fn suppress_protocol(&mut self) {
        self.pending.clear();
        self.pending_candidate_start = None;
        self.suppress_forwarding = true;
        self.suppressed_protocol = true;
    }

    fn looks_like_active_tool_json(&self, text: &str) -> bool {
        if self.known_tool_names.is_empty() {
            return false;
        }

        let Ok(value) = serde_json::from_str::<serde_json::Value>(text.trim()) else {
            return false;
        };

        match value {
            serde_json::Value::Array(items) => {
                !items.is_empty() && items.iter().all(|item| self.is_known_tool_payload(item))
            }
            serde_json::Value::Object(_) => self.is_known_tool_payload(&value),
            _ => false,
        }
    }

    fn is_known_tool_payload(&self, value: &serde_json::Value) -> bool {
        let Some(object) = value.as_object() else {
            return false;
        };

        let (name, has_args) =
            if let Some(function) = object.get("function").and_then(|value| value.as_object()) {
                (
                    function
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .or_else(|| object.get("name").and_then(serde_json::Value::as_str)),
                    function.contains_key("arguments")
                        || function.contains_key("parameters")
                        || object.contains_key("arguments")
                        || object.contains_key("parameters"),
                )
            } else {
                (
                    object.get("name").and_then(serde_json::Value::as_str),
                    object.contains_key("arguments") || object.contains_key("parameters"),
                )
            };

        let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) else {
            return false;
        };

        has_args && self.known_tool_names.contains(&name.to_ascii_lowercase())
    }

    fn should_suppress_protocol_candidate(&self, text: &str) -> bool {
        if looks_like_tool_protocol_example(text) {
            return false;
        }

        if looks_like_malformed_tool_protocol_envelope_for_known_tools(text, &self.known_tool_names)
            || contains_tool_protocol_tag_call(text)
        {
            return true;
        }

        if let Some(kind) = classify_tool_protocol_envelope(text) {
            return matches!(kind, ToolProtocolEnvelopeKind::TaggedToolCall)
                || (self.has_active_tools
                    && (matches!(kind, ToolProtocolEnvelopeKind::ToolResult)
                        || tool_protocol_envelope_mentions_known_tool(
                            text,
                            &self.known_tool_names,
                        )));
        }

        // Parsed JSON that carries protocol-only fields but cannot yield a valid
        // tool call is an internal protocol failure, not user-facing text.
        if looks_like_tool_protocol_envelope(text) {
            return true;
        }

        self.looks_like_active_tool_json(text)
    }
}

#[derive(Debug, Default)]
pub(crate) struct StreamThinkTagStripper {
    pending: String,
    in_think: bool,
}

impl StreamThinkTagStripper {
    const START_TAG: &'static str = "<think>";
    const END_TAG: &'static str = "</think>";

    pub(crate) fn push(&mut self, chunk: &str) -> String {
        if chunk.is_empty() {
            return String::new();
        }

        let mut input = std::mem::take(&mut self.pending);
        input.push_str(chunk);
        let mut visible = String::new();

        loop {
            if self.in_think {
                if let Some(end) = input.find(Self::END_TAG) {
                    input = input[end + Self::END_TAG.len()..].to_string();
                    self.in_think = false;
                    continue;
                }

                let keep_len = longest_suffix_matching_prefix(&input, Self::END_TAG);
                if keep_len > 0 {
                    self.pending = input[input.len() - keep_len..].to_string();
                }
                return visible;
            }

            if let Some(start) = input.find(Self::START_TAG) {
                visible.push_str(&input[..start]);
                input = input[start + Self::START_TAG.len()..].to_string();
                self.in_think = true;
                continue;
            }

            let keep_len = longest_suffix_matching_prefix(&input, Self::START_TAG);
            if keep_len > 0 {
                let emit_len = input.len() - keep_len;
                visible.push_str(&input[..emit_len]);
                self.pending = input[emit_len..].to_string();
            } else {
                visible.push_str(&input);
            }
            return visible;
        }
    }

    pub(crate) fn finish(&mut self) -> String {
        if self.in_think {
            self.pending.clear();
            return String::new();
        }
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod terminal_marker_stripper_tests {
    use super::StreamTerminalMarkerStripper;

    #[test]
    fn strips_single_marker_at_end() {
        let mut stripper = StreamTerminalMarkerStripper::new();
        assert_eq!(stripper.push("Summary<eom>"), "");
        assert_eq!(stripper.finish(), "Summary");
    }

    #[test]
    fn strips_pipe_eom_marker_at_end() {
        let mut stripper = StreamTerminalMarkerStripper::new();
        assert_eq!(stripper.push("Summary<|eom|>"), "");
        assert_eq!(stripper.finish(), "Summary");
    }

    #[test]
    fn strips_stacked_markers_at_end() {
        let mut stripper = StreamTerminalMarkerStripper::new();
        assert_eq!(stripper.push("Summary<eom><|eom|>"), "");
        assert_eq!(stripper.finish(), "Summary");
    }

    #[test]
    fn preserves_inline_marker_with_text_after() {
        let mut stripper = StreamTerminalMarkerStripper::new();
        assert_eq!(
            stripper.push("Text <eom> more text"),
            "Text <eom> more text"
        );
        assert_eq!(stripper.finish(), "");
    }

    #[test]
    fn handles_marker_split_across_chunks() {
        let mut stripper = StreamTerminalMarkerStripper::new();
        assert_eq!(stripper.push("Summary<"), "");
        assert_eq!(stripper.push("eom>"), "");
        assert_eq!(stripper.finish(), "Summary");
    }

    #[test]
    fn handles_pipe_marker_split_across_chunks() {
        let mut stripper = StreamTerminalMarkerStripper::new();
        assert_eq!(stripper.push("Summary<|"), "");
        assert_eq!(stripper.push("eom|>"), "");
        assert_eq!(stripper.finish(), "Summary");
    }

    #[test]
    fn handles_stacked_markers_split_across_chunks() {
        let mut stripper = StreamTerminalMarkerStripper::new();
        assert_eq!(stripper.push("Summary<eom>"), "");
        assert_eq!(stripper.push("<|"), "");
        assert_eq!(stripper.push("eom|>"), "");
        assert_eq!(stripper.finish(), "Summary");
    }

    #[test]
    fn preserves_inline_marker_then_strips_terminal() {
        let mut stripper = StreamTerminalMarkerStripper::new();
        assert_eq!(stripper.push("Text <eom> inline<eom>"), "Text <eom> inline");
        assert_eq!(stripper.finish(), "");
    }

    #[test]
    fn handles_whitespace_after_marker() {
        let mut stripper = StreamTerminalMarkerStripper::new();
        assert_eq!(stripper.push("Summary<eom>\n"), "");
        assert_eq!(stripper.finish(), "Summary");
    }

    #[test]
    fn handles_long_whitespace_after_stacked_markers() {
        let mut stripper = StreamTerminalMarkerStripper::new();
        assert_eq!(stripper.push("Summary<eom>           <|eom|>"), "");
        assert_eq!(stripper.finish(), "Summary");
    }

    #[test]
    fn empty_chunk_returns_empty() {
        let mut stripper = StreamTerminalMarkerStripper::new();
        assert_eq!(stripper.push(""), "");
        assert_eq!(stripper.finish(), "");
    }

    #[test]
    fn no_marker_passes_through() {
        let mut stripper = StreamTerminalMarkerStripper::new();
        assert_eq!(stripper.push("Normal text"), "Normal text");
        assert_eq!(stripper.finish(), "");
    }
}

/// Streaming-safe terminal marker stripper.
///
/// Strips trailing terminal markers (`<eom>`, `<|eom|>`) from streaming text chunks.
/// Handles markers split across multiple chunks, stacked markers, and markers with
/// arbitrary whitespace between them.
///
/// # State machine
///
/// The stripper maintains a `pending` buffer that accumulates text. When a complete
/// marker is found at the end, it is held in `pending` until the next chunk arrives.
/// If the next chunk is non-empty, the marker is released as inline text. If
/// `finish()` is called, the marker is discarded as terminal.
#[derive(Debug, Default)]
pub(crate) struct StreamTerminalMarkerStripper {
    pending: String,
}

impl StreamTerminalMarkerStripper {
    pub(crate) fn new() -> Self {
        Self {
            pending: String::new(),
        }
    }

    /// Push a chunk of text and return the visible text with terminal markers stripped.
    pub(crate) fn push(&mut self, chunk: &str) -> String {
        if chunk.is_empty() {
            return String::new();
        }

        // Append the new chunk
        self.pending.push_str(chunk);

        // Check if pending ends with a complete marker (possibly with trailing whitespace)
        let trimmed = self.pending.trim_end();

        for marker in ["<|eom|>", "<eom>"] {
            if trimmed.ends_with(marker) {
                // Found a complete marker at the end
                // Check if there's meaningful text after the last marker
                // If the marker is at position 0 in trimmed, it's terminal
                // If there's text before it, we need to check if that text ends with another marker

                // Find the position of the last marker in trimmed
                if let Some(marker_pos) = trimmed.rfind(marker) {
                    // Check what's before this marker
                    let before_marker = &trimmed[..marker_pos];

                    // If before_marker is empty or only whitespace, this is a terminal marker
                    if before_marker.trim().is_empty() {
                        // Terminal marker - hold everything
                        return String::new();
                    }

                    // There's text before the marker - check if it ends with another marker
                    // If so, this is stacked terminal markers
                    let before_trimmed = before_marker.trim_end();
                    let is_stacked = ["<|eom|>", "<eom>"]
                        .iter()
                        .any(|m| before_trimmed.ends_with(m));

                    if is_stacked {
                        // Stacked terminal markers - hold everything
                        return String::new();
                    }

                    // There's real text before the marker - this is an inline marker
                    // But we still need to check if there's text AFTER the marker
                    let after_marker_start = marker_pos + marker.len();
                    let after_marker = &trimmed[after_marker_start..];

                    if after_marker.trim().is_empty() {
                        // Marker is at the end (possibly with whitespace) - terminal marker
                        // Check if the text before contains an inline marker
                        // If so, release the text up to (but not including) that inline marker
                        // Otherwise, hold everything pending

                        // Look for any marker in before_marker
                        let has_inline_marker = ["<|eom|>", "<eom>"]
                            .iter()
                            .any(|m| before_marker.contains(m));

                        if has_inline_marker {
                            // There's an inline marker before this terminal marker
                            // Release everything before the terminal marker
                            let result = before_marker.to_string();
                            // Hold the terminal marker and any trailing whitespace
                            let hold_end =
                                marker_pos + marker.len() + (self.pending.len() - trimmed.len());
                            self.pending = self.pending[marker_pos..hold_end].to_string();
                            return result;
                        } else {
                            // No inline marker - hold everything pending
                            return String::new();
                        }
                    } else {
                        // There's text after the marker - inline marker
                        // Release everything (the marker is part of the inline text)
                        let result = self.pending.clone();
                        self.pending.clear();
                        return result;
                    }
                }
            }
        }

        // Check if pending ends with a partial marker prefix
        // This handles cases like "Summary<" or "Summary<|" where the marker is split
        for marker in ["<|eom|>", "<eom>"] {
            for prefix_len in 1..marker.len() {
                let prefix = &marker[..prefix_len];
                if self.pending.ends_with(prefix) {
                    // Found a partial marker - hold everything pending
                    return String::new();
                }
            }
        }

        // No marker at all - release everything
        let result = self.pending.clone();
        self.pending.clear();
        result
    }

    /// Finish the stream and return any remaining text.
    /// Discards any trailing terminal markers.
    pub(crate) fn finish(&mut self) -> String {
        if self.pending.is_empty() {
            return String::new();
        }

        // Strip trailing terminal markers (possibly with whitespace between them)
        let mut result = self.pending.trim_end().to_string();

        loop {
            let original_len = result.len();
            let trimmed = result.trim_end().to_string();

            let mut found_marker = false;
            for marker in ["<|eom|>", "<eom>"] {
                if let Some(prefix) = trimmed.strip_suffix(marker) {
                    result = prefix.to_string();
                    found_marker = true;
                    break;
                }
            }

            // If no marker found but there was trailing whitespace, remove it
            if !found_marker && trimmed.len() < original_len {
                result = trimmed.clone();
            }

            // If nothing changed, we're done
            if result.len() == trimmed.len() && !found_marker {
                break;
            }
        }

        result
    }
}
