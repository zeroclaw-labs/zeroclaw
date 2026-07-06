use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::OnceCell;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

const MAX_MATTERMOST_AUDIO_BYTES: u64 = 25 * 1024 * 1024;
/// Cadence at which auto-discovery re-runs to pick up newly-created DMs
/// and team channel changes.
const DISCOVERY_REFRESH: Duration = Duration::from_secs(60);
/// Poll interval per discovery iteration. Matches the previous single-channel
/// cadence so operators see no change in latency.
const POLL_INTERVAL: Duration = Duration::from_secs(3);

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
        }
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

    /// Resolve the set of channels this listener should poll, combining:
    ///
    /// - explicit `channel_ids` from config (looked up to learn each channel's
    ///   type so the DM/non-DM distinction reaches the receive path), or
    /// - auto-discovery via `/api/v4/users/me/channels` filtered by
    ///   `team_ids` and `discover_dms`.
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
                // Bind the sole registered provider as the agent transcription
                // provider for the channel-direct ingest path. Multi-provider
                // setups still resolve via the orchestrator's per-agent
                // routing (see orchestrator/mod.rs). See wati.rs for full
                // rationale.
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

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_channel_proxy_client_with_timeouts(
            "channel.mattermost",
            self.proxy_url.as_deref(),
            30,
            10,
        )
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
        if !id.is_empty() || !username.is_empty() {
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

        if let Some(content_length) = response.content_length()
            && content_length > MAX_MATTERMOST_AUDIO_BYTES
        {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(
                        ::serde_json::json!({"content_length": content_length, "file_id": file_id})
                    ),
                "audio file too large ( bytes)"
            );
            return None;
        }

        let bytes = match response.bytes().await {
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

        if let Some(root) = root_id {
            body_map.as_object_mut().unwrap().insert(
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

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
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
            "Mattermost channel listening"
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
}

impl MattermostChannel {
    /// Poll one target channel for new posts since its cursor, dispatch each
    /// post through `parse_mattermost_post`, and update the cursor in place.
    /// Returns `true` when the outbound mpsc was closed (caller exits the
    /// listen loop). Errors during the poll are logged and treated as a
    /// no-op for this iteration; the next iteration retries.
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

            let effective_text = if post
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .trim()
                .is_empty()
                && post_has_audio_attachment(post)
            {
                self.try_transcribe_audio_attachment(post).await
            } else {
                None
            };

            if let Some(channel_msg) = self.parse_mattermost_post(
                post,
                bot_user_id,
                bot_username,
                cursor_before_batch,
                &target.id,
                effective_text.as_deref(),
                target.is_direct,
            ) && tx.send(channel_msg).await.is_err()
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

/// Check whether a Mattermost post contains an @-mention of the bot.
///
/// Checks two sources:
/// 1. Text-based: looks for `@bot_username` in the message body (case-insensitive).
/// 2. Metadata-based: checks the post's `metadata.mentions` array for the bot user ID.
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

/// Gate incoming Mattermost content when `mention_only` is enabled.
///
/// Returns `None` if the message doesn't mention the bot, otherwise the
/// trimmed text with the mention preserved so downstream consumers can
/// see who was addressed.
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
}
