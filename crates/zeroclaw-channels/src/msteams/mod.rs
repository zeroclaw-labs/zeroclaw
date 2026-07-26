//! Microsoft Teams bot channel (Azure Bot Service / Bot Framework).
//!
//! Inbound: Teams POSTs Bot Framework activities to a channel-hosted axum
//! listener (the operator registers its public URL as the Azure Bot
//! messaging endpoint); every request is JWT-validated against the Bot
//! Framework JWKS before the body is touched. Outbound: proactive POSTs to
//! the Bot Connector API at the `service_url` carried by each inbound
//! activity, authenticated with a cached Entra client-credentials token.
//!
//! Streaming (`stream_mode = "partial"`) drives Teams' native streaming
//! protocol in personal chats — the gray in-progress bubble fed by
//! `streaminfo` typing activities, replaced by the final message. The
//! stream opens lazily on the first real status line or content chunk
//! (mirroring OpenClaw's `HttpStream`), so no placeholder frame is ever
//! posted. Group chats and team channels don't open drafts at all: they
//! show the ordinary typing indicator and receive one final reply.
//!
//! Design: `docs/msteams-channel-design.md`.

pub mod activity;
pub mod auth;
pub mod conversation;

use activity::Activity;
use anyhow::{Context, Result};
use async_trait::async_trait;
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    routing::post,
};
use conversation::{ConversationReference, ConversationStore};
use portable_atomic::{AtomicBool, AtomicU64, Ordering};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_config::schema::{MSTeamsConfig, StreamMode};

/// Resolves this alias's `MSTeamsConfig` from canonical config state at
/// use-time. No snapshot is stored on the channel (see AGENTS.md
/// "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH"): credentials, `allow_dms`,
/// and `mention_only` are all read through this resolver so a config
/// reload is observed on the next message.
pub type ConfigResolver = Arc<dyn Fn() -> Option<MSTeamsConfig> + Send + Sync>;

/// Resolves inbound external peers from canonical `peer_groups` state at
/// message-time.
pub type PeerResolver = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// The bot's own identity on Teams, learned from `activity.recipient` on
/// the first inbound activity (the platform is its source of truth; it
/// exists nowhere in config).
#[derive(Debug, Clone)]
struct BotIdentity {
    id: String,
    name: Option<String>,
}

/// Connector token provider bound to the tenant it was built for.
/// Rebuilt when the canonical `tenant_id` changes on config reload — a
/// materialized view keyed on config state, not a cached copy of it.
struct ConnectorHandle {
    tenant_id: String,
    provider: Arc<auth::ConnectorTokenProvider>,
}

/// Resolved per-call context for outbound Connector requests.
struct SendContext {
    reference: ConversationReference,
    base_id: String,
    client: reqwest::Client,
    token: String,
}

/// Per-draft native-streaming state. Source of truth created here: the
/// streaminfo sequence counter and the Teams-assigned `streamId` exist
/// nowhere else and are dropped on finalize/cancel.
struct DraftStream {
    /// Teams `streamId` — the Connector-assigned id of the first
    /// activity. `None` while the draft is lazily pending (no activity
    /// has been POSTed yet).
    stream_id: Option<String>,
    /// Next `streamSequence` (starts at 1, monotonic per stream).
    next_sequence: u64,
}

/// Per-recipient `multi_message` streaming state. Source of truth created
/// here: how many bytes of the accumulated response have already been
/// flushed as separate messages, and the thread anchor replies address.
/// Dropped on finalize/cancel.
struct MultiMessageState {
    /// Byte offset into the accumulated response already delivered.
    sent_len: usize,
    /// Thread anchor (`thread_ts`) carried from the triggering message so
    /// team-channel paragraphs stay in-thread.
    thread_ts: Option<String>,
}

/// A `typing`/`message` activity carrying a Teams `streaminfo` entity
/// (the native streaming protocol; design §4).
fn streaming_activity_body(
    activity_type: &str,
    text: &str,
    stream_type: &str,
    sequence: Option<u64>,
    stream_id: Option<&str>,
) -> serde_json::Value {
    let mut entity = serde_json::json!({
        "type": "streaminfo",
        "streamType": stream_type,
    });
    if let Some(sequence) = sequence {
        entity["streamSequence"] = serde_json::Value::from(sequence);
    }
    if let Some(stream_id) = stream_id {
        entity["streamId"] = serde_json::Value::from(stream_id);
    }
    serde_json::json!({
        "type": activity_type,
        "text": text,
        "entities": [entity],
    })
}

/// Find the first paragraph break (a blank line, `\n\n`) that is not
/// inside a fenced code block, for `multi_message` splitting. Returns the
/// trimmed paragraph text before the break and the number of bytes
/// consumed (the paragraph plus its trailing blank line). `None` when no
/// complete paragraph boundary has arrived yet, so the caller waits for
/// more streamed text rather than splitting mid-paragraph or mid-fence.
fn next_paragraph_boundary(text: &str) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    let mut in_fence = false;
    let mut pos = 0;
    while pos < bytes.len() {
        let ch = bytes[pos];
        if ch == b'`'
            && pos + 2 < bytes.len()
            && bytes[pos + 1] == b'`'
            && bytes[pos + 2] == b'`'
            && (pos == 0 || bytes[pos - 1] == b'\n')
        {
            in_fence = !in_fence;
        }
        if !in_fence && ch == b'\n' && pos + 1 < bytes.len() && bytes[pos + 1] == b'\n' {
            return Some((text[..pos].trim().to_string(), pos + 2));
        }
        pos += 1;
    }
    None
}

/// Per-message size ceiling for outbound Teams activities, in characters.
///
/// Teams measures a message's size in UTF-16 code units — including
/// `@`-mentions and reactions — and rejects anything past ~100 KB with a
/// `413` (`MessageSizeTooBig`); Microsoft recommends staying under 80 KB. This
/// budget is deliberately conservative: even all-surrogate-pair text (2 UTF-16
/// units per `char`) stays well under the hard limit, leaving headroom for the
/// mention/reaction/JSON-envelope overhead the limit also counts.
const TEAMS_MAX_MESSAGE_CHARS: usize = 18_000;

/// Split `message` into ordered chunks that each stay within
/// [`TEAMS_MAX_MESSAGE_CHARS`]. Prefers to break at a paragraph boundary
/// (blank line), then a single newline, then a space, and only hard-cuts
/// mid-token when a single unbroken run exceeds the budget. Every character is
/// preserved (no trimming), so concatenating the chunks reproduces the input
/// exactly. Returns the input as a single chunk when it already fits, so the
/// common case is byte-for-byte identical to sending without splitting.
fn split_message_for_teams(message: &str) -> Vec<String> {
    if message.chars().count() <= TEAMS_MAX_MESSAGE_CHARS {
        return vec![message.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = message;
    while !remaining.is_empty() {
        if remaining.chars().count() <= TEAMS_MAX_MESSAGE_CHARS {
            chunks.push(remaining.to_string());
            break;
        }
        // Byte offset just past the budget-th character.
        let hard_split = remaining
            .char_indices()
            .nth(TEAMS_MAX_MESSAGE_CHARS)
            .map_or(remaining.len(), |(idx, _)| idx);
        let chunk_end = preferred_teams_split_end(&remaining[..hard_split]);
        chunks.push(remaining[..chunk_end].to_string());
        remaining = &remaining[chunk_end..];
    }
    chunks
}

/// Pick the byte offset to end a chunk within `search_area` (already trimmed to
/// the character budget). Prefers a paragraph break, then a newline, then a
/// space — but only when it leaves a non-trivial chunk (at least half the
/// budget), to avoid a cascade of tiny fragments. Falls back to a hard cut at
/// the budget. The result is always `>= 1`, so the caller always makes
/// progress.
fn preferred_teams_split_end(search_area: &str) -> usize {
    let min_keep = TEAMS_MAX_MESSAGE_CHARS / 2;
    let long_enough = |prefix: &str| prefix.chars().count() >= min_keep;

    if let Some(pos) = search_area.rfind("\n\n")
        && long_enough(&search_area[..pos])
    {
        return pos + 2;
    }
    if let Some(pos) = search_area.rfind('\n')
        && long_enough(&search_area[..pos])
    {
        return pos + 1;
    }
    if let Some(pos) = search_area.rfind(' ')
        && long_enough(&search_area[..pos])
    {
        return pos + 1;
    }
    search_area.len()
}

/// Microsoft Teams channel handle.
pub struct MsTeamsChannel {
    /// The alias key under `[channels.msteams.<alias>]` this handle is
    /// bound to.
    alias: String,
    /// Resolves the alias's config block from canonical state at use-time.
    config_resolver: ConfigResolver,
    /// Resolves inbound external peers from canonical state at message-time.
    peer_resolver: PeerResolver,
    validator: Arc<auth::JwtValidator>,
    conversations: Arc<ConversationStore>,
    bot_identity: Arc<OnceLock<BotIdentity>>,
    listener_ready: Arc<AtomicBool>,
    connector: tokio::sync::RwLock<Option<ConnectorHandle>>,
    /// Per-draft Teams streaming state, keyed by the locally assigned
    /// draft handle returned from `send_draft`. Source of truth created
    /// here — the handle, streaminfo sequence counter, and (once the
    /// stream opens) the Teams `streamId` exist nowhere else. Entries
    /// are removed on finalize/cancel.
    draft_streams: parking_lot::Mutex<HashMap<String, DraftStream>>,
    /// Monotonic source for locally assigned draft handles.
    draft_counter: AtomicU64,
    /// Last draft-update instant per recipient, enforcing the
    /// `draft_update_interval_ms` floor (Teams rate-limits streaming
    /// updates to roughly one per second).
    last_draft_update: parking_lot::Mutex<HashMap<String, Instant>>,
    /// Per-recipient `multi_message` progress (see [`MultiMessageState`]).
    multi_message: parking_lot::Mutex<HashMap<String, MultiMessageState>>,
    #[cfg(test)]
    token_url_override: Option<String>,
}

impl MsTeamsChannel {
    pub fn new(
        alias: impl Into<String>,
        config_resolver: ConfigResolver,
        peer_resolver: PeerResolver,
    ) -> Self {
        Self {
            alias: alias.into(),
            config_resolver,
            peer_resolver,
            validator: Arc::new(auth::JwtValidator::new(
                auth::BOT_FRAMEWORK_OPENID_METADATA_URL,
            )),
            conversations: Arc::new(ConversationStore::default()),
            bot_identity: Arc::new(OnceLock::new()),
            listener_ready: Arc::new(AtomicBool::new(false)),
            connector: tokio::sync::RwLock::new(None),
            draft_streams: parking_lot::Mutex::new(HashMap::new()),
            draft_counter: AtomicU64::new(0),
            last_draft_update: parking_lot::Mutex::new(HashMap::new()),
            multi_message: parking_lot::Mutex::new(HashMap::new()),
            #[cfg(test)]
            token_url_override: None,
        }
    }

    /// Test hook: validate inbound JWTs against a mock OpenID/JWKS server.
    #[cfg(test)]
    fn with_openid_metadata_url(mut self, url: impl Into<String>) -> Self {
        self.validator = Arc::new(auth::JwtValidator::new(url.into()));
        self
    }

    /// Test hook: acquire connector tokens from a mock Entra endpoint.
    #[cfg(test)]
    fn with_token_url(mut self, url: impl Into<String>) -> Self {
        self.token_url_override = Some(url.into());
        self
    }

    /// Current config for this alias, resolved from canonical state.
    fn config(&self) -> Option<MSTeamsConfig> {
        (self.config_resolver)()
    }

    fn http_client(&self, proxy_url: Option<&str>) -> reqwest::Client {
        zeroclaw_config::schema::build_channel_proxy_client_with_timeouts(
            "channel.msteams",
            proxy_url,
            30,
            10,
        )
    }

    /// Token provider for the current tenant, rebuilt if `tenant_id`
    /// changed since the last send.
    async fn connector_provider(&self, tenant_id: &str) -> Arc<auth::ConnectorTokenProvider> {
        {
            let guard = self.connector.read().await;
            if let Some(handle) = guard.as_ref()
                && handle.tenant_id == tenant_id
            {
                return handle.provider.clone();
            }
        }
        let mut guard = self.connector.write().await;
        if let Some(handle) = guard.as_ref()
            && handle.tenant_id == tenant_id
        {
            return handle.provider.clone();
        }
        #[cfg(test)]
        let token_url = self
            .token_url_override
            .clone()
            .unwrap_or_else(|| auth::connector_token_url(tenant_id));
        #[cfg(not(test))]
        let token_url = auth::connector_token_url(tenant_id);
        let provider = Arc::new(auth::ConnectorTokenProvider::new(token_url));
        *guard = Some(ConnectorHandle {
            tenant_id: tenant_id.to_string(),
            provider: provider.clone(),
        });
        provider
    }

    /// Current stream mode, resolved from canonical state.
    fn stream_mode(&self) -> StreamMode {
        self.config().map(|cfg| cfg.stream_mode).unwrap_or_default()
    }

    /// Resolve everything an outbound Connector call needs for
    /// `recipient`: the stored conversation reference, an authenticated
    /// client, and a bearer token.
    async fn send_context(&self, recipient: &str) -> Result<(MSTeamsConfig, SendContext)> {
        let cfg = self.config().with_context(|| {
            format!(
                "Microsoft Teams channel '{}' has no [channels.msteams.{}] config block",
                self.alias, self.alias
            )
        })?;
        let (base_id, _) = activity::split_conversation_id(recipient);
        let reference = self.conversations.get(base_id).with_context(|| {
            format!(
                "no conversation reference for '{base_id}': references are in-memory only, \
                 so the peer must message the bot (again) after a daemon restart before \
                 proactive sends can reach them"
            )
        })?;
        let provider = self.connector_provider(&cfg.tenant_id).await;
        let token = provider.token(&cfg.app_id, &cfg.app_password).await?;
        let client = self.http_client(cfg.proxy_url.as_deref());
        let base_id = base_id.to_string();
        Ok((
            cfg,
            SendContext {
                reference,
                base_id,
                client,
                token,
            },
        ))
    }

    /// Address a thread only for channel conversations. Teams includes
    /// `;messageid=` in a personal conversation id too, but Connector rejects
    /// that form outside a channel conversation.
    fn conversation_id_for_thread(ctx: &SendContext, thread_ts: Option<&str>) -> String {
        match (
            ctx.reference.conversation_type.as_deref(),
            thread_ts.filter(|thread_id| !thread_id.is_empty()),
        ) {
            (Some("channel"), Some(thread_id)) => {
                format!("{};messageid={thread_id}", ctx.base_id)
            }
            _ => ctx.base_id.clone(),
        }
    }

    /// `{service_url}/v3/conversations/{conversation_id}/activities[/{activity_id}]`.
    fn activities_url(
        reference: &ConversationReference,
        conversation_id: &str,
        activity_id: Option<&str>,
    ) -> Result<url::Url> {
        let mut url = url::Url::parse(&reference.service_url)
            .with_context(|| format!("invalid service_url '{}'", reference.service_url))?;
        {
            let mut segments = url.path_segments_mut().map_err(|()| {
                anyhow::Error::msg(format!(
                    "service_url '{}' cannot be a base",
                    reference.service_url
                ))
            })?;
            segments
                .pop_if_empty()
                .extend(["v3", "conversations", conversation_id, "activities"]);
            if let Some(id) = activity_id {
                segments.push(id);
            }
        }
        Ok(url)
    }

    /// Issue a Connector API request; returns the activity id from the
    /// response body when the Connector provides one.
    async fn activity_request(
        ctx: &SendContext,
        method: reqwest::Method,
        url: url::Url,
        body: &serde_json::Value,
    ) -> Result<Option<String>> {
        let response = ctx
            .client
            .request(method, url)
            .bearer_auth(&ctx.token)
            .json(body)
            .send()
            .await
            .context("Teams Connector request failed")?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("Teams Connector request failed ({status}): {text}");
        }
        Ok(serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|value| {
                value
                    .get("id")
                    .and_then(|id| id.as_str().map(str::to_string))
            }))
    }

    /// Whether the `draft_update_interval_ms` floor allows an update for
    /// this recipient right now (`0` disables throttling).
    fn draft_update_allowed(&self, recipient: &str, interval_ms: u64) -> bool {
        if interval_ms == 0 {
            return true;
        }
        self.last_draft_update
            .lock()
            .get(recipient)
            .is_none_or(|last| last.elapsed().as_millis() >= u128::from(interval_ms))
    }

    fn mark_draft_update(&self, recipient: &str) {
        self.last_draft_update
            .lock()
            .insert(recipient.to_string(), Instant::now());
    }

    /// Drop all local state for a draft (finalized or cancelled),
    /// returning the Teams `streamId` if the stream ever opened.
    fn clear_draft_state(&self, recipient: &str, draft_id: &str) -> Option<String> {
        let removed = self.draft_streams.lock().remove(draft_id);
        self.last_draft_update.lock().remove(recipient);
        removed.and_then(|draft| draft.stream_id)
    }

    /// `multi_message` streaming: flush every paragraph of `text` that has
    /// fully arrived (past the per-recipient sent offset) as its own Teams
    /// message, pausing `multi_message_delay_ms` between sends. Non-fatal
    /// send failures are logged and swallowed — the finalize pass carries
    /// whatever remains.
    async fn flush_multi_message_paragraphs(&self, recipient: &str, text: &str) {
        let delay_ms = self
            .config()
            .map(|cfg| cfg.multi_message_delay_ms)
            .unwrap_or(0);
        loop {
            let (paragraph, thread_ts) = {
                let mut state = self.multi_message.lock();
                let Some(entry) = state.get_mut(recipient) else {
                    return;
                };
                // A shorter accumulation than we have already delivered
                // means the stream was cleared/restarted; reset and wait.
                if text.len() < entry.sent_len {
                    entry.sent_len = 0;
                    return;
                }
                match next_paragraph_boundary(&text[entry.sent_len..]) {
                    Some((paragraph, consumed)) => {
                        entry.sent_len += consumed;
                        (paragraph, entry.thread_ts.clone())
                    }
                    None => return,
                }
            };
            if paragraph.is_empty() {
                // Blank paragraph (e.g. consecutive blank lines); the
                // offset advanced, so keep scanning without a send.
                continue;
            }
            let msg = SendMessage::new(&paragraph, recipient).in_thread(thread_ts);
            if let Err(err) = self.send(&msg).await {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"error": format!("{err}")})),
                    "Teams multi-message paragraph send failed"
                );
            }
            if delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
        }
    }

    /// `multi_message` finalize: flush any complete paragraphs, then send
    /// the trailing text (which has no closing blank line) as a last
    /// message. Drops the per-recipient state.
    async fn finalize_multi_message(&self, recipient: &str, text: &str) -> Result<()> {
        self.flush_multi_message_paragraphs(recipient, text).await;
        let (remaining, thread_ts) = {
            let mut state = self.multi_message.lock();
            match state.remove(recipient) {
                Some(entry) => {
                    let tail = if text.len() > entry.sent_len {
                        text[entry.sent_len..].trim().to_string()
                    } else {
                        String::new()
                    };
                    (tail, entry.thread_ts)
                }
                None => (String::new(), None),
            }
        };
        if !remaining.is_empty() {
            let msg = SendMessage::new(&remaining, recipient).in_thread(thread_ts);
            self.send(&msg).await?;
        }
        Ok(())
    }

    /// POST one streaminfo activity for a draft, opening the Teams
    /// stream on the first call. The first activity carries real
    /// content — never a placeholder — mirroring OpenClaw's lazy
    /// `HttpStream`, so the gray bubble's first visible frame is actual
    /// status or response text. The sequence counter (and, on open, the
    /// Teams-assigned `streamId`) is committed only after the request
    /// succeeds, so a failed open retries as sequence 1.
    async fn push_stream_activity(
        &self,
        recipient: &str,
        draft_id: &str,
        text: &str,
        stream_type: &str,
    ) -> Result<()> {
        let Some((sequence, stream_id)) = self
            .draft_streams
            .lock()
            .get(draft_id)
            .map(|draft| (draft.next_sequence, draft.stream_id.clone()))
        else {
            return Ok(());
        };
        let (_, ctx) = self.send_context(recipient).await?;
        let body = streaming_activity_body(
            "typing",
            text,
            stream_type,
            Some(sequence),
            stream_id.as_deref(),
        );
        let url = Self::activities_url(&ctx.reference, &ctx.base_id, None)?;
        let response_id = Self::activity_request(&ctx, reqwest::Method::POST, url, &body).await?;

        if let Some(draft) = self.draft_streams.lock().get_mut(draft_id) {
            if draft.stream_id.is_none() {
                draft.stream_id = Some(
                    response_id
                        .context("Teams streaming draft opened but no streamId was returned")?,
                );
            }
            draft.next_sequence = sequence + 1;
        }
        self.mark_draft_update(recipient);
        Ok(())
    }

    /// Build the inbound activity router. Split from `listen()` so tests
    /// can bind an ephemeral port around the same handler.
    fn router(&self, path: &str, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Router {
        let state = Arc::new(ListenerState {
            alias: self.alias.clone(),
            tx,
            config_resolver: self.config_resolver.clone(),
            peer_resolver: self.peer_resolver.clone(),
            validator: self.validator.clone(),
            conversations: self.conversations.clone(),
            bot_identity: self.bot_identity.clone(),
            counter: AtomicU64::new(0),
        });
        Router::new()
            .route(path, post(handle_activity))
            .with_state(state)
    }
}

struct ListenerState {
    alias: String,
    tx: tokio::sync::mpsc::Sender<ChannelMessage>,
    config_resolver: ConfigResolver,
    peer_resolver: PeerResolver,
    validator: Arc<auth::JwtValidator>,
    conversations: Arc<ConversationStore>,
    bot_identity: Arc<OnceLock<BotIdentity>>,
    counter: AtomicU64,
}

async fn handle_activity(
    State(state): State<Arc<ListenerState>>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let Some(cfg) = (state.config_resolver)() else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };

    // Authenticate before touching the body.
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(auth::bearer_token);
    let Some(token) = token else {
        return StatusCode::UNAUTHORIZED;
    };
    let issuers = auth::connector_issuers();
    let claims = match state.validator.validate(token, &cfg.app_id, &issuers).await {
        Ok(claims) => claims,
        Err(err) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{err}")})),
                "rejecting inbound Teams activity: JWT validation failed"
            );
            return StatusCode::UNAUTHORIZED;
        }
    };

    let mut activity: Activity = match serde_json::from_slice(&body) {
        Ok(a) => a,
        Err(err) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{err}")})),
                "invalid Teams activity payload"
            );
            return StatusCode::BAD_REQUEST;
        }
    };

    // Bind the activity to the signed token before any state is recorded
    // or any outbound request is made: the channelId must be endorsed by
    // the signing key, and the outbound serviceUrl must match the signed
    // claim (retaining only the validated value). A replayed valid token
    // with a tampered body is rejected here, so the bot's Connector token
    // can never be attached to an attacker-chosen serviceUrl.
    if let Err(reason) = bind_activity_to_claims(&mut activity, &claims) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"reason": reason})),
            "rejecting inbound Teams activity: token/body binding failed"
        );
        return StatusCode::UNAUTHORIZED;
    }

    process_activity(&state, &cfg, activity).await
}

/// Confirm the activity is bound to the token that authenticated it, and
/// pin the outbound `serviceUrl` to the signed value.
///
/// Two checks, both required by Microsoft's Bot Connector authentication
/// contract:
///
/// 1. The activity's `channelId` must appear in the signing key's
///    `endorsements` — the key must be published to sign for this channel.
/// 2. The activity's `serviceUrl` must match the signed `serviceurl`
///    claim. On success the activity keeps only the validated value, so
///    every downstream conversation reference and outbound Connector call
///    addresses the URL the issuer signed, never a body-supplied one.
fn bind_activity_to_claims(
    activity: &mut Activity,
    claims: &auth::ValidatedClaims,
) -> Result<(), &'static str> {
    let channel_id = activity
        .channel_id
        .as_deref()
        .ok_or("activity carries no channelId")?;
    if !claims.endorsements.iter().any(|e| e == channel_id) {
        return Err("activity channelId is not endorsed by the token's signing key");
    }

    let signed = claims
        .serviceurl
        .as_deref()
        .ok_or("service token carries no serviceUrl claim")?;
    match activity.service_url.as_deref() {
        Some(body_url) if service_url_matches(signed, body_url) => {}
        _ => return Err("activity serviceUrl does not match the signed serviceUrl claim"),
    }
    activity.service_url = Some(signed.to_string());
    Ok(())
}

/// Compare a signed `serviceUrl` claim against the activity's `serviceUrl`,
/// tolerating only a trailing-slash difference (both forms appear in
/// practice for the same Connector endpoint).
fn service_url_matches(signed: &str, activity: &str) -> bool {
    signed.trim_end_matches('/') == activity.trim_end_matches('/')
}

/// Everything after authentication: reference recording, gating, and
/// `ChannelMessage` construction. All drops return 200 so Teams does not
/// retry delivery.
async fn process_activity(
    state: &ListenerState,
    cfg: &MSTeamsConfig,
    activity: Activity,
) -> StatusCode {
    // Record the conversation reference on every activity type; proactive
    // sends need it even if this particular activity is gated below.
    if let (Some(service_url), Some(conversation)) = (&activity.service_url, &activity.conversation)
    {
        let (base_id, _) = activity::split_conversation_id(&conversation.id);
        state.conversations.record(ConversationReference {
            service_url: service_url.clone(),
            conversation_id: base_id.to_string(),
            conversation_type: conversation.conversation_type.clone(),
        });
    }
    if let Some(recipient) = &activity.recipient {
        let _ = state.bot_identity.set(BotIdentity {
            id: recipient.id.clone(),
            name: recipient.name.clone(),
        });
    }

    if activity.activity_type != "message" {
        return StatusCode::OK;
    }
    let Some(from) = &activity.from else {
        return StatusCode::OK;
    };

    // Self-loop guard: never react to the bot's own activities.
    if activity
        .recipient
        .as_ref()
        .is_some_and(|recipient| recipient.id == from.id)
    {
        return StatusCode::OK;
    }

    let personal = activity.is_personal();
    if personal && !cfg.allow_dms {
        return StatusCode::OK;
    }
    if !personal
        && cfg.mention_only.unwrap_or(true)
        && !activity
            .recipient
            .as_ref()
            .is_some_and(|recipient| activity.mentions(&recipient.id))
    {
        return StatusCode::OK;
    }

    // Sender allowlist: match the stable Entra object id when Teams
    // provides it, else the channel-scoped `29:` id. Empty list denies
    // everyone, `"*"` allows everyone (shared allowlist semantics).
    let peers = (state.peer_resolver)();
    let candidates = [from.aad_object_id.as_deref(), Some(from.id.as_str())];
    let allowed = candidates.into_iter().flatten().any(|candidate| {
        crate::allowlist::is_user_allowed(
            &peers,
            candidate,
            crate::allowlist::Match::CaseInsensitive,
        )
    });
    if !allowed {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"sender": from.id})),
            "dropping Teams message from sender outside peer allowlist"
        );
        return StatusCode::OK;
    }

    // Strip only the bot's own @mention; other mentioned users' names are
    // preserved so a prompt like "@Bot ask @Alice" keeps "Alice".
    let bot_mention_literals = activity
        .recipient
        .as_ref()
        .map(|recipient| activity.bot_mention_literals(&recipient.id))
        .unwrap_or_default();
    let text = activity
        .text
        .as_deref()
        .map(|raw| activity::clean_message_text(raw, &bot_mention_literals))
        .unwrap_or_default();
    if text.is_empty() {
        return StatusCode::OK;
    }

    let Some(conversation) = &activity.conversation else {
        return StatusCode::OK;
    };
    let is_team_channel = conversation.conversation_type.as_deref() == Some("channel");
    let (base_id, message_id_suffix) = activity::split_conversation_id(&conversation.id);
    // In team channels, reply in-thread: on the existing thread root when
    // the message came from one, else on the triggering message itself. Teams
    // may also append `;messageid=` to non-channel conversation IDs; it is
    // not a valid thread-addressing suffix there and sending it back produces
    // Connector's "Failed to decrypt conversation id" response.
    let thread_ts = is_team_channel
        .then(|| message_id_suffix.map(str::to_string))
        .flatten()
        .or_else(|| is_team_channel.then(|| activity.id.clone()).flatten());

    let seq = state.counter.fetch_add(1, Ordering::Relaxed);
    let explicitly_addressed = personal
        || activity
            .recipient
            .as_ref()
            .is_some_and(|recipient| activity.mentions(&recipient.id));

    let msg = ChannelMessage {
        channel_alias: Some(state.alias.clone()),
        thread_ts,
        interruption_scope_id: is_team_channel
            .then(|| message_id_suffix.map(str::to_string))
            .flatten(),
        explicitly_addressed,
        ..ChannelMessage::new(
            activity
                .id
                .clone()
                .unwrap_or_else(|| format!("msteams_{seq}")),
            from.aad_object_id
                .clone()
                .unwrap_or_else(|| from.id.clone()),
            base_id,
            text,
            "msteams",
            activity.timestamp_secs(),
        )
    };

    if state.tx.send(msg).await.is_err() {
        return StatusCode::SERVICE_UNAVAILABLE;
    }
    StatusCode::OK
}

impl ::zeroclaw_api::attribution::Attributable for MsTeamsChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::MsTeams,
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for MsTeamsChannel {
    fn name(&self) -> &str {
        "msteams"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let (_, ctx) = self.send_context(&message.recipient).await?;
        let conversation_id = Self::conversation_id_for_thread(&ctx, message.thread_ts.as_deref());
        let url = Self::activities_url(&ctx.reference, &conversation_id, None)?;
        // Teams rejects any single activity past ~100 KB (413
        // MessageSizeTooBig), so split oversize content into ordered chunks —
        // a long response then lands in full instead of failing outright. The
        // common (in-budget) case is a single chunk, unchanged from a plain send.
        for chunk in split_message_for_teams(&message.content) {
            let mut body = serde_json::json!({ "type": "message", "text": chunk });
            if let Some(reply_to_id) = message.in_reply_to.as_deref() {
                body["replyToId"] = serde_json::Value::String(reply_to_id.to_string());
            }
            Self::activity_request(&ctx, reqwest::Method::POST, url.clone(), &body).await?;
        }
        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        let cfg = self.config().with_context(|| {
            format!(
                "Microsoft Teams channel '{}' has no [channels.msteams.{}] config block",
                self.alias, self.alias
            )
        })?;
        if cfg.app_id.trim().is_empty() || cfg.tenant_id.trim().is_empty() {
            anyhow::bail!(
                "Microsoft Teams channel '{}' requires `app_id` and `tenant_id`: without \
                 them inbound activities cannot be authenticated; set them under \
                 [channels.msteams.{}]",
                self.alias,
                self.alias,
            );
        }

        let path = if cfg.path.starts_with('/') {
            cfg.path.clone()
        } else {
            format!("/{}", cfg.path)
        };
        let app = self.router(&path, tx);

        let addr = std::net::SocketAddr::from(([0, 0, 0, 0], cfg.port));
        let listener = tokio::net::TcpListener::bind(addr).await?;
        self.listener_ready.store(true, Ordering::Release);
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "Microsoft Teams channel listening on http://0.0.0.0:{}{path} ...",
                cfg.port
            )
        );

        axum::serve(listener, app)
            .await
            .map_err(|e| anyhow::Error::msg(format!("Teams activity listener error: {e}")))?;
        Ok(())
    }

    async fn health_check(&self) -> bool {
        self.listener_ready.load(Ordering::Acquire)
    }

    fn self_handle(&self) -> Option<String> {
        self.bot_identity.get().map(|identity| identity.id.clone())
    }

    fn self_addressed_mention(&self) -> Option<String> {
        self.bot_identity
            .get()
            .and_then(|identity| identity.name.clone())
            .map(|name| format!("<at>{name}</at>"))
    }

    fn is_direct_message(&self, msg: &ChannelMessage) -> bool {
        let (base_id, _) = activity::split_conversation_id(&msg.reply_target);
        self.conversations
            .get(base_id)
            .is_some_and(|reference| reference.is_personal())
    }

    /// Show a typing indicator by POSTing a one-shot Bot Framework
    /// `typing` activity. Teams auto-expires the indicator after a few
    /// seconds, so the orchestrator re-invokes this on its refresh
    /// interval for the duration of the turn. Personal-chat native
    /// streaming renders its own gray bubble and the orchestrator
    /// suppresses typing there; this covers group chats, team channels,
    /// non-streaming (`off`) turns, and `multi_message`.
    async fn start_typing(&self, recipient: &str) -> Result<()> {
        let (_, ctx) = self.send_context(recipient).await?;
        let conversation_id = Self::conversation_id_for_thread(&ctx, None);
        let url = Self::activities_url(&ctx.reference, &conversation_id, None)?;
        let body = serde_json::json!({ "type": "typing" });
        Self::activity_request(&ctx, reqwest::Method::POST, url, &body).await?;
        Ok(())
    }

    /// Teams has no explicit "stop typing" activity — the indicator
    /// expires shortly after the last `typing` activity — so this is a
    /// no-op beyond the trait contract.
    async fn stop_typing(&self, _recipient: &str) -> Result<()> {
        Ok(())
    }

    fn supports_draft_updates(&self) -> bool {
        self.stream_mode() != StreamMode::Off
    }

    fn supports_draft_updates_for(&self, message: &ChannelMessage) -> bool {
        match self.stream_mode() {
            StreamMode::Off => false,
            // Teams' native streaming (the gray bubble) is personal-chat
            // only; group chats and channels use a typing indicator plus
            // one final reply instead.
            StreamMode::Partial => self.is_direct_message(message),
            // Paragraph-split delivery is a sequence of ordinary sends, so
            // it works in every conversation type.
            StreamMode::MultiMessage => true,
        }
    }

    fn supports_multi_message_streaming(&self) -> bool {
        self.stream_mode() == StreamMode::MultiMessage
    }

    fn multi_message_delay_ms(&self) -> u64 {
        self.config()
            .map(|cfg| cfg.multi_message_delay_ms)
            .unwrap_or(0)
    }

    /// Open a streaming draft for the response.
    ///
    /// - `partial` (personal chats only): register a lazy native-streaming
    ///   draft. No activity is POSTed here — the placeholder the
    ///   orchestrator passes is dropped, and the Teams stream opens on the
    ///   first real update, so the gray bubble never flashes "..." (fast
    ///   answers skip the stream entirely). Group chats and team channels
    ///   don't open a partial draft: they use the typing indicator and
    ///   deliver one final reply.
    /// - `multi_message` (any conversation type): register per-recipient
    ///   paragraph-split state. Nothing is POSTed until the first complete
    ///   paragraph arrives via `update_draft`.
    async fn send_draft(&self, message: &SendMessage) -> Result<Option<String>> {
        match self.stream_mode() {
            StreamMode::Off => Ok(None),
            StreamMode::Partial => {
                // Personal-chat check straight from the in-memory
                // conversation store; no token acquisition or network
                // traffic happens until the stream actually opens.
                let (base_id, _) = activity::split_conversation_id(&message.recipient);
                if !self
                    .conversations
                    .get(base_id)
                    .is_some_and(|reference| reference.is_personal())
                {
                    return Ok(None);
                }
                let draft_id = format!(
                    "draft-{}",
                    self.draft_counter.fetch_add(1, Ordering::Relaxed)
                );
                self.draft_streams.lock().insert(
                    draft_id.clone(),
                    DraftStream {
                        stream_id: None,
                        next_sequence: 1,
                    },
                );
                Ok(Some(draft_id))
            }
            StreamMode::MultiMessage => {
                self.multi_message.lock().insert(
                    message.recipient.clone(),
                    MultiMessageState {
                        sent_len: 0,
                        thread_ts: message.thread_ts.clone(),
                    },
                );
                let draft_id = format!(
                    "multi-{}",
                    self.draft_counter.fetch_add(1, Ordering::Relaxed)
                );
                Ok(Some(draft_id))
            }
        }
    }

    /// Stream accumulated content into the draft. For `partial`, open/edit
    /// the Teams native stream on the first call; for `multi_message`,
    /// flush any newly completed paragraphs as separate messages. Non-fatal
    /// failures are logged and swallowed — the finalize pass carries the
    /// remaining text.
    async fn update_draft(&self, recipient: &str, message_id: &str, text: &str) -> Result<()> {
        if text.trim().is_empty() {
            return Ok(());
        }
        if self.stream_mode() == StreamMode::MultiMessage {
            self.flush_multi_message_paragraphs(recipient, text).await;
            return Ok(());
        }
        let Some(cfg) = self.config() else {
            return Ok(());
        };
        if !self.draft_update_allowed(recipient, cfg.draft_update_interval_ms) {
            return Ok(());
        }
        if let Err(err) = self
            .push_stream_activity(recipient, message_id, text, "streaming")
            .await
        {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"error": format!("{err}")})),
                "Teams draft update failed"
            );
        }
        Ok(())
    }

    /// Progress/status line (tool execution etc.), shown as the gray
    /// informative text over the streaming bubble. Opens the stream if
    /// this is the draft's first real content.
    async fn update_draft_progress(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> Result<()> {
        // Status lines belong to the native streaming bubble; in
        // `multi_message` mode there is no bubble, and delivering them as
        // standalone messages would spam the conversation, so skip them.
        if self.stream_mode() == StreamMode::MultiMessage {
            return Ok(());
        }
        let Some(cfg) = self.config() else {
            return Ok(());
        };
        if text.trim().is_empty() {
            return Ok(());
        }
        if !self.draft_update_allowed(recipient, cfg.draft_update_interval_ms) {
            return Ok(());
        }
        if let Err(err) = self
            .push_stream_activity(recipient, message_id, text, "informative")
            .await
        {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"error": format!("{err}")})),
                "Teams draft progress update failed"
            );
        }
        Ok(())
    }

    /// Close the draft with the complete response. If the stream opened,
    /// post the final `message` activity — Teams replaces the gray
    /// streaming bubble with a normal message and drops the status
    /// history. If it never opened (fast answer, no intermediate
    /// updates), deliver a plain message.
    async fn finalize_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
        _suppress_voice: bool,
    ) -> Result<()> {
        if self.stream_mode() == StreamMode::MultiMessage {
            return self.finalize_multi_message(recipient, text).await;
        }
        let (_, ctx) = self.send_context(recipient).await?;
        let text = crate::util::strip_tool_call_tags(text);
        let url = Self::activities_url(&ctx.reference, &ctx.base_id, None)?;

        let body = match self.clear_draft_state(recipient, message_id) {
            Some(stream_id) => {
                streaming_activity_body("message", &text, "final", None, Some(&stream_id))
            }
            None => serde_json::json!({ "type": "message", "text": text }),
        };
        Self::activity_request(&ctx, reqwest::Method::POST, url, &body).await?;
        Ok(())
    }

    /// Best-effort removal of an abandoned draft. Drafts whose stream
    /// never opened have nothing on the wire to delete; already-sent
    /// `multi_message` paragraphs stay delivered (Teams cannot recall
    /// them), so cancel just drops the per-recipient state.
    async fn cancel_draft(&self, recipient: &str, message_id: &str) -> Result<()> {
        if self.stream_mode() == StreamMode::MultiMessage {
            self.multi_message.lock().remove(recipient);
            return Ok(());
        }
        let Some(stream_id) = self.clear_draft_state(recipient, message_id) else {
            return Ok(());
        };
        let Ok((_, ctx)) = self.send_context(recipient).await else {
            return Ok(());
        };
        let url = Self::activities_url(&ctx.reference, &ctx.base_id, Some(&stream_id))?;
        let _ =
            Self::activity_request(&ctx, reqwest::Method::DELETE, url, &serde_json::Value::Null)
                .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{Algorithm, EncodingKey, Header};
    use serde::Serialize;
    use wiremock::matchers::{body_partial_json, header as header_matcher, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zeroclaw_api::attribution::Attributable;

    const APP_ID: &str = "00000000-aaaa-bbbb-cccc-000000000000";
    const TENANT_ID: &str = "00000000-1111-2222-3333-000000000000";
    const TEST_KID: &str = "listener-test-key";
    /// Base64url RSA modulus of `auth::TEST_KEY_PEM`'s public half.
    const TEST_KEY_N: &str = "xX2UGrUUorIz6usPOp1zydsNMyL9Uy93wWSwLpJUY6HkZFW17wGqGVsZB2Sp6oUt\
                              ESOKHdCpSYeujymfj-EHVuClStkXdzKx2HcRa4R4yT87qG5BUIxt3p6fWd_7exYe\
                              H4YOKf-LwUwJU4TPMxU-ephQY9CfTVB1bQZG3TmIiqSEgR7NHCEawaZOC2e-eUXw\
                              Nt27IC36dYun2NX89NN7O3Rr_oAsQKWIf3GtSNdtFLdKSa4LDeXu_sl0uhR7zMyv\
                              ncuYW7nTso4MmLosar3qCDKgsA-MjKVyQDEq0Qb22WIMjVmF68NSah6IilXmjoIL\
                              G2OCDnwGMmWFll6E9WYuAQ";

    /// The Connector base URL the test activities and the signed token
    /// agree on.
    const SERVICE_URL: &str = "https://smba.trafficmanager.net/teams/";

    #[derive(Serialize)]
    struct TestClaims {
        iss: String,
        aud: String,
        exp: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        serviceurl: Option<String>,
    }

    fn mint_service_token() -> String {
        mint_service_token_for(SERVICE_URL)
    }

    /// Mint a valid service token whose signed `serviceurl` claim is
    /// `service_url`, so binding tests can vary it independently of the
    /// activity body.
    fn mint_service_token_for(service_url: &str) -> String {
        mint_service_token_with(Some(service_url.to_string()))
    }

    /// Mint a valid service token that carries no `serviceurl` claim at all,
    /// so the binding path can be tested for a missing (not just mismatched)
    /// claim.
    fn mint_service_token_without_serviceurl() -> String {
        mint_service_token_with(None)
    }

    fn mint_service_token_with(service_url: Option<String>) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(TEST_KID.to_string());
        let claims = TestClaims {
            iss: auth::BOT_FRAMEWORK_ISSUER.to_string(),
            aud: APP_ID.to_string(),
            exp: chrono::Utc::now().timestamp() + 3600,
            serviceurl: service_url,
        };
        let key = EncodingKey::from_rsa_pem(auth::TEST_KEY_PEM.as_bytes()).unwrap();
        jsonwebtoken::encode(&header, &claims, &key).unwrap()
    }

    async fn mock_jwks(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/metadata"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": auth::BOT_FRAMEWORK_ISSUER,
                "jwks_uri": format!("{}/keys", server.uri()),
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/keys"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "keys": [{ "kty": "RSA", "use": "sig", "kid": TEST_KID, "n": TEST_KEY_N, "e": "AQAB", "endorsements": ["msteams"] }]
            })))
            .mount(server)
            .await;
    }

    fn test_config() -> MSTeamsConfig {
        MSTeamsConfig {
            enabled: true,
            app_id: APP_ID.to_string(),
            app_password: "test-secret".to_string(),
            tenant_id: TENANT_ID.to_string(),
            ..MSTeamsConfig::default()
        }
    }

    fn channel_with(
        config: MSTeamsConfig,
        peers: Vec<String>,
        auth_server: &MockServer,
    ) -> MsTeamsChannel {
        MsTeamsChannel::new(
            "default",
            Arc::new(move || Some(config.clone())),
            Arc::new(move || peers.clone()),
        )
        .with_openid_metadata_url(format!("{}/metadata", auth_server.uri()))
    }

    /// Bind the channel's router on an ephemeral port; returns the base
    /// URL and the inbound message receiver.
    async fn spawn_listener(
        channel: &MsTeamsChannel,
    ) -> (String, tokio::sync::mpsc::Receiver<ChannelMessage>) {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let app = channel.router("/api/messages", tx);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/api/messages"), rx)
    }

    fn personal_activity(text: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "message",
            "id": "1712345",
            "timestamp": "2026-07-18T02:00:00.000Z",
            "serviceUrl": SERVICE_URL,
            "channelId": "msteams",
            "from": { "id": "29:user-x", "name": "User X", "aadObjectId": "00000000-0000-0000-0000-00000000feed" },
            "recipient": { "id": "28:bot", "name": "ZeroClaw" },
            "conversation": { "id": "a:1conv", "conversationType": "personal" },
            "text": text,
        })
    }

    async fn post_activity(
        url: &str,
        token: &str,
        activity: &serde_json::Value,
    ) -> reqwest::StatusCode {
        reqwest::Client::new()
            .post(url)
            .bearer_auth(token)
            .json(activity)
            .send()
            .await
            .unwrap()
            .status()
    }

    #[test]
    fn name_and_attribution() {
        let ch = MsTeamsChannel::new(
            "default",
            Arc::new(|| Some(MSTeamsConfig::default())),
            Arc::new(Vec::new),
        );
        assert_eq!(ch.name(), "msteams");
        assert_eq!(Attributable::alias(&ch), "default");
        assert!(matches!(
            ch.role(),
            zeroclaw_api::attribution::Role::Channel(
                zeroclaw_api::attribution::ChannelKind::MsTeams
            )
        ));
    }

    #[tokio::test]
    async fn listen_requires_app_id_and_tenant() {
        let ch = MsTeamsChannel::new(
            "default",
            Arc::new(|| Some(MSTeamsConfig::default())),
            Arc::new(Vec::new),
        );
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let err = ch.listen(tx).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("requires `app_id` and `tenant_id`")
        );
        assert!(!ch.health_check().await);
    }

    #[tokio::test]
    async fn valid_personal_message_produces_channel_message() {
        let auth_server = MockServer::start().await;
        mock_jwks(&auth_server).await;
        let ch = channel_with(test_config(), vec!["*".to_string()], &auth_server);
        let (url, mut rx) = spawn_listener(&ch).await;

        let token = mint_service_token();
        // Teams pairs a bot `<at>` mention with a `mention` entity; the
        // bot's own mention is stripped, entities are decoded.
        let mut activity = personal_activity("<at>ZeroClaw</at> 1 &lt; 2");
        activity["entities"] = serde_json::json!([{
            "type": "mention",
            "mentioned": { "id": "28:bot", "name": "ZeroClaw" },
            "text": "<at>ZeroClaw</at>"
        }]);
        assert_eq!(post_activity(&url, &token, &activity).await, 200);

        let msg = rx.recv().await.unwrap();
        assert_eq!(msg.channel, "msteams");
        assert_eq!(msg.channel_alias.as_deref(), Some("default"));
        assert_eq!(msg.sender, "00000000-0000-0000-0000-00000000feed");
        assert_eq!(msg.reply_target, "a:1conv");
        assert_eq!(msg.content, "1 < 2");
        assert!(msg.explicitly_addressed);
        assert!(msg.thread_ts.is_none());

        // The activity recorded the conversation reference and identity.
        assert!(ch.is_direct_message(&msg));
        assert_eq!(ch.self_handle().as_deref(), Some("28:bot"));
        assert_eq!(
            ch.self_addressed_mention().as_deref(),
            Some("<at>ZeroClaw</at>")
        );
    }

    #[tokio::test]
    async fn missing_or_invalid_token_is_rejected() {
        let auth_server = MockServer::start().await;
        mock_jwks(&auth_server).await;
        let ch = channel_with(test_config(), vec!["*".to_string()], &auth_server);
        let (url, mut rx) = spawn_listener(&ch).await;
        let activity = personal_activity("hi");

        let no_auth = reqwest::Client::new()
            .post(&url)
            .json(&activity)
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(no_auth, 401);
        assert_eq!(post_activity(&url, "garbage-token", &activity).await, 401);
        assert!(
            rx.try_recv().is_err(),
            "rejected requests must not produce messages"
        );
    }

    #[tokio::test]
    async fn tampered_serviceurl_is_rejected_without_recording_reference() {
        let auth_server = MockServer::start().await;
        mock_jwks(&auth_server).await;
        let ch = channel_with(test_config(), vec!["*".to_string()], &auth_server);
        let (url, mut rx) = spawn_listener(&ch).await;

        // A validly signed token, but the body's serviceUrl was swapped
        // for an attacker-controlled host — the classic token-replay
        // redirect that would leak the Connector bearer token.
        let token = mint_service_token();
        let mut activity = personal_activity("hi");
        activity["serviceUrl"] = serde_json::json!("https://evil.example.invalid/teams/");

        assert_eq!(post_activity(&url, &token, &activity).await, 401);
        assert!(
            rx.try_recv().is_err(),
            "a mismatched serviceUrl must not produce a message"
        );
        assert!(
            ch.conversations.get("a:1conv").is_none(),
            "a mismatched serviceUrl must not record a conversation reference"
        );
    }

    #[tokio::test]
    async fn missing_serviceurl_claim_is_rejected_without_recording_reference() {
        let auth_server = MockServer::start().await;
        mock_jwks(&auth_server).await;
        let ch = channel_with(test_config(), vec!["*".to_string()], &auth_server);
        let (url, mut rx) = spawn_listener(&ch).await;

        // The claim is absent rather than mismatched: the body's serviceUrl
        // is well-formed and would be usable, but nothing signed it, so
        // there is no validated URL to pin the outbound request to.
        let token = mint_service_token_without_serviceurl();
        let activity = personal_activity("hi");

        assert_eq!(post_activity(&url, &token, &activity).await, 401);
        assert!(
            rx.try_recv().is_err(),
            "an unsigned serviceUrl must not produce a message"
        );
        assert!(
            ch.conversations.get("a:1conv").is_none(),
            "an unsigned serviceUrl must not record a conversation reference, \
             so no outbound Connector request can be addressed"
        );
    }

    #[tokio::test]
    async fn missing_channel_id_is_rejected() {
        let auth_server = MockServer::start().await;
        mock_jwks(&auth_server).await;
        let ch = channel_with(test_config(), vec!["*".to_string()], &auth_server);
        let (url, mut rx) = spawn_listener(&ch).await;

        let token = mint_service_token();
        let mut activity = personal_activity("hi");
        activity.as_object_mut().unwrap().remove("channelId");

        assert_eq!(post_activity(&url, &token, &activity).await, 401);
        assert!(rx.try_recv().is_err());
        assert!(ch.conversations.get("a:1conv").is_none());
    }

    #[tokio::test]
    async fn unendorsed_channel_id_is_rejected() {
        let auth_server = MockServer::start().await;
        mock_jwks(&auth_server).await;
        let ch = channel_with(test_config(), vec!["*".to_string()], &auth_server);
        let (url, mut rx) = spawn_listener(&ch).await;

        // The signing key endorses only `msteams`; a `directline` activity
        // signed with it must be rejected.
        let token = mint_service_token();
        let mut activity = personal_activity("hi");
        activity["channelId"] = serde_json::json!("directline");

        assert_eq!(post_activity(&url, &token, &activity).await, 401);
        assert!(rx.try_recv().is_err());
        assert!(ch.conversations.get("a:1conv").is_none());
    }

    #[tokio::test]
    async fn recorded_reference_uses_signed_serviceurl() {
        let auth_server = MockServer::start().await;
        mock_jwks(&auth_server).await;
        let ch = channel_with(test_config(), vec!["*".to_string()], &auth_server);
        let (url, mut rx) = spawn_listener(&ch).await;

        // Signed claim and body agree only up to a trailing slash; the
        // stored reference must keep the validated (signed) value.
        let token = mint_service_token_for("https://smba.trafficmanager.net/teams");
        let activity = personal_activity("hi");
        assert_eq!(post_activity(&url, &token, &activity).await, 200);
        assert!(rx.recv().await.is_some());
        assert_eq!(
            ch.conversations.get("a:1conv").unwrap().service_url,
            "https://smba.trafficmanager.net/teams",
            "the stored reference must retain the signed serviceUrl, not the body's"
        );
    }

    #[tokio::test]
    async fn dm_gate_drops_personal_chats_when_disabled() {
        let auth_server = MockServer::start().await;
        mock_jwks(&auth_server).await;
        let cfg = MSTeamsConfig {
            allow_dms: false,
            ..test_config()
        };
        let ch = channel_with(cfg, vec!["*".to_string()], &auth_server);
        let (url, mut rx) = spawn_listener(&ch).await;

        let token = mint_service_token();
        assert_eq!(
            post_activity(&url, &token, &personal_activity("hi")).await,
            200
        );
        assert!(rx.try_recv().is_err());
    }

    fn channel_activity(text: &str, mention_bot: bool) -> serde_json::Value {
        let entities = if mention_bot {
            serde_json::json!([{ "type": "mention", "mentioned": { "id": "28:bot", "name": "ZeroClaw" } }])
        } else {
            serde_json::json!([])
        };
        serde_json::json!({
            "type": "message",
            "id": "1800",
            "serviceUrl": SERVICE_URL,
            "channelId": "msteams",
            "from": { "id": "29:user-x" },
            "recipient": { "id": "28:bot", "name": "ZeroClaw" },
            "conversation": {
                "id": "19:general@thread.tacv2;messageid=1700",
                "conversationType": "channel"
            },
            "text": text,
            "entities": entities,
        })
    }

    #[tokio::test]
    async fn mention_gate_applies_to_team_channels_only() {
        let auth_server = MockServer::start().await;
        mock_jwks(&auth_server).await;
        let ch = channel_with(test_config(), vec!["*".to_string()], &auth_server);
        let (url, mut rx) = spawn_listener(&ch).await;
        let token = mint_service_token();

        // Unmentioned channel message: dropped (mention_only defaults on).
        assert_eq!(
            post_activity(&url, &token, &channel_activity("status?", false)).await,
            200
        );
        assert!(rx.try_recv().is_err());

        // Mentioned channel message: delivered, threaded on the thread root.
        assert_eq!(
            post_activity(
                &url,
                &token,
                &channel_activity("<at>ZeroClaw</at> status?", true)
            )
            .await,
            200
        );
        let msg = rx.recv().await.unwrap();
        assert_eq!(msg.reply_target, "19:general@thread.tacv2");
        assert_eq!(msg.thread_ts.as_deref(), Some("1700"));
        assert_eq!(msg.interruption_scope_id.as_deref(), Some("1700"));
        assert_eq!(msg.content, "status?");
        assert_eq!(msg.sender, "29:user-x");
        assert!(!ch.is_direct_message(&msg));
    }

    /// A channel message that @-mentions the bot and another user drops
    /// only the bot's mention; the other user's name reaches the model.
    #[tokio::test]
    async fn non_bot_mentions_are_preserved_in_prompt() {
        let auth_server = MockServer::start().await;
        mock_jwks(&auth_server).await;
        let ch = channel_with(test_config(), vec!["*".to_string()], &auth_server);
        let (url, mut rx) = spawn_listener(&ch).await;
        let token = mint_service_token();

        let mut activity =
            channel_activity("<at>ZeroClaw</at> ask <at>Alice</at> for status", true);
        activity["entities"] = serde_json::json!([
            { "type": "mention", "mentioned": { "id": "28:bot", "name": "ZeroClaw" }, "text": "<at>ZeroClaw</at>" },
            { "type": "mention", "mentioned": { "id": "29:alice", "name": "Alice" }, "text": "<at>Alice</at>" }
        ]);
        assert_eq!(post_activity(&url, &token, &activity).await, 200);

        let msg = rx.recv().await.unwrap();
        assert_eq!(msg.content, "ask Alice for status");
    }

    #[tokio::test]
    async fn empty_peer_list_denies_everyone() {
        let auth_server = MockServer::start().await;
        mock_jwks(&auth_server).await;
        let ch = channel_with(test_config(), Vec::new(), &auth_server);
        let (url, mut rx) = spawn_listener(&ch).await;

        let token = mint_service_token();
        assert_eq!(
            post_activity(&url, &token, &personal_activity("hi")).await,
            200
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn allowlist_matches_aad_object_id() {
        let auth_server = MockServer::start().await;
        mock_jwks(&auth_server).await;
        let ch = channel_with(
            test_config(),
            vec!["00000000-0000-0000-0000-00000000FEED".to_string()],
            &auth_server,
        );
        let (url, mut rx) = spawn_listener(&ch).await;

        let token = mint_service_token();
        assert_eq!(
            post_activity(&url, &token, &personal_activity("hi")).await,
            200
        );
        assert!(rx.recv().await.is_some());
    }

    #[tokio::test]
    async fn self_authored_activity_is_dropped() {
        let auth_server = MockServer::start().await;
        mock_jwks(&auth_server).await;
        let ch = channel_with(test_config(), vec!["*".to_string()], &auth_server);
        let (url, mut rx) = spawn_listener(&ch).await;

        let mut activity = personal_activity("echo");
        activity["from"] = serde_json::json!({ "id": "28:bot", "name": "ZeroClaw" });
        let token = mint_service_token();
        assert_eq!(post_activity(&url, &token, &activity).await, 200);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn non_message_activities_are_acknowledged_without_output() {
        let auth_server = MockServer::start().await;
        mock_jwks(&auth_server).await;
        let ch = channel_with(test_config(), vec!["*".to_string()], &auth_server);
        let (url, mut rx) = spawn_listener(&ch).await;

        let token = mint_service_token();
        let update = serde_json::json!({
            "type": "conversationUpdate",
            "serviceUrl": SERVICE_URL,
            "channelId": "msteams",
            "conversation": { "id": "a:1conv", "conversationType": "personal" },
            "recipient": { "id": "28:bot", "name": "ZeroClaw" },
        });
        assert_eq!(post_activity(&url, &token, &update).await, 200);
        assert!(rx.try_recv().is_err());
        // But it still recorded the reference and bot identity.
        assert_eq!(ch.self_handle().as_deref(), Some("28:bot"));
        assert!(ch.conversations.get("a:1conv").is_some());
    }

    #[tokio::test]
    async fn send_posts_to_connector_with_bearer_token() {
        let connector = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "connector-tok",
                "expires_in": 3600,
            })))
            .expect(1)
            .mount(&connector)
            .await;
        Mock::given(method("POST"))
            .and(path("/teams/v3/conversations/a:1conv/activities"))
            .and(header_matcher("authorization", "Bearer connector-tok"))
            .and(body_partial_json(
                serde_json::json!({ "type": "message", "text": "hello from zeroclaw" }),
            ))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({ "id": "act-1" })),
            )
            .expect(2)
            .mount(&connector)
            .await;

        let ch = MsTeamsChannel::new(
            "default",
            Arc::new(|| Some(test_config())),
            Arc::new(Vec::new),
        )
        .with_token_url(format!("{}/token", connector.uri()));
        ch.conversations.record(ConversationReference {
            service_url: format!("{}/teams/", connector.uri()),
            conversation_id: "a:1conv".to_string(),
            conversation_type: Some("personal".to_string()),
        });

        ch.send(&SendMessage::new("hello from zeroclaw", "a:1conv"))
            .await
            .unwrap();
        // Second send reuses the cached connector token (token mock allows
        // exactly one hit).
        ch.send(&SendMessage::new("hello from zeroclaw", "a:1conv"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn send_threads_via_messageid_suffix() {
        let connector = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "connector-tok",
                "expires_in": 3600,
            })))
            .mount(&connector)
            .await;
        Mock::given(method("POST"))
            .and(path(
                "/teams/v3/conversations/19:general@thread.tacv2;messageid=1700/activities",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&connector)
            .await;

        let ch = MsTeamsChannel::new(
            "default",
            Arc::new(|| Some(test_config())),
            Arc::new(Vec::new),
        )
        .with_token_url(format!("{}/token", connector.uri()));
        ch.conversations.record(ConversationReference {
            service_url: format!("{}/teams/", connector.uri()),
            conversation_id: "19:general@thread.tacv2".to_string(),
            conversation_type: Some("channel".to_string()),
        });

        let message = SendMessage::new("threaded reply", "19:general@thread.tacv2")
            .in_thread(Some("1700".to_string()));
        ch.send(&message).await.unwrap();
    }

    #[tokio::test]
    async fn personal_send_ignores_thread_suffix_and_sets_reply_to_id() {
        let connector = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "connector-tok",
                "expires_in": 3600,
            })))
            .mount(&connector)
            .await;
        Mock::given(method("POST"))
            .and(path("/teams/v3/conversations/a:1conv/activities"))
            .and(body_partial_json(serde_json::json!({
                "type": "message",
                "text": "reply",
                "replyToId": "1784443787334",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&connector)
            .await;

        let ch = MsTeamsChannel::new(
            "default",
            Arc::new(|| Some(test_config())),
            Arc::new(Vec::new),
        )
        .with_token_url(format!("{}/token", connector.uri()));
        ch.conversations.record(ConversationReference {
            service_url: format!("{}/teams/", connector.uri()),
            conversation_id: "a:1conv".to_string(),
            conversation_type: Some("personal".to_string()),
        });

        // A non-channel activity can carry a `;messageid=` suffix, but it
        // must not become part of a Connector conversation ID.
        let message = SendMessage::new("reply", "a:1conv")
            .in_thread(Some("1784443787334".to_string()))
            .in_reply_to(Some("1784443787334".to_string()));
        ch.send(&message).await.unwrap();
    }

    #[tokio::test]
    async fn send_without_reference_fails_with_clear_error() {
        let ch = MsTeamsChannel::new(
            "default",
            Arc::new(|| Some(test_config())),
            Arc::new(Vec::new),
        );
        let err = ch
            .send(&SendMessage::new("hi", "a:unknown"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no conversation reference"));
    }

    fn streaming_config() -> MSTeamsConfig {
        MSTeamsConfig {
            stream_mode: StreamMode::Partial,
            draft_update_interval_ms: 0,
            ..test_config()
        }
    }

    async fn mock_token_endpoint(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "connector-tok",
                "expires_in": 3600,
            })))
            .mount(server)
            .await;
    }

    fn draft_channel(config: MSTeamsConfig, connector: &MockServer) -> MsTeamsChannel {
        MsTeamsChannel::new(
            "default",
            Arc::new(move || Some(config.clone())),
            Arc::new(Vec::new),
        )
        .with_token_url(format!("{}/token", connector.uri()))
    }

    fn record_reference(ch: &MsTeamsChannel, connector: &MockServer, id: &str, kind: &str) {
        ch.conversations.record(ConversationReference {
            service_url: format!("{}/teams/", connector.uri()),
            conversation_id: id.to_string(),
            conversation_type: Some(kind.to_string()),
        });
    }

    #[test]
    fn streaming_support_flags_follow_stream_mode() {
        let connector_dummy = |mode: StreamMode| {
            MsTeamsChannel::new(
                "default",
                Arc::new(move || {
                    Some(MSTeamsConfig {
                        stream_mode: mode,
                        ..MSTeamsConfig::default()
                    })
                }),
                Arc::new(Vec::new),
            )
        };
        let off = connector_dummy(StreamMode::Off);
        assert!(!off.supports_draft_updates());
        assert!(!off.supports_multi_message_streaming());

        let partial = connector_dummy(StreamMode::Partial);
        assert!(partial.supports_draft_updates());
        assert!(!partial.supports_multi_message_streaming());

        // MultiMessage uses the draft pipeline (paragraph-split sends), so
        // it reports draft support in every conversation type.
        let multi = connector_dummy(StreamMode::MultiMessage);
        assert!(multi.supports_draft_updates());
        assert!(multi.supports_multi_message_streaming());
        assert_eq!(multi.multi_message_delay_ms(), 800);
    }

    #[tokio::test]
    async fn send_draft_returns_none_when_streaming_off() {
        let ch = MsTeamsChannel::new(
            "default",
            Arc::new(|| Some(test_config())),
            Arc::new(Vec::new),
        );
        assert!(
            ch.send_draft(&SendMessage::new("hi", "a:1conv"))
                .await
                .unwrap()
                .is_none()
        );
    }

    /// Full personal-chat streaming sequence with lazy open: the draft
    /// itself hits no network; the first real progress line opens the
    /// stream (sequence 1, no streamId, no placeholder frame), then
    /// content chunks and the final message carry the Teams-assigned
    /// streamId with monotonic streamSequence.
    #[tokio::test]
    async fn personal_streaming_draft_lifecycle() {
        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;
        Mock::given(method("POST"))
            .and(path("/teams/v3/conversations/a:1conv/activities"))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({ "id": "stream-1" })),
            )
            .mount(&connector)
            .await;

        let ch = draft_channel(streaming_config(), &connector);
        record_reference(&ch, &connector, "a:1conv", "personal");

        let draft_id = ch
            .send_draft(&SendMessage::new("...", "a:1conv"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            connector.received_requests().await.unwrap().len(),
            0,
            "opening a draft must not hit the network"
        );

        ch.update_draft_progress("a:1conv", &draft_id, "Running tools...")
            .await
            .unwrap();
        ch.update_draft("a:1conv", &draft_id, "Partial answer")
            .await
            .unwrap();
        ch.finalize_draft("a:1conv", &draft_id, "Final answer", false)
            .await
            .unwrap();

        let requests = connector.received_requests().await.unwrap();
        let bodies: Vec<serde_json::Value> = requests
            .iter()
            .filter(|r| r.url.path().ends_with("/activities"))
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect();
        assert_eq!(bodies.len(), 3);

        // First visible frame is the real status line, not a placeholder.
        assert_eq!(bodies[0]["type"], "typing");
        assert_eq!(bodies[0]["text"], "Running tools...");
        assert_eq!(bodies[0]["entities"][0]["streamType"], "informative");
        assert_eq!(bodies[0]["entities"][0]["streamSequence"], 1);
        assert!(bodies[0]["entities"][0].get("streamId").is_none());

        assert_eq!(bodies[1]["type"], "typing");
        assert_eq!(bodies[1]["text"], "Partial answer");
        assert_eq!(bodies[1]["entities"][0]["streamType"], "streaming");
        assert_eq!(bodies[1]["entities"][0]["streamSequence"], 2);
        assert_eq!(bodies[1]["entities"][0]["streamId"], "stream-1");

        assert_eq!(bodies[2]["type"], "message");
        assert_eq!(bodies[2]["text"], "Final answer");
        assert_eq!(bodies[2]["entities"][0]["streamType"], "final");
        assert_eq!(bodies[2]["entities"][0]["streamId"], "stream-1");

        assert!(ch.draft_streams.lock().is_empty());
    }

    /// Fast answers that produce no intermediate updates never open a
    /// stream: finalize delivers one plain message with no streaminfo.
    #[tokio::test]
    async fn draft_without_updates_finalizes_as_plain_message() {
        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;
        Mock::given(method("POST"))
            .and(path("/teams/v3/conversations/a:1conv/activities"))
            .and(body_partial_json(
                serde_json::json!({ "type": "message", "text": "Quick answer" }),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&connector)
            .await;

        let ch = draft_channel(streaming_config(), &connector);
        record_reference(&ch, &connector, "a:1conv", "personal");

        let draft_id = ch
            .send_draft(&SendMessage::new("...", "a:1conv"))
            .await
            .unwrap()
            .unwrap();
        ch.finalize_draft("a:1conv", &draft_id, "Quick answer", false)
            .await
            .unwrap();

        let bodies: Vec<serde_json::Value> = connector
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().ends_with("/activities"))
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect();
        assert_eq!(bodies.len(), 1);
        assert!(
            bodies[0].get("entities").is_none(),
            "plain delivery must not carry streaminfo: {}",
            bodies[0]
        );
        assert!(ch.draft_streams.lock().is_empty());
    }

    #[tokio::test]
    async fn only_personal_chats_support_partial_drafts() {
        let connector = MockServer::start().await;
        let ch = draft_channel(streaming_config(), &connector);
        record_reference(&ch, &connector, "a:1conv", "personal");
        record_reference(&ch, &connector, "19:general@thread.tacv2", "channel");

        let personal = ChannelMessage::new(
            "inbound-personal",
            "sender",
            "a:1conv",
            "hello",
            "msteams",
            0,
        );
        let channel = ChannelMessage::new(
            "inbound-channel",
            "sender",
            "19:general@thread.tacv2",
            "hello",
            "msteams",
            0,
        );
        assert!(ch.supports_draft_updates_for(&personal));
        assert!(!ch.supports_draft_updates_for(&channel));
    }

    #[tokio::test]
    async fn multi_message_supports_drafts_in_every_conversation_type() {
        let connector = MockServer::start().await;
        let cfg = MSTeamsConfig {
            stream_mode: StreamMode::MultiMessage,
            multi_message_delay_ms: 0,
            ..test_config()
        };
        let ch = draft_channel(cfg, &connector);
        record_reference(&ch, &connector, "a:1conv", "personal");
        record_reference(&ch, &connector, "19:general@thread.tacv2", "channel");

        let personal =
            ChannelMessage::new("inbound-personal", "sender", "a:1conv", "hi", "msteams", 0);
        let channel = ChannelMessage::new(
            "inbound-channel",
            "sender",
            "19:general@thread.tacv2",
            "hi",
            "msteams",
            0,
        );
        assert!(ch.supports_draft_updates_for(&personal));
        assert!(ch.supports_draft_updates_for(&channel));
    }

    /// `multi_message` splits the streamed response on paragraph
    /// boundaries: each completed paragraph ships as its own message while
    /// it arrives, and finalize flushes the trailing paragraph.
    #[tokio::test]
    async fn multi_message_streaming_sends_paragraphs_then_flushes_tail() {
        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;
        Mock::given(method("POST"))
            .and(path("/teams/v3/conversations/a:1conv/activities"))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({ "id": "m" })),
            )
            .mount(&connector)
            .await;

        let cfg = MSTeamsConfig {
            stream_mode: StreamMode::MultiMessage,
            multi_message_delay_ms: 0,
            ..test_config()
        };
        let ch = draft_channel(cfg, &connector);
        record_reference(&ch, &connector, "a:1conv", "personal");

        let draft_id = ch
            .send_draft(&SendMessage::new("", "a:1conv"))
            .await
            .unwrap()
            .unwrap();
        assert!(draft_id.starts_with("multi-"));
        assert_eq!(
            connector.received_requests().await.unwrap().len(),
            0,
            "opening a multi-message draft must not hit the network"
        );

        // Only the first, fully-arrived paragraph flushes; the partial
        // second one waits for its closing blank line.
        ch.update_draft("a:1conv", &draft_id, "First paragraph.\n\nSecond para")
            .await
            .unwrap();
        ch.update_draft(
            "a:1conv",
            &draft_id,
            "First paragraph.\n\nSecond paragraph.\n\nThird",
        )
        .await
        .unwrap();
        ch.finalize_draft(
            "a:1conv",
            &draft_id,
            "First paragraph.\n\nSecond paragraph.\n\nThird and final.",
            false,
        )
        .await
        .unwrap();

        let texts: Vec<String> = connector
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().ends_with("/activities"))
            .map(|r| {
                let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
                body["text"].as_str().unwrap_or_default().to_string()
            })
            .collect();
        assert_eq!(
            texts,
            vec![
                "First paragraph.".to_string(),
                "Second paragraph.".to_string(),
                "Third and final.".to_string(),
            ]
        );
        assert!(
            ch.multi_message.lock().is_empty(),
            "multi-message state must be cleared after finalize"
        );
    }

    /// The outbound typing indicator is a one-shot Bot Framework `typing`
    /// activity; `stop_typing` posts nothing.
    #[tokio::test]
    async fn start_typing_posts_typing_activity() {
        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;
        Mock::given(method("POST"))
            .and(path("/teams/v3/conversations/a:1conv/activities"))
            .and(body_partial_json(serde_json::json!({ "type": "typing" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&connector)
            .await;

        let ch = draft_channel(test_config(), &connector);
        record_reference(&ch, &connector, "a:1conv", "channel");

        ch.start_typing("a:1conv").await.unwrap();
        ch.stop_typing("a:1conv").await.unwrap();
    }

    #[tokio::test]
    async fn draft_updates_respect_rate_limit_floor() {
        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;
        Mock::given(method("POST"))
            .and(path("/teams/v3/conversations/a:1conv/activities"))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({ "id": "stream-1" })),
            )
            .mount(&connector)
            .await;

        let cfg = MSTeamsConfig {
            draft_update_interval_ms: 60_000,
            ..streaming_config()
        };
        let ch = draft_channel(cfg, &connector);
        record_reference(&ch, &connector, "a:1conv", "personal");

        let draft_id = ch
            .send_draft(&SendMessage::new("hi", "a:1conv"))
            .await
            .unwrap()
            .unwrap();
        // The first update opens the stream; the second lands inside the
        // 60s window and short-circuits before the network.
        ch.update_draft("a:1conv", &draft_id, "one").await.unwrap();
        ch.update_draft("a:1conv", &draft_id, "two").await.unwrap();

        let activity_posts = connector
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().ends_with("/activities"))
            .count();
        assert_eq!(
            activity_posts, 1,
            "only the stream-opening update may hit the network"
        );
    }

    #[tokio::test]
    async fn cancel_draft_deletes_activity_and_clears_state() {
        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;
        Mock::given(method("POST"))
            .and(path("/teams/v3/conversations/a:1conv/activities"))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({ "id": "stream-1" })),
            )
            .mount(&connector)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/teams/v3/conversations/a:1conv/activities/stream-1"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&connector)
            .await;

        let ch = draft_channel(streaming_config(), &connector);
        record_reference(&ch, &connector, "a:1conv", "personal");

        let draft_id = ch
            .send_draft(&SendMessage::new("hi", "a:1conv"))
            .await
            .unwrap()
            .unwrap();
        // Open the stream so there is a wire activity to delete.
        ch.update_draft("a:1conv", &draft_id, "partial")
            .await
            .unwrap();
        ch.cancel_draft("a:1conv", &draft_id).await.unwrap();
        assert!(ch.draft_streams.lock().is_empty());
        assert!(ch.last_draft_update.lock().is_empty());
    }

    /// Cancelling a draft whose stream never opened has nothing on the
    /// wire to delete and must not hit the network at all.
    #[tokio::test]
    async fn cancel_unopened_draft_makes_no_network_calls() {
        let connector = MockServer::start().await;
        let ch = draft_channel(streaming_config(), &connector);
        record_reference(&ch, &connector, "a:1conv", "personal");

        let draft_id = ch
            .send_draft(&SendMessage::new("hi", "a:1conv"))
            .await
            .unwrap()
            .unwrap();
        ch.cancel_draft("a:1conv", &draft_id).await.unwrap();

        assert!(ch.draft_streams.lock().is_empty());
        assert!(connector.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn send_surfaces_connector_error_body() {
        let connector = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "connector-tok",
                "expires_in": 3600,
            })))
            .mount(&connector)
            .await;
        Mock::given(method("POST"))
            .and(path("/teams/v3/conversations/a:1conv/activities"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_string(r#"{"error":"BotNotInConversationRoster"}"#),
            )
            .mount(&connector)
            .await;

        let ch = MsTeamsChannel::new(
            "default",
            Arc::new(|| Some(test_config())),
            Arc::new(Vec::new),
        )
        .with_token_url(format!("{}/token", connector.uri()));
        ch.conversations.record(ConversationReference {
            service_url: format!("{}/teams/", connector.uri()),
            conversation_id: "a:1conv".to_string(),
            conversation_type: Some("personal".to_string()),
        });

        let err = ch
            .send(&SendMessage::new("hi", "a:1conv"))
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("403"), "missing status in: {text}");
        assert!(text.contains("BotNotInConversationRoster"));
    }

    #[test]
    fn in_budget_message_is_a_single_unchanged_chunk() {
        let msg = "hello\n\nworld ```code```";
        assert_eq!(split_message_for_teams(msg), vec![msg.to_string()]);
        // Empty content stays a single (empty) chunk, matching a plain send.
        assert_eq!(split_message_for_teams(""), vec![String::new()]);
    }

    #[test]
    fn oversize_message_splits_into_budget_sized_chunks_losslessly() {
        // A single unbroken run (no break points) forces hard cuts.
        let msg = "x".repeat(TEAMS_MAX_MESSAGE_CHARS * 2 + 500);
        let chunks = split_message_for_teams(&msg);
        assert!(
            chunks.len() >= 3,
            "expected multiple chunks, got {}",
            chunks.len()
        );
        for chunk in &chunks {
            assert!(
                chunk.chars().count() <= TEAMS_MAX_MESSAGE_CHARS,
                "chunk exceeds budget: {} chars",
                chunk.chars().count()
            );
        }
        // Every character is preserved and the order is stable.
        assert_eq!(chunks.concat(), msg);
    }

    #[test]
    fn oversize_message_prefers_paragraph_and_newline_boundaries() {
        // Two paragraphs, each just under the budget, joined by a blank line.
        let para = "a".repeat(TEAMS_MAX_MESSAGE_CHARS - 100);
        let msg = format!("{para}\n\n{para}");
        let chunks = split_message_for_teams(&msg);
        assert_eq!(chunks.len(), 2, "should break at the paragraph boundary");
        // The blank line is kept at the tail of the first chunk (lossless), so
        // the second chunk starts cleanly at the next paragraph, not a newline.
        assert!(chunks[0].ends_with("\n\n"));
        assert!(!chunks[1].starts_with('\n'));
        assert_eq!(chunks.concat(), msg);
    }
}
