//! Interactive user prompting tool for cross-channel confirmations.

use async_trait::async_trait;
use parking_lot::RwLock;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use zeroclaw_api::channel::{Channel, ChannelChoiceOutcome, ChannelMessage, SendMessage};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::policy::ToolOperation;

/// Shared handle giving tools late-bound access to the live channel map.
pub type ChannelMapHandle = Arc<RwLock<HashMap<String, Arc<dyn Channel>>>>;

/// Default timeout in seconds when waiting for a user response.
const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Agent-callable tool for sending a question to a user and waiting for their response.
pub struct AskUserTool {
    security: Arc<SecurityPolicy>,
    channels: ChannelMapHandle,
}

/// Own the receiver task so cancelling the tool cannot detach its listener.
struct QuestionListener(tokio::task::JoinHandle<anyhow::Result<()>>);

enum QuestionReply {
    Message(Box<ChannelMessage>),
    ListenerFinished(Result<anyhow::Result<()>, tokio::task::JoinError>),
}

impl Drop for QuestionListener {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl AskUserTool {
    /// Create a new ask_user tool using the given channel map.
    pub fn new(security: Arc<SecurityPolicy>, channels: ChannelMapHandle) -> Self {
        Self { security, channels }
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
                    "minimum": 1,
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
                    "ask_user: missing question parameter"
                );
                anyhow::Error::msg("Missing 'question' parameter")
            })?
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
        if timeout_secs == 0 {
            return Ok(ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(crate::i18n::get_required_tool_string(
                    "tool-ask-user-invalid-timeout",
                )),
            });
        }

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
                    output: ToolOutput::default(),
                    error: Some("No channels available yet (channels not initialized)".to_string()),
                });
            }
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
                        "ask_user: requested channel not found"
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
                        "ask_user: no channels configured"
                    );
                    anyhow::Error::msg("No channels available. Configure at least one channel.")
                })?;
                (name.clone(), ch.clone())
            }
        };

        let history = zeroclaw_api::session_events::capture::current_session_execution();
        let request_id = zeroclaw_api::session_events::EventId::new().to_string();
        if let Some(port) = &history {
            let millis = i64::try_from(timeout_secs)
                .ok()
                .and_then(|seconds| seconds.checked_mul(1000))
                .and_then(|millis| chrono::Utc::now().timestamp_millis().checked_add(millis))
                .ok_or_else(|| anyhow::Error::msg("elicitation_deadline_overflow"))?;
            let schema = choices
                .as_ref()
                .filter(|choices| !choices.is_empty())
                .map_or_else(
                    || json!({"type":"string"}),
                    |choices| json!({"type":"string","enum":choices}),
                );
            port.request_elicitation(&request_id, &question, &schema, millis)
                .await
                .map_err(zeroclaw_api::session_events::capture::capture_stopped)?;
        }
        let mut resolution_state = "unanswered";
        let mut resolution_error_kind = None;
        let result: anyhow::Result<ToolResult> = async {
            let timeout = std::time::Duration::from_secs(timeout_secs);

            // Prefer the channel's native structured-choice flow when choices are
            // present (e.g. ACP `session/request_permission` / `elicitation/create`,
            // Telegram inline keyboard).
            //
            // Only an unavailable native response on a free-form-capable
            // channel may take the send+listen fallback. Explicit refusal,
            // cancellation and timeout retain their observed state.
            // Structured-only channels (ACP, RPC Code tab, WS approval) must fail
            // fast: their `listen` never delivers a reply, so fallthrough either
            // hangs until timeout or returns a misleading "channel closed" error.
            if let Some(ref choices_vec) = choices
                && !choices_vec.is_empty()
            {
                match channel
                    .request_choice_outcome(&question, choices_vec, timeout)
                    .await
                {
                    Ok(ChannelChoiceOutcome::Answered(answer)) => {
                        return Ok(ToolResult {
                            success: true,
                            output: answer.into(),
                            error: None,
                        });
                    }
                    Ok(ChannelChoiceOutcome::Unavailable) if channel.supports_free_form_ask() => {
                        /* fall through to send+listen */
                    }
                    Ok(outcome) => {
                        resolution_state = match outcome {
                            ChannelChoiceOutcome::Declined => "declined",
                            ChannelChoiceOutcome::Cancelled => "cancelled",
                            ChannelChoiceOutcome::TimedOut => "timed_out",
                            ChannelChoiceOutcome::Unavailable => "unavailable",
                            ChannelChoiceOutcome::Answered(_) => {
                                unreachable!("answered choice returned above")
                            }
                        };
                        return Ok(ToolResult {
                            success: false,
                            output: ToolOutput::default(),
                            error: Some(crate::i18n::get_required_tool_string_with_args(
                                "tool-ask-user-choice-outcome",
                                &[("channel", &channel_name), ("state", resolution_state)],
                            )),
                        });
                    }
                    Err(e) => {
                        resolution_state = "failed";
                        return Ok(ToolResult {
                            success: false,
                            output: ToolOutput::default(),
                            error: Some(format!(
                                "Failed to ask question on channel '{channel_name}': {e}"
                            )),
                        });
                    }
                }
            } else if !channel.supports_free_form_ask() {
                resolution_state = "unavailable";
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!(
                        "Channel '{channel_name}' requires `choices` for ask_user \
                     (free-form questions await ACP elicitation Phase 2)"
                    )),
                });
            }

            // Format and send the question
            let text = format_question(&question, choices.as_deref());
            let msg = SendMessage::new(&text, "");
            if let Err(e) = channel.send(&msg).await {
                resolution_state = "failed";
                return Ok(ToolResult {
                    success: false,
                    output: ToolOutput::default(),
                    error: Some(format!(
                        "Failed to send question to channel '{channel_name}': {e}"
                    )),
                });
            }

            // Listen for user response with timeout
            let (tx, mut rx) = tokio::sync::mpsc::channel::<ChannelMessage>(1);

            // Spawn a listener task on the channel
            let listen_channel = Arc::clone(&channel);
            let mut listen_handle = QuestionListener(zeroclaw_spawn::spawn!(async move {
                listen_channel.listen(tx).await
            }));

            let response = tokio::time::timeout(timeout, async {
                tokio::select! {
                    biased;
                    message = rx.recv() => match message {
                        Some(message) => QuestionReply::Message(Box::new(message)),
                        None => QuestionReply::ListenerFinished((&mut listen_handle.0).await),
                    },
                    completion = &mut listen_handle.0 => match rx.try_recv() {
                        Ok(message) => QuestionReply::Message(Box::new(message)),
                        Err(_) => QuestionReply::ListenerFinished(completion),
                    },
                }
            })
            .await;

            // Abort the listener once we have a response or timeout
            drop(listen_handle);

            match response {
                Ok(QuestionReply::Message(msg)) => Ok(ToolResult {
                    success: true,
                    output: msg.content.into(),
                    error: None,
                }),
                Ok(QuestionReply::ListenerFinished(Ok(Ok(())))) => {
                    resolution_state = "channel_closed";
                    Ok(ToolResult {
                        success: false,
                        output: "TIMEOUT".to_string().into(),
                        error: Some("Channel closed before receiving a response".to_string()),
                    })
                }
                Ok(QuestionReply::ListenerFinished(completion)) => {
                    let (kind, diagnostic) = match completion {
                        Ok(Err(error)) => ("listener_error", format!("{error:#}")),
                        Err(error) => (
                            if error.is_panic() {
                                "listener_panic"
                            } else {
                                "listener_cancelled"
                            },
                            error.to_string(),
                        ),
                        Ok(Ok(())) => unreachable!("successful listener completion handled above"),
                    };
                    resolution_state = "failed";
                    resolution_error_kind = Some(kind);
                    let diagnostic = diagnostic.to_owned();
                    Ok(ToolResult {
                        success: false,
                        output: ToolOutput::default(),
                        error: Some(crate::i18n::get_required_tool_string_with_args(
                            "tool-ask-user-listener-failed",
                            &[("channel", &channel_name), ("error", &diagnostic)],
                        )),
                    })
                }
                Err(_) => {
                    resolution_state = "timed_out";
                    Ok(ToolResult {
                        success: false,
                        output: "TIMEOUT".to_string().into(),
                        error: Some(format!(
                            "No response received within {timeout_secs} seconds"
                        )),
                    })
                }
            }
        }
        .await;
        if let Some(port) = &history {
            let mut resolution = match &result {
                Ok(value) if value.success => {
                    json!({"state":"answered","answer":value.output.as_str()})
                }
                Ok(value) => json!({"state":resolution_state,"diagnostic":value.error}),
                Err(error) => {
                    json!({"state":"failed","diagnostic":format!("{error:#}")})
                }
            };
            if let Some(kind) = resolution_error_kind {
                resolution["error_kind"] = json!(kind);
            }
            port.resolve_elicitation(&request_id, &resolution)
                .await
                .map_err(zeroclaw_api::session_events::capture::capture_stopped)?;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stub channel that records sent messages but never produces incoming messages.
    struct SilentChannel {
        channel_name: String,
        sent: Arc<RwLock<Vec<String>>>,
        listen_started: tokio::sync::Notify,
        listen_stopped: tokio::sync::Notify,
    }

    impl SilentChannel {
        fn new(name: &str) -> Self {
            Self {
                channel_name: name.to_string(),
                sent: Arc::new(RwLock::new(Vec::new())),
                listen_started: tokio::sync::Notify::new(),
                listen_stopped: tokio::sync::Notify::new(),
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
            struct Stopped<'a>(&'a tokio::sync::Notify);
            impl Drop for Stopped<'_> {
                fn drop(&mut self) {
                    self.0.notify_one();
                }
            }
            let _stopped = Stopped(&self.listen_stopped);
            self.listen_started.notify_one();
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
                sender: "user".to_string(),
                reply_target: "user".to_string(),
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

    fn make_tool_with_channels(channels: Vec<(&str, Arc<dyn Channel>)>) -> AskUserTool {
        let handle = Arc::new(RwLock::new(HashMap::new()));
        {
            let mut map = handle.write();
            for (name, ch) in channels {
                map.insert(name.to_string(), ch);
            }
        }
        AskUserTool::new(Arc::new(SecurityPolicy::default()), handle)
    }

    // ── Metadata tests ──

    #[test]
    fn tool_name_and_description() {
        let tool = AskUserTool::new(
            Arc::new(SecurityPolicy::default()),
            Arc::new(RwLock::new(HashMap::new())),
        );
        assert_eq!(tool.name(), "ask_user");
        assert!(!tool.description().is_empty());
        assert!(tool.description().contains("question"));
    }

    #[test]
    fn parameter_schema_validation() {
        let tool = AskUserTool::new(
            Arc::new(SecurityPolicy::default()),
            Arc::new(RwLock::new(HashMap::new())),
        );
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
        let tool = AskUserTool::new(
            Arc::new(SecurityPolicy::default()),
            Arc::new(RwLock::new(HashMap::new())),
        );
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
        let tool = AskUserTool::new(
            Arc::new(SecurityPolicy::default()),
            Arc::new(RwLock::new(HashMap::new())),
        );
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
        let handle = Arc::new(RwLock::new(HashMap::new()));
        let tool = AskUserTool::new(Arc::new(SecurityPolicy::default()), handle.clone());

        // Initially empty — tool reports not initialized
        let result = tool.execute(json!({ "question": "Hello?" })).await.unwrap();
        assert!(!result.success);

        // Populate via the shared handle
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

    /// ACP/RPC-shaped channel: structured `request_choice` returns `Ok(None)`
    /// (user cancelled/declined, or no form capability), free-form is unsupported,
    /// and `listen` exits immediately without delivering a message. Falling
    /// through to send+listen would hang until `timeout_secs`.
    struct StructuredOnlyChannel {
        channel_name: String,
        choice_result: parking_lot::Mutex<Option<Result<Option<String>, String>>>,
        choice_outcome: Option<ChannelChoiceOutcome>,
        listen_called: parking_lot::Mutex<bool>,
    }

    impl StructuredOnlyChannel {
        fn new(name: &str, choice_result: Result<Option<String>, String>) -> Self {
            Self {
                channel_name: name.to_string(),
                choice_result: parking_lot::Mutex::new(Some(choice_result)),
                choice_outcome: None,
                listen_called: parking_lot::Mutex::new(false),
            }
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for StructuredOnlyChannel {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Channel(
                ::zeroclaw_api::attribution::ChannelKind::AcpChannel,
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

        async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
            Ok(())
        }

        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            *self.listen_called.lock() = true;
            // Mirrors RpcApprovalChannel / AcpChannel: listen ends without a
            // message. If ask_user falls through here, rx.recv() hangs.
            Ok(())
        }

        fn supports_free_form_ask(&self) -> bool {
            false
        }

        async fn request_choice(
            &self,
            _question: &str,
            _choices: &[String],
            _timeout: std::time::Duration,
        ) -> anyhow::Result<Option<String>> {
            match self.choice_result.lock().take() {
                Some(Ok(v)) => Ok(v),
                Some(Err(e)) => Err(anyhow::Error::msg(e)),
                None => Ok(None),
            }
        }

        async fn request_choice_outcome(
            &self,
            question: &str,
            choices: &[String],
            timeout: std::time::Duration,
        ) -> anyhow::Result<ChannelChoiceOutcome> {
            if let Some(outcome) = &self.choice_outcome {
                return Ok(outcome.clone());
            }
            Ok(
                match self.request_choice(question, choices, timeout).await? {
                    Some(answer) => ChannelChoiceOutcome::Answered(answer),
                    None => ChannelChoiceOutcome::Unavailable,
                },
            )
        }
    }

    #[tokio::test]
    async fn request_choice_none_on_structured_only_channel_fails_fast() {
        // Regression: Ok(None) from request_choice used to fall through to
        // send+listen. On ACP/RPC channels listen never delivers a reply, so
        // the tool hung until timeout (default 300s) and the session had to
        // be cancelled manually.
        let ch = Arc::new(StructuredOnlyChannel::new("rpc", Ok(None)));
        let tool = make_tool_with_channels(vec![("rpc", Arc::clone(&ch) as Arc<dyn Channel>)]);

        let started = std::time::Instant::now();
        let result = tool
            .execute(json!({
                "question": "How should users set a name?",
                "choices": ["Slash only", "Slash + picker", "Something else"],
                "timeout_secs": 30
            }))
            .await
            .unwrap();
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "must fail fast, not wait on listen; elapsed={elapsed:?}"
        );
        assert!(
            !result.success,
            "expected failure, got success: {:?}",
            result.output
        );
        let err = result.error.as_deref().unwrap_or("");
        assert!(
            err.contains("rpc")
                || err.contains("choices")
                || err.contains("free-form")
                || err.contains("declined")
                || err.contains("cancelled")
                || err.contains("cancel"),
            "error should explain structured-only failure; got: {err}"
        );
        assert!(
            !*ch.listen_called.lock(),
            "must not fall through to listen on a structured-only channel"
        );
    }
    #[derive(Default)]
    struct ElicitationCapture {
        calls: std::sync::Mutex<Vec<serde_json::Value>>,
        fail_request: bool,
        fail_resolution: bool,
    }

    #[tokio::test]
    async fn zero_timeout_is_an_ordinary_parameter_failure_before_capture() {
        let channel = Arc::new(RespondingChannel::new("test", "answer"));
        let tool = make_tool_with_channels(vec![("test", channel.clone())]);
        let capture = Arc::new(ElicitationCapture::default());
        let result = zeroclaw_api::session_events::capture::scope_session_execution(
            capture.clone(),
            tool.execute(json!({"question":"Question", "timeout_secs":0})),
        )
        .await
        .expect("parameter failures must not stop native capture");
        assert!(!result.success);
        assert_eq!(
            tool.parameters_schema()["properties"]["timeout_secs"]["minimum"],
            1
        );
        assert!(capture.calls.lock().unwrap().is_empty());
        assert!(channel.sent.read().is_empty());
    }

    #[tokio::test]
    async fn cancelling_ask_user_drops_its_free_form_listener() {
        let channel = Arc::new(SilentChannel::new("test"));
        let tool = make_tool_with_channels(vec![("test", channel.clone())]);
        let capture = Arc::new(ElicitationCapture::default());
        let task_capture = capture.clone();
        let task = zeroclaw_spawn::spawn!(async move {
            zeroclaw_api::session_events::capture::scope_session_execution(
                task_capture,
                tool.execute(json!({"question":"Question", "timeout_secs":60})),
            )
            .await
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            channel.listen_started.notified(),
        )
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            channel.listen_stopped.notified(),
        )
        .await
        .expect("cancelled tool must abort its spawned listener");
        assert_eq!(
            capture.calls.lock().unwrap().len(),
            1,
            "owner settlement resolves the interrupted request"
        );
    }

    #[tokio::test]
    async fn actual_tool_records_distinct_structured_choice_outcomes() {
        for (outcome, state) in [
            (ChannelChoiceOutcome::Declined, "declined"),
            (ChannelChoiceOutcome::Cancelled, "cancelled"),
            (ChannelChoiceOutcome::TimedOut, "timed_out"),
            (ChannelChoiceOutcome::Unavailable, "unavailable"),
        ] {
            let mut channel = StructuredOnlyChannel::new("native", Ok(None));
            channel.choice_outcome = Some(outcome);
            let channel = Arc::new(channel);
            let tool = make_tool_with_channels(vec![("native", channel.clone())]);
            let capture = Arc::new(ElicitationCapture::default());
            let result = zeroclaw_api::session_events::capture::scope_session_execution(
                capture.clone(),
                tool.execute(json!({"question":"Question", "choices":["A"], "timeout_secs":5})),
            )
            .await
            .unwrap();
            assert!(!result.success);
            assert!(!*channel.listen_called.lock());
            let calls = capture.calls.lock().unwrap();
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[1]["resolution"]["state"], state);
        }
    }

    #[tokio::test]
    async fn listener_failures_keep_diagnostics_without_losing_queued_answers() {
        struct FailingListener {
            answer_first: bool,
            panic: bool,
        }
        impl zeroclaw_api::attribution::Attributable for FailingListener {
            fn role(&self) -> zeroclaw_api::attribution::Role {
                zeroclaw_api::attribution::Role::Channel(
                    zeroclaw_api::attribution::ChannelKind::Webhook,
                )
            }
            fn alias(&self) -> &str {
                "test"
            }
        }
        #[async_trait]
        impl Channel for FailingListener {
            fn name(&self) -> &str {
                "test"
            }
            async fn send(&self, _: &SendMessage) -> anyhow::Result<()> {
                Ok(())
            }
            async fn listen(
                &self,
                tx: tokio::sync::mpsc::Sender<ChannelMessage>,
            ) -> anyhow::Result<()> {
                if self.answer_first {
                    tx.send(ChannelMessage {
                        content: "queued answer".into(),
                        ..Default::default()
                    })
                    .await?;
                }
                if self.panic {
                    panic!("fixture listener panic sk-fixturelistenercredential123456789");
                }
                Err(anyhow::Error::msg(
                    "fixture source error sk-fixturelistenercredential123456789",
                )
                .context("fixture listener failure"))
            }
        }
        for panic in [false, true] {
            for answer_first in [false, true] {
                let tool = make_tool_with_channels(vec![(
                    "test",
                    Arc::new(FailingListener {
                        answer_first,
                        panic,
                    }),
                )]);
                let capture = Arc::new(ElicitationCapture::default());
                let result = zeroclaw_api::session_events::capture::scope_session_execution(
                    capture.clone(),
                    tool.execute(json!({"question":"Question", "timeout_secs":5})),
                )
                .await
                .unwrap();
                let calls = capture.calls.lock().unwrap();
                assert_eq!(calls.len(), 2);
                let resolution = &calls[1]["resolution"];
                if answer_first {
                    assert!(result.success);
                    assert_eq!(result.output.as_str(), "queued answer");
                    assert_eq!(resolution["state"], "answered");
                    assert!(resolution.get("error_kind").is_none());
                } else {
                    assert!(!result.success);
                    assert_eq!(resolution["state"], "failed");
                    assert_eq!(
                        resolution["error_kind"],
                        if panic {
                            "listener_panic"
                        } else {
                            "listener_error"
                        }
                    );
                    let diagnostic = resolution["diagnostic"].as_str().unwrap();
                    assert!(diagnostic.contains("fixture listener"));
                    if !panic {
                        assert!(diagnostic.contains("fixture source error"));
                    }
                    assert!(diagnostic.contains("fixturelistenercredential"));
                    assert!(result.error.unwrap().contains("fixturelistenercredential"));
                }
            }
        }
    }
    #[async_trait]
    impl zeroclaw_api::session_events::capture::SessionExecutionPort for ElicitationCapture {
        async fn execute(
            &self,
            _: zeroclaw_api::session_events::capture::EngineTurnInput,
            _: zeroclaw_api::session_events::capture::EngineTurnFuture<'_>,
        ) -> anyhow::Result<String> {
            anyhow::bail!("unused fixture execution")
        }
        async fn request_elicitation(
            &self,
            id: &str,
            prompt: &str,
            schema: &serde_json::Value,
            expires_at: i64,
        ) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(
                json!({"request_id":id,"prompt":prompt,"schema":schema,"expires_at":expires_at}),
            );
            anyhow::ensure!(!self.fail_request, "fixture request commit unavailable");
            Ok(())
        }
        async fn resolve_elicitation(
            &self,
            id: &str,
            resolution: &serde_json::Value,
        ) -> anyhow::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(json!({"request_id":id,"resolution":resolution}));
            anyhow::ensure!(
                !self.fail_resolution,
                "fixture resolution commit unavailable"
            );
            Ok(())
        }
    }
    #[tokio::test]
    async fn actual_tool_records_answer_and_propagates_capture_failures() {
        use zeroclaw_api::session_events::capture::{
            SessionCaptureStopped, scope_session_execution,
        };
        for failure in ["none", "request", "resolution"] {
            let channel = Arc::new(RespondingChannel::new("test", "fixture answer"));
            let tool = make_tool_with_channels(vec![("test", channel.clone())]);
            let capture = Arc::new(ElicitationCapture {
                fail_request: failure == "request",
                fail_resolution: failure == "resolution",
                ..Default::default()
            });
            let result = scope_session_execution(
                capture.clone(),
                tool.execute(json!({"question":"fixture question","timeout_secs":1})),
            )
            .await;
            let calls = capture.calls.lock().unwrap();
            assert_eq!(calls[0]["prompt"], "fixture question");
            assert!(
                calls[0]["expires_at"].as_i64().unwrap() > chrono::Utc::now().timestamp_millis()
            );
            if failure == "request" {
                assert!(
                    channel.sent.read().is_empty(),
                    "effect must await durable request"
                );
                assert_eq!(calls.len(), 1);
            } else {
                assert_eq!(channel.sent.read().len(), 1);
                assert_eq!(calls.len(), 2);
                assert_eq!(calls[0]["request_id"], calls[1]["request_id"]);
                assert_eq!(calls[1]["resolution"]["state"], "answered");
                assert_eq!(calls[1]["resolution"]["answer"], "fixture answer");
            }
            if failure == "none" {
                assert!(result.unwrap().success)
            } else {
                assert!(
                    result
                        .unwrap_err()
                        .downcast_ref::<SessionCaptureStopped>()
                        .is_some()
                )
            }
        }
    }
}
