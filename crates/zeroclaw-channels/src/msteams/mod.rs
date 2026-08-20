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
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
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
    /// Conversation this draft streams into, with any `;messageid=`
    /// suffix stripped. Source of truth created here: the map is keyed by
    /// draft handle, and nothing else records which chat a draft belongs
    /// to — which [`MsTeamsChannel::send_draft`] needs, since Teams allows
    /// only one stream per chat at a time.
    conversation: String,
    /// When the draft was registered, used to age out the entry above.
    /// A draft is normally removed on finalize or cancel; this bounds the
    /// damage if some path drops one, so a chat cannot lose streaming for
    /// the rest of the process.
    opened_at: std::time::Instant,
    /// Teams `streamId` — the Connector-assigned id of the first
    /// activity. `None` while the draft is lazily pending (no activity
    /// has been POSTed yet).
    stream_id: Option<String>,
    /// Next `streamSequence` (starts at 1, monotonic per stream).
    next_sequence: u64,
    /// Whether a `streaming` (content) frame has been pushed yet. Teams
    /// stops rendering informative updates once content streaming
    /// begins, so later status lines would be discarded on arrival while
    /// still spending the stream's one-request-per-second budget.
    content_started: bool,
    /// The last content frame's text, empty until one is pushed. Source of
    /// truth created here: nothing else keeps what a partial draft has put
    /// on screen. Closing a stream needs it, because Teams refuses a final
    /// message that does not contain what was streamed before it, and an
    /// abandoned draft has to be closed with something.
    streamed: String,
    /// Whether this draft has already reported giving up on streaming
    /// because the response outgrew a Teams message. Every later delta
    /// takes the same branch, so the report is made once per draft.
    size_exceeded: bool,
}

/// A stream a torn-down draft left on screen: the id to address it by, and
/// the content already streamed into it, which any closing message has to
/// contain.
struct OpenedStream {
    id: String,
    streamed: String,
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

/// Per-message size ceiling for outbound Teams activities, in characters.
///
/// Teams measures a message's size in UTF-16 code units — including
/// `@`-mentions and reactions — and rejects anything past ~100 KB with a
/// `413` (`MessageSizeTooBig`); Microsoft recommends staying under 80 KB. This
/// budget is deliberately conservative: even all-surrogate-pair text (2 UTF-16
/// units per `char`) stays well under the hard limit, leaving headroom for the
/// mention/reaction/JSON-envelope overhead the limit also counts.
const TEAMS_MAX_MESSAGE_CHARS: usize = 18_000;

/// Ceiling for one informative (status line) frame.
///
/// Microsoft states informative messages "must not be more than 1 kb or 1000
/// characters" without saying how the byte figure is measured, so both bounds
/// are honoured: a status line is clamped to whichever comes first. ASCII text
/// is bounded by the character count, non-Latin scripts by the byte count.
/// Status lines are short in practice; the clamp only has to keep a runaway
/// tool label from turning every frame of a turn into a rejection.
const TEAMS_MAX_INFORMATIVE_CHARS: usize = 1_000;
const TEAMS_MAX_INFORMATIVE_BYTES: usize = 1_024;

/// Teams' hard limit on one streaming session, after which it stops the
/// bubble and refuses further frames (`403`, `Content stream finished due to
/// exceeded streaming time`).
///
/// Used here to age out a draft that was registered but never finalized or
/// cancelled: past this point its stream is dead by Teams' own rule, so it can
/// no longer be the one stream the chat is allowed.
const TEAMS_STREAM_SESSION_LIMIT: Duration = Duration::from_secs(120);

/// Clamp an informative frame to [`TEAMS_MAX_INFORMATIVE_CHARS`] /
/// [`TEAMS_MAX_INFORMATIVE_BYTES`], marking a shortened line with an ellipsis
/// so the truncation reads as deliberate. Borrows when the line already fits.
fn clamp_informative_text(text: &str) -> Cow<'_, str> {
    if text.len() <= TEAMS_MAX_INFORMATIVE_BYTES
        && text.chars().count() <= TEAMS_MAX_INFORMATIVE_CHARS
    {
        return Cow::Borrowed(text);
    }
    const ELLIPSIS: char = '…';
    let byte_budget = TEAMS_MAX_INFORMATIVE_BYTES - ELLIPSIS.len_utf8();
    let char_budget = TEAMS_MAX_INFORMATIVE_CHARS - 1;
    let end = text
        .char_indices()
        .take(char_budget)
        .take_while(|(idx, ch)| idx + ch.len_utf8() <= byte_budget)
        .map(|(idx, ch)| idx + ch.len_utf8())
        .last()
        .unwrap_or(0);
    let mut clamped = text[..end].to_string();
    clamped.push(ELLIPSIS);
    Cow::Owned(clamped)
}

/// Minimum spacing between the chunks of one oversize reply.
///
/// Teams counts every chunk as its own "send to conversation" operation, and
/// warns that message splitting drives RPS higher than callers expect. Its
/// quota is four sliding windows, each of which implies a minimum spacing:
///
/// | window | quota | spacing |
/// |--------|-------|---------|
/// | 1s     | 7     | 143ms   |
/// | 2s     | 8     | 250ms   |
/// | 30s    | 60    | 500ms   |
/// | 3600s  | 1800  | 2000ms  |
///
/// Only windows shorter than a burst can bind it, and a burst here is one
/// reply's chunk count: five chunks is already a 90 000-character answer, so
/// the 1s and 2s windows are the reachable ones. This value is the tightest
/// of those (250ms) doubled for headroom, which also lands exactly on the 30s
/// quota, and leaves the two shorter windows at 2/7 and 4/8 so a concurrent
/// turn in the same conversation, which paces itself independently, still
/// fits.
///
/// The hourly window is deliberately *not* self-enforced: 1800 sends is a
/// budget spanning a full hour, and honoring it as a rate would cost a
/// ten-chunk reply twenty seconds of delivery for a bound no realistic
/// conversation reaches. Microsoft's own answer for a window that does fill
/// up is backoff on `429`, which [`Self::activity_request`] implements.
const TEAMS_CHUNK_SEND_SPACING: Duration = Duration::from_millis(500);

/// How many times one Connector request may be attempted when Teams
/// throttles it.
///
/// Sized against the windows a retry can actually outlast. Three attempts
/// leave two waits, which with [`CONNECTOR_RETRY_BASE_DELAY_MS`] total at
/// least 2.25s even when both jitter rolls come up short, so a filled 1s or
/// 2s window has certainly reopened. The 30s and hourly windows are
/// deliberately *not* waited out: those fill only when the conversation is
/// genuinely over budget, and reporting that beats holding a turn for half a
/// minute against the deadlines in [`CONNECTOR_RETRY_MAX_DELAY_MS`].
///
/// Microsoft's own sample retries three times from a 2s base, capped at 20s.
/// This budget is tighter on purpose, for those deadlines.
const CONNECTOR_MAX_ATTEMPTS: u32 = 3;

/// First backoff step between throttled Connector attempts, doubled per
/// attempt. Used only when Teams sends no `Retry-After`.
///
/// Chosen with [`CONNECTOR_MAX_ATTEMPTS`], not independently: 1s then 2s,
/// each ±25%, cannot cumulate to less than 2.25s, which is the 2s window's
/// width plus margin. A 500ms base would peak near 1.9s and could retry back
/// into a window that had not yet reopened.
const CONNECTOR_RETRY_BASE_DELAY_MS: u64 = 1_000;

/// Whether a Connector request may be retried when Teams throttles it.
///
/// Retrying is right only where losing the request loses content, which is
/// narrower than "carries content". An intermediate streaming frame, a typing
/// indicator and a draft cancellation are all superseded by whatever comes
/// next, and their callers already treat any error as "skip"; waiting on them
/// would only stall the agent's token loop, which is the very cost
/// `draft_update_interval_ms` skips updates to avoid. The finalize activity
/// looks like the exception and is not: the orchestrator answers a failed
/// finalize by resending the whole answer through [`Channel::send`], whose
/// own chunks retry, so the content is already covered one layer up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThrottlePolicy {
    /// Losing the request loses content with nothing behind it: wait out a
    /// `429` (see [`CONNECTOR_MAX_ATTEMPTS`]) rather than drop it.
    Retry,
    /// Something behind the request covers it, whether the next frame or a
    /// caller's fallback: report a `429` immediately so that takes over.
    FailFast,
}

/// Ceiling on a single backoff wait, including a `Retry-After` Teams asks
/// for.
///
/// Every retrying request is a send outside a stream (plain replies, split
/// chunks), so one deadline covers them all:
/// the per-turn budget, `channels.message_timeout_secs`, 300s by default.
/// Requests that carry a `streamId` never reach this ceiling; the two-minute
/// session limit makes a stream that has begun refusing requests a lost cause,
/// so those fail fast instead. A 10s ceiling keeps two waits well inside the
/// turn budget, where obeying an arbitrarily long hint would fail the turn more
/// surely than giving up early does.
const CONNECTOR_RETRY_MAX_DELAY_MS: u64 = 10_000;

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
    /// Supplies the client for the two auth egresses (JWKS, Entra token).
    /// Held as a resolver, not a client: `proxy_url` is live config, and
    /// these calls have to leave through the same proxy as the Connector
    /// sends or a proxied deployment authenticates nothing.
    auth_http: auth::HttpClientResolver,
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
    #[cfg(test)]
    token_url_override: Option<String>,
}

impl MsTeamsChannel {
    pub fn new(
        alias: impl Into<String>,
        config_resolver: ConfigResolver,
        peer_resolver: PeerResolver,
    ) -> Self {
        let auth_http = Self::auth_http_resolver(&config_resolver);
        Self {
            alias: alias.into(),
            config_resolver,
            peer_resolver,
            validator: Arc::new(
                auth::JwtValidator::new(auth::BOT_FRAMEWORK_OPENID_METADATA_URL)
                    .with_http_client_resolver(auth_http.clone()),
            ),
            auth_http,
            conversations: Arc::new(ConversationStore::default()),
            bot_identity: Arc::new(OnceLock::new()),
            listener_ready: Arc::new(AtomicBool::new(false)),
            connector: tokio::sync::RwLock::new(None),
            draft_streams: parking_lot::Mutex::new(HashMap::new()),
            draft_counter: AtomicU64::new(0),
            last_draft_update: parking_lot::Mutex::new(HashMap::new()),
            #[cfg(test)]
            token_url_override: None,
        }
    }

    /// Test hook: validate inbound JWTs against a mock OpenID/JWKS server.
    #[cfg(test)]
    fn with_openid_metadata_url(mut self, url: impl Into<String>) -> Self {
        self.validator = Arc::new(
            auth::JwtValidator::new(url.into()).with_http_client_resolver(self.auth_http.clone()),
        );
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

    /// Client factory for the auth egresses, reading `proxy_url` from the
    /// same resolver every other config read goes through. The shorter
    /// timeout matches what these two endpoints had before they were
    /// routed through the proxy; the factory caches per proxy setting, so
    /// resolving on each call costs no new connection pool.
    fn auth_http_resolver(config_resolver: &ConfigResolver) -> auth::HttpClientResolver {
        let config_resolver = config_resolver.clone();
        Arc::new(move || {
            let proxy_url = config_resolver().and_then(|cfg| cfg.proxy_url);
            zeroclaw_config::schema::build_channel_proxy_client_with_timeouts(
                "channel.msteams",
                proxy_url.as_deref(),
                10,
                10,
            )
        })
    }

    /// Token provider for the current tenant, rebuilt if `tenant_id`
    /// changed since the last send. A changed `app_id` or `app_password`
    /// needs no rebuild: the provider mints per credential pair and only
    /// serves a cached token back to the pair it was minted for.
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
        let provider = Arc::new(
            auth::ConnectorTokenProvider::new(token_url)
                .with_http_client_resolver(self.auth_http.clone()),
        );
        *guard = Some(ConnectorHandle {
            tenant_id: tenant_id.to_string(),
            provider: provider.clone(),
        });
        provider
    }

    /// Effective stream mode, resolved from canonical state.
    ///
    /// `multi_message` is not offered on Teams and reads as `off` here, the
    /// same fallback Lark applies to it. Paragraph delivery publishes each
    /// paragraph as a permanent message, and the draft boundary it would
    /// publish from is the one the orchestrator does not run its outbound
    /// leak policy over, so a credential in mid-answer text could reach a
    /// message no later sanitized reply can edit or recall. That boundary is
    /// shared with Discord and Matrix and is fixed there, not here.
    /// [`Self::listen`] names the fallback in the operator's log once at
    /// startup; clamping here rather than at startup also covers a config
    /// reload into the mode.
    fn stream_mode(&self) -> StreamMode {
        match self.config().map(|cfg| cfg.stream_mode).unwrap_or_default() {
            StreamMode::MultiMessage => StreamMode::Off,
            other => other,
        }
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

    /// Refuse a destination that would carry the Connector token in clear
    /// text. Microsoft treats this token as password-equivalent and always
    /// serves `serviceUrl` over TLS, so a plain-HTTP destination is either a
    /// misconfigured deployment or an attempt to capture the credential;
    /// either way the token must not leave. Loopback is the one exception:
    /// a local mock is not a production Connector destination and cannot
    /// carry the token off the host.
    fn require_tls_destination(url: &url::Url) -> Result<()> {
        if url.scheme() == "https" {
            return Ok(());
        }
        let loopback = match url.host() {
            Some(url::Host::Domain(host)) => host == "localhost",
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        };
        if url.scheme() == "http" && loopback {
            return Ok(());
        }
        // Host but not path: conversation ids are not worth logging.
        anyhow::bail!(
            "refusing to send the Teams Connector token to non-HTTPS destination '{}://{}'",
            url.scheme(),
            url.host_str().unwrap_or("<no host>")
        )
    }

    /// `Retry-After` in delay-seconds, when Teams sends one.
    ///
    /// The HTTP-date form of the header is ignored on purpose: honoring an
    /// absolute deadline would import the service's clock into ours, and the
    /// local backoff is the better answer when the two disagree.
    fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
        headers
            .get(reqwest::header::RETRY_AFTER)?
            .to_str()
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()
            .map(Duration::from_secs)
    }

    /// How long to wait before retrying a throttled Connector request.
    ///
    /// Teams' own hint wins when it sends one, since it knows which of the
    /// per-second, per-30s and per-hour windows was hit. Otherwise the wait
    /// doubles per attempt with ±25% jitter, so several turns throttled in
    /// the same conversation do not retry in lockstep and trip the limit
    /// again together. Either way the wait is capped at
    /// [`CONNECTOR_RETRY_MAX_DELAY_MS`].
    fn connector_retry_delay(attempt: u32, retry_after: Option<Duration>) -> Duration {
        let ms = match retry_after {
            Some(hint) => u64::try_from(hint.as_millis()).unwrap_or(u64::MAX),
            None => {
                let multiplier = 1_u64.checked_shl(attempt).unwrap_or(u64::MAX);
                let base = CONNECTOR_RETRY_BASE_DELAY_MS.saturating_mul(multiplier);
                let factor = 0.75 + (rand::random::<f64>() * 0.5);
                // Safe: `factor` is in [0.75, 1.25] so the product is
                // non-negative, and an f64→u64 cast saturates on overflow.
                #[allow(
                    clippy::cast_precision_loss,
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss
                )]
                let jittered = ((base as f64) * factor) as u64;
                jittered
            }
        };
        Duration::from_millis(ms.min(CONNECTOR_RETRY_MAX_DELAY_MS))
    }

    /// Issue a Connector API request; returns the activity id from the
    /// response body when the Connector provides one.
    ///
    /// Under [`ThrottlePolicy::Retry`] a `429` is retried up to
    /// [`CONNECTOR_MAX_ATTEMPTS`] times, which Teams requires of every caller:
    /// its per-conversation ceiling is 7 sends per second and a burst is
    /// expected to be waited out, not surfaced as a failed reply. Other
    /// statuses are returned as errors on the first response. Notably
    /// `502`/`504` are not retried even though Microsoft's guidance lists
    /// them: creating an activity is not idempotent and the Connector offers
    /// no idempotency key, so a retry after an ambiguous gateway failure risks
    /// posting the message twice, which is worse for the conversation than one
    /// failed send that the caller can report.
    async fn activity_request(
        ctx: &SendContext,
        method: reqwest::Method,
        url: url::Url,
        body: &serde_json::Value,
        throttle: ThrottlePolicy,
    ) -> Result<Option<String>> {
        // Every Connector call funnels through here, so this is the one
        // place that has to hold: the check precedes the `bearer_auth` below
        // rather than living at URL construction, which binds it to the
        // credential instead of to one caller's code path.
        Self::require_tls_destination(&url)?;
        let mut attempt = 0;
        loop {
            let response = ctx
                .client
                .request(method.clone(), url.clone())
                .bearer_auth(&ctx.token)
                .json(body)
                .send()
                .await
                .context("Teams Connector request failed")?;
            let status = response.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS
                && throttle == ThrottlePolicy::Retry
                && attempt + 1 < CONNECTOR_MAX_ATTEMPTS
            {
                let delay = Self::connector_retry_delay(
                    attempt,
                    Self::parse_retry_after(response.headers()),
                );
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({
                            "attempt": attempt + 1,
                            "delay_ms": delay.as_millis(),
                        })),
                    "Teams Connector throttled (429), backing off"
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
                continue;
            }
            let text = response.text().await.unwrap_or_default();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                // Attempts actually made, which is 1 under
                // `ThrottlePolicy::FailFast`.
                let attempts = attempt + 1;
                anyhow::bail!(
                    "Teams Connector request throttled after {attempts} attempt(s) \
                     ({status}): {text}"
                );
            }
            if !status.is_success() {
                anyhow::bail!("Teams Connector request failed ({status}): {text}");
            }
            return Ok(serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|value| {
                    value
                        .get("id")
                        .and_then(|id| id.as_str().map(str::to_string))
                }));
        }
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
    /// returning the stream it leaves on screen if one ever opened.
    fn clear_draft_state(&self, recipient: &str, draft_id: &str) -> Option<OpenedStream> {
        let removed = self.draft_streams.lock().remove(draft_id);
        // Only when this call is the one that owned the draft. The pacing map
        // is keyed by recipient rather than by handle, and `finalize_draft`
        // clears twice: once inside delivery, before the closing activity,
        // and again on the way out. The freed stream slot lets the next turn
        // in this chat open its own draft during that request, so a second,
        // draft-less clear would drop *its* interval floor and let its next
        // frame go out early. `send_draft` registers a draft before it hands
        // the handle out, so `removed` is `Some` exactly on the clear that
        // owns one.
        if removed.is_some() {
            self.last_draft_update.lock().remove(recipient);
        }
        removed.and_then(|draft| {
            draft.stream_id.map(|id| OpenedStream {
                id,
                streamed: draft.streamed,
            })
        })
    }

    /// Deliver a native-streaming draft's final answer: the closing
    /// `message` activity when the stream opened, an ordinary message when
    /// it never did. Called only through [`Channel::finalize_draft`],
    /// which clears the draft's state for whatever this returns.
    async fn finalize_streaming_draft(
        &self,
        recipient: &str,
        draft_id: &str,
        text: &str,
    ) -> Result<()> {
        let (_, ctx) = self.send_context(recipient).await?;
        let text = crate::util::strip_tool_call_tags(text);
        // No `replyToId` and no thread suffix, unlike the ordinary send the
        // orchestrator falls back to. Neither is reachable from this method —
        // the trait passes the draft handle, not the message that started the
        // turn — and neither would show anything: a partial draft only exists
        // in a personal chat, where Teams has no threads and renders a
        // `replyToId` reply as a plain message at the end of the conversation
        // (visual threading is a channel-only feature). A team-channel turn
        // never opens a draft, so its threaded reply goes out through `send`,
        // which does carry the anchor.
        let url = Self::activities_url(&ctx.reference, &ctx.base_id, None)?;

        let stream = self.clear_draft_state(recipient, draft_id);
        // A stream carries its answer in one activity and cannot chunk it, so
        // an oversize response has no way to close the bubble on its own
        // content: the final message is refused for the same size reason its
        // frames were. Close it on what already streamed and deliver through
        // `send`, which splits. Without this the request is spent only to
        // fail, and the reply arrives by way of the caller's error fallback.
        if text.chars().count() > TEAMS_MAX_MESSAGE_CHARS {
            if let Some(stream) = stream.as_ref() {
                let _ = Self::close_stream_activity(&ctx, stream, &Self::cancelled_notice()).await;
            }
            return self.send(&SendMessage::new(&text, recipient)).await;
        }
        let body = match stream.as_ref() {
            Some(stream) => {
                streaming_activity_body("message", &text, "final", None, Some(&stream.id))
            }
            None => serde_json::json!({ "type": "message", "text": text }),
        };
        // Not waited out, even though it carries the answer: the caller
        // resends the whole thing through `send()` if this fails, and those
        // chunks retry. Waiting here would only delay that fallback, and once
        // the stream has passed its two-minute deadline every attempt is
        // spent on a session that cannot accept the message anyway.
        let delivered = Self::activity_request(
            &ctx,
            reqwest::Method::POST,
            url,
            &body,
            ThrottlePolicy::FailFast,
        )
        .await;
        if let (Err(_), Some(stream)) = (&delivered, stream.as_ref()) {
            // A failed finalize is answered by resending the reply as an
            // ordinary message, so the opened bubble has to go or the answer
            // lands underneath a draft frozen on whatever streamed last.
            // Teams refuses a final message that does not extend the content
            // already streamed, which a tool loop trips whenever its answer
            // is not a continuation of an earlier text segment, so this is a
            // routine outcome rather than a rare one. Closing on the streamed
            // content is the one final message that cannot be refused for
            // that reason.
            let _ = Self::close_stream_activity(&ctx, stream, &Self::cancelled_notice()).await;
        }
        delivered.map(|_| ())
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
        // An intermediate frame carries the whole response so far and the next
        // one supersedes it, so a throttled frame is dropped rather than
        // waited on: blocking here would stall the token loop feeding it.
        let response_id = Self::activity_request(
            &ctx,
            reqwest::Method::POST,
            url,
            &body,
            ThrottlePolicy::FailFast,
        )
        .await?;

        if let Some(draft) = self.draft_streams.lock().get_mut(draft_id) {
            if draft.stream_id.is_none() {
                draft.stream_id = Some(
                    response_id
                        .context("Teams streaming draft opened but no streamId was returned")?,
                );
            }
            draft.next_sequence = sequence + 1;
            if stream_type == "streaming" {
                draft.content_started = true;
                text.clone_into(&mut draft.streamed);
            }
        }
        self.mark_draft_update(recipient);
        Ok(())
    }

    /// What an abandoned bubble says when it has no streamed content to
    /// close on. Only reaches the screen if the delete that follows the
    /// closing message does not land, so it has to read as an explanation
    /// rather than as an answer.
    fn cancelled_notice() -> String {
        zeroclaw_runtime::i18n::get_required_cli_string("channel-msteams-draft-cancelled")
    }

    /// Take an abandoned draft's bubble off the screen.
    ///
    /// A delete alone does not do it. Teams accepts one against a live
    /// stream and answers `2xx`, but that only drops the activity on the
    /// service: the client goes on rendering the bubble, with a Stop button
    /// that then reports "can't stop the response" because the stream it
    /// would stop is gone. The only thing that ends the stream client-side
    /// is the final message the streaming contract asks for, so send that
    /// first and delete the ordinary message it leaves behind.
    ///
    /// A final message must contain everything already streamed, so the
    /// closing text is that content when there is any. `notice` covers the
    /// draft that never got past its status lines, and is what stays on
    /// screen if the delete does not land.
    async fn close_stream_activity(
        ctx: &SendContext,
        stream: &OpenedStream,
        notice: &str,
    ) -> Result<()> {
        let closing = if stream.streamed.trim().is_empty() {
            notice
        } else {
            stream.streamed.as_str()
        };
        let url = Self::activities_url(&ctx.reference, &ctx.base_id, None)?;
        let body = streaming_activity_body("message", closing, "final", None, Some(&stream.id));
        // Both requests are cosmetic: the caller has already decided what
        // the conversation gets instead, and waiting out a throttle here
        // would only delay it.
        Self::activity_request(
            ctx,
            reqwest::Method::POST,
            url,
            &body,
            ThrottlePolicy::FailFast,
        )
        .await
        .inspect_err(|err| {
            // Nothing else reports this, since every caller treats the
            // takedown as best-effort. Left silent, a bubble that
            // outlived the attempt looks exactly like one that was
            // removed.
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "stream_id": stream.id,
                        "error": format!("{err}"),
                    })),
                "Teams stream could not be closed; its bubble stays on screen"
            );
        })?;
        Self::delete_stream_activity(ctx, &stream.id).await
    }

    /// Remove a closed stream's message. Only useful once the stream itself
    /// has ended: against a live one this reports success and changes
    /// nothing the user can see (see [`Self::close_stream_activity`]).
    async fn delete_stream_activity(ctx: &SendContext, stream_id: &str) -> Result<()> {
        let url = Self::activities_url(&ctx.reference, &ctx.base_id, Some(stream_id))?;
        Self::activity_request(
            ctx,
            reqwest::Method::DELETE,
            url,
            &serde_json::Value::Null,
            ThrottlePolicy::FailFast,
        )
        .await
        .inspect_err(|err| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "stream_id": stream_id,
                        "error": format!("{err}"),
                    })),
                "Teams closing message could not be deleted; it stays in the conversation"
            );
        })
        .map(|_| ())
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
        // The transport-level backstop Telegram, WeChat and WhatsApp Web also
        // keep. The orchestrator strips envelopes from assistant text before
        // either a draft frame or a finalized reply, but nothing in the trait
        // obliges a caller to have run that pass, and this method also carries
        // split chunks and the oversize-stream handoff. Stripping once here
        // covers all of them.
        let content = crate::util::strip_tool_call_tags(&message.content);
        // A paragraph that was nothing but an envelope has nothing left to
        // say. Teams rejects an empty activity, and the caller wanted that
        // text delivered, not a blank message in its place.
        if content.trim().is_empty() && !message.content.trim().is_empty() {
            return Ok(());
        }
        let (_, ctx) = self.send_context(&message.recipient).await?;
        let conversation_id = Self::conversation_id_for_thread(&ctx, message.thread_ts.as_deref());
        let url = Self::activities_url(&ctx.reference, &conversation_id, None)?;
        // Teams rejects any single activity past ~100 KB (413
        // MessageSizeTooBig), so split oversize content into ordered chunks —
        // a long response then lands in full instead of failing outright. The
        // common (in-budget) case is a single chunk, unchanged from a plain send.
        let chunks = split_message_for_teams(&content);
        for (index, chunk) in chunks.iter().enumerate() {
            let mut body = serde_json::json!({ "type": "message", "text": chunk });
            if let Some(reply_to_id) = message.in_reply_to.as_deref() {
                body["replyToId"] = serde_json::Value::String(reply_to_id.to_string());
            }
            Self::activity_request(
                &ctx,
                reqwest::Method::POST,
                url.clone(),
                &body,
                ThrottlePolicy::Retry,
            )
            .await?;
            // Spacing goes between chunks, never after the last one: a
            // single-chunk reply is the common case and must not pay for a
            // split it did not need.
            if index + 1 < chunks.len() {
                tokio::time::sleep(TEAMS_CHUNK_SEND_SPACING).await;
            }
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
        // The secret is as load-bearing as the two ids above: Entra mints
        // every Connector token from it, so an enabled channel without one
        // would bind, report ready, accept activities, and then fail each
        // reply at the token exchange. Refusing at startup puts that in the
        // operator's log instead of one error per message.
        if cfg.app_password.trim().is_empty() {
            anyhow::bail!(
                "Microsoft Teams channel '{}' requires `app_password`: Entra mints the \
                 Connector token from it, so without it every reply fails; set it under \
                 [channels.msteams.{}]",
                self.alias,
                self.alias,
            );
        }
        // Said once here rather than on every draft callback, where
        // [`Self::stream_mode`] does the actual clamping.
        if cfg.stream_mode == StreamMode::MultiMessage {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "msteams: stream_mode=multi_message is not supported; falling back to off (one \
                 reply per turn). Use stream_mode=partial for the native streaming bubble in \
                 personal chats."
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
    /// suppresses typing there; this covers group chats and non-streaming
    /// (`off`) turns.
    async fn start_typing(&self, recipient: &str) -> Result<()> {
        // A team channel has no typing indicator, for a bot or for a human
        // author. The Connector still takes the activity — it answers 202 and
        // the channel shows nothing — so this cannot be discovered from the
        // response, and the orchestrator asks again every few seconds for the
        // whole turn. Sending anyway spends a request, and the conversation's
        // rate-limit budget, against the reply that does show.
        let (base_id, _) = activity::split_conversation_id(recipient);
        if self
            .conversations
            .get(base_id)
            .is_some_and(|reference| reference.is_team_channel())
        {
            return Ok(());
        }
        let (_, ctx) = self.send_context(recipient).await?;
        // Only personal and group chats reach here, and neither has threads,
        // so the conversation root is the whole address.
        let url = Self::activities_url(&ctx.reference, &ctx.base_id, None)?;
        let body = serde_json::json!({ "type": "typing" });
        // The indicator expires on its own and the orchestrator re-invokes
        // this, so a throttled one is skipped rather than retried.
        Self::activity_request(
            &ctx,
            reqwest::Method::POST,
            url,
            &body,
            ThrottlePolicy::FailFast,
        )
        .await?;
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
        // Teams' native streaming (the gray bubble) is personal-chat only;
        // group chats and channels use a typing indicator plus one final
        // reply instead.
        self.stream_mode() == StreamMode::Partial && self.is_direct_message(message)
    }

    /// Open a streaming draft for the response.
    ///
    /// `partial` (personal chats only) registers a lazy native-streaming
    /// draft. No activity is POSTed here — the placeholder the orchestrator
    /// passes is dropped, and the Teams stream opens on the first real
    /// update, so the gray bubble never flashes "..." (fast answers skip the
    /// stream entirely). Group chats and team channels don't open a draft:
    /// they use the typing indicator and deliver one final reply.
    async fn send_draft(&self, message: &SendMessage) -> Result<Option<String>> {
        match self.stream_mode() {
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
                {
                    let mut drafts = self.draft_streams.lock();
                    // Teams allows one streaming response per chat at a time,
                    // and a turn here does not know about its neighbours: with
                    // `interrupt_on_new_message` off (the default) a follow-up
                    // that arrives during a slow turn runs alongside it rather
                    // than replacing it, so both would open a stream in the
                    // same chat. The second turn goes without one instead —
                    // the orchestrator then delivers its answer as an ordinary
                    // message, which is what group chats already do. Opening
                    // it anyway would not stream either: every frame is spent
                    // on a stream Teams refuses to start.
                    if drafts.values().any(|draft| {
                        draft.conversation == base_id
                            && draft.opened_at.elapsed() < TEAMS_STREAM_SESSION_LIMIT
                    }) {
                        return Ok(None);
                    }
                    drafts.insert(
                        draft_id.clone(),
                        DraftStream {
                            conversation: base_id.to_string(),
                            opened_at: std::time::Instant::now(),
                            stream_id: None,
                            next_sequence: 1,
                            content_started: false,
                            streamed: String::new(),
                            size_exceeded: false,
                        },
                    );
                }
                Ok(Some(draft_id))
            }
            // `off`, and `multi_message` with it: no draft, so the
            // orchestrator delivers the answer through `send()`.
            _ => Ok(None),
        }
    }

    /// Stream accumulated content into the draft: open the Teams native
    /// stream on the first call, edit it afterwards. Non-fatal failures are
    /// logged and swallowed — the finalize pass carries the remaining text.
    async fn update_draft(&self, recipient: &str, message_id: &str, text: &str) -> Result<()> {
        if text.trim().is_empty() {
            return Ok(());
        }
        let Some(cfg) = self.config() else {
            return Ok(());
        };
        if !self.draft_update_allowed(recipient, cfg.draft_update_interval_ms) {
            return Ok(());
        }
        // Every frame carries the whole response so far, and Teams holds a
        // stream to the same size ceiling as a plain message (403
        // `ContentStreamNotAllowed`, "Message size too large"). Past the
        // budget no frame can land, so stop spending the stream's
        // one-per-second budget on rejections: finalize delivers the answer
        // as split messages instead.
        let length = text.chars().count();
        if length > TEAMS_MAX_MESSAGE_CHARS {
            let first_report = self
                .draft_streams
                .lock()
                .get_mut(message_id)
                .is_some_and(|draft| !std::mem::replace(&mut draft.size_exceeded, true));
            if first_report {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({
                            "chars": length,
                            "budget": TEAMS_MAX_MESSAGE_CHARS,
                        })),
                    "Teams response outgrew a message; streaming stopped, the answer will be split"
                );
            }
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
        let Some(cfg) = self.config() else {
            return Ok(());
        };
        if text.trim().is_empty() {
            return Ok(());
        }
        // Teams renders informative updates only up to the first content
        // frame and discards them afterwards, so a tool loop that resumes
        // after some answer text has streamed would spend its whole
        // per-second budget on frames the client throws away.
        if self
            .draft_streams
            .lock()
            .get(message_id)
            .is_some_and(|draft| draft.content_started)
        {
            return Ok(());
        }
        if !self.draft_update_allowed(recipient, cfg.draft_update_interval_ms) {
            return Ok(());
        }
        if let Err(err) = self
            .push_stream_activity(
                recipient,
                message_id,
                &clamp_informative_text(text),
                "informative",
            )
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
        let delivered = self
            .finalize_streaming_draft(recipient, message_id, text)
            .await;
        if let Err(err) = &delivered {
            // The handle is spent whatever happened, and nothing revisits a
            // draft the orchestrator has already fallen back on. Delivery
            // clears the state itself once it owns a context, so this only
            // fires when a preflight — live config, the conversation
            // reference, or the Connector token — returned before that
            // point. Left registered, the entry would hold this chat's one
            // stream slot until it aged past [`TEAMS_STREAM_SESSION_LIMIT`]
            // and would never leave the map at all, since removal happens
            // nowhere else. Clearing is idempotent, so the paths that
            // already cleared pay nothing.
            if let Some(stream) = self.clear_draft_state(recipient, message_id) {
                // Getting a stream back means the preflight failed with the
                // bubble already up: every path that reaches the wire closes
                // it and has cleared this state itself. Taking it down needs
                // the context that could not be resolved, so nothing is
                // retried and this is the only record that a bubble was left
                // on screen.
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "stream_id": stream.id,
                            "error": format!("{err}"),
                        })),
                    "Teams draft finalize failed its preflight; its bubble stays on screen"
                );
            }
        }
        delivered
    }

    /// Best-effort removal of an abandoned draft, as when
    /// `interrupt_on_new_message` cancels a turn that a follow-up
    /// superseded. A draft whose stream never opened has nothing on the
    /// wire to take down, so cancel just drops its state. Other turns in
    /// the same conversation keep theirs.
    async fn cancel_draft(&self, recipient: &str, message_id: &str) -> Result<()> {
        let Some(stream) = self.clear_draft_state(recipient, message_id) else {
            return Ok(());
        };
        match self.send_context(recipient).await {
            Ok((_, ctx)) => {
                Self::close_stream_activity(&ctx, &stream, &Self::cancelled_notice()).await
            }
            Err(err) => {
                // Reported for the same reason a failed close is: a bubble
                // that outlived the attempt looks exactly like one that was
                // taken down. Nothing is retried here — taking the stream
                // off the screen needs the very context that could not be
                // resolved — so this is the only record that it was left
                // there. The draft's state is already gone, which is what
                // keeps the next turn in this chat able to stream.
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "stream_id": stream.id,
                            "error": format!("{err}"),
                        })),
                    "Teams draft cancelled without a Connector context; its bubble stays on screen"
                );
                Ok(())
            }
        }
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

    /// Ids alone are not a usable configuration: inbound activities would
    /// authenticate and every reply would then fail at the Entra token
    /// exchange, so an enabled channel with no secret is refused at startup
    /// rather than binding and reporting itself ready.
    #[tokio::test]
    async fn listen_requires_the_app_secret() {
        let ch = MsTeamsChannel::new(
            "default",
            Arc::new(|| {
                Some(MSTeamsConfig {
                    app_password: String::new(),
                    ..test_config()
                })
            }),
            Arc::new(Vec::new),
        );
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let err = ch.listen(tx).await.unwrap_err();
        assert!(
            err.to_string().contains("requires `app_password`"),
            "unexpected error: {err}"
        );
        assert!(
            !ch.health_check().await,
            "a channel that refused to start must not report ready"
        );
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

    /// A channel whose config block a reload can remove: the resolver then
    /// has no block to hand back, which is the one `send_context` failure
    /// that needs no network to reproduce.
    fn removable_draft_channel(
        config: MSTeamsConfig,
        connector: &MockServer,
    ) -> (
        MsTeamsChannel,
        Arc<parking_lot::Mutex<Option<MSTeamsConfig>>>,
    ) {
        let live = Arc::new(parking_lot::Mutex::new(Some(config)));
        let resolver = {
            let live = Arc::clone(&live);
            Arc::new(move || live.lock().clone())
        };
        let ch = MsTeamsChannel::new("default", resolver, Arc::new(Vec::new))
            .with_token_url(format!("{}/token", connector.uri()));
        (ch, live)
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

        // Teams does not offer paragraph delivery, and the mode reads as
        // `off` rather than opening a draft nothing here can serve.
        let multi = connector_dummy(StreamMode::MultiMessage);
        assert_eq!(multi.stream_mode(), StreamMode::Off);
        assert!(!multi.supports_draft_updates());
        assert!(!multi.supports_multi_message_streaming());
    }

    /// A configured `multi_message` must not reach the draft pipeline at
    /// all: no handle is issued, so the orchestrator delivers the answer
    /// through `send()` exactly as `off` does. Asserted from a personal
    /// chat, the one conversation type that would otherwise stream, and
    /// through `supports_draft_updates_for`, which the orchestrator consults
    /// per message.
    #[tokio::test]
    async fn multi_message_is_refused_and_delivers_like_off() {
        let connector = MockServer::start().await;
        let ch = draft_channel(
            MSTeamsConfig {
                stream_mode: StreamMode::MultiMessage,
                ..streaming_config()
            },
            &connector,
        );
        record_reference(&ch, &connector, "a:1conv", "personal");
        let msg = ChannelMessage::new("inbound", "sender", "a:1conv", "hello", "msteams", 0);
        assert!(
            !ch.supports_draft_updates_for(&msg),
            "a refused mode must not claim per-message draft support"
        );
        assert!(
            ch.send_draft(&SendMessage::new("hi", "a:1conv"))
                .await
                .unwrap()
                .is_none(),
            "a refused mode must not hand out a draft handle"
        );
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

    /// Teams refuses a final message that does not extend the content
    /// already streamed, which a tool loop hits whenever its answer is not
    /// a continuation of an earlier text segment. The caller resends the
    /// answer as an ordinary message, so finalize has to take the opened
    /// bubble down first: otherwise the reply lands underneath a draft
    /// frozen on the last streamed frame. Taking it down means closing the
    /// stream on the one text Teams cannot refuse, what it already
    /// streamed, and then deleting the message that leaves.
    #[tokio::test]
    async fn a_rejected_finalize_takes_the_abandoned_bubble_down() {
        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;
        Mock::given(method("POST"))
            .and(path("/teams/v3/conversations/a:1conv/activities"))
            .and(body_partial_json(serde_json::json!({ "type": "typing" })))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({ "id": "stream-1" })),
            )
            .mount(&connector)
            .await;
        Mock::given(method("POST"))
            .and(path("/teams/v3/conversations/a:1conv/activities"))
            .and(body_partial_json(
                serde_json::json!({ "type": "message", "text": "Hello" }),
            ))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": {
                    "code": "ContentStreamNotAllowed",
                    "message": "Request streamed content should contain the previously streamed content",
                }
            })))
            .mount(&connector)
            .await;
        let closed = Mock::given(method("POST"))
            .and(path("/teams/v3/conversations/a:1conv/activities"))
            .and(body_partial_json(serde_json::json!({
                "type": "message",
                "text": "A brown",
                "entities": [{ "streamType": "final", "streamId": "stream-1" }],
            })))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({ "id": "stream-1" })),
            )
            .expect(1)
            .named("the abandoned stream is closed on what it streamed");
        connector.register(closed).await;
        let deleted = Mock::given(method("DELETE"))
            .and(path("/teams/v3/conversations/a:1conv/activities/stream-1"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .named("the closed stream's message is deleted");
        connector.register(deleted).await;

        let ch = draft_channel(streaming_config(), &connector);
        record_reference(&ch, &connector, "a:1conv", "personal");

        let draft_id = ch
            .send_draft(&SendMessage::new("...", "a:1conv"))
            .await
            .unwrap()
            .unwrap();
        ch.update_draft("a:1conv", &draft_id, "A brown")
            .await
            .unwrap();
        let err = ch
            .finalize_draft("a:1conv", &draft_id, "Hello", false)
            .await
            .expect_err("Teams rejects a final message that drops streamed content");
        assert!(
            format!("{err}").contains("ContentStreamNotAllowed"),
            "the caller needs the rejection to trigger its plain-message fallback, got: {err}"
        );

        // Registered with `.expect(1)`, so the delete is verified on drop.
        connector.verify().await;
        assert!(
            ch.draft_streams.lock().is_empty(),
            "a finalized draft keeps no state even when the stream is rejected"
        );
    }

    /// The rejection above is the case where finalize got far enough to be
    /// told no. It can also fail before that: `send_context` resolves live
    /// config, the conversation reference, and an Entra token, and the
    /// token exchange fails for reasons that have nothing to do with this
    /// draft — an outage, a throttled token endpoint, a secret rotated to
    /// the wrong value. The orchestrator answers by resending the reply as
    /// an ordinary message and never looks at the handle again, so the
    /// state has to go on the way out. Left registered it holds this
    /// chat's one stream slot until it ages past
    /// [`TEAMS_STREAM_SESSION_LIMIT`], and since removal happens nowhere
    /// else it never leaves the map at all.
    #[tokio::test]
    async fn a_finalize_that_cannot_mint_a_token_still_frees_the_chat() {
        let connector = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&connector)
            .await;

        let ch = draft_channel(streaming_config(), &connector);
        record_reference(&ch, &connector, "a:1conv", "personal");

        let draft_id = ch
            .send_draft(&SendMessage::new("...", "a:1conv"))
            .await
            .unwrap()
            .unwrap();
        ch.finalize_draft("a:1conv", &draft_id, "Final answer", false)
            .await
            .expect_err("without a Connector token nothing can be delivered");

        assert!(
            ch.draft_streams.lock().is_empty(),
            "a draft the caller has already fallen back on must keep no state"
        );
        assert!(
            ch.send_draft(&SendMessage::new("next turn", "a:1conv"))
                .await
                .unwrap()
                .is_some(),
            "the next turn in this chat must still be able to open a stream"
        );
    }

    /// The same failure with the bubble already on screen. The token that
    /// opened the stream is inside its refresh margin by finalize time, so
    /// closing the bubble needs a fresh one and cannot get it: that bubble
    /// stays up, and no local state can change it. What must not also
    /// happen is the chat losing streaming behind a draft nobody will
    /// finalize again.
    #[tokio::test]
    async fn a_finalize_whose_token_expired_frees_the_chat_with_the_bubble_open() {
        const ACTIVITIES: &str = "/teams/v3/conversations/a:1conv/activities";

        let connector = MockServer::start().await;
        // Expires inside `CONNECTOR_TOKEN_REFRESH_MARGIN`, so the next call
        // re-mints rather than serving this one back.
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "connector-tok",
                "expires_in": 10,
            })))
            .up_to_n_times(1)
            .mount(&connector)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&connector)
            .await;
        Mock::given(method("POST"))
            .and(path(ACTIVITIES))
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
        ch.update_draft("a:1conv", &draft_id, "A brown")
            .await
            .unwrap();
        ch.finalize_draft("a:1conv", &draft_id, "A brown fox", false)
            .await
            .expect_err("the expired token cannot be replaced");

        let posted = connector
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.url.path() == ACTIVITIES)
            .count();
        assert_eq!(
            posted, 1,
            "the frame that opened the stream is all the credentials allowed"
        );
        assert!(
            ch.draft_streams.lock().is_empty(),
            "an opened draft that could not be closed still has to release its slot"
        );
        assert!(
            ch.send_draft(&SendMessage::new("next turn", "a:1conv"))
                .await
                .unwrap()
                .is_some(),
            "the next turn in this chat must still be able to open a stream"
        );
    }

    /// The config lever on the same path, and the one that needs no
    /// network: an operator deletes `[channels.msteams.default]` and
    /// reloads mid-turn, so `send_context` has no block to resolve. The
    /// draft abandoned that way must not outlive the reload that puts the
    /// section back.
    #[tokio::test]
    async fn a_finalize_after_the_config_block_is_removed_still_frees_the_chat() {
        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;

        let (ch, live) = removable_draft_channel(streaming_config(), &connector);
        record_reference(&ch, &connector, "a:1conv", "personal");

        let draft_id = ch
            .send_draft(&SendMessage::new("...", "a:1conv"))
            .await
            .unwrap()
            .unwrap();
        *live.lock() = None;
        ch.finalize_draft("a:1conv", &draft_id, "Final answer", false)
            .await
            .expect_err("a channel with no config block cannot address the Connector");

        assert!(
            ch.draft_streams.lock().is_empty(),
            "the draft must not survive the reload that abandoned it"
        );
        *live.lock() = Some(streaming_config());
        assert!(
            ch.send_draft(&SendMessage::new("next turn", "a:1conv"))
                .await
                .unwrap()
                .is_some(),
            "restoring the config must restore streaming for this chat"
        );
    }

    /// Cancel has the same ownership problem in the other order: it takes
    /// the state first and then needs a context to close the bubble with.
    /// When that context cannot be resolved the cancel still reports
    /// success, because every caller treats the takedown as best-effort,
    /// but the state must stay gone — an abandoned turn cannot cost the
    /// chat the turn that superseded it.
    #[tokio::test]
    async fn a_cancel_that_cannot_mint_a_token_still_frees_the_chat() {
        const ACTIVITIES: &str = "/teams/v3/conversations/a:1conv/activities";

        let connector = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "connector-tok",
                "expires_in": 10,
            })))
            .up_to_n_times(1)
            .mount(&connector)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&connector)
            .await;
        Mock::given(method("POST"))
            .and(path(ACTIVITIES))
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
        ch.update_draft("a:1conv", &draft_id, "A brown")
            .await
            .unwrap();
        ch.cancel_draft("a:1conv", &draft_id)
            .await
            .expect("a takedown nobody can perform is not the caller's failure");

        assert!(
            ch.draft_streams.lock().is_empty(),
            "cancel drops the draft's state whether or not the bubble could go"
        );
        assert!(
            ch.send_draft(&SendMessage::new("next turn", "a:1conv"))
                .await
                .unwrap()
                .is_some(),
            "the turn that superseded this one must be able to stream"
        );
    }

    /// Clearing twice is what makes the state release above unconditional,
    /// and the pacing map is keyed by recipient rather than by handle. The
    /// clear that no longer owns a draft therefore has to leave that key
    /// alone: the first clear frees this chat's stream slot, so the next
    /// turn can open a draft and push a frame while the finalize it belongs
    /// to is still in flight, and dropping that draft's floor would send its
    /// next frame inside the one request per second the Connector allows.
    #[tokio::test]
    async fn the_clear_that_owns_no_draft_keeps_a_newer_ones_interval_floor() {
        let connector = MockServer::start().await;
        let ch = draft_channel(streaming_config(), &connector);
        record_reference(&ch, &connector, "a:1conv", "personal");

        let abandoned = ch
            .send_draft(&SendMessage::new("...", "a:1conv"))
            .await
            .unwrap()
            .unwrap();
        ch.mark_draft_update("a:1conv");
        ch.clear_draft_state("a:1conv", &abandoned);
        assert!(
            ch.last_draft_update.lock().is_empty(),
            "the clear that owns the draft takes its floor with it"
        );

        // The next turn, opening while that finalize is still on the wire.
        let successor = ch
            .send_draft(&SendMessage::new("...", "a:1conv"))
            .await
            .unwrap()
            .unwrap();
        ch.mark_draft_update("a:1conv");

        // The tail of the first finalize, once its request came back.
        ch.clear_draft_state("a:1conv", &abandoned);
        assert!(
            ch.last_draft_update.lock().contains_key("a:1conv"),
            "the successor's interval floor must survive a clear that is not its own"
        );
        assert!(
            ch.draft_streams.lock().contains_key(&successor),
            "and so must the successor itself"
        );
    }

    /// A throttled finalize is handed straight to the caller's fallback
    /// instead of being waited out. Teams reports an expired stream as a
    /// `429` as readily as a `403`, and once the two-minute session is gone
    /// every retry is spent on a session that can no longer accept the
    /// message, delaying the answer the fallback would already have sent.
    #[tokio::test]
    async fn a_throttled_finalize_hands_over_without_retrying() {
        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;
        Mock::given(method("POST"))
            .and(path("/teams/v3/conversations/a:1conv/activities"))
            .and(body_partial_json(serde_json::json!({ "type": "typing" })))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({ "id": "stream-1" })),
            )
            .mount(&connector)
            .await;
        // One attempt only: a retried finalize would raise this count.
        Mock::given(method("POST"))
            .and(path("/teams/v3/conversations/a:1conv/activities"))
            .and(body_partial_json(
                serde_json::json!({ "type": "message", "text": "Final answer" }),
            ))
            .respond_with(ResponseTemplate::new(429).set_body_string("API calls quota exceeded"))
            .expect(1)
            .named("finalize is attempted once")
            .mount(&connector)
            .await;
        // This draft never streamed content, only a status line, so the
        // closing message falls back to the notice.
        Mock::given(method("POST"))
            .and(path("/teams/v3/conversations/a:1conv/activities"))
            .and(body_partial_json(serde_json::json!({
                "type": "message",
                "text": MsTeamsChannel::cancelled_notice(),
            })))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({ "id": "stream-1" })),
            )
            .expect(1)
            .named("the expired stream is closed")
            .mount(&connector)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/teams/v3/conversations/a:1conv/activities/stream-1"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .named("expired bubble is still taken down")
            .mount(&connector)
            .await;

        let ch = draft_channel(streaming_config(), &connector);
        record_reference(&ch, &connector, "a:1conv", "personal");

        let draft_id = ch
            .send_draft(&SendMessage::new("...", "a:1conv"))
            .await
            .unwrap()
            .unwrap();
        ch.update_draft_progress("a:1conv", &draft_id, "Running tools...")
            .await
            .unwrap();

        let started = Instant::now();
        let err = ch
            .finalize_draft("a:1conv", &draft_id, "Final answer", false)
            .await
            .expect_err("a throttled finalize is reported, not absorbed");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "handing over must not wait out a backoff, took {:?}",
            started.elapsed()
        );
        assert!(
            format!("{err}").contains("429"),
            "the caller needs the throttle reported to run its fallback, got: {err}"
        );
        connector.verify().await;
    }

    /// Teams stops rendering informative updates once content streaming
    /// begins and discards later ones, so a tool loop that resumes after
    /// some answer text has streamed must not spend the stream's
    /// one-per-second budget on frames the client throws away.
    #[tokio::test]
    async fn status_lines_stop_once_content_has_streamed() {
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
        ch.update_draft_progress("a:1conv", &draft_id, "Running tools...")
            .await
            .unwrap();
        ch.update_draft("a:1conv", &draft_id, "A brown")
            .await
            .unwrap();
        ch.update_draft_progress("a:1conv", &draft_id, "Running more tools...")
            .await
            .unwrap();
        ch.update_draft("a:1conv", &draft_id, "A brown fox")
            .await
            .unwrap();

        let requests = connector.received_requests().await.unwrap();
        let texts =
            activity_texts_for_path(&requests, "/teams/v3/conversations/a:1conv/activities");
        assert_eq!(
            texts,
            vec!["Running tools...", "A brown", "A brown fox"],
            "the status line after the first content frame must not be sent"
        );
    }

    /// The strip must not touch a reply that only talks about the tags, which
    /// is the case the shared helper is built to keep.
    #[tokio::test]
    async fn prose_about_tool_tags_is_sent_verbatim() {
        const ACTIVITIES: &str = "/teams/v3/conversations/a:1conv/activities";
        let prose =
            "The bug is that models emit <function_calls> and never close it, hanging the parser.";

        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;
        Mock::given(method("POST"))
            .and(path(ACTIVITIES))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({ "id": "m" })),
            )
            .mount(&connector)
            .await;

        let ch = draft_channel(test_config(), &connector);
        record_reference(&ch, &connector, "a:1conv", "personal");
        ch.send(&SendMessage::new(prose, "a:1conv")).await.unwrap();

        assert_eq!(
            activity_texts_for_path(&connector.received_requests().await.unwrap(), ACTIVITIES),
            vec![prose.to_string()]
        );
    }

    /// The other half of that strip: a message the strip empties had nothing
    /// to say, and Teams rejects an empty activity, so nothing is POSTed
    /// rather than a blank bubble taking the answer's place.
    #[tokio::test]
    async fn a_message_that_is_only_a_tool_envelope_is_not_sent() {
        const ACTIVITIES: &str = "/teams/v3/conversations/a:1conv/activities";

        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;
        Mock::given(method("POST"))
            .and(path(ACTIVITIES))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({ "id": "m" })),
            )
            .mount(&connector)
            .await;

        let ch = draft_channel(test_config(), &connector);
        record_reference(&ch, &connector, "a:1conv", "personal");
        ch.send(&SendMessage::new(
            "<tool_call>{\"name\":\"shell\"}</tool_call>",
            "a:1conv",
        ))
        .await
        .unwrap();

        assert!(
            activity_texts_for_path(&connector.received_requests().await.unwrap(), ACTIVITIES)
                .is_empty(),
            "an envelope-only message must not reach the conversation"
        );
    }

    /// Microsoft caps an informative frame at "1 kb or 1000 characters", so
    /// both bounds hold: ASCII trips the character count, other scripts the
    /// byte count. A line that already fits is passed through untouched.
    #[test]
    fn informative_frames_are_clamped_to_both_documented_bounds() {
        let short = "Running tools...";
        assert!(matches!(clamp_informative_text(short), Cow::Borrowed(_)));
        assert_eq!(clamp_informative_text(short), short);

        let ascii = "x".repeat(5_000);
        let clamped = clamp_informative_text(&ascii);
        assert!(clamped.chars().count() <= TEAMS_MAX_INFORMATIVE_CHARS);
        assert!(clamped.len() <= TEAMS_MAX_INFORMATIVE_BYTES);
        assert!(clamped.ends_with('…'), "a shortened line is marked as such");

        // 1000 CJK characters sit inside the character bound but at three
        // times the byte bound, so this one has to be cut on bytes.
        let cjk = "状".repeat(1_000);
        let clamped = clamp_informative_text(&cjk);
        assert!(clamped.len() <= TEAMS_MAX_INFORMATIVE_BYTES);
        assert!(clamped.chars().count() < TEAMS_MAX_INFORMATIVE_CHARS);
        assert!(clamped.ends_with('…'));
    }

    /// Every frame carries the whole answer so far and Teams holds a stream
    /// to the same size ceiling as a plain message, so past that ceiling the
    /// channel stops pushing frames that could only be refused.
    #[tokio::test]
    async fn streaming_frames_stop_once_the_response_outgrows_a_message() {
        const ACTIVITIES: &str = "/teams/v3/conversations/a:1conv/activities";

        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;
        Mock::given(method("POST"))
            .and(path(ACTIVITIES))
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

        ch.update_draft("a:1conv", &draft_id, "A brown")
            .await
            .unwrap();
        let oversize = "x".repeat(TEAMS_MAX_MESSAGE_CHARS + 1);
        ch.update_draft("a:1conv", &draft_id, &oversize)
            .await
            .unwrap();
        // Every later delta lands in the same branch; the draft reports the
        // stopped stream once and stays silent afterwards.
        ch.update_draft("a:1conv", &draft_id, &format!("{oversize}x"))
            .await
            .unwrap();
        assert!(
            ch.draft_streams
                .lock()
                .get(&draft_id)
                .is_some_and(|draft| draft.size_exceeded)
        );

        let requests = connector.received_requests().await.unwrap();
        assert_eq!(
            activity_texts_for_path(&requests, ACTIVITIES),
            vec!["A brown".to_string()],
            "no frame past the size ceiling may be posted"
        );
    }

    /// A stream closes with a single activity and cannot chunk it, so an
    /// oversize answer takes the bubble down and arrives as split plain
    /// messages rather than as one activity Teams would refuse.
    #[tokio::test]
    async fn an_oversize_final_takes_the_bubble_down_and_splits() {
        const ACTIVITIES: &str = "/teams/v3/conversations/a:1conv/activities";

        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;
        Mock::given(method("POST"))
            .and(path(ACTIVITIES))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({ "id": "stream-1" })),
            )
            .mount(&connector)
            .await;
        let deleted = Mock::given(method("DELETE"))
            .and(path(format!("{ACTIVITIES}/stream-1")))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .named("the bubble a split answer cannot close is deleted");
        connector.register(deleted).await;

        let ch = draft_channel(streaming_config(), &connector);
        record_reference(&ch, &connector, "a:1conv", "personal");
        let draft_id = ch
            .send_draft(&SendMessage::new("...", "a:1conv"))
            .await
            .unwrap()
            .unwrap();
        ch.update_draft("a:1conv", &draft_id, "A brown")
            .await
            .unwrap();

        let oversize = "x".repeat(TEAMS_MAX_MESSAGE_CHARS + 500);
        ch.finalize_draft("a:1conv", &draft_id, &oversize, false)
            .await
            .expect("an oversize answer is delivered as chunks, not refused");

        let requests = connector.received_requests().await.unwrap();
        let texts = activity_texts_for_path(&requests, ACTIVITIES);
        assert_eq!(
            texts.len(),
            4,
            "the opening frame, the message that closes the stream, then a two-way split"
        );
        assert_eq!(
            texts[1], "A brown",
            "the stream closes on what it streamed, the only text Teams cannot refuse here"
        );
        assert_eq!(
            texts[2..].concat(),
            oversize,
            "the answer must arrive whole across the chunks"
        );
        let bodies: Vec<serde_json::Value> = requests
            .iter()
            .filter(|request| request.url.path() == ACTIVITIES)
            .map(|request| serde_json::from_slice(&request.body).unwrap())
            .collect();
        assert_eq!(
            bodies[1]["entities"][0]["streamType"], "final",
            "closing the stream is what ends the bubble; a delete alone does not"
        );
        assert!(
            bodies[2..].iter().all(|body| body["entities"].is_null()),
            "the chunks are ordinary messages, not a stream's final activity"
        );
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

    /// Activity texts POSTed to `path`, in order.
    fn activity_texts_for_path(requests: &[wiremock::Request], path: &str) -> Vec<String> {
        requests
            .iter()
            .filter(|request| request.url.path() == path)
            .map(|request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                body["text"].as_str().unwrap_or_default().to_string()
            })
            .collect()
    }

    /// The outbound typing indicator is a one-shot Bot Framework `typing`
    /// activity; `stop_typing` posts nothing.
    #[tokio::test]
    async fn start_typing_posts_typing_activity() {
        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;
        Mock::given(method("POST"))
            .and(path(
                "/teams/v3/conversations/19:group@thread.v2/activities",
            ))
            .and(body_partial_json(serde_json::json!({ "type": "typing" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&connector)
            .await;

        let ch = draft_channel(test_config(), &connector);
        record_reference(&ch, &connector, "19:group@thread.v2", "groupChat");

        ch.start_typing("19:group@thread.v2").await.unwrap();
        ch.stop_typing("19:group@thread.v2").await.unwrap();
    }

    /// Teams allows one streaming response per chat at a time. Two turns can
    /// be in flight in one conversation whenever `interrupt_on_new_message`
    /// is off, so the second must go without a draft rather than open a
    /// stream Teams will not start.
    #[tokio::test]
    async fn a_second_turn_in_one_chat_does_not_open_a_second_stream() {
        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;
        let ch = draft_channel(streaming_config(), &connector);
        record_reference(&ch, &connector, "a:1conv", "personal");
        record_reference(&ch, &connector, "a:2conv", "personal");

        let first = ch
            .send_draft(&SendMessage::new("hi", "a:1conv"))
            .await
            .unwrap();
        assert!(first.is_some(), "the first turn streams");
        assert!(
            ch.send_draft(&SendMessage::new("again", "a:1conv"))
                .await
                .unwrap()
                .is_none(),
            "a concurrent turn in the same chat must not open a second stream"
        );
        // A different chat has its own allowance.
        assert!(
            ch.send_draft(&SendMessage::new("hi", "a:2conv"))
                .await
                .unwrap()
                .is_some(),
            "the limit is per chat, not per bot"
        );

        // Once the first turn ends, the chat can stream again.
        ch.cancel_draft("a:1conv", first.as_deref().unwrap())
            .await
            .unwrap();
        assert!(
            ch.send_draft(&SendMessage::new("next", "a:1conv"))
                .await
                .unwrap()
                .is_some(),
            "the next turn streams once the previous draft is gone"
        );
    }

    /// A draft that no path ever finalized or cancelled must not cost the
    /// chat its streaming for the life of the process: past Teams' own
    /// two-minute session limit the stale stream is dead anyway.
    #[tokio::test]
    async fn a_draft_older_than_the_session_limit_stops_blocking_the_chat() {
        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;
        let ch = draft_channel(streaming_config(), &connector);
        record_reference(&ch, &connector, "a:1conv", "personal");

        let stale = ch
            .send_draft(&SendMessage::new("hi", "a:1conv"))
            .await
            .unwrap()
            .unwrap();
        assert!(
            ch.send_draft(&SendMessage::new("again", "a:1conv"))
                .await
                .unwrap()
                .is_none()
        );

        if let Some(draft) = ch.draft_streams.lock().get_mut(&stale) {
            draft.opened_at = std::time::Instant::now() - TEAMS_STREAM_SESSION_LIMIT;
        }
        assert!(
            ch.send_draft(&SendMessage::new("later", "a:1conv"))
                .await
                .unwrap()
                .is_some(),
            "a stream Teams has already ended cannot hold the chat's slot"
        );
    }

    /// Teams draws no typing indicator in a team channel, and the Connector
    /// does not say so: it answers 202 and the channel shows nothing.
    /// Refreshed every few seconds for the length of a turn, those requests
    /// are pure cost against the same rate limit the reply needs, so the
    /// channel must not even acquire a token for them.
    #[tokio::test]
    async fn typing_is_not_posted_in_a_team_channel() {
        const CONVERSATION: &str = "19:general@thread.tacv2";
        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/teams/v3/conversations/{CONVERSATION}/activities"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(0)
            .mount(&connector)
            .await;

        let ch = draft_channel(test_config(), &connector);
        record_reference(&ch, &connector, CONVERSATION, "channel");

        ch.start_typing(CONVERSATION).await.unwrap();
        // A thread-suffixed recipient resolves to the same channel reference,
        // so it is skipped on the same grounds.
        ch.start_typing(&format!("{CONVERSATION};messageid=1700"))
            .await
            .unwrap();
        ch.stop_typing(CONVERSATION).await.unwrap();

        assert!(
            connector.received_requests().await.unwrap().is_empty(),
            "a team-channel turn must not spend requests on an indicator Teams \
             never renders"
        );
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

    /// Cancelling closes the stream before deleting it. A delete on its own
    /// is answered `2xx` by Teams and takes the activity off the service,
    /// but the client keeps rendering the bubble, with a Stop button that
    /// then fails because the stream it would stop is gone. Only the final
    /// message ends the stream client-side.
    #[tokio::test]
    async fn cancel_draft_closes_the_stream_before_deleting_it() {
        const ACTIVITIES: &str = "/teams/v3/conversations/a:1conv/activities";

        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;
        Mock::given(method("POST"))
            .and(path(ACTIVITIES))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({ "id": "stream-1" })),
            )
            .mount(&connector)
            .await;
        Mock::given(method("DELETE"))
            .and(path(format!("{ACTIVITIES}/stream-1")))
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
        // Open the stream so there is a bubble on screen to take down.
        ch.update_draft("a:1conv", &draft_id, "partial")
            .await
            .unwrap();
        ch.cancel_draft("a:1conv", &draft_id).await.unwrap();

        let requests = connector.received_requests().await.unwrap();
        let closing: Vec<serde_json::Value> = requests
            .iter()
            .filter(|request| request.url.path() == ACTIVITIES)
            .map(|request| serde_json::from_slice(&request.body).unwrap())
            .filter(|body: &serde_json::Value| body["type"] == "message")
            .collect();
        assert_eq!(closing.len(), 1, "the stream is closed exactly once");
        assert_eq!(closing[0]["entities"][0]["streamType"], "final");
        assert_eq!(closing[0]["entities"][0]["streamId"], "stream-1");
        assert_eq!(
            closing[0]["text"], "partial",
            "a final message must carry what was streamed, so it closes on that"
        );
        assert!(ch.draft_streams.lock().is_empty());
        assert!(ch.last_draft_update.lock().is_empty());
    }

    /// With nothing streamed but status lines there is no content the
    /// closing message has to carry, and Teams still needs a final message
    /// to end the stream. The notice covers it, and only reaches the screen
    /// if the delete that follows does not land.
    #[tokio::test]
    async fn a_draft_that_only_showed_status_closes_on_the_notice() {
        const ACTIVITIES: &str = "/teams/v3/conversations/a:1conv/activities";

        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;
        Mock::given(method("POST"))
            .and(path(ACTIVITIES))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({ "id": "stream-1" })),
            )
            .mount(&connector)
            .await;
        Mock::given(method("DELETE"))
            .and(path(format!("{ACTIVITIES}/stream-1")))
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
        ch.update_draft_progress("a:1conv", &draft_id, "Thinking...")
            .await
            .unwrap();
        ch.cancel_draft("a:1conv", &draft_id).await.unwrap();

        let requests = connector.received_requests().await.unwrap();
        let closing: Vec<serde_json::Value> = requests
            .iter()
            .filter(|request| request.url.path() == ACTIVITIES)
            .map(|request| serde_json::from_slice(&request.body).unwrap())
            .filter(|body: &serde_json::Value| body["type"] == "message")
            .collect();
        assert_eq!(closing.len(), 1, "the stream is closed exactly once");
        assert_eq!(
            closing[0]["text"],
            MsTeamsChannel::cancelled_notice(),
            "a status line is not content, so it cannot be what the stream closes on"
        );
    }

    /// A refused takedown has to reach the caller. Teams ends a stream
    /// through a final message, the user's Stop button, or the two-minute
    /// limit, and a delete is not part of that contract, so the bubble can
    /// well outlive the request. Reporting `Ok` either way would make a
    /// stranded bubble indistinguishable from a removed one, on the wire and
    /// in the logs.
    #[tokio::test]
    async fn a_refused_bubble_takedown_is_reported_not_swallowed() {
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
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": {
                    "code": "ContentStreamNotAllowed",
                    "message": "Content stream finished due to exceeded streaming time.",
                }
            })))
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
        ch.update_draft("a:1conv", &draft_id, "partial")
            .await
            .unwrap();

        let cancelled = ch.cancel_draft("a:1conv", &draft_id).await;
        assert!(
            cancelled.is_err(),
            "a bubble Teams refused to remove must not be reported as cancelled"
        );
        // Local state goes regardless: the draft is abandoned either way, and
        // keeping it would cost the chat its one stream until the entry ages
        // out.
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

    /// The Connector token is password-equivalent, so TLS is required for
    /// every destination that is not a local mock.
    #[test]
    fn only_https_and_loopback_http_destinations_may_carry_the_token() {
        for allowed in [
            "https://smba.trafficmanager.net/teams/v3/conversations/a:1/activities",
            "https://localhost/teams/",
            "http://127.0.0.1:8080/teams/",
            "http://127.1.2.3/teams/",
            "http://[::1]:8080/teams/",
            "http://localhost:3978/teams/",
        ] {
            let url = url::Url::parse(allowed).unwrap();
            assert!(
                MsTeamsChannel::require_tls_destination(&url).is_ok(),
                "{allowed} should be allowed"
            );
        }
        for rejected in [
            // A public plain-HTTP host: the case Microsoft never produces.
            "http://smba.trafficmanager.net/teams/",
            "http://192.168.1.10/teams/",
            // `localhost.` and lookalike hosts are not loopback.
            "http://localhost.evil.test/teams/",
            "http://notlocalhost/teams/",
            // Neither is a non-HTTP scheme, host or not.
            "ftp://smba.trafficmanager.net/teams/",
            "file:///teams/",
        ] {
            let url = url::Url::parse(rejected).unwrap();
            let err = MsTeamsChannel::require_tls_destination(&url)
                .expect_err("{rejected} should be rejected");
            assert!(
                err.to_string().contains("non-HTTPS destination"),
                "unexpected error for {rejected}: {err}"
            );
        }
    }

    /// The token is acquired before the destination is known, so the guard
    /// has to stop the request that would hand it over rather than rely on
    /// the destination never being configured.
    #[tokio::test]
    async fn send_to_a_plain_http_service_url_never_leaves_with_the_token() {
        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;

        let ch = draft_channel(test_config(), &connector);
        // A service URL that is neither TLS nor loopback. Reaching the
        // network at all would be the failure: the error must come from the
        // guard, not from a DNS or connect attempt. The address is TEST-NET-1
        // (RFC 5737), so a build that lost the guard cannot hand the token
        // to anything real either.
        ch.conversations.record(ConversationReference {
            service_url: "http://192.0.2.10/teams/".to_string(),
            conversation_id: "a:1conv".to_string(),
            conversation_type: Some("personal".to_string()),
        });

        let err = ch
            .send(&SendMessage::new("hi", "a:1conv"))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("non-HTTPS destination"),
            "expected the TLS guard to refuse the send, got: {err}"
        );
    }

    /// Teams' hint wins over the local schedule, since it knows which of the
    /// per-second, per-30s and per-hour windows was actually hit.
    #[test]
    fn retry_delay_prefers_retry_after_and_stays_bounded() {
        assert_eq!(
            MsTeamsChannel::connector_retry_delay(0, Some(Duration::from_secs(2))),
            Duration::from_secs(2)
        );
        // A hint past the ceiling is clamped: waiting one out would overrun
        // the deadlines behind `CONNECTOR_RETRY_MAX_DELAY_MS` anyway.
        assert_eq!(
            MsTeamsChannel::connector_retry_delay(0, Some(Duration::from_secs(600))),
            Duration::from_millis(CONNECTOR_RETRY_MAX_DELAY_MS)
        );
        // Without a hint: doubling per attempt, ±25% jitter.
        for (attempt, low, high) in [(0, 750, 1_250), (1, 1_500, 2_500), (2, 3_000, 5_000)] {
            for _ in 0..32 {
                let delay = MsTeamsChannel::connector_retry_delay(attempt, None).as_millis();
                assert!(
                    (low..=high).contains(&delay),
                    "attempt {attempt} delay {delay}ms outside [{low}, {high}]"
                );
            }
        }
        // A shift wide enough to overflow the multiplier still yields a
        // bounded wait rather than panicking or waiting forever.
        assert_eq!(
            MsTeamsChannel::connector_retry_delay(u32::MAX, None),
            Duration::from_millis(CONNECTOR_RETRY_MAX_DELAY_MS)
        );
        // The budget's reason for being: even the unluckiest jitter across
        // every wait must outlast the 2s window, the tightest one a reply's
        // own burst can fill. Rewriting the base or the attempt count without
        // rechecking this is the mistake this guards.
        let worst_case: u64 = (0..CONNECTOR_MAX_ATTEMPTS - 1)
            .map(|attempt| {
                let multiplier = 1_u64 << attempt;
                // The floor of the ±25% jitter band for this attempt.
                CONNECTOR_RETRY_BASE_DELAY_MS * multiplier * 3 / 4
            })
            .sum();
        assert!(
            worst_case > 2_000,
            "retry budget only reaches {worst_case}ms, short of the 2s window"
        );
    }

    #[test]
    fn retry_after_reads_delay_seconds_and_ignores_other_forms() {
        let parse = |value: &str| {
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(reqwest::header::RETRY_AFTER, value.parse().unwrap());
            MsTeamsChannel::parse_retry_after(&headers)
        };
        assert_eq!(parse("3"), Some(Duration::from_secs(3)));
        assert_eq!(parse("  3  "), Some(Duration::from_secs(3)));
        assert_eq!(parse("0"), Some(Duration::ZERO));
        // The HTTP-date form is deliberately not honored, and garbage falls
        // back to the local backoff rather than to no wait at all.
        assert_eq!(parse("Wed, 21 Oct 2015 07:28:00 GMT"), None);
        assert_eq!(parse("soon"), None);
        assert_eq!(
            MsTeamsChannel::parse_retry_after(&reqwest::header::HeaderMap::new()),
            None
        );
    }

    /// Teams requires callers to wait out a `429` rather than surface it: a
    /// throttled burst is expected traffic, not a failed reply.
    #[tokio::test]
    async fn throttled_send_is_retried_and_still_delivered() {
        const ACTIVITIES: &str = "/teams/v3/conversations/a:1conv/activities";

        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;
        Mock::given(method("POST"))
            .and(path(ACTIVITIES))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
            .up_to_n_times(1)
            .mount(&connector)
            .await;
        Mock::given(method("POST"))
            .and(path(ACTIVITIES))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({ "id": "m" })),
            )
            .mount(&connector)
            .await;
        // Separate conversation, so warming the token below cannot consume
        // the throttled response staged above.
        Mock::given(method("POST"))
            .and(path("/teams/v3/conversations/a:warm/activities"))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({ "id": "w" })),
            )
            .mount(&connector)
            .await;

        let ch = draft_channel(test_config(), &connector);
        record_reference(&ch, &connector, "a:1conv", "personal");
        record_reference(&ch, &connector, "a:warm", "personal");
        // The connector token is fetched once and cached, so acquiring it
        // here keeps it out of the measured window below.
        ch.send(&SendMessage::new("warm", "a:warm")).await.unwrap();

        let started = Instant::now();
        ch.send(&SendMessage::new("hello", "a:1conv"))
            .await
            .expect("a throttled send must be retried, not failed");
        let elapsed = started.elapsed();

        let requests = connector.received_requests().await.unwrap();
        assert_eq!(
            activity_texts_for_path(&requests, ACTIVITIES),
            vec!["hello".to_string(), "hello".to_string()],
            "the same text should be re-POSTed once after the 429"
        );
        // `Retry-After: 0` was honored: the local floor alone would have
        // waited about half a second.
        assert!(
            elapsed < Duration::from_millis(CONNECTOR_RETRY_BASE_DELAY_MS / 2),
            "Retry-After was ignored in favor of the backoff, waited {elapsed:?}"
        );
    }

    /// A throttled streaming frame is skipped, not waited on. `update_draft`
    /// is awaited by the agent's token loop, so retrying here would stall
    /// token delivery for seconds to redeliver a frame the next update
    /// supersedes anyway.
    #[tokio::test]
    async fn throttled_streaming_frame_is_skipped_without_retrying() {
        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;
        Mock::given(method("POST"))
            .and(path("/teams/v3/conversations/a:1conv/activities"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "30"))
            .mount(&connector)
            .await;

        let ch = draft_channel(streaming_config(), &connector);
        record_reference(&ch, &connector, "a:1conv", "personal");
        let draft_id = ch
            .send_draft(&SendMessage::new("hi", "a:1conv"))
            .await
            .unwrap()
            .unwrap();

        let started = Instant::now();
        ch.update_draft("a:1conv", &draft_id, "partial text")
            .await
            .expect("a throttled frame is a skip, not a turn-ending error");
        let elapsed = started.elapsed();

        let activity_posts = connector
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().ends_with("/activities"))
            .count();
        assert_eq!(activity_posts, 1, "the frame must not be re-POSTed");
        // Honoring the 30s hint here (capped at 10s) would have blocked the
        // token loop; failing fast returns at once.
        assert!(
            elapsed < Duration::from_millis(CONNECTOR_RETRY_BASE_DELAY_MS),
            "streaming frame blocked for {elapsed:?}"
        );
    }

    /// The retry budget is bounded: a conversation that stays throttled
    /// gets an error the caller can report, not an unbounded wait.
    #[tokio::test]
    async fn persistently_throttled_send_fails_after_bounded_attempts() {
        const ACTIVITIES: &str = "/teams/v3/conversations/a:1conv/activities";

        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;
        Mock::given(method("POST"))
            .and(path(ACTIVITIES))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("Retry-After", "0")
                    .set_body_string("API calls quota exceeded"),
            )
            .mount(&connector)
            .await;

        let ch = draft_channel(test_config(), &connector);
        record_reference(&ch, &connector, "a:1conv", "personal");

        let err = ch
            .send(&SendMessage::new("hello", "a:1conv"))
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("throttled after"), "unexpected error: {text}");
        assert!(
            text.contains("API calls quota exceeded"),
            "body dropped: {text}"
        );

        let requests = connector.received_requests().await.unwrap();
        assert_eq!(
            activity_texts_for_path(&requests, ACTIVITIES).len(),
            CONNECTOR_MAX_ATTEMPTS as usize,
            "attempts must stop at the budget"
        );
    }

    /// Every chunk of a split reply is its own "send to conversation"
    /// operation against Teams' 7-per-second ceiling, so the chunks are
    /// spaced. A reply that fits in one activity pays nothing.
    #[tokio::test]
    async fn split_reply_paces_its_chunks_while_a_single_chunk_does_not_wait() {
        const ACTIVITIES: &str = "/teams/v3/conversations/a:1conv/activities";

        let connector = MockServer::start().await;
        mock_token_endpoint(&connector).await;
        Mock::given(method("POST"))
            .and(path(ACTIVITIES))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({ "id": "m" })),
            )
            .mount(&connector)
            .await;

        let ch = draft_channel(test_config(), &connector);
        record_reference(&ch, &connector, "a:1conv", "personal");

        // The connector token is fetched once and cached, so acquiring it
        // here keeps it out of the measured windows below.
        ch.send(&SendMessage::new("warm", "a:1conv")).await.unwrap();

        let started = Instant::now();
        ch.send(&SendMessage::new("short", "a:1conv"))
            .await
            .unwrap();
        assert!(
            started.elapsed() < TEAMS_CHUNK_SEND_SPACING,
            "an unsplit reply must not wait for spacing it does not need"
        );

        // Three chunks: two gaps.
        let oversize = "x".repeat(TEAMS_MAX_MESSAGE_CHARS * 2 + 10);
        let started = Instant::now();
        ch.send(&SendMessage::new(&oversize, "a:1conv"))
            .await
            .unwrap();
        let elapsed = started.elapsed();

        let requests = connector.received_requests().await.unwrap();
        let chunks = activity_texts_for_path(&requests, ACTIVITIES);
        // One each from the warm-up and the short send, three from the split.
        assert_eq!(chunks.len(), 5, "expected a 3-way split");
        assert!(
            elapsed >= TEAMS_CHUNK_SEND_SPACING * 2,
            "chunks were not paced, sent 3 in {elapsed:?}"
        );
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
