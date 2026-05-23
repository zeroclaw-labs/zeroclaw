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
/// - `channel`: Composite channel key (`telegram.default`, `telegram.prod`, etc.) or bare
///   type name (`telegram`, `slack`). Bare names resolve to `<type>.default`.
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
                    "description": "Composite channel key (e.g. telegram.default, telegram.prod, slack.prod) or bare type name (telegram, slack, discord, mattermost, signal, matrix, irc). Bare names resolve to <type>.default."
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
            .ok_or_else(|| anyhow::Error::msg("Missing 'channel' parameter"))?
            .to_string();

        let to = args
            .get("to")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::Error::msg("Missing 'to' parameter"))?
            .to_string();

        let body = args
            .get("body")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::Error::msg("Missing 'body' parameter"))?
            .to_string();

        let channel = {
            let channel_map = self.channel_map.read();

            // 1. exact composite key
            let channel = channel_map.get(&channel_name).cloned();

            // 2. bare type → <type>.default
            let channel = channel.or_else(|| {
                if !channel_name.contains('.') {
                    let default_key = format!("{channel_name}.default");
                    channel_map.get(&default_key).cloned()
                } else {
                    None
                }
            });

            // 3. aliased → fallback to <type>.default
            let channel = channel.or_else(|| {
                if let Some(bare) = channel_name.split('.').next() {
                    if bare != channel_name {
                        let default_key = format!("{bare}.default");
                        channel_map.get(&default_key).cloned()
                    } else {
                        None
                    }
                } else {
                    None
                }
            });

            channel.ok_or_else(|| {
                // Build a helpful list of available channels
                let available: Vec<String> = channel_map.keys().cloned().collect();
                anyhow::Error::msg(format!(
                    "Channel '{}' not found. Available channels: {:?}",
                    channel_name, available
                ))
            })?
        };

        let message = SendMessage::new(body, &to);

        channel.send(&message).await.map_err(|e| {
            anyhow::Error::msg(format!(
                "Failed to send message through '{}': {}",
                channel.name(),
                e
            ))
        })?;

        Ok(ToolResult {
            success: true,
            output: format!(
                "Message sent successfully to channel '{}', recipient '{}'",
                channel_name, to
            ),
            error: None,
        })
    }
}
