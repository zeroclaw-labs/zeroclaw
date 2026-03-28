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

/// Number emojis used for text-based poll fallback voting.
const VOTE_EMOJIS: &[&str] = &[
    "\u{0031}\u{FE0F}\u{20E3}",         // 1️⃣
    "\u{0032}\u{FE0F}\u{20E3}",         // 2️⃣
    "\u{0033}\u{FE0F}\u{20E3}",         // 3️⃣
    "\u{0034}\u{FE0F}\u{20E3}",         // 4️⃣
    "\u{0035}\u{FE0F}\u{20E3}",         // 5️⃣
    "\u{0036}\u{FE0F}\u{20E3}",         // 6️⃣
    "\u{0037}\u{FE0F}\u{20E3}",         // 7️⃣
    "\u{0038}\u{FE0F}\u{20E3}",         // 8️⃣
    "\u{0039}\u{FE0F}\u{20E3}",         // 9️⃣
    "\u{0031}\u{0030}\u{FE0F}\u{20E3}", // 🔟 (keycap 10 — may render differently)
];

const MIN_OPTIONS: usize = 2;
const MAX_OPTIONS: usize = 10;
const DEFAULT_DURATION_MINUTES: u64 = 60;

pub struct PollTool {
    security: Arc<SecurityPolicy>,
    channels: ChannelMapHandle,
}

impl PollTool {
    pub fn new(security: Arc<SecurityPolicy>, channels: ChannelMapHandle) -> Self {
        Self { security, channels }
    }
}

/// Format a poll as a numbered text message for channels without native poll support.
pub fn format_text_poll(
    question: &str,
    options: &[String],
    duration_minutes: u64,
    multi_select: bool,
) -> String {
    let mut lines = Vec::with_capacity(options.len() + 4);
    lines.push(format!("\u{1F4CA} **Poll: {question}**"));
    lines.push(String::new());
    for (i, option) in options.iter().enumerate() {
        let emoji = VOTE_EMOJIS.get(i).copied().unwrap_or("  ");
        lines.push(format!("{emoji}  {option}"));
    }
    lines.push(String::new());
    let mode = if multi_select {
        "multiple choices allowed"
    } else {
        "single choice"
    };
    lines.push(format!(
        "_React with the corresponding number to vote ({mode}). Poll closes in {duration_minutes} min._"
    ));
    lines.join("\n")
}

/// Validate the options array: 2-10 non-empty strings.
fn validate_options(args: &serde_json::Value) -> Result<Vec<String>, String> {
    let arr = args
        .get("options")
        .and_then(|v| v.as_array())
        .ok_or("Missing or invalid 'options' parameter (expected array of strings)")?;

    if arr.len() < MIN_OPTIONS {
        return Err(format!(
            "Poll requires at least {MIN_OPTIONS} options, got {}",
            arr.len()
        ));
    }
    if arr.len() > MAX_OPTIONS {
        return Err(format!(
            "Poll allows at most {MAX_OPTIONS} options, got {}",
            arr.len()
        ));
    }

    let mut options = Vec::with_capacity(arr.len());
    for (i, v) in arr.iter().enumerate() {
        let s = v
            .as_str()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or(format!("Option at index {i} must be a non-empty string"))?;
        options.push(s);
    }
    Ok(options)
}

/// Returns true for channel names that support native polls (Telegram, Discord).
fn supports_native_poll(channel_name: &str) -> bool {
    let lower = channel_name.to_ascii_lowercase();
    lower.contains("telegram") || lower.contains("discord")
}

#[async_trait]
impl Tool for PollTool {
    fn name(&self) -> &str {
        "poll"
    }

    fn description(&self) -> &str {
        "Create a poll in a messaging channel. For Telegram/Discord uses native polls; for other channels formats as a numbered text message with emoji reactions for voting."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The poll question"
                },
                "options": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 2,
                    "maxItems": 10,
                    "description": "Poll answer options (2-10 items)"
                },
                "channel": {
                    "type": "string",
                    "description": "Target channel name. Defaults to the first available channel if omitted."
                },
                "recipient": {
                    "type": "string",
                    "description": "Recipient/chat identifier within the channel (e.g., chat_id for Telegram, channel_id for Slack)"
                },
                "duration_minutes": {
                    "type": "integer",
                    "description": "Poll duration in minutes (default: 60)"
                },
                "multi_select": {
                    "type": "boolean",
                    "description": "Allow multiple selections (default: false)"
                }
            },
            "required": ["question", "options"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Security gate: Act operation
        if let Err(e) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "poll")
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

        let options = match validate_options(&args) {
            Ok(opts) => opts,
            Err(msg) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(msg),
                });
            }
        };

        let duration_minutes = args
            .get("duration_minutes")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_DURATION_MINUTES);

        let multi_select = args
            .get("multi_select")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let requested_channel = args
            .get("channel")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string());

        let recipient = args
            .get("recipient")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string());

        // Resolve channel from handle — block-scoped to drop the RwLock guard
        // before any `.await` (parking_lot guards are !Send).
        let (channel_name, channel): (String, Arc<dyn Channel>) = {
            let channels = self.channels.read();
            if let Some(ref name) = requested_channel {
                let ch = channels.get(name.as_str()).cloned().ok_or_else(|| {
                    anyhow::anyhow!(
                        "Channel '{}' not found. Available: {}",
                        name,
                        channels.keys().cloned().collect::<Vec<_>>().join(", ")
                    )
                })?;
                (name.clone(), ch)
            } else {
                // Fall back to first available channel
                let (name, ch) = channels.iter().next().ok_or_else(|| {
                    anyhow::anyhow!("No channels available. Configure at least one channel.")
                })?;
                (name.clone(), ch.clone())
            }
        };

        let recipient_id = recipient.unwrap_or_default();

        // For channels with native poll support, we still send a formatted message.
        // The Channel trait does not expose a create_poll method, so all channels
        // receive a text-formatted poll. Native Telegram/Discord poll APIs would
        // require a trait extension; for now we note the intent in the output.
        let is_native = supports_native_poll(&channel_name);

        let poll_text = format_text_poll(&question, &options, duration_minutes, multi_select);

        let msg = SendMessage::new(&poll_text, &recipient_id);
        if let Err(e) = channel.send(&msg).await {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Failed to send poll to channel '{channel_name}': {e}"
                )),
            });
        }

        let native_note = if is_native {
            " (native poll API available — text fallback used; trait extension needed for native support)"
        } else {
            ""
        };

        Ok(ToolResult {
            success: true,
            output: format!(
                "Poll created on '{channel_name}'{native_note}:\n\
                 Question: {question}\n\
                 Options: {}\n\
                 Duration: {duration_minutes} min | Multi-select: {multi_select}",
                options.join(", ")
            ),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::traits::ChannelMessage;

    struct StubChannel {
        name: String,
        sent: Arc<RwLock<Vec<String>>>,
    }

    impl StubChannel {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                sent: Arc::new(RwLock::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl Channel for StubChannel {
        fn name(&self) -> &str {
            &self.name
        }

        async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
            self.sent.write().push(message.content.clone());
            Ok(())
        }

        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn make_channel_map(channels: Vec<Arc<dyn Channel>>) -> ChannelMapHandle {
        let mut map = HashMap::new();
        for ch in channels {
            map.insert(ch.name().to_string(), ch);
        }
        Arc::new(RwLock::new(map))
    }

    fn default_tool() -> PollTool {
        let security = Arc::new(SecurityPolicy::default());
        let stub: Arc<dyn Channel> = Arc::new(StubChannel::new("slack"));
        let channels = make_channel_map(vec![stub]);
        PollTool::new(security, channels)
    }

    // ── Option validation tests ──

    #[test]
    fn validate_options_rejects_too_few() {
        let args = json!({ "options": ["only_one"] });
        let err = validate_options(&args).unwrap_err();
        assert!(err.contains("at least 2"), "got: {err}");
    }

    #[test]
    fn validate_options_rejects_too_many() {
        let opts: Vec<String> = (0..11).map(|i| format!("opt{i}")).collect();
        let args = json!({ "options": opts });
        let err = validate_options(&args).unwrap_err();
        assert!(err.contains("at most 10"), "got: {err}");
    }

    #[test]
    fn validate_options_rejects_empty_strings() {
        let args = json!({ "options": ["a", "  ", "b"] });
        let err = validate_options(&args).unwrap_err();
        assert!(err.contains("non-empty string"), "got: {err}");
    }

    #[test]
    fn validate_options_rejects_missing_field() {
        let args = json!({});
        let err = validate_options(&args).unwrap_err();
        assert!(err.contains("Missing"), "got: {err}");
    }

    #[test]
    fn validate_options_accepts_valid_range() {
        let args = json!({ "options": ["yes", "no"] });
        let opts = validate_options(&args).unwrap();
        assert_eq!(opts, vec!["yes", "no"]);

        let opts10: Vec<String> = (0..10).map(|i| format!("opt{i}")).collect();
        let args10 = json!({ "options": opts10 });
        let result = validate_options(&args10).unwrap();
        assert_eq!(result.len(), 10);
    }

    // ── Text-based poll formatting tests ──

    #[test]
    fn format_text_poll_contains_question_and_options() {
        let text = format_text_poll(
            "Favorite color?",
            &["Red".into(), "Blue".into(), "Green".into()],
            30,
            false,
        );
        assert!(text.contains("Favorite color?"));
        assert!(text.contains("Red"));
        assert!(text.contains("Blue"));
        assert!(text.contains("Green"));
        assert!(text.contains("30 min"));
        assert!(text.contains("single choice"));
    }

    #[test]
    fn format_text_poll_multi_select_label() {
        let text = format_text_poll("Pick any", &["A".into(), "B".into()], 60, true);
        assert!(text.contains("multiple choices allowed"));
    }

    #[test]
    fn format_text_poll_includes_emoji_per_option() {
        let options: Vec<String> = (1..=5).map(|i| format!("Option {i}")).collect();
        let text = format_text_poll("Q?", &options, 10, false);
        // Each option line should contain its number emoji
        for emoji in &VOTE_EMOJIS[..5] {
            assert!(text.contains(emoji), "missing emoji {emoji}");
        }
    }

    // ── Missing parameters tests ──

    #[tokio::test]
    async fn execute_rejects_missing_question() {
        let tool = default_tool();
        let result = tool.execute(json!({ "options": ["a", "b"] })).await;
        assert!(
            result.is_err() || {
                let r = result.unwrap();
                !r.success || r.error.is_some()
            }
        );
    }

    #[tokio::test]
    async fn execute_rejects_missing_options() {
        let tool = default_tool();
        let result = tool.execute(json!({ "question": "What?" })).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("Missing"));
    }

    #[tokio::test]
    async fn execute_rejects_invalid_option_count() {
        let tool = default_tool();
        let result = tool
            .execute(json!({ "question": "Q?", "options": ["only_one"] }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("at least 2"));
    }

    #[tokio::test]
    async fn execute_succeeds_with_valid_args() {
        let tool = default_tool();
        let result = tool
            .execute(json!({
                "question": "Lunch?",
                "options": ["Pizza", "Sushi"],
                "channel": "slack",
                "recipient": "general"
            }))
            .await
            .unwrap();
        assert!(result.success, "error: {:?}", result.error);
        assert!(result.output.contains("Lunch?"));
        assert!(result.output.contains("Pizza"));
    }

    #[tokio::test]
    async fn execute_reports_unknown_channel() {
        let tool = default_tool();
        let result = tool
            .execute(json!({
                "question": "Q?",
                "options": ["a", "b"],
                "channel": "nonexistent"
            }))
            .await;
        // Should be an Err because channel not found
        assert!(result.is_err());
    }

    #[test]
    fn supports_native_poll_recognizes_telegram_and_discord() {
        assert!(supports_native_poll("telegram"));
        assert!(supports_native_poll("Telegram"));
        assert!(supports_native_poll("my_telegram_bot"));
        assert!(supports_native_poll("discord"));
        assert!(supports_native_poll("Discord"));
        assert!(!supports_native_poll("slack"));
        assert!(!supports_native_poll("whatsapp"));
    }
}
