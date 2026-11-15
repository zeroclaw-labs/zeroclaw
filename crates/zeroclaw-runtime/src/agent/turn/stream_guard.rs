//! Runtime adapter for the shared incremental response parser.

use crate::tools::ToolSpec;

pub(crate) use zeroclaw_tool_call_parser::{
    ProtocolSuppressionDiagnostic, StreamTerminalMarkerStripper, StreamThinkTagStripper,
};

/// Runtime owns tool admission and event delivery; the parser crate owns the
/// incremental protocol state machine. This adapter translates admitted tool
/// specs into the parser's dependency-free name list and mirrors diagnostics
/// for the existing runtime telemetry/tests.
#[derive(Debug)]
pub(crate) struct StreamTextGuard {
    inner: zeroclaw_tool_call_parser::StreamTextGuard,
    pub(crate) suppress_forwarding: bool,
    pub(crate) suppressed_protocol: bool,
    pub(crate) suppression: Option<ProtocolSuppressionDiagnostic>,
}

impl StreamTextGuard {
    pub(crate) fn new(available_tools: Option<&[ToolSpec]>) -> Self {
        let names = available_tools.map(|tools| {
            tools
                .iter()
                .map(|tool| tool.name.to_owned())
                .collect::<Vec<_>>()
        });
        Self {
            inner: zeroclaw_tool_call_parser::StreamTextGuard::new(names.as_deref()),
            suppress_forwarding: false,
            suppressed_protocol: false,
            suppression: None,
        }
    }

    pub(crate) fn push(&mut self, chunk: &str) -> Option<String> {
        let output = self.inner.push(chunk);
        self.sync_state();
        output
    }

    pub(crate) fn finish(&mut self) -> Option<String> {
        let output = self.inner.finish();
        self.sync_state();
        output
    }

    fn sync_state(&mut self) {
        self.suppress_forwarding = self.inner.suppress_forwarding;
        self.suppressed_protocol = self.inner.suppressed_protocol;
        self.suppression = self.inner.suppression;
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

#[cfg(test)]
mod stream_text_guard_tests {
    use super::{ProtocolSuppressionDiagnostic, StreamTextGuard};

    fn guard_with_tool() -> StreamTextGuard {
        StreamTextGuard::new(Some(&[crate::tools::ToolSpec::new(
            "shell",
            "run a command",
            serde_json::json!({"type": "object"}),
        )]))
    }

    fn push_all(guard: &mut StreamTextGuard, chunks: &[&str]) -> String {
        let mut forwarded = String::new();
        for chunk in chunks {
            if let Some(text) = guard.push(chunk) {
                forwarded.push_str(&text);
            }
        }
        if let Some(tail) = guard.finish() {
            forwarded.push_str(&tail);
        }
        forwarded
    }

    /// Run one whole-message scenario through the guard and return the
    /// forwarded text, the suppression flag and the diagnostic, so split
    /// and whole-delta outcomes can be compared for equality.
    fn guard_outcome(chunks: &[&str]) -> (String, bool, Option<ProtocolSuppressionDiagnostic>) {
        guard_outcome_with(chunks, guard_with_tool)
    }

    /// The same scenario through a differently constructed guard, so
    /// outcomes can be compared across tool-spec configurations.
    fn guard_outcome_with(
        chunks: &[&str],
        new_guard: impl FnOnce() -> StreamTextGuard,
    ) -> (String, bool, Option<ProtocolSuppressionDiagnostic>) {
        let mut guard = new_guard();
        let forwarded = push_all(&mut guard, chunks);
        (forwarded, guard.suppressed_protocol, guard.suppression)
    }

    /// The scenario with no tool specs at all: the guard still sees the
    /// protocol shapes it must not deliver.
    fn guard_outcome_without_tools(
        chunks: &[&str],
    ) -> (String, bool, Option<ProtocolSuppressionDiagnostic>) {
        guard_outcome_with(chunks, || StreamTextGuard::new(None))
    }

    fn guard_with_specs(names: &[&str]) -> StreamTextGuard {
        let specs: Vec<crate::tools::ToolSpec> = names
            .iter()
            .map(|name| {
                crate::tools::ToolSpec::new(
                    *name,
                    "test tool",
                    serde_json::json!({"type": "object"}),
                )
            })
            .collect();
        StreamTextGuard::new(Some(&specs))
    }

    /// Tagged tool-call markup is never legitimate prose: it suppresses
    /// wherever it appears, but the prose buffered ahead of the candidate is
    /// ordinary text and must still be delivered.
    #[test]
    fn tagged_call_suppression_releases_buffered_prose_prefix() {
        let mut guard = guard_with_tool();
        let prefix = "Here is the call: ";
        let call =
            "<tool_call>{\"name\": \"shell\", \"arguments\": {\"command\": \"ls\"}}</tool_call>";
        let tagged_text = format!("{prefix}{call}");
        let forwarded = push_all(&mut guard, &[tagged_text.as_str()]);
        assert_eq!(
            forwarded, prefix,
            "prose buffered ahead of a suppressed candidate must be released"
        );
        assert!(guard.suppressed_protocol);
        assert_eq!(
            guard.suppression,
            Some(ProtocolSuppressionDiagnostic {
                detector: "tagged",
                candidate_offset: prefix.len(),
            })
        );
    }

    /// A preamble followed by a known-tool call envelope is the ordinary
    /// non-native tool-call leak: the envelope is withheld and the preamble
    /// is delivered.
    #[test]
    fn prose_prefix_then_known_tool_envelope_is_suppressed_and_prefix_released() {
        let mut guard = guard_with_tool();
        let prefix = "Let me check.\n";
        let forwarded = push_all(
            &mut guard,
            &[
                prefix,
                "{\"tool_calls\": [{\"function\": {\"name\": \"shell\",",
                " \"arguments\": {\"command\": \"ls\"}}}]}",
            ],
        );
        assert_eq!(forwarded, prefix);
        assert!(guard.suppressed_protocol);
        assert_eq!(
            guard.suppression,
            Some(ProtocolSuppressionDiagnostic {
                detector: "function_call",
                candidate_offset: prefix.len(),
            })
        );
    }

    /// The malformed toolcalls-key variant after a preamble is withheld the
    /// same way, with the preamble delivered.
    #[test]
    fn prose_prefix_then_malformed_known_tool_envelope_is_suppressed() {
        let mut guard = guard_with_tool();
        let prefix = "Let me check.\n";
        let forwarded = push_all(
            &mut guard,
            &[
                prefix,
                "{\"toolcalls\": [{\"call_id\": \"call_1\",",
                " \"arguments\": {\"command\": \"ls\"}}",
            ],
        );
        assert_eq!(forwarded, prefix);
        assert!(guard.suppressed_protocol);
        assert_eq!(
            guard.suppression,
            Some(ProtocolSuppressionDiagnostic {
                detector: "malformed",
                candidate_offset: prefix.len(),
            })
        );
    }

    /// The one-chunk variant of the preamble leak: the prefix and the
    /// envelope arrive in a single delta, so the prefix is buffered ahead
    /// of the candidate and must be released by the suppression itself (the
    /// split-chunk variant forwarded the prefix before any candidate
    /// existed, so it never exercised the release).
    #[test]
    fn prose_prefix_and_known_tool_envelope_in_one_chunk_is_suppressed_and_prefix_released() {
        let mut guard = guard_with_tool();
        let prefix = "Sure! ";
        let envelope = "{\"tool_calls\": [{\"function\": {\"name\": \"shell\", \"arguments\": {\"command\": \"ls\"}}}]}";
        let forwarded = push_all(&mut guard, &[format!("{prefix}{envelope}").as_str()]);
        assert_eq!(forwarded, prefix);
        assert!(guard.suppressed_protocol);
        assert_eq!(
            guard.suppression,
            Some(ProtocolSuppressionDiagnostic {
                detector: "function_call",
                candidate_offset: prefix.len(),
            })
        );
    }

    /// The one-chunk variant of the malformed preamble leak: the forwarded
    /// text is exactly the prefix.
    #[test]
    fn prose_prefix_and_malformed_known_tool_envelope_in_one_chunk_is_suppressed() {
        let mut guard = guard_with_tool();
        let prefix = "Sure! ";
        let envelope =
            "{\"toolcalls\": [{\"call_id\": \"call_1\", \"arguments\": {\"command\": \"ls\"}}";
        let forwarded = push_all(&mut guard, &[format!("{prefix}{envelope}").as_str()]);
        assert_eq!(forwarded, prefix);
        assert!(guard.suppressed_protocol);
        assert_eq!(
            guard.suppression,
            Some(ProtocolSuppressionDiagnostic {
                detector: "malformed",
                candidate_offset: prefix.len(),
            })
        );
    }

    /// Regression for the issue: prose, then a code span quoting an object
    /// with the correlation and payload keys, then more prose, streamed so
    /// the split lands inside the object. A tool-result shape after a prose
    /// preamble is delivered once the object completes, so every byte is
    /// forwarded and nothing is flagged.
    #[test]
    fn prose_code_span_object_split_inside_is_forwarded() {
        let mut guard = guard_with_tool();
        let message = "The history message looks like `{\"tool_call_id\": \"call_1\", \"content\": \"ok\"}` and that is the whole shape.";
        let forwarded = push_all(
            &mut guard,
            &[
                "The history message looks like `{",
                "\"tool_call_id\": \"call_1\",",
                " \"content\": \"ok\"}` and that is the whole shape.",
            ],
        );
        assert_eq!(forwarded, message);
        assert!(
            !guard.suppressed_protocol,
            "an embedded quoted object must not be suppressed"
        );
        assert!(guard.suppression.is_none());
    }

    /// The same object as the entire message is a genuine whole-message
    /// envelope: suppressed, with the detector and candidate offset
    /// recorded. A result shape that leads the message is withheld
    /// whether or not tool specs reached the guard: the model cannot emit
    /// results in either configuration.
    #[test]
    fn whole_message_tool_result_object_is_suppressed() {
        let expected = (
            String::new(),
            true,
            Some(ProtocolSuppressionDiagnostic {
                detector: "tool_result",
                candidate_offset: 0,
            }),
        );
        assert_eq!(
            guard_outcome(&["{\"tool_call_id\": \"call_1\",", " \"content\": \"ok\"}"]),
            expected
        );
        assert_eq!(
            guard_outcome_without_tools(&[
                "{\"tool_call_id\": \"call_1\",",
                " \"content\": \"ok\"}"
            ]),
            expected,
            "a leading result shape is withheld even when no tool specs reached the guard"
        );
    }

    /// A tool-result shape after a prose preamble, with no code span at all,
    /// is still quoted prose: the model cannot legitimately emit a tool
    /// result as its own output.
    #[test]
    fn prose_prefix_then_tool_result_object_is_forwarded() {
        let mut guard = guard_with_tool();
        let message = "The history message is {\"tool_call_id\": \"call_1\", \"content\": \"ok\"}";
        let forwarded = push_all(
            &mut guard,
            &[
                "The history message is ",
                "{\"tool_call_id\": \"call_1\", \"content\": \"ok\"}",
            ],
        );
        assert_eq!(forwarded, message);
        assert!(!guard.suppressed_protocol);
        assert!(guard.suppression.is_none());
    }

    /// The same result object, split after the content key and colon,
    /// must not be judged malformed on its first fragment: the verdict
    /// waits for completion, and the completed object plus trailing
    /// prose is delivered byte for byte.
    #[test]
    fn prose_prefix_then_tool_result_object_split_after_content_key_is_forwarded() {
        let mut guard = guard_with_tool();
        let prefix = "The history message is ";
        let head = "{\"tool_call_id\": \"call_1\", \"content\":";
        let tail = " \"ok\"}";
        let trailing = " and that is the whole shape.";
        let forwarded = push_all(&mut guard, &[prefix, head, tail, trailing]);
        assert_eq!(
            forwarded,
            format!("{prefix}{head}{tail}{trailing}"),
            "the split object must be judged when it completes, not on its first fragment"
        );
        assert!(!guard.suppressed_protocol);
        assert!(guard.suppression.is_none());
    }

    /// The one-delta variant: a complete result object followed by
    /// trailing prose in the same candidate is a completed result shape, not
    /// a malformed envelope, and the whole text is forwarded.
    #[test]
    fn prose_prefix_then_complete_tool_result_object_with_trailing_prose_is_forwarded() {
        let mut guard = guard_with_tool();
        let prefix = "The history message is ";
        let message =
            "{\"tool_call_id\": \"call_1\", \"content\": \"ok\"} and that is the whole shape.";
        let forwarded = push_all(&mut guard, &[prefix, message]);
        assert_eq!(forwarded, format!("{prefix}{message}"));
        assert!(!guard.suppressed_protocol);
        assert!(guard.suppression.is_none());
    }

    /// A result object the stream cuts off never completes, and it
    /// carries no call-shaped key, so after the preamble it is inert
    /// and the whole message is delivered at finish. One-delta and
    /// split outcomes agree.
    #[test]
    fn prose_prefix_then_cut_off_tool_result_object_is_delivered_at_finish() {
        let prefix = "The history message is ";
        let head = "{\"tool_call_id\": \"call_1\", \"content\": \"ok";
        let message = format!("{prefix}{head}");
        let expected = (message.clone(), false, None);
        assert_eq!(
            guard_outcome(&[prefix, head]),
            expected,
            "the cut-off root is inert after the preamble, so the whole message is delivered at finish"
        );
        assert_eq!(
            guard_outcome(&[message.as_str()]),
            expected,
            "one-delta delivery must match the split outcome"
        );
    }

    /// Control: a cut-off fragment carrying a call-shaped key (a
    /// registered tool's name and an arguments key) is not inert, so
    /// it is withheld at finish as malformed, the preamble delivered,
    /// whichever delta carried it.
    #[test]
    fn prose_prefix_then_cut_off_call_shaped_fragment_is_withheld_at_finish() {
        let prefix = "The history message is ";
        let head = "{\"tool_call_id\": \"call_1\", \"content\": \"ok\", \"name\": \"shell\", \"arguments\":";
        let message = format!("{prefix}{head}");
        let expected = (
            prefix.to_string(),
            true,
            Some(ProtocolSuppressionDiagnostic {
                detector: "malformed",
                candidate_offset: prefix.len(),
            }),
        );
        assert_eq!(
            guard_outcome(&[prefix, head]),
            expected,
            "a cut-off call-shaped fragment is withheld at finish, the preamble delivered"
        );
        assert_eq!(
            guard_outcome(&[message.as_str()]),
            expected,
            "one-delta delivery must match the split outcome"
        );
    }

    /// Completion is a boundary, not a blanket exemption: after the result
    /// object and its trailing prose are delivered, a later unquoted call
    /// envelope is still withheld.
    #[test]
    fn prose_prefix_result_object_then_prose_still_withholds_later_call_envelope() {
        let mut guard = guard_with_tool();
        let prefix = "The history message is ";
        let object = "{\"tool_call_id\": \"call_1\", \"content\": \"ok\"}";
        let prose = " and the call is ";
        let envelope = "{\"tool_calls\": [{\"function\": {\"name\": \"shell\", \"arguments\": {\"command\": \"ls\"}}}]}";
        let forwarded = push_all(&mut guard, &[prefix, object, prose, envelope]);
        assert_eq!(forwarded, format!("{prefix}{object}{prose}"));
        assert!(guard.suppressed_protocol);
        assert_eq!(
            guard.suppression,
            Some(ProtocolSuppressionDiagnostic {
                detector: "function_call",
                candidate_offset: prefix.len() + object.len() + prose.len(),
            })
        );
    }

    /// The one-delta variant of the same control: the preamble, the
    /// complete result object, the prose and the later call envelope all
    /// arrive in a single chunk, so the embedded finder selects the call
    /// container's opening object as the candidate and the result object
    /// ahead of it is ordinary buffered prefix, released by the
    /// suppression itself. This exercises the finder's choice of the
    /// container key's object and the prefix release, NOT
    /// result-completion re-scanning: the completed result ahead of the
    /// candidate never triggers the release branch here.
    #[test]
    fn prose_prefix_result_object_prose_and_call_envelope_in_one_delta_still_withholds_call() {
        let mut guard = guard_with_tool();
        let prefix = "The history message is ";
        let object = "{\"tool_call_id\": \"call_1\", \"content\": \"ok\"}";
        let prose = " and the call is ";
        let envelope = "{\"tool_calls\": [{\"function\": {\"name\": \"shell\", \"arguments\": {\"command\": \"ls\"}}}]}";
        let forwarded = push_all(
            &mut guard,
            &[format!("{prefix}{object}{prose}{envelope}").as_str()],
        );
        assert_eq!(
            forwarded,
            format!("{prefix}{object}{prose}"),
            "the completed result quotation and its prose are delivered, the later envelope is not"
        );
        assert!(guard.suppressed_protocol);
        assert_eq!(
            guard.suppression,
            Some(ProtocolSuppressionDiagnostic {
                detector: "function_call",
                candidate_offset: prefix.len() + object.len() + prose.len(),
            })
        );
    }

    /// Without a prose preamble nothing changes: a leading result object
    /// split the same way is withheld, with nothing forwarded.
    #[test]
    fn leading_tool_result_object_split_after_content_key_stays_suppressed() {
        let mut guard = guard_with_tool();
        let head = "{\"tool_call_id\": \"call_1\", \"content\":";
        let tail = " \"ok\"}";
        let forwarded = push_all(&mut guard, &[head, tail]);
        assert_eq!(forwarded, "");
        assert!(guard.suppressed_protocol);
        assert_eq!(
            guard.suppression,
            Some(ProtocolSuppressionDiagnostic {
                detector: "malformed",
                candidate_offset: 0,
            })
        );
    }

    /// A payload token spelling a protocol word: a result object whose
    /// `content` value spells the word `arguments`, split after that
    /// value's closing quote and before the object's closing brace, with
    /// the brace in the next delta. The verdict waits for the object
    /// to close, so a payload string cannot make the candidate look
    /// call-shaped, and the completed object plus trailing prose is
    /// forwarded byte for byte.
    #[test]
    fn prose_prefix_result_content_value_arguments_split_is_forwarded() {
        let mut guard = guard_with_tool();
        let prefix = "The history message is ";
        let head = "{\"tool_call_id\": \"call_1\", \"content\": \"arguments\"";
        let brace = "}";
        let trailing = " and that is the whole shape.";
        let forwarded = push_all(&mut guard, &[prefix, head, brace, trailing]);
        assert_eq!(
            forwarded,
            format!("{prefix}{head}{brace}{trailing}"),
            "a payload string is not a protocol key, so the split object must defer, not suppress"
        );
        assert!(!guard.suppressed_protocol);
        assert!(guard.suppression.is_none());
    }

    /// The same escape with the payload string spelling a call-container
    /// token: identical outcome, because the verdict waits for the close and
    /// values never fire it early.
    #[test]
    fn prose_prefix_result_content_value_tool_calls_split_is_forwarded() {
        let mut guard = guard_with_tool();
        let prefix = "The history message is ";
        let head = "{\"tool_call_id\": \"call_1\", \"content\": \"tool_calls\"";
        let brace = "}";
        let trailing = " and that is the whole shape.";
        let forwarded = push_all(&mut guard, &[prefix, head, brace, trailing]);
        assert_eq!(
            forwarded,
            format!("{prefix}{head}{brace}{trailing}"),
            "a payload string is not a protocol key, so the split object must defer, not suppress"
        );
        assert!(!guard.suppressed_protocol);
        assert!(guard.suppression.is_none());
    }

    /// The same escape with an `arguments` key nested one level deeper
    /// than the leading object, split inside that nested object: the
    /// verdict waits for the close, so the object defers, completes and
    /// is forwarded.
    #[test]
    fn prose_prefix_nested_arguments_key_split_is_forwarded() {
        let mut guard = guard_with_tool();
        let prefix = "The history message is ";
        let head = "{\"tool_call_id\": \"call_1\", \"content\": {\"nested\": \"yes\", \"arguments\": \"value\"";
        let closing = "}}";
        let trailing = " and that is the whole shape.";
        let forwarded = push_all(&mut guard, &[prefix, head, closing, trailing]);
        assert_eq!(
            forwarded,
            format!("{prefix}{head}{closing}{trailing}"),
            "a nested data key is not a top-level protocol key, so the object must defer"
        );
        assert!(!guard.suppressed_protocol);
        assert!(guard.suppression.is_none());
    }

    /// A quoted LIST of result objects after a preamble, split after the
    /// first element's content key: an incomplete list defers like an
    /// incomplete object and is released whole when it completes (as
    /// complete non-protocol JSON), instead of being withheld on its
    /// first fragment.
    #[test]
    fn prose_prefix_result_array_split_after_content_key_is_forwarded() {
        let mut guard = guard_with_tool();
        let prefix = "The history messages are ";
        let head = "[{\"tool_call_id\": \"call_1\", \"content\":";
        let tail = " \"ok\"}]";
        let trailing = " in order.";
        let forwarded = push_all(&mut guard, &[prefix, head, tail, trailing]);
        assert_eq!(
            forwarded,
            format!("{prefix}{head}{tail}{trailing}"),
            "a split result list behind a preamble must defer until it completes, not suppress"
        );
        assert!(!guard.suppressed_protocol);
        assert!(guard.suppression.is_none());
    }

    /// The full split matrix around one result object's identifying keys:
    /// before, inside and after the correlation key, around its colon and
    /// inside its value, after the comma, before, inside and after the
    /// payload key, after the whitespace following it, around its colon,
    /// inside its value, and before the closing brace. Every split must
    /// produce the one-delta outcome: the whole message forwarded and
    /// nothing flagged.
    #[test]
    fn prose_prefix_result_object_split_matrix_all_match_one_delta() {
        let prefix = "The history message is ";
        let object = "{\"tool_call_id\": \"call_1\", \"content\": \"ok\"}";
        let trailing = " and that is the whole shape.";
        let expected = (format!("{prefix}{object}{trailing}"), false, None);
        let one_delta = guard_outcome(&[prefix, &format!("{object}{trailing}")]);
        assert_eq!(
            one_delta, expected,
            "the one-delta spelling is the reference outcome"
        );
        let correlation = "\"tool_call_id\"";
        let payload = "\"content\"";
        let correlation_at = object.find(correlation).unwrap();
        let payload_at = object.find(payload).unwrap();
        let comma_at = object.find(',').unwrap();
        let close_at = object.find('}').unwrap();
        let named_offsets: [(&str, usize); 13] = [
            ("before the correlation key's opening quote", correlation_at),
            ("inside the correlation key", correlation_at + 5),
            (
                "after the correlation key's closing quote",
                correlation_at + correlation.len(),
            ),
            (
                "after the correlation key's colon",
                correlation_at + correlation.len() + 1,
            ),
            (
                "inside the correlation key's value",
                correlation_at + correlation.len() + 3,
            ),
            ("after the comma", comma_at + 1),
            ("before the payload key's opening quote", comma_at + 2),
            ("inside the payload key", payload_at + 3),
            (
                "after the payload key's closing quote",
                payload_at + payload.len(),
            ),
            (
                "after whitespace following the payload key",
                payload_at + payload.len() + 1,
            ),
            (
                "after the payload key's colon",
                payload_at + payload.len() + 2,
            ),
            (
                "inside the payload key's value",
                payload_at + payload.len() + 4,
            ),
            ("before the closing brace", close_at),
        ];
        for (name, offset) in named_offsets {
            let head = &object[..offset];
            let tail = &object[offset..];
            let outcome = guard_outcome(&[prefix, head, tail, trailing]);
            assert_eq!(
                outcome, expected,
                "split {name} (byte {offset}) must match the one-delta outcome"
            );
        }
    }

    /// A non-breaking space between the preamble and the object: prefix
    /// routing and the parser's helpers trim leading Unicode whitespace,
    /// so the deferral gate and the completion
    /// scan must too: the NBSP-led object is judged like any other result
    /// shape. Split after the payload key's closing
    /// quote, the object defers and the completed message is forwarded.
    #[test]
    fn prose_prefix_nbsp_led_result_split_after_payload_key_is_forwarded() {
        let prefix = "The history message is ";
        let head = "\u{00a0}{\"tool_call_id\": \"call_1\", \"content\"";
        let tail = ": \"ok\"}";
        let trailing = " and that is the whole shape.";
        let message = format!("{prefix}{head}{tail}{trailing}");
        let mut guard = guard_with_tool();
        let forwarded = push_all(&mut guard, &[prefix, head, tail, trailing]);
        assert_eq!(
            forwarded, message,
            "the scanner must read through the NBSP the way routing does"
        );
        assert!(!guard.suppressed_protocol);
        assert!(guard.suppression.is_none());
    }

    /// Control for the terminated-not-result-shaped shape: the preamble,
    /// then an object whose depth-1 members are the correlation id and a
    /// `data` object holding a `content` member, closed with a trailing
    /// comma before the final brace. The comma keeps the root from ever
    /// parsing as a complete value, and the object carries no
    /// call-shaped key, so after the preamble it is inert and the whole
    /// message is delivered at finish, whichever deltas carried it.
    #[test]
    fn prose_prefix_terminated_non_result_shaped_object_is_delivered_at_finish() {
        let prefix = "The history message is ";
        let object = "{\"tool_call_id\": \"call_1\", \"data\": {\"content\": \"x\"},}";
        let message = format!("{prefix}{object}");
        let expected = (message.clone(), false, None);
        assert_eq!(
            guard_outcome(&[prefix, object]),
            expected,
            "a root that never parses and carries no call-shaped key is delivered when the stream ends"
        );
        // The root arrives in two pieces: the unterminated first piece
        // defers, the closing piece still never parses as a complete
        // value, so the verdict waits for finish and finds the fragment
        // inert.
        let mut guard = guard_with_tool();
        let mut forwarded = String::new();
        forwarded.push_str(&guard.push(prefix).unwrap_or_default());
        forwarded.push_str(
            &guard
                .push("{\"tool_call_id\": \"call_1\", \"data\": {\"content\"")
                .unwrap_or_default(),
        );
        assert_eq!(forwarded, prefix);
        assert!(
            !guard.suppressed_protocol,
            "the unterminated root defers while the stream runs"
        );
        forwarded.push_str(&guard.push(": \"x\"},}").unwrap_or_default());
        assert_eq!(
            forwarded, prefix,
            "the trailing comma keeps the root from completing, so the fragment still defers"
        );
        assert!(
            !guard.suppressed_protocol,
            "the verdict waits for the end of the stream when the root never parses"
        );
        assert_eq!(
            guard.finish().as_deref(),
            Some(object),
            "the fragment is inert, so finish delivers it after the released preamble"
        );
        assert!(!guard.suppressed_protocol);
        assert!(guard.suppression.is_none());
        assert_eq!(
            guard_outcome(&[message.as_str()]),
            expected,
            "one-delta delivery must match the split outcomes"
        );
    }

    /// The result-shape release is the classifier's spelling alone: an
    /// object whose correlation member uses the alternate `call_id`
    /// spelling is not a tool result, so a candidate carrying it with
    /// trailing prose buffers until the stream ends (a result shape would
    /// release through the end of the object on the push that completes
    /// it), and the completed object then forwards as ordinary business
    /// JSON.
    #[test]
    fn result_shape_correlation_key_is_the_classifier_spelling_alone() {
        let prefix = "The history message is ";
        let object = "{\"call_id\": \"call_1\", \"content\": \"ok\"}";
        let trailing = " and that is the whole shape.";
        let mut guard = guard_with_tool();
        assert_eq!(
            guard
                .push(format!("{prefix}{object}{trailing}").as_str())
                .as_deref(),
            None,
            "an object whose correlation member uses the alternate spelling is not a result shape, so the candidate buffers"
        );
        let forwarded = push_all(&mut guard, &[]);
        assert_eq!(
            forwarded,
            format!("{prefix}{object}{trailing}"),
            "the completed object is ordinary JSON and forwards in full at finish"
        );
        assert!(!guard.suppressed_protocol);
        assert!(guard.suppression.is_none());
    }

    /// The classifier-rejected result shape: a prose-prefixed object whose
    /// correlation id is an empty string is not a ToolResult the
    /// result-shape release can deliver, so it is withheld as malformed, judged on the
    /// delta that terminates the root, with one-delta and split delivery
    /// agreeing.
    #[test]
    fn prose_prefix_classifier_rejected_result_shape_is_suppressed_as_malformed() {
        let prefix = "The stored message was ";
        let object = "{\"tool_call_id\": \"\", \"content\": \"ok\"}";
        let trailing = " and that is everything.";
        let message = format!("{prefix}{object}{trailing}");
        let expected = (
            prefix.to_string(),
            true,
            Some(ProtocolSuppressionDiagnostic {
                detector: "malformed",
                candidate_offset: prefix.len(),
            }),
        );
        assert_eq!(
            guard_outcome(&[message.as_str()]),
            expected,
            "a terminated result-shaped root the classifier rejects is judged on the terminating delta"
        );
        // Split after the payload key's closing quote: the unterminated
        // object defers, the delta that closes the root judges.
        let split_at = prefix.len() + object.find("\"content\"").unwrap() + "\"content\"".len();
        let (before, after) = message.split_at(split_at);
        let mut guard = guard_with_tool();
        let mut forwarded = String::new();
        forwarded.push_str(&guard.push(before).unwrap_or_default());
        assert_eq!(forwarded, "");
        assert!(!guard.suppressed_protocol, "the unterminated object defers");
        forwarded.push_str(&guard.push(after).unwrap_or_default());
        assert_eq!(forwarded, prefix);
        assert!(guard.suppressed_protocol);
        assert_eq!(
            guard.suppression,
            Some(ProtocolSuppressionDiagnostic {
                detector: "malformed",
                candidate_offset: prefix.len(),
            })
        );
        assert_eq!(
            guard_outcome(&[before, after]),
            expected,
            "split delivery must match the one-delta outcome"
        );
    }

    /// A terminated result-shaped root that is syntactically invalid (a
    /// trailing comma before its closing brace) can never parse as a
    /// complete value, and carrying no call-shaped key it is inert after
    /// the preamble: no delta carries a verdict, and the whole message
    /// is delivered at finish. One-delta and split outcomes agree.
    #[test]
    fn prose_prefix_terminated_invalid_result_object_is_delivered_at_finish() {
        let prefix = "The reply contained ";
        let object = "{\"tool_call_id\": \"call_1\", \"content\": \"ok\",}";
        let trailing = " and then prose.";
        let message = format!("{prefix}{object}{trailing}");
        let expected = (message.clone(), false, None);
        assert_eq!(
            guard_outcome(&[message.as_str()]),
            expected,
            "the one-delta outcome is the reference"
        );
        // The closing brace arrives in its own delta: no verdict before
        // it and none on it (the trailing comma keeps the root from
        // parsing as a complete value), and the finish-time judgement
        // finds the fragment inert, so the whole buffer is the tail.
        let brace_at = prefix.len() + object.len() - 1;
        let (before, after) = message.split_at(brace_at);
        let mut guard = guard_with_tool();
        let mut forwarded = String::new();
        forwarded.push_str(&guard.push(before).unwrap_or_default());
        assert_eq!(forwarded, "");
        assert!(
            !guard.suppressed_protocol,
            "no verdict before the root terminates"
        );
        forwarded.push_str(&guard.push(after).unwrap_or_default());
        assert_eq!(
            forwarded, "",
            "the closing delta still defers: the root never parses as a complete value"
        );
        assert!(!guard.suppressed_protocol);
        assert_eq!(
            guard.finish().as_deref(),
            Some(message.as_str()),
            "the inert fragment is released as the tail at finish"
        );
        assert!(!guard.suppressed_protocol);
        assert!(guard.suppression.is_none());
        assert_eq!(
            guard_outcome(&[before, after]),
            expected,
            "split delivery must match the one-delta outcome"
        );
    }

    /// An unparseable result-shaped fragment inside a one-backtick code
    /// span after a preamble, its values unquoted placeholders the way a
    /// quoted history message carries them: no call-shaped key means
    /// nothing the parser can execute, so the whole message is
    /// delivered at finish, the same judgement the completed-value
    /// release makes for a parseable result shape.
    #[test]
    fn prose_prefix_invalid_result_fragment_in_code_span_is_delivered_at_finish() {
        let preamble = "The stored turn quoted `";
        let fragment = "{\"tool_call_id\": <call-123>, \"content\": <tool output>, \"attachments\": [<attachment ref>]}";
        let trailing = " and that is the whole turn.";
        let message = format!("{preamble}{fragment}`{trailing}");
        let mut guard = guard_with_tool();
        assert_eq!(
            guard.push(message.as_str()).as_deref(),
            None,
            "the unparseable fragment defers while the stream runs"
        );
        assert_eq!(
            guard.finish().as_deref(),
            Some(message.as_str()),
            "the inert quotation is released as the whole tail at finish"
        );
        assert!(!guard.suppressed_protocol);
        assert!(guard.suppression.is_none());
    }

    /// The same message split across deltas, with the span's opening
    /// delimiter delivered on the preamble's own push: the fragment's
    /// first delta opens with the root brace, and the split outcome
    /// still pins the one-delta constants, so the fragment's inert
    /// judgement cannot depend on which delta carried the preamble or
    /// the span's delimiters.
    #[test]
    fn prose_prefix_invalid_result_fragment_in_code_span_split_across_deltas_matches_one_delta() {
        let prose = "The stored turn quoted `";
        let first_half = "{\"tool_call_id\": <call-123>, \"content\": <tool output>,";
        let second_half = " \"attachments\": [<attachment ref>]}";
        let closing = "` and that is the whole turn.";
        let message = format!("{prose}{first_half}{second_half}{closing}");
        let expected = (message.clone(), false, None);
        assert_eq!(
            guard_outcome(&[prose, first_half, second_half, closing]),
            expected,
            "split delivery must match the one-delta outcome"
        );
    }

    /// The same fragment with the preamble pushed on its own delta: the
    /// prose is delivered first, the fragment and the trailing prose
    /// follow in their own deltas, and the outcome must match the
    /// one-delta form, which delivers the whole message. The inert
    /// judgement is the fragment's, so it cannot depend on which delta
    /// carried the preamble.
    #[test]
    fn prose_prefix_invalid_result_fragment_split_after_prose_matches_one_delta() {
        let prose = "The stored turn quoted ";
        let fragment = "{\"tool_call_id\": <call-123>, \"content\": <tool output>, \"attachments\": [<attachment ref>]}";
        let trailing = " and that is the whole turn.";
        let message = format!("{prose}{fragment}{trailing}");
        let expected = (message.clone(), false, None);
        assert_eq!(
            guard_outcome(&[prose, fragment, trailing]),
            expected,
            "the split form must deliver the whole message like the one-delta form"
        );
        assert_eq!(
            guard_outcome(&[message.as_str()]),
            expected,
            "the one-delta form is the reference"
        );
    }

    /// Control: the same fragment carrying a key that names a
    /// registered tool is call-shaped, so it is not inert and is
    /// withheld at finish as malformed, with the preamble (the prose
    /// and the span's opening delimiter) delivered and the candidate
    /// starting at the object.
    #[test]
    fn prose_prefix_invalid_result_fragment_with_name_key_is_withheld_at_finish() {
        let preamble = "The stored turn quoted `";
        let fragment = "{\"tool_call_id\": <call-123>, \"content\": <tool output>, \"name\": \"shell\", \"attachments\": [<attachment ref>]}";
        let trailing = " and that is the whole turn.";
        let message = format!("{preamble}{fragment}`{trailing}");
        let expected = (
            preamble.to_string(),
            true,
            Some(ProtocolSuppressionDiagnostic {
                detector: "malformed",
                candidate_offset: preamble.len(),
            }),
        );
        assert_eq!(
            guard_outcome(&[message.as_str()]),
            expected,
            "a key naming a registered tool makes the fragment call-shaped, so it is withheld"
        );
    }

    /// Control: the same fragment leading the message, with no preamble
    /// at all, is withheld as malformed on the first delta and nothing
    /// is forwarded: a result shape the message opens with is not a
    /// quotation.
    #[test]
    fn leading_invalid_result_fragment_is_withheld_as_malformed() {
        let fragment = "{\"tool_call_id\": <call-123>, \"content\": <tool output>, \"attachments\": [<attachment ref>]}";
        let trailing = " and that is the whole turn.";
        let message = format!("{fragment}{trailing}");
        let expected = (
            String::new(),
            true,
            Some(ProtocolSuppressionDiagnostic {
                detector: "malformed",
                candidate_offset: 0,
            }),
        );
        assert_eq!(
            guard_outcome(&[message.as_str()]),
            expected,
            "a result-shaped fragment at offset zero is withheld outright"
        );
    }

    /// A prose-prefixed call object split inside is judged on the delta
    /// that closes it: while the root is still arriving the malformed
    /// signal defers (the candidate is buffered either way), and the
    /// completed object parses as JSON carrying protocol-only fields, so
    /// the verdict the complete object reaches is `malformed`, with the
    /// buffered preamble released by the suppression itself.
    #[test]
    fn prose_prefix_call_object_split_inside_is_judged_on_its_closing_delta() {
        let mut guard = guard_with_tool();
        let prefix = "The history message is ";
        let head = "{\"toolcalls\": [{\"call_id\": \"call_1\",";
        let closing = " \"arguments\": {\"command\": \"ls\"}}]}";
        let mut forwarded = String::new();
        forwarded.push_str(
            &guard
                .push(format!("{prefix}{head}").as_str())
                .unwrap_or_default(),
        );
        assert_eq!(
            forwarded, "",
            "the preamble buffers ahead of the candidate while the object arrives"
        );
        assert!(
            !guard.suppressed_protocol,
            "the incomplete call object defers while the stream runs"
        );
        forwarded.push_str(&guard.push(closing).unwrap_or_default());
        assert_eq!(
            forwarded, prefix,
            "the closing delta delivers the preamble and withholds the completed call object"
        );
        assert!(guard.suppressed_protocol);
        assert_eq!(
            guard.suppression,
            Some(ProtocolSuppressionDiagnostic {
                detector: "malformed",
                candidate_offset: prefix.len(),
            })
        );
        assert_eq!(guard.finish().as_deref(), None);
    }

    /// A call envelope inside a closed one-backtick code span after prose
    /// is withheld, not exempted: quoting call syntax in prose does not
    /// make it deliverable, which is deliberate. The opening backtick
    /// streams with the prose because the chunk that carries it seeds no
    /// candidate; the envelope's own delta is judged as a complete call
    /// container naming a registered tool, and everything from it onward,
    /// the closing backtick and the trailing prose included, is never
    /// delivered.
    #[test]
    fn quoted_call_envelope_in_code_span_is_withheld_with_prose_delivered() {
        let mut guard = guard_with_specs(&["shell", "count_tool"]);
        let prefix = "The call is `";
        let envelope = "{\"tool_calls\": [{\"function\": {\"name\": \"shell\", \"arguments\": {\"command\": \"ls\"}}}]}";
        let forwarded = push_all(&mut guard, &[prefix, envelope, "` exactly."]);
        assert_eq!(
            forwarded, prefix,
            "the prose and the opening backtick are delivered, the envelope is not"
        );
        assert!(guard.suppressed_protocol);
        assert_eq!(
            guard.suppression,
            Some(ProtocolSuppressionDiagnostic {
                detector: "function_call",
                candidate_offset: prefix.len(),
            })
        );
    }

    /// A json fence carrying a call envelope, with prose around it, is
    /// withheld from the fence onward, not exempted, which is deliberate.
    /// The fence candidate waits for the end of the stream, and at finish
    /// the malformed signal (a call container, an arguments key and a
    /// name naming a registered tool, in text that does not parse)
    /// withholds it, with the prose ahead of the fence delivered. The
    /// trailing prose after the closing fence line is never delivered.
    #[test]
    fn fenced_call_envelope_with_surrounding_prose_is_withheld() {
        let mut guard = guard_with_tool();
        let prefix = "The wire shape is:\n";
        let forwarded = push_all(
            &mut guard,
            &[
                "The wire shape is:\n```json\n{\"tool_calls\": [{\"function\": {\"name\": \"shell\",",
                " \"arguments\": {\"command\": \"ls\"}}}]}\n```\nand nothing else carries protocol.",
            ],
        );
        assert_eq!(
            forwarded, prefix,
            "the prose ahead of the fence is delivered, the fence and everything after it is not"
        );
        assert!(guard.suppressed_protocol);
        assert_eq!(
            guard.suppression,
            Some(ProtocolSuppressionDiagnostic {
                detector: "malformed",
                candidate_offset: prefix.len(),
            })
        );
    }

    /// A result-shaped fragment quoted inside a json-labelled fence after
    /// a preamble, one delta and split alike: the fenced shape is a
    /// quotation, so the whole message is delivered, the same judgement
    /// the bare fragment after a preamble earns.
    #[test]
    fn prose_prefix_json_fenced_invalid_result_fragment_matches_one_delta() {
        let prose = "The stored turn quoted this:\n";
        let fragment = "{\"tool_call_id\": <call-123>, \"content\": <tool output>, \"attachments\": [<attachment ref>]}";
        let trailing = "\nand that is the whole turn.";
        let message = format!("{prose}```json\n{fragment}\n```{trailing}");
        let expected = (message.clone(), false, None);
        assert_eq!(
            guard_outcome(&[message.as_str()]),
            expected,
            "the one-delta form delivers the fenced quotation whole"
        );
        assert_eq!(
            guard_outcome(&[prose, "```json\n", fragment, "\n```", trailing]),
            expected,
            "the split form must deliver the fenced quotation like the one-delta form"
        );
    }

    /// A complete tool-result object quoted inside a json-labelled fence
    /// after a preamble, one delta and split alike: the fenced object is a
    /// quotation, so the whole message is delivered through the fence's
    /// close, like the bare object after a preamble.
    #[test]
    fn prose_prefix_json_fenced_tool_result_object_matches_one_delta() {
        let prose = "The stored turn quoted this:\n";
        let object = "{\"tool_call_id\": \"call_1\", \"content\": \"ok\"}";
        let trailing = "\nand that is the whole turn.";
        let message = format!("{prose}```json\n{object}\n```{trailing}");
        let expected = (message.clone(), false, None);
        assert_eq!(
            guard_outcome(&[message.as_str()]),
            expected,
            "the one-delta form delivers the fenced object whole"
        );
        assert_eq!(
            guard_outcome(&[prose, "```json\n", object, "\n```", trailing]),
            expected,
            "the split form must deliver the fenced object like the one-delta form"
        );
    }

    /// Control: the same fragment inside a plain fence with no language
    /// label, after a preamble, is delivered whole, one delta and split
    /// alike. An unlabelled fence never opens a json-labelled quotation,
    /// so the delivery rides the fragment's own inert judgement, as
    /// before.
    #[test]
    fn prose_prefix_plain_fenced_invalid_result_fragment_is_delivered() {
        let prose = "The stored turn quoted this:\n";
        let fragment = "{\"tool_call_id\": <call-123>, \"content\": <tool output>, \"attachments\": [<attachment ref>]}";
        let trailing = "\nand that is the whole turn.";
        let message = format!("{prose}```\n{fragment}\n```{trailing}");
        let expected = (message.clone(), false, None);
        assert_eq!(
            guard_outcome(&[message.as_str()]),
            expected,
            "the plain fence must not change the fragment's inert delivery"
        );
        assert_eq!(
            guard_outcome(&[prose, "```\n", fragment, "\n```", trailing]),
            expected,
            "the split form must match the one-delta delivery"
        );
    }

    /// Control: a json-labelled fence carrying a call container that names
    /// a registered tool, after a preamble, is withheld from the fence
    /// onward with the preamble delivered, as before: quoting a call
    /// shape does not make it deliverable.
    #[test]
    fn prose_prefix_json_fenced_call_envelope_is_withheld() {
        let prose = "The stored turn quoted this:\n";
        let envelope = "{\"tool_calls\": [{\"function\": {\"name\": \"shell\", \"arguments\": {\"command\": \"ls\"}}}]}";
        let trailing = "\nand that is the whole turn.";
        let message = format!("{prose}```json\n{envelope}\n```{trailing}");
        let expected = (
            prose.to_string(),
            true,
            Some(ProtocolSuppressionDiagnostic {
                detector: "malformed",
                candidate_offset: prose.len(),
            }),
        );
        assert_eq!(
            guard_outcome(&[message.as_str()]),
            expected,
            "a fenced call container is withheld with the preamble delivered"
        );
        assert_eq!(
            guard_outcome(&[prose, "```json\n", envelope, "\n```", trailing]),
            expected,
            "the split form must withhold the fenced call container the same way"
        );
    }

    /// Control: a json-labelled fence carrying the result-shaped fragment
    /// plus a key that names a registered tool, after a preamble, is
    /// withheld with the preamble delivered: a call-shaped key inside the
    /// fence keeps the body from being a deliverable quotation.
    #[test]
    fn prose_prefix_json_fenced_result_fragment_with_name_key_is_withheld() {
        let prose = "The stored turn quoted this:\n";
        let fragment = "{\"tool_call_id\": <call-123>, \"content\": <tool output>, \"name\": \"shell\", \"attachments\": [<attachment ref>]}";
        let trailing = "\nand that is the whole turn.";
        let message = format!("{prose}```json\n{fragment}\n```{trailing}");
        let expected = (
            prose.to_string(),
            true,
            Some(ProtocolSuppressionDiagnostic {
                detector: "malformed",
                candidate_offset: prose.len(),
            }),
        );
        assert_eq!(
            guard_outcome(&[message.as_str()]),
            expected,
            "a fenced fragment carrying a call-shaped key is withheld"
        );
        assert_eq!(
            guard_outcome(&[prose, "```json\n", fragment, "\n```", trailing]),
            expected,
            "the split form must withhold the named fragment the same way"
        );
    }

    /// Control: a json-labelled fence carrying a complete result object
    /// with no preamble ahead of it is withheld from the start of the
    /// message, as before: a result shape that leads the message is not a
    /// quotation.
    #[test]
    fn leading_json_fenced_tool_result_object_is_withheld() {
        let object = "{\"tool_call_id\": \"call_1\", \"content\": \"ok\"}";
        let trailing = "\nand that is the whole turn.";
        let message = format!("```json\n{object}\n```{trailing}");
        let expected = (
            String::new(),
            true,
            Some(ProtocolSuppressionDiagnostic {
                detector: "malformed",
                candidate_offset: 0,
            }),
        );
        assert_eq!(
            guard_outcome(&[message.as_str()]),
            expected,
            "a fenced result object leading the message is withheld outright"
        );
    }

    /// A json-labelled fence quoting a result object, prose after it,
    /// then a bare call container: the fenced quotation and the prose are
    /// delivered, and the container is withheld where it starts, one
    /// delta and a split after the fence's close agreeing.
    #[test]
    fn prose_prefix_json_fenced_result_object_still_withholds_later_call_envelope() {
        let prose = "The stored turn quoted this:\n";
        let object = "{\"tool_call_id\": \"call_1\", \"content\": \"ok\"}";
        let mid = " and the call is ";
        let envelope = "{\"tool_calls\": [{\"function\": {\"name\": \"shell\", \"arguments\": {\"command\": \"ls\"}}}]}";
        let fence = format!("```json\n{object}\n```");
        let message = format!("{prose}{fence}{mid}{envelope}");
        let expected = (
            format!("{prose}{fence}{mid}"),
            true,
            Some(ProtocolSuppressionDiagnostic {
                detector: "function_call",
                candidate_offset: prose.len() + fence.len() + mid.len(),
            }),
        );
        assert_eq!(
            guard_outcome(&[message.as_str()]),
            expected,
            "the one-delta form delivers through the fence and withholds the container"
        );
        let head = format!("{prose}{fence}");
        let tail = format!("{mid}{envelope}");
        assert_eq!(
            guard_outcome(&[head.as_str(), tail.as_str()]),
            expected,
            "the split after the fence's close must agree with the one-delta form"
        );
    }

    /// A json-labelled fence the stream never closes, after a preamble:
    /// at finish the body is judged on its own, so an inert result
    /// fragment is delivered whole with the fence around it, while a body
    /// carrying a key that names a registered tool is withheld with the
    /// preamble delivered.
    #[test]
    fn prose_prefix_unclosed_json_fence_result_body_judged_at_finish() {
        let prose = "The stored turn quoted this:\n";
        let fragment = "{\"tool_call_id\": <call-123>, \"content\": <tool output>, \"attachments\": [<attachment ref>]}";
        let named = "{\"tool_call_id\": <call-123>, \"content\": <tool output>, \"name\": \"shell\", \"attachments\": [<attachment ref>]}";
        let expected = (format!("{prose}```json\n{fragment}"), false, None);
        assert_eq!(
            guard_outcome(&[prose, "```json\n", fragment]),
            expected,
            "an unclosed fence around an inert fragment is delivered whole at finish"
        );
        assert_eq!(
            guard_outcome(&[format!("{prose}```json\n{fragment}").as_str()]),
            expected,
            "the one-delta form must match the split form"
        );
        let withheld = (
            prose.to_string(),
            true,
            Some(ProtocolSuppressionDiagnostic {
                detector: "malformed",
                candidate_offset: prose.len(),
            }),
        );
        assert_eq!(
            guard_outcome(&[format!("{prose}```json\n{named}").as_str()]),
            withheld,
            "an unclosed fence around a call-shaped body is withheld at finish"
        );
    }

    /// A json-labelled fence quoting a result object whose content string
    /// itself carries three backticks, then a bare call container after
    /// the fence: the close found inside the string ends the released
    /// span early, and the container behind it is still withheld, so a
    /// close chosen in the wrong place can move the split but never
    /// Invariant: a json-labelled fence quoting a result object whose
    /// content string itself carries three backticks, then a bare call
    /// container after the fence. The close the guard finds inside the
    /// quoted string ends the released span early, so the split falls in
    /// the middle of the quoted object, and the container behind the
    /// fence must still be withheld: a close chosen in the wrong place
    /// can move the split but never release call-shaped text.
    #[test]
    fn prose_prefix_json_fence_with_backticks_inside_content_still_withholds_later_call_envelope() {
        let prose = "The stored turn quoted this:\n";
        let object = "{\"tool_call_id\": \"call_1\", \"content\": \"nested ``` inside\"}";
        let envelope = "{\"tool_calls\": [{\"function\": {\"name\": \"shell\", \"arguments\": {\"command\": \"ls\"}}}]}";
        let message = format!("{prose}```json\n{object}\n```{envelope}");
        let (forwarded, suppressed, _) = guard_outcome(&[message.as_str()]);
        assert!(
            suppressed,
            "the turn must end suppressed with the container withheld"
        );
        assert!(
            !forwarded.contains(envelope),
            "a close found inside the quoted object must never release the container behind the fence"
        );
    }

    /// A flat object carrying the correlation and payload keys of a
    /// result plus the name and arguments keys of a call, after a
    /// preamble, the whole hybrid in one delta: the classifier calls it
    /// a tool result, but its top level names something the parser
    /// could execute, so it is withheld like the call it also shapes,
    /// with the preamble delivered.
    #[test]
    fn prose_prefix_hybrid_result_object_one_delta_is_withheld() {
        let prose = "The stored turn quoted this:\n";
        let hybrid = "{\"tool_call_id\": \"call_1\", \"content\": \"ok\", \"name\": \"shell\", \"arguments\": {\"command\": \"ls\"}}";
        let expected = (
            prose.to_string(),
            true,
            Some(ProtocolSuppressionDiagnostic {
                detector: "tool_result",
                candidate_offset: prose.len(),
            }),
        );
        assert_eq!(
            guard_outcome(&[prose, hybrid]),
            expected,
            "a result shape with call-shaped top-level keys is withheld after a preamble"
        );
    }

    /// The same hybrid split inside, judged on the delta that closes
    /// it: the split after the payload key, with the preamble released
    /// ahead or riding the head, matches the one-delta outcome, so the
    /// verdict never rests on the fragment alone.
    #[test]
    fn prose_prefix_hybrid_result_object_split_deltas_match_one_delta() {
        let prose = "The stored turn quoted this:\n";
        let head = "{\"tool_call_id\": \"call_1\", \"content\":";
        let tail = " \"ok\", \"name\": \"shell\", \"arguments\": {\"command\": \"ls\"}}";
        let expected = (
            prose.to_string(),
            true,
            Some(ProtocolSuppressionDiagnostic {
                detector: "tool_result",
                candidate_offset: prose.len(),
            }),
        );
        assert_eq!(
            guard_outcome(&[prose, head, tail]),
            expected,
            "the split after the payload key is judged when the object closes"
        );
        let head_delta = format!("{prose}{head}");
        assert_eq!(
            guard_outcome(&[head_delta.as_str(), tail]),
            expected,
            "the preamble riding the head must match the split form"
        );
    }

    /// The hybrid inside a json-labelled fence after a preamble, one
    /// delta and split alike: the fenced body is not a releasable
    /// result, so the fence opens no quotation and the whole fence is
    /// withheld like the call its body also shapes, with the preamble
    /// delivered and the candidate starting at the fence.
    #[test]
    fn prose_prefix_json_fenced_hybrid_result_object_is_withheld() {
        let prose = "The stored turn quoted this:\n";
        let hybrid = "{\"tool_call_id\": \"call_1\", \"content\": \"ok\", \"name\": \"shell\", \"arguments\": {\"command\": \"ls\"}}";
        let fence = format!("```json\n{hybrid}\n```");
        let expected = (
            prose.to_string(),
            true,
            Some(ProtocolSuppressionDiagnostic {
                detector: "tool_result",
                candidate_offset: prose.len(),
            }),
        );
        assert_eq!(
            guard_outcome(&[format!("{prose}{fence}").as_str()]),
            expected,
            "the one-delta form withholds the fenced hybrid at the fence"
        );
        assert_eq!(
            guard_outcome(&[prose, "```json\n", hybrid, "\n```"]),
            expected,
            "the split form must match the one-delta outcome"
        );
    }

    /// Control: a result object whose payload array holds an object
    /// with a name key (the name sits inside the array, not at the top
    /// level) is a releasable result, delivered whole after a preamble,
    /// bare and fenced, one delta and split alike, nothing withheld.
    #[test]
    fn prose_prefix_result_object_with_nested_name_is_delivered_bare_and_fenced() {
        let prose = "The stored turn quoted this:\n";
        let nested = "{\"tool_call_id\": \"call_1\", \"content\": [{\"name\": \"shell\"}]}";
        let fence = format!("```json\n{nested}\n```");
        let bare = (format!("{prose}{nested}"), false, None);
        assert_eq!(
            guard_outcome(&[prose, nested]),
            bare,
            "the bare form delivers the nested-name result whole"
        );
        assert_eq!(
            guard_outcome(&[format!("{prose}{nested}").as_str()]),
            bare,
            "the one-delta bare form must match the split form"
        );
        let fenced = (format!("{prose}{fence}"), false, None);
        assert_eq!(
            guard_outcome(&[prose, "```json\n", nested, "\n```"]),
            fenced,
            "the fenced form delivers the nested-name result whole"
        );
        assert_eq!(
            guard_outcome(&[format!("{prose}{fence}").as_str()]),
            fenced,
            "the one-delta fenced form must match the split form"
        );
    }

    /// A plain result released after a preamble, then prose, then the
    /// hybrid in one delta: the released result leaves the guard
    /// re-scanning for later candidates, and the hybrid delta is
    /// judged on its own, withheld like the call it also shapes, with
    /// everything ahead of it delivered.
    #[test]
    fn result_object_then_prose_then_hybrid_one_delta_is_withheld() {
        let prose = "The stored turn quoted this:\n";
        let object = "{\"tool_call_id\": \"call_1\", \"content\": \"ok\"}";
        let mid = " and the call is ";
        let hybrid = "{\"tool_call_id\": \"call_1\", \"content\": \"ok\", \"name\": \"shell\", \"arguments\": {\"command\": \"ls\"}}";
        let expected = (
            format!("{prose}{object}{mid}"),
            true,
            Some(ProtocolSuppressionDiagnostic {
                detector: "tool_result",
                candidate_offset: prose.len() + object.len() + mid.len(),
            }),
        );
        assert_eq!(
            guard_outcome(&[prose, object, mid, hybrid]),
            expected,
            "the hybrid after a released result and prose is withheld at its own offset"
        );
    }

    /// Regression for review finding B1: a json-labelled fence after a
    /// preamble whose body is the reviewer's result shape — the
    /// correlation key and a payload carrying tagged tool-call markup —
    /// is not a quotation the fence may release. The fence branch runs
    /// ahead of the tagged detector, so the release itself must apply
    /// the same tagged rejection through the shared predicate: the
    /// preamble is delivered, the fence is withheld from its opening
    /// backticks, and the detector is `tagged`.
    #[test]
    fn prose_prefix_json_fenced_tagged_result_body_is_withheld_as_tagged() {
        let prose = "The stored turn quoted this:\n";
        let invoke = [
            "<inv",
            "oke name=\"",
            "sh",
            "ell\">",
            "<para",
            "meter name=\"",
            "com",
            "mand\">",
            "pwd</para",
            "meter></inv",
            "oke>",
        ]
        .concat();
        let body = format!("{{\"tool_call_id\": <call-123>, \"content\": {invoke}}}");
        let message = format!("{prose}```json\n{body}\n```");
        let expected = (
            prose.to_string(),
            true,
            Some(ProtocolSuppressionDiagnostic {
                detector: "tagged",
                candidate_offset: prose.len(),
            }),
        );
        assert_eq!(
            guard_outcome(&[message.as_str()]),
            expected,
            "the fenced tagged body must be withheld as tagged, the preamble delivered"
        );
    }

    /// The same fenced tagged body across split deltas: the boundary
    /// lands inside the opener line (after the complete `json` label,
    /// before its newline — a boundary between the label's own
    /// characters releases the label head as ordinary text before any
    /// candidate exists, because the candidate finders match complete
    /// patterns only, the same before and after this change), inside
    /// the `<invoke` markup, and just before the closing backticks.
    /// Every split must reach the one-delta outcome: preamble
    /// delivered, fence withheld, detector `tagged`.
    #[test]
    fn prose_prefix_json_fenced_tagged_result_body_split_deltas_match_one_delta() {
        let prose = "The stored turn quoted this:\n";
        let invoke = [
            "<inv",
            "oke name=\"",
            "sh",
            "ell\">",
            "<para",
            "meter name=\"",
            "com",
            "mand\">",
            "pwd</para",
            "meter></inv",
            "oke>",
        ]
        .concat();
        let body = format!("{{\"tool_call_id\": <call-123>, \"content\": {invoke}}}");
        let expected = (
            prose.to_string(),
            true,
            Some(ProtocolSuppressionDiagnostic {
                detector: "tagged",
                candidate_offset: prose.len(),
            }),
        );
        assert_eq!(
            guard_outcome(&[
                format!("{prose}```json").as_str(),
                format!("\n{body}\n```").as_str(),
            ]),
            expected,
            "the split inside the opener line must match the one-delta outcome"
        );
        let invoke_at = body.find("<inv").expect("the body carries the invoke");
        let (head, tail) = body.split_at(invoke_at);
        assert_eq!(
            guard_outcome(&[
                format!("{prose}```json\n{head}").as_str(),
                format!("{tail}\n```").as_str(),
            ]),
            expected,
            "the split inside the invoke markup must match the one-delta outcome"
        );
        assert_eq!(
            guard_outcome(&[format!("{prose}```json\n{body}\n").as_str(), "```",]),
            expected,
            "the split just before the closing backticks must match the one-delta outcome"
        );
    }

    /// The unclosed-fence finish path of the same tagged body: at
    /// `finish` the fence branch judges the rest of the candidate as
    /// the body, and the shared gate withholds it there too, so the
    /// verdict falls through to the tagged detector. The preamble is
    /// delivered and the fence withheld as `tagged`, split and
    /// one-delta alike — the closed-fence outcome.
    #[test]
    fn prose_prefix_unclosed_json_fence_tagged_body_withheld_at_finish() {
        let prose = "The stored turn quoted this:\n";
        let invoke = [
            "<inv",
            "oke name=\"",
            "sh",
            "ell\">",
            "<para",
            "meter name=\"",
            "com",
            "mand\">",
            "pwd</para",
            "meter></inv",
            "oke>",
        ]
        .concat();
        let body = format!("{{\"tool_call_id\": <call-123>, \"content\": {invoke}}}");
        let expected = (
            prose.to_string(),
            true,
            Some(ProtocolSuppressionDiagnostic {
                detector: "tagged",
                candidate_offset: prose.len(),
            }),
        );
        assert_eq!(
            guard_outcome(&[prose, format!("```json\n{body}").as_str()]),
            expected,
            "the unclosed fence around the tagged body is withheld at finish"
        );
        assert_eq!(
            guard_outcome(&[format!("{prose}```json\n{body}").as_str()]),
            expected,
            "the one-delta form must match the split form"
        );
    }

    /// The bare-result form of the same tagged payload: a valid result
    /// object whose content string carries parseable tagged-call
    /// markup (single-quoted invoke attributes, so the markup survives
    /// JSON quoting and the parser can still read it), followed by a
    /// second bare result. The tagged rejection fires on the candidate
    /// before any release — and the release sites share the same gate,
    /// so no bare-release path can release the result and re-judge the
    /// rest: the result is withheld from its own opening brace, the
    /// preamble and anything before it are delivered, and everything
    /// from the tagged-carrying result on is withheld.
    #[test]
    fn prose_prefix_bare_tagged_content_result_then_second_result_withholds_from_first() {
        let prose = "The history messages are ";
        let invoke = [
            "<inv",
            "oke name='sh",
            "ell'>",
            "<para",
            "meter name='com",
            "mand'>",
            "pwd</para",
            "meter></inv",
            "oke>",
        ]
        .concat();
        let tagged = format!("{{\"tool_call_id\": \"call_1\", \"content\": \"{invoke}\"}}");
        let second = "{\"tool_call_id\": \"call_2\", \"content\": \"second\"}";
        let expected = (
            prose.to_string(),
            true,
            Some(ProtocolSuppressionDiagnostic {
                detector: "tagged",
                candidate_offset: prose.len(),
            }),
        );
        assert_eq!(
            guard_outcome(&[prose, tagged.as_str(), second]),
            expected,
            "the tagged-carrying result is withheld from its start, the second result after it too"
        );
        assert_eq!(
            guard_outcome(&[prose, format!("{tagged}{second}").as_str()]),
            expected,
            "the pair in one delta must match the split form"
        );
    }

    /// Positive control for the fence gate: the reviewer's shape with
    /// the invoke replaced by a plain placeholder is an inert result
    /// fragment, so the fenced quotation is delivered byte for byte,
    /// one delta and split alike — the gate withholds tagged payloads
    /// only.
    #[test]
    fn prose_prefix_json_fenced_inert_result_fragment_placeholder_is_delivered() {
        let prose = "The stored turn quoted this:\n";
        let fragment = "{\"tool_call_id\": <call-123>, \"content\": <text>}";
        let trailing = "\nand that is the whole turn.";
        let message = format!("{prose}```json\n{fragment}\n```{trailing}");
        let expected = (message.clone(), false, None);
        assert_eq!(
            guard_outcome(&[message.as_str()]),
            expected,
            "the one-delta form delivers the fenced inert fragment whole"
        );
        assert_eq!(
            guard_outcome(&[prose, "```json\n", fragment, "\n```", trailing]),
            expected,
            "the split form must deliver the fenced inert fragment like the one-delta form"
        );
    }

    /// Positive control at finish: the same inert fragment inside an
    /// unclosed json fence is delivered whole when the stream ends —
    /// the gate never withholds a fragment with no tagged markup.
    #[test]
    fn prose_prefix_unclosed_json_fence_inert_result_fragment_delivered_at_finish() {
        let prose = "The stored turn quoted this:\n";
        let fragment = "{\"tool_call_id\": <call-123>, \"content\": <text>}";
        let message = format!("{prose}```json\n{fragment}");
        let expected = (message.clone(), false, None);
        assert_eq!(
            guard_outcome(&[prose, format!("```json\n{fragment}").as_str()]),
            expected,
            "the unclosed fence around an inert fragment is delivered whole at finish"
        );
        assert_eq!(
            guard_outcome(&[message.as_str()]),
            expected,
            "the one-delta form must match the split form"
        );
    }

    /// Regression for review finding B2: prose followed by two
    /// thousand quoted-result fences in one delta. The fence release
    /// used to re-enter `evaluate_pending` per fence, whose fence
    /// branch called `release_through` again — one nested pair per
    /// fence, deep enough to overflow the test's 512 KiB stack. The
    /// re-scan now loops: every fence drains through the release loop,
    /// the delivered output equals the input byte for byte, and nothing
    /// is flagged. The stack leaves room for one release predicate call
    /// per fence (it runs the full tool-call parser, whose debug frames
    /// exceeded 128 KiB on Linux CI) while a nesting of two thousand
    /// still overflows it.
    #[test]
    fn two_thousand_quoted_result_fences_in_one_delta_release_without_recursion() {
        let prose = "The history messages are ";
        let fence = "```json\n{\"tool_call_id\": \"call_1\", \"content\": \"ok\"}\n```";
        let mut input = format!("{prose}{fence}");
        for _ in 1..2000 {
            input.push(' ');
            input.push_str(fence);
        }
        let assert_input = input.clone();
        let moved = input;
        let handle = std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || {
                let mut guard = guard_with_tool();
                let forwarded = push_all(&mut guard, &[moved.as_str()]);
                (forwarded, guard.suppressed_protocol)
            })
            .expect("the small-stack thread must spawn");
        let (forwarded, suppressed) = handle.join().expect("the loop must not overflow the stack");
        assert_eq!(
            forwarded, assert_input,
            "every fence is delivered byte for byte"
        );
        assert!(
            !suppressed,
            "two thousand clean fences must not flag anything"
        );
    }

    /// Control for the fence loop: the same two thousand fences with a
    /// known-tool call envelope behind the last one. The loop drains
    /// every fence, the re-scan seeds the envelope as the next
    /// candidate, and the detector withholds it at its own offset: the
    /// delivered text is exactly the preamble and the fences, the
    /// envelope is withheld, and the diagnostic records the byte offset
    /// of the envelope's first byte in the concatenated input.
    #[test]
    fn two_thousand_quoted_result_fences_then_known_tool_envelope_withholds_envelope() {
        let prose = "The history messages are ";
        let fence = "```json\n{\"tool_call_id\": \"call_1\", \"content\": \"ok\"}\n```";
        let envelope = "{\"tool_calls\": [{\"function\": {\"name\": \"shell\", \"arguments\": {\"command\": \"ls\"}}}]}";
        let mut delivered = format!("{prose}{fence}");
        for _ in 1..2000 {
            delivered.push(' ');
            delivered.push_str(fence);
        }
        let expected_offset = delivered.len() + 1;
        let input = format!("{delivered} {envelope}");
        // The space before the envelope is prose ahead of the withheld
        // candidate: the suppression itself releases it, so the
        // delivered text carries the trailing space.
        let assert_delivered = input[..expected_offset].to_string();
        let moved = input;
        let handle = std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || {
                let mut guard = guard_with_tool();
                let forwarded = push_all(&mut guard, &[moved.as_str()]);
                (forwarded, guard.suppressed_protocol, guard.suppression)
            })
            .expect("the small-stack thread must spawn");
        let (forwarded, suppressed, suppression) =
            handle.join().expect("the loop must not overflow the stack");
        assert_eq!(
            forwarded, assert_delivered,
            "the preamble and every fence are delivered, the envelope is not"
        );
        assert!(
            suppressed,
            "the known-tool envelope behind the fences is withheld"
        );
        assert_eq!(
            suppression,
            Some(ProtocolSuppressionDiagnostic {
                detector: "function_call",
                candidate_offset: expected_offset,
            })
        );
    }

    /// The untagged call forms `parse_tool_calls` reads, each inside a
    /// result fragment that never parses (review 5369095801): a GLM-style
    /// `tool/param>value` line and a plain `TOOL_CALL` block. Neither
    /// carries a tag marker, so the tagged check does not see them.
    const UNTAGGED_CALL_FRAGMENTS: [(&str, &str); 2] = [
        (
            "glm",
            "{\"tool_call_id\":\"call_1\",\"content\":\nshell/command>printf guard_probe\n}",
        ),
        (
            "TOOL_CALL",
            "{\"tool_call_id\":\"call_1\",\"content\":\nTOOL_CALL { tool => \"shell\", args => { --command \"pwd\" }} /TOOL_CALL\n}",
        ),
    ];

    /// The base outcome for these fragments: the preamble is delivered
    /// and the malformed candidate is withheld from its first byte.
    fn untagged_withheld_after(
        prose: &str,
    ) -> (String, bool, Option<ProtocolSuppressionDiagnostic>) {
        (
            prose.to_string(),
            true,
            Some(ProtocolSuppressionDiagnostic {
                detector: "malformed",
                candidate_offset: prose.len(),
            }),
        )
    }

    /// Regression for review 5369095801: a closed json fence quoting a
    /// result fragment that carries an untagged call. The fence release
    /// used to call the fragment inert and deliver it; the parser reads
    /// the call, so it is withheld, one delta and split alike.
    #[test]
    fn prose_prefix_json_fenced_untagged_call_fragment_is_withheld() {
        let prose = "Result shape:\n";
        for (label, fragment) in UNTAGGED_CALL_FRAGMENTS {
            let expected = untagged_withheld_after(prose);
            let (head, tail) = fragment.split_once('\n').expect("fragment spans lines");
            assert_eq!(
                guard_outcome(&[format!("{prose}```json\n{fragment}\n```").as_str()]),
                expected,
                "{label}: one delta"
            );
            assert_eq!(
                guard_outcome(&[prose, "```json\n", fragment, "\n```"]),
                expected,
                "{label}: fence and fragment in separate deltas"
            );
            assert_eq!(
                guard_outcome(&[prose, "```json\n", head, "\n", tail, "\n```"]),
                expected,
                "{label}: fragment split before the call line"
            );
        }
    }

    /// The same fragments in a json fence that never closes: judged at
    /// finish, withheld, one delta and split alike.
    #[test]
    fn prose_prefix_unclosed_json_fence_untagged_call_fragment_is_withheld() {
        let prose = "Result shape:\n";
        for (label, fragment) in UNTAGGED_CALL_FRAGMENTS {
            let expected = untagged_withheld_after(prose);
            assert_eq!(
                guard_outcome(&[format!("{prose}```json\n{fragment}").as_str()]),
                expected,
                "{label}: one delta"
            );
            assert_eq!(
                guard_outcome(&[prose, format!("```json\n{fragment}").as_str()]),
                expected,
                "{label}: preamble in an earlier delta"
            );
        }
    }

    /// The bare fragment after a preamble, which reaches the finish-time
    /// detector (preamble in the same delta) or the finish fallback
    /// (preamble in an earlier delta): both withhold it. The `TOOL_CALL`
    /// form is covered only when the fragment's first line arrives in a
    /// delta of its own: a delta carrying the whole unfenced fragment has
    /// its last brace inside the `args` block, so the candidate finder
    /// seeds nothing and the text is delivered as prose. That finder is
    /// unchanged from base, which delivers the same text.
    #[test]
    fn prose_prefix_bare_untagged_call_fragment_is_withheld_at_finish() {
        let prose = "Result shape: ";
        for (label, fragment) in UNTAGGED_CALL_FRAGMENTS {
            let expected = untagged_withheld_after(prose);
            let (head, tail) = fragment.split_once('\n').expect("fragment spans lines");
            assert_eq!(
                guard_outcome(&[prose, head, "\n", tail]),
                expected,
                "{label}: fragment split before the call line"
            );
            if label == "TOOL_CALL" {
                continue;
            }
            assert_eq!(
                guard_outcome(&[format!("{prose}{fragment}").as_str()]),
                expected,
                "{label}: one delta"
            );
            assert_eq!(
                guard_outcome(&[prose, fragment]),
                expected,
                "{label}: preamble in an earlier delta"
            );
        }
    }

    /// Positive control: a result fragment whose content has a path with
    /// a slash, which the GLM line parser does not read as a call (no
    /// `>` and no JSON after the slash), stays inert and is delivered
    /// byte for byte, fenced and bare, one delta and split.
    #[test]
    fn prose_prefix_result_fragment_with_path_content_is_still_delivered() {
        let prose = "Result shape:\n";
        let fragment = "{\"tool_call_id\":\"call_1\",\"content\":\nsrc/lib.rs line 4\n}";
        let fenced = format!("{prose}```json\n{fragment}\n```");
        assert_eq!(
            guard_outcome(&[fenced.as_str()]),
            (fenced.clone(), false, None),
            "fenced, one delta"
        );
        assert_eq!(
            guard_outcome(&[prose, "```json\n", fragment, "\n```"]),
            (fenced.clone(), false, None),
            "fenced, split"
        );
        let bare = format!("{prose}{fragment}");
        assert_eq!(
            guard_outcome(&[bare.as_str()]),
            (bare.clone(), false, None),
            "bare, one delta"
        );
        assert_eq!(
            guard_outcome(&[prose, fragment]),
            (bare.clone(), false, None),
            "bare, split"
        );
    }

    /// Review 5372714287, case 1: a complete quoted result whose content
    /// string opens a single-quoted invoke, with the closing tag in the
    /// next delta. Judged alone, the value parses no call and was
    /// released, and the bare closer that followed seeded no candidate,
    /// so the call reached the client in two pieces. The opener now keeps
    /// the value buffered; once the closer arrives the candidate parses
    /// as a tagged call and is withheld from its first byte, as when the
    /// value and closer share a delta.
    #[test]
    fn prose_prefix_result_opening_invoke_closed_in_next_delta_is_withheld() {
        let prose = "The history messages are ";
        let value = "{\"tool_call_id\": \"call_1\", \"content\": \"<invoke name='shell'><parameter name='command'>pwd</parameter>\"}";
        let closer = "</invoke>";
        let expected = (
            prose.to_string(),
            true,
            Some(ProtocolSuppressionDiagnostic {
                detector: "tagged",
                candidate_offset: prose.len(),
            }),
        );
        assert_eq!(
            guard_outcome(&[prose, value, closer]),
            expected,
            "split after the value"
        );
        assert_eq!(
            guard_outcome(&[prose, format!("{value}{closer}").as_str()]),
            expected,
            "value and closer in one delta"
        );
        // With the preamble in the same delta, the finder seeds the
        // candidate at the invoke rather than at the value's brace, so
        // the call is withheld from there and the result text ahead of it
        // is delivered: no call reaches the client either way.
        let invoke_at = prose.len() + value.find("<invoke").expect("value carries the invoke");
        assert_eq!(
            guard_outcome(&[format!("{prose}{value}{closer}").as_str()]),
            (
                format!("{prose}{}", &value[..invoke_at - prose.len()]),
                true,
                Some(ProtocolSuppressionDiagnostic {
                    detector: "tagged",
                    candidate_offset: invoke_at,
                }),
            ),
            "preamble, value and closer in one delta"
        );
    }

    /// Review 5372714287, case 2: a closed json fence whose invalid body
    /// opens an invoke, with the closing tag after the fence. The body
    /// alone parses no call and was released at the fence close; the
    /// opener now keeps the fence buffered, and the candidate with the
    /// closer is withheld as a tagged call, one delta and split alike.
    #[test]
    fn prose_prefix_json_fence_opening_invoke_closed_after_fence_is_withheld() {
        let prose = "Result shape:\n";
        let body = "{\"tool_call_id\":\"call_1\",\"content\": <invoke name=\"shell\"><parameter name=\"command\">pwd</parameter>";
        let expected = (
            prose.to_string(),
            true,
            Some(ProtocolSuppressionDiagnostic {
                detector: "tagged",
                candidate_offset: prose.len(),
            }),
        );
        assert_eq!(
            guard_outcome(&[format!("{prose}```json\n{body}\n```\n</invoke>").as_str()]),
            expected,
            "one delta"
        );
        assert_eq!(
            guard_outcome(&[prose, "```json\n", body, "\n```\n", "</invoke>"]),
            expected,
            "split at the fence close and before the closer"
        );
    }

    /// Review 5372714287, `TOOL_CALL` variant: a complete quoted result
    /// whose content ends in `TOOL_CALL {`, followed in the same delta by
    /// the rest of the block. The value is not released on its own, so
    /// the block never reaches the client; the preamble still does.
    #[test]
    fn prose_prefix_result_opening_tool_call_block_completed_after_value_is_withheld() {
        let prose = "The history messages are ";
        let rest = "{\"tool_call_id\": \"call_1\", \"content\": \"TOOL_CALL {\"} tool => \"shell\", args => { --command \"pwd\" }} /TOOL_CALL";
        let (forwarded, suppressed, suppression) = guard_outcome(&[prose, rest]);
        assert_eq!(forwarded, prose, "only the preamble is delivered");
        assert!(suppressed, "the completed block is withheld");
        assert_eq!(
            suppression.map(|diagnostic| diagnostic.candidate_offset),
            Some(prose.len()),
            "withheld from the quoted result's first byte"
        );
    }
}
