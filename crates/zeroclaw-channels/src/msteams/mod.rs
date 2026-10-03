//! Microsoft Teams bot channel (Azure Bot Service / Bot Framework).
//!
//! Inbound: Teams POSTs Bot Framework activities to a channel-hosted axum
//! listener (the operator registers its public URL as the Azure Bot
//! messaging endpoint); every request is JWT-validated against the Bot
//! Framework JWKS before the body is touched. Outbound: proactive POSTs to
//! the Bot Connector API at the `service_url` carried by each inbound
//! activity, authenticated with a cached Entra client-credentials token.
//!
//! Each turn produces one reply, posted when the answer is complete, and a
//! reply that exceeds Teams' per-activity limit is split across consecutive
//! messages. While the turn runs, a personal or group chat shows the ordinary
//! typing indicator; Teams draws no such indicator in a team channel, so one
//! is not posted there.
//!
//! Teams' native streaming protocol (the gray in-progress bubble) is not
//! used. It requires each frame to carry the whole response so far and to
//! only extend what the previous frame published, which cannot be reconciled
//! with redacting a credential the model emits across several frames: the
//! frame that renders a not-yet-recognizable prefix cannot be retracted.
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
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_config::schema::MSTeamsConfig;

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

/// A startup failure no retry can resolve, constructed by [`MsTeamsChannel::listen`]
/// and downcast by the orchestrator's channel supervisor. Missing credentials
/// fail identically on every attempt, so restarting on a backoff only fills the
/// log; the supervisor parks the listener instead and waits for a config change
/// or shutdown. Modelled on Discord's `DiscordListenerFatalError`, and
/// deliberately narrow: a bind conflict or an unreachable Entra endpoint is
/// transient and stays on the retry path.
#[derive(Debug)]
pub(crate) struct MsTeamsListenerFatalError {
    message: String,
}

impl MsTeamsListenerFatalError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for MsTeamsListenerFatalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for MsTeamsListenerFatalError {}

/// Resolved per-call context for outbound Connector requests.
struct SendContext {
    reference: ConversationReference,
    base_id: String,
    client: reqwest::Client,
    token: String,
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
/// narrower than "carries content". A typing indicator carries none: it is
/// superseded by the reply itself, its caller already treats any error as
/// "skip", and waiting on it would only stall the turn it is meant to
/// announce. A reply is the opposite — nothing follows it to carry the answer
/// again — so it waits the throttle out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThrottlePolicy {
    /// Losing the request loses content with nothing behind it: wait out a
    /// `429` (see [`CONNECTOR_MAX_ATTEMPTS`]) rather than drop it.
    Retry,
    /// Something behind the request covers it: report a `429` immediately so
    /// that takes over.
    FailFast,
}

/// Ceiling on a single backoff wait, including a `Retry-After` Teams asks
/// for.
///
/// Every retrying request delivers a reply or one of its split chunks, so one
/// deadline covers them all: the per-turn budget,
/// `channels.message_timeout_secs`, 300s by default. A 10s ceiling keeps two
/// waits well inside that budget, where obeying an arbitrarily long hint would
/// fail the turn more surely than giving up early does.
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

    /// Post `text` as ordered chunks, stopping at the first refusal.
    ///
    /// Teams rejects any single activity past ~100 KB (413
    /// `MessageSizeTooBig`), so oversize content is split, preferring
    /// paragraph, then line, then word boundaries; a long reply then lands in
    /// full instead of failing outright, and the common in-budget case is a
    /// single chunk, unchanged from a plain send.
    ///
    /// A refusal ends the send rather than skipping ahead, since the chunks
    /// are ordered, and it is final: [`Self::activity_request`] retries only a
    /// `429`, for the reason given there.
    async fn post_in_chunks(
        ctx: &SendContext,
        url: &url::Url,
        text: &str,
        in_reply_to: Option<&str>,
    ) -> Result<()> {
        let chunks = split_message_for_teams(text);
        for (index, chunk) in chunks.iter().enumerate() {
            let mut body = serde_json::json!({ "type": "message", "text": chunk });
            if let Some(reply_to_id) = in_reply_to {
                body["replyToId"] = serde_json::Value::String(reply_to_id.to_string());
            }
            Self::activity_request(
                ctx,
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

    /// Issue a Connector API request; returns the activity id from the
    /// response body when the Connector provides one.
    ///
    /// Under [`ThrottlePolicy::Retry`] a `429` is waited out and the request
    /// reissued, for [`CONNECTOR_MAX_ATTEMPTS`] attempts in all, which Teams
    /// requires of every caller:
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
        // The transport-level backstop Telegram, Discord, WeChat and WhatsApp
        // Web also keep. The orchestrator strips envelopes from assistant text
        // before a reply, but nothing in the trait obliges a caller to have
        // run that pass, and this method also carries split chunks. Stripping
        // once here covers all of them.
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
        Self::post_in_chunks(&ctx, &url, &content, message.in_reply_to.as_deref()).await
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        let Some(cfg) = self.config() else {
            return Err(anyhow::Error::new(MsTeamsListenerFatalError::new(format!(
                "Microsoft Teams channel '{}' has no [channels.msteams.{}] config block",
                self.alias, self.alias
            ))));
        };
        if cfg.app_id.trim().is_empty() || cfg.tenant_id.trim().is_empty() {
            return Err(anyhow::Error::new(MsTeamsListenerFatalError::new(format!(
                "Microsoft Teams channel '{}' requires `app_id` and `tenant_id`: `app_id` is \
                 the audience inbound activities are authenticated against and `tenant_id` \
                 names the Entra tenant outbound tokens come from; set them under \
                 [channels.msteams.{}]",
                self.alias, self.alias,
            ))));
        }
        // The secret is as load-bearing as the two ids above: Entra mints
        // every Connector token from it, so an enabled channel without one
        // would bind, report ready, accept activities, and then fail each
        // reply at the token exchange. Refusing at startup puts that in the
        // operator's log instead of one error per message.
        if cfg.app_password.trim().is_empty() {
            return Err(anyhow::Error::new(MsTeamsListenerFatalError::new(format!(
                "Microsoft Teams channel '{}' requires `app_password`: Entra mints the \
                 Connector token from it, so without it every reply fails; set it under \
                 [channels.msteams.{}]",
                self.alias, self.alias,
            ))));
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
    /// interval for the duration of the turn. Personal and group chats both
    /// show it; a team channel draws no indicator at all and is dropped
    /// below.
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{Algorithm, EncodingKey, Header};
    use serde::Serialize;
    use std::time::Instant;
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
        assert!(
            err.downcast_ref::<MsTeamsListenerFatalError>().is_some(),
            "no retry resolves a missing id, so the supervisor has to be able to \
             recognise this as fatal rather than restart on a backoff: {err}"
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
            err.downcast_ref::<MsTeamsListenerFatalError>().is_some(),
            "no retry mints a secret, so the supervisor has to be able to recognise \
             this as fatal rather than restart on a backoff: {err}"
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

    /// A channel that mints its tokens from the mock connector, so a test can
    /// exercise a send without reaching the real Entra endpoint.
    fn connected_channel(config: MSTeamsConfig, connector: &MockServer) -> MsTeamsChannel {
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

        let ch = connected_channel(test_config(), &connector);
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

        let ch = connected_channel(test_config(), &connector);
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

        let ch = connected_channel(test_config(), &connector);
        record_reference(&ch, &connector, "19:group@thread.v2", "groupChat");

        ch.start_typing("19:group@thread.v2").await.unwrap();
        ch.stop_typing("19:group@thread.v2").await.unwrap();
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

        let ch = connected_channel(test_config(), &connector);
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

        let ch = connected_channel(test_config(), &connector);
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

        let ch = connected_channel(test_config(), &connector);
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

        let ch = connected_channel(test_config(), &connector);
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

    /// A gateway error after the Connector has already recorded the activity
    /// leaves the delivery outcome unknown. Posting again, or moving on to the
    /// next chunk, could show the reader text twice, so the send stops after
    /// that one request and reports the error. The orchestrator does not
    /// resend a failed plain reply either, so this is the only attempt.
    #[tokio::test]
    async fn an_ambiguous_gateway_failure_is_posted_once_and_not_continued() {
        const ACTIVITIES: &str = "/teams/v3/conversations/a:1conv/activities";

        for text in [
            "in budget".to_string(),
            "x ".repeat(TEAMS_MAX_MESSAGE_CHARS),
        ] {
            let connector = MockServer::start().await;
            mock_token_endpoint(&connector).await;
            Mock::given(method("POST"))
                .and(path(ACTIVITIES))
                .respond_with(ResponseTemplate::new(502).set_body_string("Bad Gateway"))
                .mount(&connector)
                .await;

            let ch = connected_channel(test_config(), &connector);
            record_reference(&ch, &connector, "a:1conv", "personal");

            let err = ch
                .send(&SendMessage::new(&text, "a:1conv"))
                .await
                .unwrap_err();
            assert!(err.to_string().contains("502"), "unexpected error: {err}");

            let requests = connector.received_requests().await.unwrap();
            assert_eq!(
                activity_texts_for_path(&requests, ACTIVITIES).len(),
                1,
                "a {}-char reply must be attempted exactly once",
                text.len()
            );
        }
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

        let ch = connected_channel(test_config(), &connector);
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
