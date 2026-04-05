//! Interactive user prompting tool for cross-channel confirmations.
//!
//! Exposes `ask_user` as an agent-callable tool that sends a question to a
//! messaging channel and waits for the user's response. The tool holds a
//! late-binding channel map handle that is populated once channels are
//! initialized (after tool construction). This mirrors the pattern used by
//! [`ReactionTool`](super::reaction::ReactionTool).

use super::traits::{Tool, ToolResult};
use crate::channels::traits::{Channel, ChannelMessage, SendMessage};
use crate::security::SecurityPolicy;
use crate::security::policy::ToolOperation;
use async_trait::async_trait;
use parking_lot::RwLock;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

/// Shared handle giving tools late-bound access to the live channel map.
pub type ChannelMapHandle = Arc<RwLock<HashMap<String, Arc<dyn Channel>>>>;

/// Default timeout in seconds when waiting for a user response.
const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Agent-callable tool for sending a question to a user and waiting for their response.
pub struct AskUserTool {
    security: Arc<SecurityPolicy>,
    channels: ChannelMapHandle,
}

impl AskUserTool {
    /// Create a new ask_user tool with an empty channel map.
    /// Call [`channel_map_handle`] and write to the returned handle once channels
    /// are available.
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self {
            security,
            channels: Arc::new(RwLock::new(HashMap::new())),
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

/// Format a question with optional choices for display.
fn format_question(question: &str, choices: Option<&[String]>) -> String {
    let mut lines = Vec::new();
    lines.push(format!("**{question}**"));

    if let Some(choices) = choices {
        lines.push(String::new());
        for (i, choice) in choices.iter().enumerate() {
            lines.push(format!("{}. {choice}", i + 1));
        }
        lines.push(String::new());
        lines.push("_Reply with a number or type your answer._".to_string());
    }

    lines.join("\n")
}

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn description(&self) -> &str {
        "Ask the user a question and wait for their response. \
         Sends the question to a messaging channel and blocks until the user replies \
         or the timeout expires. Optionally provide choices for structured responses."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The question to ask the user"
                },
                "choices": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional list of choices (renders as buttons on Telegram, numbered list on CLI)"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Seconds to wait for a response (default: 300)"
                },
                "channel": {
                    "type": "string",
                    "description": "Target channel name. Defaults to the first available channel if omitted."
                }
            },
            "required": ["question"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Security gate: Act operation
        if let Err(e) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "ask_user")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Action blocked: {e}")),
            });
        }

        // Parse required params
        let question = args
            .get("question")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing 'question' parameter"))?
            .to_string();

        let choices: Option<Vec<String>> = args.get("choices").and_then(|v| {
            v.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|item| item.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
        });

        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        let requested_channel = args
            .get("channel")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string());

        // Resolve channel from handle — block-scoped to drop the RwLock guard
        // before any `.await` (parking_lot guards are !Send).
        let (channel_name, channel): (String, Arc<dyn Channel>) = {
            let channels = self.channels.read();
            if channels.is_empty() {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("No channels available yet (channels not initialized)".to_string()),
                });
            }
            if let Some(ref name) = requested_channel {
                let ch = channels.get(name.as_str()).cloned().ok_or_else(|| {
                    let available: Vec<String> = channels.keys().cloned().collect();
                    anyhow::anyhow!(
                        "Channel '{}' not found. Available: {}",
                        name,
                        available.join(", ")
                    )
                })?;
                (name.clone(), ch)
            } else {
                let (name, ch) = channels.iter().next().ok_or_else(|| {
                    anyhow::anyhow!("No channels available. Configure at least one channel.")
                })?;
                (name.clone(), ch.clone())
            }
        };

        // Format and send the question
        let text = format_question(&question, choices.as_deref());
        let msg = SendMessage::new(&text, "");
        if let Err(e) = channel.send(&msg).await {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Failed to send question to channel '{channel_name}': {e}"
                )),
            });
        }

        // Listen for user response with timeout
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ChannelMessage>(1);
        let timeout = std::time::Duration::from_secs(timeout_secs);

        // Spawn a listener task on the channel
        let listen_channel = Arc::clone(&channel);
        let listen_handle = tokio::spawn(async move { listen_channel.listen(tx).await });

        let response = tokio::time::timeout(timeout, rx.recv()).await;

        // Abort the listener once we have a response or timeout
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
                sender: "user".to_string(),
                reply_target: "user".to_string(),
                content: self.response.clone(),
                channel: self.channel_name.clone(),
                timestamp: 1000,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: vec![],
                is_dm: true,
            };
            let _ = tx.send(msg).await;
            Ok(())
        }
    }

    fn make_tool_with_channels(channels: Vec<(&str, Arc<dyn Channel>)>) -> AskUserTool {
        let tool = AskUserTool::new(Arc::new(SecurityPolicy::default()));
        let map: HashMap<String, Arc<dyn Channel>> = channels
            .into_iter()
            .map(|(name, ch)| (name.to_string(), ch))
            .collect();
        tool.populate(map);
        tool
    }

    // ── Metadata tests ──

    #[test]
    fn tool_name_and_description() {
        let tool = AskUserTool::new(Arc::new(SecurityPolicy::default()));
        assert_eq!(tool.name(), "ask_user");
        assert!(!tool.description().is_empty());
        assert!(tool.description().contains("question"));
    }

    #[test]
    fn parameter_schema_validation() {
        let tool = AskUserTool::new(Arc::new(SecurityPolicy::default()));
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["question"].is_object());
        assert!(schema["properties"]["choices"].is_object());
        assert!(schema["properties"]["timeout_secs"].is_object());
        assert!(schema["properties"]["channel"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "question"));
        // choices, timeout_secs, channel are optional
        assert!(!required.iter().any(|v| v == "choices"));
        assert!(!required.iter().any(|v| v == "timeout_secs"));
        assert!(!required.iter().any(|v| v == "channel"));
    }

    #[test]
    fn spec_matches_metadata() {
        let tool = AskUserTool::new(Arc::new(SecurityPolicy::default()));
        let spec = tool.spec();
        assert_eq!(spec.name, "ask_user");
        assert_eq!(spec.description, tool.description());
        assert!(spec.parameters["required"].is_array());
    }

    // ── Format question tests ──

    #[test]
    fn format_question_without_choices() {
        let text = format_question("Are you sure?", None);
        assert!(text.contains("Are you sure?"));
        assert!(!text.contains("1."));
    }

    #[test]
    fn format_question_with_choices() {
        let choices = vec!["Yes".to_string(), "No".to_string(), "Maybe".to_string()];
        let text = format_question("Continue?", Some(&choices));
        assert!(text.contains("Continue?"));
        assert!(text.contains("1. Yes"));
        assert!(text.contains("2. No"));
        assert!(text.contains("3. Maybe"));
        assert!(text.contains("Reply with a number"));
    }

    // ── Execute tests ──

    #[tokio::test]
    async fn execute_rejects_missing_question() {
        let tool = make_tool_with_channels(vec![(
            "test",
            Arc::new(SilentChannel::new("test")) as Arc<dyn Channel>,
        )]);
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_rejects_empty_question() {
        let tool = make_tool_with_channels(vec![(
            "test",
            Arc::new(SilentChannel::new("test")) as Arc<dyn Channel>,
        )]);
        let result = tool.execute(json!({ "question": "  " })).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn empty_channels_returns_not_initialized() {
        let tool = AskUserTool::new(Arc::new(SecurityPolicy::default()));
        let result = tool.execute(json!({ "question": "Hello?" })).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("not initialized"));
    }

    #[tokio::test]
    async fn unknown_channel_returns_error() {
        let tool = make_tool_with_channels(vec![(
            "slack",
            Arc::new(SilentChannel::new("slack")) as Arc<dyn Channel>,
        )]);
        let result = tool
            .execute(json!({ "question": "Hello?", "channel": "nonexistent" }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn timeout_returns_timeout_output() {
        let tool = make_tool_with_channels(vec![(
            "test",
            Arc::new(SilentChannel::new("test")) as Arc<dyn Channel>,
        )]);
        let result = tool
            .execute(json!({
                "question": "Confirm?",
                "timeout_secs": 1
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert_eq!(result.output, "TIMEOUT");
        assert!(result.error.as_deref().unwrap().contains("1 seconds"));
    }

    #[tokio::test]
    async fn successful_response_flow() {
        let tool = make_tool_with_channels(vec![(
            "test",
            Arc::new(RespondingChannel::new("test", "Yes, proceed!")) as Arc<dyn Channel>,
        )]);
        let result = tool
            .execute(json!({
                "question": "Should we deploy?",
                "timeout_secs": 5
            }))
            .await
            .unwrap();
        assert!(result.success, "error: {:?}", result.error);
        assert_eq!(result.output, "Yes, proceed!");
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn successful_response_with_choices() {
        let tool = make_tool_with_channels(vec![(
            "telegram",
            Arc::new(RespondingChannel::new("telegram", "2")) as Arc<dyn Channel>,
        )]);
        let result = tool
            .execute(json!({
                "question": "Pick an option",
                "choices": ["Option A", "Option B"],
                "channel": "telegram",
                "timeout_secs": 5
            }))
            .await
            .unwrap();
        assert!(result.success, "error: {:?}", result.error);
        assert_eq!(result.output, "2");
    }

    #[tokio::test]
    async fn channel_map_handle_allows_late_binding() {
        let tool = AskUserTool::new(Arc::new(SecurityPolicy::default()));
        let handle = tool.channel_map_handle();

        // Initially empty — tool reports not initialized
        let result = tool.execute(json!({ "question": "Hello?" })).await.unwrap();
        assert!(!result.success);

        // Populate via the handle
        {
            let mut map = handle.write();
            map.insert(
                "cli".to_string(),
                Arc::new(RespondingChannel::new("cli", "ok")) as Arc<dyn Channel>,
            );
        }

        // Now the tool can route to the channel
        let result = tool
            .execute(json!({ "question": "Hello?", "timeout_secs": 5 }))
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "ok");
    }
}
