use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use hmac::{Hmac, Mac};
use parking_lot::RwLock;
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_config::schema::{LineDmPolicy, LineGroupPolicy};
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
/// - `allowlist` — respond only to user IDs in `allowed_users`
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
    /// Allowlist — used when `dm_policy = Allowlist`.
    allowed_users: Arc<RwLock<Vec<String>>>,
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
    allowed_users: Arc<RwLock<Vec<String>>>,
    pairing: Option<Arc<PairingGuard>>,
    pending_tokens: Arc<RwLock<HashMap<String, String>>>,
    /// HTTP client and credentials for downloading audio content.
    client: reqwest::Client,
    channel_access_token: String,
    content_api_base_url: String,
    /// Optional transcription manager — `None` when transcription is disabled.
    transcription_manager: Option<Arc<super::transcription::TranscriptionManager>>,
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
        anyhow::bail!("LINE: audio download failed ({status}) for message {message_id}");
    }

    let mut bytes = Vec::new();
    let mut stream = resp;
    while let Some(chunk) = stream.chunk().await? {
        bytes.extend_from_slice(&chunk);
        if bytes.len() as u64 > MAX_LINE_AUDIO_BYTES {
            anyhow::bail!(
                "LINE: audio exceeds {} byte limit for message {message_id}",
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
        tracing::warn!("LINE: rejected request with invalid signature");
        return StatusCode::UNAUTHORIZED;
    }

    // 2. Parse payload
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("LINE: invalid JSON payload: {e}");
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
                    tracing::debug!("LINE: audio message ignored (transcription not configured)");
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
                        tracing::warn!("LINE: audio download failed for {msg_id}: {e}");
                        continue;
                    }
                };
                let transcript = match manager.transcribe(&audio, "audio.m4a").await {
                    Ok(t) if !t.trim().is_empty() => t,
                    Ok(_) => {
                        tracing::debug!("LINE: empty transcript for {msg_id}");
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!("LINE: transcription failed for {msg_id}: {e}");
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

        // 3. Group policy gate
        if is_group {
            match state.group_policy {
                LineGroupPolicy::Disabled => continue,
                LineGroupPolicy::Open => {}
                LineGroupPolicy::Mention => {
                    let mention_span = LineChannel::find_bot_mention(msg_obj, &state.bot_user_id);
                    if mention_span.is_none() {
                        tracing::debug!(
                            "LINE: skipping group message without bot mention (userId: {})",
                            state.bot_user_id
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
                    let allowed = state
                        .allowed_users
                        .read()
                        .iter()
                        .any(|u| u == "*" || u == user_id);
                    if !allowed {
                        tracing::warn!(
                            "LINE: ignoring DM from unauthorized user: {user_id}. \
                            Add to channels.line.allowed_users or use dm_policy = pairing."
                        );
                        continue;
                    }
                }
                LineDmPolicy::Pairing => {
                    let already_allowed = state
                        .allowed_users
                        .read()
                        .iter()
                        .any(|u| u == "*" || u == user_id);

                    if !already_allowed {
                        // Try pairing bind
                        if let Some(code) = LineChannel::extract_bind_code(text) {
                            if let Some(ref guard) = state.pairing {
                                match guard.try_pair(code, user_id).await {
                                    Ok(Some(_)) => {
                                        state.allowed_users.write().push(user_id.to_string());
                                        tracing::info!("LINE: paired userId={user_id}");
                                        // Send confirmation via Push API (no reply token yet)
                                        // We forward a synthetic message to let the agent greet
                                    }
                                    Ok(None) => {
                                        tracing::warn!(
                                            "LINE: invalid bind code from userId={user_id}"
                                        );
                                    }
                                    Err(wait_ms) => {
                                        tracing::warn!(
                                            "LINE: bind rate-limited for userId={user_id}, retry after {wait_ms}ms"
                                        );
                                    }
                                }
                            }
                            continue; // bind commands are not forwarded to agent
                        }

                        tracing::warn!(
                            "LINE: ignoring message from unpaired user: {user_id}. \
                            Send `{LINE_BIND_COMMAND} <code>` to pair."
                        );
                        continue;
                    }
                }
            }
        }

        // 5. Mention gate for group messages using `mention` policy
        let mention_span = if is_group && state.group_policy == LineGroupPolicy::Mention {
            LineChannel::find_bot_mention(msg_obj, &state.bot_user_id)
        } else {
            None
        };

        // 6. Resolve recipient (groupId/roomId for group context)
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

        // 7. Strip the @mention span from group messages using char index/length.
        let content = if let Some((idx, len)) = mention_span {
            let stripped = LineChannel::strip_mention_range(text, idx, len);
            if stripped.is_empty() {
                continue;
            }
            stripped
        } else {
            text.trim().to_string()
        };

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
            timestamp,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        };

        if state.tx.send(channel_msg).await.is_err() {
            tracing::warn!("LINE: receiver dropped, shutting down webhook server");
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    }

    StatusCode::OK
}

// ---------------------------------------------------------------------------
// LineChannel implementation
// ---------------------------------------------------------------------------

impl LineChannel {
    /// Create a new `LineChannel`.
    ///
    /// `channel_access_token` and `channel_secret` fall back to the
    /// `LINE_CHANNEL_ACCESS_TOKEN` and `LINE_CHANNEL_SECRET` environment
    /// variables respectively when the config value is empty.
    pub fn new(
        channel_access_token: String,
        channel_secret: String,
        dm_policy: LineDmPolicy,
        group_policy: LineGroupPolicy,
        allowed_users: Vec<String>,
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

        let pairing = if dm_policy == LineDmPolicy::Pairing && allowed_users.is_empty() {
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
            allowed_users: Arc::new(RwLock::new(allowed_users)),
            pairing,
            webhook_port,
            pending_tokens: Arc::new(RwLock::new(HashMap::new())),
            client: zeroclaw_config::schema::build_channel_proxy_client("channel.line", None),
            api_base_url: "https://api.line.me".to_string(),
            content_api_base_url: "https://api-data.line.me".to_string(),
            transcription_manager: None,
        }
    }

    /// Construct a `LineChannel` directly from a [`zeroclaw_config::schema::LineConfig`].
    ///
    /// Mirrors [`LarkChannel::from_config`] — keeps construction logic inside the
    /// channel crate rather than duplicating it across orchestrator call sites.
    pub fn from_config(config: &zeroclaw_config::schema::LineConfig) -> Self {
        Self::new(
            config.channel_access_token.clone(),
            config.channel_secret.clone(),
            config.dm_policy.clone(),
            config.group_policy.clone(),
            config.allowed_users.clone(),
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
                self.transcription_manager = Some(Arc::new(m));
            }
            Err(e) => {
                tracing::warn!(
                    "LINE: transcription manager init failed, audio transcription disabled: {e}"
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
            anyhow::bail!("LINE: failed to fetch bot info ({status}): {err}");
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

    /// Remove a mention span from `text` using character-unit `index`/`length`
    /// from LINE's mention data.
    ///
    /// After removal, any run of whitespace characters at the join point is
    /// collapsed to a single space so that "hey @Bot help" → "hey help".
    fn strip_mention_range(text: &str, char_index: usize, char_length: usize) -> String {
        let chars: Vec<char> = text.chars().collect();
        let total = chars.len();

        let before: String = chars[..char_index.min(total)].iter().collect();
        let end = (char_index + char_length).min(total);
        let after: String = chars[end..].iter().collect();

        // Trim trailing whitespace from `before` and leading from `after`,
        // then rejoin with a single space only when both sides are non-empty.
        let before_trimmed = before.trim_end();
        let after_trimmed = after.trim_start();

        match (before_trimmed.is_empty(), after_trimmed.is_empty()) {
            (true, true) => String::new(),
            (true, false) => after_trimmed.to_string(),
            (false, true) => before_trimmed.to_string(),
            (false, false) => format!("{before_trimmed} {after_trimmed}"),
        }
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

    /// Send text via the Reply API (consumes `reply_token`).
    async fn send_reply(&self, reply_token: &str, text: &str) -> anyhow::Result<()> {
        let url = format!("{}/v2/bot/message/reply", self.api_base_url);
        let messages: Vec<serde_json::Value> = Self::split_message(text)
            .into_iter()
            .map(|chunk| serde_json::json!({"type": "text", "text": chunk}))
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
                anyhow::bail!("LINE Reply API failed ({status}): {err}");
            }
        }
        Ok(())
    }

    /// Send text via the Push API (requires a paid LINE plan for high volume).
    async fn send_push(&self, to: &str, text: &str) -> anyhow::Result<()> {
        let url = format!("{}/v2/bot/message/push", self.api_base_url);
        let messages: Vec<serde_json::Value> = Self::split_message(text)
            .into_iter()
            .map(|chunk| serde_json::json!({"type": "text", "text": chunk}))
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
                anyhow::bail!("LINE Push API failed ({status}): {err}");
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
            allowed_users: Arc::clone(&self.allowed_users),
            pairing: self.pairing.clone(),
            pending_tokens: Arc::clone(&self.pending_tokens),
            client: self.client.clone(),
            channel_access_token: self.channel_access_token.clone(),
            content_api_base_url: self.content_api_base_url.clone(),
            transcription_manager: self.transcription_manager.clone(),
        });

        let app = build_webhook_router(state);
        axum::serve(listener, app)
            .await
            .map_err(|e| anyhow::anyhow!("LINE webhook server error: {e}"))
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
                    tracing::warn!(
                        "LINE: Reply API failed (token may be expired), falling back to Push: {e}"
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
        tracing::info!(
            "LINE: connected as '{}' (userId: {})",
            bot_info.display_name,
            bot_info.user_id
        );

        let addr = std::net::SocketAddr::from(([0, 0, 0, 0], self.webhook_port));
        tracing::info!(
            "LINE: webhook server listening on http://0.0.0.0:{}/line/webhook",
            self.webhook_port
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

    fn make_channel() -> LineChannel {
        LineChannel::new(
            "test_access_token".into(),
            "test_secret".into(),
            LineDmPolicy::Open,
            LineGroupPolicy::Mention,
            vec![],
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
        let jh = tokio::spawn(async move {
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
    fn pairing_mode_creates_guard_when_no_allowed_users() {
        let ch = LineChannel::new(
            "tok".into(),
            "sec".into(),
            LineDmPolicy::Pairing,
            LineGroupPolicy::Mention,
            vec![],
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
            vec!["Uallowed".into()],
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
            vec![],
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
            vec![],
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
    fn strip_mention_range_prefix() {
        let result = LineChannel::strip_mention_range("@Bot hello", 0, 4);
        assert_eq!(result, "hello");
    }

    #[test]
    fn strip_mention_range_mid() {
        let result = LineChannel::strip_mention_range("hey @Bot help", 4, 4);
        assert_eq!(result, "hey help");
    }

    #[test]
    fn strip_mention_range_unicode() {
        // "สวัสดี @Bot ช่วยด้วย"
        // สวัสดี = 6 chars, space = 1 → @Bot starts at char index 7, length 4
        let text = "สวัสดี @Bot ช่วยด้วย";
        let result = LineChannel::strip_mention_range(text, 7, 4);
        assert_eq!(result, "สวัสดี ช่วยด้วย");
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
            vec![],
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
            vec![],
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
            vec![],
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
            vec![],
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
            vec![],
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
            vec![],
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
            vec![],
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
            vec![],
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
            vec!["Uallowed".to_string()],
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
            vec!["Uallowed".to_string()],
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
            vec![],
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
            vec![],
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
        // If dm_policy=Pairing but a user is already in allowed_users,
        // they should be forwarded without needing to /bind again.
        let ch = LineChannel::new(
            "tok".into(),
            "mysecret".into(),
            LineDmPolicy::Pairing,
            LineGroupPolicy::Open,
            vec!["Utrusted".to_string()],
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
            vec![],
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
            vec![],
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
            vec![],
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
            vec![],
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
        // Mention span stripped: "@Bot " removed, leaving "help me"
        assert_eq!(msg.content, "help me");
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
            vec![],
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
            vec![],
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
            vec![],
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
            vec![],
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
            vec![],
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
            default_transcription_provider: "local_whisper".to_string(),
            api_key: None,
            api_url: String::new(),
            model: "whisper-1".to_string(),
            language: None,
            initial_prompt: None,
            max_duration_secs: 120,
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
            vec![],
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
}
