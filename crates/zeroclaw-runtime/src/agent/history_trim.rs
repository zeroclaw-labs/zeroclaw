//! Whole-turn history trimming. One rule: keep the most recent whole turns
//! that fit the token budget, drop the rest, never cut a turn in half.

use crate::agent::history::estimate_history_tokens;
use zeroclaw_api::model_provider::ConversationMessage;
use zeroclaw_providers::ChatMessage;

const TOOL_RESULTS_PREFIX: &str = "[Tool results]";

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
pub(crate) struct MessageCountTrimResult {
    pub history: Vec<ConversationMessage>,
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
            if chat.role == "user" && !is_breadcrumb
    )
}

/// Drop the oldest whole conversation turns until the non-system body fits
/// `max_messages`, while always retaining the newest complete turn.
pub(crate) fn trim_conversation_to_recent_turns(
    history: Vec<ConversationMessage>,
    max_messages: usize,
    has_leading_breadcrumb: bool,
) -> MessageCountTrimResult {
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
    let counted_messages = history
        .iter()
        .filter(|message| !is_conversation_system(message))
        .count()
        - synthetic_messages;
    if counted_messages <= max_messages || total_turns <= 1 {
        return MessageCountTrimResult {
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

    let mut first_kept = boundaries[1];
    let mut dropped_turns = 1;
    for (turn_index, &boundary) in boundaries.iter().enumerate().skip(1) {
        first_kept = boundary;
        dropped_turns = turn_index;
        if body.len() - boundary <= max_messages || turn_index == boundaries.len() - 1 {
            break;
        }
    }

    let dropped_messages = first_kept - synthetic_messages;
    system.extend(body.into_iter().skip(first_kept));
    MessageCountTrimResult {
        history: system,
        dropped_messages,
        dropped_turns,
        kept_turns: boundaries.len() - dropped_turns,
        trimmed: true,
    }
}

fn is_turn_boundary(msg: &ChatMessage) -> bool {
    msg.role == "user" && !msg.content.starts_with(TOOL_RESULTS_PREFIX)
}

fn is_system(msg: &ChatMessage) -> bool {
    msg.role == "system"
}

/// Drop oldest whole turns until the history fits `budget_tokens`, always
/// keeping leading system messages and at least the most recent whole turn.
/// When `budget_tokens` is zero the history is returned untouched.
pub fn trim_to_recent_turns(history: Vec<ChatMessage>, budget_tokens: usize) -> TrimResult {
    let total_turns = count_turns(&history);
    let tokens_before = estimate_history_tokens(&history);
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

    let leading_system = history.iter().take_while(|m| is_system(m)).count();
    let system: Vec<ChatMessage> = history[..leading_system].to_vec();
    let body = &history[leading_system..];

    let boundaries: Vec<usize> = body
        .iter()
        .enumerate()
        .filter(|(_, m)| is_turn_boundary(m))
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
        let mut probe = system.clone();
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
    let mut kept = system;
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

pub fn trim_to_reported_budget(
    history: Vec<ChatMessage>,
    budget_tokens: usize,
    reported_input_tokens: usize,
) -> TrimResult {
    let estimated = estimate_history_tokens(&history);
    if budget_tokens == 0 || reported_input_tokens <= budget_tokens || estimated == 0 {
        let total_turns = count_turns(&history);
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
    let scaled =
        (budget_tokens as u128 * estimated as u128 / reported_input_tokens as u128).max(1) as usize;
    let result = trim_to_recent_turns(history, scaled);
    let ratio = reported_input_tokens as f64 / estimated as f64;
    TrimResult {
        tokens_before: reported_input_tokens,
        tokens_after: (result.tokens_after as f64 * ratio).round() as usize,
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

fn count_turns(history: &[ChatMessage]) -> usize {
    history.iter().filter(|m| is_turn_boundary(m)).count()
}

/// Front breadcrumb injected after the system messages so the model SEES that
/// earlier turns were cut and cannot confabulate dropped work as present.
pub fn breadcrumb() -> ChatMessage {
    ChatMessage::user(crate::i18n::get_required_cli_string("history-trim-breadcrumb").as_str())
}

/// Insert the trim breadcrumb after the leading system messages, unless one is
/// already sitting there.
pub fn insert_breadcrumb_deduped(history: &mut Vec<ChatMessage>) {
    let system_count = history.iter().take_while(|m| is_system(m)).count();
    let crumb = breadcrumb();
    let already_present = history
        .get(system_count)
        .is_some_and(|m| m.role == crumb.role && m.content == crumb.content);
    if already_present {
        return;
    }
    history.insert(system_count, crumb);
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
    fn trim_conversation_to_recent_turns_drops_old_turn_and_keeps_tool_heavy_turn() {
        let mut history = vec![
            conversation_user("old request"),
            conversation_assistant("old answer"),
            conversation_user("new request"),
        ];
        for index in 0..25 {
            push_tool_exchange(&mut history, index);
        }
        history.push(conversation_assistant("new answer"));

        let result = trim_conversation_to_recent_turns(history, 50, false);

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

        let result = trim_conversation_to_recent_turns(history, 2, false);

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

        let result = trim_conversation_to_recent_turns(history, 2, false);

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

        let result = trim_conversation_to_recent_turns(history, 2, false);

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

        let result = trim_conversation_to_recent_turns(history, 2, false);

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
    fn trim_conversation_to_recent_turns_drops_minimum_oldest_turns_for_exact_cap() {
        let history = vec![
            conversation_user("old request"),
            conversation_assistant("old answer"),
            conversation_user("middle request"),
            conversation_assistant("middle answer"),
            conversation_user("new request"),
            conversation_assistant("new answer"),
        ];

        let result = trim_conversation_to_recent_turns(history, 4, false);

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

        let first = trim_conversation_to_recent_turns(history, 4, true);
        assert!(
            !first.trimmed,
            "a synthetic breadcrumb must not push an exactly-at-cap body over the limit"
        );

        history = first.history;
        history.push(conversation_user("new request"));
        history.push(conversation_assistant("new answer"));
        let mut second = trim_conversation_to_recent_turns(history, 4, true);

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
        let r = trim_to_recent_turns(h, 1_000_000);
        assert!(!r.trimmed);
        assert_eq!(r.history.len(), n);
        assert_eq!(r.dropped_turns, 0);
    }

    #[test]
    fn zero_budget_is_untouched() {
        let h = vec![sys("s"), user("hi"), asst("yo")];
        let n = h.len();
        let r = trim_to_recent_turns(h, 0);
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
        let r = trim_to_recent_turns(h, 200);
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
        let r = trim_to_recent_turns(h, 200);
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
        let r = trim_to_recent_turns(h, 1_000_000);
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
        let r = trim_to_recent_turns(h, 150);
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
        let r = trim_to_recent_turns(h, 50);
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
        let r = trim_to_recent_turns(h, 200);
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

        let r = trim_to_recent_turns(h, 200);

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
        let r = trim_to_recent_turns(h, 120);
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
        let r = trim_to_reported_budget(h, budget, reported);
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
        let r = trim_to_reported_budget(h, estimated * 4, estimated);
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
        let r = trim_to_reported_budget(h, budget, reported);
        assert!(r.trimmed, "extreme ratio must still enforce, not no-op");
        assert!(r.history.iter().any(|m| m.content.contains("recent short")));
    }

    #[test]
    fn insert_breadcrumb_deduped_does_not_stack() {
        let mut h = vec![sys("system"), user("turn1"), asst("a1")];
        insert_breadcrumb_deduped(&mut h);
        let after_first = h.len();
        insert_breadcrumb_deduped(&mut h);
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
    }

    #[test]
    fn insert_breadcrumb_deduped_sits_after_leading_system() {
        let mut h = vec![sys("s1"), sys("s2"), user("turn1"), asst("a1")];
        insert_breadcrumb_deduped(&mut h);
        assert_eq!(h[0].role, "system");
        assert_eq!(h[1].role, "system");
        assert_eq!(h[2].role, breadcrumb().role);
        assert_eq!(h[2].content, breadcrumb().content);
    }
}
