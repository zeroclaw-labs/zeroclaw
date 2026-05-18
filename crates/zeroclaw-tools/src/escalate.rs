//! Human escalation tool with urgency-aware routing.
//!
//! Exposes `escalate_to_human` as an agent-callable tool that sends a structured
//! escalation message to a messaging channel. High/critical urgency escalations
//! additionally notify any channels listed in `[escalation] alert_channels`.
//! Supports optional blocking mode to wait for a human response.

use crate::ask_user::ChannelMapHandle;
use async_trait::async_trait;
use parking_lot::RwLock;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::policy::ToolOperation;

const DEFAULT_TIMEOUT_SECS: u64 = 600;

const VALID_URGENCY_LEVELS: &[&str] = &["low", "medium", "high", "critical"];

/// Agent-callable tool for escalating situations to a human operator with urgency routing.
pub struct EscalateToHumanTool {
    security: Arc<SecurityPolicy>,
    channel_map: ChannelMapHandle,
    alert_channels: Vec<String>,
}

impl EscalateToHumanTool {
    pub fn new(security: Arc<SecurityPolicy>, alert_channels: Vec<String>) -> Self {
        Self {
            security,
            channel_map: Arc::new(RwLock::new(HashMap::new())),
            alert_channels,
        }
    }

    /// Return the shared handle so callers can populate it after channel init.
    pub fn channel_map_handle(&self) -> ChannelMapHandle {
        Arc::clone(&self.channel_map)
    }

    /// Format the escalation message with urgency prefix.
    fn format_message(urgency: &str, summary: &str, context: Option<&str>) -> String {
        let prefix = match urgency {
            "low" => "\u{2139}\u{fe0f} [LOW]",
            "high" => "\u{1f534} [HIGH]",
            "critical" => "\u{1f6a8} [CRITICAL]",
            // "medium" and any other value
            _ => "\u{26a0}\u{fe0f} [MEDIUM]",
        };

        let mut lines = vec![
            format!("{prefix} Agent Escalation"),
            format!("Summary: {summary}"),
        ];

        if let Some(ctx) = context {
            lines.push(format!("Context: {ctx}"));
        }

        lines.push("---".to_string());
        lines.push("Reply to this message to respond.".to_string());

        lines.join("\n")
    }

    /// Send best-effort alerts to configured alert channels for high/critical urgency.
    async fn send_alerts(&self, text: &str) {
        // Collect Arc clones while holding the lock, then drop the guard before awaiting.
        let targets: Vec<(String, Arc<dyn Channel>)> = {
            let channels = self.channel_map.read();
            self.alert_channels
                .iter()
                .filter_map(|name| {
                    if let Some(ch) = channels.get(name) {
                        Some((name.clone(), Arc::clone(ch)))
                    } else {
                        tracing::warn!(
                            "escalate_to_human: alert channel '{name}' not found in channel map"
                        );
                        None
                    }
                })
                .collect()
        };
        for (name, ch) in targets {
            let msg = SendMessage::new(text, "");
            if let Err(e) = ch.send(&msg).await {
                tracing::warn!("escalate_to_human: alert to channel '{name}' failed: {e}");
            }
        }
    }
}

#[async_trait]
impl Tool for EscalateToHumanTool {
    fn name(&self) -> &str {
        "escalate_to_human"
    }

    fn description(&self) -> &str {
        "Escalate a situation to a human operator with urgency routing. \
         Sends a structured message to the active channel. High/critical urgency \
         also notifies any channels listed in `[escalation] alert_channels`. \
         Optionally blocks to wait for a human response."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "One-line escalation summary"
                },
                "context": {
                    "type": "string",
                    "description": "Detailed context for the human"
                },
                "urgency": {
                    "type": "string",
                    "enum": ["low", "medium", "high", "critical"],
                    "description": "Urgency level (default: medium). high/critical also notifies alert_channels."
                },
                "wait_for_response": {
                    "type": "boolean",
                    "description": "Block and return the human's reply (default: false)"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Seconds to wait for a response when wait_for_response is true (default: 600)"
                }
            },
            "required": ["summary"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Security gate
        if let Err(e) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "escalate_to_human")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Action blocked: {e}")),
            });
        }

        // Parse required params
        let summary = args
            .get("summary")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing 'summary' parameter"))?
            .to_string();

        let context = args
            .get("context")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let urgency = args
            .get("urgency")
            .and_then(|v| v.as_str())
            .unwrap_or("medium");

        if !VALID_URGENCY_LEVELS.contains(&urgency) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Invalid urgency '{}'. Must be one of: {}",
                    urgency,
                    VALID_URGENCY_LEVELS.join(", ")
                )),
            });
        }

        let wait_for_response = args
            .get("wait_for_response")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        // Format the message
        let text = Self::format_message(urgency, &summary, context.as_deref());

        // Resolve channel — block-scoped to drop the RwLock guard before any .await
        let (channel_name, channel): (String, Arc<dyn Channel>) = {
            let channels = self.channel_map.read();
            if channels.is_empty() {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("No channels available yet (channels not initialized)".to_string()),
                });
            }
            let (name, ch) = channels.iter().next().ok_or_else(|| {
                anyhow::anyhow!("No channels available. Configure at least one channel.")
            })?;
            (name.clone(), ch.clone())
        };

        // Channels without free-form `listen` support (e.g. ACP today, until
        // the elicitation RFD lands) can't deliver the human's reply. Fail
        // fast so the agent can route the escalation differently or proceed
        // without blocking — the alternative is silently timing out for
        // `timeout_secs` seconds.
        // RFD: https://github.com/zed-industries/agent-client-protocol/blob/main/docs/rfds/elicitation.mdx
        if wait_for_response && !channel.supports_free_form_ask() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Channel '{channel_name}' cannot receive a free-form reply, \
                     so `wait_for_response` is unsupported (awaits ACP elicitation RFD). \
                     Retry with `wait_for_response: false`."
                )),
            });
        }

        // Send the escalation message
        let msg = SendMessage::new(&text, "");
        if let Err(e) = channel.send(&msg).await {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Failed to send escalation to channel '{channel_name}': {e}"
                )),
            });
        }

        // Notify alert channels for high/critical urgency (non-blocking, best-effort)
        if (urgency == "high" || urgency == "critical") && !self.alert_channels.is_empty() {
            self.send_alerts(&text).await;
        }

        if wait_for_response {
            // Block and wait for human response (same pattern as ask_user)
            let (tx, mut rx) = tokio::sync::mpsc::channel::<ChannelMessage>(1);
            let timeout = std::time::Duration::from_secs(timeout_secs);

            let listen_channel = Arc::clone(&channel);
            let listen_handle = tokio::spawn(async move { listen_channel.listen(tx).await });

            let response = tokio::time::timeout(timeout, rx.recv()).await;
            listen_handle.abort();

            match response {
                Ok(Some(msg)) => Ok(ToolResult {
                    success: true,
                    output: msg.content,
                    error: None,
                }),
                Ok(None) => Ok(ToolResult {
                    success: false,
                    output: "TIMEOUT".to_string(),
                    error: Some("Channel closed before receiving a response".to_string()),
                }),
                Err(_) => Ok(ToolResult {
                    success: false,
                    output: "TIMEOUT".to_string(),
                    error: Some(format!(
                        "No response received within {timeout_secs} seconds"
                    )),
                }),
            }
        } else {
            // Non-blocking: return confirmation
            Ok(ToolResult {
                success: true,
                output: json!({
                    "status": "escalated",
                    "urgency": urgency,
                    "channel": channel_name,
                })
                .to_string(),
                error: None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stub channel that records sent messages but never produces incoming messages.
    struct SilentChannel {
        channel_name: String,
        sent: Arc<RwLock<Vec<String>>>,
    }

    impl SilentChannel {
        fn new(name: &str) -> Self {
            Self {
                channel_name: name.to_string(),
                sent: Arc::new(RwLock::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl Channel for SilentChannel {
        fn name(&self) -> &str {
            &self.channel_name
        }

        async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
            self.sent.write().push(message.content.clone());
            Ok(())
        }

        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            // Never sends anything — simulates no user response
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
            Ok(())
        }
    }

    /// A stub channel that immediately responds with a canned message.
    struct RespondingChannel {
        channel_name: String,
        response: String,
        sent: Arc<RwLock<Vec<String>>>,
    }

    impl RespondingChannel {
        fn new(name: &str, response: &str) -> Self {
            Self {
                channel_name: name.to_string(),
                response: response.to_string(),
                sent: Arc::new(RwLock::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl Channel for RespondingChannel {
        fn name(&self) -> &str {
            &self.channel_name
        }

        async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
            self.sent.write().push(message.content.clone());
            Ok(())
        }

        async fn listen(
            &self,
            tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            let msg = ChannelMessage {
                id: "resp_1".to_string(),
                sender: "human".to_string(),
                reply_target: "human".to_string(),
                content: self.response.clone(),
                channel: self.channel_name.clone(),
                timestamp: 1000,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
            };
            let _ = tx.send(msg).await;
            Ok(())
        }
    }

    fn make_tool_with_channels(channels: Vec<(&str, Arc<dyn Channel>)>) -> EscalateToHumanTool {
        let tool = EscalateToHumanTool::new(Arc::new(SecurityPolicy::default()), vec![]);
        let map: HashMap<String, Arc<dyn Channel>> = channels
            .into_iter()
            .map(|(name, ch)| (name.to_string(), ch))
            .collect();
        *tool.channel_map.write() = map;
        tool
    }

    // ── 1. test_tool_metadata ──

    #[test]
    fn test_tool_metadata() {
        let tool = EscalateToHumanTool::new(Arc::new(SecurityPolicy::default()), vec![]);
        assert_eq!(tool.name(), "escalate_to_human");
        assert!(!tool.description().is_empty());
        assert!(tool.description().to_lowercase().contains("escalat"));
    }

    // ── 2. test_parameters_schema ──

    #[test]
    fn test_parameters_schema() {
        let tool = EscalateToHumanTool::new(Arc::new(SecurityPolicy::default()), vec![]);
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["summary"].is_object());
        assert!(schema["properties"]["urgency"].is_object());
        assert!(schema["properties"]["context"].is_object());
        assert!(schema["properties"]["wait_for_response"].is_object());
        assert!(schema["properties"]["timeout_secs"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "summary"));
        // Optional fields should not be in required
        assert!(!required.iter().any(|v| v == "urgency"));
        assert!(!required.iter().any(|v| v == "context"));
        assert!(!required.iter().any(|v| v == "wait_for_response"));
        assert!(!required.iter().any(|v| v == "timeout_secs"));
    }

    // ── 3. test_default_urgency_is_medium ──

    #[tokio::test]
    async fn test_default_urgency_is_medium() {
        let channel = Arc::new(SilentChannel::new("test"));
        let sent = Arc::clone(&channel.sent);
        let tool = make_tool_with_channels(vec![("test", channel as Arc<dyn Channel>)]);

        let result = tool
            .execute(json!({ "summary": "Need help" }))
            .await
            .unwrap();

        assert!(result.success, "error: {:?}", result.error);
        // Check the output JSON contains medium urgency
        assert!(result.output.contains("\"medium\""));
        // Check the sent message contains MEDIUM prefix
        let messages = sent.read();
        assert!(!messages.is_empty());
        assert!(messages[0].contains("[MEDIUM]"));
    }

    // ── 4. test_message_format_low ──

    #[test]
    fn test_message_format_low() {
        let msg = EscalateToHumanTool::format_message("low", "Disk space low", None);
        assert!(msg.starts_with("\u{2139}\u{fe0f} [LOW]"));
        assert!(msg.contains("Summary: Disk space low"));
        assert!(msg.contains("Reply to this message to respond."));
    }

    // ── 5. test_message_format_critical ──

    #[test]
    fn test_message_format_critical() {
        let msg = EscalateToHumanTool::format_message(
            "critical",
            "Production down",
            Some("Database unreachable for 5 minutes"),
        );
        assert!(msg.starts_with("\u{1f6a8} [CRITICAL]"));
        assert!(msg.contains("Summary: Production down"));
        assert!(msg.contains("Context: Database unreachable for 5 minutes"));
    }

    // ── 6. test_invalid_urgency_rejected ──

    #[tokio::test]
    async fn test_invalid_urgency_rejected() {
        let tool = make_tool_with_channels(vec![(
            "test",
            Arc::new(SilentChannel::new("test")) as Arc<dyn Channel>,
        )]);

        let result = tool
            .execute(json!({ "summary": "Help", "urgency": "extreme" }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("Invalid urgency"));
        assert!(result.error.as_deref().unwrap().contains("extreme"));
    }

    // ── 7. test_non_blocking_returns_status ──

    #[tokio::test]
    async fn test_non_blocking_returns_status() {
        let tool = make_tool_with_channels(vec![(
            "slack",
            Arc::new(SilentChannel::new("slack")) as Arc<dyn Channel>,
        )]);

        let result = tool
            .execute(json!({
                "summary": "Need approval",
                "urgency": "low"
            }))
            .await
            .unwrap();

        assert!(result.success, "error: {:?}", result.error);
        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["status"], "escalated");
        assert_eq!(parsed["urgency"], "low");
        assert_eq!(parsed["channel"], "slack");
    }

    // ── 8. test_blocking_mode_returns_response ──

    #[tokio::test]
    async fn test_blocking_mode_returns_response() {
        let tool = make_tool_with_channels(vec![(
            "test",
            Arc::new(RespondingChannel::new("test", "Approved, go ahead")) as Arc<dyn Channel>,
        )]);

        let result = tool
            .execute(json!({
                "summary": "Need deployment approval",
                "wait_for_response": true,
                "timeout_secs": 5
            }))
            .await
            .unwrap();

        assert!(result.success, "error: {:?}", result.error);
        assert_eq!(result.output, "Approved, go ahead");
    }

    // ── 9. test_blocking_mode_timeout ──

    #[tokio::test]
    async fn test_blocking_mode_timeout() {
        let tool = make_tool_with_channels(vec![(
            "test",
            Arc::new(SilentChannel::new("test")) as Arc<dyn Channel>,
        )]);

        let result = tool
            .execute(json!({
                "summary": "Waiting for response",
                "wait_for_response": true,
                "timeout_secs": 1
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert_eq!(result.output, "TIMEOUT");
        assert!(result.error.as_deref().unwrap().contains("1 seconds"));
    }

    /// Stub channel that mirrors ACP's constraint: `send` works, but
    /// `listen` is unsupported and `supports_free_form_ask` reports false.
    struct StructuredOnlyChannel {
        channel_name: String,
        sent: Arc<RwLock<Vec<String>>>,
    }

    impl StructuredOnlyChannel {
        fn new(name: &str) -> Self {
            Self {
                channel_name: name.to_string(),
                sent: Arc::new(RwLock::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl Channel for StructuredOnlyChannel {
        fn name(&self) -> &str {
            &self.channel_name
        }

        async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
            self.sent.write().push(message.content.clone());
            Ok(())
        }

        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            anyhow::bail!("listen not supported")
        }

        fn supports_free_form_ask(&self) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn wait_for_response_fails_fast_on_structured_only_channel() {
        // ACP-shaped channel: can't listen, so wait_for_response must fail
        // immediately rather than timing out silently.
        let stub = Arc::new(StructuredOnlyChannel::new("acp"));
        let stub_clone: Arc<dyn Channel> = stub.clone();
        let tool = make_tool_with_channels(vec![("acp", stub_clone)]);

        let started = std::time::Instant::now();
        let result = tool
            .execute(json!({
                "summary": "Need confirmation",
                "wait_for_response": true,
                "timeout_secs": 30,
            }))
            .await
            .unwrap();
        let elapsed = started.elapsed();

        assert!(!result.success, "expected failure, got: {:?}", result);
        let err = result.error.unwrap_or_default();
        assert!(
            err.contains("wait_for_response"),
            "error should mention wait_for_response: {err}"
        );
        // Must fail fast — well under the 30s timeout.
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "expected fast-fail; took {elapsed:?}"
        );
        // No message should have been sent — gate fires before send.
        assert!(stub.sent.read().is_empty());
    }

    #[tokio::test]
    async fn non_blocking_works_on_structured_only_channel() {
        // The gate must NOT fire when wait_for_response is false — the
        // escalation message itself goes through `send`, which ACP supports.
        let stub = Arc::new(StructuredOnlyChannel::new("acp"));
        let stub_clone: Arc<dyn Channel> = stub.clone();
        let tool = make_tool_with_channels(vec![("acp", stub_clone)]);

        let result = tool
            .execute(json!({
                "summary": "FYI: deploy started",
                "urgency": "low",
            }))
            .await
            .unwrap();

        assert!(result.success, "error: {:?}", result.error);
        assert_eq!(stub.sent.read().len(), 1);
    }

    // ── 10. test_high_urgency_succeeds_without_alert_channels ──

    #[tokio::test]
    async fn test_high_urgency_succeeds_without_alert_channels() {
        // High urgency with no alert_channels configured should still succeed
        let tool = make_tool_with_channels(vec![(
            "test",
            Arc::new(SilentChannel::new("test")) as Arc<dyn Channel>,
        )]);

        let result = tool
            .execute(json!({
                "summary": "Critical alert",
                "urgency": "high"
            }))
            .await
            .unwrap();

        assert!(result.success, "error: {:?}", result.error);
        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["status"], "escalated");
        assert_eq!(parsed["urgency"], "high");
    }
}
