use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use uuid::Uuid;

/// Linq channel — uses the Linq Partner V3 API for iMessage, RCS, and SMS.
///
/// This channel operates in webhook mode (push-based) rather than polling.
/// Messages are received via the gateway's `/linq` webhook endpoint.
/// The `listen` method here is a keepalive placeholder; actual message handling
/// happens in the gateway when Linq sends webhook events.
pub struct LinqChannel {
    api_token: String,
    from_phone: String,
    allowed_senders: Vec<String>,
    client: reqwest::Client,
    transcription_manager: Option<std::sync::Arc<super::transcription::TranscriptionManager>>,
}

const LINQ_API_BASE: &str = "https://api.linqapp.com/api/partner/v3";

/// Maximum audio download size (25MB)
const MAX_LINQ_AUDIO_BYTES: u64 = 25 * 1024 * 1024;

/// Returns true if the URL host resolves to a private, loopback, or link-local address.
fn is_private_url(url: &str) -> bool {
    let parsed = match reqwest::Url::parse(url) {
        Ok(u) => u,
        Err(_) => return true, // unparseable URLs are blocked
    };

    let host = match parsed.host_str() {
        Some(h) => h.to_ascii_lowercase(),
        None => return true, // no host → block
    };

    // Block localhost variants
    if host == "localhost" || host == "127.0.0.1" || host == "[::1]" || host == "::1" {
        return true;
    }

    // Block private IP ranges
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback()
                    || v4.is_private()
                    || v4.is_link_local()
                    || v4.octets()[0] == 169 && v4.octets()[1] == 254 // link-local
            }
            std::net::IpAddr::V6(v6) => v6.is_loopback(),
        };
    }

    // Block internal hostnames
    if host.ends_with(".local") || host.ends_with(".internal") || host == "metadata.google.internal"
    {
        return true;
    }

    false
}

/// Map MIME subtype or URL extension to a supported audio filename extension.
fn extension_for_audio(format: &str) -> Option<&'static str> {
    match format.to_ascii_lowercase().as_str() {
        "flac" => Some("flac"),
        "mp3" | "mpeg" | "mpga" => Some("mp3"),
        "mp4" | "m4a" => Some("mp4"),
        "ogg" | "oga" | "opus" => Some("ogg"),
        "wav" => Some("wav"),
        "webm" => Some("webm"),
        _ => None,
    }
}

impl LinqChannel {
    pub fn new(api_token: String, from_phone: String, allowed_senders: Vec<String>) -> Self {
        Self {
            api_token,
            from_phone,
            allowed_senders,
            client: reqwest::Client::new(),
            transcription_manager: None,
        }
    }

    pub fn with_transcription(mut self, config: crate::config::TranscriptionConfig) -> Self {
        if !config.enabled {
            return self;
        }
        match super::transcription::TranscriptionManager::new(&config) {
            Ok(m) => {
                self.transcription_manager = Some(std::sync::Arc::new(m));
            }
            Err(e) => {
                tracing::warn!(
                    "transcription manager init failed, audio transcription disabled: {e}"
                );
            }
        }
        self
    }

    /// Check if a sender phone number is allowed (E.164 format: +1234567890)
    fn is_sender_allowed(&self, phone: &str) -> bool {
        self.allowed_senders.iter().any(|n| n == "*" || n == phone)
    }

    /// Get the bot's phone number
    pub fn phone_number(&self) -> &str {
        &self.from_phone
    }

    fn media_part_to_content_marker(part: &serde_json::Value) -> Option<String> {
        let source = part
            .get("url")
            .or_else(|| part.get("value"))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())?;

        let mime_type = part
            .get("mime_type")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .unwrap_or_default()
            .to_ascii_lowercase();

        if mime_type.starts_with("audio/") {
            return Some(format!("[AUDIO:{source}]"));
        }

        if !mime_type.starts_with("image/") {
            return None;
        }

        Some(format!("[IMAGE:{source}]"))
    }

    fn sender_is_from_me(data: &serde_json::Value) -> bool {
        // Legacy format: data.is_from_me
        if let Some(v) = data.get("is_from_me").and_then(|value| value.as_bool()) {
            return v;
        }

        // New format: data.sender_handle.is_me OR data.direction == "outbound"
        let is_me = data
            .get("sender_handle")
            .and_then(|value| value.get("is_me"))
            .and_then(|value| value.as_bool())
            .unwrap_or(false);

        let is_outbound = matches!(
            data.get("direction").and_then(|value| value.as_str()),
            Some("outbound")
        );

        is_me || is_outbound
    }

    fn sender_handle(data: &serde_json::Value) -> Option<&str> {
        data.get("from")
            .and_then(|value| value.as_str())
            .or_else(|| {
                data.get("sender_handle")
                    .and_then(|value| value.get("handle"))
                    .and_then(|value| value.as_str())
            })
    }

    fn chat_id(data: &serde_json::Value) -> Option<&str> {
        data.get("chat_id")
            .and_then(|value| value.as_str())
            .or_else(|| {
                data.get("chat")
                    .and_then(|value| value.get("id"))
                    .and_then(|value| value.as_str())
            })
    }

    fn message_parts(data: &serde_json::Value) -> Option<&Vec<serde_json::Value>> {
        data.get("message")
            .and_then(|value| value.get("parts"))
            .and_then(|value| value.as_array())
            .or_else(|| data.get("parts").and_then(|value| value.as_array()))
    }

    /// Parse an incoming webhook payload from Linq and extract messages.
    ///
    /// Supports two webhook formats:
    ///
    /// **New format (webhook_version 2026-02-03):**
    /// ```json
    /// {
    ///   "api_version": "v3",
    ///   "webhook_version": "2026-02-03",
    ///   "event_type": "message.received",
    ///   "data": {
    ///     "id": "msg-...",
    ///     "direction": "inbound",
    ///     "sender_handle": { "handle": "+1...", "is_me": false },
    ///     "chat": { "id": "chat-..." },
    ///     "parts": [{ "type": "text", "value": "..." }]
    ///   }
    /// }
    /// ```
    ///
    /// **Legacy format (webhook_version 2025-01-01):**
    /// ```json
    /// {
    ///   "api_version": "v3",
    ///   "event_type": "message.received",
    ///   "data": {
    ///     "chat_id": "...",
    ///     "from": "+1...",
    ///     "is_from_me": false,
    ///     "message": {
    ///       "id": "...",
    ///       "parts": [{ "type": "text", "value": "..." }]
    ///     }
    ///   }
    /// }
    /// ```
    ///
    /// Also accepts the current 2026-02-03 payload shape where `chat_id`,
    /// `from`, `is_from_me`, and `message.parts` moved under:
    /// `data.chat.id`, `data.sender_handle.handle`, `data.sender_handle.is_me`,
    /// and `data.parts`.
    pub fn parse_webhook_payload(&self, payload: &serde_json::Value) -> Vec<ChannelMessage> {
        let mut messages = Vec::new();

        // Only handle message.received events
        let event_type = payload
            .get("event_type")
            .and_then(|e| e.as_str())
            .unwrap_or("");
        if event_type != "message.received" {
            tracing::debug!("Linq: skipping non-message event: {event_type}");
            return messages;
        }

        let Some(data) = payload.get("data") else {
            return messages;
        };

        // Skip messages sent by the bot itself
        if Self::sender_is_from_me(data) {
            tracing::debug!("Linq: skipping is_from_me message");
            return messages;
        }

        // Get sender phone number
        let Some(from) = Self::sender_handle(data) else {
            return messages;
        };

        // Normalize to E.164 format
        let normalized_from = if from.starts_with('+') {
            from.to_string()
        } else {
            format!("+{from}")
        };

        // Check allowlist
        if !self.is_sender_allowed(&normalized_from) {
            tracing::warn!(
                "Linq: ignoring message from unauthorized sender: {normalized_from}. \
                Add to channels.linq.allowed_senders in config.toml, \
                or run `zeroclaw onboard --channels-only` to configure interactively."
            );
            return messages;
        }

        // Get chat_id for reply routing
        let chat_id = Self::chat_id(data).unwrap_or("").to_string();

        // Extract text from message parts
        let Some(parts) = Self::message_parts(data) else {
            return messages;
        };

        let content_parts: Vec<String> = parts
            .iter()
            .filter_map(|part| {
                let part_type = part.get("type").and_then(|t| t.as_str())?;
                match part_type {
                    "text" => part
                        .get("value")
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string),
                    "media" | "image" => {
                        if let Some(marker) = Self::media_part_to_content_marker(part) {
                            Some(marker)
                        } else {
                            tracing::debug!("Linq: skipping unsupported {part_type} part");
                            None
                        }
                    }
                    _ => {
                        tracing::debug!("Linq: skipping {part_type} part");
                        None
                    }
                }
            })
            .collect();

        if content_parts.is_empty() {
            return messages;
        }

        let content = content_parts.join("\n").trim().to_string();

        if content.is_empty() {
            return messages;
        }

        // Get timestamp from created_at or use current time
        let timestamp = payload
            .get("created_at")
            .and_then(|t| t.as_str())
            .and_then(|t| {
                chrono::DateTime::parse_from_rfc3339(t)
                    .ok()
                    .map(|dt| dt.timestamp().cast_unsigned())
            })
            .unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            });

        // Use chat_id as reply_target so replies go to the right conversation
        let reply_target = if chat_id.is_empty() {
            normalized_from.clone()
        } else {
            chat_id
        };

        messages.push(ChannelMessage {
            id: Uuid::new_v4().to_string(),
            reply_target,
            sender: normalized_from,
            content,
            channel: "linq".to_string(),
            timestamp,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: vec![],
        });

        messages
    }

    /// Download and transcribe an audio file from a URL.
    ///
    /// Returns the transcript text on success, or `None` if transcription is not configured,
    /// the download fails, or the audio format cannot be determined.
    pub async fn try_transcribe_audio_part(&self, url: &str) -> Option<String> {
        let manager = self.transcription_manager.as_ref()?;

        #[cfg(not(test))]
        if is_private_url(url) {
            tracing::warn!("blocked audio fetch to private/internal URL: {url}");
            return None;
        }

        // Download the audio file
        let response = match self.client.get(url).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("failed to download audio from {url}: {e}");
                return None;
            }
        };

        if !response.status().is_success() {
            tracing::warn!(
                "audio download failed with status {}: {url}",
                response.status()
            );
            return None;
        }

        if let Some(content_length) = response.content_length() {
            if content_length > MAX_LINQ_AUDIO_BYTES {
                tracing::warn!(
                    "audio download skipped for {url}: content-length {content_length} exceeds {} bytes",
                    MAX_LINQ_AUDIO_BYTES
                );
                return None;
            }
        }

        // Derive extension from Content-Type header
        let extension = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|ct| ct.to_str().ok())
            .and_then(|ct| {
                let subtype = ct.split('/').nth(1)?.split(';').next()?.trim();
                extension_for_audio(subtype)
            })
            .or_else(|| {
                // Fallback: derive extension from URL path (strip query and fragment)
                url.split('/')
                    .next_back()?
                    .split('?')
                    .next()?
                    .split('#')
                    .next()?
                    .rsplit_once('.')
                    .and_then(|(_, ext)| extension_for_audio(ext))
            });

        let Some(ext) = extension else {
            tracing::warn!("could not determine audio format for {url}");
            return None;
        };

        let audio_bytes = match response.bytes().await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("failed to read audio bytes from {url}: {e}");
                return None;
            }
        };

        if audio_bytes.len() as u64 > MAX_LINQ_AUDIO_BYTES {
            tracing::warn!(
                "audio download too large after fetch for {url}: {} bytes exceeds {} bytes",
                audio_bytes.len(),
                MAX_LINQ_AUDIO_BYTES
            );
            return None;
        }

        let file_name = format!("audio.{ext}");
        match manager.transcribe(&audio_bytes, &file_name).await {
            Ok(transcript) => Some(transcript),
            Err(e) => {
                tracing::warn!("transcription failed for {url}: {e}");
                None
            }
        }
    }
}

#[async_trait]
impl Channel for LinqChannel {
    fn name(&self) -> &str {
        "linq"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        // If reply_target looks like a chat_id, send to existing chat.
        // Otherwise create a new chat with the recipient phone number.
        let recipient = &message.recipient;

        let body = serde_json::json!({
            "message": {
                "parts": [{
                    "type": "text",
                    "value": message.content
                }]
            }
        });

        // Try sending to existing chat (recipient is chat_id)
        let url = format!("{LINQ_API_BASE}/chats/{recipient}/messages");

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_token)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if resp.status().is_success() {
            return Ok(());
        }

        // If the chat_id-based send failed with 404, try creating a new chat
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            let new_chat_body = serde_json::json!({
                "from": self.from_phone,
                "to": [recipient],
                "message": {
                    "parts": [{
                        "type": "text",
                        "value": message.content
                    }]
                }
            });

            let create_resp = self
                .client
                .post(format!("{LINQ_API_BASE}/chats"))
                .bearer_auth(&self.api_token)
                .header("Content-Type", "application/json")
                .json(&new_chat_body)
                .send()
                .await?;

            if !create_resp.status().is_success() {
                let status = create_resp.status();
                let error_body = create_resp.text().await.unwrap_or_default();
                tracing::error!("Linq create chat failed: {status} — {error_body}");
                anyhow::bail!("Linq API error: {status}");
            }

            return Ok(());
        }

        let status = resp.status();
        let error_body = resp.text().await.unwrap_or_default();
        tracing::error!("Linq send failed: {status} — {error_body}");
        anyhow::bail!("Linq API error: {status}");
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // Linq uses webhooks (push-based), not polling.
        // Messages are received via the gateway's /linq endpoint.
        tracing::info!(
            "Linq channel active (webhook mode). \
            Configure Linq webhook to POST to your gateway's /linq endpoint."
        );

        // Keep the task alive — it will be cancelled when the channel shuts down
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    async fn health_check(&self) -> bool {
        // Check if we can reach the Linq API
        let url = format!("{LINQ_API_BASE}/phonenumbers");

        self.client
            .get(&url)
            .bearer_auth(&self.api_token)
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    async fn start_typing(&self, recipient: &str) -> anyhow::Result<()> {
        let url = format!("{LINQ_API_BASE}/chats/{recipient}/typing");

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_token)
            .send()
            .await?;

        if !resp.status().is_success() {
            tracing::debug!("Linq start_typing failed: {}", resp.status());
        }

        Ok(())
    }

    async fn stop_typing(&self, recipient: &str) -> anyhow::Result<()> {
        let url = format!("{LINQ_API_BASE}/chats/{recipient}/typing");

        let resp = self
            .client
            .delete(&url)
            .bearer_auth(&self.api_token)
            .send()
            .await?;

        if !resp.status().is_success() {
            tracing::debug!("Linq stop_typing failed: {}", resp.status());
        }

        Ok(())
    }
}

/// Verify a Linq webhook signature.
///
/// Linq signs webhooks with HMAC-SHA256 over `"{timestamp}.{body}"`.
/// The signature is sent in `X-Webhook-Signature` (hex-encoded) and the
/// timestamp in `X-Webhook-Timestamp`. Reject timestamps older than 300s.
pub fn verify_linq_signature(secret: &str, body: &str, timestamp: &str, signature: &str) -> bool {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    // Reject stale timestamps (>300s old)
    if let Ok(ts) = timestamp.parse::<i64>() {
        let now = chrono::Utc::now().timestamp();
        if (now - ts).unsigned_abs() > 300 {
            tracing::warn!("Linq: rejecting stale webhook timestamp ({ts}, now={now})");
            return false;
        }
    } else {
        tracing::warn!("Linq: invalid webhook timestamp: {timestamp}");
        return false;
    }

    // Compute HMAC-SHA256 over "{timestamp}.{body}"
    let message = format!("{timestamp}.{body}");
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(message.as_bytes());
    let signature_hex = signature
        .trim()
        .strip_prefix("sha256=")
        .unwrap_or(signature);
    let Ok(provided) = hex::decode(signature_hex.trim()) else {
        tracing::warn!("Linq: invalid webhook signature format");
        return false;
    };

    // Constant-time comparison via HMAC verify.
    mac.verify_slice(&provided).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_channel() -> LinqChannel {
        LinqChannel::new(
            "test-token".into(),
            "+15551234567".into(),
            vec!["+1234567890".into()],
        )
    }

    #[test]
    fn linq_channel_name() {
        let ch = make_channel();
        assert_eq!(ch.name(), "linq");
    }

    #[test]
    fn linq_sender_allowed_exact() {
        let ch = make_channel();
        assert!(ch.is_sender_allowed("+1234567890"));
        assert!(!ch.is_sender_allowed("+9876543210"));
    }

    #[test]
    fn linq_sender_allowed_wildcard() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        assert!(ch.is_sender_allowed("+1234567890"));
        assert!(ch.is_sender_allowed("+9999999999"));
    }

    #[test]
    fn linq_sender_allowed_empty() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec![]);
        assert!(!ch.is_sender_allowed("+1234567890"));
    }

    #[test]
    fn linq_parse_valid_text_message() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "api_version": "v3",
            "event_type": "message.received",
            "event_id": "evt-123",
            "created_at": "2025-01-15T12:00:00Z",
            "trace_id": "trace-456",
            "data": {
                "chat_id": "chat-789",
                "from": "+1234567890",
                "recipient_phone": "+15551234567",
                "is_from_me": false,
                "service": "iMessage",
                "message": {
                    "id": "msg-abc",
                    "parts": [{
                        "type": "text",
                        "value": "Hello ZeroClaw!"
                    }]
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "+1234567890");
        assert_eq!(msgs[0].content, "Hello ZeroClaw!");
        assert_eq!(msgs[0].channel, "linq");
        assert_eq!(msgs[0].reply_target, "chat-789");
    }

    #[test]
    fn linq_parse_latest_webhook_shape() {
        let ch = LinqChannel::new(
            "tok".into(),
            "+15551234567".into(),
            vec!["+1234567890".into()],
        );
        let payload = serde_json::json!({
            "api_version": "v3",
            "webhook_version": "2026-02-03",
            "event_type": "message.received",
            "created_at": "2026-02-03T12:00:00Z",
            "data": {
                "chat": {
                    "id": "chat-2026"
                },
                "direction": "inbound",
                "id": "msg-2026",
                "parts": [{
                    "type": "text",
                    "value": "Hello from the latest payload"
                }],
                "sender_handle": {
                    "handle": "1234567890",
                    "is_me": false
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "+1234567890");
        assert_eq!(msgs[0].content, "Hello from the latest payload");
        assert_eq!(msgs[0].reply_target, "chat-2026");
    }

    #[test]
    fn linq_parse_skip_is_from_me() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat_id": "chat-789",
                "from": "+1234567890",
                "is_from_me": true,
                "message": {
                    "id": "msg-abc",
                    "parts": [{ "type": "text", "value": "My own message" }]
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "is_from_me messages should be skipped");
    }

    #[test]
    fn linq_parse_skip_latest_outbound_message() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat": {
                    "id": "chat-789"
                },
                "direction": "outbound",
                "parts": [{
                    "type": "text",
                    "value": "My own message"
                }],
                "sender_handle": {
                    "handle": "+1234567890",
                    "is_me": true
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(
            msgs.is_empty(),
            "latest outbound messages from the bot should be skipped"
        );
    }

    #[test]
    fn linq_parse_skip_non_message_event() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "event_type": "message.delivered",
            "data": {
                "chat_id": "chat-789",
                "message_id": "msg-abc"
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "Non-message events should be skipped");
    }

    #[test]
    fn linq_parse_unauthorized_sender() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat_id": "chat-789",
                "from": "+9999999999",
                "is_from_me": false,
                "message": {
                    "id": "msg-abc",
                    "parts": [{ "type": "text", "value": "Spam" }]
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "Unauthorized senders should be filtered");
    }

    #[test]
    fn linq_parse_empty_payload() {
        let ch = make_channel();
        let payload = serde_json::json!({});
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn linq_parse_media_only_translated_to_image_marker() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat_id": "chat-789",
                "from": "+1234567890",
                "is_from_me": false,
                "message": {
                    "id": "msg-abc",
                    "parts": [{
                        "type": "media",
                        "url": "https://example.com/image.jpg",
                        "mime_type": "image/jpeg"
                    }]
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "[IMAGE:https://example.com/image.jpg]");
    }

    #[test]
    fn linq_parse_media_non_image_still_skipped() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat_id": "chat-789",
                "from": "+1234567890",
                "is_from_me": false,
                "message": {
                    "id": "msg-abc",
                    "parts": [{
                        "type": "media",
                        "url": "https://example.com/video.mp4",
                        "mime_type": "video/mp4"
                    }]
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(
            msgs.is_empty(),
            "Non-image, non-audio media should still be skipped"
        );
    }

    #[test]
    fn linq_parse_multiple_text_parts() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat_id": "chat-789",
                "from": "+1234567890",
                "is_from_me": false,
                "message": {
                    "id": "msg-abc",
                    "parts": [
                        { "type": "text", "value": "First part" },
                        { "type": "text", "value": "Second part" }
                    ]
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "First part\nSecond part");
    }

    /// Fixture secret used exclusively in signature-verification unit tests (not a real credential).
    const TEST_WEBHOOK_SECRET: &str = "test_webhook_secret";

    #[test]
    fn linq_signature_verification_valid() {
        let secret = TEST_WEBHOOK_SECRET;
        let body = r#"{"event_type":"message.received"}"#;
        let now = chrono::Utc::now().timestamp().to_string();

        // Compute expected signature
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let message = format!("{now}.{body}");
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(message.as_bytes());
        let signature = hex::encode(mac.finalize().into_bytes());

        assert!(verify_linq_signature(secret, body, &now, &signature));
    }

    #[test]
    fn linq_signature_verification_invalid() {
        let secret = TEST_WEBHOOK_SECRET;
        let body = r#"{"event_type":"message.received"}"#;
        let now = chrono::Utc::now().timestamp().to_string();

        assert!(!verify_linq_signature(
            secret,
            body,
            &now,
            "deadbeefdeadbeefdeadbeef"
        ));
    }

    #[test]
    fn linq_signature_verification_stale_timestamp() {
        let secret = TEST_WEBHOOK_SECRET;
        let body = r#"{"event_type":"message.received"}"#;
        // 10 minutes ago — stale
        let stale_ts = (chrono::Utc::now().timestamp() - 600).to_string();

        // Even with correct signature, stale timestamp should fail
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let message = format!("{stale_ts}.{body}");
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(message.as_bytes());
        let signature = hex::encode(mac.finalize().into_bytes());

        assert!(
            !verify_linq_signature(secret, body, &stale_ts, &signature),
            "Stale timestamps (>300s) should be rejected"
        );
    }

    #[test]
    fn linq_signature_verification_accepts_sha256_prefix() {
        let secret = TEST_WEBHOOK_SECRET;
        let body = r#"{"event_type":"message.received"}"#;
        let now = chrono::Utc::now().timestamp().to_string();

        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let message = format!("{now}.{body}");
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(message.as_bytes());
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));

        assert!(verify_linq_signature(secret, body, &now, &signature));
    }

    #[test]
    fn linq_signature_verification_accepts_uppercase_hex() {
        let secret = TEST_WEBHOOK_SECRET;
        let body = r#"{"event_type":"message.received"}"#;
        let now = chrono::Utc::now().timestamp().to_string();

        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let message = format!("{now}.{body}");
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(message.as_bytes());
        let signature = hex::encode(mac.finalize().into_bytes()).to_ascii_uppercase();

        assert!(verify_linq_signature(secret, body, &now, &signature));
    }

    #[test]
    fn linq_parse_normalizes_phone_with_plus() {
        let ch = LinqChannel::new(
            "tok".into(),
            "+15551234567".into(),
            vec!["+1234567890".into()],
        );
        // API sends without +, normalize to +
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat_id": "chat-789",
                "from": "1234567890",
                "is_from_me": false,
                "message": {
                    "id": "msg-abc",
                    "parts": [{ "type": "text", "value": "Hi" }]
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "+1234567890");
    }

    #[test]
    fn linq_parse_missing_data() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "event_type": "message.received"
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn linq_parse_missing_message_parts() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat_id": "chat-789",
                "from": "+1234567890",
                "is_from_me": false,
                "message": {
                    "id": "msg-abc"
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn linq_parse_empty_text_value() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat_id": "chat-789",
                "from": "+1234567890",
                "is_from_me": false,
                "message": {
                    "id": "msg-abc",
                    "parts": [{ "type": "text", "value": "" }]
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "Empty text should be skipped");
    }

    #[test]
    fn linq_parse_fallback_reply_target_when_no_chat_id() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "from": "+1234567890",
                "is_from_me": false,
                "message": {
                    "id": "msg-abc",
                    "parts": [{ "type": "text", "value": "Hi" }]
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        // Falls back to sender phone number when no chat_id
        assert_eq!(msgs[0].reply_target, "+1234567890");
    }

    #[test]
    fn linq_phone_number_accessor() {
        let ch = make_channel();
        assert_eq!(ch.phone_number(), "+15551234567");
    }

    // ---- New format (2026-02-03) tests ----

    #[test]
    fn linq_parse_new_format_text_message() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "api_version": "v3",
            "webhook_version": "2026-02-03",
            "event_type": "message.received",
            "event_id": "evt-123",
            "created_at": "2026-03-01T12:00:00Z",
            "trace_id": "trace-456",
            "data": {
                "id": "msg-abc",
                "direction": "inbound",
                "sender_handle": {
                    "handle": "+1234567890",
                    "is_me": false
                },
                "chat": { "id": "chat-789" },
                "service": "iMessage",
                "parts": [{
                    "type": "text",
                    "value": "Hello from new format!"
                }]
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "+1234567890");
        assert_eq!(msgs[0].content, "Hello from new format!");
        assert_eq!(msgs[0].channel, "linq");
        assert_eq!(msgs[0].reply_target, "chat-789");
    }

    #[test]
    fn linq_parse_new_format_skip_is_me() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "webhook_version": "2026-02-03",
            "data": {
                "id": "msg-abc",
                "direction": "outbound",
                "sender_handle": {
                    "handle": "+15551234567",
                    "is_me": true
                },
                "chat": { "id": "chat-789" },
                "parts": [{ "type": "text", "value": "My own message" }]
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(
            msgs.is_empty(),
            "is_me messages should be skipped in new format"
        );
    }

    #[test]
    fn linq_parse_new_format_skip_outbound_direction() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "webhook_version": "2026-02-03",
            "data": {
                "id": "msg-abc",
                "direction": "outbound",
                "sender_handle": {
                    "handle": "+15551234567",
                    "is_me": false
                },
                "chat": { "id": "chat-789" },
                "parts": [{ "type": "text", "value": "Outbound" }]
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "outbound direction should be skipped");
    }

    #[test]
    fn linq_parse_new_format_unauthorized_sender() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "event_type": "message.received",
            "webhook_version": "2026-02-03",
            "data": {
                "id": "msg-abc",
                "direction": "inbound",
                "sender_handle": {
                    "handle": "+9999999999",
                    "is_me": false
                },
                "chat": { "id": "chat-789" },
                "parts": [{ "type": "text", "value": "Spam" }]
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(
            msgs.is_empty(),
            "Unauthorized senders should be filtered in new format"
        );
    }

    #[test]
    fn linq_parse_new_format_media_image() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "webhook_version": "2026-02-03",
            "data": {
                "id": "msg-abc",
                "direction": "inbound",
                "sender_handle": {
                    "handle": "+1234567890",
                    "is_me": false
                },
                "chat": { "id": "chat-789" },
                "parts": [{
                    "type": "media",
                    "url": "https://example.com/photo.png",
                    "mime_type": "image/png"
                }]
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "[IMAGE:https://example.com/photo.png]");
    }

    #[test]
    fn linq_parse_new_format_multiple_parts() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "webhook_version": "2026-02-03",
            "data": {
                "id": "msg-abc",
                "direction": "inbound",
                "sender_handle": {
                    "handle": "+1234567890",
                    "is_me": false
                },
                "chat": { "id": "chat-789" },
                "parts": [
                    { "type": "text", "value": "Check this out" },
                    { "type": "media", "url": "https://example.com/img.jpg", "mime_type": "image/jpeg" }
                ]
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(
            msgs[0].content,
            "Check this out\n[IMAGE:https://example.com/img.jpg]"
        );
    }

    #[test]
    fn linq_parse_new_format_fallback_reply_target_when_no_chat() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "webhook_version": "2026-02-03",
            "data": {
                "id": "msg-abc",
                "direction": "inbound",
                "sender_handle": {
                    "handle": "+1234567890",
                    "is_me": false
                },
                "parts": [{ "type": "text", "value": "Hi" }]
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].reply_target, "+1234567890");
    }

    #[test]
    fn linq_parse_new_format_normalizes_phone() {
        let ch = LinqChannel::new(
            "tok".into(),
            "+15551234567".into(),
            vec!["+1234567890".into()],
        );
        let payload = serde_json::json!({
            "event_type": "message.received",
            "webhook_version": "2026-02-03",
            "data": {
                "id": "msg-abc",
                "direction": "inbound",
                "sender_handle": {
                    "handle": "1234567890",
                    "is_me": false
                },
                "chat": { "id": "chat-789" },
                "parts": [{ "type": "text", "value": "Hi" }]
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "+1234567890");
    }

    // ---- Transcription tests ----

    #[test]
    fn linq_manager_none_when_transcription_not_configured() {
        let ch = make_channel();
        assert!(ch.transcription_manager.is_none());
    }

    #[test]
    fn linq_manager_some_when_valid_config() {
        let config = crate::config::TranscriptionConfig {
            enabled: true,
            default_provider: "local_whisper".into(),
            local_whisper: Some(crate::config::LocalWhisperConfig {
                url: "http://localhost:8001/v1/transcribe".into(),
                bearer_token: Some("test-token".to_string()),
                max_audio_bytes: 25 * 1024 * 1024,
                timeout_secs: 120,
            }),
            ..Default::default()
        };
        let ch = LinqChannel::new(
            "test-token".into(),
            "+15551234567".into(),
            vec!["+1234567890".into()],
        )
        .with_transcription(config);
        assert!(ch.transcription_manager.is_some());
    }

    #[test]
    fn linq_manager_none_and_warn_on_init_failure() {
        let config = crate::config::TranscriptionConfig {
            enabled: true,
            default_provider: "groq".into(),
            api_key: Some(String::new()),
            ..Default::default()
        };
        let ch = LinqChannel::new(
            "test-token".into(),
            "+15551234567".into(),
            vec!["+1234567890".into()],
        )
        .with_transcription(config);
        assert!(ch.transcription_manager.is_none());
    }

    #[test]
    fn linq_parse_audio_media_part_emits_placeholder() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat_id": "chat-789",
                "from": "+1234567890",
                "is_from_me": false,
                "message": {
                    "id": "msg-abc",
                    "parts": [{
                        "type": "media",
                        "url": "https://example.com/voice.mp3",
                        "mime_type": "audio/mpeg"
                    }]
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "[AUDIO:https://example.com/voice.mp3]");
    }

    #[test]
    fn linq_parse_audio_part_new_format() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "api_version": "v3",
            "webhook_version": "2026-02-03",
            "event_type": "message.received",
            "data": {
                "id": "msg-abc",
                "direction": "inbound",
                "sender_handle": {
                    "handle": "+1234567890",
                    "is_me": false
                },
                "chat": { "id": "chat-789" },
                "parts": [{
                    "type": "media",
                    "url": "https://example.com/voice.ogg",
                    "mime_type": "audio/ogg"
                }]
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "[AUDIO:https://example.com/voice.ogg]");
    }

    #[test]
    fn linq_parse_image_part_unchanged() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat_id": "chat-789",
                "from": "+1234567890",
                "is_from_me": false,
                "message": {
                    "id": "msg-abc",
                    "parts": [{
                        "type": "media",
                        "url": "https://example.com/photo.jpg",
                        "mime_type": "image/jpeg"
                    }]
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "[IMAGE:https://example.com/photo.jpg]");
    }

    #[test]
    fn linq_parse_non_image_non_audio_media_still_skipped() {
        let ch = LinqChannel::new("tok".into(), "+15551234567".into(), vec!["*".into()]);
        let payload = serde_json::json!({
            "event_type": "message.received",
            "data": {
                "chat_id": "chat-789",
                "from": "+1234567890",
                "is_from_me": false,
                "message": {
                    "id": "msg-abc",
                    "parts": [{
                        "type": "media",
                        "url": "https://example.com/video.mp4",
                        "mime_type": "video/mp4"
                    }]
                }
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn linq_try_transcribe_audio_part_returns_none_when_manager_absent() {
        let ch = make_channel();
        let result = ch
            .try_transcribe_audio_part("https://example.com/voice.mp3")
            .await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn linq_try_transcribe_audio_part_downloads_and_transcribes() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };

        let mock_media = MockServer::start().await;
        let mock_transcription = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/voice.mp3"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(vec![0u8; 100])
                    .insert_header("Content-Type", "audio/mpeg"),
            )
            .mount(&mock_media)
            .await;

        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "text": "test transcript"
            })))
            .mount(&mock_transcription)
            .await;

        let config = crate::config::TranscriptionConfig {
            enabled: true,
            default_provider: "groq".into(),
            api_url: format!("{}/v1/audio/transcriptions", mock_transcription.uri()),
            model: "whisper-large-v3".into(),
            api_key: Some("test-key".into()),
            language: None,
            initial_prompt: None,
            max_duration_secs: 300,
            openai: None,
            deepgram: None,
            assemblyai: None,
            google: None,
            local_whisper: None,
            transcribe_non_ptt_audio: false,
        };

        let ch = LinqChannel::new(
            "test-token".into(),
            "+15551234567".into(),
            vec!["+1234567890".into()],
        )
        .with_transcription(config);

        let url = format!("{}/voice.mp3", mock_media.uri());
        let result = ch.try_transcribe_audio_part(&url).await;

        assert_eq!(result, Some("test transcript".to_string()));
    }

    #[tokio::test]
    async fn linq_try_transcribe_audio_part_returns_none_on_download_error() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let config = crate::config::TranscriptionConfig {
            enabled: false,
            ..Default::default()
        };

        let ch = LinqChannel::new(
            "test-token".into(),
            "+15551234567".into(),
            vec!["+1234567890".into()],
        )
        .with_transcription(config);

        let url = format!("{}/voice.mp3", mock_server.uri());
        let result = ch.try_transcribe_audio_part(&url).await;

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn linq_try_transcribe_audio_part_derives_extension_from_url_when_no_content_type() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };

        let mock_media = MockServer::start().await;
        let mock_transcription = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/voice.ogg"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(vec![0u8; 100])
                    .insert_header("Content-Type", "application/octet-stream"),
            )
            .mount(&mock_media)
            .await;

        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "text": "url extension transcript"
            })))
            .mount(&mock_transcription)
            .await;

        let config = crate::config::TranscriptionConfig {
            enabled: true,
            default_provider: "groq".into(),
            api_url: format!("{}/v1/audio/transcriptions", mock_transcription.uri()),
            model: "whisper-large-v3".into(),
            api_key: Some("test-key".into()),
            language: None,
            initial_prompt: None,
            max_duration_secs: 300,
            openai: None,
            deepgram: None,
            assemblyai: None,
            google: None,
            local_whisper: None,
            transcribe_non_ptt_audio: false,
        };

        let ch = LinqChannel::new(
            "test-token".into(),
            "+15551234567".into(),
            vec!["+1234567890".into()],
        )
        .with_transcription(config);

        let url = format!("{}/voice.ogg", mock_media.uri());
        let result = ch.try_transcribe_audio_part(&url).await;

        assert_eq!(result, Some("url extension transcript".to_string()));
    }

    #[tokio::test]
    async fn linq_try_transcribe_audio_part_returns_none_when_extension_cannot_be_derived() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(vec![0u8; 100])
                    .insert_header("Content-Type", "application/octet-stream"),
            )
            .mount(&mock_server)
            .await;

        let config = crate::config::TranscriptionConfig {
            enabled: false,
            ..Default::default()
        };

        let ch = LinqChannel::new(
            "test-token".into(),
            "+15551234567".into(),
            vec!["+1234567890".into()],
        )
        .with_transcription(config);

        let url = format!("{}/unknownfile", mock_server.uri());
        let result = ch.try_transcribe_audio_part(&url).await;

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn linq_audio_routes_through_local_whisper() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };

        let mock_media = MockServer::start().await;
        let mock_whisper = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/voice.mp3"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(vec![0u8; 100])
                    .insert_header("Content-Type", "audio/mpeg"),
            )
            .mount(&mock_media)
            .await;

        Mock::given(method("POST"))
            .and(path("/transcribe"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "text": "local whisper transcript"
            })))
            .mount(&mock_whisper)
            .await;

        let config = crate::config::TranscriptionConfig {
            enabled: true,
            default_provider: "local_whisper".into(),
            api_url: String::new(),
            model: String::new(),
            api_key: None,
            language: None,
            initial_prompt: None,
            max_duration_secs: 300,
            openai: None,
            deepgram: None,
            assemblyai: None,
            google: None,
            local_whisper: Some(crate::config::LocalWhisperConfig {
                url: format!("{}/transcribe", mock_whisper.uri()),
                bearer_token: Some("test-token".to_string()),
                max_audio_bytes: 25 * 1024 * 1024,
                timeout_secs: 120,
            }),
            transcribe_non_ptt_audio: false,
        };

        let ch = LinqChannel::new(
            "test-token".into(),
            "+15551234567".into(),
            vec!["+1234567890".into()],
        )
        .with_transcription(config);

        let url = format!("{}/voice.mp3", mock_media.uri());
        let result = ch.try_transcribe_audio_part(&url).await;

        assert_eq!(result, Some("local whisper transcript".to_string()));
    }

    #[test]
    fn is_private_url_blocks_localhost() {
        assert!(is_private_url("http://localhost:8080/audio.mp3"));
        assert!(is_private_url("http://127.0.0.1:9000/voice.ogg"));
        assert!(is_private_url("http://[::1]:3000/file.wav"));
    }

    #[test]
    fn is_private_url_blocks_private_ranges() {
        assert!(is_private_url("http://10.0.0.1/audio.mp3"));
        assert!(is_private_url("http://192.168.1.1/audio.mp3"));
        assert!(is_private_url("http://172.16.0.1/audio.mp3"));
        assert!(is_private_url("http://172.31.255.1/audio.mp3"));
        assert!(is_private_url("http://169.254.1.1/audio.mp3"));
    }

    #[test]
    fn is_private_url_blocks_internal_hostnames() {
        assert!(is_private_url("http://host.local/audio.mp3"));
        assert!(is_private_url("http://svc.internal/audio.mp3"));
        assert!(is_private_url("http://metadata.google.internal/audio.mp3"));
    }

    #[test]
    fn is_private_url_allows_public_urls() {
        assert!(!is_private_url("https://cdn.example.com/audio.mp3"));
        assert!(!is_private_url("https://api.linq.ai/media/voice.ogg"));
    }

    #[test]
    fn is_private_url_blocks_subdomain_bypass() {
        // A hostname like 169.254.169.254.evil.com must NOT bypass the guard
        assert!(!is_private_url("http://169.254.169.254.evil.com/audio.mp3"));
        // But the actual link-local IP must be blocked
        assert!(is_private_url("http://169.254.169.254/audio.mp3"));
    }

    #[test]
    fn is_private_url_blocks_unparseable() {
        assert!(is_private_url("not-a-url"));
    }
}
