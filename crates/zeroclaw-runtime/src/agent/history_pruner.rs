use zeroclaw_api::provider::ChatMessage;

pub use zeroclaw_config::scattered_types::HistoryPrunerConfig;

// ---------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneStats {
    pub messages_before: usize,
    pub messages_after: usize,
    pub collapsed_pairs: usize,
    pub dropped_messages: usize,
}

// ---------------------------------------------------------------------------
// Token estimation
// ---------------------------------------------------------------------------

fn estimate_tokens(messages: &[ChatMessage]) -> usize {
    let raw: usize = messages
        .iter()
        .map(|m| m.content.len().div_ceil(4) + 4)
        .sum();
    // Apply 1.2x safety margin consistent with context_compressor to avoid
    // underestimation that leads to context_length_exceeded errors.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        (raw as f64 * 1.2) as usize
    }
}

// ---------------------------------------------------------------------------
// Protected-index helpers
// ---------------------------------------------------------------------------

fn protected_indices(messages: &[ChatMessage], keep_recent: usize) -> Vec<bool> {
    let len = messages.len();
    let mut protected = vec![false; len];
    for (i, msg) in messages.iter().enumerate() {
        if msg.role == "system" {
            protected[i] = true;
        }
    }
    let recent_start = len.saturating_sub(keep_recent);
    for p in protected.iter_mut().skip(recent_start) {
        *p = true;
    }
    protected
}

// ---------------------------------------------------------------------------
// Orphaned tool-message sanitiser
// ---------------------------------------------------------------------------

/// Remove `tool`-role messages whose `tool_call_id` has no matching
/// `tool_use` / `tool_calls` entry in a preceding assistant message.
///
/// After any history truncation (drain, remove, prune) the first surviving
/// message(s) may be `tool` results whose assistant request was trimmed away.
/// The Anthropic API (and others) reject these with a 400 error.
///
/// Returns the number of messages removed.
pub fn remove_orphaned_tool_messages(messages: &mut Vec<ChatMessage>) -> usize {
    // Pass 1: Remove assistant(tool_calls) + their tool_results when the
    // assistant is preceded by another assistant. Normalization would merge
    // them, destroying structured tool_use blocks and orphaning the results.
    let mut removed = 0usize;
    let mut i = 0;
    while i < messages.len() {
        if messages[i].role == "assistant"
            && messages[i].content.contains("tool_calls")
            && i > 0
            && messages[i - 1].role == "assistant"
        {
            // Collect tool_call_ids from this assistant to find matching tool_results.
            let doomed_content = messages[i].content.clone();
            messages.remove(i);
            removed += 1;
            // Remove following tool messages that reference this assistant.
            while i < messages.len() && messages[i].role == "tool" {
                let dominated = match extract_tool_call_id(&messages[i].content) {
                    Some(id) => doomed_content.contains(&id),
                    None => true,
                };
                if dominated {
                    messages.remove(i);
                    removed += 1;
                } else {
                    break;
                }
            }
        } else {
            i += 1;
        }
    }

    // Pass 2: Remove remaining orphan tool messages whose tool_call_id
    // doesn't appear in the immediately preceding assistant.
    i = 0;
    while i < messages.len() {
        if messages[i].role != "tool" {
            i += 1;
            continue;
        }

        let assistant_idx = (0..i)
            .rev()
            .take_while(|&j| messages[j].role == "assistant" || messages[j].role == "tool")
            .find(|&j| messages[j].role == "assistant");

        let is_orphan = match assistant_idx {
            None => true,
            Some(idx) => {
                let assistant_content = &messages[idx].content;
                if assistant_content.contains("tool_calls") {
                    match extract_tool_call_id(&messages[i].content) {
                        Some(tool_call_id) => !assistant_content.contains(&tool_call_id),
                        None => false,
                    }
                } else {
                    true
                }
            }
        };

        if is_orphan {
            messages.remove(i);
            removed += 1;
        } else {
            i += 1;
        }
    }
    if removed > 0 {
        tracing::warn!(
            count = removed,
            "Removed {removed} orphaned tool message(s) from history — this indicates a prior \
             tool_use/tool_result pairing inconsistency that was auto-healed"
        );
    }
    removed
}

/// Try to extract a `tool_call_id` from a tool-role message's JSON content.
///
/// Tool messages are stored as JSON like:
/// `{"content": "...", "tool_call_id": "toolu_01Abc..."}`
fn extract_tool_call_id(content: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(content).ok()?;
    value
        .get("tool_call_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Strip `tool_calls` entries from assistant messages when no following
/// `tool` message pairs with the call's id.
///
/// This complements `remove_orphaned_tool_messages`, which only handles the
/// inverse case (tool messages without a matching assistant). Unpaired
/// `tool_use` blocks in assistant messages cause Bedrock/Anthropic to reject
/// the next request with: "Expected toolResult blocks at messages.N.content
/// for the following Ids: tooluse_*". The usual trigger is the agent loop
/// hitting `max_tool_iterations` immediately after emitting a tool_use but
/// before the runner recorded the tool_result.
///
/// Behaviour:
/// * If SOME of an assistant's `tool_calls` ids pair with later `tool`
///   messages and some do not, the unpaired entries are removed and the
///   others are retained.
/// * If NONE of the `tool_calls` pair, the `tool_calls` field is removed
///   entirely; the assistant's text content is preserved.
///
/// Returns the number of assistant messages that had at least one unpaired
/// tool_call stripped.
pub fn strip_orphaned_tool_calls_from_assistants(messages: &mut [ChatMessage]) -> usize {
    // suffix_tool_ids[i] = set of tool_call_ids referenced by tool-role
    // messages at positions >= i. Pre-computed so each assistant check is O(1)
    // in message-index lookups.
    let mut suffix_tool_ids: Vec<std::collections::HashSet<String>> =
        vec![std::collections::HashSet::new(); messages.len() + 1];
    for i in (0..messages.len()).rev() {
        let mut set = suffix_tool_ids[i + 1].clone();
        if messages[i].role == "tool"
            && let Some(id) = extract_tool_call_id(&messages[i].content)
        {
            set.insert(id);
        }
        suffix_tool_ids[i] = set;
    }

    let mut stripped = 0usize;
    for (idx, message) in messages.iter_mut().enumerate() {
        if message.role != "assistant" || !message.content.contains("tool_calls") {
            continue;
        }
        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&message.content) else {
            continue;
        };
        let Some(calls) = value.get("tool_calls").and_then(|v| v.as_array()) else {
            continue;
        };

        let following_ids = &suffix_tool_ids[idx + 1];
        let paired_calls: Vec<serde_json::Value> = calls
            .iter()
            .filter(|call| {
                call.get("id")
                    .and_then(|v| v.as_str())
                    .is_some_and(|id| following_ids.contains(id))
            })
            .cloned()
            .collect();

        if paired_calls.len() == calls.len() {
            continue; // every tool_call is paired — nothing to do
        }

        let orphan_ids: Vec<String> = calls
            .iter()
            .filter_map(|call| call.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()))
            .filter(|id| !following_ids.contains(id))
            .collect();

        if let serde_json::Value::Object(ref mut map) = value {
            if paired_calls.is_empty() {
                map.remove("tool_calls");
            } else {
                map.insert(
                    "tool_calls".to_string(),
                    serde_json::Value::Array(paired_calls),
                );
            }
        }
        message.content = value.to_string();
        stripped += 1;

        tracing::warn!(
            orphan_ids = ?orphan_ids,
            "Stripped unpaired tool_calls from assistant history message — likely a \
             max_tool_iterations early exit"
        );
    }
    stripped
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn prune_history(messages: &mut Vec<ChatMessage>, config: &HistoryPrunerConfig) -> PruneStats {
    let messages_before = messages.len();
    if !config.enabled || messages.is_empty() {
        return PruneStats {
            messages_before,
            messages_after: messages_before,
            collapsed_pairs: 0,
            dropped_messages: 0,
        };
    }

    let mut collapsed_pairs: usize = 0;

    // Phase 1 – collapse assistant+tool groups atomically.
    // An assistant message followed by one or more consecutive tool messages
    // forms an atomic group (tool_use + tool_result pairing). Collapsing only
    // part of the group would orphan tool_use blocks, causing API 400 errors
    // from providers that enforce pairing (e.g., Anthropic). See #4810.
    if config.collapse_tool_results {
        let mut i = 0;
        while i < messages.len() {
            let protected = protected_indices(messages, config.keep_recent);
            if messages[i].role == "assistant" && !protected[i] {
                // Count consecutive tool messages following this assistant
                let mut tool_count = 0;
                while i + 1 + tool_count < messages.len()
                    && messages[i + 1 + tool_count].role == "tool"
                    && !protected[i + 1 + tool_count]
                {
                    tool_count += 1;
                }
                if tool_count > 0 {
                    let summary =
                        format!("[Tool exchange: {tool_count} tool call(s) — results collapsed]");
                    messages[i] = ChatMessage {
                        role: "assistant".to_string(),
                        content: summary,
                    };
                    for _ in 0..tool_count {
                        messages.remove(i + 1);
                    }
                    collapsed_pairs += tool_count;
                    continue;
                }
            }
            i += 1;
        }
    }

    // Phase 2 – budget enforcement: drop messages to fit token budget.
    // Tool groups (assistant + consecutive tool messages) are dropped
    // atomically to preserve tool_use/tool_result pairing. See #4810.
    let mut dropped_messages: usize = 0;
    while estimate_tokens(messages) > config.max_tokens {
        let protected = protected_indices(messages, config.keep_recent);
        let mut dropped_any = false;
        let mut i = 0;
        while i < messages.len() {
            if protected[i] {
                i += 1;
                continue;
            }
            if messages[i].role == "assistant" {
                // Count following tool messages — drop as atomic group,
                // but skip if any tool in the group is protected.
                let mut tool_count = 0;
                let mut any_tool_protected = false;
                while i + 1 + tool_count < messages.len()
                    && messages[i + 1 + tool_count].role == "tool"
                {
                    if protected[i + 1 + tool_count] {
                        any_tool_protected = true;
                    }
                    tool_count += 1;
                }
                if tool_count > 0 && !any_tool_protected {
                    for _ in 0..=tool_count {
                        messages.remove(i);
                    }
                    dropped_messages += 1 + tool_count;
                    dropped_any = true;
                    break;
                } else if tool_count > 0 {
                    // Group has protected tools — skip past it
                    i += 1 + tool_count;
                    continue;
                }
            }
            // Non-tool-group message — safe to drop individually
            messages.remove(i);
            dropped_messages += 1;
            dropped_any = true;
            break;
        }
        if !dropped_any {
            break;
        }
    }

    // Phase 3 – remove orphaned tool messages left behind by phases 1-2.
    let orphans_removed = remove_orphaned_tool_messages(messages);
    dropped_messages += orphans_removed;

    PruneStats {
        messages_before,
        messages_after: messages.len(),
        collapsed_pairs,
        dropped_messages,
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

    #[test]
    fn prune_disabled_is_noop() {
        let mut messages = vec![
            msg("system", "You are helpful."),
            msg("user", "Hello"),
            msg("assistant", "Hi there!"),
        ];
        let config = HistoryPrunerConfig {
            enabled: false,
            ..Default::default()
        };
        let stats = prune_history(&mut messages, &config);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].content, "You are helpful.");
        assert_eq!(stats.messages_before, 3);
        assert_eq!(stats.messages_after, 3);
        assert_eq!(stats.collapsed_pairs, 0);
    }

    #[test]
    fn prune_under_budget_no_change() {
        let mut messages = vec![
            msg("system", "You are helpful."),
            msg("user", "Hello"),
            msg("assistant", "Hi!"),
        ];
        let config = HistoryPrunerConfig {
            enabled: true,
            max_tokens: 8192,
            keep_recent: 2,
            collapse_tool_results: false,
        };
        let stats = prune_history(&mut messages, &config);
        assert_eq!(messages.len(), 3);
        assert_eq!(stats.collapsed_pairs, 0);
        assert_eq!(stats.dropped_messages, 0);
    }

    #[test]
    fn prune_collapses_tool_pairs() {
        let tool_result = "a".repeat(160);
        let mut messages = vec![
            msg("system", "sys"),
            msg("assistant", "calling tool X"),
            msg("tool", &tool_result),
            msg("user", "thanks"),
            msg("assistant", "done"),
        ];
        let config = HistoryPrunerConfig {
            enabled: true,
            max_tokens: 100_000,
            keep_recent: 2,
            collapse_tool_results: true,
        };
        let stats = prune_history(&mut messages, &config);
        assert_eq!(stats.collapsed_pairs, 1);
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[1].role, "assistant");
        assert!(messages[1].content.contains("1 tool call(s)"));
    }

    #[test]
    fn prune_preserves_system_and_recent() {
        let big = "x".repeat(40_000);
        let mut messages = vec![
            msg("system", "system prompt"),
            msg("user", &big),
            msg("assistant", "old reply"),
            msg("user", "recent1"),
            msg("assistant", "recent2"),
        ];
        let config = HistoryPrunerConfig {
            enabled: true,
            max_tokens: 100,
            keep_recent: 2,
            collapse_tool_results: false,
        };
        let stats = prune_history(&mut messages, &config);
        assert!(messages.iter().any(|m| m.role == "system"));
        assert!(messages.iter().any(|m| m.content == "recent1"));
        assert!(messages.iter().any(|m| m.content == "recent2"));
        assert!(stats.dropped_messages > 0);
    }

    #[test]
    fn prune_drops_oldest_when_over_budget() {
        let filler = "y".repeat(400);
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", &filler),
            msg("assistant", &filler),
            msg("user", "recent-user"),
            msg("assistant", "recent-assistant"),
        ];
        let config = HistoryPrunerConfig {
            enabled: true,
            max_tokens: 150,
            keep_recent: 2,
            collapse_tool_results: false,
        };
        let stats = prune_history(&mut messages, &config);
        assert!(stats.dropped_messages >= 1);
        assert_eq!(messages[0].role, "system");
        assert!(messages.iter().any(|m| m.content == "recent-user"));
        assert!(messages.iter().any(|m| m.content == "recent-assistant"));
    }

    #[test]
    fn prune_empty_messages() {
        let mut messages: Vec<ChatMessage> = vec![];
        let config = HistoryPrunerConfig {
            enabled: true,
            ..Default::default()
        };
        let stats = prune_history(&mut messages, &config);
        assert_eq!(stats.messages_before, 0);
        assert_eq!(stats.messages_after, 0);
    }

    #[test]
    fn prune_collapses_multi_tool_group() {
        let mut messages = vec![
            msg("system", "sys"),
            msg(
                "assistant",
                r#"{"content":null,"tool_calls":[{"id":"t1","name":"shell","arguments":"{}"},{"id":"t2","name":"web","arguments":"{}"}]}"#,
            ),
            msg("tool", r#"{"tool_call_id":"t1","content":"result1"}"#),
            msg("tool", r#"{"tool_call_id":"t2","content":"result2"}"#),
            msg("user", "thanks"),
            msg("assistant", "done"),
        ];
        let config = HistoryPrunerConfig {
            enabled: true,
            max_tokens: 100_000,
            keep_recent: 2,
            collapse_tool_results: true,
        };
        let stats = prune_history(&mut messages, &config);
        assert_eq!(stats.collapsed_pairs, 2);
        // assistant(tool_calls) + 2 tool messages → 1 summary assistant
        assert_eq!(messages.len(), 4); // sys, summary, user, assistant
        assert!(messages[1].content.contains("2 tool call(s)"));
        // No tool messages remain
        assert!(!messages.iter().any(|m| m.role == "tool"));
    }

    #[test]
    fn prune_drops_tool_group_atomically() {
        let big = "x".repeat(2000);
        let mut messages = vec![
            msg("system", "sys"),
            msg("assistant", &big),
            msg("tool", &big),
            msg("tool", &big),
            msg("user", "recent"),
            msg("assistant", "recent reply"),
        ];
        let config = HistoryPrunerConfig {
            enabled: true,
            max_tokens: 50, // very low — forces drops
            keep_recent: 2,
            collapse_tool_results: false, // skip collapse, go straight to drop
        };
        let stats = prune_history(&mut messages, &config);
        assert!(stats.dropped_messages >= 3); // assistant + 2 tools dropped together
        // No orphaned tool messages
        for (i, m) in messages.iter().enumerate() {
            if m.role == "tool" {
                assert!(
                    i > 0 && messages[i - 1].role == "assistant",
                    "tool message at index {i} has no preceding assistant"
                );
            }
        }
    }

    #[test]
    fn prune_never_orphans_tool_use() {
        // Simulate a conversation with multiple tool groups
        let filler = "y".repeat(500);
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", "q1"),
            msg("assistant", &filler), // tool group 1
            msg("tool", &filler),
            msg("user", "q2"),
            msg("assistant", &filler), // tool group 2
            msg("tool", &filler),
            msg("tool", &filler),
            msg("user", "recent"),
            msg("assistant", "recent reply"),
        ];
        let config = HistoryPrunerConfig {
            enabled: true,
            max_tokens: 100,
            keep_recent: 2,
            collapse_tool_results: true,
        };
        prune_history(&mut messages, &config);
        // Verify invariant: no tool message without a preceding assistant
        for (i, m) in messages.iter().enumerate() {
            if m.role == "tool" {
                assert!(
                    i > 0 && messages[i - 1].role == "assistant",
                    "orphaned tool message at index {i}: {:?}",
                    messages.iter().map(|m| &m.role).collect::<Vec<_>>()
                );
            }
        }
    }

    #[test]
    fn prune_protects_recent_tool_groups() {
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", "old"),
            msg("assistant", "old reply"),
            msg("user", "do something"),
            msg(
                "assistant",
                r#"{"content":"checking","tool_calls":[{"id":"toolu_recent","name":"shell","arguments":"{}"}]}"#,
            ),
            msg(
                "tool",
                r#"{"tool_call_id":"toolu_recent","content":"tool result"}"#,
            ),
            msg("user", "recent"),
        ];
        let config = HistoryPrunerConfig {
            enabled: true,
            max_tokens: 100_000,
            keep_recent: 3, // protects last 3: tool call, tool result, recent
            collapse_tool_results: true,
        };
        let stats = prune_history(&mut messages, &config);
        // Protected tool group should not be collapsed
        assert!(messages.iter().any(|m| m.role == "tool"));
        assert_eq!(stats.collapsed_pairs, 0);
    }

    #[test]
    fn prune_under_realistic_token_pressure_preserves_tool_pairing() {
        // Simulate 15 tool iterations with realistic content sizes
        let mut messages = vec![msg("system", "You are helpful.")];
        messages.push(msg("user", "Research this topic thoroughly"));

        // 15 tool iterations — each adds assistant(tool_calls) + tool(result)
        for i in 0..15 {
            let tool_json = format!(
                r#"{{"content":"iteration {i}","tool_calls":[{{"id":"t{i}","name":"web_search","arguments":"{{}}"}}]}}"#
            );
            messages.push(msg("assistant", &tool_json));
            // Realistic tool result size (~2K chars each)
            let result = format!(
                r#"{{"tool_call_id":"t{i}","content":"{}"}}"#,
                "x".repeat(2000)
            );
            messages.push(msg("tool", &result));
        }
        messages.push(msg("assistant", "Here's what I found..."));

        // 33 messages total: system + user + 15*(assistant+tool) + final assistant
        assert_eq!(messages.len(), 33);

        let config = HistoryPrunerConfig {
            enabled: true,
            max_tokens: 2000, // Forces pruning of older iterations
            keep_recent: 4,
            collapse_tool_results: true,
        };

        prune_history(&mut messages, &config);

        // Invariant: no orphaned tool messages after pruning
        for (i, m) in messages.iter().enumerate() {
            if m.role == "tool" {
                assert!(
                    i > 0 && messages[i - 1].role == "assistant",
                    "orphaned tool at index {i}: roles = {:?}",
                    messages.iter().map(|m| &m.role).collect::<Vec<_>>()
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // remove_orphaned_tool_messages tests
    // -----------------------------------------------------------------------

    #[test]
    fn orphan_tool_at_start_is_removed() {
        // Simulates the exact bug: session drain removes the assistant
        // message but leaves its tool results at the start.
        let mut messages = vec![
            msg("system", "sys"),
            msg(
                "tool",
                r#"{"content":"file listing","tool_call_id":"toolu_01HiJXWbhx"}"#,
            ),
            msg(
                "tool",
                r#"{"content":"another result","tool_call_id":"toolu_01AQP25qUz"}"#,
            ),
            msg("user", "thanks"),
            msg("assistant", "done"),
        ];
        let removed = remove_orphaned_tool_messages(&mut messages);
        assert_eq!(removed, 2);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[2].role, "assistant");
    }

    #[test]
    fn valid_tool_pair_preserved() {
        // A properly paired assistant+tool sequence must survive.
        let assistant_with_tools = r#"{"content":"checking","tool_calls":[{"id":"toolu_abc123","name":"shell","arguments":"{}"}]}"#;
        let tool_result = r#"{"content":"ok","tool_call_id":"toolu_abc123"}"#;
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", "do it"),
            msg("assistant", assistant_with_tools),
            msg("tool", tool_result),
            msg("assistant", "done"),
        ];
        let removed = remove_orphaned_tool_messages(&mut messages);
        assert_eq!(removed, 0);
        assert_eq!(messages.len(), 5);
    }

    #[test]
    fn multi_tool_call_batch_preserved() {
        // An assistant with 3 tool_calls followed by 3 tool results.
        let assistant_content = r#"{"content":"running","tool_calls":[{"id":"toolu_aaa","name":"shell","arguments":"{}"},{"id":"toolu_bbb","name":"shell","arguments":"{}"},{"id":"toolu_ccc","name":"shell","arguments":"{}"}]}"#;
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", "do all 3"),
            msg("assistant", assistant_content),
            msg("tool", r#"{"content":"r1","tool_call_id":"toolu_aaa"}"#),
            msg("tool", r#"{"content":"r2","tool_call_id":"toolu_bbb"}"#),
            msg("tool", r#"{"content":"r3","tool_call_id":"toolu_ccc"}"#),
            msg("assistant", "all done"),
        ];
        let removed = remove_orphaned_tool_messages(&mut messages);
        assert_eq!(removed, 0);
        assert_eq!(messages.len(), 7);
    }

    #[test]
    fn mismatched_tool_id_is_removed() {
        // Tool result references a tool_call_id not in the assistant message.
        let assistant_content = r#"{"content":"running","tool_calls":[{"id":"toolu_aaa","name":"shell","arguments":"{}"}]}"#;
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", "go"),
            msg("assistant", assistant_content),
            msg("tool", r#"{"content":"ok","tool_call_id":"toolu_aaa"}"#),
            msg("tool", r#"{"content":"stale","tool_call_id":"toolu_GONE"}"#),
            msg("assistant", "done"),
        ];
        let removed = remove_orphaned_tool_messages(&mut messages);
        assert_eq!(removed, 1);
        assert_eq!(messages.len(), 5);
        // The valid tool result stays, the orphan is gone.
        assert_eq!(messages[3].role, "tool");
        assert!(messages[3].content.contains("toolu_aaa"));
    }

    #[test]
    fn orphan_tool_in_middle_after_collapsed_pair() {
        // Phase 1 collapsed an assistant+tool pair into a summary, but
        // a subsequent tool message referenced the original tool_call_id.
        let mut messages = vec![
            msg("system", "sys"),
            msg("assistant", "[Tool result: truncated...]"), // collapsed
            msg(
                "tool",
                r#"{"content":"leftover","tool_call_id":"toolu_OLD"}"#,
            ),
            msg("user", "next"),
            msg("assistant", "ok"),
        ];
        let removed = remove_orphaned_tool_messages(&mut messages);
        assert_eq!(removed, 1);
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[2].role, "user");
    }

    #[test]
    fn consecutive_assistant_with_tool_calls_stripped() {
        // When poisoned turn removal leaves an assistant(text) followed by
        // assistant(tool_calls), the second assistant and its tool_results
        // must be removed — normalization would merge them, destroying the
        // structured tool_use blocks and orphaning the results at the API.
        let tool_calls_assistant = r#"{"content":null,"tool_calls":[{"id":"toolu_DEAD","name":"shell","arguments":"{}"}]}"#;
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", "do something"),
            msg("assistant", "Here are the results."),
            msg("assistant", tool_calls_assistant),
            msg("tool", r#"{"content":"ok","tool_call_id":"toolu_DEAD"}"#),
            msg("assistant", "The provider returned an empty response."),
        ];
        let removed = remove_orphaned_tool_messages(&mut messages);
        assert_eq!(
            removed, 2,
            "should remove assistant(tool_calls) + tool_result"
        );
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[2].role, "assistant");
        assert_eq!(messages[2].content, "Here are the results.");
        assert_eq!(messages[3].role, "assistant");
        assert_eq!(
            messages[3].content,
            "The provider returned an empty response."
        );
    }

    #[test]
    fn tool_without_parseable_id_kept_if_assistant_has_tool_calls() {
        // Conservative: if we can't parse the tool_call_id, keep the
        // message as long as the preceding assistant has tool_calls.
        let assistant_content = r#"{"content":"running","tool_calls":[{"id":"toolu_x","name":"shell","arguments":"{}"}]}"#;
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", "go"),
            msg("assistant", assistant_content),
            msg("tool", "plain text result without json"),
            msg("assistant", "done"),
        ];
        let removed = remove_orphaned_tool_messages(&mut messages);
        assert_eq!(removed, 0);
        assert_eq!(messages.len(), 5);
    }

    #[test]
    fn phase2_budget_respects_protected_tool_messages() {
        // Phase 2 should not drop tool messages that fall within the
        // keep_recent protection window, even when the assistant that
        // starts the group is outside the window.
        let tool_content = r#"{"tool_call_id":"toolu_recent","content":"result"}"#;
        let assistant_tool = r#"{"content":"calling","tool_calls":[{"id":"toolu_recent","name":"shell","arguments":"{}"}]}"#;
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", "old question"),
            msg(
                "assistant",
                "old answer with lots of padding text to inflate token count significantly beyond budget",
            ),
            msg("user", "another old question"),
            msg("assistant", assistant_tool),  // outside keep_recent
            msg("tool", tool_content),         // inside keep_recent (3rd from end)
            msg("user", "recent question"),    // inside keep_recent (2nd from end)
            msg("assistant", "recent answer"), // inside keep_recent (1st from end)
        ];
        // Budget tight enough that Phase 2 fires, keep_recent=3 protects last 3
        let config = HistoryPrunerConfig {
            enabled: true,
            max_tokens: 50,
            keep_recent: 3,
            collapse_tool_results: true,
        };
        prune_history(&mut messages, &config);
        // The protected tool message must survive
        assert!(
            messages.iter().any(|m| m.content.contains("toolu_recent")),
            "Protected tool message was dropped by Phase 2 budget enforcement"
        );
    }

    /// Regression test for issue #5743: MiniMax rejects orphaned tool-role
    /// messages whose assistant (with `tool_calls`) was trimmed by the
    /// channel orchestrator's proactive history trimming.
    #[test]
    fn orphan_tool_from_trimmed_channel_history() {
        // Simulates the scenario: channel history was trimmed and the
        // assistant message containing tool_calls was dropped, leaving
        // orphaned tool results with MiniMax-style IDs.
        let tool_result =
            r#"{"content":"search results","tool_call_id":"chatcmpl-tool-92a12a15c14f3b36"}"#;
        let mut messages = vec![
            msg("system", "You are a helpful assistant"),
            msg("tool", tool_result),
            msg("assistant", "Here are the search results"),
            msg("user", "Thanks, now summarize them"),
        ];
        let removed = remove_orphaned_tool_messages(&mut messages);
        assert_eq!(removed, 1, "orphaned tool message should be removed");
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[2].role, "user");
    }

    // ------------------------------------------------------------------
    // strip_orphaned_tool_calls_from_assistants tests
    // ------------------------------------------------------------------

    #[test]
    fn strip_orphan_tool_calls_drops_tool_calls_when_no_result_follows() {
        // Bug 3 canonical case: loop hit max_tool_iterations after the
        // assistant emitted a tool_use but before any tool_result landed.
        // The Bedrock converter would then receive an orphaned tool_use
        // and AWS returns: "Expected toolResult blocks at messages.N.content".
        let tool_calls_assistant =
            r#"{"content":"looking it up","tool_calls":[{"id":"toolu_ORPHAN","name":"search","arguments":"{}"}]}"#;
        let mut messages = vec![
            msg("user", "search for X"),
            msg("assistant", tool_calls_assistant),
        ];
        let stripped = strip_orphaned_tool_calls_from_assistants(&mut messages);
        assert_eq!(stripped, 1);
        let parsed: serde_json::Value = serde_json::from_str(&messages[1].content).unwrap();
        assert!(
            parsed.get("tool_calls").is_none(),
            "tool_calls must be gone; got: {}",
            parsed
        );
        assert_eq!(parsed.get("content").and_then(|v| v.as_str()), Some("looking it up"));
    }

    #[test]
    fn strip_orphan_tool_calls_retains_paired_calls() {
        let tool_calls_assistant = r#"{"content":null,"tool_calls":[{"id":"toolu_OK","name":"search","arguments":"{}"}]}"#;
        let tool_result = r#"{"content":"result","tool_call_id":"toolu_OK"}"#;
        let mut messages = vec![
            msg("user", "q"),
            msg("assistant", tool_calls_assistant),
            msg("tool", tool_result),
        ];
        let stripped = strip_orphaned_tool_calls_from_assistants(&mut messages);
        assert_eq!(stripped, 0, "paired tool_call must not be stripped");
        assert!(messages[1].content.contains("toolu_OK"));
    }

    #[test]
    fn strip_orphan_tool_calls_partial_keeps_paired_drops_orphans() {
        // One paired, one orphaned — the paired entry must survive and the
        // orphan must go.
        let tool_calls_assistant = r#"{"content":null,"tool_calls":[{"id":"toolu_OK","name":"a","arguments":"{}"},{"id":"toolu_ORPHAN","name":"b","arguments":"{}"}]}"#;
        let tool_result = r#"{"content":"result","tool_call_id":"toolu_OK"}"#;
        let mut messages = vec![
            msg("user", "q"),
            msg("assistant", tool_calls_assistant),
            msg("tool", tool_result),
        ];
        let stripped = strip_orphaned_tool_calls_from_assistants(&mut messages);
        assert_eq!(stripped, 1);
        let parsed: serde_json::Value = serde_json::from_str(&messages[1].content).unwrap();
        let calls = parsed.get("tool_calls").and_then(|v| v.as_array()).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].get("id").and_then(|v| v.as_str()), Some("toolu_OK"));
        assert!(!messages[1].content.contains("toolu_ORPHAN"));
    }

    #[test]
    fn strip_orphan_tool_calls_no_op_on_plain_assistants() {
        let mut messages = vec![
            msg("user", "hi"),
            msg("assistant", "hello"),
            msg("user", "how are you"),
            msg("assistant", "great"),
        ];
        let stripped = strip_orphaned_tool_calls_from_assistants(&mut messages);
        assert_eq!(stripped, 0);
        assert_eq!(messages.len(), 4);
    }
}
