//! Streaming-text guards: protocol-fragment buffering and `<think>` tag stripping.

use super::protocol_detect::{
    complete_json_fence_protocol_state, complete_non_protocol_json,
    find_embedded_protocol_candidate_start, find_incomplete_protocol_candidate_start,
    longest_suffix_matching_prefix, starts_suspicious_protocol_prefix,
    starts_suspicious_tag_or_fence_prefix,
};
use std::collections::HashSet;
use zeroclaw_tool_call_parser::{
    EXAMPLE_FRAMING_WINDOW, TERMINAL_MARKERS, ToolProtocolEnvelopeKind,
    classify_tool_protocol_envelope, contains_tool_protocol_tag_call,
    embedded_tool_protocol_envelope_mentions_known_tool,
    looks_like_malformed_tool_protocol_envelope_for_known_tools, looks_like_tool_protocol_envelope,
    looks_like_tool_protocol_example, names_known_tool, strip_trailing_terminal_markers,
    tool_protocol_envelope_mentions_known_tool, unframed_embedded_protocol_mentions_known_tool,
};

#[derive(Debug, Default)]
pub(crate) struct StreamTextGuard {
    // Suspicious leading chunks can split `"toolcalls"` / `<tool_call>` across
    // deltas. Buffer just that prefix until it is clearly protocol or normal JSON.
    pending: String,
    pending_candidate_start: Option<usize>,
    /// The tail of the prose already forwarded, so a candidate can be judged
    /// with the same framing the completed-response check sees: an example
    /// introduced in an earlier delta must not be suppressed here and
    /// rejected later for no reason.
    recent_prose: String,
    known_tool_names: HashSet<String>,
    has_active_tools: bool,
    pub(crate) suppress_forwarding: bool,
    pub(crate) suppressed_protocol: bool,
}

impl StreamTextGuard {
    /// `available_tools` are the specs sent with the request (absent in
    /// text-tool mode); `known_tool_names` are the tools active for the turn
    /// regardless of how the request serializes them.
    pub(crate) fn new(
        available_tools: Option<&[crate::tools::ToolSpec]>,
        known_tool_names: &HashSet<String>,
    ) -> Self {
        let mut names: HashSet<String> = known_tool_names
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect();
        names.extend(
            available_tools
                .unwrap_or(&[])
                .iter()
                .map(|tool| tool.name.to_ascii_lowercase()),
        );
        Self {
            has_active_tools: !names.is_empty(),
            known_tool_names: names,
            ..Self::default()
        }
    }

    fn remember_forwarded(&mut self, text: &str) {
        self.recent_prose.push_str(text);
        let keep = EXAMPLE_FRAMING_WINDOW * 2;
        if self.recent_prose.len() > keep {
            let mut cut = self.recent_prose.len() - keep;
            while !self.recent_prose.is_char_boundary(cut) {
                cut += 1;
            }
            self.recent_prose.drain(..cut);
        }
    }

    /// Prose immediately before the current candidate: what was already
    /// forwarded plus the held prefix of this chunk.
    fn prose_before_candidate(&self) -> String {
        let prefix = self
            .pending_candidate_start
            .and_then(|start| self.pending.get(..start))
            .unwrap_or("");
        format!("{}{}", self.recent_prose, prefix)
    }

    pub(crate) fn push(&mut self, chunk: &str) -> Option<String> {
        if self.suppress_forwarding || chunk.is_empty() {
            return None;
        }

        if self.pending.is_empty() && !starts_suspicious_protocol_prefix(chunk) {
            if let Some(start) = find_embedded_protocol_candidate_start(chunk) {
                self.pending_candidate_start = Some(start);
                self.pending.push_str(chunk);
                let prose_before = self.prose_before_candidate();
                return if self
                    .should_suppress_protocol_candidate(&self.pending[start..], &prose_before)
                {
                    self.suppress_protocol();
                    None
                } else {
                    self.evaluate_pending(false)
                };
            }
            if let Some(start) = find_incomplete_protocol_candidate_start(chunk) {
                self.pending_candidate_start = Some(start);
                self.pending.push_str(chunk);
                return None;
            }
            if self.buffer_embeds_leak(chunk) {
                self.suppress_protocol();
                return None;
            }
            self.remember_forwarded(chunk);
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
        self.release_pending()
    }

    fn evaluate_pending(&mut self, finalizing: bool) -> Option<String> {
        let prose_before = self.prose_before_candidate();
        let candidate = self
            .pending_candidate_start
            .and_then(|start| self.pending.get(start..))
            .unwrap_or(&self.pending);

        if !finalizing && starts_suspicious_tag_or_fence_prefix(candidate) {
            return None;
        }

        if self.should_suppress_protocol_candidate(candidate, &prose_before) {
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
            return self.release_pending();
        }

        if complete_non_protocol_json(candidate, &self.known_tool_names) {
            self.pending_candidate_start = None;
            return self.release_pending();
        }

        None
    }

    fn release_pending(&mut self) -> Option<String> {
        if self.buffer_embeds_leak(&self.pending) {
            self.suppress_protocol();
            return None;
        }
        let released = std::mem::take(&mut self.pending);
        self.remember_forwarded(&released);
        Some(released)
    }

    /// Whether releasing `text` would stream a protocol object for an active
    /// tool. The candidate anchor is the last opener, but a release emits the
    /// WHOLE buffer, so a leak whose identifying key the anchor search does
    /// not recognize — a unicode-escaped `tool_calls`, a `tool_call_id`
    /// result envelope — would otherwise ride out ahead of the anchor.
    fn buffer_embeds_leak(&self, text: &str) -> bool {
        self.has_active_tools
            && embedded_tool_protocol_envelope_mentions_known_tool(text, &self.known_tool_names)
            && (!(looks_like_tool_protocol_example(text)
                || looks_like_tool_protocol_example(&format!("{}{}", self.recent_prose, text)))
                || unframed_embedded_protocol_mentions_known_tool(
                    &self.recent_prose,
                    text,
                    &self.known_tool_names,
                ))
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

        has_args && names_known_tool(name, &self.known_tool_names)
    }

    fn should_suppress_protocol_candidate(&self, text: &str, prose_before: &str) -> bool {
        // Same exemption the completed-response check applies, and judged the
        // same way: over the prose already streamed PLUS the candidate, so
        // every embedded object is measured against the clause immediately
        // before it. Asking `example_framing_precedes(prose_before)` alone
        // would let one framing phrase exempt the whole rest of the stream,
        // including a second, unframed leak after the illustrated one.
        if (looks_like_tool_protocol_example(text)
            || looks_like_tool_protocol_example(&format!("{prose_before}{text}")))
            && !unframed_embedded_protocol_mentions_known_tool(
                prose_before,
                text,
                &self.known_tool_names,
            )
        {
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

        // A protocol object for an active tool embedded in the held-back text
        // (a python tool stub, or an envelope inside other JSON) is valid JSON
        // that the checks above do not recognize; releasing it would stream a
        // leak the final response check then rejects.
        if self.has_active_tools
            && embedded_tool_protocol_envelope_mentions_known_tool(text, &self.known_tool_names)
        {
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
mod embedded_protocol_stream_tests {
    use super::StreamTextGuard;

    fn shell_names() -> std::collections::HashSet<String> {
        std::collections::HashSet::from(["shell".to_string()])
    }

    fn shell_guard() -> StreamTextGuard {
        let specs = vec![crate::tools::ToolSpec::new(
            "shell",
            "run a command",
            serde_json::json!({"type": "object"}),
        )];
        StreamTextGuard::new(Some(&specs), &shell_names())
    }

    /// Text-tool mode: the request carries no native tool specs, but the
    /// turn's known tool names are still supplied.
    fn text_mode_shell_guard() -> StreamTextGuard {
        StreamTextGuard::new(None, &shell_names())
    }

    const STUB: &str =
        r#"{"content":"One moment.","tool_code":"print(shell(\"ls\"))","tool_name":"shell"}"#;

    fn drive(guard: &mut StreamTextGuard, chunks: &[&str]) -> String {
        let mut forwarded = String::new();
        for chunk in chunks {
            if let Some(out) = guard.push(chunk) {
                forwarded.push_str(&out);
            }
        }
        if let Some(out) = guard.finish() {
            forwarded.push_str(&out);
        }
        forwarded
    }

    #[test]
    fn python_tool_stub_leak_is_never_streamed() {
        let mut guard = shell_guard();
        let forwarded = drive(
            &mut guard,
            &["Creating the draft now.\n", STUB, " Done shortly."],
        );
        assert!(
            !forwarded.contains("tool_code"),
            "stub bytes reached the stream: {forwarded:?}"
        );
        assert!(guard.suppressed_protocol, "the leak must be suppressed");
    }

    #[test]
    fn stub_split_at_every_boundary_is_never_streamed() {
        // A leak arrives in arbitrary deltas. Whatever the split, no protocol
        // bytes may be forwarded before the turn is rejected — including the
        // first half, which carries no recognizable protocol key yet.
        let text = format!("Creating now. {STUB} Done shortly.");
        let lead = "Creating now. ".len();
        for split in lead..lead + STUB.len() {
            if !text.is_char_boundary(split) {
                continue;
            }
            let mut guard = shell_guard();
            let forwarded = drive(&mut guard, &[&text[..split], &text[split..]]);
            assert!(
                !forwarded.contains("tool_code") && !forwarded.contains("tool_name"),
                "split at {split}: protocol bytes forwarded: {forwarded:?}"
            );
            assert!(
                guard.suppressed_protocol,
                "split at {split}: not suppressed"
            );
        }
    }

    #[test]
    fn framing_exempts_only_the_object_it_illustrates_when_streaming() {
        // One framing phrase must not exempt everything that follows it.
        // The completed-response check rejects this reply, so releasing it
        // would put both stubs on screen before the turn is retried.
        // One delta carrying both: the candidate runs from the first opener
        // to the end, so judging the framing once for the whole candidate
        // would release the unframed leak along with the illustration.
        let mut guard = shell_guard();
        let forwarded = drive(
            &mut guard,
            &[&format!(
                "For example, the stub looks like this: {STUB} Now run it: {STUB}"
            )],
        );
        assert!(
            guard.suppressed_protocol,
            "an unframed leak beside an illustration must be suppressed"
        );
        assert!(
            !forwarded.contains("tool_code"),
            "protocol bytes reached the stream: {forwarded:?}"
        );

        // Split across deltas, the illustration has already been streamed
        // when the unframed leak arrives; the leak itself must still be
        // suppressed rather than released on the earlier framing.
        let mut guard = shell_guard();
        let forwarded = drive(
            &mut guard,
            &[
                "For example, the stub looks like this: ",
                STUB,
                " Now run it: ",
                STUB,
            ],
        );
        assert!(guard.suppressed_protocol, "second leak must be suppressed");
        assert_eq!(
            forwarded.matches("tool_code").count(),
            1,
            "only the illustration may stream: {forwarded:?}"
        );
    }

    #[test]
    fn a_leak_ahead_of_the_candidate_anchor_is_not_released() {
        // The anchor is the last opener, but a release emits the whole
        // buffer. A unicode-escaped protocol key is invisible to the anchor
        // search while the completed-response check detects it.
        let escaped = r#"Running now: {"content":null,"tool_calls":[{"arguments":{"command":"id"},"id":"c1","name":"shell"}]}"#;
        let mut guard = shell_guard();
        let forwarded = drive(&mut guard, &[escaped]);
        assert!(
            !forwarded.contains("ool_calls"),
            "escaped-key leak reached the stream: {forwarded:?}"
        );
        assert!(guard.suppressed_protocol);

        // Same shape for a tool-result envelope, which the anchor search
        // also does not key on.
        let result_envelope =
            r#"Result received: {"tool_call_id":"c1","name":"shell","content":{"files":["a"]}}"#;
        let mut guard = shell_guard();
        let forwarded = drive(&mut guard, &[result_envelope]);
        assert!(
            !forwarded.contains("tool_call_id"),
            "tool-result leak reached the stream: {forwarded:?}"
        );
    }

    #[test]
    fn inline_json_in_prose_keeps_streaming() {
        // Holding an opener until it is identifiable must not hold an
        // ordinary inline object forever: the value has closed, nothing
        // further can identify it, and the rest of the reply would
        // otherwise arrive in one burst at end of stream.
        let mut guard = shell_guard();
        let first = guard.push("Set this in your config: {\"retries\": 3} and restart.");
        assert!(
            first.is_some_and(|text| text.contains("retries")),
            "inline JSON must stream as it arrives"
        );
        let second = guard.push(" Then run the doctor.");
        assert!(
            second.is_some_and(|text| text.contains("doctor")),
            "streaming must continue after inline JSON"
        );
        assert!(!guard.suppressed_protocol);
    }

    #[test]
    fn a_tag_example_does_not_carry_a_bare_leak_onto_the_stream() {
        let mut guard = shell_guard();
        let forwarded = drive(
            &mut guard,
            &[
                r#"For example, a call looks like this: <tool_call>{"name":"shell","arguments":{"command":"id"}}</tool_call>"#,
                " Now run it: ",
                STUB,
            ],
        );
        assert!(guard.suppressed_protocol, "bare leak not suppressed");
        assert!(!forwarded.contains("tool_code"), "{forwarded:?}");
    }

    #[test]
    fn a_brace_inside_a_string_does_not_release_the_outer_stub() {
        // `{placeholder}` inside the content string must not be mistaken for
        // the start of the candidate: the unfinished outer stub is what has
        // to be held, at every split.
        let prose = "Creating now. ";
        let stubs = [
            "{\"content\":\"Status {placeholder} is ready\",\n\"tool_code\":\"print(shell())\",\"tool_name\":\"shell\"}",
            // A lone closer inside the string must not count as the object
            // closing.
            "{\"content\":\"Done } now\",\n\"tool_code\":\"print(shell())\",\"tool_name\":\"shell\"}",
        ];
        for stub in stubs {
            let text = format!("{prose}{stub}");
            for split in prose.len() + 1..text.len() {
                let (first, second) = text.split_at(split);
                let mut guard = shell_guard();
                let forwarded = drive(&mut guard, &[first, second]);
                assert!(
                    !forwarded.contains("tool_code") && !forwarded.contains("content"),
                    "split at {split}: stub bytes streamed: {forwarded:?}"
                );
                assert!(
                    guard.suppressed_protocol,
                    "split at {split}: not suppressed"
                );
            }
        }
        // Completed inline JSON with a brace inside a string still streams.
        let mut guard = shell_guard();
        let first = guard.push("Set {\"greeting\":\"hi {name}\"} in the config and restart.");
        assert!(first.is_some_and(|text| text.contains("greeting")));
    }

    #[test]
    fn a_complete_protocol_object_in_one_delta_is_held_and_judged() {
        // A value that closes inside the chunk is still held when it shows a
        // protocol key, so a tool result arriving whole is not forwarded.
        let mut guard = shell_guard();
        let forwarded = drive(
            &mut guard,
            &["Done.\n{\"tool_call_id\":\"call_1\",\"content\":\"ok\"}"],
        );
        assert!(!forwarded.contains("tool_call_id"), "{forwarded:?}");
        assert!(guard.suppressed_protocol);
    }

    #[test]
    fn many_openers_before_a_split_stub_do_not_release_it() {
        // Harmless closed values must not exhaust the scan budget before a
        // later unfinished stub. Neither delta can be held by the
        // protocol-key rule on its own: the first carries no key, the second
        // carries no opener.
        let first = format!(
            "Status: {} Creating now. {{\"content\":\"One moment.\",",
            "[]".repeat(64)
        );
        let mut guard = shell_guard();
        let forwarded = drive(
            &mut guard,
            &[
                &first,
                "\"tool_code\":\"print(shell())\",\"tool_name\":\"shell\"}",
            ],
        );
        assert!(
            !forwarded.contains("tool_code") && !forwarded.contains("One moment."),
            "stub bytes streamed: {forwarded:?}"
        );
        assert!(guard.suppressed_protocol, "not suppressed");
    }

    #[test]
    fn a_stray_bracket_in_prose_does_not_hold_the_reply() {
        let mut guard = shell_guard();
        let first =
            guard.push("Intervals like [0, 1) are half-open; a \"function\" maps [a, b] to reals.");
        assert!(
            first.is_some_and(|text| text.contains("reals")),
            "prose with an unclosed bracket must keep streaming"
        );
    }

    #[test]
    fn a_framed_example_split_mid_object_still_streams() {
        // Streaming parity at every chunk boundary: an example that is still
        // arriving is naturally unfinished — past its `print(shell(` it
        // already names the tool — and suppressing it would reject
        // documentation the completed-response check exempts.
        for split in 1..STUB.len() {
            let (first, second) = STUB.split_at(split);
            let mut guard = shell_guard();
            let forwarded = drive(
                &mut guard,
                &["For example, the stub looks like this: ", first, second],
            );
            assert!(
                !guard.suppressed_protocol,
                "split at {split}: framed example suppressed"
            );
            assert!(
                forwarded.ends_with(STUB),
                "split at {split}: example not streamed whole: {forwarded:?}"
            );
        }
    }

    #[test]
    fn text_tool_mode_still_suppresses_a_stub_leak() {
        let mut guard = text_mode_shell_guard();
        let forwarded = drive(&mut guard, &["Creating now. ", STUB]);
        assert!(!forwarded.contains("tool_code"), "{forwarded:?}");
        assert!(guard.suppressed_protocol);
    }

    #[test]
    fn example_framed_in_an_earlier_delta_is_not_suppressed() {
        // The framing clause streamed in a previous delta must exempt the
        // object exactly as the completed-response check does.
        let mut guard = shell_guard();
        let forwarded = drive(
            &mut guard,
            &[
                "For example, the stub looks like this: ",
                STUB,
                " and that is all.",
            ],
        );
        assert!(
            !guard.suppressed_protocol,
            "framed example must not be suppressed"
        );
        assert!(
            forwarded.contains("tool_code"),
            "framed example must reach the stream: {forwarded:?}"
        );
        assert!(zeroclaw_tool_call_parser::looks_like_tool_protocol_example(
            &forwarded
        ));
    }

    #[test]
    fn unrelated_example_phrase_in_earlier_prose_does_not_exempt() {
        let mut guard = shell_guard();
        let forwarded = drive(
            &mut guard,
            &["For example, see the schema above. Run this now: ", STUB],
        );
        assert!(!forwarded.contains("tool_code"), "{forwarded:?}");
        assert!(guard.suppressed_protocol);
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
