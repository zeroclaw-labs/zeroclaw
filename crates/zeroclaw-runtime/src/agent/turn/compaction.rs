//! Turn-side wiring for context compaction.
//!
//! The compaction itself lives in [`crate::agent::context_compaction`]. This
//! module owns the turn-side decisions: when a compaction is due, and how the
//! result is announced to the client and to the observers.
//!
//! The announcement reuses the `history_trimmed` contract: a client that
//! already shows "the context was trimmed" also shows "the context was
//! compacted", with the same token accounting.

use std::time::Duration;

use zeroclaw_api::agent::TokenCountSource;
use zeroclaw_api::agent::TurnEvent;
use zeroclaw_api::model_provider::{ChatMessage, ModelProvider};
use zeroclaw_config::schema::{Config, ResolvedContextLimits};

use crate::agent::context_compaction::{self, CompactionOutcome};
use crate::agent::turn::events::{DraftEvent, ProgressEvent, send_progress};
use crate::agent::turn::{MemoryPreamble, retained_context_snapshot};
use crate::observability::{Observer, ObserverEvent};

/// The manual command a user types to compact the conversation.
const MANUAL_COMPACTION_COMMAND: &str = "/compact";

/// Whether the last user message is the manual `/compact` command.
///
/// Only an exact, standalone command counts: a message that merely mentions
/// `/compact` is ordinary user content. The runtime stamps user turns with
/// their arrival time (`[CURRENT DATE & TIME: …]` for a local turn,
/// `[2026-01-01 09:00:00 CET]` for a channel turn) and, on some channel
/// scopes, with the sender (`[Message from …]`); those stamps are stripped
/// before the comparison, so the command is still recognized.
pub(crate) fn is_manual_compaction_request(history: &[ChatMessage]) -> bool {
    history
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .is_some_and(|message| {
            strip_runtime_stamps(&message.content)
                .trim()
                .eq_ignore_ascii_case(MANUAL_COMPACTION_COMMAND)
        })
}

/// Strip the runtime stamps a user turn may carry.
///
/// Only the known stamp shapes are removed, and only at the very beginning of
/// the message: a user sentence that happens to start with brackets is left
/// alone.
fn strip_runtime_stamps(text: &str) -> &str {
    let mut rest = text.trim_start();
    while let Some(after_bracket) = rest.strip_prefix('[') {
        let Some(end) = after_bracket.find(']') else {
            break;
        };
        let stamp = &after_bracket[..end];
        if !is_runtime_stamp(stamp) {
            break;
        }
        rest = after_bracket[end + 1..].trim_start();
        // A stamp on its own line is followed by a blank line; a timestamp
        // stamp shares the line with the message.
        rest = rest.strip_prefix('\n').unwrap_or(rest).trim_start();
    }
    rest
}

/// Whether a bracketed prefix is one of the runtime stamps.
fn is_runtime_stamp(stamp: &str) -> bool {
    let stamp = stamp.trim();
    stamp.starts_with("CURRENT DATE & TIME:")
        || stamp.starts_with("Message from ")
        || is_timestamp_stamp(stamp)
}

/// Whether the stamp looks like a `[YYYY-MM-DD HH:MM:SS …]` timestamp.
fn is_timestamp_stamp(stamp: &str) -> bool {
    let Some((date, rest)) = stamp.split_once(' ') else {
        return false;
    };
    let date_ok = date.len() == 10
        && date.chars().enumerate().all(|(index, c)| match index {
            4 | 7 => c == '-',
            _ => c.is_ascii_digit(),
        });
    date_ok && rest.contains(':')
}

/// The user-facing answer of a manual compaction.
pub(crate) fn manual_outcome_notice(outcome: &CompactionOutcome) -> String {
    if !outcome.compacted {
        return crate::i18n::get_required_cli_string("history-compaction-nothing-to-do");
    }
    crate::i18n::get_required_cli_string_with_args(
        "history-compaction-done",
        &[
            ("turns", &outcome.turns_compacted.to_string()),
            ("before", &outcome.tokens_before.to_string()),
            ("after", &outcome.tokens_after.to_string()),
        ],
    )
}

/// Everything one compaction attempt needs from its caller.
pub(crate) struct CompactionRequest<'a> {
    /// The provider-facing history, rewritten in place when the compaction
    /// succeeds.
    pub(crate) history: &'a mut Vec<ChatMessage>,
    /// The owner's record of the synthetic head slot.
    pub(crate) crumb_present: &'a mut bool,
    /// The injected memory preamble, when the turn carries one.
    pub(crate) injected_memory_preamble: &'a Option<MemoryPreamble>,
    /// Provider used for the summary.
    pub(crate) provider: &'a dyn ModelProvider,
    /// Model used for the summary.
    pub(crate) model: &'a str,
    /// Sampling temperature for the summary call.
    pub(crate) temperature: Option<f64>,
    /// Runtime configuration, when the caller has one.
    pub(crate) config: Option<&'a Config>,
    /// The acting agent.
    pub(crate) agent_alias: Option<&'a str>,
    /// Limits of the selected route.
    pub(crate) context_limits: ResolvedContextLimits,
    /// Observer for the compaction event.
    pub(crate) observer: &'a dyn Observer,
    /// Client event channel.
    pub(crate) event_tx: Option<&'a tokio::sync::mpsc::Sender<TurnEvent>>,
    /// Live delta channel, for the compaction lifecycle state.
    pub(crate) on_delta: Option<&'a tokio::sync::mpsc::Sender<DraftEvent>>,
    /// Turn identifier, stamped onto the observer event.
    pub(crate) turn_id: &'a str,
    /// Token count that motivated the attempt.
    pub(crate) trigger_tokens: usize,
    /// Compact even when the history is under the threshold (manual path).
    pub(crate) forced: bool,
}

/// Compact the history when it is due, and announce the result.
///
/// The announcement only happens when a summary was actually written: a
/// failed attempt leaves the history untouched and says nothing.
pub(crate) async fn compact_and_announce(request: CompactionRequest<'_>) -> CompactionOutcome {
    let CompactionRequest {
        history,
        crumb_present,
        injected_memory_preamble,
        provider,
        model,
        temperature,
        config,
        agent_alias,
        context_limits,
        observer,
        event_tx,
        on_delta,
        turn_id,
        trigger_tokens,
        forced,
    } = request;
    let Some(compaction) = context_compaction::effective_config(config, agent_alias) else {
        return CompactionOutcome::skipped(trigger_tokens);
    };
    if !forced && !compaction.enabled {
        return CompactionOutcome::skipped(trigger_tokens);
    }
    let outcome = context_compaction::compact_history(context_compaction::CompactionRequest {
        history,
        crumb_present,
        provider,
        model,
        temperature,
        config: compaction,
        context_window: context_limits.model_context_window,
        forced,
        timeout: Duration::from_secs(compaction.timeout_secs),
    })
    .await;
    if !outcome.compacted {
        return outcome;
    }
    send_progress(on_delta, ProgressEvent::CompactingContext).await;
    let reason = crate::i18n::get_required_cli_string("history-compaction-reason");
    let kept_turns = crate::agent::history_trim::count_turns(history);
    emit_client_event(
        event_tx,
        &outcome,
        kept_turns,
        &reason,
        context_limits,
        injected_memory_preamble,
        history,
        *crumb_present,
    )
    .await;
    observer.record_event(&ObserverEvent::HistoryTrimmed {
        dropped_messages: outcome.messages_compacted,
        kept_turns,
        reason,
        channel: None,
        agent_alias: agent_alias.map(str::to_string),
        turn_id: Some(turn_id.to_string()),
        token_budget: Some(context_limits.context_token_budget as u64),
        tokens_before: Some(outcome.tokens_before as u64),
        tokens_after: Some(outcome.tokens_after as u64),
        tokens_before_source: Some(TokenCountSource::Estimated),
        tokens_after_source: Some(TokenCountSource::Estimated),
        unsatisfiable_floor: None,
    });
    outcome
}

/// Emit the client-visible `history_trimmed` event for a compaction.
///
/// The event reuses the trim contract on purpose: a client that already shows
/// "the context was trimmed" also shows "the context was compacted", with the
/// same token accounting.
#[allow(clippy::too_many_arguments)]
async fn emit_client_event(
    event_tx: Option<&tokio::sync::mpsc::Sender<TurnEvent>>,
    outcome: &CompactionOutcome,
    kept_turns: usize,
    reason: &str,
    context_limits: ResolvedContextLimits,
    injected_memory_preamble: &Option<MemoryPreamble>,
    history: &[ChatMessage],
    crumb_present: bool,
) {
    let Some(tx) = event_tx else {
        return;
    };
    let _ = tx
        .send(TurnEvent::HistoryTrimmed {
            dropped_messages: outcome.messages_compacted,
            dropped_turns: outcome.turns_compacted,
            kept_turns,
            reason: reason.to_string(),
            token_budget: Some(context_limits.context_token_budget as u64),
            tokens_before: Some(outcome.tokens_before as u64),
            tokens_after: Some(outcome.tokens_after as u64),
            tokens_before_source: Some(TokenCountSource::Estimated),
            tokens_after_source: Some(TokenCountSource::Estimated),
            unsatisfiable_floor: None,
            retained_context: Some(retained_context_snapshot(
                injected_memory_preamble,
                history,
                crumb_present,
            )),
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::{is_manual_compaction_request, manual_outcome_notice};
    use crate::agent::context_compaction::CompactionOutcome;
    use zeroclaw_api::model_provider::ChatMessage;

    fn history_with_last_user(text: &str) -> Vec<ChatMessage> {
        vec![
            ChatMessage::system("system prompt"),
            ChatMessage::user("first turn"),
            ChatMessage::assistant("answer"),
            ChatMessage::user(text),
        ]
    }

    #[test]
    fn an_exact_command_is_a_compaction_request() {
        assert!(is_manual_compaction_request(&history_with_last_user(
            "/compact"
        )));
        assert!(is_manual_compaction_request(&history_with_last_user(
            "  /COMPACT  "
        )));
    }

    #[test]
    fn a_stamped_user_turn_is_still_the_command() {
        // The runtime stamps local turns with their arrival time.
        let stamped = "[CURRENT DATE & TIME: 2026-01-01 09:00:00 CET]\n\n/compact";
        assert!(is_manual_compaction_request(&history_with_last_user(
            stamped
        )));
        // Channel turns carry a timestamp prefix on the same line.
        let channel = "[2026-01-01 09:00:00 CET] /compact";
        assert!(is_manual_compaction_request(&history_with_last_user(
            channel
        )));
        // Multi-sender scopes attribute the turn.
        let attributed = "[Message from alice]\n/compact";
        assert!(is_manual_compaction_request(&history_with_last_user(
            attributed
        )));
    }

    #[test]
    fn a_message_mentioning_the_command_is_ordinary_content() {
        assert!(!is_manual_compaction_request(&history_with_last_user(
            "run /compact for me"
        )));
        assert!(!is_manual_compaction_request(&history_with_last_user(
            "/compact-model"
        )));
        // A user sentence that starts with brackets is left alone.
        assert!(!is_manual_compaction_request(&history_with_last_user(
            "[note] /compact"
        )));
    }

    #[test]
    fn the_tool_result_carrier_is_not_the_command() {
        // The newest user message is a tool-results carrier, not the command:
        // the command stays the last real user turn only when it really is one.
        let history = vec![
            ChatMessage::system("system prompt"),
            ChatMessage::user("/compact"),
            ChatMessage::user(format!(
                "{}\nresults",
                zeroclaw_api::tool_carrier::TOOL_RESULTS_PREFIX
            )),
        ];
        assert!(!is_manual_compaction_request(&history));
    }

    #[test]
    fn the_notice_formats_in_every_committed_locale() {
        let compacted = CompactionOutcome {
            compacted: true,
            turns_compacted: 3,
            messages_compacted: 6,
            tokens_before: 950_000,
            tokens_after: 40_000,
        };
        let notice = manual_outcome_notice(&compacted);
        assert!(notice.contains('3'), "turns must be named: {notice}");
        assert!(notice.contains("950000"), "before must be named: {notice}");
        assert!(notice.contains('4'), "after must be named: {notice}");

        let skipped = CompactionOutcome::skipped(100);
        assert!(!manual_outcome_notice(&skipped).is_empty());
    }
}
