use crate::providers::traits::ChatMessage;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

fn default_max_tokens() -> usize {
    8192
}

fn default_keep_recent() -> usize {
    4
}

fn default_collapse() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HistoryPrunerConfig {
    /// Enable history pruning. Default: false.
    #[serde(default)]
    pub enabled: bool,
    /// Maximum estimated tokens for message history. Default: 8192.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: usize,
    /// Keep the N most recent messages untouched. Default: 4.
    #[serde(default = "default_keep_recent")]
    pub keep_recent: usize,
    /// Collapse old tool call/result pairs into short summaries. Default: true.
    #[serde(default = "default_collapse")]
    pub collapse_tool_results: bool,
}

impl Default for HistoryPrunerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_tokens: 8192,
            keep_recent: 4,
            collapse_tool_results: true,
        }
    }
}

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
    messages.iter().map(|m| m.content.len() / 4).sum()
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
            msg("assistant", "tool call"),
            msg("tool", "tool result"),
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
}
