//! Inter-agent message tool for Phase 1 multi-agent communication.
//!
//! This module implements the `InterAgentMessageTool` which enables agents
//! to send messages to each other using direct messages, broadcasts, and
//! request-response patterns.

use super::traits::{Tool, ToolResult};
use crate::coordination::channel::AgentMessageChannel;
use crate::coordination::message::{AgentId, AgentMessage, MessagePayload};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;

/// Tool for sending messages between agents.
///
/// Supports three communication patterns:
/// - Direct messages (notify mode): One-way communication
/// - Broadcast messages: Send to all agents
/// - Request-response (request mode): Blocking communication with timeout
pub struct InterAgentMessageTool {
    /// Message channel for communication
    channel: std::sync::Arc<dyn AgentMessageChannel>,
    /// This agent's ID
    sender_id: AgentId,
    /// Security policy for access control
    security: std::sync::Arc<SecurityPolicy>,
}

impl InterAgentMessageTool {
    /// Create a new InterAgentMessageTool.
    ///
    /// # Arguments
    ///
    /// * `channel` - Message channel for communication
    /// * `sender_id` - This agent's ID
    /// * `security` - Security policy for access control
    pub fn new(
        channel: std::sync::Arc<dyn AgentMessageChannel>,
        sender_id: AgentId,
        security: std::sync::Arc<SecurityPolicy>,
    ) -> Self {
        Self {
            channel,
            sender_id,
            security,
        }
    }

    /// Validate target agent ID against security policy.
    fn validate_target(&self, target: &str) -> anyhow::Result<()> {
        // Check for empty target
        if target.is_empty() {
            anyhow::bail!("target agent ID cannot be empty");
        }

        // Check for suspicious patterns
        if target.contains('/') || target.contains("..") {
            anyhow::bail!("invalid target agent ID: contains path traversal characters");
        }

        // Length check
        if target.len() > 256 {
            anyhow::bail!("target agent ID too long (max 256 characters)");
        }

        Ok(())
    }

    /// Parse timeout from arguments.
    fn parse_timeout(&self, args: &serde_json::Value) -> anyhow::Result<std::time::Duration> {
        let timeout_seconds = args
            .get("timeout_seconds")
            .and_then(|v| v.as_u64())
            .unwrap_or(30);

        if timeout_seconds > 3600 {
            anyhow::bail!("timeout too large (max 3600 seconds)");
        }

        Ok(std::time::Duration::from_secs(timeout_seconds))
    }
}

#[async_trait]
impl Tool for InterAgentMessageTool {
    fn name(&self) -> &str {
        "send_agent_message"
    }

    fn description(&self) -> &str {
        "Send a message to another agent. Supports direct messages, broadcasts, and request-response patterns."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "to": {
                    "type": "string",
                    "description": "Target agent ID (or '*' for broadcast)"
                },
                "message": {
                    "type": "string",
                    "description": "Message content"
                },
                "mode": {
                    "type": "string",
                    "enum": ["notify", "request"],
                    "description": "Communication mode: 'notify' for one-way, 'request' for response",
                    "default": "notify"
                },
                "timeout_seconds": {
                    "type": "number",
                    "description": "Timeout for request mode (default: 30)",
                    "default": 30
                }
            },
            "required": ["to", "message"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Apply security policy check
        use crate::security::policy::ToolOperation;
        if let Err(e) = self.security.enforce_tool_operation(ToolOperation::Act, "send_agent_message") {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Security policy denied operation: {}", e)),
            });
        }

        // Extract arguments
        let to = args
            .get("to")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required field: 'to'"))?;

        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required field: 'message'"))?;

        let mode = args
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("notify");

        // Validate target
        if let Err(e) = self.validate_target(to) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            });
        }

        match mode {
            "notify" => {
                if to == "*" {
                    // Broadcast
                    let payload = MessagePayload::text(message);
                    let msg = AgentMessage::broadcast(self.sender_id.clone(), payload);
                    self.channel.send(msg).await.map_err(|e| {
                        anyhow::anyhow!("failed to send broadcast: {}", e)
                    })?;

                    Ok(ToolResult {
                        success: true,
                        output: format!("Broadcast sent to all agents"),
                        error: None,
                    })
                } else {
                    // Direct message
                    let target_id = AgentId::new(to.to_string());
                    let payload = MessagePayload::text(message);
                    let msg = AgentMessage::notification(
                        self.sender_id.clone(),
                        target_id,
                        payload,
                    );
                    self.channel.send(msg).await.map_err(|e| {
                        anyhow::anyhow!("failed to send message: {}", e)
                    })?;

                    Ok(ToolResult {
                        success: true,
                        output: format!("Message sent to agent '{}'", to),
                        error: None,
                    })
                }
            }
            "request" => {
                if to == "*" {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("broadcast mode not supported for request".to_string()),
                    });
                }

                let target_id = AgentId::new(to.to_string());
                let payload = MessagePayload::text(message);
                let timeout = self.parse_timeout(&args)?;

                match self.channel.request(target_id, payload, timeout).await {
                    Ok(response_payload) => {
                        let response_text = match response_payload {
                            MessagePayload::Text { content } => content,
                            MessagePayload::Data { value } => value.to_string(),
                            _ => "(non-text response)".to_string(),
                        };

                        Ok(ToolResult {
                            success: true,
                            output: format!("Response from '{}': {}", to, response_text),
                            error: None,
                        })
                    }
                    Err(e) => {
                        Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some(format!("Request failed: {}", e)),
                        })
                    }
                }
            }
            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("invalid mode: '{}'. Use 'notify' or 'request'", mode)),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordination::channel::MemoryMessageChannel;
    use std::collections::HashMap;

    fn create_test_policy() -> std::sync::Arc<SecurityPolicy> {
        std::sync::Arc::new(SecurityPolicy::default())
    }

    #[tokio::test]
    async fn tool_name_and_description() {
        let channel = std::sync::Arc::new(MemoryMessageChannel::new());
        let sender_id = AgentId::new("agent_a".to_string());
        let tool = InterAgentMessageTool::new(channel, sender_id, create_test_policy());

        assert_eq!(tool.name(), "send_agent_message");
        assert!(!tool.description().is_empty());
    }

    #[tokio::test]
    async fn parameters_schema_contains_required_fields() {
        let channel = std::sync::Arc::new(MemoryMessageChannel::new());
        let sender_id = AgentId::new("agent_a".to_string());
        let tool = InterAgentMessageTool::new(channel, sender_id, create_test_policy());

        let schema = tool.parameters_schema();
        let props = schema.get("properties").unwrap().as_object().unwrap();
        let required = schema.get("required").unwrap().as_array().unwrap();

        assert!(props.contains_key("to"));
        assert!(props.contains_key("message"));
        assert!(props.contains_key("mode"));
        assert!(props.contains_key("timeout_seconds"));

        assert_eq!(required.len(), 2);
        assert!(required.iter().any(|v| v == "to"));
        assert!(required.iter().any(|v| v == "message"));
    }

    #[tokio::test]
    async fn validate_target_rejects_empty() {
        let channel = std::sync::Arc::new(MemoryMessageChannel::new());
        let sender_id = AgentId::new("agent_a".to_string());
        let tool = InterAgentMessageTool::new(channel, sender_id, create_test_policy());

        assert!(tool.validate_target("").is_err());
    }

    #[tokio::test]
    async fn validate_target_rejects_path_traversal() {
        let channel = std::sync::Arc::new(MemoryMessageChannel::new());
        let sender_id = AgentId::new("agent_a".to_string());
        let tool = InterAgentMessageTool::new(channel, sender_id, create_test_policy());

        assert!(tool.validate_target("../etc/passwd").is_err());
        assert!(tool.validate_target("agent/../../etc").is_err());
    }

    #[tokio::test]
    async fn validate_target_rejects_too_long() {
        let channel = std::sync::Arc::new(MemoryMessageChannel::new());
        let sender_id = AgentId::new("agent_a".to_string());
        let tool = InterAgentMessageTool::new(channel, sender_id, create_test_policy());

        let long_id = "a".repeat(300);
        assert!(tool.validate_target(&long_id).is_err());
    }

    #[tokio::test]
    async fn validate_target_accepts_valid() {
        let channel = std::sync::Arc::new(MemoryMessageChannel::new());
        let sender_id = AgentId::new("agent_a".to_string());
        let tool = InterAgentMessageTool::new(channel, sender_id, create_test_policy());

        assert!(tool.validate_target("agent_b").is_ok());
        assert!(tool.validate_target("delegate:researcher").is_ok());
        assert!(tool.validate_target("*").is_ok());
    }

    #[tokio::test]
    async fn parse_timeout_defaults_to_30() {
        let channel = std::sync::Arc::new(MemoryMessageChannel::new());
        let sender_id = AgentId::new("agent_a".to_string());
        let tool = InterAgentMessageTool::new(channel, sender_id, create_test_policy());

        let args = json!({});
        let timeout = tool.parse_timeout(&args).unwrap();
        assert_eq!(timeout, Duration::from_secs(30));
    }

    #[tokio::test]
    async fn parse_timeout_from_args() {
        let channel = std::sync::Arc::new(MemoryMessageChannel::new());
        let sender_id = AgentId::new("agent_a".to_string());
        let tool = InterAgentMessageTool::new(channel, sender_id, create_test_policy());

        let args = json!({"timeout_seconds": 60});
        let timeout = tool.parse_timeout(&args).unwrap();
        assert_eq!(timeout, Duration::from_secs(60));
    }

    #[tokio::test]
    async fn parse_timeout_rejects_too_large() {
        let channel = std::sync::Arc::new(MemoryMessageChannel::new());
        let sender_id = AgentId::new("agent_a".to_string());
        let tool = InterAgentMessageTool::new(channel, sender_id, create_test_policy());

        let args = json!({"timeout_seconds": 5000});
        assert!(tool.parse_timeout(&args).is_err());
    }

    #[tokio::test]
    async fn execute_notify_sends_message() {
        let channel = std::sync::Arc::new(MemoryMessageChannel::new());
        let agent_a = AgentId::new("agent_a".to_string());
        let agent_b = AgentId::new("agent_b".to_string());

        channel.register(&agent_a).await.unwrap();
        channel.register(&agent_b).await.unwrap();

        let tool = InterAgentMessageTool::new(
            channel.clone(),
            agent_a.clone(),
            create_test_policy(),
        );

        let args = json!({
            "to": "agent_b",
            "message": "hello from a",
            "mode": "notify"
        });

        let result = tool.execute(args).await.unwrap();
        assert!(result.success);
        assert!(result.error.is_none());

        // Verify message was received
        let msg = channel
            .receive(&agent_b, Duration::from_millis(100))
            .await
            .unwrap();
        assert_eq!(msg.from(), &agent_a);
    }

    #[tokio::test]
    async fn execute_broadcast_sends_to_all() {
        let channel = std::sync::Arc::new(MemoryMessageChannel::new());
        let agent_a = AgentId::new("agent_a".to_string());
        let agent_b = AgentId::new("agent_b".to_string());
        let agent_c = AgentId::new("agent_c".to_string());

        channel.register(&agent_a).await.unwrap();
        channel.register(&agent_b).await.unwrap();
        channel.register(&agent_c).await.unwrap();

        let tool = InterAgentMessageTool::new(
            channel.clone(),
            agent_a.clone(),
            create_test_policy(),
        );

        let args = json!({
            "to": "*",
            "message": "broadcast test",
            "mode": "notify"
        });

        let result = tool.execute(args).await.unwrap();
        assert!(result.success);

        // Verify both agents received the message
        let msg_b = channel
            .receive(&agent_b, Duration::from_millis(100))
            .await
            .unwrap();
        let msg_c = channel
            .receive(&agent_c, Duration::from_millis(100))
            .await
            .unwrap();

        assert_eq!(msg_b.payload().as_text(), Some("broadcast test"));
        assert_eq!(msg_c.payload().as_text(), Some("broadcast test"));
    }

    #[tokio::test]
    async fn execute_request_mode_errors_on_broadcast() {
        let channel = std::sync::Arc::new(MemoryMessageChannel::new());
        let agent_a = AgentId::new("agent_a".to_string());

        channel.register(&agent_a).await.unwrap();

        let tool = InterAgentMessageTool::new(channel, agent_a, create_test_policy());

        let args = json!({
            "to": "*",
            "message": "test",
            "mode": "request"
        });

        let result = tool.execute(args).await.unwrap();
        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[tokio::test]
    async fn execute_missing_to_field_errors() {
        let channel = std::sync::Arc::new(MemoryMessageChannel::new());
        let agent_a = AgentId::new("agent_a".to_string());
        let tool = InterAgentMessageTool::new(channel, agent_a, create_test_policy());

        let args = json!({"message": "test"});

        let result = tool.execute(args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_missing_message_field_errors() {
        let channel = std::sync::Arc::new(MemoryMessageChannel::new());
        let agent_a = AgentId::new("agent_a".to_string());
        let tool = InterAgentMessageTool::new(channel, agent_a, create_test_policy());

        let args = json!({"to": "agent_b"});

        let result = tool.execute(args).await;
        assert!(result.is_err());
    }
}
