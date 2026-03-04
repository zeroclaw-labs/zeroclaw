use super::traits::{Tool, ToolResult};
use crate::config::Config;
use crate::proactive_messaging::{
    guardrails, store, GuardrailDecision, GuardrailDenialReason, MessagePriority,
};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

pub struct SendUserMessageTool {
    config: Arc<Config>,
    security: Arc<SecurityPolicy>,
}

impl SendUserMessageTool {
    pub fn new(config: Arc<Config>, security: Arc<SecurityPolicy>) -> Self {
        Self { config, security }
    }
}

#[async_trait]
impl Tool for SendUserMessageTool {
    fn name(&self) -> &str {
        "send_user_message"
    }

    fn description(&self) -> &str {
        "Proactively send a message to a user on a specific channel. Messages are gated by quiet \
         hours and rate limits. During quiet hours, messages are queued for delivery when the \
         window ends. Use this to notify users of important events (e.g. failed deliveries, \
         completed tasks)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "The channel to send through (e.g. 'signal', 'telegram', 'discord', 'slack')"
                },
                "recipient": {
                    "type": "string",
                    "description": "The recipient identifier (phone number, user ID, chat ID, etc.)"
                },
                "message": {
                    "type": "string",
                    "description": "The message content to send"
                },
                "priority": {
                    "type": "string",
                    "enum": ["low", "normal", "high", "urgent"],
                    "description": "Message priority. 'urgent' bypasses quiet hours. Default: 'normal'"
                }
            },
            "required": ["channel", "recipient", "message"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Security gate (same pattern as PushoverTool)
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

        // Parse args
        let channel = args
            .get("channel")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing 'channel' parameter"))?;

        let recipient = args
            .get("recipient")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing 'recipient' parameter"))?;

        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing 'message' parameter"))?;

        let priority: MessagePriority = args
            .get("priority")
            .and_then(|v| v.as_str())
            .unwrap_or("normal")
            .parse()
            .unwrap_or(MessagePriority::Normal);

        let pm_config = &self.config.proactive_messaging;
        let workspace_dir = &self.config.workspace_dir;

        // Evaluate guardrails
        let decision = guardrails::evaluate_guardrails(pm_config, workspace_dir, priority);

        match decision {
            GuardrailDecision::Allowed => {
                // Deliver immediately
                match deliver_now(&self.config, channel, recipient, message).await {
                    Ok(()) => Ok(ToolResult {
                        success: true,
                        output: format!(
                            "Message sent to {recipient} on {channel}."
                        ),
                        error: None,
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to deliver message: {e}")),
                    }),
                }
            }
            GuardrailDecision::Denied(GuardrailDenialReason::QuietHours {
                window_end_description,
            }) => {
                // Queue for later delivery
                match store::enqueue_message(
                    workspace_dir,
                    channel,
                    recipient,
                    message,
                    priority,
                    Some(&format!("quiet hours (ends {window_end_description})")),
                    None,
                    pm_config.queue.ttl_hours,
                ) {
                    Ok(queued) => Ok(ToolResult {
                        success: true,
                        output: format!(
                            "Message queued (id: {}). Currently in quiet hours. \
                             Expected delivery after {window_end_description}.",
                            queued.id,
                        ),
                        error: None,
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to queue message: {e}")),
                    }),
                }
            }
            GuardrailDecision::Denied(reason) => {
                // Rate limit or other denial — do NOT queue
                Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Message denied: {reason}")),
                })
            }
        }
    }
}

async fn deliver_now(
    config: &Config,
    channel: &str,
    recipient: &str,
    message: &str,
) -> anyhow::Result<()> {
    // Try live channel first
    if let Some(live_ch) = crate::channels::get_live_channel(channel) {
        use crate::channels::SendMessage;
        live_ch.send(&SendMessage::new(message, recipient)).await?;
        return Ok(());
    }

    // Fall back to cron delivery
    crate::cron::scheduler::deliver_announcement(config, channel, recipient, message).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AutonomyLevel;

    fn test_security(level: AutonomyLevel, max_actions: u32) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: level,
            max_actions_per_hour: max_actions,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    #[test]
    fn tool_name_and_description() {
        let config = Arc::new(Config::default());
        let security = test_security(AutonomyLevel::Full, 100);
        let tool = SendUserMessageTool::new(config, security);
        assert_eq!(tool.name(), "send_user_message");
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn schema_has_required_fields() {
        let config = Arc::new(Config::default());
        let security = test_security(AutonomyLevel::Full, 100);
        let tool = SendUserMessageTool::new(config, security);
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("channel")));
        assert!(required.contains(&json!("recipient")));
        assert!(required.contains(&json!("message")));
    }

    #[tokio::test]
    async fn blocks_readonly_autonomy() {
        let config = Arc::new(Config::default());
        let security = test_security(AutonomyLevel::ReadOnly, 100);
        let tool = SendUserMessageTool::new(config, security);

        let result = tool
            .execute(json!({
                "channel": "telegram",
                "recipient": "123",
                "message": "hello"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn blocks_rate_limit() {
        let config = Arc::new(Config::default());
        let security = test_security(AutonomyLevel::Full, 0);
        let tool = SendUserMessageTool::new(config, security);

        let result = tool
            .execute(json!({
                "channel": "telegram",
                "recipient": "123",
                "message": "hello"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.unwrap().contains("rate limit"));
    }
}
