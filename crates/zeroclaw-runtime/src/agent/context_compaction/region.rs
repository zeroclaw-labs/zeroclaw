//! Region selection and installation for context compaction.
//!
//! The compacted region is a range of whole turns between the protected head
//! and the protected tail. Whole turns only: a tool call and its result stay
//! together, so the model never receives a half-finished exchange.

use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_config::scattered_types::ContextCompressionConfig;

/// The range of turns a compaction pass replaces, plus the transcript handed
/// to the summarizer.
#[derive(Debug, Clone)]
pub(crate) struct CompactionRegion {
    /// Number of leading system messages kept verbatim.
    pub(crate) head_len: usize,
    /// Absolute index where the protected head turns begin: right after the
    /// old synthetic head slot (trim breadcrumb or previous summary), which
    /// the new summary replaces.
    pub(crate) keep_from: usize,
    /// Absolute index of the first compacted message. The turns between the
    /// head slot and this index are the protected head turns: they stay
    /// verbatim, before the summary.
    pub(crate) start: usize,
    /// Absolute index just after the last compacted message.
    pub(crate) end: usize,
    /// Whole turns inside the compacted range.
    pub(crate) turns: usize,
    /// Messages inside the compacted range.
    pub(crate) messages: usize,
    /// The transcript handed to the summarizer.
    pub(crate) transcript: String,
}

/// Pick the range of turns to compact.
///
/// Returns `None` when the history is too short to compact: the protected
/// head and tail turns would already cover everything.
pub(crate) fn select(
    history: &[ChatMessage],
    crumb_present: bool,
    config: &ContextCompressionConfig,
) -> Option<CompactionRegion> {
    let head_len = leading_system_len(history);
    // The synthetic head slot (trim breadcrumb or previous compaction
    // summary) sits right after the leading system messages. It is part of
    // the compacted range: re-summarizing it keeps a single rolling summary
    // instead of stacking one summary per compaction.
    let has_head_slot = crumb_present && history.len() > head_len;
    let body_start = head_len + usize::from(has_head_slot);
    let boundaries = turn_boundaries(history, body_start);
    let total_turns = boundaries.len();
    let protect_first = config.protect_first_n.min(total_turns);
    let protect_last = config
        .protect_last_n
        .min(total_turns.saturating_sub(protect_first));
    if total_turns <= protect_first + protect_last {
        return None;
    }
    let first_turn = protect_first;
    let last_turn = total_turns - protect_last;
    let start = boundaries[first_turn];
    let end = boundaries[last_turn];
    if start >= end {
        return None;
    }
    let mut transcript = String::new();
    if has_head_slot && let Some(previous) = history.get(head_len) {
        append_transcript_line(&mut transcript, &previous.role, &previous.content);
    }
    for message in &history[start..end] {
        append_transcript_line(&mut transcript, &message.role, &message.content);
    }
    Some(CompactionRegion {
        head_len,
        keep_from: body_start,
        start,
        end,
        turns: last_turn - first_turn,
        messages: end - start,
        transcript,
    })
}

/// Replace the compacted range with the summary message.
///
/// The summary takes the synthetic head slot (right after the leading system
/// messages) so the whole-turn trim keeps it: the owner records that the
/// population carries a head slot, and the trim never drops it.
pub(crate) fn install(
    history: &mut Vec<ChatMessage>,
    crumb_present: &mut bool,
    region: &CompactionRegion,
    summary: &str,
) {
    let summary_message = ChatMessage::user(summary_message_content(region, summary));
    let mut rewritten: Vec<ChatMessage> = Vec::with_capacity(history.len());
    // Leading system messages, then the summary in the head slot, then the
    // protected head turns (between the old slot and the compacted range),
    // then everything after the compacted range.
    rewritten.extend_from_slice(&history[..region.head_len]);
    rewritten.push(summary_message);
    rewritten.extend_from_slice(&history[region.keep_from..region.start]);
    rewritten.extend_from_slice(&history[region.end..]);
    *history = rewritten;
    *crumb_present = true;
    // The compacted range was cut on turn boundaries, so tool pairs stay
    // intact. The orphan sweep stays as the safety net for histories that
    // were already inconsistent.
    crate::agent::history_pruner::remove_orphaned_tool_messages(history);
    crate::agent::history_pruner::strip_orphaned_tool_calls_from_assistants(history);
}

/// Build the model-visible summary message.
fn summary_message_content(region: &CompactionRegion, summary: &str) -> String {
    let header = crate::i18n::get_required_cli_string_with_args(
        "history-compaction-summary-header",
        &[
            ("turns", &region.turns.to_string()),
            ("messages", &region.messages.to_string()),
        ],
    );
    format!("{header}\n\n{summary}")
}

/// Number of leading system messages.
fn leading_system_len(history: &[ChatMessage]) -> usize {
    history
        .iter()
        .take_while(|message| message.role == "system")
        .count()
}

/// Absolute indices of the turn boundaries at or after `from`.
fn turn_boundaries(history: &[ChatMessage], from: usize) -> Vec<usize> {
    history
        .iter()
        .enumerate()
        .skip(from)
        .filter(|(_, message)| crate::agent::history_trim::is_turn_boundary(message))
        .map(|(index, _)| index)
        .collect()
}

/// Append one `ROLE: content` line to the transcript.
fn append_transcript_line(transcript: &mut String, role: &str, content: &str) {
    use std::fmt::Write;
    let _ = writeln!(transcript, "{}: {}", role.to_uppercase(), content.trim());
}

/// Truncate a transcript to `max_chars`, on a character boundary.
pub(crate) fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let mut end = max_chars;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut truncated = text[..end].to_string();
    truncated.push_str("...");
    truncated
}

/// Strip the local media markers a text-only summarizer must never receive.
pub(crate) fn strip_media_markers(text: &str) -> String {
    zeroclaw_providers::multimodal::strip_media_markers(text)
}
