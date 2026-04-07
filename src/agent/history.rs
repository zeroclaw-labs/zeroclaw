use crate::providers::ChatMessage;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Default trigger for auto-compaction when non-system message count exceeds this threshold.
/// Prefer passing the config-driven value via `run_tool_call_loop`; this constant is only
/// used when callers omit the parameter.
pub(crate) const DEFAULT_MAX_HISTORY_MESSAGES: usize = 50;

/// Truncate a tool result to `max_chars`, keeping head (2/3) + tail (1/3)
/// with a marker in the middle. Returns input unchanged if within limit or
/// `max_chars == 0` (disabled).
pub(crate) fn truncate_tool_result(output: &str, max_chars: usize) -> String {
    if max_chars == 0 || output.len() <= max_chars {
        return output.to_string();
    }
    let head_len = max_chars * 2 / 3;
    let tail_len = max_chars.saturating_sub(head_len);
    let head_end = crate::util::floor_char_boundary(output, head_len);
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
        return output[..crate::util::floor_char_boundary(output, max_chars)].to_string();
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

/// Trim conversation history to prevent unbounded growth.
/// Preserves the system prompt (first message if role=system) and the most recent messages.
pub(crate) fn trim_history(history: &mut Vec<ChatMessage>, max_history: usize) {
    // Nothing to trim if within limit
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
