use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelApprovalResponse {
    /// Execute this one call.
    Approve,
    /// Deny this call.
    Deny,
    /// Execute and add tool to session-scoped allowlist.
    #[serde(rename = "always")]
    AlwaysApprove,
    /// Deny this call and supply an edited replacement for the arguments.
    #[serde(rename = "deny_with_edit")]
    DenyWithEdit { replacement: String },
}

/// Conversation history scope for an inbound channel message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChannelConversationScope {
    /// Isolate history by channel, room/reply target, thread, and sender.
    #[default]
    Sender,
    /// Share history for everyone in the room/reply target.
    ReplyTarget,
}

/// A message received from or sent to a channel
#[derive(Debug, Clone, Default)]
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
    /// Email subject for reply threading.
    pub subject: Option<String>,
    /// When true, the orchestrator records this as context only and must not
    /// start an agent turn or emit visible channel side effects.
    pub passive_context: bool,
    /// Controls whether conversation history is sender-scoped or room-scoped.
    pub conversation_scope: ChannelConversationScope,
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
    /// Message-ID to set as In-Reply-To header (email threading).
    pub in_reply_to: Option<String>,
    /// When `true`, channels that support TTS must not synthesise this
    /// message as a voice note. Use for error notices, system alerts, and
    /// other non-conversational content that should never be voiced.
    pub suppress_voice: bool,
}

/// Cross-channel room visibility used by room-management APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomVisibility {
    Private,
    Public,
}

impl RoomVisibility {
    pub const SCHEMA_VALUES: &'static [&'static str] = &["private", "public"];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Public => "public",
        }
    }
}

impl fmt::Display for RoomVisibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RoomVisibility {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "private" => Ok(Self::Private),
            "public" => Ok(Self::Public),
            other => {
                anyhow::bail!("unsupported room visibility '{other}': expected private or public")
            }
        }
    }
}

/// Room creation options shared by channel implementations that support
/// creating group conversations.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomCreationOptions {
    pub name: Option<String>,
    pub topic: Option<String>,
    pub invites: Vec<String>,
    pub visibility: Option<RoomVisibility>,
    pub encryption: Option<bool>,
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
            in_reply_to: None,
            suppress_voice: false,
        }
    }

    /// Prevent TTS channels from voicing this message.
    pub fn suppress_voice(mut self) -> Self {
        self.suppress_voice = true;
        self
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
            in_reply_to: None,
            suppress_voice: false,
        }
    }

    /// Set the In-Reply-To header for email threading.
    pub fn in_reply_to(mut self, msg_id: Option<String>) -> Self {
        self.in_reply_to = msg_id;
        self
    }

    /// Set the subject on an existing SendMessage (builder style).
    pub fn subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
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

impl ChannelMessage {
    /// Construct a `ChannelMessage` with all required fields set and all optional
    /// fields zeroed. Prefer this over raw struct literals so that new optional
    /// fields added to `ChannelMessage` in the future don't require mechanical
    /// updates at every call site.
    pub fn new(
        id: impl Into<String>,
        sender: impl Into<String>,
        reply_target: impl Into<String>,
        content: impl Into<String>,
        channel: impl Into<String>,
        timestamp: u64,
    ) -> Self {
        Self {
            id: id.into(),
            sender: sender.into(),
            reply_target: reply_target.into(),
            content: content.into(),
            channel: channel.into(),
            timestamp,
            ..Self::default()
        }
    }
}

impl SendMessage {
    /// Build a reply `SendMessage` from an inbound `ChannelMessage`.
    ///
    /// Sets `recipient` from `msg.reply_target`, threads via `in_reply_to` and
    /// `thread_ts`, and prepends `Re:` to the subject when the inbound message
    /// carried one. Safe to call from any channel handler; the `in_reply_to`
    /// field is silently ignored by channels that don't support it.
    pub fn reply_to(msg: &ChannelMessage, content: impl Into<String>) -> Self {
        let mut sm = Self::new(content, &msg.reply_target)
            .in_thread(msg.thread_ts.clone())
            .in_reply_to(Some(msg.id.clone()));
        if let Some(ref subj) = msg.subject {
            let reply_subject = if subj.to_ascii_lowercase().starts_with("re:") {
                subj.clone()
            } else {
                format!("Re: {}", subj)
            };
            sm = sm.subject(reply_subject);
        }
        sm
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

    /// Whether an inbound message is a direct, one-to-one conversation
    /// with the bot (a DM/IM), as opposed to a group or broadcast
    /// channel. A direct message is definitionally addressed to the
    /// bot, so the orchestrator skips the reply-intent classifier and
    /// goes straight to the tool-capable agent turn.
    ///
    /// Default `false`: channels that cannot prove a one-to-one context
    /// keep the classifier precheck. Channels that distinguish DMs from
    /// group traffic override this.
    fn is_direct_message(&self, _msg: &ChannelMessage) -> bool {
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

    /// Create a new platform room/conversation when the channel supports it.
    async fn create_room(&self, _options: &RoomCreationOptions) -> anyhow::Result<String> {
        anyhow::bail!("channel does not support room creation")
    }

    /// Invite a user to an existing platform room/conversation.
    async fn invite_user(&self, _room_id: &str, _user_id: &str) -> anyhow::Result<()> {
        anyhow::bail!("channel does not support room invites")
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

    /// The name of the back-channel that produced the most recent
    /// [`Channel::request_approval`] decision, when this channel fans a single
    /// request out to several registered back-channels (the agent's approval
    /// bridge does this so an ACP editor and a WebSocket dashboard can both
    /// answer). Ordinary single channels return `None` — their own
    /// [`Channel::name`] already identifies the deciding surface — so the
    /// approval audit trail can record the channel that actually decided
    /// instead of the turn loop's static channel name.
    fn last_decision_channel(&self) -> Option<String> {
        None
    }

    /// Ask the user a multiple-choice question and return the chosen option's text.
    ///
    /// Returns `Ok(Some(answer))` if the channel handled the question natively
    /// (e.g. ACP `elicitation/create` with a single-select enum schema, or
    /// the legacy `session/request_permission` fallback for older ACP clients;
    /// Telegram inline keyboard; etc.). Returns `Ok(None)` to signal the
    /// caller should fall back to the generic `send` + `listen` flow.
    /// Default impl returns `None`.
    ///
    /// Free-form (no-choices) questions are not modeled by this method.
    /// Multiple-choice support landed via ACP `elicitation/create` (see
    /// the ACP elicitation RFD: https://agentclientprotocol.com/rfds/elicitation);
    /// free-form text is tracked under that spec's Phase 2.
    async fn request_choice(
        &self,
        _question: &str,
        _choices: &[String],
        _timeout: std::time::Duration,
    ) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    /// Ask the user a multi-select multiple-choice question and return the
    /// chosen options' text.
    ///
    /// Returns `Ok(Some(answers))` if the channel handled it natively (e.g.
    /// ACP `elicitation/create` with a `type: array` schema). Returns
    /// `Ok(None)` to signal the caller should fall back to a non-native
    /// path (formatted text + reactions, etc.). Default impl returns `None`.
    ///
    /// `min_items` and `max_items` map to JSON Schema's `minItems` /
    /// `maxItems` — clients enforce the bound before submitting.
    async fn request_multi_choice(
        &self,
        _question: &str,
        _choices: &[String],
        _min_items: usize,
        _max_items: usize,
        _timeout: std::time::Duration,
    ) -> anyhow::Result<Option<Vec<String>>> {
        Ok(None)
    }

    /// Whether this channel can answer free-form (no-choices) `ask_user`
    /// questions via the standard `send` + `listen` flow.
    ///
    /// Channels that can only handle structured choices (e.g. ACP in Phase 1
    /// of the elicitation rollout — see
    /// the ACP elicitation RFD: https://agentclientprotocol.com/rfds/elicitation)
    /// should return `false` so callers can fail fast with a useful error
    /// instead of timing out on `listen`. Free-form text support flips this
    /// to `true` in Phase 2.
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
        ChannelMessage::new("1", sender, "", "hi", "stub", 0)
    }

    #[test]
    fn channel_message_new_zeros_optional_fields() {
        let msg = ChannelMessage::new("id1", "alice", "room-1", "hello", "slack", 42);
        assert_eq!(msg.id, "id1");
        assert_eq!(msg.sender, "alice");
        assert_eq!(msg.reply_target, "room-1");
        assert_eq!(msg.content, "hello");
        assert_eq!(msg.channel, "slack");
        assert_eq!(msg.timestamp, 42);
        assert!(msg.channel_alias.is_none());
        assert!(msg.thread_ts.is_none());
        assert!(msg.interruption_scope_id.is_none());
        assert!(msg.attachments.is_empty());
        assert!(msg.subject.is_none());
        assert!(!msg.passive_context);
        assert_eq!(msg.conversation_scope, ChannelConversationScope::Sender);
    }

    #[test]
    fn send_message_reply_to_sets_threading_fields() {
        let inbound = ChannelMessage {
            id: "msg-001".into(),
            reply_target: "user@example.com".into(),
            thread_ts: Some("thread-1".into()),
            subject: Some("Hello there".into()),
            ..ChannelMessage::new("msg-001", "alice", "user@example.com", "", "email", 0)
        };
        let reply = SendMessage::reply_to(&inbound, "Got it");
        assert_eq!(reply.recipient, "user@example.com");
        assert_eq!(reply.in_reply_to.as_deref(), Some("msg-001"));
        assert_eq!(reply.thread_ts.as_deref(), Some("thread-1"));
        assert_eq!(reply.subject.as_deref(), Some("Re: Hello there"));
        assert_eq!(reply.content, "Got it");
    }

    #[test]
    fn send_message_reply_to_does_not_double_re_prefix() {
        let inbound = ChannelMessage {
            subject: Some("Re: Already prefixed".into()),
            ..ChannelMessage::new("msg-002", "alice", "user@example.com", "", "email", 0)
        };
        let reply = SendMessage::reply_to(&inbound, "");
        assert_eq!(reply.subject.as_deref(), Some("Re: Already prefixed"));
    }

    #[test]
    fn send_message_reply_to_no_subject_omits_subject() {
        let inbound = ChannelMessage::new("msg-003", "alice", "room-1", "ping", "slack", 0);
        let reply = SendMessage::reply_to(&inbound, "pong");
        assert!(reply.subject.is_none());
        assert_eq!(reply.in_reply_to.as_deref(), Some("msg-003"));
    }

    #[test]
    fn room_visibility_parses_supported_values() {
        assert_eq!(
            "private".parse::<RoomVisibility>().unwrap(),
            RoomVisibility::Private
        );
        assert_eq!(
            "PUBLIC".parse::<RoomVisibility>().unwrap(),
            RoomVisibility::Public
        );
    }

    #[test]
    fn room_visibility_rejects_unknown_values() {
        let err = "shared".parse::<RoomVisibility>().unwrap_err();
        assert!(err.to_string().contains("expected private or public"));
    }

    #[tokio::test]
    async fn room_management_defaults_report_unsupported() {
        let channel = StubChannel { handle: None };

        let create = channel
            .create_room(&RoomCreationOptions {
                name: Some("ops".into()),
                ..RoomCreationOptions::default()
            })
            .await
            .unwrap_err();
        assert!(
            create
                .to_string()
                .contains("does not support room creation")
        );

        let invite = channel
            .invite_user("!room:example.org", "@alice:example.org")
            .await
            .unwrap_err();
        assert!(invite.to_string().contains("does not support room invites"));
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

    #[test]
    fn deny_with_edit_round_trips_through_serde() {
        let r = ChannelApprovalResponse::DenyWithEdit {
            replacement: "new content".to_string(),
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: ChannelApprovalResponse = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(back, ChannelApprovalResponse::DenyWithEdit { replacement } if replacement == "new content")
        );
    }
}
