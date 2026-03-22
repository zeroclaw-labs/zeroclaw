//! Multi-channel bridge for ZeroClaw.
//!
//! Relays messages between different messaging channels, enabling users
//! on one platform (e.g., KakaoTalk) to interact with conversations on
//! another platform (e.g., Discord, Slack, Telegram).
//!
//! ## Design
//! - Bridge rules define source ↔ target channel mappings
//! - Messages with channel-prefix triggers (e.g., `@discord ...`) are relayed
//! - Bidirectional mode synchronizes conversations in both directions
//! - Per-channel message formatting adapts to target platform limits
//! - Rate limiting prevents relay loops and spam

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use super::traits::{ChannelMessage, SendMessage};

// ── Channel capabilities ─────────────────────────────────────────

/// Capabilities and limits for a messaging channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ChannelCapabilities {
    /// Channel name (e.g., "telegram", "discord", "kakao").
    pub channel: String,
    /// Supports direct messages.
    pub dm: bool,
    /// Supports group conversations.
    pub group: bool,
    /// Supports message threading.
    pub threading: bool,
    /// Supports reactions/emoji responses.
    pub reactions: bool,
    /// Supports polls.
    pub polls: bool,
    /// Supports message editing after send.
    pub edit: bool,
    /// Supports message deletion (unsend).
    pub unsend: bool,
    /// Supports media attachments.
    pub media: bool,
    /// Supports native slash commands.
    pub native_commands: bool,
    /// Supports block/streaming responses.
    pub block_streaming: bool,
    /// Maximum text chunk size (characters).
    pub text_chunk_limit: usize,
}

/// Predefined capabilities for known channels.
pub fn channel_capabilities(channel: &str) -> ChannelCapabilities {
    match channel {
        "telegram" => ChannelCapabilities {
            channel: "telegram".into(),
            dm: true,
            group: true,
            threading: true,
            reactions: true,
            polls: true,
            edit: true,
            unsend: true,
            media: true,
            native_commands: true,
            block_streaming: true,
            text_chunk_limit: 4096,
        },
        "discord" => ChannelCapabilities {
            channel: "discord".into(),
            dm: true,
            group: true,
            threading: true,
            reactions: true,
            polls: true,
            edit: true,
            unsend: true,
            media: true,
            native_commands: true,
            block_streaming: true,
            text_chunk_limit: 2000,
        },
        "slack" => ChannelCapabilities {
            channel: "slack".into(),
            dm: true,
            group: true,
            threading: true,
            reactions: true,
            polls: false,
            edit: true,
            unsend: true,
            media: true,
            native_commands: false,
            block_streaming: true,
            text_chunk_limit: 4000,
        },
        "whatsapp" => ChannelCapabilities {
            channel: "whatsapp".into(),
            dm: true,
            group: true,
            threading: false,
            reactions: true,
            polls: true,
            edit: false,
            unsend: true,
            media: true,
            native_commands: false,
            block_streaming: true,
            text_chunk_limit: 65536,
        },
        "signal" => ChannelCapabilities {
            channel: "signal".into(),
            dm: true,
            group: true,
            threading: false,
            reactions: false,
            polls: false,
            edit: true,
            unsend: true,
            media: true,
            native_commands: false,
            block_streaming: true,
            text_chunk_limit: 65536,
        },
        "line" => ChannelCapabilities {
            channel: "line".into(),
            dm: true,
            group: true,
            threading: false,
            reactions: false,
            polls: false,
            edit: false,
            unsend: false,
            media: true,
            native_commands: false,
            block_streaming: true,
            text_chunk_limit: 5000,
        },
        "kakao" => ChannelCapabilities {
            channel: "kakao".into(),
            dm: true,
            group: false,
            threading: false,
            reactions: false,
            polls: false,
            edit: false,
            unsend: false,
            media: true,
            native_commands: false,
            block_streaming: false,
            text_chunk_limit: 1000,
        },
        "matrix" => ChannelCapabilities {
            channel: "matrix".into(),
            dm: true,
            group: true,
            threading: false,
            reactions: false,
            polls: false,
            edit: false,
            unsend: false,
            media: true,
            native_commands: false,
            block_streaming: true,
            text_chunk_limit: 65536,
        },
        _ => ChannelCapabilities {
            channel: channel.into(),
            dm: true,
            group: false,
            threading: false,
            reactions: false,
            polls: false,
            edit: false,
            unsend: false,
            media: false,
            native_commands: false,
            block_streaming: false,
            text_chunk_limit: 4096,
        },
    }
}

// ── Bridge rule ──────────────────────────────────────────────────

/// A bridge rule defining source → target channel relay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeRule {
    /// Rule identifier.
    pub id: String,
    /// Source channel name.
    pub source_channel: String,
    /// Target channel name.
    pub target_channel: String,
    /// Enable bidirectional relay (target → source too).
    pub bidirectional: bool,
    /// Trigger prefix (e.g., "@discord", "@slack"). None = relay all messages.
    pub trigger_prefix: Option<String>,
    /// Target recipient/room on the target channel.
    pub target_recipient: String,
    /// Whether this rule is active.
    pub enabled: bool,
}

impl BridgeRule {
    /// Check if a message should be relayed by this rule.
    pub fn matches(&self, message: &ChannelMessage) -> bool {
        if !self.enabled {
            return false;
        }

        if message.channel != self.source_channel {
            return false;
        }

        if let Some(ref prefix) = self.trigger_prefix {
            message.content.starts_with(prefix)
        } else {
            true
        }
    }

    /// Extract the relay content from a message, stripping the trigger prefix.
    pub fn extract_content(&self, message: &ChannelMessage) -> String {
        if let Some(ref prefix) = self.trigger_prefix {
            message
                .content
                .strip_prefix(prefix)
                .unwrap_or(&message.content)
                .trim()
                .to_string()
        } else {
            message.content.clone()
        }
    }
}

// ── Relay record ─────────────────────────────────────────────────

/// Record of a relayed message for deduplication.
#[derive(Debug, Clone)]
struct RelayRecord {
    /// Original message ID.
    source_message_id: String,
    /// Timestamp when relayed (epoch seconds).
    relayed_at: i64,
}

// ── Channel bridge manager ───────────────────────────────────────

/// Manages cross-channel message bridging.
pub struct ChannelBridge {
    /// Active bridge rules.
    rules: Vec<BridgeRule>,
    /// Recent relay records for deduplication (keyed by source_msg_id).
    recent_relays: Arc<Mutex<HashMap<String, RelayRecord>>>,
    /// Maximum deduplication window in seconds.
    dedup_window_secs: i64,
    /// Whether bridging is enabled.
    enabled: bool,
}

impl ChannelBridge {
    /// Create a new channel bridge.
    pub fn new(enabled: bool) -> Self {
        Self {
            rules: Vec::new(),
            recent_relays: Arc::new(Mutex::new(HashMap::new())),
            dedup_window_secs: 60,
            enabled,
        }
    }

    /// Add a bridge rule.
    pub fn add_rule(&mut self, rule: BridgeRule) {
        self.rules.push(rule);
    }

    /// Remove a bridge rule by ID.
    pub fn remove_rule(&mut self, rule_id: &str) -> bool {
        let before = self.rules.len();
        self.rules.retain(|r| r.id != rule_id);
        self.rules.len() < before
    }

    /// List all bridge rules.
    pub fn list_rules(&self) -> &[BridgeRule] {
        &self.rules
    }

    /// Find matching rules for an incoming message.
    pub fn find_matching_rules(&self, message: &ChannelMessage) -> Vec<&BridgeRule> {
        if !self.enabled {
            return Vec::new();
        }

        self.rules.iter().filter(|r| r.matches(message)).collect()
    }

    /// Prepare relay messages for an incoming message.
    ///
    /// Returns a list of (target_channel_name, SendMessage) pairs
    /// that should be dispatched to the corresponding channels.
    pub async fn prepare_relays(&self, message: &ChannelMessage) -> Vec<(String, SendMessage)> {
        if !self.enabled {
            return Vec::new();
        }

        // Deduplication: skip if this message was recently relayed
        {
            let relays = self.recent_relays.lock().await;
            if relays.contains_key(&message.id) {
                return Vec::new();
            }
        }

        let matching_rules = self.find_matching_rules(message);

        let mut result = Vec::new();
        for rule in matching_rules {
            let content = rule.extract_content(message);
            if content.is_empty() {
                continue;
            }

            // Format for target channel
            let target_caps = channel_capabilities(&rule.target_channel);
            let formatted =
                format_for_channel(&content, &message.sender, &message.channel, &target_caps);

            result.push((
                rule.target_channel.clone(),
                SendMessage::new(formatted, &rule.target_recipient),
            ));
        }

        // Record relay for deduplication
        if !result.is_empty() {
            let now = chrono::Utc::now().timestamp();
            let mut relays = self.recent_relays.lock().await;
            relays.insert(
                message.id.clone(),
                RelayRecord {
                    source_message_id: message.id.clone(),
                    relayed_at: now,
                },
            );
        }

        result
    }

    /// Clean up expired deduplication records.
    pub async fn cleanup_dedup_records(&self) {
        let now = chrono::Utc::now().timestamp();
        let mut relays = self.recent_relays.lock().await;
        relays.retain(|_, record| now - record.relayed_at < self.dedup_window_secs);
    }

    /// Get the number of active rules.
    pub fn rule_count(&self) -> usize {
        self.rules.iter().filter(|r| r.enabled).count()
    }

    /// Check if bridging is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

// ── Message formatting ───────────────────────────────────────────

/// Format a relay message for the target channel, respecting its limits.
fn format_for_channel(
    content: &str,
    sender: &str,
    source_channel: &str,
    target_caps: &ChannelCapabilities,
) -> String {
    let header = format!("[{source_channel}:{sender}] ");
    let max_content = target_caps.text_chunk_limit.saturating_sub(header.len());

    if content.chars().count() <= max_content {
        format!("{header}{content}")
    } else {
        let truncated: String = content
            .chars()
            .take(max_content.saturating_sub(3))
            .collect();
        format!("{header}{truncated}...")
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_message(channel: &str, content: &str) -> ChannelMessage {
        ChannelMessage {
            id: "msg-001".into(),
            sender: "zeroclaw_user".into(),
            reply_target: "zeroclaw_user".into(),
            content: content.into(),
            channel: channel.into(),
            timestamp: 1000,
            thread_ts: None,
            silent: false,
        }
    }

    fn sample_rule() -> BridgeRule {
        BridgeRule {
            id: "rule-1".into(),
            source_channel: "kakao".into(),
            target_channel: "discord".into(),
            bidirectional: false,
            trigger_prefix: Some("@discord ".into()),
            target_recipient: "channel-123".into(),
            enabled: true,
        }
    }

    #[test]
    fn capabilities_telegram() {
        let caps = channel_capabilities("telegram");
        assert!(caps.dm);
        assert!(caps.group);
        assert!(caps.threading);
        assert_eq!(caps.text_chunk_limit, 4096);
    }

    #[test]
    fn capabilities_kakao() {
        let caps = channel_capabilities("kakao");
        assert!(caps.dm);
        assert!(!caps.group);
        assert!(!caps.block_streaming);
        assert_eq!(caps.text_chunk_limit, 1000);
    }

    #[test]
    fn capabilities_discord() {
        let caps = channel_capabilities("discord");
        assert_eq!(caps.text_chunk_limit, 2000);
        assert!(caps.reactions);
        assert!(caps.native_commands);
    }

    #[test]
    fn capabilities_unknown_channel() {
        let caps = channel_capabilities("myplatform");
        assert_eq!(caps.channel, "myplatform");
        assert_eq!(caps.text_chunk_limit, 4096);
    }

    #[test]
    fn bridge_rule_matches_with_prefix() {
        let rule = sample_rule();
        let msg = sample_message("kakao", "@discord hello world");
        assert!(rule.matches(&msg));

        let msg_no_prefix = sample_message("kakao", "just a normal message");
        assert!(!rule.matches(&msg_no_prefix));
    }

    #[test]
    fn bridge_rule_matches_wrong_channel() {
        let rule = sample_rule();
        let msg = sample_message("telegram", "@discord hello");
        assert!(!rule.matches(&msg));
    }

    #[test]
    fn bridge_rule_disabled_never_matches() {
        let mut rule = sample_rule();
        rule.enabled = false;
        let msg = sample_message("kakao", "@discord hello");
        assert!(!rule.matches(&msg));
    }

    #[test]
    fn bridge_rule_no_prefix_matches_all() {
        let rule = BridgeRule {
            id: "rule-all".into(),
            source_channel: "kakao".into(),
            target_channel: "slack".into(),
            bidirectional: true,
            trigger_prefix: None,
            target_recipient: "general".into(),
            enabled: true,
        };

        let msg = sample_message("kakao", "any message at all");
        assert!(rule.matches(&msg));
    }

    #[test]
    fn extract_content_strips_prefix() {
        let rule = sample_rule();
        let msg = sample_message("kakao", "@discord hello world");
        assert_eq!(rule.extract_content(&msg), "hello world");
    }

    #[test]
    fn extract_content_no_prefix_returns_full() {
        let rule = BridgeRule {
            trigger_prefix: None,
            ..sample_rule()
        };
        let msg = sample_message("kakao", "full message");
        assert_eq!(rule.extract_content(&msg), "full message");
    }

    #[test]
    fn format_for_channel_within_limit() {
        let caps = channel_capabilities("discord");
        let result = format_for_channel("hello", "zeroclaw_user", "kakao", &caps);
        assert!(result.starts_with("[kakao:zeroclaw_user] "));
        assert!(result.contains("hello"));
        assert!(result.len() <= 2000);
    }

    #[test]
    fn format_for_channel_truncates_long_message() {
        let caps = ChannelCapabilities {
            channel: "test".into(),
            dm: true,
            group: false,
            threading: false,
            reactions: false,
            polls: false,
            edit: false,
            unsend: false,
            media: false,
            native_commands: false,
            block_streaming: false,
            text_chunk_limit: 50,
        };

        let long_msg = "a".repeat(200);
        let result = format_for_channel(&long_msg, "user", "src", &caps);
        assert!(result.chars().count() <= 50);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn bridge_add_and_remove_rules() {
        let mut bridge = ChannelBridge::new(true);
        assert_eq!(bridge.rule_count(), 0);

        bridge.add_rule(sample_rule());
        assert_eq!(bridge.rule_count(), 1);

        let removed = bridge.remove_rule("rule-1");
        assert!(removed);
        assert_eq!(bridge.rule_count(), 0);

        let not_found = bridge.remove_rule("nonexistent");
        assert!(!not_found);
    }

    #[test]
    fn bridge_find_matching_rules() {
        let mut bridge = ChannelBridge::new(true);
        bridge.add_rule(sample_rule());
        bridge.add_rule(BridgeRule {
            id: "rule-2".into(),
            source_channel: "kakao".into(),
            target_channel: "slack".into(),
            bidirectional: false,
            trigger_prefix: Some("@slack ".into()),
            target_recipient: "general".into(),
            enabled: true,
        });

        let msg = sample_message("kakao", "@discord hello");
        let matches = bridge.find_matching_rules(&msg);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].target_channel, "discord");
    }

    #[test]
    fn bridge_disabled_returns_no_matches() {
        let mut bridge = ChannelBridge::new(false);
        bridge.add_rule(sample_rule());

        let msg = sample_message("kakao", "@discord hello");
        let matches = bridge.find_matching_rules(&msg);
        assert!(matches.is_empty());
    }

    #[tokio::test]
    async fn bridge_prepare_relays() {
        let mut bridge = ChannelBridge::new(true);
        bridge.add_rule(sample_rule());

        let msg = sample_message("kakao", "@discord hello world");
        let relays = bridge.prepare_relays(&msg).await;

        assert_eq!(relays.len(), 1);
        assert_eq!(relays[0].0, "discord");
        assert!(relays[0].1.content.contains("hello world"));
        assert_eq!(relays[0].1.recipient, "channel-123");
    }

    #[tokio::test]
    async fn bridge_deduplication() {
        let mut bridge = ChannelBridge::new(true);
        bridge.add_rule(sample_rule());

        let msg = sample_message("kakao", "@discord hello");

        // First relay works
        let relays1 = bridge.prepare_relays(&msg).await;
        assert_eq!(relays1.len(), 1);

        // Duplicate is suppressed
        let relays2 = bridge.prepare_relays(&msg).await;
        assert!(relays2.is_empty());
    }

    #[tokio::test]
    async fn bridge_empty_content_skipped() {
        let mut bridge = ChannelBridge::new(true);
        bridge.add_rule(sample_rule());

        // Message is just the prefix with no content
        let msg = sample_message("kakao", "@discord ");
        let relays = bridge.prepare_relays(&msg).await;
        assert!(relays.is_empty());
    }

    #[tokio::test]
    async fn bridge_no_matching_rules() {
        let mut bridge = ChannelBridge::new(true);
        bridge.add_rule(sample_rule());

        let msg = sample_message("kakao", "just chatting");
        let relays = bridge.prepare_relays(&msg).await;
        assert!(relays.is_empty());
    }

    #[test]
    fn bridge_list_rules() {
        let mut bridge = ChannelBridge::new(true);
        bridge.add_rule(sample_rule());
        bridge.add_rule(BridgeRule {
            id: "rule-2".into(),
            source_channel: "telegram".into(),
            target_channel: "slack".into(),
            bidirectional: true,
            trigger_prefix: None,
            target_recipient: "general".into(),
            enabled: true,
        });

        assert_eq!(bridge.list_rules().len(), 2);
    }

    #[tokio::test]
    async fn bridge_cleanup_dedup() {
        let mut bridge = ChannelBridge::new(true);
        bridge.dedup_window_secs = 0; // Expire immediately
        bridge.add_rule(sample_rule());

        let msg = sample_message("kakao", "@discord test");
        let _ = bridge.prepare_relays(&msg).await;

        // Cleanup expired records
        bridge.cleanup_dedup_records().await;

        // Now the same message can be relayed again
        let relays = bridge.prepare_relays(&msg).await;
        assert_eq!(relays.len(), 1);
    }

    #[test]
    fn bridge_is_enabled() {
        let bridge = ChannelBridge::new(true);
        assert!(bridge.is_enabled());

        let bridge = ChannelBridge::new(false);
        assert!(!bridge.is_enabled());
    }
}
