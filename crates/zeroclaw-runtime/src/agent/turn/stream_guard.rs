//! Streaming-text guards: protocol-fragment buffering and `<think>` tag stripping.

use super::protocol_detect::{
    complete_json_fence_protocol_state, complete_non_protocol_json,
    find_embedded_protocol_candidate_start, find_incomplete_protocol_candidate_start,
    json_fence_body, longest_suffix_matching_prefix, starts_suspicious_protocol_prefix,
    starts_suspicious_tag_or_fence_prefix,
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
    /// Byte offset of the candidate into the released-then-pending text:
    /// a UTF-8 byte offset into the model text after `<think>` tag and
    /// terminal-marker stripping (the text the guard itself receives on
    /// each push), not raw provider bytes and not a token index.
    pub(crate) candidate_offset: usize,
}

#[derive(Debug, Default)]
pub(crate) struct StreamTextGuard {
    // Chunks can split `"toolcalls"` / `<tool_call>` and other protocol
    // shapes across deltas, so a chunk that may contain a candidate is
    // buffered whole (prose ahead of the candidate stays releasable) and
    // candidate text keeps accumulating once one is seeded.
    pending: String,
    pending_candidate_start: Option<usize>,
    known_tool_names: HashSet<String>,
    has_active_tools: bool,
    // Text already delivered to the caller before the current candidate
    // was established: it is part of the message, not protocol, and it
    // positions any later candidate past the start of the message.
    released_bytes: usize,
    released_prose: bool,
    pub(crate) suppress_forwarding: bool,
    pub(crate) suppressed_protocol: bool,
    pub(crate) suppression: Option<ProtocolSuppressionDiagnostic>,
}

/// The byte offset just past the leading complete JSON value in `text`,
/// when `text` begins (after whitespace) with a value that has fully
/// arrived. `None` when there is no complete leading value: the text does
/// not start with one, or the first value is still being streamed.
fn leading_complete_json_value_end(text: &str) -> Option<usize> {
    let mut stream = serde_json::Deserializer::from_str(text).into_iter::<serde_json::Value>();
    match stream.next() {
        Some(Ok(_)) => Some(stream.byte_offset()),
        _ => None,
    }
}

/// The candidate opens with a JSON container whose root value has not
/// terminated: more of the value may still arrive, so a verdict that
/// rests on a substring signal alone cannot be final yet.
fn opens_with_unterminated_container(text: &str) -> bool {
    let trimmed = text.trim_start();
    (trimmed.starts_with('{') || trimmed.starts_with('['))
        && leading_complete_json_value_end(trimmed).is_none()
}

/// A result-shaped fragment whose root never parses as a complete
/// leading JSON value, and which carries no call-shaped key, names
/// nothing the parser can execute; after a prose preamble it is a
/// quotation of the result shape, delivered at finish. A candidate
/// whose leading value completed is the classifier's business, whatever
/// text follows the value. Leading (offset 0) fragments are not
/// covered: a result shape that leads the message is withheld, as
/// before.
fn is_inert_result_fragment(text: &str) -> bool {
    let trimmed = text.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return false;
    }
    // A root whose leading value completed is the completed-envelope
    // classifier's business: a released result shape is its judgement,
    // and a shape the classifier rejects stays withheld. The fragment
    // case is a root that never yields a complete leading value: cut
    // off mid-value, or syntactically broken so no prefix parses.
    if leading_complete_json_value_end(trimmed).is_some() {
        return false;
    }
    if !trimmed.contains("\"tool_call_id\"") {
        return false;
    }
    // A call-shaped key can name something the parser would execute.
    // The correlation key's result spelling embeds the shorter call-id
    // spelling, so the call-shaped keys are matched only after that
    // longer form is removed.
    let remainder = trimmed.replace("\"tool_call_id\"", "");
    for key in [
        "\"tool_calls\"",
        "\"toolcalls\"",
        "\"function_call\"",
        "\"arguments\"",
        "\"parameters\"",
        "\"name\"",
        "\"function\"",
        "\"call_id\"",
    ] {
        if remainder.contains(key) {
            return false;
        }
    }
    true
}

/// A parsed JSON value whose own top level carries one of the keys the
/// call-shaped envelopes use (or an array item's top level does): such
/// a value names something the parser might execute, so a result shape
/// carrying one is withheld like a call, not quoted like a result.
/// Top level only — a quoted result whose nested content mentions a
/// name field is still a result.
fn has_call_shaped_top_level_key(value: &serde_json::Value) -> bool {
    let keys = [
        "tool_calls",
        "toolcalls",
        "function_call",
        "arguments",
        "parameters",
        "name",
        "function",
        "call_id",
    ];
    let object_has_key = |object: &serde_json::Map<String, serde_json::Value>| {
        keys.iter().any(|key| object.contains_key(*key))
    };
    match value {
        serde_json::Value::Object(object) => object_has_key(object),
        serde_json::Value::Array(items) => items
            .iter()
            .any(|item| item.as_object().is_some_and(&object_has_key)),
        _ => false,
    }
}

/// A result-shaped value the guard may deliver as quoted prose after a
/// preamble: the classifier calls it a tool result, and no top-level
/// key of the parsed value — or of the body a json-labelled fence wraps
/// it in — is call-shaped. A flat object carrying both shapes is
/// withheld like the call it also shapes, so every release path and the
/// detector gate on this one check.
fn is_releasable_result(text: &str) -> bool {
    if !matches!(
        classify_tool_protocol_envelope(text),
        Some(ToolProtocolEnvelopeKind::ToolResult)
    ) {
        return false;
    }
    let trimmed = text.trim();
    let body = json_fence_body(trimmed).unwrap_or(trimmed);
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    !has_call_shaped_top_level_key(&value)
}

/// The end of a json-labelled fence quoting a result shape, as a byte
/// offset into the candidate just past the fence's close. The candidate
/// must open with a fence whose language line is `json`, and the fenced
/// body, judged on its own, must be a result shape: a complete envelope
/// the classifier calls a tool result whose top level carries no
/// call-shaped key, or an unparseable fragment that carries no
/// call-shaped key anywhere. The close is the first three-backtick run
/// after the opening line, so a run inside the quoted body ends the span
/// early: choosing the wrong close can only move where a release splits
/// the text, because the body test runs on exactly the span the release
/// would deliver, and call-shaped text never passes it. A fence with no
/// close yet waits while the stream runs; at finish the rest of the
/// candidate is the body and the end is the candidate's end.
fn quoted_result_fence_end(candidate: &str, finalizing: bool) -> Option<usize> {
    let rest = candidate.strip_prefix("```")?;
    let first_newline = rest.find("\n")?;
    let language = rest[..first_newline].trim().trim_end_matches("\r");
    if !language.eq_ignore_ascii_case("json") {
        return None;
    }
    let body_start = "```".len() + first_newline + 1;
    let body_with_close = &candidate[body_start..];
    let (body, end) = match body_with_close.find("```") {
        Some(close_start) => (
            &body_with_close[..close_start],
            body_start + close_start + "```".len(),
        ),
        None => {
            if !finalizing {
                return None;
            }
            (body_with_close, candidate.len())
        }
    };
    let body = body.trim();
    let result_shape = is_releasable_result(body) || is_inert_result_fragment(body);
    result_shape.then_some(end)
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
        // bare delimiter is not: the fragment alone can look like protocol
        // while the completed shape is ordinary prose, so it waits for more
        // candidate text.
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
        if self.suppress_forwarding {
            return None;
        }
        let mut forwarded = String::new();
        if !self.pending.is_empty() {
            // A result-shape release re-scans its remainder into a new
            // candidate, so keep resolving while the buffer still holds
            // decidable text.
            while !self.suppress_forwarding && !self.pending.is_empty() {
                let Some(text) = self.evaluate_pending(true) else {
                    // The fallback is the whole-buffer malformed verdict
                    // for a candidate the model is not quoting: a
                    // preamble ahead of it does not make a call envelope
                    // legitimate. An inert result fragment after a
                    // preamble was released by the detector, and the
                    // same judgement holds here, whichever delta carried
                    // the preamble.
                    let candidate_start = self.pending_candidate_start.unwrap_or(0);
                    let candidate = self.pending.get(candidate_start..).unwrap_or(&self.pending);
                    if !(self.candidate_has_prose_prefix() && is_inert_result_fragment(candidate))
                        && looks_like_malformed_tool_protocol_envelope_for_known_tools(
                            &self.pending,
                            &self.known_tool_names,
                        )
                        && let Some(prefix) = self.suppress_protocol("malformed")
                    {
                        forwarded.push_str(&prefix);
                    }
                    break;
                };
                forwarded.push_str(&text);
            }
            if !self.suppressed_protocol && !self.pending.is_empty() {
                let tail = std::mem::take(&mut self.pending);
                self.note_released(&tail);
                forwarded.push_str(&tail);
            }
        }
        (!forwarded.is_empty()).then_some(forwarded)
    }

    /// Judge the buffered candidate, returning released text when the
    /// candidate resolves as ordinary prose and leaving it buffered while
    /// no verdict can be reached. The order of the checks:
    ///
    /// 1. Quoted result fence: a prose-prefixed candidate opening with a
    ///    json-labelled fence whose body is a result shape is delivered
    ///    through the fence's close, and the remainder is re-seeded like
    ///    a fresh chunk.
    /// 2. Prefix wait: a candidate still opening with a tag or fence
    ///    prefix is left to accumulate until finish, when it is judged in
    ///    full.
    /// 3. Example: the parser's explicit teaching-example exception skips
    ///    every suppression check.
    /// 4. Tagged: tagged tool-call markup, a machine directive withheld
    ///    wherever it appears.
    /// 5. Result shape: a prose-prefixed candidate whose leading complete
    ///    JSON value is a tool result with no call-shaped top-level key
    ///    is delivered through the end of that value, and the remainder
    ///    is re-seeded like a fresh chunk.
    /// 6. Detectors: the parser-side suppression sequence, in which an
    ///    invalid-JSON result fragment after a preamble is inert and
    ///    released at finish.
    /// 7. Complete-fence state and complete non-protocol JSON, on the
    ///    candidate itself.
    fn evaluate_pending(&mut self, finalizing: bool) -> Option<String> {
        let candidate_start = self.pending_candidate_start.unwrap_or(0);
        let candidate = self.pending.get(candidate_start..).unwrap_or(&self.pending);

        // A json-labelled fence quoting a result shape after a prose
        // preamble is a quotation like the bare shape: deliver the
        // preamble and the fence through its close, then re-scan the
        // remainder like a fresh chunk, so a second fence or a later
        // envelope behind the close is judged on its own.
        if self.candidate_has_prose_prefix()
            && let Some(end) = quoted_result_fence_end(candidate, finalizing)
        {
            return self.release_through(candidate_start + end, finalizing);
        }

        if !finalizing && starts_suspicious_tag_or_fence_prefix(candidate) {
            return None;
        }

        if !looks_like_tool_protocol_example(candidate) {
            if contains_tool_protocol_tag_call(candidate) {
                return self.suppress_protocol("tagged");
            }
            if let Some(kind) = classify_tool_protocol_envelope(candidate)
                && matches!(kind, ToolProtocolEnvelopeKind::TaggedToolCall)
            {
                return self.suppress_protocol("tagged");
            }

            // A tool result with no call-shaped top-level key after a
            // prose preamble is quoted prose (the model cannot
            // legitimately emit a tool result): deliver the preamble plus
            // the completed object and re-scan the remainder like a fresh
            // chunk, so a later unquoted envelope in that remainder is
            // still withheld. An object that has not completed keeps
            // buffering, and at finish the detectors withhold it when it
            // carries a call-shaped key and release it when it does not.
            if self.candidate_has_prose_prefix()
                && candidate.trim_start().starts_with('{')
                && let Some(end) = leading_complete_json_value_end(candidate)
                && is_releasable_result(&candidate[..end])
            {
                return self.release_through(candidate_start + end, finalizing);
            }

            if let Some(detector) = self.protocol_suppression_detector(candidate, finalizing) {
                return self.suppress_protocol(detector);
            }
        }

        if let Some(is_protocol) =
            complete_json_fence_protocol_state(candidate, &self.known_tool_names)
        {
            if is_protocol && self.has_active_tools {
                return self.suppress_protocol("function_call");
            }
            return Some(self.release_pending());
        }

        if complete_non_protocol_json(candidate, &self.known_tool_names) {
            return Some(self.release_pending());
        }

        None
    }

    /// Release the whole pending buffer as ordinary text: the candidate
    /// turned out not to be protocol.
    fn release_pending(&mut self) -> String {
        self.pending_candidate_start = None;
        let release = std::mem::take(&mut self.pending);
        self.note_released(&release);
        release
    }

    /// Text delivered to the caller before a later candidate appears: it
    /// is part of the message, not protocol, and it positions any later
    /// candidate past the start of the message.
    fn note_released(&mut self, text: &str) {
        self.released_bytes += text.len();
        self.released_prose |= !text.trim().is_empty();
    }

    /// Release through `boundary`, a byte offset into `pending` just past
    /// the closing brace of a completed tool-result value that sits after
    /// a prose preamble. The text through the boundary is prose plus a
    /// quoted result, so deliver it, then re-scan the remainder exactly
    /// as `push` treats a fresh chunk: a later unquoted envelope in that
    /// remainder is still found and withheld, with its own prose prefix
    /// released. Another completed result behind a prose preamble in the
    /// remainder releases the same way, so the re-scan loops instead of
    /// recursing.
    fn release_through(&mut self, mut boundary: usize, finalizing: bool) -> Option<String> {
        let mut release = String::new();
        loop {
            let head = self.pending[..boundary].to_string();
            self.pending.drain(..boundary);
            self.pending_candidate_start = None;
            self.note_released(&head);
            release.push_str(&head);

            // Re-scan the remainder with the same routing `push` applies
            // to a fresh chunk: release it outright, or seed a candidate.
            if let Some(start) = find_embedded_protocol_candidate_start(&self.pending) {
                self.pending_candidate_start = Some(start);
                let candidate = self.pending.get(start..).unwrap_or(&self.pending);
                if self.candidate_has_prose_prefix()
                    && candidate.trim_start().starts_with('{')
                    && let Some(end) = leading_complete_json_value_end(candidate)
                    && is_releasable_result(&candidate[..end])
                {
                    boundary = start + end;
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
            // Plain text with no candidate: release it the way `push`
            // releases a candidate-free chunk.
            let tail = std::mem::take(&mut self.pending);
            self.note_released(&tail);
            release.push_str(&tail);
            return Some(release);
        }
    }

    /// A candidate has a prose prefix when prose was already released or
    /// is buffered ahead of it. A preamble makes a tool-result shape
    /// quoted prose (the model cannot legitimately emit a tool result),
    /// but it never legitimizes a tool-call envelope.
    fn candidate_has_prose_prefix(&self) -> bool {
        let candidate_start = self.pending_candidate_start.unwrap_or(0);
        self.released_prose
            || self
                .pending
                .get(..candidate_start)
                .is_some_and(|prefix| !prefix.trim().is_empty())
    }

    /// Withhold from the candidate onward: record which detector fired
    /// and where the candidate began, deliver the text buffered ahead of
    /// the candidate, and stop forwarding the rest of the stream. Only a
    /// candidate that starts at offset 0 of the pending buffer clears the
    /// whole buffer; text already delivered by `push` never sits in the
    /// buffer, so it is never delivered twice.
    fn suppress_protocol(&mut self, detector: &'static str) -> Option<String> {
        let candidate_start = self.pending_candidate_start.unwrap_or(0);
        self.suppression = Some(ProtocolSuppressionDiagnostic {
            detector,
            candidate_offset: self.released_bytes + candidate_start,
        });
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
    /// envelope that must be withheld from channel output: a malformed
    /// envelope, a classified envelope (a tool result leading the message,
    /// or a call-shaped envelope naming a registered tool), protocol-only
    /// JSON, or active-tool JSON. A preamble ahead of a tool-result shape
    /// makes it quoted prose the model cannot legitimately emit, while
    /// call-shaped envelopes (malformed or valid, naming a known tool) are
    /// the ordinary non-native tool-call leak and are withheld even after
    /// a preamble.
    fn protocol_suppression_detector(&self, text: &str, finalizing: bool) -> Option<&'static str> {
        let prose_prefixed = self.candidate_has_prose_prefix();

        if looks_like_malformed_tool_protocol_envelope_for_known_tools(text, &self.known_tool_names)
        {
            // A substring signal on a fragment cannot tell a leaking call
            // from an arriving quoted result: the object is buffered
            // either way, so the verdict waits for its close or the end of
            // the stream; a cut-off object with a call-shaped key is
            // withheld at finish, and one with none is released.
            if prose_prefixed && !finalizing && opens_with_unterminated_container(text) {
                return None;
            }
            // A result-shaped fragment whose root never completes,
            // with no call-shaped key, names nothing the parser can
            // execute, so after a preamble it is a quotation of the
            // result shape, delivered at finish. Past this detector the
            // candidate matches neither the complete-fence step nor the
            // complete-JSON step, and the finish fallback releases an
            // inert candidate on the same terms, so the outcome does
            // not depend on which delta carried the preamble.
            if prose_prefixed && finalizing && is_inert_result_fragment(text) {
                return None;
            }
            return Some("malformed");
        }

        if let Some(kind) = classify_tool_protocol_envelope(text) {
            if matches!(kind, ToolProtocolEnvelopeKind::ToolResult) {
                // A tool result is never legitimate model output, so it is
                // withheld whenever it leads the message, whether or not
                // tool specs reached this guard (text-protocol turns hand
                // the guard none while the model still cannot emit
                // results).
                if !prose_prefixed {
                    return Some("tool_result");
                }
                // A releasable result behind a preamble was already
                // delivered by the result-shape release (or is still
                // buffering); one whose top level carries a call-shaped
                // key names something the parser might execute, so it is
                // withheld like the call it also shapes.
                if is_releasable_result(text) {
                    return None;
                }
                return Some("tool_result");
            }
            if self.has_active_tools
                && tool_protocol_envelope_mentions_known_tool(text, &self.known_tool_names)
            {
                return Some("function_call");
            }
            // A classified call-shaped envelope is judged solely by the
            // verdict above: without active tool specs it is the text-tool
            // channel, not a leak, and a tool name that is unknown to the
            // registered set is left to the parse-issue detectors
            // downstream.
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
}
