//! Streaming-text guards: protocol-fragment buffering and `<think>` tag stripping.

use super::protocol_detect::{
    complete_json_fence_protocol_state, complete_non_protocol_json,
    find_embedded_protocol_candidate_start, find_incomplete_protocol_candidate_start,
    json_fence_close_end, json_fence_has_trailing_text, longest_suffix_matching_prefix,
    starts_suspicious_protocol_prefix, starts_suspicious_tag_or_fence_prefix,
};
use std::collections::HashSet;
use zeroclaw_tool_call_parser::{
    TERMINAL_MARKERS, ToolProtocolEnvelopeKind, classify_tool_protocol_envelope,
    contains_tool_protocol_tag_call, looks_like_malformed_tool_protocol_envelope_for_known_tools,
    looks_like_tool_protocol_envelope, looks_like_tool_protocol_example,
    strip_trailing_terminal_markers, tool_protocol_envelope_mentions_known_tool,
};

/// Which guard detector suppressed a candidate and where the candidate
/// began, so a suppression can be diagnosed from the trace log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProtocolSuppressionDiagnostic {
    /// One of `tool_result`, `function_call`, `tagged`, `malformed`,
    /// `active_tool_json`.
    pub(crate) detector: &'static str,
    /// Byte offset of the candidate into the released-then-pending text.
    pub(crate) candidate_offset: usize,
}

#[derive(Debug, Default)]
pub(crate) struct StreamTextGuard {
    // Chunks can split `"toolcalls"` / `<tool_call>` and other protocol
    // shapes across deltas, so a chunk that may contain a candidate is
    // buffered whole (prose ahead of the candidate stays releasable) and
    // candidate text keeps accumulating once one is seeded. A quoted span
    // can hold part of this buffer until its closer arrives or the stream
    // finishes.
    pending: String,
    pending_candidate_start: Option<usize>,
    known_tool_names: HashSet<String>,
    has_active_tools: bool,
    // Text already delivered to the caller before the current candidate was
    // established, plus the inline-code and fence parity of that text so a
    // later candidate can tell whether it sits inside a code span. The raw
    // released text is deliberately not stored.
    released_bytes: usize,
    released_prose: bool,
    released_inline_code: bool,
    // Length of the backtick run that opened the currently open inline code
    // span in released text (0 when none is open): a closing run must match
    // it exactly.
    released_open_code_run: usize,
    released_in_fence: bool,
    released_backtick_run: usize,
    pub(crate) suppress_forwarding: bool,
    pub(crate) suppressed_protocol: bool,
    pub(crate) suppression: Option<ProtocolSuppressionDiagnostic>,
}

/// How the quoted region around a buffered candidate is bounded, so an
/// exemption for quoted protocol covers exactly the quotation and never
/// the text that follows its closing delimiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuoteRegion {
    /// The candidate is quoted material and the quotation ends at this byte
    /// offset in `pending`, just past its closing delimiter: the closing
    /// backtick run of an inline code span, or the real closing fence line
    /// of a json fence that carries text after it.
    Closed(usize),
    /// An inline code span is open ahead of the candidate and its closing
    /// run has not arrived: the candidate cannot be judged yet.
    Open,
    /// No code span or fence quotes the candidate.
    Unquoted,
}

/// Fold one completed backtick run into the code-span/fence parity: a run
/// of three or more backticks is a fence marker and toggles fence state,
/// while a run of one or two backticks opens an inline code span when none
/// is open and closes the open one only when its length matches the opening
/// run. A different-length run is literal content inside the span, and no
/// run toggles code parity inside a fence, where backticks are literal
/// content.
fn toggle_backtick_run(run: usize, in_code: &mut bool, open_run: &mut usize, in_fence: &mut bool) {
    if run >= 3 {
        *in_fence = !*in_fence;
    } else if !*in_fence {
        if !*in_code {
            *in_code = true;
            *open_run = run;
        } else if run == *open_run {
            *in_code = false;
            *open_run = 0;
        }
    }
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

        // A chunk carrying a full protocol pattern (a key, tag, or fence) is
        // evaluated as soon as it is buffered; a speculative seed from a
        // bare delimiter is not: the fragment alone can look malformed
        // while the completed shape is ordinary quoted prose, so it waits
        // for more candidate text.
        let mut evaluate_now = false;
        if self.pending.is_empty() && !starts_suspicious_protocol_prefix(chunk) {
            // Buffer the whole chunk with the candidate offset intact: the
            // prose ahead of the candidate is not protocol and stays
            // releasable if the candidate itself is suppressed or resolved.
            if let Some(start) = find_embedded_protocol_candidate_start(chunk) {
                self.pending_candidate_start = Some(start);
                self.pending.push_str(chunk);
                evaluate_now = true;
            } else if let Some(start) = find_incomplete_protocol_candidate_start(chunk) {
                self.pending_candidate_start = Some(start);
                self.pending.push_str(chunk);
            } else {
                self.note_released(chunk);
                return Some(chunk.to_string());
            }
        } else {
            self.pending.push_str(chunk);
            evaluate_now = true;
        }

        if !evaluate_now {
            return None;
        }
        self.evaluate_pending(false)
    }

    pub(crate) fn finish(&mut self) -> Option<String> {
        if self.suppress_forwarding || self.pending.is_empty() {
            return None;
        }
        let mut forwarded = String::new();
        // A quoted-span release re-scans its remainder into a new candidate,
        // so keep resolving while the buffer still holds decidable text.
        while !self.suppress_forwarding && !self.pending.is_empty() {
            let Some(text) = self.evaluate_pending(true) else {
                // A malformed verdict on the whole buffer can only apply to
                // a candidate the model is not quoting: a preamble ahead of
                // it does not make a call envelope legitimate, a closed
                // quotation already released through its closer, and an
                // unclosed opener is not a quote.
                if looks_like_malformed_tool_protocol_envelope_for_known_tools(
                    &self.pending,
                    &self.known_tool_names,
                ) && let Some(prefix) = self.suppress_protocol("malformed")
                {
                    forwarded.push_str(&prefix);
                }
                break;
            };
            forwarded.push_str(&text);
        }
        if self.suppressed_protocol || self.pending.is_empty() {
            return (!forwarded.is_empty()).then_some(forwarded);
        }
        let tail = std::mem::take(&mut self.pending);
        self.note_released(&tail);
        forwarded.push_str(&tail);
        Some(forwarded)
    }

    fn evaluate_pending(&mut self, finalizing: bool) -> Option<String> {
        let candidate_start = self.pending_candidate_start.unwrap_or(0);
        let candidate = self.pending.get(candidate_start..).unwrap_or(&self.pending);

        if !finalizing && starts_suspicious_tag_or_fence_prefix(candidate) {
            return None;
        }

        // Teaching examples are the parser's explicit exception, and tagged
        // tool-call markup is a machine directive that is withheld wherever
        // it appears: neither verdict depends on the quoting state, so both
        // are checked before a quoted region is resolved.
        if !looks_like_tool_protocol_example(candidate) {
            if contains_tool_protocol_tag_call(candidate) {
                return self.suppress_protocol("tagged");
            }
            if let Some(kind) = classify_tool_protocol_envelope(candidate)
                && matches!(kind, ToolProtocolEnvelopeKind::TaggedToolCall)
            {
                return self.suppress_protocol("tagged");
            }

            match self.candidate_quote_region(finalizing) {
                // The candidate is quoted material: deliver the text
                // through its closing delimiter and re-scan the remainder
                // like a fresh chunk, so a later unquoted envelope in that
                // remainder is still judged.
                QuoteRegion::Closed(boundary) => {
                    return self.release_through_quote(boundary, finalizing);
                }
                // An open span cannot be judged yet: keep buffering. At
                // finish the unclosed opener is not a quote, so the
                // detectors below then judge the candidate as unquoted.
                QuoteRegion::Open if !finalizing => return None,
                QuoteRegion::Open | QuoteRegion::Unquoted => {}
            }

            if let Some(detector) = self.protocol_suppression_detector(candidate) {
                return self.suppress_protocol(detector);
            }
        }

        if let Some(is_protocol) =
            complete_json_fence_protocol_state(candidate, &self.known_tool_names)
        {
            // A fence carrying a known-tool envelope is an internal protocol
            // leak unless the fence is quoted material: a preamble ahead of
            // it does not make a call envelope legitimate, but a code span
            // or surrounding text does. A fence with text after its real
            // close never reaches here; it already released through that
            // close as quoted material.
            if is_protocol && self.has_active_tools {
                return self.suppress_protocol("function_call");
            }
            self.pending_candidate_start = None;
            let release = std::mem::take(&mut self.pending);
            self.note_released(&release);
            return Some(release);
        }

        if complete_non_protocol_json(candidate, &self.known_tool_names) {
            self.pending_candidate_start = None;
            let release = std::mem::take(&mut self.pending);
            self.note_released(&release);
            return Some(release);
        }

        None
    }

    /// Text delivered to the caller before a later candidate appears: it is
    /// part of the message, not protocol, and it positions any later
    /// candidate past the start of the message. The backtick parity of the
    /// released text is folded into the running code-span/fence state so a
    /// later candidate can tell whether it sits inside an inline code span.
    fn note_released(&mut self, text: &str) {
        self.released_bytes += text.len();
        self.released_prose |= !text.trim().is_empty();
        let mut run = self.released_backtick_run;
        let mut in_code = self.released_inline_code;
        let mut open_run = self.released_open_code_run;
        let mut in_fence = self.released_in_fence;
        for ch in text.chars() {
            if ch == '`' {
                run += 1;
            } else if run > 0 {
                toggle_backtick_run(run, &mut in_code, &mut open_run, &mut in_fence);
                run = 0;
            }
        }
        self.released_backtick_run = run;
        self.released_inline_code = in_code;
        self.released_open_code_run = open_run;
        self.released_in_fence = in_fence;
    }

    /// A candidate is quoted (the model showing protocol to the reader, not
    /// emitting it) only when the inline code span that opened ahead of it
    /// CLOSES: the closer is a backtick run of the same length (one or two;
    /// runs of three or more are fence markers and never close an inline
    /// span) appearing at or after the candidate start. A json fence is
    /// quoted material only when its real closing fence line arrives with
    /// text after it, and the quoted region then ends at that close. An
    /// opener whose closer has not arrived leaves the candidate unquoted at
    /// finish, so an unclosed span can never exempt a leaked envelope.
    fn candidate_quote_region(&self, finalizing: bool) -> QuoteRegion {
        let candidate_start = self.pending_candidate_start.unwrap_or(0);
        let candidate = self.pending.get(candidate_start..).unwrap_or(&self.pending);
        // A json fence carrying text beyond its real closing fence line is
        // quoted material inside a larger message: the quotation ends at
        // that close, so the text after it is re-scanned rather than
        // exempted. A fence with no text after its close is the whole
        // candidate and is judged by the detectors like any other envelope.
        if json_fence_has_trailing_text(candidate) {
            // Trailing text implies a real closing fence line.
            let close_end = json_fence_close_end(candidate).unwrap_or(candidate.len());
            return QuoteRegion::Closed(candidate_start + close_end);
        }

        // Fold the released text and the buffered prefix ahead of the
        // candidate into the running code-span/fence parity; a run dangling
        // at the prefix edge resolves against the candidate's first
        // character, exactly as `note_released` folds it.
        let mut in_code = self.released_inline_code;
        let mut open_run = self.released_open_code_run;
        let mut in_fence = self.released_in_fence;
        let mut run = self.released_backtick_run;
        if let Some(prefix) = self.pending.get(..candidate_start) {
            for ch in prefix.chars() {
                if ch == '`' {
                    run += 1;
                } else if run > 0 {
                    toggle_backtick_run(run, &mut in_code, &mut open_run, &mut in_fence);
                    run = 0;
                }
            }
        }
        if run > 0
            && let Some(first) = candidate.chars().next()
            && first != '`'
        {
            toggle_backtick_run(run, &mut in_code, &mut open_run, &mut in_fence);
        }
        if !in_code {
            return QuoteRegion::Unquoted;
        }
        self.inline_closer_boundary(candidate_start, open_run, finalizing)
            .map_or(QuoteRegion::Open, QuoteRegion::Closed)
    }

    /// Byte offset into `pending` just past the backtick run that closes the
    /// open inline code span, when that run has arrived at or after the
    /// candidate start. The closer must be a completed run of exactly the
    /// opening length; a run still growing at the end of the buffer counts
    /// only once the stream is finishing.
    fn inline_closer_boundary(
        &self,
        candidate_start: usize,
        open_run: usize,
        finalizing: bool,
    ) -> Option<usize> {
        let scan = self.pending.get(candidate_start..).unwrap_or("");
        let mut run_len = 0usize;
        for (offset, ch) in scan.char_indices() {
            if ch == '`' {
                run_len += 1;
                continue;
            }
            if run_len == open_run {
                return Some(candidate_start + offset);
            }
            run_len = 0;
        }
        (run_len == open_run && finalizing).then_some(candidate_start + scan.len())
    }

    /// A candidate's quoted region closed at `boundary`, a byte offset into
    /// `pending` just past the closing delimiter. The text through the
    /// closer is prose plus a quotation, so deliver it, then re-scan the
    /// remainder exactly as `push` treats a fresh chunk: a later unquoted
    /// envelope in that remainder is still found and withheld, with its own
    /// prose prefix released. Another closed quotation in the remainder
    /// releases the same way, so the re-scan loops instead of recursing.
    fn release_through_quote(&mut self, mut boundary: usize, finalizing: bool) -> Option<String> {
        let mut release = String::new();
        loop {
            let head = self.pending[..boundary].to_string();
            self.pending.drain(..boundary);
            self.pending_candidate_start = None;
            self.note_released(&head);
            release.push_str(&head);
            // The release ends with the closing delimiter, which the parity
            // fold leaves as a dangling run: complete it against the known
            // boundary so the stored state reflects the closed span rather
            // than an open one.
            let closer_run = self.released_backtick_run;
            if closer_run > 0 {
                self.released_backtick_run = 0;
                toggle_backtick_run(
                    closer_run,
                    &mut self.released_inline_code,
                    &mut self.released_open_code_run,
                    &mut self.released_in_fence,
                );
            }

            // Re-scan the remainder with the same routing `push` applies to
            // a fresh chunk: release it outright, or seed a candidate. A
            // seeded candidate inside another closed quotation releases the
            // same way (the loop above); anything else is left to the
            // normal evaluation paths.
            if let Some(start) = find_embedded_protocol_candidate_start(&self.pending) {
                self.pending_candidate_start = Some(start);
                if let QuoteRegion::Closed(next) = self.candidate_quote_region(finalizing) {
                    boundary = next;
                    continue;
                }
                // A full pattern is evaluated immediately, exactly like
                // `push` treats a fresh chunk carrying one.
                if let Some(text) = self.evaluate_pending(finalizing) {
                    release.push_str(&text);
                }
                return Some(release);
            }
            if let Some(start) = find_incomplete_protocol_candidate_start(&self.pending) {
                // A speculative seed waits for more candidate text, exactly
                // like `push` treats a fresh chunk whose delimiter may yet
                // be ordinary prose JSON.
                self.pending_candidate_start = Some(start);
                return Some(release);
            }
            release.push_str(&self.pending);
            let tail = std::mem::take(&mut self.pending);
            self.note_released(&tail);
            return Some(release);
        }
    }

    /// A candidate has a prose prefix when prose was already released or is
    /// buffered ahead of it. A preamble makes a tool-result shape quoted
    /// prose (the model cannot legitimately emit a tool result), but it
    /// never legitimizes a tool-call envelope.
    fn candidate_has_prose_prefix(&self) -> bool {
        let candidate_start = self.pending_candidate_start.unwrap_or(0);
        self.released_prose
            || self
                .pending
                .get(..candidate_start)
                .is_some_and(|prefix| !prefix.trim().is_empty())
    }

    fn suppress_protocol(&mut self, detector: &'static str) -> Option<String> {
        let candidate_start = self.pending_candidate_start.unwrap_or(0);
        self.suppression = Some(ProtocolSuppressionDiagnostic {
            detector,
            candidate_offset: self.released_bytes + candidate_start,
        });
        // The text buffered ahead of the candidate is ordinary prose, not
        // protocol: deliver it and withhold from the candidate onward. Only
        // a candidate that starts at offset 0 of the pending buffer (or the
        // split-prefix buffer, which has no pre-candidate text) clears the
        // whole buffer.
        let release = (candidate_start > 0).then(|| self.pending[..candidate_start].to_string());
        self.pending.clear();
        self.pending_candidate_start = None;
        self.suppress_forwarding = true;
        self.suppressed_protocol = true;
        release
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

    /// Which detector (if any) marks `text` as an internal tool-protocol
    /// envelope that must be withheld from channel output.
    ///
    /// This runs only after the quoting state has been resolved: a candidate
    /// inside a closed code span, or a json fence with text after its real
    /// close, was already delivered as quoted prose, and an open span kept
    /// the guard buffering until its closer arrived or the stream finished.
    /// What reaches here is therefore unquoted: a preamble ahead of a
    /// tool-result shape still makes it quoted prose the model cannot
    /// legitimately emit, while call-shaped envelopes (malformed or valid,
    /// naming a known tool) are the ordinary non-native tool-call leak and
    /// are withheld even after a preamble. Once the classifier returns a
    /// verdict, that verdict alone decides: unclassified protocol-only JSON
    /// is judged by the envelope-shape and active-tool detectors.
    fn protocol_suppression_detector(&self, text: &str) -> Option<&'static str> {
        let prose_prefixed = self.candidate_has_prose_prefix();

        if looks_like_malformed_tool_protocol_envelope_for_known_tools(text, &self.known_tool_names)
        {
            return Some("malformed");
        }

        if let Some(kind) = classify_tool_protocol_envelope(text) {
            if matches!(kind, ToolProtocolEnvelopeKind::ToolResult) {
                // A tool result is never legitimate model output, so it is
                // withheld whenever it leads the message, whether or not
                // tool specs reached this guard (text-protocol turns hand
                // the guard none while the model still cannot emit results).
                if !prose_prefixed {
                    return Some("tool_result");
                }
                return None;
            }
            if self.has_active_tools
                && tool_protocol_envelope_mentions_known_tool(text, &self.known_tool_names)
            {
                return Some("function_call");
            }
            // A classified call-shaped envelope is judged solely by the
            // verdict above: without active tool specs it is the text-tool
            // channel, not a leak, and an unknown tool name is left to the
            // parse-issue detectors downstream.
            return None;
        }

        // Parsed JSON that carries protocol-only fields but cannot yield a valid
        // tool call is an internal protocol failure, not user-facing text.
        if looks_like_tool_protocol_envelope(text) {
            return Some("malformed");
        }

        if self.looks_like_active_tool_json(text) {
            return Some("active_tool_json");
        }

        None
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

    /// Regression for the issue: prose, then a code span quoting an object
    /// with the tool_call_id and content keys, then more prose, streamed so
    /// the split lands inside the object. The quoted snippet is prose, not
    /// a leaked envelope: every byte is forwarded and nothing is flagged.
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
    /// envelope: suppressed, with the detector and candidate offset recorded.
    #[test]
    fn whole_message_tool_result_object_is_suppressed() {
        let mut guard = guard_with_tool();
        let forwarded = push_all(
            &mut guard,
            &["{\"tool_call_id\": \"call_1\",", " \"content\": \"ok\"}"],
        );
        assert_eq!(forwarded, "");
        assert!(guard.suppressed_protocol);
        assert_eq!(
            guard.suppression,
            Some(ProtocolSuppressionDiagnostic {
                detector: "tool_result",
                candidate_offset: 0,
            })
        );
    }

    /// A fenced block containing the object, surrounded by prose that never
    /// uses an example word: the fence is not the whole message, so the
    /// quoted envelope is forwarded like any other prose.
    #[test]
    fn fenced_object_with_surrounding_prose_is_forwarded() {
        let mut guard = guard_with_tool();
        let message = "The wire shape is:\n```json\n{\"tool_call_id\": \"call_1\", \"content\": \"ok\"}\n```\nand nothing else carries protocol.";
        let forwarded = push_all(
            &mut guard,
            &[
                "The wire shape is:\n```json\n{\"tool_",
                "call_id\": \"call_1\", \"content\": \"ok\"}\n```\nand nothing else carries protocol.",
            ],
        );
        assert_eq!(forwarded, message);
        assert!(!guard.suppressed_protocol);
        assert!(guard.suppression.is_none());
    }

    /// A message that is nothing but a json fence whose body is the object
    /// stays suppressed: that is a whole-message envelope, not quoted prose.
    #[test]
    fn fence_only_message_with_object_body_is_suppressed() {
        let mut guard = guard_with_tool();
        let message = "```json\n{\"tool_call_id\": \"call_1\", \"content\": \"ok\"}\n```";
        let forwarded = push_all(&mut guard, &[message]);
        assert_eq!(forwarded, "");
        assert!(guard.suppressed_protocol);
        assert_eq!(
            guard.suppression,
            Some(ProtocolSuppressionDiagnostic {
                detector: "tool_result",
                candidate_offset: 0,
            })
        );
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

    /// The same known-tool call envelope inside an inline code span is the
    /// model quoting the protocol: forwarded byte-for-byte, nothing flagged.
    #[test]
    fn code_span_known_tool_envelope_is_forwarded() {
        let mut guard = guard_with_tool();
        let message = "The call is `{\"tool_calls\": [{\"function\": {\"name\": \"shell\", \"arguments\": {\"command\": \"ls\"}}}]}` exactly.";
        let forwarded = push_all(
            &mut guard,
            &[
                "The call is `",
                "{\"tool_calls\": [{\"function\": {\"name\": \"shell\", \"arguments\": {\"command\": \"ls\"}}}]}` exactly.",
            ],
        );
        assert_eq!(forwarded, message);
        assert!(!guard.suppressed_protocol);
        assert!(guard.suppression.is_none());
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

    /// A backtick with no matching closer never exempts a call envelope: at
    /// finish the unclosed opener is not a quote, so the envelope is
    /// withheld with the prose through the backtick delivered (the
    /// reattack's unmatched-opener escape).
    #[test]
    fn unclosed_code_span_opener_does_not_exempt_later_call_envelope() {
        let mut guard = guard_with_tool();
        let prefix = "The shell prompt shows `";
        let envelope = "{\"tool_calls\": [{\"function\": {\"name\": \"shell\", \"arguments\": {\"command\": \"ls\"}}}]}";
        let forwarded = push_all(&mut guard, &[prefix, envelope]);
        assert_eq!(
            forwarded, prefix,
            "the prose through the dangling backtick is delivered, the envelope is not"
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

    /// A closed inline quotation exempts only the quoted region: everything
    /// through the closing backtick plus the prose after it is delivered,
    /// and a later unquoted call envelope in the same buffered suffix is
    /// still withheld (the reattack's closed-quote escape).
    #[test]
    fn closed_code_span_quote_does_not_exempt_later_call_envelope() {
        let mut guard = guard_with_tool();
        let quoted =
            "The history message looks like `{\"tool_call_id\": \"call_1\", \"content\": \"ok\"}`";
        let prose = " and the tool call is ";
        let envelope = "{\"tool_calls\": [{\"function\": {\"name\": \"shell\", \"arguments\": {\"command\": \"ls\"}}}]}";
        let forwarded = push_all(
            &mut guard,
            &[
                "The history message looks like `{",
                "\"tool_call_id\": \"call_1\",",
                " \"content\": \"ok\"}",
                format!("`{prose}{envelope}").as_str(),
            ],
        );
        assert_eq!(
            forwarded,
            format!("{quoted}{prose}"),
            "the quotation and the prose after it are delivered, the later envelope is not"
        );
        assert!(guard.suppressed_protocol);
        assert_eq!(
            guard.suppression,
            Some(ProtocolSuppressionDiagnostic {
                detector: "function_call",
                candidate_offset: quoted.len() + prose.len(),
            })
        );
    }

    /// The same closed-quote shape with the closer arriving in a later chunk
    /// than the opener and the object: identical outcome, so chunk
    /// boundaries cannot reopen the leak.
    #[test]
    fn closed_code_span_quote_with_split_closer_still_bounded() {
        let mut guard = guard_with_tool();
        let quoted =
            "The history message looks like `{\"tool_call_id\": \"call_1\", \"content\": \"ok\"}`";
        let prose = " and the tool call is ";
        let envelope = "{\"tool_calls\": [{\"function\": {\"name\": \"shell\", \"arguments\": {\"command\": \"ls\"}}}]}";
        let forwarded = push_all(
            &mut guard,
            &[
                "The history message looks like `{",
                "\"tool_call_id\": \"call_1\", \"content\": \"ok\"}",
                "`",
                format!("{prose}{envelope}").as_str(),
            ],
        );
        assert_eq!(
            forwarded,
            format!("{quoted}{prose}"),
            "the quotation and the prose after it are delivered, the later envelope is not"
        );
        assert!(guard.suppressed_protocol);
        assert_eq!(
            guard.suppression,
            Some(ProtocolSuppressionDiagnostic {
                detector: "function_call",
                candidate_offset: quoted.len() + prose.len(),
            })
        );
    }

    /// A fence run never closes an inline span: a two-backtick opener
    /// followed by a three-backtick run leaves the span open, so a later
    /// call envelope is withheld at finish (the reattack's matrix row).
    #[test]
    fn fence_run_does_not_close_inline_code_span_opener() {
        let mut guard = guard_with_tool();
        let prefix = "Compare `` and ``` output: ";
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

    /// A triple-backtick run inside a JSON string is not a closing fence: an
    /// unterminated json fence carrying a known-tool call body is withheld
    /// at finish as malformed, not released as quoted material (the
    /// reattack's inner-backticks escape).
    #[test]
    fn unterminated_json_fence_with_inner_backticks_is_suppressed_as_malformed() {
        let mut guard = guard_with_tool();
        let message = "```json\n{\"tool_calls\": [{\"function\": {\"name\": \"shell\", \"arguments\": {\"command\": \"echo ```done\"}}}]}";
        let forwarded = push_all(&mut guard, &[message]);
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
    use super::{StreamTerminalMarkerStripper, StreamTextGuard};
    use std::collections::HashSet;
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

        // Guard parity on the same contract for the protocol guard: prose
        // quoting a tool-result-shaped object in a code span. The
        // non-streaming parse-issue detector raises nothing on this text,
        // so the streaming guard must deliver it byte-for-byte too.
        let known_tool_names = HashSet::from(["shell".to_string()]);
        let guard_tools = [crate::tools::ToolSpec::new(
            "shell",
            "run a command",
            serde_json::json!({"type": "object"}),
        )];
        let quoted = "The history message looks like `{\"tool_call_id\": \"call_1\", \"content\": \"ok\"}` and that is the whole shape.";
        assert!(
            super::super::protocol_detect::detect_tool_call_parse_issue_for_known_tools(
                quoted,
                &[],
                &known_tool_names
            )
            .is_none(),
            "the non-streaming detector must not flag quoted protocol prose"
        );
        let mut guard = StreamTextGuard::new(Some(&guard_tools));
        let live = guard.push(quoted);
        let flushed = guard.finish();
        let streamed = format!(
            "{}{}",
            live.unwrap_or_default(),
            flushed.unwrap_or_default()
        );
        assert_eq!(
            streamed, quoted,
            "the streaming guard must forward quoted protocol prose byte-for-byte"
        );
        assert!(!guard.suppressed_protocol);

        // Parity for a tool-result shape after a prose preamble (no code
        // span): the non-streaming detector raises nothing on the whole
        // text, so the streaming guard must deliver it too. A call envelope
        // after a preamble is deliberately NOT a parity case: the
        // non-streaming whole-text detector does not flag it, while the
        // streaming guard withholds it as a tool-call leak (pinned by the
        // dedicated suppression test above).
        let prefixed = "The history message is {\"tool_call_id\": \"call_1\", \"content\": \"ok\"}";
        assert!(
            super::super::protocol_detect::detect_tool_call_parse_issue_for_known_tools(
                prefixed,
                &[],
                &known_tool_names
            )
            .is_none(),
            "the non-streaming detector must not flag a prefixed tool-result quote"
        );
        let mut guard = StreamTextGuard::new(Some(&guard_tools));
        let live = guard.push(prefixed);
        let flushed = guard.finish();
        let streamed = format!(
            "{}{}",
            live.unwrap_or_default(),
            flushed.unwrap_or_default()
        );
        assert_eq!(
            streamed, prefixed,
            "the streaming guard must forward a tool-result quote after a preamble"
        );
        assert!(!guard.suppressed_protocol);
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
