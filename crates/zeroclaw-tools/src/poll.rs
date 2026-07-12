use async_trait::async_trait;
use parking_lot::RwLock;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use zeroclaw_api::channel::{Channel, SendMessage};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::policy::ToolOperation;

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

fn duration_minutes_or_default(args: &serde_json::Value) -> u64 {
    args.get("duration_minutes")
        .and_then(|v| v.as_u64())
        .filter(|duration| *duration > 0)
        .unwrap_or(DEFAULT_DURATION_MINUTES)
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
        "Create a poll in a messaging channel. For Telegram/Discord uses native polls; for other channels formats as a numbered text message with emoji reactions for voting. \
On ACP channels that advertise elicitation.form, the tool blocks until the user picks and returns a JSON-encoded result string with keys `question`, `answer` (or `answers` for multi-select), and `channel`; otherwise it returns a human-readable confirmation string."
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
                    "minimum": 1,
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
                output: ToolOutput::default(),
                error: Some(format!("Action blocked: {e}")),
            });
        }

        // Parse required params
        let question = args
            .get("question")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"param": "question"})),
                    "poll: missing question parameter"
                );
                anyhow::Error::msg("Missing 'question' parameter")
            })?
            .to_string();

        let options = match validate_options(&args) {
            Ok(opts) => opts,
            Err(msg) => {
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(msg),
                });
            }
        };

        let duration_minutes = duration_minutes_or_default(&args);

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
                    let available = channels.keys().cloned().collect::<Vec<_>>().join(", ");
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "channel_requested": name,
                                "available": &available,
                            })),
                        "poll: requested channel not found"
                    );
                    anyhow::Error::msg(format!(
                        "Channel '{name}' not found. Available: {available}"
                    ))
                })?;
                (name.clone(), ch)
            } else {
                // Fall back to first available channel
                let (name, ch) = channels.iter().next().ok_or_else(|| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"missing": "channels"})),
                        "poll: no channels configured"
                    );
                    anyhow::Error::msg("No channels available. Configure at least one channel.")
                })?;
                (name.clone(), ch.clone())
            }
        };

        let recipient_id = recipient.unwrap_or_default();

        let interactive_timeout =
            std::time::Duration::from_secs(duration_minutes.saturating_mul(60));

        if multi_select {
            match channel
                .request_multi_choice(&question, &options, 1, options.len(), interactive_timeout)
                .await
            {
                Ok(Some(answers)) => {
                    return Ok(ToolResult {
                        success: true,
                        output: json!({
                            "question": question,
                            "answers": answers,
                            "channel": channel_name,
                        })
                        .to_string()
                        .into(),
                        error: None,
                    });
                }
                Ok(None) => { /* fall through to text-poll fallback */ }
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(format!("Interactive poll failed: {e}")),
                    });
                }
            }
        } else {
            match channel
                .request_choice(&question, &options, interactive_timeout)
                .await
            {
                Ok(Some(answer)) => {
                    return Ok(ToolResult {
                        success: true,
                        output: json!({
                            "question": question,
                            "answer": answer,
                            "channel": channel_name,
                        })
                        .to_string()
                        .into(),
                        error: None,
                    });
                }
                Ok(None) => { /* fall through to text-poll fallback */ }
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(format!("Interactive poll failed: {e}")),
                    });
                }
            }
        }

        let is_native = supports_native_poll(&channel_name);

        let poll_text = format_text_poll(&question, &options, duration_minutes, multi_select);

        let msg = SendMessage::new(&poll_text, &recipient_id);
        if let Err(e) = channel.send(&msg).await {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
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
            )
            .into(),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::channel::ChannelMessage;

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

    impl ::zeroclaw_api::attribution::Attributable for StubChannel {
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
    fn duration_minutes_zero_uses_default() {
        assert_eq!(
            duration_minutes_or_default(&json!({ "duration_minutes": 0 })),
            DEFAULT_DURATION_MINUTES
        );
        assert_eq!(
            duration_minutes_or_default(&json!({ "duration_minutes": 15 })),
            15
        );
    }

    #[tokio::test]
    async fn execute_zero_duration_uses_default_in_sent_text_and_output() {
        let security = Arc::new(SecurityPolicy::default());
        let stub = Arc::new(StubChannel::new("slack"));
        let sent = Arc::clone(&stub.sent);
        let channel: Arc<dyn Channel> = stub;
        let channels = make_channel_map(vec![channel]);
        let tool = PollTool::new(security, channels);

        let result = tool
            .execute(json!({
                "question": "Lunch?",
                "options": ["Pizza", "Sushi"],
                "channel": "slack",
                "recipient": "general",
                "duration_minutes": 0
            }))
            .await
            .unwrap();

        assert!(result.success, "error: {:?}", result.error);
        assert!(result.output.contains("Duration: 60 min"));

        let sent = sent.read();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("Poll closes in 60 min"));
        assert!(!sent[0].contains("Poll closes in 0 min"));
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

    // ── Task 7: ACP elicitation routing tests ──

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Mock channel that simulates an ACP client with `elicitation.form`:
    /// `request_choice` / `request_multi_choice` return a canned `Some(_)`
    /// answer, and `send` increments a counter so tests can assert it was
    /// never invoked (i.e. the formatted-text fallback path was skipped).
    struct ElicitationCapableChannel {
        channel_name: String,
        single_answer: Option<String>,
        multi_answer: Option<Vec<String>>,
        send_calls: Arc<AtomicUsize>,
    }

    impl ElicitationCapableChannel {
        fn single_returns(name: &str, answer: &str) -> Self {
            Self {
                channel_name: name.to_string(),
                single_answer: Some(answer.to_string()),
                multi_answer: None,
                send_calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn multi_returns(name: &str, answers: Vec<String>) -> Self {
            Self {
                channel_name: name.to_string(),
                single_answer: None,
                multi_answer: Some(answers),
                send_calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn send_call_count(&self) -> usize {
            self.send_calls.load(Ordering::SeqCst)
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for ElicitationCapableChannel {
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
    impl Channel for ElicitationCapableChannel {
        fn name(&self) -> &str {
            &self.channel_name
        }

        async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
            self.send_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn request_choice(
            &self,
            _question: &str,
            _choices: &[String],
            _timeout: std::time::Duration,
        ) -> anyhow::Result<Option<String>> {
            Ok(self.single_answer.clone())
        }

        async fn request_multi_choice(
            &self,
            _question: &str,
            _choices: &[String],
            _min_items: usize,
            _max_items: usize,
            _timeout: std::time::Duration,
        ) -> anyhow::Result<Option<Vec<String>>> {
            Ok(self.multi_answer.clone())
        }
    }

    /// Mock channel that simulates ANY non-ACP channel (or a legacy ACP client
    /// without `elicitation.form`): it inherits the default trait impls of
    /// `request_choice` and `request_multi_choice`, both of which return
    /// `Ok(None)`. `send` records that the formatted-text fallback fired.
    struct FallbackOnlyChannel {
        channel_name: String,
        send_calls: Arc<AtomicUsize>,
    }

    impl FallbackOnlyChannel {
        fn new(name: &str) -> Self {
            Self {
                channel_name: name.to_string(),
                send_calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn send_call_count(&self) -> usize {
            self.send_calls.load(Ordering::SeqCst)
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for FallbackOnlyChannel {
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
    impl Channel for FallbackOnlyChannel {
        fn name(&self) -> &str {
            &self.channel_name
        }

        async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
            self.send_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        // request_choice and request_multi_choice intentionally NOT overridden:
        // both inherit the default Ok(None) trait impl.
    }

    #[tokio::test]
    async fn execute_uses_request_choice_when_channel_supports_single() {
        let stub = Arc::new(ElicitationCapableChannel::single_returns("acp", "Option B"));
        let stub_for_assert = Arc::clone(&stub);
        let channel: Arc<dyn Channel> = stub;
        let channels = make_channel_map(vec![channel]);
        let security = Arc::new(SecurityPolicy::default());
        let tool = PollTool::new(security, channels);

        let result = tool
            .execute(json!({
                "question": "Pick one",
                "options": ["Option A", "Option B", "Option C"],
                "channel": "acp",
                "multi_select": false,
            }))
            .await
            .unwrap();

        assert!(result.success, "error: {:?}", result.error);
        let out: serde_json::Value = serde_json::from_str(&result.output)
            .expect("output must be JSON when elicitation hits");
        assert_eq!(out["answer"], "Option B");
        assert_eq!(out["question"], "Pick one");
        assert_eq!(out["channel"], "acp");
        assert_eq!(
            stub_for_assert.send_call_count(),
            0,
            "must not fall back to formatted-text send when elicitation returned a result"
        );
    }

    #[tokio::test]
    async fn execute_uses_request_multi_choice_when_channel_supports_multi() {
        let stub = Arc::new(ElicitationCapableChannel::multi_returns(
            "acp",
            vec!["Red".to_string(), "Blue".to_string()],
        ));
        let stub_for_assert = Arc::clone(&stub);
        let channel: Arc<dyn Channel> = stub;
        let channels = make_channel_map(vec![channel]);
        let security = Arc::new(SecurityPolicy::default());
        let tool = PollTool::new(security, channels);

        let result = tool
            .execute(json!({
                "question": "Pick colors",
                "options": ["Red", "Green", "Blue"],
                "channel": "acp",
                "multi_select": true,
            }))
            .await
            .unwrap();

        assert!(result.success, "error: {:?}", result.error);
        let out: serde_json::Value = serde_json::from_str(&result.output)
            .expect("output must be JSON when elicitation hits");
        assert_eq!(out["answers"], json!(["Red", "Blue"]));
        assert_eq!(out["question"], "Pick colors");
        assert_eq!(out["channel"], "acp");
        assert_eq!(
            stub_for_assert.send_call_count(),
            0,
            "must not fall back to formatted-text send when elicitation returned a result"
        );
    }

    #[tokio::test]
    async fn execute_falls_back_to_formatted_text_when_channel_returns_none() {
        let stub = Arc::new(FallbackOnlyChannel::new("slack"));
        let stub_for_assert = Arc::clone(&stub);
        let channel: Arc<dyn Channel> = stub;
        let channels = make_channel_map(vec![channel]);
        let security = Arc::new(SecurityPolicy::default());
        let tool = PollTool::new(security, channels);

        let result = tool
            .execute(json!({
                "question": "Pick one",
                "options": ["A", "B"],
                "channel": "slack",
                "multi_select": false,
            }))
            .await
            .unwrap();

        assert!(result.success, "error: {:?}", result.error);
        assert_eq!(
            stub_for_assert.send_call_count(),
            1,
            "formatted-text fallback must fire when channel returns Ok(None)"
        );
        // Existing pre-task-7 success path renders a string like
        // "Poll created on '...':\nQuestion: ...". Match that shape.
        assert!(
            result.output.contains("Poll created on"),
            "unexpected fallback output: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn execute_with_huge_duration_does_not_panic() {
        // duration_minutes * 60 used to overflow u64 for very large inputs.
        // saturating_mul keeps the value at u64::MAX, which Duration::from_secs
        // accepts. We don't care what timeout the channel sees — only that the
        // tool returns rather than panicking.
        let stub = Arc::new(FallbackOnlyChannel::new("noop"));
        let stub_for_assert = Arc::clone(&stub);
        let channel: Arc<dyn Channel> = stub;
        let channels = make_channel_map(vec![channel]);
        let security = Arc::new(SecurityPolicy::default());
        let tool = PollTool::new(security, channels);

        let result = tool
            .execute(json!({
                "question": "Pick one",
                "options": ["A", "B"],
                "channel": "noop",
                "multi_select": false,
                "duration_minutes": u64::MAX,
            }))
            .await
            .unwrap();
        assert!(result.success);
        // Took the fallback path (no overflow during interactive_timeout calc).
        assert_eq!(stub_for_assert.send_call_count(), 1);
    }
}
