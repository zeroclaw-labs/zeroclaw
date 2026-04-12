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
pub(crate) fn remove_orphaned_tool_messages(messages: &mut Vec<ChatMessage>) -> usize {
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
                // Count following tool messages — drop as atomic group
                let mut tool_count = 0;
                while i + 1 + tool_count < messages.len()
                    && messages[i + 1 + tool_count].role == "tool"
                {
                    tool_count += 1;
                }
                if tool_count > 0 {
                    for _ in 0..=tool_count {
                        messages.remove(i);
                    }
                    dropped_messages += 1 + tool_count;
                    dropped_any = true;
                    break;
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
}
