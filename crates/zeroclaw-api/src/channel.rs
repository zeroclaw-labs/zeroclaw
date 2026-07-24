use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use tokio_util::sync::CancellationToken;

use crate::media::MediaAttachment;

/// Reserved `ChannelMessage.subject` prefix the git/forge channel uses to
/// label SOP-ingress events. Routing is NOT keyed on this (see
/// `ChannelMessage::internal_sop_event`); it exists so channels that fill
/// `subject` from user-controlled data (email) can keep this reserved
/// namespace out of inbound subjects.
pub const CHANNEL_SOP_SUBJECT_PREFIX: &str = "zeroclaw:sop-event:";

/// The single authority for the channel-SOP event topic grammar
/// `channel.alias:event_type`. The producer that lifts a forge/platform event
/// into SOP ingress builds the topic here; the SOP engine parses it here. The
/// grammar lives in one place so the two sides cannot drift.
///
/// `alias` separates from `channel` with `.`; `event_type` separates from the
/// `channel.alias` head with `:`. A bare `channel` (no alias, no event type)
/// and the `channel/alias` message form are both accepted by `parse` so the
/// same matcher serves agent-loop message triggers and forge event triggers.
pub struct ChannelSopTopic;

impl ChannelSopTopic {
    const ALIAS_SEP: char = '.';
    const EVENT_SEP: char = ':';
    const MESSAGE_ALIAS_SEP: char = '/';

    /// Build a forge/platform event topic `channel.alias:event_type`.
    #[must_use]
    pub fn build(channel: &str, alias: &str, event_type: &str) -> String {
        format!(
            "{channel}{}{alias}{}{event_type}",
            Self::ALIAS_SEP,
            Self::EVENT_SEP
        )
    }

    /// Parse a channel-SOP topic into `(channel, alias, event_type)`. The head
    /// before the event separator yields the channel kind and optional alias;
    /// the tail after it is the optional event type. Accepts both the forge
    /// form (`channel.alias:event_type`) and the message form (`channel` or
    /// `channel/alias`).
    #[must_use]
    pub fn parse(topic: &str) -> (&str, Option<&str>, Option<&str>) {
        let (head, event_type) = match topic.split_once(Self::EVENT_SEP) {
            Some((before, after)) => (before, Some(after)),
            None => (topic, None),
        };
        let (channel, alias) = head
            .split_once(Self::ALIAS_SEP)
            .or_else(|| head.split_once(Self::MESSAGE_ALIAS_SEP))
            .map_or((head, None), |(c, a)| (c, Some(a)));
        (channel, alias, event_type)
    }
}

// ── Channel approval types ──────────────────────────────────────

/// Compact description of a tool call presented to the user for approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelApprovalRequest {
    /// Stable tool name from the runtime tool registry.
    pub tool_name: String,
    /// Human-readable argument summary for channels that cannot render a
    /// structured approval view.
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

/// An approval response together with the back-channel that produced it.
///
/// When a channel fans one approval request out to several back-channels,
/// `decided_by` names the back-channel that actually answered, so the audit
/// trail attributes the decision to the deciding surface. The attribution
/// travels with the returned decision, so concurrent approvals on the same
/// channel instance cannot cross-wire it. Single channels leave it `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributedApprovalResponse {
    pub response: ChannelApprovalResponse,
    pub decided_by: Option<String>,
}

/// A long-lived, channel-agnostic gate prompt (e.g. a parked SOP approval):
/// rendered natively per channel (Discord embed + buttons, Telegram inline
/// keyboard, ...), answered through the channel's normal inbound path — a
/// component click or a `<choice> <reference>` text reply — NOT a blocking wait.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelGatePrompt {
    /// Short heading, e.g. "SOP approval needed: gitea-triage-pipeline".
    pub title: String,
    /// Body: what is waiting and how to answer (includes the text-reply form
    /// for humans on channels that render it as plain text).
    pub description: String,
    /// Correlation key the answer must carry (e.g. the SOP run id). Encoded in
    /// component custom_ids and expected in text replies.
    pub reference: String,
    /// The presented choices, in order.
    pub choices: Vec<GateChoice>,
    /// Body a RESOLVED prompt should keep showing (the context, without the
    /// how-to-answer instructions): on finalize the channel appends the outcome
    /// line under it, so the record of WHAT was approved survives in place.
    /// `None` = the outcome replaces the body entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_description: Option<String>,
}

/// The fixed vocabulary of gate-answer tokens, and the single source of truth
/// for their wire spelling. A gate answer crosses two stringly-typed transports
/// — a Discord `custom_id` and a `<choice> <reference>` text reply — so the
/// token must be a string on the wire; this enum keeps producer (the route
/// adapter that mints [`GateChoice::id`]) and consumer (the orchestrator that
/// maps an answer to an approval decision) matching on ONE definition instead of
/// re-typing the literal at each site, so adding a choice is a compile error at
/// every place that must handle it rather than a silent drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateChoiceKind {
    /// Approve the gate as presented.
    Approve,
    /// Deny the gate (cancels / fails the run per the step's policy).
    Deny,
    /// Approve with an operator amendment to the declared editable field.
    Edit,
    /// Ask for a re-draft with guidance (checkpoint with an llm predecessor).
    Revise,
}

impl GateChoiceKind {
    /// The wire token — the string carried in a `custom_id` and a text reply.
    pub fn id(self) -> &'static str {
        match self {
            GateChoiceKind::Approve => "approve",
            GateChoiceKind::Deny => "deny",
            GateChoiceKind::Edit => "edit",
            GateChoiceKind::Revise => "revise",
        }
    }

    /// Parse a wire token back to its kind (case-insensitive). `None` for any
    /// unknown token, so an unrecognized answer is dropped, never coerced.
    pub fn from_id(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "approve" => Some(GateChoiceKind::Approve),
            "deny" => Some(GateChoiceKind::Deny),
            "edit" => Some(GateChoiceKind::Edit),
            "revise" => Some(GateChoiceKind::Revise),
            _ => None,
        }
    }

    /// True when answering this choice collects free text (the amended draft or
    /// the re-draft guidance) — the channels that can, render a form for it.
    pub fn collects_text(self) -> bool {
        matches!(self, GateChoiceKind::Edit | GateChoiceKind::Revise)
    }
}

/// One selectable choice on a [`ChannelGatePrompt`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateChoice {
    /// Stable machine id carried back in the answer (e.g. "approve"). Mint it
    /// from [`GateChoiceKind::id`] so it stays in lockstep with the parser.
    pub id: String,
    /// Human label for the button / keyboard entry.
    pub label: String,
    /// Visual emphasis hint for channels that support it.
    pub emphasis: GateChoiceEmphasis,
    /// When set, this choice collects free text from the operator before it is
    /// answered (e.g. "Edit" amends a draft, "Revise" sends guidance). Channels
    /// with a native form (Discord modal) render one; channels without simply
    /// omit the choice — plain approve/deny stays universally answerable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<GateChoiceInput>,
}

/// The text-collection spec of an input-bearing [`GateChoice`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateChoiceInput {
    /// Field label shown on the form (e.g. "Edited draft").
    pub label: String,
    /// Pre-filled text (e.g. the current draft an Edit starts from).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefill: Option<String>,
}

/// Rendering hint for a [`GateChoice`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GateChoiceEmphasis {
    /// The affirmative action (e.g. a green/primary button).
    Positive,
    /// The destructive/refusing action (e.g. a red button).
    Negative,
    /// No particular emphasis.
    Neutral,
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

/// A channel envelope for inbound user text and synthetic runtime messages.
///
/// The channel implementation owns platform-native parsing before this point.
/// Runtime code treats this as the trusted delivery envelope: sender, route,
/// thread, and scope fields come from the channel, while `content` remains user
/// or controller text.
#[derive(Debug, Clone, Default)]
pub struct ChannelMessage {
    /// Channel-native event id, or a synthetic id for controller-generated
    /// continuation messages.
    pub id: String,
    /// Channel-native sender identity.
    pub sender: String,
    /// Channel-native reply target such as room id, channel id, or direct-chat
    /// target.
    pub reply_target: String,
    /// User/controller message body. Command parsing and prompt construction
    /// treat this as untrusted text.
    pub content: String,
    /// Channel family name such as `matrix`, `telegram`, or `cli`.
    pub channel: String,
    /// ZeroClaw channel alias (the `<alias>` half of `[channels.<type>.<alias>]`)
    /// when the platform supports multiple bot instances. Session-key
    /// construction uses this so two bots on the same platform compute
    /// distinct session IDs and don't share conversation history. `None`
    /// for channels without an alias concept (webhook, cli).
    pub channel_alias: Option<String>,
    /// Channel event timestamp in the platform's normalized integer form.
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
    /// Email-style subject for transports that use subject-based threading.
    pub subject: Option<String>,
    /// Internal SOP-ingress marker carrying the event topic, set ONLY by the
    /// git/forge channel producer. The orchestrator routes a message into the
    /// SOP engine when (and only when) this is `Some`, so the decision can
    /// never be driven by user-controlled fields like `subject` or `content`.
    /// Never round-trips through serde.
    pub internal_sop_event: Option<String>,
    /// When true, the orchestrator records this as context only and must not
    /// start an agent turn or emit visible channel side effects.
    pub passive_context: bool,
    /// Channel adapter observed that this inbound message explicitly addressed
    /// the bot through a platform-level signal such as an @mention.
    pub explicitly_addressed: bool,
    /// Controls whether conversation history is sender-scoped or room-scoped.
    pub conversation_scope: ChannelConversationScope,
}

/// Outbound message request for a channel implementation.
///
/// This is delivery intent, not a durable transcript row. Each channel decides
/// how to map thread ids, subjects, attachments, and voice suppression to its
/// platform API.
#[derive(Debug, Clone)]
pub struct SendMessage {
    /// Text body to deliver.
    pub content: String,
    /// Channel-native recipient, room, or reply target.
    pub recipient: String,
    /// Optional subject for email-like transports.
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
    /// When `true`, channels that support TTS must deliver this message as
    /// a voice note even if the peer's default modality is text.
    /// Ignored when `suppress_voice` is also `true`.
    pub force_voice: bool,
}

/// Cross-channel room visibility used by room-management APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomVisibility {
    /// Invite-only or otherwise non-public room/channel visibility.
    Private,
    /// Publicly discoverable/joinable room/channel visibility.
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

/// Room creation request shared by channel implementations that support
/// creating group conversations.
///
/// Unsupported fields are ignored by individual transports; the caller must not
/// infer that a field was honored unless the channel reports success with
/// platform-specific details.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomCreationOptions {
    /// Desired room/channel display name.
    pub name: Option<String>,
    /// Desired room/channel topic.
    pub topic: Option<String>,
    /// Channel-native user ids or handles to invite.
    pub invites: Vec<String>,
    /// Desired room visibility, when supported by the transport.
    pub visibility: Option<RoomVisibility>,
    /// Desired encryption setting, when supported by the transport.
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
            force_voice: false,
        }
    }

    /// Prevent TTS channels from voicing this message.
    pub fn suppress_voice(mut self) -> Self {
        self.suppress_voice = true;
        self
    }

    /// Force TTS channels to deliver this message as a voice note.
    pub fn force_voice(mut self) -> Self {
        self.force_voice = true;
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
            force_voice: false,
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

/// A low-level, provider-relative forge API request routed through a
/// forge-backed channel. Channel-neutral so the `Channel` trait carries no
/// forge-specific types; the git channel maps this onto its provider's
/// `forge_request`. `method` is an uppercase HTTP verb (`GET`/`POST`/`PATCH`/
/// `PUT`/`DELETE`); `path` is relative to the provider's API base (e.g.
/// `repos/owner/repo/issues/12/labels`); `body` is an optional JSON payload.
#[derive(Debug, Clone)]
pub struct ForgeApiRequest {
    pub method: String,
    pub path: String,
    pub body: Option<serde_json::Value>,
}

/// The outcome of a forge API request: the HTTP status and decoded JSON body
/// (`Null` when the response had no body). Non-2xx statuses are carried here
/// rather than raised, so the caller inspects the forge's own error envelope.
#[derive(Debug, Clone)]
pub struct ForgeApiResponse {
    pub status: u16,
    pub body: serde_json::Value,
}

/// Core channel trait — implement for any messaging platform.
///
/// Every `Channel` is `Attributable`: the orchestrator's spawn site opens
/// `attribution_span!(&*ch)` so log emissions from within `listen()` /
/// `send()` inherit `channel = <type>.<alias>`.
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

    /// Send a discrete-choice prompt with options.
    ///
    /// Each `(callback_id, label)` pair represents one choice. Whether
    /// the `callback_id` round-trips on inbound is **channel-specific**:
    ///
    /// - **WhatsApp Cloud** preserves the `callback_id` exactly — it
    ///   appears as `interactive.button_reply.id` /
    ///   `interactive.list_reply.id`, surfaced as
    ///   `[choice]<callback_id>` in the inbound `ChannelMessage.content`.
    /// - **Signal** uses native polls. Real signal-cli `pollVote`
    ///   payloads round-trip selected option indexes, surfaced as
    ///   `[choice-index]N` with a 1-based index. Some alternate
    ///   `pollAnswer` payloads may include selected titles and surface
    ///   as `[choice]<label>`, but callers needing stable callback ids
    ///   should maintain a side map keyed by poll option index.
    /// - **Default text fallback** (Matrix, SMS, IRC, mock) renders a
    ///   numbered list with a "reply with name or number" hint and
    ///   relies on the consumer's own matcher to resolve the user's
    ///   text reply against the option list.
    ///
    /// `options.is_empty()` is treated as "send the prompt as plain
    /// text with no choices" rather than rendering an empty selection
    /// hint; if `prompt` is also empty the call is a no-op (returns
    /// `Ok(())`).
    ///
    /// `prompt` is the question / title; rendered ABOVE the options.
    async fn send_choice(
        &self,
        recipient: &str,
        prompt: &str,
        options: &[(String, String)],
    ) -> anyhow::Result<()> {
        let trimmed_prompt = prompt.trim();
        if options.is_empty() {
            // No options to render. Send the prompt as plain text; if
            // there's nothing to say either, this is a no-op so we
            // don't ship an empty message that confuses the client.
            if trimmed_prompt.is_empty() {
                return Ok(());
            }
            return self
                .send(&SendMessage::new(trimmed_prompt, recipient))
                .await;
        }

        let mut text = String::new();
        if !trimmed_prompt.is_empty() {
            text.push_str(trimmed_prompt);
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

    /// Whether `send` actually delivers a message OUTBOUND on this channel. Default
    /// `true`. An INBOUND-ONLY transport (e.g. an AMQP trigger source whose `send` is a
    /// deliberate no-op that returns `Ok`) overrides this to `false`, so a surface that
    /// must genuinely deliver - such as the SOP approval route adapter - can refuse to
    /// route to it (and report the misconfiguration) instead of silently succeeding
    /// without sending anything.
    fn supports_outbound_send(&self) -> bool {
        true
    }

    /// Self-loop guard for multi-agent runs: the bot's own handle/identity on
    /// this channel, so the orchestrator can drop inbound events whose
    /// `sender` matches. A bot must never respond to its own messages, even
    /// if a misconfigured peer group lists the bot's handle as an external
    /// peer.
    ///
    /// **Channels that handle inbound traffic must override this.** The
    /// default `None` disables both layers of the self-loop guard
    /// (`drop_self_messages` here and the agent-loop fallback), leaving the
    /// channel unprotected from looping on its own outbound. Outbound-only
    /// channels can keep the default.
    fn self_handle(&self) -> Option<String> {
        None
    }

    /// The exact form the bot expects when addressed by users on this channel
    /// (e.g. Discord `<@snowflake>`, Telegram `@bot_username`). Returned
    /// verbatim into the per-channel system prompt. Default `None` for
    /// channels with no inbound mention concept. Channels that override
    /// `self_handle` should usually override this too.
    fn self_addressed_mention(&self) -> Option<String> {
        None
    }

    /// Whether the orchestrator should drop an inbound message as
    /// self-authored (multi-agent self-loop guard). Default compares
    /// `msg.sender` against [`Self::self_handle`] case-insensitively after
    /// stripping a leading `@` from each side. Override only for platforms
    /// whose identity comparison is non-string.
    fn drop_self_messages(&self, msg: &ChannelMessage) -> bool {
        let Some(handle) = self.self_handle() else {
            return false;
        };
        let handle_norm = handle.trim_start_matches('@').to_ascii_lowercase();
        let sender_norm = msg.sender.trim_start_matches('@').to_ascii_lowercase();
        !handle_norm.is_empty() && handle_norm == sender_norm
    }

    async fn forge_request(&self, _request: ForgeApiRequest) -> anyhow::Result<ForgeApiResponse> {
        anyhow::bail!(
            "channel '{}' does not support forge API requests",
            self.name()
        )
    }

    /// Whether an inbound message is a direct one-to-one conversation with
    /// the bot. A DM is definitionally addressed to the bot, so the
    /// orchestrator skips the reply-intent classifier and goes straight to
    /// the tool-capable agent turn. Default `false`: channels that cannot
    /// prove a one-to-one context keep the classifier precheck.
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
    /// `suppress_voice` forces text delivery even on voice-only peers.
    async fn finalize_draft(
        &self,
        _recipient: &str,
        _message_id: &str,
        _text: &str,
        _suppress_voice: bool,
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
    /// channel's configured `approval_timeout_secs`; timeouts surface as
    /// `Deny`. Returns `Ok(None)` only for channels that do not implement
    /// the prompt at all — the caller falls back to its default policy
    /// (typically auto-deny).
    async fn request_approval(
        &self,
        _recipient: &str,
        _request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
        Ok(None)
    }

    /// Request approval from one exact authenticated principal.
    ///
    /// The default is deliberately fail-closed: adapters that cannot verify
    /// the responder identity must not approve a goal-bound action merely
    /// because they can display a prompt.
    async fn request_approval_for_principal(
        &self,
        _recipient: &str,
        _principal: &str,
        _request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
        Ok(None)
    }

    /// Like [`Channel::request_approval`], but also reports which
    /// back-channel produced the decision when this channel fans the request
    /// out. Default delegates to [`Channel::request_approval`] with
    /// `decided_by: None`; only a fan-out bridge needs to override.
    async fn request_approval_attributed(
        &self,
        recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<AttributedApprovalResponse>> {
        Ok(self
            .request_approval(recipient, request)
            .await?
            .map(|response| AttributedApprovalResponse {
                response,
                decided_by: None,
            }))
    }

    /// Present a long-lived, out-of-band gate prompt (e.g. a parked SOP
    /// approval) on this channel, rendered natively — an embed with one button
    /// per choice on Discord, an inline keyboard on Telegram, and so on.
    ///
    /// UNLIKE [`Channel::request_approval`] this does NOT wait for the answer:
    /// the prompt outlives the call (and daemon restarts). The choice comes
    /// back through the channel's normal INBOUND path — a component click
    /// (stamped with an internal `sop.gate:` marker by the channel's own
    /// interaction producer, never derivable from message text) or a plain
    /// `<choice> <reference>` text reply — which the orchestrator resolves
    /// against the parked gate.
    ///
    /// Default `Ok(false)` = "no native prompt on this channel"; the caller
    /// then falls back to a plain text notice carrying the reply instructions.
    async fn send_gate_prompt(
        &self,
        _recipient: &str,
        _prompt: &ChannelGatePrompt,
    ) -> anyhow::Result<bool> {
        Ok(false)
    }

    /// Mark a previously sent gate prompt as resolved: strip its interactive
    /// controls and replace the body with `outcome` (e.g. "Approved by @user —
    /// run resumed"), so a decided gate cannot be clicked again and the
    /// decision is visible in place. `reference` is the same correlation key
    /// the prompt was sent with. Best-effort: `Ok(false)` when this channel
    /// has nothing to finalize (no native prompt, or the mapping was lost to a
    /// restart) — the gate state itself is never affected.
    async fn finalize_gate_prompt(&self, _reference: &str, _outcome: &str) -> anyhow::Result<bool> {
        Ok(false)
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
    /// the ACP elicitation RFD: <https://agentclientprotocol.com/rfds/elicitation>);
    /// free-form text is tracked under that spec's Phase 2.
    async fn request_choice(
        &self,
        _question: &str,
        _choices: &[String],
        _timeout: std::time::Duration,
    ) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

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
    /// questions via the standard `send` + `listen` flow. Channels that only
    /// handle structured choices return `false` so callers fail fast with a
    /// useful error instead of timing out on `listen`.
    fn supports_free_form_ask(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_choice_kind_ids_round_trip_and_reject_unknown() {
        for kind in [
            GateChoiceKind::Approve,
            GateChoiceKind::Deny,
            GateChoiceKind::Edit,
            GateChoiceKind::Revise,
        ] {
            assert_eq!(GateChoiceKind::from_id(kind.id()), Some(kind));
        }
        // Case-insensitive parse; unknown tokens are None (dropped, never coerced).
        assert_eq!(
            GateChoiceKind::from_id("APPROVE"),
            Some(GateChoiceKind::Approve)
        );
        assert_eq!(GateChoiceKind::from_id("escalate"), None);
        assert_eq!(GateChoiceKind::from_id(""), None);
        // Only the text-collecting choices report collects_text.
        assert!(GateChoiceKind::Edit.collects_text());
        assert!(GateChoiceKind::Revise.collects_text());
        assert!(!GateChoiceKind::Approve.collects_text());
        assert!(!GateChoiceKind::Deny.collects_text());
    }

    #[test]
    fn channel_sop_topic_build_parse_roundtrip() {
        let topic = ChannelSopTopic::build("git", "main", "pull_request.opened");
        assert_eq!(topic, "git.main:pull_request.opened");
        let (channel, alias, event_type) = ChannelSopTopic::parse(&topic);
        assert_eq!(channel, "git");
        assert_eq!(alias, Some("main"));
        assert_eq!(event_type, Some("pull_request.opened"));
    }

    #[test]
    fn channel_sop_topic_parses_message_forms() {
        let (channel, alias, event_type) = ChannelSopTopic::parse("telegram");
        assert_eq!(channel, "telegram");
        assert_eq!(alias, None);
        assert_eq!(event_type, None);

        let (channel, alias, event_type) = ChannelSopTopic::parse("telegram/prod");
        assert_eq!(channel, "telegram");
        assert_eq!(alias, Some("prod"));
        assert_eq!(event_type, None);
    }

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
        assert!(!msg.explicitly_addressed);
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
