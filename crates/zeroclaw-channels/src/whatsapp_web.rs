//! WhatsApp Web channel using wa-rs (native Rust implementation)

use super::whatsapp_storage::RusqliteStore;
use anyhow::{Context, Result};
use async_trait::async_trait;
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::select;
use waproto::whatsapp::device_props::PlatformType;
use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelConversationScope,
    ChannelMessage, SendMessage,
};
#[cfg(feature = "whatsapp-web")]
use zeroclaw_api::media::MediaAttachment;
use zeroclaw_runtime::i18n;

/// What a pending approval token is bound to.
///
/// The Cloud transport's map stores only `token -> sender`, which is why a
/// token is answerable by anyone who can see it. On Web the token is delivered
/// into a chat that may have other members, so the binding travels with the
/// token and is re-checked when a reply arrives.
#[cfg(feature = "whatsapp-web")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ApprovalBinding {
    /// The configured alias that ISSUED this request.
    ///
    /// The map below is process-wide, so without this the binding is only
    /// (token, chat) and the reply path resolves the authorized peers using
    /// whichever instance happened to receive the reply. Two aliases sharing a
    /// contact or group then leak authority across the boundary an operator
    /// drew between them: a responder alias A denies can approve alias A's tool
    /// call by replying through alias B, because B's allowlist is the one
    /// consulted. Binding the issuer and enforcing it on resolve keeps each
    /// alias's approvals answerable only under its own policy.
    alias: String,
    /// The chat JID the prompt was posted into. A reply carrying a valid token
    /// from a DIFFERENT chat is not this request's answer.
    chat: String,
    /// True when the prompt landed in a group, i.e. when the token was visible
    /// to members other than the operator.
    is_group: bool,
}

#[cfg(feature = "whatsapp-web")]
struct PendingApproval {
    registration_id: uuid::Uuid,
    responder: tokio::sync::oneshot::Sender<ChannelApprovalResponse>,
    binding: ApprovalBinding,
}

#[cfg(feature = "whatsapp-web")]
struct PendingApprovalRegistration {
    token: String,
    receiver: tokio::sync::oneshot::Receiver<ChannelApprovalResponse>,
    binding: ApprovalBinding,
    guard: PendingApprovalGuard,
}

/// Removes a parked token when the requesting future goes away.
///
/// Every explicit cleanup path lives INSIDE the async body: send-failure and
/// the channel's own timeout both remove the token before returning. None of
/// that code runs if the future is CANCELLED or DROPPED, which the routed
/// approval timeout and the orchestrator's cancellation branch both do. The
/// entry then outlives its receiver, and a late reply passes every
/// authorization gate, removes the entry, and is logged as accepted while the
/// oneshot is already gone, so no tool call can ever receive that decision. An
/// approval recorded as granted that nothing acted on is worse than a denial:
/// the log says the operator approved and the system disagrees.
///
/// `Drop` is the only hook that survives cancellation, so registration is made
/// cancellation-safe by tying the entry's lifetime to a guard rather than to
/// any code path. The guard is disarmed once the decision has been taken, so
/// the normal path still owns its own removal.
#[cfg(feature = "whatsapp-web")]
struct PendingApprovalGuard {
    token: Option<String>,
    registration_id: uuid::Uuid,
}

#[cfg(feature = "whatsapp-web")]
impl PendingApprovalGuard {
    fn new(token: String, registration_id: uuid::Uuid) -> Self {
        Self {
            token: Some(token),
            registration_id,
        }
    }

    /// The decision was taken through a normal path; stop guarding.
    fn disarm(&mut self) {
        self.token = None;
    }
}

#[cfg(feature = "whatsapp-web")]
impl Drop for PendingApprovalGuard {
    fn drop(&mut self) {
        let Some(token) = self.token.take() else {
            return;
        };
        // Drop is not async, and blocking here would stall whatever runtime
        // thread is unwinding. Spawn the removal instead; the entry is gone
        // before any realistic reply, and a reply that beats it still finds a
        // dead receiver and cannot be reported as accepted.
        let registration_id = self.registration_id;
        zeroclaw_spawn::spawn!(async move {
            remove_pending_approval_if_matches(&token, registration_id).await;
        });
    }
}

#[cfg(feature = "whatsapp-web")]
type PendingApprovalsMap = tokio::sync::Mutex<std::collections::HashMap<String, PendingApproval>>;

/// Process-wide pending approvals, keyed by token.
///
/// Static rather than per-instance for the same reason as the Cloud transport:
/// `request_approval` is called on one handle while the reply arrives on the
/// `listen()` task, and the two do not share a `&self`.
#[cfg(feature = "whatsapp-web")]
static PENDING_APPROVALS: std::sync::LazyLock<Arc<PendingApprovalsMap>> =
    std::sync::LazyLock::new(|| {
        Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()))
    });

#[cfg(feature = "whatsapp-web")]
async fn remove_pending_approval_if_matches(token: &str, registration_id: uuid::Uuid) -> bool {
    let mut pending = PENDING_APPROVALS.lock().await;
    if pending
        .get(token)
        .is_some_and(|entry| entry.registration_id == registration_id)
    {
        pending.remove(token);
        true
    } else {
        false
    }
}

/// Reserve an unused reply token and tie its map entry to a cancellation guard.
#[cfg(feature = "whatsapp-web")]
async fn register_pending_approval(binding: ApprovalBinding) -> PendingApprovalRegistration {
    loop {
        let token = crate::util::new_approval_token();
        let mut pending = PENDING_APPROVALS.lock().await;

        if let std::collections::hash_map::Entry::Vacant(slot) = pending.entry(token.clone()) {
            let registration_id = uuid::Uuid::new_v4();
            let (responder, receiver) = tokio::sync::oneshot::channel();
            slot.insert(PendingApproval {
                registration_id,
                responder,
                binding: binding.clone(),
            });
            drop(pending);

            return PendingApprovalRegistration {
                token: token.clone(),
                receiver,
                binding,
                guard: PendingApprovalGuard::new(token, registration_id),
            };
        }
    }
}

/// Why a syntactically valid approval reply was refused.
///
/// Named rather than a bool so the log says which gate rejected, the way
/// discord's component path reports `UnauthorizedUser`.
#[cfg(feature = "whatsapp-web")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalRefusal {
    /// No such token, or it was already answered or timed out.
    UnknownToken,
    /// The token exists but the reply came from a different chat.
    ForeignChat,
    /// The reply came from the right chat, but the responder is not on
    /// an authorized peer for this alias. This is the gate the other transports omit.
    UnauthorizedResponder,
    /// The reply cleared every authorization gate, but the requesting future
    /// was already cancelled or dropped, so the oneshot receiver is gone and
    /// no tool call can receive the decision. Reporting this as accepted would
    /// record an approval that nothing acted on.
    ReceiverGone,
    /// A valid token replied to through a DIFFERENT configured alias than the
    /// one that issued it. Distinct from `ForeignChat` because two aliases can
    /// legitimately share a chat, so the chat can match while the issuer does
    /// not, and answering under the wrong alias means the wrong
    /// alias's authorized peers decide.
    ForeignAlias,
    /// The token belongs to a group that the live channel policy no longer
    /// admits.
    GroupNoLongerAllowed,
    /// The token belongs to a direct message that the live channel policy no
    /// longer admits. Separate from the group variant because this name is
    /// what the refusal log prints, and an operator reading `Group` on a DM
    /// refusal would be told something untrue about their own configuration.
    DmNoLongerAllowed,
}

/// Decide whether an approval reply may resolve `token`, and resolve it if so.
///
/// Returns `Ok(())` when the decision was delivered, or the refusal reason.
///
/// AUTHORIZATION, and the reason this is not a mirror of the Cloud path: a
/// 6-character token is a correlator, not a credential. It is delivered in
/// plaintext into a chat, so in a group every member can read it and reply with
/// it. Treating possession of the token as authority makes the approval
/// answerable by any group member, which is the shape confirmed on slack,
/// telegram and matrix. Discord already gates the equivalent decision on the
/// clicking user, so the gate is required rather than novel; this applies the
/// same rule to a text reply.
#[cfg(feature = "whatsapp-web")]
async fn resolve_approval_reply(
    token: &str,
    response: ChannelApprovalResponse,
    from_alias: &str,
    from_chat: &str,
    responder_is_allowlisted: bool,
) -> std::result::Result<(), ApprovalRefusal> {
    let mut map = PENDING_APPROVALS.lock().await;
    let Some(pending) = map.get(token) else {
        return Err(ApprovalRefusal::UnknownToken);
    };
    // The issuing alias is checked FIRST, before the chat and before the
    // allowlist. `responder_is_allowlisted` is computed by whichever instance
    // received the reply, so evaluating it against another alias's request is
    // the bypass itself: alias B's allowlist would decide alias A's approval.
    // Two aliases can legitimately share a contact or a group, which is what
    // makes the chat check alone insufficient here.
    if pending.binding.alias != from_alias {
        return Err(ApprovalRefusal::ForeignAlias);
    }
    if pending.binding.chat != from_chat {
        return Err(ApprovalRefusal::ForeignChat);
    }
    if !responder_is_allowlisted {
        return Err(ApprovalRefusal::UnauthorizedResponder);
    }
    // Only remove once the reply has cleared every gate. A refused reply must
    // leave the request pending so the real operator can still answer it,
    // otherwise anyone who can see the token can cancel the approval by
    // replying to it badly.
    let pending = map
        .remove(token)
        .expect("token was present under this same lock");
    // Report acceptance ONLY when the decision reached a live receiver.
    //
    // `send` fails when the oneshot receiver is already gone, which happens
    // when the requesting future was cancelled or dropped. Discarding that
    // error would log the reply as accepted while no tool call can ever act on
    // it, so the record would say the operator approved and the system would
    // disagree. A stale token is a refusal, not an approval, and the caller
    // logs each refusal distinctly.
    pending
        .responder
        .send(response)
        .map_err(|_| ApprovalRefusal::ReceiverGone)?;
    Ok(())
}

/// Re-check live group admission before resolving a pending approval reply.
#[cfg(feature = "whatsapp-web")]
async fn resolve_approval_reply_with_group_admission(
    token: &str,
    response: ChannelApprovalResponse,
    from_alias: &str,
    from_chat: &str,
    is_group: bool,
    responder_is_allowlisted: bool,
    allowed_groups_resolver: &(dyn Fn() -> Vec<String> + Send + Sync),
    group_policy: &zeroclaw_config::schema::WhatsAppChatPolicy,
    dm_policy: &zeroclaw_config::schema::WhatsAppChatPolicy,
    self_chat: SelfChatVerdict,
) -> std::result::Result<(), ApprovalRefusal> {
    // The policies are re-read here, not captured when the approval was issued:
    // an operator who closes access while a prompt is outstanding must not have
    // that reply honoured.
    //
    // A reply executes the pending tool, so it clears the same two gates an
    // ordinary message clears. The chat-type gate decides whether this KIND of
    // chat is answered at all; the identity gate decides whether THIS group is
    // listed. `is_group_chat_allowed` is only the second, and a non-empty list
    // matching this chat satisfies it under every policy, `ignore` included, so
    // the identity gate alone would honour a reply in a chat the operator told
    // this channel to ignore.
    //
    // Only the two ignore verdicts are consulted. Responder authorization stays
    // with `resolve_approval_reply`, which reports it as `UnauthorizedResponder`
    // rather than collapsing it into a chat refusal.
    match self_chat {
        // The channel ignores this thread entirely, so a reply in it cannot
        // resolve a pending tool either.
        SelfChatVerdict::Disabled => return Err(ApprovalRefusal::DmNoLongerAllowed),
        // The documented personal-mode exception: the operator's own thread is
        // admitted whatever `dm_policy` says, and a reply must be admitted on
        // the same terms or the prompt it answers can never be answered.
        SelfChatVerdict::Admitted => {}
        SelfChatVerdict::NotSelfChat => {
            match chat_type_policy_decision(
                is_group,
                group_policy,
                dm_policy,
                responder_is_allowlisted,
            ) {
                ChatPolicyDecision::DropGroupIgnored => {
                    return Err(ApprovalRefusal::GroupNoLongerAllowed);
                }
                ChatPolicyDecision::DropDmIgnored => {
                    return Err(ApprovalRefusal::DmNoLongerAllowed);
                }
                ChatPolicyDecision::Admit | ChatPolicyDecision::DropUnrecognizedSender => {}
            }
        }
    }
    if is_group && !is_group_chat_allowed(from_chat, &allowed_groups_resolver(), group_policy) {
        return Err(ApprovalRefusal::GroupNoLongerAllowed);
    }

    resolve_approval_reply(
        token,
        response,
        from_alias,
        from_chat,
        responder_is_allowlisted,
    )
    .await
}

/// Test-only interception point for the approval prompt's send.
///
/// The send is the only moment at which this request's token is registered
/// and its cleanup has not yet run, so it is the only place a test can stand
/// between the two. Without it the generation check in `request_approval`'s
/// send-error branch is unobservable from outside: `request_approval` mints
/// its own random token, so a separately parked sentinel is never the key
/// that branch removes, and an unconditional `remove` there behaves exactly
/// like a generation-checked one.
///
/// It returns the send's own `Result` rather than being a bare fail flag,
/// because a hook that can return `Ok` also reaches the wait without a vendor
/// client, which is what driving the timeout arm for real would need.
#[cfg(all(test, feature = "whatsapp-web"))]
type ApprovalSendHook = Arc<
    dyn Fn(SendMessage) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>
        + Send
        + Sync,
>;

#[cfg(feature = "whatsapp-web")]
pub struct WhatsAppWebChannel {
    /// Session database path
    session_path: String,
    /// Phone number for pair code linking (optional)
    pair_phone: Option<String>,
    /// Custom pair code (optional)
    pair_code: Option<String>,
    /// Override WebSocket URL (test / proxy setups). Sourced from
    /// `[whatsapp.ws_url]` — replaces the legacy `WHATSAPP_WS_URL` env-var
    /// read.
    ws_url: Option<String>,
    /// Display name announced to contacts (optional)
    push_name: Option<String>,
    /// The alias key under `[channels.whatsapp.<alias>]` this handle is
    /// bound to. Used to scope peer-group writes and resolver lookups.
    alias: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    /// When true, only respond to messages that @-mention the bot in groups
    mention_only: bool,
    /// When true, allowed unaddressed group messages become context-only
    /// history entries instead of being dropped.
    passive_group_context: bool,
    /// Whether outgoing PDF documents get a first-page preview. Read from
    /// `[channels.whatsapp.<alias>].document_thumbnails` in
    /// [`WhatsAppWebChannel::new`].
    document_thumbnails: bool,
    /// Bot phone number (digits only), resolved from pair_phone or device identity at runtime
    bot_phone: Arc<Mutex<Option<String>>>,
    /// Bot LID number (digits only), resolved from device identity at runtime
    bot_lid: Arc<Mutex<Option<String>>>,
    /// Usage mode (business vs personal policy filtering)
    mode: zeroclaw_config::schema::WhatsAppWebMode,
    /// DM policy. Consulted under BOTH modes.
    dm_policy: zeroclaw_config::schema::WhatsAppChatPolicy,
    /// Group policy. Consulted under BOTH modes, same as `dm_policy`.
    group_policy: zeroclaw_config::schema::WhatsAppChatPolicy,
    /// Whether to always respond in self-chat when mode = personal
    self_chat_mode: bool,
    /// Seconds to wait for an operator approval reply before denying.
    ///
    /// Read from `[channels.whatsapp.<alias>].approval_timeout_secs` in
    /// [`WhatsAppWebChannel::new`], so every construction path picks it up
    /// without threading it through a builder. Default 300; `0` denies
    /// immediately, which is what an already-elapsed `tokio::time::timeout`
    /// does and the safer of the two readings of zero.
    approval_timeout_secs: u64,
    /// Bot handle for shutdown.
    /// Handle returned by `Bot::spawn` in whatsapp-rust 0.7 (a Future + abort)
    /// rather than a tokio JoinHandle directly.
    bot_handle: Arc<Mutex<Option<whatsapp_rust::bot::BotHandle>>>,
    /// Client handle for sending messages and typing indicators
    client: Arc<Mutex<Option<Arc<whatsapp_rust::Client>>>>,
    /// Message sender channel
    tx: Arc<Mutex<Option<tokio::sync::mpsc::Sender<ChannelMessage>>>>,
    /// Voice transcription (STT) config
    transcription: Option<zeroclaw_config::schema::TranscriptionConfig>,
    transcription_manager: Option<std::sync::Arc<super::transcription::TranscriptionManager>>,
    /// Text-to-speech runtime for voice replies (built from
    /// `tts_providers.<type>.<alias>`).
    tts_manager: Option<Arc<super::tts::TtsManager>>,
    /// Chats awaiting a voice reply — maps chat JID to the latest substantive
    /// reply text. A background task debounces and sends the voice note after
    /// the agent finishes its turn (no new send() for 3 seconds).
    pending_voice:
        Arc<std::sync::Mutex<std::collections::HashMap<String, (String, std::time::Instant)>>>,
    /// Chats whose last incoming message was a voice note.
    voice_chats: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// Compiled mention patterns for DM mention gating.
    dm_mention_patterns: Arc<Vec<regex::Regex>>,
    /// Compiled mention patterns for group-chat mention gating.
    /// When non-empty, only group messages matching at least one pattern are
    /// processed; matched fragments are stripped from the forwarded content.
    group_mention_patterns: Arc<Vec<regex::Regex>>,
    /// Resolved channel workspace root used to bound outbound local media
    /// marker reads. The source of truth remains
    /// `Config::channel_workspace_dir("whatsapp.<alias>")`; this is the
    /// runtime trust boundary for file delivery.
    workspace_dir: Option<PathBuf>,
    /// Resolves allowed group chats from canonical config at message-time.
    /// Empty admits no group unless `group_policy` is `all`, which admits
    /// every group. Direct messages bypass.
    allowed_groups_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    /// Optional pairing-persist handle to the canonical shared `Config`.
    /// `None` in tests; `Some` in the long-running daemon, wired via
    /// `.with_persistence(config)`. Same contract as WeChat's handle: on
    /// connect, the linked account is persisted into `peer_groups` through
    /// `crate::identity_persist` (no channel-local allowlist cache).
    persist: Option<Arc<parking_lot::RwLock<zeroclaw_config::schema::Config>>>,
    /// See [`ApprovalSendHook`]. `None` outside the tests that need to act
    /// between a token's registration and the cleanup that follows it.
    #[cfg(test)]
    approval_send_hook: Option<ApprovalSendHook>,
}

#[cfg(feature = "whatsapp-web")]
struct SenderAllowlistResolution {
    mapped_phone: Option<String>,
    candidates: Vec<String>,
    allowed_phone: Option<String>,
}

#[cfg(feature = "whatsapp-web")]
#[derive(Clone)]
struct WhatsAppInboundContext {
    tx: tokio::sync::mpsc::Sender<ChannelMessage>,
    alias: Arc<String>,
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    allowed_groups_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    mode: zeroclaw_config::schema::WhatsAppWebMode,
    dm_policy: zeroclaw_config::schema::WhatsAppChatPolicy,
    group_policy: zeroclaw_config::schema::WhatsAppChatPolicy,
    self_chat_mode: bool,
    mention_only: bool,
    passive_group_context: bool,
    bot_phone: Arc<Mutex<Option<String>>>,
    bot_lid: Arc<Mutex<Option<String>>>,
    dm_mention_patterns: Arc<Vec<regex::Regex>>,
    group_mention_patterns: Arc<Vec<regex::Regex>>,
    transcription_config: Option<zeroclaw_config::schema::TranscriptionConfig>,
    transcription_manager: Option<Arc<super::transcription::TranscriptionManager>>,
    voice_chats: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}

impl WhatsAppWebChannel {
    #[cfg(feature = "whatsapp-web")]
    pub fn new(
        config: &zeroclaw_config::schema::WhatsAppConfig,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
        allowed_groups_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Self {
        let session_path = config.session_path.clone().unwrap_or_default();
        let pair_phone = config.pair_phone.clone();
        let pair_code = config.pair_code.clone();
        let ws_url = config.ws_url.clone();
        let push_name = config.push_name.clone();
        let mention_only = config.mention_only;
        let passive_group_context = config.passive_group_context;
        let document_thumbnails = config.document_thumbnails;
        let mode = config.mode.clone();
        let dm_policy = config.dm_policy.clone();
        let group_policy = config.group_policy.clone();
        let self_chat_mode = config.self_chat_mode;
        let approval_timeout_secs = config.approval_timeout_secs;

        // Seed bot_phone from pair_phone (digits only)
        let bot_phone = pair_phone
            .as_ref()
            .map(|p| p.chars().filter(|c| c.is_ascii_digit()).collect::<String>())
            .filter(|digits| !digits.is_empty());

        // Only the NEWLY-closed case warns. Personal mode with `group_policy =
        // "ignore"` was already closed, and `group_policy = "all"` explicitly
        // preserves open access, so neither is reported: an operator whose
        // behaviour did not change should not be told that it did. The predicate
        // is shared with `config validate` so the two cannot disagree about which
        // configurations changed.
        if zeroclaw_config::schema::whatsapp_empty_group_list_is_newly_closed(&mode, &group_policy)
            && allowed_groups_resolver().is_empty()
        {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "group_policy": format!("{group_policy:?}"),
                        "mode": format!("{mode:?}"),
                    })),
                format!(
                    "allowed_groups is empty and group_policy is \"allowlist\", \
                     so this channel will answer no group. An empty list used \
                     to admit every group at the identity gate; that gate is \
                     now decided by group_policy. To restore group access, {}.",
                    // Shared with the `config validate` warning, so the two
                    // surfaces cannot offer different remedies for one config.
                    zeroclaw_config::schema::whatsapp_empty_group_list_remedy(&group_policy)
                )
            );
        }

        if mention_only && bot_phone.is_none() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "mention_only enabled but pair_phone not set. \
                Bot identity will be resolved after connection. Group messages \
                will be skipped until identity is known."
            );
        }

        Self {
            session_path,
            pair_phone,
            pair_code,
            ws_url,
            push_name,
            alias: alias.into(),
            peer_resolver,
            mention_only,
            passive_group_context,
            document_thumbnails,
            bot_phone: Arc::new(Mutex::new(bot_phone)),
            bot_lid: Arc::new(Mutex::new(None)),
            mode,
            dm_policy,
            group_policy,
            self_chat_mode,
            approval_timeout_secs,
            allowed_groups_resolver,
            bot_handle: Arc::new(Mutex::new(None)),
            client: Arc::new(Mutex::new(None)),
            tx: Arc::new(Mutex::new(None)),
            transcription: None,
            transcription_manager: None,
            tts_manager: None,
            pending_voice: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            voice_chats: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            dm_mention_patterns: Arc::new(Vec::new()),
            group_mention_patterns: Arc::new(Vec::new()),
            workspace_dir: None,
            persist: None,
            #[cfg(test)]
            approval_send_hook: None,
        }
    }

    /// Return the alias under `[channels.whatsapp.<alias>]` that this
    /// channel handle is bound to.
    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// Wire the shared Config handle so a completed pairing can persist the
    /// linked account into `peer_groups` and save — the same contract as
    /// `WeChatChannel::with_persistence`. The long-running daemon sets this
    /// from the orchestrator; tests and one-shot callers leave it unset
    /// (pairing works at runtime, doesn't persist).
    #[cfg(feature = "whatsapp-web")]
    pub fn with_persistence(
        mut self,
        config: Arc<parking_lot::RwLock<zeroclaw_config::schema::Config>>,
    ) -> Self {
        self.persist = Some(config);
        self
    }

    #[cfg(feature = "whatsapp-web")]
    pub fn with_workspace_dir(mut self, dir: PathBuf) -> Self {
        self.workspace_dir = Some(dir);
        self
    }

    /// Substitute the approval prompt's send. See [`ApprovalSendHook`].
    #[cfg(all(test, feature = "whatsapp-web"))]
    fn with_approval_send_hook(mut self, hook: ApprovalSendHook) -> Self {
        self.approval_send_hook = Some(hook);
        self
    }

    /// Deliver an approval prompt.
    ///
    /// A pass-through to [`Channel::send`] in production. The indirection
    /// exists so a test can substitute the send, which is the only window in
    /// which this request's map entry is live and its cleanup has not run.
    #[cfg(feature = "whatsapp-web")]
    async fn send_approval_prompt(&self, message: &SendMessage) -> Result<()> {
        #[cfg(test)]
        if let Some(hook) = self.approval_send_hook.clone() {
            return hook(message.clone()).await;
        }
        self.send(message).await
    }

    /// Configure voice transcription from a `[transcription]` snapshot.
    ///
    /// Compatibility and test path. The daemon routes every channel through
    /// `with_transcription_manager` with a manager built from live
    /// config and the owning agent's resolved provider; this path can only see
    /// the legacy section, so it binds a lone registered provider and
    /// otherwise leaves the choice unbound (see
    /// `transcription::manager_from_snapshot`).
    #[cfg(feature = "whatsapp-web")]
    pub fn with_transcription(self, config: zeroclaw_config::schema::TranscriptionConfig) -> Self {
        let manager = super::transcription::manager_from_snapshot(&config);
        self.with_transcription_manager(config, manager)
    }

    /// Store an already-built transcription manager, or nothing. The config is
    /// recorded only alongside a manager, so a channel never advertises
    /// transcription it cannot perform.
    #[cfg(feature = "whatsapp-web")]
    pub(crate) fn with_transcription_manager(
        mut self,
        config: zeroclaw_config::schema::TranscriptionConfig,
        manager: Option<std::sync::Arc<super::transcription::TranscriptionManager>>,
    ) -> Self {
        if let Some(manager) = manager {
            self.transcription_manager = Some(manager);
            self.transcription = Some(config);
        }
        self
    }

    #[cfg(feature = "whatsapp-web")]
    pub fn with_tts(mut self, config: &zeroclaw_config::schema::Config) -> Self {
        if config.tts.enabled {
            let owner = config.agent_for_channel(&format!("whatsapp.{}", self.alias));
            match super::tts::TtsManager::from_config_for_agent(config, owner) {
                Ok(m) => self.tts_manager = Some(Arc::new(m)),
                Err(e) => ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "TTS disabled"
                ),
            }
        }
        self
    }

    /// Set mention patterns for DM mention gating.
    /// Each pattern string is compiled as a case-insensitive regex.
    /// Invalid patterns are logged and skipped.
    #[cfg(feature = "whatsapp-web")]
    pub fn with_dm_mention_patterns(mut self, patterns: Vec<String>) -> Self {
        self.dm_mention_patterns = Arc::new(
            super::whatsapp::WhatsAppChannel::compile_mention_patterns(&patterns),
        );
        self
    }

    /// Set mention patterns for group-chat mention gating.
    /// Each pattern string is compiled as a case-insensitive regex.
    /// Invalid patterns are logged and skipped.
    #[cfg(feature = "whatsapp-web")]
    pub fn with_group_mention_patterns(mut self, patterns: Vec<String>) -> Self {
        self.group_mention_patterns = Arc::new(
            super::whatsapp::WhatsAppChannel::compile_mention_patterns(&patterns),
        );
        self
    }

    /// Check if a phone number is allowed (E.164 format: +1234567890)
    #[cfg(feature = "whatsapp-web")]
    fn is_number_allowed(&self, phone: &str) -> bool {
        let peers = (self.peer_resolver)();
        Self::is_number_allowed_for_list(&peers, phone)
    }

    #[cfg(feature = "whatsapp-web")]
    fn is_number_allowed_for_list(allowed_numbers: &[String], phone: &str) -> bool {
        Self::are_numbers_allowed_for_list(allowed_numbers, &[phone])
    }

    /// One sender reaches this channel under several numbers (its JID, the
    /// alternate JID and the LID mapping), so they are evaluated as one
    /// account: a deny naming any of them rejects the sender, whichever number
    /// would otherwise have carried the grant.
    #[cfg(feature = "whatsapp-web")]
    fn are_numbers_allowed_for_list(allowed_numbers: &[String], phones: &[&str]) -> bool {
        // The surrounding-whitespace wildcard this channel accepts is now what
        // the shared helper accepts, so there is no broader local notion of the
        // wildcard left to keep in step with the deny check.
        crate::allowlist::is_identity_allowed_by(allowed_numbers, phones, Self::phone_matches)
    }

    /// Both WhatsApp surfaces admit the same accounts, so the raw / `+E.164` /
    /// JID identity rule lives once in [`crate::whatsapp`] and both call it.
    /// Keeping a second copy here let the two drift, and the Cloud webhook was
    /// left comparing exactly while this path canonicalized.
    #[cfg(feature = "whatsapp-web")]
    fn phone_matches(entry: &str, phone: &str) -> bool {
        crate::whatsapp::phone_matches(entry, phone)
    }

    #[cfg(feature = "whatsapp-web")]
    fn normalize_phone_token(value: &str) -> Option<String> {
        crate::whatsapp::normalize_phone_token(value)
    }

    #[cfg(feature = "whatsapp-web")]
    fn lid_rejection_diagnostic(
        sender: &wacore_binary::jid::Jid,
        mapped_phone: Option<&str>,
    ) -> String {
        if !sender.is_lid() {
            return String::new();
        }
        if mapped_phone.is_none() {
            // Deliberately identifier-free. This string reaches `record!(WARN, ..)`, which the
            // repository fans out to the dashboard and the optional persisted JSONL, and
            // `identity_persist.rs` already sets the contract for exactly this class: a raw
            // linked-account identity is durable personal data and must not reach the log sink.
            // The failure REASON and the candidate count at the call site are what the operator
            // needs to act; the sender JID and the raw LID user value are not.
            //
            // The remedy also had to change. `allowed_numbers` is a V2 field that migrates into
            // `peer_groups` on load, so steering an operator to it pointed at a knob the current
            // config model no longer has.
            " (sender is a LID and LID→phone resolution returned None, so phone-number entries \
             cannot match it. Add this contact to the applicable \
             [peer_groups.<name>].external_peers, or wait for the in-memory LID cache to \
             populate for it.)"
                .to_string()
        } else {
            " (sender is LID; resolved phone did not match any allowlist entry)".to_string()
        }
    }

    /// Build normalized sender candidates from sender JID, optional alt JID, and optional LID->PN mapping.
    #[cfg(feature = "whatsapp-web")]
    fn sender_phone_candidates(
        sender: &wacore_binary::jid::Jid,
        sender_alt: Option<&wacore_binary::jid::Jid>,
        mapped_phone: Option<&str>,
    ) -> Vec<String> {
        let mut candidates = Vec::new();

        let mut add_candidate = |candidate: Option<String>| {
            if let Some(candidate) = candidate
                && !candidates.iter().any(|existing| existing == &candidate)
            {
                candidates.push(candidate);
            }
        };

        add_candidate(Self::normalize_phone_token(&sender.to_string()));
        if let Some(alt) = sender_alt {
            add_candidate(Self::normalize_phone_token(&alt.to_string()));
        }
        if let Some(mapped_phone) = mapped_phone {
            add_candidate(Self::normalize_phone_token(mapped_phone));
        }

        candidates
    }

    #[cfg(feature = "whatsapp-web")]
    async fn resolve_sender_allowlist(
        client: &whatsapp_rust::Client,
        sender: &wacore_binary::jid::Jid,
        sender_alt: Option<&wacore_binary::jid::Jid>,
        allowed_numbers: &[String],
    ) -> SenderAllowlistResolution {
        let mapped_phone = if sender.is_lid() {
            client
                .get_lid_pn_entry(sender)
                .await
                .ok()
                .flatten()
                .map(|entry| entry.phone_number.to_string())
        } else {
            None
        };
        let candidates = Self::sender_phone_candidates(sender, sender_alt, mapped_phone.as_deref());
        // Authorize the sender as one account first, so a deny naming any of
        // its numbers is not sidestepped by another number of the same sender.
        // Only then pick the number to report downstream.
        let candidate_refs: Vec<&str> = candidates.iter().map(String::as_str).collect();
        let allowed_phone = if Self::are_numbers_allowed_for_list(allowed_numbers, &candidate_refs)
        {
            candidates
                .iter()
                .find(|candidate| Self::is_number_allowed_for_list(allowed_numbers, candidate))
                .cloned()
        } else {
            None
        };

        SenderAllowlistResolution {
            mapped_phone,
            candidates,
            allowed_phone,
        }
    }

    /// Fan a delivered batch out to the per-message handler, in arrival order.
    ///
    /// whatsapp-rust 0.7 replaced `Event::Message(msg, info)` with
    /// `Event::Messages(batch)`. Live traffic still arrives as a batch of one;
    /// an offline drain delivers one batch per durable commit. Each message is
    /// dispatched through its own call so that a per-message early return skips
    /// only that message: inlining the loop into the handler body would turn
    /// each of its early returns into "abandon the rest of the batch".
    #[cfg(feature = "whatsapp-web")]
    async fn handle_inbound_message_event(
        event: &wacore::types::events::Event,
        client: &whatsapp_rust::Client,
        context: &WhatsAppInboundContext,
    ) {
        let wacore::types::events::Event::Messages(batch) = event else {
            return;
        };

        for inbound in batch.messages.iter() {
            Self::handle_one_inbound_message(&inbound.message, &inbound.info, client, context)
                .await;
        }
    }

    #[cfg(feature = "whatsapp-web")]
    async fn handle_one_inbound_message(
        msg: &waproto::whatsapp::Message,
        info: &wacore::types::message::MessageInfo,
        client: &whatsapp_rust::Client,
        context: &WhatsAppInboundContext,
    ) {
        use wacore::proto_helpers::MessageExt;
        use wacore_binary::jid::JidExt as _;

        let sender_jid = info.source.sender.clone();
        let sender_alt = info.source.sender_alt.clone();
        let sender = sender_jid.user().to_string();
        let chat = info.source.chat.to_string();

        let allowed_peers = (context.peer_resolver)();
        let sender_resolution = Self::resolve_sender_allowlist(
            client,
            &sender_jid,
            sender_alt.as_ref(),
            &allowed_peers,
        )
        .await;
        let mapped_phone = sender_resolution.mapped_phone;
        let sender_candidates = sender_resolution.candidates;
        let normalized = sender_resolution.allowed_phone;

        let is_group = info.source.is_group;
        let reply_target = Self::compute_reply_target(&chat);

        // Computed HERE rather than further down, because the approval-reply
        // interception below needs the same verdict the conversation path uses
        // and sits ahead of where that path derives it.
        let self_chat = self_chat_verdict(
            &context.mode,
            context.self_chat_mode,
            is_group,
            sender_jid.user(),
            &chat,
            info.source.is_from_me,
        );

        // Business-mode `fromMe` events are delivery mirrors for messages sent
        // by the linked account, not new user input. Reject them before either
        // approval handling or `ChannelMessage` construction so their chat JID
        // cannot grant the direct-message reply-intent bypass downstream.
        if context.mode == zeroclaw_config::schema::WhatsAppWebMode::Business
            && info.source.is_from_me
        {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"chat": chat, "sender": sender})),
                "ignoring fromMe delivery mirror in business mode"
            );
            return;
        }

        // ── Approval-reply interception ──
        //
        // Must live here rather than in the gateway: the generic resolver at
        // zeroclaw-gateway lib.rs sits inside the Cloud WEBHOOK handler, and
        // Web never reaches that endpoint because it runs its own listen().
        // So the token is intercepted on this inbound path, the way discord
        // and signal do it.
        //
        // Ahead of the agent dispatch, because an approval reply is a control
        // message and must not also be handled as conversation.
        if let Some((token, response)) = msg
            .text_content()
            .and_then(crate::util::parse_approval_reply)
        {
            // `normalized` is Some only when a sender candidate matched
            // an authorized peer, so it IS the authorization signal, already
            // computed above for the message path.
            match resolve_approval_reply_with_group_admission(
                &token,
                response,
                &context.alias,
                &reply_target,
                is_group,
                normalized.is_some(),
                context.allowed_groups_resolver.as_ref(),
                &context.group_policy,
                &context.dm_policy,
                self_chat,
            )
            .await
            {
                Ok(()) => {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({"chat": reply_target})),
                        "approval reply accepted"
                    );
                    return;
                }
                Err(ApprovalRefusal::UnknownToken) => {
                    // Not ours, already answered, or timed out. Fall through:
                    // a bare 6-char word plus "yes" is plausible conversation.
                }
                Err(refusal) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "denial": format!("{:?}", refusal),
                                "chat": reply_target,
                                "is_group": is_group,
                            })),
                        "rejecting unauthorized approval reply"
                    );
                    return;
                }
            }
        }

        let allowed_groups = (context.allowed_groups_resolver)();
        if is_group && !is_group_chat_allowed(&chat, &allowed_groups, &context.group_policy) {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({ "chat": chat })),
                "dropping group message: chat not in allowed_groups"
            );
            return;
        }

        // ── Personal-mode sender semantics ──
        //
        // Self-chat and fromMe handling stay personal-only: they describe an
        // operator's own linked device talking to itself, and the self-chat
        // exception is a personal-mode affordance. The chat-type policies
        // further down are NOT personal-only and run under both modes.
        let operator_self_chat = self_chat == SelfChatVerdict::Admitted;
        if context.mode == zeroclaw_config::schema::WhatsAppWebMode::Personal {
            if self_chat == SelfChatVerdict::Disabled {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "ignoring self-chat message (self_chat_mode=false)"
                );
                return;
            }
            if self_chat == SelfChatVerdict::NotSelfChat
                && info.source.is_from_me
                && !fromme_outside_self_chat_is_operator_trigger(
                    is_group,
                    &context.dm_mention_patterns,
                    &context.group_mention_patterns,
                    msg.text_content().unwrap_or(""),
                )
            {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"chat": chat, "sender": sender})),
                    "ignoring fromMe message outside self-chat thread (chat=, sender=)"
                );
                return;
            }
        }

        // ── Chat-type policy, enforced under BOTH modes ──
        //
        // This block used to sit inside the personal-mode branch above, so a
        // business-mode deployment validated dm_policy and group_policy and
        // then never consulted either one.
        match composed_chat_policy_decision(
            &context.mode,
            operator_self_chat,
            is_group,
            &context.group_policy,
            &context.dm_policy,
            normalized.is_some(),
        ) {
            ChatPolicyDecision::Admit => {}
            ChatPolicyDecision::DropGroupIgnored => {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "ignoring group message (group_policy=ignore)"
                );
                return;
            }
            ChatPolicyDecision::DropDmIgnored => {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "ignoring DM (dm_policy=ignore)"
                );
                return;
            }
            ChatPolicyDecision::DropUnrecognizedSender => {
                let lid_diag = Self::lid_rejection_diagnostic(&sender_jid, mapped_phone.as_deref());
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!(
                        "message from unrecognized sender not in allowed list (candidates_count={}){}",
                        sender_candidates.len(),
                        lid_diag
                    )
                );
                return;
            }
        }

        let normalized = normalized.unwrap_or_else(|| sender.clone());
        let conversation_scope = Self::group_context_scope(context.passive_group_context, is_group);
        let mut passive_context = false;
        let text_content = msg.text_content().unwrap_or("").trim().to_string();
        let mut content = Self::media_fallback_content(text_content, msg);

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "WhatsApp Web message received (sender_len={}, chat_len={}, content_len={})",
                sender.len(),
                chat.len(),
                content.len()
            )
        );
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!("WhatsApp Web message content: {}", content)
        );

        if context.mention_only && is_group {
            let bot_phone = context.bot_phone.lock();
            let bot_lid = context.bot_lid.lock();
            if bot_phone.is_some() || bot_lid.is_some() {
                let bp = bot_phone.as_deref().unwrap_or("");
                let bl = bot_lid.as_deref();
                let addressed = Self::is_message_addressed_to_bot(msg, &content, bp, bl);
                if Self::should_record_passive_group_context(
                    context.passive_group_context,
                    is_group,
                    addressed,
                ) {
                    passive_context = true;
                } else if !addressed {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        "ignoring group message not addressed to bot"
                    );
                    return;
                }
            } else {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "mention_only active but bot identity unknown, skipping group msg"
                );
                return;
            }
        }

        let passive_from_mention_gating_possible = Self::should_record_passive_group_context(
            context.passive_group_context,
            is_group,
            false,
        );
        if !passive_context && passive_from_mention_gating_possible {
            match super::whatsapp::WhatsAppChannel::apply_mention_gating(
                &context.dm_mention_patterns,
                &context.group_mention_patterns,
                &content,
                is_group,
            ) {
                Some(c) => content = c,
                None => passive_context = true,
            }
        }

        if passive_context {
            if content.is_empty() {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    &format!("ignoring empty passive group context from {}", normalized)
                );
                return;
            }
            Self::send_inbound_channel_message(
                &context.tx,
                context.alias.as_ref(),
                &normalized,
                reply_target,
                content,
                Vec::new(),
                true,
                conversation_scope,
            )
            .await;
            return;
        }

        let voice_text = if let Some(audio) = msg.audio_message.as_option() {
            let is_ptt = audio.ptt == Some(true);
            let non_ptt_enabled = context
                .transcription_config
                .as_ref()
                .is_some_and(|c| c.transcribe_non_ptt_audio);
            if is_ptt || non_ptt_enabled {
                Self::try_transcribe_voice_note(
                    client,
                    audio,
                    context.transcription_config.as_ref(),
                    context.transcription_manager.as_deref(),
                )
                .await
            } else {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    &format!("ignoring non-PTT audio message from {}", normalized)
                );
                None
            }
        } else {
            None
        };

        if let Some(ref vt) = voice_text {
            if let Ok(mut vs) = context.voice_chats.lock() {
                vs.insert(reply_target.clone());
            }
            content = format!("[Voice] {vt}");
        } else if let Ok(mut vs) = context.voice_chats.lock() {
            vs.remove(&reply_target);
        }
        content = Self::media_fallback_content(content, msg);

        if content.is_empty() {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                &format!("ignoring empty or non-text message from {}", normalized)
            );
            return;
        }

        if !passive_from_mention_gating_possible {
            content = match super::whatsapp::WhatsAppChannel::apply_mention_gating(
                &context.dm_mention_patterns,
                &context.group_mention_patterns,
                &content,
                is_group,
            ) {
                Some(c) => c,
                None => {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({"normalized": normalized})),
                        "message from did not match mention patterns, dropping"
                    );
                    return;
                }
            };
        }

        let mut attachments = Vec::new();
        Self::collect_media_attachments(client, msg, "", false, &mut attachments).await;
        if let Some(quoted) = Self::extract_quoted_message(msg) {
            Self::collect_media_attachments(client, quoted, "quoted-", true, &mut attachments)
                .await;
        }

        Self::send_inbound_channel_message(
            &context.tx,
            context.alias.as_ref(),
            &normalized,
            reply_target,
            content,
            attachments,
            false,
            conversation_scope,
        )
        .await;
    }

    #[cfg(feature = "whatsapp-web")]
    fn compute_reply_target(chat_jid: &str) -> String {
        // Pass through unchanged - library handles LID resolution internally
        chat_jid.to_string()
    }

    /// Resolve an outbound recipient. With whatsapp-rust 0.6+ and
    /// LID JIDs are handled internally by the library, so we pass through unchanged.
    #[cfg(feature = "whatsapp-web")]
    fn resolve_outbound_recipient(recipient: &str) -> String {
        // Pass through unchanged - library handles LID resolution internally
        recipient.trim().to_string()
    }

    /// Normalize phone number to E.164 format
    #[cfg(feature = "whatsapp-web")]
    fn normalize_phone(&self, phone: &str) -> String {
        if let Some(normalized) = Self::normalize_phone_token(phone) {
            return normalized;
        }

        let trimmed = phone.trim();
        let user_part = trimmed
            .split_once('@')
            .map(|(user, _)| user)
            .unwrap_or(trimmed);
        let normalized_user = user_part.trim_start_matches('+');
        format!("+{normalized_user}")
    }

    /// Whether the recipient string is a WhatsApp JID (contains a domain suffix).
    #[cfg(feature = "whatsapp-web")]
    fn is_jid(recipient: &str) -> bool {
        recipient.trim().contains('@')
    }

    /// Render a WhatsApp pairing QR payload into terminal-friendly text.
    #[cfg(feature = "whatsapp-web")]
    fn render_pairing_qr(code: &str) -> Result<String> {
        let payload = code.trim();
        if payload.is_empty() {
            anyhow::bail!("QR payload is empty");
        }

        let qr = qrcode::QrCode::new(payload.as_bytes()).map_err(|err| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", err)})),
                "Failed to encode WhatsApp Web QR payload"
            );
            anyhow::Error::msg(format!("Failed to encode WhatsApp Web QR payload: {err}"))
        })?;

        Ok(qr
            .render::<qrcode::render::unicode::Dense1x2>()
            .quiet_zone(true)
            .build())
    }

    #[cfg(feature = "whatsapp-web")]
    fn recipient_to_jid(&self, recipient: &str) -> Result<wacore_binary::jid::Jid> {
        let trimmed = recipient.trim();
        if trimmed.is_empty() {
            anyhow::bail!("Recipient cannot be empty");
        }

        if trimmed.contains('@') {
            return trimmed.parse::<wacore_binary::jid::Jid>().map_err(|e| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "trimmed": trimmed,
                            "error": format!("{}", e),
                        })),
                    "whatsapp_web: invalid JID"
                );
                anyhow::Error::msg(format!("Invalid WhatsApp JID `{trimmed}`: {e}"))
            });
        }

        let digits: String = trimmed.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            anyhow::bail!("Recipient `{trimmed}` does not contain a valid phone number");
        }

        Ok(wacore_binary::jid::Jid::pn(digits))
    }

    // ── Reconnect state-machine helpers (used by listen() and tested directly) ──

    /// Reconnect retry constants.
    const MAX_RETRIES: u32 = 10;
    const BASE_DELAY_SECS: u64 = 3;
    const MAX_DELAY_SECS: u64 = 300;

    /// Compute the exponential-backoff delay for a given 1-based attempt number.
    /// Doubles each attempt from `BASE_DELAY_SECS`, capped at `MAX_DELAY_SECS`.
    fn compute_retry_delay(attempt: u32) -> u64 {
        std::cmp::min(
            Self::BASE_DELAY_SECS.saturating_mul(2u64.saturating_pow(attempt.saturating_sub(1))),
            Self::MAX_DELAY_SECS,
        )
    }

    /// Determine whether session files should be purged.
    /// Returns `true` only when `Event::LoggedOut` was explicitly observed.
    fn should_purge_session(session_revoked: &std::sync::atomic::AtomicBool) -> bool {
        session_revoked.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Record a reconnect attempt and return `(attempt_number, exceeded_max)`.
    fn record_retry(retry_count: &std::sync::atomic::AtomicU32) -> (u32, bool) {
        let attempts = retry_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        (attempts, attempts > Self::MAX_RETRIES)
    }

    /// Reset the retry counter (called on `Event::Connected`).
    fn reset_retry(retry_count: &std::sync::atomic::AtomicU32) {
        retry_count.store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// Expand `~` in a configured `session_path`. Single source of truth
    /// for the on-disk location — the run loop and the readiness probe
    /// must agree on the file they are looking at.
    fn expand_session_path(session_path: &str) -> String {
        shellexpand::tilde(session_path).to_string()
    }

    /// Channel-owned persisted-login probe: reports whether the session
    /// database at the configured `session_path` holds a device linked to a
    /// WhatsApp account (`device.pn` written by a completed QR pairing).
    /// Stricter than the run loop's resume check on purpose — a channel
    /// waiting for its QR scan persists an unregistered device row, which
    /// must not read as an authenticated login. Read-only; never creates
    /// the database or its sidecar files.
    pub fn has_persisted_session(session_path: &str) -> bool {
        if session_path.is_empty() {
            return false;
        }
        super::whatsapp_storage::persisted_device_exists(Self::expand_session_path(session_path))
    }

    /// Return the session file paths to remove (primary + WAL + SHM sidecars).
    fn session_file_paths(expanded_session_path: &str) -> [String; 3] {
        [
            expanded_session_path.to_string(),
            format!("{expanded_session_path}-wal"),
            format!("{expanded_session_path}-shm"),
        ]
    }

    /// Channel-owned relink hook: delete the persisted session so the next
    /// channel start finds no device and begins a fresh QR pairing.
    ///
    /// Removes the same triple the logged-out purge path removes —
    /// [`Self::session_file_paths`] is the single source of truth for both.
    /// Returns the paths actually removed; already absent files are not an
    /// error, so relinking an unpaired channel is a safe no-op that returns
    /// an empty list. Never creates the database.
    ///
    /// This only clears disk state. A currently running channel keeps its
    /// live connection until it is restarted; callers own scheduling that
    /// restart (e.g. a daemon reload).
    pub fn clear_persisted_session(session_path: &str) -> std::io::Result<Vec<String>> {
        let mut removed = Vec::new();
        if session_path.is_empty() {
            return Ok(removed);
        }
        let expanded = Self::expand_session_path(session_path);
        for path in Self::session_file_paths(&expanded) {
            match std::fs::remove_file(&path) {
                Ok(()) => removed.push(path),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(removed)
    }

    /// Attempt to download and transcribe a WhatsApp voice note.
    /// Returns `None` if transcription is disabled, download fails, or
    /// transcription fails (all logged as warnings).
    #[cfg(feature = "whatsapp-web")]
    async fn try_transcribe_voice_note(
        client: &whatsapp_rust::Client,
        audio: &waproto::whatsapp::message::AudioMessage,
        transcription_config: Option<&zeroclaw_config::schema::TranscriptionConfig>,
        transcription_manager: Option<&super::transcription::TranscriptionManager>,
    ) -> Option<String> {
        let config = transcription_config?;
        let manager = transcription_manager?;

        // Enforce duration limit
        if let Some(seconds) = audio.seconds
            && u64::from(seconds) > config.max_duration_secs
        {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                &format!(
                    "skipping voice note ({}s exceeds {}s limit)",
                    seconds, config.max_duration_secs
                )
            );
            return None;
        }

        // Download the encrypted audio
        use whatsapp_rust::download::Downloadable;
        let audio_data = match client.download(audio as &dyn Downloadable).await {
            Ok(data) => data,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "failed to download voice note"
                );
                return None;
            }
        };

        // Determine filename from mimetype for transcription API
        let file_name = match audio.mimetype.as_deref() {
            Some(m) if m.contains("opus") || m.contains("ogg") => "voice.ogg",
            Some(m) if m.contains("mp4") || m.contains("m4a") => "voice.m4a",
            Some(m) if m.contains("mpeg") || m.contains("mp3") => "voice.mp3",
            Some(m) if m.contains("webm") => "voice.webm",
            _ => "voice.ogg", // WhatsApp default
        };

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "transcribing voice note ({} bytes, file={})",
                audio_data.len(),
                file_name
            )
        );

        match manager.transcribe(&audio_data, file_name).await {
            Ok(text) if text.trim().is_empty() => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "voice transcription returned empty text, skipping"
                );
                None
            }
            Ok(text) => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    &format!("voice note transcribed ({} chars)", text.len())
                );
                Some(text)
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "voice transcription failed"
                );
                None
            }
        }
    }

    #[cfg(feature = "whatsapp-web")]
    fn extract_context_info(
        msg: &waproto::whatsapp::Message,
    ) -> Option<&waproto::whatsapp::ContextInfo> {
        use wacore::proto_helpers::MessageExt;
        let base = msg.get_base_message();

        // waproto 0.7 renders optional submessages as `MessageField` rather than
        // `Option<Box<_>>`, so each arm reads through `as_option()`. Order is
        // load-bearing: the first populated variant wins, as before.
        if let Some(ext) = base.extended_text_message.as_option()
            && let Some(ctx) = ext.context_info.as_option()
        {
            return Some(ctx);
        }
        if let Some(img) = base.image_message.as_option()
            && let Some(ctx) = img.context_info.as_option()
        {
            return Some(ctx);
        }
        if let Some(vid) = base.video_message.as_option()
            && let Some(ctx) = vid.context_info.as_option()
        {
            return Some(ctx);
        }
        if let Some(doc) = base.document_message.as_option()
            && let Some(ctx) = doc.context_info.as_option()
        {
            return Some(ctx);
        }
        if let Some(aud) = base.audio_message.as_option()
            && let Some(ctx) = aud.context_info.as_option()
        {
            return Some(ctx);
        }
        if let Some(stk) = base.sticker_message.as_option()
            && let Some(ctx) = stk.context_info.as_option()
        {
            return Some(ctx);
        }

        None
    }

    #[cfg(feature = "whatsapp-web")]
    fn extract_quoted_message(
        msg: &waproto::whatsapp::Message,
    ) -> Option<&waproto::whatsapp::Message> {
        Self::extract_context_info(msg).and_then(|ctx| ctx.quoted_message.as_option())
    }

    #[cfg(feature = "whatsapp-web")]
    fn mime_extension(mime: &str, fallback: &str) -> String {
        let subtype = mime
            .split(';')
            .next()
            .and_then(|clean| clean.split_once('/').map(|(_, subtype)| subtype))
            .and_then(|subtype| subtype.split('+').next())
            .filter(|subtype| {
                !subtype.is_empty()
                    && subtype
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '.')
            })
            .unwrap_or(fallback);

        match subtype {
            "jpeg" => "jpg".to_string(),
            "svg+xml" => "svg".to_string(),
            other => other.to_string(),
        }
    }

    #[cfg(feature = "whatsapp-web")]
    async fn push_downloaded_attachment(
        client: &whatsapp_rust::Client,
        downloadable: &dyn whatsapp_rust::download::Downloadable,
        file_name: String,
        mime_type: Option<String>,
        attachments: &mut Vec<MediaAttachment>,
    ) {
        let data = match client.download(downloadable).await {
            Ok(data) => data,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "file": file_name,
                            "error": format!("{}", e),
                        })),
                    "failed to download WhatsApp media attachment"
                );
                return;
            }
        };

        attachments.push(MediaAttachment {
            file_name,
            data,
            mime_type,
            marker: None,
        });
    }

    #[cfg(feature = "whatsapp-web")]
    async fn collect_media_attachments(
        client: &whatsapp_rust::Client,
        msg: &waproto::whatsapp::Message,
        file_prefix: &str,
        include_audio: bool,
        attachments: &mut Vec<MediaAttachment>,
    ) {
        use wacore::proto_helpers::MessageExt;
        use whatsapp_rust::download::Downloadable;

        let base = msg.get_base_message();

        if let Some(image) = base.image_message.as_option() {
            let mime = image
                .mimetype
                .clone()
                .unwrap_or_else(|| "image/jpeg".to_string());
            let file_name = format!(
                "{file_prefix}whatsapp-image.{}",
                Self::mime_extension(&mime, "jpg")
            );
            Self::push_downloaded_attachment(
                client,
                image as &dyn Downloadable,
                file_name,
                Some(mime),
                attachments,
            )
            .await;
        }

        if let Some(video) = base.video_message.as_option() {
            let mime = video
                .mimetype
                .clone()
                .unwrap_or_else(|| "video/mp4".to_string());
            let file_name = format!(
                "{file_prefix}whatsapp-video.{}",
                Self::mime_extension(&mime, "mp4")
            );
            Self::push_downloaded_attachment(
                client,
                video as &dyn Downloadable,
                file_name,
                Some(mime),
                attachments,
            )
            .await;
        }

        if include_audio && let Some(audio) = base.audio_message.as_option() {
            let mime = audio
                .mimetype
                .clone()
                .unwrap_or_else(|| "audio/ogg".to_string());
            let file_name = format!(
                "{file_prefix}whatsapp-audio.{}",
                Self::mime_extension(&mime, "ogg")
            );
            Self::push_downloaded_attachment(
                client,
                audio as &dyn Downloadable,
                file_name,
                Some(mime),
                attachments,
            )
            .await;
        }

        if let Some(sticker) = base.sticker_message.as_option() {
            let mime = sticker
                .mimetype
                .clone()
                .unwrap_or_else(|| "image/webp".to_string());
            let file_name = format!(
                "{file_prefix}whatsapp-sticker.{}",
                Self::mime_extension(&mime, "webp")
            );
            Self::push_downloaded_attachment(
                client,
                sticker as &dyn Downloadable,
                file_name,
                Some(mime),
                attachments,
            )
            .await;
        }
    }

    #[cfg(feature = "whatsapp-web")]
    fn media_fallback_content(content: String, msg: &waproto::whatsapp::Message) -> String {
        if !content.is_empty() {
            return content;
        }

        use wacore::proto_helpers::MessageExt;
        let base = msg.get_base_message();

        if base.sticker_message.is_set() {
            return "[Sticker]".to_string();
        }
        if base.image_message.is_set() {
            return "[Image]".to_string();
        }
        if base.video_message.is_set() {
            return "[Video]".to_string();
        }
        if base.document_message.is_set() {
            return "[Document]".to_string();
        }
        if let Some(loc) = base.location_message.as_option() {
            // Live locations are silently ignored — they stream
            // periodic updates and have no meaningful static content.
            if loc.is_live == Some(true) {
                return String::new();
            }
            let lat = match loc.degrees_latitude {
                Some(l) => l,
                None => return String::new(),
            };
            let lng = match loc.degrees_longitude {
                Some(l) => l,
                None => return String::new(),
            };
            return crate::util::format_location_content(lat, lng, loc.name.as_deref());
        }

        String::new()
    }

    /// Hold `content` as this chat's pending voice reply, unless something says
    /// it must not be spoken. Returns the reason it was not queued.
    ///
    /// The refusal happens before the queue is touched, which is the whole
    /// point: a suppressed notice must not overwrite the conversational reply
    /// already waiting to be spoken, nor push its timer out by arriving.
    #[cfg(feature = "whatsapp-web")]
    fn queue_pending_voice(
        &self,
        recipient: &str,
        content: &str,
        suppress_voice: bool,
    ) -> Option<&'static str> {
        let skip = Self::voice_queue_skip_reason(suppress_voice, content);
        if skip.is_none()
            && let Ok(mut pv) = self.pending_voice.lock()
        {
            pv.insert(
                recipient.to_string(),
                (content.to_string(), std::time::Instant::now()),
            );
        }
        skip
    }

    /// Why an outbound message must not join the automatic voice queue, or
    /// `None` when it may.
    ///
    /// `suppress_voice` is asked first and on its own terms: the sender of a
    /// system notice or of an explicitly text-only reply has already decided,
    /// and that decision does not depend on what the text looks like. It is
    /// answered before the queue is touched, so a suppressed message cannot
    /// replace the conversational reply already waiting there, nor push its
    /// timer out by arriving.
    #[cfg(feature = "whatsapp-web")]
    fn voice_queue_skip_reason(suppress_voice: bool, content: &str) -> Option<&'static str> {
        if suppress_voice {
            return Some("suppress_voice");
        }
        crate::util::voice_reply_skip_reason(content)
    }

    #[cfg(feature = "whatsapp-web")]
    fn group_context_scope(
        passive_group_context: bool,
        is_group: bool,
    ) -> ChannelConversationScope {
        if passive_group_context && is_group {
            ChannelConversationScope::ReplyTarget
        } else {
            ChannelConversationScope::Sender
        }
    }

    #[cfg(feature = "whatsapp-web")]
    fn should_record_passive_group_context(
        passive_group_context: bool,
        is_group: bool,
        addressed_to_bot: bool,
    ) -> bool {
        passive_group_context && is_group && !addressed_to_bot
    }

    #[cfg(feature = "whatsapp-web")]
    async fn send_inbound_channel_message(
        tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
        alias: &str,
        sender: &str,
        reply_target: String,
        content: String,
        attachments: Vec<MediaAttachment>,
        passive_context: bool,
        conversation_scope: ChannelConversationScope,
    ) {
        if let Err(e) = tx
            .send(ChannelMessage {
                id: uuid::Uuid::new_v4().to_string(),
                channel: "whatsapp".to_string(),
                channel_alias: Some(alias.to_string()),
                sender: sender.to_string(),
                platform_sender_id: None,
                // Reply to the originating chat JID (DM or group), passed
                // through unchanged (library handles LID addressing internally).
                reply_target,
                content,
                timestamp: chrono::Utc::now().timestamp() as u64,
                thread_ts: None,
                interruption_scope_id: None,
                attachments,
                subject: None,
                internal_sop_event: None,
                passive_context,
                explicitly_addressed: false,
                conversation_scope,
                references: Vec::new(),
                voice_origin: false,
            })
            .await
        {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "failed to send message to channel"
            );
        }
    }

    /// Synthesize text to speech and send as a WhatsApp voice note (static version for spawned tasks).
    #[cfg(feature = "whatsapp-web")]
    async fn synthesize_voice_static(
        client: &whatsapp_rust::Client,
        to: &wacore_binary::jid::Jid,
        text: &str,
        tts_manager: &super::tts::TtsManager,
    ) -> Result<()> {
        let audio_bytes = tts_manager.synthesize_opus(text).await?;
        let audio_len = audio_bytes.len();
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!("TTS: synthesized {} bytes of audio", audio_len)
        );

        if audio_bytes.is_empty() {
            anyhow::bail!("TTS returned empty audio");
        }

        use wacore::download::MediaType;
        use whatsapp_rust::upload::UploadOptions;
        let upload = client
            .upload(audio_bytes, MediaType::Audio, UploadOptions::default())
            .await
            .map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "Failed to upload TTS audio"
                );
                anyhow::Error::msg(format!("Failed to upload TTS audio: {e}"))
            })?;

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "TTS: uploaded audio (url_len={}, file_length={})",
                upload.url.len(),
                upload.file_length
            )
        );

        // Estimate duration from file size: Opus at ~32 kbps → bytes / 4000 ≈ seconds
        #[allow(clippy::cast_possible_truncation)]
        let estimated_seconds = std::cmp::max(1, (upload.file_length / 4000) as u32);

        // UploadResponse cryptographic fields are `[u8; 32]`, and the
        // `_vec()` helpers are gone. Copy them out before consuming the
        // strings so the partial-move on `upload.direct_path` doesn't bite.
        let media_key = upload.media_key.to_vec();
        let file_enc_sha256 = upload.file_enc_sha256.to_vec();
        let file_sha256 = upload.file_sha256.to_vec();
        let voice_msg = waproto::whatsapp::Message {
            audio_message: waproto::whatsapp::message::AudioMessage {
                url: Some(upload.url),
                direct_path: Some(upload.direct_path),
                media_key: Some(media_key),
                file_enc_sha256: Some(file_enc_sha256),
                file_sha256: Some(file_sha256),
                file_length: Some(upload.file_length),
                mimetype: Some("audio/ogg; codecs=opus".to_string()),
                ptt: Some(true),
                seconds: Some(estimated_seconds),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        };

        Box::pin(client.send_message(to.clone(), voice_msg))
            .await
            .map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "Failed to send voice note"
                );
                anyhow::Error::msg(format!("Failed to send voice note: {e}"))
            })?;
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "TTS: sent voice note ({} bytes, ~{}s)",
                audio_len, estimated_seconds
            )
        );
        Ok(())
    }

    #[cfg(feature = "whatsapp-web")]
    async fn send_media_marker(
        client: &whatsapp_rust::Client,
        to: &wacore_binary::jid::Jid,
        marker: &WhatsAppMediaMarker,
        path: &Path,
        document_thumbnails: bool,
    ) -> Result<()> {
        let bytes = tokio::fs::read(path)
            .await
            .with_context(|| format!("read WhatsApp marker target {}", path.display()))?;
        if bytes.is_empty() {
            anyhow::bail!("WhatsApp marker target {} is empty", path.display());
        }

        let media_type = marker.kind.media_type();
        let mime = marker.kind.mime_for_path(path);
        let image_preview = if matches!(marker.kind, WhatsAppMediaKind::Image) {
            image_preview(bytes.clone()).await
        } else {
            None
        };

        use whatsapp_rust::upload::UploadOptions;
        // The preview is rendered and uploaded while the file uploads, so it
        // adds latency only when it outlasts the upload, bounded by the
        // preview timeouts. Its upload is encrypted under the document's
        // media key, so the key is chosen here rather than by the library.
        let wants_preview = wants_document_preview(document_thumbnails, marker.kind, &mime);
        let media_key: [u8; 32] = rand::random();
        let options = if wants_preview {
            UploadOptions::default().with_media_key(media_key)
        } else {
            UploadOptions::default()
        };
        let (upload, preview) =
            tokio::join!(Box::pin(client.upload(bytes, media_type, options)), async {
                if wants_preview {
                    document_preview(client, path, &media_key).await
                } else {
                    DocumentPreview::default()
                }
            });
        let upload =
            upload.map_err(|e| anyhow::Error::msg(format!("WhatsApp media upload failed: {e}")))?;

        let media_key = upload.media_key.to_vec();
        let file_enc_sha256 = upload.file_enc_sha256.to_vec();
        let file_sha256 = upload.file_sha256.to_vec();
        let outgoing = match marker.kind {
            WhatsAppMediaKind::Image => {
                let mut image = waproto::whatsapp::message::ImageMessage {
                    url: Some(upload.url),
                    direct_path: Some(upload.direct_path),
                    media_key: Some(media_key),
                    file_enc_sha256: Some(file_enc_sha256),
                    file_sha256: Some(file_sha256),
                    file_length: Some(upload.file_length),
                    mimetype: Some(mime),
                    ..Default::default()
                };
                if let Some(preview) = image_preview {
                    preview.apply_to(&mut image);
                }
                waproto::whatsapp::Message {
                    image_message: image.into(),
                    ..Default::default()
                }
            }
            WhatsAppMediaKind::Video => waproto::whatsapp::Message {
                video_message: waproto::whatsapp::message::VideoMessage {
                    url: Some(upload.url),
                    direct_path: Some(upload.direct_path),
                    media_key: Some(media_key),
                    file_enc_sha256: Some(file_enc_sha256),
                    file_sha256: Some(file_sha256),
                    file_length: Some(upload.file_length),
                    mimetype: Some(mime),
                    ..Default::default()
                }
                .into(),
                ..Default::default()
            },
            WhatsAppMediaKind::Audio | WhatsAppMediaKind::Voice => {
                #[allow(clippy::cast_possible_truncation)]
                let estimated_seconds = std::cmp::max(1, (upload.file_length / 4000) as u32);
                waproto::whatsapp::Message {
                    audio_message: waproto::whatsapp::message::AudioMessage {
                        url: Some(upload.url),
                        direct_path: Some(upload.direct_path),
                        media_key: Some(media_key),
                        file_enc_sha256: Some(file_enc_sha256),
                        file_sha256: Some(file_sha256),
                        file_length: Some(upload.file_length),
                        mimetype: Some(mime),
                        ptt: Some(matches!(marker.kind, WhatsAppMediaKind::Voice)),
                        seconds: Some(estimated_seconds),
                        ..Default::default()
                    }
                    .into(),
                    ..Default::default()
                }
            }
            WhatsAppMediaKind::Document => {
                let file_name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("attachment")
                    .to_string();
                let mut document = waproto::whatsapp::message::DocumentMessage {
                    url: Some(upload.url),
                    direct_path: Some(upload.direct_path),
                    media_key: Some(media_key),
                    file_enc_sha256: Some(file_enc_sha256),
                    file_sha256: Some(file_sha256),
                    file_length: Some(upload.file_length),
                    mimetype: Some(mime),
                    file_name: Some(file_name.clone()),
                    title: Some(file_name),
                    ..Default::default()
                };
                preview.apply_to(&mut document);
                waproto::whatsapp::Message {
                    document_message: document.into(),
                    ..Default::default()
                }
            }
        };

        Box::pin(client.send_message(to.clone(), outgoing))
            .await
            .map_err(|e| anyhow::Error::msg(format!("WhatsApp media send failed: {e}")))?;
        Ok(())
    }

    /// Send a native location pin. No file read or media upload is involved —
    /// the coordinates and labels travel inline in a `LocationMessage`.
    #[cfg(feature = "whatsapp-web")]
    async fn send_location(
        client: &whatsapp_rust::Client,
        to: &wacore_binary::jid::Jid,
        loc: &WhatsAppLocation,
    ) -> Result<()> {
        let outgoing = waproto::whatsapp::Message {
            location_message: waproto::whatsapp::message::LocationMessage {
                degrees_latitude: Some(loc.lat),
                degrees_longitude: Some(loc.lng),
                name: loc.name.clone(),
                address: loc.address.clone(),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        };
        Box::pin(client.send_message(to.clone(), outgoing))
            .await
            .map_err(|e| anyhow::Error::msg(format!("WhatsApp location send failed: {e}")))?;
        Ok(())
    }

    /// The display name to push, or `None` when it is unset, blank, or
    /// already what the device announces.
    #[cfg(feature = "whatsapp-web")]
    fn push_name_to_apply<'a>(configured: Option<&'a str>, current: &str) -> Option<&'a str> {
        let desired = configured?.trim();
        (!desired.is_empty() && desired != current.trim()).then_some(desired)
    }

    // ── Mention detection helpers (used when mention_only is enabled) ──

    /// Extract digits from a JID string (e.g. "919211916069@s.whatsapp.net" -> "919211916069").
    #[cfg(feature = "whatsapp-web")]
    fn jid_digits(jid: &str) -> String {
        let user_part = jid.split_once('@').map(|(u, _)| u).unwrap_or(jid);
        let user_part = user_part
            .split_once(':')
            .map(|(u, _)| u)
            .unwrap_or(user_part);
        user_part.chars().filter(|c| c.is_ascii_digit()).collect()
    }

    #[cfg(feature = "whatsapp-web")]
    fn store_jid_digits(slot: &Arc<Mutex<Option<String>>>, jid: &str) -> Option<String> {
        let digits = Self::jid_digits(jid);
        if digits.is_empty() {
            None
        } else {
            *slot.lock() = Some(digits.clone());
            Some(digits)
        }
    }

    #[cfg(feature = "whatsapp-web")]
    fn jid_matches_bot(jid: &str, bot_phone: &str, bot_lid: Option<&str>) -> bool {
        let digits = Self::jid_digits(jid);
        !digits.is_empty()
            && ((!bot_phone.is_empty() && digits == bot_phone)
                || bot_lid.is_some_and(|lid| !lid.is_empty() && digits == lid))
    }

    /// Extract mentioned JIDs from the base (unwrapped) message's context_info.
    #[cfg(feature = "whatsapp-web")]
    fn extract_mentioned_jids(msg: &waproto::whatsapp::Message) -> Vec<String> {
        Self::extract_context_info(msg)
            .map(|ctx| ctx.mentioned_jid.clone())
            .unwrap_or_default()
    }

    /// Check whether the bot is mentioned -- either structurally or via text fallback.
    #[cfg(feature = "whatsapp-web")]
    fn contains_bot_mention(
        text: &str,
        mentioned_jids: &[String],
        bot_phone: &str,
        bot_lid: Option<&str>,
    ) -> bool {
        // 1. Structured: check if any mentioned_jid's digits match the bot's phone or LID digits
        for jid in mentioned_jids {
            if Self::jid_matches_bot(jid, bot_phone, bot_lid) {
                return true;
            }
        }

        // 2. Text fallback: word-boundary-aware match for @<bot_digits>.
        //    Scan all occurrences -- an earlier prefix false-match must not mask a later real mention.
        fn has_text_mention(text: &str, digits: &str) -> bool {
            if digits.is_empty() {
                return false;
            }

            let pattern = format!("@{digits}");
            let mut search_from = 0;
            while let Some(rel_pos) = text[search_from..].find(&pattern) {
                let pos = search_from + rel_pos;
                let after_idx = pos + pattern.len();
                let leading_ok = pos == 0
                    || text[..pos]
                        .chars()
                        .next_back()
                        .is_none_or(|ch| !ch.is_ascii_alphanumeric());
                let trailing_ok = text[after_idx..]
                    .chars()
                    .next()
                    .is_none_or(|ch| !ch.is_ascii_digit());
                if leading_ok && trailing_ok {
                    return true;
                }
                search_from = after_idx;
            }
            false
        }

        has_text_mention(text, bot_phone) || bot_lid.is_some_and(|lid| has_text_mention(text, lid))
    }

    /// Extract the author JID of the message quoted by a reply.
    #[cfg(feature = "whatsapp-web")]
    fn extract_reply_participant(msg: &waproto::whatsapp::Message) -> Option<&str> {
        Self::extract_context_info(msg).and_then(|ctx| ctx.participant.as_deref())
    }

    #[cfg(feature = "whatsapp-web")]
    fn is_reply_to_bot(
        msg: &waproto::whatsapp::Message,
        bot_phone: &str,
        bot_lid: Option<&str>,
    ) -> bool {
        Self::extract_reply_participant(msg)
            .is_some_and(|participant| Self::jid_matches_bot(participant, bot_phone, bot_lid))
    }

    #[cfg(feature = "whatsapp-web")]
    fn is_message_addressed_to_bot(
        msg: &waproto::whatsapp::Message,
        text: &str,
        bot_phone: &str,
        bot_lid: Option<&str>,
    ) -> bool {
        let mentioned_jids = Self::extract_mentioned_jids(msg);
        Self::contains_bot_mention(text, &mentioned_jids, bot_phone, bot_lid)
            || Self::is_reply_to_bot(msg, bot_phone, bot_lid)
    }
}

#[cfg(feature = "whatsapp-web")]
fn fromme_outside_self_chat_is_operator_trigger(
    is_group: bool,
    dm_mention_patterns: &[regex::Regex],
    group_mention_patterns: &[regex::Regex],
    text: &str,
) -> bool {
    let applicable = if is_group {
        group_mention_patterns
    } else {
        dm_mention_patterns
    };
    if applicable.is_empty() {
        return false;
    }
    super::whatsapp::WhatsAppChannel::text_matches_patterns(applicable, text)
}

/// WhatsApp JID domains that identify a one-to-one chat.
///
/// This is an allow-list rather than "anything that is not `@g.us`". Broadcast
/// lists, newsletters and call JIDs are not group chats either, yet they are
/// not direct messages, and treating an unrecognised future domain as a DM
/// would silently widen every `is_direct_message()` bypass downstream.
#[cfg(feature = "whatsapp-web")]
const DIRECT_MESSAGE_JID_DOMAINS: [&str; 2] = ["s.whatsapp.net", "lid"];

/// Whether an originating chat JID denotes a one-to-one conversation.
///
/// `reply_target` carries the originating chat JID unchanged, so the domain is
/// the authoritative signal: `@g.us` is a group, `@s.whatsapp.net` and `@lid`
/// are individual chats (the latter is WhatsApp's hidden-identity addressing).
#[cfg(feature = "whatsapp-web")]
fn is_direct_message_jid(chat_jid: &str) -> bool {
    chat_jid.rsplit_once('@').is_some_and(|(user, domain)| {
        !user.is_empty() && DIRECT_MESSAGE_JID_DOMAINS.contains(&domain)
    })
}

#[cfg(feature = "whatsapp-web")]
/// Whether a group chat may be processed.
///
/// An empty `allowed_groups` is NOT permission. A list that admits everything is
/// indistinguishable from a list nobody configured, so open group access has to
/// be asked for by name: `group_policy = "all"`. Under `"allowlist"` an empty
/// list admits nothing, which is what an allowlist means everywhere else in this
/// codebase; `"ignore"` also admits nothing.
///
/// A non-empty list still filters under every policy, so `"all"` widens the
/// default rather than overriding an explicit list.
fn is_group_chat_allowed(
    chat_jid: &str,
    allowed_groups: &[String],
    group_policy: &zeroclaw_config::schema::WhatsAppChatPolicy,
) -> bool {
    if allowed_groups.is_empty() {
        return matches!(
            group_policy,
            zeroclaw_config::schema::WhatsAppChatPolicy::All
        );
    }
    let chat_user = chat_jid
        .split_once('@')
        .map(|(user, _)| user)
        .unwrap_or(chat_jid);
    allowed_groups.iter().any(|entry| {
        let entry = entry.trim();
        !entry.is_empty() && (entry == chat_jid || entry == chat_user)
    })
}

/// What the chat-type policies decide about one inbound message.
///
/// Each variant maps to one of the log lines the message loop emits, so
/// extracting the decision does not flatten four distinct reasons into one.
#[cfg(feature = "whatsapp-web")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChatPolicyDecision {
    Admit,
    DropGroupIgnored,
    DropDmIgnored,
    DropUnrecognizedSender,
}

/// Apply `group_policy` or `dm_policy` to one message.
///
/// Deliberately takes no `WhatsAppWebMode`. These two policies apply under both
/// modes, and a function that cannot see the mode cannot quietly start
/// depending on it again, which is the defect this replaces: the decision used
/// to sit inside a personal-mode branch, so business mode validated both keys
/// and consulted neither.
#[cfg(feature = "whatsapp-web")]
fn chat_type_policy_decision(
    is_group: bool,
    group_policy: &zeroclaw_config::schema::WhatsAppChatPolicy,
    dm_policy: &zeroclaw_config::schema::WhatsAppChatPolicy,
    sender_recognized: bool,
) -> ChatPolicyDecision {
    use zeroclaw_config::schema::WhatsAppChatPolicy as Policy;

    let policy = if is_group { group_policy } else { dm_policy };
    match policy {
        Policy::Ignore => {
            if is_group {
                ChatPolicyDecision::DropGroupIgnored
            } else {
                ChatPolicyDecision::DropDmIgnored
            }
        }
        Policy::All => ChatPolicyDecision::Admit,
        Policy::Allowlist => {
            if sender_recognized {
                ChatPolicyDecision::Admit
            } else {
                ChatPolicyDecision::DropUnrecognizedSender
            }
        }
    }
}

/// What Personal-mode self-chat handling decides, before the chat-type
/// policies run.
///
/// Extracted so the conversation path and the approval-reply path reach the
/// same verdict from one place. An approval reply resolves a pending tool, so
/// it has to be admitted on the same terms as the message that requested it:
/// admitting it more narrowly strands a prompt the operator can never answer,
/// and admitting it more widely honours a reply in a thread the channel drops.
#[cfg(feature = "whatsapp-web")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelfChatVerdict {
    /// Not the operator's own thread, or not personal mode. The chat-type
    /// policies decide.
    NotSelfChat,
    /// The operator's own thread with the affordance on. Admitted whatever
    /// `dm_policy` says, which is what `self_chat_mode = true` means.
    Admitted,
    /// The operator's own thread with `self_chat_mode = false`. The channel
    /// ignores it, so a reply here must not resolve an approval either.
    Disabled,
}

/// Classify one inbound message against the personal-mode self-chat rules.
#[cfg(feature = "whatsapp-web")]
fn self_chat_verdict(
    mode: &zeroclaw_config::schema::WhatsAppWebMode,
    self_chat_mode: bool,
    is_group: bool,
    sender_user: &str,
    chat: &str,
    is_from_me: bool,
) -> SelfChatVerdict {
    if *mode != zeroclaw_config::schema::WhatsAppWebMode::Personal {
        return SelfChatVerdict::NotSelfChat;
    }
    let chat_user = chat.split_once('@').map(|(u, _)| u).unwrap_or(chat);
    if is_group || sender_user != chat_user || !is_from_me {
        return SelfChatVerdict::NotSelfChat;
    }
    if self_chat_mode {
        SelfChatVerdict::Admitted
    } else {
        SelfChatVerdict::Disabled
    }
}

/// Compose the mode-specific self-chat exception with the cross-mode policy.
///
/// The policy decision is always evaluated for business mode. Personal mode
/// may bypass it only for an operator self-chat that was recognized upstream.
#[cfg(feature = "whatsapp-web")]
fn composed_chat_policy_decision(
    mode: &zeroclaw_config::schema::WhatsAppWebMode,
    operator_self_chat: bool,
    is_group: bool,
    group_policy: &zeroclaw_config::schema::WhatsAppChatPolicy,
    dm_policy: &zeroclaw_config::schema::WhatsAppChatPolicy,
    sender_recognized: bool,
) -> ChatPolicyDecision {
    if *mode == zeroclaw_config::schema::WhatsAppWebMode::Personal && operator_self_chat {
        ChatPolicyDecision::Admit
    } else {
        chat_type_policy_decision(is_group, group_policy, dm_policy, sender_recognized)
    }
}

#[cfg(feature = "whatsapp-web")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WhatsAppMediaKind {
    Image,
    Document,
    Video,
    Audio,
    Voice,
}

/// Upper bound for each preview step (a tool run, or the whole preview
/// upload). The preview is best-effort, so a slow or stuck step must not hold
/// up the document it decorates.
///
/// For the upload this is a budget for the host loop, not the deadline that
/// stops a request: dropping a future cannot cancel work already handed to a
/// blocking client. The deadline that does the stopping is
/// [`DOCUMENT_THUMBNAIL_TIMEOUT_SECS`], enforced by the transport itself.
#[cfg(feature = "whatsapp-web")]
const DOCUMENT_PREVIEW_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Transport deadlines for one thumbnail upload request.
///
/// The thumbnail is decoration: a CDN host that accepts the connection and
/// then says nothing must cost the document a few seconds, not the send. These
/// are deliberately shorter than anything the document's own upload uses, and
/// they are enforced by the HTTP client, so the request ends rather than being
/// abandoned while it runs on.
#[cfg(feature = "whatsapp-web")]
const DOCUMENT_THUMBNAIL_TIMEOUT_SECS: u64 = 4;

#[cfg(feature = "whatsapp-web")]
const DOCUMENT_THUMBNAIL_CONNECT_TIMEOUT_SECS: u64 = 2;

/// Longest side of the JPEG carried inline in the message. Phones drop a
/// larger inline preview (a 600 px one never showed), so this stays at the
/// size the official apps send.
#[cfg(feature = "whatsapp-web")]
const DOCUMENT_PREVIEW_INLINE_SIDE: u32 = 96;

/// Longest side of the preview uploaded next to the document. Phones download
/// it to draw the card sharply; the inline one alone looks blurred there.
#[cfg(feature = "whatsapp-web")]
const DOCUMENT_PREVIEW_UPLOAD_SIDE: u32 = 480;

/// The inline JPEG travels in the message itself.
#[cfg(feature = "whatsapp-web")]
const DOCUMENT_PREVIEW_INLINE_MAX_BYTES: usize = 16 * 1024;

#[cfg(feature = "whatsapp-web")]
const DOCUMENT_PREVIEW_UPLOAD_MAX_BYTES: usize = 96 * 1024;

/// HKDF info for a document's uploaded preview. It is encrypted with the
/// document's own media key; `wacore::download::MediaType` has no variant for
/// it, so the upload is done here.
#[cfg(feature = "whatsapp-web")]
const DOCUMENT_THUMBNAIL_KEY_INFO: &[u8] = b"WhatsApp Document Thumbnail Keys";

#[cfg(feature = "whatsapp-web")]
const DOCUMENT_THUMBNAIL_UPLOAD_PATH: &str = "/mms/thumbnail-document";

/// First-page preview metadata for an outgoing document card. Every part may
/// be missing; whatever is present is attached.
#[cfg(feature = "whatsapp-web")]
#[derive(Debug, Default, PartialEq, Eq)]
struct DocumentPreview {
    inline: Option<DocumentThumbnail>,
    uploaded: Option<UploadedThumbnail>,
    page_count: Option<u32>,
}

#[cfg(feature = "whatsapp-web")]
#[derive(Debug, PartialEq, Eq)]
struct DocumentThumbnail {
    jpeg: Vec<u8>,
    width: u32,
    height: u32,
}

/// Where the uploaded preview lives and how the recipient checks it.
#[cfg(feature = "whatsapp-web")]
#[derive(Debug, PartialEq, Eq)]
struct UploadedThumbnail {
    direct_path: String,
    sha256: [u8; 32],
    enc_sha256: [u8; 32],
    width: u32,
    height: u32,
}

/// Page 1 rendered at both sizes, before anything is uploaded.
#[cfg(feature = "whatsapp-web")]
#[derive(Debug, Default, PartialEq, Eq)]
struct RenderedPreview {
    inline: Option<DocumentThumbnail>,
    upload: Option<DocumentThumbnail>,
    page_count: Option<u32>,
}

/// Media ciphertext as WhatsApp stores it: AES-256-CBC, then a 10-byte
/// truncated HMAC-SHA256, with the hashes the message carries.
#[cfg(feature = "whatsapp-web")]
#[derive(Debug)]
struct EncryptedBlob {
    data: Vec<u8>,
    sha256: [u8; 32],
    enc_sha256: [u8; 32],
}

#[cfg(feature = "whatsapp-web")]
impl DocumentPreview {
    fn apply_to(self, document: &mut waproto::whatsapp::message::DocumentMessage) {
        if let Some(inline) = self.inline {
            document.thumbnail_width = Some(inline.width);
            document.thumbnail_height = Some(inline.height);
            document.jpeg_thumbnail = Some(inline.jpeg);
        }
        // The declared size is that of the best preview on offer.
        if let Some(uploaded) = self.uploaded {
            document.thumbnail_width = Some(uploaded.width);
            document.thumbnail_height = Some(uploaded.height);
            document.thumbnail_direct_path = Some(uploaded.direct_path);
            document.thumbnail_sha256 = Some(uploaded.sha256.to_vec());
            document.thumbnail_enc_sha256 = Some(uploaded.enc_sha256.to_vec());
        }
        if let Some(page_count) = self.page_count {
            document.page_count = Some(page_count);
        }
    }
}

#[cfg(feature = "whatsapp-web")]
impl DocumentThumbnail {
    fn from_jpeg(jpeg: Vec<u8>, max_bytes: usize) -> Option<Self> {
        if jpeg.is_empty() || jpeg.len() > max_bytes {
            return None;
        }
        let (width, height) = jpeg_dimensions(&jpeg)?;
        Some(Self {
            jpeg,
            width,
            height,
        })
    }
}

/// Only PDF documents get a preview, and only when the operator enabled it.
#[cfg(feature = "whatsapp-web")]
fn wants_document_preview(enabled: bool, kind: WhatsAppMediaKind, mime: &str) -> bool {
    enabled && matches!(kind, WhatsAppMediaKind::Document) && mime == "application/pdf"
}

/// Render the preview of the PDF at `path` and upload its larger copy under
/// `media_key`, the key the document itself is uploaded with. Never fails:
/// each part that cannot be produced is left empty.
#[cfg(feature = "whatsapp-web")]
async fn document_preview(
    client: &whatsapp_rust::Client,
    path: &Path,
    media_key: &[u8; 32],
) -> DocumentPreview {
    let rendered = render_pdf_preview(path).await;
    let uploaded = match rendered.upload {
        Some(thumbnail) => {
            match tokio::time::timeout(
                DOCUMENT_PREVIEW_TIMEOUT,
                upload_document_thumbnail(client, media_key, thumbnail),
            )
            .await
            {
                Ok(Ok(uploaded)) => Some(uploaded),
                Ok(Err(reason)) => {
                    note_preview_skipped("thumbnail-upload", &reason);
                    None
                }
                Err(_) => {
                    note_preview_skipped(
                        "thumbnail-upload",
                        &format!("timed out after {DOCUMENT_PREVIEW_TIMEOUT:?}"),
                    );
                    None
                }
            }
        }
        None => None,
    };
    DocumentPreview {
        inline: rendered.inline,
        uploaded,
        page_count: rendered.page_count,
    }
}

/// Render page 1 of the PDF at `path` at the inline and upload sizes, and read
/// its page count, using `pdftoppm` and `pdfinfo` from poppler-utils. A
/// missing tool, a non-zero exit, a timeout, or unusable output leaves that
/// part empty.
#[cfg(feature = "whatsapp-web")]
async fn render_pdf_preview(path: &Path) -> RenderedPreview {
    // The official apps send a progressive inline JPEG; the uploaded copy is
    // a plain baseline one.
    let inline_args = pdftoppm_args(path, DOCUMENT_PREVIEW_INLINE_SIDE, true);
    let upload_args = pdftoppm_args(path, DOCUMENT_PREVIEW_UPLOAD_SIDE, false);
    let info_args = [path.as_os_str().to_os_string()];
    let (inline, upload, info) = tokio::join!(
        run_preview_tool("pdftoppm", &inline_args, DOCUMENT_PREVIEW_TIMEOUT),
        run_preview_tool("pdftoppm", &upload_args, DOCUMENT_PREVIEW_TIMEOUT),
        run_preview_tool("pdfinfo", &info_args, DOCUMENT_PREVIEW_TIMEOUT),
    );

    let page_count = match info {
        Ok(stdout) => pdfinfo_page_count(&String::from_utf8_lossy(&stdout)),
        Err(reason) => {
            note_preview_skipped("pdfinfo", &reason);
            None
        }
    };
    RenderedPreview {
        inline: rendered_thumbnail(inline, DOCUMENT_PREVIEW_INLINE_MAX_BYTES),
        upload: rendered_thumbnail(upload, DOCUMENT_PREVIEW_UPLOAD_MAX_BYTES),
        page_count,
    }
}

#[cfg(feature = "whatsapp-web")]
fn pdftoppm_args(path: &Path, max_side: u32, progressive: bool) -> Vec<std::ffi::OsString> {
    let jpeg_options = if progressive {
        "quality=70,progressive=y"
    } else {
        "quality=75"
    };
    let mut args: Vec<std::ffi::OsString> = [
        "-f",
        "1",
        "-l",
        "1",
        "-singlefile",
        "-jpeg",
        "-jpegopt",
        jpeg_options,
        "-scale-to",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    args.push(max_side.to_string().into());
    args.push(path.as_os_str().to_os_string());
    args
}

#[cfg(feature = "whatsapp-web")]
fn rendered_thumbnail(
    render: std::result::Result<Vec<u8>, String>,
    max_bytes: usize,
) -> Option<DocumentThumbnail> {
    match render {
        Ok(jpeg) => {
            let thumbnail = DocumentThumbnail::from_jpeg(jpeg, max_bytes);
            if thumbnail.is_none() {
                note_preview_skipped(
                    "pdftoppm",
                    "output was not a usable JPEG within the size cap",
                );
            }
            thumbnail
        }
        Err(reason) => {
            note_preview_skipped("pdftoppm", &reason);
            None
        }
    }
}

/// Encrypt `plaintext` the way WhatsApp media is encrypted, with keys expanded
/// from `media_key` under `info`.
#[cfg(feature = "whatsapp-web")]
fn encrypt_media_blob(
    media_key: &[u8; 32],
    info: &[u8],
    plaintext: &[u8],
) -> std::result::Result<EncryptedBlob, String> {
    use sha2::Digest as _;

    let mut expanded = [0u8; 112];
    wacore::crypto::hkdf_sha256_into(media_key, None, info, &mut expanded)
        .map_err(|e| format!("key expansion failed: {e}"))?;
    let (iv, rest) = expanded.split_at(16);
    let (cipher_key, rest) = rest.split_at(32);
    let mac_key = &rest[..32];

    let mut data = Vec::with_capacity(plaintext.len() + 32);
    wacore::libsignal::crypto::aes_256_cbc_encrypt_into(plaintext, cipher_key, iv, &mut data)
        .map_err(|e| format!("encryption failed: {e}"))?;
    let mac = wacore::libsignal::crypto::hmac_sha256_two_part(mac_key, iv, &data);
    data.extend_from_slice(&mac[..10]);
    Ok(EncryptedBlob {
        sha256: sha2::Sha256::digest(plaintext).into(),
        enc_sha256: sha2::Sha256::digest(&data).into(),
        data,
    })
}

/// Encrypt the preview under the document's key and upload it to the media
/// hosts, trying each once. The error never includes the upload URL, which
/// carries the media auth token.
#[cfg(feature = "whatsapp-web")]
async fn upload_document_thumbnail(
    client: &whatsapp_rust::Client,
    media_key: &[u8; 32],
    thumbnail: DocumentThumbnail,
) -> std::result::Result<UploadedThumbnail, String> {
    use base64::Engine as _;

    let blob = encrypt_media_blob(media_key, DOCUMENT_THUMBNAIL_KEY_INFO, &thumbnail.jpeg)?;
    let conn = client
        .refresh_media_conn(false)
        .await
        .map_err(|_| "could not get media hosts".to_string())?;
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(blob.enc_sha256);
    let body = bytes::Bytes::from(blob.data);
    let requests: Vec<wacore::net::HttpRequest> = conn
        .hosts
        .iter()
        .map(|host| document_thumbnail_upload_request(&host.hostname, &conn.auth, &token))
        .collect();
    // Not the client the library uses for media: that one runs a blocking
    // request under `spawn_blocking` with no deadline of its own, so a host
    // that stops responding leaves the request running after this future is
    // dropped. This one carries its own deadlines, and dropping it ends the
    // request.
    let http = zeroclaw_config::schema::build_channel_proxy_client_with_timeouts(
        "channel.whatsapp.document_thumbnail",
        None,
        DOCUMENT_THUMBNAIL_TIMEOUT_SECS,
        DOCUMENT_THUMBNAIL_CONNECT_TIMEOUT_SECS,
    );
    let direct_path = post_document_thumbnail(&http, &requests, body).await?;
    Ok(UploadedThumbnail {
        direct_path,
        sha256: blob.sha256,
        enc_sha256: blob.enc_sha256,
        width: thumbnail.width,
        height: thumbnail.height,
    })
}

/// POST the encrypted thumbnail to each media host in turn, returning the
/// `direct_path` the first one that accepts it reports.
///
/// Takes the composed requests rather than hostnames: the caller owns where
/// the thumbnail goes, and this owns what happens on the wire, which is what
/// lets the transport behaviour be exercised against a real socket. Everything
/// above it needs a paired client, and none of it decides what a host that
/// goes quiet costs the document.
#[cfg(feature = "whatsapp-web")]
async fn post_document_thumbnail(
    http: &reqwest::Client,
    requests: &[wacore::net::HttpRequest],
    body: bytes::Bytes,
) -> std::result::Result<String, String> {
    let mut last_error = "no media hosts".to_string();
    for request in requests {
        let mut post = http.post(&request.url).body(body.clone());
        for (key, value) in &request.headers {
            post = post.header(key, value);
        }
        match post.send().await {
            Ok(response) if response.status() == reqwest::StatusCode::OK => {
                match response.bytes().await {
                    Ok(payload) => match upload_response_direct_path(&payload) {
                        Some(direct_path) => return Ok(direct_path),
                        None => last_error = "upload response had no direct_path".to_string(),
                    },
                    Err(_) => last_error = "upload response body could not be read".to_string(),
                }
            }
            Ok(response) => last_error = format!("upload returned {}", response.status().as_u16()),
            // The deadline lands here, as an ordinary request failure: the
            // request is over, and the next host gets its own budget.
            Err(e) if e.is_timeout() => {
                last_error = format!("upload timed out after {DOCUMENT_THUMBNAIL_TIMEOUT_SECS}s");
            }
            Err(_) => last_error = "upload request failed".to_string(),
        }
    }
    Err(last_error)
}

#[cfg(feature = "whatsapp-web")]
fn document_thumbnail_upload_request(
    hostname: &str,
    auth: &str,
    token: &str,
) -> wacore::net::HttpRequest {
    wacore::net::HttpRequest::post(format!(
        "https://{hostname}{DOCUMENT_THUMBNAIL_UPLOAD_PATH}/{token}?auth={auth}&token={token}"
    ))
    .with_header("Content-Type", "application/octet-stream")
    .with_header("Origin", wacore::net::WHATSAPP_WEB_ORIGIN)
}

#[cfg(feature = "whatsapp-web")]
fn upload_response_direct_path(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()?
        .get("direct_path")?
        .as_str()
        .filter(|path| !path.is_empty())
        .map(str::to_string)
}

/// Run one preview tool and return its stdout, or why it could not be used.
/// The reason never includes the document path.
#[cfg(feature = "whatsapp-web")]
async fn run_preview_tool(
    program: &str,
    args: &[std::ffi::OsString],
    timeout: std::time::Duration,
) -> std::result::Result<Vec<u8>, String> {
    use std::process::Stdio;

    let child = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("could not start: {}", e.kind()))?;
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| format!("timed out after {timeout:?}"))?
        .map_err(|e| format!("failed while running: {}", e.kind()))?;
    if !output.status.success() {
        return Err(format!("exited with {}", output.status));
    }
    Ok(output.stdout)
}

#[cfg(feature = "whatsapp-web")]
fn note_preview_skipped(step: &str, reason: &str) {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Skip)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
            .with_attrs(::serde_json::json!({ "step": step, "reason": reason })),
        "whatsapp-web: document preview part skipped"
    );
}

/// The `Pages:` line of `pdfinfo` output.
#[cfg(feature = "whatsapp-web")]
fn pdfinfo_page_count(stdout: &str) -> Option<u32> {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("Pages:"))
        .and_then(|value| value.trim().parse().ok())
        .filter(|pages| *pages > 0)
}

/// Width and height from the first start-of-frame segment of a JPEG.
#[cfg(feature = "whatsapp-web")]
fn jpeg_dimensions(jpeg: &[u8]) -> Option<(u32, u32)> {
    if !jpeg.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut pos = 2;
    while pos + 4 <= jpeg.len() {
        if jpeg[pos] != 0xFF {
            return None;
        }
        let marker = jpeg[pos + 1];
        // Fill bytes before a marker.
        if marker == 0xFF {
            pos += 1;
            continue;
        }
        let length = usize::from(u16::from_be_bytes([jpeg[pos + 2], jpeg[pos + 3]]));
        // SOF0..SOF15, except DHT (C4), JPG (C8) and DAC (CC).
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let frame = jpeg.get(pos + 4..pos + 9)?;
            let height = u32::from(u16::from_be_bytes([frame[1], frame[2]]));
            let width = u32::from(u16::from_be_bytes([frame[3], frame[4]]));
            return (width > 0 && height > 0).then_some((width, height));
        }
        if length < 2 {
            return None;
        }
        pos += 2 + length;
    }
    None
}

#[cfg(feature = "whatsapp-web")]
impl WhatsAppMediaKind {
    fn from_marker(kind: &str) -> Option<Self> {
        match kind {
            "IMAGE" | "PHOTO" => Some(Self::Image),
            "DOCUMENT" | "FILE" => Some(Self::Document),
            "VIDEO" => Some(Self::Video),
            "AUDIO" => Some(Self::Audio),
            "VOICE" => Some(Self::Voice),
            _ => None,
        }
    }

    fn media_type(self) -> wacore::download::MediaType {
        match self {
            Self::Image => wacore::download::MediaType::Image,
            Self::Document => wacore::download::MediaType::Document,
            Self::Video => wacore::download::MediaType::Video,
            Self::Audio | Self::Voice => wacore::download::MediaType::Audio,
        }
    }

    fn mime_for_path(self, path: &Path) -> String {
        let ext = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase);
        if matches!(self, Self::Voice) && matches!(ext.as_deref(), Some("ogg" | "oga" | "opus")) {
            return "audio/ogg; codecs=opus".to_string();
        }
        match ext.as_deref() {
            Some("png") => "image/png",
            Some("jpg" | "jpeg") => "image/jpeg",
            Some("gif") => "image/gif",
            Some("webp") => "image/webp",
            Some("bmp") => "image/bmp",
            Some("mp4") => "video/mp4",
            Some("mov") => "video/quicktime",
            Some("mkv") => "video/x-matroska",
            Some("avi") => "video/x-msvideo",
            Some("webm") => "video/webm",
            Some("mp3") => "audio/mpeg",
            Some("m4a") => "audio/mp4",
            Some("wav") => "audio/wav",
            Some("flac") => "audio/flac",
            Some("ogg" | "oga") => "audio/ogg",
            Some("opus") => "audio/opus",
            Some("pdf") => "application/pdf",
            Some("doc") => "application/msword",
            Some("docx") => {
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            }
            Some("xls") => "application/vnd.ms-excel",
            Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Some("csv") => "text/csv",
            Some("txt") => "text/plain",
            _ => "application/octet-stream",
        }
        .to_string()
    }
}

#[cfg(feature = "whatsapp-web")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct WhatsAppMediaMarker {
    kind: WhatsAppMediaKind,
    /// Path-like target resolved against the workspace before upload.
    target: String,
}

#[cfg(feature = "whatsapp-web")]
use crate::util::WhatsAppLocation;

#[cfg(feature = "whatsapp-web")]
#[derive(Debug, Clone, PartialEq)]
enum WhatsAppMarker {
    Media(WhatsAppMediaMarker),
    Location(String),
}

#[cfg(feature = "whatsapp-web")]
impl WhatsAppMarker {
    fn from_shared_marker(kind: String, target: String) -> Option<Self> {
        if kind.eq_ignore_ascii_case("LOCATION") {
            return Some(Self::Location(target));
        }
        let kind = WhatsAppMediaKind::from_marker(&kind)?;
        Some(Self::Media(WhatsAppMediaMarker { kind, target }))
    }

    /// Short label for structured logs.
    fn kind_label(&self) -> String {
        match self {
            Self::Media(m) => format!("{:?}", m.kind),
            Self::Location(_) => "Location".to_string(),
        }
    }
}

#[cfg(feature = "whatsapp-web")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WhatsAppMarkerFailure {
    Refused,
    Failed,
}

#[cfg(feature = "whatsapp-web")]
#[derive(Debug)]
enum WhatsAppMarkerError {
    Refused(anyhow::Error),
    Failed(anyhow::Error),
}

#[cfg(feature = "whatsapp-web")]
impl std::fmt::Display for WhatsAppMarkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(err) | Self::Failed(err) => write!(f, "{err}"),
        }
    }
}

#[cfg(feature = "whatsapp-web")]
impl WhatsAppMarkerError {
    fn kind(&self) -> WhatsAppMarkerFailure {
        match self {
            Self::Refused(_) => WhatsAppMarkerFailure::Refused,
            Self::Failed(_) => WhatsAppMarkerFailure::Failed,
        }
    }
}

#[cfg(feature = "whatsapp-web")]
fn validate_whatsapp_marker_target(
    target: &str,
    workspace_dir: Option<&Path>,
) -> std::result::Result<PathBuf, WhatsAppMarkerError> {
    if target.starts_with("http://") || target.starts_with("https://") {
        return Err(WhatsAppMarkerError::Refused(anyhow::Error::msg(
            "WhatsApp Web media markers currently accept local workspace files only",
        )));
    }
    let disallowed_scheme = if target.starts_with("data:") {
        Some("data")
    } else if target.starts_with("file:") {
        Some("file")
    } else if target.contains("://") {
        Some(target.split("://").next().unwrap_or("?"))
    } else {
        None
    };
    if let Some(scheme) = disallowed_scheme {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"scheme": scheme})),
            "whatsapp-web: marker target uses disallowed scheme"
        );
        return Err(WhatsAppMarkerError::Refused(anyhow::Error::msg(
            "WhatsApp Web marker target uses a disallowed scheme",
        )));
    }

    let workspace = workspace_dir.ok_or_else(|| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"reason": "no_workspace_dir"})),
            "whatsapp-web: local marker target has no workspace_dir"
        );
        WhatsAppMarkerError::Refused(anyhow::Error::msg(
            "WhatsApp Web channel was started without a workspace_dir",
        ))
    })?;
    let workspace_canon = std::fs::canonicalize(workspace)
        .with_context(|| format!("canonicalize workspace {}", workspace.display()))
        .map_err(WhatsAppMarkerError::Refused)?;
    let target_path = Path::new(target);
    let absolute = if target_path.is_absolute() {
        target_path.to_path_buf()
    } else {
        workspace_canon.join(target_path)
    };
    let target_canon = match std::fs::canonicalize(&absolute) {
        Ok(path) => path,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"reason": "not_found"})),
                "whatsapp-web: marker target not found on disk"
            );
            return Err(WhatsAppMarkerError::Failed(anyhow::Error::msg(
                "WhatsApp Web marker target not found on disk",
            )));
        }
        Err(err) => {
            return Err(WhatsAppMarkerError::Refused(
                anyhow::Error::from(err).context("canonicalize WhatsApp marker target"),
            ));
        }
    };

    if !target_canon.starts_with(&workspace_canon) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"reason": "outside_workspace"})),
            "whatsapp-web: marker target escapes workspace_dir"
        );
        return Err(WhatsAppMarkerError::Refused(anyhow::Error::msg(
            "WhatsApp Web marker target resolves outside workspace_dir",
        )));
    }
    Ok(target_canon)
}

#[cfg(feature = "whatsapp-web")]
fn validate_whatsapp_location_target(
    target: &str,
) -> std::result::Result<WhatsAppLocation, WhatsAppMarkerError> {
    WhatsAppLocation::parse(target).ok_or_else(|| {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"reason": "invalid_location"})),
            "whatsapp-web: location marker target is malformed or outside WGS84 range"
        );
        WhatsAppMarkerError::Refused(anyhow::Error::msg(
            "WhatsApp Web location marker must be `lat,lng[,name[,address]]` with in-range WGS84 coordinates",
        ))
    })
}

#[cfg(feature = "whatsapp-web")]
fn whatsapp_delivery_failure_note(failure_count: usize) -> Option<String> {
    if failure_count == 0 {
        return None;
    }
    let count = failure_count.to_string();
    let key = if failure_count == 1 {
        "channel-whatsapp-web-delivery-failure-note-one"
    } else {
        "channel-whatsapp-web-delivery-failure-note-many"
    };
    Some(i18n::get_required_cli_string_with_args(
        key,
        &[("count", count.as_str())],
    ))
}

/// Markdown spans whose doubled marker collapses to WhatsApp's single one.
#[cfg(feature = "whatsapp-web")]
const WHATSAPP_INLINE_SPANS: [(&str, char); 3] = [("**", '*'), ("__", '_'), ("~~", '~')];

/// Convert Markdown to WhatsApp's formatting dialect.
///
/// WhatsApp renders `*bold*`, `_italic_`, `~strikethrough~`, `` `code` ``,
/// ``` ```monospace``` ```, bullet and numbered lists and `>` quotes natively,
/// and auto-links bare URLs, so only the markers Markdown spells differently
/// are rewritten. Text already written in WhatsApp style passes through
/// unchanged.
#[cfg(feature = "whatsapp-web")]
fn markdown_to_whatsapp(text: &str) -> String {
    let mut result_lines: Vec<String> = Vec::new();
    // Length of the backtick run that opened the current fenced block.
    let mut open_fence: Option<usize> = None;

    for line in text.split('\n') {
        let trimmed = line.trim_start();
        let run = backtick_run(trimmed, 0);

        if let Some(fence) = open_fence {
            // Only a run at least as long as the opener with nothing but
            // whitespace after it closes the block (CommonMark fenced code
            // blocks); a shorter run or one carrying an info string is content.
            if run >= fence && trimmed[run..].trim().is_empty() {
                open_fence = None;
            }
            result_lines.push(line.to_string());
            continue;
        }

        // An opening fence is three or more backticks whose info string holds
        // no backtick — ```mono``` on one line is a WhatsApp monospace span.
        if run >= 3 && !trimmed[run..].contains('`') {
            // WhatsApp shares the backtick fence but has no notion of an info
            // string, so it would render `rust` as the first code line.
            let indent = &line[..line.len() - trimmed.len()];
            result_lines.push(format!("{indent}{}", &trimmed[..run]));
            open_fence = Some(run);
            continue;
        }

        // Headings: `## Title` → `*Title*`. WhatsApp has no heading of its own.
        let after_hashes = line.trim_start_matches('#');
        let level = line.len() - after_hashes.len();
        if (1..=6).contains(&level) && after_hashes.starts_with(' ') {
            // The whole line is bold, so `# **Title**` sheds its own bold
            // markers instead of gaining a second wrapper around them.
            let title = markdown_inline_to_whatsapp_in(after_hashes.trim(), true);
            result_lines.push(format!("*{title}*"));
            continue;
        }

        result_lines.push(markdown_inline_to_whatsapp(line));
    }

    result_lines.join("\n")
}

/// Rewrite the inline Markdown markers of a single non-fenced line.
#[cfg(feature = "whatsapp-web")]
fn markdown_inline_to_whatsapp(line: &str) -> String {
    markdown_inline_to_whatsapp_in(line, false)
}

/// [`markdown_inline_to_whatsapp`] for text that is already inside a bold
/// span, where a nested `**bold**` contributes only its text.
#[cfg(feature = "whatsapp-web")]
fn markdown_inline_to_whatsapp_in(line: &str, inside_bold: bool) -> String {
    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // Inline code is copied verbatim so markers inside it stay literal. A
        // span closes at the next backtick run of exactly the opening length
        // (CommonMark code spans); an unmatched run is literal text.
        if bytes[i] == b'`' {
            let run = backtick_run(line, i);
            let end = match find_backtick_run(line, i + run, run) {
                Some(close) => close + run,
                None => i + run,
            };
            out.push_str(&line[i..end]);
            i = end;
            continue;
        }

        // A bare URL is copied through whole so the `_`, `*` and `~` in its
        // path stay literal; WhatsApp auto-links it as written.
        if let Some(end) = bare_url_end(line, i) {
            out.push_str(&line[i..end]);
            i = end;
            continue;
        }

        // `**bold**` → `*bold*`, `__x__` → `_x_`, `~~strike~~` → `~strike~`.
        // A single marker is already WhatsApp syntax, so a leading `* ` list
        // bullet and a lone `*bold*` both fall through untouched.
        if let Some(&(marker, replacement)) = WHATSAPP_INLINE_SPANS
            .iter()
            .find(|(marker, _)| line[i..].starts_with(marker))
            && let Some(end) = line[i + 2..].find(marker)
        {
            let inner = &line[i + 2..i + 2 + end];
            if inside_bold && replacement == '*' {
                out.push_str(inner);
            } else {
                out.push(replacement);
                out.push_str(inner);
                out.push(replacement);
            }
            i += 4 + end;
            continue;
        }

        // `[text](url)` → `text: url`; WhatsApp auto-links the bare URL. The
        // destination is read with CommonMark's boundaries (angle-bracket
        // form, balanced or escaped parentheses, optional title), so its
        // bytes reach the recipient unchanged; anything else stays literal.
        if bytes[i] == b'['
            && let Some(bracket_end) = line[i + 1..].find(']')
        {
            let after_bracket = i + 1 + bracket_end + 1;
            if after_bracket < len
                && bytes[after_bracket] == b'('
                && let Some((url, end)) = parse_link_destination(line, after_bracket)
            {
                let text = &line[i + 1..i + 1 + bracket_end];
                out.push_str(text);
                out.push_str(": ");
                out.push_str(&url);
                i = end;
                continue;
            }
        }

        let ch = line[i..].chars().next().unwrap_or_default();
        out.push(ch);
        i += ch.len_utf8();
    }

    out
}

/// Length of the backtick run starting at byte `at`.
#[cfg(feature = "whatsapp-web")]
fn backtick_run(text: &str, at: usize) -> usize {
    text.as_bytes()[at..]
        .iter()
        .take_while(|&&b| b == b'`')
        .count()
}

/// Byte index of the first backtick run of exactly `len` at or after `from`.
#[cfg(feature = "whatsapp-web")]
fn find_backtick_run(text: &str, from: usize, len: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let run = backtick_run(text, i);
        if run == len {
            return Some(i);
        }
        i += run;
    }
    None
}

/// End of the bare URL starting at byte `at`, if one starts there. The URL
/// runs to whitespace or `<`, less trailing punctuation and an unbalanced `)`
/// (GFM autolink extension), so `(see https://x/y).` links `https://x/y`.
#[cfg(feature = "whatsapp-web")]
fn bare_url_end(text: &str, at: usize) -> Option<usize> {
    let rest = &text[at..];
    if !(rest.starts_with("http://") || rest.starts_with("https://")) {
        return None;
    }
    let mut end = at
        + rest
            .find(|c: char| c.is_whitespace() || c == '<')
            .unwrap_or(rest.len());
    while let Some(last) = text[at..end].chars().next_back() {
        let url = &text[at..end];
        let unbalanced_paren = last == ')' && url.matches(')').count() > url.matches('(').count();
        if !unbalanced_paren && !"?!.,:*_~'\"".contains(last) {
            break;
        }
        end -= last.len_utf8();
    }
    Some(end)
}

/// Reads the `(destination "title")` tail of an inline link whose `(` sits at
/// byte `open`. Returns the destination with backslash escapes resolved and
/// the byte index just past the closing `)`, or `None` when the bytes are not
/// a CommonMark link destination (the caller then keeps them literal).
///
/// CommonMark allows two destination forms: `<...>` (any characters except an
/// unescaped `<` or `>`, so spaces are allowed) and a bare run without spaces
/// or control characters in which parentheses must be balanced or escaped.
/// An optional title (`"..."`, `'...'` or `(...)`) may follow after spaces.
#[cfg(feature = "whatsapp-web")]
fn parse_link_destination(text: &str, open: usize) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    let escaped =
        |at: usize| bytes[at] == b'\\' && bytes.get(at + 1).is_some_and(u8::is_ascii_punctuation);
    let skip_spaces = |mut at: usize| {
        while at < bytes.len() && (bytes[at] == b' ' || bytes[at] == b'\t') {
            at += 1;
        }
        at
    };

    let mut i = skip_spaces(open + 1);
    let mut url = String::new();
    if bytes.get(i) == Some(&b'<') {
        i += 1;
        loop {
            match *bytes.get(i)? {
                b'>' => {
                    i += 1;
                    break;
                }
                b'<' => return None,
                _ if escaped(i) => {
                    url.push(bytes[i + 1] as char);
                    i += 2;
                }
                _ => {
                    let ch = text[i..].chars().next()?;
                    url.push(ch);
                    i += ch.len_utf8();
                }
            }
        }
    } else {
        let mut depth = 0usize;
        loop {
            match *bytes.get(i)? {
                b')' if depth == 0 => break,
                b' ' | b'\t' => break,
                _ if escaped(i) => {
                    url.push(bytes[i + 1] as char);
                    i += 2;
                }
                b'(' => {
                    depth += 1;
                    url.push('(');
                    i += 1;
                }
                b')' => {
                    depth -= 1;
                    url.push(')');
                    i += 1;
                }
                b if b.is_ascii_control() => return None,
                _ => {
                    let ch = text[i..].chars().next()?;
                    url.push(ch);
                    i += ch.len_utf8();
                }
            }
        }
    }

    // A title has no WhatsApp form; it is parsed only to find the closing `)`.
    let after_destination = i;
    i = skip_spaces(i);
    if i > after_destination
        && let Some(close) = match bytes.get(i) {
            Some(b'"') => Some(b'"'),
            Some(b'\'') => Some(b'\''),
            Some(b'(') => Some(b')'),
            _ => None,
        }
    {
        let opener = bytes[i];
        i += 1;
        loop {
            let b = *bytes.get(i)?;
            if escaped(i) {
                i += 2;
            } else if b == close {
                i += 1;
                break;
            } else if b == opener && opener == b'(' {
                return None;
            } else {
                i += 1;
            }
        }
        i = skip_spaces(i);
    }

    (bytes.get(i) == Some(&b')')).then(|| (url, i + 1))
}

#[cfg(feature = "whatsapp-web")]
impl ::zeroclaw_api::attribution::Attributable for WhatsAppWebChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::WhatsappWeb,
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[cfg(feature = "whatsapp-web")]
#[async_trait]
impl Channel for WhatsAppWebChannel {
    fn name(&self) -> &str {
        "whatsapp"
    }

    /// Without this the trait default (`false`) applies, so every WhatsApp DM
    /// is treated as a non-direct message. Callers that exist to spare direct
    /// messages extra handling — notably the reply-intent precheck bypass in
    /// the channel orchestrator — then never fire for WhatsApp at all.
    fn is_direct_message(&self, msg: &ChannelMessage) -> bool {
        is_direct_message_jid(&msg.reply_target)
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let client = self.client.lock().clone();
        let Some(client) = client else {
            anyhow::bail!("WhatsApp Web client not connected. Initialize the bot first.");
        };

        // Validate recipient allowlist only for direct phone-number targets.
        if !Self::is_jid(&message.recipient) {
            let normalized = self.normalize_phone(&message.recipient);
            if !self.is_number_allowed(&normalized) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!("recipient {} not in allowed list", message.recipient)
                );
                return Ok(());
            }
        }

        let deliverable_recipient = Self::resolve_outbound_recipient(&message.recipient);
        let to = self.recipient_to_jid(&deliverable_recipient)?;
        let raw_content = if message.content.contains("<function_calls")
            || message.content.contains("</function_calls")
            || message.content.contains("<tool_call")
            || message.content.contains("</tool_call")
            || message.content.contains("<tool_calls")
            || message.content.contains("</tool_calls")
        {
            crate::util::strip_tool_call_tags(&message.content)
        } else {
            message.content.clone()
        };
        let (mut text_content, raw_markers) = if raw_content.contains('[')
            && raw_content.contains(':')
            && raw_content.contains(']')
        {
            let (cleaned, raw_markers) = super::util::parse_attachment_markers(&raw_content);
            if raw_markers.is_empty() {
                (raw_content, raw_markers)
            } else {
                (cleaned, raw_markers)
            }
        } else {
            (raw_content, Vec::new())
        };
        let markers = raw_markers
            .into_iter()
            .filter_map(|(kind, target)| WhatsAppMarker::from_shared_marker(kind, target))
            .collect::<Vec<_>>();

        // Voice chat mode: send text normally AND queue a voice note of the
        // final answer. Only substantive messages (not tool outputs) are queued.
        // A debounce task waits 10s after the last substantive message, then
        // sends ONE voice note. Text in → text out. Voice in → text + voice out.
        let is_voice_chat = self
            .voice_chats
            .lock()
            .map(|vs| vs.contains(&message.recipient))
            .unwrap_or(false);

        if is_voice_chat && let Some(tts_manager) = self.tts_manager.clone() {
            let content = &text_content;
            // Only queue substantive natural-language replies for voice.
            // Skip tool outputs: URLs, JSON, code blocks, errors, short status.
            let skip_reason =
                self.queue_pending_voice(&message.recipient, content, message.suppress_voice);
            if let Some(reason) = skip_reason {
                // Stable literal per the logging contract: the classification
                // and per-event measurements ride solely in `attributes` above.
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Skip)
                        .with_attrs(::serde_json::json!({
                            "recipient": message.recipient,
                            "reason": reason,
                            "content_len": content.len(),
                        })),
                    "voice reply skipped"
                );
            }

            if skip_reason.is_none() {
                let pending = self.pending_voice.clone();
                let voice_chats = self.voice_chats.clone();
                let client_clone = client.clone();
                let to_clone = to.clone();
                let recipient = message.recipient.clone();
                zeroclaw_spawn::spawn!(async move {
                    // Wait 10 seconds — long enough for the agent to finish its
                    // full tool chain and send the final answer.
                    tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

                    // Atomic check-and-remove: only one task gets the value
                    let to_voice = pending.lock().ok().and_then(|mut pv| {
                        if let Some((_, ts)) = pv.get(&recipient)
                            && ts.elapsed().as_secs() >= 8
                        {
                            return pv.remove(&recipient).map(|(text, _)| text);
                        }
                        None
                    });

                    if let Some(text) = to_voice {
                        if let Ok(mut vc) = voice_chats.lock() {
                            vc.remove(&recipient);
                        }
                        match Box::pin(WhatsAppWebChannel::synthesize_voice_static(
                            &client_clone,
                            &to_clone,
                            &text,
                            &tts_manager,
                        ))
                        .await
                        {
                            Ok(()) => {
                                ::zeroclaw_log::record!(
                                    INFO,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    ),
                                    &format!("voice reply sent ({} chars)", text.len())
                                );
                            }
                            Err(e) => {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                                    "TTS voice reply failed"
                                );
                            }
                        }
                    }
                });
            }
            // Fall through to send text normally (voice chat gets BOTH)
        }

        let mut delivered_markers = 0usize;
        let mut failed_marker_count = 0usize;
        for marker in &markers {
            // Location markers carry inline data (lat,lng,...) and skip the
            // workspace file validator, but the coordinates must still parse
            // and land in WGS84 range; media markers must resolve to a real
            // file inside the workspace before upload.
            let result = match marker {
                WhatsAppMarker::Location(target) => {
                    match validate_whatsapp_location_target(target) {
                        Ok(loc) => Self::send_location(&client, &to, &loc).await,
                        Err(err) => {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                .with_attrs(::serde_json::json!({
                                    "kind": "Location",
                                    "reason": "invalid coordinates",
                                    "error": err.to_string(),
                                })),
                                "whatsapp-web: dropping unresolved outbound attachment marker"
                            );
                            failed_marker_count += 1;
                            continue;
                        }
                    }
                }
                WhatsAppMarker::Media(media) => {
                    let target = match validate_whatsapp_marker_target(
                        &media.target,
                        self.workspace_dir.as_deref(),
                    ) {
                        Ok(path) => path,
                        Err(err) => {
                            let reason = match err.kind() {
                                WhatsAppMarkerFailure::Refused => "trust boundary",
                                WhatsAppMarkerFailure::Failed => "not found",
                            };
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                .with_attrs(::serde_json::json!({
                                    "kind": format!("{:?}", media.kind),
                                    "reason": reason,
                                    "error": err.to_string(),
                                })),
                                "whatsapp-web: dropping unresolved outbound attachment marker"
                            );
                            failed_marker_count += 1;
                            continue;
                        }
                    };
                    Self::send_media_marker(&client, &to, media, &target, self.document_thumbnails)
                        .await
                }
            };
            match result {
                Ok(()) => delivered_markers += 1,
                Err(err) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "kind": marker.kind_label(),
                                "error": err.to_string(),
                            })),
                        "whatsapp-web: marker delivery failed"
                    );
                    failed_marker_count += 1;
                }
            }
        }

        if let Some(note) = whatsapp_delivery_failure_note(failed_marker_count) {
            if text_content.is_empty() {
                text_content = note;
            } else {
                text_content.push_str("\n\n");
                text_content.push_str(&note);
            }
        }

        if !markers.is_empty() && text_content.is_empty() && delivered_markers > 0 {
            return Ok(());
        }

        // Send text message
        let outgoing = waproto::whatsapp::Message {
            conversation: Some(markdown_to_whatsapp(&text_content)),
            ..Default::default()
        };

        // Box::pin the large future (~34KB) so it doesn't inflate the
        // enclosing Send future's stack slot — clippy::large_futures.
        // whatsapp-rust 0.6: send_message returns `SendResult { message_id, to }`
        // instead of a bare `String` (oxidezap/whatsapp-rust
        let send_result = Box::pin(client.send_message(to, outgoing)).await?;
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "sent text to {} (id: {})",
                message.recipient, send_result.message_id
            )
        );
        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        // Store the sender channel for incoming messages
        *self.tx.lock() = Some(tx.clone());

        // Capture alias as an Arc so the long-running event closure (inside
        // the reconnect loop) can clone cheaply per spawned message without
        // borrowing `self` for its 'static lifetime.
        let alias = std::sync::Arc::new(self.alias.clone());

        use wacore::store::DevicePropsOverride;
        use wacore::types::events::Event;
        use wacore_binary::jid::JidExt as _;
        use whatsapp_rust::TokioRuntime;
        use whatsapp_rust::bot::Bot;
        use whatsapp_rust::pair_code::PairCodeOptions;
        use whatsapp_rust::store::{Device, DeviceStore};
        use whatsapp_rust_tokio_transport::TokioWebSocketTransportFactory;
        use whatsapp_rust_ureq_http_client::UreqHttpClient;

        let retry_count = Arc::new(std::sync::atomic::AtomicU32::new(0));

        loop {
            let expanded_session_path = Self::expand_session_path(&self.session_path);

            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                &format!("channel starting (session: {})", expanded_session_path)
            );

            // Initialize storage backend
            let storage = RusqliteStore::new(&expanded_session_path)?;
            let backend = Arc::new(storage);

            // Check if we have a saved device to load
            let mut device = Device::new(backend.clone());
            if backend.exists().await? {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "found existing session, loading device"
                );
                if let Some(core_device) = backend.load().await? {
                    device.load_from_serializable(core_device);
                } else {
                    anyhow::bail!("Device exists but failed to load");
                }
                if let Some(ref pn) = device.pn
                    && let Some(digits) = Self::store_jid_digits(&self.bot_phone, pn.user())
                {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        &format!("pre-resolved bot phone from saved session: +{}", digits)
                    );
                }
                if let Some(ref lid) = device.lid
                    && let Some(digits) = Self::store_jid_digits(&self.bot_lid, lid.user())
                {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        &format!("pre-resolved bot LID from saved session: {}", digits)
                    );
                }
            } else {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "no existing session, new device will be created during pairing"
                );
            };

            // Create transport factory. WebSocket URL override comes from
            // `[whatsapp.ws_url]`; legacy `WHATSAPP_WS_URL` env var is gone.
            let mut transport_factory = TokioWebSocketTransportFactory::new();
            if let Some(ref ws_url) = self.ws_url {
                transport_factory = transport_factory.with_url(ws_url.clone());
            }

            // Create HTTP client for media operations
            let http_client = UreqHttpClient::new();

            // Channel to signal logout from the event handler back to the listen loop.
            let (logout_tx, mut logout_rx) = tokio::sync::broadcast::channel::<()>(1);

            // Tracks whether Event::LoggedOut actually fired (vs task crash).
            let session_revoked = Arc::new(std::sync::atomic::AtomicBool::new(false));

            // Build the bot
            let logout_tx_clone = logout_tx.clone();
            let retry_count_clone = retry_count.clone();
            let session_revoked_clone = session_revoked.clone();
            let mention_only = self.mention_only;
            let bot_phone_clone = self.bot_phone.clone();
            let bot_lid_clone = self.bot_lid.clone();
            let persist_clone = self.persist.clone();
            let inbound_context = WhatsAppInboundContext {
                tx: tx.clone(),
                alias: Arc::clone(&alias),
                peer_resolver: Arc::clone(&self.peer_resolver),
                allowed_groups_resolver: Arc::clone(&self.allowed_groups_resolver),
                mode: self.mode.clone(),
                dm_policy: self.dm_policy.clone(),
                group_policy: self.group_policy.clone(),
                self_chat_mode: self.self_chat_mode,
                mention_only: self.mention_only,
                passive_group_context: self.passive_group_context,
                bot_phone: self.bot_phone.clone(),
                bot_lid: self.bot_lid.clone(),
                dm_mention_patterns: self.dm_mention_patterns.clone(),
                group_mention_patterns: self.group_mention_patterns.clone(),
                transcription_config: self.transcription.clone(),
                transcription_manager: self.transcription_manager.clone(),
                voice_chats: self.voice_chats.clone(),
            };
            let configured_push_name = self.push_name.clone();

            let mut builder = Bot::builder()
                .with_backend_arc(backend)
                .with_transport_factory(transport_factory)
                .with_http_client(http_client)
                .with_runtime(TokioRuntime)
                .with_device_props(
                    DevicePropsOverride::new()
                        .with_os("ZeroClaw")
                        .with_platform_type(PlatformType::Desktop),
                )
                .on_event({
                    let alias = Arc::clone(&alias);
                    let inbound_context = inbound_context.clone();
                    move |event, client| {
                    let logout_tx = logout_tx_clone.clone();
                    let retry_count = retry_count_clone.clone();
                    let session_revoked = session_revoked_clone.clone();
                    let alias = Arc::clone(&alias);
                    let bot_phone_inner = bot_phone_clone.clone();
                    let bot_lid_inner = bot_lid_clone.clone();
                    let persist_inner = persist_clone.clone();
                    let inbound_context = inbound_context.clone();
                    let configured_push_name = configured_push_name.clone();
                    async move {
                        // Event handlers receive `Arc<Event>`, so match on
                        // `&*event` to get a `&Event` and bind variant fields
                        // by reference.
                        match &*event {
                            Event::Messages(_) => {
                                Self::handle_inbound_message_event(
                                    event.as_ref(),
                                    &client,
                                    &inbound_context,
                                )
                                .await;
                            }
                            Event::Connected(_) => {
                                crate::login_events::LoginEvent::Connected.emit(
                                    "whatsapp",
                                    alias.as_ref(),
                                    "WhatsApp Web connected successfully",
                                );
                                WhatsAppWebChannel::reset_retry(&retry_count);
                                // 0.7 returns the snapshot directly, not a future.
                                let device = client.persistence_manager().get_device_snapshot();
                                // Resolve bot identity from the device store
                                if mention_only {
                                    if let Some(ref pn) = device.pn
                                        && let Some(digits) =
                                            Self::store_jid_digits(&bot_phone_inner, pn.user())
                                    {
                                        ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), &format!("resolved bot identity from device: +{}", digits));
                                    }
                                    if let Some(ref lid) = device.lid
                                        && let Some(digits) =
                                            Self::store_jid_digits(&bot_lid_inner, lid.user())
                                    {
                                        ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), &format!("resolved bot LID from device: {}", digits));
                                    }
                                }
                                // `Bot::builder().with_push_name()` is not used: it writes
                                // the local store pre-connect and the server's
                                // `setting_pushName` sync overwrites it. Connected fires
                                // after that sync, so the device snapshot is authoritative
                                // and comparing against it keeps reconnects from emitting a
                                // redundant app-state patch.
                                if let Some(desired) = WhatsAppWebChannel::push_name_to_apply(
                                    configured_push_name.as_deref(),
                                    &device.push_name,
                                ) {
                                    match client.profile().set_push_name(desired).await {
                                        Ok(()) => {
                                            ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), &format!("set WhatsApp push name (configured len={}, previous len={})", desired.len(), device.push_name.len()));
                                        }
                                        Err(e) => {
                                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown), &format!("failed to set WhatsApp push name (configured len={}); keeping the account's current name: {e}", desired.len()));
                                        }
                                    }
                                }
                                // Persist the linked account as an authorized
                                // peer in canonical peer_groups (same shape
                                // WeChat pairing writes). Idempotent, so the
                                // reconnect-after-resume case is a no-op.
                                if let Some(ref pn) = device.pn {
                                    let digits = Self::jid_digits(pn.user());
                                    if !digits.is_empty()
                                        && let Err(e) =
                                            crate::identity_persist::persist_external_peer(
                                                persist_inner.as_ref(),
                                                "whatsapp",
                                                alias.as_ref(),
                                                &format!("+{digits}"),
                                                Self::phone_matches,
                                            )
                                            .await
                                    {
                                        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown), &format!("failed to persist linked WhatsApp identity: {e}"));
                                    }
                                }
                            }
                            Event::LoggedOut(_) => {
                                session_revoked.store(true, std::sync::atomic::Ordering::Relaxed);
                                crate::login_events::LoginEvent::LoggedOut.emit(
                                    "whatsapp",
                                    alias.as_ref(),
                                    "WhatsApp Web was logged out — will clear session and reconnect",
                                );
                                let _ = logout_tx.send(());
                            }
                            Event::StreamError(stream_error) => {
                                ::zeroclaw_log::record!(ERROR, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail).with_outcome(::zeroclaw_log::EventOutcome::Failure), &format!("stream error: {:?}", stream_error));
                            }
                            Event::PairingCode(pairing) => {
                                let code = &pairing.code;
                                crate::login_events::LoginEvent::PairCode { code: code.as_str() }
                                    .emit(
                                    "whatsapp",
                                    alias.as_ref(),
                                    "WhatsApp Web pair code received — enter it in WhatsApp > Linked Devices",
                                );
                                eprintln!();
                                eprintln!("pair code: {code}");
                                eprintln!();
                            }
                            Event::PairingQrCode(qr) => {
                                let code = &qr.code;
                                crate::login_events::LoginEvent::Qr {
                                    payload: code.as_str(),
                                    image_url: None,
                                    attempt: None,
                                    max_attempts: None,
                                }
                                .emit(
                                    "whatsapp",
                                    alias.as_ref(),
                                    "WhatsApp Web QR code received (scan with WhatsApp > Linked Devices)",
                                );
                                match Self::render_pairing_qr(code) {
                                    Ok(rendered) => {
                                        eprintln!();
                                        eprintln!(
                                            "WhatsApp Web QR code (scan in WhatsApp > Linked Devices):"
                                        );
                                        eprintln!("{rendered}");
                                        eprintln!();
                                    }
                                    Err(err) => {
                                        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown), &format!("failed to render pairing QR in terminal: {}", err));
                                        eprintln!();
                                        eprintln!("WhatsApp Web QR payload: {code}");
                                        eprintln!();
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }});

            // Configure pair-code flow when a phone number is provided.
            if let Some(ref phone) = self.pair_phone {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "pair-code flow enabled for configured phone number"
                );
                let options = PairCodeOptions {
                    phone_number: phone.clone(),
                    custom_code: self.pair_code.clone(),
                    ..Default::default()
                };
                let refresh_options = options.clone();
                let failure_alias = Arc::clone(&alias);
                builder = builder
                    .with_pair_code(options)
                    .on_pair_code_refresh(move |_force_manual, client| {
                        let options = refresh_options.clone();
                        async move {
                            // The 0.7 flow retires the old code before emitting
                            // this event; the consumer must explicitly request
                            // its replacement. Any failure emits the dedicated
                            // PairingCodeError event handled below.
                            let _ = client.pair_with_code(options).await;
                        }
                    })
                    .on_pair_code_error(move |_error, _client| {
                        let alias = Arc::clone(&failure_alias);
                        async move {
                            crate::login_events::LoginEvent::Failed {
                                reason: "pair-code request failed",
                            }
                            .emit(
                                "whatsapp",
                                alias.as_ref(),
                                "WhatsApp Web pair-code request failed; retry or use the QR flow",
                            );
                        }
                    });
            } else if self.pair_code.is_some() {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "pair_code is set but pair_phone is missing; pair code config is ignored"
                );
            }

            let bot = builder.build().await?;
            *self.client.lock() = Some(bot.client());

            // `run` consumes and drives the bot in place in 0.7; `spawn`
            // returns the abortable handle this channel owns.
            let bot_handle = bot.spawn();

            // Store the bot handle for later shutdown
            *self.bot_handle.lock() = Some(bot_handle);

            // Drop the outer sender so logout_rx.recv() returns Err when the
            // bot task ends without emitting LoggedOut (e.g. crash/panic).
            drop(logout_tx);

            // Wait for a logout signal or process shutdown.
            let should_reconnect = select! {
                res = logout_rx.recv() => {
                    // Both Ok(()) and Err (sender dropped) mean the session ended.
                    let _ = res;
                    true
                }
                _ = tokio::signal::ctrl_c() => {
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "channel received Ctrl+C");
                    false
                }
            };

            *self.client.lock() = None;
            let handle = self.bot_handle.lock().take();
            if let Some(handle) = handle {
                // 0.7's graceful shutdown flushes the device snapshot,
                // receipts, and message secrets before the run loop exits.
                // If it exceeds the bound, dropping the shutdown future drops
                // BotHandle, whose AbortHandle is the documented fallback.
                if tokio::time::timeout(std::time::Duration::from_secs(30), handle.shutdown())
                    .await
                    .is_err()
                {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail,)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        "graceful WhatsApp Web shutdown timed out; bot task aborted"
                    );
                }
            }

            // Drop the separate device reference before removing SQLite
            // session files after a confirmed logout.
            drop(device);

            if should_reconnect {
                let (attempts, exceeded) = Self::record_retry(&retry_count);
                if exceeded {
                    anyhow::bail!(
                        "exceeded {} reconnect attempts, giving up",
                        Self::MAX_RETRIES
                    );
                }

                // Only purge session files when LoggedOut was explicitly observed.
                // A transient task crash (Err from recv) should not wipe a valid session.
                if Self::should_purge_session(&session_revoked) {
                    for path in Self::session_file_paths(&expanded_session_path) {
                        match tokio::fs::remove_file(&path).await {
                            Ok(()) => {}
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                            Err(e) => ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                                &format!("failed to remove session file {}: {e}", path)
                            ),
                        }
                    }
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        "session files removed, restarting for QR pairing"
                    );
                } else {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        "bot stopped without LoggedOut; reconnecting with existing session"
                    );
                }

                let delay = Self::compute_retry_delay(attempts);
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    &format!(
                        "reconnecting in {}s (attempt {}/{})",
                        delay,
                        attempts,
                        Self::MAX_RETRIES
                    )
                );
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                continue;
            }

            break;
        }

        Ok(())
    }

    async fn health_check(&self) -> bool {
        let bot_handle_guard = self.bot_handle.lock();
        bot_handle_guard.is_some()
    }

    fn supports_native_polls(&self) -> bool {
        true
    }

    /// Post a native WhatsApp poll. Unlike `send`, a recipient outside the
    /// allowlist is an error rather than a silent no-op: this is a tool call,
    /// and the caller has to learn that nothing was posted.
    async fn send_poll(&self, poll: &zeroclaw_api::channel::PollRequest) -> Result<()> {
        // Validated before the client is touched, so a caller that got the
        // recipient wrong hears that instead of a connection error.
        if !Self::is_jid(&poll.recipient) {
            let normalized = self.normalize_phone(&poll.recipient);
            anyhow::ensure!(
                self.is_number_allowed(&normalized),
                "recipient `{}` is not in this channel's allowlist",
                poll.recipient
            );
        }
        let deliverable_recipient = Self::resolve_outbound_recipient(&poll.recipient);
        let to = self.recipient_to_jid(&deliverable_recipient)?;

        let client = self.client.lock().clone();
        let Some(client) = client else {
            anyhow::bail!("WhatsApp Web client not connected. Initialize the bot first.");
        };

        Box::pin(
            client
                .polls()
                .create(to, &poll.question, &poll.options, poll.selectable_count),
        )
        .await
        .map_err(|e| anyhow::Error::msg(format!("WhatsApp poll creation failed: {e}")))?;
        Ok(())
    }

    async fn start_typing(&self, recipient: &str) -> Result<()> {
        let client = self.client.lock().clone();
        let Some(client) = client else {
            anyhow::bail!("WhatsApp Web client not connected. Initialize the bot first.");
        };

        if !Self::is_jid(recipient) {
            let normalized = self.normalize_phone(recipient);
            if !self.is_number_allowed(&normalized) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!("typing target {} not in allowed list", recipient)
                );
                return Ok(());
            }
        }

        let deliverable_recipient = Self::resolve_outbound_recipient(recipient);
        let to = self.recipient_to_jid(&deliverable_recipient)?;
        client.chatstate().send_composing(&to).await.map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "Failed to send typing state (composing)"
            );
            anyhow::Error::msg(format!("Failed to send typing state (composing): {e}"))
        })?;

        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!("start typing for {}", recipient)
        );
        Ok(())
    }

    async fn stop_typing(&self, recipient: &str) -> Result<()> {
        let client = self.client.lock().clone();
        let Some(client) = client else {
            anyhow::bail!("WhatsApp Web client not connected. Initialize the bot first.");
        };

        if !Self::is_jid(recipient) {
            let normalized = self.normalize_phone(recipient);
            if !self.is_number_allowed(&normalized) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!("typing target {} not in allowed list", recipient)
                );
                return Ok(());
            }
        }

        let deliverable_recipient = Self::resolve_outbound_recipient(recipient);
        let to = self.recipient_to_jid(&deliverable_recipient)?;
        client.chatstate().send_paused(&to).await.map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "Failed to send typing state (paused)"
            );
            anyhow::Error::msg(format!("Failed to send typing state (paused): {e}"))
        })?;

        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!("stop typing for {}", recipient)
        );
        Ok(())
    }

    /// Ask the operator to approve a tool call, over the chat this request came
    /// from, and wait up to `approval_timeout_secs` for a reply.
    ///
    /// Before this existed the channel inherited the trait default of
    /// `Ok(None)`, so an `always_ask` tool failed closed immediately and
    /// `approval_timeout_secs` was accepted by the config and never read.
    /// Failing closed was safe, but it made configured
    /// interactive approval unavailable on this transport.
    ///
    /// The request half mirrors the Cloud transport: token, prompt, wait,
    /// deny-on-timeout, and remove the token on the way out so a late reply
    /// cannot resolve a request nobody is waiting for. The RESOLUTION half does
    /// not mirror it, and [`resolve_approval_reply`] says why.
    async fn request_approval(
        &self,
        recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> Result<Option<ChannelApprovalResponse>> {
        // Bind the token to the issuing alias AND the chat it is about to be
        // posted into, so a reply from anywhere else cannot answer it. The
        // alias matters because the map is process-wide: without it, a second
        // configured alias sharing this chat would resolve this request under
        // ITS allowlist rather than ours.
        let binding = ApprovalBinding {
            alias: self.alias.clone(),
            chat: Self::compute_reply_target(recipient),
            is_group: recipient.contains("@g.us"),
        };
        let PendingApprovalRegistration {
            token,
            receiver,
            binding,
            mut guard,
        } = register_pending_approval(binding).await;

        // Shared with the Cloud transport, discord, signal and slack, so the
        // prompt's prose comes from the runtime Fluent catalogue while the
        // token and the yes/no/always keywords stay protocol-exact ASCII that
        // `parse_approval_reply` can still read.
        let mut text = crate::util::build_yesno_approval_prompt(
            &token,
            &request.tool_name,
            &request.arguments_summary,
            request.position_counter(),
        );
        if binding.is_group {
            // Say so in the prompt. The token is now readable by everyone in
            // this group, and the reason a stranger's reply will bounce is not
            // obvious from the prompt alone.
            //
            // The authority named here has to match what the code consults,
            // which is the canonical peer resolver backed by
            // `[peer_groups.<name>].external_peers` scoped to this alias.
            // V3 has no `allowed_numbers` field: that key was a V2 spelling
            // and migrates into a peer group, so naming it would send an
            // operator whose reply bounced to configure something that no
            // longer exists.
            text.push_str("\n\n");
            text.push_str(&i18n::get_required_cli_string(
                "channel-approval-group-visibility-warning",
            ));
        }

        if let Err(e) = self
            .send_approval_prompt(&SendMessage::new(text, recipient))
            .await
        {
            // Never leave a token pending for a prompt that was never
            // delivered; it would sit until the timeout and deny anyway, but
            // with no operator ever having seen it.
            //
            // Removal is generation-checked rather than by token alone. A
            // resolver can take this entry out while this branch is still in
            // flight, and a later request can then reserve the same
            // six-character code, at which point an unconditional remove here
            // would delete that unrelated request instead of ours.
            remove_pending_approval_if_matches(&token, guard.registration_id).await;
            guard.disarm();
            return Err(e);
        }

        let timeout = std::time::Duration::from_secs(self.approval_timeout_secs);
        let response = match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(response)) => response,
            // Timed out, or the sender was dropped. Either way deny, and drop
            // the token so a later reply cannot resolve a dead request.
            _ => {
                // Generation-checked for the same reason as the send-error
                // branch above: by the time this fires, the entry may already
                // belong to a different request that reused the code.
                remove_pending_approval_if_matches(&token, guard.registration_id).await;
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "tool": request.tool_name,
                            "timeout_secs": self.approval_timeout_secs,
                        })),
                    "approval request denied: no reply before approval_timeout_secs"
                );
                ChannelApprovalResponse::Deny
            }
        };
        // Both arms above reached a decision through a normal path and own their
        // own removal, so the guard has nothing left to clean up. Disarming only
        // HERE is deliberate: every path that skips this line is a path where
        // the entry would otherwise be orphaned, which is exactly what the
        // guard exists to cover.
        guard.disarm();
        Ok(Some(response))
    }
}

// Stub implementation when feature is not enabled
#[cfg(not(feature = "whatsapp-web"))]
pub struct WhatsAppWebChannel {
    _private: (),
}

#[cfg(not(feature = "whatsapp-web"))]
impl WhatsAppWebChannel {
    pub fn new(
        _session_path: String,
        _pair_phone: Option<String>,
        _pair_code: Option<String>,
        _ws_url: Option<String>,
        _alias: impl Into<String>,
        _peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
        _mention_only: bool,
        _mode: zeroclaw_config::schema::WhatsAppWebMode,
        _dm_policy: zeroclaw_config::schema::WhatsAppChatPolicy,
        _group_policy: zeroclaw_config::schema::WhatsAppChatPolicy,
        _self_chat_mode: bool,
    ) -> Self {
        Self { _private: () }
    }

    pub fn with_transcription(self, _config: zeroclaw_config::schema::TranscriptionConfig) -> Self {
        self
    }

    pub(crate) fn with_transcription_manager(
        self,
        _config: zeroclaw_config::schema::TranscriptionConfig,
        _manager: Option<std::sync::Arc<super::transcription::TranscriptionManager>>,
    ) -> Self {
        self
    }

    pub fn with_tts(self, _config: zeroclaw_config::schema::TtsConfig) -> Self {
        self
    }
}

#[cfg(not(feature = "whatsapp-web"))]
impl ::zeroclaw_api::attribution::Attributable for WhatsAppWebChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::WhatsappWeb,
        )
    }
    fn alias(&self) -> &str {
        "whatsapp"
    }
}

#[cfg(not(feature = "whatsapp-web"))]
#[async_trait]
impl Channel for WhatsAppWebChannel {
    fn name(&self) -> &str {
        "whatsapp"
    }

    async fn send(&self, _message: &SendMessage) -> Result<()> {
        anyhow::bail!(i18n::get_required_cli_string(
            "channel-whatsapp-web-feature-missing-error"
        ));
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        anyhow::bail!(i18n::get_required_cli_string(
            "channel-whatsapp-web-feature-missing-error"
        ));
    }

    async fn health_check(&self) -> bool {
        false
    }

    async fn start_typing(&self, _recipient: &str) -> Result<()> {
        anyhow::bail!(i18n::get_required_cli_string(
            "channel-whatsapp-web-feature-missing-error"
        ));
    }

    async fn stop_typing(&self, _recipient: &str) -> Result<()> {
        anyhow::bail!(i18n::get_required_cli_string(
            "channel-whatsapp-web-feature-missing-error"
        ));
    }
}

/// Longest side of the inline JPEG on an outgoing image. Phones draw the
/// image card from it until the full image is downloaded, and they do not
/// download automatically from senders outside the contact list, so without
/// it the card stays empty. The official apps send about this size.
#[cfg(feature = "whatsapp-web")]
const IMAGE_PREVIEW_SIDE: u32 = 100;

#[cfg(feature = "whatsapp-web")]
const IMAGE_PREVIEW_JPEG_QUALITY: u8 = 60;

/// The inline JPEG travels in the message itself.
#[cfg(feature = "whatsapp-web")]
const IMAGE_PREVIEW_MAX_BYTES: usize = 16 * 1024;

/// Decoding limits for the image being previewed, which may be a file the
/// agent downloaded. Anything larger is sent without a preview.
#[cfg(feature = "whatsapp-web")]
const IMAGE_PREVIEW_MAX_SOURCE_SIDE: u32 = 12_000;

#[cfg(feature = "whatsapp-web")]
const IMAGE_PREVIEW_MAX_ALLOC: u64 = 256 * 1024 * 1024;

/// Size and inline preview for an outgoing image card.
#[cfg(feature = "whatsapp-web")]
#[derive(Debug, PartialEq, Eq)]
struct ImagePreview {
    width: u32,
    height: u32,
    jpeg: Option<Vec<u8>>,
}

#[cfg(feature = "whatsapp-web")]
impl ImagePreview {
    fn apply_to(self, image: &mut waproto::whatsapp::message::ImageMessage) {
        image.width = Some(self.width);
        image.height = Some(self.height);
        if let Some(jpeg) = self.jpeg {
            image.jpeg_thumbnail = Some(jpeg);
        }
    }
}

/// Best-effort preview for the image in `bytes`: `None`, with a warning,
/// when it cannot be decoded within the limits.
#[cfg(feature = "whatsapp-web")]
async fn image_preview(bytes: Vec<u8>) -> Option<ImagePreview> {
    let rendered = tokio::task::spawn_blocking(move || {
        render_image_preview(
            &bytes,
            IMAGE_PREVIEW_MAX_SOURCE_SIDE,
            IMAGE_PREVIEW_MAX_ALLOC,
        )
    })
    .await
    .unwrap_or_else(|_| Err("preview task did not finish".to_string()));
    match rendered {
        Ok(preview) => {
            if preview.jpeg.is_none() {
                note_image_preview_skipped("rendered preview exceeds the inline size cap");
            }
            Some(preview)
        }
        Err(reason) => {
            note_image_preview_skipped(&reason);
            None
        }
    }
}

/// Decode `bytes`, apply the EXIF orientation so the preview matches what
/// the recipient sees, and scale it into an inline JPEG. The reported size is
/// that of the oriented full image.
#[cfg(feature = "whatsapp-web")]
fn render_image_preview(
    bytes: &[u8],
    max_source_side: u32,
    max_alloc: u64,
) -> std::result::Result<ImagePreview, String> {
    use image::ImageDecoder as _;

    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("could not read image: {e}"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(max_source_side);
    limits.max_image_height = Some(max_source_side);
    limits.max_alloc = Some(max_alloc);
    reader.limits(limits);
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| format!("could not decode image: {e}"))?;
    // `into_decoder` + `from_decoder` skips the reservation `ImageReader::decode`
    // makes, and the JPEG decoder enforces dimensions but not `max_alloc`, so a
    // small file within the side limits could still ask for a buffer past the
    // budget. Check it before anything is allocated.
    let needed = decoder.total_bytes();
    if needed > max_alloc {
        return Err(format!(
            "decoded image needs {needed} bytes, over the {max_alloc} byte budget"
        ));
    }
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut full = image::DynamicImage::from_decoder(decoder)
        .map_err(|e| format!("could not decode image: {e}"))?;
    full.apply_orientation(orientation);

    let thumbnail = full
        .thumbnail(IMAGE_PREVIEW_SIDE, IMAGE_PREVIEW_SIDE)
        .to_rgb8();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, IMAGE_PREVIEW_JPEG_QUALITY)
        .encode_image(&thumbnail)
        .map_err(|e| format!("could not encode preview: {e}"))?;
    Ok(ImagePreview {
        width: full.width(),
        height: full.height(),
        jpeg: (jpeg.len() <= IMAGE_PREVIEW_MAX_BYTES).then_some(jpeg),
    })
}

#[cfg(feature = "whatsapp-web")]
fn note_image_preview_skipped(reason: &str) {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Skip)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
            .with_attrs(::serde_json::json!({ "reason": reason })),
        "whatsapp-web: image preview skipped"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "whatsapp-web")]
    use wacore_binary::jid::Jid;

    // ── Outgoing image previews ──

    #[cfg(feature = "whatsapp-web")]
    fn encoded_image(width: u32, height: u32, format: image::ImageFormat) -> Vec<u8> {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::RgbImage::from_pixel(width, height, image::Rgb([200, 40, 60]))
            .write_to(&mut bytes, format)
            .expect("encode test image");
        bytes.into_inner()
    }

    /// A JPEG whose EXIF block says "rotate 90° clockwise to display"
    /// (orientation 6), as phone cameras write for portrait shots.
    #[cfg(feature = "whatsapp-web")]
    fn rotated_jpeg(stored_width: u32, stored_height: u32) -> Vec<u8> {
        let jpeg = encoded_image(stored_width, stored_height, image::ImageFormat::Jpeg);
        let mut exif = b"Exif\0\0II*\0".to_vec();
        exif.extend_from_slice(&8u32.to_le_bytes());
        exif.extend_from_slice(&1u16.to_le_bytes());
        exif.extend_from_slice(&[0x12, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00]);
        exif.extend_from_slice(&6u32.to_le_bytes());
        exif.extend_from_slice(&0u32.to_le_bytes());
        let length = u16::try_from(exif.len() + 2).expect("short segment");
        let mut out = jpeg[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(&exif);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn image_preview_reports_the_full_size_and_a_small_inline_jpeg() {
        let png = encoded_image(1400, 1981, image::ImageFormat::Png);
        let preview =
            render_image_preview(&png, IMAGE_PREVIEW_MAX_SOURCE_SIDE, IMAGE_PREVIEW_MAX_ALLOC)
                .expect("renders");
        assert_eq!((preview.width, preview.height), (1400, 1981));
        let jpeg = preview.jpeg.expect("inline preview");
        assert!(
            jpeg.starts_with(&[0xFF, 0xD8]),
            "the inline preview is a JPEG"
        );
        let thumbnail = image::load_from_memory(&jpeg).expect("decodes");
        assert_eq!(
            thumbnail.height(),
            100,
            "phones only draw an inline preview about the size the official apps send"
        );
        assert!(
            thumbnail.width() < thumbnail.height(),
            "keeps the aspect ratio"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn image_preview_follows_the_exif_orientation() {
        let preview = render_image_preview(
            &rotated_jpeg(40, 20),
            IMAGE_PREVIEW_MAX_SOURCE_SIDE,
            IMAGE_PREVIEW_MAX_ALLOC,
        )
        .expect("renders");
        assert_eq!(
            (preview.width, preview.height),
            (20, 40),
            "a portrait photo stored sideways is reported as portrait"
        );
        let thumbnail =
            image::load_from_memory(&preview.jpeg.expect("inline preview")).expect("decodes");
        assert!(thumbnail.width() < thumbnail.height());
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn image_preview_fails_on_unreadable_or_oversized_input() {
        assert!(
            render_image_preview(
                b"not an image",
                IMAGE_PREVIEW_MAX_SOURCE_SIDE,
                IMAGE_PREVIEW_MAX_ALLOC
            )
            .is_err()
        );
        let png = encoded_image(40, 40, image::ImageFormat::Png);
        assert!(
            render_image_preview(&png, 20, IMAGE_PREVIEW_MAX_ALLOC).is_err(),
            "images over the decoding limit get no preview"
        );
    }

    /// The side limits let a small file through whose decoded buffer is huge:
    /// a 10000x10000 RGB JPEG is inside 12000 px per side but needs 300 MB.
    /// The decoder enforces dimensions, not the allocation budget, so the
    /// budget has to be checked before the buffer is asked for.
    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn image_preview_refuses_a_decode_that_would_blow_the_allocation_budget() {
        let png = encoded_image(200, 200, image::ImageFormat::Png);
        let needed = 200u64 * 200 * 3;

        let error = render_image_preview(&png, IMAGE_PREVIEW_MAX_SOURCE_SIDE, needed - 1)
            .expect_err("a decode over the budget is refused");
        assert!(error.contains("budget"), "{error}");

        assert!(
            render_image_preview(&png, IMAGE_PREVIEW_MAX_SOURCE_SIDE, needed).is_ok(),
            "the same image renders when the budget covers it"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn image_preview_fills_the_image_card_fields() {
        let mut image = waproto::whatsapp::message::ImageMessage::default();
        ImagePreview {
            width: 1400,
            height: 1981,
            jpeg: Some(vec![0xFF, 0xD8, 0xFF]),
        }
        .apply_to(&mut image);
        assert_eq!((image.width, image.height), (Some(1400), Some(1981)));
        assert_eq!(image.jpeg_thumbnail, Some(vec![0xFF, 0xD8, 0xFF]));

        let mut size_only = waproto::whatsapp::message::ImageMessage::default();
        ImagePreview {
            width: 10,
            height: 20,
            jpeg: None,
        }
        .apply_to(&mut size_only);
        assert_eq!((size_only.width, size_only.height), (Some(10), Some(20)));
        assert_eq!(size_only.jpeg_thumbnail, None);
    }

    /// Wrap one message in the single-entry batch that 0.7 delivers for live
    /// traffic, so tests keep expressing "one inbound message" directly.
    #[cfg(feature = "whatsapp-web")]
    fn single_message_event(
        message: std::sync::Arc<waproto::whatsapp::Message>,
        info: std::sync::Arc<wacore::types::message::MessageInfo>,
    ) -> wacore::types::events::Event {
        message_batch_event(vec![(message, info)])
    }

    /// Build the real multi-message event shape delivered by offline drains.
    #[cfg(feature = "whatsapp-web")]
    fn message_batch_event(
        messages: Vec<(
            std::sync::Arc<waproto::whatsapp::Message>,
            std::sync::Arc<wacore::types::message::MessageInfo>,
        )>,
    ) -> wacore::types::events::Event {
        use wacore::types::events::{BatchOrigin, InboundMessage, MessageBatch};
        // Both types are #[non_exhaustive]; bon builders are the supported
        // construction path. `hook_committed` defaults to false.
        let inbound = messages
            .into_iter()
            .map(|(message, info)| {
                InboundMessage::builder()
                    .message(message)
                    .info(info)
                    .build()
            })
            .collect::<Vec<_>>();
        wacore::types::events::Event::Messages(
            MessageBatch::builder()
                .messages(std::sync::Arc::from(inbound))
                .origin(BatchOrigin::Live)
                .build(),
        )
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn clear_persisted_session_removes_db_triple_and_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("session.db");
        let db_str = db.to_string_lossy().into_owned();
        std::fs::write(&db, b"db").unwrap();
        std::fs::write(format!("{db_str}-wal"), b"wal").unwrap();
        std::fs::write(format!("{db_str}-shm"), b"shm").unwrap();

        let removed = WhatsAppWebChannel::clear_persisted_session(&db_str).unwrap();
        assert_eq!(removed.len(), 3);
        for path in WhatsAppWebChannel::session_file_paths(&db_str) {
            assert!(
                !std::path::Path::new(&path).exists(),
                "{path} must be removed"
            );
        }

        // Relinking an already unpaired channel is a safe no-op that
        // must not create the database.
        let removed = WhatsAppWebChannel::clear_persisted_session(&db_str).unwrap();
        assert!(removed.is_empty());
        assert!(!db.exists());

        // Empty session_path (channel saved without one) clears nothing.
        assert!(
            WhatsAppWebChannel::clear_persisted_session("")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn media_markers_reuse_shared_parser_kinds() {
        let (cleaned, raw) = super::super::util::parse_attachment_markers(
            "send [IMAGE:photo.png] [DOCUMENT:report.pdf] [VOICE:voice.ogg]",
        );
        let markers = raw
            .into_iter()
            .filter_map(|(kind, target)| WhatsAppMarker::from_shared_marker(kind, target))
            .collect::<Vec<_>>();

        assert_eq!(cleaned, "send");
        assert_eq!(
            markers
                .iter()
                .map(|marker| match marker {
                    WhatsAppMarker::Media(m) => m.kind,
                    WhatsAppMarker::Location(_) => panic!("expected media markers"),
                })
                .collect::<Vec<_>>(),
            vec![
                WhatsAppMediaKind::Image,
                WhatsAppMediaKind::Document,
                WhatsAppMediaKind::Voice
            ]
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn from_shared_marker_routes_location_kind() {
        let marker = WhatsAppMarker::from_shared_marker(
            "LOCATION".to_string(),
            "40.7128,-74.0060".to_string(),
        );
        assert!(matches!(marker, Some(WhatsAppMarker::Location(_))));
        // An invalid target still routes to the Location variant: validation
        // is deferred to the send path so the marker counts as a failed
        // delivery instead of being stripped silently.
        assert_eq!(
            WhatsAppMarker::from_shared_marker("LOCATION".to_string(), "999,999".to_string()),
            Some(WhatsAppMarker::Location("999,999".to_string()))
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn validate_location_target_refuses_invalid_coordinates() {
        // Out-of-range and malformed targets are refused — the send loop
        // counts these as failed deliveries rather than sending a bogus pin.
        for target in ["999,999", "not-a-number,0.0", "40.7128", ""] {
            let err = validate_whatsapp_location_target(target)
                .expect_err("out-of-range or malformed target must be refused");
            assert_eq!(err.kind(), WhatsAppMarkerFailure::Refused, "{target}");
        }
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn allowed_groups_empty_admits_only_under_policy_all() {
        // An empty list is NOT permission. It admits only when the operator
        // asked for open groups by name.
        let jid = "123456789012345@g.us";
        assert!(super::is_group_chat_allowed(
            jid,
            &[],
            &zeroclaw_config::schema::WhatsAppChatPolicy::All
        ));
        assert!(!super::is_group_chat_allowed(
            jid,
            &[],
            &zeroclaw_config::schema::WhatsAppChatPolicy::Allowlist
        ));
        assert!(!super::is_group_chat_allowed(
            jid,
            &[],
            &zeroclaw_config::schema::WhatsAppChatPolicy::Ignore
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn allowed_groups_non_empty_still_filters_under_policy_all() {
        // CONTROL for the test above: `all` widens the empty-list default, it does
        // not override an explicit list. Without this, a change making `all` bypass
        // filtering entirely would still pass every other case here.
        let groups = vec!["123456789012345".to_string()];
        assert!(super::is_group_chat_allowed(
            "123456789012345@g.us",
            &groups,
            &zeroclaw_config::schema::WhatsAppChatPolicy::All
        ));
        assert!(!super::is_group_chat_allowed(
            "999999999999999@g.us",
            &groups,
            &zeroclaw_config::schema::WhatsAppChatPolicy::All
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn direct_message_jid_accepts_individual_chats() {
        // Both individual addressing forms: the plain phone JID and the hidden
        // identity (LID) form WhatsApp uses for privacy-preserving chats.
        assert!(super::is_direct_message_jid("15550001111@s.whatsapp.net"));
        assert!(super::is_direct_message_jid("100000000000001@lid"));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn direct_message_jid_rejects_groups() {
        assert!(!super::is_direct_message_jid("120363000000000001@g.us"));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn direct_message_jid_rejects_non_conversational_domains() {
        // Not groups, but not direct messages either. An allow-list keeps these
        // out; a "not @g.us" check would wrongly admit all three.
        assert!(!super::is_direct_message_jid("status@broadcast"));
        assert!(!super::is_direct_message_jid(
            "120363000000000000@newsletter"
        ));
        assert!(!super::is_direct_message_jid("15550001111@call"));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn direct_message_jid_rejects_malformed_input() {
        assert!(!super::is_direct_message_jid(""));
        assert!(!super::is_direct_message_jid("15550001111"));
        assert!(!super::is_direct_message_jid("@s.whatsapp.net"));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn validate_marker_target_accepts_workspace_relative_file() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let file = workspace.path().join("photo.png");
        std::fs::write(&file, b"png").expect("write fixture");

        let resolved =
            validate_whatsapp_marker_target("photo.png", Some(workspace.path())).expect("inside");

        assert_eq!(resolved, file.canonicalize().expect("canonical fixture"));
    }

    /// Both modes enforce chat policies; only a personal self-chat bypasses.
    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn composed_policy_decision_enforces_both_modes() {
        use zeroclaw_config::schema::{WhatsAppChatPolicy as Policy, WhatsAppWebMode as Mode};

        for mode in [Mode::Business, Mode::Personal] {
            for is_group in [false, true] {
                assert_eq!(
                    super::composed_chat_policy_decision(
                        &mode,
                        false,
                        is_group,
                        &Policy::Allowlist,
                        &Policy::Allowlist,
                        false,
                    ),
                    super::ChatPolicyDecision::DropUnrecognizedSender,
                    "{mode:?} must enforce the default policy (is_group={is_group})"
                );
            }
        }

        assert_eq!(
            super::composed_chat_policy_decision(
                &Mode::Personal,
                true,
                false,
                &Policy::Allowlist,
                &Policy::Allowlist,
                false,
            ),
            super::ChatPolicyDecision::Admit,
            "a recognized personal self-chat bypasses chat-type policy"
        );
        assert_eq!(
            super::composed_chat_policy_decision(
                &Mode::Business,
                true,
                false,
                &Policy::Allowlist,
                &Policy::Allowlist,
                false,
            ),
            super::ChatPolicyDecision::DropUnrecognizedSender,
            "business mode cannot acquire the personal self-chat exception"
        );
    }

    /// The mode-independence invariant, pinned at the decision itself.
    ///
    /// Business mode used to skip this decision entirely, so an unlisted sender
    /// was admitted no matter what `dm_policy` said. The policy default is
    /// `Allowlist`, so a default business config must now drop that sender, and
    /// the same holds for a group.
    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn default_policy_drops_unrecognized_sender_in_dm_and_group() {
        use zeroclaw_config::schema::WhatsAppChatPolicy as Policy;
        let default = Policy::default();
        assert_eq!(default, Policy::Allowlist, "the default must be allowlist");

        for is_group in [false, true] {
            assert_eq!(
                super::chat_type_policy_decision(is_group, &default, &default, false),
                super::ChatPolicyDecision::DropUnrecognizedSender,
                "an unrecognized sender must be dropped under the default policy \
                 (is_group={is_group})"
            );
            assert_eq!(
                super::chat_type_policy_decision(is_group, &default, &default, true),
                super::ChatPolicyDecision::Admit,
                "a recognized sender is still admitted (is_group={is_group})"
            );
        }
    }

    /// `ignore` must keep its own reason rather than collapsing into the
    /// unrecognized-sender path, because the two emit different log lines and a
    /// reader diagnosing a silent bot needs to know which one fired.
    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn ignore_reports_the_chat_type_it_dropped() {
        use zeroclaw_config::schema::WhatsAppChatPolicy as Policy;

        assert_eq!(
            super::chat_type_policy_decision(true, &Policy::Ignore, &Policy::All, true),
            super::ChatPolicyDecision::DropGroupIgnored
        );
        assert_eq!(
            super::chat_type_policy_decision(false, &Policy::All, &Policy::Ignore, true),
            super::ChatPolicyDecision::DropDmIgnored
        );
    }

    /// Walks the whole decision surface: both chat types, all three policies,
    /// recognized and unrecognized. Twelve rows, and the point of walking rather
    /// than sampling is that the group and DM halves must agree row for row.
    ///
    /// Mode does not appear here, which is the fix: the decision takes no
    /// `WhatsAppWebMode`, so it cannot depend on one. This decision was
    /// previously reachable only under personal mode.
    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn decision_surface_is_total_and_mode_free() {
        use super::ChatPolicyDecision as D;
        use zeroclaw_config::schema::WhatsAppChatPolicy as Policy;

        let mut rows = 0;
        for policy in [Policy::Allowlist, Policy::Ignore, Policy::All] {
            for recognized in [false, true] {
                let group =
                    super::chat_type_policy_decision(true, &policy, &Policy::All, recognized);
                let dm = super::chat_type_policy_decision(false, &Policy::All, &policy, recognized);

                let (want_group, want_dm) = match (&policy, recognized) {
                    (Policy::Ignore, _) => (D::DropGroupIgnored, D::DropDmIgnored),
                    (Policy::All, _) => (D::Admit, D::Admit),
                    (Policy::Allowlist, false) => {
                        (D::DropUnrecognizedSender, D::DropUnrecognizedSender)
                    }
                    (Policy::Allowlist, true) => (D::Admit, D::Admit),
                };

                assert_eq!(group, want_group, "group row {policy:?}/{recognized}");
                assert_eq!(dm, want_dm, "dm row {policy:?}/{recognized}");
                rows += 1;
            }
        }
        assert_eq!(rows, 6, "the loop must not silently walk zero rows");
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn allowed_groups_full_jid_match() {
        let groups = vec!["123456789012345@g.us".to_string()];
        assert!(super::is_group_chat_allowed(
            "123456789012345@g.us",
            &groups,
            &zeroclaw_config::schema::WhatsAppChatPolicy::Allowlist
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn validate_marker_target_rejects_workspace_escape() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::NamedTempFile::new().expect("outside file");

        let err = validate_whatsapp_marker_target(
            outside.path().to_str().expect("utf8 path"),
            Some(workspace.path()),
        )
        .expect_err("outside workspace must be refused");

        assert_eq!(err.kind(), WhatsAppMarkerFailure::Refused);
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn allowed_groups_jid_prefix_match() {
        let groups = vec!["123456789012345".to_string()];
        assert!(super::is_group_chat_allowed(
            "123456789012345@g.us",
            &groups,
            &zeroclaw_config::schema::WhatsAppChatPolicy::Allowlist
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn validate_marker_target_rejects_without_workspace() {
        let err = validate_whatsapp_marker_target("photo.png", None)
            .expect_err("workspace is required for local marker reads");

        assert_eq!(err.kind(), WhatsAppMarkerFailure::Refused);
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn allowed_groups_no_match_drops() {
        let groups = vec!["123456789012345".to_string()];
        assert!(!super::is_group_chat_allowed(
            "999999999999999@g.us",
            &groups,
            &zeroclaw_config::schema::WhatsAppChatPolicy::Allowlist
        ));
        // Blank / whitespace-only entries never match.
        assert!(!super::is_group_chat_allowed(
            "123@g.us",
            &["   ".to_string()],
            &zeroclaw_config::schema::WhatsAppChatPolicy::Allowlist
        ));
        // Prefix entries match the user part EXACTLY, not as a string prefix:
        // "123" must admit "123@g.us" but never "123999@g.us".
        assert!(super::is_group_chat_allowed(
            "123@g.us",
            &["123".to_string()],
            &zeroclaw_config::schema::WhatsAppChatPolicy::Allowlist
        ));
        assert!(!super::is_group_chat_allowed(
            "123999@g.us",
            &["123".to_string()],
            &zeroclaw_config::schema::WhatsAppChatPolicy::Allowlist
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn validate_marker_target_marks_missing_as_failed() {
        let workspace = tempfile::tempdir().expect("tempdir");

        let err = validate_whatsapp_marker_target("missing.png", Some(workspace.path()))
            .expect_err("missing file should fail delivery");

        assert_eq!(err.kind(), WhatsAppMarkerFailure::Failed);
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn delivery_failure_note_is_count_only() {
        let count: usize = 2;
        let note = whatsapp_delivery_failure_note(count).expect("note");

        // Locale-independent: count is always rendered as Arabic digits in
        // every shipped locale's FTL template (`{$count}`). The literal
        // English phrase used to live here but the assertion would break
        // on any CI runner with a non-English `$LANG`.
        assert!(!note.is_empty(), "note must be non-empty");
        assert!(
            note.contains(count.to_string().as_str()),
            "note must contain the failure count"
        );
        assert!(!note.contains("/"), "note must not contain path separators");
        assert!(
            !note.contains("workspace"),
            "note must not echo local marker targets"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn voice_marker_uses_opus_mime_for_ogg_family() {
        assert_eq!(
            WhatsAppMediaKind::Voice.mime_for_path(Path::new("voice.ogg")),
            "audio/ogg; codecs=opus"
        );
        assert_eq!(
            WhatsAppMediaKind::Audio.mime_for_path(Path::new("voice.ogg")),
            "audio/ogg"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn allowed_groups_dm_bypasses_filter() {
        // DMs bypass: the call site gates on `is_group`, so a direct message
        // is admitted even when a non-empty allowed_groups would not match it.
        let groups = vec!["123456789012345".to_string()];
        let is_group = false;
        let dm_jid = "987654321098765@s.whatsapp.net";
        let admitted = !is_group
            || super::is_group_chat_allowed(
                dm_jid,
                &groups,
                &zeroclaw_config::schema::WhatsAppChatPolicy::Allowlist,
            );
        assert!(admitted);
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn passive_group_context_is_default_off_and_group_only() {
        assert!(!WhatsAppWebChannel::should_record_passive_group_context(
            false, true, false
        ));
        assert!(!WhatsAppWebChannel::should_record_passive_group_context(
            true, false, false
        ));
        assert!(!WhatsAppWebChannel::should_record_passive_group_context(
            true, true, true
        ));
        assert!(WhatsAppWebChannel::should_record_passive_group_context(
            true, true, false
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn passive_group_context_uses_reply_target_scope_for_groups() {
        assert_eq!(
            WhatsAppWebChannel::group_context_scope(false, true),
            ChannelConversationScope::Sender
        );
        assert_eq!(
            WhatsAppWebChannel::group_context_scope(true, false),
            ChannelConversationScope::Sender
        );
        assert_eq!(
            WhatsAppWebChannel::group_context_scope(true, true),
            ChannelConversationScope::ReplyTarget
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_channel_name() {
        let mention_only = false;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test-whatsapp.db".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
            Arc::new(Vec::new),
        );
        assert_eq!(ch.name(), "whatsapp");
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_number_allowed_exact() {
        let mention_only = false;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test-whatsapp.db".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
            Arc::new(Vec::new),
        );
        assert!(ch.is_number_allowed("+1234567890"));
        assert!(!ch.is_number_allowed("+9876543210"));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_number_allowed_wildcard() {
        let mention_only = false;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test.db".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["*".into()]),
            Arc::new(Vec::new),
        );
        assert!(ch.is_number_allowed("+1234567890"));
        assert!(ch.is_number_allowed("+9999999999"));
    }

    /// Resolve peers the way the daemon does, so these cover the
    /// config-to-adapter boundary rather than a hand-built vector.
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_peers_from_config(toml_src: &str, alias: &str) -> Vec<String> {
        let config: zeroclaw_config::schema::Config =
            toml::from_str(toml_src).expect("peer-group config should parse");

        config.channel_external_peers("whatsapp", alias)
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_deny_survives_the_whitespace_wildcard() {
        // A wildcard written with surrounding whitespace, resolved from config.
        // Gating deny emission on the exact string `"*"` emitted no deny here
        // while this matcher still read the entry as a wildcard.
        let peers = whatsapp_peers_from_config(
            r#"
            [peer_groups.whatsapp_ops]
            channel = "whatsapp.ops"
            external_peers = [" * "]
            ignore = ["+15551234567"]
            "#,
            "ops",
        );
        assert!(!WhatsAppWebChannel::is_number_allowed_for_list(
            &peers,
            "+15551234567"
        ));
        assert!(WhatsAppWebChannel::is_number_allowed_for_list(
            &peers,
            "+15559999999"
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_ignore_denies_an_equivalent_phone_spelling() {
        // This matcher reads `+15551234567` and `15551234567` as one number.
        // Resolving the deny by comparing raw strings kept the grant and
        // dropped the `ignore`, and the sender was then admitted.
        let peers = whatsapp_peers_from_config(
            r#"
            [peer_groups.whatsapp_ops]
            channel = "whatsapp.ops"
            external_peers = ["+15551234567"]
            ignore = ["15551234567"]
            "#,
            "ops",
        );
        assert!(!WhatsAppWebChannel::is_number_allowed_for_list(
            &peers,
            "+15551234567"
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_ignore_denies_a_jid_spelling_of_the_granted_number() {
        // A sender arrives as a JID, which normalizes to the same number.
        let peers = whatsapp_peers_from_config(
            r#"
            [peer_groups.whatsapp_ops]
            channel = "whatsapp.ops"
            external_peers = ["*"]
            ignore = ["15551234567@s.whatsapp.net"]
            "#,
            "ops",
        );
        assert!(!WhatsAppWebChannel::is_number_allowed_for_list(
            &peers,
            "+15551234567"
        ));
        assert!(WhatsAppWebChannel::is_number_allowed_for_list(
            &peers,
            "+15559999999"
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_deny_on_one_sender_number_covers_the_others() {
        // A sender arrives as several numbers (JID, alternate JID, LID
        // mapping). Denying one of them must reject the sender rather than
        // letting the next number pick up the wildcard.
        let peers = vec!["*".to_string(), "!+15551234567".to_string()];
        assert!(!WhatsAppWebChannel::are_numbers_allowed_for_list(
            &peers,
            &["+447700900000", "+15551234567"]
        ));
        assert!(WhatsAppWebChannel::are_numbers_allowed_for_list(
            &peers,
            &["+447700900000", "+15559999999"]
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_number_denied_empty() {
        let mention_only = false;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test.db".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(Vec::new),
            Arc::new(Vec::new),
        );
        // Empty allowlist means "deny all" (matches channel-wide allowlist policy).
        assert!(!ch.is_number_allowed("+1234567890"));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_normalize_phone_adds_plus() {
        let mention_only = false;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test-whatsapp.db".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
            Arc::new(Vec::new),
        );
        assert_eq!(ch.normalize_phone("1234567890"), "+1234567890");
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_normalize_phone_preserves_plus() {
        let mention_only = false;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test-whatsapp.db".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
            Arc::new(Vec::new),
        );
        assert_eq!(ch.normalize_phone("+1234567890"), "+1234567890");
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_normalize_phone_from_jid() {
        let mention_only = false;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test-whatsapp.db".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
            Arc::new(Vec::new),
        );
        assert_eq!(
            ch.normalize_phone("1234567890@s.whatsapp.net"),
            "+1234567890"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_normalize_phone_token_accepts_formatted_phone() {
        assert_eq!(
            WhatsAppWebChannel::normalize_phone_token("+1 (555) 123-4567"),
            Some("+15551234567".to_string())
        );
    }

    /// Config to writer to runtime, on the one identity WhatsApp spells three
    /// ways. The writer runs on every reconnect, so a wrong answer here is the
    /// operator's whole experience of pairing: told it worked, never able to
    /// talk, and no conflict to act on.
    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_pairing_write_honors_a_deny_spelled_as_a_jid() {
        use zeroclaw_config::multi_agent::{PeerGroupConfig, PeerUsername};
        use zeroclaw_config::providers::ChannelRef;

        let mut config = zeroclaw_config::schema::Config::default();
        config.channels.whatsapp.insert(
            "admin".to_string(),
            zeroclaw_config::schema::WhatsAppConfig {
                enabled: true,
                ..Default::default()
            },
        );
        config.peer_groups.insert(
            "whatsapp_admin".to_string(),
            PeerGroupConfig {
                channel: ChannelRef::new("whatsapp.admin".to_string()),
                external_peers: vec![PeerUsername::new("+15551234567".to_string())],
                ignore: vec![PeerUsername::new("15551234567@s.whatsapp.net".to_string())],
                ..Default::default()
            },
        );

        let resolved = config.channel_external_peers("whatsapp", "admin");
        assert!(
            !WhatsAppWebChannel::are_numbers_allowed_for_list(&resolved, &["+15551234567"]),
            "the runtime canonicalizes the JID deny and rejects the account"
        );

        let err = crate::identity_persist::merge_external_peer(
            &mut config,
            "whatsapp",
            "admin",
            "+15551234567",
            WhatsAppWebChannel::phone_matches,
        )
        .expect_err("the deny names this account, whichever spelling it uses");
        assert!(
            err.to_string().contains("ignore"),
            "the operator is told which field to edit: {err}"
        );

        config
            .peer_groups
            .get_mut("whatsapp_admin")
            .expect("group exists")
            .ignore
            .clear();
        assert!(
            crate::identity_persist::merge_external_peer(
                &mut config,
                "whatsapp",
                "admin",
                "+15551234567",
                WhatsAppWebChannel::phone_matches,
            )
            .expect("merge succeeds once the deny is gone")
            .is_none(),
            "the existing grant is now effective, so nothing needs writing"
        );
        let resolved = config.channel_external_peers("whatsapp", "admin");
        assert!(
            WhatsAppWebChannel::are_numbers_allowed_for_list(&resolved, &["+15551234567"]),
            "a no-op write leaves the identity admissible: the recovery path ends"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_allowlist_matches_normalized_format() {
        let allowed = vec!["+15551234567".to_string()];
        assert!(WhatsAppWebChannel::is_number_allowed_for_list(
            &allowed,
            "+1 (555) 123-4567"
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_sender_candidates_include_sender_alt_phone() {
        let sender = Jid::lid("76188559093817");
        let sender_alt = Jid::pn("15551234567");
        let candidates =
            WhatsAppWebChannel::sender_phone_candidates(&sender, Some(&sender_alt), None);
        assert!(candidates.contains(&"+15551234567".to_string()));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_sender_candidates_include_lid_mapping_phone() {
        let sender = Jid::lid("76188559093817");
        let candidates =
            WhatsAppWebChannel::sender_phone_candidates(&sender, None, Some("15551234567"));
        assert!(candidates.contains(&"+15551234567".to_string()));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn compute_reply_target_preserves_lid_dm() {
        // LID DM → preserved as-is (library handles LID resolution internally)
        let chat_jid = "76188559093817@lid";
        let result = WhatsAppWebChannel::compute_reply_target(chat_jid);
        assert_eq!(
            result, chat_jid,
            "LID DM must be preserved - library handles LID addressing natively"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn compute_reply_target_preserves_pn_dm() {
        // PN DM → preserved as-is
        let chat_jid = "15551234567@s.whatsapp.net";
        let result = WhatsAppWebChannel::compute_reply_target(chat_jid);
        assert_eq!(result, chat_jid, "PN DM must preserve original chat JID");
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn compute_reply_target_preserves_group() {
        // Group chat → preserved as-is
        let chat_jid = "120363012345678901@g.us";
        let result = WhatsAppWebChannel::compute_reply_target(chat_jid);
        assert_eq!(
            result, chat_jid,
            "Group chat must preserve original chat JID"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn lid_rejection_diagnostic_empty_for_non_lid_sender() {
        let sender = Jid::pn("15551234567");
        let diag = WhatsAppWebChannel::lid_rejection_diagnostic(&sender, None);
        assert!(
            diag.is_empty(),
            "non-LID senders must not generate any LID-resolution suffix; got {diag:?}"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn lid_rejection_diagnostic_names_resolution_failure_for_lid_with_no_phone() {
        let sender = Jid::lid("76188559093817");
        let diag = WhatsAppWebChannel::lid_rejection_diagnostic(&sender, None);
        assert!(
            diag.contains("LID→phone resolution returned None"),
            "diagnostic must name the resolution failure mode #6350 describes; got {diag:?}"
        );
        // These two previously asserted the OPPOSITE, which is what pinned the defect in place:
        // the diagnostic reaches a WARN that fans out to the dashboard and the persisted log, so
        // the raw identity must never appear, and the remedy must name a field that still exists.
        assert!(
            !diag.contains("76188559093817"),
            "the raw LID is durable personal data and must not reach the log sink; got {diag:?}"
        );
        assert!(
            !diag.contains("allowed_numbers"),
            "allowed_numbers is a V2 field that migrates into peer_groups, so it is not a remedy an \
             operator can apply today; got {diag:?}"
        );
        assert!(
            diag.contains("external_peers"),
            "diagnostic must point at the admission source the current config model actually uses; \
             got {diag:?}"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn lid_rejection_diagnostic_never_leaks_sender_identity_on_either_branch() {
        // The control for the two negative assertions above. Those alone would pass against a
        // diagnostic that returned the empty string on every input, so this drives BOTH LID
        // branches and requires each to stay informative while carrying no identifier.
        let sender = Jid::lid("76188559093817");
        for (label, mapped) in [
            ("unresolved", None),
            ("resolved-mismatch", Some("15551234567")),
        ] {
            let diag = WhatsAppWebChannel::lid_rejection_diagnostic(&sender, mapped);
            assert!(
                !diag.is_empty(),
                "{label}: a LID sender must still produce a reason; got {diag:?}"
            );
            assert!(
                !diag.contains("76188559093817"),
                "{label}: the LID user value must not appear; got {diag:?}"
            );
            assert!(
                !diag.contains(&sender.to_string()),
                "{label}: the full sender JID must not appear; got {diag:?}"
            );
            if let Some(phone) = mapped {
                assert!(
                    !diag.contains(phone),
                    "{label}: the resolved phone number must not appear either; got {diag:?}"
                );
            }
        }
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn lid_rejection_diagnostic_distinguishes_resolved_phone_mismatch() {
        // LID resolved successfully but the resulting phone wasn't in the
        // allowlist. Different cause from the resolution failure path; the
        // operator shouldn't be steered toward the LID workaround.
        let sender = Jid::lid("76188559093817");
        let diag = WhatsAppWebChannel::lid_rejection_diagnostic(&sender, Some("15551234567"));
        assert!(
            !diag.contains("LID→phone resolution returned None"),
            "must not suggest resolution failed when mapped_phone is Some; got {diag:?}"
        );
        assert!(
            diag.contains("did not match"),
            "diagnostic must explain the resolved phone failed the allowlist; got {diag:?}"
        );
    }

    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn whatsapp_web_health_check_disconnected() {
        let mention_only = false;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test-whatsapp.db".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
            Arc::new(Vec::new),
        );
        assert!(!ch.health_check().await);
    }

    #[cfg(feature = "whatsapp-web")]
    #[derive(Default)]
    struct TestCacheStore {
        entries: tokio::sync::RwLock<std::collections::HashMap<(String, String), Vec<u8>>>,
        changed: tokio::sync::Notify,
    }

    #[cfg(feature = "whatsapp-web")]
    impl TestCacheStore {
        async fn contains(&self, namespace: &str, key: &str) -> bool {
            self.entries
                .read()
                .await
                .contains_key(&(namespace.to_string(), key.to_string()))
        }

        async fn wait_until_present(&self, namespace: &str, key: &str) {
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    let changed = self.changed.notified();
                    if self.contains(namespace, key).await {
                        return;
                    }
                    changed.await;
                }
            })
            .await
            .expect("client cache warm-up must complete");
        }
    }

    #[cfg(feature = "whatsapp-web")]
    #[async_trait::async_trait]
    impl wacore::store::CacheStore for TestCacheStore {
        async fn get(&self, namespace: &str, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
            Ok(self
                .entries
                .read()
                .await
                .get(&(namespace.to_string(), key.to_string()))
                .cloned())
        }

        async fn set(
            &self,
            namespace: &str,
            key: &str,
            value: &[u8],
            _ttl: Option<std::time::Duration>,
        ) -> anyhow::Result<()> {
            self.entries
                .write()
                .await
                .insert((namespace.to_string(), key.to_string()), value.to_vec());
            self.changed.notify_waiters();
            Ok(())
        }

        async fn delete(&self, namespace: &str, key: &str) -> anyhow::Result<()> {
            self.entries
                .write()
                .await
                .remove(&(namespace.to_string(), key.to_string()));
            Ok(())
        }

        async fn clear(&self, namespace: &str) -> anyhow::Result<()> {
            self.entries
                .write()
                .await
                .retain(|(entry_namespace, _), _| entry_namespace != namespace);
            Ok(())
        }
    }

    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn whatsapp_client_persistent_lid_mapping_drives_allowlist() {
        use wacore::store::CacheStore;
        use wacore::store::traits::{LidPnMappingEntry, ProtocolStore};
        use wacore::types::message::{MessageInfo, MessageSource};
        use whatsapp_rust::bot::Bot;
        use whatsapp_rust::{CacheConfig, CacheStores, TokioRuntime};
        use whatsapp_rust_tokio_transport::TokioWebSocketTransportFactory;
        use whatsapp_rust_ureq_http_client::UreqHttpClient;

        const LID_NAMESPACE: &str = "lid_pn_by_lid";
        const PN_NAMESPACE: &str = "lid_pn_by_pn";
        const SENTINEL_LID: &str = "37207519834000";
        const SENTINEL_PHONE: &str = "15550000000";
        const ALLOWED_LID: &str = "37207519834264";
        const WRONG_LID: &str = "37207519834265";
        const MISSING_LID: &str = "37207519834266";

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::whatsapp_storage::RusqliteStore::new(tmp.path()).unwrap());
        ProtocolStore::put_lid_mapping(
            &*store,
            &LidPnMappingEntry {
                lid: SENTINEL_LID.to_string(),
                phone_number: SENTINEL_PHONE.to_string(),
                created_at: 1_700_000_000,
                updated_at: 1_700_000_000,
                learning_source: "usync".to_string(),
            },
        )
        .await
        .unwrap();

        let cache = Arc::new(TestCacheStore::default());
        let bot = Bot::builder()
            .with_backend_arc(store.clone())
            .with_transport_factory(TokioWebSocketTransportFactory::new())
            .with_http_client(UreqHttpClient::new())
            .with_runtime(TokioRuntime)
            .with_cache_config(CacheConfig {
                cache_stores: CacheStores {
                    lid_pn_cache: Some(cache.clone()),
                    ..Default::default()
                },
                ..Default::default()
            })
            .build()
            .await
            .unwrap();
        let client = bot.client();

        cache.wait_until_present(LID_NAMESPACE, SENTINEL_LID).await;
        cache.wait_until_present(PN_NAMESPACE, SENTINEL_PHONE).await;
        cache.clear(LID_NAMESPACE).await.unwrap();
        cache.clear(PN_NAMESPACE).await.unwrap();

        for (lid, phone) in [(ALLOWED_LID, "15551234567"), (WRONG_LID, "15550001111")] {
            ProtocolStore::put_lid_mapping(
                &*store,
                &LidPnMappingEntry {
                    lid: lid.to_string(),
                    phone_number: phone.to_string(),
                    created_at: 1_700_000_100,
                    updated_at: 1_700_000_100,
                    learning_source: "usync".to_string(),
                },
            )
            .await
            .unwrap();
            assert!(!cache.contains(LID_NAMESPACE, lid).await);
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let context = WhatsAppInboundContext {
            tx,
            alias: Arc::new("persistent-lid-test".to_string()),
            peer_resolver: Arc::new(|| vec!["+15551234567".to_string()]),
            allowed_groups_resolver: Arc::new(Vec::new),
            mode: zeroclaw_config::schema::WhatsAppWebMode::Personal,
            dm_policy: zeroclaw_config::schema::WhatsAppChatPolicy::Allowlist,
            group_policy: zeroclaw_config::schema::WhatsAppChatPolicy::Allowlist,
            self_chat_mode: false,
            mention_only: false,
            passive_group_context: false,
            bot_phone: Arc::new(Mutex::new(None)),
            bot_lid: Arc::new(Mutex::new(None)),
            dm_mention_patterns: Arc::new(Vec::new()),
            group_mention_patterns: Arc::new(Vec::new()),
            transcription_config: None,
            transcription_manager: None,
            voice_chats: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        };
        let message_event = |lid: &str| {
            single_message_event(
                Arc::new(waproto::whatsapp::Message {
                    conversation: Some("persistent LID mapping".to_string()),
                    ..Default::default()
                }),
                Arc::new(MessageInfo {
                    source: MessageSource {
                        chat: Jid::lid(lid),
                        sender: Jid::lid(lid),
                        is_from_me: false,
                        is_group: false,
                        ..Default::default()
                    },
                    id: format!("lid-test-{lid}"),
                    r#type: "text".to_string(),
                    push_name: "LID Test Sender".to_string(),
                    timestamp: chrono::Utc::now(),
                    ..Default::default()
                }),
            )
        };

        let accepted_event = message_event(ALLOWED_LID);
        WhatsAppWebChannel::handle_inbound_message_event(&accepted_event, &client, &context).await;
        let dispatched = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("allowed mapped sender must reach shared dispatch")
            .expect("shared dispatch sender must remain open");
        assert_eq!(dispatched.sender, "+15551234567");
        assert_eq!(dispatched.content, "persistent LID mapping");
        assert!(cache.contains(LID_NAMESPACE, ALLOWED_LID).await);

        let wrong_event = message_event(WRONG_LID);
        WhatsAppWebChannel::handle_inbound_message_event(&wrong_event, &client, &context).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
                .await
                .is_err(),
            "mapped phone outside the allowlist must not dispatch"
        );

        let missing_event = message_event(MISSING_LID);
        WhatsAppWebChannel::handle_inbound_message_event(&missing_event, &client, &context).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
                .await
                .is_err(),
            "missing persistent mapping must fail closed before dispatch"
        );
    }

    /// The mode-independence invariant, driven through the production path.
    ///
    /// The helper tests above call the decision directly. This one calls
    /// `handle_inbound_message_event`, which is what the event loop dispatches
    /// to, so the group gate, the personal-mode sender semantics, the chat-type
    /// policy and the dispatch tail all run composed. Business mode used to skip
    /// the policy entirely, so every dropped row below reached dispatch under
    /// `mode = "business"` no matter what `dm_policy` or `group_policy` said.
    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn inbound_path_enforces_chat_policy_under_both_modes() {
        use wacore::types::message::{MessageInfo, MessageSource};
        use whatsapp_rust::TokioRuntime;
        use whatsapp_rust::bot::Bot;
        use whatsapp_rust_tokio_transport::TokioWebSocketTransportFactory;
        use whatsapp_rust_ureq_http_client::UreqHttpClient;
        use zeroclaw_config::schema::{WhatsAppChatPolicy as Policy, WhatsAppWebMode as Mode};

        const ALLOWED: &str = "15551234567";
        const UNKNOWN: &str = "15559999999";
        const GROUP_JID: &str = "120363012345678901@g.us";

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::whatsapp_storage::RusqliteStore::new(tmp.path()).unwrap());
        let bot = Bot::builder()
            .with_backend_arc(store)
            .with_transport_factory(TokioWebSocketTransportFactory::new())
            .with_http_client(UreqHttpClient::new())
            .with_runtime(TokioRuntime)
            .build()
            .await
            .unwrap();
        let client = bot.client();

        // Plain phone JIDs, so admission is decided from the JID itself and no
        // LID mapping is in play. `allowed_groups` lists GROUP_JID explicitly,
        // so each group row passes the group-identity gate and reaches the
        // chat-type policy this test is about. An empty list would stop every
        // group row at the identity gate under any policy but `all`, which is
        // the empty-list contract exercised by the dedicated tests above.
        let event = |sender: &str, is_group: bool| {
            let sender_jid: Jid = format!("{sender}@s.whatsapp.net")
                .parse()
                .expect("sender jid parses");
            let chat: Jid = if is_group {
                GROUP_JID.parse().expect("group jid parses")
            } else {
                sender_jid.clone()
            };
            single_message_event(
                Arc::new(waproto::whatsapp::Message {
                    conversation: Some("policy probe".to_string()),
                    ..Default::default()
                }),
                Arc::new(MessageInfo {
                    source: MessageSource {
                        chat,
                        sender: sender_jid,
                        is_from_me: false,
                        is_group,
                        ..Default::default()
                    },
                    id: format!("policy-{sender}-{is_group}"),
                    r#type: "text".to_string(),
                    push_name: "Policy Probe".to_string(),
                    timestamp: chrono::Utc::now(),
                    ..Default::default()
                }),
            )
        };

        let context_for =
            |mode: &Mode, policy: &Policy, tx: tokio::sync::mpsc::Sender<ChannelMessage>| {
                WhatsAppInboundContext {
                    tx,
                    alias: Arc::new("both-modes-policy".to_string()),
                    peer_resolver: Arc::new(|| vec![format!("+{ALLOWED}")]),
                    allowed_groups_resolver: Arc::new(|| vec![GROUP_JID.to_string()]),
                    mode: mode.clone(),
                    dm_policy: policy.clone(),
                    group_policy: policy.clone(),
                    self_chat_mode: false,
                    mention_only: false,
                    passive_group_context: false,
                    bot_phone: Arc::new(Mutex::new(None)),
                    bot_lid: Arc::new(Mutex::new(None)),
                    dm_mention_patterns: Arc::new(Vec::new()),
                    group_mention_patterns: Arc::new(Vec::new()),
                    transcription_config: None,
                    transcription_manager: None,
                    voice_chats: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
                }
            };

        for mode in [Mode::Business, Mode::Personal] {
            for is_group in [false, true] {
                // `allowlist` (the default) must reject an unlisted sender under
                // either mode.
                let (tx, mut rx) = tokio::sync::mpsc::channel(4);
                let context = context_for(&mode, &Policy::Allowlist, tx);

                let unlisted = event(UNKNOWN, is_group);
                WhatsAppWebChannel::handle_inbound_message_event(&unlisted, &client, &context)
                    .await;
                assert!(
                    tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
                        .await
                        .is_err(),
                    "{mode:?} must drop an unlisted sender under allowlist (is_group={is_group})"
                );

                // Control for the assertion above: the same harness, the same
                // mode and the same chat type DO deliver a listed sender, so an
                // empty channel is a policy decision rather than a probe that
                // could never have dispatched anything.
                let listed = event(ALLOWED, is_group);
                WhatsAppWebChannel::handle_inbound_message_event(&listed, &client, &context).await;
                let dispatched = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
                    .await
                    .unwrap_or_else(|_| {
                        panic!("{mode:?} must dispatch a listed sender (is_group={is_group})")
                    })
                    .expect("dispatch sender must remain open");
                assert_eq!(dispatched.content, "policy probe");

                // `ignore` drops the chat type outright, so even the listed
                // sender that just dispatched must now be refused. This is the
                // row that proves the policy is consulted at all rather than the
                // allowlist doing all the work.
                let (tx, mut rx) = tokio::sync::mpsc::channel(4);
                let context = context_for(&mode, &Policy::Ignore, tx);
                let listed = event(ALLOWED, is_group);
                WhatsAppWebChannel::handle_inbound_message_event(&listed, &client, &context).await;
                assert!(
                    tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
                        .await
                        .is_err(),
                    "{mode:?} must honour policy=ignore for a listed sender (is_group={is_group})"
                );
            }
        }
    }

    /// Rejecting one entry in an offline-drain batch must not abandon later
    /// entries, and the accepted messages must preserve their arrival order.
    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn inbound_batch_isolates_early_returns_and_preserves_order() {
        use wacore::types::message::{MessageInfo, MessageSource};
        use whatsapp_rust::TokioRuntime;
        use whatsapp_rust::bot::Bot;
        use whatsapp_rust_tokio_transport::TokioWebSocketTransportFactory;
        use whatsapp_rust_ureq_http_client::UreqHttpClient;
        use zeroclaw_config::schema::{WhatsAppChatPolicy as Policy, WhatsAppWebMode as Mode};

        const ALLOWED: &str = "15551234567";
        const DENIED: &str = "15559999999";

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::whatsapp_storage::RusqliteStore::new(tmp.path()).unwrap());
        let bot = Bot::builder()
            .with_backend_arc(store)
            .with_transport_factory(TokioWebSocketTransportFactory::new())
            .with_http_client(UreqHttpClient::new())
            .with_runtime(TokioRuntime)
            .build()
            .await
            .unwrap();
        let client = bot.client();

        let inbound = |sender: &str, id: &str, content: &str| {
            let jid = Jid::pn(sender);
            (
                Arc::new(waproto::whatsapp::Message {
                    conversation: Some(content.to_string()),
                    ..Default::default()
                }),
                Arc::new(MessageInfo {
                    source: MessageSource {
                        chat: jid.clone(),
                        sender: jid,
                        is_from_me: false,
                        is_group: false,
                        ..Default::default()
                    },
                    id: id.to_string(),
                    r#type: "text".to_string(),
                    push_name: "Batch Probe".to_string(),
                    timestamp: chrono::Utc::now(),
                    ..Default::default()
                }),
            )
        };
        let batch = message_batch_event(vec![
            inbound(DENIED, "batch-denied", "must not dispatch"),
            inbound(ALLOWED, "batch-second", "second"),
            inbound(ALLOWED, "batch-third", "third"),
        ]);

        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let context = WhatsAppInboundContext {
            tx,
            alias: Arc::new("batch-order".to_string()),
            peer_resolver: Arc::new(|| vec![format!("+{ALLOWED}")]),
            allowed_groups_resolver: Arc::new(Vec::new),
            mode: Mode::Business,
            dm_policy: Policy::Allowlist,
            group_policy: Policy::Allowlist,
            self_chat_mode: false,
            mention_only: false,
            passive_group_context: false,
            bot_phone: Arc::new(Mutex::new(None)),
            bot_lid: Arc::new(Mutex::new(None)),
            dm_mention_patterns: Arc::new(Vec::new()),
            group_mention_patterns: Arc::new(Vec::new()),
            transcription_config: None,
            transcription_manager: None,
            voice_chats: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        };

        WhatsAppWebChannel::handle_inbound_message_event(&batch, &client, &context).await;

        let second = rx
            .recv()
            .await
            .expect("the second batch entry must dispatch");
        let third = rx
            .recv()
            .await
            .expect("the third batch entry must dispatch");
        assert_eq!(
            (second.content.as_str(), third.content.as_str()),
            ("second", "third")
        );
        assert!(
            rx.try_recv().is_err(),
            "the refused first entry must not dispatch or duplicate later entries"
        );
    }

    /// The self-chat exception composes with the policy, and stays personal-only.
    ///
    /// Driven through `handle_inbound_message_event` so the `operator_self_chat`
    /// flag is produced by the personal-mode branch rather than passed in.
    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn inbound_path_keeps_self_chat_bypass_personal_only() {
        use wacore::types::message::{MessageInfo, MessageSource};
        use whatsapp_rust::TokioRuntime;
        use whatsapp_rust::bot::Bot;
        use whatsapp_rust_tokio_transport::TokioWebSocketTransportFactory;
        use whatsapp_rust_ureq_http_client::UreqHttpClient;
        use zeroclaw_config::schema::{WhatsAppChatPolicy as Policy, WhatsAppWebMode as Mode};

        // Deliberately absent from `peer_resolver` below: the personal-mode
        // bypass has to be what admits this message, not the allowlist.
        const OPERATOR: &str = "15557654321";

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::whatsapp_storage::RusqliteStore::new(tmp.path()).unwrap());
        let bot = Bot::builder()
            .with_backend_arc(store)
            .with_transport_factory(TokioWebSocketTransportFactory::new())
            .with_http_client(UreqHttpClient::new())
            .with_runtime(TokioRuntime)
            .build()
            .await
            .unwrap();
        let client = bot.client();

        // A self-chat: chat and sender are the same JID and the message is
        // fromMe, which is the shape the personal branch recognizes.
        let self_chat_event = || {
            let jid: Jid = format!("{OPERATOR}@s.whatsapp.net")
                .parse()
                .expect("operator jid parses");
            single_message_event(
                Arc::new(waproto::whatsapp::Message {
                    conversation: Some("note to self".to_string()),
                    ..Default::default()
                }),
                Arc::new(MessageInfo {
                    source: MessageSource {
                        chat: jid.clone(),
                        sender: jid,
                        is_from_me: true,
                        is_group: false,
                        ..Default::default()
                    },
                    id: "self-chat-probe".to_string(),
                    r#type: "text".to_string(),
                    push_name: "Operator".to_string(),
                    timestamp: chrono::Utc::now(),
                    ..Default::default()
                }),
            )
        };

        let context_for =
            |mode: Mode, tx: tokio::sync::mpsc::Sender<ChannelMessage>| WhatsAppInboundContext {
                tx,
                alias: Arc::new("self-chat-policy".to_string()),
                peer_resolver: Arc::new(Vec::new),
                allowed_groups_resolver: Arc::new(Vec::new),
                mode,
                dm_policy: Policy::Allowlist,
                group_policy: Policy::Allowlist,
                self_chat_mode: true,
                mention_only: false,
                passive_group_context: false,
                bot_phone: Arc::new(Mutex::new(None)),
                bot_lid: Arc::new(Mutex::new(None)),
                dm_mention_patterns: Arc::new(Vec::new()),
                group_mention_patterns: Arc::new(Vec::new()),
                transcription_config: None,
                transcription_manager: None,
                voice_chats: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            };

        // Personal mode: the operator's own self-chat is admitted even though
        // the allowlist is empty.
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let context = context_for(Mode::Personal, tx);
        let event = self_chat_event();
        WhatsAppWebChannel::handle_inbound_message_event(&event, &client, &context).await;
        let dispatched = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("personal self-chat must reach dispatch")
            .expect("dispatch sender must remain open");
        assert_eq!(dispatched.content, "note to self");

        // Business mode: the same message takes the policy path, and the
        // allowlist is empty, so it is refused.
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let context = context_for(Mode::Business, tx);
        let event = self_chat_event();
        WhatsAppWebChannel::handle_inbound_message_event(&event, &client, &context).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
                .await
                .is_err(),
            "business mode must not acquire the personal self-chat bypass"
        );
    }

    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn inbound_path_rejects_business_from_me_before_direct_message_bypass() {
        use wacore::types::message::{MessageInfo, MessageSource};
        use whatsapp_rust::TokioRuntime;
        use whatsapp_rust::bot::Bot;
        use whatsapp_rust_tokio_transport::TokioWebSocketTransportFactory;
        use whatsapp_rust_ureq_http_client::UreqHttpClient;
        use zeroclaw_config::schema::{WhatsAppChatPolicy as Policy, WhatsAppWebMode as Mode};

        const OPERATOR: &str = "15557654321";
        const CUSTOMER: &str = "15551234567";

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::whatsapp_storage::RusqliteStore::new(tmp.path()).unwrap());
        let bot = Bot::builder()
            .with_backend_arc(store)
            .with_transport_factory(TokioWebSocketTransportFactory::new())
            .with_http_client(UreqHttpClient::new())
            .with_runtime(TokioRuntime)
            .build()
            .await
            .unwrap();
        let client = bot.client();

        let event = |sender: &str, from_me: bool, content: &str| {
            single_message_event(
                Arc::new(waproto::whatsapp::Message {
                    conversation: Some(content.to_string()),
                    ..Default::default()
                }),
                Arc::new(MessageInfo {
                    source: MessageSource {
                        chat: Jid::pn(CUSTOMER),
                        sender: Jid::pn(sender),
                        is_from_me: from_me,
                        is_group: false,
                        ..Default::default()
                    },
                    id: format!("business-from-me-{from_me}"),
                    r#type: "text".to_string(),
                    push_name: "Business DM Probe".to_string(),
                    timestamp: chrono::Utc::now(),
                    ..Default::default()
                }),
            )
        };

        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let context = WhatsAppInboundContext {
            tx,
            alias: Arc::new("business-dm-provenance".to_string()),
            peer_resolver: Arc::new(Vec::new),
            allowed_groups_resolver: Arc::new(Vec::new),
            mode: Mode::Business,
            dm_policy: Policy::All,
            group_policy: Policy::All,
            self_chat_mode: false,
            mention_only: false,
            passive_group_context: false,
            bot_phone: Arc::new(Mutex::new(None)),
            bot_lid: Arc::new(Mutex::new(None)),
            dm_mention_patterns: Arc::new(Vec::new()),
            group_mention_patterns: Arc::new(Vec::new()),
            transcription_config: None,
            transcription_manager: None,
            voice_chats: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        };

        let outbound_echo = event(OPERATOR, true, "outbound delivery mirror");
        WhatsAppWebChannel::handle_inbound_message_event(&outbound_echo, &client, &context).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
                .await
                .is_err(),
            "a business-mode fromMe mirror must not reach channel dispatch"
        );

        let customer_message = event(CUSTOMER, false, "genuine customer message");
        WhatsAppWebChannel::handle_inbound_message_event(&customer_message, &client, &context)
            .await;
        let dispatched = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("a genuine business DM must reach channel dispatch")
            .expect("channel dispatch sender must remain open");
        assert_eq!(dispatched.content, "genuine customer message");

        let channel = WhatsAppWebChannel::new(
            &zeroclaw_config::schema::WhatsAppConfig::default(),
            "business-dm-provenance",
            Arc::new(Vec::new),
            Arc::new(Vec::new),
        );
        assert!(zeroclaw_api::channel::Channel::is_direct_message(
            &channel,
            &dispatched
        ));
    }

    // ── Reconnect retry state machine tests (exercise production helpers) ──

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn compute_retry_delay_doubles_with_cap() {
        // Uses the production helper that listen() calls for backoff.
        // attempt 1 → 3s, 2 → 6s, 3 → 12s, … 7 → 192s, 8 → 300s (capped)
        let expected = [3, 6, 12, 24, 48, 96, 192, 300, 300, 300];
        for (i, &want) in expected.iter().enumerate() {
            let attempt = (i + 1) as u32;
            assert_eq!(
                WhatsAppWebChannel::compute_retry_delay(attempt),
                want,
                "attempt {attempt}"
            );
        }
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn compute_retry_delay_zero_attempt() {
        // Edge case: attempt 0 should still produce BASE (saturating_sub clamps).
        assert_eq!(
            WhatsAppWebChannel::compute_retry_delay(0),
            WhatsAppWebChannel::BASE_DELAY_SECS
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn record_retry_increments_and_detects_exceeded() {
        use std::sync::atomic::AtomicU32;
        let counter = AtomicU32::new(0);

        // First MAX_RETRIES attempts should not exceed.
        for i in 1..=WhatsAppWebChannel::MAX_RETRIES {
            let (attempt, exceeded) = WhatsAppWebChannel::record_retry(&counter);
            assert_eq!(attempt, i);
            assert!(!exceeded, "attempt {i} should not exceed max");
        }

        // Next attempt exceeds the limit.
        let (attempt, exceeded) = WhatsAppWebChannel::record_retry(&counter);
        assert_eq!(attempt, WhatsAppWebChannel::MAX_RETRIES + 1);
        assert!(exceeded);
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn reset_retry_clears_counter() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let counter = AtomicU32::new(0);

        // Simulate several reconnect attempts via the production helper.
        for _ in 0..5 {
            WhatsAppWebChannel::record_retry(&counter);
        }
        assert_eq!(counter.load(Ordering::Relaxed), 5);

        // Event::Connected calls reset_retry — verify it zeroes the counter.
        WhatsAppWebChannel::reset_retry(&counter);
        assert_eq!(counter.load(Ordering::Relaxed), 0);

        // After reset, record_retry starts from 1 again.
        let (attempt, exceeded) = WhatsAppWebChannel::record_retry(&counter);
        assert_eq!(attempt, 1);
        assert!(!exceeded);
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn should_purge_session_only_when_revoked() {
        use std::sync::atomic::AtomicBool;
        let flag = AtomicBool::new(false);

        // Transient crash: flag is false → should NOT purge.
        assert!(!WhatsAppWebChannel::should_purge_session(&flag));

        // Explicit LoggedOut: flag set to true → should purge.
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(WhatsAppWebChannel::should_purge_session(&flag));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn with_transcription_sets_config_when_enabled() {
        let tc = zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            api_key: Some("test_key".to_string()),
            ..Default::default()
        };

        let mention_only = false;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test-whatsapp.db".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
            Arc::new(Vec::new),
        )
        .with_transcription(tc);
        assert!(ch.transcription.is_some());
        assert!(ch.transcription_manager.is_some());
    }

    /// Regression: channel wiring must be able to install a manager the caller
    /// already bound to the owning agent's provider, instead of the
    /// legacy-only manager `with_transcription` builds.
    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn with_transcription_manager_installs_caller_resolved_manager() {
        let mut config = zeroclaw_config::schema::Config::default();
        config.transcription.enabled = true;
        config.providers.transcription.groq.insert(
            "fast".to_string(),
            zeroclaw_config::schema::GroqTranscriptionProviderConfig {
                base: zeroclaw_config::schema::TranscriptionProviderConfig {
                    api_key: Some("test-key".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let manager =
            super::super::transcription::build_channel_transcription_manager(&config, "groq.fast")
                .expect("typed provider must build a manager");

        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test-whatsapp.db".into()),
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
            Arc::new(Vec::new),
        )
        .with_transcription_manager(config.transcription.clone(), Some(Arc::new(manager)));

        assert!(ch.transcription.is_some());
        let manager = ch
            .transcription_manager
            .as_ref()
            .expect("caller-resolved manager must be installed on the channel");
        assert_eq!(manager.bound_provider(), "groq.fast");
    }

    /// REGRESSION: WhatsApp Web's own `with_transcription` never bound a
    /// provider, so voice notes always failed with "no transcription_provider
    /// configured". The shared snapshot path binds the lone provider.
    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn with_transcription_binds_the_sole_provider() {
        let tc = zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            api_key: Some("test_key".to_string()),
            ..Default::default()
        };
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test-whatsapp.db".into()),
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
            Arc::new(Vec::new),
        )
        .with_transcription(tc);
        let manager = ch.transcription_manager.as_ref().expect("manager is built");
        assert_eq!(manager.bound_provider(), "groq");
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn with_transcription_ignores_when_disabled() {
        let tc = zeroclaw_config::schema::TranscriptionConfig::default(); // enabled = false
        let mention_only = false;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test-whatsapp.db".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
            Arc::new(Vec::new),
        )
        .with_transcription(tc);
        assert!(ch.transcription.is_none());
        assert!(ch.transcription_manager.is_none());
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn session_file_paths_includes_wal_and_shm() {
        let paths = WhatsAppWebChannel::session_file_paths("/tmp/test.db");
        assert_eq!(
            paths,
            [
                "/tmp/test.db".to_string(),
                "/tmp/test.db-wal".to_string(),
                "/tmp/test.db-shm".to_string(),
            ]
        );
    }

    // ── Mention detection tests ──

    #[cfg(feature = "whatsapp-web")]
    fn extended_text_reply(
        participant: &str,
        mentioned_jids: &[&str],
    ) -> waproto::whatsapp::Message {
        waproto::whatsapp::Message {
            extended_text_message: waproto::whatsapp::message::ExtendedTextMessage {
                text: Some("expand the previous response".to_string()),
                context_info: waproto::whatsapp::ContextInfo {
                    participant: Some(participant.to_string()),
                    mentioned_jid: mentioned_jids
                        .iter()
                        .map(|jid| (*jid).to_string())
                        .collect(),
                    ..Default::default()
                }
                .into(),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        }
    }

    #[cfg(feature = "whatsapp-web")]
    fn sticker_reply(
        participant: &str,
        quoted_message: Option<waproto::whatsapp::Message>,
    ) -> waproto::whatsapp::Message {
        waproto::whatsapp::Message {
            sticker_message: waproto::whatsapp::message::StickerMessage {
                mimetype: Some("image/webp".to_string()),
                context_info: waproto::whatsapp::ContextInfo {
                    participant: Some(participant.to_string()),
                    quoted_message: quoted_message.into(),
                    ..Default::default()
                }
                .into(),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        }
    }

    #[cfg(feature = "whatsapp-web")]
    fn image_mention(mentioned_jids: &[&str]) -> waproto::whatsapp::Message {
        waproto::whatsapp::Message {
            image_message: waproto::whatsapp::message::ImageMessage {
                mimetype: Some("image/jpeg".to_string()),
                context_info: waproto::whatsapp::ContextInfo {
                    mentioned_jid: mentioned_jids
                        .iter()
                        .map(|jid| (*jid).to_string())
                        .collect(),
                    ..Default::default()
                }
                .into(),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        }
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn jid_digits_extracts_phone_from_jid() {
        assert_eq!(
            WhatsAppWebChannel::jid_digits("919211916069@s.whatsapp.net"),
            "919211916069"
        );
        assert_eq!(
            WhatsAppWebChannel::jid_digits("76188559093817@lid"),
            "76188559093817"
        );
        assert_eq!(WhatsAppWebChannel::jid_digits("15551234567"), "15551234567");
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn contains_bot_mention_structured() {
        let jids = vec!["919211916069@s.whatsapp.net".to_string()];
        assert!(WhatsAppWebChannel::contains_bot_mention(
            "hey @919211916069 check this",
            &jids,
            "919211916069",
            None
        ));
        assert!(WhatsAppWebChannel::contains_bot_mention(
            "hey check this",
            &jids,
            "919211916069",
            None
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn contains_bot_mention_text_fallback() {
        let no_jids: Vec<String> = vec![];
        assert!(WhatsAppWebChannel::contains_bot_mention(
            "hey @919211916069 check this",
            &no_jids,
            "919211916069",
            None
        ));
        assert!(WhatsAppWebChannel::contains_bot_mention(
            "hey @919211916069",
            &no_jids,
            "919211916069",
            None
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn contains_bot_mention_prefix_false_positive() {
        let no_jids: Vec<String> = vec![];
        assert!(!WhatsAppWebChannel::contains_bot_mention(
            "hey @919211916069 check this",
            &no_jids,
            "91921191606",
            None
        ));
        assert!(!WhatsAppWebChannel::contains_bot_mention(
            "hey @155512345678",
            &no_jids,
            "15551234567",
            None
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn contains_bot_mention_no_match() {
        let no_jids: Vec<String> = vec![];
        assert!(!WhatsAppWebChannel::contains_bot_mention(
            "just a regular message",
            &no_jids,
            "919211916069",
            None
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn contains_bot_mention_scans_past_prefix_false_match() {
        let no_jids: Vec<String> = vec![];
        assert!(WhatsAppWebChannel::contains_bot_mention(
            "@9192119160691 real @919211916069",
            &no_jids,
            "919211916069",
            None
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn contains_bot_mention_rejects_embedded_at() {
        let no_jids: Vec<String> = vec![];
        assert!(!WhatsAppWebChannel::contains_bot_mention(
            "foo@919211916069 bar",
            &no_jids,
            "919211916069",
            None
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn jid_digits_strips_device_suffix() {
        assert_eq!(
            WhatsAppWebChannel::jid_digits("919211916069:16@s.whatsapp.net"),
            "919211916069"
        );
        assert_eq!(
            WhatsAppWebChannel::jid_digits("227728477442093:3@lid"),
            "227728477442093"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn contains_bot_mention_matches_lid() {
        let jids = vec!["227728477442093@lid".to_string()];
        assert!(WhatsAppWebChannel::contains_bot_mention(
            "hey @DisplayName check this",
            &jids,
            "6287778315246",
            Some("227728477442093")
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn contains_bot_mention_matches_lid_when_phone_unknown() {
        let jids = vec!["227728477442093@lid".to_string()];
        assert!(WhatsAppWebChannel::contains_bot_mention(
            "hey @DisplayName check this",
            &jids,
            "",
            Some("227728477442093")
        ));
        assert!(!WhatsAppWebChannel::contains_bot_mention(
            "plain @ mention",
            &[],
            "",
            None
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn extract_mentioned_jids_reads_media_context_info() {
        let msg = image_mention(&["100@s.whatsapp.net"]);
        assert_eq!(
            WhatsAppWebChannel::extract_mentioned_jids(&msg),
            vec!["100@s.whatsapp.net".to_string()]
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn message_addressed_to_bot_accepts_reply_to_phone_jid() {
        let msg = extended_text_reply("100@s.whatsapp.net", &[]);
        assert!(WhatsAppWebChannel::is_message_addressed_to_bot(
            &msg,
            "expand the previous response",
            "100",
            None,
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn message_addressed_to_bot_accepts_reply_to_lid_jid() {
        let msg = extended_text_reply("200@lid", &[]);
        assert!(WhatsAppWebChannel::is_message_addressed_to_bot(
            &msg,
            "expand the previous response",
            "100",
            Some("200"),
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn message_addressed_to_bot_accepts_media_reply_to_lid_jid() {
        let msg = sticker_reply("200@lid", None);
        assert!(WhatsAppWebChannel::is_message_addressed_to_bot(
            &msg,
            "[Sticker]",
            "100",
            Some("200"),
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn extract_quoted_message_reads_media_context_info() {
        let quoted = waproto::whatsapp::Message {
            image_message: waproto::whatsapp::message::ImageMessage {
                mimetype: Some("image/png".to_string()),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        };
        let msg = sticker_reply("200@lid", Some(quoted));
        let quoted = WhatsAppWebChannel::extract_quoted_message(&msg)
            .expect("sticker reply should expose the quoted message");
        assert!(quoted.image_message.is_set());
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn media_fallback_content_keeps_sticker_messages_addressable() {
        let msg = sticker_reply("200@lid", None);
        assert_eq!(
            WhatsAppWebChannel::media_fallback_content(String::new(), &msg),
            "[Sticker]"
        );
        assert_eq!(
            WhatsAppWebChannel::media_fallback_content("hello".to_string(), &msg),
            "hello"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn media_fallback_content_leaves_non_media_messages_empty() {
        let msg = waproto::whatsapp::Message::default();
        assert_eq!(
            WhatsAppWebChannel::media_fallback_content(String::new(), &msg),
            ""
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn media_fallback_content_parses_static_location() {
        let msg = waproto::whatsapp::Message {
            location_message: waproto::whatsapp::message::LocationMessage {
                degrees_latitude: Some(40.7128),
                degrees_longitude: Some(-74.0060),
                name: Some("NYC".into()),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        };
        assert_eq!(
            WhatsAppWebChannel::media_fallback_content(String::new(), &msg),
            "[Location: 40.712800, -74.006000 — NYC]"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn media_fallback_content_skips_live_location() {
        let msg = waproto::whatsapp::Message {
            location_message: waproto::whatsapp::message::LocationMessage {
                degrees_latitude: Some(40.7128),
                degrees_longitude: Some(-74.0060),
                is_live: Some(true),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        };
        assert_eq!(
            WhatsAppWebChannel::media_fallback_content(String::new(), &msg),
            ""
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn media_fallback_content_skips_missing_coordinates() {
        // Missing longitude — should silently drop, not fabricate 0,0
        let msg = waproto::whatsapp::Message {
            location_message: waproto::whatsapp::message::LocationMessage {
                degrees_latitude: Some(40.7128),
                degrees_longitude: None,
                ..Default::default()
            }
            .into(),
            ..Default::default()
        };
        assert_eq!(
            WhatsAppWebChannel::media_fallback_content(String::new(), &msg),
            ""
        );
        // Missing latitude
        let msg = waproto::whatsapp::Message {
            location_message: waproto::whatsapp::message::LocationMessage {
                degrees_latitude: None,
                degrees_longitude: Some(-74.0060),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        };
        assert_eq!(
            WhatsAppWebChannel::media_fallback_content(String::new(), &msg),
            ""
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn mime_extension_uses_safe_subtype() {
        assert_eq!(
            WhatsAppWebChannel::mime_extension("image/jpeg; name=photo", "jpg"),
            "jpg"
        );
        assert_eq!(
            WhatsAppWebChannel::mime_extension("application/vnd.ms-excel", "bin"),
            "vnd.ms-excel"
        );
        assert_eq!(
            WhatsAppWebChannel::mime_extension("image/../../png", "bin"),
            "bin"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn message_addressed_to_bot_rejects_reply_to_other_participant() {
        let msg = extended_text_reply("300@s.whatsapp.net", &[]);
        assert!(!WhatsAppWebChannel::is_message_addressed_to_bot(
            &msg,
            "expand the previous response",
            "100",
            Some("200"),
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn message_addressed_to_bot_accepts_explicit_mention_in_other_reply() {
        let msg = extended_text_reply("300@s.whatsapp.net", &["100@s.whatsapp.net"]);
        assert!(WhatsAppWebChannel::is_message_addressed_to_bot(
            &msg,
            "expand the previous response",
            "100",
            Some("200"),
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn constructor_seeds_bot_phone_from_pair_phone() {
        let mention_only = true;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test.db".into()),
            pair_phone: Some("919211916069".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["*".into()]),
            Arc::new(Vec::new),
        );
        assert_eq!(*ch.bot_phone.lock(), Some("919211916069".to_string()));
        assert_eq!(*ch.bot_lid.lock(), None);
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn constructor_carries_push_name_from_config() {
        let mk = |push_name: Option<&str>| {
            let cfg = zeroclaw_config::schema::WhatsAppConfig {
                enabled: true,
                session_path: Some("/tmp/test.db".into()),
                push_name: push_name.map(Into::into),
                ..Default::default()
            };
            WhatsAppWebChannel::new(
                &cfg,
                "whatsapp_web_test_alias",
                Arc::new(|| vec!["*".into()]),
                Arc::new(Vec::new),
            )
        };
        assert_eq!(mk(Some("סוכן")).push_name.as_deref(), Some("סוכן"));
        assert_eq!(mk(None).push_name, None);
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn push_name_to_apply_sends_only_on_a_real_difference() {
        // Multi-byte throughout, so any byte-slicing of the value shows up here.
        let non_ascii = "סוכן – שירות";
        assert!(non_ascii.len() > non_ascii.chars().count());

        // (configured, name the device announces, name to send)
        let cases: [(Option<&str>, &str, Option<&str>); 11] = [
            (None, "Galaxy S21", None),
            (None, "", None),
            (Some(""), "Old", None),
            (Some("   \t "), "Old", None),
            (Some("ZeroClawAgent"), "Galaxy S21", Some("ZeroClawAgent")),
            (Some("ZeroClawAgent"), "", Some("ZeroClawAgent")),
            (Some("ZeroClawAgent"), "ZeroClawAgent", None),
            (Some("  ZeroClawAgent  "), "ZeroClawAgent", None),
            (Some(non_ascii), "Galaxy S21", Some(non_ascii)),
            (Some(non_ascii), non_ascii, None),
            // A value that is a strict byte-prefix of the device's name is
            // still a difference.
            (Some("סוכן"), "סוכנת", Some("סוכן")),
        ];
        for (configured, current, want) in cases {
            assert_eq!(
                WhatsAppWebChannel::push_name_to_apply(configured, current),
                want,
                "configured {configured:?} against device {current:?}"
            );
        }
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn constructor_no_pair_phone_leaves_bot_phone_none() {
        let mention_only = true;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test.db".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["*".into()]),
            Arc::new(Vec::new),
        );
        assert_eq!(*ch.bot_phone.lock(), None);
    }

    // ── fromme_outside_self_chat_is_operator_trigger ───────────

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn fromme_trigger_drops_when_no_mention_patterns_configured() {
        let dm: Vec<regex::Regex> = vec![];
        let group: Vec<regex::Regex> = vec![];
        // Without configured patterns, a fromMe message in a third-party
        // DM or group must drop — there is no opt-in signal that says the
        // operator wants outbound mirrors to be treated as triggers.
        assert!(!fromme_outside_self_chat_is_operator_trigger(
            false,
            &dm,
            &group,
            "TinyBot foo"
        ));
        assert!(!fromme_outside_self_chat_is_operator_trigger(
            true,
            &dm,
            &group,
            "TinyBot foo"
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn fromme_trigger_falls_through_when_dm_pattern_matches() {
        // @ilteoood's configured workflow: dm_mention_patterns = ["TinyBot"].
        // Operator types "TinyBot translate this" in a friend's DM →
        // intentional invocation, must fall through.
        let dm = vec![
            regex::RegexBuilder::new("TinyBot")
                .case_insensitive(true)
                .build()
                .unwrap(),
        ];
        let group: Vec<regex::Regex> = vec![];
        assert!(fromme_outside_self_chat_is_operator_trigger(
            false,
            &dm,
            &group,
            "TinyBot translate this"
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn fromme_trigger_drops_when_dm_pattern_does_not_match() {
        // Operator types a normal message in a friend's DM — even with
        // patterns configured, no match means it stays an outbound mirror
        // and must be dropped to prevent impersonation.
        let dm = vec![
            regex::RegexBuilder::new("TinyBot")
                .case_insensitive(true)
                .build()
                .unwrap(),
        ];
        let group: Vec<regex::Regex> = vec![];
        assert!(!fromme_outside_self_chat_is_operator_trigger(
            false,
            &dm,
            &group,
            "see you at 7"
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn fromme_trigger_uses_group_patterns_for_group_threads() {
        // group_mention_patterns gates the group case; dm patterns must
        // not be consulted for group messages and vice versa. This pins
        // the predicate's branch selection.
        let dm: Vec<regex::Regex> = vec![
            regex::RegexBuilder::new("DmTrigger")
                .case_insensitive(true)
                .build()
                .unwrap(),
        ];
        let group = vec![
            regex::RegexBuilder::new("GroupTrigger")
                .case_insensitive(true)
                .build()
                .unwrap(),
        ];
        // In a group, only group_patterns matter.
        assert!(fromme_outside_self_chat_is_operator_trigger(
            true,
            &dm,
            &group,
            "GroupTrigger hi"
        ));
        assert!(!fromme_outside_self_chat_is_operator_trigger(
            true,
            &dm,
            &group,
            "DmTrigger hi"
        ));
        // In a DM, only dm_patterns matter.
        assert!(fromme_outside_self_chat_is_operator_trigger(
            false,
            &dm,
            &group,
            "DmTrigger hi"
        ));
        assert!(!fromme_outside_self_chat_is_operator_trigger(
            false,
            &dm,
            &group,
            "GroupTrigger hi"
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn fromme_trigger_drops_when_text_is_empty() {
        // Voice notes and media-only messages return empty text. With no
        // text to match against, the operator-trigger path must drop —
        // never transcribe a fromMe voice note just to check whether it
        // is a bot trigger (cost + impersonation risk).
        let dm = vec![
            regex::RegexBuilder::new("TinyBot")
                .case_insensitive(true)
                .build()
                .unwrap(),
        ];
        let group: Vec<regex::Regex> = vec![];
        assert!(!fromme_outside_self_chat_is_operator_trigger(
            false, &dm, &group, ""
        ));
    }

    // -- request_approval on WhatsApp Web --
    //
    // These drive the decision helpers directly rather than a live socket,
    // because the authorization and lifecycle gates are the behavior under
    // test.

    #[cfg(feature = "whatsapp-web")]
    fn approval_cfg(approval_timeout_secs: u64) -> zeroclaw_config::schema::WhatsAppConfig {
        zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test-whatsapp-approval.db".into()),
            approval_timeout_secs,
            ..Default::default()
        }
    }

    /// Park a token bound to `chat` under the default test alias.
    #[cfg(feature = "whatsapp-web")]
    async fn park_token(
        token: &str,
        chat: &str,
        is_group: bool,
    ) -> tokio::sync::oneshot::Receiver<ChannelApprovalResponse> {
        park_token_as("default", token, chat, is_group).await
    }

    /// Park a token bound to a NAMED alias, for the cross-alias cases.
    #[cfg(feature = "whatsapp-web")]
    async fn park_token_as(
        alias: &str,
        token: &str,
        chat: &str,
        is_group: bool,
    ) -> tokio::sync::oneshot::Receiver<ChannelApprovalResponse> {
        let (responder, rx) = tokio::sync::oneshot::channel();
        PENDING_APPROVALS.lock().await.insert(
            token.to_string(),
            PendingApproval {
                registration_id: uuid::Uuid::new_v4(),
                responder,
                binding: ApprovalBinding {
                    alias: alias.to_string(),
                    chat: chat.to_string(),
                    is_group,
                },
            },
        );
        rx
    }

    /// Drives the send-error branch of `request_approval` for real, rather than
    /// calling the cleanup helper directly. A channel with no vendor client
    /// fails `send` deterministically, which is the one production exit that
    /// needs no cancellation and no reply to reach.
    ///
    /// WHAT IT DOES NOT COVER, stated first because the obvious reading is
    /// wrong. This does NOT demonstrate the generation-scoped cleanup that the
    /// same branch uses. It cannot: `request_approval` mints its own random
    /// token, which never collides with the sentinel parked below, so an
    /// unconditional `remove` of the generated token leaves the sentinel alone
    /// too. Checked rather than assumed, by reverting the branch to the
    /// unconditional removal and re-running this test, which still passed.
    /// Proving that scoping needs the entry replaced while the branch is in
    /// flight, which is what
    /// [`send_error_cleanup_leaves_a_reused_token_alone`] does through the
    /// send hook. The two are kept separate because they pin different things
    /// and fail for different reasons.
    ///
    /// What it DOES pin is narrower and real: the send-error path is reachable
    /// and terminates. A channel with no vendor client fails `send`
    /// deterministically, so this drives the one production exit that needs
    /// neither a reply nor a cancellation, and asserts the caller gets that
    /// error back rather than a hang or a silent approval.
    ///
    /// It deliberately makes no assertion about the map's size or key set.
    /// These tests share one process-global map and run in parallel, so a
    /// whole-map assertion would be racing every other approval test.
    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn request_approval_surfaces_a_send_failure_to_the_caller() {
        let sentinel = "snt001";
        let sentinel_id = uuid::Uuid::new_v4();
        let (responder, _keep_alive) = tokio::sync::oneshot::channel();
        PENDING_APPROVALS.lock().await.insert(
            sentinel.to_string(),
            PendingApproval {
                registration_id: sentinel_id,
                responder,
                binding: ApprovalBinding {
                    alias: "alias-a".to_string(),
                    chat: "1@s.whatsapp.net".to_string(),
                    is_group: false,
                },
            },
        );

        let cfg = zeroclaw_config::schema::WhatsAppConfig::default();
        let channel =
            WhatsAppWebChannel::new(&cfg, "alias-a", Arc::new(Vec::new), Arc::new(Vec::new));
        let request = ChannelApprovalRequest {
            tool_name: "shell".to_string(),
            arguments_summary: "ls".to_string(),
            raw_arguments: None,
            position: None,
        };

        let err = channel
            .request_approval("1@s.whatsapp.net", &request)
            .await
            .expect_err("a channel with no client must fail at send");
        assert!(
            err.to_string().contains("not connected"),
            "fixture must fail at the send, not somewhere earlier: {err}"
        );

        let pending = PENDING_APPROVALS.lock().await;
        let entry = pending
            .get(sentinel)
            .expect("the send-error branch removed an unrelated registration");
        assert_eq!(
            entry.registration_id, sentinel_id,
            "the unrelated registration must be the original one, not a replacement"
        );
        drop(pending);
        PENDING_APPROVALS.lock().await.remove(sentinel);
    }

    /// The approval prompt's prose comes from the Fluent catalogue, in BOTH
    /// the direct-chat and the group shape.
    ///
    /// Driven through the real `request_approval` -> `send_approval_prompt`
    /// seam rather than by rebuilding the string here, so it pins the text an
    /// operator actually receives. The send hook returns `Err`, which lands
    /// `request_approval` on its send-error branch: that exit needs neither a
    /// reply nor the timeout clock, and it removes its own token on the way
    /// out.
    ///
    /// Every catalogue lookup is also asserted NOT to be the missing-string
    /// sentinel. Without that the test would pass vacuously on a typo'd or
    /// undefined key, because `get_required_cli_string` falls back to
    /// `{key}` and the production text and the expected text would then agree
    /// on that same sentinel while the operator saw a raw key.
    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn approval_prompt_prose_comes_from_the_catalogue_in_both_chat_shapes() {
        async fn capture_prompt(recipient: &str) -> String {
            let seen = Arc::new(std::sync::Mutex::new(None::<String>));
            let recorder = Arc::clone(&seen);
            let channel = WhatsAppWebChannel::new(
                &zeroclaw_config::schema::WhatsAppConfig::default(),
                "alias-prompt",
                Arc::new(Vec::new),
                Arc::new(Vec::new),
            )
            .with_approval_send_hook(Arc::new(move |message: SendMessage| {
                let recorder = Arc::clone(&recorder);
                Box::pin(async move {
                    *recorder.lock().unwrap() = Some(message.content.clone());
                    Err(anyhow::Error::msg("send refused by the test hook"))
                })
            }));

            let request = ChannelApprovalRequest {
                tool_name: "shell".to_string(),
                arguments_summary: "ls -la".to_string(),
                raw_arguments: None,
                position: None,
            };
            channel
                .request_approval(recipient, &request)
                .await
                .expect_err("the hook refuses the send, so the call must surface that error");

            let captured = seen.lock().unwrap().clone();
            captured.expect("request_approval must send a prompt before it fails")
        }

        fn token_of(prompt: &str) -> String {
            prompt
                .split_once('[')
                .and_then(|(_, rest)| rest.split_once(']'))
                .map(|(token, _)| token.to_string())
                .expect("the prompt must carry its token in brackets")
        }

        let catalogue = |key: &str| {
            let value = i18n::get_required_cli_string(key);
            assert_ne!(
                value,
                format!("{{{key}}}"),
                "{key} resolves to the missing-string sentinel, so this test would pass vacuously"
            );
            value
        };

        let heading = catalogue("channel-approval-heading-shout");
        let tool_label = catalogue("channel-approval-tool-label");
        let args_label = catalogue("channel-approval-args-label");
        let group_warning = catalogue("channel-approval-group-visibility-warning");

        // Direct chat: catalogue prose, verbatim tool and args, and no group
        // warning, because nobody else can read this token.
        let dm = capture_prompt("1@s.whatsapp.net").await;
        for part in [&heading, &tool_label, &args_label] {
            assert!(
                dm.contains(part.as_str()),
                "the direct-chat prompt lost {part:?}: {dm:?}"
            );
        }
        assert!(
            dm.contains("shell") && dm.contains("ls -la"),
            "the prompt must echo the tool name and args verbatim: {dm:?}"
        );
        assert!(
            !dm.contains(group_warning.as_str()),
            "a direct chat must not carry the group visibility warning: {dm:?}"
        );

        // Exact, not `contains`: this transport must emit the SHARED builder's
        // output rather than a look-alike of its own. A hand-rolled prompt that
        // happens to match today's English would drift the moment the
        // catalogue text changes, and `contains` would not notice.
        let dm_token = token_of(&dm);
        assert_eq!(
            dm,
            crate::util::build_yesno_approval_prompt(&dm_token, "shell", "ls -la", None),
            "the direct-chat prompt must be exactly what the shared builder produces"
        );

        // Group chat: the same prose plus the warning, because the token is
        // now readable by every member of the group.
        let group = capture_prompt("120363000000000000@g.us").await;
        for part in [&heading, &tool_label, &args_label, &group_warning] {
            assert!(
                group.contains(part.as_str()),
                "the group prompt lost {part:?}: {group:?}"
            );
        }
        let group_token = token_of(&group);
        assert_eq!(
            group,
            format!(
                "{}\n\n{group_warning}",
                crate::util::build_yesno_approval_prompt(&group_token, "shell", "ls -la", None)
            ),
            "the group prompt must be the shared builder's output plus exactly one warning"
        );

        // The protocol half. Whatever the catalogue says, the reply keywords
        // embedded in the prompt must remain the literal ASCII words that
        // `parse_approval_reply` reads, or a localized prompt would instruct
        // the operator to send something the parser cannot decode.
        for prompt in [&dm, &group] {
            let token = token_of(prompt);
            for (word, expected) in [
                ("yes", ChannelApprovalResponse::Approve),
                ("no", ChannelApprovalResponse::Deny),
                ("always", ChannelApprovalResponse::AlwaysApprove),
            ] {
                let reply = format!("{token} {word}");
                assert!(
                    prompt.contains(&reply),
                    "the prompt must show the exact reply {reply:?}: {prompt:?}"
                );
                let (parsed, response) = crate::util::parse_approval_reply(&reply)
                    .unwrap_or_else(|| panic!("{reply:?} must parse"));
                assert_eq!(parsed, token);
                assert_eq!(response, expected);
            }
        }

        // The group warning has to name BOTH things a group member can read.
        // The token is the obvious one, and the one the warning used to stop
        // at. The arguments are the other: `arguments_summary` is rendered
        // verbatim into the prompt, so posting an approval into a group
        // publishes the command line to every member. An operator deciding
        // whether to approve in a group needs that stated rather than
        // inferred from the fact that the args happen to be printed above.
        //
        // Read in English deliberately. This binary never initialises the
        // i18n locale, so the in-crate English catalogue is what resolves,
        // and the neighbouring-key assert names a locale change as the cause
        // rather than letting it read as a deleted disclosure.
        assert_eq!(
            i18n::get_required_cli_string("channel-approval-args-label"),
            "Args",
            "this assertion reads the English catalogue, and a different one resolved"
        );
        assert!(
            group_warning.to_lowercase().contains("arguments"),
            "the group warning must disclose that the tool arguments are visible to every \
             member, not only the reply code: {group_warning:?}"
        );
        assert!(
            group.contains("ls -la"),
            "the group prompt must carry the arguments the warning discloses: {group:?}"
        );
    }

    /// No runtime assertion can catch a prompt that reverts to a hardcoded
    /// English string, so this one reads the source instead.
    ///
    /// Under the `en` locale a correct literal renders byte-identically to what
    /// the catalogue produces, so every assertion above passes against it. The
    /// key-scanning guard in `util.rs` does not close that gap either: it finds
    /// a key that is still referenced but misspelled, while a reverted prompt
    /// stops referencing the key at all.
    ///
    /// Splitting on the test module is what makes this checkable, since the
    /// needles below have to appear here in order to be searched for.
    #[test]
    fn production_carries_no_approval_prose_literal() {
        const SRC: &str = include_str!("whatsapp_web.rs");
        const TEST_MODULE: &str = "\n#[cfg(test)]\nmod tests {";

        let (production, tests) = SRC
            .split_once(TEST_MODULE)
            .expect("the test module marker moved, so this guard searched the wrong text");
        assert!(
            !tests.is_empty(),
            "the split produced an empty test half, so the marker matched something unintended"
        );

        for prose in ["APPROVAL REQUIRED", "Tool: {}", "so everyone here can see"] {
            assert!(
                !production.contains(prose),
                "approval prose {prose:?} is hardcoded in production code; it belongs in the \
                 Fluent catalogue so that every locale moves with it"
            );
        }
    }

    /// The approval-reply interception, driven through the real inbound
    /// handler rather than by calling the resolver directly.
    ///
    /// This is the one piece of the approval path that lives in the shared
    /// message handler instead of in a helper, which makes it the piece a
    /// refactor of that handler can silently drop: every resolver-level test
    /// in this file would still pass with the interception deleted, because
    /// they call the resolver themselves. This calls
    /// `handle_inbound_message_event`, which is what the live listener calls.
    ///
    /// It pins BOTH halves of the contract, because either alone is passable
    /// with the interception broken: the decision must reach the waiting
    /// request, AND the reply must not also travel on as conversation, since
    /// an approval reply is a control message.
    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn approval_reply_is_intercepted_by_the_inbound_handler() {
        use wacore::types::message::{MessageInfo, MessageSource};
        use whatsapp_rust::TokioRuntime;
        use whatsapp_rust::bot::Bot;
        use whatsapp_rust_tokio_transport::TokioWebSocketTransportFactory;
        use whatsapp_rust_ureq_http_client::UreqHttpClient;

        const SENDER_PHONE: &str = "15551234567";
        let chat = Jid::pn(SENDER_PHONE).to_string();
        let alias = "approval-interception-test";

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::whatsapp_storage::RusqliteStore::new(tmp.path()).unwrap());
        let bot = Bot::builder()
            .with_backend_arc(store.clone())
            .with_transport_factory(TokioWebSocketTransportFactory::new())
            .with_http_client(UreqHttpClient::new())
            .with_runtime(TokioRuntime)
            .build()
            .await
            .unwrap();
        let client = bot.client();

        // A real pending request, registered the way `request_approval` does.
        let PendingApprovalRegistration {
            token,
            receiver,
            mut guard,
            ..
        } = register_pending_approval(ApprovalBinding {
            alias: alias.to_string(),
            chat: chat.clone(),
            is_group: false,
        })
        .await;

        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let context = WhatsAppInboundContext {
            tx,
            alias: Arc::new(alias.to_string()),
            peer_resolver: Arc::new(|| vec![format!("+{SENDER_PHONE}")]),
            allowed_groups_resolver: Arc::new(Vec::new),
            mode: zeroclaw_config::schema::WhatsAppWebMode::Personal,
            dm_policy: zeroclaw_config::schema::WhatsAppChatPolicy::Allowlist,
            group_policy: zeroclaw_config::schema::WhatsAppChatPolicy::Allowlist,
            self_chat_mode: false,
            mention_only: false,
            passive_group_context: false,
            bot_phone: Arc::new(Mutex::new(None)),
            bot_lid: Arc::new(Mutex::new(None)),
            dm_mention_patterns: Arc::new(Vec::new()),
            group_mention_patterns: Arc::new(Vec::new()),
            transcription_config: None,
            transcription_manager: None,
            voice_chats: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        };

        let reply_event = single_message_event(
            Arc::new(waproto::whatsapp::Message {
                conversation: Some(format!("{token} yes")),
                ..Default::default()
            }),
            Arc::new(MessageInfo {
                source: MessageSource {
                    chat: Jid::pn(SENDER_PHONE),
                    sender: Jid::pn(SENDER_PHONE),
                    is_from_me: false,
                    is_group: false,
                    ..Default::default()
                },
                id: "approval-interception".to_string(),
                r#type: "text".to_string(),
                push_name: "Approval Replier".to_string(),
                timestamp: chrono::Utc::now(),
                ..Default::default()
            }),
        );

        WhatsAppWebChannel::handle_inbound_message_event(&reply_event, &client, &context).await;

        let decision = tokio::time::timeout(std::time::Duration::from_secs(1), receiver)
            .await
            .expect("the interception must resolve the pending approval")
            .expect("the approval sender must still be live");
        assert_eq!(
            decision,
            ChannelApprovalResponse::Approve,
            "a `<token> yes` reply must approve the pending request"
        );

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
                .await
                .is_err(),
            "an approval reply is a control message and must not also dispatch as conversation"
        );

        // The interception already removed the entry, so the guard has nothing
        // left to clean up.
        guard.disarm();
    }

    /// Every ordinary decision, driven from `request_approval` all the way
    /// through the production inbound boundary and back to the waiting caller.
    ///
    /// The test above starts at a hand-registered token, which leaves the first
    /// half of the round trip unexercised: nothing checks that the prompt
    /// `request_approval` actually sends carries a token the listener can read,
    /// nor that the decision the listener delivers is the one the waiting trait
    /// call returns. Every other approval test in this file either parks a
    /// token itself or calls the resolver itself, so the two halves are only
    /// ever proven separately, and a listener that derived the wrong alias,
    /// chat or authorization signal would leave all of them green.
    ///
    /// The send hook is the join. It runs inside `request_approval`, after the
    /// token is registered and while the caller is about to wait, so it is the
    /// only place a test can read this request's real token and then answer it
    /// the way a person would.
    ///
    /// `always` matters as much as `yes` here: the interception decodes the
    /// keyword, and a parser that collapsed `always` into a plain approve would
    /// silently downgrade a blanket grant. `no` is the one that needs the outer
    /// deadline below, because a timed-out request ALSO denies. Without a guard
    /// shorter than `approval_timeout_secs`, a completely broken interception
    /// would return `Deny` after 300s and the `no` case would pass for the
    /// wrong reason.
    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn every_decision_round_trips_from_the_send_seam_through_interception() {
        use wacore::types::message::{MessageInfo, MessageSource};
        use whatsapp_rust::TokioRuntime;
        use whatsapp_rust::bot::Bot;
        use whatsapp_rust_tokio_transport::TokioWebSocketTransportFactory;
        use whatsapp_rust_ureq_http_client::UreqHttpClient;

        const SENDER_PHONE: &str = "15557654321";
        let chat = Jid::pn(SENDER_PHONE).to_string();
        let alias = "approval-round-trip-test";

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::whatsapp_storage::RusqliteStore::new(tmp.path()).unwrap());
        let bot = Bot::builder()
            .with_backend_arc(store.clone())
            .with_transport_factory(TokioWebSocketTransportFactory::new())
            .with_http_client(UreqHttpClient::new())
            .with_runtime(TokioRuntime)
            .build()
            .await
            .unwrap();
        let client = bot.client();

        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let context = WhatsAppInboundContext {
            tx,
            alias: Arc::new(alias.to_string()),
            peer_resolver: Arc::new(|| vec![format!("+{SENDER_PHONE}")]),
            allowed_groups_resolver: Arc::new(Vec::new),
            mode: zeroclaw_config::schema::WhatsAppWebMode::Personal,
            dm_policy: zeroclaw_config::schema::WhatsAppChatPolicy::Allowlist,
            group_policy: zeroclaw_config::schema::WhatsAppChatPolicy::Allowlist,
            self_chat_mode: false,
            mention_only: false,
            passive_group_context: false,
            bot_phone: Arc::new(Mutex::new(None)),
            bot_lid: Arc::new(Mutex::new(None)),
            dm_mention_patterns: Arc::new(Vec::new()),
            group_mention_patterns: Arc::new(Vec::new()),
            transcription_config: None,
            transcription_manager: None,
            voice_chats: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        };

        for (word, expected) in [
            ("yes", ChannelApprovalResponse::Approve),
            ("no", ChannelApprovalResponse::Deny),
            ("always", ChannelApprovalResponse::AlwaysApprove),
        ] {
            // The hook publishes this request's real token the moment the
            // prompt "sends", which is what lets the reply below be a genuine
            // answer to it rather than a token the test chose.
            let (token_tx, mut token_rx) = tokio::sync::mpsc::channel::<String>(1);
            let channel = WhatsAppWebChannel::new(
                &zeroclaw_config::schema::WhatsAppConfig::default(),
                alias,
                Arc::new(|| vec![format!("+{SENDER_PHONE}")]),
                Arc::new(Vec::new),
            )
            .with_approval_send_hook(Arc::new(move |message: SendMessage| {
                let token_tx = token_tx.clone();
                Box::pin(async move {
                    let token = message
                        .content
                        .split_once('[')
                        .and_then(|(_, rest)| rest.split_once(']'))
                        .map(|(token, _)| token.to_string())
                        .expect("the prompt must carry its token in brackets");
                    token_tx
                        .send(token)
                        .await
                        .expect("the driver must still be waiting for the token");
                    // Delivered. Control reaches the wait, which is where the
                    // inbound reply has to find it.
                    Ok(())
                })
            }));

            let request = ChannelApprovalRequest {
                tool_name: "shell".to_string(),
                arguments_summary: format!("echo {word}"),
                raw_arguments: None,
                position: None,
            };

            let asking = channel.request_approval(&chat, &request);

            let replying = async {
                let token = token_rx
                    .recv()
                    .await
                    .expect("the send hook must publish the token before the wait");
                let reply_event = single_message_event(
                    Arc::new(waproto::whatsapp::Message {
                        conversation: Some(format!("{token} {word}")),
                        ..Default::default()
                    }),
                    Arc::new(MessageInfo {
                        source: MessageSource {
                            chat: Jid::pn(SENDER_PHONE),
                            sender: Jid::pn(SENDER_PHONE),
                            is_from_me: false,
                            is_group: false,
                            ..Default::default()
                        },
                        id: format!("approval-round-trip-{word}"),
                        r#type: "text".to_string(),
                        push_name: "Approval Replier".to_string(),
                        timestamp: chrono::Utc::now(),
                        ..Default::default()
                    }),
                );
                WhatsAppWebChannel::handle_inbound_message_event(&reply_event, &client, &context)
                    .await;
                token
            };

            // Shorter than `approval_timeout_secs`, on purpose. See the note
            // above about `no`: this deadline is what separates "the reply was
            // delivered" from "nobody answered and the timeout denied".
            let (decision, token) =
                tokio::time::timeout(std::time::Duration::from_secs(10), async {
                    tokio::join!(asking, replying)
                })
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "the {word:?} reply never reached the waiting request; a decision that \
                     arrives through interception resolves in milliseconds, so this deadline \
                     means the inbound boundary dropped it"
                    )
                });

            assert_eq!(
                decision.expect("a delivered decision must not surface as an error"),
                Some(expected.clone()),
                "a `<token> {word}` reply must resolve the waiting request to {expected:?}"
            );

            assert!(
                !PENDING_APPROVALS.lock().await.contains_key(&token),
                "a resolved request must leave no entry behind for a later reply to hit"
            );

            // Deterministic rather than timed: `handle_inbound_message_event`
            // is awaited to completion above, so anything it dispatched is
            // already queued by now.
            assert!(
                rx.try_recv().is_err(),
                "an approval reply is a control message and must not also dispatch as \
                 conversation"
            );
        }
    }

    /// The send-error branch's generation check, proven from outside.
    ///
    /// Scoping is only observable when the entry under THIS request's token is
    /// replaced while the branch is in flight, which is the race the check
    /// exists for: a resolver takes this registration out, a later request
    /// reserves the same six-character code, and an unconditional `remove`
    /// here would delete that unrelated request instead of ours.
    ///
    /// The send hook is what makes the replacement reachable. It runs after
    /// the token is registered and before any cleanup, which is the only
    /// window in which the map holds this request's entry untouched. Reverting
    /// the branch to `PENDING_APPROVALS.lock().await.remove(&token)` fails
    /// this test at the `expect` below, which is the check it is here to pin.
    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn send_error_cleanup_leaves_a_reused_token_alone() {
        // The registration a later request would hold after reusing the code.
        let reuser_id = uuid::Uuid::new_v4();
        let seen_token = Arc::new(std::sync::Mutex::new(None::<String>));
        // Hold the replacement's receiver open for the length of the test, so
        // the entry the cleanup must spare is a live one rather than a husk.
        let reuser_receiver = Arc::new(std::sync::Mutex::new(None));

        let recorder = Arc::clone(&seen_token);
        let receiver_slot = Arc::clone(&reuser_receiver);
        let channel = WhatsAppWebChannel::new(
            &zeroclaw_config::schema::WhatsAppConfig::default(),
            "alias-a",
            Arc::new(Vec::new),
            Arc::new(Vec::new),
        )
        .with_approval_send_hook(Arc::new(move |message: SendMessage| {
            let recorder = Arc::clone(&recorder);
            let receiver_slot = Arc::clone(&receiver_slot);
            Box::pin(async move {
                let token = message
                    .content
                    .split_once('[')
                    .and_then(|(_, rest)| rest.split_once(']'))
                    .map(|(token, _)| token.to_string())
                    .expect("the prompt must carry its token in brackets");

                let (responder, receiver) = tokio::sync::oneshot::channel();
                PENDING_APPROVALS.lock().await.insert(
                    token.clone(),
                    PendingApproval {
                        registration_id: reuser_id,
                        responder,
                        binding: ApprovalBinding {
                            alias: "alias-b".to_string(),
                            chat: "2@s.whatsapp.net".to_string(),
                            is_group: false,
                        },
                    },
                );
                *receiver_slot.lock().unwrap() = Some(receiver);
                *recorder.lock().unwrap() = Some(token);

                Err(anyhow::Error::msg("send refused by the test hook"))
            })
        }));

        let request = ChannelApprovalRequest {
            tool_name: "shell".to_string(),
            arguments_summary: "ls".to_string(),
            raw_arguments: None,
            position: None,
        };
        let err = channel
            .request_approval("1@s.whatsapp.net", &request)
            .await
            .expect_err("the hook must fail the send");
        assert!(
            err.to_string().contains("send refused by the test hook"),
            "the caller must get the send's own error, not a later one: {err}"
        );

        let token = seen_token
            .lock()
            .unwrap()
            .clone()
            .expect("the hook must have run");
        let pending = PENDING_APPROVALS.lock().await;
        let entry = pending.get(&token).expect(
            "the send-error cleanup removed a token that a later request had already reserved",
        );
        assert_eq!(
            entry.registration_id, reuser_id,
            "the surviving entry must be the reusing registration, not ours"
        );
        drop(pending);
        PENDING_APPROVALS.lock().await.remove(&token);
    }

    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn cancelled_request_leaves_no_live_token() {
        let registration = register_pending_approval(ApprovalBinding {
            alias: "alias-a".into(),
            chat: "1@s.whatsapp.net".into(),
            is_group: false,
        })
        .await;
        let token = registration.token.clone();
        assert!(PENDING_APPROVALS.lock().await.contains_key(&token));

        let request = zeroclaw_spawn::spawn!(async move {
            let _registration = registration;
            std::future::pending::<()>().await;
        });
        request.abort();
        let _ = request.await;

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while PENDING_APPROVALS.lock().await.contains_key(&token) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dropping a registered request must remove its token");
    }

    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn approval_reply_to_dropped_receiver_is_refused() {
        let receiver = park_token_as("alias-a", "aaa012", "1@s.whatsapp.net", false).await;
        drop(receiver);

        let out = resolve_approval_reply(
            "aaa012",
            ChannelApprovalResponse::Approve,
            "alias-a",
            "1@s.whatsapp.net",
            true,
        )
        .await;
        assert_eq!(out, Err(ApprovalRefusal::ReceiverGone));
        assert!(!PENDING_APPROVALS.lock().await.contains_key("aaa012"));
    }

    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn stale_cleanup_does_not_remove_reused_token() {
        let old_registration_id = uuid::Uuid::new_v4();
        let current_registration_id = uuid::Uuid::new_v4();
        let (responder, _receiver) = tokio::sync::oneshot::channel();
        PENDING_APPROVALS.lock().await.insert(
            "aaa013".to_string(),
            PendingApproval {
                registration_id: current_registration_id,
                responder,
                binding: ApprovalBinding {
                    alias: "default".into(),
                    chat: "1@s.whatsapp.net".into(),
                    is_group: false,
                },
            },
        );

        assert!(
            !remove_pending_approval_if_matches("aaa013", old_registration_id).await,
            "cleanup from an older registration must not remove a reused token"
        );
        assert_eq!(
            PENDING_APPROVALS
                .lock()
                .await
                .get("aaa013")
                .map(|entry| entry.registration_id),
            Some(current_registration_id)
        );
        PENDING_APPROVALS.lock().await.remove("aaa013");
    }

    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn group_admission_is_rechecked_before_an_approval_reply_resolves() {
        const GROUP: &str = "123@g.us";
        let mut receiver = park_token("aaa011", GROUP, true).await;
        let allowed_groups = Arc::new(parking_lot::RwLock::new(vec![GROUP.to_string()]));
        let live_groups = Arc::clone(&allowed_groups);
        let resolver = move || live_groups.read().clone();

        *allowed_groups.write() = vec!["other@g.us".to_string()];
        let refused = resolve_approval_reply_with_group_admission(
            "aaa011",
            ChannelApprovalResponse::Approve,
            "default",
            GROUP,
            true,
            true,
            &resolver,
            &zeroclaw_config::schema::WhatsAppChatPolicy::Allowlist,
            &zeroclaw_config::schema::WhatsAppChatPolicy::All,
            SelfChatVerdict::NotSelfChat,
        )
        .await;
        assert_eq!(refused, Err(ApprovalRefusal::GroupNoLongerAllowed));
        assert!(receiver.try_recv().is_err());
        assert!(PENDING_APPROVALS.lock().await.contains_key("aaa011"));

        allowed_groups.write().push(GROUP.to_string());
        let accepted = resolve_approval_reply_with_group_admission(
            "aaa011",
            ChannelApprovalResponse::Approve,
            "default",
            GROUP,
            true,
            true,
            &resolver,
            &zeroclaw_config::schema::WhatsAppChatPolicy::Allowlist,
            &zeroclaw_config::schema::WhatsAppChatPolicy::All,
            SelfChatVerdict::NotSelfChat,
        )
        .await;
        assert_eq!(accepted, Ok(()));
        assert_eq!(receiver.await.unwrap(), ChannelApprovalResponse::Approve);
    }

    /// Emptying `allowed_groups` entirely is a revocation too, not a reset to
    /// open. An operator who clears the list while an approval is outstanding
    /// must not have that reply honoured.
    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn clearing_allowed_groups_refuses_an_outstanding_approval() {
        const GROUP: &str = "124@g.us";
        let mut receiver = park_token("aaa014", GROUP, true).await;
        let allowed_groups = Arc::new(parking_lot::RwLock::new(vec![GROUP.to_string()]));
        let live_groups = Arc::clone(&allowed_groups);
        let resolver = move || live_groups.read().clone();

        allowed_groups.write().clear();
        let refused = resolve_approval_reply_with_group_admission(
            "aaa014",
            ChannelApprovalResponse::Approve,
            "default",
            GROUP,
            true,
            true,
            &resolver,
            &zeroclaw_config::schema::WhatsAppChatPolicy::Allowlist,
            &zeroclaw_config::schema::WhatsAppChatPolicy::All,
            SelfChatVerdict::NotSelfChat,
        )
        .await;
        assert_eq!(refused, Err(ApprovalRefusal::GroupNoLongerAllowed));
        assert!(receiver.try_recv().is_err());

        // CONTROL: the same cleared list under `all` still admits, so the refusal
        // above is the policy deciding rather than the clear() alone.
        let accepted = resolve_approval_reply_with_group_admission(
            "aaa014",
            ChannelApprovalResponse::Approve,
            "default",
            GROUP,
            true,
            true,
            &resolver,
            &zeroclaw_config::schema::WhatsAppChatPolicy::All,
            &zeroclaw_config::schema::WhatsAppChatPolicy::All,
            SelfChatVerdict::NotSelfChat,
        )
        .await;
        assert_eq!(accepted, Ok(()));
        assert_eq!(receiver.await.unwrap(), ChannelApprovalResponse::Approve);
    }

    /// An approval reply executes a pending tool, so it has to clear the same
    /// gates an ordinary message clears. A matching non-empty `allowed_groups`
    /// satisfies the identity gate under every policy, `ignore` included, so
    /// the identity gate alone would let a control reply act in a chat the
    /// operator told ZeroClaw to ignore.
    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn an_ignored_group_refuses_an_approval_its_own_messages_cannot_reach() {
        use zeroclaw_config::schema::WhatsAppChatPolicy as Policy;
        const GROUP: &str = "125@g.us";
        let mut receiver = park_token("aaa015", GROUP, true).await;
        let allowed_groups = Arc::new(parking_lot::RwLock::new(vec![GROUP.to_string()]));
        let live_groups = Arc::clone(&allowed_groups);
        let resolver = move || live_groups.read().clone();

        // The list MATCHES this group, so the identity gate admits and only the
        // chat-type policy can refuse.
        let refused = resolve_approval_reply_with_group_admission(
            "aaa015",
            ChannelApprovalResponse::Approve,
            "default",
            GROUP,
            true,
            true,
            &resolver,
            &Policy::Ignore,
            &Policy::All,
            SelfChatVerdict::NotSelfChat,
        )
        .await;
        assert_eq!(
            refused,
            Err(ApprovalRefusal::GroupNoLongerAllowed),
            "an approval reply must not resolve in a group the policy ignores"
        );
        assert!(receiver.try_recv().is_err());
        assert!(PENDING_APPROVALS.lock().await.contains_key("aaa015"));

        // CONTROLS: the identical call under the two policies that DO answer
        // groups must still resolve, so the refusal above is policy selection
        // rather than a gate that now rejects every group approval.
        for policy in [Policy::Allowlist, Policy::All] {
            let accepted = resolve_approval_reply_with_group_admission(
                "aaa015",
                ChannelApprovalResponse::Approve,
                "default",
                GROUP,
                true,
                true,
                &resolver,
                &policy,
                &Policy::All,
                SelfChatVerdict::NotSelfChat,
            )
            .await;
            assert_eq!(
                accepted,
                Ok(()),
                "{policy:?} answers groups and must resolve"
            );
            assert_eq!(receiver.await.unwrap(), ChannelApprovalResponse::Approve);
            receiver = park_token("aaa015", GROUP, true).await;
        }
        PENDING_APPROVALS.lock().await.remove("aaa015");
    }

    /// The DM half of the same gap. `dm_policy` was not a parameter at all, so
    /// an approval reply in a direct message skipped the chat-type gate outright
    /// while an ordinary message in that same chat was dropped.
    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn an_ignored_dm_refuses_an_approval_its_own_messages_cannot_reach() {
        use zeroclaw_config::schema::WhatsAppChatPolicy as Policy;
        const DM: &str = "15550001@s.whatsapp.net";
        let mut receiver = park_token("aaa016", DM, false).await;
        let resolver = || Vec::new();

        let refused = resolve_approval_reply_with_group_admission(
            "aaa016",
            ChannelApprovalResponse::Approve,
            "default",
            DM,
            false,
            true,
            &resolver,
            &Policy::All,
            &Policy::Ignore,
            SelfChatVerdict::NotSelfChat,
        )
        .await;
        assert_eq!(
            refused,
            Err(ApprovalRefusal::DmNoLongerAllowed),
            "an approval reply must not resolve in a DM the policy ignores"
        );
        assert!(receiver.try_recv().is_err());
        assert!(PENDING_APPROVALS.lock().await.contains_key("aaa016"));

        // CONTROL: the identical call under the two policies that DO answer DMs
        // must still resolve, so the refusal above is policy selection rather
        // than a gate that now rejects every DM approval.
        for policy in [Policy::Allowlist, Policy::All] {
            let accepted = resolve_approval_reply_with_group_admission(
                "aaa016",
                ChannelApprovalResponse::Approve,
                "default",
                DM,
                false,
                true,
                &resolver,
                &Policy::All,
                &policy,
                SelfChatVerdict::NotSelfChat,
            )
            .await;
            assert_eq!(accepted, Ok(()), "{policy:?} answers DMs and must resolve");
            assert_eq!(receiver.await.unwrap(), ChannelApprovalResponse::Approve);
            receiver = park_token("aaa016", DM, false).await;
        }
        PENDING_APPROVALS.lock().await.remove("aaa016");
    }

    /// The personal-mode self-chat exception reaches the approval path too.
    /// The conversation path admits the operator's own thread whatever
    /// `dm_policy` says, so a reply there has to be admitted on the same terms;
    /// gating it on `dm_policy` alone strands a prompt the operator requested
    /// and can never answer.
    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn a_personal_self_chat_resolves_an_approval_an_ignored_dm_could_not() {
        use zeroclaw_config::schema::WhatsAppChatPolicy as Policy;
        const SELF: &str = "15550002@s.whatsapp.net";
        let receiver = park_token("aaa017", SELF, false).await;
        let resolver = || Vec::new();

        // `dm_policy = ignore` throughout: the ONLY difference between the two
        // calls below is the self-chat verdict, so it is the exception being
        // exercised rather than a permissive policy.
        let accepted = resolve_approval_reply_with_group_admission(
            "aaa017",
            ChannelApprovalResponse::Approve,
            "default",
            SELF,
            false,
            true,
            &resolver,
            &Policy::All,
            &Policy::Ignore,
            SelfChatVerdict::Admitted,
        )
        .await;
        assert_eq!(
            accepted,
            Ok(()),
            "an enabled personal self-chat must resolve its own approval"
        );
        assert_eq!(receiver.await.unwrap(), ChannelApprovalResponse::Approve);

        // CONTROL: the identical call, same policy, differing only in the
        // verdict, must still refuse. Without this the acceptance above would
        // also pass a gate that admitted every DM.
        let mut receiver = park_token("aaa017", SELF, false).await;
        let refused = resolve_approval_reply_with_group_admission(
            "aaa017",
            ChannelApprovalResponse::Approve,
            "default",
            SELF,
            false,
            true,
            &resolver,
            &Policy::All,
            &Policy::Ignore,
            SelfChatVerdict::NotSelfChat,
        )
        .await;
        assert_eq!(refused, Err(ApprovalRefusal::DmNoLongerAllowed));
        assert!(receiver.try_recv().is_err());

        // And a self-chat the operator has switched OFF is dropped by the
        // conversation path, so a reply in it must not resolve either.
        let refused = resolve_approval_reply_with_group_admission(
            "aaa017",
            ChannelApprovalResponse::Approve,
            "default",
            SELF,
            false,
            true,
            &resolver,
            &Policy::All,
            &Policy::All,
            SelfChatVerdict::Disabled,
        )
        .await;
        assert_eq!(
            refused,
            Err(ApprovalRefusal::DmNoLongerAllowed),
            "self_chat_mode=false is ignored by the channel, so a reply cannot resolve"
        );
        assert!(receiver.try_recv().is_err());
        PENDING_APPROVALS.lock().await.remove("aaa017");
    }

    /// The predicate itself, so the two call sites cannot disagree about what
    /// counts as a self-chat.
    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn self_chat_verdict_matrix() {
        use zeroclaw_config::schema::WhatsAppWebMode as Mode;
        let v = super::self_chat_verdict;
        const U: &str = "15550002";
        const C: &str = "15550002@s.whatsapp.net";

        assert_eq!(
            v(&Mode::Personal, true, false, U, C, true),
            SelfChatVerdict::Admitted
        );
        assert_eq!(
            v(&Mode::Personal, false, false, U, C, true),
            SelfChatVerdict::Disabled
        );
        // Business mode has no self-chat affordance at all.
        assert_eq!(
            v(&Mode::Business, true, false, U, C, true),
            SelfChatVerdict::NotSelfChat
        );
        // Each remaining leg alone is enough to make it not a self-chat.
        assert_eq!(
            v(&Mode::Personal, true, true, U, C, true),
            SelfChatVerdict::NotSelfChat,
            "a group is never the operator self-chat"
        );
        assert_eq!(
            v(&Mode::Personal, true, false, "15550003", C, true),
            SelfChatVerdict::NotSelfChat,
            "a different sender is not the operator talking to themselves"
        );
        assert_eq!(
            v(&Mode::Personal, true, false, U, C, false),
            SelfChatVerdict::NotSelfChat,
            "not fromMe is not the operator talking to themselves"
        );
    }

    /// The same exception, driven through `handle_inbound_message_event`
    /// rather than through the helper.
    ///
    /// The helper-level tests cannot see this: they are handed a verdict, so
    /// they stay green even if the handler computes the wrong one or passes it
    /// to the wrong parameter. This exercises the hoisted call site itself.
    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn inbound_path_admits_a_personal_self_chat_approval_reply() {
        use wacore::types::message::{MessageInfo, MessageSource};
        use whatsapp_rust::TokioRuntime;
        use whatsapp_rust::bot::Bot;
        use whatsapp_rust_tokio_transport::TokioWebSocketTransportFactory;
        use whatsapp_rust_ureq_http_client::UreqHttpClient;
        use zeroclaw_config::schema::{WhatsAppChatPolicy as Policy, WhatsAppWebMode as Mode};

        const OPERATOR: &str = "15551230001";

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::whatsapp_storage::RusqliteStore::new(tmp.path()).unwrap());
        let bot = Bot::builder()
            .with_backend_arc(store)
            .with_transport_factory(TokioWebSocketTransportFactory::new())
            .with_http_client(UreqHttpClient::new())
            .with_runtime(TokioRuntime)
            .build()
            .await
            .unwrap();
        let client = bot.client();

        // A self-chat is the operator's own thread: chat == sender, and the
        // message is fromMe. That is what `self_chat_verdict` keys on.
        let self_chat_reply = |token: &str| {
            let jid: Jid = format!("{OPERATOR}@s.whatsapp.net")
                .parse()
                .expect("jid parses");
            single_message_event(
                Arc::new(waproto::whatsapp::Message {
                    conversation: Some(format!("{token} yes")),
                    ..Default::default()
                }),
                Arc::new(MessageInfo {
                    source: MessageSource {
                        chat: jid.clone(),
                        sender: jid,
                        is_from_me: true,
                        is_group: false,
                        ..Default::default()
                    },
                    id: format!("selfchat-approval-{token}"),
                    r#type: "text".to_string(),
                    push_name: "Operator".to_string(),
                    timestamp: chrono::Utc::now(),
                    ..Default::default()
                }),
            )
        };

        // `dm_policy = ignore` in BOTH contexts below. The only difference is
        // `self_chat_mode`, so what is being exercised is the exception rather
        // than a permissive policy.
        let context_for = |self_chat_mode: bool, tx: tokio::sync::mpsc::Sender<ChannelMessage>| {
            WhatsAppInboundContext {
                tx,
                alias: Arc::new("default".to_string()),
                peer_resolver: Arc::new(|| vec![format!("+{OPERATOR}")]),
                allowed_groups_resolver: Arc::new(Vec::new),
                mode: Mode::Personal,
                dm_policy: Policy::Ignore,
                group_policy: Policy::Ignore,
                self_chat_mode,
                mention_only: false,
                passive_group_context: false,
                bot_phone: Arc::new(Mutex::new(None)),
                bot_lid: Arc::new(Mutex::new(None)),
                dm_mention_patterns: Arc::new(Vec::new()),
                group_mention_patterns: Arc::new(Vec::new()),
                transcription_config: None,
                transcription_manager: None,
                voice_chats: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            }
        };

        // self_chat_mode = true: the documented exception. The reply must
        // resolve the pending tool even though dm_policy ignores DMs.
        let chat = format!("{OPERATOR}@s.whatsapp.net");
        let receiver = park_token("aaa018", &chat, false).await;
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let context = context_for(true, tx);
        WhatsAppWebChannel::handle_inbound_message_event(
            &self_chat_reply("aaa018"),
            &client,
            &context,
        )
        .await;
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), receiver)
                .await
                .expect("the self-chat approval must resolve, not time out")
                .expect("the responder must still be open"),
            ChannelApprovalResponse::Approve,
            "an enabled personal self-chat must resolve its own approval"
        );

        // CONTROL: identical event and identical dm_policy, differing only in
        // self_chat_mode. The channel ignores that thread, so the reply must
        // NOT resolve. Without this the acceptance above would also pass a
        // handler that ignored the policy entirely.
        let mut receiver = park_token("aaa019", &chat, false).await;
        let (tx, _rx2) = tokio::sync::mpsc::channel(4);
        let context = context_for(false, tx);
        WhatsAppWebChannel::handle_inbound_message_event(
            &self_chat_reply("aaa019"),
            &client,
            &context,
        )
        .await;
        assert!(
            receiver.try_recv().is_err(),
            "self_chat_mode=false is ignored by the channel, so the reply must not resolve"
        );
        PENDING_APPROVALS.lock().await.remove("aaa019");
    }

    /// `PENDING_APPROVALS` is process-wide, so before the alias was part of the
    /// binding this was a real authorization bypass rather than a tidiness
    /// issue: the reply path resolves the authorized peers using whichever
    /// instance received the reply. A responder alias A refuses could therefore
    /// approve alias A's tool call by answering through alias B, because B's
    /// allowlist decided. Two aliases sharing a group is a supported
    /// configuration, which is why binding the chat alone did not close it.
    ///
    /// The second half is the part worth pinning: the refusal must leave the
    /// request PENDING, so the legitimate operator can still answer. A refusal
    /// that consumed the token would hand any bystander a way to cancel an
    /// approval by replying to it badly.
    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn approval_reply_from_another_alias_is_refused() {
        const SHARED_GROUP: &str = "shared-group@g.us";
        let _rx = park_token_as("alias-a", "aaa010", SHARED_GROUP, true).await;

        // Alias B replies, and B's OWN allowlist says this responder is fine.
        // That is exactly the bypass: B's policy must not decide A's request.
        let out = resolve_approval_reply(
            "aaa010",
            ChannelApprovalResponse::Approve,
            "alias-b",
            SHARED_GROUP,
            true,
        )
        .await;
        assert_eq!(
            out,
            Err(ApprovalRefusal::ForeignAlias),
            "alias B must not resolve alias A's token, even in a shared chat \
             and even when B's own allowlist would admit the responder"
        );
        assert!(
            PENDING_APPROVALS.lock().await.contains_key("aaa010"),
            "a refused cross-alias reply must leave the request PENDING so the \
             real operator can still answer it"
        );

        // The issuing alias still resolves it normally.
        let out = resolve_approval_reply(
            "aaa010",
            ChannelApprovalResponse::Approve,
            "alias-a",
            SHARED_GROUP,
            true,
        )
        .await;
        assert_eq!(
            out,
            Ok(()),
            "the issuing alias must still be able to answer"
        );
        PENDING_APPROVALS.lock().await.remove("aaa010");
    }

    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn approval_reply_approves() {
        let rx = park_token("aaa001", "1234@s.whatsapp.net", false).await;
        let out = resolve_approval_reply(
            "aaa001",
            ChannelApprovalResponse::Approve,
            "default",
            "1234@s.whatsapp.net",
            true,
        )
        .await;
        assert!(out.is_ok());
        assert_eq!(rx.await.unwrap(), ChannelApprovalResponse::Approve);
    }

    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn approval_reply_denies() {
        let rx = park_token("aaa002", "1234@s.whatsapp.net", false).await;
        assert!(
            resolve_approval_reply(
                "aaa002",
                ChannelApprovalResponse::Deny,
                "default",
                "1234@s.whatsapp.net",
                true,
            )
            .await
            .is_ok()
        );
        assert_eq!(rx.await.unwrap(), ChannelApprovalResponse::Deny);
    }

    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn approval_reply_always_approves() {
        let rx = park_token("aaa003", "1234@s.whatsapp.net", false).await;
        assert!(
            resolve_approval_reply(
                "aaa003",
                ChannelApprovalResponse::AlwaysApprove,
                "default",
                "1234@s.whatsapp.net",
                true,
            )
            .await
            .is_ok()
        );
        assert_eq!(rx.await.unwrap(), ChannelApprovalResponse::AlwaysApprove);
    }

    /// A reply carrying a VALID token, from the right chat, but from a number
    /// that is not an authorized peer, must not decide anything. This is the
    /// case slack, telegram and matrix currently allow.
    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn approval_reply_from_unauthorized_responder_is_refused() {
        let mut rx = park_token("aaa004", "999@g.us", true).await;
        assert_eq!(
            resolve_approval_reply(
                "aaa004",
                ChannelApprovalResponse::Approve,
                "default",
                "999@g.us",
                false,
            )
            .await,
            Err(ApprovalRefusal::UnauthorizedResponder)
        );
        // Nothing was decided.
        assert!(rx.try_recv().is_err());
        // And the request is STILL PENDING, so a stranger cannot cancel an
        // approval by answering it badly; the operator can still answer it.
        assert!(
            resolve_approval_reply(
                "aaa004",
                ChannelApprovalResponse::Approve,
                "default",
                "999@g.us",
                true,
            )
            .await
            .is_ok()
        );
        assert_eq!(rx.await.unwrap(), ChannelApprovalResponse::Approve);
    }

    /// A valid token replayed from a DIFFERENT chat is not this request's
    /// answer, even from an allowlisted number.
    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn approval_reply_from_foreign_chat_is_refused() {
        let mut rx = park_token("aaa005", "111@s.whatsapp.net", false).await;
        assert_eq!(
            resolve_approval_reply(
                "aaa005",
                ChannelApprovalResponse::Approve,
                "default",
                "222@s.whatsapp.net",
                true,
            )
            .await,
            Err(ApprovalRefusal::ForeignChat)
        );
        assert!(rx.try_recv().is_err());
        PENDING_APPROVALS.lock().await.remove("aaa005");
    }

    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn approval_reply_unknown_token_is_refused() {
        assert_eq!(
            resolve_approval_reply(
                "zzz999",
                ChannelApprovalResponse::Approve,
                "default",
                "1@s.whatsapp.net",
                true,
            )
            .await,
            Err(ApprovalRefusal::UnknownToken)
        );
    }

    // ── Automatic voice queue ──

    #[cfg(feature = "whatsapp-web")]
    fn voice_channel() -> WhatsAppWebChannel {
        WhatsAppWebChannel::new(
            &approval_cfg(300),
            "alias",
            Arc::new(Vec::new),
            Arc::new(Vec::new),
        )
    }

    /// Long enough and plain enough to pass the content heuristic, so what the
    /// tests below observe is the suppression flag and nothing else.
    #[cfg(feature = "whatsapp-web")]
    const SPOKEN_REPLY: &str = "Sure, the tasting is on Friday at seven and there are still seats.";

    /// A notice that says it must not be spoken is not spoken, however
    /// conversational it reads. Producers that set this flag include SOP
    /// approval notices and `send_via` against a text-only peer group.
    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn a_suppressed_message_never_reaches_the_voice_queue() {
        let ch = voice_channel();

        assert_eq!(
            ch.queue_pending_voice("chat", SPOKEN_REPLY, true),
            Some("suppress_voice")
        );
        assert!(
            ch.pending_voice.lock().expect("lock").is_empty(),
            "a suppressed message queues nothing to synthesize"
        );

        // Positive control: the same text, unsuppressed, is queued.
        assert_eq!(ch.queue_pending_voice("chat", SPOKEN_REPLY, false), None);
        assert!(ch.pending_voice.lock().expect("lock").contains_key("chat"));
    }

    /// The reason suppression is answered before the queue is touched: a
    /// notice arriving mid-conversation used to overwrite the reply waiting to
    /// be spoken and restart its ten-second timer, so the chat heard the
    /// notice instead of the answer, later.
    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn a_suppressed_notice_leaves_a_queued_reply_untouched() {
        let ch = voice_channel();
        ch.queue_pending_voice("chat", SPOKEN_REPLY, false);
        let queued = ch
            .pending_voice
            .lock()
            .expect("lock")
            .get("chat")
            .cloned()
            .expect("queued");

        ch.queue_pending_voice("chat", "Approval request expired.", true);

        let after = ch
            .pending_voice
            .lock()
            .expect("lock")
            .get("chat")
            .cloned()
            .expect("still queued");
        assert_eq!(after.0, queued.0, "the reply text is the one queued");
        assert_eq!(after.1, queued.1, "and its timer was not restarted");
    }

    /// Suppression is a property of one message, not a sign that the
    /// conversation went back to text: the chat stays marked, so the next
    /// conversational reply is still spoken.
    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn suppression_does_not_end_the_voice_conversation() {
        let ch = voice_channel();
        ch.voice_chats
            .lock()
            .expect("lock")
            .insert("chat".to_string());

        ch.queue_pending_voice("chat", "Approval request expired.", true);

        assert!(
            ch.voice_chats.lock().expect("lock").contains("chat"),
            "the chat is still a voice chat"
        );
        assert_eq!(ch.queue_pending_voice("chat", SPOKEN_REPLY, false), None);
    }

    /// The content heuristic is unchanged, and still applies when nothing is
    /// suppressed.
    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn the_content_heuristic_still_decides_when_nothing_is_suppressed() {
        for (content, reason) in [
            (
                "{\"status\": \"ok\", \"count\": 3, \"note\": \"a long json body\"}",
                "json_object",
            ),
            (
                "https://example.com/a/rather/long/link/to/somewhere/else",
                "url_prefix",
            ),
            (
                "Error: the upstream request timed out after thirty seconds",
                "error_prefix",
            ),
            ("ok", "too_short"),
        ] {
            assert_eq!(
                WhatsAppWebChannel::voice_queue_skip_reason(false, content),
                Some(reason),
                "{content}"
            );
        }
        assert_eq!(
            WhatsAppWebChannel::voice_queue_skip_reason(false, SPOKEN_REPLY),
            None
        );
    }

    /// `approval_timeout_secs` reaches the channel from config, so every
    /// construction path picks it up rather than three call sites each
    /// remembering to.
    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn approval_timeout_is_read_from_config() {
        let ch = WhatsAppWebChannel::new(
            &approval_cfg(45),
            "alias",
            Arc::new(Vec::new),
            Arc::new(Vec::new),
        );
        assert_eq!(ch.approval_timeout_secs, 45);
    }

    /// A config PARSED from TOML with the key absent gets 300, the documented
    /// channel default. This is the path every real operator takes.
    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn approval_timeout_defaults_to_300_when_parsed() {
        let cfg: zeroclaw_config::schema::WhatsAppConfig =
            toml::from_str("enabled = true").expect("minimal whatsapp config parses");
        assert_eq!(
            cfg.approval_timeout_secs, 300,
            "serde default = default_channel_approval_timeout_secs"
        );
        let ch = WhatsAppWebChannel::new(&cfg, "alias", Arc::new(Vec::new), Arc::new(Vec::new));
        assert_eq!(ch.approval_timeout_secs, 300);
    }

    // ── Native polls ──

    #[cfg(feature = "whatsapp-web")]
    fn poll_channel(allowed_numbers: &[&str]) -> WhatsAppWebChannel {
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test-whatsapp-polls.db".into()),
            ..Default::default()
        };
        let peers: Vec<String> = allowed_numbers.iter().map(|n| (*n).to_string()).collect();
        WhatsAppWebChannel::new(
            &cfg,
            "poll_alias",
            Arc::new(move || peers.clone()),
            Arc::new(Vec::new),
        )
    }

    #[cfg(feature = "whatsapp-web")]
    fn poll_request(recipient: &str) -> zeroclaw_api::channel::PollRequest {
        zeroclaw_api::channel::PollRequest::new(
            recipient,
            "Which tasting slot?",
            vec!["Friday".into(), "Saturday".into()],
        )
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn the_channel_advertises_native_polls() {
        assert!(poll_channel(&["+15550001111"]).supports_native_polls());
    }

    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn a_poll_without_a_client_reports_the_same_not_connected_error_as_send() {
        let channel = poll_channel(&["+15550001111"]);
        let error = channel
            .send_poll(&poll_request("+15550001111"))
            .await
            .expect_err("no client is connected");
        assert!(error.to_string().contains("not connected"), "{error}");
    }

    /// `send` drops a disallowed recipient with a warning and reports success.
    /// A poll is a tool call, so it has to say that nothing was posted.
    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn a_poll_to_a_number_outside_the_allowlist_fails_loudly() {
        let channel = poll_channel(&["+15550001111"]);
        let error = channel
            .send_poll(&poll_request("+15559999999"))
            .await
            .expect_err("the recipient is not allowed");
        let message = error.to_string();
        assert!(message.contains("allowlist"), "{message}");
        assert!(
            !message.contains("not connected"),
            "the allowlist must be checked before the client, so the caller learns the real reason: {message}"
        );
    }

    /// ...and a config built in Rust now agrees with one parsed from a file.
    ///
    /// This test used to assert the opposite. It pinned `Default::default()` at
    /// 0 and reasoned that operators were unaffected because their config is
    /// parsed. That reasoning was wrong: the supported alias-creation surfaces
    /// build the struct in Rust and persist the result, so the zero reached a
    /// file and survived reload, and zero is an already-elapsed deadline rather
    /// than "wait forever". Every approval on such an alias denied at once.
    ///
    /// The split is fixed in `zeroclaw-config`, so this asserts the channel end
    /// of it: whichever way an operator's alias came into being, the channel
    /// waits the documented timeout. The `zeroclaw-config` side additionally
    /// pins every field against the serde defaults so a new field cannot
    /// reintroduce the divergence quietly.
    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn a_config_built_in_rust_waits_the_documented_timeout() {
        let cfg = zeroclaw_config::schema::WhatsAppConfig::default();
        assert_eq!(
            cfg.approval_timeout_secs, 300,
            "the Rust default must match the documented serde default"
        );
        let ch = WhatsAppWebChannel::new(&cfg, "alias", Arc::new(Vec::new), Arc::new(Vec::new));
        assert_eq!(
            ch.approval_timeout_secs, 300,
            "the channel must inherit that timeout rather than denying at once"
        );
    }

    /// The timeout arm of `request_approval`, driven end to end.
    ///
    /// This replaces two tests that could not fail for the reason their names
    /// gave. One asserted that `tokio::time::timeout` elapses at zero, which is
    /// a property of tokio and holds whatever this channel does. The other
    /// parked a token, performed the removal ITSELF under a comment reading
    /// "what the timeout arm of request_approval does", and then observed that
    /// the token was gone - so it re-proved its own setup line and never
    /// executed the arm. Checked rather than assumed: with the arm's cleanup
    /// deleted AND its `Deny` flipped to `AlwaysApprove`, both stayed green,
    /// along with the other 1522 tests in this crate. A timed-out request
    /// silently granting blanket approval is the worst outcome this transport
    /// has, and nothing in the suite noticed.
    ///
    /// The send hook is what makes the real arm reachable: it stands in for the
    /// vendor client, so the prompt "delivers" and control reaches the timeout
    /// instead of returning early at the send error. `approval_timeout_secs` is
    /// 0 so the deadline is already elapsed and the test does not sleep.
    ///
    /// Both assertions below are load-bearing and fail for different reasons.
    /// The decision assert pins that a timeout DENIES; deleting the cleanup
    /// alone leaves it green. The removal assert pins that the arm drops its
    /// own token so a late reply cannot resolve a request nobody is waiting
    /// on; flipping the decision alone leaves it green.
    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn timeout_denies_and_removes_its_own_token() {
        let seen_token = Arc::new(std::sync::Mutex::new(None::<String>));
        let recorder = Arc::clone(&seen_token);

        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            approval_timeout_secs: 0,
            ..Default::default()
        };

        let channel =
            WhatsAppWebChannel::new(&cfg, "alias-a", Arc::new(Vec::new), Arc::new(Vec::new))
                .with_approval_send_hook(Arc::new(move |message: SendMessage| {
                    let recorder = Arc::clone(&recorder);
                    Box::pin(async move {
                        let token = message
                            .content
                            .split_once('[')
                            .and_then(|(_, rest)| rest.split_once(']'))
                            .map(|(token, _)| token.to_string())
                            .expect("the prompt must carry its token in brackets");
                        *recorder.lock().unwrap() = Some(token);
                        // Delivered. Let the timeout arm run.
                        Ok(())
                    })
                }));

        let request = ChannelApprovalRequest {
            tool_name: "shell".to_string(),
            arguments_summary: "ls".to_string(),
            raw_arguments: None,
            position: None,
        };

        let decision = channel
            .request_approval("1@s.whatsapp.net", &request)
            .await
            .expect("a timeout must resolve to a decision, not an error");
        assert_eq!(
            decision,
            Some(ChannelApprovalResponse::Deny),
            "a request nobody answered must deny, never approve"
        );

        let token = seen_token
            .lock()
            .unwrap()
            .clone()
            .expect("the hook must have run, or the send never reached the timeout");
        assert!(
            !PENDING_APPROVALS.lock().await.contains_key(&token),
            "the timeout arm must remove its own token, or a late reply can \
             resolve a request whose receiver is already gone"
        );
    }

    /// A POSITIVE `approval_timeout_secs` is waited out in full, then denies.
    ///
    /// The test above configures `0`, which reaches the same arm and cannot
    /// distinguish waiting from not waiting: an already-elapsed deadline fires
    /// identically whether the channel read its own configured duration, read
    /// some other field, or hardcoded a constant. That gap is exactly the shape
    /// of the defect this branch fixed on the config side, where an unset field
    /// silently became `0` and every approval denied at once.
    ///
    /// So this asserts the DURATION, not just the decision, and does it twice
    /// with different values. One value would be satisfied by a hardcoded
    /// constant that happened to match; two are not.
    ///
    /// Paused time is what makes that affordable. Under
    /// `#[tokio::test(start_paused = true)]` the runtime advances its clock to
    /// the next deadline whenever nothing is runnable, so the 300-second wait
    /// is measured exactly and costs no wall-clock. The send hook returns `Ok`
    /// so control reaches the wait rather than returning early at the send.
    #[tokio::test(start_paused = true)]
    #[cfg(feature = "whatsapp-web")]
    async fn a_positive_timeout_is_waited_out_in_full_before_denying() {
        for configured_secs in [300_u64, 45_u64] {
            let seen_token = Arc::new(std::sync::Mutex::new(None::<String>));
            let recorder = Arc::clone(&seen_token);

            let cfg = zeroclaw_config::schema::WhatsAppConfig {
                approval_timeout_secs: configured_secs,
                ..Default::default()
            };
            let channel = WhatsAppWebChannel::new(
                &cfg,
                "alias-paused",
                Arc::new(Vec::new),
                Arc::new(Vec::new),
            )
            .with_approval_send_hook(Arc::new(move |message: SendMessage| {
                let recorder = Arc::clone(&recorder);
                Box::pin(async move {
                    let token = message
                        .content
                        .split_once('[')
                        .and_then(|(_, rest)| rest.split_once(']'))
                        .map(|(token, _)| token.to_string())
                        .expect("the prompt must carry its token in brackets");
                    *recorder.lock().unwrap() = Some(token);
                    Ok(())
                })
            }));

            let request = ChannelApprovalRequest {
                tool_name: "shell".to_string(),
                arguments_summary: "ls".to_string(),
                raw_arguments: None,
                position: None,
            };

            let started = tokio::time::Instant::now();
            let decision = channel
                .request_approval("1@s.whatsapp.net", &request)
                .await
                .expect("a timeout must resolve to a decision, not an error");
            let waited = started.elapsed();

            assert_eq!(
                decision,
                Some(ChannelApprovalResponse::Deny),
                "a request nobody answered must deny, never approve"
            );

            let configured = std::time::Duration::from_secs(configured_secs);
            assert!(
                waited >= configured,
                "the channel returned after {waited:?}, short of the configured \
                 {configured:?}; an operator who set that value is not getting it"
            );
            assert!(
                waited < configured + std::time::Duration::from_secs(1),
                "the channel waited {waited:?} against a configured {configured:?}; the \
                 deadline must come from approval_timeout_secs, not from elsewhere"
            );

            let token = seen_token
                .lock()
                .unwrap()
                .clone()
                .expect("the hook must have run, or the send never reached the timeout");
            assert!(
                !PENDING_APPROVALS.lock().await.contains_key(&token),
                "the timeout arm must remove its own token whatever the duration"
            );
        }
    }

    /// A late reply to a timed-out request is refused, from the caller's side.
    ///
    /// The removal assert above pins the map; this pins what the removal BUYS,
    /// which is the property an operator actually depends on.
    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn a_reply_after_timeout_is_refused() {
        let _rx = park_token("aaa006", "1@s.whatsapp.net", false).await;
        assert!(PENDING_APPROVALS.lock().await.contains_key("aaa006"));
        // Bind the id in its own statement. Reading it inline as an argument
        // keeps the map guard alive for the whole call expression, and the
        // helper takes the same lock, so that self-deadlocks - and because the
        // map is process-global, it hangs every other approval test with it.
        let registration_id = {
            let pending = PENDING_APPROVALS.lock().await;
            pending.get("aaa006").expect("just parked").registration_id
        };
        remove_pending_approval_if_matches("aaa006", registration_id).await;
        assert_eq!(
            resolve_approval_reply(
                "aaa006",
                ChannelApprovalResponse::Approve,
                "default",
                "1@s.whatsapp.net",
                true,
            )
            .await,
            Err(ApprovalRefusal::UnknownToken)
        );
    }

    /// The prompt text the operator receives must round-trip through the
    /// channel's own reply parser, or the instructions it prints are wrong.
    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn prompt_instructions_parse_back() {
        let token = crate::util::new_approval_token();
        assert_eq!(token.len(), 6, "parse_approval_reply requires 6 chars");
        for (word, want) in [
            ("yes", ChannelApprovalResponse::Approve),
            ("no", ChannelApprovalResponse::Deny),
            ("always", ChannelApprovalResponse::AlwaysApprove),
        ] {
            let (got_token, got) = crate::util::parse_approval_reply(&format!("{token} {word}"))
                .expect("the prompt own instruction must parse");
            assert_eq!(got_token, token.to_lowercase());
            assert_eq!(got, want);
        }
    }

    // ── PDF document previews ──

    /// A JPEG header carrying only what `jpeg_dimensions` reads: SOI, an
    /// APP0 segment, optionally a DHT segment, then a start-of-frame marker.
    #[cfg(feature = "whatsapp-web")]
    fn jpeg_header(sof_marker: u8, width: u16, height: u16, with_dht: bool) -> Vec<u8> {
        let mut jpeg = vec![0xFF, 0xD8];
        jpeg.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]);
        if with_dht {
            jpeg.extend_from_slice(&[0xFF, 0xC4, 0x00, 0x04, 0x00, 0x00]);
        }
        jpeg.extend_from_slice(&[0xFF, sof_marker, 0x00, 0x11, 0x08]);
        jpeg.extend_from_slice(&height.to_be_bytes());
        jpeg.extend_from_slice(&width.to_be_bytes());
        jpeg.extend_from_slice(&[0x03; 12]);
        jpeg
    }

    /// A minimal three-page PDF, so the preview can be exercised end to end
    /// without a fixture file.
    #[cfg(feature = "whatsapp-web")]
    fn three_page_pdf() -> Vec<u8> {
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>".to_string(),
        ];
        for _ in 0..3 {
            objects.push(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R >>"
                    .to_string(),
            );
        }
        let stream = "0.2 0.4 0.8 rg 72 420 468 200 re f";
        objects.push(format!(
            "<< /Length {} >>\nstream\n{stream}\nendstream",
            stream.len()
        ));

        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    #[cfg(feature = "whatsapp-web")]
    fn tool_on_path(program: &str) -> bool {
        std::env::var_os("PATH").is_some_and(|paths| {
            std::env::split_paths(&paths).any(|dir| dir.join(program).is_file())
        })
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn document_thumbnails_are_off_by_default() {
        assert!(!zeroclaw_config::schema::WhatsAppConfig::default().document_thumbnails);
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn jpeg_dimensions_reads_the_start_of_frame() {
        assert_eq!(
            jpeg_dimensions(&jpeg_header(0xC0, 464, 600, false)),
            Some((464, 600))
        );
        assert_eq!(
            jpeg_dimensions(&jpeg_header(0xC2, 600, 338, false)),
            Some((600, 338)),
            "progressive JPEGs carry the size in SOF2"
        );
        assert_eq!(
            jpeg_dimensions(&jpeg_header(0xC0, 464, 600, true)),
            Some((464, 600)),
            "a DHT segment (0xC4) is not a frame header and must be skipped"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn jpeg_dimensions_rejects_malformed_input() {
        let full = jpeg_header(0xC0, 464, 600, false);
        for bytes in [
            &b""[..],
            &b"%PDF-1.4"[..],
            &full[..full.len() - 14],
            &jpeg_header(0xC0, 0, 600, false)[..],
        ] {
            assert_eq!(jpeg_dimensions(bytes), None);
        }
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn pdfinfo_page_count_reads_the_pages_line() {
        let stdout =
            "Producer:        example\nPages:           12\nPage size:       612 x 792 pts\n";
        assert_eq!(pdfinfo_page_count(stdout), Some(12));
        assert_eq!(pdfinfo_page_count("Producer: example\n"), None);
        assert_eq!(pdfinfo_page_count("Pages:           0\n"), None);
        assert_eq!(pdfinfo_page_count("Pages:           many\n"), None);
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn thumbnail_is_dropped_when_empty_oversized_or_unreadable() {
        let jpeg = jpeg_header(0xC0, 75, 96, false);
        let thumbnail =
            DocumentThumbnail::from_jpeg(jpeg.clone(), DOCUMENT_PREVIEW_INLINE_MAX_BYTES)
                .expect("valid JPEG");
        assert_eq!((thumbnail.width, thumbnail.height), (75, 96));

        assert!(
            DocumentThumbnail::from_jpeg(Vec::new(), DOCUMENT_PREVIEW_INLINE_MAX_BYTES).is_none()
        );
        assert!(
            DocumentThumbnail::from_jpeg(b"not a jpeg".to_vec(), DOCUMENT_PREVIEW_INLINE_MAX_BYTES)
                .is_none()
        );
        let mut oversized = jpeg;
        oversized.resize(DOCUMENT_PREVIEW_INLINE_MAX_BYTES + 1, 0);
        assert!(
            DocumentThumbnail::from_jpeg(oversized, DOCUMENT_PREVIEW_INLINE_MAX_BYTES).is_none()
        );
    }

    /// The failure this guards is the one that does not look like a failure:
    /// a media host that accepts the connection and then says nothing. An
    /// `await` deadline around a request cannot end it, so the deadline has to
    /// belong to the transport. What the caller must see is an error, soon,
    /// after which the document goes out with the inline preview it already
    /// has — the fallback `inline_only_preview_fills_the_card_with_its_own_size`
    /// covers.
    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn a_silent_thumbnail_host_ends_the_request_rather_than_hanging() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let host = listener.local_addr().expect("addr").to_string();
        // Take the request in full and then say nothing: the connection is
        // open, the upload was accepted, and no status line ever comes. That is
        // the shape the report names, and the one an `await` deadline around a
        // blocking client cannot end.
        let silent = ::zeroclaw_spawn::spawn!(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut scratch = [0u8; 1024];
            let _read = tokio::io::AsyncReadExt::read(&mut socket, &mut scratch).await;
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        });

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(500))
            .connect_timeout(std::time::Duration::from_millis(500))
            .build()
            .expect("client");

        let started = std::time::Instant::now();
        let result = post_document_thumbnail(
            &http,
            &[wacore::net::HttpRequest::post(format!(
                "http://{host}/upload"
            ))],
            bytes::Bytes::from_static(b"encrypted thumbnail"),
        )
        .await;
        let elapsed = started.elapsed();
        silent.abort();

        assert!(
            result.is_err(),
            "a host that never answers cannot report a direct_path"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "the request ended on its own deadline rather than outliving the send, in {elapsed:?}"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn inline_only_preview_fills_the_card_with_its_own_size() {
        let jpeg = jpeg_header(0xC2, 75, 96, false);
        let mut document = waproto::whatsapp::message::DocumentMessage::default();
        DocumentPreview {
            inline: DocumentThumbnail::from_jpeg(jpeg.clone(), DOCUMENT_PREVIEW_INLINE_MAX_BYTES),
            uploaded: None,
            page_count: Some(3),
        }
        .apply_to(&mut document);
        assert_eq!(document.jpeg_thumbnail.as_deref(), Some(&jpeg[..]));
        assert_eq!(document.thumbnail_width, Some(75));
        assert_eq!(document.thumbnail_height, Some(96));
        assert_eq!(document.page_count, Some(3));
        assert_eq!(document.thumbnail_direct_path, None);

        let mut untouched = waproto::whatsapp::message::DocumentMessage::default();
        DocumentPreview::default().apply_to(&mut untouched);
        assert_eq!(
            untouched,
            waproto::whatsapp::message::DocumentMessage::default(),
            "no preview must leave the card exactly as it was"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn uploaded_preview_is_referenced_and_sets_the_card_size() {
        let jpeg = jpeg_header(0xC2, 75, 96, false);
        let mut document = waproto::whatsapp::message::DocumentMessage::default();
        DocumentPreview {
            inline: DocumentThumbnail::from_jpeg(jpeg.clone(), DOCUMENT_PREVIEW_INLINE_MAX_BYTES),
            uploaded: Some(UploadedThumbnail {
                direct_path: "/v/t62.example/thumbnail.enc".into(),
                sha256: [1; 32],
                enc_sha256: [2; 32],
                width: 371,
                height: 480,
            }),
            page_count: Some(3),
        }
        .apply_to(&mut document);
        assert_eq!(document.jpeg_thumbnail.as_deref(), Some(&jpeg[..]));
        assert_eq!(
            document.thumbnail_direct_path.as_deref(),
            Some("/v/t62.example/thumbnail.enc")
        );
        assert_eq!(document.thumbnail_sha256, Some(vec![1; 32]));
        assert_eq!(document.thumbnail_enc_sha256, Some(vec![2; 32]));
        assert_eq!(
            (document.thumbnail_width, document.thumbnail_height),
            (Some(371), Some(480)),
            "the card size is that of the uploaded preview"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn media_blob_encryption_matches_the_library_scheme() {
        // Link thumbnails are a type the library encrypts itself, so the two
        // outputs must agree byte for byte under the same key and info.
        let key = [7u8; 32];
        let plaintext = jpeg_header(0xC0, 371, 480, false);
        let ours = encrypt_media_blob(&key, b"WhatsApp Link Thumbnail Keys", &plaintext)
            .expect("encrypts");
        let library = wacore::upload::encrypt_media_with_key(
            &plaintext,
            wacore::download::MediaType::LinkThumbnail,
            Some(&key),
        )
        .expect("library encrypts");
        assert_eq!(ours.data, library.data_to_upload);
        assert_eq!(ours.sha256, library.file_sha256);
        assert_eq!(ours.enc_sha256, library.file_enc_sha256);
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn document_thumbnail_is_not_encrypted_with_the_document_keys() {
        let key = [7u8; 32];
        let plaintext = jpeg_header(0xC0, 371, 480, false);
        let blob =
            encrypt_media_blob(&key, DOCUMENT_THUMBNAIL_KEY_INFO, &plaintext).expect("encrypts");
        assert!(
            wacore::download::DownloadUtils::verify_and_decrypt(
                &blob.data,
                &key,
                wacore::download::MediaType::Document
            )
            .is_err(),
            "phones reject a preview encrypted with the document's own keys"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn document_thumbnail_upload_goes_to_the_thumbnail_path() {
        let request = document_thumbnail_upload_request("media.example.net", "auth-1", "tok");
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.url,
            "https://media.example.net/mms/thumbnail-document/tok?auth=auth-1&token=tok"
        );
        assert_eq!(
            request.headers.get("Content-Type").map(String::as_str),
            Some("application/octet-stream")
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn upload_response_yields_the_direct_path() {
        assert_eq!(
            upload_response_direct_path(
                br#"{"url":"https://media.example.net/v/t62/x.enc","direct_path":"/v/t62/x.enc"}"#
            )
            .as_deref(),
            Some("/v/t62/x.enc")
        );
        assert_eq!(upload_response_direct_path(br#"{"url":"x"}"#), None);
        assert_eq!(upload_response_direct_path(br#"{"direct_path":""}"#), None);
        assert_eq!(upload_response_direct_path(b"<html>"), None);
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn markdown_inline_markers_collapse_to_whatsapp_syntax() {
        assert_eq!(markdown_to_whatsapp("**bold**"), "*bold*");
        assert_eq!(markdown_to_whatsapp("__underscored__"), "_underscored_");
        assert_eq!(markdown_to_whatsapp("~~gone~~"), "~gone~");
        assert_eq!(
            markdown_to_whatsapp("a **b** and ~~c~~ and __d__ end"),
            "a *b* and ~c~ and _d_ end"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn markdown_headings_become_bold_lines() {
        assert_eq!(markdown_to_whatsapp("# Title"), "*Title*");
        assert_eq!(markdown_to_whatsapp("###### Deep"), "*Deep*");
        // Seven hashes is not a heading, and neither is a bare `#tag`.
        assert_eq!(markdown_to_whatsapp("####### Nope"), "####### Nope");
        assert_eq!(markdown_to_whatsapp("#tag"), "#tag");
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn markdown_links_become_text_then_url() {
        assert_eq!(
            markdown_to_whatsapp("see [the docs](https://example.com/x) now"),
            "see the docs: https://example.com/x now"
        );
        // A bare URL is left for WhatsApp to auto-link.
        assert_eq!(
            markdown_to_whatsapp("https://example.com/x"),
            "https://example.com/x"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_native_constructs_pass_through() {
        let native = "- first\n- second\n1. one\n2. two\n> quoted\n`inline code`";
        assert_eq!(markdown_to_whatsapp(native), native);
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn already_whatsapp_styled_text_is_unchanged() {
        let styled = "*bold* and _italic_ and ~struck~ and `code`";
        assert_eq!(markdown_to_whatsapp(styled), styled);
        assert_eq!(
            markdown_to_whatsapp(&markdown_to_whatsapp("**bold** and __italic__")),
            "*bold* and _italic_"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn leading_list_marker_is_not_read_as_bold() {
        assert_eq!(markdown_to_whatsapp("* item one"), "* item one");
        assert_eq!(
            markdown_to_whatsapp("* item one\n* item two"),
            "* item one\n* item two"
        );
        assert_eq!(markdown_to_whatsapp("* **hot** item"), "* *hot* item");
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn code_fences_keep_their_contents_and_lose_the_language_tag() {
        assert_eq!(
            markdown_to_whatsapp("```rust\nlet x = **y**;\n```"),
            "```\nlet x = **y**;\n```"
        );
        assert_eq!(
            markdown_to_whatsapp("```\n# not a heading\n[a](b)\n```"),
            "```\n# not a heading\n[a](b)\n```"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn mixed_hebrew_and_english_gains_no_direction_marks() {
        let input = "## סיכום Summary\n**חשוב**: ראה [the docs](https://example.com) עכשיו";
        let rendered = markdown_to_whatsapp(input);
        assert_eq!(
            rendered,
            "*סיכום Summary*\n*חשוב*: ראה the docs: https://example.com עכשיו"
        );
        assert!(!rendered.contains('\u{200f}'));
        assert!(!rendered.contains('\u{200e}'));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn multi_line_message_converts_heading_list_and_bold() {
        let input = "# Report\n\nThe **build** is green.\n\n- run `cargo test`\n- read [the log](https://ci.example.com/1)\n";
        assert_eq!(
            markdown_to_whatsapp(input),
            "*Report*\n\nThe *build* is green.\n\n- run `cargo test`\n- read the log: https://ci.example.com/1\n"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn link_destination_boundaries_follow_commonmark() {
        // An angle-bracket destination may contain spaces.
        assert_eq!(
            markdown_to_whatsapp("[docs](<https://example.test/a b>)"),
            "docs: https://example.test/a b"
        );
        // An escaped parenthesis belongs to the destination.
        assert_eq!(
            markdown_to_whatsapp("[docs](https://example.test/a\\)b)"),
            "docs: https://example.test/a)b"
        );
        assert_eq!(
            markdown_to_whatsapp("[docs](<https://example.test/a\\>b>)"),
            "docs: https://example.test/a>b"
        );
        // A title in any of its three forms is dropped, not sent as URL.
        assert_eq!(
            markdown_to_whatsapp("[docs](https://example.test/x \"Title\")"),
            "docs: https://example.test/x"
        );
        assert_eq!(
            markdown_to_whatsapp("[docs](https://example.test/x 'Title')"),
            "docs: https://example.test/x"
        );
        assert_eq!(
            markdown_to_whatsapp("[docs](https://example.test/x (Title))"),
            "docs: https://example.test/x"
        );
        // Not a CommonMark link: an unclosed angle bracket or a bare
        // destination with a space stays literal.
        assert_eq!(
            markdown_to_whatsapp("[docs](<https://example.test/a b)"),
            "[docs](<https://example.test/a b)"
        );
        assert_eq!(
            markdown_to_whatsapp("[docs](https://example.test/a b)"),
            "[docs](https://example.test/a b)"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn pdftoppm_renders_page_one_at_the_requested_size() {
        let path = Path::new("/tmp/example.pdf");
        let inline = pdftoppm_args(path, DOCUMENT_PREVIEW_INLINE_SIDE, true);
        let upload = pdftoppm_args(path, DOCUMENT_PREVIEW_UPLOAD_SIDE, false);
        let as_strs = |args: &[std::ffi::OsString]| {
            args.iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        let inline = as_strs(&inline);
        let upload = as_strs(&upload);
        for args in [&inline, &upload] {
            assert_eq!(&args[..4], ["-f", "1", "-l", "1"]);
            assert_eq!(args.last().map(String::as_str), Some("/tmp/example.pdf"));
        }
        assert!(inline.contains(&"96".to_string()));
        assert!(inline.contains(&"quality=70,progressive=y".to_string()));
        assert!(upload.contains(&"480".to_string()));
        assert!(upload.contains(&"quality=75".to_string()));
    }

    #[tokio::test]
    #[cfg(all(feature = "whatsapp-web", unix))]
    async fn preview_tool_failures_are_reported_not_raised() {
        let short = std::time::Duration::from_millis(200);

        let missing = run_preview_tool("zeroclaw-no-such-preview-tool", &[], short)
            .await
            .expect_err("a missing tool is an error value");
        assert!(missing.contains("could not start"), "{missing}");

        let failed = run_preview_tool("false", &[], short)
            .await
            .expect_err("a non-zero exit is an error value");
        assert!(failed.contains("exited"), "{failed}");

        let started = std::time::Instant::now();
        let slow = run_preview_tool("sleep", &["5".into()], short)
            .await
            .expect_err("a slow tool is cut off");
        assert!(slow.contains("timed out"), "{slow}");
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn preview_of_an_unreadable_pdf_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("broken.pdf");
        std::fs::write(&path, b"not a pdf").expect("write");
        assert_eq!(render_pdf_preview(&path).await, RenderedPreview::default());
    }

    /// Runs only where poppler-utils is installed; elsewhere there is nothing
    /// to render with and the other tests cover the fallback.
    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn preview_renders_the_first_page_when_poppler_is_installed() {
        if !(tool_on_path("pdftoppm") && tool_on_path("pdfinfo")) {
            eprintln!("skipping: pdftoppm/pdfinfo not on PATH");
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("example-spec-sheet.pdf");
        std::fs::write(&path, three_page_pdf()).expect("write");

        let preview = render_pdf_preview(&path).await;
        assert_eq!(preview.page_count, Some(3));
        for (thumbnail, side) in [
            (preview.inline, DOCUMENT_PREVIEW_INLINE_SIDE),
            (preview.upload, DOCUMENT_PREVIEW_UPLOAD_SIDE),
        ] {
            let thumbnail = thumbnail.expect("page 1 is rendered");
            assert_eq!(thumbnail.width.max(thumbnail.height), side);
            assert!(
                thumbnail.width < thumbnail.height,
                "letter pages are portrait"
            );
        }
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn bare_url_bytes_survive_unchanged() {
        let url = "https://example.test/a__b__c";
        assert_eq!(markdown_to_whatsapp(url), url);
        assert_eq!(
            markdown_to_whatsapp("see https://example.test/a__b__c now"),
            "see https://example.test/a__b__c now"
        );
        // Trailing punctuation and an unbalanced `)` belong to the sentence,
        // not the URL, so the markers after a URL still convert.
        assert_eq!(
            markdown_to_whatsapp("(see https://example.test/x_(y)_z). **ok**"),
            "(see https://example.test/x_(y)_z). *ok*"
        );
        assert_eq!(
            markdown_to_whatsapp("**https://example.test/a~~b~~c**"),
            "*https://example.test/a~~b~~c*"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn only_enabled_pdf_documents_get_a_preview() {
        assert!(wants_document_preview(
            true,
            WhatsAppMediaKind::Document,
            "application/pdf"
        ));
        assert!(!wants_document_preview(
            false,
            WhatsAppMediaKind::Document,
            "application/pdf"
        ));
        assert!(!wants_document_preview(
            true,
            WhatsAppMediaKind::Document,
            "application/msword"
        ));
        assert!(!wants_document_preview(
            true,
            WhatsAppMediaKind::Image,
            "application/pdf"
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn link_destination_keeps_balanced_parentheses() {
        assert_eq!(
            markdown_to_whatsapp("[docs](https://example.test/a_(b)/c)"),
            "docs: https://example.test/a_(b)/c"
        );
        assert_eq!(
            markdown_to_whatsapp("[docs](https://example.test/a__b \"title\") tail"),
            "docs: https://example.test/a__b tail"
        );
        // An unbalanced destination is not a link, and the URL still passes whole.
        assert_eq!(
            markdown_to_whatsapp("[docs](https://example.test/a_(b"),
            "[docs](https://example.test/a_(b"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn channel_takes_document_thumbnails_from_config() {
        for enabled in [false, true] {
            let cfg = zeroclaw_config::schema::WhatsAppConfig {
                enabled: true,
                session_path: Some("/tmp/test-whatsapp.db".into()),
                document_thumbnails: enabled,
                ..Default::default()
            };
            let ch = WhatsAppWebChannel::new(&cfg, "alias", Arc::new(Vec::new), Arc::new(Vec::new));
            assert_eq!(ch.document_thumbnails, enabled);
        }
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn code_span_closes_only_on_a_matching_backtick_run() {
        assert_eq!(markdown_to_whatsapp("``a **b**``"), "``a **b**``");
        assert_eq!(
            markdown_to_whatsapp("``has ` inside`` and **bold**"),
            "``has ` inside`` and *bold*"
        );
        // An unmatched run is literal text and does not swallow the line.
        assert_eq!(markdown_to_whatsapp("``open **bold**"), "``open *bold*");
        assert_eq!(markdown_to_whatsapp("```mono __x__```"), "```mono __x__```");
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn longer_fence_encloses_a_triple_backtick_example() {
        let input = "````\n```rust\nlet x = **y**;\n```\n[a](b)\n````\n**after**";
        assert_eq!(
            markdown_to_whatsapp(input),
            "````\n```rust\nlet x = **y**;\n```\n[a](b)\n````\n*after*"
        );
        // A fence line carrying an info string never closes a block.
        assert_eq!(
            markdown_to_whatsapp("```\n```rust\n**x**\n```\n**y**"),
            "```\n```rust\n**x**\n```\n*y*"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn bold_heading_is_wrapped_once_and_stays_put() {
        assert_eq!(markdown_to_whatsapp("# **Title**"), "*Title*");
        assert_eq!(
            markdown_to_whatsapp(&markdown_to_whatsapp("# **Title**")),
            "*Title*"
        );
        // Bold inside a heading is redundant: the whole line is already bold.
        assert_eq!(markdown_to_whatsapp("## Plain **part**"), "*Plain part*");
        assert_eq!(markdown_to_whatsapp("## `**` and __u__"), "*`**` and _u_*");
    }
}
