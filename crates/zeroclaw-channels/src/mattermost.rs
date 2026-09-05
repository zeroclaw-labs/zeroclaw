use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt, stream::SplitSink, stream::SplitStream};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::OnceCell;
use tokio::{io::AsyncRead, io::AsyncWrite};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use zeroclaw_api::channel::{Channel, ChannelApprovalResponse, ChannelMessage, SendMessage};
pub(crate) use zeroclaw_config::schema::MattermostListenMode;

const MAX_MATTERMOST_AUDIO_BYTES: u64 = 25 * 1024 * 1024;
/// Cadence at which auto-discovery re-runs to pick up newly-created DMs
/// and team channel changes.
const DISCOVERY_REFRESH: Duration = Duration::from_secs(60);
/// Poll interval per discovery iteration. Matches the previous single-channel
/// cadence so operators see no change in latency.
const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// A prompt awaiting an answer, bound to the destination it was posted into.
///
/// The token alone is not enough to identify the answer. It travels in
/// plaintext into a channel, one `MattermostChannel` serves many channels
/// (`poll_channel` iterates targets; the WebSocket path sees every post the bot
/// can read), and peer-group membership is scoped to the alias rather than to a
/// room. Without the destination an authorized user could answer a prompt from a
/// channel it was never posted to.
struct PendingApproval {
    /// Distinguishes this registration from any later one that happens to draw
    /// the same token. `new_approval_token` is random but only six characters,
    /// so token equality alone is not registration identity — without this, an
    /// older still-armed guard could retire a newer request's prompt.
    generation: u64,
    /// Mattermost channel ID the prompt was posted into.
    channel_id: String,
    sender: tokio::sync::oneshot::Sender<ChannelApprovalResponse>,
}

/// How many retired tokens to remember. A token only needs to outlive the
/// in-flight events that were already in flight when it was retired, so this is
/// deliberately small and bounded — it is a race window, not a history.
const RETIRED_APPROVAL_TOKENS: usize = 64;

/// All approval bookkeeping, behind one lock.
///
/// The two maps and the retired-token ring are a single unit of state because
/// every operation on them has to be one indivisible transition. Splitting them
/// across separate locks — or taking the same lock more than once per decision —
/// lets the reaction path retire a token in between the text path's *recognize*
/// and *remove* steps, at which point the text resolver can no longer tell "this
/// was never mine" from "I just lost the race", and the losing `<token> yes` is
/// forwarded to the model as ordinary conversation.
#[derive(Default)]
struct ApprovalState {
    /// Live prompts, keyed by the token echoed back in a reply.
    pending: HashMap<String, PendingApproval>,
    /// Prompt post ID -> (token, generation), for resolving one-tap emoji
    /// reactions. The generation is carried so cleanup can drop only the
    /// binding belonging to the registration being retired.
    posts: HashMap<String, (String, u64)>,
    /// Tokens already answered, cancelled, or timed out. Retained briefly so a
    /// reply that lost the race is still recognized as approval traffic and
    /// suppressed rather than delivered to the model.
    retired: std::collections::VecDeque<String>,
    /// Monotonic registration counter. Never reused within a channel's life.
    next_generation: u64,
}

impl ApprovalState {
    /// Retire `token` only if the live registration is the one identified by
    /// `generation`.
    ///
    /// A guard armed for an earlier request may still be alive when a later
    /// request draws the same six-character token. Retiring by token alone would
    /// then delete the newer request's sender and post binding, silently
    /// stranding an approval the operator can still see.
    fn retire_generation(&mut self, token: &str, generation: u64) -> Option<PendingApproval> {
        let matches = self
            .pending
            .get(token)
            .is_some_and(|pending| pending.generation == generation);
        if !matches {
            return None;
        }
        let removed = self.pending.remove(token);
        self.posts.retain(|_, (bound, bound_generation)| {
            bound != token || *bound_generation != generation
        });
        self.remember_retired(token);
        removed
    }

    fn remember_retired(&mut self, token: &str) {
        if !self.retired.iter().any(|seen| seen == token) {
            if self.retired.len() == RETIRED_APPROVAL_TOKENS {
                self.retired.pop_front();
            }
            self.retired.push_back(token.to_string());
        }
    }

    /// Draw a token that is neither live nor inside the retired window.
    ///
    /// `new_approval_token` is six random characters, so redraws can collide.
    /// A collision with a *live* token would overwrite another request's
    /// registration; one with a *retired* token would make that request's late
    /// decision look like it belongs to this prompt. Both hand authority to the
    /// wrong request rather than merely denying one, so neither is acceptable.
    ///
    /// Returns `None` if no free token is found within the attempt budget. With
    /// a 36^6 space and a retired window of 64, that means something is very
    /// wrong, and refusing to prompt is safer than reusing an identity.
    fn allocate_token(&self) -> Option<String> {
        const ALLOCATION_ATTEMPTS: usize = 16;
        (0..ALLOCATION_ATTEMPTS)
            .map(|_| crate::util::new_approval_token())
            .find(|token| !self.pending.contains_key(token) && !self.was_retired(token))
    }

    /// True when `token` is currently held by the registration identified by
    /// `generation` — the check every decision path makes before acting, so a
    /// later request that drew the same token cannot receive an older one's
    /// decision.
    fn owns(&self, token: &str, generation: u64) -> bool {
        self.pending
            .get(token)
            .is_some_and(|pending| pending.generation == generation)
    }

    fn was_retired(&self, token: &str) -> bool {
        self.retired.iter().any(|seen| seen == token)
    }
}

/// What the text resolver did with a post.
///
/// The distinction that matters is `NotFound` versus everything else: only text
/// this channel has no claim on may continue to the model. Every other outcome —
/// resolved, rejected, or too late — is approval-protocol traffic and is
/// swallowed, so a decision the operator typed can never surface as a chat
/// message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalReplyOutcome {
    /// Not an approval reply for any prompt this channel knows about.
    NotFound,
    /// Recognized as approval traffic and consumed.
    Consumed,
}

/// Retires a prompt's state when the request future goes away.
///
/// Registration happens before the prompt is posted, but the explicit cleanup
/// branches only run if `request_approval_attributed` reaches its own match arm.
/// A future that is *dropped* never reaches them, and the routed approval caller
/// wraps this one in an outer `tokio::time::timeout` whose default (120s) is
/// shorter than this channel's own (300s) — so cancellation mid-prompt is a
/// supported configuration, not a theoretical executor behaviour. Without this
/// guard each routed timeout would strand a token and its post binding in a
/// long-lived channel, and a later reply or tap would appear to answer a prompt
/// whose tool call had already been abandoned.
///
/// `Drop` cannot await, which is why [`ApprovalState`] sits behind a synchronous
/// mutex: cleanup runs inline and is therefore deterministic to assert in tests,
/// rather than being deferred to a spawned task.
struct ApprovalGuard {
    approvals: Arc<Mutex<ApprovalState>>,
    token: String,
    /// The registration this guard owns. Cleanup is conditional on it, so a
    /// stale guard cannot retire a later request that reused the token.
    generation: u64,
    /// Cleared once the request has retired the token itself, so the normal
    /// completion path does not retire it twice.
    armed: bool,
}

impl ApprovalGuard {
    fn new(approvals: Arc<Mutex<ApprovalState>>, token: String, generation: u64) -> Self {
        Self {
            approvals,
            token,
            generation,
            armed: true,
        }
    }

    /// Retire the prompt now and stand the guard down.
    fn retire(&mut self) {
        if self.armed {
            self.approvals
                .lock()
                .retire_generation(&self.token, self.generation);
            self.armed = false;
        }
    }
}

impl Drop for ApprovalGuard {
    fn drop(&mut self) {
        self.retire();
    }
}

/// The channel ID half of a recipient.
///
/// Recipients are `channel_id` or `channel_id:root_id` for a threaded reply.
/// Binding is on the channel, not the thread: an operator answering in the
/// channel rather than inside the prompt's thread is still answering in the
/// right room, and Mattermost clients make it easy to do either.
fn recipient_channel_id(recipient: &str) -> &str {
    recipient
        .split_once(':')
        .map_or(recipient, |(channel, _)| channel)
}

/// Emoji that stand for the two one-tap approval decisions. Mattermost's bare
/// emoji names, not the `:shortcode:` spelling the API rejects.
const APPROVAL_EMOJI_APPROVE: &str = "white_check_mark";
const APPROVAL_EMOJI_DENY: &str = "x";

/// Map a reaction emoji to the decision it stands for.
///
/// Anything else is a normal reaction on the prompt post — an operator giving a
/// thumbs-up, say — and must not be read as an answer.
fn approval_response_for_emoji(emoji: &str) -> Option<ChannelApprovalResponse> {
    match emoji {
        APPROVAL_EMOJI_APPROVE => Some(ChannelApprovalResponse::Approve),
        APPROVAL_EMOJI_DENY => Some(ChannelApprovalResponse::Deny),
        _ => None,
    }
}

/// Application-level ping interval for the Mattermost WebSocket protocol.
const WS_PING_INTERVAL: Duration = Duration::from_secs(30);
/// Deadline for authentication and the server's `hello` event.
const WS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// Read timeout for the active WebSocket session. If no frame arrives
/// within this window the peer is considered unresponsive and the
/// listener exits into the reconnect path. Set to 3× ping interval so
/// the server can miss two pings before we declare it dead.
const WS_READ_TIMEOUT: Duration = Duration::from_secs(90);

/// One channel the bot will poll. `is_direct` flags DM (`type=D`) and group DM
/// (`type=G`) channels so the receive path can bypass `mention_only` for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TargetChannel {
    pub id: String,
    pub is_direct: bool,
}

/// Mattermost channel `type` is a single-character code: `O` = open/public,
/// `P` = private, `G` = group DM, `D` = direct DM. Group DMs are private
/// multi-user conversations and share the no-ambient-noise semantic with 1:1
/// DMs, so both are treated as "direct" for `mention_only` purposes.
pub(crate) fn is_direct_channel(channel_type: &str) -> bool {
    matches!(channel_type, "D" | "G")
}

/// Filter a raw `/api/v4/users/me/channels` response down to the channels the
/// bot should poll. Public/private channels are gated by `team_ids` (empty =
/// all teams); DM/group-DM channels are gated by `discover_dms`. DMs carry
/// no `team_id`, so the team allowlist deliberately doesn't apply to them.
pub(crate) fn filter_discovered_channels(
    channels: &[serde_json::Value],
    team_ids: &[String],
    discover_dms: bool,
) -> Vec<TargetChannel> {
    channels
        .iter()
        .filter_map(|c| {
            let id = c.get("id").and_then(|v| v.as_str())?;
            let ty = c.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let team = c.get("team_id").and_then(|v| v.as_str()).unwrap_or("");
            let direct = is_direct_channel(ty);
            if direct {
                if !discover_dms {
                    return None;
                }
            } else if !team_ids.is_empty() && !team_ids.iter().any(|allowed| allowed == team) {
                return None;
            }
            Some(TargetChannel {
                id: id.to_string(),
                is_direct: direct,
            })
        })
        .collect()
}

/// Mattermost channel — polls channel posts via REST API v4.
/// Mattermost is API-compatible with many Slack patterns but uses a dedicated v4 structure.
pub struct MattermostChannel {
    base_url: String, // e.g., https://mm.example.com
    /// Static bot token from the config. Preferred over login when set.
    bot_token: Option<String>,
    /// Login ID for the password login flow. Used when `bot_token` is None.
    login_id: Option<String>,
    /// Password for the login flow. Used when `bot_token` is None.
    password: Option<String>,
    /// Resolved session token used by all API calls. Populated lazily on
    /// first use, either by copying `bot_token` or by performing the login
    /// flow with `login_id` and `password`.
    session_token: OnceCell<String>,
    /// (user_id, username) for the bot, fetched once from `/users/me`
    /// inside `get_bot_identity`. Read by `self_handle` /
    /// `self_addressed_mention` so the identity block reaches the prompt.
    bot_identity: OnceCell<(String, String)>,
    /// Channel IDs from config. Empty or `["*"]` triggers auto-discovery.
    channel_ids: Vec<String>,
    /// Team allowlist for auto-discovery. Empty = all teams.
    team_ids: Vec<String>,
    /// When true, auto-discovery includes DM (`type=D`) and group DM (`type=G`)
    /// channels. Defaults to true at construction; `with_discover_dms` overrides.
    discover_dms: bool,
    /// The alias key under `[channels.mattermost.<alias>]` this handle is
    /// bound to. Used to scope peer-group writes and resolver lookups.
    alias: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    /// When true (default), replies thread on the original post's root_id.
    /// When false, replies go to the channel root.
    thread_replies: bool,
    /// When true, only respond to messages that @-mention the bot.
    mention_only: bool,
    /// Handle for the background typing-indicator loop (aborted on stop_typing).
    typing_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Per-channel proxy URL override.
    proxy_url: Option<String>,
    transcription: Option<zeroclaw_config::schema::TranscriptionConfig>,
    transcription_manager: Option<Arc<super::transcription::TranscriptionManager>>,
    /// How this channel receives inbound messages. Defaults to `Polling`.
    listen_mode: MattermostListenMode,
    /// In-flight approval prompts, the post bindings that resolve one-tap
    /// reactions, and the recently-retired tokens — see [`ApprovalState`].
    ///
    /// A `parking_lot` mutex, like the rest of this file. The guard is never
    /// held across an `.await`: everything asynchronous a decision needs (the
    /// bot identity, the peer list) is resolved before the lock is taken, so
    /// each decision is one uninterruptible critical section. That is also what
    /// lets [`ApprovalGuard`] clean up synchronously from `Drop`, where awaiting
    /// is not possible.
    approvals: Arc<Mutex<ApprovalState>>,
    /// Seconds to wait for an operator reply before the runtime denies on its
    /// own authority. Mirrors `[channels.mattermost.<alias>].approval_timeout_secs`.
    approval_timeout_secs: u64,
}

impl MattermostChannel {
    pub fn new(
        base_url: String,
        bot_token: Option<String>,
        login_id: Option<String>,
        password: Option<String>,
        channel_ids: Vec<String>,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
        thread_replies: bool,
        mention_only: bool,
    ) -> Self {
        // Ensure base_url doesn't have a trailing slash for consistent path joining
        let base_url = base_url.trim_end_matches('/').to_string();
        Self {
            base_url,
            bot_token,
            login_id,
            password,
            session_token: OnceCell::new(),
            bot_identity: OnceCell::new(),
            channel_ids,
            team_ids: Vec::new(),
            discover_dms: true,
            alias: alias.into(),
            peer_resolver,
            thread_replies,
            mention_only,
            typing_handle: Mutex::new(None),
            proxy_url: None,
            transcription: None,
            transcription_manager: None,
            listen_mode: MattermostListenMode::default(),
            approvals: Arc::new(Mutex::new(ApprovalState::default())),
            // Sourced from the config type so the in-Rust default cannot drift
            // from the serde one. `0` here would be an already-elapsed
            // deadline that denies every approval.
            approval_timeout_secs: zeroclaw_config::schema::MattermostConfig::default()
                .approval_timeout_secs,
        }
    }

    /// Seconds to wait for an operator reply to an approval prompt before the
    /// runtime denies on its own authority.
    #[must_use]
    pub fn with_approval_timeout_secs(mut self, approval_timeout_secs: u64) -> Self {
        self.approval_timeout_secs = approval_timeout_secs;
        self
    }

    /// Restrict auto-discovery to the given team IDs. Empty = all teams the
    /// bot belongs to. No effect when `channel_ids` lists explicit IDs.
    pub fn with_team_ids(mut self, team_ids: Vec<String>) -> Self {
        self.team_ids = team_ids;
        self
    }

    /// Include (`true`, default) or omit (`false`) DM and group-DM channels
    /// during auto-discovery. No effect when `channel_ids` lists explicit IDs.
    pub fn with_discover_dms(mut self, discover_dms: bool) -> Self {
        self.discover_dms = discover_dms;
        self
    }

    /// Normalize a raw `channel_ids` entry: trim, drop blanks and the `*`
    /// wildcard sentinel. Returns `None` when the entry should not contribute
    /// to the explicit-scope list.
    pub(crate) fn normalized_channel_id(input: Option<&str>) -> Option<String> {
        input
            .map(str::trim)
            .filter(|v| !v.is_empty() && *v != "*")
            .map(ToOwned::to_owned)
    }

    /// Resolve the explicit channel scope from `channel_ids`. Returns `None`
    /// when the config asks for auto-discovery (empty list or wildcard-only).
    pub(crate) fn scoped_channel_ids(&self) -> Option<Vec<String>> {
        let mut seen = HashSet::new();
        let ids: Vec<String> = self
            .channel_ids
            .iter()
            .filter_map(|entry| Self::normalized_channel_id(Some(entry)))
            .filter(|id| seen.insert(id.clone()))
            .collect();
        if ids.is_empty() { None } else { Some(ids) }
    }

    pub(crate) async fn list_target_channels(&self) -> Result<Vec<TargetChannel>> {
        let token = self.token().await?.to_string();
        if let Some(ids) = self.scoped_channel_ids() {
            let mut out = Vec::with_capacity(ids.len());
            for id in ids {
                let resp = self
                    .http_client()
                    .get(format!("{}/api/v4/channels/{}", self.base_url, id))
                    .bearer_auth(&token)
                    .send()
                    .await
                    .with_context(|| format!("GET /channels/{id} failed"))?;
                if !resp.status().is_success() {
                    bail!(
                        "GET /channels/{id} returned {}: explicit channel_id is not accessible to this bot",
                        resp.status()
                    );
                }
                let body: serde_json::Value = resp
                    .json()
                    .await
                    .with_context(|| format!("decode /channels/{id} body"))?;
                let ty = body.get("type").and_then(|v| v.as_str()).unwrap_or("");
                out.push(TargetChannel {
                    id,
                    is_direct: is_direct_channel(ty),
                });
            }
            return Ok(out);
        }
        let resp = self
            .http_client()
            .get(format!("{}/api/v4/users/me/channels", self.base_url))
            .bearer_auth(&token)
            .send()
            .await
            .context("GET /users/me/channels failed")?;
        if !resp.status().is_success() {
            bail!("GET /users/me/channels returned {}", resp.status());
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .context("decode /users/me/channels body")?;
        let arr = body.as_array().cloned().unwrap_or_default();
        Ok(filter_discovered_channels(
            &arr,
            &self.team_ids,
            self.discover_dms,
        ))
    }

    /// Return the alias under `[channels.mattermost.<alias>]` that this
    /// channel handle is bound to.
    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// Resolve the session token, performing the login flow on first call
    /// if `bot_token` is not set.
    async fn token(&self) -> Result<&str> {
        self.session_token
            .get_or_try_init(|| async {
                if let Some(ref t) = self.bot_token {
                    return Ok::<String, anyhow::Error>(t.clone());
                }
                let login_id = self.login_id.as_deref().ok_or_else(|| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "missing": "login_id",
                                "reason": "no_bot_token",
                            })),
                        "mattermost: bot_token unset and login_id missing"
                    );
                    anyhow::Error::msg(
                        "bot_token is unset; configure either bot_token or both login_id and password",
                    )
                })?;
                let password = self.password.as_deref().ok_or_else(|| {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "missing": "password",
                                "reason": "no_bot_token",
                            })),
                        "mattermost: bot_token unset and password missing"
                    );
                    anyhow::Error::msg(
                        "bot_token is unset and password is missing; both login_id and password must be set",
                    )
                })?;
                self.login(login_id, password).await
            })
            .await
            .map(String::as_str)
    }

    /// Perform the Mattermost password login flow and return the session
    /// token. The session token is returned via the `Token` response header
    /// per Mattermost API v4.
    async fn login(&self, login_id: &str, password: &str) -> Result<String> {
        let resp = self
            .http_client()
            .post(format!("{}/api/v4/users/login", self.base_url))
            .json(&serde_json::json!({
                "login_id": login_id,
                "password": password,
            }))
            .send()
            .await
            .context("login request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("login failed ({status}): {body}");
        }
        let token = resp
            .headers()
            .get("Token")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "login succeeded but the response had no Token header"
                );
                anyhow::Error::msg("login succeeded but the response had no Token header")
            })?
            .to_string();
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "login succeeded; session token cached"
        );
        Ok(token)
    }

    /// Set a per-channel proxy URL that overrides the global proxy config.
    pub fn with_proxy_url(mut self, proxy_url: Option<String>) -> Self {
        self.proxy_url = proxy_url;
        self
    }

    pub fn with_transcription(
        mut self,
        config: zeroclaw_config::schema::TranscriptionConfig,
    ) -> Self {
        if !config.enabled {
            return self;
        }
        match super::transcription::TranscriptionManager::new(&config) {
            Ok(m) => {
                let names = m.available_providers();
                let m = if names.len() == 1 {
                    let only = names[0].to_string();
                    m.with_agent_transcription_provider(only)
                } else {
                    m
                };
                self.transcription_manager = Some(Arc::new(m));
                self.transcription = Some(config);
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"e": e.to_string()})),
                    "transcription manager init failed, voice transcription disabled"
                );
            }
        }
        self
    }

    /// Set the listen mode. Defaults to `Polling` when not called.
    pub fn with_listen_mode(mut self, listen_mode: MattermostListenMode) -> Self {
        self.listen_mode = listen_mode;
        self
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_channel_proxy_client_with_timeouts(
            "channel.mattermost",
            self.proxy_url.as_deref(),
            30,
            10,
        )
    }

    /// Derive the WebSocket URL from the REST base URL.
    fn ws_url(&self) -> String {
        self.base_url
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1)
            + "/api/v4/websocket"
    }

    fn ws_auth_response(event: &serde_json::Value, auth_seq: i64) -> Option<bool> {
        (event.get("seq_reply").and_then(|v| v.as_i64()) == Some(auth_seq))
            .then(|| event.get("status").and_then(|v| v.as_str()) == Some("OK"))
    }

    fn ws_post_from_event(event: &serde_json::Value) -> Option<serde_json::Value> {
        let post = event.get("data")?.get("post")?.as_str()?;
        serde_json::from_str(post).ok()
    }

    async fn authenticate_websocket<S>(
        write: &mut SplitSink<WebSocketStream<S>, WsMessage>,
        read: &mut SplitStream<WebSocketStream<S>>,
        token: &str,
        auth_seq: i64,
        timeout: Duration,
    ) -> Result<String>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let auth = serde_json::json!({
            "seq": auth_seq,
            "action": "authentication_challenge",
            "data": { "token": token }
        });
        write
            .send(WsMessage::Text(auth.to_string().into()))
            .await
            .context("Mattermost WebSocket authentication send failed")?;

        let deadline = tokio::time::Instant::now() + timeout;
        let mut authenticated = false;
        let mut server_version = None;

        loop {
            if authenticated && server_version.is_some() {
                return Ok(server_version.unwrap_or_else(|| "unknown".to_string()));
            }

            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    bail!("Mattermost WebSocket authentication handshake timed out");
                }
                frame = read.next() => {
                    let text = match frame {
                        Some(Ok(WsMessage::Text(text))) => text,
                        Some(Ok(WsMessage::Ping(payload))) => {
                            write
                                .send(WsMessage::Pong(payload))
                                .await
                                .context("Mattermost WebSocket handshake pong failed")?;
                            continue;
                        }
                        Some(Ok(WsMessage::Close(frame))) => {
                            let reason = frame
                                .as_ref()
                                .map(|frame| frame.reason.as_ref())
                                .unwrap_or("");
                            bail!("Mattermost WebSocket closed during authentication: {reason}");
                        }
                        Some(Err(error)) => {
                            return Err(error).context("Mattermost WebSocket handshake read failed");
                        }
                        None => bail!("Mattermost WebSocket ended during authentication"),
                        Some(Ok(_)) => continue,
                    };

                    let event: serde_json::Value = serde_json::from_str(text.as_ref())
                        .context("Mattermost WebSocket handshake returned invalid JSON")?;

                    if let Some(ok) = Self::ws_auth_response(&event, auth_seq) {
                        if !ok {
                            bail!("Mattermost WebSocket authentication was rejected");
                        }
                        authenticated = true;
                    }

                    if event.get("event").and_then(|value| value.as_str()) == Some("hello") {
                        server_version = Some(
                            event
                                .get("data")
                                .and_then(|data| data.get("server_version"))
                                .and_then(|value| value.as_str())
                                .unwrap_or("unknown")
                                .to_string(),
                        );
                    }
                }
            }
        }
    }

    /// Check if a user ID is in the allowlist.
    /// Empty list means deny everyone. "*" means allow everyone.
    fn is_user_allowed(&self, user_id: &str) -> bool {
        let peers = (self.peer_resolver)();
        crate::allowlist::is_user_allowed(&peers, user_id, crate::allowlist::Match::Sensitive)
    }

    /// Get the bot's own user ID and username so we can ignore our own messages
    /// and detect @-mentions by username. Result cached on the channel
    /// so `self_handle` / `self_addressed_mention` can read it sync.
    async fn get_bot_identity(&self) -> (String, String) {
        if let Some(cached) = self.bot_identity.get() {
            return cached.clone();
        }
        let token = match self.token().await {
            Ok(t) => t.to_string(),
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "auth failed in get_bot_identity"
                );
                return (String::new(), String::new());
            }
        };
        let resp: Option<serde_json::Value> = async {
            self.http_client()
                .get(format!("{}/api/v4/users/me", self.base_url))
                .bearer_auth(&token)
                .send()
                .await
                .ok()?
                .json()
                .await
                .ok()
        }
        .await;

        let id = resp
            .as_ref()
            .and_then(|v| v.get("id"))
            .and_then(|u| u.as_str())
            .unwrap_or("")
            .to_string();
        let username = resp
            .as_ref()
            .and_then(|v| v.get("username"))
            .and_then(|u| u.as_str())
            .unwrap_or("")
            .to_string();
        // Cache only a *complete* identity. The ID is the half the approval path
        // depends on — it is what distinguishes the bot's own post or seeded
        // reaction from an operator's decision — so caching a username-only
        // response would pin an empty ID for the lifetime of the channel and
        // permanently disable the self-approval guard. Leaving it uncached costs
        // one extra request and lets the next call recover.
        if !id.is_empty() {
            let _ = self.bot_identity.set((id.clone(), username.clone()));
        }
        (id, username)
    }

    async fn try_transcribe_audio_attachment(&self, post: &serde_json::Value) -> Option<String> {
        let config = self.transcription.as_ref()?;
        let manager = self.transcription_manager.as_deref()?;

        let files = post
            .get("metadata")
            .and_then(|m| m.get("files"))
            .and_then(|f| f.as_array())?;

        let audio_file = files.iter().find(|f| is_audio_file(f))?;

        if let Some(duration_ms) = audio_file.get("duration").and_then(|d| d.as_u64()) {
            let duration_secs = duration_ms / 1000;
            if duration_secs > config.max_duration_secs {
                ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"duration_secs": duration_secs, "max": config.max_duration_secs})), "audio attachment exceeds max duration, skipping");
                return None;
            }
        }

        let file_id = audio_file.get("id").and_then(|i| i.as_str())?;
        let file_name = audio_file
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("audio");

        let token = match self.token().await {
            Ok(t) => t.to_string(),
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({"error": format!("{}", e), "file_id": file_id})
                        ),
                    "audio download auth failed for"
                );
                return None;
            }
        };
        let response = match self
            .http_client()
            .get(format!("{}/api/v4/files/{}", self.base_url, file_id))
            .bearer_auth(&token)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({"error": format!("{}", e), "file_id": file_id})
                        ),
                    "audio download failed for"
                );
                return None;
            }
        };

        if !response.status().is_success() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!("audio download returned {}: {file_id}", response.status())
            );
            return None;
        }

        let bytes =
            match crate::util::read_response_body_limited(response, MAX_MATTERMOST_AUDIO_BYTES)
                .await
            {
                Ok(b) => b,
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(
                                ::serde_json::json!({"error": format!("{}", e), "file_id": file_id})
                            ),
                        "failed to read audio bytes for"
                    );
                    return None;
                }
            };

        match manager.transcribe(&bytes, file_name).await {
            Ok(text) => {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        "transcription returned empty text, skipping"
                    );
                    None
                } else {
                    Some(format!("[Voice] {trimmed}"))
                }
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "audio transcription failed"
                );
                None
            }
        }
    }

    /// Create a post and return its ID.
    ///
    /// `send` discards the ID; the approval path needs it, because a reaction
    /// event identifies the post it landed on and nothing else. Kept as one
    /// method so both paths build the request body identically.
    async fn create_post(&self, message: &SendMessage) -> Result<String> {
        // Mattermost supports threading via 'root_id'.
        // We pack 'channel_id:root_id' into recipient if it's a thread.
        let (channel_id, root_id) = if let Some((c, r)) = message.recipient.split_once(':') {
            (c, Some(r))
        } else {
            (message.recipient.as_str(), None)
        };

        let mut body_map = serde_json::json!({
            "channel_id": channel_id,
            "message": message.content
        });

        if let Some(root) = root_id
            && let Some(body) = body_map.as_object_mut()
        {
            body.insert(
                "root_id".to_string(),
                serde_json::Value::String(root.to_string()),
            );
        }

        let token = self.token().await?;
        let resp = self
            .http_client()
            .post(format!("{}/api/v4/posts", self.base_url))
            .bearer_auth(token)
            .json(&body_map)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response: {e}>"));
            bail!("post failed ({status}): {body}");
        }

        // A post that succeeded but whose ID we cannot read is not an error for
        // `send`, which ignores it. Return an empty ID rather than failing the
        // send; the approval path treats an empty ID as "no reaction path".
        Ok(resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|body| {
                body.get("id")
                    .and_then(|id| id.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_default())
    }

    /// React to a post as the bot.
    ///
    /// `emoji_name` is Mattermost's bare name (`white_check_mark`), not the
    /// `:shortcode:` spelling.
    async fn react_to_post(&self, post_id: &str, emoji_name: &str) -> Result<()> {
        let (bot_user_id, _) = self.get_bot_identity().await;
        if bot_user_id.is_empty() {
            bail!("cannot react without a bot user ID");
        }
        let token = self.token().await?;
        let resp = self
            .http_client()
            .post(format!("{}/api/v4/reactions", self.base_url))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "user_id": bot_user_id,
                "post_id": post_id,
                "emoji_name": emoji_name,
            }))
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response: {e}>"));
            bail!("reaction failed ({status}): {body}");
        }
        Ok(())
    }
}

impl ::zeroclaw_api::attribution::Attributable for MattermostChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::Mattermost,
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for MattermostChannel {
    fn name(&self) -> &str {
        "mattermost"
    }

    fn self_handle(&self) -> Option<String> {
        self.bot_identity
            .get()
            .map(|(id, _)| id.clone())
            .filter(|id| !id.is_empty())
    }

    fn self_addressed_mention(&self) -> Option<String> {
        self.bot_identity
            .get()
            .map(|(_, username)| username.clone())
            .filter(|u| !u.is_empty())
            .map(|u| format!("@{u}"))
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        self.create_post(message).await.map(|_| ())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        match self.listen_mode {
            MattermostListenMode::Polling => self.listen_polling(tx).await,
            MattermostListenMode::Websocket => self.listen_websocket(tx).await,
        }
    }

    async fn health_check(&self) -> bool {
        let Ok(token) = self.token().await else {
            return false;
        };
        self.http_client()
            .get(format!("{}/api/v4/users/me", self.base_url))
            .bearer_auth(token)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    async fn start_typing(&self, recipient: &str) -> Result<()> {
        // Cancel any existing typing loop before starting a new one.
        self.stop_typing(recipient).await?;

        let client = self.http_client();
        let token = self.token().await?.to_string();
        let base_url = self.base_url.clone();

        // recipient is "channel_id" or "channel_id:root_id"
        let (channel_id, parent_id) = match recipient.split_once(':') {
            Some((channel, parent)) => (channel.to_string(), Some(parent.to_string())),
            None => (recipient.to_string(), None),
        };

        let handle = zeroclaw_spawn::spawn!(async move {
            let url = format!("{base_url}/api/v4/users/me/typing");
            loop {
                let mut body = serde_json::json!({ "channel_id": channel_id });
                if let Some(ref pid) = parent_id {
                    body.as_object_mut()
                        .unwrap()
                        .insert("parent_id".to_string(), serde_json::json!(pid));
                }

                if let Ok(r) = client
                    .post(&url)
                    .bearer_auth(&token)
                    .json(&body)
                    .send()
                    .await
                    && !r.status().is_success()
                {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_attrs(::serde_json::json!({"status": r.status().to_string()})),
                        "typing indicator failed"
                    );
                }

                // Mattermost typing events expire after ~6s; re-fire every 4s.
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
            }
        });

        let mut guard = self.typing_handle.lock();
        *guard = Some(handle);

        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> Result<()> {
        let mut guard = self.typing_handle.lock();
        if let Some(handle) = guard.take() {
            handle.abort();
        }
        Ok(())
    }

    async fn request_approval(
        &self,
        recipient: &str,
        request: &zeroclaw_api::channel::ChannelApprovalRequest,
    ) -> Result<Option<ChannelApprovalResponse>> {
        Ok(self
            .request_approval_attributed(recipient, request)
            .await?
            .map(|attributed| attributed.response))
    }

    /// Prompt the operator by posting a token-prefixed message and waiting for
    /// a reply that echoes the token.
    ///
    /// Mattermost's interactive buttons post to an integration URL, which a
    /// polling deployment has no endpoint for, so the prompt is the shared
    /// text form used by Signal and Discord's plaintext fallback and is parsed
    /// by the shared [`crate::util::parse_approval_reply`].
    ///
    /// Only a real token echo counts as an operator decision. The dropped-sender
    /// and timeout arms are the runtime denying on its own authority and are
    /// attributed as such, so the model is never told a human refused when none
    /// was asked.
    async fn request_approval_attributed(
        &self,
        recipient: &str,
        request: &zeroclaw_api::channel::ChannelApprovalRequest,
    ) -> Result<Option<zeroclaw_api::channel::AttributedApprovalResponse>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        // Allocation and registration are one critical section, and the token is
        // drawn so it collides with neither a live prompt nor one still inside
        // the retired window. Six random characters are not an identity: a
        // redraw that lands on a token another request already owns would let a
        // decision meant for one prompt resolve the other.
        let (token, generation) = {
            let mut state = self.approvals.lock();
            let Some(token) = state.allocate_token() else {
                return Err(anyhow::Error::msg(
                    "could not allocate a free approval token; too many prompts in flight",
                ));
            };
            let generation = state.next_generation;
            state.next_generation += 1;
            state.pending.insert(
                token.clone(),
                PendingApproval {
                    generation,
                    channel_id: recipient_channel_id(recipient).to_string(),
                    sender: tx,
                },
            );
            (token, generation)
        };

        let text = crate::util::build_yesno_approval_prompt(
            &token,
            &request.tool_name,
            &request.arguments_summary,
        );
        // Armed from here on: every exit below — including a dropped future —
        // retires this registration and any post bound to it. Cleanup is keyed
        // by generation, so a guard outliving its request cannot evict a later
        // one that drew the same token.
        let mut guard = ApprovalGuard::new(Arc::clone(&self.approvals), token.clone(), generation);

        let post_id = self.create_post(&SendMessage::new(text, recipient)).await?;

        // Offer the one-tap path only when we would actually see the tap.
        // Reactions reach us as `reaction_added` WebSocket events; the polling
        // listener reads posts and never sees them, so seeding the emoji there
        // would put two buttons on the prompt that silently do nothing.
        if self.listen_mode == zeroclaw_config::schema::MattermostListenMode::Websocket
            && !post_id.is_empty()
        {
            // `create_post` awaited, so confirm this registration still owns the
            // token before binding its post. Binding unconditionally would let a
            // reaction on this post resolve whichever request holds the token
            // later, handing one prompt's decision to another.
            let still_live = {
                let mut state = self.approvals.lock();
                if state.owns(&token, generation) {
                    state
                        .posts
                        .insert(post_id.clone(), (token.clone(), generation));
                    true
                } else {
                    false
                }
            };
            if still_live {
                self.seed_approval_reactions(&post_id).await;
            }
        }

        let attributed =
            match tokio::time::timeout(Duration::from_secs(self.approval_timeout_secs), rx).await {
                Ok(Ok(response)) => {
                    zeroclaw_api::channel::AttributedApprovalResponse::operator(response)
                }
                Ok(Err(_)) => zeroclaw_api::channel::AttributedApprovalResponse::from_runtime(
                    ChannelApprovalResponse::Deny,
                    zeroclaw_api::channel::ApprovalSource::Unreachable,
                ),
                Err(_) => zeroclaw_api::channel::AttributedApprovalResponse::from_runtime(
                    ChannelApprovalResponse::Deny,
                    zeroclaw_api::channel::ApprovalSource::TimedOut,
                ),
            };
        // One retirement for every outcome. The resolving path already removed
        // the pending entry, but the token still has to be remembered as retired
        // and any post binding dropped, so a tap arriving after the decision
        // cannot find stale state.
        guard.retire();
        Ok(Some(attributed))
    }
}

impl MattermostChannel {
    async fn listen_polling(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        // Resolve auth up front so misconfiguration fails fast at listen-time.
        let initial_token = self.token().await?.to_string();
        let (bot_user_id, bot_username) = self.get_bot_identity().await;

        let auto_discover = self.scoped_channel_ids().is_none();
        let mut target_channels = self.list_target_channels().await?;
        let mut last_discovery = Instant::now();
        let mut last_create_at_by_channel: HashMap<String, i64> = HashMap::new();

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "alias": self.alias,
                    "channel_count": target_channels.len(),
                    "auto_discover": auto_discover,
                    "team_ids": self.team_ids,
                    "discover_dms": self.discover_dms,
                })
            ),
            "Mattermost channel listening (polling)"
        );

        loop {
            tokio::time::sleep(POLL_INTERVAL).await;

            if auto_discover && last_discovery.elapsed() >= DISCOVERY_REFRESH {
                match self.list_target_channels().await {
                    Ok(refreshed) => {
                        if refreshed != target_channels {
                            ::zeroclaw_log::record!(
                                INFO,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note,
                                )
                                .with_attrs(::serde_json::json!({
                                    "alias": self.alias,
                                    "before": target_channels.len(),
                                    "after": refreshed.len(),
                                })),
                                "Mattermost auto-discovery refreshed channel list"
                            );
                            target_channels = refreshed;
                        }
                    }
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note,
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({
                                "alias": self.alias,
                                "error": format!("{}", e),
                            })),
                            "Mattermost auto-discovery refresh failed; keeping previous channel list"
                        );
                    }
                }
                last_discovery = Instant::now();
            }

            if target_channels.is_empty() {
                continue;
            }

            #[allow(clippy::cast_possible_truncation)]
            let bootstrap_ms = (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()) as i64;

            for target in target_channels.clone() {
                if self
                    .poll_channel(
                        &target,
                        &initial_token,
                        &bot_user_id,
                        &bot_username,
                        bootstrap_ms,
                        &mut last_create_at_by_channel,
                        &tx,
                    )
                    .await
                {
                    return Ok(());
                }
            }
        }
    }

    async fn listen_websocket(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        let token = self.token().await?.to_string();
        let (bot_user_id, bot_username) = self.get_bot_identity().await;
        let auto_discover = self.scoped_channel_ids().is_none();
        let target_channels = self.list_target_channels().await?;
        let mut channel_direct_map: HashMap<String, bool> = target_channels
            .into_iter()
            .map(|target| (target.id, target.is_direct))
            .collect();

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "alias": self.alias,
                    "channel_count": channel_direct_map.len(),
                    "auto_discover": auto_discover,
                    "mode": "websocket",
                })
            ),
            "Mattermost WebSocket listening"
        );

        let ws_url = self.ws_url();
        let (ws_stream, _) = zeroclaw_config::schema::ws_connect_with_proxy(
            &ws_url,
            "channel.mattermost",
            self.proxy_url.as_deref(),
        )
        .await
        .with_context(|| format!("Mattermost WebSocket connect failed: {ws_url}"))?;

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "alias": self.alias,
                    "ws_url": &ws_url,
                })
            ),
            "Mattermost WebSocket connected"
        );

        let (mut write, mut read) = ws_stream.split();
        let auth_seq = 1;
        let server_version = Self::authenticate_websocket(
            &mut write,
            &mut read,
            &token,
            auth_seq,
            WS_HANDSHAKE_TIMEOUT,
        )
        .await?;

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "alias": self.alias,
                    "server_version": server_version,
                })
            ),
            "Mattermost WebSocket authenticated"
        );

        let mut seq = auth_seq.wrapping_add(1);
        let mut ping_interval = tokio::time::interval(WS_PING_INTERVAL);
        ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ping_interval.reset();

        let mut discovery_interval = tokio::time::interval(DISCOVERY_REFRESH);
        discovery_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        discovery_interval.reset();

        let mut last_frame = tokio::time::Instant::now();

        loop {
            let read_deadline = last_frame + WS_READ_TIMEOUT;
            tokio::select! {
                _ = discovery_interval.tick(), if auto_discover => {
                    match self.list_target_channels().await {
                        Ok(refreshed) => {
                            let refreshed_map: HashMap<String, bool> = refreshed
                                .into_iter()
                                .map(|target| (target.id, target.is_direct))
                                .collect();
                            if refreshed_map != channel_direct_map {
                                ::zeroclaw_log::record!(
                                    INFO,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note,
                                    )
                                    .with_attrs(::serde_json::json!({
                                        "alias": self.alias,
                                        "before": channel_direct_map.len(),
                                        "after": refreshed_map.len(),
                                    })),
                                    "Mattermost WS in-session auto-discovery refreshed"
                                );
                                channel_direct_map = refreshed_map;
                            }
                        }
                        Err(error) => {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note,
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                .with_attrs(::serde_json::json!({
                                    "alias": self.alias,
                                    "error": error.to_string(),
                                })),
                                "Mattermost WS in-session discovery refresh failed"
                            );
                        }
                    }
                }
                _ = ping_interval.tick() => {
                    let ping = serde_json::json!({"seq": seq, "action": "ping"});
                    write
                        .send(WsMessage::Text(ping.to_string().into()))
                        .await
                        .context("Mattermost WebSocket ping send failed")?;
                    seq = seq.wrapping_add(1);
                }
                frame = read.next() => {
                    let frame = match frame {
                        Some(Ok(frame)) => {
                            last_frame = tokio::time::Instant::now();
                            frame
                        }
                        Some(Err(error)) => {
                            return Err(error).context("Mattermost WebSocket read failed");
                        }
                        None => bail!("Mattermost WebSocket stream ended"),
                    };

                    let text = match frame {
                        WsMessage::Text(text) => text,
                        WsMessage::Ping(payload) => {
                            write
                                .send(WsMessage::Pong(payload))
                                .await
                                .context("Mattermost WebSocket pong send failed")?;
                            continue;
                        }
                        WsMessage::Close(frame) => {
                            let reason = frame
                                .as_ref()
                                .map(|frame| frame.reason.as_ref())
                                .unwrap_or("");
                            bail!("Mattermost WebSocket closed: {reason}");
                        }
                        _ => continue,
                    };

                    let event: serde_json::Value = match serde_json::from_str(text.as_ref()) {
                        Ok(event) => event,
                        Err(error) => {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note,
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                .with_attrs(::serde_json::json!({
                                    "alias": self.alias,
                                    "error": error.to_string(),
                                })),
                                "Mattermost WS event parse failed"
                            );
                            continue;
                        }
                    };

                    match event.get("event").and_then(|value| value.as_str()) {
                        Some("posted") => {}
                        // One-tap approvals. Reactions are never routed to the
                        // model, so this arm is terminal either way.
                        Some("reaction_added") => {
                            self.try_resolve_approval_reaction(&event).await;
                            continue;
                        }
                        _ => continue,
                    }

                    let Some(post) = Self::ws_post_from_event(&event) else {
                        continue;
                    };
                    let channel_id = post
                        .get("channel_id")
                        .and_then(|value| value.as_str())
                        .unwrap_or("");
                    let Some(&is_direct) = channel_direct_map.get(channel_id) else {
                        continue;
                    };

                    if self
                        .process_inbound_post(
                            &post,
                            &bot_user_id,
                            &bot_username,
                            0,
                            channel_id,
                            is_direct,
                            &tx,
                        )
                        .await
                    {
                        return Ok(());
                    }
                }
                _ = tokio::time::sleep_until(read_deadline) => {
                    bail!(
                        "Mattermost WebSocket idle for {} seconds",
                        WS_READ_TIMEOUT.as_secs()
                    );
                }
            }
        }
    }
}

impl MattermostChannel {
    #[allow(clippy::too_many_arguments)]
    async fn process_inbound_post(
        &self,
        post: &serde_json::Value,
        bot_user_id: &str,
        bot_username: &str,
        last_create_at: i64,
        channel_id: &str,
        is_direct: bool,
        tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
    ) -> bool {
        let effective_text = if post
            .get("message")
            .and_then(|message| message.as_str())
            .unwrap_or("")
            .trim()
            .is_empty()
            && post_has_audio_attachment(post)
        {
            self.try_transcribe_audio_attachment(post).await
        } else {
            None
        };

        // Approval replies are consumed here, before the normal message path.
        //
        // They must be intercepted ahead of `parse_mattermost_post` because
        // that function applies `mention_only`, and an operator answering
        // `abc123 yes` in a team channel has no reason to @-mention the bot.
        // The allowlist is re-checked explicitly rather than inherited, since
        // skipping the parse also skips its authorization gate — approving a
        // tool call is at least as privileged as sending a message.
        // Only text with no claim on a prompt continues to the model. Every
        // other outcome — resolved, rejected, or arriving after the prompt was
        // already retired — is approval-protocol traffic, so a decision the
        // operator typed can never surface as a chat message.
        if self.try_resolve_approval_reply(post, bot_user_id).await
            == ApprovalReplyOutcome::Consumed
        {
            return false;
        }

        let Some(message) = self.parse_mattermost_post(
            post,
            bot_user_id,
            bot_username,
            last_create_at,
            channel_id,
            effective_text.as_deref(),
            is_direct,
        ) else {
            return false;
        };

        tx.send(message).await.is_err()
    }

    /// Put the two decision emoji on the prompt post so the operator can tap
    /// one instead of typing the token back.
    ///
    /// Best-effort: a failure here leaves the text path, which is the
    /// authoritative one, completely intact. Failing the approval request
    /// because an emoji could not be placed would deny a tool call over a
    /// cosmetic problem, so this only warns.
    ///
    /// Deliberately two emoji, not three. `always` is also accepted by the text
    /// path, but it grants a session-scoped allowlist entry rather than
    /// permitting one call, and a mis-tap next to `approve` is far too cheap a
    /// way to widen that. Escalating to `always` stays a typed decision.
    async fn seed_approval_reactions(&self, post_id: &str) {
        for emoji in [APPROVAL_EMOJI_APPROVE, APPROVAL_EMOJI_DENY] {
            if let Err(error) = self.react_to_post(post_id, emoji).await {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "alias": self.alias,
                            "post_id": post_id,
                            "emoji": emoji,
                            "error": error.to_string(),
                        })),
                    "failed to seed Mattermost approval reaction; the text reply path still works"
                );
            }
        }
    }

    /// Drop both handles on a prompt the runtime is about to answer itself.
    ///
    /// Leaving either behind would let a late reply or a late tap resolve a
    /// prompt whose caller has already been told the answer.
    /// Decode a `reaction_added` event's payload. Mattermost sends the reaction
    /// as a JSON *string* under `data.reaction`, the same shape as `data.post`.
    fn ws_reaction_from_event(event: &serde_json::Value) -> Option<serde_json::Value> {
        let reaction = event.get("data")?.get("reaction")?.as_str()?;
        serde_json::from_str(reaction).ok()
    }

    /// Resolve a pending approval from a reaction on its prompt post.
    ///
    /// Returns `true` when the reaction was consumed as a decision. Unlike the
    /// reply path there is nothing to keep away from the model — a reaction is
    /// never routed as a message — so the return value exists for tests and
    /// logging rather than for control flow.
    async fn try_resolve_approval_reaction(&self, event: &serde_json::Value) -> bool {
        let Some(reaction) = Self::ws_reaction_from_event(event) else {
            return false;
        };
        let emoji = reaction
            .get("emoji_name")
            .and_then(|emoji| emoji.as_str())
            .unwrap_or("");
        let Some(response) = approval_response_for_emoji(emoji) else {
            return false;
        };
        let post_id = reaction
            .get("post_id")
            .and_then(|post| post.as_str())
            .unwrap_or("");

        // Cheap check first: a reaction on any other post is not ours, and
        // rejecting it here keeps unrelated chatter out of the auth logs.
        let Some((token, generation)) = self.approvals.lock().posts.get(post_id).cloned() else {
            return false;
        };

        let user_id = reaction
            .get("user_id")
            .and_then(|user| user.as_str())
            .unwrap_or("");

        // Resolved before the lock so the decision below stays one
        // uninterruptible critical section.
        let (bot_user_id, _) = self.get_bot_identity().await;

        // The bot seeds both emoji itself, so its own reactions come straight
        // back as events. Answering them would auto-approve every prompt the
        // instant it is posted. An event with no author is rejected for the same
        // reason it is on the text path: a wildcard peer group would otherwise
        // match the empty string and treat "no identity" as authorized.
        //
        // An unknown bot ID fails closed here too, and this path is the sharper
        // case: the bot's own seeded ✅ arrives within milliseconds of the
        // prompt, so skipping the self-check while identity is unresolved would
        // approve the tool call almost immediately and without any human input.
        let reason = if user_id.is_empty() {
            Some("missing_author_identity")
        } else if bot_user_id.is_empty() {
            Some("unknown_bot_identity")
        } else if user_id == bot_user_id {
            // The bot's own seeded reaction: expected on every prompt, so this
            // is routine rather than a rejection worth logging.
            return false;
        } else {
            None
        };
        if let Some(reason) = reason {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"token": token, "reason": reason})),
                "refusing to decide an approval without a trustworthy bot identity"
            );
            return false;
        }

        if !self.is_user_allowed(user_id) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "user_id": user_id,
                        "token": token,
                        "emoji": emoji,
                    })),
                "ignoring approval reaction from unauthorized user"
            );
            // Leave the prompt pending so an authorized operator can still
            // answer it, by reaction or by reply.
            return false;
        }

        // Retire by generation, not by token. This post belongs to one specific
        // registration; if a later request has since drawn the same token, a tap
        // on this older, still-visible prompt must not resolve it. Retiring also
        // drops the post binding, so a second tap finds nothing.
        let Some(pending) = self.approvals.lock().retire_generation(&token, generation) else {
            return false;
        };
        // A closed receiver means the waiter already timed out; the decision is
        // simply dropped.
        let _ = pending.sender.send(response);
        true
    }

    /// Consume `post` if it is an authorized reply to a pending approval
    /// prompt. Returns `true` when the post was consumed, so the caller skips
    /// the normal message path and the reply never reaches the model.
    /// `bot_user_id` is the authenticated identity from `get_bot_identity`.
    /// It is passed in rather than read here because this runs *before*
    /// `parse_mattermost_post`, and so inherits none of that parser's
    /// protections — including its self-loop guard.
    async fn try_resolve_approval_reply(
        &self,
        post: &serde_json::Value,
        bot_user_id: &str,
    ) -> ApprovalReplyOutcome {
        let text = post
            .get("message")
            .and_then(|message| message.as_str())
            .unwrap_or("");
        let Some((token, response)) = crate::util::parse_approval_reply(text) else {
            return ApprovalReplyOutcome::NotFound;
        };

        let user_id = post
            .get("user_id")
            .and_then(|user| user.as_str())
            .unwrap_or("");
        let reply_channel = post
            .get("channel_id")
            .and_then(|channel| channel.as_str())
            .unwrap_or("");

        // Ownership first. A refusal below must never swallow text this channel
        // has no claim on: with the identity checks ahead of this lookup, an
        // unresolved bot identity turned *every* six-character approval-shaped
        // message into `Consumed`, losing unrelated user text instead of
        // forwarding it. Peeking here is safe even though the decision itself
        // happens in a later critical section, because a token retired in
        // between is still recognized through `was_retired`.
        // The observed registration is captured here, not just the fact that the
        // token is ours. `get_bot_identity` below may await, and a request that
        // draws this token during that window must not receive this reply.
        let observed_generation = {
            let state = self.approvals.lock();
            match state.pending.get(&token) {
                Some(pending) => Some(pending.generation),
                None if state.was_retired(&token) => None,
                None => return ApprovalReplyOutcome::NotFound,
            }
        };

        // The token is ours, so a decision is genuinely at stake — recover the
        // identity if the listener started before `/users/me` was reachable.
        // The listener copies the ID once at startup and never refetches, so
        // without this a transient lookup failure would leave every typed
        // approval permanently unanswerable until the daemon was restarted.
        // Empty results are deliberately not cached, so this recovers as soon as
        // the endpoint does; a successful lookup is cached and costs nothing
        // thereafter.
        let recovered;
        let bot_user_id = if bot_user_id.is_empty() {
            recovered = self.get_bot_identity().await.0;
            recovered.as_str()
        } else {
            bot_user_id
        };

        // Identity is checked before the allowlist, not through it. A wildcard
        // peer group matches any string, including the empty one this yields for
        // a malformed event and including the bot's own ID — so consulting the
        // allowlist first would let the bot's own approval-shaped post answer a
        // live prompt, and would treat "no identity" as "some authorized user".
        //
        // An *unknown* bot ID fails closed rather than skipping the comparison.
        // Treating it as "no self-check needed" would invert the guard exactly
        // when identity is least certain: under a wildcard peer group the bot's
        // own post would then pass authorization and approve the tool it had
        // just prompted for.
        let reason = if user_id.is_empty() {
            Some("missing_author_identity")
        } else if bot_user_id.is_empty() {
            Some("unknown_bot_identity")
        } else if user_id == bot_user_id {
            Some("self_authored")
        } else {
            None
        };
        if let Some(reason) = reason {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"token": token, "reason": reason})),
                "refusing to decide an approval without a trustworthy author identity"
            );
            // Consumed, and the prompt stays pending: approval-shaped text must
            // never reach the model, and refusing here only defers the decision
            // to a reply made once identity can be established.
            return ApprovalReplyOutcome::Consumed;
        }

        // One critical section for the whole decision. Recognition, destination,
        // and removal cannot be split: the reaction path shares this state, and
        // a token retired between two of those steps would otherwise look like
        // text that was never ours and be forwarded to the model.
        let (outcome, resolved) = {
            let mut state = self.approvals.lock();

            let Some(pending) = state.pending.get(&token) else {
                // Already answered, cancelled, or timed out: still ours, so the
                // losing reply is swallowed rather than delivered as chat.
                return if state.was_retired(&token) {
                    ApprovalReplyOutcome::Consumed
                } else {
                    ApprovalReplyOutcome::NotFound
                };
            };

            // A different registration holds the token than the one this reply
            // was recognized against — it was reused while the identity lookup
            // awaited. The reply is still approval traffic, so it is swallowed,
            // but it must not answer a prompt it was never shown.
            if observed_generation != Some(pending.generation) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"token": token})),
                    "ignoring approval reply whose prompt was replaced by a newer request"
                );
                return ApprovalReplyOutcome::Consumed;
            }

            // The reply has to come from the channel the prompt went to. The
            // token is readable by everyone who can see the prompt, and this one
            // channel instance serves many Mattermost channels, so without this
            // an authorized user could carry a token into a different room and
            // answer there. Peer-group membership is a separate, necessary
            // condition: it says *who* may decide, not *where*.
            if pending.channel_id != reply_channel {
                let origin_channel = pending.channel_id.clone();
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "token": token,
                            "reply_channel": reply_channel,
                            "origin_channel": origin_channel,
                        })),
                    "ignoring approval reply from a channel the prompt was not posted to"
                );
                // Consumed, not resolved: the prompt stays answerable in the
                // channel it actually belongs to.
                return ApprovalReplyOutcome::Consumed;
            }

            if !self.is_user_allowed(user_id) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"user_id": user_id, "token": token})),
                    "ignoring approval reply from unauthorized user"
                );
                // Leaves the prompt pending so an authorized operator can still
                // answer, but never forwards the attempt to the model.
                return ApprovalReplyOutcome::Consumed;
            }

            (
                ApprovalReplyOutcome::Consumed,
                observed_generation
                    .and_then(|generation| state.retire_generation(&token, generation))
                    .map(|pending| pending.sender),
            )
        };

        if let Some(sender) = resolved {
            // A closed receiver means the waiter already gave up; the decision
            // is dropped and the reply is still consumed.
            let _ = sender.send(response);
        }
        outcome
    }

    #[allow(clippy::too_many_arguments)]
    async fn poll_channel(
        &self,
        target: &TargetChannel,
        token: &str,
        bot_user_id: &str,
        bot_username: &str,
        bootstrap_ms: i64,
        cursors: &mut HashMap<String, i64>,
        tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
    ) -> bool {
        let cursor = *cursors.entry(target.id.clone()).or_insert(bootstrap_ms);

        let resp = match self
            .http_client()
            .get(format!(
                "{}/api/v4/channels/{}/posts",
                self.base_url, target.id
            ))
            .bearer_auth(token)
            .query(&[("since", cursor.to_string())])
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "alias": self.alias,
                            "channel_id": target.id,
                            "error": format!("{}", e),
                        })),
                    "Mattermost poll error"
                );
                return false;
            }
        };

        let data: serde_json::Value = match resp.json().await {
            Ok(d) => d,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "alias": self.alias,
                            "channel_id": target.id,
                            "error": format!("{}", e),
                        })),
                    "Mattermost parse error"
                );
                return false;
            }
        };

        let Some(posts) = data.get("posts").and_then(|p| p.as_object()) else {
            return false;
        };

        let mut post_list: Vec<_> = posts.values().collect();
        post_list.sort_by_key(|p| p.get("create_at").and_then(|c| c.as_i64()).unwrap_or(0));

        let cursor_before_batch = cursor;
        let mut new_cursor = cursor;
        for post in post_list {
            let create_at = post
                .get("create_at")
                .and_then(|c| c.as_i64())
                .unwrap_or(new_cursor);
            new_cursor = new_cursor.max(create_at);

            if self
                .process_inbound_post(
                    post,
                    bot_user_id,
                    bot_username,
                    cursor_before_batch,
                    &target.id,
                    target.is_direct,
                    tx,
                )
                .await
            {
                return true;
            }
        }
        cursors.insert(target.id.clone(), new_cursor);
        false
    }

    fn parse_mattermost_post(
        &self,
        post: &serde_json::Value,
        bot_user_id: &str,
        bot_username: &str,
        last_create_at: i64,
        channel_id: &str,
        injected_text: Option<&str>,
        is_direct: bool,
    ) -> Option<ChannelMessage> {
        let id = post.get("id").and_then(|i| i.as_str()).unwrap_or("");
        let user_id = post.get("user_id").and_then(|u| u.as_str()).unwrap_or("");
        let text = post.get("message").and_then(|m| m.as_str()).unwrap_or("");
        let create_at = post.get("create_at").and_then(|c| c.as_i64()).unwrap_or(0);
        let root_id = post.get("root_id").and_then(|r| r.as_str()).unwrap_or("");

        if user_id == bot_user_id || create_at <= last_create_at {
            return None;
        }

        let effective_text = if text.is_empty() {
            injected_text?
        } else {
            text
        };

        if !self.is_user_allowed(user_id) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"user_id": user_id})),
                "ignoring message from unauthorized user"
            );
            return None;
        }

        // DM and group-DM channels have no ambient noise to filter against, so
        // mention_only is bypassed for them. The flag still applies on public
        // and private team channels.
        let content = if self.mention_only && !is_direct {
            let normalized =
                normalize_mattermost_content(effective_text, bot_user_id, bot_username, post);
            normalized?
        } else {
            effective_text.to_string()
        };

        // Reply routing depends on thread_replies config:
        //   - Existing thread (root_id set): always stay in the thread.
        //   - Top-level post + thread_replies=true: thread on the original post.
        //   - Top-level post + thread_replies=false: reply at channel level.
        let reply_target = if !root_id.is_empty() {
            format!("{}:{}", channel_id, root_id)
        } else if self.thread_replies {
            format!("{}:{}", channel_id, id)
        } else {
            channel_id.to_string()
        };

        Some(ChannelMessage {
            id: format!("mattermost_{id}"),
            sender: user_id.to_string(),
            reply_target,
            content,
            channel: "mattermost".to_string(),
            channel_alias: Some(self.alias.clone()),
            #[allow(clippy::cast_sign_loss)]
            timestamp: (create_at / 1000) as u64,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,

            ..Default::default()
        })
    }
}

fn post_has_audio_attachment(post: &serde_json::Value) -> bool {
    let files = post
        .get("metadata")
        .and_then(|m| m.get("files"))
        .and_then(|f| f.as_array());
    let Some(files) = files else { return false };
    files.iter().any(is_audio_file)
}

fn is_audio_file(file: &serde_json::Value) -> bool {
    let mime = file.get("mime_type").and_then(|m| m.as_str()).unwrap_or("");
    if mime.starts_with("audio/") {
        return true;
    }
    let ext = file.get("extension").and_then(|e| e.as_str()).unwrap_or("");
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "ogg" | "mp3" | "m4a" | "wav" | "opus" | "flac"
    )
}

#[cfg(test)]
fn contains_bot_mention_mm(
    text: &str,
    bot_user_id: &str,
    bot_username: &str,
    post: &serde_json::Value,
) -> bool {
    // 1. Text-based: @username (case-insensitive, word-boundary aware)
    if !find_bot_mention_spans(text, bot_username).is_empty() {
        return true;
    }

    // 2. Metadata-based: Mattermost may include a "metadata.mentions" array of user IDs.
    if !bot_user_id.is_empty()
        && let Some(mentions) = post
            .get("metadata")
            .and_then(|m| m.get("mentions"))
            .and_then(|m| m.as_array())
        && mentions.iter().any(|m| m.as_str() == Some(bot_user_id))
    {
        return true;
    }

    false
}

fn is_mattermost_username_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'
}

fn find_bot_mention_spans(text: &str, bot_username: &str) -> Vec<(usize, usize)> {
    if bot_username.is_empty() {
        return Vec::new();
    }

    let mention = format!("@{}", bot_username.to_ascii_lowercase());
    let mention_len = mention.len();
    if mention_len == 0 {
        return Vec::new();
    }

    let mention_bytes = mention.as_bytes();
    let text_bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut index = 0;

    while index + mention_len <= text_bytes.len() {
        let is_match = text_bytes[index] == b'@'
            && text_bytes[index..index + mention_len]
                .iter()
                .zip(mention_bytes.iter())
                .all(|(left, right)| left.eq_ignore_ascii_case(right));

        if is_match {
            let end = index + mention_len;
            let at_boundary = text[end..]
                .chars()
                .next()
                .is_none_or(|next| !is_mattermost_username_char(next));
            if at_boundary {
                spans.push((index, end));
                index = end;
                continue;
            }
        }

        let step = text[index..].chars().next().map_or(1, char::len_utf8);
        index += step;
    }

    spans
}

fn normalize_mattermost_content(
    text: &str,
    bot_user_id: &str,
    bot_username: &str,
    post: &serde_json::Value,
) -> Option<String> {
    let mention_spans = find_bot_mention_spans(text, bot_username);
    let metadata_mentions_bot = !bot_user_id.is_empty()
        && post
            .get("metadata")
            .and_then(|m| m.get("mentions"))
            .and_then(|m| m.as_array())
            .is_some_and(|mentions| mentions.iter().any(|m| m.as_str() == Some(bot_user_id)));

    if mention_spans.is_empty() && !metadata_mentions_bot {
        return None;
    }

    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mattermost_url_trimming() {
        let thread_replies = false;
        let mention_only = false;
        let ch = MattermostChannel::new(
            "https://mm.example.com/".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(Vec::new),
            thread_replies,
            mention_only,
        );
        assert_eq!(ch.base_url, "https://mm.example.com");
    }

    #[test]
    fn mattermost_allowlist_wildcard() {
        let thread_replies = false;
        let mention_only = false;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        );
        assert!(ch.is_user_allowed("any-id"));
    }

    #[test]
    fn mattermost_parse_post_basic() {
        let thread_replies = true;
        let mention_only = false;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        );
        let post = json!({
            "id": "post123",
            "user_id": "user456",
            "message": "hello world",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "botname",
                1_500_000_000_000_i64,
                "chan789",
                None,
                false,
            )
            .unwrap();
        assert_eq!(msg.sender, "user456");
        assert_eq!(msg.content, "hello world");
        assert_eq!(msg.reply_target, "chan789:post123"); // Default threaded reply
    }

    #[test]
    fn mattermost_parse_post_thread_replies_enabled() {
        let thread_replies = true;
        let mention_only = false;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        );
        let post = json!({
            "id": "post123",
            "user_id": "user456",
            "message": "hello world",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "botname",
                1_500_000_000_000_i64,
                "chan789",
                None,
                false,
            )
            .unwrap();
        assert_eq!(msg.reply_target, "chan789:post123"); // Threaded reply
    }

    #[test]
    fn mattermost_parse_post_thread() {
        let thread_replies = false;
        let mention_only = false;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        );
        let post = json!({
            "id": "post123",
            "user_id": "user456",
            "message": "reply",
            "create_at": 1_600_000_000_000_i64,
            "root_id": "root789"
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "botname",
                1_500_000_000_000_i64,
                "chan789",
                None,
                false,
            )
            .unwrap();
        assert_eq!(msg.reply_target, "chan789:root789"); // Stays in the thread
    }

    #[test]
    fn mattermost_parse_post_ignore_self() {
        let thread_replies = false;
        let mention_only = false;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        );
        let post = json!({
            "id": "post123",
            "user_id": "bot123",
            "message": "my own message",
            "create_at": 1_600_000_000_000_i64
        });

        let msg = ch.parse_mattermost_post(
            &post,
            "bot123",
            "botname",
            1_500_000_000_000_i64,
            "chan789",
            None,
            false,
        );
        assert!(msg.is_none());
    }

    #[test]
    fn mattermost_parse_post_ignore_old() {
        let thread_replies = false;
        let mention_only = false;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        );
        let post = json!({
            "id": "post123",
            "user_id": "user456",
            "message": "old message",
            "create_at": 1_400_000_000_000_i64
        });

        let msg = ch.parse_mattermost_post(
            &post,
            "bot123",
            "botname",
            1_500_000_000_000_i64,
            "chan789",
            None,
            false,
        );
        assert!(msg.is_none());
    }

    #[test]
    fn mattermost_parse_post_no_thread_when_disabled() {
        let thread_replies = false;
        let mention_only = false;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        );
        let post = json!({
            "id": "post123",
            "user_id": "user456",
            "message": "hello world",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "botname",
                1_500_000_000_000_i64,
                "chan789",
                None,
                false,
            )
            .unwrap();
        assert_eq!(msg.reply_target, "chan789"); // No thread suffix
    }

    #[test]
    fn mattermost_existing_thread_always_threads() {
        // Even with thread_replies=false, replies to existing threads stay in the thread
        let thread_replies = false;
        let mention_only = false;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        );
        let post = json!({
            "id": "post123",
            "user_id": "user456",
            "message": "reply in thread",
            "create_at": 1_600_000_000_000_i64,
            "root_id": "root789"
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "botname",
                1_500_000_000_000_i64,
                "chan789",
                None,
                false,
            )
            .unwrap();
        assert_eq!(msg.reply_target, "chan789:root789"); // Stays in existing thread
    }

    // ── mention_only tests ────────────────────────────────────────

    #[test]
    fn mention_only_skips_message_without_mention() {
        let thread_replies = true;
        let mention_only = true;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        );
        let post = json!({
            "id": "post1",
            "user_id": "user1",
            "message": "hello everyone",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch.parse_mattermost_post(
            &post,
            "bot123",
            "mybot",
            1_500_000_000_000_i64,
            "chan1",
            None,
            false,
        );
        assert!(msg.is_none());
    }

    #[test]
    fn mention_only_accepts_message_with_at_mention() {
        let thread_replies = true;
        let mention_only = true;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        );
        let post = json!({
            "id": "post1",
            "user_id": "user1",
            "message": "@mybot what is the weather?",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "mybot",
                1_500_000_000_000_i64,
                "chan1",
                None,
                false,
            )
            .unwrap();
        assert_eq!(msg.content, "@mybot what is the weather?");
    }

    #[test]
    fn mention_only_preserves_mention_in_body() {
        let thread_replies = true;
        let mention_only = true;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        );
        let post = json!({
            "id": "post1",
            "user_id": "user1",
            "message": "  @mybot  run status  ",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "mybot",
                1_500_000_000_000_i64,
                "chan1",
                None,
                false,
            )
            .unwrap();
        assert_eq!(msg.content, "@mybot  run status");
    }

    #[test]
    fn mention_only_admits_caption_that_is_only_the_mention() {
        let thread_replies = true;
        let mention_only = true;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        );
        let post = json!({
            "id": "post1",
            "user_id": "user1",
            "message": "@mybot",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "mybot",
                1_500_000_000_000_i64,
                "chan1",
                None,
                false,
            )
            .unwrap();
        assert_eq!(msg.content, "@mybot");
    }

    #[test]
    fn mention_only_case_insensitive() {
        let thread_replies = true;
        let mention_only = true;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        );
        let post = json!({
            "id": "post1",
            "user_id": "user1",
            "message": "@MyBot hello",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "mybot",
                1_500_000_000_000_i64,
                "chan1",
                None,
                false,
            )
            .unwrap();
        assert_eq!(msg.content, "@MyBot hello");
    }

    #[test]
    fn mention_only_detects_metadata_mentions() {
        // Even without @username in text, metadata.mentions should trigger.
        let thread_replies = true;
        let mention_only = true;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        );
        let post = json!({
            "id": "post1",
            "user_id": "user1",
            "message": "hey check this out",
            "create_at": 1_600_000_000_000_i64,
            "root_id": "",
            "metadata": {
                "mentions": ["bot123"]
            }
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "mybot",
                1_500_000_000_000_i64,
                "chan1",
                None,
                false,
            )
            .unwrap();
        // Content is preserved as-is since no @username was in the text to strip.
        assert_eq!(msg.content, "hey check this out");
    }

    #[test]
    fn mention_only_word_boundary_prevents_partial_match() {
        let thread_replies = true;
        let mention_only = true;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        );
        // "@mybotextended" should NOT match "@mybot" because it extends the username.
        let post = json!({
            "id": "post1",
            "user_id": "user1",
            "message": "@mybotextended hello",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch.parse_mattermost_post(
            &post,
            "bot123",
            "mybot",
            1_500_000_000_000_i64,
            "chan1",
            None,
            false,
        );
        assert!(msg.is_none());
    }

    #[test]
    fn mention_only_mention_in_middle_of_text() {
        let thread_replies = true;
        let mention_only = true;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        );
        let post = json!({
            "id": "post1",
            "user_id": "user1",
            "message": "hey @mybot how are you?",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "mybot",
                1_500_000_000_000_i64,
                "chan1",
                None,
                false,
            )
            .unwrap();
        assert_eq!(msg.content, "hey @mybot how are you?");
    }

    #[test]
    fn mention_only_disabled_passes_all_messages() {
        // With mention_only=false (default), messages pass through unfiltered.
        let thread_replies = true;
        let mention_only = false;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        );
        let post = json!({
            "id": "post1",
            "user_id": "user1",
            "message": "no mention here",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "mybot",
                1_500_000_000_000_i64,
                "chan1",
                None,
                false,
            )
            .unwrap();
        assert_eq!(msg.content, "no mention here");
    }

    // ── contains_bot_mention_mm unit tests ────────────────────────

    #[test]
    fn contains_mention_text_at_end() {
        let post = json!({});
        assert!(contains_bot_mention_mm(
            "hello @mybot",
            "bot123",
            "mybot",
            &post
        ));
    }

    #[test]
    fn contains_mention_text_at_start() {
        let post = json!({});
        assert!(contains_bot_mention_mm(
            "@mybot hello",
            "bot123",
            "mybot",
            &post
        ));
    }

    #[test]
    fn contains_mention_text_alone() {
        let post = json!({});
        assert!(contains_bot_mention_mm("@mybot", "bot123", "mybot", &post));
    }

    #[test]
    fn no_mention_different_username() {
        let post = json!({});
        assert!(!contains_bot_mention_mm(
            "@otherbot hello",
            "bot123",
            "mybot",
            &post
        ));
    }

    #[test]
    fn no_mention_partial_username() {
        let post = json!({});
        // "mybot" is a prefix of "mybotx" — should NOT match
        assert!(!contains_bot_mention_mm(
            "@mybotx hello",
            "bot123",
            "mybot",
            &post
        ));
    }

    #[test]
    fn mention_detects_later_valid_mention_after_partial_prefix() {
        let post = json!({});
        assert!(contains_bot_mention_mm(
            "@mybotx ignore this, but @mybot handle this",
            "bot123",
            "mybot",
            &post
        ));
    }

    #[test]
    fn mention_followed_by_punctuation() {
        let post = json!({});
        // "@mybot," — comma is not alphanumeric/underscore/dash/dot, so it's a boundary
        assert!(contains_bot_mention_mm(
            "@mybot, hello",
            "bot123",
            "mybot",
            &post
        ));
    }

    #[test]
    fn mention_via_metadata_only() {
        let post = json!({
            "metadata": { "mentions": ["bot123"] }
        });
        assert!(contains_bot_mention_mm(
            "no at mention",
            "bot123",
            "mybot",
            &post
        ));
    }

    #[test]
    fn no_mention_empty_username_no_metadata() {
        let post = json!({});
        assert!(!contains_bot_mention_mm("hello world", "bot123", "", &post));
    }

    // ── normalize_mattermost_content unit tests ───────────────────

    #[test]
    fn normalize_preserves_mention_and_trims() {
        let post = json!({});
        let result = normalize_mattermost_content("  @mybot  do stuff  ", "bot123", "mybot", &post);
        assert_eq!(result.as_deref(), Some("@mybot  do stuff"));
    }

    #[test]
    fn normalize_returns_none_for_no_mention() {
        let post = json!({});
        let result = normalize_mattermost_content("hello world", "bot123", "mybot", &post);
        assert!(result.is_none());
    }

    #[test]
    fn normalize_admits_mention_only_caption() {
        let post = json!({});
        let result = normalize_mattermost_content("@mybot", "bot123", "mybot", &post);
        assert_eq!(result.as_deref(), Some("@mybot"));
    }

    #[test]
    fn normalize_preserves_text_for_metadata_mention() {
        let post = json!({
            "metadata": { "mentions": ["bot123"] }
        });
        let result = normalize_mattermost_content("check this out", "bot123", "mybot", &post);
        assert_eq!(result.as_deref(), Some("check this out"));
    }

    #[test]
    fn normalize_preserves_multiple_mentions() {
        let post = json!({});
        let result =
            normalize_mattermost_content("@mybot hello @mybot world", "bot123", "mybot", &post);
        assert_eq!(result.as_deref(), Some("@mybot hello @mybot world"));
    }

    #[test]
    fn normalize_keeps_partial_username_mentions() {
        let post = json!({});
        let result =
            normalize_mattermost_content("@mybot hello @mybotx world", "bot123", "mybot", &post);
        assert_eq!(result.as_deref(), Some("@mybot hello @mybotx world"));
    }

    // ── Transcription tests ───────────────────────────────────────

    #[test]
    fn mattermost_manager_none_when_transcription_not_configured() {
        let thread_replies = false;
        let mention_only = false;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        );
        assert!(ch.transcription_manager.is_none());
    }

    #[test]
    fn mattermost_manager_some_when_valid_config() {
        let thread_replies = false;
        let mention_only = false;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        )
        .with_transcription(zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            api_key: Some("test_key".to_string()),
            api_url: "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
            model: "whisper-large-v3".to_string(),
            language: None,
            initial_prompt: None,
            max_audio_bytes: None,
            max_duration_secs: 600,
            openai: None,
            deepgram: None,
            assemblyai: None,
            google: None,
            local_whisper: None,
            transcribe_non_ptt_audio: false,
        });
        assert!(ch.transcription_manager.is_some());
    }

    #[test]
    fn mattermost_manager_none_and_warn_on_init_failure() {
        let thread_replies = false;
        let mention_only = false;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        )
        .with_transcription(zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            api_key: Some(String::new()),
            api_url: "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
            model: "whisper-large-v3".to_string(),
            language: None,
            initial_prompt: None,
            max_audio_bytes: None,
            max_duration_secs: 600,
            openai: None,
            deepgram: None,
            assemblyai: None,
            google: None,
            local_whisper: None,
            transcribe_non_ptt_audio: false,
        });
        assert!(ch.transcription_manager.is_none());
    }

    #[test]
    fn mattermost_post_has_audio_attachment_true_for_audio_mime() {
        let post = json!({
            "metadata": {
                "files": [
                    {
                        "id": "file1",
                        "mime_type": "audio/ogg",
                        "name": "voice.ogg"
                    }
                ]
            }
        });
        assert!(post_has_audio_attachment(&post));
    }

    #[test]
    fn mattermost_post_has_audio_attachment_true_for_audio_ext() {
        let post = json!({
            "metadata": {
                "files": [
                    {
                        "id": "file1",
                        "mime_type": "application/octet-stream",
                        "extension": "ogg"
                    }
                ]
            }
        });
        assert!(post_has_audio_attachment(&post));
    }

    #[test]
    fn mattermost_post_has_audio_attachment_false_for_image() {
        let post = json!({
            "metadata": {
                "files": [
                    {
                        "id": "file1",
                        "mime_type": "image/png",
                        "name": "screenshot.png"
                    }
                ]
            }
        });
        assert!(!post_has_audio_attachment(&post));
    }

    #[test]
    fn mattermost_post_has_audio_attachment_false_when_no_files() {
        let post = json!({
            "metadata": {}
        });
        assert!(!post_has_audio_attachment(&post));
    }

    #[test]
    fn mattermost_parse_post_uses_injected_text() {
        let thread_replies = true;
        let mention_only = false;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        );
        let post = json!({
            "id": "post123",
            "user_id": "user456",
            "message": "",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "botname",
                1_500_000_000_000_i64,
                "chan789",
                Some("transcript text"),
                false,
            )
            .unwrap();
        assert_eq!(msg.content, "transcript text");
    }

    #[test]
    fn mattermost_parse_post_rejects_empty_message_without_injected() {
        let thread_replies = true;
        let mention_only = false;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        );
        let post = json!({
            "id": "post123",
            "user_id": "user456",
            "message": "",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch.parse_mattermost_post(
            &post,
            "bot123",
            "botname",
            1_500_000_000_000_i64,
            "chan789",
            None,
            false,
        );
        assert!(msg.is_none());
    }

    #[tokio::test]
    async fn mattermost_transcribe_skips_when_manager_none() {
        let thread_replies = false;
        let mention_only = false;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        );
        let post = json!({
            "metadata": {
                "files": [
                    {
                        "id": "file1",
                        "mime_type": "audio/ogg",
                        "name": "voice.ogg"
                    }
                ]
            }
        });
        let result = ch.try_transcribe_audio_attachment(&post).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn mattermost_transcribe_skips_over_duration_limit() {
        let thread_replies = false;
        let mention_only = false;
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_test_alias",
            Arc::new(|| vec!["*".into()]),
            thread_replies,
            mention_only,
        )
        .with_transcription(zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            api_key: Some("test_key".to_string()),
            api_url: "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
            model: "whisper-large-v3".to_string(),
            language: None,
            initial_prompt: None,
            max_audio_bytes: None,
            max_duration_secs: 3600,
            openai: None,
            deepgram: None,
            assemblyai: None,
            google: None,
            local_whisper: None,
            transcribe_non_ptt_audio: false,
        });

        let post = json!({
            "metadata": {
                "files": [
                    {
                        "id": "file1",
                        "mime_type": "audio/ogg",
                        "name": "voice.ogg",
                        "duration": 7_200_000_u64
                    }
                ]
            }
        });

        let result = ch.try_transcribe_audio_attachment(&post).await;
        assert!(result.is_none());
    }

    #[cfg(test)]
    mod http_tests {
        use super::*;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        #[tokio::test]
        async fn mattermost_audio_routes_through_local_whisper() {
            let mock_server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/api/v4/files/file1"))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(b"audio bytes"))
                .mount(&mock_server)
                .await;

            Mock::given(method("POST"))
                .and(path("/v1/audio/transcriptions"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({"text": "test transcript"})),
                )
                .mount(&mock_server)
                .await;

            let whisper_url = format!("{}/v1/audio/transcriptions", mock_server.uri());
            let thread_replies = false;
            let mention_only = false;
            let ch = MattermostChannel::new(
                mock_server.uri(),
                Some("test_token".to_string()),
                None,
                None,
                Vec::new(),
                "mattermost_test_alias",
                Arc::new(|| vec!["*".into()]),
                thread_replies,
                mention_only,
            )
            .with_transcription(zeroclaw_config::schema::TranscriptionConfig {
                enabled: true,
                api_key: None,
                api_url: "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
                model: "whisper-large-v3".to_string(),
                language: None,
                initial_prompt: None,
                max_audio_bytes: None,
                max_duration_secs: 600,
                openai: None,
                deepgram: None,
                assemblyai: None,
                google: None,
                local_whisper: Some(zeroclaw_config::schema::LocalWhisperConfig {
                    url: whisper_url,
                    bearer_token: Some("test_token".to_string()),
                    max_audio_bytes: 25_000_000,
                    timeout_secs: 300,
                }),
                transcribe_non_ptt_audio: false,
            });

            let post = json!({
                "metadata": {
                    "files": [
                        {
                            "id": "file1",
                            "mime_type": "audio/ogg",
                            "name": "voice.ogg"
                        }
                    ]
                }
            });

            let result = ch.try_transcribe_audio_attachment(&post).await;
            assert_eq!(result.as_deref(), Some("[Voice] test transcript"));
        }

        #[tokio::test]
        async fn mattermost_audio_rejects_declared_oversize_before_transcription() {
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                MAX_MATTERMOST_AUDIO_BYTES + 1
            )
            .into_bytes();
            let (mattermost_url, server) =
                crate::util::spawn_raw_http_response(response, true).await;
            let mock_server = MockServer::start().await;

            Mock::given(method("POST"))
                .and(path("/v1/audio/transcriptions"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"text": "wrong"})))
                .expect(0)
                .mount(&mock_server)
                .await;

            let ch = MattermostChannel::new(
                mattermost_url,
                Some("test_token".to_string()),
                None,
                None,
                Vec::new(),
                "mattermost_test_alias",
                Arc::new(|| vec!["*".into()]),
                false,
                false,
            )
            .with_transcription(zeroclaw_config::schema::TranscriptionConfig {
                enabled: true,
                api_key: None,
                api_url: "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
                model: "whisper-large-v3".to_string(),
                language: None,
                initial_prompt: None,
                max_audio_bytes: None,
                max_duration_secs: 600,
                openai: None,
                deepgram: None,
                assemblyai: None,
                google: None,
                local_whisper: Some(zeroclaw_config::schema::LocalWhisperConfig {
                    url: format!("{}/v1/audio/transcriptions", mock_server.uri()),
                    bearer_token: Some("test_token".to_string()),
                    max_audio_bytes: 25_000_000,
                    timeout_secs: 300,
                }),
                transcribe_non_ptt_audio: false,
            });
            let post = json!({
                "metadata": {
                    "files": [{
                        "id": "file1",
                        "mime_type": "audio/ogg",
                        "name": "voice.ogg"
                    }]
                }
            });

            let result = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                ch.try_transcribe_audio_attachment(&post),
            )
            .await
            .expect("declared oversize must be rejected before reading the body");
            server.abort();

            assert!(result.is_none());
        }

        #[tokio::test]
        async fn mattermost_audio_skips_non_audio_attachment() {
            let mock_server = MockServer::start().await;

            let thread_replies = false;
            let mention_only = false;
            let ch = MattermostChannel::new(
                mock_server.uri(),
                Some("test_token".to_string()),
                None,
                None,
                Vec::new(),
                "mattermost_test_alias",
                Arc::new(|| vec!["*".into()]),
                thread_replies,
                mention_only,
            )
            .with_transcription(zeroclaw_config::schema::TranscriptionConfig {
                enabled: true,
                api_key: None,
                api_url: "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
                model: "whisper-large-v3".to_string(),
                language: None,
                initial_prompt: None,
                max_audio_bytes: None,
                max_duration_secs: 600,
                openai: None,
                deepgram: None,
                assemblyai: None,
                google: None,
                local_whisper: Some(zeroclaw_config::schema::LocalWhisperConfig {
                    url: mock_server.uri(),
                    bearer_token: Some("test_token".to_string()),
                    max_audio_bytes: 25_000_000,
                    timeout_secs: 300,
                }),
                transcribe_non_ptt_audio: false,
            });

            let post = json!({
                "metadata": {
                    "files": [
                        {
                            "id": "file1",
                            "mime_type": "image/png",
                            "name": "screenshot.png"
                        }
                    ]
                }
            });

            let result = ch.try_transcribe_audio_attachment(&post).await;
            assert!(result.is_none());
        }
    }

    // ── Multi-channel + DM contract (red) ────────────────────────────

    fn make_ch_for_scope(channel_ids: Vec<String>) -> MattermostChannel {
        MattermostChannel::new(
            "https://mm.example.com".into(),
            Some("token".into()),
            None,
            None,
            channel_ids,
            "mattermost_scope_alias",
            Arc::new(|| vec!["*".into()]),
            true,
            false,
        )
    }

    #[test]
    fn normalized_channel_id_strips_wildcard_and_blank() {
        assert_eq!(MattermostChannel::normalized_channel_id(None), None);
        assert_eq!(MattermostChannel::normalized_channel_id(Some("")), None);
        assert_eq!(MattermostChannel::normalized_channel_id(Some("   ")), None);
        assert_eq!(MattermostChannel::normalized_channel_id(Some("*")), None);
        assert_eq!(
            MattermostChannel::normalized_channel_id(Some("  abc123 ")),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn scoped_channel_ids_empty_returns_none() {
        let ch = make_ch_for_scope(Vec::new());
        assert_eq!(ch.scoped_channel_ids(), None);
    }

    #[test]
    fn scoped_channel_ids_wildcard_only_returns_none() {
        let ch = make_ch_for_scope(vec!["*".into()]);
        assert_eq!(ch.scoped_channel_ids(), None);
    }

    #[test]
    fn scoped_channel_ids_explicit_returns_dedup() {
        let ch = make_ch_for_scope(vec![
            "abc".into(),
            "  def  ".into(),
            "abc".into(),
            "*".into(),
            "".into(),
        ]);
        assert_eq!(
            ch.scoped_channel_ids(),
            Some(vec!["abc".to_string(), "def".to_string()])
        );
    }

    #[test]
    fn is_direct_channel_treats_dm_and_group_dm_as_direct() {
        assert!(is_direct_channel("D"));
        assert!(is_direct_channel("G"));
    }

    #[test]
    fn is_direct_channel_rejects_public_and_private_team_channels() {
        assert!(!is_direct_channel("O"));
        assert!(!is_direct_channel("P"));
        assert!(!is_direct_channel(""));
        assert!(!is_direct_channel("X"));
    }

    fn ch_obj(id: &str, ty: &str, team: &str) -> serde_json::Value {
        json!({"id": id, "type": ty, "team_id": team})
    }

    #[test]
    fn filter_discovered_channels_includes_all_when_no_filters() {
        let raw = vec![
            ch_obj("pub1", "O", "teamA"),
            ch_obj("priv1", "P", "teamA"),
            ch_obj("dm1", "D", ""),
            ch_obj("gdm1", "G", ""),
        ];
        let kept = filter_discovered_channels(&raw, &[], true);
        let ids: Vec<&str> = kept.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["pub1", "priv1", "dm1", "gdm1"]);
        assert!(!kept[0].is_direct);
        assert!(!kept[1].is_direct);
        assert!(kept[2].is_direct);
        assert!(kept[3].is_direct);
    }

    #[test]
    fn filter_discovered_channels_respects_team_ids_allowlist() {
        let raw = vec![
            ch_obj("pub_a", "O", "teamA"),
            ch_obj("pub_b", "O", "teamB"),
            ch_obj("priv_a", "P", "teamA"),
        ];
        let kept = filter_discovered_channels(&raw, &["teamA".to_string()], true);
        let ids: Vec<&str> = kept.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["pub_a", "priv_a"]);
    }

    #[test]
    fn filter_discovered_channels_omits_dms_when_discover_dms_false() {
        let raw = vec![
            ch_obj("pub1", "O", "teamA"),
            ch_obj("dm1", "D", ""),
            ch_obj("gdm1", "G", ""),
        ];
        let kept = filter_discovered_channels(&raw, &[], false);
        let ids: Vec<&str> = kept.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["pub1"]);
    }

    #[test]
    fn filter_discovered_channels_keeps_dms_regardless_of_team_ids() {
        let raw = vec![
            ch_obj("pub_b", "O", "teamB"),
            ch_obj("dm1", "D", ""),
            ch_obj("gdm1", "G", ""),
        ];
        let kept = filter_discovered_channels(&raw, &["teamA".to_string()], true);
        let ids: Vec<&str> = kept.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["dm1", "gdm1"]);
    }

    #[test]
    fn mention_only_bypassed_for_direct_channels_in_parse() {
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_dm_alias",
            Arc::new(|| vec!["*".into()]),
            false,
            true,
        );
        let post = json!({
            "id": "post1",
            "user_id": "user1",
            "message": "no mention here, just talking",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch
            .parse_mattermost_post(
                &post,
                "bot123",
                "mybot",
                1_500_000_000_000_i64,
                "dm_channel",
                None,
                true,
            )
            .expect("DM message must bypass mention_only and produce a ChannelMessage");
        assert_eq!(msg.content, "no mention here, just talking");
    }

    #[test]
    fn mention_only_applied_in_parse_when_is_direct_false() {
        let ch = MattermostChannel::new(
            "url".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "mattermost_group_alias",
            Arc::new(|| vec!["*".into()]),
            false,
            true,
        );
        let post = json!({
            "id": "post1",
            "user_id": "user1",
            "message": "no mention here, just talking",
            "create_at": 1_600_000_000_000_i64,
            "root_id": ""
        });

        let msg = ch.parse_mattermost_post(
            &post,
            "bot123",
            "mybot",
            1_500_000_000_000_i64,
            "pub_channel",
            None,
            false,
        );
        assert!(msg.is_none(), "public channel must enforce mention_only");
    }

    #[cfg(test)]
    mod discovery_http_tests {
        use super::*;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        #[tokio::test]
        async fn list_target_channels_discovers_via_users_me_channels() {
            let mock_server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/api/v4/users/me"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"id": "bot123", "username": "mybot"})),
                )
                .mount(&mock_server)
                .await;

            Mock::given(method("GET"))
                .and(path("/api/v4/users/me/channels"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                    {"id": "pub_a", "type": "O", "team_id": "teamA"},
                    {"id": "pub_b", "type": "O", "team_id": "teamB"},
                    {"id": "dm_x",  "type": "D", "team_id": ""},
                    {"id": "gdm_y", "type": "G", "team_id": ""},
                ])))
                .mount(&mock_server)
                .await;

            let ch = MattermostChannel::new(
                mock_server.uri(),
                Some("token".into()),
                None,
                None,
                Vec::new(),
                "mattermost_discover_alias",
                Arc::new(|| vec!["*".into()]),
                false,
                false,
            )
            .with_team_ids(vec!["teamA".to_string()])
            .with_discover_dms(true);

            let targets = ch
                .list_target_channels()
                .await
                .expect("discovery must succeed");
            let ids: Vec<&str> = targets.iter().map(|t| t.id.as_str()).collect();
            assert_eq!(
                ids,
                vec!["pub_a", "dm_x", "gdm_y"],
                "discovery should keep teamA channels and all DMs"
            );
            assert!(!targets[0].is_direct);
            assert!(targets[1].is_direct);
            assert!(targets[2].is_direct);
        }

        #[tokio::test]
        async fn list_target_channels_explicit_ids_skip_discovery_and_lookup_types() {
            let mock_server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/api/v4/channels/explicit_dm"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": "explicit_dm",
                    "type": "D",
                    "team_id": ""
                })))
                .mount(&mock_server)
                .await;

            Mock::given(method("GET"))
                .and(path("/api/v4/channels/explicit_pub"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": "explicit_pub",
                    "type": "O",
                    "team_id": "teamA"
                })))
                .mount(&mock_server)
                .await;

            let ch = MattermostChannel::new(
                mock_server.uri(),
                Some("token".into()),
                None,
                None,
                vec!["explicit_dm".into(), "explicit_pub".into()],
                "mattermost_explicit_alias",
                Arc::new(|| vec!["*".into()]),
                false,
                false,
            );

            let targets = ch
                .list_target_channels()
                .await
                .expect("explicit lookup must succeed");
            let by_id: std::collections::HashMap<_, _> = targets
                .iter()
                .map(|t| (t.id.as_str(), t.is_direct))
                .collect();
            assert_eq!(by_id.get("explicit_dm"), Some(&true));
            assert_eq!(by_id.get("explicit_pub"), Some(&false));
            assert_eq!(targets.len(), 2);
        }
    }

    #[test]
    fn test_ws_url_conversion() {
        let ch = MattermostChannel::new(
            "https://mm.example.com".into(),
            Some("token".into()),
            None,
            None,
            vec![],
            "test",
            Arc::new(Vec::new),
            false,
            false,
        );
        assert_eq!(ch.ws_url(), "wss://mm.example.com/api/v4/websocket");

        let ch2 = MattermostChannel::new(
            "http://localhost:8065".into(),
            Some("token".into()),
            None,
            None,
            vec![],
            "test",
            Arc::new(Vec::new),
            false,
            false,
        );
        assert_eq!(ch2.ws_url(), "ws://localhost:8065/api/v4/websocket");

        // server URL with path prefix should preserve it
        let ch3 = MattermostChannel::new(
            "https://mm.example.com/subpath".into(),
            Some("token".into()),
            None,
            None,
            vec![],
            "test",
            Arc::new(Vec::new),
            false,
            false,
        );
        assert_eq!(
            ch3.ws_url(),
            "wss://mm.example.com/subpath/api/v4/websocket"
        );
    }

    #[test]
    fn test_listen_mode_default_is_polling() {
        assert_eq!(
            MattermostListenMode::default(),
            MattermostListenMode::Polling
        );
    }

    #[test]
    fn test_listen_mode_serde() {
        // serialize
        assert_eq!(
            serde_json::to_string(&MattermostListenMode::Polling).unwrap(),
            "\"polling\""
        );
        assert_eq!(
            serde_json::to_string(&MattermostListenMode::Websocket).unwrap(),
            "\"websocket\""
        );

        // deserialize
        let polling: MattermostListenMode = serde_json::from_str("\"polling\"").unwrap();
        assert_eq!(polling, MattermostListenMode::Polling);

        let websocket: MattermostListenMode = serde_json::from_str("\"websocket\"").unwrap();
        assert_eq!(websocket, MattermostListenMode::Websocket);

        // deserialize unknown variant -> error
        assert!(serde_json::from_str::<MattermostListenMode>("\"unknown\"").is_err());
    }

    #[test]
    fn test_ws_event_posted_parsing() {
        let post = json!({
            "id": "post123",
            "user_id": "user456",
            "message": "hello world",
            "create_at": 1717000000000i64,
            "root_id": "",
            "channel_id": "chan789",
            "type": ""
        });

        let ch = MattermostChannel::new(
            "https://mm.example.com".into(),
            Some("token".into()),
            None,
            None,
            vec![],
            "test",
            Arc::new(|| vec!["user456".into()]),
            false,
            false,
        );

        let msg = ch
            .parse_mattermost_post(&post, "bot_user", "bot_username", 0, "chan789", None, false)
            .expect("should parse posted event post");

        assert_eq!(msg.id, "mattermost_post123");
        assert_eq!(msg.sender, "user456");
        assert_eq!(msg.content, "hello world");
    }

    #[test]
    fn test_ws_posted_envelope_post_is_json_string() {
        // Mattermost sends data.post as a JSON-encoded string, not a nested
        // object. This test exercises the extraction path the WebSocket listener
        // uses: Value::String → as_str() → from_str. The old to_string() path
        // would re-serialize as a quoted literal and silently drop the event.
        let post_obj = json!({
            "id": "post789",
            "user_id": "user999",
            "message": "ws message",
            "create_at": 1717000000000i64,
            "root_id": "",
            "channel_id": "chan111",
            "type": ""
        });
        let post_str = serde_json::to_string(&post_obj).unwrap();

        let envelope = json!({
            "event": "posted",
            "data": {
                "post": post_str
            }
        });

        let post = MattermostChannel::ws_post_from_event(&envelope)
            .expect("should parse the inner JSON string");

        let ch = MattermostChannel::new(
            "https://mm.example.com".into(),
            Some("token".into()),
            None,
            None,
            vec![],
            "test",
            Arc::new(|| vec!["user999".into()]),
            false,
            false,
        );

        let msg = ch
            .parse_mattermost_post(&post, "bot_user", "bot_username", 0, "chan111", None, false)
            .expect("should parse posted event post from envelope");

        assert_eq!(msg.id, "mattermost_post789");
        assert_eq!(msg.sender, "user999");
        assert_eq!(msg.content, "ws message");
    }

    #[test]
    fn test_ws_ping_message_format() {
        // Verify the application-level ping frame the heartbeat pinger sends.
        let seq = 1i64;
        let ping = serde_json::json!({"seq": seq, "action": "ping"});
        assert_eq!(ping["seq"], serde_json::json!(1i64));
        assert_eq!(ping["action"], serde_json::json!("ping"));

        // Round-trip: the message is a Text frame whose content is the JSON
        // string. The Mattermost server expects this exact shape.
        let text = ping.to_string();
        let roundtripped: serde_json::Value =
            serde_json::from_str(&text).expect("ping json must round-trip");
        assert_eq!(roundtripped["action"], serde_json::json!("ping"));
        assert!(roundtripped["seq"].is_i64());
    }

    #[test]
    fn test_ws_auth_challenge_format() {
        // Verify the authentication_challenge frame sent immediately after connect.
        let token = "test_bot_token";
        let seq = 1i64;
        let auth = serde_json::json!({
            "seq": seq,
            "action": "authentication_challenge",
            "data": { "token": token }
        });
        assert_eq!(auth["seq"], serde_json::json!(1i64));
        assert_eq!(
            auth["action"],
            serde_json::json!("authentication_challenge")
        );
        assert_eq!(auth["data"]["token"], serde_json::json!("test_bot_token"));

        let text = auth.to_string();
        let roundtripped: serde_json::Value =
            serde_json::from_str(&text).expect("auth json must round-trip");
        assert_eq!(
            roundtripped["data"]["token"],
            serde_json::json!("test_bot_token")
        );
    }

    #[test]
    fn test_ws_auth_response_matches_challenge_sequence() {
        let success = json!({"status": "OK", "seq_reply": 7});
        let failure = json!({"status": "FAIL", "seq_reply": 7});
        let unrelated = json!({"status": "OK", "seq_reply": 8});

        assert_eq!(MattermostChannel::ws_auth_response(&success, 7), Some(true));
        assert_eq!(
            MattermostChannel::ws_auth_response(&failure, 7),
            Some(false)
        );
        assert_eq!(MattermostChannel::ws_auth_response(&unrelated, 7), None);
    }

    #[tokio::test]
    async fn test_ws_handshake_sends_auth_before_waiting_for_hello() {
        use tokio_tungstenite::tungstenite::protocol::Role;

        let (client_io, server_io) = tokio::io::duplex(4096);
        let client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let (mut write, mut read) = client.split();

        let server_task = zeroclaw_spawn::spawn!(async move {
            let first = server
                .next()
                .await
                .expect("client should send auth first")
                .expect("auth frame should be readable");
            let WsMessage::Text(text) = first else {
                panic!("first client frame should be text auth");
            };
            let auth: serde_json::Value =
                serde_json::from_str(text.as_ref()).expect("auth should be JSON");
            assert_eq!(auth["action"], "authentication_challenge");
            assert_eq!(auth["data"]["token"], "test-token");

            server
                .send(WsMessage::Text(
                    json!({"status": "OK", "seq_reply": 7}).to_string().into(),
                ))
                .await
                .expect("server should send auth response");
            server
                .send(WsMessage::Text(
                    json!({"event": "hello", "data": {"server_version": "10.8.0"}})
                        .to_string()
                        .into(),
                ))
                .await
                .expect("server should send hello");
        });

        let version = MattermostChannel::authenticate_websocket(
            &mut write,
            &mut read,
            "test-token",
            7,
            Duration::from_secs(1),
        )
        .await
        .expect("auth response followed by hello should complete the handshake");

        assert_eq!(version, "10.8.0");
        server_task.await.expect("fake server should finish");
    }

    #[tokio::test]
    async fn test_ws_handshake_rejects_failed_auth() {
        use tokio_tungstenite::tungstenite::protocol::Role;

        let (client_io, server_io) = tokio::io::duplex(4096);
        let client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let (mut write, mut read) = client.split();

        let server_task = zeroclaw_spawn::spawn!(async move {
            server
                .next()
                .await
                .expect("auth frame should arrive")
                .unwrap();
            server
                .send(WsMessage::Text(
                    json!({"status": "FAIL", "seq_reply": 3}).to_string().into(),
                ))
                .await
                .expect("server should send rejection");
        });

        let error = MattermostChannel::authenticate_websocket(
            &mut write,
            &mut read,
            "bad-token",
            3,
            Duration::from_secs(1),
        )
        .await
        .expect_err("failed auth must end the listener attempt");

        assert!(error.to_string().contains("authentication was rejected"));
        server_task.await.expect("fake server should finish");
    }

    #[tokio::test]
    async fn test_ws_handshake_times_out_after_auth_send() {
        use tokio_tungstenite::tungstenite::protocol::Role;

        let (client_io, server_io) = tokio::io::duplex(4096);
        let client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let (mut write, mut read) = client.split();

        let server_task = zeroclaw_spawn::spawn!(async move {
            server
                .next()
                .await
                .expect("auth frame should arrive")
                .unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let error = MattermostChannel::authenticate_websocket(
            &mut write,
            &mut read,
            "test-token",
            4,
            Duration::from_millis(10),
        )
        .await
        .expect_err("a silent server must fail the handshake deadline");

        assert!(error.to_string().contains("handshake timed out"));
        server_task.abort();
    }

    #[tokio::test]
    async fn test_ws_handshake_times_out_without_hello() {
        use tokio_tungstenite::tungstenite::protocol::Role;

        let (client_io, server_io) = tokio::io::duplex(4096);
        let client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let (mut write, mut read) = client.split();

        let server_task = zeroclaw_spawn::spawn!(async move {
            server
                .next()
                .await
                .expect("auth frame should arrive")
                .unwrap();
            server
                .send(WsMessage::Text(
                    json!({"status": "OK", "seq_reply": 5}).to_string().into(),
                ))
                .await
                .expect("server should send auth response");
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let error = MattermostChannel::authenticate_websocket(
            &mut write,
            &mut read,
            "test-token",
            5,
            Duration::from_millis(10),
        )
        .await
        .expect_err("auth without hello must fail the handshake deadline");

        assert!(error.to_string().contains("handshake timed out"));
        server_task.abort();
    }

    #[tokio::test]
    async fn test_ws_handshake_times_out_without_auth_response() {
        use tokio_tungstenite::tungstenite::protocol::Role;

        let (client_io, server_io) = tokio::io::duplex(4096);
        let client = WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
        let (mut write, mut read) = client.split();

        let server_task = zeroclaw_spawn::spawn!(async move {
            server
                .next()
                .await
                .expect("auth frame should arrive")
                .unwrap();
            server
                .send(WsMessage::Text(
                    json!({"event": "hello", "data": {"server_version": "10.8.0"}})
                        .to_string()
                        .into(),
                ))
                .await
                .expect("server should send hello");
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let error = MattermostChannel::authenticate_websocket(
            &mut write,
            &mut read,
            "test-token",
            6,
            Duration::from_millis(10),
        )
        .await
        .expect_err("hello without auth response must fail the handshake deadline");

        assert!(error.to_string().contains("handshake timed out"));
        server_task.abort();
    }

    #[test]
    fn test_ws_timeout_constants() {
        // WS_READ_TIMEOUT must be strictly greater than WS_PING_INTERVAL
        // so a single missed ping does not trigger a false positive.
        assert!(
            WS_READ_TIMEOUT > WS_PING_INTERVAL,
            "WS_READ_TIMEOUT ({:?}) must exceed WS_PING_INTERVAL ({:?})",
            WS_READ_TIMEOUT,
            WS_PING_INTERVAL
        );
        // WS_READ_TIMEOUT should be at least 3× ping interval so the
        // server can miss two pings before the listener reconnects.
        assert!(
            WS_READ_TIMEOUT >= WS_PING_INTERVAL.mul_f64(3.0),
            "WS_READ_TIMEOUT ({:?}) must be ≥ 3× WS_PING_INTERVAL ({:?})",
            WS_READ_TIMEOUT,
            WS_PING_INTERVAL
        );
        assert!(WS_HANDSHAKE_TIMEOUT <= WS_READ_TIMEOUT);
    }

    #[tokio::test]
    async fn test_ws_read_timeout_detects_silent_peer() {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(10);
        tokio::select! {
            () = std::future::pending::<()>() => panic!("silent peer unexpectedly produced a frame"),
            () = tokio::time::sleep_until(deadline) => {}
        }
    }
}

/// Approval-prompt routing. Mattermost previously inherited the `Channel`
/// trait's default `request_approval`, which returns `Ok(None)` meaning "this
/// channel does not implement the prompt at all" — the runtime then denied
/// every gated tool call with `ApprovalSource::Unavailable`, and the operator
/// was never asked.
#[cfg(test)]
mod approval_tests {
    use super::*;
    use serde_json::json;

    const ALLOWED_USER: &str = "operator_user_id";
    const BOT_USER: &str = "bot_user_id";

    fn channel_with_peers(peers: Vec<String>) -> MattermostChannel {
        let ch = MattermostChannel::new(
            "https://mm.example.com".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "approval_test_alias",
            Arc::new(move || peers.clone()),
            false,
            false,
        );
        // Seed the identity these fixtures would otherwise have to fetch. The
        // host is unreachable, so `get_bot_identity` would yield an empty ID and
        // the reaction path — which now fails closed on an unknown bot identity —
        // would refuse to decide. Before that guard existed these fixtures passed
        // only because the check was skipped when the ID was empty, so seeding it
        // is what makes them exercise a real, known identity.
        let _ = ch
            .bot_identity
            .set((BOT_USER.to_string(), "glados".to_string()));
        ch
    }

    /// The channel every fixture prompt is posted into unless stated otherwise.
    const ORIGIN_CHANNEL: &str = "chan1";

    fn post_from(user_id: &str, message: &str) -> serde_json::Value {
        post_from_in(ORIGIN_CHANNEL, user_id, message)
    }

    /// The same, in an explicit channel: destination binding is only testable
    /// when the fixture can name a channel other than the prompt's.
    fn post_from_in(channel_id: &str, user_id: &str, message: &str) -> serde_json::Value {
        json!({
            "id": "post1",
            "channel_id": channel_id,
            "user_id": user_id,
            "message": message,
            "create_at": 1,
        })
    }

    /// Register a pending approval bound to `ORIGIN_CHANNEL`, as
    /// `request_approval_attributed` does.
    async fn pending_in_origin(
        ch: &MattermostChannel,
        token: &str,
    ) -> tokio::sync::oneshot::Receiver<ChannelApprovalResponse> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        ch.approvals.lock().pending.insert(
            token.to_string(),
            PendingApproval {
                generation: 0,
                channel_id: ORIGIN_CHANNEL.to_string(),
                sender: tx,
            },
        );
        rx
    }

    /// A `reaction_added` WebSocket event. Mattermost sends the reaction as a
    /// JSON *string* under `data.reaction`, so the fixture encodes it the same
    /// way the server does — a plain object here would pass a test that the
    /// real event shape fails.
    fn reaction_event(user_id: &str, post_id: &str, emoji_name: &str) -> serde_json::Value {
        json!({
            "event": "reaction_added",
            "data": {
                "reaction": json!({
                    "user_id": user_id,
                    "post_id": post_id,
                    "emoji_name": emoji_name,
                    "create_at": 1,
                })
                .to_string(),
            },
        })
    }

    /// Register a pending approval reachable by both paths, as
    /// `request_approval_attributed` does in Websocket mode.
    async fn pending_with_post(
        ch: &MattermostChannel,
        token: &str,
        post_id: &str,
    ) -> tokio::sync::oneshot::Receiver<ChannelApprovalResponse> {
        let rx = pending_in_origin(ch, token).await;
        ch.approvals
            .lock()
            .posts
            .insert(post_id.to_string(), (token.to_string(), 0));
        rx
    }

    /// The point of the feature: a tap on the seeded checkmark answers the
    /// prompt without the operator typing the token back.
    #[tokio::test]
    async fn authorized_reaction_resolves_pending_approval() {
        let ch = channel_with_peers(vec![ALLOWED_USER.to_string()]);
        let rx = pending_with_post(&ch, "abc123", "prompt_post").await;

        let consumed = ch
            .try_resolve_approval_reaction(&reaction_event(
                ALLOWED_USER,
                "prompt_post",
                APPROVAL_EMOJI_APPROVE,
            ))
            .await;

        assert!(consumed, "a decision reaction must resolve the prompt");
        assert_eq!(rx.await.unwrap(), ChannelApprovalResponse::Approve);
        assert!(
            ch.approvals.lock().pending.is_empty(),
            "resolving must retire the pending token"
        );
        assert!(
            ch.approvals.lock().posts.is_empty(),
            "resolving must retire the post mapping so a later tap finds nothing"
        );
    }

    /// The deny emoji must mean deny — not merely "something was tapped".
    #[tokio::test]
    async fn deny_reaction_resolves_as_deny() {
        let ch = channel_with_peers(vec![ALLOWED_USER.to_string()]);
        let rx = pending_with_post(&ch, "abc123", "prompt_post").await;

        assert!(
            ch.try_resolve_approval_reaction(&reaction_event(
                ALLOWED_USER,
                "prompt_post",
                APPROVAL_EMOJI_DENY,
            ))
            .await
        );
        assert_eq!(rx.await.unwrap(), ChannelApprovalResponse::Deny);
    }

    /// Reacting is as privileged as replying, so the same allowlist applies.
    /// The prompt must stay pending: an unauthorized tap cannot consume the
    /// decision an authorized operator has not made yet.
    #[tokio::test]
    async fn unauthorized_reaction_cannot_approve() {
        let ch = channel_with_peers(vec![ALLOWED_USER.to_string()]);
        let mut rx = pending_with_post(&ch, "abc123", "prompt_post").await;

        let consumed = ch
            .try_resolve_approval_reaction(&reaction_event(
                "intruder_user_id",
                "prompt_post",
                APPROVAL_EMOJI_APPROVE,
            ))
            .await;

        assert!(
            !consumed,
            "an unauthorized tap must not count as a decision"
        );
        assert!(
            rx.try_recv().is_err(),
            "an unauthorized tap must not deliver a response"
        );
        assert!(
            !ch.approvals.lock().pending.is_empty(),
            "the prompt must stay answerable by an authorized operator"
        );
    }

    /// Regression: the bot seeds both emoji itself, so its own reactions arrive
    /// as events on the post it just created. Treating those as decisions would
    /// approve every prompt the instant it was posted.
    #[tokio::test]
    async fn bot_own_seeded_reaction_is_ignored() {
        let ch = channel_with_peers(vec![ALLOWED_USER.to_string()]);
        let mut rx = pending_with_post(&ch, "abc123", "prompt_post").await;
        // `get_bot_identity` caches; seed it so the test makes no HTTP call.
        let _ = ch
            .bot_identity
            .set(("bot_user_id".to_string(), "glados".to_string()));

        let consumed = ch
            .try_resolve_approval_reaction(&reaction_event(
                "bot_user_id",
                "prompt_post",
                APPROVAL_EMOJI_APPROVE,
            ))
            .await;

        assert!(
            !consumed,
            "the bot's own seeded emoji must not self-approve"
        );
        assert!(rx.try_recv().is_err(), "no response must be delivered");
        assert!(
            !ch.approvals.lock().pending.is_empty(),
            "the prompt must stay pending"
        );
    }

    /// An ordinary reaction on the prompt post is not an answer. Only the two
    /// decision emoji are.
    #[tokio::test]
    async fn unrelated_emoji_is_not_a_decision() {
        let ch = channel_with_peers(vec![ALLOWED_USER.to_string()]);
        let mut rx = pending_with_post(&ch, "abc123", "prompt_post").await;

        assert!(
            !ch.try_resolve_approval_reaction(&reaction_event(
                ALLOWED_USER,
                "prompt_post",
                "thumbsup",
            ))
            .await
        );
        assert!(rx.try_recv().is_err());
        assert!(!ch.approvals.lock().pending.is_empty());
    }

    /// A decision emoji on some other post must not resolve a live prompt.
    #[tokio::test]
    async fn reaction_on_unrelated_post_is_ignored() {
        let ch = channel_with_peers(vec![ALLOWED_USER.to_string()]);
        let mut rx = pending_with_post(&ch, "abc123", "prompt_post").await;

        assert!(
            !ch.try_resolve_approval_reaction(&reaction_event(
                ALLOWED_USER,
                "some_other_post",
                APPROVAL_EMOJI_APPROVE,
            ))
            .await
        );
        assert!(rx.try_recv().is_err());
        assert!(!ch.approvals.lock().pending.is_empty());
    }

    /// The two paths share one decision. Whichever answers first retires the
    /// token, and the other must find nothing left to answer.
    #[tokio::test]
    async fn reply_and_reaction_cannot_both_answer_one_prompt() {
        let ch = channel_with_peers(vec![ALLOWED_USER.to_string()]);
        let rx = pending_with_post(&ch, "abc123", "prompt_post").await;

        assert!(
            ch.try_resolve_approval_reply(&post_from(ALLOWED_USER, "abc123 yes"), BOT_USER)
                .await
                == ApprovalReplyOutcome::Consumed
        );
        assert_eq!(rx.await.unwrap(), ChannelApprovalResponse::Approve);

        // The reaction arrives afterwards — the operator tapped as well as typed.
        assert!(
            !ch.try_resolve_approval_reaction(&reaction_event(
                ALLOWED_USER,
                "prompt_post",
                APPROVAL_EMOJI_DENY,
            ))
            .await,
            "a second answer must not resolve an already-retired prompt"
        );
    }

    /// A token echo from an allow-listed operator resolves the waiting prompt
    /// and is consumed, so the bare `token yes` never reaches the model.
    #[tokio::test]
    async fn authorized_reply_resolves_pending_approval() {
        let ch = channel_with_peers(vec![ALLOWED_USER.to_string()]);
        let rx = pending_in_origin(&ch, "abc123").await;

        let consumed = ch
            .try_resolve_approval_reply(&post_from(ALLOWED_USER, "abc123 yes"), BOT_USER)
            .await;

        assert_eq!(
            consumed,
            ApprovalReplyOutcome::Consumed,
            "an approval reply must not fall through to the model"
        );
        assert_eq!(rx.await.unwrap(), ChannelApprovalResponse::Approve);
        assert!(
            ch.approvals.lock().pending.is_empty(),
            "resolving must retire the pending token"
        );
    }

    /// Approving a tool call is at least as privileged as sending a message,
    /// so the allowlist is re-checked on the approval path.
    #[tokio::test]
    async fn unauthorized_reply_cannot_approve() {
        let ch = channel_with_peers(vec![ALLOWED_USER.to_string()]);
        let mut rx = pending_in_origin(&ch, "abc123").await;

        let consumed = ch
            .try_resolve_approval_reply(&post_from("intruder_user_id", "abc123 yes"), BOT_USER)
            .await;

        assert_eq!(
            consumed,
            ApprovalReplyOutcome::Consumed,
            "the reply is protocol text, not a message for the model"
        );
        assert!(
            rx.try_recv().is_err(),
            "an unauthorized reply must not decide the approval"
        );
        assert!(
            ch.approvals.lock().pending.contains_key("abc123"),
            "the prompt must stay pending so an authorized operator can still answer"
        );
    }

    /// A well-formed reply for a token this channel never issued belongs to
    /// someone else (or nobody) and must reach the normal message path.
    #[tokio::test]
    async fn unknown_token_is_not_consumed() {
        let ch = channel_with_peers(vec![ALLOWED_USER.to_string()]);
        assert!(
            ch.try_resolve_approval_reply(&post_from(ALLOWED_USER, "zzz999 yes"), BOT_USER)
                .await
                == ApprovalReplyOutcome::NotFound
        );
    }

    /// Regression: a token is readable by everyone who can see the prompt, and
    /// one channel instance serves many Mattermost channels, so an authorized
    /// user must not be able to carry a valid token into a different room and
    /// answer there.
    ///
    /// Driven through `process_inbound_post`, the production inbound path, so
    /// the binding is proven where posts actually arrive rather than only in the
    /// resolver.
    #[tokio::test]
    async fn approval_reply_from_another_channel_cannot_answer() {
        let ch = channel_with_peers(vec![ALLOWED_USER.to_string()]);
        let mut rx = pending_in_origin(&ch, "abc123").await;
        let (tx, mut inbound) = tokio::sync::mpsc::channel(8);

        // Same token, same authorized operator, wrong channel.
        ch.process_inbound_post(
            &post_from_in("chan2", ALLOWED_USER, "abc123 yes"),
            "bot_user_id",
            "glados",
            0,
            "chan2",
            false,
            &tx,
        )
        .await;

        assert!(
            rx.try_recv().is_err(),
            "a reply from another channel must not deliver a decision"
        );
        assert!(
            ch.approvals.lock().pending.contains_key("abc123"),
            "the prompt must stay answerable in the channel it belongs to"
        );
        assert!(
            inbound.try_recv().is_err(),
            "approval-reply text must not fall through to the model either"
        );

        // The same reply in the origin channel resolves it.
        ch.process_inbound_post(
            &post_from(ALLOWED_USER, "abc123 yes"),
            "bot_user_id",
            "glados",
            0,
            ORIGIN_CHANNEL,
            false,
            &tx,
        )
        .await;

        assert_eq!(rx.await.unwrap(), ChannelApprovalResponse::Approve);
        assert!(ch.approvals.lock().pending.is_empty());
    }

    /// Build a channel pointed at a mock Mattermost server.
    fn channel_at(base_url: &str, peers: Vec<String>) -> MattermostChannel {
        MattermostChannel::new(
            base_url.to_string(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "approval_test_alias",
            Arc::new(move || peers.clone()),
            false,
            false,
        )
    }

    fn approval_request() -> zeroclaw_api::channel::ChannelApprovalRequest {
        zeroclaw_api::channel::ChannelApprovalRequest {
            tool_name: "shell".into(),
            arguments_summary: "rm -rf /".into(),
            raw_arguments: None,
        }
    }

    /// The real timeout regression: the prompt posts successfully, nobody
    /// answers, and the runtime denies on its own authority.
    ///
    /// `approval_timeout_secs = 0` is an already-elapsed deadline, which is the
    /// documented "0 denies immediately" semantics, so this also pins that.
    #[tokio::test]
    async fn timeout_after_successful_post_is_attributed_to_the_runtime() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v4/posts"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "prompt_post"})))
            .mount(&server)
            .await;

        let ch =
            channel_at(&server.uri(), vec![ALLOWED_USER.to_string()]).with_approval_timeout_secs(0);

        let attributed = ch
            .request_approval_attributed(ORIGIN_CHANNEL, &approval_request())
            .await
            .expect("a successful post must not surface as an error")
            .expect("Mattermost answers approvals, so it must not return None");

        assert_eq!(attributed.response, ChannelApprovalResponse::Deny);
        assert_eq!(
            attributed.source,
            zeroclaw_api::channel::ApprovalSource::TimedOut,
            "an unanswered prompt is the runtime denying, not an operator refusing"
        );
        assert!(
            ch.approvals.lock().pending.is_empty(),
            "a timed-out prompt must not leak its token"
        );
        assert!(
            ch.approvals.lock().posts.is_empty(),
            "a timed-out prompt must not leak its post binding"
        );
    }

    /// The prompt actually reaches Mattermost, and websocket mode actually
    /// seeds both decision emoji. Asserted at the HTTP boundary, since
    /// `create_post` and `react_to_post` are otherwise only exercised by their
    /// callers' return values.
    #[tokio::test]
    async fn websocket_mode_posts_prompt_and_seeds_both_reactions() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/users/me"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"id": "bot_user_id", "username": "glados"})),
            )
            .mount(&server)
            .await;
        // The prompt carries the token and the tool, or the operator cannot
        // answer. Matched on those protocol-exact fields rather than on the
        // heading: the heading comes from the Fluent catalogue and is localized,
        // so asserting its English spelling would fail this test under a
        // supported non-English locale even though the prompt is correct. The
        // token and tool name are echoed verbatim in every locale — that is
        // precisely what `parse_approval_reply` matches on.
        Mock::given(method("POST"))
            .and(path("/api/v4/posts"))
            .and(body_string_contains("shell"))
            .and(body_string_contains("rm -rf /"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "prompt_post"})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v4/reactions"))
            .and(body_string_contains(APPROVAL_EMOJI_APPROVE))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v4/reactions"))
            .and(body_string_contains(r#""emoji_name":"x""#))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let ch = channel_at(&server.uri(), vec![ALLOWED_USER.to_string()])
            .with_listen_mode(zeroclaw_config::schema::MattermostListenMode::Websocket)
            .with_approval_timeout_secs(0);

        let _ = ch
            .request_approval_attributed(ORIGIN_CHANNEL, &approval_request())
            .await;

        // MockServer verifies the `expect(..)` counts when it drops.
        drop(server);
    }

    /// The mirror of the test above: polling mode must not seed emoji, because
    /// the polling listener never receives `reaction_added` and the operator
    /// would be tapping a control that does nothing.
    #[tokio::test]
    async fn polling_mode_posts_prompt_without_seeding_reactions() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v4/posts"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "prompt_post"})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v4/reactions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(0)
            .mount(&server)
            .await;

        let ch =
            channel_at(&server.uri(), vec![ALLOWED_USER.to_string()]).with_approval_timeout_secs(0);
        let _ = ch
            .request_approval_attributed(ORIGIN_CHANNEL, &approval_request())
            .await;

        assert!(
            ch.approvals.lock().posts.is_empty(),
            "polling mode must not register a reaction binding"
        );
        drop(server);
    }

    /// Ordinary conversation must never be swallowed by the approval path.
    #[tokio::test]
    async fn ordinary_message_is_not_consumed() {
        let ch = channel_with_peers(vec![ALLOWED_USER.to_string()]);
        let _rx = pending_in_origin(&ch, "abc123").await;

        assert!(
            ch.try_resolve_approval_reply(
                &post_from(ALLOWED_USER, "what is the weather"),
                BOT_USER
            )
            .await
                == ApprovalReplyOutcome::NotFound
        );
        assert!(ch.approvals.lock().pending.contains_key("abc123"));
    }

    /// `deny` and `always` must map to their own outcomes rather than
    /// collapsing into the generic approve/deny pair.
    #[tokio::test]
    async fn deny_and_always_map_to_distinct_responses() {
        for (reply, expected) in [
            ("abc123 no", ChannelApprovalResponse::Deny),
            ("abc123 always", ChannelApprovalResponse::AlwaysApprove),
        ] {
            let ch = channel_with_peers(vec![ALLOWED_USER.to_string()]);
            let rx = pending_in_origin(&ch, "abc123").await;
            assert!(
                ch.try_resolve_approval_reply(&post_from(ALLOWED_USER, reply), BOT_USER)
                    .await
                    == ApprovalReplyOutcome::Consumed
            );
            assert_eq!(rx.await.unwrap(), expected);
        }
    }

    /// A zero timeout is an already-elapsed deadline. The config default must
    /// therefore be the documented budget, not the zero a derived `Default`
    /// would produce — otherwise an alias built in Rust denies every approval.
    #[test]
    fn constructor_default_timeout_is_not_zero() {
        let ch = channel_with_peers(Vec::new());
        assert_eq!(
            ch.approval_timeout_secs,
            zeroclaw_config::schema::MattermostConfig::default().approval_timeout_secs,
        );
        assert!(ch.approval_timeout_secs > 0);
    }

    /// A prompt that never reaches Mattermost is an error, not a decision.
    ///
    /// Renamed from `timeout_denies_attributed_to_runtime_not_operator`, which
    /// this never tested: the unreachable test host makes `create_post` fail, so
    /// it returns through the post-`Err` branch and never reaches the timeout
    /// arms. `timeout_after_successful_post_is_attributed_to_the_runtime` below
    /// is the actual timeout regression.
    #[tokio::test]
    async fn failed_prompt_post_is_an_error_not_an_operator_decision() {
        let ch = channel_with_peers(vec![ALLOWED_USER.to_string()]).with_approval_timeout_secs(0);
        let request = zeroclaw_api::channel::ChannelApprovalRequest {
            tool_name: "shell".into(),
            arguments_summary: "rm -rf /".into(),
            raw_arguments: None,
        };

        // `send` fails against the unreachable test host, which surfaces as an
        // Err rather than a synthesized operator decision.
        let result = ch.request_approval_attributed("chan1", &request).await;
        assert!(
            result.is_err(),
            "a failed prompt post must not be reported as an operator decision"
        );
        assert!(
            ch.approvals.lock().pending.is_empty(),
            "a failed prompt must not leak its pending token"
        );
    }
}

/// Regressions for the three approval lifecycle/authorization defects raised in
/// review: a losing text reply reaching the model, cancelled futures stranding
/// state, and approval decisions bypassing the identity checks that
/// `parse_mattermost_post` applies to ordinary messages.
#[cfg(test)]
mod approval_lifecycle_tests {
    use super::*;
    use serde_json::json;

    const ALLOWED_USER: &str = "operator_user_id";
    const BOT_USER: &str = "bot_user_id";
    const ORIGIN_CHANNEL: &str = "chan1";

    fn channel_with_peers(peers: Vec<String>) -> MattermostChannel {
        MattermostChannel::new(
            "https://mm.example.com".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "approval_lifecycle_alias",
            Arc::new(move || peers.clone()),
            false,
            false,
        )
    }

    fn post_from(user_id: &str, message: &str) -> serde_json::Value {
        json!({
            "id": "post1",
            "user_id": user_id,
            "message": message,
            "create_at": 1,
            "channel_id": ORIGIN_CHANNEL,
        })
    }

    fn pending(
        ch: &MattermostChannel,
        token: &str,
    ) -> tokio::sync::oneshot::Receiver<ChannelApprovalResponse> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        ch.approvals.lock().pending.insert(
            token.to_string(),
            PendingApproval {
                generation: 0,
                channel_id: ORIGIN_CHANNEL.to_string(),
                sender: tx,
            },
        );
        rx
    }

    /// The losing half of a reply/reaction race must not become chat.
    ///
    /// Recognition and removal are one transition now, so a reply arriving after
    /// the reaction already answered cannot be mistaken for text that was never
    /// ours. Previously the final `remove` returned `None`, the resolver
    /// reported "not mine", and `process_inbound_post` forwarded the operator's
    /// `<token> yes` to the model as ordinary conversation.
    #[tokio::test]
    async fn losing_reply_after_reaction_wins_is_not_forwarded_to_the_model() {
        let ch = channel_with_peers(vec![ALLOWED_USER.to_string()]);
        let rx = pending(&ch, "abc123");
        let (tx, mut inbound) = tokio::sync::mpsc::channel(8);

        // The reaction path answers first and retires the token, the way
        // production does it — by generation, not by token.
        let generation = ch
            .approvals
            .lock()
            .pending
            .get("abc123")
            .expect("token was live")
            .generation;
        let resolved = ch
            .approvals
            .lock()
            .retire_generation("abc123", generation)
            .expect("token was live");
        let _ = resolved.sender.send(ChannelApprovalResponse::Approve);
        assert_eq!(rx.await.unwrap(), ChannelApprovalResponse::Approve);

        // The operator's typed reply is already in flight and lands afterwards.
        ch.process_inbound_post(
            &post_from(ALLOWED_USER, "abc123 yes"),
            BOT_USER,
            "glados",
            0,
            ORIGIN_CHANNEL,
            false,
            &tx,
        )
        .await;

        assert!(
            inbound.try_recv().is_err(),
            "a reply that lost the race must be suppressed, not delivered to the model"
        );
    }

    /// Text that merely looks like an approval reply, for a token this channel
    /// never issued, is ordinary conversation and must still reach the model.
    #[tokio::test]
    async fn unrelated_token_shaped_text_still_reaches_the_model() {
        let ch = channel_with_peers(vec![ALLOWED_USER.to_string()]);
        assert_eq!(
            ch.try_resolve_approval_reply(&post_from(ALLOWED_USER, "zzz999 yes"), BOT_USER)
                .await,
            ApprovalReplyOutcome::NotFound,
            "only tokens this channel knows about may be swallowed"
        );
    }

    /// Retired-token memory is bounded, so a long-lived channel cannot grow it
    /// without limit.
    #[test]
    fn retired_token_memory_is_bounded() {
        let ch = channel_with_peers(Vec::new());
        for index in 0..(RETIRED_APPROVAL_TOKENS * 3) {
            ch.approvals
                .lock()
                .remember_retired(&format!("tok{index:04}"));
        }
        assert_eq!(
            ch.approvals.lock().retired.len(),
            RETIRED_APPROVAL_TOKENS,
            "the retired ring must stay capped"
        );
    }

    /// Dropping the request future must retire everything it registered.
    ///
    /// The routed approval caller wraps this future in an outer timeout whose
    /// default is shorter than this channel's own, so cancellation mid-prompt is
    /// a supported configuration. Cleanup used to live only in the explicit
    /// match arms, which a dropped future never reaches.
    #[test]
    fn dropping_the_guard_retires_pending_and_post_state() {
        let ch = channel_with_peers(vec![ALLOWED_USER.to_string()]);
        let _rx = pending(&ch, "abc123");
        ch.approvals
            .lock()
            .posts
            .insert("post1".to_string(), ("abc123".to_string(), 0));

        {
            let _guard = ApprovalGuard::new(Arc::clone(&ch.approvals), "abc123".to_string(), 0);
            assert!(!ch.approvals.lock().pending.is_empty());
        } // dropped here, as an outer timeout would

        let state = ch.approvals.lock();
        assert!(
            state.pending.is_empty(),
            "a cancelled request must not strand its pending token"
        );
        assert!(
            state.posts.is_empty(),
            "a cancelled request must not strand its post binding"
        );
        assert!(
            state.was_retired("abc123"),
            "the token must be remembered so a late tap is still recognized"
        );
    }

    /// A guard that already retired its token must not retire it a second time
    /// when dropped — otherwise the normal completion path would evict a token
    /// a *later* request had legitimately reused.
    #[test]
    fn explicit_retire_disarms_the_guard() {
        let ch = channel_with_peers(Vec::new());
        let _rx = pending(&ch, "abc123");
        {
            let mut guard = ApprovalGuard::new(Arc::clone(&ch.approvals), "abc123".to_string(), 0);
            guard.retire();
            // A new request reuses the same token before the guard drops.
            let _reused = pending(&ch, "abc123");
        }
        assert!(
            ch.approvals.lock().pending.contains_key("abc123"),
            "a disarmed guard must not evict a later request's token"
        );
    }

    /// The bot's own approval-shaped post must never answer a prompt, even when
    /// the peer group is a wildcard that matches every identity.
    #[tokio::test]
    async fn bot_authored_reply_cannot_self_approve_under_wildcard() {
        let ch = channel_with_peers(vec!["*".to_string()]);
        let mut rx = pending(&ch, "abc123");

        let outcome = ch
            .try_resolve_approval_reply(&post_from(BOT_USER, "abc123 yes"), BOT_USER)
            .await;

        assert_eq!(
            outcome,
            ApprovalReplyOutcome::Consumed,
            "the bot's own echo is protocol traffic, not a message for the model"
        );
        assert!(
            rx.try_recv().is_err(),
            "the bot must not be able to approve its own prompt"
        );
        assert!(
            ch.approvals.lock().pending.contains_key("abc123"),
            "the prompt must stay answerable by a real operator"
        );
    }

    /// A malformed event with no author must not be treated as an authorized
    /// operator by a wildcard allowlist.
    #[tokio::test]
    async fn missing_identity_reply_cannot_approve_under_wildcard() {
        let ch = channel_with_peers(vec!["*".to_string()]);
        let mut rx = pending(&ch, "abc123");

        let outcome = ch
            .try_resolve_approval_reply(
                &json!({
                    "id": "post1",
                    "message": "abc123 yes",
                    "create_at": 1,
                    "channel_id": ORIGIN_CHANNEL,
                }),
                BOT_USER,
            )
            .await;

        assert_eq!(outcome, ApprovalReplyOutcome::Consumed);
        assert!(
            rx.try_recv().is_err(),
            "an event with no identity must not decide an approval"
        );
    }

    /// The reaction path has the same missing-identity exposure and the same
    /// rule: a wildcard peer group must not turn an absent author into an
    /// authorized one.
    #[tokio::test]
    async fn missing_identity_reaction_cannot_approve_under_wildcard() {
        let ch = channel_with_peers(vec!["*".to_string()]);
        let mut rx = pending(&ch, "abc123");
        ch.approvals
            .lock()
            .posts
            .insert("post1".to_string(), ("abc123".to_string(), 0));

        let event = json!({
            "data": {
                "reaction": json!({
                    "post_id": "post1",
                    "emoji_name": APPROVAL_EMOJI_APPROVE,
                })
                .to_string(),
            }
        });

        assert!(
            !ch.try_resolve_approval_reaction(&event).await,
            "a reaction with no author must not resolve an approval"
        );
        assert!(rx.try_recv().is_err());
        assert!(ch.approvals.lock().pending.contains_key("abc123"));
    }
}

/// The approval path must fail closed when the authenticated bot identity is
/// unavailable.
///
/// `get_bot_identity` yields an empty ID whenever `/users/me` is unreachable,
/// unauthorized, or answers without one. The self-approval guard used to skip
/// the comparison in that case, which inverted it exactly when identity was
/// least certain: with the documented wildcard peer group the bot's own user ID
/// passes authorization, so the bot could answer the prompt it had just posted —
/// and in WebSocket mode its own seeded ✅ arrives within milliseconds.
#[cfg(test)]
mod approval_identity_tests {
    use super::*;
    use serde_json::json;

    const BOT_USER: &str = "bot_user_id";
    const ORIGIN_CHANNEL: &str = "chan1";
    /// The identity lookup failed, so the resolver has no ID to compare against.
    const UNKNOWN_BOT: &str = "";

    /// The documented wildcard peer group, which accepts every non-empty ID and
    /// is therefore the configuration in which this guard is load-bearing.
    fn wildcard_channel() -> MattermostChannel {
        MattermostChannel::new(
            "https://mm.example.com".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "approval_identity_alias",
            Arc::new(|| vec!["*".to_string()]),
            false,
            false,
        )
    }

    fn pending(
        ch: &MattermostChannel,
        token: &str,
    ) -> tokio::sync::oneshot::Receiver<ChannelApprovalResponse> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        ch.approvals.lock().pending.insert(
            token.to_string(),
            PendingApproval {
                generation: 0,
                channel_id: ORIGIN_CHANNEL.to_string(),
                sender: tx,
            },
        );
        rx
    }

    /// Production boundary: a bot-authored reply, an unknown bot ID, and a
    /// wildcard allowlist must not resolve the prompt or reach the model.
    #[tokio::test]
    async fn bot_reply_cannot_approve_when_bot_identity_is_unknown() {
        let ch = wildcard_channel();
        let mut rx = pending(&ch, "abc123");
        let (tx, mut inbound) = tokio::sync::mpsc::channel(8);

        ch.process_inbound_post(
            &json!({
                "id": "post1",
                "user_id": BOT_USER,
                "message": "abc123 yes",
                "create_at": 1,
                "channel_id": ORIGIN_CHANNEL,
            }),
            UNKNOWN_BOT,
            "glados",
            0,
            ORIGIN_CHANNEL,
            false,
            &tx,
        )
        .await;

        assert!(
            rx.try_recv().is_err(),
            "an unresolved bot identity must not let the bot approve its own prompt"
        );
        assert!(
            ch.approvals.lock().pending.contains_key("abc123"),
            "the prompt must stay answerable once identity can be established"
        );
        assert!(
            inbound.try_recv().is_err(),
            "approval-shaped text must not fall through to the model"
        );
    }

    /// The reaction path is the sharper case: the bot seeds ✅ itself, so an
    /// unknown identity here would approve within milliseconds of prompting.
    #[tokio::test]
    async fn bot_reaction_cannot_approve_when_bot_identity_is_unknown() {
        let ch = wildcard_channel();
        let mut rx = pending(&ch, "abc123");
        ch.approvals
            .lock()
            .posts
            .insert("post1".to_string(), ("abc123".to_string(), 0));

        // `get_bot_identity` will fail against this unreachable host, yielding
        // the empty ID this test is about — the same value the listener passes
        // on when `/users/me` is unavailable.
        let event = json!({
            "data": {
                "reaction": json!({
                    "post_id": "post1",
                    "user_id": BOT_USER,
                    "emoji_name": APPROVAL_EMOJI_APPROVE,
                })
                .to_string(),
            }
        });

        assert!(
            !ch.try_resolve_approval_reaction(&event).await,
            "the bot's own seeded reaction must not approve while identity is unknown"
        );
        assert!(rx.try_recv().is_err());
        assert!(
            ch.approvals.lock().pending.contains_key("abc123"),
            "the prompt must stay pending"
        );
    }

    /// Even a genuine third-party operator is refused while identity is unknown:
    /// without a bot ID the resolver cannot tell them apart from the bot, so it
    /// declines rather than guessing.
    #[tokio::test]
    async fn third_party_reply_is_refused_while_bot_identity_is_unknown() {
        let ch = wildcard_channel();
        let mut rx = pending(&ch, "abc123");

        let outcome = ch
            .try_resolve_approval_reply(
                &json!({
                    "id": "post1",
                    "user_id": "operator_user_id",
                    "message": "abc123 yes",
                    "create_at": 1,
                    "channel_id": ORIGIN_CHANNEL,
                }),
                UNKNOWN_BOT,
            )
            .await;

        assert_eq!(
            outcome,
            ApprovalReplyOutcome::Consumed,
            "the reply is still approval traffic and must not reach the model"
        );
        assert!(rx.try_recv().is_err());
        assert!(ch.approvals.lock().pending.contains_key("abc123"));
    }

    /// A username-only `/users/me` response must not be cached: pinning an empty
    /// ID would disable the self-approval guard for the channel's whole life.
    ///
    /// Driven through the real `get_bot_identity` against a mock server, so it
    /// tests the production caching rule rather than restating it.
    #[tokio::test]
    async fn partial_identity_response_is_not_cached() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // A response carrying a username but no `id` — the shape the review
        // identified as permanently poisoning the cache.
        Mock::given(method("GET"))
            .and(path("/api/v4/users/me"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"username": "glados"})))
            .mount(&server)
            .await;

        let ch = MattermostChannel::new(
            server.uri(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "approval_identity_alias",
            Arc::new(|| vec!["*".to_string()]),
            false,
            false,
        );

        let (id, username) = ch.get_bot_identity().await;
        assert!(id.is_empty(), "the response carried no id");
        assert_eq!(username, "glados");
        assert!(
            ch.bot_identity.get().is_none(),
            "a response with no ID must not populate the identity cache, or the \
             self-approval guard stays disabled for the channel's whole life"
        );
    }
}

/// Regressions for the registration-generation race and the identity-recovery
/// availability gap raised in review.
#[cfg(test)]
mod approval_generation_tests {
    use super::*;
    use serde_json::json;

    const ALLOWED_USER: &str = "operator_user_id";
    const BOT_USER: &str = "bot_user_id";
    const ORIGIN_CHANNEL: &str = "chan1";

    fn channel(peers: Vec<String>, base_url: String) -> MattermostChannel {
        MattermostChannel::new(
            base_url,
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "approval_generation_alias",
            Arc::new(move || peers.clone()),
            false,
            false,
        )
    }

    /// Register a prompt the way `request_approval_attributed` does, returning
    /// the receiver and the generation it was given.
    fn register(
        ch: &MattermostChannel,
        token: &str,
        post_id: &str,
    ) -> (tokio::sync::oneshot::Receiver<ChannelApprovalResponse>, u64) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut state = ch.approvals.lock();
        let generation = state.next_generation;
        state.next_generation += 1;
        state.pending.insert(
            token.to_string(),
            PendingApproval {
                generation,
                channel_id: ORIGIN_CHANNEL.to_string(),
                sender: tx,
            },
        );
        state
            .posts
            .insert(post_id.to_string(), (token.to_string(), generation));
        (rx, generation)
    }

    /// A guard armed for an earlier request must not retire a later request that
    /// happened to draw the same token.
    ///
    /// `new_approval_token` is six random characters — finite, and not a
    /// uniqueness guarantee. Keying cleanup on the token alone meant a stale
    /// guard could delete a live prompt's sender and post binding, stranding an
    /// approval the operator could still see on screen.
    #[test]
    fn stale_guard_cannot_retire_a_newer_same_token_registration() {
        let ch = channel(Vec::new(), "https://mm.example.com".into());

        // Old request registers, and its guard stays armed.
        let (_old_rx, old_generation) = register(&ch, "abc123", "old_post");
        let old_guard = ApprovalGuard::new(
            Arc::clone(&ch.approvals),
            "abc123".to_string(),
            old_generation,
        );

        // The old registration goes away the way a resolving path would retire
        // it, and a new request then draws the same token.
        ch.approvals
            .lock()
            .retire_generation("abc123", old_generation);
        let (mut new_rx, new_generation) = register(&ch, "abc123", "new_post");
        assert_ne!(old_generation, new_generation);

        // The stale guard drops afterwards.
        drop(old_guard);

        let state = ch.approvals.lock();
        assert!(
            state.pending.contains_key("abc123"),
            "a stale guard must not retire a newer registration's prompt"
        );
        assert!(
            state.posts.contains_key("new_post"),
            "a stale guard must not drop a newer registration's post binding"
        );
        drop(state);
        assert!(
            new_rx.try_recv().is_err(),
            "the newer prompt must still be awaiting a decision, not cancelled"
        );
    }

    /// The guard for the *live* registration still cleans up on drop.
    #[test]
    fn matching_guard_still_retires_its_own_registration() {
        let ch = channel(Vec::new(), "https://mm.example.com".into());
        let (_rx, generation) = register(&ch, "abc123", "post1");
        drop(ApprovalGuard::new(
            Arc::clone(&ch.approvals),
            "abc123".to_string(),
            generation,
        ));
        let state = ch.approvals.lock();
        assert!(
            state.pending.is_empty(),
            "the owning guard must still clean up"
        );
        assert!(state.posts.is_empty(), "and must drop its own post binding");
    }

    /// A transient `/users/me` failure at listener startup must not leave typed
    /// approvals permanently unanswerable.
    ///
    /// Both listeners copy the bot ID once and never refetch, so before this the
    /// operator had to restart the daemon. The resolver now recovers the
    /// identity when the copied value is empty; empty results are not cached, so
    /// the first successful lookup is the one that sticks.
    #[tokio::test]
    async fn typed_approval_recovers_after_a_transient_identity_failure() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // Fails first, then succeeds — the shape of a listener that started
        // while Mattermost was briefly unavailable.
        Mock::given(method("GET"))
            .and(path("/api/v4/users/me"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v4/users/me"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"id": BOT_USER, "username": "glados"})),
            )
            .mount(&server)
            .await;

        let ch = channel(vec![ALLOWED_USER.to_string()], server.uri());

        // The listener's startup lookup fails, yielding the empty ID it then
        // carries into every `process_inbound_post` call.
        let (startup_id, _) = ch.get_bot_identity().await;
        assert!(startup_id.is_empty(), "the first lookup must fail");

        let (_rx, _generation) = register(&ch, "abc123", "post1");
        let outcome = ch
            .try_resolve_approval_reply(
                &json!({
                    "id": "post2",
                    "user_id": ALLOWED_USER,
                    "message": "abc123 yes",
                    "create_at": 1,
                    "channel_id": ORIGIN_CHANNEL,
                }),
                &startup_id,
            )
            .await;

        assert_eq!(outcome, ApprovalReplyOutcome::Consumed);
        assert!(
            ch.approvals.lock().pending.is_empty(),
            "the operator must be able to answer once the identity lookup recovers, \
             without restarting the daemon"
        );
    }

    /// A refusal must not swallow text this channel has no claim on.
    ///
    /// With the identity checks ahead of the ownership lookup, an unresolved bot
    /// identity turned every six-character approval-shaped message into
    /// `Consumed`, losing unrelated user text.
    #[tokio::test]
    async fn unknown_token_reaches_the_model_even_when_identity_is_unavailable() {
        let ch = channel(vec!["*".to_string()], "https://mm.example.com".into());
        assert_eq!(
            ch.try_resolve_approval_reply(
                &json!({
                    "id": "post1",
                    "user_id": ALLOWED_USER,
                    "message": "zzz999 yes",
                    "create_at": 1,
                    "channel_id": ORIGIN_CHANNEL,
                }),
                "",
            )
            .await,
            ApprovalReplyOutcome::NotFound,
            "a security refusal must not also lose unrelated user text"
        );
    }
}

/// Registration identity must hold across token allocation, post binding, and
/// both decision paths — not just guard cleanup.
///
/// The six-character token is not a request identity. Where a decision path
/// acted on the token alone, a later request that drew the same token could
/// receive a decision made for an earlier prompt: a confused deputy that grants
/// authority to the wrong request rather than merely denying one.
#[cfg(test)]
mod approval_registration_identity_tests {
    use super::*;
    use serde_json::json;

    const ALLOWED_USER: &str = "operator_user_id";
    const BOT_USER: &str = "bot_user_id";
    const ORIGIN_CHANNEL: &str = "chan1";

    fn channel() -> MattermostChannel {
        let ch = MattermostChannel::new(
            "https://mm.example.com".into(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "approval_registration_alias",
            Arc::new(|| vec![ALLOWED_USER.to_string()]),
            false,
            false,
        );
        let _ = ch
            .bot_identity
            .set((BOT_USER.to_string(), "glados".to_string()));
        ch
    }

    fn register(
        ch: &MattermostChannel,
        token: &str,
        post_id: Option<&str>,
    ) -> (tokio::sync::oneshot::Receiver<ChannelApprovalResponse>, u64) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut state = ch.approvals.lock();
        let generation = state.next_generation;
        state.next_generation += 1;
        state.pending.insert(
            token.to_string(),
            PendingApproval {
                generation,
                channel_id: ORIGIN_CHANNEL.to_string(),
                sender: tx,
            },
        );
        if let Some(post_id) = post_id {
            state
                .posts
                .insert(post_id.to_string(), (token.to_string(), generation));
        }
        (rx, generation)
    }

    /// A tap on an older prompt's still-visible post must not answer the newer
    /// request that has since taken the token.
    #[tokio::test]
    async fn reaction_on_an_older_post_cannot_resolve_a_newer_registration() {
        let ch = channel();

        // Request A registers and binds its post, then goes away.
        let (_a_rx, a_generation) = register(&ch, "abc123", Some("post_a"));
        ch.approvals.lock().pending.remove("abc123");

        // Request B draws the same token. A's post binding is still resident.
        let (mut b_rx, b_generation) = register(&ch, "abc123", None);
        assert_ne!(a_generation, b_generation);

        let event = json!({
            "data": {
                "reaction": json!({
                    "post_id": "post_a",
                    "user_id": ALLOWED_USER,
                    "emoji_name": APPROVAL_EMOJI_APPROVE,
                })
                .to_string(),
            }
        });

        assert!(
            !ch.try_resolve_approval_reaction(&event).await,
            "a reaction on A's post must not resolve B"
        );
        assert!(
            b_rx.try_recv().is_err(),
            "B must not receive a decision made for A's prompt"
        );
        assert!(
            ch.approvals.lock().owns("abc123", b_generation),
            "B's registration must survive a tap meant for A"
        );
    }

    /// A typed reply recognized against one registration must not resolve a
    /// different one that took the token while the identity lookup awaited.
    ///
    /// The swap has to happen *inside* `get_bot_identity().await` — that is the
    /// window the reviewer identified — so the mock delays its response while
    /// the test replaces the registration.
    #[tokio::test]
    async fn typed_reply_cannot_resolve_a_registration_that_replaced_it() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/users/me"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"id": BOT_USER, "username": "glados"}))
                    .set_delay(Duration::from_millis(300)),
            )
            .mount(&server)
            .await;

        let ch = Arc::new(MattermostChannel::new(
            server.uri(),
            Some("token".into()),
            None,
            None,
            Vec::new(),
            "approval_registration_alias",
            Arc::new(|| vec![ALLOWED_USER.to_string()]),
            false,
            false,
        ));

        let (_a_rx, a_generation) = register(&ch, "abc123", None);

        // Empty bot ID forces the identity refresh, so the resolver recognizes
        // A's registration and then awaits the delayed lookup.
        let resolver = {
            let ch = Arc::clone(&ch);
            zeroclaw_spawn::spawn!(async move {
                ch.try_resolve_approval_reply(
                    &json!({
                        "id": "post1",
                        "user_id": ALLOWED_USER,
                        "message": "abc123 yes",
                        "create_at": 1,
                        "channel_id": ORIGIN_CHANNEL,
                    }),
                    "",
                )
                .await
            })
        };

        // While that await is in flight, A goes away and B takes the token.
        tokio::time::sleep(Duration::from_millis(50)).await;
        ch.approvals
            .lock()
            .retire_generation("abc123", a_generation);
        let (mut b_rx, b_generation) = register(&ch, "abc123", None);
        assert_ne!(a_generation, b_generation);

        let outcome = resolver.await.expect("resolver task must not panic");

        assert_eq!(
            outcome,
            ApprovalReplyOutcome::Consumed,
            "the reply is approval traffic and must not reach the model"
        );
        assert!(
            b_rx.try_recv().is_err(),
            "B must not receive a reply written in answer to A's prompt"
        );
        assert!(
            ch.approvals.lock().owns("abc123", b_generation),
            "B must still be awaiting its own decision"
        );
    }

    /// Allocation must not hand out a token that is live or still inside the
    /// retired window — either collision would let one prompt's decision land
    /// on another.
    #[test]
    fn allocation_skips_live_and_retired_tokens() {
        let ch = channel();
        let (_rx, _generation) = register(&ch, "live01", None);
        ch.approvals.lock().remember_retired("gone02");

        for _ in 0..256 {
            let token = ch
                .approvals
                .lock()
                .allocate_token()
                .expect("a free token must be available");
            assert_ne!(token, "live01", "must not reuse a live token");
            assert_ne!(
                token, "gone02",
                "must not reuse a token in the retired window"
            );
        }
    }

    /// The binding is only added when the registration still owns the token, so
    /// a prompt posted for a request that has already gone away cannot leave a
    /// binding that a later request would answer through.
    #[test]
    fn post_binding_requires_the_registration_to_still_be_live() {
        let ch = channel();
        let (_rx, generation) = register(&ch, "abc123", None);
        ch.approvals.lock().retire_generation("abc123", generation);

        let state = ch.approvals.lock();
        assert!(
            !state.owns("abc123", generation),
            "the registration is gone, so `owns` must gate the binding out"
        );
    }
}
