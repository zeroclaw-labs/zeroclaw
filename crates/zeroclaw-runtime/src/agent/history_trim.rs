//! Whole-turn history trimming. One rule: keep the most recent whole turns
//! that fit the token budget, drop the rest, never cut a turn in half.
//!
//! A turn starts at a real user message and runs until the next real user
//! message, covering the assistant reply, any assistant tool-call rows, and
//! any tool-result rows in between. Tool exchanges live entirely inside a
//! turn, so dropping whole turns can never split a tool_use/tool_result pair.
//! Pairing safety for providers like Anthropic is structural, not swept.

use crate::agent::history::estimate_history_tokens;
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

#[cfg(test)]
mod tests {
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
}
