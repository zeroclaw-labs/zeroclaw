//! Channel send tool — lets the agent deliver messages to configured channels.
//!
//! Wraps `Channel::send()` so the daemon/CLI agent loop can push messages
//! into Telegram, Slack, Discord, etc. The agent receives configured channel
//! names and target IDs from system prompt injection.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::channel::{Channel, SendMessage};
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;

/// Shared channel map handle — Arc<RwLock<HashMap<String, Arc<dyn Channel>>>>
pub type ChannelMapHandle =
    Arc<parking_lot::RwLock<std::collections::HashMap<String, Arc<dyn Channel>>>>;

/// Tool that sends a message through a configured channel.
///
/// Parameters:
/// - `channel`: Channel name as registered (`telegram`, `slack`, `discord`, etc.)
/// - `to`: Recipient/target ID (chat ID, channel name, etc.)
/// - `body`: Message content to send
pub struct ChannelSendTool {
    security: Arc<SecurityPolicy>,
    channel_map: ChannelMapHandle,
}

impl ChannelSendTool {
    pub fn new(security: Arc<SecurityPolicy>, channel_map: ChannelMapHandle) -> Self {
        Self {
            security,
            channel_map,
        }
    }
}

#[async_trait]
impl Tool for ChannelSendTool {
    fn name(&self) -> &str {
        "channel_send"
    }

    fn description(&self) -> &str {
        "Send a message through a configured messaging channel (e.g. telegram, slack, discord). Use when the agent needs to deliver a message to an external channel."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "Channel name (e.g. telegram, slack, discord, mattermost, signal, matrix, irc)"
                },
                "to": {
                    "type": "string",
                    "description": "Recipient ID (chat ID, channel name, etc.)"
                },
                "body": {
                    "type": "string",
                    "description": "Message content to send"
                }
            },
            "required": ["channel", "to", "body"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: rate limit exceeded".into()),
            });
        }

        let channel_name = args
            .get("channel")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing 'channel' parameter"))?
            .to_string();

        let to = args
            .get("to")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing 'to' parameter"))?
            .to_string();

        let body = args
            .get("body")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing 'body' parameter"))?
            .to_string();

        let channel = {
            let channel_map = self.channel_map.read();
            channel_map.get(&channel_name).cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "Channel '{}' not found. Check the configured channel name.",
                    channel_name
                )
            })?
        };

        let message = SendMessage::new(body, &to);

        channel.send(&message).await.map_err(|e| {
            anyhow::anyhow!("Failed to send message through '{}': {}", channel.name(), e)
        })?;

        Ok(ToolResult {
            success: true,
            output: format!(
                "Message sent successfully to channel '{}', recipient '{}'",
                channel.name(),
                to
            ),
            error: None,
        })
    }
}
