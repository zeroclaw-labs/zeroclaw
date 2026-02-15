//! Transcript repair — fix orphaned tool results, missing results, duplicates,
//! and enforce role alternation before sending messages to the LLM.
//!
//! The LLM API expects:
//! 1. Every `tool_use` block in an assistant message has exactly one matching
//!    `tool_result` in a subsequent user/tool message.
//! 2. Messages strictly alternate user → assistant → user → …
//!
//! These repairs run on the in-memory message list before each API call so that
//! transient corruption (crashes mid-turn, duplicate appends) never reaches the
//! model.

use std::collections::{HashMap, HashSet};

use super::types::{AgentMessage, ContentBlock, Role};

/// Summary of what `repair_tool_use` changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolRepairReport {
    /// Tool-use IDs that had no matching result — a synthetic error result was inserted.
    pub missing_results: Vec<String>,
    /// Tool-result IDs that had no matching `tool_use` — dropped.
    pub orphaned_results: Vec<String>,
    /// Tool-result IDs that appeared more than once — duplicates removed.
    pub deduplicated_results: Vec<String>,
}

// ── Tool-use / tool-result repair ────────────────────────────────

/// Repair tool-use ↔ tool-result pairing in a message list.
///
/// 1. **Missing results** — if an assistant message contains a `ToolUse` block
///    whose `id` never appears in any subsequent `ToolResult`, a synthetic error
///    result is appended in a new tool-role message.
/// 2. **Orphaned results** — `ToolResult` blocks whose `tool_use_id` does not
///    match any preceding `ToolUse` are removed.
/// 3. **Duplicate results** — only the first `ToolResult` for a given
///    `tool_use_id` is kept; later duplicates are removed.
///
/// Returns the repaired message list and a report of changes.
pub fn repair_tool_use(messages: &[AgentMessage]) -> (Vec<AgentMessage>, ToolRepairReport) {
    let mut report = ToolRepairReport::default();

    // Pass 1: collect all tool_use ids in order.
    let mut tool_use_ids: Vec<String> = Vec::new();
    let mut tool_use_set: HashSet<String> = HashSet::new();
    for msg in messages {
        if msg.role == Role::Assistant {
            for block in &msg.content {
                if let ContentBlock::ToolUse { id, .. } = block {
                    if tool_use_set.insert(id.clone()) {
                        tool_use_ids.push(id.clone());
                    }
                }
            }
        }
    }

    // Pass 2: collect all tool_result ids, tracking first-seen for dedup.
    let mut seen_results: HashMap<String, usize> = HashMap::new(); // tool_use_id → count
    for msg in messages {
        for block in &msg.content {
            if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                *seen_results.entry(tool_use_id.clone()).or_insert(0) += 1;
            }
        }
    }

    // Pass 3: rebuild messages with orphan/dup removal.
    let mut result_messages: Vec<AgentMessage> = Vec::new();
    let mut result_seen: HashSet<String> = HashSet::new(); // track first-seen for dedup

    for msg in messages {
        let mut new_content: Vec<ContentBlock> = Vec::new();

        for block in &msg.content {
            match block {
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    if !tool_use_set.contains(tool_use_id) {
                        // Orphaned result — no matching tool_use
                        report.orphaned_results.push(tool_use_id.clone());
                        continue;
                    }
                    if !result_seen.insert(tool_use_id.clone()) {
                        // Duplicate result
                        report.deduplicated_results.push(tool_use_id.clone());
                        continue;
                    }
                    new_content.push(block.clone());
                }
                _ => {
                    new_content.push(block.clone());
                }
            }
        }

        // Only keep messages that still have content.
        if !new_content.is_empty() {
            let mut repaired = msg.clone();
            repaired.content = new_content;
            result_messages.push(repaired);
        }
    }

    // Pass 4: insert synthetic error results for missing tool results.
    let result_ids: HashSet<&String> = result_seen.iter().collect();
    let mut missing: Vec<String> = Vec::new();
    for id in &tool_use_ids {
        if !result_ids.contains(id) {
            missing.push(id.clone());
        }
    }

    if !missing.is_empty() {
        let synthetic_blocks: Vec<ContentBlock> = missing
            .iter()
            .map(|id| ContentBlock::ToolResult {
                tool_use_id: id.clone(),
                content: "Error: tool execution was interrupted — no result recorded.".into(),
                is_error: true,
            })
            .collect();

        report.missing_results = missing;

        result_messages.push(AgentMessage {
            message_id: None,
            role: Role::User,
            content: synthetic_blocks,
            timestamp: None,
            usage: None,
            model: None,
            metadata: None,
        });
    }

    (result_messages, report)
}

// ── Role alternation repair ──────────────────────────────────────

/// Enforce strict user/assistant role alternation.
///
/// The LLM API requires that messages alternate between user and assistant
/// roles. If two consecutive messages share the same role, they are merged
/// (their content blocks concatenated). System messages are left in place
/// at the start; tool-role messages are treated as user-role for alternation
/// purposes.
pub fn repair_role_ordering(messages: &[AgentMessage]) -> Vec<AgentMessage> {
    if messages.is_empty() {
        return Vec::new();
    }

    let mut result: Vec<AgentMessage> = Vec::new();

    for msg in messages {
        let effective_role = match msg.role {
            Role::Tool => Role::User,
            Role::System => {
                // System messages pass through — only expected at the start.
                result.push(msg.clone());
                continue;
            }
            ref r => r.clone(),
        };

        if let Some(last) = result.last_mut() {
            let last_effective = match last.role {
                Role::Tool => Role::User,
                ref r => r.clone(),
            };

            if last_effective == effective_role && last_effective != Role::System {
                // Same role as previous — merge content blocks.
                last.content.extend(msg.content.clone());
                // Keep the later timestamp if present.
                if msg.timestamp.is_some() {
                    last.timestamp = msg.timestamp.clone();
                }
                // Merge usage.
                if let (Some(ref mut a), Some(ref b)) = (&mut last.usage, &msg.usage) {
                    a.input_tokens += b.input_tokens;
                    a.output_tokens += b.output_tokens;
                    a.cache_read_tokens += b.cache_read_tokens;
                    a.cache_write_tokens += b.cache_write_tokens;
                } else if last.usage.is_none() && msg.usage.is_some() {
                    last.usage = msg.usage.clone();
                }
                continue;
            }
        }

        // Convert tool role to user for the output.
        let mut out = msg.clone();
        if out.role == Role::Tool {
            out.role = Role::User;
        }
        result.push(out);
    }

    result
}

/// Extract all `tool_use` IDs from a message list.
pub fn extract_tool_call_ids(messages: &[AgentMessage]) -> Vec<String> {
    let mut ids = Vec::new();
    for msg in messages {
        for block in &msg.content {
            if let ContentBlock::ToolUse { id, .. } = block {
                ids.push(id.clone());
            }
        }
    }
    ids
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::types::NormalizedUsage;

    fn user_msg(text: &str) -> AgentMessage {
        AgentMessage {
            message_id: None,
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
            timestamp: None,
            usage: None,
            model: None,
            metadata: None,
        }
    }

    fn assistant_msg(text: &str) -> AgentMessage {
        AgentMessage {
            message_id: None,
            role: Role::Assistant,
            content: vec![ContentBlock::Text { text: text.into() }],
            timestamp: None,
            usage: None,
            model: None,
            metadata: None,
        }
    }

    fn assistant_with_tool(text: &str, tool_id: &str, tool_name: &str) -> AgentMessage {
        AgentMessage {
            message_id: None,
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text { text: text.into() },
                ContentBlock::ToolUse {
                    id: tool_id.into(),
                    name: tool_name.into(),
                    input: serde_json::json!({}),
                },
            ],
            timestamp: None,
            usage: None,
            model: None,
            metadata: None,
        }
    }

    fn tool_result_msg(tool_use_id: &str, content: &str) -> AgentMessage {
        AgentMessage {
            message_id: None,
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.into(),
                content: content.into(),
                is_error: false,
            }],
            timestamp: None,
            usage: None,
            model: None,
            metadata: None,
        }
    }

    // ── repair_tool_use ──────────────────────────────────────────

    #[test]
    fn no_repairs_needed() {
        let messages = vec![
            user_msg("hello"),
            assistant_with_tool("let me search", "tu-1", "search"),
            tool_result_msg("tu-1", "found it"),
            assistant_msg("here you go"),
        ];

        let (repaired, report) = repair_tool_use(&messages);
        assert_eq!(repaired.len(), 4);
        assert!(report.missing_results.is_empty());
        assert!(report.orphaned_results.is_empty());
        assert!(report.deduplicated_results.is_empty());
    }

    #[test]
    fn missing_tool_result_gets_synthetic_error() {
        let messages = vec![
            user_msg("hello"),
            assistant_with_tool("let me search", "tu-1", "search"),
            // No tool result for tu-1!
            assistant_msg("oops"),
        ];

        let (repaired, report) = repair_tool_use(&messages);
        assert_eq!(report.missing_results, vec!["tu-1"]);

        // Should have appended a synthetic result message.
        let last = repaired.last().unwrap();
        assert_eq!(last.role, Role::User);
        assert!(matches!(
            &last.content[0],
            ContentBlock::ToolResult { is_error: true, .. }
        ));
    }

    #[test]
    fn orphaned_tool_result_is_dropped() {
        let messages = vec![
            user_msg("hello"),
            assistant_msg("sure"),
            // Tool result with no matching tool_use.
            tool_result_msg("tu-ghost", "phantom result"),
        ];

        let (repaired, report) = repair_tool_use(&messages);
        assert_eq!(report.orphaned_results, vec!["tu-ghost"]);

        // The orphan message had only a single block which was removed,
        // so the entire message should be gone.
        assert_eq!(repaired.len(), 2);
    }

    #[test]
    fn duplicate_tool_result_is_deduplicated() {
        let messages = vec![
            user_msg("hello"),
            assistant_with_tool("searching", "tu-1", "search"),
            tool_result_msg("tu-1", "first result"),
            tool_result_msg("tu-1", "duplicate result"),
        ];

        let (repaired, report) = repair_tool_use(&messages);
        assert_eq!(report.deduplicated_results, vec!["tu-1"]);

        // Count how many ToolResult blocks reference tu-1.
        let result_count: usize = repaired
            .iter()
            .flat_map(|m| &m.content)
            .filter(|b| matches!(b, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "tu-1"))
            .count();
        assert_eq!(result_count, 1);
    }

    #[test]
    fn mixed_repairs() {
        let messages = vec![
            user_msg("hello"),
            assistant_with_tool("doing two things", "tu-1", "read"),
            assistant_with_tool("and another", "tu-2", "write"),
            tool_result_msg("tu-1", "done reading"),
            // tu-2 is missing, tu-orphan is orphaned
            tool_result_msg("tu-orphan", "orphan"),
        ];

        let (repaired, report) = repair_tool_use(&messages);
        assert_eq!(report.missing_results, vec!["tu-2"]);
        assert_eq!(report.orphaned_results, vec!["tu-orphan"]);
        assert!(report.deduplicated_results.is_empty());

        // Verify synthetic error for tu-2 exists.
        let has_synthetic = repaired.iter().any(|m| {
            m.content.iter().any(|b| {
                matches!(b, ContentBlock::ToolResult { tool_use_id, is_error: true, .. } if tool_use_id == "tu-2")
            })
        });
        assert!(has_synthetic);
    }

    // ── repair_role_ordering ─────────────────────────────────────

    #[test]
    fn already_alternating() {
        let messages = vec![
            user_msg("hello"),
            assistant_msg("hi"),
            user_msg("how are you"),
            assistant_msg("good"),
        ];

        let repaired = repair_role_ordering(&messages);
        assert_eq!(repaired.len(), 4);
    }

    #[test]
    fn consecutive_user_messages_merge() {
        let messages = vec![
            user_msg("hello"),
            user_msg("world"),
            assistant_msg("hi there"),
        ];

        let repaired = repair_role_ordering(&messages);
        assert_eq!(repaired.len(), 2);
        assert_eq!(repaired[0].content.len(), 2); // merged
        assert_eq!(repaired[0].role, Role::User);
        assert_eq!(repaired[1].role, Role::Assistant);
    }

    #[test]
    fn consecutive_assistant_messages_merge() {
        let messages = vec![
            user_msg("hello"),
            assistant_msg("part 1"),
            assistant_msg("part 2"),
        ];

        let repaired = repair_role_ordering(&messages);
        assert_eq!(repaired.len(), 2);
        assert_eq!(repaired[1].content.len(), 2);
        assert_eq!(repaired[1].role, Role::Assistant);
    }

    #[test]
    fn tool_role_treated_as_user() {
        let messages = vec![
            user_msg("hello"),
            assistant_with_tool("searching", "tu-1", "search"),
            AgentMessage {
                message_id: None,
                role: Role::Tool,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "tu-1".into(),
                    content: "found".into(),
                    is_error: false,
                }],
                timestamp: None,
                usage: None,
                model: None,
                metadata: None,
            },
            assistant_msg("here you go"),
        ];

        let repaired = repair_role_ordering(&messages);
        // Tool message should be converted to user role.
        assert_eq!(repaired[2].role, Role::User);
    }

    #[test]
    fn usage_merging_on_role_collapse() {
        let msg1 = AgentMessage {
            message_id: None,
            role: Role::User,
            content: vec![ContentBlock::Text { text: "a".into() }],
            timestamp: None,
            usage: Some(NormalizedUsage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            }),
            model: None,
            metadata: None,
        };
        let msg2 = AgentMessage {
            message_id: None,
            role: Role::User,
            content: vec![ContentBlock::Text { text: "b".into() }],
            timestamp: None,
            usage: Some(NormalizedUsage {
                input_tokens: 20,
                output_tokens: 10,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            }),
            model: None,
            metadata: None,
        };

        let repaired = repair_role_ordering(&[msg1, msg2]);
        assert_eq!(repaired.len(), 1);
        let usage = repaired[0].usage.as_ref().unwrap();
        assert_eq!(usage.input_tokens, 30);
        assert_eq!(usage.output_tokens, 15);
    }

    #[test]
    fn empty_input_returns_empty() {
        assert!(repair_role_ordering(&[]).is_empty());
        let (repaired, report) = repair_tool_use(&[]);
        assert!(repaired.is_empty());
        assert_eq!(report, ToolRepairReport::default());
    }

    #[test]
    fn system_messages_pass_through() {
        let messages = vec![
            AgentMessage {
                message_id: None,
                role: Role::System,
                content: vec![ContentBlock::Text {
                    text: "system prompt".into(),
                }],
                timestamp: None,
                usage: None,
                model: None,
                metadata: None,
            },
            user_msg("hello"),
            assistant_msg("hi"),
        ];

        let repaired = repair_role_ordering(&messages);
        assert_eq!(repaired.len(), 3);
        assert_eq!(repaired[0].role, Role::System);
    }

    // ── extract_tool_call_ids ────────────────────────────────────

    #[test]
    fn extract_tool_ids() {
        let messages = vec![
            user_msg("hello"),
            assistant_with_tool("a", "tu-1", "read"),
            assistant_with_tool("b", "tu-2", "write"),
            tool_result_msg("tu-1", "done"),
        ];

        let ids = extract_tool_call_ids(&messages);
        assert_eq!(ids, vec!["tu-1", "tu-2"]);
    }

    #[test]
    fn extract_no_tool_ids() {
        let messages = vec![user_msg("hello"), assistant_msg("hi")];
        assert!(extract_tool_call_ids(&messages).is_empty());
    }
}
