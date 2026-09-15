//! Human escalation tool with urgency-aware routing.

use crate::ask_user::ChannelMapHandle;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
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
    pub fn new(
        security: Arc<SecurityPolicy>,
        alert_channels: Vec<String>,
        channel_map: ChannelMapHandle,
    ) -> Self {
        Self {
            security,
            channel_map,
            alert_channels,
        }
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

    /// Send best-effort alerts to configured alert channels for high/critical
    /// urgency.
    ///
    /// Returns the names of channels that actually accepted the message, so
    /// callers can distinguish "notified a human somewhere" from "notified
    /// nobody". Channels whose `send` is a no-op (`supports_outbound_send()`
    /// is false) are skipped rather than counted as delivered — otherwise an
    /// alert-channel list made solely of back-channels would look successful.
    ///
    /// `already_sent` is the channel that has just received the escalation
    /// directly, if any. It is skipped by pointer identity rather than by name:
    /// one channel is commonly reachable under both a bare type key
    /// (`discord`) and a dotted alias (`discord.default`), so comparing names
    /// would still deliver the message twice into the same room.
    async fn send_alerts(
        &self,
        text: &str,
        already_sent: Option<&Arc<dyn Channel>>,
    ) -> Vec<String> {
        // Collect Arc clones while holding the lock, then drop the guard before awaiting.
        let targets: Vec<(String, Arc<dyn Channel>)> = {
            let channels = self.channel_map.read();
            self.alert_channels
                .iter()
                .filter_map(|name| {
                    let Some(ch) = channels.get(name) else {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"name": name})),
                            "escalate_to_human: alert channel not found in channel map"
                        );
                        return None;
                    };
                    if !ch.supports_outbound_send() {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"name": name})),
                            "escalate_to_human: alert channel cannot deliver outbound messages"
                        );
                        return None;
                    }
                    if already_sent.is_some_and(|origin| Arc::ptr_eq(origin, ch)) {
                        return None;
                    }
                    Some((name.clone(), Arc::clone(ch)))
                })
                .collect()
        };
        let mut delivered = Vec::new();
        for (name, ch) in targets {
            let msg = SendMessage::new(text, "");
            if let Err(e) = ch.send(&msg).await {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e), "name": name})),
                    "escalate_to_human: alert to channel failed"
                );
            } else {
                delivered.push(name);
            }
        }
        delivered
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
         also notifies any channels listed in `[escalation] alert_channels`, which \
         additionally serve as a fallback when the active channel cannot deliver. \
         The active channel is never alerted twice, and the result reports \
         `alerted_to` so you can see which alert channels actually accepted it — \
         an empty list means only the active channel was reached. \
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
                },
                "channel": {
                    "type": "string",
                    "description": "Channel to escalate on. Defaults to the channel this conversation is happening on."
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
                output: ToolOutput::default(),
                error: Some(format!("Action blocked: {e}")),
            });
        }

        // Parse required params
        let summary = args
            .get("summary")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "summary"})),
                    "escalate: missing summary parameter"
                );
                anyhow::Error::msg("Missing 'summary' parameter")
            })?
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
                output: ToolOutput::default(),
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

        let requested_channel = args
            .get("channel")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        // Resolve channel — block-scoped to drop the RwLock guard before any .await
        let (channel_name, channel): (String, Arc<dyn Channel>) = {
            let channels = self.channel_map.read();
            if channels.is_empty() {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some("No channels available yet (channels not initialized)".to_string()),
                });
            }
            if let Some(ref name) = requested_channel {
                // Explicit or origin-injected channel: honour it exactly rather
                // than silently escalating somewhere the operator isn't looking.
                let ch = channels.get(name.as_str()).cloned().ok_or_else(|| {
                    let available = channels.keys().cloned().collect::<Vec<_>>().join(", ");
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "channel_requested": name,
                                "available": &available,
                            })),
                        "escalate: requested channel not found"
                    );
                    anyhow::Error::msg(format!(
                        "Channel '{name}' not found. Available: {available}"
                    ))
                })?;
                (name.clone(), ch)
            } else {
                let (name, ch) = channels.iter().next().ok_or_else(|| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"missing": "channels"})),
                        "escalate: no channels configured"
                    );
                    anyhow::Error::msg("No channels available. Configure at least one channel.")
                })?;
                (name.clone(), ch.clone())
            }
        };

        // An escalation that nobody receives is worse than a failed one: the
        // agent believes a human was notified. RPC/WS back-channels return
        // `Ok(())` from `send` without rendering anything.
        //
        // Rather than fail outright, try the configured alert channels as a
        // genuine fallback — that is the one remedy available at this point in
        // the call, and for high/critical urgency they would have been notified
        // anyway. Only report success if a channel actually accepted the
        // message; otherwise fail honestly and suggest only remedies that work.
        if !channel.supports_outbound_send() {
            let delivered = if self.alert_channels.is_empty() {
                Vec::new()
            } else {
                // Nothing was delivered on the origin channel here, so there is
                // no prior send to deduplicate against.
                self.send_alerts(&text, None).await
            };

            if delivered.is_empty() {
                let remedy = if self.alert_channels.is_empty() {
                    "Re-run `escalate_to_human` with an explicit `channel` that can deliver, \
                     or configure `[escalation] alert_channels` as a fallback."
                } else {
                    "Re-run `escalate_to_human` with an explicit `channel` that can deliver; \
                     the configured `[escalation] alert_channels` could not deliver either."
                };
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!(
                        "Channel '{channel_name}' cannot deliver an escalation message \
                         (no outbound send support), so the human would never see it. \
                         {remedy}"
                    )),
                });
            }

            // A human was reached, but not on the requested channel, and this
            // path cannot host a free-form reply.
            if wait_for_response {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!(
                        "Channel '{channel_name}' cannot deliver an escalation message, so the \
                         alert was routed to `[escalation] alert_channels` ({}) instead. Those \
                         channels cannot return a reply to this turn, so `wait_for_response` is \
                         unsupported here. Retry with `wait_for_response: false`.",
                        delivered.join(", ")
                    )),
                });
            }

            return Ok(ToolResult {
                success: true,
                output: json!({
                    "status": "escalated_via_alert_channels",
                    "urgency": urgency,
                    "channel": channel_name,
                    "delivered_to": delivered,
                    "note": format!(
                        "Channel '{channel_name}' cannot deliver outbound messages; \
                         escalation was routed to the configured alert channels instead."
                    ),
                })
                .to_string()
                .into(),
                error: None,
            });
        }

        if wait_for_response && !channel.supports_free_form_ask() {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Channel '{channel_name}' cannot receive a free-form reply, \
                     so `wait_for_response` is unsupported (awaits ACP elicitation Phase 2). \
                     Retry with `wait_for_response: false`."
                )),
            });
        }

        // Send the escalation message
        let msg = SendMessage::new(&text, "");
        if let Err(e) = channel.send(&msg).await {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(format!(
                    "Failed to send escalation to channel '{channel_name}': {e}"
                )),
            });
        }

        // Notify alert channels for high/critical urgency. Best-effort, but not
        // silent: the model is told which channels took it, so it cannot claim
        // an alert reached anyone when every configured channel refused.
        // The origin channel is excluded — it already has this message.
        let alert_requested =
            (urgency == "high" || urgency == "critical") && !self.alert_channels.is_empty();
        let alerted_to = if alert_requested {
            self.send_alerts(&text, Some(&channel)).await
        } else {
            Vec::new()
        };

        if wait_for_response {
            // Block and wait for human response (same pattern as ask_user)
            let (tx, mut rx) = tokio::sync::mpsc::channel::<ChannelMessage>(1);
            let timeout = std::time::Duration::from_secs(timeout_secs);

            let listen_channel = Arc::clone(&channel);
            let listen_handle =
                zeroclaw_spawn::spawn!(async move { listen_channel.listen(tx).await });

            let response = tokio::time::timeout(timeout, rx.recv()).await;
            listen_handle.abort();

            match response {
                Ok(Some(msg)) => Ok(ToolResult {
                    success: true,
                    output: msg.content.into(),
                    error: None,
                }),
                Ok(None) => Ok(ToolResult {
                    success: false,
                    output: "TIMEOUT".to_string().into(),
                    error: Some("Channel closed before receiving a response".to_string()),
                }),
                Err(_) => Ok(ToolResult {
                    success: false,
                    output: "TIMEOUT".to_string().into(),
                    error: Some(format!(
                        "No response received within {timeout_secs} seconds"
                    )),
                }),
            }
        } else {
            // Non-blocking: return confirmation. When extra alerting was asked
            // for, say what actually happened to it — an empty list after a
            // high/critical escalation means only the origin channel saw this.
            let mut payload = json!({
                "status": "escalated",
                "urgency": urgency,
                "channel": channel_name,
            });
            if alert_requested {
                payload["alerted_to"] = json!(alerted_to);
                if alerted_to.is_empty() {
                    payload["alert_note"] = json!(
                        "No configured `[escalation] alert_channels` accepted this alert; \
                         only the origin channel received it."
                    );
                }
            }
            Ok(ToolResult {
                success: true,
                output: payload.to_string().into(),
                error: None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::RwLock;
    use std::collections::HashMap;

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

    impl ::zeroclaw_api::attribution::Attributable for SilentChannel {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Channel(
                ::zeroclaw_api::attribution::ChannelKind::Webhook,
            )
        }
        fn alias(&self) -> &str {
            "test"
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

    impl ::zeroclaw_api::attribution::Attributable for RespondingChannel {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Channel(
                ::zeroclaw_api::attribution::ChannelKind::Webhook,
            )
        }
        fn alias(&self) -> &str {
            "test"
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
                channel_alias: None,
                timestamp: 1000,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                subject: None,

                ..Default::default()
            };
            let _ = tx.send(msg).await;
            Ok(())
        }
    }

    fn make_tool_with_channels(channels: Vec<(&str, Arc<dyn Channel>)>) -> EscalateToHumanTool {
        make_tool_with_channels_and_alerts(channels, vec![])
    }

    fn make_tool_with_channels_and_alerts(
        channels: Vec<(&str, Arc<dyn Channel>)>,
        alert_channels: Vec<&str>,
    ) -> EscalateToHumanTool {
        let tool = EscalateToHumanTool::new(
            Arc::new(SecurityPolicy::default()),
            alert_channels.into_iter().map(String::from).collect(),
            Arc::new(RwLock::new(HashMap::new())),
        );
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
        let tool = EscalateToHumanTool::new(
            Arc::new(SecurityPolicy::default()),
            vec![],
            Arc::new(RwLock::new(HashMap::new())),
        );
        assert_eq!(tool.name(), "escalate_to_human");
        assert!(!tool.description().is_empty());
        assert!(tool.description().to_lowercase().contains("escalat"));
    }

    // ── 2. test_parameters_schema ──

    #[test]
    fn test_parameters_schema() {
        let tool = EscalateToHumanTool::new(
            Arc::new(SecurityPolicy::default()),
            vec![],
            Arc::new(RwLock::new(HashMap::new())),
        );
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

    impl ::zeroclaw_api::attribution::Attributable for StructuredOnlyChannel {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Channel(
                ::zeroclaw_api::attribution::ChannelKind::Webhook,
            )
        }
        fn alias(&self) -> &str {
            "test"
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

    /// Stub whose `send` silently no-ops, mirroring `RpcApprovalChannel` /
    /// `WsApprovalChannel`: returns `Ok(())` but renders nothing.
    struct NoDeliveryChannel {
        channel_name: String,
        sent: Arc<RwLock<Vec<String>>>,
    }

    impl NoDeliveryChannel {
        fn new(name: &str) -> Self {
            Self {
                channel_name: name.to_string(),
                sent: Arc::new(RwLock::new(Vec::new())),
            }
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for NoDeliveryChannel {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Channel(
                ::zeroclaw_api::attribution::ChannelKind::Webhook,
            )
        }
        fn alias(&self) -> &str {
            "test"
        }
    }

    #[async_trait]
    impl Channel for NoDeliveryChannel {
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

        fn supports_outbound_send(&self) -> bool {
            false
        }

        fn supports_free_form_ask(&self) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn escalate_uses_requested_channel_not_arbitrary_map_entry() {
        // Regression: escalate_to_human had no `channel` parameter and always
        // used channels.iter().next() (HashMap order), so an escalation could
        // land on an arbitrary configured channel while the operator watched
        // the channel that actually issued the turn.
        let origin = Arc::new(SilentChannel::new("telegram"));
        let other_a = Arc::new(SilentChannel::new("discord"));
        let other_b = Arc::new(SilentChannel::new("slack"));
        let tool = make_tool_with_channels(vec![
            ("discord", Arc::clone(&other_a) as Arc<dyn Channel>),
            ("telegram", Arc::clone(&origin) as Arc<dyn Channel>),
            ("slack", Arc::clone(&other_b) as Arc<dyn Channel>),
        ]);

        let result = tool
            .execute(json!({
                "summary": "Need a human",
                "channel": "telegram",
            }))
            .await
            .unwrap();

        assert!(result.success, "error: {:?}", result.error);
        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["channel"], "telegram");
        assert_eq!(
            origin.sent.read().len(),
            1,
            "escalation must go to the requested channel"
        );
        assert!(
            other_a.sent.read().is_empty() && other_b.sent.read().is_empty(),
            "no other channel may receive the escalation"
        );
    }

    #[tokio::test]
    async fn escalate_reports_unknown_requested_channel() {
        let tool = make_tool_with_channels(vec![(
            "telegram",
            Arc::new(SilentChannel::new("telegram")) as Arc<dyn Channel>,
        )]);

        let result = tool
            .execute(json!({ "summary": "Help", "channel": "nope" }))
            .await;

        // Unknown channel surfaces as an error rather than silently escalating
        // somewhere the caller did not ask for.
        let err = match result {
            Err(e) => e.to_string(),
            Ok(r) => r.error.unwrap_or_default(),
        };
        assert!(
            err.contains("nope"),
            "error should name the missing channel: {err}"
        );
    }

    #[tokio::test]
    async fn escalate_fails_honestly_when_channel_cannot_deliver() {
        // Regression: routing escalations to the originating channel means RPC
        // and WS back-channels are now reachable. Their `send` returns Ok(())
        // without rendering anything, so reporting "escalated" would tell the
        // agent a human was notified when nobody was.
        let ch = Arc::new(NoDeliveryChannel::new("rpc"));
        let tool = make_tool_with_channels(vec![("rpc", Arc::clone(&ch) as Arc<dyn Channel>)]);

        let result = tool
            .execute(json!({ "summary": "Production down", "channel": "rpc" }))
            .await
            .unwrap();

        assert!(
            !result.success,
            "must not claim escalation succeeded on a non-delivering channel"
        );
        let err = result.error.unwrap_or_default();
        assert!(
            err.contains("rpc") && err.contains("deliver"),
            "error should explain the delivery gap: {err}"
        );
        assert!(
            err.contains("alert_channels"),
            "with no alert channels configured, suggesting them is a valid remedy: {err}"
        );
    }

    #[tokio::test]
    async fn undeliverable_origin_falls_back_to_alert_channels() {
        // Review follow-up: the error text suggested configuring
        // `[escalation] alert_channels`, but the guard returned before alerts
        // were ever sent, so that remedy could not change this path. Configured
        // alert channels are now a real fallback.
        let origin = Arc::new(NoDeliveryChannel::new("rpc"));
        let alert = Arc::new(SilentChannel::new("slack"));
        let tool = make_tool_with_channels_and_alerts(
            vec![
                ("rpc", Arc::clone(&origin) as Arc<dyn Channel>),
                ("slack", Arc::clone(&alert) as Arc<dyn Channel>),
            ],
            vec!["slack"],
        );

        let result = tool
            .execute(json!({ "summary": "Production down", "channel": "rpc" }))
            .await
            .unwrap();

        assert!(
            result.success,
            "a delivered alert-channel fallback is a real escalation: {:?}",
            result.error
        );
        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["status"], "escalated_via_alert_channels");
        assert_eq!(parsed["delivered_to"][0], "slack");
        assert_eq!(
            alert.sent.read().len(),
            1,
            "the alert channel must actually receive the escalation"
        );
        assert!(
            origin.sent.read().is_empty(),
            "must not pretend to send on the undeliverable origin channel"
        );
    }

    #[tokio::test]
    async fn undeliverable_origin_fails_when_alert_channels_also_cannot_deliver() {
        // The fallback must not become a new false-success path: an
        // alert_channels list made only of no-op channels delivers nothing.
        let origin = Arc::new(NoDeliveryChannel::new("rpc"));
        let dud = Arc::new(NoDeliveryChannel::new("ws"));
        let tool = make_tool_with_channels_and_alerts(
            vec![
                ("rpc", Arc::clone(&origin) as Arc<dyn Channel>),
                ("ws", Arc::clone(&dud) as Arc<dyn Channel>),
            ],
            vec!["ws"],
        );

        let result = tool
            .execute(json!({ "summary": "Production down", "channel": "rpc" }))
            .await
            .unwrap();

        assert!(
            !result.success,
            "no channel delivered, so this is not an escalation"
        );
        let err = result.error.unwrap_or_default();
        assert!(
            err.contains("could not deliver either"),
            "error must not re-suggest alert_channels that already failed: {err}"
        );
        assert!(
            dud.sent.read().is_empty(),
            "a no-op alert channel must be skipped, not counted as delivered"
        );
    }

    #[tokio::test]
    async fn undeliverable_origin_fallback_rejects_wait_for_response() {
        // Alert channels cannot return a reply into this turn, so a fallback
        // delivery must not be reported as satisfying wait_for_response.
        let origin = Arc::new(NoDeliveryChannel::new("rpc"));
        let alert = Arc::new(SilentChannel::new("slack"));
        let tool = make_tool_with_channels_and_alerts(
            vec![
                ("rpc", Arc::clone(&origin) as Arc<dyn Channel>),
                ("slack", Arc::clone(&alert) as Arc<dyn Channel>),
            ],
            vec!["slack"],
        );

        let started = std::time::Instant::now();
        let result = tool
            .execute(json!({
                "summary": "Need a decision",
                "channel": "rpc",
                "wait_for_response": true,
                "timeout_secs": 30,
            }))
            .await
            .unwrap();
        let elapsed = started.elapsed();

        assert!(!result.success);
        let err = result.error.unwrap_or_default();
        assert!(
            err.contains("wait_for_response"),
            "error should name the unsupported option: {err}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "must fail fast rather than wait for a reply that cannot arrive; took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn escalate_without_channel_still_uses_map_fallback() {
        // Back-compat: callers that omit `channel` (and turns with no
        // originating channel to inject) keep the previous behaviour.
        let tool = make_tool_with_channels(vec![(
            "telegram",
            Arc::new(SilentChannel::new("telegram")) as Arc<dyn Channel>,
        )]);

        let result = tool.execute(json!({ "summary": "Help" })).await.unwrap();

        assert!(result.success, "error: {:?}", result.error);
        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["channel"], "telegram");
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

    #[tokio::test]
    async fn high_urgency_does_not_alert_the_origin_channel_twice() {
        // One channel, reachable under both a bare type key and a dotted
        // alias — the common shape when a type has a single configured
        // instance. Excluding by name alone would still double-send here.
        let origin = Arc::new(SilentChannel::new("webhook"));
        let sent = Arc::clone(&origin.sent);
        let shared: Arc<dyn Channel> = origin;
        let tool = make_tool_with_channels_and_alerts(
            vec![
                ("webhook", Arc::clone(&shared)),
                ("webhook.default", Arc::clone(&shared)),
            ],
            vec!["webhook"],
        );

        let result = tool
            .execute(json!({
                "summary": "Disk is full",
                "urgency": "critical",
                "channel": "webhook.default",
            }))
            .await
            .unwrap();

        assert!(result.success, "error: {:?}", result.error);
        assert_eq!(
            sent.read().len(),
            1,
            "the origin channel must receive the escalation exactly once, not again via the alert fan-out",
        );
        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(
            parsed["alerted_to"],
            json!([]),
            "the origin channel must not be counted as an alert target",
        );
    }

    #[tokio::test]
    async fn alert_fan_out_reports_which_channels_took_it() {
        let origin = Arc::new(SilentChannel::new("origin"));
        let pager = Arc::new(SilentChannel::new("pager"));
        let pager_sent = Arc::clone(&pager.sent);
        let tool = make_tool_with_channels_and_alerts(
            vec![
                ("origin", Arc::clone(&origin) as Arc<dyn Channel>),
                ("pager", Arc::clone(&pager) as Arc<dyn Channel>),
            ],
            vec!["pager"],
        );

        let result = tool
            .execute(json!({
                "summary": "Database unreachable",
                "urgency": "critical",
                "channel": "origin",
            }))
            .await
            .unwrap();

        assert!(result.success, "error: {:?}", result.error);
        assert_eq!(
            pager_sent.read().len(),
            1,
            "the alert channel must be notified"
        );
        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["alerted_to"], json!(["pager"]));
        assert!(
            parsed.get("alert_note").is_none(),
            "no note is warranted when an alert channel accepted the message",
        );
    }

    #[tokio::test]
    async fn alerts_that_reached_nobody_are_reported_not_hidden() {
        // The alert channel is configured but absent from the channel map, so
        // nothing accepts the alert. The escalation still succeeded on the
        // origin channel, so this stays a success — but the model must be able
        // to tell that its high-urgency alerting reached no one.
        let tool = make_tool_with_channels_and_alerts(
            vec![(
                "origin",
                Arc::new(SilentChannel::new("origin")) as Arc<dyn Channel>,
            )],
            vec!["pager-that-is-not-configured"],
        );

        let result = tool
            .execute(json!({
                "summary": "Certificate expires today",
                "urgency": "high",
                "channel": "origin",
            }))
            .await
            .unwrap();

        assert!(result.success, "error: {:?}", result.error);
        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["alerted_to"], json!([]));
        assert!(
            parsed["alert_note"]
                .as_str()
                .is_some_and(|n| n.contains("alert_channels")),
            "an alert that reached nobody must say so, got: {parsed}",
        );
    }

    #[tokio::test]
    async fn medium_urgency_does_not_report_alert_fields() {
        let tool = make_tool_with_channels_and_alerts(
            vec![(
                "origin",
                Arc::new(SilentChannel::new("origin")) as Arc<dyn Channel>,
            )],
            vec!["pager"],
        );

        let result = tool
            .execute(json!({
                "summary": "Routine notice",
                "urgency": "medium",
                "channel": "origin",
            }))
            .await
            .unwrap();

        assert!(result.success, "error: {:?}", result.error);
        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert!(
            parsed.get("alerted_to").is_none() && parsed.get("alert_note").is_none(),
            "alert reporting belongs only to urgencies that actually fan out, got: {parsed}",
        );
    }
}
