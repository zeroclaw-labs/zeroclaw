//! Emoji reaction tool for cross-channel message reactions.
//!
//! Exposes `add_reaction` and `remove_reaction` from the [`Channel`] trait as an
//! agent-callable tool. The tool holds a late-binding channel map handle that is
//! populated once channels are initialized (after tool construction). This mirrors
//! the pattern used by [`DelegateTool`] for its parent-tools handle.

use super::traits::{Tool, ToolResult};
use crate::channels::traits::Channel;
use crate::security::policy::ToolOperation;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use parking_lot::RwLock;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

/// Shared handle to the channel map. Starts empty; populated once channels boot.
pub type ChannelMapHandle = Arc<RwLock<HashMap<String, Arc<dyn Channel>>>>;

/// Agent-callable tool for adding or removing emoji reactions on messages.
pub struct ReactionTool {
    channels: ChannelMapHandle,
    security: Arc<SecurityPolicy>,
}

impl ReactionTool {
    /// Create a new reaction tool with an empty channel map.
    /// Call [`populate`] or write to the returned [`ChannelMapHandle`] once channels
    /// are available.
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self {
            channels: Arc::new(RwLock::new(HashMap::new())),
            security,
        }
    }

    /// Return the shared handle so callers can populate it after channel init.
    pub fn channel_map_handle(&self) -> ChannelMapHandle {
        Arc::clone(&self.channels)
    }

    /// Convenience: populate the channel map from a pre-built map.
    pub fn populate(&self, map: HashMap<String, Arc<dyn Channel>>) {
        *self.channels.write() = map;
    }
}

#[async_trait]
impl Tool for ReactionTool {
    fn name(&self) -> &str {
        "reaction"
    }

    fn description(&self) -> &str {
        "Add or remove an emoji reaction on a message in any active channel. \
         Provide the channel name (e.g. 'discord', 'slack'), the platform message ID, \
         and the emoji (Unicode character or platform shortcode)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "Name of the channel to react in (e.g. 'discord', 'slack', 'telegram')"
                },
                "message_id": {
                    "type": "string",
                    "description": "Platform-scoped message identifier to react to"
                },
                "emoji": {
                    "type": "string",
                    "description": "Emoji to react with (Unicode character or platform shortcode)"
                },
                "action": {
                    "type": "string",
                    "enum": ["add", "remove"],
                    "description": "Whether to add or remove the reaction (default: 'add')"
                }
            },
            "required": ["channel", "message_id", "emoji"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Security gate
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "reaction")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        let channel_name = args
            .get("channel")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'channel' parameter"))?;

        let message_id = args
            .get("message_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'message_id' parameter"))?;

        let emoji = args
            .get("emoji")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'emoji' parameter"))?;

        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("add");

        if action != "add" && action != "remove" {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Invalid action '{action}': must be 'add' or 'remove'"
                )),
            });
        }

        // Read-lock the channel map to find the target channel.
        let channel = {
            let map = self.channels.read();
            if map.is_empty() {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("No channels available yet (channels not initialized)".to_string()),
                });
            }
            match map.get(channel_name) {
                Some(ch) => Arc::clone(ch),
                None => {
                    let available: Vec<String> = map.keys().cloned().collect();
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "Channel '{channel_name}' not found. Available channels: {}",
                            available.join(", ")
                        )),
                    });
                }
            }
        };

        let result = if action == "add" {
            channel.add_reaction(channel_name, message_id, emoji).await
        } else {
            channel
                .remove_reaction(channel_name, message_id, emoji)
                .await
        };

        match result {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!(
                    "Reaction {action}ed: {emoji} on message {message_id} in {channel_name}"
                ),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to {action} reaction: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::traits::{ChannelMessage, SendMessage};
    use std::sync::atomic::{AtomicBool, Ordering};

    struct MockChannel {
        reaction_added: AtomicBool,
        reaction_removed: AtomicBool,
        fail_on_add: bool,
    }

    impl MockChannel {
        fn new() -> Self {
            Self {
                reaction_added: AtomicBool::new(false),
                reaction_removed: AtomicBool::new(false),
                fail_on_add: false,
            }
        }

        fn failing() -> Self {
            Self {
                reaction_added: AtomicBool::new(false),
                reaction_removed: AtomicBool::new(false),
                fail_on_add: true,
            }
        }
    }

    #[async_trait]
    impl Channel for MockChannel {
        fn name(&self) -> &str {
            "mock"
        }

        async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
            Ok(())
        }

        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn add_reaction(
            &self,
            _channel_id: &str,
            _message_id: &str,
            _emoji: &str,
        ) -> anyhow::Result<()> {
            if self.fail_on_add {
                return Err(anyhow::anyhow!("API error: rate limited"));
            }
            self.reaction_added.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn remove_reaction(
            &self,
            _channel_id: &str,
            _message_id: &str,
            _emoji: &str,
        ) -> anyhow::Result<()> {
            self.reaction_removed.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    fn make_tool_with_channels(channels: Vec<(&str, Arc<dyn Channel>)>) -> ReactionTool {
        let tool = ReactionTool::new(Arc::new(SecurityPolicy::default()));
        let map: HashMap<String, Arc<dyn Channel>> = channels
            .into_iter()
            .map(|(name, ch)| (name.to_string(), ch))
            .collect();
        tool.populate(map);
        tool
    }

    #[test]
    fn tool_metadata() {
        let tool = ReactionTool::new(Arc::new(SecurityPolicy::default()));
        assert_eq!(tool.name(), "reaction");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["channel"].is_object());
        assert!(schema["properties"]["message_id"].is_object());
        assert!(schema["properties"]["emoji"].is_object());
        assert!(schema["properties"]["action"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "channel"));
        assert!(required.iter().any(|v| v == "message_id"));
        assert!(required.iter().any(|v| v == "emoji"));
        // action is optional (defaults to "add")
        assert!(!required.iter().any(|v| v == "action"));
    }

    #[tokio::test]
    async fn add_reaction_success() {
        let mock: Arc<dyn Channel> = Arc::new(MockChannel::new());
        let tool = make_tool_with_channels(vec![("discord", Arc::clone(&mock))]);

        let result = tool
            .execute(json!({
                "channel": "discord",
                "message_id": "msg_123",
                "emoji": "\u{2705}"
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("added"));
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn remove_reaction_success() {
        let mock: Arc<dyn Channel> = Arc::new(MockChannel::new());
        let tool = make_tool_with_channels(vec![("slack", Arc::clone(&mock))]);

        let result = tool
            .execute(json!({
                "channel": "slack",
                "message_id": "msg_456",
                "emoji": "\u{1F440}",
                "action": "remove"
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("removed"));
    }

    #[tokio::test]
    async fn unknown_channel_returns_error() {
        let tool = make_tool_with_channels(vec![(
            "discord",
            Arc::new(MockChannel::new()) as Arc<dyn Channel>,
        )]);

        let result = tool
            .execute(json!({
                "channel": "nonexistent",
                "message_id": "msg_1",
                "emoji": "\u{2705}"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        let err = result.error.as_deref().unwrap();
        assert!(err.contains("not found"));
        assert!(err.contains("discord"));
    }

    #[tokio::test]
    async fn invalid_action_returns_error() {
        let tool = make_tool_with_channels(vec![(
            "discord",
            Arc::new(MockChannel::new()) as Arc<dyn Channel>,
        )]);

        let result = tool
            .execute(json!({
                "channel": "discord",
                "message_id": "msg_1",
                "emoji": "\u{2705}",
                "action": "toggle"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("toggle"));
    }

    #[tokio::test]
    async fn channel_error_propagated() {
        let mock: Arc<dyn Channel> = Arc::new(MockChannel::failing());
        let tool = make_tool_with_channels(vec![("discord", mock)]);

        let result = tool
            .execute(json!({
                "channel": "discord",
                "message_id": "msg_1",
                "emoji": "\u{2705}"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("rate limited"));
    }

    #[tokio::test]
    async fn missing_required_params() {
        let tool = make_tool_with_channels(vec![(
            "test",
            Arc::new(MockChannel::new()) as Arc<dyn Channel>,
        )]);

        // Missing channel
        let result = tool.execute(json!({"message_id": "1", "emoji": "x"})).await;
        assert!(result.is_err());

        // Missing message_id
        let result = tool.execute(json!({"channel": "a", "emoji": "x"})).await;
        assert!(result.is_err());

        // Missing emoji
        let result = tool
            .execute(json!({"channel": "a", "message_id": "1"}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn empty_channels_returns_not_initialized() {
        let tool = ReactionTool::new(Arc::new(SecurityPolicy::default()));
        // No channels populated

        let result = tool
            .execute(json!({
                "channel": "discord",
                "message_id": "msg_1",
                "emoji": "\u{2705}"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("not initialized"));
    }

    #[tokio::test]
    async fn default_action_is_add() {
        let mock = Arc::new(MockChannel::new());
        let mock_ch: Arc<dyn Channel> = Arc::clone(&mock) as Arc<dyn Channel>;
        let tool = make_tool_with_channels(vec![("test", mock_ch)]);

        let result = tool
            .execute(json!({
                "channel": "test",
                "message_id": "msg_1",
                "emoji": "\u{2705}"
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(mock.reaction_added.load(Ordering::SeqCst));
        assert!(!mock.reaction_removed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn channel_map_handle_allows_late_binding() {
        let tool = ReactionTool::new(Arc::new(SecurityPolicy::default()));
        let handle = tool.channel_map_handle();

        // Initially empty — tool reports not initialized
        let result = tool
            .execute(json!({
                "channel": "slack",
                "message_id": "msg_1",
                "emoji": "\u{2705}"
            }))
            .await
            .unwrap();
        assert!(!result.success);

        // Populate via the handle
        {
            let mut map = handle.write();
            map.insert(
                "slack".to_string(),
                Arc::new(MockChannel::new()) as Arc<dyn Channel>,
            );
        }

        // Now the tool can route to the channel
        let result = tool
            .execute(json!({
                "channel": "slack",
                "message_id": "msg_1",
                "emoji": "\u{2705}"
            }))
            .await
            .unwrap();
        assert!(result.success);
    }

    #[test]
    fn spec_matches_metadata() {
        let tool = ReactionTool::new(Arc::new(SecurityPolicy::default()));
        let spec = tool.spec();
        assert_eq!(spec.name, "reaction");
        assert_eq!(spec.description, tool.description());
        assert!(spec.parameters["required"].is_array());
    }
}
