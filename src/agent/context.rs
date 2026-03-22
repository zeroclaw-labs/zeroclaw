use crate::providers::ChatMessage;
use crate::util::truncate_with_ellipsis;
use regex::{Captures, Regex};
use serde_json::Value;
use std::sync::LazyLock;

/// Fallback context-window budget used when callers do not pass a concrete
/// configured value. This mirrors the previous ZeroClaw default.
pub(crate) const DEFAULT_CONTEXT_WINDOW_TOKENS: usize = 32_000;
/// Per-tool hard floor so even constrained budgets keep actionable output.
pub(crate) const TOOL_RESULT_MIN_CHARS: usize = 1_200;
/// Per-tool hard cap to prevent a single tool from dominating context.
pub(crate) const TOOL_RESULT_MAX_CHARS: usize = 12_000;

static TOOL_RESULT_BLOCK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?s)<tool_result\b([^>]*)>(.*?)</tool_result>"#).unwrap());
static TOOL_RESULT_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"name\s*=\s*"([^"]+)""#).unwrap());

/// Estimate token count for a message history using ~4 chars/token heuristic.
/// Includes a small overhead per message for role/framing tokens.
pub(crate) fn estimate_history_tokens(history: &[ChatMessage]) -> usize {
    history
        .iter()
        .map(|message| message.content.len().div_ceil(4) + 4)
        .sum()
}

pub(crate) fn tool_result_budget_chars(
    history: &[ChatMessage],
    context_window_tokens: usize,
) -> usize {
    let effective_window = context_window_tokens.max(1);
    let used_tokens = estimate_history_tokens(history);
    let remaining_tokens = effective_window.saturating_sub(used_tokens);

    // Reserve ~25% of the remaining window for each tool result payload.
    let per_tool_tokens = remaining_tokens.div_ceil(4).max(300);
    (per_tool_tokens.saturating_mul(4)).clamp(TOOL_RESULT_MIN_CHARS, TOOL_RESULT_MAX_CHARS)
}

fn take_prefix_chars(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

fn take_suffix_chars(text: &str, chars: usize) -> String {
    let total = text.chars().count();
    text.chars().skip(total.saturating_sub(chars)).collect()
}

pub(crate) fn truncate_tool_result_for_context(
    tool_name: &str,
    output: &str,
    max_chars: usize,
) -> String {
    let total_chars = output.chars().count();
    if total_chars <= max_chars {
        return output.to_string();
    }

    let removed = total_chars.saturating_sub(max_chars);
    let marker = format!("\n...[{tool_name} output truncated: removed {removed} chars]...\n");
    let marker_chars = marker.chars().count();

    if marker_chars >= max_chars {
        return truncate_with_ellipsis(output, max_chars);
    }

    let available = max_chars - marker_chars;
    let head_chars = available.saturating_mul(2) / 3;
    let tail_chars = available.saturating_sub(head_chars);
    let head = take_prefix_chars(output, head_chars);
    let tail = take_suffix_chars(output, tail_chars);

    format!("{head}{marker}{tail}")
}

fn truncate_string_field(tool_name: &str, value: &mut serde_json::Value, max_chars: usize) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };

    if text.chars().count() <= max_chars {
        return false;
    }

    *value = Value::String(truncate_tool_result_for_context(tool_name, text, max_chars));
    true
}

fn repair_native_tool_result_message(message: &mut ChatMessage, max_chars: usize) -> bool {
    let Ok(mut payload) = serde_json::from_str::<Value>(&message.content) else {
        if message.role == "tool" && message.content.chars().count() > max_chars {
            message.content = truncate_tool_result_for_context("tool", &message.content, max_chars);
            return true;
        }
        return false;
    };

    let Some(object) = payload.as_object_mut() else {
        return false;
    };

    let mut changed = false;
    for key in ["content", "result"] {
        if let Some(value) = object.get_mut(key) {
            changed |= truncate_string_field("tool", value, max_chars);
        }
    }

    if changed {
        if let Ok(serialized) = serde_json::to_string(&payload) {
            message.content = serialized;
        }
    }

    changed
}

fn tool_name_from_attrs(attrs: &str) -> &str {
    TOOL_RESULT_NAME_RE
        .captures(attrs)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str())
        .unwrap_or("tool")
}

fn repair_prompt_tool_result_blocks(content: &str, max_chars: usize) -> Option<String> {
    if !content.contains("<tool_result") {
        return None;
    }

    let mut changed = false;
    let replaced = TOOL_RESULT_BLOCK_RE.replace_all(content, |captures: &Captures<'_>| {
        let attrs = captures.get(1).map(|value| value.as_str()).unwrap_or("");
        let raw_body = captures.get(2).map(|value| value.as_str()).unwrap_or("");
        let tool_name = tool_name_from_attrs(attrs);
        let body = raw_body.trim();
        let truncated = if body.chars().count() > max_chars {
            changed = true;
            truncate_tool_result_for_context(tool_name, body, max_chars)
        } else {
            body.to_string()
        };
        format!("<tool_result{attrs}>\n{truncated}\n</tool_result>")
    });

    if changed {
        Some(replaced.into_owned())
    } else {
        None
    }
}

fn repair_message_tool_results(message: &mut ChatMessage, max_chars: usize) -> bool {
    let mut changed = false;

    if message.role == "tool" {
        changed |= repair_native_tool_result_message(message, max_chars);
    }

    if let Some(repaired) = repair_prompt_tool_result_blocks(&message.content, max_chars) {
        message.content = repaired;
        changed = true;
    }

    changed
}

/// Re-truncate oversized tool results already stored in history.
///
/// Returns the number of history messages that were rewritten.
pub(crate) fn repair_oversized_tool_results(
    history: &mut [ChatMessage],
    context_window_tokens: usize,
) -> usize {
    let budget_chars = tool_result_budget_chars(history, context_window_tokens);
    let mut rewritten = 0usize;

    for message in history.iter_mut() {
        if repair_message_tool_results(message, budget_chars) {
            rewritten += 1;
        }
    }

    rewritten
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_prompt_tool_result_blocks_truncates_large_body() {
        let mut history = vec![ChatMessage::user(format!(
            "[Tool results]\n<tool_result name=\"shell\">\nHEAD{}TAIL\n</tool_result>",
            "x".repeat(8_000)
        ))];

        let rewritten = repair_oversized_tool_results(&mut history, 4_000);
        assert_eq!(rewritten, 1);
        assert!(history[0].content.contains("HEAD"));
        assert!(history[0].content.contains("TAIL"));
        assert!(history[0].content.contains("output truncated"));
    }

    #[test]
    fn repair_native_tool_result_message_truncates_content_field() {
        let payload = serde_json::json!({
            "tool_call_id": "call_1",
            "content": format!("HEAD{}TAIL", "x".repeat(8_000)),
        });
        let mut history = vec![ChatMessage::tool(payload.to_string())];

        let rewritten = repair_oversized_tool_results(&mut history, 4_000);
        assert_eq!(rewritten, 1);

        let repaired: Value = serde_json::from_str(&history[0].content).unwrap();
        let content = repaired["content"].as_str().unwrap();
        assert!(content.contains("HEAD"));
        assert!(content.contains("TAIL"));
        assert!(content.contains("output truncated"));
    }

    #[test]
    fn repair_oversized_tool_results_leaves_normal_messages_untouched() {
        let original = ChatMessage::assistant("normal assistant reply");
        let mut history = vec![original.clone()];

        let rewritten = repair_oversized_tool_results(&mut history, 4_000);
        assert_eq!(rewritten, 0);
        assert_eq!(history[0].content, original.content);
    }
}
