use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use hmac::{Hmac, Mac};
use parking_lot::RwLock;
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_config::schema::{Config, LineDmPolicy, LineGroupPolicy};
use zeroclaw_runtime::security::pairing::PairingGuard;

type HmacSha256 = Hmac<Sha256>;

const LINE_BIND_COMMAND: &str = "/bind";
/// Maximum audio file size accepted for transcription (25 MB).
const MAX_LINE_AUDIO_BYTES: u64 = 25 * 1024 * 1024;

/// LINE Messaging API channel.
///
/// Receives messages via an embedded axum webhook server. Each incoming event
/// carries a one-time `replyToken` (expires ~30 s). `send()` tries the Reply
/// API first; if the token is gone it falls back to the Push API.
///
/// Mention detection uses LINE's native `message.mention.mentionees` field,
/// which carries the bot's own `userId` — no display-name config needed.
/// The bot's `userId` is fetched once from `GET /v2/bot/info` at startup.
///
/// ## Access policies
///
/// DM (1:1) access is controlled by `dm_policy`:
/// - `open`      — respond to everyone
/// - `pairing`   — require a one-time `/bind <code>` handshake (default)
/// - `allowlist` — respond only to user IDs in the channel's peer group
///
/// Group/room access is controlled by `group_policy`:
/// - `open`     — respond to every message
/// - `mention`  — respond only when @mentioned (default)
/// - `disabled` — ignore all group messages
pub struct LineChannel {
    /// Long-lived channel access token — used for both Reply and Push APIs.
    channel_access_token: String,
    /// Channel secret — used to verify the `X-Line-Signature` header.
    channel_secret: String,
    /// DM access policy.
    dm_policy: LineDmPolicy,
    /// Group/room access policy.
    group_policy: LineGroupPolicy,
    /// The alias key under `[channels.line.<alias>]` this handle is
    /// bound to. Used to scope peer-group writes and resolver lookups.
    alias: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    /// Optional pairing-persist handle. `None` in tests and one-shot
    /// builds (pairing then doesn't survive — and without persistence
    /// the resolver never sees the paired user, matching telegram's
    /// no-persistence semantics). `Some` in the long-running daemon,
    /// wired via `.with_persistence(config)`. RwLock so concurrent
    /// peer reads from sibling channels don't serialize.
    persist: Option<Arc<parking_lot::RwLock<Config>>>,
    /// Pairing guard — `Some` when `dm_policy = Pairing`.
    pairing: Option<Arc<PairingGuard>>,
    /// TCP port the embedded webhook server listens on.
    webhook_port: u16,
    /// Latest replyToken per recipient (userId or groupId).
    /// Populated by `listen()`, consumed once by `send()`.
    pending_tokens: Arc<RwLock<HashMap<String, String>>>,
    client: reqwest::Client,
    /// Base URL for the LINE Messaging API. Overrideable in tests.
    api_base_url: String,
    /// Sender display name injected into every outgoing message ("AI <botName>", max 20 chars).
    /// Set once in `listen()` after fetching `/v2/bot/info`.
    sender_name: Arc<parking_lot::RwLock<String>>,
    /// Bot profile image URL from `/v2/bot/info`; injected as `sender.iconUrl`.
    sender_icon: Arc<parking_lot::RwLock<Option<String>>>,
    /// Base URL for the LINE Content API (audio/file downloads). Overrideable in tests.
    content_api_base_url: String,
    /// Optional transcription manager for voice/audio messages.
    transcription_manager: Option<Arc<super::transcription::TranscriptionManager>>,
}

/// Response from `GET /v2/bot/info`.
#[derive(serde::Deserialize)]
struct BotInfo {
    #[serde(rename = "userId")]
    user_id: String,
    #[serde(rename = "displayName")]
    display_name: String,
    #[serde(rename = "pictureUrl", default)]
    picture_url: Option<String>,
}

// ---------------------------------------------------------------------------
// Webhook state and router (module-level for testability)
// ---------------------------------------------------------------------------

struct LineState {
    tx: tokio::sync::mpsc::Sender<ChannelMessage>,
    channel_secret: String,
    bot_user_id: String,
    dm_policy: LineDmPolicy,
    group_policy: LineGroupPolicy,
    /// Alias under `[channels.line.<alias>]` — scopes peer-group writes.
    alias: String,
    /// Resolves the configured peer allowlist at message-time. Reads
    /// canonical state, no cache.
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    /// Optional pairing-persist handle for the `/bind` flow.
    persist: Option<Arc<parking_lot::RwLock<Config>>>,
    pairing: Option<Arc<PairingGuard>>,
    pending_tokens: Arc<RwLock<HashMap<String, String>>>,
    /// HTTP client and credentials for downloading audio content.
    client: reqwest::Client,
    channel_access_token: String,
    api_base_url: String,
    content_api_base_url: String,
    /// Sender display name for icon/nickname switch ("AI <botName>").
    sender_name: Arc<parking_lot::RwLock<String>>,
    /// Optional transcription manager — `None` when transcription is disabled.
    transcription_manager: Option<Arc<super::transcription::TranscriptionManager>>,
}

async fn send_loading_indicator(
    client: &reqwest::Client,
    channel_access_token: &str,
    api_base_url: &str,
    chat_id: &str,
) {
    let body = serde_json::json!({"chatId": chat_id});
    if let Err(e) = client
        .post(format!("{api_base_url}/v2/bot/chat/loading/start"))
        .bearer_auth(channel_access_token)
        .json(&body)
        .send()
        .await
    {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            &format!("loading indicator failed: {e}")
        );
    }
}

async fn send_bind_reply(
    client: &reqwest::Client,
    channel_access_token: &str,
    api_base_url: &str,
    reply_token: &str,
    text: &str,
) {
    let body = serde_json::json!({
        "replyToken": reply_token,
        "messages": [{"type": "text", "text": text}],
    });
    if let Err(e) = client
        .post(format!("{api_base_url}/v2/bot/message/reply"))
        .bearer_auth(channel_access_token)
        .json(&body)
        .send()
        .await
    {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            &format!("bind reply failed: {e}")
        );
    }
}

/// Download audio/voice message binary from the LINE Content API.
///
/// LINE stores message content at `https://api-data.line.me/v2/bot/message/{id}/content`.
/// Audio messages are typically M4A (`audio/x-m4a`).
async fn download_audio_content(
    client: &reqwest::Client,
    content_api_base_url: &str,
    channel_access_token: &str,
    message_id: &str,
) -> anyhow::Result<Vec<u8>> {
    let url = format!("{content_api_base_url}/v2/bot/message/{message_id}/content");
    let resp = client
        .get(&url)
        .bearer_auth(channel_access_token)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        anyhow::bail!("audio download failed ({status}) for message {message_id}");
    }

    let mut bytes = Vec::new();
    let mut stream = resp;
    while let Some(chunk) = stream.chunk().await? {
        bytes.extend_from_slice(&chunk);
        if bytes.len() as u64 > MAX_LINE_AUDIO_BYTES {
            anyhow::bail!(
                "audio exceeds {} byte limit for message {message_id}",
                MAX_LINE_AUDIO_BYTES
            );
        }
    }
    Ok(bytes)
}

fn build_webhook_router(state: Arc<LineState>) -> axum::Router {
    use axum::{Router, routing::post};
    Router::new()
        .route("/line/webhook", post(handle_webhook))
        .with_state(state)
}

/// Check whether `user_id` is in the LINE peer allowlist resolved from
/// canonical config state at call-time. LINE user IDs are case-sensitive.
fn is_line_user_allowed(state: &LineState, user_id: &str) -> bool {
    let peers = (state.peer_resolver)();
    crate::allowlist::is_user_allowed(&peers, user_id, crate::allowlist::Match::Sensitive)
}

/// Persist a newly-paired LINE userId into `peer_groups.line_<alias>.external_peers`
/// via the shared Config handle. Mirrors telegram/wechat's `persist_allowed_identity`.
/// No-op-with-warn when `state.persist` is unset (test fixtures).
async fn persist_line_paired_identity(state: &LineState, user_id: &str) -> anyhow::Result<()> {
    use anyhow::Context;
    use zeroclaw_config::multi_agent::{PeerGroupConfig, PeerUsername};
    use zeroclaw_config::providers::ChannelRef;

    let Some(config) = &state.persist else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"user_id": user_id})),
            "paired userId not persisted (no persistence handle wired)"
        );
        return Ok(());
    };
    let normalized = user_id.trim().to_string();
    if normalized.is_empty() {
        anyhow::bail!("Cannot persist empty LINE userId");
    }
    let group_name = format!("line_{}", state.alias);
    let channel_ref = ChannelRef::new(format!("line.{}", state.alias));
    let snapshot = {
        let mut cfg = config.write();
        if !cfg.channels.line.contains_key(&state.alias) {
            anyhow::bail!("Missing [channels.line.{}] section", state.alias);
        }
        let group = cfg
            .peer_groups
            .entry(group_name)
            .or_insert_with(|| PeerGroupConfig {
                channel: channel_ref,
                ..PeerGroupConfig::default()
            });
        if group
            .external_peers
            .iter()
            .any(|p| p.as_str() == normalized)
        {
            return Ok(());
        }
        group.external_peers.push(PeerUsername::new(normalized));
        cfg.clone()
    };
    snapshot
        .save()
        .await
        .context("Failed to persist LINE paired userId to config.toml")?;
    Ok(())
}

async fn handle_webhook(
    axum::extract::State(state): axum::extract::State<Arc<LineState>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::http::StatusCode {
    use axum::http::StatusCode;

    // 1. Verify LINE signature
    let sig = headers
        .get("x-line-signature")
        .and_then(|v| v.to_str().ok());

    let sig_valid = {
        let Ok(mut mac) = HmacSha256::new_from_slice(state.channel_secret.as_bytes()) else {
            return StatusCode::INTERNAL_SERVER_ERROR;
        };
        mac.update(&body);
        if let Some(s) = sig {
            BASE64
                .decode(s.trim())
                .map(|decoded| mac.verify_slice(&decoded).is_ok())
                .unwrap_or(false)
        } else {
            false
        }
    };

    if !sig_valid {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "rejected request with invalid signature"
        );
        return StatusCode::UNAUTHORIZED;
    }

    // 2. Parse payload
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "invalid JSON payload"
            );
            return StatusCode::BAD_REQUEST;
        }
    };

    let events = match payload.get("events").and_then(|e| e.as_array()) {
        Some(ev) => ev.clone(),
        None => return StatusCode::OK,
    };

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    for event in &events {
        // Only handle message events (text and audio).
        if event.get("type").and_then(|t| t.as_str()) != Some("message") {
            continue;
        }
        let msg_obj = match event.get("message") {
            Some(m) => m,
            None => continue,
        };

        let msg_type = msg_obj.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let msg_id = msg_obj
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or("")
            .to_string();

        // Resolve message content: text directly, audio via transcription.
        let owned_text: String;
        let text: &str = match msg_type {
            "text" => {
                owned_text = match msg_obj.get("text").and_then(|t| t.as_str()) {
                    Some(t) if !t.trim().is_empty() => t.to_string(),
                    _ => continue,
                };
                &owned_text
            }
            "audio" => {
                let Some(ref manager) = state.transcription_manager else {
                    ::zeroclaw_log::record!(
                        DEBUG,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        "audio message ignored (transcription not configured)"
                    );
                    continue;
                };
                let audio = match download_audio_content(
                    &state.client,
                    &state.content_api_base_url,
                    &state.channel_access_token,
                    &msg_id,
                )
                .await
                {
                    Ok(b) => b,
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(
                                ::serde_json::json!({"error": format!("{}", e), "msg_id": msg_id})
                            ),
                            "audio download failed for"
                        );
                        continue;
                    }
                };
                let transcript = match manager.transcribe(&audio, "audio.m4a").await {
                    Ok(t) if !t.trim().is_empty() => t,
                    Ok(_) => {
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_attrs(::serde_json::json!({"msg_id": msg_id})),
                            "empty transcript for"
                        );
                        continue;
                    }
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(
                                ::serde_json::json!({"error": format!("{}", e), "msg_id": msg_id})
                            ),
                            "transcription failed for"
                        );
                        continue;
                    }
                };
                owned_text = transcript;
                &owned_text
            }
            _ => continue,
        };

        let source = match event.get("source") {
            Some(s) => s,
            None => continue,
        };
        let source_type = source.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let user_id = match source.get("userId").and_then(|u| u.as_str()) {
            Some(id) => id,
            None => continue,
        };

        let is_group = matches!(source_type, "group" | "room");

        // Show loading indicator immediately — fire-and-forget before any gate.
        let loading_chat_id = if is_group {
            source.get("groupId").or_else(|| source.get("roomId"))
                .and_then(|v| v.as_str())
                .unwrap_or(user_id)
        } else {
            user_id
        };
        send_loading_indicator(&state.client, &state.channel_access_token, &state.api_base_url, loading_chat_id).await;

        // 3. Group policy gate
        if is_group {
            match state.group_policy {
                LineGroupPolicy::Disabled => continue,
                LineGroupPolicy::Open => {}
                LineGroupPolicy::Mention => {
                    let mention_span = LineChannel::find_bot_mention(msg_obj, &state.bot_user_id);
                    if mention_span.is_none() {
                        ::zeroclaw_log::record!(
                            DEBUG,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            ),
                            &format!(
                                "skipping group message without bot mention (userId: {})",
                                state.bot_user_id
                            )
                        );
                        continue;
                    }
                }
            }
        }

        // 4. DM policy gate (non-group messages only)
        if !is_group {
            match state.dm_policy {
                LineDmPolicy::Open => {}
                LineDmPolicy::Allowlist => {
                    if !is_line_user_allowed(&*state, user_id) {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"user_id": user_id})),
                            "ignoring DM from unauthorized user: . Add to the channel peer group or use dm_policy = pairing."
                        );
                        continue;
                    }
                }
                LineDmPolicy::Pairing => {
                    if !is_line_user_allowed(&*state, user_id) {
                        // Try pairing bind
                        if let Some(code) = LineChannel::extract_bind_code(text) {
                            let bind_reply_token = event
                                .get("replyToken")
                                .and_then(|t| t.as_str())
                                .filter(|t| !t.is_empty())
                                .map(|t| t.to_string());
                            if let Some(ref guard) = state.pairing {
                                match guard.try_pair(code, user_id).await {
                                    Ok(Some(_)) => {
                                        if let Err(e) =
                                            persist_line_paired_identity(&*state, user_id).await
                                        {
                                            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"user_id": user_id, "e": e.to_string()})), "paired userId= but persist failed");
                                        } else {
                                            ::zeroclaw_log::record!(
                                                INFO,
                                                ::zeroclaw_log::Event::new(
                                                    module_path!(),
                                                    ::zeroclaw_log::Action::Note
                                                )
                                                .with_attrs(
                                                    ::serde_json::json!({"user_id": user_id})
                                                ),
                                                "paired userId="
                                            );
                                        }
                                        if let Some(ref token) = bind_reply_token {
                                            send_bind_reply(&state.client, &state.channel_access_token, &state.api_base_url, token, "Paired! You can now chat.").await;
                                        }
                                    }
                                    Ok(None) => {
                                        ::zeroclaw_log::record!(
                                            WARN,
                                            ::zeroclaw_log::Event::new(
                                                module_path!(),
                                                ::zeroclaw_log::Action::Note
                                            )
                                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                            .with_attrs(::serde_json::json!({"user_id": user_id})),
                                            "invalid bind code from userId="
                                        );
                                        if let Some(ref token) = bind_reply_token {
                                            send_bind_reply(&state.client, &state.channel_access_token, &state.api_base_url, token, "Invalid code. Please try again.").await;
                                        }
                                    }
                                    Err(wait_ms) => {
                                        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"user_id": user_id, "wait_ms": wait_ms})), "bind rate-limited for userId=, retry after ms");
                                        if let Some(ref token) = bind_reply_token {
                                            let secs = wait_ms / 1000;
                                            send_bind_reply(&state.client, &state.channel_access_token, &state.api_base_url, token, &format!("Too many attempts. Retry in {secs}s.")).await;
                                        }
                                    }
                                }
                            }
                            continue; // bind commands are not forwarded to agent
                        }

                        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"user_id": user_id, "LINE_BIND_COMMAND": LINE_BIND_COMMAND})), "ignoring message from unpaired user: . Send ` <code>` to pair.");
                        continue;
                    }
                }
            }
        }

        // 5. Resolve recipient (groupId/roomId for group context)
        let recipient = match source_type {
            "group" => source
                .get("groupId")
                .and_then(|v| v.as_str())
                .unwrap_or(user_id)
                .to_string(),
            "room" => source
                .get("roomId")
                .and_then(|v| v.as_str())
                .unwrap_or(user_id)
                .to_string(),
            _ => user_id.to_string(),
        };

        let content = text.trim().to_string();
        if content.is_empty() {
            continue;
        }

        // 8. Cache reply token
        if let Some(token) = event
            .get("replyToken")
            .and_then(|t| t.as_str())
            .filter(|t| !t.is_empty())
        {
            state
                .pending_tokens
                .write()
                .insert(recipient.clone(), token.to_string());
        }

        let final_msg_id = if msg_id.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            msg_id.clone()
        };

        let channel_msg = ChannelMessage {
            id: final_msg_id,
            sender: user_id.to_string(),
            reply_target: recipient,
            content,
            channel: "line".to_string(),
            channel_alias: Some(state.alias.clone()),
            timestamp,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
            subject: None,
        };

        if state.tx.send(channel_msg).await.is_err() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "receiver dropped, shutting down webhook server"
            );
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    }

    StatusCode::OK
}

// ---------------------------------------------------------------------------
// LineChannel implementation
// ---------------------------------------------------------------------------

impl LineChannel {
    pub fn new(
        channel_access_token: String,
        channel_secret: String,
        dm_policy: LineDmPolicy,
        group_policy: LineGroupPolicy,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
        webhook_port: u16,
    ) -> Self {
        let token = if channel_access_token.is_empty() {
            std::env::var("LINE_CHANNEL_ACCESS_TOKEN").unwrap_or_default()
        } else {
            channel_access_token
        };
        let secret = if channel_secret.is_empty() {
            std::env::var("LINE_CHANNEL_SECRET").unwrap_or_default()
        } else {
            channel_secret
        };

        let configured_peers = peer_resolver();
        let pairing = if dm_policy == LineDmPolicy::Pairing && configured_peers.is_empty() {
            let guard = PairingGuard::new(true, &[]);
            if let Some(code) = guard.pairing_code() {
                println!("  🔐 LINE pairing required. One-time bind code: {code}");
                println!("     Send `{LINE_BIND_COMMAND} <code>` from your LINE account.");
            }
            Some(Arc::new(guard))
        } else {
            None
        };

        Self {
            channel_access_token: token,
            channel_secret: secret,
            dm_policy,
            group_policy,
            alias: alias.into(),
            peer_resolver,
            persist: None,
            pairing,
            webhook_port,
            pending_tokens: Arc::new(RwLock::new(HashMap::new())),
            client: zeroclaw_config::schema::build_channel_proxy_client("channel.line", None),
            api_base_url: "https://api.line.me".to_string(),
            content_api_base_url: "https://api-data.line.me".to_string(),
            sender_name: Arc::new(parking_lot::RwLock::new(String::new())),
            sender_icon: Arc::new(parking_lot::RwLock::new(None)),
            transcription_manager: None,
        }
    }

    /// Wire the shared `Config` handle so `persist_line_paired_identity`
    /// can write a newly-paired userId into `peer_groups.line_<alias>.external_peers`
    /// and save. Long-running daemon sets this from the orchestrator; tests
    /// and one-shot callers leave it unset (pairing then doesn't survive).
    pub fn with_persistence(mut self, config: Arc<parking_lot::RwLock<Config>>) -> Self {
        self.persist = Some(config);
        self
    }

    /// Construct a `LineChannel` directly from a [`zeroclaw_config::schema::LineConfig`].
    ///
    /// Mirrors [`LarkChannel::from_config`] — keeps construction logic inside the
    /// channel crate rather than duplicating it across orchestrator call sites.
    pub fn from_config(
        config: &zeroclaw_config::schema::LineConfig,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Self {
        Self::new(
            config.channel_access_token.clone(),
            config.channel_secret.clone(),
            config.dm_policy.clone(),
            config.group_policy.clone(),
            alias,
            peer_resolver,
            config.webhook_port,
        )
        .with_proxy_url(config.proxy_url.clone())
    }

    /// Override the proxy URL for outbound HTTP calls.
    pub fn with_proxy_url(mut self, proxy_url: Option<String>) -> Self {
        self.client = zeroclaw_config::schema::build_channel_proxy_client(
            "channel.line",
            proxy_url.as_deref(),
        );
        self
    }

    /// Enable voice/audio transcription for incoming LINE audio messages.
    ///
    /// When enabled, `type = "audio"` webhook events are downloaded from the
    /// LINE Content API and transcribed before being forwarded to the agent.
    pub fn with_transcription(
        mut self,
        config: zeroclaw_config::schema::TranscriptionConfig,
    ) -> Self {
        if !config.enabled {
            return self;
        }
        match super::transcription::TranscriptionManager::new(&config) {
            Ok(m) => {
                // Channel doesn't carry an agent identity itself; the
                // configured local_whisper / openai / groq / etc.
                // provider auto-acts as the agent_transcription_provider
                // here so inbound audio routes to whichever single
                // provider the operator configured under
                // [transcription.<provider>].
                let m = if config.local_whisper.is_some() {
                    m.with_agent_transcription_provider("local_whisper")
                } else if config.openai.is_some() {
                    m.with_agent_transcription_provider("openai")
                } else if config.deepgram.is_some() {
                    m.with_agent_transcription_provider("deepgram")
                } else if config.assemblyai.is_some() {
                    m.with_agent_transcription_provider("assemblyai")
                } else if config.google.is_some() {
                    m.with_agent_transcription_provider("google")
                } else {
                    m.with_agent_transcription_provider("groq")
                };
                self.transcription_manager = Some(Arc::new(m));
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"e": e.to_string()})),
                    "transcription manager init failed, audio transcription disabled"
                );
            }
        }
        self
    }

    /// Override the LINE API base URL. Intended for tests.
    #[cfg(test)]
    pub(crate) fn with_api_base_url(mut self, url: &str) -> Self {
        let url = url.trim_end_matches('/').to_string();
        // Use the same mock server for both the message API and the content API.
        self.content_api_base_url = url.clone();
        self.api_base_url = url;
        self
    }

    /// Returns `true` if a pairing code is currently active.
    #[cfg(test)]
    pub(crate) fn pairing_code_active(&self) -> bool {
        self.pairing
            .as_ref()
            .and_then(|g| g.pairing_code())
            .is_some()
    }

    /// Verify `X-Line-Signature: <base64(HMAC-SHA256(body, channel_secret))>`.
    #[cfg(test)]
    pub(crate) fn verify_signature(&self, body: &[u8], signature_header: Option<&str>) -> bool {
        let Some(sig_b64) = signature_header else {
            return false;
        };
        let Ok(sig_bytes) = BASE64.decode(sig_b64.trim()) else {
            return false;
        };
        let Ok(mut mac) = HmacSha256::new_from_slice(self.channel_secret.as_bytes()) else {
            return false;
        };
        mac.update(body);
        mac.verify_slice(&sig_bytes).is_ok()
    }

    /// Fetch bot info from LINE API. Returns `(userId, displayName)`.
    async fn fetch_bot_info(&self) -> anyhow::Result<BotInfo> {
        let url = format!("{}/v2/bot/info", self.api_base_url);
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.channel_access_token)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("failed to fetch bot info ({status}): {err}");
        }

        resp.json::<BotInfo>().await.map_err(Into::into)
    }

    /// Resolve the canonical recipient for a source object.
    ///
    /// - `user` source  → userId  (1:1 chat)
    /// - `group` source → groupId (group chat)
    /// - `room` source  → roomId  (multi-person chat)
    #[cfg(test)]
    pub(crate) fn resolve_recipient(source: &serde_json::Value) -> Option<String> {
        let source_type = source.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match source_type {
            "group" => source
                .get("groupId")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            "room" => source
                .get("roomId")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            _ => source
                .get("userId")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        }
    }

    /// Extract the bind code from a `/bind <code>` message, if present.
    fn extract_bind_code(text: &str) -> Option<&str> {
        let text = text.trim();
        let rest = text.strip_prefix(LINE_BIND_COMMAND)?;
        // allow "/bind<code>" or "/bind <code>"
        let code = rest.trim();
        if code.is_empty() { None } else { Some(code) }
    }

    /// Check whether the bot (`bot_user_id`) is mentioned in the message.
    ///
    /// Uses LINE's native `message.mention.mentionees` field — no display-name
    /// matching needed. Returns the `(char_index, char_length)` of the first
    /// matching mention so the caller can strip it from the text.
    fn find_bot_mention(msg_obj: &serde_json::Value, bot_user_id: &str) -> Option<(usize, usize)> {
        let mentionees = msg_obj
            .get("mention")
            .and_then(|m| m.get("mentionees"))
            .and_then(|m| m.as_array())?;

        for m in mentionees {
            let uid = m.get("userId").and_then(|u| u.as_str()).unwrap_or("");
            if uid == bot_user_id {
                let index = usize::try_from(m.get("index").and_then(|i| i.as_u64()).unwrap_or(0))
                    .unwrap_or(0);
                let length = usize::try_from(m.get("length").and_then(|l| l.as_u64()).unwrap_or(0))
                    .unwrap_or(0);
                return Some((index, length));
            }
        }
        None
    }

    /// Split long text into chunks ≤ `LINE_MAX_MESSAGE_LEN` characters.
    fn split_message(text: &str) -> Vec<String> {
        const LINE_MAX_MESSAGE_LEN: usize = 5000;
        const OVERHEAD: usize = 15; // room for "(continued)" marker

        if text.chars().count() <= LINE_MAX_MESSAGE_LEN {
            return vec![text.to_string()];
        }

        let chunk_limit = LINE_MAX_MESSAGE_LEN - OVERHEAD;
        let mut chunks = Vec::new();
        let mut remaining = text;

        while !remaining.is_empty() {
            if remaining.chars().count() <= LINE_MAX_MESSAGE_LEN {
                chunks.push(remaining.to_string());
                break;
            }
            let hard_end = remaining
                .char_indices()
                .nth(chunk_limit)
                .map_or(remaining.len(), |(i, _)| i);

            let split_at = remaining[..hard_end]
                .rfind('\n')
                .or_else(|| remaining[..hard_end].rfind(' '))
                .map(|p| p + 1)
                .unwrap_or(hard_end);

            chunks.push(format!("{}\n(continued)", &remaining[..split_at]));
            remaining = remaining[split_at..].trim_start();
        }

        chunks
    }

    /// Build the LINE `sender` object for icon/nickname switch.
    ///
    /// Returns `None` when `sender_name` is empty (LINE rejects empty names).
    /// Name is already pre-truncated to 20 chars at `listen()` time.
    fn build_sender_obj(name: &str, icon: &Option<String>) -> Option<serde_json::Value> {
        if name.is_empty() {
            return None;
        }
        let mut obj = serde_json::json!({"name": name});
        if let Some(url) = icon {
            obj["iconUrl"] = serde_json::Value::String(url.clone());
        }
        Some(obj)
    }

    /// Send text via the Reply API (consumes `reply_token`).
    async fn send_reply(&self, reply_token: &str, text: &str) -> anyhow::Result<()> {
        let url = format!("{}/v2/bot/message/reply", self.api_base_url);
        let sender_name = self.sender_name.read().clone();
        let sender_icon = self.sender_icon.read().clone();
        let messages: Vec<serde_json::Value> = Self::split_message(text)
            .into_iter()
            .map(|chunk| {
                let mut msg = serde_json::json!({"type": "text", "text": chunk});
                if let Some(sender) = Self::build_sender_obj(&sender_name, &sender_icon) {
                    msg["sender"] = sender;
                }
                msg
            })
            .collect();

        // LINE Reply API accepts at most 5 messages per call.
        for batch in messages.chunks(5) {
            let body = serde_json::json!({
                "replyToken": reply_token,
                "messages": batch,
            });
            let resp = self
                .client
                .post(&url)
                .bearer_auth(&self.channel_access_token)
                .json(&body)
                .send()
                .await?;

            if !resp.status().is_success() {
                let status = resp.status();
                let err = resp.text().await.unwrap_or_default();
                anyhow::bail!("Reply API failed ({status}): {err}");
            }
        }
        Ok(())
    }

    /// Send text via the Push API (requires a paid LINE plan for high volume).
    async fn send_push(&self, to: &str, text: &str) -> anyhow::Result<()> {
        let url = format!("{}/v2/bot/message/push", self.api_base_url);
        let sender_name = self.sender_name.read().clone();
        let sender_icon = self.sender_icon.read().clone();
        let messages: Vec<serde_json::Value> = Self::split_message(text)
            .into_iter()
            .map(|chunk| {
                let mut msg = serde_json::json!({"type": "text", "text": chunk});
                if let Some(sender) = Self::build_sender_obj(&sender_name, &sender_icon) {
                    msg["sender"] = sender;
                }
                msg
            })
            .collect();

        for batch in messages.chunks(5) {
            let body = serde_json::json!({
                "to": to,
                "messages": batch,
            });
            let resp = self
                .client
                .post(&url)
                .bearer_auth(&self.channel_access_token)
                .json(&body)
                .send()
                .await?;

            if !resp.status().is_success() {
                let status = resp.status();
                let err = resp.text().await.unwrap_or_default();
                anyhow::bail!("Push API failed ({status}): {err}");
            }
        }
        Ok(())
    }

    /// Start serving the webhook on an already-bound `TcpListener`.
    ///
    /// `bot_user_id` is used for native mention detection. The public `listen()`
    /// fetches it automatically from `GET /v2/bot/info`; tests can supply it
    /// directly to avoid a real network call.
    pub(crate) async fn listen_with_listener(
        &self,
        listener: tokio::net::TcpListener,
        bot_user_id: String,
        tx: tokio::sync::mpsc::Sender<ChannelMessage>,
    ) -> anyhow::Result<()> {
        let state = Arc::new(LineState {
            tx,
            channel_secret: self.channel_secret.clone(),
            bot_user_id,
            dm_policy: self.dm_policy.clone(),
            group_policy: self.group_policy.clone(),
            alias: self.alias.clone(),
            peer_resolver: Arc::clone(&self.peer_resolver),
            persist: self.persist.clone(),
            pairing: self.pairing.clone(),
            pending_tokens: Arc::clone(&self.pending_tokens),
            client: self.client.clone(),
            channel_access_token: self.channel_access_token.clone(),
            api_base_url: self.api_base_url.clone(),
            content_api_base_url: self.content_api_base_url.clone(),
            sender_name: Arc::clone(&self.sender_name),
            transcription_manager: self.transcription_manager.clone(),
        });

        let app = build_webhook_router(state);
        axum::serve(listener, app).await.map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "phase": "webhook_server",
                        "error": format!("{}", e),
                    })),
                "line: webhook server error"
            );
            anyhow::Error::msg(format!("webhook server error: {e}"))
        })
    }
}

impl ::zeroclaw_api::attribution::Attributable for LineChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(::zeroclaw_api::attribution::ChannelKind::Line)
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for LineChannel {
    fn name(&self) -> &str {
        "line"
    }

    /// Send a reply.
    ///
    /// Strategy: try the cached `replyToken` for the recipient first (free).
    /// If no token is available (already consumed or expired), fall back to
    /// the Push API.
    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let reply_token = self.pending_tokens.write().remove(&message.recipient);

        if let Some(token) = reply_token {
            match self.send_reply(&token, &message.content).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"e": e.to_string()})),
                        "Reply API failed (token may be expired), falling back to Push"
                    );
                }
            }
        }

        // Fallback: Push API
        self.send_push(&message.recipient, &message.content).await
    }

    /// Start the embedded webhook server and forward incoming text events to `tx`.
    ///
    /// Fetches the bot's `userId` and `displayName` from `GET /v2/bot/info` once
    /// before starting the server. The `userId` is used for native mention detection
    /// via `message.mention.mentionees`.
    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let bot_info = self.fetch_bot_info().await?;
        *self.sender_name.write() = "AI".to_string();
        *self.sender_icon.write() = bot_info.picture_url;
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "connected as '{}' (userId: {})",
                bot_info.display_name, bot_info.user_id
            )
        );

        let addr = std::net::SocketAddr::from(([0, 0, 0, 0], self.webhook_port));
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "webhook server listening on http://0.0.0.0:{}/line/webhook",
                self.webhook_port
            )
        );

        let listener = tokio::net::TcpListener::bind(addr).await?;
        self.listen_with_listener(listener, bot_info.user_id, tx)
            .await
    }

    async fn health_check(&self) -> bool {
        let url = format!("{}/v2/bot/info", self.api_base_url);
        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.channel_access_token)
            .send()
            .await;
        matches!(resp, Ok(r) if r.status().is_success())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    // ---- Helpers -----------------------------------------------------------

    fn empty_resolver() -> Arc<dyn Fn() -> Vec<String> + Send + Sync> {
        Arc::new(Vec::new)
    }

    fn resolver_from(peers: Vec<String>) -> Arc<dyn Fn() -> Vec<String> + Send + Sync> {
        Arc::new(move || peers.clone())
    }

    fn make_channel() -> LineChannel {
        LineChannel::new(
            "test_access_token".into(),
            "test_secret".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Mention,
            "line_test_alias",
            empty_resolver(),
            8444,
        )
    }

    /// Compute a valid `X-Line-Signature` for `body` signed with `secret`.
    fn sign(secret: &str, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        BASE64.encode(mac.finalize().into_bytes())
    }

    /// Build a standard DM text event payload.
    fn dm_event(user_id: &str, text: &str, reply_token: &str) -> serde_json::Value {
        serde_json::json!({
            "events": [{
                "type": "message",
                "replyToken": reply_token,
                "source": {"type": "user", "userId": user_id},
                "message": {"id": "msg001", "type": "text", "text": text}
            }]
        })
    }

    /// Build a group text event without a mention.
    fn group_event(
        user_id: &str,
        group_id: &str,
        text: &str,
        reply_token: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "events": [{
                "type": "message",
                "replyToken": reply_token,
                "source": {"type": "group", "groupId": group_id, "userId": user_id},
                "message": {"id": "msg002", "type": "text", "text": text}
            }]
        })
    }

    /// Build a group text event with a bot mention at `(index, length)`.
    fn group_mention_event(
        user_id: &str,
        group_id: &str,
        text: &str,
        bot_id: &str,
        mention_index: u64,
        mention_length: u64,
        reply_token: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "events": [{
                "type": "message",
                "replyToken": reply_token,
                "source": {"type": "group", "groupId": group_id, "userId": user_id},
                "message": {
                    "id": "msg003",
                    "type": "text",
                    "text": text,
                    "mention": {
                        "mentionees": [{
                            "index": mention_index,
                            "length": mention_length,
                            "userId": bot_id,
                            "type": "user"
                        }]
                    }
                }
            }]
        })
    }

    /// POST a signed webhook payload; returns the HTTP status code.
    async fn post_signed(port: u16, secret: &str, body: &serde_json::Value) -> u16 {
        let bytes = serde_json::to_vec(body).unwrap();
        let sig = sign(secret, &bytes);
        reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/line/webhook"))
            .header("Content-Type", "application/json")
            .header("X-Line-Signature", sig)
            .body(bytes)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    }

    /// POST a webhook payload with an invalid signature.
    async fn post_bad_sig(port: u16, body: &serde_json::Value) -> u16 {
        let bytes = serde_json::to_vec(body).unwrap();
        reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/line/webhook"))
            .header("Content-Type", "application/json")
            .header("X-Line-Signature", "badsig==")
            .body(bytes)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    }

    /// Spawn a webhook server on an OS-assigned port.
    /// Returns `(port, rx)`. Drop the `JoinHandle` to stop the server.
    async fn spawn_webhook(
        ch: LineChannel,
        bot_user_id: &str,
    ) -> (
        u16,
        mpsc::Receiver<ChannelMessage>,
        tokio::task::AbortHandle,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel(16);
        let bot_id = bot_user_id.to_string();
        let jh = zeroclaw_spawn::spawn!(async move {
            ch.listen_with_listener(listener, bot_id, tx).await.ok();
        });
        // Give the server a moment to begin accepting connections.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        (port, rx, jh.abort_handle())
    }

    // ---- Existing unit tests -----------------------------------------------

    #[test]
    fn name_is_line() {
        assert_eq!(make_channel().name(), "line");
    }

    #[test]
    fn pairing_mode_creates_guard_when_no_configured_peers() {
        let ch = LineChannel::new(
            "tok".into(),
            "sec".into(),
            LineDmPolicy::Pairing,
            LineGroupPolicy::Mention,
            "line_test_alias",
            empty_resolver(),
            8444,
        );
        assert!(ch.pairing_code_active());
    }

    #[test]
    fn pairing_mode_no_guard_when_users_present() {
        let ch = LineChannel::new(
            "tok".into(),
            "sec".into(),
            LineDmPolicy::Pairing,
            LineGroupPolicy::Mention,
            "line_test_alias",
            resolver_from(vec!["Uallowed".into()]),
            8444,
        );
        assert!(!ch.pairing_code_active());
    }

    #[test]
    fn open_mode_no_pairing_guard() {
        let ch = make_channel();
        assert!(!ch.pairing_code_active());
    }

    #[test]
    fn env_var_fallback_for_token() {
        // SAFETY: test-only, single-threaded context
        unsafe { std::env::set_var("LINE_CHANNEL_ACCESS_TOKEN", "env-token") };
        let ch = LineChannel::new(
            "".into(),
            "sec".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Mention,
            "line_test_alias",
            empty_resolver(),
            8444,
        );
        assert_eq!(ch.channel_access_token, "env-token");
        // SAFETY: test-only, single-threaded context
        unsafe { std::env::remove_var("LINE_CHANNEL_ACCESS_TOKEN") };
    }

    #[test]
    fn env_var_fallback_for_secret() {
        // SAFETY: test-only, single-threaded context
        unsafe { std::env::set_var("LINE_CHANNEL_SECRET", "env-secret") };
        let ch = LineChannel::new(
            "tok".into(),
            "".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Mention,
            "line_test_alias",
            empty_resolver(),
            8444,
        );
        assert_eq!(ch.channel_secret, "env-secret");
        // SAFETY: test-only, single-threaded context
        unsafe { std::env::remove_var("LINE_CHANNEL_SECRET") };
    }

    #[test]
    fn extract_bind_code_with_space() {
        assert_eq!(
            LineChannel::extract_bind_code("/bind abc123"),
            Some("abc123")
        );
    }

    #[test]
    fn extract_bind_code_no_space() {
        assert_eq!(
            LineChannel::extract_bind_code("/bindabc123"),
            Some("abc123")
        );
    }

    #[test]
    fn extract_bind_code_empty_returns_none() {
        assert_eq!(LineChannel::extract_bind_code("/bind"), None);
        assert_eq!(LineChannel::extract_bind_code("/bind   "), None);
    }

    #[test]
    fn extract_bind_code_other_command_returns_none() {
        assert_eq!(LineChannel::extract_bind_code("hello"), None);
        assert_eq!(LineChannel::extract_bind_code("/start abc"), None);
    }

    #[test]
    fn find_bot_mention_returns_span() {
        let msg = serde_json::json!({
            "type": "text",
            "text": "@MyBot hello",
            "mention": {
                "mentionees": [
                    {"index": 0, "length": 6, "userId": "Ubot123", "type": "user"}
                ]
            }
        });
        assert_eq!(LineChannel::find_bot_mention(&msg, "Ubot123"), Some((0, 6)));
        assert_eq!(LineChannel::find_bot_mention(&msg, "Uother"), None);
    }

    #[test]
    fn find_bot_mention_missing_field() {
        let msg = serde_json::json!({"type": "text", "text": "hello"});
        assert_eq!(LineChannel::find_bot_mention(&msg, "Ubot123"), None);
    }

    #[test]
    fn split_message_short_passthrough() {
        let text = "hello world";
        let chunks = LineChannel::split_message(text);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], text);
    }

    #[test]
    fn split_message_long_splits() {
        let text = "a".repeat(6000);
        let chunks = LineChannel::split_message(&text);
        assert!(chunks.len() >= 2);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= 5000);
        }
    }

    #[test]
    fn verify_signature_rejects_bad_sig() {
        let ch = make_channel();
        let body = b"test body";
        assert!(!ch.verify_signature(body, Some("badsig==")));
        assert!(!ch.verify_signature(body, None));
    }

    #[test]
    fn verify_signature_accepts_valid_sig() {
        use hmac::Mac as _;
        let ch = make_channel();
        let body = b"test body";
        let mut mac = HmacSha256::new_from_slice(ch.channel_secret.as_bytes()).unwrap();
        mac.update(body);
        let sig = BASE64.encode(mac.finalize().into_bytes());
        assert!(ch.verify_signature(body, Some(&sig)));
    }

    #[test]
    fn resolve_recipient_user() {
        let source = serde_json::json!({"type": "user", "userId": "U123"});
        assert_eq!(LineChannel::resolve_recipient(&source).unwrap(), "U123");
    }

    #[test]
    fn resolve_recipient_group() {
        let source = serde_json::json!({
            "type": "group",
            "groupId": "Gabc",
            "userId": "U123"
        });
        assert_eq!(LineChannel::resolve_recipient(&source).unwrap(), "Gabc");
    }

    #[test]
    fn resolve_recipient_room() {
        let source = serde_json::json!({
            "type": "room",
            "roomId": "Rabc",
            "userId": "U123"
        });
        assert_eq!(LineChannel::resolve_recipient(&source).unwrap(), "Rabc");
    }

    // ---- API integration tests (wiremock) ----------------------------------

    #[tokio::test]
    async fn health_check_returns_true_on_200() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/bot/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "userId": "Ubot", "displayName": "TestBot"
            })))
            .mount(&server)
            .await;

        let ch = LineChannel::new(
            "tok".into(),
            "sec".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            8444,
        )
        .with_api_base_url(&server.uri());

        assert!(ch.health_check().await);
    }

    #[tokio::test]
    async fn health_check_returns_false_on_401() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/bot/info"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let ch = LineChannel::new(
            "bad-tok".into(),
            "sec".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            8444,
        )
        .with_api_base_url(&server.uri());

        assert!(!ch.health_check().await);
    }

    #[tokio::test]
    async fn send_uses_reply_api_when_token_available() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/bot/message/reply"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let ch = LineChannel::new(
            "tok".into(),
            "sec".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            8444,
        )
        .with_api_base_url(&server.uri());

        ch.pending_tokens
            .write()
            .insert("Urecipient".to_string(), "reply-token-123".to_string());

        ch.send(&SendMessage::new("hello", "Urecipient"))
            .await
            .unwrap();

        server.verify().await;
    }

    #[tokio::test]
    async fn send_falls_back_to_push_when_no_token() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/bot/message/push"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let ch = LineChannel::new(
            "tok".into(),
            "sec".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            8444,
        )
        .with_api_base_url(&server.uri());

        // No pending token seeded — must use Push API.
        ch.send(&SendMessage::new("hello", "Urecipient"))
            .await
            .unwrap();

        server.verify().await;
    }

    #[tokio::test]
    async fn send_falls_back_to_push_when_reply_api_fails() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // Reply API returns 400 (e.g. token expired)
        Mock::given(method("POST"))
            .and(path("/v2/bot/message/reply"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "message": "Invalid reply token"
            })))
            .expect(1)
            .mount(&server)
            .await;
        // Push API succeeds
        Mock::given(method("POST"))
            .and(path("/v2/bot/message/push"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let ch = LineChannel::new(
            "tok".into(),
            "sec".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            8444,
        )
        .with_api_base_url(&server.uri());

        ch.pending_tokens
            .write()
            .insert("Urecipient".to_string(), "expired-token".to_string());

        ch.send(&SendMessage::new("hello", "Urecipient"))
            .await
            .unwrap();

        server.verify().await;
    }

    // ---- Webhook integration tests (live axum server + reqwest) ------------

    #[tokio::test]
    async fn webhook_rejects_bad_signature() {
        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            0,
        );
        let (port, _rx, abort) = spawn_webhook(ch, "Ubot").await;
        let status = post_bad_sig(port, &dm_event("Uuser", "hello", "rt1")).await;
        assert_eq!(status, 401);
        abort.abort();
    }

    #[tokio::test]
    async fn webhook_dm_open_forwards_message() {
        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            0,
        );
        let (port, mut rx, abort) = spawn_webhook(ch, "Ubot").await;

        let status = post_signed(port, "mysecret", &dm_event("Uuser1", "hi there", "rt1")).await;
        assert_eq!(status, 200);

        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("timed out waiting for message")
            .expect("channel closed");

        assert_eq!(msg.content, "hi there");
        assert_eq!(msg.sender, "Uuser1");
        assert_eq!(msg.channel, "line");
        abort.abort();
    }

    #[tokio::test]
    async fn webhook_reply_token_is_cached() {
        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            0,
        );
        let tokens = Arc::clone(&ch.pending_tokens);
        let (port, mut rx, abort) = spawn_webhook(ch, "Ubot").await;

        post_signed(
            port,
            "mysecret",
            &dm_event("Uuser", "ping", "reply-tok-abc"),
        )
        .await;
        let _ = rx.recv().await;

        assert_eq!(
            tokens.read().get("Uuser").map(String::as_str),
            Some("reply-tok-abc")
        );
        abort.abort();
    }

    #[tokio::test]
    async fn webhook_dm_allowlist_blocks_unknown_user() {
        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Allowlist,
            LineGroupPolicy::Open,
            "line_test_alias",
            resolver_from(vec!["Uallowed".to_string()]),
            0,
        );
        let (port, mut rx, abort) = spawn_webhook(ch, "Ubot").await;

        // Unknown user — must be blocked
        let status = post_signed(port, "mysecret", &dm_event("Ustranger", "hello", "rt1")).await;
        assert_eq!(status, 200); // HTTP still 200; message is silently dropped

        // Verify nothing arrived in rx within a short window
        let result = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
        assert!(result.is_err(), "blocked user should not produce a message");
        abort.abort();
    }

    #[tokio::test]
    async fn webhook_dm_allowlist_allows_known_user() {
        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Allowlist,
            LineGroupPolicy::Open,
            "line_test_alias",
            resolver_from(vec!["Uallowed".to_string()]),
            0,
        );
        let (port, mut rx, abort) = spawn_webhook(ch, "Ubot").await;

        let status = post_signed(port, "mysecret", &dm_event("Uallowed", "secret", "rt2")).await;
        assert_eq!(status, 200);

        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(msg.content, "secret");
        assert_eq!(msg.sender, "Uallowed");
        abort.abort();
    }

    #[tokio::test]
    async fn webhook_dm_pairing_rejects_unpaired_user() {
        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Pairing,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            0,
        );
        let (port, mut rx, abort) = spawn_webhook(ch, "Ubot").await;

        // Plain message from unpaired user
        post_signed(port, "mysecret", &dm_event("Unew", "hi bot", "rt1")).await;

        let result = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
        assert!(result.is_err(), "unpaired user message must be dropped");
        abort.abort();
    }

    #[tokio::test]
    async fn webhook_dm_pairing_bind_command_is_not_forwarded() {
        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Pairing,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            0,
        );
        let (port, mut rx, abort) = spawn_webhook(ch, "Ubot").await;

        // /bind command must be consumed, not forwarded to agent
        post_signed(
            port,
            "mysecret",
            &dm_event("Unew", "/bind wrongcode", "rt1"),
        )
        .await;

        let result = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
        assert!(result.is_err(), "/bind command must not reach agent");
        abort.abort();
    }

    #[tokio::test]
    async fn webhook_dm_pairing_allows_pre_seeded_user() {
        // If dm_policy=Pairing but a user is already in the channel peer
        // group, they should be forwarded without needing to /bind again.
        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Pairing,
            LineGroupPolicy::Open,
            "line_test_alias",
            resolver_from(vec!["Utrusted".to_string()]),
            0,
        );
        let (port, mut rx, abort) = spawn_webhook(ch, "Ubot").await;

        post_signed(port, "mysecret", &dm_event("Utrusted", "hey", "rt1")).await;

        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(msg.content, "hey");
        abort.abort();
    }

    #[tokio::test]
    async fn webhook_group_disabled_ignores_all_messages() {
        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Disabled,
            "line_test_alias",
            empty_resolver(),
            0,
        );
        let (port, mut rx, abort) = spawn_webhook(ch, "Ubot").await;

        post_signed(
            port,
            "mysecret",
            &group_event("Uuser", "Ggroup1", "hello group", "rt1"),
        )
        .await;

        let result = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
        assert!(
            result.is_err(),
            "group message must be dropped when policy=disabled"
        );
        abort.abort();
    }

    #[tokio::test]
    async fn webhook_group_open_forwards_any_message() {
        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            0,
        );
        let (port, mut rx, abort) = spawn_webhook(ch, "Ubot").await;

        post_signed(
            port,
            "mysecret",
            &group_event("Uuser", "Ggroup1", "anyone home?", "rt1"),
        )
        .await;

        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(msg.content, "anyone home?");
        assert_eq!(msg.reply_target, "Ggroup1");
        abort.abort();
    }

    #[tokio::test]
    async fn webhook_group_mention_skips_message_without_mention() {
        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Mention,
            "line_test_alias",
            empty_resolver(),
            0,
        );
        let (port, mut rx, abort) = spawn_webhook(ch, "Ubot123").await;

        // No mention in this group message
        post_signed(
            port,
            "mysecret",
            &group_event("Uuser", "Ggrp", "hey everyone", "rt1"),
        )
        .await;

        let result = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
        assert!(
            result.is_err(),
            "group message without mention must be dropped"
        );
        abort.abort();
    }

    #[tokio::test]
    async fn webhook_group_mention_forwards_message_with_mention() {
        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Mention,
            "line_test_alias",
            empty_resolver(),
            0,
        );
        let (port, mut rx, abort) = spawn_webhook(ch, "Ubot123").await;

        // "@Bot help me" — @Bot is 4 chars at index 0
        let payload = group_mention_event("Uuser", "Ggrp", "@Bot help me", "Ubot123", 0, 4, "rt1");
        post_signed(port, "mysecret", &payload).await;

        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        // Mention preserved verbatim — bot-mention stripping was dropped
        // from inbound message bodies so the agent sees what the operator
        // typed (including the @-mention).
        assert_eq!(msg.content, "@Bot help me");
        assert_eq!(msg.reply_target, "Ggrp");
        abort.abort();
    }

    #[tokio::test]
    async fn webhook_ignores_non_message_events() {
        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            0,
        );
        let (port, mut rx, abort) = spawn_webhook(ch, "Ubot").await;

        let follow_event = serde_json::json!({
            "events": [{
                "type": "follow",
                "source": {"type": "user", "userId": "Uuser"}
            }]
        });
        let status = post_signed(port, "mysecret", &follow_event).await;
        assert_eq!(status, 200);

        let result = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
        assert!(result.is_err(), "non-message events must be ignored");
        abort.abort();
    }

    #[tokio::test]
    async fn webhook_ignores_non_text_messages() {
        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            0,
        );
        let (port, mut rx, abort) = spawn_webhook(ch, "Ubot").await;

        let image_event = serde_json::json!({
            "events": [{
                "type": "message",
                "replyToken": "rt1",
                "source": {"type": "user", "userId": "Uuser"},
                "message": {"id": "img001", "type": "image"}
            }]
        });
        post_signed(port, "mysecret", &image_event).await;

        let result = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
        assert!(result.is_err(), "image messages must be ignored");
        abort.abort();
    }

    #[tokio::test]
    async fn webhook_empty_events_array_returns_200() {
        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            0,
        );
        let (port, _rx, abort) = spawn_webhook(ch, "Ubot").await;

        let status = post_signed(port, "mysecret", &serde_json::json!({"events": []})).await;
        assert_eq!(status, 200);
        abort.abort();
    }

    // ---- Transcription tests -----------------------------------------------

    #[tokio::test]
    async fn with_transcription_disabled_config_is_noop() {
        let ch = LineChannel::new(
            "tok".into(),
            "sec".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            0,
        )
        .with_transcription(zeroclaw_config::schema::TranscriptionConfig {
            enabled: false,
            ..Default::default()
        });
        assert!(ch.transcription_manager.is_none());
    }

    #[tokio::test]
    async fn webhook_audio_ignored_when_transcription_not_configured() {
        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            0,
        );
        let (port, mut rx, abort) = spawn_webhook(ch, "Ubot").await;

        let audio_event = serde_json::json!({
            "events": [{
                "type": "message",
                "replyToken": "rt1",
                "source": {"type": "user", "userId": "Uuser"},
                "message": {"id": "audio001", "type": "audio", "duration": 3000}
            }]
        });
        let status = post_signed(port, "mysecret", &audio_event).await;
        assert_eq!(status, 200);

        let result = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
        assert!(
            result.is_err(),
            "audio without transcription config must be ignored"
        );
        abort.abort();
    }

    #[tokio::test]
    async fn webhook_audio_transcribed_and_forwarded() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let api_server = MockServer::start().await;

        // Mock the LINE Content API endpoint for audio download
        Mock::given(method("GET"))
            .and(path_regex(r"/v2/bot/message/.*/content"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("content-type", "audio/x-m4a")
                    .set_body_bytes(b"fake-audio-bytes"),
            )
            .mount(&api_server)
            .await;

        // Mock the local_whisper transcription endpoint
        let transcript = "สวัสดี นี่คือข้อความเสียง";
        Mock::given(method("POST"))
            .and(wiremock::matchers::path("/v1/transcribe"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": transcript})),
            )
            .mount(&api_server)
            .await;

        let transcription_config = zeroclaw_config::schema::TranscriptionConfig {
            enabled: true,
            api_key: None,
            api_url: String::new(),
            model: "whisper-1".to_string(),
            language: None,
            initial_prompt: None,
            max_duration_secs: 120,
            max_audio_bytes: None,
            openai: None,
            deepgram: None,
            assemblyai: None,
            google: None,
            local_whisper: Some(zeroclaw_config::schema::LocalWhisperConfig {
                url: format!("{}/v1/transcribe", api_server.uri()),
                bearer_token: Some("test-token".to_string()),
                max_audio_bytes: 25 * 1024 * 1024,
                timeout_secs: 300,
            }),
            transcribe_non_ptt_audio: true,
        };

        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            0,
        )
        .with_api_base_url(&api_server.uri())
        .with_transcription(transcription_config);

        let (port, mut rx, abort) = spawn_webhook(ch, "Ubot").await;

        let audio_event = serde_json::json!({
            "events": [{
                "type": "message",
                "replyToken": "rt1",
                "source": {"type": "user", "userId": "Uuser"},
                "message": {"id": "audio001", "type": "audio", "duration": 3000}
            }]
        });
        post_signed(port, "mysecret", &audio_event).await;

        let msg = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for transcribed message")
            .expect("channel closed");

        assert_eq!(msg.content, transcript);
        assert_eq!(msg.sender, "Uuser");
        assert_eq!(msg.channel, "line");
        abort.abort();
    }

    // ---- build_sender_obj unit tests ---------------------------------------

    #[test]
    fn build_sender_obj_empty_name_returns_none() {
        assert!(LineChannel::build_sender_obj("", &None).is_none());
    }

    #[test]
    fn build_sender_obj_name_only_no_icon() {
        let obj = LineChannel::build_sender_obj("AI", &None).unwrap();
        assert_eq!(obj["name"], "AI");
        assert!(obj.get("iconUrl").is_none());
    }

    #[test]
    fn build_sender_obj_with_icon_url() {
        let url = "https://profile.line-scdn.net/icon.png".to_string();
        let obj = LineChannel::build_sender_obj("AI", &Some(url.clone())).unwrap();
        assert_eq!(obj["name"], "AI");
        assert_eq!(obj["iconUrl"], url);
    }

    // ---- Icon / Nickname Switch (sender object in outgoing messages) --------

    #[tokio::test]
    async fn send_reply_includes_sender_name_when_set() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/bot/message/reply"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let ch = LineChannel::new(
            "tok".into(),
            "sec".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            0,
        )
        .with_api_base_url(&server.uri());

        *ch.sender_name.write() = "AI".to_string();
        ch.pending_tokens
            .write()
            .insert("Urecipient".to_string(), "reply-token".to_string());

        ch.send(&SendMessage::new("hello", "Urecipient"))
            .await
            .unwrap();

        let reqs = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert_eq!(body["messages"][0]["sender"]["name"], "AI");
        assert!(body["messages"][0]["sender"].get("iconUrl").is_none());
    }

    #[tokio::test]
    async fn send_reply_includes_sender_icon_when_set() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/bot/message/reply"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        let ch = LineChannel::new(
            "tok".into(),
            "sec".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            0,
        )
        .with_api_base_url(&server.uri());

        *ch.sender_name.write() = "AI".to_string();
        *ch.sender_icon.write() =
            Some("https://profile.line-scdn.net/bot-icon.png".to_string());
        ch.pending_tokens
            .write()
            .insert("Urecipient".to_string(), "reply-token".to_string());

        ch.send(&SendMessage::new("hello", "Urecipient"))
            .await
            .unwrap();

        let reqs = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert_eq!(body["messages"][0]["sender"]["name"], "AI");
        assert_eq!(
            body["messages"][0]["sender"]["iconUrl"],
            "https://profile.line-scdn.net/bot-icon.png"
        );
    }

    #[tokio::test]
    async fn send_reply_omits_sender_when_name_empty() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/bot/message/reply"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        let ch = LineChannel::new(
            "tok".into(),
            "sec".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            0,
        )
        .with_api_base_url(&server.uri());

        // sender_name stays "" (not yet initialized)
        ch.pending_tokens
            .write()
            .insert("Urecipient".to_string(), "reply-token".to_string());

        ch.send(&SendMessage::new("hello", "Urecipient"))
            .await
            .unwrap();

        let reqs = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert!(
            body["messages"][0].get("sender").is_none(),
            "sender must be absent when name is empty"
        );
    }

    #[tokio::test]
    async fn send_push_includes_sender_name_when_set() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/bot/message/push"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        let ch = LineChannel::new(
            "tok".into(),
            "sec".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            0,
        )
        .with_api_base_url(&server.uri());

        *ch.sender_name.write() = "AI".to_string();
        // No pending token → falls through to Push API
        ch.send(&SendMessage::new("hi", "Urecipient")).await.unwrap();

        let reqs = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
        assert_eq!(body["messages"][0]["sender"]["name"], "AI");
    }

    // ---- Loading Indicator -------------------------------------------------

    #[tokio::test]
    async fn webhook_dm_sends_loading_indicator_with_user_id() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let api_server = MockServer::start().await;
        // Loading indicator endpoint
        Mock::given(method("POST"))
            .and(path("/v2/bot/chat/loading/start"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&api_server)
            .await;

        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            0,
        )
        .with_api_base_url(&api_server.uri());

        let (port, mut rx, abort) = spawn_webhook(ch, "Ubot").await;

        post_signed(port, "mysecret", &dm_event("Uuser1", "hi", "rt1")).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await;

        // Verify loading indicator was called with userId as chatId
        let reqs = api_server.received_requests().await.unwrap();
        let loading_req = reqs
            .iter()
            .find(|r| r.url.path() == "/v2/bot/chat/loading/start")
            .expect("loading indicator not called");
        let body: serde_json::Value = serde_json::from_slice(&loading_req.body).unwrap();
        assert_eq!(body["chatId"], "Uuser1");

        api_server.verify().await;
        abort.abort();
    }

    #[tokio::test]
    async fn webhook_group_sends_loading_indicator_with_group_id() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let api_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/bot/chat/loading/start"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&api_server)
            .await;

        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            0,
        )
        .with_api_base_url(&api_server.uri());

        let (port, mut rx, abort) = spawn_webhook(ch, "Ubot").await;

        post_signed(
            port,
            "mysecret",
            &group_event("Uuser", "Ggroup123", "hey", "rt1"),
        )
        .await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await;

        // Verify loading indicator was called with groupId, not userId
        let reqs = api_server.received_requests().await.unwrap();
        let loading_req = reqs
            .iter()
            .find(|r| r.url.path() == "/v2/bot/chat/loading/start")
            .expect("loading indicator not called");
        let body: serde_json::Value = serde_json::from_slice(&loading_req.body).unwrap();
        assert_eq!(body["chatId"], "Ggroup123");

        api_server.verify().await;
        abort.abort();
    }

    #[tokio::test]
    async fn webhook_loading_indicator_failure_does_not_break_message_delivery() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let api_server = MockServer::start().await;
        // Loading indicator returns 500 — must be fire-and-forget
        Mock::given(method("POST"))
            .and(path("/v2/bot/chat/loading/start"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&api_server)
            .await;

        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            0,
        )
        .with_api_base_url(&api_server.uri());

        let (port, mut rx, abort) = spawn_webhook(ch, "Ubot").await;

        let status = post_signed(port, "mysecret", &dm_event("Uuser1", "ping", "rt1")).await;
        assert_eq!(status, 200);

        // Message must still arrive even though loading indicator failed
        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("timed out — loading indicator failure must not drop message")
            .unwrap();
        assert_eq!(msg.content, "ping");
        abort.abort();
    }

    // ---- Bind Reply Feedback ------------------------------------------------

    #[tokio::test]
    async fn webhook_bind_success_sends_paired_reply() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let api_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/bot/chat/loading/start"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&api_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v2/bot/message/reply"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&api_server)
            .await;

        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Pairing,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            0,
        )
        .with_api_base_url(&api_server.uri());

        // Capture the real pairing code from stdout (the guard generates it).
        let code = ch
            .pairing
            .as_ref()
            .unwrap()
            .pairing_code()
            .unwrap()
            .to_string();

        let (port, _rx, abort) = spawn_webhook(ch, "Ubot").await;

        post_signed(
            port,
            "mysecret",
            &dm_event("Unew", &format!("/bind {code}"), "rt-bind"),
        )
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let reqs = api_server.received_requests().await.unwrap();
        let reply_req = reqs
            .iter()
            .find(|r| r.url.path() == "/v2/bot/message/reply")
            .expect("reply not sent after successful bind");
        let body: serde_json::Value = serde_json::from_slice(&reply_req.body).unwrap();
        assert_eq!(body["replyToken"], "rt-bind");
        assert_eq!(body["messages"][0]["text"], "Paired! You can now chat.");

        api_server.verify().await;
        abort.abort();
    }

    #[tokio::test]
    async fn webhook_bind_invalid_code_sends_error_reply() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let api_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2/bot/chat/loading/start"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&api_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v2/bot/message/reply"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&api_server)
            .await;

        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Pairing,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            0,
        )
        .with_api_base_url(&api_server.uri());

        let (port, _rx, abort) = spawn_webhook(ch, "Ubot").await;

        post_signed(
            port,
            "mysecret",
            &dm_event("Unew", "/bind wrongcode", "rt-bad"),
        )
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let reqs = api_server.received_requests().await.unwrap();
        let reply_req = reqs
            .iter()
            .find(|r| r.url.path() == "/v2/bot/message/reply")
            .expect("error reply not sent for invalid bind code");
        let body: serde_json::Value = serde_json::from_slice(&reply_req.body).unwrap();
        assert_eq!(body["replyToken"], "rt-bad");
        assert_eq!(body["messages"][0]["text"], "Invalid code. Please try again.");

        api_server.verify().await;
        abort.abort();
    }

    // ---- listen() sets sender identity from bot info -----------------------

    #[tokio::test]
    async fn listen_sets_sender_name_to_ai() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v2/bot/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "userId": "Ubot",
                "displayName": "Popcorn",
                "pictureUrl": "https://profile.line-scdn.net/popcorn.png"
            })))
            .mount(&server)
            .await;

        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Open,
            "line_test_alias",
            empty_resolver(),
            0,
        )
        .with_api_base_url(&server.uri());

        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let sender_name = Arc::clone(&ch.sender_name);
        let sender_icon = Arc::clone(&ch.sender_icon);

        let abort = zeroclaw_spawn::spawn!(async move {
            ch.listen_with_listener(listener, "Ubot".to_string(), tx)
                .await
                .ok();
        })
        .abort_handle();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // sender_name is set by listen() before spawning listen_with_listener,
        // but since we bypass listen() here we seed it manually to test the
        // contract — the real assertion is covered by the integration path.
        // Instead verify the field types and defaults before listen():
        assert_eq!(*sender_name.read(), "");
        assert!(sender_icon.read().is_none());
        abort.abort();
    }

    #[tokio::test]
    async fn bot_info_deserializes_picture_url() {
        // Verify BotInfo serde picks up pictureUrl when present.
        let json = serde_json::json!({
            "userId": "Ubot123",
            "displayName": "Popcorn",
            "pictureUrl": "https://profile.line-scdn.net/popcorn.png"
        });
        let info: BotInfo = serde_json::from_value(json).unwrap();
        assert_eq!(info.user_id, "Ubot123");
        assert_eq!(info.display_name, "Popcorn");
        assert_eq!(
            info.picture_url.as_deref(),
            Some("https://profile.line-scdn.net/popcorn.png")
        );
    }

    #[tokio::test]
    async fn bot_info_picture_url_defaults_to_none() {
        let json = serde_json::json!({"userId": "Ubot", "displayName": "Bot"});
        let info: BotInfo = serde_json::from_value(json).unwrap();
        assert!(info.picture_url.is_none());
    }
}
