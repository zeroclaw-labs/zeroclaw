use super::traits::{Channel, ChannelMessage, SendMessage};
use crate::config::schema::{BlueBubblesDmPolicy, BlueBubblesGroupPolicy};
use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use uuid::Uuid;

const FROM_ME_CACHE_MAX: usize = 500;

/// Audio attachment metadata extracted from a BlueBubbles webhook payload.
struct AudioAttachment {
    /// BlueBubbles attachment GUID, used to download via the BB REST API.
    ///
    /// Note: BB attachment GUIDs (e.g. `p:0/UUID`) are BB-internal identifiers
    /// distinct from iMessage's `attachment.guid` column in `chat.db`.
    guid: String,
    /// Original filename from the BB webhook (e.g. `"Audio Message.caf"`).
    /// Used to derive the file extension when writing to a temp file and when
    /// calling the Groq Whisper API (which validates the filename extension).
    transfer_name: String,
}

/// Maps short effect names to full Apple `effectId` strings for BB Private API.
const EFFECT_MAP: &[(&str, &str)] = &[
    // Bubble effects
    ("slam", "com.apple.MobileSMS.expressivesend.impact"),
    ("loud", "com.apple.MobileSMS.expressivesend.loud"),
    ("gentle", "com.apple.MobileSMS.expressivesend.gentle"),
    (
        "invisible-ink",
        "com.apple.MobileSMS.expressivesend.invisibleink",
    ),
    (
        "invisible_ink",
        "com.apple.MobileSMS.expressivesend.invisibleink",
    ),
    (
        "invisibleink",
        "com.apple.MobileSMS.expressivesend.invisibleink",
    ),
    // Screen effects
    ("echo", "com.apple.messages.effect.CKEchoEffect"),
    ("spotlight", "com.apple.messages.effect.CKSpotlightEffect"),
    (
        "balloons",
        "com.apple.messages.effect.CKHappyBirthdayEffect",
    ),
    ("confetti", "com.apple.messages.effect.CKConfettiEffect"),
    ("love", "com.apple.messages.effect.CKHeartEffect"),
    ("heart", "com.apple.messages.effect.CKHeartEffect"),
    ("hearts", "com.apple.messages.effect.CKHeartEffect"),
    ("lasers", "com.apple.messages.effect.CKLasersEffect"),
    ("fireworks", "com.apple.messages.effect.CKFireworksEffect"),
    ("celebration", "com.apple.messages.effect.CKSparklesEffect"),
];

/// Extract and resolve a `[EFFECT:name]` tag from the end of a message string.
///
/// Returns `(cleaned_text, Option<effect_id>)`. The `[EFFECT:…]` tag is stripped
/// from the text regardless of whether the name resolves to a known effect ID.
fn extract_effect(text: &str) -> (String, Option<String>) {
    // Scan from end for the last [EFFECT:...] token
    let trimmed = text.trim_end();
    if let Some(start) = trimmed.rfind("[EFFECT:") {
        let rest = &trimmed[start..];
        if let Some(end) = rest.find(']') {
            let tag_content = &rest[8..end]; // skip "[EFFECT:"
            let cleaned = format!("{}{}", &trimmed[..start], &trimmed[start + end + 1..]);
            let cleaned = cleaned.trim_end().to_string();
            let name = tag_content.trim().to_lowercase();
            let effect_id = EFFECT_MAP
                .iter()
                .find(|(k, _)| *k == name.as_str())
                .map(|(_, v)| v.to_string())
                .or_else(|| {
                    // Pass through full Apple effect IDs directly
                    if name.starts_with("com.apple.") {
                        Some(tag_content.trim().to_string())
                    } else {
                        None
                    }
                });
            return (cleaned, effect_id);
        }
    }
    (text.to_string(), None)
}

/// A cached `fromMe` message — kept so reply context can be resolved when
/// the other party replies to something the bot sent.
struct FromMeCacheEntry {
    chat_guid: String,
    body: String,
}

/// Interior-mutable FIFO cache for `fromMe` messages.
/// Uses a `VecDeque<String>` to track insertion order for correct eviction,
/// and a `HashMap` for O(1) lookup.
struct FromMeCache {
    map: HashMap<String, FromMeCacheEntry>,
    order: VecDeque<String>,
}

impl FromMeCache {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn insert(&mut self, id: String, entry: FromMeCacheEntry) {
        if self.map.len() >= FROM_ME_CACHE_MAX {
            if let Some(oldest) = self.order.pop_front() {
                self.map.remove(&oldest);
            }
        }
        self.order.push_back(id.clone());
        self.map.insert(id, entry);
    }

    fn get_body(&self, id: &str) -> Option<&str> {
        self.map.get(id).map(|e| e.body.as_str())
    }
}

/// BlueBubbles channel — uses the BlueBubbles REST API to send and receive
/// iMessages via a locally-running BlueBubbles server on macOS.
///
/// This channel operates in webhook mode (push-based) rather than polling.
/// Messages are received via the gateway's `/bluebubbles` webhook endpoint.
/// The `listen` method is a keepalive placeholder; actual message handling
/// happens in the gateway when BlueBubbles POSTs webhook events.
///
/// BlueBubbles server must be configured to send webhooks to:
///   `https://<your-zeroclaw-host>/bluebubbles`
///
/// Authentication: BlueBubbles uses `?password=<password>` as a query
/// parameter on every API call (not an Authorization header).
pub struct BlueBubblesChannel {
    server_url: String,
    password: String,
    allowed_senders: Vec<String>,
    pub ignore_senders: Vec<String>,
    /// Access policy for direct (1:1) messages.
    dm_policy: BlueBubblesDmPolicy,
    /// Access policy for group chats.
    group_policy: BlueBubblesGroupPolicy,
    /// Chat GUIDs allowed when `group_policy = Allowlist`. Use `["*"]` to allow all groups.
    group_allow_from: Vec<String>,
    /// When true, POST a read receipt to BB after processing each inbound message.
    send_read_receipts: bool,
    client: reqwest::Client,
    /// Cache of recent `fromMe` messages keyed by message GUID.
    /// Used to inject reply context when the user replies to a bot message.
    from_me_cache: Mutex<FromMeCache>,
    /// Per-recipient background tasks that periodically refresh typing indicators.
    /// BB typing indicators expire in ~5 s; tasks refresh every 4 s.
    /// Keyed by chat GUID so concurrent conversations don't cancel each other.
    typing_handles: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    /// Optional Groq Whisper transcription config — populated by `with_transcription`
    /// when `[transcription]` is configured and enabled.
    transcription: Option<crate::config::TranscriptionConfig>,
}

impl BlueBubblesChannel {
    pub fn new(
        server_url: String,
        password: String,
        allowed_senders: Vec<String>,
        ignore_senders: Vec<String>,
    ) -> anyhow::Result<Self> {
        let client = reqwest::ClientBuilder::new()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build BlueBubbles HTTP client: {e}"))?;
        Ok(Self {
            server_url: server_url.trim_end_matches('/').to_string(),
            password,
            allowed_senders,
            ignore_senders,
            dm_policy: BlueBubblesDmPolicy::Open,
            group_policy: BlueBubblesGroupPolicy::Open,
            group_allow_from: Vec::new(),
            send_read_receipts: true,
            client,
            from_me_cache: Mutex::new(FromMeCache::new()),
            typing_handles: Mutex::new(HashMap::new()),
            transcription: None,
        })
    }

    /// Configure DM and group policies.
    pub(crate) fn with_policies(
        mut self,
        dm_policy: BlueBubblesDmPolicy,
        group_policy: BlueBubblesGroupPolicy,
        group_allow_from: Vec<String>,
        send_read_receipts: bool,
    ) -> Self {
        self.dm_policy = dm_policy;
        self.group_policy = group_policy;
        self.group_allow_from = group_allow_from;
        self.send_read_receipts = send_read_receipts;
        self
    }

    /// Configure voice transcription via Groq Whisper.
    ///
    /// When `config.enabled` is false the builder is a no-op so callers can
    /// pass `config.transcription.clone()` unconditionally.
    pub fn with_transcription(mut self, config: crate::config::TranscriptionConfig) -> Self {
        if config.enabled {
            self.transcription = Some(config);
        }
        self
    }

    /// Check if a sender address is in the ignore list.
    ///
    /// Ignored senders are silently dropped before allowlist evaluation.
    fn is_sender_ignored(&self, sender: &str) -> bool {
        self.ignore_senders
            .iter()
            .any(|s| s == "*" || s.eq_ignore_ascii_case(sender))
    }

    /// Return true if the chat GUID identifies a group chat.
    ///
    /// BB group GUIDs contain `;+;`; DM GUIDs contain `;-;`.
    /// When the GUID is absent (empty string) we treat the message as a DM.
    fn is_group_chat(chat_guid: &str) -> bool {
        chat_guid.contains(";+;")
    }

    /// Check if a DM from `sender` is allowed under `dm_policy`.
    ///
    /// - `Disabled` → always deny.
    /// - `Allowlist` → `allowed_senders` must be non-empty and match.
    /// - `Open` → apply the legacy `allowed_senders` check (empty = allow all).
    fn is_dm_allowed(&self, sender: &str) -> bool {
        match self.dm_policy {
            BlueBubblesDmPolicy::Disabled => false,
            BlueBubblesDmPolicy::Allowlist => {
                if self.allowed_senders.is_empty() {
                    return false;
                }
                self.allowed_senders
                    .iter()
                    .any(|a| a == "*" || a.eq_ignore_ascii_case(sender))
            }
            BlueBubblesDmPolicy::Open => {
                // Preserve legacy behaviour: empty list = allow all.
                if self.allowed_senders.is_empty() {
                    return true;
                }
                self.allowed_senders
                    .iter()
                    .any(|a| a == "*" || a.eq_ignore_ascii_case(sender))
            }
        }
    }

    /// Check if a group message from `chat_guid` / `sender` is allowed under `group_policy`.
    ///
    /// - `Disabled` → always deny.
    /// - `Allowlist` → `group_allow_from` must contain the chat GUID.
    /// - `Open` → allow all groups, but still apply `allowed_senders` as a
    ///   per-sender gate when configured, preserving backward-compat for
    ///   deployments that relied on sender gating before policy fields were added.
    fn is_group_allowed(&self, chat_guid: &str, sender: &str) -> bool {
        match self.group_policy {
            BlueBubblesGroupPolicy::Disabled => false,
            BlueBubblesGroupPolicy::Allowlist => self
                .group_allow_from
                .iter()
                .any(|g| g == "*" || g.eq_ignore_ascii_case(chat_guid)),
            BlueBubblesGroupPolicy::Open => {
                if self.allowed_senders.is_empty() {
                    true
                } else {
                    self.allowed_senders
                        .iter()
                        .any(|a| a == "*" || a.eq_ignore_ascii_case(sender))
                }
            }
        }
    }

    /// Check if a sender address is allowed (legacy helper used by tapback path).
    ///
    /// Matches OpenClaw behaviour: empty list → allow all (no allowlist
    /// configured means "open"). Use `"*"` for explicit wildcard.
    fn is_sender_allowed(&self, sender: &str) -> bool {
        if self.allowed_senders.is_empty() {
            return true;
        }
        self.allowed_senders
            .iter()
            .any(|a| a == "*" || a.eq_ignore_ascii_case(sender))
    }

    /// Mark a chat as read on the BB server (fire-and-forget).
    ///
    /// No-op when `send_read_receipts = false`. Errors are logged but not
    /// propagated — a failed read-receipt must never prevent the reply from
    /// sending.
    pub(crate) async fn mark_read(&self, chat_guid: &str) {
        if !self.send_read_receipts {
            return;
        }
        // BB's `/read` path lookup requires an explicit service prefix.  When the
        // webhook payload reports a group as "any;+;…" (service unknown), normalise
        // it to "iMessage;+;…" so the Private API can send the actual receipt.
        let normalised: std::borrow::Cow<str> = if chat_guid.starts_with("any;") {
            std::borrow::Cow::Owned(chat_guid.replacen("any;", "iMessage;", 1))
        } else {
            std::borrow::Cow::Borrowed(chat_guid)
        };
        let encoded = urlencoding::encode(normalised.as_ref()).into_owned();
        let url = self.api_url(&format!("/api/v1/chat/{encoded}/read"));
        match self
            .client
            .post(&url)
            .query(&[("password", &self.password)])
            .send()
            .await
        {
            Err(e) => tracing::warn!("BlueBubbles mark_read failed: {}", e.without_url()),
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    tracing::info!("BlueBubbles mark_read ok status={status}");
                } else {
                    tracing::warn!("BlueBubbles mark_read got {status}");
                }
            }
        }
    }

    /// Build a full API URL for the given endpoint path.
    fn api_url(&self, path: &str) -> String {
        format!("{}{path}", self.server_url)
    }

    /// Normalize a BlueBubbles handle, matching OpenClaw's `normalizeBlueBubblesHandle`:
    /// - Strip service prefixes: `imessage:`, `sms:`, `auto:`
    /// - Email addresses → lowercase
    /// - Phone numbers → strip internal whitespace only
    fn normalize_handle(raw: &str) -> String {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        let lower = trimmed.to_ascii_lowercase();
        let stripped = if lower.starts_with("imessage:") {
            &trimmed[9..]
        } else if lower.starts_with("sms:") {
            &trimmed[4..]
        } else if lower.starts_with("auto:") {
            &trimmed[5..]
        } else {
            trimmed
        };
        // Recurse if another prefix is still present
        let stripped_lower = stripped.to_ascii_lowercase();
        if stripped_lower.starts_with("imessage:")
            || stripped_lower.starts_with("sms:")
            || stripped_lower.starts_with("auto:")
        {
            return Self::normalize_handle(stripped);
        }
        if stripped.contains('@') {
            stripped.to_ascii_lowercase()
        } else {
            stripped.chars().filter(|c| !c.is_whitespace()).collect()
        }
    }

    /// Extract sender from multiple possible locations in the payload `data`
    /// object, matching OpenClaw's fallback chain.
    fn extract_sender(data: &serde_json::Value) -> Option<String> {
        // handle / sender nested object
        let handle = data.get("handle").or_else(|| data.get("sender"));
        if let Some(h) = handle {
            for key in &["address", "handle", "id"] {
                if let Some(addr) = h.get(key).and_then(|v| v.as_str()) {
                    let normalized = Self::normalize_handle(addr);
                    if !normalized.is_empty() {
                        return Some(normalized);
                    }
                }
            }
        }
        // Top-level fallbacks
        for key in &["senderId", "sender", "from"] {
            if let Some(v) = data.get(key).and_then(|v| v.as_str()) {
                let normalized = Self::normalize_handle(v);
                if !normalized.is_empty() {
                    return Some(normalized);
                }
            }
        }
        None
    }

    /// Extract the chat GUID from multiple possible locations in the `data`
    /// object. Preference order matches OpenClaw: direct fields, nested chat,
    /// then chats array.
    fn extract_chat_guid(data: &serde_json::Value) -> Option<String> {
        // Direct fields
        for key in &["chatGuid", "chat_guid"] {
            if let Some(g) = data.get(key).and_then(|v| v.as_str()) {
                let t = g.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
        // Nested chat/conversation object
        if let Some(chat) = data.get("chat").or_else(|| data.get("conversation")) {
            for key in &["chatGuid", "chat_guid", "guid"] {
                if let Some(g) = chat.get(key).and_then(|v| v.as_str()) {
                    let t = g.trim();
                    if !t.is_empty() {
                        return Some(t.to_string());
                    }
                }
            }
        }
        // chats array (BB webhook format)
        if let Some(arr) = data.get("chats").and_then(|c| c.as_array()) {
            if let Some(first) = arr.first() {
                for key in &["chatGuid", "chat_guid", "guid"] {
                    if let Some(g) = first.get(key).and_then(|v| v.as_str()) {
                        let t = g.trim();
                        if !t.is_empty() {
                            return Some(t.to_string());
                        }
                    }
                }
            }
        }
        None
    }

    /// Extract the message GUID/ID from the `data` object.
    fn extract_message_id(data: &serde_json::Value) -> Option<String> {
        for key in &["guid", "id", "messageId"] {
            if let Some(v) = data.get(key).and_then(|v| v.as_str()) {
                let t = v.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
        None
    }

    /// Normalize a BB timestamp: values > 1e12 are milliseconds → convert to
    /// seconds. Values ≤ 1e12 are already seconds.
    fn normalize_timestamp(raw: u64) -> u64 {
        if raw > 1_000_000_000_000 {
            raw / 1000
        } else {
            raw
        }
    }

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn extract_timestamp(data: &serde_json::Value) -> u64 {
        data.get("dateCreated")
            .or_else(|| data.get("date"))
            .or_else(|| data.get("timestamp"))
            .and_then(|t| t.as_u64())
            .map(Self::normalize_timestamp)
            .unwrap_or_else(Self::now_secs)
    }

    /// Cache a `fromMe` message for later reply-context resolution.
    fn cache_from_me(&self, message_id: &str, chat_guid: &str, body: &str) {
        if message_id.is_empty() {
            return;
        }
        self.from_me_cache.lock().insert(
            message_id.to_string(),
            FromMeCacheEntry {
                chat_guid: chat_guid.to_string(),
                body: body.to_string(),
            },
        );
    }

    /// Look up the body of a cached `fromMe` message by its GUID.
    /// Used to inject reply context when a user replies to a bot message.
    pub fn lookup_reply_context(&self, message_id: &str) -> Option<String> {
        self.from_me_cache
            .lock()
            .get_body(message_id)
            .map(|s| s.to_string())
    }

    /// Build the text content and attachment placeholder from a BB `data`
    /// object. Matches OpenClaw's `buildAttachmentPlaceholder` format:
    ///   `<media:image> (1 image)`, `<media:video> (2 videos)`, etc.
    fn extract_content(data: &serde_json::Value) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();

        // Text field (try several names)
        for key in &["text", "body", "subject"] {
            if let Some(text) = data.get(key).and_then(|t| t.as_str()) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    parts.push(trimmed.to_string());
                    break;
                }
            }
        }

        // Attachment placeholder
        if let Some(attachments) = data.get("attachments").and_then(|a| a.as_array()) {
            if !attachments.is_empty() {
                let mime_types: Vec<&str> = attachments
                    .iter()
                    .filter_map(|att| {
                        att.get("mimeType")
                            .or_else(|| att.get("mime_type"))
                            .and_then(|m| m.as_str())
                    })
                    .collect();

                let all_images =
                    !mime_types.is_empty() && mime_types.iter().all(|m| m.starts_with("image/"));
                let all_videos =
                    !mime_types.is_empty() && mime_types.iter().all(|m| m.starts_with("video/"));
                let all_audio =
                    !mime_types.is_empty() && mime_types.iter().all(|m| m.starts_with("audio/"));

                let (tag, label) = if all_images {
                    ("<media:image>", "image")
                } else if all_videos {
                    ("<media:video>", "video")
                } else if all_audio {
                    ("<media:audio>", "audio")
                } else {
                    ("<media:attachment>", "file")
                };

                let count = attachments.len();
                let suffix = if count == 1 {
                    label.to_string()
                } else {
                    format!("{label}s")
                };
                parts.push(format!("{tag} ({count} {suffix})"));
            }
        }

        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n"))
        }
    }

    /// Return the first audio `AudioAttachment` from a BB webhook `data` object.
    ///
    /// Supports both camelCase (`mimeType`, `transferName`) and snake_case
    /// (`mime_type`, `filename`) field names for forward compatibility.
    fn extract_audio_attachment(data: &serde_json::Value) -> Option<AudioAttachment> {
        let arr = data.get("attachments").and_then(|a| a.as_array())?;
        arr.iter().find_map(|att| {
            let mime = att
                .get("mimeType")
                .or_else(|| att.get("mime_type"))
                .and_then(|m| m.as_str())
                .unwrap_or("");
            if !mime.starts_with("audio/") {
                return None;
            }
            // BB attachment GUID is required to download via the REST API.
            let guid = att.get("guid").and_then(|g| g.as_str())?.to_string();
            let transfer_name = att
                .get("transferName")
                .or_else(|| att.get("filename"))
                .and_then(|f| f.as_str())
                .unwrap_or("voice.m4a")
                .to_string();
            Some(AudioAttachment {
                guid,
                transfer_name,
            })
        })
    }

    /// Download raw audio bytes for an attachment from the BlueBubbles server.
    ///
    /// Uses `GET /api/v1/attachment/{guid}/download`. The attachment GUID is
    /// URL-encoded so `p:0/UUID`-style identifiers work as URL path segments.
    ///
    /// Enforces a 25 MB cap (matching the Groq Whisper API limit) via the
    /// `Content-Length` header (before streaming) and by accumulating chunks
    /// and bailing as soon as the running total exceeds the limit, preventing
    /// memory exhaustion even when the server omits `Content-Length`.
    async fn download_attachment_bytes(&self, attachment_guid: &str) -> anyhow::Result<Vec<u8>> {
        /// 25 MB — matches `transcription::MAX_AUDIO_BYTES`.
        const MAX_BYTES: usize = 25 * 1024 * 1024;

        let encoded = urlencoding::encode(attachment_guid);
        let url = self.api_url(&format!("/api/v1/attachment/{encoded}/download"));
        let resp = self
            .client
            .get(&url)
            .query(&[("password", &self.password)])
            .send()
            .await
            .map_err(|e| {
                anyhow::anyhow!("BB attachment download request failed: {}", e.without_url())
            })?;
        if !resp.status().is_success() {
            anyhow::bail!("BlueBubbles attachment download failed ({})", resp.status());
        }
        // Reject before buffering when Content-Length already exceeds the cap.
        if let Some(len) = resp.content_length() {
            if usize::try_from(len).unwrap_or(usize::MAX) > MAX_BYTES {
                anyhow::bail!("BB attachment too large ({len} bytes, max {MAX_BYTES})");
            }
        }
        // Stream in chunks so the size cap is enforced before the full body
        // is buffered in memory (guards against servers that omit Content-Length).
        let mut resp = resp;
        let mut buf = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| anyhow::anyhow!("BB attachment body read failed: {}", e.without_url()))?
        {
            buf.extend_from_slice(&chunk);
            if buf.len() > MAX_BYTES {
                anyhow::bail!("BB attachment too large (>{MAX_BYTES} bytes)");
            }
        }
        Ok(buf)
    }

    /// Transcribe an audio attachment received via the BlueBubbles webhook.
    ///
    /// Downloads the raw audio bytes via the BB REST API, writes them to a
    /// temporary file, and transcribes using the local `whisper` CLI. Falls
    /// back to the Groq Whisper API when whisper is not installed and an API
    /// key is configured via `[transcription]`.
    ///
    /// Returns `Ok(None)` when the transcript is empty (e.g. silence).
    async fn transcribe_local(&self, att: &AudioAttachment) -> anyhow::Result<Option<String>> {
        let bytes = self.download_attachment_bytes(&att.guid).await?;

        // Derive the file extension from the transfer name; default to m4a.
        let ext = std::path::Path::new(&att.transfer_name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("m4a");

        // Local whisper handles CAF (iMessage voice memos) and any format
        // supported by ffmpeg — no pre-conversion step needed.
        if super::transcription::whisper_available() {
            let tmp_path =
                std::env::temp_dir().join(format!("zc_bb_audio_{}.{ext}", uuid::Uuid::new_v4()));
            // Write with owner-only permissions to protect audio content on shared hosts.
            {
                use tokio::io::AsyncWriteExt as _;
                let mut f = {
                    #[cfg(unix)]
                    {
                        tokio::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .mode(0o600)
                            .open(&tmp_path)
                            .await
                            .map_err(|e| anyhow::anyhow!("Failed to create temp audio file: {e}"))?
                    }
                    #[cfg(not(unix))]
                    {
                        tokio::fs::File::create(&tmp_path)
                            .await
                            .map_err(|e| anyhow::anyhow!("Failed to create temp audio file: {e}"))?
                    }
                };
                f.write_all(&bytes).await?;
            }
            let result =
                super::transcription::transcribe_audio_local(tmp_path.to_str().ok_or_else(
                    || anyhow::anyhow!("Temp audio path contains non-UTF-8 characters"),
                )?)
                .await;
            // Clean up temp file regardless of result.
            tokio::fs::remove_file(&tmp_path).await.ok();
            let transcript = result?;
            if transcript.trim().is_empty() {
                return Ok(None);
            }
            return Ok(Some(transcript));
        }

        // Groq API fallback: BB typically serves converted MP3 for iMessage
        // voice memos, but raw CAF files are rejected by the Groq API. Give a
        // clear error rather than the generic "Unsupported audio format" message.
        if let Some(ref config) = self.transcription {
            if std::path::Path::new(&att.transfer_name)
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("caf"))
                .unwrap_or(false)
            {
                anyhow::bail!(
                    "CAF audio is not supported by the Groq API fallback — \
                     install openai-whisper (`pip install openai-whisper`) for local transcription"
                );
            }
            let transcript =
                super::transcription::transcribe_audio(bytes, &att.transfer_name, config).await?;
            if transcript.trim().is_empty() {
                return Ok(None);
            }
            return Ok(Some(transcript));
        }

        anyhow::bail!(
            "No transcription backend available: install openai-whisper \
             (`pip install openai-whisper`) or configure [transcription].api_key"
        )
    }

    /// Like `parse_webhook_payload` but transcribes audio attachments when
    /// `[transcription]` is configured. Gracefully falls back to the
    /// `<media:audio>` placeholder if transcription is disabled, whisper is
    /// absent, or transcription fails.
    pub async fn parse_webhook_payload_with_transcription(
        &self,
        payload: &serde_json::Value,
    ) -> Vec<ChannelMessage> {
        // Local whisper needs no config — allow transcription whenever
        // whisper-cli is installed, even if [transcription] is not configured.
        if self.transcription.is_none() && !super::transcription::whisper_available() {
            return self.parse_webhook_payload(payload);
        }

        let event_type = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let Some(data) = payload.get("data") else {
            return self.parse_webhook_payload(payload);
        };

        if event_type != "new-message" {
            return self.parse_webhook_payload(payload);
        }

        let Some(att) = Self::extract_audio_attachment(data) else {
            return self.parse_webhook_payload(payload);
        };

        // Gate typing on the same prechecks parse_webhook_payload uses, so
        // fromMe / ignored / disallowed senders never trigger side-effects.
        let is_from_me = data
            .get("isFromMe")
            .or_else(|| data.get("is_from_me"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if is_from_me {
            return self.parse_webhook_payload(payload);
        }
        let Some(sender) = Self::extract_sender(data) else {
            return self.parse_webhook_payload(payload);
        };
        let chat_guid_for_policy = Self::extract_chat_guid(data)
            .filter(|g| !g.is_empty())
            .unwrap_or_default();
        let allowed = if Self::is_group_chat(&chat_guid_for_policy) {
            self.is_group_allowed(&chat_guid_for_policy, &sender)
        } else {
            self.is_dm_allowed(&sender)
        };
        if self.is_sender_ignored(&sender) || !allowed {
            return self.parse_webhook_payload(payload);
        }

        // Start typing indicator before whisper runs — transcription can take >30 s
        // and without this the user sees no feedback during the slow CPU phase.
        // Capture guid once so we can stop typing on all exit paths.
        let typing_guid = Self::extract_chat_guid(data).filter(|g| !g.is_empty());
        if let Some(guid) = typing_guid.as_deref() {
            let _ = self.start_typing(guid).await;
        }

        let result = match self.transcribe_local(&att).await {
            Ok(Some(transcript)) => {
                // Inject transcript as text; clear attachments so parse_webhook_payload
                // skips the <media:audio> placeholder
                let mut enriched = data.clone();
                if let Some(obj) = enriched.as_object_mut() {
                    let existing = obj
                        .get("text")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    let new_text = if existing.is_empty() {
                        format!("[Voice] {transcript}")
                    } else {
                        format!("{existing}\n[Voice] {transcript}")
                    };
                    obj.insert("text".into(), serde_json::Value::String(new_text));
                    // Remove only audio attachments; preserve non-audio (images, files)
                    // so parse_webhook_payload retains their [IMAGE:…] / placeholder context.
                    if let Some(atts) = obj.get("attachments").and_then(|a| a.as_array()) {
                        let non_audio: Vec<serde_json::Value> = atts
                            .iter()
                            .filter(|a| {
                                let mime = a
                                    .get("mimeType")
                                    .or_else(|| a.get("mime_type"))
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("");
                                !mime.starts_with("audio/")
                            })
                            .cloned()
                            .collect();
                        obj.insert("attachments".into(), serde_json::Value::Array(non_audio));
                    }
                }
                let enriched_payload = serde_json::json!({"type": "new-message", "data": enriched});
                self.parse_webhook_payload(&enriched_payload)
            }
            Ok(None) => self.parse_webhook_payload(payload),
            Err(e) => {
                tracing::warn!("BB audio transcription failed: {e}");
                self.parse_webhook_payload(payload)
            }
        };

        // Stop typing on all transcription exit paths (success, None, or error).
        if let Some(guid) = typing_guid.as_deref() {
            let _ = self.stop_typing(guid).await;
        }

        result
    }

    /// Parse a `type: "updated-message"` webhook for tapback reactions.
    ///
    /// BlueBubbles sends these when a user adds a tapback (love, like, dislike,
    /// laugh, emphasize, question) to any message. The `associatedMessageType`
    /// field indicates the reaction:
    ///   - 2000–2005 = tapback additions (love / like / dislike / laugh / emphasize / question)
    ///   - 3000–3005 = tapback removals (silently ignored)
    ///
    /// Tapbacks from the bot itself are discarded. Tapbacks on bot messages
    /// include a quoted excerpt of the original body when it is in the reply cache.
    fn parse_tapback_event(&self, data: &serde_json::Value) -> Vec<ChannelMessage> {
        let associated_type = data
            .get("associatedMessageType")
            .and_then(|t| t.as_u64())
            .unwrap_or(0);

        let tapback_label = match associated_type {
            2000 => "\u{2764}\u{FE0F} Loved",      // ❤️
            2001 => "\u{1F44D} Liked",             // 👍
            2002 => "\u{1F44E} Disliked",          // 👎
            2003 => "\u{1F602} Laughed at",        // 😂
            2004 => "\u{203C}\u{FE0F} Emphasized", // ‼️
            2005 => "\u{2753} Questioned",         // ❓
            // 3000–3005 are removal tapbacks; ignore silently.
            3000..=3005 => return vec![],
            // 0 indicates a non-tapback updated-message (read receipt, delivery, etc.).
            0 => {
                tracing::debug!(
                    "BlueBubbles: updated-message is not a tapback (associatedMessageType=0)"
                );
                return vec![];
            }
            // Unknown types may indicate a new BB API version; log for diagnosis.
            _ => {
                tracing::debug!(
                    "BlueBubbles: unknown associatedMessageType {associated_type}, skipping"
                );
                return vec![];
            }
        };

        let is_from_me = data
            .get("isFromMe")
            .or_else(|| data.get("is_from_me"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if is_from_me {
            return vec![];
        }

        let Some(sender) = Self::extract_sender(data) else {
            return vec![];
        };
        if self.is_sender_ignored(&sender) {
            return vec![];
        }

        let reply_target = Self::extract_chat_guid(data)
            .filter(|g| !g.is_empty())
            .unwrap_or_else(|| sender.clone());

        // Apply DM vs group policy to tapbacks.
        let allowed = if Self::is_group_chat(&reply_target) {
            self.is_group_allowed(&reply_target, &sender)
        } else {
            self.is_dm_allowed(&sender)
        };
        if !allowed {
            return vec![];
        }

        // If the reacted-to message was from the bot, include a quoted excerpt.
        // `lookup_reply_context` returns None for unknown or empty GUIDs, so no
        // separate empty-check is needed here.
        let original_guid = data
            .get("associatedMessageGuid")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let context = self
            .lookup_reply_context(original_guid)
            .map(|body| format!(" on \"{body}\""))
            .unwrap_or_default();

        let content = format!("[Tapback] {tapback_label}{context}");
        let id = Self::extract_message_id(data).unwrap_or_else(|| Uuid::new_v4().to_string());
        let timestamp = Self::extract_timestamp(data);

        vec![ChannelMessage {
            id,
            sender,
            reply_target,
            content,
            channel: "bluebubbles".to_string(),
            timestamp,
            thread_ts: None,
        }]
    }

    /// Parse an incoming webhook payload from BlueBubbles and extract messages.
    ///
    /// Handles two event types:
    /// - `"new-message"` — standard inbound messages (text + attachments)
    /// - `"updated-message"` — tapback reactions, surfaced as system events
    ///
    /// BlueBubbles webhook envelope:
    /// ```json
    /// {
    ///   "type": "new-message",
    ///   "data": {
    ///     "guid": "p:0/...",
    ///     "text": "Hello!",
    ///     "isFromMe": false,
    ///     "dateCreated": 1_708_987_654_321,
    ///     "handle": { "address": "+1_234_567_890" },
    ///     "chats": [{ "guid": "iMessage;-;+1_234_567_890", "style": 45 }],
    ///     "attachments": []
    ///   }
    /// }
    /// ```
    ///
    /// `fromMe` messages are cached for reply-context resolution but are not
    /// returned as processable messages (the bot doesn't respond to itself).
    pub fn parse_webhook_payload(&self, payload: &serde_json::Value) -> Vec<ChannelMessage> {
        let mut messages = Vec::new();

        let event_type = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");

        // Handle tapback reactions from updated-message events.
        if event_type == "updated-message" {
            let Some(data) = payload.get("data") else {
                return messages;
            };
            return self.parse_tapback_event(data);
        }

        if event_type != "new-message" {
            tracing::debug!("BlueBubbles: skipping non-message event: {event_type}");
            return messages;
        }

        let Some(data) = payload.get("data") else {
            return messages;
        };

        let is_from_me = data
            .get("isFromMe")
            .or_else(|| data.get("is_from_me"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if is_from_me {
            // Cache outgoing messages so reply context can be resolved later.
            let message_id = Self::extract_message_id(data).unwrap_or_default();
            let chat_guid = Self::extract_chat_guid(data).unwrap_or_default();
            let body = data
                .get("text")
                .or_else(|| data.get("body"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            self.cache_from_me(&message_id, &chat_guid, &body);
            tracing::debug!("BlueBubbles: cached fromMe message {message_id}");
            return messages;
        }

        let Some(sender) = Self::extract_sender(data) else {
            tracing::debug!("BlueBubbles: skipping message with no sender");
            return messages;
        };

        if self.is_sender_ignored(&sender) {
            tracing::debug!("BlueBubbles: ignoring message from ignored sender");
            return messages;
        }

        // Use chat GUID as reply_target — ensures replies go to the correct
        // conversation (important for group chats). Falls back to sender address.
        let reply_target = Self::extract_chat_guid(data)
            .filter(|g| !g.is_empty())
            .unwrap_or_else(|| sender.clone());

        // Apply DM vs group policy gate.
        if Self::is_group_chat(&reply_target) {
            if !self.is_group_allowed(&reply_target, &sender) {
                tracing::debug!("BlueBubbles: group message denied by group_policy");
                return messages;
            }
        } else if !self.is_dm_allowed(&sender) {
            tracing::debug!("BlueBubbles: DM denied by dm_policy");
            return messages;
        }

        let Some(mut content) = Self::extract_content(data) else {
            tracing::debug!("BlueBubbles: skipping empty message");
            return messages;
        };

        // If the user is replying to a bot message, inject the original body
        // as context — matches OpenClaw's reply-context resolution.
        let reply_guid = data
            .get("replyMessage")
            .and_then(|r| r.get("guid"))
            .or_else(|| data.get("associatedMessageGuid"))
            .and_then(|v| v.as_str());
        if let Some(guid) = reply_guid {
            if let Some(bot_body) = self.lookup_reply_context(guid) {
                content = format!("[In reply to: {bot_body}]\n{content}");
            }
        }

        let timestamp = Self::extract_timestamp(data);

        // Prefer the BB message GUID for deduplication; fall back to a new UUID.
        let id = Self::extract_message_id(data).unwrap_or_else(|| Uuid::new_v4().to_string());

        messages.push(ChannelMessage {
            id,
            sender,
            reply_target,
            content,
            channel: "bluebubbles".to_string(),
            timestamp,
            thread_ts: None,
        });

        messages
    }
}

/// Map a Unicode emoji (or short alias) to a BlueBubbles Private API tapback
/// reaction name.
///
/// BB Private API accepts these names: `"love"`, `"like"`, `"dislike"`,
/// `"laugh"`, `"emphasize"`, `"question"`. Removal uses the `"-"` prefix
/// (e.g. `"-love"`).
fn emoji_to_bb_tapback(emoji: &str) -> Option<&'static str> {
    // ❤️ / ♥️ / 🥰 / 😍 → love
    // 👍 / ✅ / +1 → like
    // 👎 / -1 → dislike
    // 😂 / 🤣 / 😆 → laugh
    // ‼️ / ❗ / ❕ / 🔥 → emphasize
    // ❓ / ❔ / 🤔 → question
    match emoji {
        "\u{2764}\u{FE0F}" | "\u{2665}\u{FE0F}" | "\u{1F970}" | "\u{1F60D}" | "love" => {
            Some("love")
        }
        "\u{1F44D}" | "\u{2705}" | "+1" | "like" => Some("like"),
        "\u{1F44E}" | "dislike" | "-1" => Some("dislike"),
        "\u{1F602}" | "\u{1F923}" | "\u{1F606}" | "laugh" | "haha" => Some("laugh"),
        "\u{203C}\u{FE0F}" | "\u{2757}" | "\u{2755}" | "emphasize" | "wow" | "\u{1F525}" => {
            Some("emphasize")
        }
        "\u{2753}" | "\u{2754}" | "\u{1F914}" | "question" => Some("question"),
        _ => None,
    }
}

/// Inline text formatting state threaded through `markdown_to_attributed_body`.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Default)]
struct TextStyle {
    bold: bool,
    italic: bool,
    strike: bool,
    underline: bool,
}

/// Flush the current text buffer as one `attributedBody` segment.
/// Clears `buf` via `std::mem::take` — no separate `clear()` needed.
fn flush_attributed_segment(buf: &mut String, style: TextStyle, out: &mut Vec<serde_json::Value>) {
    if buf.is_empty() {
        return;
    }
    let mut attrs = serde_json::Map::new();
    if style.bold {
        attrs.insert("bold".into(), serde_json::Value::Bool(true));
    }
    if style.italic {
        attrs.insert("italic".into(), serde_json::Value::Bool(true));
    }
    if style.strike {
        attrs.insert("strikethrough".into(), serde_json::Value::Bool(true));
    }
    if style.underline {
        attrs.insert("underline".into(), serde_json::Value::Bool(true));
    }
    let mut seg = serde_json::Map::new();
    seg.insert(
        "string".into(),
        serde_json::Value::String(std::mem::take(buf)),
    );
    seg.insert("attributes".into(), serde_json::Value::Object(attrs));
    out.push(serde_json::Value::Object(seg));
}

/// Convert markdown to a BlueBubbles Private API `attributedBody` array.
///
/// Supported inline markers (paired toggles):
/// - `**text**`  → bold
/// - `*text*`    → italic (single asterisk; checked after double)
/// - `~~text~~`  → strikethrough
/// - `__text__`  → underline (double underscore)
/// - `` `text` ``→ inline code → bold (backticks stripped from output)
///
/// Block-level patterns:
/// - ` ``` … ``` ` code fence → plain text; opening/closing fence lines stripped
/// - `# ` / `## ` / `### ` at line start → bold until end of line; `#` prefix stripped
///
/// Newlines and spaces within text are preserved verbatim.
/// Unrecognised characters (single `_`, etc.) pass through unchanged.
fn markdown_to_attributed_body(text: &str) -> Vec<serde_json::Value> {
    let mut segments: Vec<serde_json::Value> = Vec::new();
    let mut buf = String::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut bold = false;
    let mut italic = false;
    let mut strike = false;
    let mut underline = false;
    let mut code = false; // single backtick inline code → renders as bold
    let mut header_bold = false; // active markdown header → bold until \n
    let mut in_code_block = false; // inside ``` … ``` block → plain text
    let mut at_line_start = true;

    while i < len {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        let next2 = chars.get(i + 2).copied();

        // Newline: flush header-bold segment, reset header state
        if c == '\n' {
            if header_bold {
                flush_attributed_segment(
                    &mut buf,
                    TextStyle {
                        bold: true,
                        italic,
                        strike,
                        underline,
                    },
                    &mut segments,
                );
                header_bold = false;
                buf.push('\n');
                flush_attributed_segment(&mut buf, TextStyle::default(), &mut segments);
            } else {
                buf.push('\n');
            }
            at_line_start = true;
            i += 1;
            continue;
        }

        // Inside a code block: only watch for closing ```
        if in_code_block {
            if c == '`' && next == Some('`') && next2 == Some('`') {
                flush_attributed_segment(&mut buf, TextStyle::default(), &mut segments);
                in_code_block = false;
                i += 3;
                while i < len && chars[i] != '\n' {
                    i += 1;
                }
                at_line_start = true;
            } else {
                buf.push(c);
                i += 1;
            }
            continue;
        }

        // Header marker at line start: #/##/### followed by a space
        if at_line_start && c == '#' {
            let mut j = i;
            while j < len && chars[j] == '#' {
                j += 1;
            }
            if j < len && chars[j] == ' ' {
                flush_attributed_segment(
                    &mut buf,
                    TextStyle {
                        bold: bold || code,
                        italic,
                        strike,
                        underline,
                    },
                    &mut segments,
                );
                header_bold = true;
                i = j + 1; // skip all # chars and the space
                at_line_start = false;
                continue;
            }
        }

        at_line_start = false;
        let eff_bold = bold || code || header_bold;

        // Triple backtick: opening code fence
        if c == '`' && next == Some('`') && next2 == Some('`') {
            flush_attributed_segment(
                &mut buf,
                TextStyle {
                    bold: eff_bold,
                    italic,
                    strike,
                    underline,
                },
                &mut segments,
            );
            in_code_block = true;
            i += 3;
            // Skip language hint on the same line as opening fence
            while i < len && chars[i] != '\n' {
                i += 1;
            }
            if i < len {
                i += 1; // skip the newline after the opening fence
            }
            at_line_start = true;
            continue;
        }

        // Single backtick: inline code → bold
        if c == '`' {
            flush_attributed_segment(
                &mut buf,
                TextStyle {
                    bold: eff_bold,
                    italic,
                    strike,
                    underline,
                },
                &mut segments,
            );
            code = !code;
            i += 1;
            continue;
        }

        // **bold**
        if c == '*' && next == Some('*') {
            flush_attributed_segment(
                &mut buf,
                TextStyle {
                    bold: eff_bold,
                    italic,
                    strike,
                    underline,
                },
                &mut segments,
            );
            bold = !bold;
            i += 2;
            continue;
        }

        // ~~strikethrough~~
        if c == '~' && next == Some('~') {
            flush_attributed_segment(
                &mut buf,
                TextStyle {
                    bold: eff_bold,
                    italic,
                    strike,
                    underline,
                },
                &mut segments,
            );
            strike = !strike;
            i += 2;
            continue;
        }

        // __underline__
        if c == '_' && next == Some('_') {
            flush_attributed_segment(
                &mut buf,
                TextStyle {
                    bold: eff_bold,
                    italic,
                    strike,
                    underline,
                },
                &mut segments,
            );
            underline = !underline;
            i += 2;
            continue;
        }

        // *italic*
        if c == '*' {
            flush_attributed_segment(
                &mut buf,
                TextStyle {
                    bold: eff_bold,
                    italic,
                    strike,
                    underline,
                },
                &mut segments,
            );
            italic = !italic;
            i += 1;
            continue;
        }

        buf.push(c);
        i += 1;
    }

    flush_attributed_segment(
        &mut buf,
        TextStyle {
            bold: bold || code || header_bold,
            italic,
            strike,
            underline,
        },
        &mut segments,
    );

    if segments.is_empty() {
        segments.push(serde_json::json!({ "string": "", "attributes": {} }));
    }

    segments
}

#[async_trait]
impl Channel for BlueBubblesChannel {
    fn name(&self) -> &str {
        "bluebubbles"
    }

    /// Send a message via the BlueBubbles REST API using the Private API for
    /// rich text. Converts Discord-style markdown (`**bold**`, `*italic*`,
    /// `~~strikethrough~~`, `__underline__`) to a BB `attributedBody` array.
    /// The plain `message` field carries marker-stripped text as a fallback.
    ///
    /// `message.recipient` must be a chat GUID (e.g. `iMessage;-;+15_551_234_567`).
    /// Authentication is via `?password=` query param (not a Bearer header).
    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let url = self.api_url("/api/v1/message/text");

        // Strip [EFFECT:name] tag from content before rendering
        let (content_no_effect, effect_id) = extract_effect(&message.content);
        let attributed = markdown_to_attributed_body(&content_no_effect);

        // Plain-text fallback: concatenate all segment strings (markers stripped)
        let plain: String = attributed
            .iter()
            .filter_map(|s| s.get("string").and_then(|v| v.as_str()))
            .collect();

        let mut body = serde_json::json!({
            "chatGuid": message.recipient,
            "tempGuid": Uuid::new_v4().to_string(),
            "message": plain,
            "method": "private-api",
            "attributedBody": attributed,
        });

        // Append effectId if present
        if let Some(ref eid) = effect_id {
            body.as_object_mut()
                .expect("serde_json::json!({}) always produces an object")
                .insert("effectId".into(), serde_json::Value::String(eid.clone()));
        }

        let resp = self
            .client
            .post(&url)
            .query(&[("password", &self.password)])
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("BlueBubbles send request failed: {}", e.without_url()))?;

        if resp.status().is_success() {
            return Ok(());
        }

        let status = resp.status();
        let error_body = resp.text().await.unwrap_or_default();
        let sanitized = crate::providers::sanitize_api_error(&error_body);
        tracing::error!("BlueBubbles send failed: {status} — {sanitized}");
        anyhow::bail!("BlueBubbles API error: {status}");
    }

    /// Send a typing indicator to the given chat GUID via the BB Private API.
    /// BB typing indicators expire in ~5 s; this method spawns a background
    /// loop that re-fires every 4 s so the indicator stays visible while the
    /// LLM is processing.
    async fn start_typing(&self, recipient: &str) -> anyhow::Result<()> {
        self.stop_typing(recipient).await?;

        let client = self.client.clone();
        let server_url = self.server_url.clone();
        let password = self.password.clone();
        let chat_guid = urlencoding::encode(recipient).into_owned();

        let handle = tokio::spawn(async move {
            let url = format!("{server_url}/api/v1/chat/{chat_guid}/typing");
            loop {
                let _ = client
                    .post(&url)
                    .query(&[("password", &password)])
                    .send()
                    .await;
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
            }
        });

        self.typing_handles
            .lock()
            .insert(recipient.to_string(), handle);
        Ok(())
    }

    /// Stop the typing indicator background loop for the given recipient.
    async fn stop_typing(&self, recipient: &str) -> anyhow::Result<()> {
        if let Some(handle) = self.typing_handles.lock().remove(recipient) {
            handle.abort();
        }
        Ok(())
    }

    /// Keepalive placeholder — actual messages arrive via the `/bluebubbles` webhook.
    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        tracing::info!(
            "BlueBubbles channel active (webhook mode). \
            Configure your BlueBubbles server to POST webhooks to /bluebubbles."
        );
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    /// Verify the BlueBubbles server is reachable.
    /// Uses `/api/v1/ping` — the lightest probe endpoint (matches OpenClaw).
    /// Authentication is via `?password=` query param.
    async fn health_check(&self) -> bool {
        let url = self.api_url("/api/v1/ping");
        self.client
            .get(&url)
            .query(&[("password", &self.password)])
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// Add a tapback reaction to a message via the BlueBubbles Private API.
    ///
    /// `channel_id` is the chat GUID (e.g. `iMessage;-;+15551234567`).
    /// `message_id` is the BB message GUID (e.g. `p:0/UUID`).
    /// `emoji` is a Unicode emoji or short alias (see `emoji_to_bb_tapback`).
    ///
    /// Supported reactions: ❤️ 👍 👎 😂 ‼️ ❓ (and string aliases).
    async fn add_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        // Fail fast on empty identifiers before making a network call.
        if channel_id.is_empty() {
            anyhow::bail!("add_reaction: channel_id must not be empty");
        }
        if message_id.is_empty() {
            anyhow::bail!("add_reaction: message_id must not be empty");
        }
        if emoji.is_empty() {
            anyhow::bail!("add_reaction: emoji must not be empty");
        }
        let Some(reaction) = emoji_to_bb_tapback(emoji) else {
            anyhow::bail!(
                "Unsupported tapback emoji '{emoji}' — supported: \
                 \u{2764}\u{FE0F} \u{1F44D} \u{1F44E} \u{1F602} \u{203C}\u{FE0F} \u{2753}"
            );
        };
        let url = self.api_url("/api/v1/message/react");
        let body = serde_json::json!({
            "chatGuid": channel_id,
            "selectedMessageGuid": message_id,
            "reaction": reaction,
            "method": "private-api",
        });
        let resp = self
            .client
            .post(&url)
            .query(&[("password", &self.password)])
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "BlueBubbles add_reaction request failed: {}",
                    e.without_url()
                )
            })?;
        if resp.status().is_success() {
            return Ok(());
        }
        let status = resp.status();
        let error_body = resp.text().await.unwrap_or_default();
        let sanitized = crate::providers::sanitize_api_error(&error_body);
        tracing::error!("BlueBubbles add_reaction failed: {status} — {sanitized}");
        anyhow::bail!("BlueBubbles tapback failed: {status}");
    }

    /// Remove a tapback reaction from a message via the BlueBubbles Private API.
    ///
    /// BB uses a `"-"` prefix convention for removal (e.g. `"-love"` removes
    /// a previously added love tapback).
    async fn remove_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        // Fail fast on empty identifiers before making a network call.
        if channel_id.is_empty() {
            anyhow::bail!("remove_reaction: channel_id must not be empty");
        }
        if message_id.is_empty() {
            anyhow::bail!("remove_reaction: message_id must not be empty");
        }
        if emoji.is_empty() {
            anyhow::bail!("remove_reaction: emoji must not be empty");
        }
        let Some(reaction) = emoji_to_bb_tapback(emoji) else {
            anyhow::bail!(
                "Unsupported tapback emoji '{emoji}' — supported: \
                 \u{2764}\u{FE0F} \u{1F44D} \u{1F44E} \u{1F602} \u{203C}\u{FE0F} \u{2753}"
            );
        };
        let url = self.api_url("/api/v1/message/react");
        let body = serde_json::json!({
            "chatGuid": channel_id,
            "selectedMessageGuid": message_id,
            "reaction": format!("-{reaction}"),
            "method": "private-api",
        });
        let resp = self
            .client
            .post(&url)
            .query(&[("password", &self.password)])
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "BlueBubbles remove_reaction request failed: {}",
                    e.without_url()
                )
            })?;
        if resp.status().is_success() {
            return Ok(());
        }
        let status = resp.status();
        let error_body = resp.text().await.unwrap_or_default();
        let sanitized = crate::providers::sanitize_api_error(&error_body);
        tracing::error!("BlueBubbles remove_reaction failed: {status} — {sanitized}");
        anyhow::bail!("BlueBubbles remove tapback failed: {status}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_channel() -> BlueBubblesChannel {
        BlueBubblesChannel::new(
            "http://localhost:1234".into(),
            String::new(),
            vec!["+1_234_567_890".into()],
            vec![],
        )
        .expect("test HTTP client build")
    }

    fn make_open_channel() -> BlueBubblesChannel {
        BlueBubblesChannel::new(
            "http://localhost:1234".into(),
            String::new(),
            vec!["*".into()],
            vec![],
        )
        .expect("test HTTP client build")
    }

    #[test]
    fn bluebubbles_channel_name() {
        let ch = make_channel();
        assert_eq!(ch.name(), "bluebubbles");
    }

    #[test]
    fn bluebubbles_sender_allowed_exact() {
        let ch = make_channel();
        assert!(ch.is_sender_allowed("+1_234_567_890"));
        assert!(!ch.is_sender_allowed("+9_876_543_210"));
    }

    #[test]
    fn bluebubbles_sender_allowed_wildcard() {
        let ch = make_open_channel();
        assert!(ch.is_sender_allowed("+1_234_567_890"));
        assert!(ch.is_sender_allowed("user@example.com"));
    }

    #[test]
    fn bluebubbles_sender_allowed_empty_list_allows_all() {
        // Empty allowlist = no restriction (matches OpenClaw behaviour)
        let ch = BlueBubblesChannel::new(
            "http://localhost:1234".into(),
            String::new(),
            vec![],
            vec![],
        )
        .expect("test HTTP client build");
        assert!(ch.is_sender_allowed("+1_234_567_890"));
        assert!(ch.is_sender_allowed("anyone@example.com"));
    }

    // --- Policy gate tests ---

    #[test]
    fn bluebubbles_is_group_chat() {
        assert!(BlueBubblesChannel::is_group_chat("iMessage;+;chat123"));
        assert!(!BlueBubblesChannel::is_group_chat(
            "iMessage;-;+15551234567"
        ));
        assert!(!BlueBubblesChannel::is_group_chat(""));
    }

    #[test]
    fn bluebubbles_dm_policy_open_empty_allows_all() {
        // Open + empty allowed_senders = allow everyone (legacy behaviour)
        let ch = BlueBubblesChannel::new("http://localhost".into(), String::new(), vec![], vec![])
            .expect("test HTTP client build");
        assert!(ch.is_dm_allowed("+1_555_000_0001"));
    }

    #[test]
    fn bluebubbles_dm_policy_open_with_list_filters() {
        let ch = BlueBubblesChannel::new(
            "http://localhost".into(),
            String::new(),
            vec!["+1_555_000_0001".into()],
            vec![],
        )
        .expect("test HTTP client build");
        assert!(ch.is_dm_allowed("+1_555_000_0001"));
        assert!(!ch.is_dm_allowed("+1_555_000_0099"));
    }

    #[test]
    fn bluebubbles_dm_policy_allowlist_empty_denies_all() {
        let ch = BlueBubblesChannel::new("http://localhost".into(), String::new(), vec![], vec![])
            .expect("test HTTP client build")
            .with_policies(
                BlueBubblesDmPolicy::Allowlist,
                BlueBubblesGroupPolicy::Open,
                vec![],
                true,
            );
        assert!(!ch.is_dm_allowed("+1_555_000_0001"));
    }

    #[test]
    fn bluebubbles_dm_policy_allowlist_matches() {
        let ch = BlueBubblesChannel::new(
            "http://localhost".into(),
            String::new(),
            vec!["+1_555_000_0001".into()],
            vec![],
        )
        .expect("test HTTP client build")
        .with_policies(
            BlueBubblesDmPolicy::Allowlist,
            BlueBubblesGroupPolicy::Open,
            vec![],
            true,
        );
        assert!(ch.is_dm_allowed("+1_555_000_0001"));
        assert!(!ch.is_dm_allowed("+1_555_000_0099"));
    }

    #[test]
    fn bluebubbles_dm_policy_disabled_denies_all() {
        let ch = BlueBubblesChannel::new(
            "http://localhost".into(),
            String::new(),
            vec!["*".into()],
            vec![],
        )
        .expect("test HTTP client build")
        .with_policies(
            BlueBubblesDmPolicy::Disabled,
            BlueBubblesGroupPolicy::Open,
            vec![],
            true,
        );
        assert!(!ch.is_dm_allowed("+1_555_000_0001"));
    }

    #[test]
    fn bluebubbles_group_policy_open_allows_any() {
        let ch = BlueBubblesChannel::new("http://localhost".into(), String::new(), vec![], vec![])
            .expect("test HTTP client build");
        // Open + empty allowed_senders: any sender, any chat
        assert!(ch.is_group_allowed("iMessage;+;chat1", "sender_a"));
        assert!(ch.is_group_allowed("iMessage;+;chat2", "sender_b"));
    }

    #[test]
    fn bluebubbles_group_policy_open_with_allowed_senders_filters_sender() {
        let ch = BlueBubblesChannel::new(
            "http://localhost".into(),
            String::new(),
            vec!["sender_a".into()],
            vec![],
        )
        .expect("test HTTP client build");
        // Open + allowed_senders set: sender filter still applies for backward-compat
        assert!(ch.is_group_allowed("iMessage;+;chat1", "sender_a"));
        assert!(!ch.is_group_allowed("iMessage;+;chat1", "sender_b"));
    }

    #[test]
    fn bluebubbles_group_policy_disabled_denies_all() {
        let ch = BlueBubblesChannel::new("http://localhost".into(), String::new(), vec![], vec![])
            .expect("test HTTP client build")
            .with_policies(
                BlueBubblesDmPolicy::Open,
                BlueBubblesGroupPolicy::Disabled,
                vec![],
                true,
            );
        assert!(!ch.is_group_allowed("iMessage;+;chat1", "sender_a"));
    }

    #[test]
    fn bluebubbles_group_policy_allowlist_filters() {
        let ch = BlueBubblesChannel::new("http://localhost".into(), String::new(), vec![], vec![])
            .expect("test HTTP client build")
            .with_policies(
                BlueBubblesDmPolicy::Open,
                BlueBubblesGroupPolicy::Allowlist,
                vec!["iMessage;+;chat_allowed".into()],
                true,
            );
        assert!(ch.is_group_allowed("iMessage;+;chat_allowed", "sender_a"));
        assert!(!ch.is_group_allowed("iMessage;+;chat_other", "sender_a"));
    }

    #[test]
    fn bluebubbles_group_policy_allowlist_empty_denies_all() {
        let ch = BlueBubblesChannel::new("http://localhost".into(), String::new(), vec![], vec![])
            .expect("test HTTP client build")
            .with_policies(
                BlueBubblesDmPolicy::Open,
                BlueBubblesGroupPolicy::Allowlist,
                vec![],
                true,
            );
        assert!(!ch.is_group_allowed("iMessage;+;any_group", "sender_a"));
    }

    #[test]
    fn bluebubbles_group_policy_allowlist_wildcard() {
        let ch = BlueBubblesChannel::new("http://localhost".into(), String::new(), vec![], vec![])
            .expect("test HTTP client build")
            .with_policies(
                BlueBubblesDmPolicy::Open,
                BlueBubblesGroupPolicy::Allowlist,
                vec!["*".into()],
                true,
            );
        assert!(ch.is_group_allowed("iMessage;+;any_group", "sender_a"));
    }

    #[test]
    fn bluebubbles_server_url_trailing_slash_trimmed() {
        let ch = BlueBubblesChannel::new(
            "http://localhost:1234/".into(),
            String::new(),
            vec!["*".into()],
            vec![],
        )
        .expect("test HTTP client build");
        assert_eq!(
            ch.api_url("/api/v1/server/info"),
            "http://localhost:1234/api/v1/server/info"
        );
    }

    #[test]
    fn bluebubbles_normalize_handle_strips_service_prefix() {
        assert_eq!(
            BlueBubblesChannel::normalize_handle("iMessage:+1_234_567_890"),
            "+1_234_567_890"
        );
        assert_eq!(
            BlueBubblesChannel::normalize_handle("sms:+1_234_567_890"),
            "+1_234_567_890"
        );
        assert_eq!(
            BlueBubblesChannel::normalize_handle("auto:+1_234_567_890"),
            "+1_234_567_890"
        );
    }

    #[test]
    fn bluebubbles_normalize_handle_email_lowercased() {
        assert_eq!(
            BlueBubblesChannel::normalize_handle("User@Example.COM"),
            "user@example.com"
        );
    }

    #[test]
    fn bluebubbles_parse_valid_dm_message() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "type": "new-message",
            "data": {
                "guid": "p:0/abc123",
                "text": "Hello ZeroClaw!",
                "isFromMe": false,
                "dateCreated": 1_708_987_654_321_u64,
                "handle": { "address": "+1_234_567_890" },
                "chats": [{ "guid": "iMessage;-;+1_234_567_890", "style": 45 }],
                "attachments": []
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].id, "p:0/abc123");
        assert_eq!(msgs[0].sender, "+1_234_567_890");
        assert_eq!(msgs[0].content, "Hello ZeroClaw!");
        assert_eq!(msgs[0].reply_target, "iMessage;-;+1_234_567_890");
        assert_eq!(msgs[0].channel, "bluebubbles");
        assert_eq!(msgs[0].timestamp, 1_708_987_654); // ms → s
    }

    #[test]
    fn bluebubbles_parse_group_chat_message() {
        let ch = make_open_channel();
        let payload = serde_json::json!({
            "type": "new-message",
            "data": {
                "guid": "p:0/def456",
                "text": "Group message",
                "isFromMe": false,
                "dateCreated": 1_708_987_654_000_u64,
                "handle": { "address": "+1_111_111_111" },
                "chats": [{ "guid": "iMessage;+;group-abc", "style": 43 }],
                "attachments": []
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "+1_111_111_111");
        assert_eq!(msgs[0].reply_target, "iMessage;+;group-abc");
    }

    #[test]
    fn bluebubbles_parse_skip_is_from_me() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "type": "new-message",
            "data": {
                "guid": "p:0/sent",
                "text": "My own message",
                "isFromMe": true,
                "dateCreated": 1_708_987_654_000_u64,
                "handle": { "address": "+1_234_567_890" },
                "chats": [{ "guid": "iMessage;-;+1_234_567_890", "style": 45 }],
                "attachments": []
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "fromMe messages must not be processed");
        // Verify it was cached and is readable via lookup_reply_context
        assert_eq!(
            ch.lookup_reply_context("p:0/sent").as_deref(),
            Some("My own message"),
            "fromMe message should be in reply cache"
        );
    }

    #[test]
    fn bluebubbles_parse_skip_non_message_event() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "type": "updated-message",
            "data": { "guid": "p:0/abc", "isFromMe": false }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "Non new-message events should be skipped");
    }

    #[test]
    fn bluebubbles_parse_skip_unauthorized_sender() {
        let ch = make_channel();
        let payload = serde_json::json!({
            "type": "new-message",
            "data": {
                "guid": "p:0/spam",
                "text": "Spam",
                "isFromMe": false,
                "dateCreated": 1_708_987_654_000_u64,
                "handle": { "address": "+9_999_999_999" },
                "chats": [{ "guid": "iMessage;-;+9_999_999_999", "style": 45 }],
                "attachments": []
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "Unauthorized senders should be filtered");
    }

    #[test]
    fn bluebubbles_parse_skip_empty_text_no_attachments() {
        let ch = make_open_channel();
        let payload = serde_json::json!({
            "type": "new-message",
            "data": {
                "guid": "p:0/empty",
                "text": "",
                "isFromMe": false,
                "dateCreated": 1_708_987_654_000_u64,
                "handle": { "address": "+1_234_567_890" },
                "chats": [{ "guid": "iMessage;-;+1_234_567_890", "style": 45 }],
                "attachments": []
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert!(
            msgs.is_empty(),
            "Empty text with no attachments should be skipped"
        );
    }

    #[test]
    fn bluebubbles_parse_image_attachment() {
        let ch = make_open_channel();
        let payload = serde_json::json!({
            "type": "new-message",
            "data": {
                "guid": "p:0/img",
                "isFromMe": false,
                "dateCreated": 1_708_987_654_000_u64,
                "handle": { "address": "+1_234_567_890" },
                "chats": [{ "guid": "iMessage;-;+1_234_567_890", "style": 45 }],
                "attachments": [{
                    "guid": "att-guid",
                    "transferName": "photo.jpg",
                    "mimeType": "image/jpeg",
                    "totalBytes": 102_400
                }]
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "<media:image> (1 image)");
    }

    #[test]
    fn bluebubbles_parse_non_image_attachment() {
        let ch = make_open_channel();
        let payload = serde_json::json!({
            "type": "new-message",
            "data": {
                "guid": "p:0/doc",
                "isFromMe": false,
                "dateCreated": 1_708_987_654_000_u64,
                "handle": { "address": "+1_234_567_890" },
                "chats": [{ "guid": "iMessage;-;+1_234_567_890", "style": 45 }],
                "attachments": [{
                    "guid": "att-guid",
                    "transferName": "contract.pdf",
                    "mimeType": "application/pdf",
                    "totalBytes": 204_800
                }]
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "<media:attachment> (1 file)");
    }

    #[test]
    fn bluebubbles_parse_text_with_attachment() {
        let ch = make_open_channel();
        let payload = serde_json::json!({
            "type": "new-message",
            "data": {
                "guid": "p:0/mixed",
                "text": "See attached",
                "isFromMe": false,
                "dateCreated": 1_708_987_654_000_u64,
                "handle": { "address": "+1_234_567_890" },
                "chats": [{ "guid": "iMessage;-;+1_234_567_890", "style": 45 }],
                "attachments": [{
                    "guid": "att-guid",
                    "transferName": "doc.pdf",
                    "mimeType": "application/pdf",
                    "totalBytes": 1024
                }]
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "See attached\n<media:attachment> (1 file)");
    }

    #[test]
    fn bluebubbles_parse_fallback_reply_target_when_no_chats() {
        let ch = make_open_channel();
        let payload = serde_json::json!({
            "type": "new-message",
            "data": {
                "guid": "p:0/nochats",
                "text": "Hi",
                "isFromMe": false,
                "dateCreated": 1_708_987_654_000_u64,
                "handle": { "address": "+1_234_567_890" },
                "chats": [],
                "attachments": []
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].reply_target, "+1_234_567_890");
    }

    #[test]
    fn bluebubbles_parse_missing_data_field() {
        let ch = make_channel();
        let payload = serde_json::json!({ "type": "new-message" });
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty());
    }

    #[test]
    fn bluebubbles_parse_email_handle() {
        let ch = BlueBubblesChannel::new(
            "http://localhost:1234".into(),
            String::new(),
            vec!["user@example.com".into()],
            vec![],
        )
        .expect("test HTTP client build");
        let payload = serde_json::json!({
            "type": "new-message",
            "data": {
                "guid": "p:0/email",
                "text": "Hello via Apple ID",
                "isFromMe": false,
                "dateCreated": 1_708_987_654_000_u64,
                "handle": { "address": "user@example.com" },
                "chats": [{ "guid": "iMessage;-;user@example.com", "style": 45 }],
                "attachments": []
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].sender, "user@example.com");
        assert_eq!(msgs[0].reply_target, "iMessage;-;user@example.com");
    }

    #[test]
    fn bluebubbles_parse_direct_chat_guid_field() {
        // chatGuid at the top-level data field (some BB versions)
        let ch = make_open_channel();
        let payload = serde_json::json!({
            "type": "new-message",
            "data": {
                "guid": "p:0/direct",
                "text": "Hi",
                "isFromMe": false,
                "chatGuid": "iMessage;-;+1_111_111_111",
                "dateCreated": 1_708_987_654_000_u64,
                "handle": { "address": "+1_111_111_111" },
                "attachments": []
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].reply_target, "iMessage;-;+1_111_111_111");
    }

    #[test]
    fn bluebubbles_parse_timestamp_seconds_not_double_divided() {
        // Timestamp already in seconds (< 1e12) should not be divided again
        let ch = make_open_channel();
        let payload = serde_json::json!({
            "type": "new-message",
            "data": {
                "guid": "p:0/ts",
                "text": "Hi",
                "isFromMe": false,
                "dateCreated": 1_708_987_654_u64, // seconds
                "handle": { "address": "+1_234_567_890" },
                "chats": [{ "guid": "iMessage;-;+1_234_567_890" }],
                "attachments": []
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs[0].timestamp, 1_708_987_654);
    }

    #[test]
    fn bluebubbles_parse_video_attachment() {
        let ch = make_open_channel();
        let payload = serde_json::json!({
            "type": "new-message",
            "data": {
                "guid": "p:0/vid",
                "isFromMe": false,
                "dateCreated": 1_708_987_654_000_u64,
                "handle": { "address": "+1_234_567_890" },
                "chats": [{ "guid": "iMessage;-;+1_234_567_890" }],
                "attachments": [{ "mimeType": "video/mp4", "transferName": "clip.mp4" }]
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs[0].content, "<media:video> (1 video)");
    }

    #[test]
    fn bluebubbles_parse_multiple_images() {
        let ch = make_open_channel();
        let payload = serde_json::json!({
            "type": "new-message",
            "data": {
                "guid": "p:0/imgs",
                "isFromMe": false,
                "dateCreated": 1_708_987_654_000_u64,
                "handle": { "address": "+1_234_567_890" },
                "chats": [{ "guid": "iMessage;-;+1_234_567_890" }],
                "attachments": [
                    { "mimeType": "image/jpeg", "transferName": "a.jpg" },
                    { "mimeType": "image/png", "transferName": "b.png" }
                ]
            }
        });

        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs[0].content, "<media:image> (2 images)");
    }

    // -- markdown_to_attributed_body tests --

    #[test]
    fn attributed_body_plain_text_no_markers() {
        let segs = markdown_to_attributed_body("Hello world");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0]["string"], "Hello world");
        assert_eq!(segs[0]["attributes"], serde_json::json!({}));
    }

    #[test]
    fn attributed_body_bold() {
        let segs = markdown_to_attributed_body("**bold**");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0]["string"], "bold");
        assert_eq!(segs[0]["attributes"]["bold"], true);
        assert_eq!(segs[0]["attributes"]["italic"], serde_json::Value::Null);
    }

    #[test]
    fn attributed_body_italic() {
        let segs = markdown_to_attributed_body("*italic*");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0]["string"], "italic");
        assert_eq!(segs[0]["attributes"]["italic"], true);
        assert_eq!(segs[0]["attributes"]["bold"], serde_json::Value::Null);
    }

    #[test]
    fn attributed_body_strikethrough() {
        let segs = markdown_to_attributed_body("~~strike~~");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0]["string"], "strike");
        assert_eq!(segs[0]["attributes"]["strikethrough"], true);
    }

    #[test]
    fn attributed_body_underline() {
        let segs = markdown_to_attributed_body("__under__");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0]["string"], "under");
        assert_eq!(segs[0]["attributes"]["underline"], true);
    }

    #[test]
    fn attributed_body_mixed_three_segments() {
        let segs = markdown_to_attributed_body("Hello **world** there");
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0]["string"], "Hello ");
        assert_eq!(segs[0]["attributes"], serde_json::json!({}));
        assert_eq!(segs[1]["string"], "world");
        assert_eq!(segs[1]["attributes"]["bold"], true);
        assert_eq!(segs[2]["string"], " there");
        assert_eq!(segs[2]["attributes"], serde_json::json!({}));
    }

    #[test]
    fn attributed_body_nested_bold_italic() {
        // "bold " (bold), "and italic" (bold+italic), " text" (bold)
        let segs = markdown_to_attributed_body("**bold *and italic* text**");
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0]["string"], "bold ");
        assert_eq!(segs[0]["attributes"]["bold"], true);
        assert_eq!(segs[0]["attributes"]["italic"], serde_json::Value::Null);
        assert_eq!(segs[1]["string"], "and italic");
        assert_eq!(segs[1]["attributes"]["bold"], true);
        assert_eq!(segs[1]["attributes"]["italic"], true);
        assert_eq!(segs[2]["string"], " text");
        assert_eq!(segs[2]["attributes"]["bold"], true);
        assert_eq!(segs[2]["attributes"]["italic"], serde_json::Value::Null);
    }

    #[test]
    fn attributed_body_empty_string() {
        let segs = markdown_to_attributed_body("");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0]["string"], "");
    }

    #[test]
    fn attributed_body_plain_text_preserved_in_send_message_field() {
        // Verify the plain-text fallback strips markers
        let segs = markdown_to_attributed_body("Say **hello** to *everyone*");
        let plain: String = segs
            .iter()
            .filter_map(|s| s.get("string").and_then(|v| v.as_str()))
            .collect();
        assert_eq!(plain, "Say hello to everyone");
    }

    #[test]
    fn attributed_body_inline_code_renders_as_bold() {
        let segs = markdown_to_attributed_body("`cargo build`");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0]["string"], "cargo build");
        assert_eq!(segs[0]["attributes"]["bold"], true);
    }

    #[test]
    fn attributed_body_inline_code_in_sentence() {
        let segs = markdown_to_attributed_body("Run `cargo build` now");
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0]["string"], "Run ");
        assert_eq!(segs[0]["attributes"], serde_json::json!({}));
        assert_eq!(segs[1]["string"], "cargo build");
        assert_eq!(segs[1]["attributes"]["bold"], true);
        assert_eq!(segs[2]["string"], " now");
        assert_eq!(segs[2]["attributes"], serde_json::json!({}));
    }

    #[test]
    fn attributed_body_header_bold() {
        let segs = markdown_to_attributed_body("## Section");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0]["string"], "Section");
        assert_eq!(segs[0]["attributes"]["bold"], true);
    }

    #[test]
    fn attributed_body_header_resets_after_newline() {
        let segs = markdown_to_attributed_body("## Title\nBody text");
        let title = segs
            .iter()
            .find(|s| s["string"].as_str() == Some("Title"))
            .expect("Title segment missing");
        assert_eq!(title["attributes"]["bold"], true);
        // Body text must be plain (bold reset after \n)
        let plain: String = segs
            .iter()
            .filter_map(|s| s["string"].as_str())
            .filter(|s| s.contains("Body"))
            .collect();
        assert!(plain.contains("Body text"));
        let body_seg = segs
            .iter()
            .find(|s| s["string"].as_str() == Some("Body text"))
            .expect("Body text segment missing");
        assert_eq!(body_seg["attributes"], serde_json::json!({}));
    }

    #[test]
    fn attributed_body_code_block_plain_fences_stripped() {
        let segs = markdown_to_attributed_body("```\nhello world\n```");
        // Content rendered plain; no segment should contain backticks
        for seg in &segs {
            assert!(
                !seg["string"].as_str().unwrap_or("").contains("```"),
                "Fence markers must not appear in segments: {seg}"
            );
        }
        let all_text: String = segs.iter().filter_map(|s| s["string"].as_str()).collect();
        assert!(
            all_text.contains("hello world"),
            "Code content must be preserved"
        );
    }

    #[test]
    fn attributed_body_code_block_with_language_hint() {
        let segs = markdown_to_attributed_body("```rust\nfn main() {}\n```");
        // "rust" language hint on opening fence line must be stripped
        let all_text: String = segs.iter().filter_map(|s| s["string"].as_str()).collect();
        assert!(!all_text.contains("```"), "Fence markers must not appear");
        assert!(
            !all_text.contains("rust\n"),
            "Language hint must be stripped"
        );
        assert!(
            all_text.contains("fn main()"),
            "Code content must be preserved"
        );
    }

    #[test]
    fn bluebubbles_ignore_sender_exact() {
        let ch = BlueBubblesChannel::new(
            "http://localhost:1234".into(),
            String::new(),
            vec!["*".into()],
            vec!["+1_999_000_0000".into()],
        )
        .expect("test HTTP client build");
        assert!(ch.is_sender_ignored("+1_999_000_0000"));
        assert!(!ch.is_sender_ignored("+1_234_567_890"));
    }

    #[test]
    fn bluebubbles_ignore_sender_takes_precedence_over_allowlist() {
        // A sender in both ignore_senders and allowed_senders must be dropped.
        let ch = BlueBubblesChannel::new(
            "http://localhost:1234".into(),
            String::new(),
            vec!["+1_999_000_0000".into()],
            vec!["+1_999_000_0000".into()],
        )
        .expect("test HTTP client build");
        assert!(ch.is_sender_ignored("+1_999_000_0000"));
    }

    #[test]
    fn bluebubbles_ignore_sender_empty_list_ignores_nothing() {
        let ch = make_open_channel(); // ignore_senders = []
        assert!(!ch.is_sender_ignored("+1_234_567_890"));
        assert!(!ch.is_sender_ignored("anyone@example.com"));
    }

    #[test]
    fn extract_audio_attachment_detects_audio() {
        let data = serde_json::json!({
            "attachments": [{
                "guid": "p:0/B7C54F16-3F8B-4B03-A5D5-E9C29D6E9E4E",
                "mimeType": "audio/mpeg",
                "transferName": "Audio Message.mp3"
            }]
        });
        let att = BlueBubblesChannel::extract_audio_attachment(&data).unwrap();
        // Uses the BB attachment GUID (not the message GUID) for REST API download.
        assert_eq!(att.guid, "p:0/B7C54F16-3F8B-4B03-A5D5-E9C29D6E9E4E");
        assert_eq!(att.transfer_name, "Audio Message.mp3");
    }

    #[test]
    fn extract_audio_attachment_detects_caf() {
        // CAF (Core Audio Format) is the native iMessage voice memo format.
        let data = serde_json::json!({
            "attachments": [{
                "guid": "p:0/A1B2C3D4-E5F6-7890-ABCD-EF1234567890",
                "mimeType": "audio/x-caf",
                "transferName": "Audio Message.caf"
            }]
        });
        let att = BlueBubblesChannel::extract_audio_attachment(&data).unwrap();
        assert_eq!(att.guid, "p:0/A1B2C3D4-E5F6-7890-ABCD-EF1234567890");
        assert_eq!(att.transfer_name, "Audio Message.caf");
    }

    #[test]
    fn extract_audio_attachment_skips_missing_guid() {
        // No attachment guid → cannot download via REST API; must be skipped.
        let data = serde_json::json!({
            "attachments": [{"mimeType": "audio/mpeg", "transferName": "voice.mp3"}]
        });
        assert!(BlueBubblesChannel::extract_audio_attachment(&data).is_none());
    }

    #[test]
    fn extract_audio_attachment_skips_non_audio() {
        let data = serde_json::json!({
            "attachments": [{
                "guid": "p:0/abc",
                "mimeType": "image/png",
                "transferName": "photo.png"
            }]
        });
        assert!(BlueBubblesChannel::extract_audio_attachment(&data).is_none());
    }

    #[test]
    fn extract_audio_attachment_returns_first_audio_only() {
        // Multiple attachments: first image, then audio — must return the audio.
        let data = serde_json::json!({
            "attachments": [
                { "guid": "p:0/img", "mimeType": "image/jpeg", "transferName": "photo.jpg" },
                { "guid": "p:0/audio", "mimeType": "audio/mpeg", "transferName": "voice.mp3" }
            ]
        });
        let att = BlueBubblesChannel::extract_audio_attachment(&data).unwrap();
        assert_eq!(att.guid, "p:0/audio");
    }

    // -- emoji_to_bb_tapback tests --

    #[test]
    fn emoji_to_bb_tapback_maps_heart_to_love() {
        assert_eq!(emoji_to_bb_tapback("❤️"), Some("love"));
        assert_eq!(emoji_to_bb_tapback("love"), Some("love"));
    }

    #[test]
    fn emoji_to_bb_tapback_maps_thumbs_up_to_like() {
        assert_eq!(emoji_to_bb_tapback("👍"), Some("like"));
        assert_eq!(emoji_to_bb_tapback("+1"), Some("like"));
    }

    #[test]
    fn emoji_to_bb_tapback_maps_thumbs_down_to_dislike() {
        assert_eq!(emoji_to_bb_tapback("👎"), Some("dislike"));
        assert_eq!(emoji_to_bb_tapback("-1"), Some("dislike"));
    }

    #[test]
    fn emoji_to_bb_tapback_maps_laugh() {
        assert_eq!(emoji_to_bb_tapback("😂"), Some("laugh"));
        assert_eq!(emoji_to_bb_tapback("haha"), Some("laugh"));
    }

    #[test]
    fn emoji_to_bb_tapback_maps_emphasize() {
        assert_eq!(emoji_to_bb_tapback("‼️"), Some("emphasize"));
        assert_eq!(emoji_to_bb_tapback("emphasize"), Some("emphasize"));
    }

    #[test]
    fn emoji_to_bb_tapback_maps_question() {
        assert_eq!(emoji_to_bb_tapback("❓"), Some("question"));
        assert_eq!(emoji_to_bb_tapback("question"), Some("question"));
    }

    #[test]
    fn emoji_to_bb_tapback_returns_none_for_unknown() {
        assert_eq!(emoji_to_bb_tapback("🎉"), None);
        assert_eq!(emoji_to_bb_tapback("hello"), None);
        assert_eq!(emoji_to_bb_tapback(""), None);
    }

    // -- parse_tapback_event / updated-message tests --

    #[test]
    fn bluebubbles_parse_tapback_love() {
        let ch = make_open_channel();
        let payload = serde_json::json!({
            "type": "updated-message",
            "data": {
                "guid": "p:0/tapback-guid",
                "associatedMessageType": 2000,
                "associatedMessageGuid": "p:0/original",
                "isFromMe": false,
                "dateCreated": 1_708_987_654_000_u64,
                "handle": { "address": "+1_234_567_890" },
                "chats": [{ "guid": "iMessage;-;+1_234_567_890" }]
            }
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert!(
            msgs[0].content.contains("Loved"),
            "expected 'Loved' in tapback content: {}",
            msgs[0].content
        );
        assert_eq!(msgs[0].sender, "+1_234_567_890");
    }

    #[test]
    fn bluebubbles_parse_tapback_like_on_bot_message() {
        let ch = make_open_channel();
        // Seed the bot's message in the from-me cache.
        ch.cache_from_me("p:0/original", "iMessage;-;+1_234_567_890", "Great idea!");

        let payload = serde_json::json!({
            "type": "updated-message",
            "data": {
                "guid": "p:0/tapback-guid",
                "associatedMessageType": 2001,
                "associatedMessageGuid": "p:0/original",
                "isFromMe": false,
                "dateCreated": 1_708_987_654_000_u64,
                "handle": { "address": "+1_234_567_890" },
                "chats": [{ "guid": "iMessage;-;+1_234_567_890" }]
            }
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert_eq!(msgs.len(), 1);
        assert!(
            msgs[0].content.contains("Liked"),
            "expected 'Liked' in tapback content: {}",
            msgs[0].content
        );
        assert!(
            msgs[0].content.contains("Great idea!"),
            "expected quoted bot message body in content: {}",
            msgs[0].content
        );
    }

    #[test]
    fn bluebubbles_parse_tapback_removal_ignored() {
        // associatedMessageType 3000–3005 = removal; should produce no messages.
        let ch = make_open_channel();
        let payload = serde_json::json!({
            "type": "updated-message",
            "data": {
                "guid": "p:0/removal",
                "associatedMessageType": 3000,
                "isFromMe": false,
                "dateCreated": 1_708_987_654_000_u64,
                "handle": { "address": "+1_234_567_890" },
                "chats": [{ "guid": "iMessage;-;+1_234_567_890" }]
            }
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(
            msgs.is_empty(),
            "tapback removals should produce no messages"
        );
    }

    #[test]
    fn bluebubbles_parse_tapback_from_me_ignored() {
        let ch = make_open_channel();
        let payload = serde_json::json!({
            "type": "updated-message",
            "data": {
                "guid": "p:0/my-tapback",
                "associatedMessageType": 2000,
                "isFromMe": true,
                "dateCreated": 1_708_987_654_000_u64,
                "handle": { "address": "+1_234_567_890" },
                "chats": [{ "guid": "iMessage;-;+1_234_567_890" }]
            }
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(msgs.is_empty(), "fromMe tapbacks should be ignored");
    }

    #[test]
    fn bluebubbles_parse_tapback_unauthorized_sender_ignored() {
        let ch = make_channel(); // only allows +1_234_567_890
        let payload = serde_json::json!({
            "type": "updated-message",
            "data": {
                "guid": "p:0/unauthorized-tapback",
                "associatedMessageType": 2001,
                "isFromMe": false,
                "dateCreated": 1_708_987_654_000_u64,
                "handle": { "address": "+9_999_999_999" },
                "chats": [{ "guid": "iMessage;-;+9_999_999_999" }]
            }
        });
        let msgs = ch.parse_webhook_payload(&payload);
        assert!(
            msgs.is_empty(),
            "tapbacks from unauthorized senders must be ignored"
        );
    }
}
