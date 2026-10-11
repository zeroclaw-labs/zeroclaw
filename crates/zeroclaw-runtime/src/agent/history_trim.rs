//! Whole-turn history trimming. One rule: keep the most recent whole turns
//! that fit the token budget, drop the rest, never cut a turn in half.

use crate::agent::history::estimate_history_tokens;
use zeroclaw_api::model_provider::ConversationMessage;
use zeroclaw_providers::ChatMessage;

/// Prefixes of the user-role rows the tool loop itself appends mid-turn to give
/// the model feedback on its own output (see the malformed-protocol retry in
/// `turn/mod.rs`). They are user-role only because no `tool_call_id` exists to
/// attach them to, and they belong to the turn they interrupt.
pub(crate) const RUNTIME_FEEDBACK_PREFIXES: &[&str] = &["[Tool call parse error]"];

/// A user row that opens a turn for tool-context retention: a turn boundary
/// that is not runtime feedback. Whole-turn trimming keeps its own boundary
/// rule; retention must not let the loop's own feedback row push the running
/// turn's tool rows out of the retained window.
fn opens_retention_turn(msg: &ChatMessage) -> bool {
    is_turn_boundary(msg)
        && !RUNTIME_FEEDBACK_PREFIXES
            .iter()
            .any(|prefix| msg.content.starts_with(prefix))
}

/// Outcome of a trim pass. `trimmed` is true only when at least one whole turn
/// was dropped, in which case the caller emits a user-visible event and injects
/// a breadcrumb so the loss is never silent.
#[derive(Debug, Clone)]
pub struct TrimResult {
    pub history: Vec<ChatMessage>,
    pub dropped_messages: usize,
    pub dropped_turns: usize,
    pub kept_turns: usize,
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub trimmed: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct TurnCountTrimResult<T> {
    pub history: Vec<T>,
    pub dropped_messages: usize,
    pub dropped_turns: usize,
    pub kept_turns: usize,
    pub trimmed: bool,
}

fn is_conversation_system(msg: &ConversationMessage) -> bool {
    matches!(msg, ConversationMessage::Chat(chat) if chat.role == "system")
}

fn is_conversation_turn_boundary(msg: &ConversationMessage, is_breadcrumb: bool) -> bool {
    matches!(
        msg,
        ConversationMessage::Chat(chat)
            if chat.role == "user"
                && !chat
                    .content
                    .starts_with(zeroclaw_api::tool_carrier::TOOL_RESULTS_PREFIX)
                && !is_breadcrumb
    )
}

/// Keep at most `max_turns` recent structured conversation turns, while always
/// retaining the newest complete turn. The legacy config key is named
/// `max_history_messages`, but tool-call and tool-result rows do not consume
/// independent slots.
pub(crate) fn trim_conversation_to_recent_turns(
    history: Vec<ConversationMessage>,
    max_turns: usize,
    has_leading_breadcrumb: bool,
) -> TurnCountTrimResult<ConversationMessage> {
    let first_non_system = history
        .iter()
        .position(|message| !is_conversation_system(message));
    let breadcrumb_index = first_non_system.filter(|_| has_leading_breadcrumb);
    let synthetic_messages = usize::from(breadcrumb_index.is_some());
    let total_turns = history
        .iter()
        .enumerate()
        .filter(|(index, message)| {
            is_conversation_turn_boundary(message, Some(*index) == breadcrumb_index)
        })
        .count();
    if total_turns <= max_turns || total_turns <= 1 {
        return TurnCountTrimResult {
            history,
            dropped_messages: 0,
            dropped_turns: 0,
            kept_turns: total_turns,
            trimmed: false,
        };
    }

    let mut system = Vec::new();
    let mut body = Vec::new();
    for message in history {
        if is_conversation_system(&message) {
            system.push(message);
        } else {
            body.push(message);
        }
    }

    let boundaries: Vec<usize> = body
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            is_conversation_turn_boundary(message, has_leading_breadcrumb && index == 0)
                .then_some(index)
        })
        .collect();

    let kept_turns = max_turns.max(1).min(boundaries.len());
    let dropped_turns = boundaries.len() - kept_turns;
    let first_kept = boundaries[dropped_turns];

    let dropped_messages = first_kept - synthetic_messages;
    system.extend(body.into_iter().skip(first_kept));
    TurnCountTrimResult {
        history: system,
        dropped_messages,
        dropped_turns,
        kept_turns,
        trimmed: true,
    }
}

fn is_turn_boundary(msg: &ChatMessage) -> bool {
    msg.role == "user"
        && !msg
            .content
            .starts_with(zeroclaw_api::tool_carrier::TOOL_RESULTS_PREFIX)
}

fn is_system(msg: &ChatMessage) -> bool {
    msg.role == "system"
}

/// Drop oldest whole turns until the history fits `budget_tokens`, always
/// keeping leading system messages and at least the most recent whole turn.
/// When `budget_tokens` is zero the history is returned untouched.
pub fn trim_to_recent_turns(history: Vec<ChatMessage>, budget_tokens: usize) -> TrimResult {
    trim_to_recent_turns_with_crumb(history, budget_tokens, false)
}

/// Crumb-aware variant of `trim_to_recent_turns`: when `crumb_present` is
/// true the population is assumed to carry the synthetic breadcrumb immediately
/// after the leading system messages. That breadcrumb is never counted as a
/// turn boundary, never dropped, and never double-counted in
/// `dropped_messages`. The owner flag decides, not message text.
pub fn trim_to_recent_turns_with_crumb(
    history: Vec<ChatMessage>,
    budget_tokens: usize,
    crumb_present: bool,
) -> TrimResult {
    let tokens_before = estimate_history_tokens(&history);
    let leading_system = history.iter().take_while(|m| is_system(m)).count();
    let crumb_offset = usize::from(crumb_present && history.len() > leading_system);
    // `total_turns` excludes the synthetic crumb so `kept_turns` stays
    // breadcrumb-aware. When the flag is set the crumb is at
    // `leading_system` by contract, so the body starts after it.
    let body_start = leading_system + crumb_offset;
    let body = if body_start <= history.len() {
        &history[body_start..]
    } else {
        &[][..]
    };
    let total_turns = body.iter().filter(|m| is_turn_boundary(m)).count();
    if budget_tokens == 0 || tokens_before <= budget_tokens {
        return TrimResult {
            history,
            dropped_messages: 0,
            dropped_turns: 0,
            kept_turns: total_turns,
            tokens_before,
            tokens_after: tokens_before,
            trimmed: false,
        };
    }

    let prefix: Vec<ChatMessage> = history[..body_start].to_vec();

    let boundaries: Vec<usize> = body
        .iter()
        .enumerate()
        .filter(|(_, message)| is_turn_boundary(message))
        .map(|(i, _)| i)
        .collect();

    if boundaries.len() <= 1 {
        return TrimResult {
            history,
            dropped_messages: 0,
            dropped_turns: 0,
            kept_turns: total_turns,
            tokens_before,
            tokens_after: tokens_before,
            trimmed: false,
        };
    }

    let mut start = 0usize;
    for &b in boundaries.iter().take(boundaries.len() - 1) {
        let candidate_start = next_boundary_after(&boundaries, b);
        let mut probe = prefix.clone();
        probe.extend_from_slice(&body[candidate_start..]);
        start = candidate_start;
        if estimate_history_tokens(&probe) <= budget_tokens {
            break;
        }
    }

    if start == 0 {
        return TrimResult {
            history,
            dropped_messages: 0,
            dropped_turns: 0,
            kept_turns: total_turns,
            tokens_before,
            tokens_after: tokens_before,
            trimmed: false,
        };
    }

    let dropped_messages = start;
    let dropped_turns = boundaries.iter().filter(|&&b| b < start).count();
    let mut kept = prefix;
    kept.extend_from_slice(&body[start..]);
    let kept_turns = total_turns - dropped_turns;
    let tokens_after = estimate_history_tokens(&kept);

    TrimResult {
        history: kept,
        dropped_messages,
        dropped_turns,
        kept_turns,
        tokens_before,
        tokens_after,
        trimmed: true,
    }
}

/// Keep at most `max_turns` recent provider-facing turns. This is the
/// count-based companion to [`trim_to_recent_turns`]; both use identical turn
/// boundaries and always retain the newest complete turn.
pub(crate) fn trim_to_recent_turn_count(
    history: Vec<ChatMessage>,
    max_turns: usize,
    has_leading_breadcrumb: bool,
) -> TurnCountTrimResult<ChatMessage> {
    let total_turns = count_turns(&history).saturating_sub(usize::from(has_leading_breadcrumb));
    if total_turns <= max_turns || total_turns <= 1 {
        return TurnCountTrimResult {
            history,
            dropped_messages: 0,
            dropped_turns: 0,
            kept_turns: total_turns,
            trimmed: false,
        };
    }

    let leading_system = history
        .iter()
        .take_while(|message| is_system(message))
        .count();
    let system = history[..leading_system].to_vec();
    let body = &history[leading_system..];
    let breadcrumb_index = has_leading_breadcrumb.then_some(0);
    let boundaries: Vec<usize> = body
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            (is_turn_boundary(message) && Some(index) != breadcrumb_index).then_some(index)
        })
        .collect();
    let kept_turns = max_turns.max(1).min(boundaries.len());
    let dropped_turns = boundaries.len() - kept_turns;
    let first_kept = boundaries[dropped_turns];

    let mut kept = system;
    if has_leading_breadcrumb {
        kept.push(body[0].clone());
    }
    kept.extend_from_slice(&body[first_kept..]);
    TurnCountTrimResult {
        history: kept,
        dropped_messages: first_kept - usize::from(has_leading_breadcrumb),
        dropped_turns,
        kept_turns,
        trimmed: true,
    }
}

pub fn trim_to_reported_budget(
    history: Vec<ChatMessage>,
    budget_tokens: usize,
    reported_input_tokens: usize,
    // Estimated token count of the exact message population that produced
    // `reported_input_tokens` (the pre-request `prepared_messages`, before any
    // assistant/tool-result output for this iteration was appended to
    // `history`). Scaling the selection target against this measured population
    // keeps it consistent with the calibration ratio used for `tokens_after`;
    // re-estimating the larger post-append `history` here would select more
    // retention than the calibration justifies and let the final history
    // overrun the budget.
    reported_population_estimated: usize,
    tool_schema_tokens: usize,
) -> TrimResult {
    trim_to_reported_budget_with_crumb(
        history,
        budget_tokens,
        reported_input_tokens,
        reported_population_estimated,
        tool_schema_tokens,
        false,
    )
}

/// Crumb-aware variant: when `crumb_present` is true the population carries
/// the synthetic breadcrumb immediately after leading system messages.
/// The breadcrumb is never counted as a turn and never dropped.
pub fn trim_to_reported_budget_with_crumb(
    history: Vec<ChatMessage>,
    budget_tokens: usize,
    reported_input_tokens: usize,
    reported_population_estimated: usize,
    tool_schema_tokens: usize,
    crumb_present: bool,
) -> TrimResult {
    let estimated = reported_population_estimated;
    if budget_tokens == 0 || reported_input_tokens <= budget_tokens || estimated == 0 {
        let total_turns = count_turns(&history).saturating_sub(usize::from(crumb_present));
        return TrimResult {
            tokens_before: reported_input_tokens,
            tokens_after: reported_input_tokens,
            history,
            dropped_messages: 0,
            dropped_turns: 0,
            kept_turns: total_turns,
            trimmed: false,
        };
    }
    // Native tool schemas are constant across a trim, so reserve them inside
    // the scaled total and trim the history portion to what remains: the
    // retained history plus tools then fits the budget when the reported
    // count is faithful.
    let target_total =
        (budget_tokens as u128 * estimated as u128 / reported_input_tokens as u128).max(1) as usize;
    let scaled = target_total.saturating_sub(tool_schema_tokens).max(1);
    let result = trim_to_recent_turns_with_crumb(history, scaled, crumb_present);
    let ratio = reported_input_tokens as f64 / estimated as f64;
    TrimResult {
        tokens_before: reported_input_tokens,
        tokens_after: ((result.tokens_after + tool_schema_tokens) as f64 * ratio).round() as usize,
        ..result
    }
}

fn next_boundary_after(boundaries: &[usize], current: usize) -> usize {
    boundaries
        .iter()
        .copied()
        .find(|&b| b > current)
        .unwrap_or(current)
}

pub(crate) fn count_turns(history: &[ChatMessage]) -> usize {
    history.iter().filter(|m| is_turn_boundary(m)).count()
}

/// Provider-facing population derived from a history whose older turns lost
/// their tool context. `source_rows[i]` is the index, in the input history, of
/// the row `messages[i]` stands for; a tool-exchange summary stands for the
/// assistant row that issued the calls it summarises. The input is never
/// modified, so the working history and every transcript an owner persists
/// from it keep all their rows.
#[derive(Debug, Clone, Default)]
pub(crate) struct CollapsedToolContext {
    pub messages: Vec<ChatMessage>,
    pub source_rows: Vec<usize>,
    /// Tool-call and tool-result rows that `messages` leaves out.
    pub dropped_messages: usize,
    pub collapsed_turns: usize,
}

impl CollapsedToolContext {
    fn keep(&mut self, source_row: usize, message: ChatMessage) {
        self.messages.push(message);
        self.source_rows.push(source_row);
    }

    fn identity(history: &[ChatMessage]) -> Self {
        Self {
            messages: history.to_vec(),
            source_rows: (0..history.len()).collect(),
            dropped_messages: 0,
            collapsed_turns: 0,
        }
    }
}

/// A closing reply is an assistant row that issued no native tool calls; an
/// assistant row that did is a carrier whose results may never have arrived.
fn is_closing_reply(message: &ChatMessage) -> bool {
    message.role == "assistant"
        && crate::agent::history_pruner::extract_assistant_tool_call_ids(&message.content).is_none()
}

fn count_tool_calls(rows: &[ChatMessage]) -> usize {
    let from_carriers: usize = rows
        .iter()
        .filter(|m| m.role == "assistant")
        .filter_map(|m| crate::agent::history_pruner::extract_assistant_tool_call_ids(&m.content))
        .map(|ids| ids.len())
        .sum();
    if from_carriers > 0 {
        return from_carriers;
    }
    rows.iter()
        .filter(|m| {
            m.role == "tool"
                || (m.role == "user"
                    && m.content
                        .starts_with(zeroclaw_api::tool_carrier::TOOL_RESULTS_PREFIX))
        })
        .count()
        .max(1)
}

/// Build the provider-facing population in which every turn older than the
/// newest `keep_prior_turns + 1` turns keeps its user prompt and closing
/// assistant reply but loses its tool-call and tool-result rows. The running
/// turn is always sent whole because the model needs its own results, so `0`
/// keeps tool context for the running turn only. Leading system messages and
/// the breadcrumb are copied through. A turn collapses as a unit: the assistant
/// row that issued the calls is replaced by one `[Tool exchange: …]` summary
/// row (the marker providers already recognise) and the rows that answered it
/// are left out, so no orphan call or result is created. A turn that ended on a
/// call or a result keeps only its prompt and the summary.
pub(crate) fn collapse_tool_context_older_than(
    history: &[ChatMessage],
    keep_prior_turns: usize,
    crumb_present: bool,
) -> CollapsedToolContext {
    let leading_system = history.iter().take_while(|m| is_system(m)).count();
    let body_start = leading_system + usize::from(crumb_present);
    let boundaries: Vec<usize> = history
        .iter()
        .enumerate()
        .skip(body_start)
        .filter(|(_, m)| opens_retention_turn(m))
        .map(|(i, _)| i)
        .collect();
    let protected = keep_prior_turns.saturating_add(1);
    if boundaries.len() <= protected {
        return CollapsedToolContext::identity(history);
    }
    let mut out = CollapsedToolContext::default();
    let mut cursor = 0;
    for turn in 0..boundaries.len() - protected {
        let boundary = boundaries[turn];
        for (row, message) in history.iter().enumerate().take(boundary + 1).skip(cursor) {
            out.keep(row, message.clone());
        }
        let span_start = boundary + 1;
        let span_end = boundaries[turn + 1];
        let closing = (span_end > span_start && is_closing_reply(&history[span_end - 1]))
            .then_some(span_end - 1);
        let drain_end = closing.unwrap_or(span_end);
        if drain_end > span_start {
            let drained = &history[span_start..drain_end];
            out.dropped_messages += drained.len();
            out.collapsed_turns += 1;
            if history[span_start].role == "assistant" {
                out.keep(
                    span_start,
                    ChatMessage::assistant(ChatMessage::pruned_tool_exchange_summary(
                        count_tool_calls(drained),
                    )),
                );
            }
        }
        if let Some(row) = closing {
            out.keep(row, history[row].clone());
        }
        cursor = span_end;
    }
    for (row, message) in history.iter().enumerate().skip(cursor) {
        out.keep(row, message.clone());
    }
    out
}

/// Drop the oldest whole turn (after leading system messages and an optional
/// breadcrumb), preserving the most recent whole turn and the system prefix.
/// `crumb_present` is the OWNER's authoritative record that the population
/// carries the synthetic trim breadcrumb — never inferred from message text,
/// so a genuine user turn that happens to equal the localized breadcrumb
/// string keeps its turn-boundary role regardless of locale. Returns how many
/// messages were dropped — zero when only the newest turn remains, which the
/// caller treats as the unsatisfiable floor rather than silently claiming the
/// history fits.
pub(crate) fn drop_oldest_whole_turn(history: &mut Vec<ChatMessage>, crumb_present: bool) -> usize {
    let leading_system = history.iter().take_while(|m| is_system(m)).count();
    let body_start = leading_system + usize::from(crumb_present);
    let body = &history[body_start..];
    let boundaries: Vec<usize> = body
        .iter()
        .enumerate()
        .filter(|(_, m)| is_turn_boundary(m))
        .map(|(i, _)| body_start + i)
        .collect();
    if boundaries.len() <= 1 {
        return 0;
    }
    let drop_end = next_boundary_after(&boundaries, boundaries[0]);
    let dropped = drop_end - body_start;
    history.drain(body_start..drop_end);
    dropped
}

/// Front breadcrumb injected after the system messages so the model SEES that
/// earlier turns were cut and cannot confabulate dropped work as present.
pub fn breadcrumb() -> ChatMessage {
    ChatMessage::user(crate::i18n::get_required_cli_string("history-trim-breadcrumb").as_str())
}

/// Insert the trim breadcrumb after the leading system messages unless the
/// owner's `crumb_present` record says one is already sitting there. Returns
/// whether a breadcrumb is present after the call, so the caller can store it
/// as the population's authoritative provenance instead of re-inferring it
/// from text later.
pub fn insert_breadcrumb_deduped(history: &mut Vec<ChatMessage>, crumb_present: bool) -> bool {
    if crumb_present {
        return true;
    }
    let system_count = history.iter().take_while(|m| is_system(m)).count();
    history.insert(system_count, breadcrumb());
    true
}

/// Insert the trim breadcrumb into structured history after leading system
/// messages. The owning Agent tracks whether a synthetic breadcrumb exists.
pub(crate) fn insert_conversation_breadcrumb(history: &mut Vec<ConversationMessage>) {
    let system_count = history
        .iter()
        .take_while(|message| is_conversation_system(message))
        .count();
    history.insert(system_count, ConversationMessage::Chat(breadcrumb()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_providers::{ToolCall, ToolResultMessage};

    fn sys(c: &str) -> ChatMessage {
        ChatMessage::system(c)
    }
    fn user(c: &str) -> ChatMessage {
        ChatMessage::user(c)
    }
    fn asst(c: &str) -> ChatMessage {
        ChatMessage::assistant(c)
    }
    fn tool(c: &str) -> ChatMessage {
        ChatMessage::tool(c)
    }

    /// A declared native tool-result carrier for one image: the envelope the
    /// runtime writes, with the attachment at the fixed position.
    fn image_tool_carrier(target: &str) -> String {
        let attachments = vec![zeroclaw_api::media::RenderedMarker {
            target: target.to_string(),
            kind: zeroclaw_api::media::MarkerKind::Image,
        }];
        serde_json::json!({
            "tool_call_id": "call_image",
            "content": "",
            "attachments": zeroclaw_api::tool_carrier::render_native_attachments(&attachments),
        })
        .to_string()
    }

    fn conversation_system(content: &str) -> ConversationMessage {
        ConversationMessage::Chat(ChatMessage::system(content))
    }

    fn conversation_user(content: &str) -> ConversationMessage {
        ConversationMessage::Chat(ChatMessage::user(content))
    }

    fn conversation_assistant(content: &str) -> ConversationMessage {
        ConversationMessage::Chat(ChatMessage::assistant(content))
    }

    fn push_tool_exchange(history: &mut Vec<ConversationMessage>, index: usize) {
        let id = format!("call-{index}");
        history.push(ConversationMessage::AssistantToolCalls {
            text: Some(format!("calling tool {index}")),
            tool_calls: vec![ToolCall {
                id: id.clone(),
                name: "shell".into(),
                arguments: "{}".into(),
                extra_content: None,
            }],
            reasoning_content: None,
        });
        history.push(ConversationMessage::ToolResults(vec![ToolResultMessage {
            tool_call_id: id,
            content: format!("result {index}"),
            tool_name: "shell".into(),
        }]));
    }

    fn assert_structural_tool_pairs(history: &[ConversationMessage]) {
        for (index, message) in history.iter().enumerate() {
            match message {
                ConversationMessage::AssistantToolCalls { tool_calls, .. } => {
                    let Some(ConversationMessage::ToolResults(results)) = history.get(index + 1)
                    else {
                        panic!("assistant tool calls must be followed by tool results");
                    };
                    assert_eq!(results.len(), tool_calls.len());
                    assert_eq!(results[0].tool_call_id, tool_calls[0].id);
                }
                ConversationMessage::ToolResults(_) => assert!(matches!(
                    index
                        .checked_sub(1)
                        .and_then(|previous| history.get(previous)),
                    Some(ConversationMessage::AssistantToolCalls { .. })
                )),
                ConversationMessage::Chat(_) => {}
            }
        }
    }

    #[test]
    fn trim_conversation_to_recent_turns_keeps_single_tool_heavy_turn_over_cap() {
        let mut history = vec![conversation_user("run the workflow")];
        for index in 0..31 {
            push_tool_exchange(&mut history, index);
        }
        history.push(conversation_assistant("workflow complete"));
        assert_eq!(history.len(), 64);

        let result = trim_conversation_to_recent_turns(history, 50, false);

        assert!(!result.trimmed);
        assert_eq!(result.dropped_messages, 0);
        assert_eq!(result.dropped_turns, 0);
        assert_eq!(result.kept_turns, 1);
        assert_eq!(result.history.len(), 64);
        assert!(matches!(
            result.history.first(),
            Some(ConversationMessage::Chat(message))
                if message.role == "user" && message.content == "run the workflow"
        ));
        assert!(matches!(
            result.history.last(),
            Some(ConversationMessage::Chat(message))
                if message.role == "assistant" && message.content == "workflow complete"
        ));
        assert_structural_tool_pairs(&result.history);
    }

    #[test]
    fn trim_conversation_to_recent_turns_drops_old_turn_at_one_turn_limit() {
        let mut history = vec![
            conversation_user("old request"),
            conversation_assistant("old answer"),
            conversation_user("new request"),
        ];
        for index in 0..25 {
            push_tool_exchange(&mut history, index);
        }
        history.push(conversation_assistant("new answer"));

        let result = trim_conversation_to_recent_turns(history, 1, false);

        assert!(result.trimmed);
        assert_eq!(result.dropped_turns, 1);
        assert_eq!(result.dropped_messages, 2);
        assert_eq!(result.kept_turns, 1);
        assert!(matches!(
            result.history.first(),
            Some(ConversationMessage::Chat(message))
                if message.role == "user" && message.content == "new request"
        ));
        assert!(matches!(
            result.history.last(),
            Some(ConversationMessage::Chat(message))
                if message.role == "assistant" && message.content == "new answer"
        ));
        assert_structural_tool_pairs(&result.history);
    }

    #[test]
    fn trim_conversation_to_recent_turns_counts_turns_not_tool_rows() {
        let mut history = vec![conversation_system("system")];
        for turn in 0..60 {
            history.push(conversation_user(&format!("request {turn}")));
            for tool in 0..3 {
                push_tool_exchange(&mut history, turn * 10 + tool);
            }
            history.push(conversation_assistant(&format!("answer {turn}")));
        }

        let result = trim_conversation_to_recent_turns(history, 50, false);

        assert!(result.trimmed);
        assert_eq!(result.dropped_turns, 10);
        assert_eq!(result.kept_turns, 50);
        assert!(matches!(
            result.history.get(1),
            Some(ConversationMessage::Chat(message))
                if message.role == "user" && message.content == "request 10"
        ));
        assert!(matches!(
            result.history.last(),
            Some(ConversationMessage::Chat(message))
                if message.role == "assistant" && message.content == "answer 59"
        ));
        assert_structural_tool_pairs(&result.history);
    }

    #[test]
    fn trim_conversation_to_recent_turns_zero_cap_preserves_newest_complete_turn() {
        let history = vec![
            conversation_user("old request"),
            conversation_assistant("old answer"),
            conversation_user("new request"),
            conversation_assistant("new answer"),
        ];

        let result = trim_conversation_to_recent_turns(history, 0, false);

        assert!(result.trimmed);
        assert_eq!(result.dropped_messages, 2);
        assert_eq!(result.dropped_turns, 1);
        assert_eq!(result.kept_turns, 1);
        assert_eq!(result.history.len(), 2);
        assert!(matches!(
            result.history.first(),
            Some(ConversationMessage::Chat(message))
                if message.role == "user" && message.content == "new request"
        ));
    }

    #[test]
    fn trim_conversation_to_recent_turns_excludes_and_normalizes_system_messages() {
        let history = vec![
            conversation_system("primary system"),
            conversation_user("old request"),
            conversation_assistant("old answer"),
            conversation_system("late system"),
            conversation_user("new request"),
            conversation_assistant("new answer"),
        ];

        let result = trim_conversation_to_recent_turns(history, 1, false);

        assert!(result.trimmed);
        assert_eq!(result.dropped_messages, 2);
        assert_eq!(result.dropped_turns, 1);
        assert_eq!(result.kept_turns, 1);
        assert_eq!(result.history.len(), 4);
        assert!(matches!(
            &result.history[..],
            [
                ConversationMessage::Chat(first_system),
                ConversationMessage::Chat(second_system),
                ConversationMessage::Chat(user),
                ConversationMessage::Chat(assistant),
            ] if first_system.role == "system"
                && first_system.content == "primary system"
                && second_system.role == "system"
                && second_system.content == "late system"
                && user.role == "user"
                && user.content == "new request"
                && assistant.role == "assistant"
                && assistant.content == "new answer"
        ));
    }

    #[test]
    fn trim_conversation_to_recent_turns_under_cap_preserves_late_system_order() {
        let history = vec![
            conversation_user("request"),
            conversation_assistant("answer"),
            conversation_system("late system"),
        ];
        let original = serde_json::to_value(&history).expect("fixture should serialize");

        let result = trim_conversation_to_recent_turns(history, 3, false);

        assert!(!result.trimmed);
        assert_eq!(result.dropped_messages, 0);
        assert_eq!(result.dropped_turns, 0);
        assert_eq!(result.kept_turns, 1);
        assert_eq!(
            serde_json::to_value(&result.history).expect("result should serialize"),
            original,
            "under-cap history must remain shape- and order-identical"
        );
    }

    #[test]
    fn trim_conversation_to_recent_turns_non_system_at_cap_preserves_late_system_order() {
        let history = vec![
            conversation_user("request"),
            conversation_assistant("answer"),
            conversation_system("late system"),
        ];
        let original = serde_json::to_value(&history).expect("fixture should serialize");

        let result = trim_conversation_to_recent_turns(history, 1, false);

        assert!(!result.trimmed);
        assert_eq!(result.dropped_messages, 0);
        assert_eq!(result.dropped_turns, 0);
        assert_eq!(result.kept_turns, 1);
        assert_eq!(
            serde_json::to_value(&result.history).expect("result should serialize"),
            original,
            "system messages must not create cap pressure or change history order"
        );
    }

    #[test]
    fn trim_conversation_to_recent_turns_leaves_history_without_user_boundary_unchanged() {
        let mut history = vec![conversation_system("system")];
        push_tool_exchange(&mut history, 0);
        history.push(conversation_assistant("done"));
        let original = serde_json::to_value(&history).expect("fixture should serialize");

        let result = trim_conversation_to_recent_turns(history, 1, false);

        assert!(!result.trimmed);
        assert_eq!(result.dropped_messages, 0);
        assert_eq!(result.dropped_turns, 0);
        assert_eq!(result.kept_turns, 0);
        assert_eq!(
            serde_json::to_value(&result.history).expect("result should serialize"),
            original
        );
        assert_structural_tool_pairs(&result.history);
    }

    #[test]
    fn trim_conversation_to_recent_turns_counts_later_user_matching_breadcrumb() {
        let breadcrumb_content = breadcrumb().content;
        let history = vec![
            conversation_system("system"),
            conversation_user("old request"),
            conversation_assistant("old answer"),
            conversation_user(&breadcrumb_content),
            conversation_assistant("new answer"),
        ];

        let result = trim_conversation_to_recent_turns(history, 1, false);

        assert!(result.trimmed);
        assert_eq!(result.dropped_messages, 2);
        assert_eq!(result.dropped_turns, 1);
        assert_eq!(result.kept_turns, 1);
        assert!(matches!(
            &result.history[..],
            [
                ConversationMessage::Chat(system),
                ConversationMessage::Chat(user),
                ConversationMessage::Chat(assistant),
            ] if system.role == "system"
                && user.role == "user"
                && user.content == breadcrumb_content
                && assistant.role == "assistant"
                && assistant.content == "new answer"
        ));
    }

    #[test]
    fn trim_conversation_to_recent_turns_does_not_mistake_first_user_for_breadcrumb() {
        let breadcrumb_content = breadcrumb().content;
        let history = vec![
            conversation_system("system"),
            conversation_user(&breadcrumb_content),
            conversation_assistant("old answer"),
            conversation_user("new request"),
            conversation_assistant("new answer"),
        ];

        let result = trim_conversation_to_recent_turns(history, 1, false);

        assert!(result.trimmed);
        assert_eq!(result.dropped_messages, 2);
        assert_eq!(result.dropped_turns, 1);
        assert_eq!(result.kept_turns, 1);
        assert!(matches!(
            &result.history[..],
            [
                ConversationMessage::Chat(system),
                ConversationMessage::Chat(user),
                ConversationMessage::Chat(assistant),
            ] if system.role == "system"
                && user.role == "user"
                && user.content == "new request"
                && assistant.role == "assistant"
                && assistant.content == "new answer"
        ));
    }

    #[test]
    fn trim_conversation_to_recent_turns_drops_minimum_oldest_turns_for_turn_cap() {
        let history = vec![
            conversation_user("old request"),
            conversation_assistant("old answer"),
            conversation_user("middle request"),
            conversation_assistant("middle answer"),
            conversation_user("new request"),
            conversation_assistant("new answer"),
        ];

        let result = trim_conversation_to_recent_turns(history, 2, false);

        assert!(result.trimmed);
        assert_eq!(result.dropped_messages, 2);
        assert_eq!(result.dropped_turns, 1);
        assert_eq!(result.kept_turns, 2);
        assert_eq!(result.history.len(), 4);
        assert!(matches!(
            result.history.first(),
            Some(ConversationMessage::Chat(message))
                if message.role == "user" && message.content == "middle request"
        ));
        assert!(matches!(
            result.history.last(),
            Some(ConversationMessage::Chat(message))
                if message.role == "assistant" && message.content == "new answer"
        ));
    }

    #[test]
    fn trim_conversation_to_recent_turns_second_trim_excludes_leading_breadcrumb() {
        let mut history = vec![
            conversation_system("system"),
            ConversationMessage::Chat(breadcrumb()),
            conversation_user("old request"),
            conversation_assistant("old answer"),
            conversation_user("middle request"),
            conversation_assistant("middle answer"),
        ];

        let first = trim_conversation_to_recent_turns(history, 2, true);
        assert!(
            !first.trimmed,
            "a synthetic breadcrumb must not push an exactly-at-cap body over the limit"
        );

        history = first.history;
        history.push(conversation_user("new request"));
        history.push(conversation_assistant("new answer"));
        let mut second = trim_conversation_to_recent_turns(history, 2, true);

        assert!(second.trimmed);
        assert_eq!(second.dropped_messages, 2);
        assert_eq!(second.dropped_turns, 1);
        assert_eq!(second.kept_turns, 2);
        assert_eq!(second.history.len(), 5);
        insert_conversation_breadcrumb(&mut second.history);
        let breadcrumb_content = breadcrumb().content;
        assert_eq!(
            second
                .history
                .iter()
                .filter(|message| matches!(
                    message,
                    ConversationMessage::Chat(chat)
                        if chat.role == "user" && chat.content == breadcrumb_content
                ))
                .count(),
            1
        );
        assert!(matches!(
            second.history.get(2),
            Some(ConversationMessage::Chat(message))
                if message.role == "user" && message.content == "middle request"
        ));
        assert!(matches!(
            second.history.last(),
            Some(ConversationMessage::Chat(message))
                if message.role == "assistant" && message.content == "new answer"
        ));
    }

    #[test]
    fn under_budget_is_untouched() {
        let h = vec![sys("s"), user("hi"), asst("yo")];
        let n = h.len();
        let r = trim_to_recent_turns_with_crumb(h, 1_000_000, false);
        assert!(!r.trimmed);
        assert_eq!(r.history.len(), n);
        assert_eq!(r.dropped_turns, 0);
    }

    #[test]
    fn zero_budget_is_untouched() {
        let h = vec![sys("s"), user("hi"), asst("yo")];
        let n = h.len();
        let r = trim_to_recent_turns_with_crumb(h, 0, false);
        assert!(!r.trimmed);
        assert_eq!(r.history.len(), n);
    }

    #[test]
    fn drops_oldest_whole_turns_keeps_system() {
        let big = "x".repeat(2000);
        let h = vec![
            sys("system"),
            user(&format!("turn1 {big}")),
            asst("a1"),
            user(&format!("turn2 {big}")),
            asst("a2"),
            user("turn3 short"),
            asst("a3"),
        ];
        let r = trim_to_recent_turns_with_crumb(h, 200, false);
        assert!(r.trimmed);
        assert_eq!(r.history[0].role, "system");
        assert!(r.dropped_turns >= 1);
        assert!(r.kept_turns >= 1);
        // most recent turn survived
        assert!(r.history.iter().any(|m| m.content.contains("turn3 short")));
    }

    #[test]
    fn token_accounting_is_populated_and_coherent() {
        let big = "x".repeat(2000);
        let h = vec![
            sys("system"),
            user(&format!("turn1 {big}")),
            asst("a1"),
            user(&format!("turn2 {big}")),
            asst("a2"),
            user("turn3 short"),
            asst("a3"),
        ];
        let r = trim_to_recent_turns_with_crumb(h, 200, false);
        assert!(r.trimmed);
        // the sick-log fields must reflect a real reduction
        assert!(r.tokens_before > r.tokens_after);
        assert!(r.tokens_before > 200, "before should exceed budget");
        assert!(
            r.tokens_before.saturating_sub(r.tokens_after) > 0,
            "reclaimed must be positive when trimmed"
        );
    }

    #[test]
    fn untouched_reports_equal_before_after() {
        let h = vec![sys("s"), user("hi"), asst("yo")];
        let r = trim_to_recent_turns_with_crumb(h, 1_000_000, false);
        assert!(!r.trimmed);
        assert_eq!(r.tokens_before, r.tokens_after);
    }

    #[test]
    fn never_splits_tool_pair() {
        let big = "y".repeat(2000);
        let h = vec![
            sys("system"),
            user(&format!("turn1 {big}")),
            asst("calling tool"),
            tool("tool_use_1 result"),
            user("[Tool results]\nmore"),
            asst("done1"),
            user("turn2 short"),
            asst("done2"),
        ];
        let r = trim_to_recent_turns_with_crumb(h, 150, false);
        assert!(r.trimmed);
        // a tool row must never appear without its preceding assistant turn-head
        let mut seen_user = false;
        for m in &r.history {
            if is_turn_boundary(m) {
                seen_user = true;
            }
            if m.role == "tool" {
                assert!(seen_user, "tool result kept without its turn head");
            }
        }
    }

    /// Known limit of prefix provenance: the trimmer identifies a prompt-mode
    /// tool-result row as a `user` row whose content starts with
    /// `[Tool results]`, so a genuine user message that happens to begin with
    /// that prefix is grouped with the preceding assistant turn instead of
    /// opening a turn of its own. This pins that grouping: the row is dropped
    /// together with that turn and never counted as an independent turn.
    #[test]
    fn user_text_matching_tool_results_prefix_is_dropped_with_its_turn() {
        let history = vec![
            sys("system"),
            user("old request"),
            asst("old answer"),
            user("[Tool results]\nthis is literal user text"),
            asst("middle answer"),
            user("new request"),
            asst("new answer"),
        ];

        let result = trim_to_recent_turn_count(history, 1, false);

        assert!(result.trimmed);
        assert_eq!(
            result.dropped_turns, 1,
            "the prefix-matching row must not count as a turn of its own"
        );
        assert_eq!(result.kept_turns, 1);
        assert_eq!(result.dropped_messages, 4);
        assert!(
            !result
                .history
                .iter()
                .any(|message| message.content == "[Tool results]\nthis is literal user text"),
            "the prefix-matching row drops with its turn rather than surviving as its own"
        );
    }

    #[test]
    fn keeps_last_turn_even_if_over_budget() {
        let huge = "z".repeat(10_000);
        let h = vec![
            sys("system"),
            user("old"),
            asst("a"),
            user(&format!("recent {huge}")),
            asst("a2"),
        ];
        let r = trim_to_recent_turns_with_crumb(h, 50, false);
        // last turn alone exceeds budget; option a keeps it rather than nuking.
        assert!(r.kept_turns >= 1);
        assert!(r.history.iter().any(|m| m.content.contains("recent")));
    }

    #[test]
    fn breadcrumb_is_user_role() {
        assert_eq!(breadcrumb().role, "user");
    }

    #[test]
    fn trimmed_history_has_no_orphan_tool_calls() {
        use crate::agent::history_pruner::remove_orphaned_tool_messages;
        let big = "q".repeat(3000);
        let asst_call = |id: &str| {
            asst(
                &serde_json::json!({
                    "content": "",
                    "tool_calls": [{"id": id, "name": "file_read", "arguments": "{}"}]
                })
                .to_string(),
            )
        };
        let tool_res =
            |id: &str| tool(&serde_json::json!({"tool_call_id": id, "content": "ok"}).to_string());
        let h = vec![
            sys("system"),
            user(&format!("turn1 {big}")),
            asst_call("call_1"),
            tool_res("call_1"),
            asst("summary1"),
            user("turn2"),
            asst_call("call_2"),
            tool_res("call_2"),
            asst("summary2"),
        ];
        let r = trim_to_recent_turns_with_crumb(h, 200, false);
        assert!(r.trimmed, "oversized history must trim");
        let mut kept = r.history.clone();
        let swept = remove_orphaned_tool_messages(&mut kept);
        assert_eq!(
            swept.removed, 0,
            "whole-turn trim must leave zero orphan tool messages; the orphan \
             sweep (the anti-400 net) should find nothing to remove"
        );
        assert_eq!(kept.len(), r.history.len(), "no messages removed by sweep");
    }

    #[test]
    fn preserves_kept_tool_call_id_envelope_when_trimming_whole_turns() {
        let old_big = "old ".repeat(2000);
        let envelope = serde_json::json!({
            "tool_call_id": "call_1",
            "content": "raw tool output",
        });
        let h = vec![
            sys("system"),
            user(&format!("old turn {old_big}")),
            asst("old answer"),
            user("recent"),
            asst("calling tool"),
            tool(&envelope.to_string()),
            asst("done"),
        ];

        let r = trim_to_recent_turns_with_crumb(h, 200, false);

        assert!(r.trimmed, "oversized history must drop an old whole turn");
        assert_eq!(r.dropped_turns, 1);
        let kept_tool = r
            .history
            .iter()
            .find(|msg| msg.role == "tool")
            .expect("recent tool result should be kept");
        let kept_envelope: serde_json::Value =
            serde_json::from_str(&kept_tool.content).expect("tool content remains JSON");
        assert_eq!(
            kept_envelope
                .get("tool_call_id")
                .and_then(serde_json::Value::as_str),
            Some("call_1"),
        );
        assert_eq!(
            kept_envelope
                .get("content")
                .and_then(serde_json::Value::as_str),
            Some("raw tool output"),
        );
    }

    #[test]
    fn breadcrumb_inserts_after_leading_system() {
        let big = "w".repeat(3000);
        let h = vec![
            sys("sysA"),
            sys("sysB"),
            user(&format!("old {big}")),
            asst("a"),
            user("recent"),
            asst("a2"),
        ];
        let r = trim_to_recent_turns_with_crumb(h, 120, false);
        assert!(r.trimmed);
        let mut trimmed = r.history;
        let system_count = trimmed.iter().take_while(|m| m.role == "system").count();
        trimmed.insert(system_count, breadcrumb());
        assert_eq!(trimmed[0].role, "system");
        assert_eq!(trimmed[system_count].role, "user");
        assert!(
            trimmed[..system_count].iter().all(|m| m.role == "system"),
            "breadcrumb must sit after every leading system message"
        );
    }

    #[test]
    fn second_budget_trim_excludes_explicit_leading_breadcrumb_from_turn_counts() {
        let old = "old ".repeat(2_000);
        let history = vec![
            sys("system"),
            breadcrumb(),
            user(&old),
            asst("old answer"),
            user("new request"),
            asst("new answer"),
        ];

        let result = trim_to_recent_turns_with_crumb(history, 200, true);

        assert!(result.trimmed);
        assert_eq!(result.dropped_messages, 2);
        assert_eq!(result.dropped_turns, 1);
        assert_eq!(result.kept_turns, 1);
        assert!(result.history.iter().any(|m| m.content == "new request"));
    }

    #[test]
    fn second_count_trim_excludes_explicit_leading_breadcrumb_from_turn_counts() {
        let history = vec![
            sys("system"),
            breadcrumb(),
            user("old request"),
            asst("old answer"),
            user("new request"),
            asst("new answer"),
        ];

        let result = trim_to_recent_turn_count(history, 1, true);

        assert!(result.trimmed);
        assert_eq!(result.dropped_messages, 2);
        assert_eq!(result.dropped_turns, 1);
        assert_eq!(result.kept_turns, 1);
        assert!(result.history.iter().any(|m| m.content == "new request"));
    }

    #[test]
    fn reported_budget_trims_when_reported_exceeds_budget() {
        let big = "x".repeat(2000);
        let h = vec![
            sys("system"),
            user(&format!("turn1 {big}")),
            asst("a1"),
            user(&format!("turn2 {big}")),
            asst("a2"),
            user("turn3 short"),
            asst("a3"),
        ];
        let estimated = estimate_history_tokens(&h);
        let reported = estimated * 4;
        let budget = reported / 2;
        let r = trim_to_reported_budget(h, budget, reported, estimated, 0);
        assert!(
            r.trimmed,
            "must trim when provider-reported tokens exceed budget"
        );
        assert!(r.dropped_turns >= 1);
        assert!(r.history.iter().any(|m| m.content.contains("turn3 short")));
    }

    #[test]
    fn reported_budget_no_trim_when_real_tokens_fit() {
        let h = vec![sys("system"), user("hi"), asst("hello")];
        let estimated = estimate_history_tokens(&h);
        let r = trim_to_reported_budget(h, estimated * 4, estimated, estimated, 0);
        assert!(!r.trimmed);
    }

    #[test]
    fn reported_budget_trims_under_extreme_ratio() {
        let big = "x".repeat(4000);
        let h = vec![
            sys("system"),
            user(&format!("old {big}")),
            asst("a1"),
            user("recent short"),
            asst("a2"),
        ];
        let estimated = estimate_history_tokens(&h);
        let reported = estimated * 5000;
        let budget = reported / 100;
        let r = trim_to_reported_budget(h, budget, reported, estimated, 0);
        assert!(r.trimmed, "extreme ratio must still enforce, not no-op");
        assert!(r.history.iter().any(|m| m.content.contains("recent short")));
    }

    #[test]
    fn reported_budget_reserves_room_for_large_native_tool_schema() {
        let big = "x".repeat(2000);
        let h = vec![
            sys("system"),
            user(&format!("turn1 {big}")),
            asst("a1"),
            user(&format!("turn2 {big}")),
            asst("a2"),
            user("turn3 short"),
            asst("a3"),
        ];
        // A large native tool schema that a provider would serialize into the
        // request and count in `input_tokens` alongside the messages.
        let spec = crate::tools::ToolSpec::new(
            "large_schema_tool",
            "a tool with a very large parameter schema",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "payload": {
                        "type": "string",
                        "description": "big".repeat(300),
                    }
                }
            }),
        );
        let tool_tokens = crate::agent::history::estimate_tool_schema_tokens(&[spec]);
        assert!(
            tool_tokens > 0,
            "a large tool schema must contribute a nonzero token estimate"
        );

        // Faithful provider: reported == messages + tool schemas.
        let estimated = estimate_history_tokens(&h) + tool_tokens;
        let reported = estimated;
        let budget = reported / 2;
        assert!(budget > tool_tokens, "budget must leave headroom for tools");

        let r = trim_to_reported_budget(h, budget, reported, estimated, tool_tokens);
        assert!(
            r.trimmed,
            "must trim when reported population exceeds budget"
        );
        let kept_total = estimate_history_tokens(&r.history) + tool_tokens;
        assert!(
            kept_total <= budget,
            "retained history plus constant tool schemas must fit the budget (kept {kept_total}, budget {budget})"
        );
        assert_eq!(
            r.tokens_after, kept_total,
            "tokens_after must cover the full provider population, tool schemas included"
        );
        assert!(r.history.iter().any(|m| m.content.contains("turn3 short")));
    }

    #[test]
    fn reported_budget_calibrates_selection_against_the_measured_population() {
        // `reported_population_estimated` describes the population that produced
        // `reported` — the pre-request transcript. `history` here is the
        // already-appended transcript (the provider calls back with a larger
        // estimate after the assistant/tool output was added). The selection
        // target must scale from the MEASURED population, not from the fresher,
        // larger post-append estimate, or the retained set would exceed the
        // budget once calibrated.
        let big = "x".repeat(2000);
        // The measured (pre-request) population: several substantial turns so
        // there is plenty of room to trim toward the budget.
        let mut measured = vec![sys("system")];
        for i in 0..8 {
            measured.push(user(format!("m{i} {big}").as_str()));
            measured.push(asst(format!("a{i}").as_str()));
        }
        let reported = estimate_history_tokens(&measured) * 4;
        let budget = reported / 2;
        let measured_population_estimated = estimate_history_tokens(&measured);

        // Post-request append: this iteration's assistant output lands on the
        // same transcript `trim_to_reported_budget` sees, making it larger than
        // the measured population — but small enough that the newest whole turn
        // still fits the post-trim target (no oversized-turn exception).
        let appended = "y".repeat(400);
        let mut history = measured;
        history.push(ChatMessage::assistant(&appended));
        assert!(
            estimate_history_tokens(&history) > measured_population_estimated,
            "the appended output must make the post-append estimate the larger one"
        );

        let r =
            trim_to_reported_budget(history, budget, reported, measured_population_estimated, 0);
        assert!(r.trimmed, "must trim when reported exceeds budget");
        assert!(
            r.tokens_after <= budget,
            "selection must not outpace the calibration ratio: tokens_after {} > budget {budget}",
            r.tokens_after
        );
        // The retained history must also fit the budget under the measured
        // calibration ratio (the calibration's own check).
        let kept = estimate_history_tokens(&r.history);
        let calibrated = (kept as f64 * reported as f64
            / measured_population_estimated.max(1) as f64)
            .round() as u64;
        assert!(
            calibrated <= budget as u64,
            "retained history must respect the budget under the measured ratio \
             (calibrated {calibrated}, budget {budget})"
        );
    }

    #[test]
    fn insert_breadcrumb_deduped_does_not_stack() {
        let mut h = vec![sys("system"), user("turn1"), asst("a1")];
        let mut crumb_present = insert_breadcrumb_deduped(&mut h, false);
        let after_first = h.len();
        crumb_present = insert_breadcrumb_deduped(&mut h, crumb_present);
        assert_eq!(
            h.len(),
            after_first,
            "a second trim must not stack another breadcrumb behind the system block"
        );
        let crumbs = h
            .iter()
            .filter(|m| m.role == breadcrumb().role && m.content == breadcrumb().content)
            .count();
        assert_eq!(crumbs, 1);
        assert!(crumb_present);
        // A genuine first user turn equal to the breadcrumb string must not
        // be mistaken for an existing crumb: the OWNER record decides.
        let mut colliding = vec![sys("system"), user(&breadcrumb().content), asst("a1")];
        let inserted = insert_breadcrumb_deduped(&mut colliding, false);
        assert!(
            inserted,
            "the owner record says no crumb exists, so a fresh one is inserted"
        );
        let crumbs = colliding
            .iter()
            .filter(|m| m.role == breadcrumb().role && m.content == breadcrumb().content)
            .count();
        assert_eq!(
            crumbs, 2,
            "the real user turn stays untouched and the synthetic crumb is added"
        );
    }

    #[test]
    fn repeated_interactive_recovery_keeps_one_breadcrumb_and_real_turn_counts() {
        // Mirrors the interactive overflow-recovery sequence in
        // `agent::loop_`: on each provider context-overflow error it must
        // call the crumb-aware trim with the owner's current flag, then
        // insert the breadcrumb only if it isn't already present. A
        // crumb-blind call (the pre-fix bug) would treat an existing
        // synthetic breadcrumb as the oldest real user turn and drop it as
        // though real history had been removed.
        let big = "x".repeat(400);
        let mut history = vec![sys("system")];
        for i in 0..6 {
            history.push(user(&format!("turn {i} {big}")));
            history.push(asst(&format!("reply {i} {big}")));
        }
        let mut crumb_present = false;

        // First overflow: trims some real turns and inserts the crumb.
        let budget_after_first_trim = estimate_history_tokens(&history) / 2;
        let result = trim_to_recent_turns_with_crumb(
            std::mem::take(&mut history),
            budget_after_first_trim,
            crumb_present,
        );
        assert!(result.trimmed, "fixture must overflow the first budget");
        history = result.history;
        crumb_present = insert_breadcrumb_deduped(&mut history, crumb_present);
        assert!(crumb_present);
        let real_turns_after_first = history.iter().filter(|m| is_turn_boundary(m)).count() - /* crumb counts as a user turn boundary */ 1;

        // Second overflow on the already-recovered history: the crumb must
        // not be miscounted as a droppable real turn, and inserting again
        // must not stack a second marker.
        let budget_after_second_trim = estimate_history_tokens(&history) / 2;
        let result = trim_to_recent_turns_with_crumb(
            std::mem::take(&mut history),
            budget_after_second_trim,
            crumb_present,
        );
        history = result.history;
        crumb_present = insert_breadcrumb_deduped(&mut history, crumb_present);
        assert!(crumb_present);

        let crumbs = history
            .iter()
            .filter(|m| m.role == breadcrumb().role && m.content == breadcrumb().content)
            .count();
        assert_eq!(
            crumbs, 1,
            "repeated recovery must never stack a second synthetic breadcrumb"
        );
        assert!(
            result.kept_turns <= real_turns_after_first,
            "the second recovery must not report more kept real turns than existed \
             before it (kept_turns {}, real turns before {real_turns_after_first})",
            result.kept_turns
        );
    }

    #[test]
    fn insert_breadcrumb_deduped_sits_after_leading_system() {
        let mut h = vec![sys("s1"), sys("s2"), user("turn1"), asst("a1")];
        let has_breadcrumb = insert_breadcrumb_deduped(&mut h, false);
        assert_eq!(h[0].role, "system");
        assert_eq!(h[1].role, "system");
        assert_eq!(h[2].role, breadcrumb().role);
        assert_eq!(h[2].content, breadcrumb().content);
        assert!(has_breadcrumb);
    }

    #[test]
    fn five_image_tool_results_in_one_round_are_budgeted_as_images() {
        use crate::agent::history::IMAGE_TOKEN_ESTIMATE;

        let assistant_tool_calls = serde_json::json!({
            "content": "",
            "tool_calls": (0..5)
                .map(|index| {
                    serde_json::json!({
                        "id": format!("call_{index}"),
                        "name": "image_info",
                        "arguments": "{}",
                    })
                })
                .collect::<Vec<_>>(),
        })
        .to_string();

        let image_history = |tool_contents: Vec<String>| {
            vec![
                sys("system"),
                user(&format!("old turn {}", "x".repeat(8_000))),
                asst("old answer"),
                user("new turn"),
                asst(&assistant_tool_calls),
            ]
            .into_iter()
            .chain(tool_contents.into_iter().map(|content| tool(&content)))
            .collect::<Vec<ChatMessage>>()
        };

        // Path targets: five images coming back in one native-tool round as
        // declared carriers, the shape the runtime writes.
        let history = image_history(
            (0..5)
                .map(|index| image_tool_carrier(&format!("/tmp/slide-{index}.png")))
                .collect(),
        );
        assert!(
            estimate_history_tokens(&history) >= 5 * IMAGE_TOKEN_ESTIMATE,
            "five image tool results must be budgeted as five images"
        );

        let result = trim_to_recent_turns(history, 5 * IMAGE_TOKEN_ESTIMATE + 1_000);
        assert!(result.trimmed, "the old text turn must be dropped to fit");
        assert_eq!(result.dropped_turns, 1);
        assert!(
            !result
                .history
                .iter()
                .any(|m| m.content.contains("old turn")),
            "the old turn should be dropped"
        );
        assert!(
            result
                .history
                .iter()
                .any(|m| m.content.contains("new turn")),
            "the newest turn head must survive"
        );
        assert_eq!(
            result.history.iter().filter(|m| m.role == "tool").count(),
            5,
            "the newest round must keep all five image results whole"
        );

        // The same round as ~600 KB data-URI attachments: per-image pricing
        // keeps the history under a 20k budget, where per-byte pricing would
        // see ~150k tokens per result and throw the old turn away.
        let history = image_history(
            (0..5)
                .map(|_| {
                    image_tool_carrier(&format!("data:image/png;base64,{}", "A".repeat(600_000)))
                })
                .collect(),
        );
        let before = history.len();
        let result = trim_to_recent_turns(history, 20_000);
        assert!(
            !result.trimmed,
            "data-URI markers must price like the path form, not like bytes/4"
        );
        assert_eq!(result.dropped_turns, 0);
        assert_eq!(result.history.len(), before);
        assert!(
            result
                .history
                .iter()
                .any(|m| m.content.contains("old turn")),
            "nothing should be dropped when the estimate fits the budget"
        );
    }

    // Legacy path markers are text under the attachment-identity contract,
    // so a stale legacy carrier is delivered and priced as its bytes: the
    // marker syntax stays in the body and nothing is stripped. Thirty short
    // markers fit the 32k budget by bytes, so a tool round that only ever
    // quoted markers cannot evict a turn that fits.
    #[test]
    fn stale_tool_images_do_not_force_a_trim() {
        let markers: Vec<String> = (0..30)
            .map(|index| format!("[IMAGE:/tmp/stale-{index}.png]"))
            .collect();
        // The latest message is a genuine user turn, so the whole tool run is
        // stale: replay delivers it verbatim, and its price is its bytes.
        let history = vec![
            sys("s"),
            user("u"),
            asst("a"),
            tool(&markers.join("\n")),
            user("v"),
        ];

        let result = trim_to_recent_turns(history, 32_000);

        assert!(
            !result.trimmed,
            "stale tool images are text, not per-image charges, and must not force a trim"
        );
        assert_eq!(result.dropped_turns, 0);
        assert_eq!(result.history.len(), 5);
    }

    #[test]
    fn drop_oldest_whole_turn_uses_owner_crumb_record_not_text_inference() {
        // The history's first non-system turn IS the exact localized
        // breadcrumb text — but the owner record says it is a REAL user turn.
        // Whole-turn selection must treat it as a turn boundary, never skip
        // it as synthetic, regardless of the active locale's wording.
        let mut h = vec![
            sys("system"),
            user(&breadcrumb().content),
            asst("answer to what looks like a crumb"),
            user("newest request"),
            asst("newest answer"),
        ];
        let dropped = drop_oldest_whole_turn(&mut h, false);
        assert_eq!(dropped, 2, "the colliding first turn drops as a whole turn");
        assert!(
            matches!(
                h.get(1),
                Some(m) if m.role == "user" && m.content == "newest request"
            ),
            "the colliding turn is dropped whole and the newest turn survives"
        );
        // With the owner record saying a crumb IS present, selection starts
        // after it even when the crumb slot holds ordinary text.
        let mut with_marker = vec![
            sys("system"),
            user("[synthetic] earlier history was trimmed"),
            user("old request"),
            asst("old answer"),
            user("newest request"),
            asst("newest answer"),
        ];
        let dropped = drop_oldest_whole_turn(&mut with_marker, true);
        assert_eq!(
            dropped, 2,
            "drop starts after the owner-recorded synthetic crumb"
        );
        assert!(matches!(
            with_marker.get(1),
            Some(m) if m.role == "user" && m.content.contains("synthetic")
        ));
        assert!(matches!(
            with_marker.get(2),
            Some(m) if m.role == "user" && m.content == "newest request"
        ));
    }
}

#[cfg(test)]
mod collapse_tests {
    use super::*;

    fn sys(c: &str) -> ChatMessage {
        ChatMessage::system(c)
    }
    fn user(c: &str) -> ChatMessage {
        ChatMessage::user(c)
    }
    fn asst(c: &str) -> ChatMessage {
        ChatMessage::assistant(c)
    }
    fn tool(c: &str) -> ChatMessage {
        ChatMessage::tool(c)
    }
    fn carrier(ids: &[&str]) -> ChatMessage {
        let calls: Vec<String> = ids
            .iter()
            .map(|id| format!("{{\"id\":\"{id}\"}}"))
            .collect();
        asst(&format!("{{\"tool_calls\":[{}]}}", calls.join(",")))
    }

    /// user → assistant(call) → tool → assistant(final)
    fn native_turn(n: usize) -> Vec<ChatMessage> {
        vec![
            user(&format!("request {n}")),
            carrier(&[&format!("call_{n}")]),
            tool(&format!(
                "{{\"tool_call_id\":\"call_{n}\",\"content\":\"result {n}\"}}"
            )),
            asst(&format!("answer {n}")),
        ]
    }

    /// user → assistant(call) → user([Tool results]) → assistant(final)
    fn prompt_mode_turn(n: usize) -> Vec<ChatMessage> {
        vec![
            user(&format!("request {n}")),
            asst(&format!("[tool_call] shell {n}")),
            user(&format!("[Tool results]\nresult {n}")),
            asst(&format!("answer {n}")),
        ]
    }

    fn rows(history: &[ChatMessage]) -> Vec<(String, String)> {
        history
            .iter()
            .map(|m| (m.role.clone(), m.content.clone()))
            .collect()
    }

    fn summary(calls: usize) -> String {
        ChatMessage::pruned_tool_exchange_summary(calls)
    }

    fn assert_identity(c: &CollapsedToolContext, history: &[ChatMessage]) {
        assert_eq!(rows(&c.messages), rows(history));
        assert_eq!(c.source_rows, (0..history.len()).collect::<Vec<_>>());
        assert_eq!(c.dropped_messages, 0);
        assert_eq!(c.collapsed_turns, 0);
    }

    #[test]
    fn keeps_running_turn_plus_prior_turns_and_summarises_older_ones() {
        let mut h = vec![sys("s")];
        for n in 1..=4 {
            h.extend(native_turn(n));
        }
        let before = rows(&h);
        let c = collapse_tool_context_older_than(&h, 1, false);
        assert_eq!(rows(&h), before, "input history is never modified");
        assert_eq!(c.collapsed_turns, 2);
        assert_eq!(c.dropped_messages, 4);
        let expected: Vec<(String, String)> = [
            ("system", "s"),
            ("user", "request 1"),
            ("assistant", summary(1).as_str()),
            ("assistant", "answer 1"),
            ("user", "request 2"),
            ("assistant", summary(1).as_str()),
            ("assistant", "answer 2"),
        ]
        .iter()
        .map(|(r, t)| (r.to_string(), t.to_string()))
        .chain(rows(&native_turn(3)))
        .chain(rows(&native_turn(4)))
        .collect();
        assert_eq!(rows(&c.messages), expected);
        assert!(c.messages[2].is_pruned_tool_exchange_summary());
        // Every request row maps to the history row it stands for; the summary
        // stands for the carrier it replaced.
        assert_eq!(
            c.source_rows,
            vec![0, 1, 2, 4, 5, 6, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
        assert_eq!(c.source_rows.len(), c.messages.len());
    }

    #[test]
    fn prompt_mode_results_carrier_is_not_a_turn_boundary() {
        let mut h = vec![sys("s")];
        for n in 1..=3 {
            h.extend(prompt_mode_turn(n));
        }
        let c = collapse_tool_context_older_than(&h, 0, false);
        assert_eq!(c.collapsed_turns, 2);
        assert_eq!(c.dropped_messages, 4);
        assert_eq!(count_turns(&c.messages), 3);
        assert!(
            !c.messages
                .iter()
                .any(|m| m.content == "[Tool results]\nresult 1")
        );
        assert!(
            c.messages
                .iter()
                .any(|m| m.content == "[Tool results]\nresult 3")
        );
        assert_eq!(c.messages[2].content, summary(1));
    }

    #[test]
    fn zero_keeps_tool_context_for_the_running_turn_only() {
        let mut h = native_turn(1);
        h.extend(native_turn(2));
        let c = collapse_tool_context_older_than(&h, 0, false);
        assert_eq!(c.collapsed_turns, 1);
        let mut expected = vec![
            ("user".to_string(), "request 1".to_string()),
            ("assistant".to_string(), summary(1)),
            ("assistant".to_string(), "answer 1".to_string()),
        ];
        expected.extend(rows(&native_turn(2)));
        assert_eq!(rows(&c.messages), expected);
    }

    #[test]
    fn breadcrumb_and_system_prefix_are_copied_and_not_counted() {
        let mut h = vec![sys("a"), sys("b"), breadcrumb()];
        h.extend(native_turn(1));
        h.extend(native_turn(2));
        let c = collapse_tool_context_older_than(&h, 0, true);
        assert_eq!(c.collapsed_turns, 1);
        assert_eq!(rows(&c.messages[..3]), rows(&h[..3]));
        assert_eq!(c.messages[3].content, "request 1");
        assert_eq!(c.messages[4].content, summary(1));
        assert_eq!(c.messages[5].content, "answer 1");
        assert_eq!(&c.source_rows[..6], &[0, 1, 2, 3, 4, 6]);
    }

    #[test]
    fn aborted_turn_keeps_prompt_and_summary_but_never_a_dangling_carrier() {
        let mut h = vec![
            user("request 1"),
            carrier(&["call_1"]),
            tool("{\"tool_call_id\":\"call_1\",\"content\":\"done\"}"),
            carrier(&["call_2", "call_3"]),
        ];
        h.extend(native_turn(2));
        let c = collapse_tool_context_older_than(&h, 0, false);
        assert_eq!(c.dropped_messages, 3);
        let mut expected = vec![
            ("user".to_string(), "request 1".to_string()),
            ("assistant".to_string(), summary(3)),
        ];
        expected.extend(rows(&native_turn(2)));
        assert_eq!(rows(&c.messages), expected);
    }

    #[test]
    fn runtime_feedback_rows_do_not_split_the_running_turn() {
        // The running turn already holds a tool round when the loop appends
        // its own feedback row (twice, the retry budget). With the smallest
        // window the model must still see that round.
        let mut h = native_turn(1);
        h.extend(native_turn(2));
        h.pop();
        h.push(user(
            "[Tool call parse error]\nYour previous response looked like...",
        ));
        h.push(user(
            "[Tool call parse error]\nYour previous response looked like...",
        ));
        let c = collapse_tool_context_older_than(&h, 0, false);
        assert_eq!(c.collapsed_turns, 1, "only the prior turn collapses");
        let mut expected = vec![
            ("user".to_string(), "request 1".to_string()),
            ("assistant".to_string(), summary(1)),
            ("assistant".to_string(), "answer 1".to_string()),
        ];
        expected.extend(rows(&h[4..]));
        assert_eq!(rows(&c.messages), expected);
        assert!(
            c.messages
                .iter()
                .any(|m| m.role == "tool" && m.content.contains("result 2")),
            "the running turn keeps its own tool result"
        );
    }

    #[test]
    fn text_only_turns_and_small_histories_pass_through_unchanged() {
        let mut h = vec![
            user("hi"),
            asst("hello"),
            user("again"),
            asst("hello again"),
        ];
        h.extend(native_turn(3));
        let c = collapse_tool_context_older_than(&h, 0, false);
        assert_identity(&c, &h);

        let mut h2 = native_turn(1);
        h2.extend(native_turn(2));
        let c2 = collapse_tool_context_older_than(&h2, 1, false);
        assert_identity(&c2, &h2);

        let mut h3 = vec![sys("s")];
        for n in 1..=6 {
            h3.extend(native_turn(n));
        }
        let c3 = collapse_tool_context_older_than(&h3, usize::MAX, false);
        assert_identity(&c3, &h3);
    }
}
