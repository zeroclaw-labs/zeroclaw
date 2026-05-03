use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::media::MediaAttachment;

// ── Channel approval types ──────────────────────────────────────

/// Compact description of a tool call presented to the user for approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelApprovalRequest {
    pub tool_name: String,
    pub arguments_summary: String,
}

/// The operator's response to a channel-presented approval prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelApprovalResponse {
    /// Execute this one call.
    Approve,
    /// Deny this call.
    Deny,
    /// Execute and add tool to session-scoped allowlist.
    #[serde(rename = "always")]
    AlwaysApprove,
}

/// A message received from or sent to a channel
#[derive(Debug, Clone)]
pub struct ChannelMessage {
    pub id: String,
    pub sender: String,
    pub reply_target: String,
    pub content: String,
    pub channel: String,
    pub timestamp: u64,
    /// Platform thread identifier (e.g. Slack `ts`, Discord thread ID).
    /// When set, replies should be posted as threaded responses.
    pub thread_ts: Option<String>,
    /// Thread scope identifier for interruption/cancellation grouping.
    /// Distinct from `thread_ts` (reply anchor): this is `Some` only when the message
    /// is genuinely inside a reply thread and should be isolated from other threads.
    /// `None` means top-level — scope is sender+channel only.
    pub interruption_scope_id: Option<String>,
    /// Media attachments (audio, images, video) for the media pipeline.
    /// Channels populate this when they receive media alongside a text message.
    /// Defaults to empty — existing channels are unaffected.
    pub attachments: Vec<MediaAttachment>,
}

/// Message to send through a channel
#[derive(Debug, Clone)]
pub struct SendMessage {
    pub content: String,
    pub recipient: String,
    pub subject: Option<String>,
    /// Platform thread identifier for threaded replies (e.g. Slack `thread_ts`).
    pub thread_ts: Option<String>,
    /// Optional cancellation token for interruptible delivery (e.g. multi-message mode).
    pub cancellation_token: Option<CancellationToken>,
    /// File attachments to send with the message.
    /// Channels that don't support attachments ignore this field.
    pub attachments: Vec<MediaAttachment>,
}

impl SendMessage {
    /// Create a new message with content and recipient
    pub fn new(content: impl Into<String>, recipient: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            recipient: recipient.into(),
            subject: None,
            thread_ts: None,
            cancellation_token: None,
            attachments: vec![],
        }
    }

    /// Create a new message with content, recipient, and subject
    pub fn with_subject(
        content: impl Into<String>,
        recipient: impl Into<String>,
        subject: impl Into<String>,
    ) -> Self {
        Self {
            content: content.into(),
            recipient: recipient.into(),
            subject: Some(subject.into()),
            thread_ts: None,
            cancellation_token: None,
            attachments: vec![],
        }
    }

    /// Set the thread identifier for threaded replies.
    pub fn in_thread(mut self, thread_ts: Option<String>) -> Self {
        self.thread_ts = thread_ts;
        self
    }

    /// Attach a cancellation token for interruptible delivery.
    pub fn with_cancellation(mut self, token: CancellationToken) -> Self {
        self.cancellation_token = Some(token);
        self
    }

    /// Attach files to this message.
    pub fn with_attachments(mut self, attachments: Vec<MediaAttachment>) -> Self {
        self.attachments = attachments;
        self
    }
}

/// Core channel trait — implement for any messaging platform
#[async_trait]
pub trait Channel: Send + Sync {
    /// Human-readable channel name
    fn name(&self) -> &str;

    /// Send a message through this channel
    async fn send(&self, message: &SendMessage) -> anyhow::Result<()>;

    /// Start listening for incoming messages (long-running)
    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()>;

    /// Check if channel is healthy
    async fn health_check(&self) -> bool {
        true
    }

    /// Send a discrete-choice prompt with options.
    ///
    /// Each `(callback_id, label)` pair represents one choice. The
    /// `callback_id` is the stable identifier the channel uses when the
    /// user makes a selection — for native-interactive channels, this
    /// id round-trips back through the inbound message (e.g. as the
    /// `id` field of a WhatsApp `interactive.button_reply`, or via a
    /// `[choice]<callback_id>` content sentinel for Signal poll votes).
    ///
    /// The default impl renders a numbered text fallback with an
    /// explicit "reply with name or number" hint, suitable for any
    /// channel that has no native UI for discrete choices (Matrix, SMS,
    /// IRC, etc.). Channels with native interactive support (Telegram
    /// inline keyboards, Signal polls, WhatsApp interactive buttons /
    /// lists) override this to render natively. The contract: the
    /// caller will see the user's selection arrive on the inbound side
    /// — either as the rendered numbered text (which the caller is
    /// expected to match against its options) or as a structured
    /// channel-specific sentinel.
    ///
    /// `prompt` is the question / title; rendered ABOVE the options.
    async fn send_choice(
        &self,
        recipient: &str,
        prompt: &str,
        options: &[(String, String)],
    ) -> anyhow::Result<()> {
        let mut text = String::new();
        if !prompt.trim().is_empty() {
            text.push_str(prompt.trim());
            text.push_str("\n\n");
        }
        text.push_str("(reply with name or number)\n");
        for (idx, (_id, label)) in options.iter().enumerate() {
            text.push_str(&format!("{}. {}\n", idx + 1, label.trim()));
        }
        // Trim trailing newline so the message looks tidy across clients.
        let trimmed = text.trim_end().to_string();
        self.send(&SendMessage::new(trimmed, recipient)).await
    }

    /// Signal that the bot is processing a response (e.g. "typing" indicator).
    async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        Ok(())
    }

    /// Stop any active typing indicator.
    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        Ok(())
    }

    /// Whether this channel supports progressive message updates via draft edits.
    fn supports_draft_updates(&self) -> bool {
        false
    }

    /// Whether this channel supports multi-message streaming delivery.
    fn supports_multi_message_streaming(&self) -> bool {
        false
    }

    /// Minimum delay (ms) between sending each paragraph in multi-message mode.
    fn multi_message_delay_ms(&self) -> u64 {
        800
    }

    /// Send an initial draft message. Returns a platform-specific message ID for later edits.
    async fn send_draft(&self, _message: &SendMessage) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    /// Update a previously sent draft message with new accumulated content.
    async fn update_draft(
        &self,
        _recipient: &str,
        _message_id: &str,
        _text: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Show a progress/status update (e.g. tool execution status).
    async fn update_draft_progress(
        &self,
        _recipient: &str,
        _message_id: &str,
        _text: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Finalize a draft with the complete response (e.g. apply Markdown formatting).
    async fn finalize_draft(
        &self,
        _recipient: &str,
        _message_id: &str,
        _text: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Cancel and remove a previously sent draft message if the channel supports it.
    async fn cancel_draft(&self, _recipient: &str, _message_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    /// Add a reaction (emoji) to a message.
    async fn add_reaction(
        &self,
        _channel_id: &str,
        _message_id: &str,
        _emoji: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Remove a reaction (emoji) from a message previously added by this bot.
    async fn remove_reaction(
        &self,
        _channel_id: &str,
        _message_id: &str,
        _emoji: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Pin a message in the channel.
    async fn pin_message(&self, _channel_id: &str, _message_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    /// Unpin a previously pinned message.
    async fn unpin_message(&self, _channel_id: &str, _message_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    /// Redact (delete) a message from the channel.
    async fn redact_message(
        &self,
        _channel_id: &str,
        _message_id: &str,
        _reason: Option<String>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Request interactive tool-call approval from the channel operator.
    ///
    /// Returns `Ok(Some(response))` when the operator answers within the
    /// channel's configured `approval_timeout_secs`; timeouts are surfaced
    /// as `Deny`. Returns `Ok(None)` only for channels that do not implement
    /// the prompt at all — the caller should fall back to its default policy
    /// (typically auto-deny).
    ///
    /// Surface varies by channel:
    /// - **Telegram** uses inline keyboard buttons.
    /// - **Slack** Socket Mode uses Block Kit buttons; webhook fallback and
    ///   non–Socket Mode deployments use a token text reply.
    /// - **Discord, Signal, Matrix, WhatsApp** embed a 6-character
    ///   alphanumeric token in the prompt and wait for a
    ///   `<token> approve|deny|always` reply on the same conversation.
    async fn request_approval(
        &self,
        _recipient: &str,
        _request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
        Ok(None)
    }

    /// Ask the user a multiple-choice question and return the chosen option's text.
    ///
    /// Returns `Ok(Some(answer))` if the channel handled the question natively
    /// (e.g. ACP `session/request_permission`, Telegram inline keyboard).
    /// Returns `Ok(None)` to signal the caller should fall back to the
    /// generic `send` + `listen` flow. Default impl returns `None`.
    ///
    /// Free-form questions (no choices) are not modeled here yet — they
    /// require the ACP elicitation RFD to land for a clean cross-channel API.
    async fn request_choice(
        &self,
        _question: &str,
        _choices: &[String],
        _timeout: std::time::Duration,
    ) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    /// Whether this channel can answer free-form (no-choices) `ask_user`
    /// questions via the standard `send` + `listen` flow.
    ///
    /// Channels that can only handle structured choices (e.g. ACP today, until
    /// the elicitation RFD lands) should return `false` so callers can fail
    /// fast with a useful error instead of timing out on `listen`.
    fn supports_free_form_ask(&self) -> bool {
        true
    }
}
