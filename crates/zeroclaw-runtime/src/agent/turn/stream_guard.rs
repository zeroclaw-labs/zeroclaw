//! Streaming-text guards: protocol-fragment buffering and `<think>` tag stripping.

use super::protocol_detect::{
    complete_json_fence_protocol_state, complete_non_protocol_json,
    find_embedded_protocol_candidate_start, find_incomplete_protocol_candidate_start,
    longest_suffix_matching_prefix, starts_suspicious_protocol_prefix,
    starts_suspicious_tag_or_fence_prefix,
};
use std::collections::HashSet;
use zeroclaw_tool_call_parser::{
    TERMINAL_MARKERS, ToolProtocolEnvelopeKind, classify_tool_protocol_envelope,
    contains_tool_protocol_tag_call, looks_like_malformed_tool_protocol_envelope_for_known_tools,
    looks_like_tool_protocol_envelope, looks_like_tool_protocol_example,
    strip_trailing_terminal_markers, tool_protocol_envelope_mentions_known_tool,
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
    use zeroclaw_tool_call_parser::{TERMINAL_MARKERS, strip_trailing_terminal_markers};

    #[test]
    fn strips_single_marker_at_end() {
        let mut stripper = StreamTerminalMarkerStripper::new();
        // The safe prefix streams live; only the marker is held and discarded
        // on finish.
        assert_eq!(stripper.push("Summary<eom>"), "Summary");
        assert_eq!(stripper.finish(), "");
    }

    #[test]
    fn strips_pipe_eom_marker_at_end() {
        let mut stripper = StreamTerminalMarkerStripper::new();
        assert_eq!(stripper.push("Summary<|eom|>"), "Summary");
        assert_eq!(stripper.finish(), "");
    }

    #[test]
    fn strips_stacked_markers_at_end() {
        let mut stripper = StreamTerminalMarkerStripper::new();
        assert_eq!(stripper.push("Summary<eom><|eom|>"), "Summary");
        assert_eq!(stripper.finish(), "");
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
        assert_eq!(stripper.push("Summary<"), "Summary");
        assert_eq!(stripper.push("eom>"), "");
        assert_eq!(stripper.finish(), "");
    }

    #[test]
    fn handles_pipe_marker_split_across_chunks() {
        let mut stripper = StreamTerminalMarkerStripper::new();
        assert_eq!(stripper.push("Summary<|"), "Summary");
        assert_eq!(stripper.push("eom|>"), "");
        assert_eq!(stripper.finish(), "");
    }

    #[test]
    fn handles_stacked_markers_split_across_chunks() {
        let mut stripper = StreamTerminalMarkerStripper::new();
        assert_eq!(stripper.push("Summary<eom>"), "Summary");
        assert_eq!(stripper.push("<|"), "");
        assert_eq!(stripper.push("eom|>"), "");
        assert_eq!(stripper.finish(), "");
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
        assert_eq!(stripper.push("Summary<eom>\n"), "Summary");
        assert_eq!(stripper.finish(), "");
    }

    #[test]
    fn handles_long_whitespace_after_stacked_markers() {
        let mut stripper = StreamTerminalMarkerStripper::new();
        assert_eq!(stripper.push("Summary<eom>           <|eom|>"), "Summary");
        assert_eq!(stripper.finish(), "");
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

    /// Regression for the live-streaming timing bug: a provider that sends the
    /// whole answer plus a terminal marker in ONE delta must still forward the
    /// answer immediately. The old implementation held the entire chunk in
    /// `pending` and only released it on `finish()`, so nothing streamed until
    /// the provider's `Final` event.
    #[test]
    fn push_releases_safe_prefix_and_holds_marker_suffix() {
        let mut stripper = StreamTerminalMarkerStripper::new();
        assert_eq!(
            stripper.push("A large answer<eom>"),
            "A large answer",
            "the safe prefix must stream immediately; only the marker is held"
        );
        assert_eq!(
            stripper.finish(),
            "",
            "the held marker is terminal and must be discarded on finish"
        );
    }

    /// Regression for the incomplete-marker-prefix data loss: `push` holds a
    /// possible split marker (`<eom`) plus the ordinary trailing space that
    /// follows it, and `finish` must preserve that whitespace verbatim because
    /// no complete terminal marker was produced. The non-streaming helper keeps
    /// the same input, so the two paths must agree.
    #[test]
    fn finish_preserves_whitespace_after_incomplete_marker_prefix() {
        let mut stripper = StreamTerminalMarkerStripper::new();
        // `<eom` is held as a possible split marker; the trailing space is
        // ordinary text, not part of a completed marker suffix.
        assert_eq!(stripper.push("Answer<eom "), "Answer");
        assert_eq!(
            stripper.finish(),
            "<eom ",
            "no complete marker was stripped, so the trailing space is kept"
        );
        assert_eq!(
            strip_trailing_terminal_markers("Answer<eom "),
            "Answer<eom ",
            "the non-streaming helper must preserve the same input"
        );
    }

    /// The streaming stripper and the non-streaming
    /// `strip_trailing_terminal_markers` helper must agree on the same inputs.
    /// This guards the shared-marker-vocabulary invariant: a change to
    /// [`TERMINAL_MARKERS`] or to one path that is not mirrored in the other
    /// fails here.
    #[test]
    fn streaming_matches_non_streaming_on_complete_input() {
        let cases = [
            ("Summary<eom>", "Summary"),
            ("Summary<|eom|>", "Summary"),
            ("Summary<eom><|eom|>", "Summary"),
            ("Summary<eom>  \n", "Summary"),
            ("Summary<eom>           <|eom|>", "Summary"),
            ("Text with <eom> inline", "Text with <eom> inline"),
            ("<eom>", ""),
            ("<eom>\n<|eom|>", ""),
            ("Answer<eom ", "Answer<eom "),
            ("", ""),
        ];
        for (input, expected) in cases {
            let non_streaming = strip_trailing_terminal_markers(input);
            assert_eq!(
                non_streaming, expected,
                "non-streaming helper diverged for {input:?}"
            );
            let mut stripper = StreamTerminalMarkerStripper::new();
            let live = stripper.push(input);
            let flushed = stripper.finish();
            let streamed = format!("{live}{flushed}");
            assert_eq!(
                streamed, expected,
                "streaming stripper diverged from the non-streaming helper for {input:?}"
            );
        }
    }

    /// The canonical marker vocabulary must stay aligned between the streaming
    /// state machine and the non-streaming helper. If the table is duplicated
    /// again or a marker is added to only one path, this pin fails.
    #[test]
    fn marker_vocabulary_is_shared_with_non_streaming_path() {
        assert_eq!(
            TERMINAL_MARKERS,
            ["<|eom|>", "<eom>"],
            "the canonical marker table must match the documented spellings"
        );
        for marker in TERMINAL_MARKERS {
            assert_eq!(
                strip_trailing_terminal_markers(&format!("Summary{marker}")),
                "Summary",
                "non-streaming helper must strip the shared marker {marker:?}"
            );
            let mut stripper = StreamTerminalMarkerStripper::new();
            assert_eq!(
                stripper.push(&format!("Summary{marker}")),
                "Summary",
                "streaming stripper must recognize the shared marker {marker:?}"
            );
            assert_eq!(stripper.finish(), "");
        }
    }
}

/// Streaming-safe terminal marker stripper.
///
/// Strips trailing terminal markers ([`TERMINAL_MARKERS`]) from streaming text
/// chunks. Handles markers split across multiple chunks, stacked markers, and
/// markers with arbitrary whitespace between them.
///
/// # State machine
///
/// The stripper maintains a `pending` buffer that accumulates text. When a
/// complete marker is found at the end, only the possible marker/whitespace
/// suffix is held in `pending`; the safe prefix is emitted immediately so a
/// single delta that ends in a terminal marker still streams live instead of
/// buffering the whole chunk until `finish()`. If the next chunk is non-empty
/// and turns the held suffix into inline text, the suffix is released as inline
/// text. If `finish()` is called, the marker is discarded as terminal.
#[derive(Debug, Default)]
pub(crate) struct StreamTerminalMarkerStripper {
    pending: String,
}

/// Length of the longest [`TERMINAL_MARKERS`] prefix that `text` ends with, if
/// any. Used to hold a marker that is split across chunk boundaries (e.g. a
/// chunk ending in `<` or `<|`) until the rest of the marker arrives.
fn longest_terminal_marker_prefix(text: &str) -> Option<usize> {
    TERMINAL_MARKERS
        .iter()
        .flat_map(|marker| (1..marker.len()).map(move |len| &marker[..len]))
        .filter(|prefix| text.ends_with(prefix))
        .map(str::len)
        .max()
}

impl StreamTerminalMarkerStripper {
    pub(crate) fn new() -> Self {
        Self {
            pending: String::new(),
        }
    }

    /// Push a chunk of text and return the visible text with terminal markers stripped.
    ///
    /// The safe prefix is emitted immediately and only the possible
    /// marker/whitespace suffix (including a marker split across chunk
    /// boundaries) is held, so a provider that sends a full answer plus a
    /// terminal marker in one delta still streams the answer live instead of
    /// buffering the whole chunk until [`Self::finish`]. A complete marker
    /// followed by a partial marker is held as one unit: it may resolve into
    /// stacked terminal markers.
    pub(crate) fn push(&mut self, chunk: &str) -> String {
        if chunk.is_empty() {
            return String::new();
        }

        // Append the new chunk
        self.pending.push_str(chunk);

        // From the end, strip the trailing run of whitespace / complete
        // markers / partial marker prefixes. What remains is the safe prefix.
        let mut hold_start = self.pending.len();
        loop {
            let before = hold_start;
            let ws_trimmed = self.pending[..hold_start].trim_end().len();

            let mut stripped = false;
            for marker in TERMINAL_MARKERS {
                if self.pending[..ws_trimmed].ends_with(marker) {
                    hold_start = ws_trimmed - marker.len();
                    stripped = true;
                    break;
                }
            }
            if !stripped
                && let Some(prefix_len) =
                    longest_terminal_marker_prefix(&self.pending[..ws_trimmed])
            {
                hold_start = ws_trimmed - prefix_len;
                stripped = true;
            }
            if !stripped {
                // No marker or partial prefix in the tail: the trailing
                // whitespace (if any) belongs to normal text — emit it.
                hold_start = before;
                break;
            }
            if hold_start == 0 {
                // Everything is a possible terminal marker — hold it all until
                // the next chunk decides inline vs terminal.
                return String::new();
            }
        }

        if hold_start == self.pending.len() {
            // No marker at all — release everything.
            let result = std::mem::take(&mut self.pending);
            return result;
        }

        let result = self.pending[..hold_start].to_string();
        self.pending.drain(..hold_start);
        result
    }

    /// Finish the stream and return any remaining text.
    /// Discards any trailing terminal markers.
    pub(crate) fn finish(&mut self) -> String {
        if self.pending.is_empty() {
            return String::new();
        }

        // Delegate to the shared non-streaming helper so both paths apply the
        // same policy: only whitespace that follows a *complete* recognized
        // marker is trimmed. An incomplete marker prefix plus ordinary trailing
        // whitespace (e.g. `<eom␠` with no closing `>`) is preserved verbatim —
        // it is user-visible prose, not a terminal marker suffix.
        strip_trailing_terminal_markers(&self.pending)
    }
}
