//! Channel send tool — lets the agent deliver messages to configured channels.
//!
//! Wraps `Channel::send()` so the daemon/CLI agent loop can push messages
//! into Telegram, Slack, Discord, etc. The agent receives configured channel
//! names and target IDs from system prompt injection.
//!
//! **Security:** The `to` parameter is optional. When omitted, the configured
//! `default_target` for the resolved channel key is used. When provided, it
//! must match the configured `default_target` — arbitrary recipients are
//! rejected. This ensures the model can only send to operator-configured
//! destinations.

use async_trait::async_trait;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use zeroclaw_api::channel::{Channel, SendMessage};
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;

/// Per-tool channel-map handle — matches `zeroclaw_runtime::tools::PerToolChannelHandle`.
/// Defined locally so `zeroclaw-tools` doesn't depend on `zeroclaw-runtime`.
pub type PerToolChannelHandle =
    Arc<parking_lot::RwLock<std::collections::HashMap<String, Arc<dyn Channel>>>>;

/// Tool that sends a message through a configured channel.
///
/// Parameters:
/// - `channel`: Composite channel key (`telegram.default`, `telegram.prod`, etc.) or bare
///   type name (`telegram`, `slack`). Bare names resolve to `<type>.default`.
/// - `to`: Optional recipient/target ID. When omitted, uses the configured `default_target`.
///   When provided, must match the configured `default_target` for the resolved channel.
/// - `body`: Message content to send
pub struct ChannelSendTool {
    security: Arc<SecurityPolicy>,
    channel_map: PerToolChannelHandle,
    default_targets: Arc<parking_lot::RwLock<HashMap<String, String>>>,
}

impl ChannelSendTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        channel_map: PerToolChannelHandle,
        default_targets: Arc<parking_lot::RwLock<HashMap<String, String>>>,
    ) -> Self {
        Self {
            security,
            channel_map,
            default_targets,
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
                    "description": "Optional recipient ID. When omitted, uses the configured default_target for the channel. When provided, must match the configured default_target."
                },
                "body": {
                    "type": "string",
                    "description": "Message content to send"
                }
            },
            "required": ["channel", "body"]
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

        let body = args
            .get("body")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::Error::msg("Missing 'body' parameter"))?
            .to_string();

        let provided_to = args
            .get("to")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string);

        let (channel, resolved_key) = {
            let channel_map = self.channel_map.read();

            // 1. exact composite key
            let channel = channel_map.get(&channel_name).cloned();
            let resolved_key = if channel.is_some() {
                channel_name.clone()
            } else {
                String::new()
            };

            // 2. bare type → <type>.default
            let channel = channel.or_else(|| {
                if !channel_name.contains('.') {
                    let default_key = format!("{channel_name}.default");
                    channel_map.get(&default_key).cloned()
                } else {
                    None
                }
            });
            let resolved_key = if channel.is_some() && resolved_key.is_empty() {
                if !channel_name.contains('.') {
                    format!("{channel_name}.default")
                } else {
                    resolved_key
                }
            } else {
                resolved_key
            };

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
            let resolved_key = if channel.is_some() && resolved_key.is_empty() {
                if let Some(bare) = channel_name.split('.').next() {
                    if bare != channel_name {
                        format!("{bare}.default")
                    } else {
                        resolved_key
                    }
                } else {
                    resolved_key
                }
            } else {
                resolved_key
            };

            let channel = channel.ok_or_else(|| {
                // Intentional: enumerating available channel keys in the error
                // helps the agent correct a wrong channel name. These keys are
                // operator-configured, not secrets — safe to expose in ToolResult.error.
                let available: Vec<String> = channel_map.keys().cloned().collect();
                anyhow::Error::msg(format!(
                    "Channel '{}' not found. Available channels: {:?}",
                    channel_name, available
                ))
            })?;

            (channel, resolved_key)
        };

        // Resolve the recipient: validate against configured default_target.
        let to = {
            let targets = self.default_targets.read();
            let configured_target = targets.get(&resolved_key).cloned();

            match (provided_to, configured_target) {
                (Some(provided), Some(configured)) => {
                    if provided != configured {
                        return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(format!(
                                "Recipient '{}' does not match the configured default_target '{}' for channel '{}'. Arbitrary recipients are not allowed.",
                                provided, configured, resolved_key
                            )),
                        });
                    }
                    provided
                }
                (None, Some(configured)) => {
                    // Use the configured default_target.
                    configured
                }
                (Some(provided), None) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "No default_target configured for channel '{}'. Cannot send to arbitrary recipient '{}'.",
                            resolved_key, provided
                        )),
                    });
                }
                (None, None) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "No default_target configured for channel '{}'. Either configure a default_target or provide a matching recipient.",
                            resolved_key
                        )),
                    });
                }
            }
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
                resolved_key, to
            ),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use zeroclaw_api::attribution::{Attributable, ChannelKind, Role};

    /// Stub channel for tests — records send calls.
    struct StubChannel {
        name: String,
    }

    impl StubChannel {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
            }
        }
    }

    impl Attributable for StubChannel {
        fn role(&self) -> Role {
            Role::Channel(ChannelKind::Webhook)
        }
        fn alias(&self) -> &str {
            "test"
        }
    }

    #[async_trait]
    impl Channel for StubChannel {
        fn name(&self) -> &str {
            &self.name
        }
        async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
            Ok(())
        }
        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<zeroclaw_api::channel::ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn make_tool(
        handles: PerToolChannelHandle,
        default_targets: HashMap<String, String>,
    ) -> ChannelSendTool {
        ChannelSendTool::new(
            Arc::new(SecurityPolicy::default()),
            handles,
            Arc::new(parking_lot::RwLock::new(default_targets)),
        )
    }

    #[test]
    fn tool_name_and_description() {
        let tool = make_tool(
            Arc::new(parking_lot::RwLock::new(HashMap::new())),
            HashMap::new(),
        );
        assert_eq!(tool.name(), "channel_send");
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn parameter_schema_has_required_fields() {
        let tool = make_tool(
            Arc::new(parking_lot::RwLock::new(HashMap::new())),
            HashMap::new(),
        );
        let schema = tool.parameters_schema();
        let required = schema.get("required").unwrap().as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("channel")));
        assert!(required.iter().any(|v| v.as_str() == Some("body")));
        // 'to' is no longer required
        assert!(!required.iter().any(|v| v.as_str() == Some("to")));
    }

    #[test]
    fn spec_matches_metadata() {
        let tool = make_tool(
            Arc::new(parking_lot::RwLock::new(HashMap::new())),
            HashMap::new(),
        );
        let spec = tool.spec();
        assert_eq!(spec.name, tool.name());
        assert_eq!(spec.description, tool.description());
    }

    #[tokio::test]
    async fn empty_channel_map_returns_error_with_available_list() {
        let tool = make_tool(
            Arc::new(parking_lot::RwLock::new(HashMap::new())),
            HashMap::new(),
        );
        let result = tool
            .execute(serde_json::json!({
                "channel": "telegram.default",
                "body": "hello"
            }))
            .await;
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"));
        assert!(err.contains("Available channels"));
    }

    #[tokio::test]
    async fn composite_key_resolves_exact_match() {
        let map: PerToolChannelHandle = Arc::new(parking_lot::RwLock::new(HashMap::new()));
        map.write().insert(
            "telegram.prod".to_string(),
            Arc::new(StubChannel::new("telegram.prod")),
        );
        let mut targets = HashMap::new();
        targets.insert("telegram.prod".to_string(), "chat_482910".to_string());
        let tool = make_tool(map, targets);
        let result = tool
            .execute(serde_json::json!({
                "channel": "telegram.prod",
                "to": "chat_482910",
                "body": "hello"
            }))
            .await;
        assert!(result.unwrap().success);
    }

    #[tokio::test]
    async fn bare_type_key_resolves_to_default() {
        let map: PerToolChannelHandle = Arc::new(parking_lot::RwLock::new(HashMap::new()));
        map.write().insert(
            "telegram.default".to_string(),
            Arc::new(StubChannel::new("telegram.default")),
        );
        let mut targets = HashMap::new();
        targets.insert("telegram.default".to_string(), "chat_482910".to_string());
        let tool = make_tool(map, targets);
        let result = tool
            .execute(serde_json::json!({
                "channel": "telegram",
                "to": "chat_482910",
                "body": "hello"
            }))
            .await;
        assert!(result.unwrap().success);
    }

    #[tokio::test]
    async fn aliased_key_falls_back_to_default() {
        let map: PerToolChannelHandle = Arc::new(parking_lot::RwLock::new(HashMap::new()));
        map.write().insert(
            "telegram.default".to_string(),
            Arc::new(StubChannel::new("telegram.default")),
        );
        let mut targets = HashMap::new();
        targets.insert("telegram.default".to_string(), "chat_482910".to_string());
        let tool = make_tool(map, targets);
        let result = tool
            .execute(serde_json::json!({
                "channel": "telegram.prod",
                "to": "chat_482910",
                "body": "hello"
            }))
            .await;
        assert!(result.unwrap().success);
    }

    /// Regression: configured/default target is accepted.
    #[tokio::test]
    async fn configured_target_is_accepted() {
        let map: PerToolChannelHandle = Arc::new(parking_lot::RwLock::new(HashMap::new()));
        map.write().insert(
            "telegram.default".to_string(),
            Arc::new(StubChannel::new("telegram.default")),
        );
        let mut targets = HashMap::new();
        targets.insert("telegram.default".to_string(), "chat_123".to_string());
        let tool = make_tool(map, targets);
        let result = tool
            .execute(serde_json::json!({
                "channel": "telegram",
                "to": "chat_123",
                "body": "hello"
            }))
            .await;
        assert!(result.unwrap().success);
    }

    /// Regression: arbitrary different recipient is rejected before Channel::send().
    #[tokio::test]
    async fn arbitrary_recipient_is_rejected() {
        let map: PerToolChannelHandle = Arc::new(parking_lot::RwLock::new(HashMap::new()));
        map.write().insert(
            "telegram.default".to_string(),
            Arc::new(StubChannel::new("telegram.default")),
        );
        let mut targets = HashMap::new();
        targets.insert("telegram.default".to_string(), "chat_123".to_string());
        let tool = make_tool(map, targets);
        let result = tool
            .execute(serde_json::json!({
                "channel": "telegram",
                "to": "chat_999_evil",
                "body": "hello"
            }))
            .await;
        let result = result.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("does not match"));
        assert!(result.error.as_ref().unwrap().contains("chat_123"));
    }

    /// Regression: omitted `to` resolves to configured default_target.
    #[tokio::test]
    async fn omitted_to_resolves_to_configured_default() {
        let map: PerToolChannelHandle = Arc::new(parking_lot::RwLock::new(HashMap::new()));
        map.write().insert(
            "telegram.default".to_string(),
            Arc::new(StubChannel::new("telegram.default")),
        );
        let mut targets = HashMap::new();
        targets.insert("telegram.default".to_string(), "chat_123".to_string());
        let tool = make_tool(map, targets);
        let result = tool
            .execute(serde_json::json!({
                "channel": "telegram",
                "body": "hello"
            }))
            .await;
        assert!(result.unwrap().success);
    }

    /// Regression: no default_target configured and no `to` provided — error.
    #[tokio::test]
    async fn no_default_target_and_no_to_returns_error() {
        let map: PerToolChannelHandle = Arc::new(parking_lot::RwLock::new(HashMap::new()));
        map.write().insert(
            "telegram.default".to_string(),
            Arc::new(StubChannel::new("telegram.default")),
        );
        let tool = make_tool(map, HashMap::new());
        let result = tool
            .execute(serde_json::json!({
                "channel": "telegram",
                "body": "hello"
            }))
            .await;
        let result = result.unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_ref()
                .unwrap()
                .contains("No default_target configured")
        );
    }

    /// Regression: no default_target configured but `to` provided — error (arbitrary recipient).
    #[tokio::test]
    async fn no_default_target_with_to_returns_error() {
        let map: PerToolChannelHandle = Arc::new(parking_lot::RwLock::new(HashMap::new()));
        map.write().insert(
            "telegram.default".to_string(),
            Arc::new(StubChannel::new("telegram.default")),
        );
        let tool = make_tool(map, HashMap::new());
        let result = tool
            .execute(serde_json::json!({
                "channel": "telegram",
                "to": "chat_999",
                "body": "hello"
            }))
            .await;
        let result = result.unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_ref()
                .unwrap()
                .contains("Cannot send to arbitrary recipient")
        );
    }
}
