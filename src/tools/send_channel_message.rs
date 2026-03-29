//! Direct per-user channel delivery tool.
//!
//! Sends an outbound message to a specified user/channel without requiring a
//! scheduled job workaround. The tool holds a late-binding channel map handle
//! that is populated once channels are initialized, mirroring the pattern used
//! by [`ReactionTool`](super::reaction::ReactionTool) and
//! [`PollTool`](super::poll::PollTool).

use super::traits::{Tool, ToolResult};
use crate::channels::traits::{Channel, SendMessage};
use crate::security::SecurityPolicy;
use crate::security::policy::ToolOperation;
use async_trait::async_trait;
use parking_lot::RwLock;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

/// Shared handle giving tools late-bound access to the live channel map.
pub type ChannelMapHandle = Arc<RwLock<HashMap<String, Arc<dyn Channel>>>>;

/// Agent-callable tool for sending a message directly to a user via any active channel.
pub struct SendChannelMessageTool {
    channels: ChannelMapHandle,
    security: Arc<SecurityPolicy>,
}

impl SendChannelMessageTool {
    /// Create a new tool with an empty channel map.
    /// The channel map is populated later via the returned [`ChannelMapHandle`].
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self {
            channels: Arc::new(RwLock::new(HashMap::new())),
            security,
        }
    }

    /// Create a new tool sharing an existing channel map handle.
    pub fn with_channel_map(security: Arc<SecurityPolicy>, channels: ChannelMapHandle) -> Self {
        Self { channels, security }
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
impl Tool for SendChannelMessageTool {
    fn name(&self) -> &str {
        "send_channel_message"
    }

    fn description(&self) -> &str {
        "Send a message directly to a user or chat via any active channel \
         (e.g. 'telegram', 'slack', 'discord'). Use this for one-off outbound \
         messages without needing a scheduled cron job."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "Name of the channel to send through (e.g. 'telegram', 'slack', 'discord')"
                },
                "recipient": {
                    "type": "string",
                    "description": "Recipient identifier — user ID, chat ID, or channel ID depending on the platform"
                },
                "message": {
                    "type": "string",
                    "description": "The message content to send"
                }
            },
            "required": ["channel", "recipient", "message"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Security gate
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "send_channel_message")
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

        let recipient = args
            .get("recipient")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'recipient' parameter"))?;

        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'message' parameter"))?;

        if recipient.trim().is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Recipient must not be empty.".into()),
            });
        }

        if message.trim().is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Message content must not be empty.".into()),
            });
        }

        // Read-lock the channel map to find the target channel.
        // Block-scoped to drop the parking_lot guard before any `.await`.
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

        let send_msg = SendMessage::new(message, recipient);

        match channel.send(&send_msg).await {
            Ok(()) => Ok(ToolResult {
                success: true,
                output: format!("Message sent to '{recipient}' via {channel_name}."),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to send message: {e}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::traits::ChannelMessage;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct MockChannel {
        sent: AtomicBool,
        last_recipient: parking_lot::Mutex<Option<String>>,
        last_content: parking_lot::Mutex<Option<String>>,
        fail_on_send: bool,
    }

    impl MockChannel {
        fn new() -> Self {
            Self {
                sent: AtomicBool::new(false),
                last_recipient: parking_lot::Mutex::new(None),
                last_content: parking_lot::Mutex::new(None),
                fail_on_send: false,
            }
        }

        fn failing() -> Self {
            Self {
                sent: AtomicBool::new(false),
                last_recipient: parking_lot::Mutex::new(None),
                last_content: parking_lot::Mutex::new(None),
                fail_on_send: true,
            }
        }
    }

    #[async_trait]
    impl Channel for MockChannel {
        fn name(&self) -> &str {
            "mock"
        }

        async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
            if self.fail_on_send {
                return Err(anyhow::anyhow!("API error: service unavailable"));
            }
            *self.last_recipient.lock() = Some(message.recipient.clone());
            *self.last_content.lock() = Some(message.content.clone());
            self.sent.store(true, Ordering::SeqCst);
            Ok(())
        }

        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn make_tool_with_channels(channels: Vec<(&str, Arc<dyn Channel>)>) -> SendChannelMessageTool {
        let tool = SendChannelMessageTool::new(Arc::new(SecurityPolicy::default()));
        let map: HashMap<String, Arc<dyn Channel>> = channels
            .into_iter()
            .map(|(name, ch)| (name.to_string(), ch))
            .collect();
        tool.populate(map);
        tool
    }

    #[test]
    fn tool_metadata() {
        let tool = SendChannelMessageTool::new(Arc::new(SecurityPolicy::default()));
        assert_eq!(tool.name(), "send_channel_message");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["channel"].is_object());
        assert!(schema["properties"]["recipient"].is_object());
        assert!(schema["properties"]["message"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "channel"));
        assert!(required.iter().any(|v| v == "recipient"));
        assert!(required.iter().any(|v| v == "message"));
    }

    #[tokio::test]
    async fn send_message_success() {
        let mock = Arc::new(MockChannel::new());
        let mock_ch: Arc<dyn Channel> = Arc::clone(&mock) as Arc<dyn Channel>;
        let tool = make_tool_with_channels(vec![("telegram", mock_ch)]);

        let result = tool
            .execute(json!({
                "channel": "telegram",
                "recipient": "123456",
                "message": "Hello from the agent!"
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("123456"));
        assert!(result.output.contains("telegram"));
        assert!(result.error.is_none());
        assert!(mock.sent.load(Ordering::SeqCst));
        assert_eq!(mock.last_recipient.lock().as_deref(), Some("123456"));
        assert_eq!(
            mock.last_content.lock().as_deref(),
            Some("Hello from the agent!")
        );
    }

    #[tokio::test]
    async fn send_message_channel_error_propagated() {
        let mock: Arc<dyn Channel> = Arc::new(MockChannel::failing());
        let tool = make_tool_with_channels(vec![("slack", mock)]);

        let result = tool
            .execute(json!({
                "channel": "slack",
                "recipient": "U01ABCDEF",
                "message": "Test message"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap()
                .contains("service unavailable")
        );
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
                "recipient": "user1",
                "message": "hello"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        let err = result.error.as_deref().unwrap();
        assert!(err.contains("not found"));
        assert!(err.contains("discord"));
    }

    #[tokio::test]
    async fn empty_channels_returns_not_initialized() {
        let tool = SendChannelMessageTool::new(Arc::new(SecurityPolicy::default()));

        let result = tool
            .execute(json!({
                "channel": "telegram",
                "recipient": "123",
                "message": "hello"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("not initialized"));
    }

    #[tokio::test]
    async fn rejects_empty_message() {
        let tool = make_tool_with_channels(vec![(
            "telegram",
            Arc::new(MockChannel::new()) as Arc<dyn Channel>,
        )]);

        let result = tool
            .execute(json!({
                "channel": "telegram",
                "recipient": "123",
                "message": "   "
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("empty"));
    }

    #[tokio::test]
    async fn rejects_empty_recipient() {
        let tool = make_tool_with_channels(vec![(
            "telegram",
            Arc::new(MockChannel::new()) as Arc<dyn Channel>,
        )]);

        let result = tool
            .execute(json!({
                "channel": "telegram",
                "recipient": "  ",
                "message": "hello"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("empty"));
    }

    #[tokio::test]
    async fn missing_required_params() {
        let tool = make_tool_with_channels(vec![(
            "test",
            Arc::new(MockChannel::new()) as Arc<dyn Channel>,
        )]);

        // Missing channel
        assert!(
            tool.execute(json!({"recipient": "u1", "message": "hi"}))
                .await
                .is_err()
        );

        // Missing recipient
        assert!(
            tool.execute(json!({"channel": "test", "message": "hi"}))
                .await
                .is_err()
        );

        // Missing message
        assert!(
            tool.execute(json!({"channel": "test", "recipient": "u1"}))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn channel_map_handle_allows_late_binding() {
        let tool = SendChannelMessageTool::new(Arc::new(SecurityPolicy::default()));
        let handle = tool.channel_map_handle();

        // Initially empty
        let result = tool
            .execute(json!({
                "channel": "slack",
                "recipient": "U01",
                "message": "hello"
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

        // Now the tool can send
        let result = tool
            .execute(json!({
                "channel": "slack",
                "recipient": "U01",
                "message": "hello"
            }))
            .await
            .unwrap();
        assert!(result.success);
    }

    #[test]
    fn spec_matches_metadata() {
        let tool = SendChannelMessageTool::new(Arc::new(SecurityPolicy::default()));
        let spec = tool.spec();
        assert_eq!(spec.name, "send_channel_message");
        assert_eq!(spec.description, tool.description());
        assert!(spec.parameters["required"].is_array());
    }

    #[test]
    fn with_channel_map_shares_handle() {
        let shared: ChannelMapHandle = Arc::new(RwLock::new(HashMap::new()));
        let tool = SendChannelMessageTool::with_channel_map(
            Arc::new(SecurityPolicy::default()),
            Arc::clone(&shared),
        );
        // Writing to the shared handle should be visible through the tool
        {
            let mut map = shared.write();
            map.insert(
                "test".to_string(),
                Arc::new(MockChannel::new()) as Arc<dyn Channel>,
            );
        }
        let handle = tool.channel_map_handle();
        assert!(handle.read().contains_key("test"));
    }
}
