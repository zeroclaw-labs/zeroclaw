//! Structural tool_use / tool_result pairing repair.
//!
//! Ensures every tool_use in assistant messages has a matching tool_result,
//! and every tool_result references a valid tool_use. Runs BEFORE the LLM
//! call so the request is always structurally valid — errors are prevented,
//! not detected after the fact.
//!
//! Pattern from Claude Code's `ensureToolResultPairing` (messages.ts) and
//! OpenClaw's `repairToolUseResultPairing` (session-transcript-repair.ts).
//!
//! ## Upstream hook (channels/mod.rs, cfg-gated)
//!
//! ```ignore
//! #[cfg(feature = "one2x")]
//! crate::one2x::tool_pairing::repair_tool_pairing(&mut history);
//! ```

use zeroclaw_api::provider::ChatMessage;

/// Synthetic content for missing tool results (matches Claude Code's pattern).
const SYNTHETIC_TOOL_RESULT: &str = "[Tool result missing — internal error]";

/// Extract tool_use IDs from an assistant message's content.
/// Handles both native JSON array format and single-object format.
fn extract_tool_use_ids(content: &str) -> Vec<String> {
    let trimmed = content.trim();
    let mut ids = Vec::new();

    if trimmed.starts_with('[') {
        if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(trimmed) {
            for v in &arr {
                if v.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                    if let Some(id) = v.get("id").and_then(|i| i.as_str()) {
                        ids.push(id.to_string());
                    }
                }
            }
        }
    } else if trimmed.starts_with('{') {
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(calls) = obj.get("tool_calls").and_then(|c| c.as_array()) {
                for call in calls {
                    if let Some(id) = call.get("id").and_then(|i| i.as_str()) {
                        ids.push(id.to_string());
                    }
                }
            }
        }
    }

    ids
}

/// Extract tool_call_id from a tool result message.
fn extract_tool_result_id(content: &str) -> Option<String> {
    let trimmed = content.trim();
    if trimmed.starts_with('{') {
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(trimmed) {
            return obj
                .get("tool_call_id")
                .and_then(|i| i.as_str())
                .map(|s| s.to_string());
        }
    }
    None
}

/// Structurally repair tool_use/tool_result pairing in conversation history.
///
/// For each assistant message containing tool_use blocks:
/// 1. Collect the tool_use IDs
/// 2. Check following tool messages for matching tool_call_ids
/// 3. Inject synthetic error results for missing pairs
/// 4. Remove orphaned tool_results that don't match any tool_use
///
/// This ensures the LLM request is always structurally valid.
pub fn repair_tool_pairing(history: &mut Vec<ChatMessage>) {
    if history.len() < 2 {
        return;
    }

    let mut repaired = Vec::with_capacity(history.len() + 4);
    let mut i = 0;
    let mut total_injected = 0usize;
    let mut total_removed = 0usize;

    while i < history.len() {
        let msg = &history[i];

        // Non-assistant messages: check if it's an orphaned tool at the start
        if msg.role != "assistant" {
            if msg.role == "tool" && (repaired.is_empty() || repaired.last().map_or(true, |m: &ChatMessage| m.role != "assistant")) {
                // Orphaned tool result not preceded by assistant — drop it
                total_removed += 1;
                i += 1;
                continue;
            }
            repaired.push(msg.clone());
            i += 1;
            continue;
        }

        // Assistant message — check for tool_use blocks
        let tool_use_ids = extract_tool_use_ids(&msg.content);

        if tool_use_ids.is_empty() {
            // Regular assistant text, no tool calls
            repaired.push(msg.clone());
            i += 1;
            continue;
        }

        // Push the assistant message
        repaired.push(msg.clone());
        i += 1;

        // Collect following tool messages and match them to tool_use IDs
        let mut matched_ids = std::collections::HashSet::new();
        let mut seen_result_ids = std::collections::HashSet::new();
        let tool_use_id_set: std::collections::HashSet<_> = tool_use_ids.iter().cloned().collect();

        // Scan forward through tool messages
        let scan_start = i;
        while i < history.len() && history[i].role == "tool" {
            if let Some(result_id) = extract_tool_result_id(&history[i].content) {
                if tool_use_id_set.contains(&result_id) && !seen_result_ids.contains(&result_id) {
                    // Valid match — keep it
                    matched_ids.insert(result_id.clone());
                    seen_result_ids.insert(result_id);
                    repaired.push(history[i].clone());
                } else {
                    // Orphaned or duplicate tool_result — drop it
                    total_removed += 1;
                }
            } else {
                // Tool message without parseable tool_call_id
                // (XML-style tool results) — keep as-is
                repaired.push(history[i].clone());
            }
            i += 1;
        }

        // Inject synthetic results for missing tool_use IDs
        for use_id in &tool_use_ids {
            if !matched_ids.contains(use_id) {
                let synthetic = serde_json::json!({
                    "tool_call_id": use_id,
                    "content": SYNTHETIC_TOOL_RESULT,
                });
                repaired.push(ChatMessage::tool(synthetic.to_string()));
                total_injected += 1;
            }
        }
    }

    if total_injected > 0 || total_removed > 0 {
        tracing::info!(
            injected = total_injected,
            removed = total_removed,
            "repair_tool_pairing: fixed tool_use/tool_result mismatches"
        );
        *history = repaired;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
        }
    }

    fn assistant_with_tool_use(id: &str) -> ChatMessage {
        msg(
            "assistant",
            &serde_json::json!([{"type": "tool_use", "id": id, "name": "shell", "input": {}}]).to_string(),
        )
    }

    fn tool_result(id: &str, content: &str) -> ChatMessage {
        msg(
            "tool",
            &serde_json::json!({"tool_call_id": id, "content": content}).to_string(),
        )
    }

    #[test]
    fn no_change_when_paired() {
        let mut history = vec![
            msg("system", "sys"),
            msg("user", "hello"),
            assistant_with_tool_use("call_1"),
            tool_result("call_1", "ok"),
            msg("assistant", "done"),
        ];
        let before = history.len();
        repair_tool_pairing(&mut history);
        assert_eq!(history.len(), before);
    }

    #[test]
    fn inject_missing_tool_result() {
        let mut history = vec![
            msg("system", "sys"),
            msg("user", "hello"),
            assistant_with_tool_use("call_1"),
            // Missing tool_result for call_1
            msg("user", "next"),
        ];
        repair_tool_pairing(&mut history);
        // Should inject synthetic tool_result between assistant and user
        assert_eq!(history.len(), 5);
        assert_eq!(history[3].role, "tool");
        assert!(history[3].content.contains("call_1"));
        assert!(history[3].content.contains("missing"));
    }

    #[test]
    fn remove_orphaned_tool_result() {
        let mut history = vec![
            msg("system", "sys"),
            msg("user", "hello"),
            assistant_with_tool_use("call_1"),
            tool_result("call_1", "ok"),
            tool_result("call_ORPHAN", "bad"), // no matching tool_use
            msg("user", "next"),
        ];
        repair_tool_pairing(&mut history);
        // Orphaned tool_result should be removed
        assert_eq!(history.len(), 5);
        assert!(!history.iter().any(|m| m.content.contains("call_ORPHAN")));
    }

    #[test]
    fn remove_duplicate_tool_result() {
        let mut history = vec![
            msg("system", "sys"),
            assistant_with_tool_use("call_1"),
            tool_result("call_1", "first"),
            tool_result("call_1", "duplicate"), // same ID twice
        ];
        repair_tool_pairing(&mut history);
        // Only first tool_result kept
        let tool_msgs: Vec<_> = history.iter().filter(|m| m.role == "tool").collect();
        assert_eq!(tool_msgs.len(), 1);
        assert!(tool_msgs[0].content.contains("first"));
    }

    #[test]
    fn remove_orphaned_tool_at_start() {
        let mut history = vec![
            msg("system", "sys"),
            tool_result("orphan", "bad"), // tool before any assistant
            msg("user", "hello"),
        ];
        repair_tool_pairing(&mut history);
        assert_eq!(history.len(), 2);
        assert!(!history.iter().any(|m| m.role == "tool"));
    }

    #[test]
    fn handles_multiple_tool_calls() {
        let mut history = vec![
            msg("system", "sys"),
            msg("user", "hello"),
            msg(
                "assistant",
                &serde_json::json!([
                    {"type": "tool_use", "id": "call_A", "name": "a", "input": {}},
                    {"type": "tool_use", "id": "call_B", "name": "b", "input": {}},
                ])
                .to_string(),
            ),
            tool_result("call_A", "result A"),
            // Missing call_B result
            msg("user", "next"),
        ];
        repair_tool_pairing(&mut history);
        // Should inject synthetic for call_B
        let tool_msgs: Vec<_> = history.iter().filter(|m| m.role == "tool").collect();
        assert_eq!(tool_msgs.len(), 2);
        assert!(tool_msgs[1].content.contains("call_B"));
        assert!(tool_msgs[1].content.contains("missing"));
    }

    #[test]
    fn plain_text_assistant_unchanged() {
        let mut history = vec![
            msg("system", "sys"),
            msg("user", "hello"),
            msg("assistant", "just text, no tools"),
            msg("user", "ok"),
        ];
        let before = history.len();
        repair_tool_pairing(&mut history);
        assert_eq!(history.len(), before);
    }
}
