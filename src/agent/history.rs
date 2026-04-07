use crate::providers::ChatMessage;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Default trigger for auto-compaction when non-system message count exceeds this threshold.
/// Prefer passing the config-driven value via `run_tool_call_loop`; this constant is only
/// used when callers omit the parameter.
pub(crate) const DEFAULT_MAX_HISTORY_MESSAGES: usize = 50;

/// Find the largest byte index `<= i` that is a valid char boundary.
/// MSRV-compatible replacement for `str::floor_char_boundary` (stable in 1.91).
pub(crate) fn floor_char_boundary(s: &str, i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    let mut pos = i;
    while pos > 0 && !s.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

/// Truncate a tool result to `max_chars`, keeping head (2/3) + tail (1/3)
/// with a marker in the middle. Returns input unchanged if within limit or
/// `max_chars == 0` (disabled).
pub(crate) fn truncate_tool_result(output: &str, max_chars: usize) -> String {
    if max_chars == 0 || output.len() <= max_chars {
        return output.to_string();
    }
    let head_len = max_chars * 2 / 3;
    let tail_len = max_chars.saturating_sub(head_len);
    let head_end = floor_char_boundary(output, head_len);
    // ceil_char_boundary: find smallest byte index >= i on a char boundary
    let tail_start_raw = output.len().saturating_sub(tail_len);
    let tail_start = if tail_start_raw >= output.len() {
        output.len()
    } else {
        let mut pos = tail_start_raw;
        while pos < output.len() && !output.is_char_boundary(pos) {
            pos += 1;
        }
        pos
    };
    // Guard against overlap when max_chars is very small
    if head_end >= tail_start {
        return output[..floor_char_boundary(output, max_chars)].to_string();
    }
    let truncated_chars = tail_start - head_end;
    format!(
        "{}\n\n[... {} characters truncated ...]\n\n{}",
        &output[..head_end],
        truncated_chars,
        &output[tail_start..]
    )
}

/// Aggressively trim old tool result messages in history to recover from
/// context overflow. Keeps the last `protect_last_n` messages untouched.
/// Returns total characters saved.
pub(crate) fn fast_trim_tool_results(
    history: &mut [crate::providers::ChatMessage],
    protect_last_n: usize,
) -> usize {
    let trim_to = 2000;
    let mut saved = 0;
    let cutoff = history.len().saturating_sub(protect_last_n);
    for msg in &mut history[..cutoff] {
        if msg.role == "tool" && msg.content.len() > trim_to {
            let original_len = msg.content.len();
            msg.content = truncate_tool_result(&msg.content, trim_to);
            saved += original_len - msg.content.len();
        }
    }
    saved
}

/// Emergency: drop oldest non-system, non-recent messages from history.
/// Returns number of messages dropped.
///
/// After the main drop pass, continues removing messages until the first
/// non-system message has role `user`, so that the resulting sequence
/// satisfies provider constraints (e.g. Zhipu GLM rejects `system -> assistant`).
pub(crate) fn emergency_history_trim(
    history: &mut Vec<crate::providers::ChatMessage>,
    keep_recent: usize,
) -> usize {
    let mut dropped = 0;
    let target_drop = history.len() / 3;
    let mut i = 0;
    while dropped < target_drop && i < history.len().saturating_sub(keep_recent) {
        if history[i].role == "system" {
            i += 1;
        } else {
            history.remove(i);
            dropped += 1;
        }
    }
    align_to_user_boundary(history, keep_recent, &mut dropped);
    dropped
}

/// Estimate token count for a message history using ~4 chars/token heuristic.
/// Includes a small overhead per message for role/framing tokens.
pub(crate) fn estimate_history_tokens(history: &[ChatMessage]) -> usize {
    history
        .iter()
        .map(|m| {
            // ~4 chars per token + ~4 framing tokens per message (role, delimiters)
            m.content.len().div_ceil(4) + 4
        })
        .sum()
}

/// After trimming, drop leading non-system messages until the first one has
/// role `user`. This prevents sequences like `system -> assistant -> …` which
/// some providers (e.g. Zhipu GLM) reject.
///
/// `keep_recent` specifies how many trailing messages must stay untouched.
/// Dropped count is accumulated into `dropped`.
pub(crate) fn align_to_user_boundary(
    history: &mut Vec<ChatMessage>,
    keep_recent: usize,
    dropped: &mut usize,
) {
    loop {
        let first = history.iter().position(|m| m.role != "system");
        match first {
            Some(idx)
                if history[idx].role != "user"
                    && idx < history.len().saturating_sub(keep_recent) =>
            {
                history.remove(idx);
                *dropped += 1;
            }
            _ => break,
        }
    }
}

/// Trim conversation history to prevent unbounded growth.
/// Preserves the system prompt (first message if role=system) and the most recent messages.
///
/// After the bulk drain, delegates to [`align_to_user_boundary`] so that the
/// first non-system message has role `user` (required by providers like Zhipu GLM).
///
/// Note: this function does not have a `keep_recent` guard, so if no `user`
/// message exists after the drain point the alignment pass may remove all
/// remaining non-system messages. This is acceptable because a conversation
/// with zero user messages is inherently degenerate.
pub(crate) fn trim_history(history: &mut Vec<ChatMessage>, max_history: usize) {
    let has_system = history.first().map_or(false, |m| m.role == "system");
    let non_system_count = if has_system {
        history.len() - 1
    } else {
        history.len()
    };

    if non_system_count <= max_history {
        return;
    }

    let start = if has_system { 1 } else { 0 };
    let to_remove = non_system_count - max_history;
    history.drain(start..start + to_remove);
    let mut extra = 0usize;
    align_to_user_boundary(history, 0, &mut extra);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InteractiveSessionState {
    pub(crate) version: u32,
    pub(crate) history: Vec<ChatMessage>,
}

impl InteractiveSessionState {
    fn from_history(history: &[ChatMessage]) -> Self {
        Self {
            version: 1,
            history: history.to_vec(),
        }
    }
}

pub(crate) fn load_interactive_session_history(
    path: &Path,
    system_prompt: &str,
) -> Result<Vec<ChatMessage>> {
    if !path.exists() {
        return Ok(vec![ChatMessage::system(system_prompt)]);
    }

    let raw = std::fs::read_to_string(path)?;
    let mut state: InteractiveSessionState = serde_json::from_str(&raw)?;
    if state.history.is_empty() {
        state.history.push(ChatMessage::system(system_prompt));
    } else if state.history.first().map(|msg| msg.role.as_str()) != Some("system") {
        state.history.insert(0, ChatMessage::system(system_prompt));
    }

    Ok(state.history)
}

pub(crate) fn save_interactive_session_history(path: &Path, history: &[ChatMessage]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let payload = serde_json::to_string_pretty(&InteractiveSessionState::from_history(history))?;
    std::fs::write(path, payload)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ChatMessage;

    // ── trim_history boundary alignment tests ─────────────────────

    #[test]
    fn trim_history_aligns_to_user_boundary() {
        let mut history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("u1"),
            ChatMessage::assistant("a1"),
            ChatMessage::user("u2"),
            ChatMessage::assistant("a2"),
            ChatMessage::user("u3"),
            ChatMessage::assistant("a3"),
        ];
        // Keep 4 non-system → remove 2 oldest (u1, a1).
        // After drain the first non-system is u2 (user) — no extra alignment needed.
        trim_history(&mut history, 4);
        assert_eq!(history[0].role, "system");
        assert_eq!(history[1].role, "user");
        assert_eq!(history[1].content, "u2");
    }

    #[test]
    fn trim_history_extends_drain_past_assistant() {
        let mut history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("u1"),
            ChatMessage::assistant("a1"),
            ChatMessage::tool("t1"),
            ChatMessage::user("u2"),
            ChatMessage::assistant("a2"),
        ];
        // Keep 3 non-system → remove 2 (u1, a1). Drain ends at index 3 = tool.
        // Boundary alignment extends drain to include tool, so first non-system = u2.
        trim_history(&mut history, 3);
        assert_eq!(history[0].role, "system");
        assert_eq!(history[1].role, "user");
        assert_eq!(history[1].content, "u2");
    }

    #[test]
    fn trim_history_noop_when_within_limit() {
        let mut history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("hello"),
            ChatMessage::assistant("hi"),
        ];
        trim_history(&mut history, 10);
        assert_eq!(history.len(), 3);
    }

    // ── emergency_history_trim boundary alignment tests ───────────

    #[test]
    fn emergency_trim_aligns_to_user_boundary() {
        let mut history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("u1"),
            ChatMessage::assistant("a1"),
            ChatMessage::assistant("a2"),
            ChatMessage::user("u2"),
            ChatMessage::assistant("a3"),
        ];
        // target_drop = 6/3 = 2, keep_recent = 2.
        // Drops u1, a1. First non-system is now a2 (assistant).
        // Boundary alignment drops a2 as well. First non-system = u2.
        let dropped = emergency_history_trim(&mut history, 2);
        assert!(dropped >= 2);
        assert_eq!(history[0].role, "system");
        let first_non_sys = history.iter().position(|m| m.role != "system").unwrap();
        assert_eq!(history[first_non_sys].role, "user");
    }

    #[test]
    fn emergency_trim_noop_when_already_user_first() {
        let mut history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("u1"),
            ChatMessage::assistant("a1"),
        ];
        let dropped = emergency_history_trim(&mut history, 2);
        assert_eq!(dropped, 0);
        assert_eq!(history.len(), 3);
    }

    // ── align_to_user_boundary unit tests ─────────────────────────

    #[test]
    fn align_noop_when_user_is_first() {
        let mut msgs = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("hello"),
            ChatMessage::assistant("hi"),
        ];
        let mut dropped = 0;
        align_to_user_boundary(&mut msgs, 0, &mut dropped);
        assert_eq!(dropped, 0);
        assert_eq!(msgs.len(), 3);
    }

    #[test]
    fn align_drops_assistant_and_tool_before_user() {
        let mut msgs = vec![
            ChatMessage::system("sys"),
            ChatMessage::assistant("a1"),
            ChatMessage::tool("t1"),
            ChatMessage::user("u1"),
            ChatMessage::assistant("a2"),
        ];
        let mut dropped = 0;
        align_to_user_boundary(&mut msgs, 0, &mut dropped);
        assert_eq!(dropped, 2);
        assert_eq!(msgs[0].role, "system");
        assert_eq!(msgs[1].role, "user");
        assert_eq!(msgs[1].content, "u1");
    }

    #[test]
    fn align_respects_keep_recent() {
        let mut msgs = vec![ChatMessage::system("sys"), ChatMessage::assistant("a1")];
        let mut dropped = 0;
        // a1 is protected by keep_recent=1
        align_to_user_boundary(&mut msgs, 1, &mut dropped);
        assert_eq!(dropped, 0);
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn align_noop_on_empty() {
        let mut msgs: Vec<ChatMessage> = vec![];
        let mut dropped = 0;
        align_to_user_boundary(&mut msgs, 0, &mut dropped);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn align_noop_when_only_system() {
        let mut msgs = vec![ChatMessage::system("sys")];
        let mut dropped = 0;
        align_to_user_boundary(&mut msgs, 0, &mut dropped);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn align_with_multiple_system_messages() {
        let mut msgs = vec![
            ChatMessage::system("sys1"),
            ChatMessage::system("sys2"),
            ChatMessage::assistant("response"),
            ChatMessage::user("followup"),
        ];
        let mut dropped = 0;
        align_to_user_boundary(&mut msgs, 0, &mut dropped);
        assert_eq!(dropped, 1);
        assert_eq!(msgs[0].role, "system");
        assert_eq!(msgs[1].role, "system");
        assert_eq!(msgs[2].role, "user");
        assert_eq!(msgs[2].content, "followup");
    }
}
