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

/// Streaming-safe stripper for **trailing** provider/model end-of-message
/// markers (issue #9006 — `<eom>` and `<|eom|>` leaking into transcripts).
///
/// Peels complete markers from the input tail on every chunk and withholds a
/// small suffix that could still become a marker on the next chunk. Mid-text
/// markers are left intact — the issue explicitly demands "narrowly scoped"
/// normalization so prose like "literal <eom> in code" stays untouched.
#[derive(Debug, Default)]
pub(crate) struct StreamTerminalMarkerStripper {
    pending: String,
}

impl StreamTerminalMarkerStripper {
    const MARKERS: &'static [&'static str] = &["<|eom|>", "<eom>"];

    pub(crate) fn push(&mut self, chunk: &str) -> String {
        if chunk.is_empty() {
            return String::new();
        }

        let old_pending = std::mem::take(&mut self.pending);
        let old_pending_was_complete_marker = Self::MARKERS.contains(&old_pending.as_str());

        // If the previous call held a complete candidate marker and the new
        // chunk is non-empty, the marker was inline. Release it into the
        // visible output before processing the new text.
        let mut input = String::with_capacity(old_pending.len() + chunk.len());
        let mut visible = String::new();
        if old_pending_was_complete_marker {
            visible.push_str(&old_pending);
            input.push_str(chunk);
        } else {
            input.push_str(&old_pending);
            input.push_str(chunk);
        }

        loop {
            let Some(start) = input.find('<') else {
                visible.push_str(&input);
                self.pending.clear();
                return visible;
            };

            visible.push_str(&input[..start]);
            let tail = &input[start..];

            // Case 1: `tail` is exactly a complete marker. Hold it
            // provisionally until either the next chunk proves it inline or
            // `finish()` confirms it terminal.
            if let Some(marker) = Self::MARKERS.iter().find(|m| tail == **m).copied() {
                self.pending.clear();
                self.pending.push_str(marker);
                return visible;
            }

            // Case 2: `tail` starts with a complete marker but has more text
            // after it within this same chunk. The marker is inline.
            if let Some(marker) = Self::MARKERS
                .iter()
                .find(|m| tail.starts_with(**m))
                .copied()
            {
                visible.push_str(marker);
                input = tail[marker.len()..].to_string();
                continue;
            }

            // Case 3: `tail` is a strict prefix of some marker. Keep it
            // pending. `longest_suffix_matching_prefix` is safe here because
            // `pattern[..len]` walks byte offsets within an ASCII marker; the
            // returned `len` is at most `tail.len()`, and the slice boundary
            // sits at a known ASCII prefix so the resulting split is a valid
            // UTF-8 boundary.
            let keep_len = Self::MARKERS
                .iter()
                .map(|m| longest_suffix_matching_prefix(tail, m))
                .max()
                .unwrap_or(0);
            if keep_len > 0 && keep_len == tail.len() {
                self.pending.clear();
                self.pending.push_str(tail);
                return visible;
            }

            // Case 4: `tail` begins with `<` but is not a marker prefix.
            // Consume the `<` and continue scanning.
            visible.push('<');
            input = tail[1..].to_string();
        }
    }

    pub(crate) fn finish(&mut self) -> String {
        let final_pending = std::mem::take(&mut self.pending);
        // If the held suffix is itself a complete marker, the stream ended
        // before any further text arrived — discard it. Otherwise emit the
        // held text verbatim (it may be a partial marker prefix or ordinary
        // trailing bytes that the loop above chose to retain).
        if Self::MARKERS.contains(&final_pending.as_str()) {
            String::new()
        } else {
            final_pending
        }
    }
}

#[cfg(test)]
mod terminal_marker_stripper_tests {
    use super::StreamTerminalMarkerStripper;

    /// Drive the stripper to completion and concatenate the result.
    /// Per-call emissions are intentionally not asserted: the stripper holds
    /// a bounded suffix for streaming safety, so an intermediate `push`
    /// return value may include only the safe-to-emit prefix. The aggregate
    /// (visible text + finish flush) is the contract that matters.
    fn drain(chunks: &[&str]) -> String {
        let mut stripper = StreamTerminalMarkerStripper::default();
        let mut out = String::new();
        for chunk in chunks {
            out.push_str(&stripper.push(chunk));
        }
        out.push_str(&stripper.finish());
        out
    }

    #[test]
    fn strips_complete_trailing_eom() {
        assert_eq!(drain(&["Summary<eom>"]), "Summary");
    }

    #[test]
    fn strips_complete_trailing_pipe_eom() {
        assert_eq!(drain(&["Summary<|eom|>"]), "Summary");
    }

    #[test]
    fn preserves_inline_eom_when_more_text_follows() {
        // Audacity88 round-2 Blocking 1: a marker followed by more text in a
        // later chunk must be preserved verbatim.
        assert_eq!(
            drain(&["literal <eom>", " in code"]),
            "literal <eom> in code"
        );
    }

    #[test]
    fn preserves_inline_pipe_eom_when_more_text_follows() {
        assert_eq!(
            drain(&["literal <|eom|>", " in code"]),
            "literal <|eom|> in code"
        );
    }

    #[test]
    fn splits_eom_across_chunks() {
        // Marker fragmented across four chunks. The split must not leak any
        // piece of `<eom>` into the visible transcript.
        assert_eq!(drain(&["Summary", "<", "eom", ">"]), "Summary");
    }

    #[test]
    fn splits_pipe_eom_across_chunks() {
        assert_eq!(drain(&["Summary", "<|", "eom|", ">"]), "Summary");
    }

    #[test]
    fn preserves_inline_marker_after_complete_chunk_boundary() {
        // The original over-eager-strip regression: chunk ends with the
        // complete marker, more text arrives in a later chunk. The marker
        // must be emitted verbatim, not stripped.
        assert_eq!(
            drain(&["Summary<eom>", " more text"]),
            "Summary<eom> more text"
        );
    }

    #[test]
    fn holds_partial_marker_prefix_across_chunks() {
        assert_eq!(drain(&["hello<", "world"]), "hello<world");
    }

    #[test]
    fn handles_multibyte_text_adjacent_to_marker_prefix() {
        // Audacity88 round-2 Blocking 2: a multibyte scalar immediately
        // before a `<` marker prefix must not panic on byte slicing.
        assert_eq!(drain(&["你<", "world"]), "你<world");
    }

    #[test]
    fn handles_multibyte_text_alone() {
        assert_eq!(drain(&["你好世界"]), "你好世界");
    }

    #[test]
    fn handles_empty_chunk() {
        let mut stripper = StreamTerminalMarkerStripper::default();
        assert_eq!(stripper.push(""), "");
        // State must remain untouched.
        assert_eq!(stripper.push("hello"), "hello");
        assert_eq!(stripper.finish(), "");
    }

    #[test]
    fn preserves_plain_text_without_markers() {
        assert_eq!(
            drain(&["plain answer with no tag"]),
            "plain answer with no tag"
        );
    }

    #[test]
    fn finish_returns_held_suffix_when_stream_ends_with_partial_marker() {
        // `push("hello<")` returns `"hello"` (held the `<`); `finish()`
        // returns the held `<`.
        let mut stripper = StreamTerminalMarkerStripper::default();
        let first = stripper.push("hello<");
        let tail = stripper.finish();
        assert_eq!(first + &tail, "hello<");
    }

    #[test]
    fn finish_returns_empty_when_nothing_held() {
        let mut stripper = StreamTerminalMarkerStripper::default();
        assert_eq!(stripper.finish(), "");
        // Idempotency: second call still empty.
        assert_eq!(stripper.finish(), "");
    }

    #[test]
    fn finish_is_idempotent() {
        let mut stripper = StreamTerminalMarkerStripper::default();
        stripper.push("hello<");
        let first = stripper.finish();
        let second = stripper.finish();
        // First call drained the held suffix; second must be empty.
        assert_eq!(first, "<");
        assert_eq!(second, "");
    }

    #[test]
    fn does_not_strip_marker_like_text() {
        // Trailing character invalidates the marker — must not strip.
        assert_eq!(drain(&["Summary<eomx>"]), "Summary<eomx>");
    }

    #[test]
    fn marker_inside_word_is_inline() {
        // `<eom` mid-word with no closing `>` is not a complete marker.
        assert_eq!(drain(&["see<eomfile"]), "see<eomfile");
    }
}
