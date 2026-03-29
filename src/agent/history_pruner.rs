use crate::memory::media_cache::{self, MediaCache};
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
    pub cached_images: usize,
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
    prune_history_with_cache(messages, config, None)
}

/// Prune history with optional media cache support.
///
/// When a `media_cache` is provided, images in dropped/collapsed messages are
/// saved to disk and their markers replaced with `[CACHED_IMAGE:{hash}]`.
pub fn prune_history_with_cache(
    messages: &mut Vec<ChatMessage>,
    config: &HistoryPrunerConfig,
    media_cache: Option<&MediaCache>,
) -> PruneStats {
    let messages_before = messages.len();
    if !config.enabled || messages.is_empty() {
        return PruneStats {
            messages_before,
            messages_after: messages_before,
            collapsed_pairs: 0,
            dropped_messages: 0,
            cached_images: 0,
        };
    }

    let mut collapsed_pairs: usize = 0;
    let mut cached_images: usize = 0;

    // Phase 1 – collapse assistant+tool pairs
    if config.collapse_tool_results {
        let mut i = 0;
        while i + 1 < messages.len() {
            let protected = protected_indices(messages, config.keep_recent);
            if messages[i].role == "assistant"
                && messages[i + 1].role == "tool"
                && !protected[i]
                && !protected[i + 1]
            {
                // Cache images from the tool result before collapsing.
                if let Some(cache) = media_cache {
                    cached_images += cache_images_from_content(&messages[i + 1].content, cache);
                }

                let tool_content = &messages[i + 1].content;
                let truncated: String = tool_content.chars().take(100).collect();
                let summary = format!("[Tool result: {truncated}...]");
                messages[i] = ChatMessage {
                    role: "assistant".to_string(),
                    content: summary,
                };
                messages.remove(i + 1);
                collapsed_pairs += 1;
            } else {
                i += 1;
            }
        }
    }

    // Phase 2 – budget enforcement
    let mut dropped_messages: usize = 0;
    while estimate_tokens(messages) > config.max_tokens {
        let protected = protected_indices(messages, config.keep_recent);
        if let Some(idx) = protected
            .iter()
            .enumerate()
            .find(|&(_, &p)| !p)
            .map(|(i, _)| i)
        {
            // Cache images from the message before dropping it.
            if let Some(cache) = media_cache {
                let count = cache_images_from_content(&messages[idx].content, cache);
                if count > 0 {
                    // Replace the message content with cached markers before drop,
                    // in case the caller inspects dropped messages.
                    messages[idx].content =
                        media_cache::cache_images_in_message(&messages[idx].content, cache);
                    cached_images += count;
                }
            }

            messages.remove(idx);
            dropped_messages += 1;
        } else {
            break;
        }
    }

    PruneStats {
        messages_before,
        messages_after: messages.len(),
        collapsed_pairs,
        dropped_messages,
        cached_images,
    }
}

/// Count (and cache) data-URI images from a message's content.
/// Returns the number of images successfully cached.
fn cache_images_from_content(content: &str, cache: &MediaCache) -> usize {
    let prefix = "[IMAGE:";
    let mut count = 0usize;
    let mut cursor = 0usize;

    while let Some(rel_start) = content[cursor..].find(prefix) {
        let start = cursor + rel_start;
        let marker_start = start + prefix.len();
        let Some(rel_end) = content[marker_start..].find(']') else {
            break;
        };
        let end = marker_start + rel_end;
        let image_ref = content[marker_start..end].trim();

        if image_ref.starts_with("data:") {
            if let Some((mime, bytes)) = media_cache::decode_data_uri(image_ref) {
                if cache.put(&bytes, &mime, None).is_ok() {
                    count += 1;
                }
            }
        }

        cursor = end + 1;
    }

    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};

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
        assert!(messages[1].content.starts_with("[Tool result: "));
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
    fn prune_caches_images_from_dropped_messages() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cache = MediaCache::new(tmp.path(), 100).unwrap();
        let png_bytes = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        let b64 = STANDARD.encode(&png_bytes);
        let big_image_msg = format!("{} [IMAGE:data:image/png;base64,{b64}]", "x".repeat(40_000));

        let mut messages = vec![
            msg("system", "sys"),
            msg("user", &big_image_msg),
            msg("assistant", "old"),
            msg("user", "recent1"),
            msg("assistant", "recent2"),
        ];

        let config = HistoryPrunerConfig {
            enabled: true,
            max_tokens: 100,
            keep_recent: 2,
            collapse_tool_results: false,
        };

        let stats = prune_history_with_cache(&mut messages, &config, Some(&cache));
        assert!(stats.dropped_messages > 0);
        assert_eq!(stats.cached_images, 1);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn prune_without_cache_reports_zero_cached() {
        let mut messages = vec![
            msg("system", "sys"),
            msg("user", &"x".repeat(40_000)),
            msg("user", "recent"),
            msg("assistant", "recent"),
        ];
        let config = HistoryPrunerConfig {
            enabled: true,
            max_tokens: 100,
            keep_recent: 2,
            collapse_tool_results: false,
        };
        let stats = prune_history(&mut messages, &config);
        assert_eq!(stats.cached_images, 0);
    }
}
