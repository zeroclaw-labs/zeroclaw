//! Streaming-text guards: protocol-fragment buffering and `<think>` tag stripping.

use super::protocol_detect::{
    complete_json_fence_protocol_state, complete_non_protocol_json, contains_close_tag_marker,
    find_embedded_protocol_candidate_start, find_incomplete_protocol_candidate_start,
    first_protocol_envelope_end, longest_suffix_matching_prefix, starts_suspicious_protocol_prefix,
    starts_suspicious_tag_or_fence_prefix, suppressed_continuation_trailing,
    trailing_partial_close_fragment,
};
use std::collections::HashSet;
use zeroclaw_tool_call_parser::{
    MarkdownFenceTracker, ToolProtocolEnvelopeKind, classify_tool_protocol_envelope,
    contains_tool_protocol_tag_call, contains_truncated_ascii_dsml_envelope,
    contains_truncated_fullwidth_dsml_envelope,
    looks_like_malformed_tool_protocol_envelope_for_known_tools, looks_like_tool_protocol_envelope,
    looks_like_tool_protocol_example, tool_protocol_envelope_mentions_known_tool,
};

fn is_documentation_fence(fence: &MarkdownFenceTracker) -> bool {
    let Some((delim, _)) = fence.active() else {
        return false;
    };
    match delim {
        '~' => true,
        '`' => {
            let lower = fence.info().unwrap_or("").trim().to_ascii_lowercase();
            !(lower.starts_with("tool_call")
                || lower.starts_with("toolcall")
                || lower.starts_with("tool-call")
                || lower.starts_with("invoke")
                || lower.starts_with("json")
                || lower.starts_with("tool "))
        }
        _ => false,
    }
}

#[derive(Debug, Default)]
pub(crate) struct StreamTextGuard {
    // Suspicious leading chunks can split `"toolcalls"` / `<tool_call>` across
    // deltas. Buffer just that prefix until it is clearly protocol or normal JSON.
    pending: String,
    pending_candidate_start: Option<usize>,
    pending_partial_close: String,
    known_tool_names: HashSet<String>,
    has_active_tools: bool,
    pub(crate) suppress_forwarding: bool,
    pub(crate) suppressed_protocol: bool,
    fence: MarkdownFenceTracker,
    fence_pending: String,
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
        if self.fence_forward_documentation(chunk) {
            return Some(chunk.to_string());
        }

        let mut chunk = chunk.to_string();
        if !self.pending_partial_close.is_empty() {
            self.pending_partial_close.push_str(&chunk);
            chunk = std::mem::take(&mut self.pending_partial_close);
        }
        if let Some(fragment) = trailing_partial_close_fragment(&chunk) {
            let (head, tail) = chunk.split_at(chunk.len() - fragment.len());
            self.pending_partial_close = tail.to_string();
            let release = if head.is_empty() {
                None
            } else {
                self.push_impl(head)
            };
            self.suppress_forwarding = true;
            return release;
        }
        self.push_impl(&chunk)
    }

    fn fence_forward_documentation(&mut self, chunk: &str) -> bool {
        let marker = find_embedded_protocol_candidate_start(chunk)
            .or_else(|| find_incomplete_protocol_candidate_start(chunk))
            .unwrap_or(chunk.len());

        let mut replay = self.fence.clone();
        let mut pending = self.fence_pending.clone();
        pending.push_str(&chunk[..marker]);
        while let Some(nl) = pending.find('\n') {
            let line = pending[..nl].to_string();
            pending = pending[nl + 1..].to_string();
            replay.feed_line(&line);
        }
        let forward = is_documentation_fence(&replay);

        self.fence_pending.push_str(chunk);
        while let Some(nl) = self.fence_pending.find('\n') {
            let line = self.fence_pending[..nl].to_string();
            self.fence_pending = self.fence_pending[nl + 1..].to_string();
            self.fence.feed_line(&line);
        }
        forward
    }

    fn push_impl(&mut self, chunk: &str) -> Option<String> {
        if self.suppress_forwarding {
            if self.pending.is_empty() && self.continuation_of_suppressed_protocol(chunk) {
                return suppressed_continuation_trailing(chunk).map(str::to_string);
            }
            self.suppress_forwarding = false;
        }
        if chunk.is_empty() {
            return None;
        }

        if self.pending.is_empty() && !starts_suspicious_protocol_prefix(chunk) {
            if let Some(start) = find_embedded_protocol_candidate_start(chunk) {
                self.pending_candidate_start = Some(0);
                self.pending.push_str(&chunk[start..]);
                return if self.should_suppress_protocol_candidate(&self.pending) {
                    self.suppress_protocol(&chunk[..start])
                } else if self.suppressed_protocol {
                    // A prior complete envelope was already suppressed; any
                    // narration before a new (still incomplete) protocol
                    // candidate is user-visible text and must be released now
                    // rather than buffered until that candidate resolves.
                    let narration = chunk[..start].trim();
                    if narration.is_empty() {
                        None
                    } else {
                        Some(narration.to_string())
                    }
                } else {
                    self.pending.insert_str(0, &chunk[..start]);
                    self.pending_candidate_start = Some(start);
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
        self.pending_partial_close.clear();
        self.suppress_forwarding = false;
        if self.pending.is_empty() {
            return None;
        }
        if let Some(release) = self.evaluate_pending(true) {
            return Some(release);
        }
        // evaluate_pending may have suppressed and cleared the buffer.
        if self.pending.is_empty() {
            return None;
        }
        if looks_like_malformed_tool_protocol_envelope_for_known_tools(
            &self.pending,
            &self.known_tool_names,
        ) {
            return self.suppress_protocol(&self.narration_before_candidate());
        }
        // The final fragment is classified on its own merits, not by whether an
        // unrelated envelope was suppressed earlier in the turn. Pending text
        // that still carries a protocol signal (a truncated envelope, a close
        // marker fragment) must not leak; anything else -- including narration
        // that merely ends in '<' -- is released. A turn-global
        // `suppressed_protocol` check here would eat valid EOF text like
        // `Compare 2 <` just because envelope A appeared earlier.
        if self.pending_has_protocol_signal_at_finish(&self.pending) {
            return None;
        }
        Some(std::mem::take(&mut self.pending))
    }

    /// Protocol signals that still make the FINAL pending fragment unsafe to
    /// forward at EOF. Deliberately narrower than [`Self::tail_has_protocol_signal`]:
    /// fragments that merely START with a suspicious prefix or carry an
    /// embedded partial marker (e.g. `plain <｜dsmldata`) are released, not
    /// swallowed -- only complete-but-malformed envelopes and close-marker
    /// fragments must fail closed.
    fn pending_has_protocol_signal_at_finish(&self, pending: &str) -> bool {
        contains_truncated_ascii_dsml_envelope(pending)
            || contains_truncated_fullwidth_dsml_envelope(pending)
            || contains_tool_protocol_tag_call(pending)
            || contains_close_tag_marker(pending)
            || looks_like_malformed_tool_protocol_envelope_for_known_tools(
                pending,
                &self.known_tool_names,
            )
    }

    fn evaluate_pending(&mut self, finalizing: bool) -> Option<String> {
        let candidate = self
            .pending_candidate_start
            .and_then(|start| self.pending.get(start..))
            .unwrap_or(&self.pending);

        if !finalizing && starts_suspicious_tag_or_fence_prefix(candidate) {
            if !self.has_active_tools {
                return None;
            }
            if !looks_like_tool_protocol_example(candidate)
                && self.should_suppress_protocol_candidate(candidate)
            {
                return self.suppress_protocol(&self.narration_before_candidate());
            }
            return None;
        }

        if self.should_suppress_protocol_candidate(candidate) {
            return self.suppress_protocol(&self.narration_before_candidate());
        }

        if let Some(is_protocol) =
            complete_json_fence_protocol_state(candidate, &self.known_tool_names)
        {
            if is_protocol && self.has_active_tools {
                return self.suppress_protocol(&self.narration_before_candidate());
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

    fn suppress_protocol(&mut self, narration: &str) -> Option<String> {
        let mut parts = Vec::new();
        let narration = narration.trim();
        if !narration.is_empty() {
            parts.push(narration.to_string());
        }

        let mut scan = self
            .pending_candidate_start
            .unwrap_or(0)
            .min(self.pending.len());
        loop {
            let start = match find_embedded_protocol_candidate_start(&self.pending[scan..]) {
                Some(rel) => scan + rel,
                None => scan,
            };
            let Some(end) = first_protocol_envelope_end(&self.pending, start) else {
                // Incomplete envelope at the tail: fail closed, do not forward.
                break;
            };
            let between = self.pending[scan..start].trim();
            if !between.is_empty() {
                parts.push(between.to_string());
            }
            scan = end;
        }
        let tail = &self.pending[scan..];
        let mut carried: Option<String> = None;
        if !self.tail_has_protocol_signal(tail) {
            if let Some(partial) = find_incomplete_protocol_candidate_start(tail) {
                // The tail ends in an incomplete marker (e.g. a bare '<' that
                // could open `<|DSML|>` / `<｜DSML｜...>` / `<＼DSML＼...>`).
                // Carry it into the next push instead of forwarding it: the
                // next delta may complete the marker, and envelope B must still
                // be recognized and suppressed rather than leaking as text.
                let before_partial = tail[..partial].trim();
                if !before_partial.is_empty() {
                    parts.push(before_partial.to_string());
                }
                carried = Some(tail[partial..].to_string());
            } else {
                let tail = tail.trim();
                if !tail.is_empty() {
                    parts.push(tail.to_string());
                }
            }
        }

        if let Some(carried) = carried {
            self.pending = carried;
            self.pending_candidate_start = Some(0);
        } else {
            self.pending.clear();
            self.pending_candidate_start = None;
        }
        self.suppressed_protocol = true;
        self.suppress_forwarding = true;
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" "))
        }
    }

    fn tail_has_protocol_signal(&self, tail: &str) -> bool {
        if find_embedded_protocol_candidate_start(tail).is_some()
            || starts_suspicious_protocol_prefix(tail)
            || contains_tool_protocol_tag_call(tail)
            || contains_truncated_ascii_dsml_envelope(tail)
            || contains_truncated_fullwidth_dsml_envelope(tail)
            || looks_like_malformed_tool_protocol_envelope_for_known_tools(
                tail,
                &self.known_tool_names,
            )
        {
            return true;
        }
        tail.match_indices(['{', '[']).any(|(idx, _)| {
            looks_like_malformed_tool_protocol_envelope_for_known_tools(
                &tail[idx..],
                &self.known_tool_names,
            )
        })
    }

    fn continuation_of_suppressed_protocol(&self, chunk: &str) -> bool {
        if suppressed_continuation_trailing(chunk).is_some() {
            return true;
        }
        let lower = chunk.trim_start().to_ascii_lowercase();
        let closed = lower.starts_with("</")
            || lower.starts_with("}")
            || lower.starts_with("]")
            || lower.starts_with("```");
        closed && (starts_suspicious_protocol_prefix(chunk) || contains_close_tag_marker(chunk))
    }

    fn narration_before_candidate(&self) -> String {
        self.pending_candidate_start
            .map(|start| {
                self.pending[..start.min(self.pending.len())]
                    .trim()
                    .to_string()
            })
            .unwrap_or_default()
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

        if contains_truncated_ascii_dsml_envelope(text) {
            return true;
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
mod tests {
    use super::*;
    use crate::tools::ToolSpec;

    fn shell_guard() -> StreamTextGuard {
        StreamTextGuard::new(Some(&[ToolSpec::new(
            "shell",
            "run a command",
            serde_json::json!({}),
        )]))
    }

    #[test]
    fn suppresses_dsml_envelope_split_across_stream_chunks() {
        let mut guard = shell_guard();

        assert_eq!(guard.push("<|DSML|>"), None);
        assert_eq!(guard.push("\n{\"name\":\"shell\""), None);
        assert_eq!(guard.push(",\"arguments\":{\"cmd\":\"ls\"}}"), None);
        assert_eq!(guard.push("\n</|DSML|>"), None);

        assert_eq!(guard.finish(), None);
        assert!(
            guard.suppressed_protocol,
            "DSML envelope must be suppressed"
        );
    }

    #[test]
    fn suppresses_dsml_envelope_and_forwards_clean_text_afterwards() {
        let mut guard = shell_guard();

        assert_eq!(
            guard.push(
                "narration <|DSML|>invoke name=\"shell\"><|DSML|>parameter name=\"command\" string=\"true\">ls</|DSML|>parameter></|DSML|>invoke></|DSML|>tool_calls> Done!"
            ),
            Some("narration Done!".to_string()),
            "trailing text after the envelope must survive suppression"
        );
        assert!(guard.suppressed_protocol);

        assert_eq!(guard.push("More text."), Some("More text.".to_string()));
        assert_eq!(guard.finish(), None);
    }

    #[test]
    fn suppression_scoped_to_envelope_not_stream() {
        let mut guard = shell_guard();

        assert_eq!(
            guard.push(
                "<|DSML|>invoke name=\"shell\"><|DSML|>parameter name=\"command\" string=\"true\">ls</|DSML|>parameter></|DSML|>invoke></|DSML|>tool_calls>"
            ),
            None,
            "standalone envelope must be buffered"
        );
        assert_eq!(guard.finish(), None);
        assert!(guard.suppressed_protocol);

        assert_eq!(guard.push("After."), Some("After.".to_string()));
    }

    #[test]
    fn split_envelope_trailing_close_tag_still_suppressed() {
        let mut guard = shell_guard();

        assert_eq!(guard.push("<|DSML|>"), None);
        assert_eq!(guard.push("\n{\"name\":\"shell\""), None);
        assert_eq!(guard.push(",\"arguments\":{\"cmd\":\"ls\"}}"), None);
        assert_eq!(guard.push("\n</|DSML|>"), None);

        assert_eq!(guard.finish(), None);
        assert!(guard.suppressed_protocol);

        assert_eq!(guard.push("Next."), Some("Next.".to_string()));
    }

    #[test]
    fn envelope_without_narration_keeps_trailing_text() {
        let mut guard = shell_guard();

        assert_eq!(
            guard.push(
                "<|DSML|>invoke name=\"shell\"><|DSML|>parameter name=\"command\" string=\"true\">ls</|DSML|>parameter></|DSML|>invoke></|DSML|>tool_calls> Done!"
            ),
            Some("Done!".to_string()),
            "trailing text must survive even without leading narration"
        );
        assert!(guard.suppressed_protocol);
        assert_eq!(guard.finish(), None);
    }

    #[test]
    fn close_fragment_with_trailing_text_forwards_trailing_only() {
        let mut guard = shell_guard();

        assert_eq!(
            guard.push("Do it. <|DSML|>invoke name=\"shell\"><|DSML|>parameter name=\"command\" string=\"true\">ls"),
            Some("Do it.".to_string()),
            "narration before the envelope must be preserved"
        );
        assert_eq!(
            guard.push("</|DSML|>parameter></|DSML|>invoke></|DSML|>tool_calls> Done!"),
            Some("Done!".to_string())
        );
        assert!(guard.suppressed_protocol);

        assert_eq!(
            guard.push("</|DSML|> Done!"),
            Some("Done!".to_string()),
            "trailing text after a close fragment must be forwarded"
        );
        assert_eq!(guard.push("More."), Some("More.".to_string()));
        assert_eq!(guard.finish(), None);
    }

    #[test]
    fn forwards_plain_text_with_dsml_prefixes() {
        let mut guard = shell_guard();

        assert_eq!(guard.push("<|dsml"), None);
        assert_eq!(guard.push("data"), None);
        assert_eq!(guard.finish(), Some("<|dsmldata".to_string()));
    }

    #[test]
    fn suppresses_fullwidth_dsml_envelope_split_across_stream_chunks() {
        let mut guard = shell_guard();

        assert_eq!(guard.push("<｜DSML｜tool_calls>\n"), None);
        assert_eq!(guard.push("<｜DSML｜invoke name=\"shell\">\n"), None);
        assert_eq!(
            guard.push(
                "<｜DSML｜parameter name=\"command\" string=\"true\">ls</｜DSML｜parameter>\n"
            ),
            None
        );
        assert_eq!(guard.push("</｜DSML｜invoke>\n</｜DSML｜tool_calls>"), None);

        assert_eq!(guard.finish(), None);
        assert!(
            guard.suppressed_protocol,
            "fullwidth DSML envelope must be suppressed"
        );
    }

    #[test]
    fn sequential_envelopes_release_narration_when_next_envelope_starts() {
        let mut guard = shell_guard();

        assert_eq!(
            guard.push(
                "<｜DSML｜tool_calls>\n<｜DSML｜invoke name=\"shell\"><｜DSML｜parameter name=\"command\" string=\"true\">ls</｜DSML｜parameter></｜DSML｜invoke>\n</｜DSML｜tool_calls>"
            ),
            None,
            "first envelope must be suppressed"
        );
        assert_eq!(
            guard.push("\nDone step one.\n<｜DSML｜tool_calls>\n"),
            Some("Done step one.".to_string()),
            "narration after a suppressed envelope is released as soon as the next envelope starts"
        );
        assert_eq!(
            guard.push(
                "<｜DSML｜invoke name=\"shell\"><｜DSML｜parameter name=\"command\" string=\"true\">pwd</｜DSML｜parameter></｜DSML｜invoke>\n"
            ),
            None,
            "second envelope body must be buffered"
        );
        assert_eq!(
            guard.push("</｜DSML｜tool_calls>"),
            None,
            "second envelope must be suppressed"
        );
        assert_eq!(guard.push("\nAll done."), Some("\nAll done.".to_string()));
        assert_eq!(guard.finish(), None);
    }

    #[test]
    fn forwards_plain_text_with_fullwidth_dsml_prefixes() {
        let mut guard = shell_guard();

        assert_eq!(guard.push("<｜dsml"), None);
        assert_eq!(guard.push("data"), None);
        assert_eq!(guard.finish(), Some("<｜dsmldata".to_string()));
    }

    #[test]
    fn forwards_narration_before_embedded_dsml_marker() {
        let mut guard = shell_guard();

        assert_eq!(
            guard.push(
                "I will run it. <｜DSML｜tool_calls><｜DSML｜invoke name=\"shell\"><｜DSML｜parameter name=\"command\" string=\"true\">ls</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜tool_calls>"
            ),
            Some("I will run it.".to_string())
        );
        assert_eq!(guard.finish(), None);
        assert!(
            guard.suppressed_protocol,
            "narration forwarded but DSML envelope suppressed"
        );
    }

    #[test]
    fn forwards_narration_split_across_chunks() {
        let mut guard = shell_guard();

        assert_eq!(guard.push("I will run it. <｜DSML｜tool_calls>"), None);
        assert_eq!(guard.push("<｜DSML｜invoke name=\"shell\">\n"), None);
        assert_eq!(
            guard.push(
                "<｜DSML｜parameter name=\"command\" string=\"true\">ls</｜DSML｜parameter>\n"
            ),
            None
        );
        assert_eq!(
            guard.push("</｜DSML｜invoke>\n</｜DSML｜tool_calls>"),
            Some("I will run it.".to_string())
        );
        assert_eq!(guard.finish(), None);
        assert!(guard.suppressed_protocol);
    }

    #[test]
    fn narration_before_marker_split_at_chunk_boundary() {
        let mut guard = shell_guard();

        assert_eq!(guard.push("I will run it. <｜DSM"), None);
        assert_eq!(guard.push("L｜tool_calls>"), None);
        assert_eq!(guard.push("<｜DSML｜invoke name=\"shell\">\n"), None);
        assert_eq!(
            guard.push(
                "<｜DSML｜parameter name=\"command\" string=\"true\">ls</｜DSML｜parameter>\n"
            ),
            None
        );
        assert_eq!(
            guard.push("</｜DSML｜invoke>\n</｜DSML｜tool_calls>"),
            Some("I will run it.".to_string())
        );
        assert_eq!(guard.finish(), None);
        assert!(guard.suppressed_protocol);
    }

    #[test]
    fn narration_before_marker_split_at_boundary_suppresses_full_envelope() {
        let mut guard = shell_guard();

        assert_eq!(guard.push("Narration. <|D"), None);
        assert_eq!(guard.push("SML|>\n"), None);
        assert_eq!(guard.push("{\"name\":\"shell\""), None);
        assert_eq!(
            guard.push(",\"arguments\":{\"cmd\":\"ls\"}}"),
            Some("Narration.".to_string()),
            "narration is released when the guard becomes certain the DSML envelope is protocol"
        );
        assert_eq!(guard.push("\n</|DSML|>"), None);
        assert_eq!(guard.finish(), None);
        assert!(
            guard.suppressed_protocol,
            "DSML envelope must be suppressed after forwarding narration"
        );
    }

    #[test]
    fn finish_suppresses_standalone_envelope_and_returns_narration() {
        let mut guard = shell_guard();

        assert_eq!(
            guard.push(
                "<|DSML|>invoke name=\"shell\"><|DSML|>parameter name=\"command\" string=\"true\">ls</|DSML|>parameter></|DSML|>invoke></|DSML|>tool_calls>"
            ),
            None,
            "standalone envelope must be buffered"
        );
        assert_eq!(
            guard.finish(),
            None,
            "standalone envelope must not be released as visible text"
        );
        assert!(guard.suppressed_protocol);
    }

    #[test]
    fn finish_returns_narration_before_envelope_suppressed_at_finish() {
        let mut guard = shell_guard();

        assert_eq!(guard.push("Before: "), Some("Before: ".to_string()));
        assert_eq!(
            guard.push(
                "<|DSML|>invoke name=\"shell\"><|DSML|>parameter name=\"command\" string=\"true\">ls</|DSML|>parameter></|DSML|>invoke></|DSML|>tool_calls>"
            ),
            None,
            "narration already forwarded; envelope buffered"
        );
        assert_eq!(guard.finish(), None);
        assert!(guard.suppressed_protocol);
    }

    #[test]
    fn plain_text_with_marker_prefix_still_forwards() {
        let mut guard = shell_guard();

        assert_eq!(guard.push("plain <｜dsml"), None);
        assert_eq!(guard.push("data"), None);
        assert_eq!(guard.finish(), Some("plain <｜dsmldata".to_string()));
    }

    #[test]
    fn split_fenced_example_without_tools_waits_for_trailer() {
        let mut guard = StreamTextGuard::new(None);

        assert_eq!(
            guard.push(
                "```tool_call\n{\"name\":\"shell\",\"arguments\":{\"command\":\"pwd\"}}\n```"
            ),
            None,
            "a complete fence must be buffered until the example trailer resolves it"
        );
        assert_eq!(
            guard.push("\nThis is an example, not an invocation."),
            None,
            "the trailer must not be forwarded until the candidate is classified"
        );
        assert_eq!(
            guard.finish(),
            Some(
                "```tool_call\n{\"name\":\"shell\",\"arguments\":{\"command\":\"pwd\"}}\n```\nThis is an example, not an invocation.".to_string()
            )
        );
        assert!(
            !guard.suppressed_protocol,
            "fenced examples must not be suppressed when no tools are enabled"
        );
    }

    #[test]
    fn standalone_fence_without_tools_is_suppressed_at_finish() {
        let mut guard = StreamTextGuard::new(None);

        assert_eq!(
            guard.push(
                "```tool_call\n{\"name\":\"shell\",\"arguments\":{\"command\":\"pwd\"}}\n```"
            ),
            None,
            "fence must be buffered rather than suppressed mid-stream"
        );
        assert_eq!(guard.finish(), None);
        assert!(
            guard.suppressed_protocol,
            "a standalone fence without an example trailer is a protocol leak"
        );
    }

    #[test]
    fn suppresses_reverse_solidus_dsml_envelope_split_across_stream_chunks() {
        let mut guard = shell_guard();

        assert_eq!(guard.push("<＼DSML＼tool_calls>\n"), None);
        assert_eq!(guard.push("<＼DSML＼invoke name=\"shell\">\n"), None);
        assert_eq!(
            guard.push(
                "<＼DSML＼parameter name=\"command\" string=\"true\">ls</＼DSML＼parameter>\n"
            ),
            None
        );
        assert_eq!(guard.push("</＼DSML＼invoke>\n</＼DSML＼tool_calls>"), None);

        assert_eq!(guard.finish(), None);
        assert!(
            guard.suppressed_protocol,
            "fullwidth reverse-solidus DSML envelope must be suppressed"
        );
    }

    #[test]
    fn forwards_plain_text_with_reverse_solidus_dsml_prefixes() {
        let mut guard = shell_guard();

        assert_eq!(guard.push("<＼dsml"), None);
        assert_eq!(guard.push("data"), None);
        assert_eq!(guard.finish(), Some("<＼dsmldata".to_string()));
    }

    #[test]
    fn forwards_narration_before_embedded_reverse_solidus_dsml_marker() {
        let mut guard = shell_guard();

        assert_eq!(
            guard.push(
                "I will run it. <＼DSML＼tool_calls><＼DSML＼invoke name=\"shell\"><＼DSML＼parameter name=\"command\" string=\"true\">ls</＼DSML＼parameter></＼DSML＼invoke></＼DSML＼tool_calls>"
            ),
            Some("I will run it.".to_string())
        );
        assert_eq!(guard.finish(), None);
        assert!(
            guard.suppressed_protocol,
            "narration forwarded but reverse-solidus DSML envelope suppressed"
        );
    }

    #[test]
    fn narration_before_reverse_solidus_marker_split_at_chunk_boundary() {
        let mut guard = shell_guard();

        assert_eq!(guard.push("I will run it. <＼DSM"), None);
        assert_eq!(guard.push("L＼tool_calls>"), None);
        assert_eq!(guard.push("<＼DSML＼invoke name=\"shell\">\n"), None);
        assert_eq!(
            guard.push(
                "<＼DSML＼parameter name=\"command\" string=\"true\">ls</＼DSML＼parameter>\n"
            ),
            None
        );
        assert_eq!(
            guard.push("</＼DSML＼invoke>\n</＼DSML＼tool_calls>"),
            Some("I will run it.".to_string())
        );
        assert_eq!(guard.finish(), None);
        assert!(guard.suppressed_protocol);
    }

    #[test]
    fn split_fullwidth_close_tag_across_chunks_suppresses_fragment() {
        let mut guard = shell_guard();

        assert_eq!(guard.push("<｜DSML｜tool_calls>\n"), None);
        assert_eq!(guard.push("<｜DSML｜invoke name=\"shell\">\n"), None);
        assert_eq!(
            guard.push(
                "<｜DSML｜parameter name=\"command\" string=\"true\">ls</｜DSML｜parameter>\n</｜DSML｜invoke>\n"
            ),
            None
        );
        // The wrapper close tag is split mid-token: `</｜DSML｜tool_cal` alone.
        assert_eq!(guard.push("</｜DSML｜tool_cal"), None);
        assert_eq!(
            guard.push("ls> Done!"),
            Some("Done!".to_string()),
            "the completing chunk must forward only the trailing text, never the split tag tail"
        );

        assert_eq!(guard.finish(), None);
        assert!(
            guard.suppressed_protocol,
            "fullwidth DSML envelope must be suppressed when its close tag splits across chunks"
        );
    }

    #[test]
    fn split_ascii_close_tag_across_chunks_suppresses_fragment() {
        let mut guard = shell_guard();

        assert_eq!(guard.push("<|DSML|>"), None);
        assert_eq!(guard.push("\n{\"name\":\"shell\""), None);
        assert_eq!(guard.push(",\"arguments\":{\"cmd\":\"ls\"}}"), None);
        assert_eq!(guard.push("\n</|DSML|>"), None);

        // A fresh close-tag fragment after suppression must not forward its tail.
        assert_eq!(guard.push("</|DSML|"), None);
        assert_eq!(
            guard.push("> Done!"),
            Some("Done!".to_string()),
            "only the trailing text after the completed close tag may be forwarded"
        );

        assert_eq!(guard.finish(), None);
        assert!(guard.suppressed_protocol);
    }

    #[test]
    fn opening_marker_split_after_bare_angle_bracket_is_recognized() {
        let mut guard = shell_guard();

        assert_eq!(guard.push("Do it. <"), None);
        assert_eq!(guard.push("|DSML|>\n"), None);
        assert_eq!(
            guard.push("{\"name\":\"shell\",\"arguments\":{\"cmd\":\"ls\"}}"),
            Some("Do it.".to_string())
        );
        assert_eq!(guard.push("\n</|DSML|>"), None);
        assert_eq!(guard.finish(), None);
        assert!(
            guard.suppressed_protocol,
            "envelope opened across a bare '<' boundary must be suppressed"
        );
    }

    #[test]
    fn fullwidth_opening_marker_split_after_bare_angle_bracket_is_recognized() {
        let mut guard = shell_guard();

        assert_eq!(guard.push("Do it. <"), None);
        assert_eq!(guard.push("｜DSML｜tool_calls>\n"), None);
        assert_eq!(
            guard.push("<｜DSML｜invoke name=\"shell\">\n<｜DSML｜parameter name=\"command\" string=\"true\">ls</｜DSML｜parameter>\n"),
            None
        );
        // The envelope completes here: narration is released and the envelope
        // itself is suppressed, never forwarded.
        assert_eq!(
            guard.push("</｜DSML｜invoke>\n</｜DSML｜tool_calls>"),
            Some("Do it.".to_string())
        );
        assert_eq!(guard.finish(), None);
        assert!(
            guard.suppressed_protocol,
            "fullwidth envelope opened across a bare '<' boundary must be suppressed"
        );
    }

    #[test]
    fn closing_marker_split_after_bare_angle_bracket_is_suppressed() {
        let mut guard = shell_guard();

        assert_eq!(guard.push("<|DSML|>\n"), None);
        assert_eq!(
            guard.push("{\"name\":\"shell\",\"arguments\":{\"cmd\":\"ls\"}}"),
            None
        );
        // The closing `</|DSML|>` is split as `<` + `/|DSML|>`.
        assert_eq!(guard.push("\n<"), None);
        assert_eq!(guard.push("/|DSML|>"), None);
        assert_eq!(guard.finish(), None);
        assert!(
            guard.suppressed_protocol,
            "closing marker split across a bare '<' boundary must not leak"
        );
    }

    #[test]
    fn same_delta_sequential_envelopes_preserve_narration_between() {
        let mut guard = shell_guard();

        let delta = "<|DSML|>invoke name=\"shell\"><|DSML|>parameter name=\"command\" string=\"true\">ls</|DSML|>parameter></|DSML|>invoke></|DSML|>tool_calls> \
            Done step one. \
            <|DSML|>invoke name=\"file_read\"><|DSML|>parameter name=\"path\" string=\"true\">a.txt</|DSML|>parameter></|DSML|>invoke></|DSML|>tool_calls> \
            All done.";
        assert_eq!(
            guard.push(delta),
            Some("Done step one. All done.".to_string())
        );
        assert_eq!(guard.finish(), None);
        assert!(
            guard.suppressed_protocol,
            "same-delta sequential envelopes must be suppressed"
        );
    }

    #[test]
    fn same_delta_dsml_then_json_envelope_reclassifies_suffix() {
        let mut guard = shell_guard();

        let delta = "<|DSML|>invoke name=\"shell\"><|DSML|>parameter name=\"command\" string=\"true\">ls</|DSML|>parameter></|DSML|>invoke></|DSML|>tool_calls> \
            narration {\"tool_calls\":[{\"function\":{\"name\":\"shell\",\"arguments\":\"{}\"}}]} \
            trailing";
        assert_eq!(
            guard.push(delta),
            Some("narration trailing".to_string()),
            "the JSON tool envelope after narration must be suppressed, not forwarded"
        );
        assert_eq!(guard.finish(), None);
        assert!(guard.suppressed_protocol);
    }

    #[test]
    fn streamed_tilde_fenced_dsml_example_stays_visible() {
        let mut guard = shell_guard();

        assert_eq!(
            guard.push("~~~xml\n<｜DSML｜tool_calls>\n<｜DSML｜invoke name=\"shell\">\n"),
            Some("~~~xml\n<｜DSML｜tool_calls>\n<｜DSML｜invoke name=\"shell\">\n".to_string()),
            "quoted tilde-fenced documentation must remain visible"
        );
        assert_eq!(
            guard.push("<｜DSML｜parameter name=\"command\" string=\"true\">ls</｜DSML｜parameter>\n</｜DSML｜invoke>\n</｜DSML｜tool_calls>\n~~~\nEnd."),
            Some("<｜DSML｜parameter name=\"command\" string=\"true\">ls</｜DSML｜parameter>\n</｜DSML｜invoke>\n</｜DSML｜tool_calls>\n~~~\nEnd.".to_string())
        );
        assert_eq!(guard.finish(), None);
        assert!(
            !guard.suppressed_protocol,
            "tilde-fenced documentation must not be suppressed"
        );
    }

    /// REGRESSION: a DSML example inside a CommonMark blockquoted backtick
    /// fence must stay visible in the STREAM, independent of prose keywords.
    #[test]
    fn streamed_blockquoted_backtick_fenced_dsml_example_stays_visible() {
        let mut guard = shell_guard();

        assert_eq!(
            guard.push("> ```xml\n> <｜DSML｜tool_calls>\n> <｜DSML｜invoke name=\"shell\">\n"),
            Some(
                "> ```xml\n> <｜DSML｜tool_calls>\n> <｜DSML｜invoke name=\"shell\">\n".to_string()
            ),
            "blockquoted backtick-fenced documentation must remain visible"
        );
        assert_eq!(
            guard.push("> <｜DSML｜parameter name=\"command\" string=\"true\">ls</｜DSML｜parameter>\n> </｜DSML｜invoke>\n> </｜DSML｜tool_calls>\n> ```\nEnd."),
            Some("> <｜DSML｜parameter name=\"command\" string=\"true\">ls</｜DSML｜parameter>\n> </｜DSML｜invoke>\n> </｜DSML｜tool_calls>\n> ```\nEnd.".to_string())
        );
        assert_eq!(guard.finish(), None);
        assert!(
            !guard.suppressed_protocol,
            "blockquoted backtick-fenced documentation must not be suppressed"
        );
    }

    /// REGRESSION: the blockquoted tilde fence has the same streaming exposure.
    #[test]
    fn streamed_blockquoted_tilde_fenced_dsml_example_stays_visible() {
        let mut guard = shell_guard();

        assert_eq!(
            guard.push("> ~~~xml\n> <｜DSML｜tool_calls>\n> <｜DSML｜invoke name=\"shell\">\n"),
            Some(
                "> ~~~xml\n> <｜DSML｜tool_calls>\n> <｜DSML｜invoke name=\"shell\">\n".to_string()
            ),
            "blockquoted tilde-fenced documentation must remain visible"
        );
        assert_eq!(
            guard.push("> <｜DSML｜parameter name=\"command\" string=\"true\">ls</｜DSML｜parameter>\n> </｜DSML｜invoke>\n> </｜DSML｜tool_calls>\n> ~~~\nEnd."),
            Some("> <｜DSML｜parameter name=\"command\" string=\"true\">ls</｜DSML｜parameter>\n> </｜DSML｜invoke>\n> </｜DSML｜tool_calls>\n> ~~~\nEnd.".to_string())
        );
        assert_eq!(guard.finish(), None);
        assert!(
            !guard.suppressed_protocol,
            "blockquoted tilde-fenced documentation must not be suppressed"
        );
    }

    /// REGRESSION: a blockquoted ```tool_call fence is an EXECUTABLE protocol
    /// fence, not documentation: with a real call inside it must be suppressed,
    /// proving the container-prefix fix does not white-list tool fences. Only
    /// the bare blockquote marker before the fence is narration.
    #[test]
    fn streamed_blockquoted_tool_call_fence_with_call_is_suppressed() {
        let mut guard = shell_guard();

        assert_eq!(
            guard.push(
                "> ```tool_call\n> {\"name\":\"shell\",\"arguments\":{\"command\":\"pwd\"}}\n> ```"
            ),
            Some(">".to_string()),
            "the blockquote marker before the fence is narration; the fence itself must be buffered"
        );
        assert_eq!(guard.finish(), None);
        assert!(
            guard.suppressed_protocol,
            "a blockquoted tool_call fence without an example trailer is a protocol leak"
        );
    }

    /// REGRESSION: the fullwidth marker has the same carry exposure as ASCII:
    /// envelope A plus narration ending in '<' must carry the marker so a
    /// following `｜DSML｜tool_calls>` delta is still recognized.
    #[test]
    fn suppressed_fullwidth_envelope_then_partial_marker_recognizes_next_envelope() {
        let mut guard = shell_guard();

        let delta_a = concat!(
            "<｜DSML｜tool_calls>\n<｜DSML｜invoke name=\"shell\">\n<｜DSML｜parameter name=\"command\" string=\"true\">ls</｜DSML｜parameter>\n</｜DSML｜invoke>\n</｜DSML｜tool_calls>",
            " Mid <"
        );
        assert_eq!(
            guard.push(delta_a),
            Some("Mid".to_string()),
            "narration before the trailing '<' is released; the '<' itself is carried"
        );
        assert_eq!(
            guard.push("｜DSML｜tool_calls>\n"),
            None,
            "fullwidth envelope B continues from the carried '<' and must not be forwarded"
        );
        assert_eq!(
            guard.push("<｜DSML｜invoke name=\"shell\">\n<｜DSML｜parameter name=\"command\" string=\"true\">pwd</｜DSML｜parameter>\n</｜DSML｜invoke>\n</｜DSML｜tool_calls>"),
            None,
            "fullwidth envelope B's body must not leak"
        );
        assert_eq!(guard.finish(), None);
        assert!(
            guard.suppressed_protocol,
            "fullwidth envelope B opened across the carried marker must be suppressed"
        );
    }

    /// REGRESSION: the carry path must preserve LONGER partial markers too.
    /// After envelope A, ` Mid <|DS` carries `<|DS`; the next delta completes
    /// `<|DSML|>` and envelope B is still recognized.
    #[test]
    fn suppressed_envelope_then_multi_char_partial_marker_recognizes_next_envelope() {
        let mut guard = shell_guard();

        let delta_a = concat!(
            "<|DSML|>invoke name=\"shell\"><|DSML|>parameter name=\"command\" string=\"true\">ls</|DSML|>parameter></|DSML|>invoke></|DSML|>tool_calls>",
            " Mid <|DS"
        );
        assert_eq!(
            guard.push(delta_a),
            Some("Mid".to_string()),
            "narration before the partial marker is released; `<|DS` is carried"
        );
        assert_eq!(
            guard.push("ML|>\n"),
            None,
            "envelope B continues from the carried `<|DS` and must not be forwarded"
        );
        assert_eq!(
            guard.push("{\"name\":\"shell\",\"arguments\":{\"cmd\":\"pwd\"}}\n</|DSML|>"),
            None
        );
        assert_eq!(guard.finish(), None);
        assert!(
            guard.suppressed_protocol,
            "envelope B opened across the carried multi-char marker must be suppressed"
        );
    }

    /// REGRESSION: a carried bare '<' that never grows into a marker at EOF is
    /// classified on its own merits: it is not a protocol signal, so it is
    /// released rather than swallowed by the earlier suppression.
    #[test]
    fn carried_bare_angle_bracket_is_released_at_eof() {
        let mut guard = shell_guard();

        assert_eq!(
            guard.push(
                "<|DSML|>invoke name=\"shell\"><|DSML|>parameter name=\"command\" string=\"true\">ls</|DSML|>parameter></|DSML|>invoke></|DSML|>tool_calls> Mid <"
            ),
            Some("Mid".to_string()),
            "narration is released and the bare '<' is carried"
        );
        assert_eq!(
            guard.finish(),
            Some("<".to_string()),
            "the carried bare '<' that never completed a marker is released at EOF"
        );
        assert!(guard.suppressed_protocol);
    }

    /// REGRESSION: the STREAM path must also fail closed on the close-borrow
    /// shape. An unclosed ASCII envelope followed by a valid same-family
    /// wrapper arrives as one guarded stream; it must be suppressed, not
    /// executed from malformed combined content.
    #[test]
    fn streamed_borrow_shape_unclosed_ascii_envelope_is_suppressed() {
        let mut guard = shell_guard();

        assert_eq!(
            guard.push("<|DSML|>\n"),
            None,
            "the unclosed first envelope must be buffered"
        );
        assert_eq!(
            guard.push("{\"name\":\"shell\",\"arguments\":{\"command\":\"rm -rf /tmp/x\"}}\n"),
            None
        );
        assert_eq!(
            guard.push("<|DSML|>\n"),
            None,
            "a second opener before any close must not be treated as clean text"
        );
        assert_eq!(
            guard.push("{\"name\":\"shell\",\"arguments\":{\"command\":\"ls\"}}\n</|DSML|>"),
            None,
            "the borrowed close must not resolve the malformed combined content"
        );
        assert_eq!(guard.finish(), None);
        assert!(
            guard.suppressed_protocol,
            "the borrow shape must be suppressed end to end in the stream"
        );
    }

    /// REGRESSION: after envelope A is suppressed, narration ending in a bare
    /// '<' inside the SAME delta must carry that '<' as pending state, so the
    /// next delta that completes `<|DSML|>` is still recognized as envelope B
    /// instead of leaking its protocol text.
    #[test]
    fn suppressed_envelope_then_partial_marker_recognizes_next_envelope() {
        let mut guard = shell_guard();

        let delta_a = concat!(
            "<|DSML|>invoke name=\"shell\"><|DSML|>parameter name=\"command\" string=\"true\">ls</|DSML|>parameter></|DSML|>invoke></|DSML|>tool_calls>",
            " Mid <"
        );
        assert_eq!(
            guard.push(delta_a),
            Some("Mid".to_string()),
            "narration before the trailing '<' is released; the '<' itself is carried"
        );
        assert_eq!(
            guard.push("|DSML|>\n"),
            None,
            "envelope B continues from the carried '<' and must not be forwarded"
        );
        assert_eq!(
            guard.push("{\"name\":\"shell\",\"arguments\":{\"cmd\":\"pwd\"}}\n</|DSML|>"),
            None,
            "envelope B's body must not leak"
        );
        assert_eq!(guard.finish(), None);
        assert!(
            guard.suppressed_protocol,
            "envelope B opened across the carried marker must be suppressed"
        );
    }

    /// REGRESSION: finish() must classify the final pending fragment on its own
    /// merits. A valid EOF fragment like `Compare 2 <` must be released even
    /// when an unrelated envelope was suppressed earlier in the turn.
    #[test]
    fn suppressed_envelope_then_valid_eof_fragment_is_released() {
        let mut guard = shell_guard();

        assert_eq!(
            guard.push(
                "<|DSML|>invoke name=\"shell\"><|DSML|>parameter name=\"command\" string=\"true\">ls</|DSML|>parameter></|DSML|>invoke></|DSML|>tool_calls>"
            ),
            None,
            "standalone envelope must be suppressed"
        );
        assert_eq!(
            guard.push("Compare 2 <"),
            None,
            "the trailing '<' is buffered as a possible marker start"
        );
        assert_eq!(
            guard.finish(),
            Some("Compare 2 <".to_string()),
            "valid EOF text must not disappear because an envelope was suppressed earlier"
        );
    }
}
