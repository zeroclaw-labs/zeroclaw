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
    /// Raw tool arguments for channels (e.g. ACP) that can render structured
    /// diffs instead of a plain summary string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_arguments: Option<serde_json::Value>,
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
    /// ZeroClaw channel alias (the `<alias>` half of `[channels.<type>.<alias>]`)
    /// when the platform supports multiple bot instances. Used by
    /// session_key construction so two bots on the same platform compute
    /// distinct session IDs and don't share conversation history. `None`
    /// for channels that don't have an alias concept yet (webhook, cli).
    pub channel_alias: Option<String>,
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

/// Core channel trait — implement for any messaging platform.
///
/// Every `Channel` is `Attributable`: the orchestrator's spawn site opens
/// `attribution_span!(&*ch)` so log emissions from within `listen()` / `send()`
/// inherit `channel = <type>.<alias>` from the trait object's role + alias.
#[async_trait]
pub trait Channel: Send + Sync + crate::attribution::Attributable {
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

    /// Self-loop guard for multi-agent runs.
    ///
    /// Returns the bot's own handle/identity on this channel
    /// (e.g. `@my_bot` for Telegram, the bot's user ID for Discord)
    /// when known, so the orchestrator can drop inbound events whose
    /// `sender` matches: a bot must never respond to its own
    /// messages, even if a misconfigured peer group lists the bot's
    /// handle as an external peer.
    ///
    /// **Channels that handle inbound traffic must override this.**
    /// The default `None` makes both layers of the orchestrator's
    /// self-loop guard (the SDK-side `drop_self_messages` here, and
    /// the agent-loop fallback `peers::should_drop_self_loop`) into
    /// no-ops — both layers consult the same `self_handle`, so a
    /// channel that returns `None` has no protection from looping on
    /// its own outbound. Outbound-only channels (webhook, gmail-push,
    /// voice-call) never see inbound and can keep the default. The
    /// in-tree overrides currently cover Telegram (`bot_username`
    /// cache), IRC (configured nickname), Discord (decoded from token),
    /// Slack (cached `auth.test` user_id); other inbound channels
    /// remain on the default and rely on per-impl filtering instead
    /// of the shared guard.
    fn self_handle(&self) -> Option<String> {
        None
    }

    /// The exact form the bot expects to see when addressed by users on
    /// this channel. Discord wraps the snowflake as `<@1088...>`,
    /// Telegram presents `@bot_username`, Matrix presents
    /// `@bot:server`, Slack wraps the user ID as `<@U02...>`. Returned
    /// verbatim into the per-channel system prompt so the agent
    /// recognizes its own mention without guessing, and uses the same
    /// form to tag itself or peers in outbound replies.
    ///
    /// Default `None` for channels that have no inbound mention
    /// concept (CLI, webhook, hardware, ACP elicitation). Channels
    /// that override `self_handle` should usually override this too,
    /// applying their platform-native mention wrapper to the handle.
    fn self_addressed_mention(&self) -> Option<String> {
        None
    }

    /// Whether the orchestrator should drop an inbound message as
    /// self-authored (multi-agent self-loop guard).
    ///
    /// Default implementation compares `msg.sender` against
    /// [`Self::self_handle`] case-insensitively, after stripping a
    /// leading `@` from each side so Telegram-style handles match
    /// regardless of which form the SDK delivers. Override only for
    /// platforms whose identity comparison is non-string (e.g. a
    /// numeric Discord user ID is `as_str` already; this default
    /// works there too).
    fn drop_self_messages(&self, msg: &ChannelMessage) -> bool {
        let Some(handle) = self.self_handle() else {
            return false;
        };
        let handle_norm = handle.trim_start_matches('@').to_ascii_lowercase();
        let sender_norm = msg.sender.trim_start_matches('@').to_ascii_lowercase();
        !handle_norm.is_empty() && handle_norm == sender_norm
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Stub channel that overrides `self_handle` so the default
    /// `drop_self_messages` implementation can be exercised.
    struct StubChannel {
        handle: Option<String>,
    }

    impl crate::attribution::Attributable for StubChannel {
        fn role(&self) -> crate::attribution::Role {
            crate::attribution::Role::Channel(crate::attribution::ChannelKind::Webhook)
        }
        fn alias(&self) -> &str {
            "stub"
        }
    }

    #[async_trait]
    impl Channel for StubChannel {
        fn name(&self) -> &str {
            "stub"
        }
        async fn send(&self, _message: &SendMessage) -> anyhow::Result<()> {
            Ok(())
        }
        async fn listen(
            &self,
            _tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        fn self_handle(&self) -> Option<String> {
            self.handle.clone()
        }
    }

    fn msg_from(sender: &str) -> ChannelMessage {
        ChannelMessage {
            id: "1".into(),
            sender: sender.into(),
            reply_target: String::new(),
            content: "hi".into(),
            channel: "stub".into(),
            channel_alias: None,
            timestamp: 0,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: Vec::new(),
        }
    }

    #[test]
    fn drop_self_messages_default_returns_false_when_handle_unknown() {
        let channel = StubChannel { handle: None };
        assert!(!channel.drop_self_messages(&msg_from("@anyone")));
    }

    #[test]
    fn drop_self_messages_matches_exact_handle() {
        let channel = StubChannel {
            handle: Some("@my_bot".into()),
        };
        assert!(channel.drop_self_messages(&msg_from("@my_bot")));
        assert!(!channel.drop_self_messages(&msg_from("@other_bot")));
    }

    #[test]
    fn drop_self_messages_normalizes_at_prefix_and_case() {
        let channel = StubChannel {
            handle: Some("My_Bot".into()),
        };
        // SDK delivered with @ prefix, handle stored without. Match.
        assert!(channel.drop_self_messages(&msg_from("@my_bot")));
        // Both with @, mixed case. Match.
        let channel = StubChannel {
            handle: Some("@My_Bot".into()),
        };
        assert!(channel.drop_self_messages(&msg_from("@MY_BOT")));
    }

    #[test]
    fn drop_self_messages_does_not_match_empty_handle() {
        // A handle of "@" (effectively empty after normalization) must
        // not match every inbound message; the guard only fires when
        // the bot has a real handle to compare against.
        let channel = StubChannel {
            handle: Some("@".into()),
        };
        assert!(!channel.drop_self_messages(&msg_from("@anyone")));
    }
}
