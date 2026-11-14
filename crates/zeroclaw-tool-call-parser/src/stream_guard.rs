//! Streaming-text guards: protocol-fragment buffering and `<think>` tag stripping.

use crate::protocol_detect::{
    complete_json_fence_protocol_state, complete_non_protocol_json,
    find_embedded_protocol_candidate_start, find_incomplete_protocol_candidate_start,
    json_fence_body, longest_suffix_matching_prefix, starts_suspicious_protocol_prefix,
    starts_suspicious_tag_or_fence_prefix,
};
use crate::{
    TERMINAL_MARKERS, ToolProtocolEnvelopeKind, classify_tool_protocol_envelope,
    contains_parseable_tool_call, contains_tool_call_opener, contains_tool_protocol_tag_call,
    looks_like_malformed_tool_protocol_envelope_for_known_tools, looks_like_tool_protocol_envelope,
    looks_like_tool_protocol_example, strip_trailing_terminal_markers,
    tool_protocol_envelope_mentions_known_tool,
};
use std::collections::HashSet;

/// Streaming-safe terminal marker stripper.
///
/// Strips trailing terminal markers (`TERMINAL_MARKERS`) from streaming text
/// chunks. Handles markers split across multiple chunks, stacked markers, and
/// markers with arbitrary whitespace between them.
#[derive(Debug, Default)]
pub struct StreamTerminalMarkerStripper {
    pending: String,
}

/// Return the longest terminal-marker prefix that `text` ends with. This lets
/// the state machine hold a marker split across chunk boundaries until the
/// next chunk determines whether it is protocol metadata or inline text.
fn longest_terminal_marker_prefix(text: &str) -> Option<usize> {
    TERMINAL_MARKERS
        .iter()
        .flat_map(|marker| (1..marker.len()).map(move |len| &marker[..len]))
        .filter(|prefix| text.ends_with(prefix))
        .map(str::len)
        .max()
}

impl StreamTerminalMarkerStripper {
    pub fn new() -> Self {
        Self {
            pending: String::new(),
        }
    }

    /// Push a chunk and return visible text with terminal markers stripped.
    pub fn push(&mut self, chunk: &str) -> String {
        if chunk.is_empty() {
            return String::new();
        }

        self.pending.push_str(chunk);

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
                hold_start = before;
                break;
            }
            if hold_start == 0 {
                return String::new();
            }
        }

        if hold_start == self.pending.len() {
            return std::mem::take(&mut self.pending);
        }

        let result = self.pending[..hold_start].to_string();
        self.pending.drain(..hold_start);
        result
    }

    /// Finish the stream and return any remaining visible text.
    pub fn finish(&mut self) -> String {
        if self.pending.is_empty() {
            return String::new();
        }

        strip_trailing_terminal_markers(&self.pending)
    }
}

/// Which guard detector suppressed a candidate and where the candidate
/// began, so a suppression can be diagnosed from the trace log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolSuppressionDiagnostic {
    /// One of `tool_result`, `function_call`, `tagged`, `malformed`,
    /// `active_tool_json`.
    pub detector: &'static str,
    /// Byte offset of the candidate into the released-then-pending text:
    /// a UTF-8 byte offset into the model text after `<think>` tag and
    /// terminal-marker stripping (the text the guard itself receives on
    /// each push), not raw provider bytes and not a token index.
    pub candidate_offset: usize,
}

#[derive(Debug, Default)]
pub struct StreamTextGuard {
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
    pub suppress_forwarding: bool,
    pub suppressed_protocol: bool,
    pub suppression: Option<ProtocolSuppressionDiagnostic>,
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
/// leading JSON value, and which carries no call-shaped key and no call
/// the parser reads (tagged or untagged), names nothing the parser can
/// execute; after a prose preamble it is a quotation of the result
/// shape, delivered at finish. A candidate whose leading value
/// completed is the classifier's business, whatever text follows the
/// value. Leading (offset 0) fragments are not covered: a result shape
/// that leads the message is withheld, as before.
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
    // A fragment carrying any call the parser reads is not a harmless
    // quotation: tagged markup, and also the untagged GLM-style
    // `tool/param>value` lines and `TOOL_CALL` blocks, which carry no tag
    // marker for the tagged check to find. The finish-time detector and
    // fallback paths must not release it where the base guard withheld
    // the malformed candidate.
    !contains_parseable_tool_call(trimmed)
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
            .any(|item| item.as_object().is_some_and(object_has_key)),
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

/// The end of a json-labelled fence opening the candidate, as a byte
/// offset into the candidate just past the fence's close, together with
/// the trimmed body the fence wraps. The candidate must open with a
/// fence whose language line is `json`; whether that body is a shape the
/// guard may release as quoted prose is the caller's judgement — the
/// shared release predicate, which also applies the tagged rejection
/// `evaluate_pending` orders before any quoted-result release. The close
/// is the first three-backtick run after the opening line, so a run
/// inside the quoted body ends the span early: choosing the wrong close
/// can only move where a release splits the text, because the body test
/// runs on exactly the span the release would deliver, and that test
/// rejects a span opening a call whose closer could follow the close. A
/// fence with no close yet waits while the stream runs; at finish the
/// rest of the candidate is the body and the end is the candidate's end.
fn quoted_result_fence_end(candidate: &str, finalizing: bool) -> Option<(usize, &str)> {
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
    Some((end, body.trim()))
}

impl StreamTextGuard {
    pub fn new(available_tools: Option<&[String]>) -> Self {
        let available_tools = available_tools.unwrap_or(&[]);
        let known_tool_names = available_tools
            .iter()
            .map(|tool| tool.to_ascii_lowercase())
            .collect();
        Self {
            known_tool_names,
            has_active_tools: !available_tools.is_empty(),
            ..Self::default()
        }
    }

    pub fn push(&mut self, chunk: &str) -> Option<String> {
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

    pub fn finish(&mut self) -> Option<String> {
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
    /// 7. Open call: while streaming, a prose-prefixed candidate that
    ///    opens call syntax waits for more text instead of reaching the
    ///    release in step 8, so a closer in a later delta is judged with
    ///    its opener.
    /// 8. Complete-fence state and complete non-protocol JSON, on the
    ///    candidate itself.
    fn evaluate_pending(&mut self, finalizing: bool) -> Option<String> {
        let candidate_start = self.pending_candidate_start.unwrap_or(0);
        let candidate = self.pending.get(candidate_start..).unwrap_or(&self.pending);

        // A json-labelled fence quoting a result shape after a prose
        // preamble is a quotation like the bare shape: deliver the
        // preamble and the fence through its close, then re-scan the
        // remainder like a fresh chunk, so a second fence or a later
        // envelope behind the close is judged on its own. The quoted
        // body passes the same shared release predicate the bare result
        // path applies, so tagged-call markup inside the quoted shape is
        // withheld here exactly where the tagged rejection would
        // withhold it as the candidate itself.
        if self.candidate_has_prose_prefix()
            && let Some((end, body)) = quoted_result_fence_end(candidate, finalizing)
            && self.releases_as_quoted_result(candidate, body)
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
            // The completed value passes the same shared release
            // predicate the fenced body does, so tagged-call markup
            // riding in the value is withheld on this path too.
            if self.candidate_has_prose_prefix()
                && candidate.trim_start().starts_with('{')
                && let Some(end) = leading_complete_json_value_end(candidate)
                && self.releases_as_quoted_result(candidate, &candidate[..end])
            {
                return self.release_through(candidate_start + end, finalizing);
            }

            if let Some(detector) = self.protocol_suppression_detector(candidate, finalizing) {
                return self.suppress_protocol(detector);
            }

            // A prose-prefixed candidate the release predicate refused for
            // an open call (the closer has not arrived) must not leave
            // through the complete-JSON release below either: the closer
            // seeds no candidate of its own, so the call would reach the
            // client in two pieces. It waits for more text; at finish the
            // whole candidate is judged with whatever closer arrived.
            if self.candidate_has_prose_prefix()
                && !finalizing
                && contains_tool_call_opener(candidate)
            {
                return None;
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

    /// Whether `span` — the body a json-labelled fence wraps, or the
    /// completed result value a release would deliver inside `candidate`
    /// — may be released as quoted prose. Every site that releases a
    /// result shape after a preamble gates on this one predicate, so the
    /// tagged rejection `evaluate_pending` applies before its own
    /// quoted-result releases cannot be bypassed by another release path:
    /// the span is rejected when it carries a call the parser reads in
    /// any syntax, opens call syntax whose closer has not arrived, or
    /// classifies as a tagged call envelope, unless the whole candidate
    /// reads as a teaching example (the same exemption and order
    /// `evaluate_pending` applies). The opener rule matters because the
    /// span is judged alone: a call opened inside a quoted result can
    /// close in text after the span, and that closer seeds no candidate
    /// of its own, so releasing the span would deliver the call in two
    /// pieces. A rejected span stays buffered and is judged with what
    /// follows. Otherwise the span faces the result test the release
    /// paths already apply: a releasable result, or a result fragment
    /// that never parses and carries neither a call-shaped key nor a call
    /// the parser reads in any syntax.
    fn releases_as_quoted_result(&self, candidate: &str, span: &str) -> bool {
        if !looks_like_tool_protocol_example(candidate) {
            if contains_tool_call_opener(span) || contains_parseable_tool_call(span) {
                return false;
            }
            if let Some(kind) = classify_tool_protocol_envelope(span)
                && matches!(kind, ToolProtocolEnvelopeKind::TaggedToolCall)
            {
                return false;
            }
        }
        is_releasable_result(span) || is_inert_result_fragment(span)
    }

    /// Release through `boundary`, a byte offset into `pending` just past
    /// the closing brace of a completed tool-result value, or the close
    /// of a json-labelled fence quoting a result shape, each after a
    /// prose preamble. The text through the boundary is prose plus a
    /// quoted result, so deliver it, then re-scan the remainder exactly
    /// as `push` treats a fresh chunk: a later unquoted envelope in that
    /// remainder is still found and withheld, with its own prose prefix
    /// released. Another completed result or quoted-result fence behind a
    /// prose preamble in the remainder releases the same way, so the
    /// re-scan loops instead of recursing.
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
                // The same quoted-result fence test `evaluate_pending`
                // applies first: a prose-prefixed candidate opening with
                // a json fence whose body passes the shared release
                // predicate releases through the fence's close, so
                // consecutive quoted-result fences drain through this
                // loop instead of recursing one `evaluate_pending` into
                // `release_through` pair per fence.
                if self.candidate_has_prose_prefix()
                    && let Some((end, body)) = quoted_result_fence_end(candidate, finalizing)
                    && self.releases_as_quoted_result(candidate, body)
                {
                    boundary = start + end;
                    continue;
                }
                if self.candidate_has_prose_prefix()
                    && candidate.trim_start().starts_with('{')
                    && let Some(end) = leading_complete_json_value_end(candidate)
                    && self.releases_as_quoted_result(candidate, &candidate[..end])
                {
                    boundary = start + end;
                    continue;
                }
                // A full pattern is evaluated immediately, exactly like
                // `push` treats a fresh chunk carrying one.
                //
                // `evaluate_pending` on this candidate cannot reach a
                // branch that calls `release_through` again for it: the
                // loop has already tested the same predicates
                // (`candidate_has_prose_prefix` plus the shared release
                // predicate over the fence body or the completed leading
                // value, via the same finders' candidate start) on this
                // exact candidate with unchanged guard state, and both
                // returned false here. Its remaining outcomes — the
                // prefix wait, a suppression, or a whole-buffer release —
                // never re-enter `release_through`, so the re-scan stays
                // iterative no matter how many quoted shapes follow.
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
pub struct StreamThinkTagStripper {
    pending: String,
    in_think: bool,
}

impl StreamThinkTagStripper {
    const START_TAG: &'static str = "<think>";
    const END_TAG: &'static str = "</think>";

    pub fn push(&mut self, chunk: &str) -> String {
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

    pub fn finish(&mut self) -> String {
        if self.in_think {
            self.pending.clear();
            return String::new();
        }
        std::mem::take(&mut self.pending)
    }
}
