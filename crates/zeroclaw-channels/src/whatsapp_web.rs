//! WhatsApp Web channel using wa-rs (native Rust implementation)
//!
//! This channel provides direct WhatsApp Web integration with:
//! - QR code and pair code linking
//! - End-to-end encryption via Signal Protocol
//! - Full Baileys parity (groups, media, presence, reactions, editing/deletion)
//!
//! # Feature Flag
//!
//! This channel requires the `whatsapp-web` feature flag:
//! ```sh
//! cargo build --features whatsapp-web
//! ```
//!
//! # Configuration
//!
//! ```toml
//! [channels_config.whatsapp]
//! session_path = "~/.zeroclaw/whatsapp-session.db"  # Required for Web mode
//! pair_phone = "15551234567"  # Optional: for pair code linking
//! allowed_numbers = ["+1234567890", "*"]  # Same as Cloud API
//! ```
//!
//! # Runtime Negotiation
//!
//! This channel is automatically selected when `session_path` is set in the config.
//! The Cloud API channel is used when `phone_number_id` is set.

use super::whatsapp_storage::RusqliteStore;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use parking_lot::Mutex;
use std::path::Path;
use std::sync::Arc;
use tokio::select;
use wa_rs_proto::whatsapp::device_props::PlatformType;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

/// WhatsApp Web channel using wa-rs with custom rusqlite storage
///
/// # Status: Functional Implementation
///
/// This implementation uses the wa-rs Bot with our custom RusqliteStore backend.
///
/// # Configuration
///
/// ```toml
/// [channels_config.whatsapp]
/// session_path = "~/.zeroclaw/whatsapp-session.db"
/// pair_phone = "15551234567"  # Optional
/// allowed_numbers = ["+1234567890", "*"]
/// ```
#[cfg(feature = "whatsapp-web")]
pub struct WhatsAppWebChannel {
    /// Session database path
    session_path: String,
    /// Phone number for pair code linking (optional)
    pair_phone: Option<String>,
    /// Custom pair code (optional)
    pair_code: Option<String>,
    /// Allowed phone numbers (E.164 format) or "*" for all
    allowed_numbers: Vec<String>,
    /// When true, only respond to messages that @-mention the bot in groups
    mention_only: bool,
    /// Bot phone number (digits only), resolved from pair_phone or device identity at runtime
    bot_phone: Arc<Mutex<Option<String>>>,
    /// Usage mode (business vs personal policy filtering)
    mode: zeroclaw_config::schema::WhatsAppWebMode,
    /// DM policy when mode = personal
    dm_policy: zeroclaw_config::schema::WhatsAppChatPolicy,
    /// Group policy when mode = personal
    group_policy: zeroclaw_config::schema::WhatsAppChatPolicy,
    /// Whether to always respond in self-chat when mode = personal
    self_chat_mode: bool,
    /// Bot handle for shutdown
    bot_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Client handle for sending messages and typing indicators
    client: Arc<Mutex<Option<Arc<wa_rs::Client>>>>,
    /// Message sender channel
    tx: Arc<Mutex<Option<tokio::sync::mpsc::Sender<ChannelMessage>>>>,
    /// Voice transcription (STT) config
    transcription: Option<zeroclaw_config::schema::TranscriptionConfig>,
    transcription_manager: Option<std::sync::Arc<super::transcription::TranscriptionManager>>,
    /// Text-to-speech config for voice replies
    tts_config: Option<zeroclaw_config::schema::TtsConfig>,
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
}

impl WhatsAppWebChannel {
    /// Create a new WhatsApp Web channel
    ///
    /// # Arguments
    ///
    /// * `session_path` - Path to the SQLite session database
    /// * `pair_phone` - Optional phone number for pair code linking (format: "15551234567")
    /// * `pair_code` - Optional custom pair code (leave empty for auto-generated)
    /// * `allowed_numbers` - Phone numbers allowed to interact (E.164 format) or "*" for all
    /// * `mode` - Usage mode (business or personal)
    /// * `dm_policy` - DM policy when mode = personal
    /// * `group_policy` - Group policy when mode = personal
    /// * `mention_only` - When true, only respond to group messages that @-mention the bot
    /// * `self_chat_mode` - Whether to always respond in self-chat when mode = personal
    #[cfg(feature = "whatsapp-web")]
    pub fn new(
        session_path: String,
        pair_phone: Option<String>,
        pair_code: Option<String>,
        allowed_numbers: Vec<String>,
        mention_only: bool,
        mode: zeroclaw_config::schema::WhatsAppWebMode,
        dm_policy: zeroclaw_config::schema::WhatsAppChatPolicy,
        group_policy: zeroclaw_config::schema::WhatsAppChatPolicy,
        self_chat_mode: bool,
    ) -> Self {
        // Seed bot_phone from pair_phone (digits only)
        let bot_phone = pair_phone
            .as_ref()
            .map(|p| p.chars().filter(|c| c.is_ascii_digit()).collect::<String>())
            .filter(|digits| !digits.is_empty());

        if mention_only && bot_phone.is_none() {
            tracing::warn!(
                "WhatsApp Web: mention_only enabled but pair_phone not set. \
                Bot identity will be resolved after connection. Group messages \
                will be skipped until identity is known."
            );
        }

        Self {
            session_path,
            pair_phone,
            pair_code,
            allowed_numbers,
            mention_only,
            bot_phone: Arc::new(Mutex::new(bot_phone)),
            mode,
            dm_policy,
            group_policy,
            self_chat_mode,
            bot_handle: Arc::new(Mutex::new(None)),
            client: Arc::new(Mutex::new(None)),
            tx: Arc::new(Mutex::new(None)),
            transcription: None,
            transcription_manager: None,
            tts_config: None,
            pending_voice: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            voice_chats: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            dm_mention_patterns: Arc::new(Vec::new()),
            group_mention_patterns: Arc::new(Vec::new()),
        }
    }

    /// Configure voice transcription (STT) for incoming voice notes.
    #[cfg(feature = "whatsapp-web")]
    pub fn with_transcription(
        mut self,
        config: zeroclaw_config::schema::TranscriptionConfig,
    ) -> Self {
        if !config.enabled {
            return self;
        }
        match super::transcription::TranscriptionManager::new(&config) {
            Ok(m) => {
                self.transcription_manager = Some(std::sync::Arc::new(m));
                self.transcription = Some(config);
            }
            Err(e) => {
                tracing::warn!(
                    "transcription manager init failed, voice transcription disabled: {e}"
                );
            }
        }
        self
    }

    /// Configure text-to-speech for outgoing voice replies.
    #[cfg(feature = "whatsapp-web")]
    pub fn with_tts(mut self, config: zeroclaw_config::schema::TtsConfig) -> Self {
        if config.enabled {
            self.tts_config = Some(config);
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
        Self::is_number_allowed_for_list(&self.allowed_numbers, phone)
    }

    /// Check whether a phone number is allowed against a provided allowlist.
    #[cfg(feature = "whatsapp-web")]
    fn is_number_allowed_for_list(allowed_numbers: &[String], phone: &str) -> bool {
        if allowed_numbers.iter().any(|entry| entry.trim() == "*") {
            return true;
        }

        let Some(phone_norm) = Self::normalize_phone_token(phone) else {
            return false;
        };

        allowed_numbers.iter().any(|entry| {
            Self::normalize_phone_token(entry)
                .as_deref()
                .is_some_and(|allowed_norm| allowed_norm == phone_norm)
        })
    }

    /// Normalize a phone-like token to canonical E.164 (`+<digits>`).
    ///
    /// Accepts raw numbers, `+` numbers, and JIDs (uses the user part before `@`).
    #[cfg(feature = "whatsapp-web")]
    fn normalize_phone_token(value: &str) -> Option<String> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return None;
        }

        let user_part = trimmed
            .split_once('@')
            .map(|(user, _)| user)
            .unwrap_or(trimmed)
            .trim();

        let digits: String = user_part.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            None
        } else {
            Some(format!("+{digits}"))
        }
    }

    /// Build normalized sender candidates from sender JID, optional alt JID, and optional LID->PN mapping.
    #[cfg(feature = "whatsapp-web")]
    fn sender_phone_candidates(
        sender: &wa_rs_binary::jid::Jid,
        sender_alt: Option<&wa_rs_binary::jid::Jid>,
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

        let qr = qrcode::QrCode::new(payload.as_bytes())
            .map_err(|err| anyhow!("Failed to encode WhatsApp Web QR payload: {err}"))?;

        Ok(qr
            .render::<qrcode::render::unicode::Dense1x2>()
            .quiet_zone(true)
            .build())
    }

    /// Convert a recipient to a wa-rs JID.
    ///
    /// Supports:
    /// - Full JIDs (e.g. "12345@s.whatsapp.net")
    /// - E.164-like numbers (e.g. "+1234567890")
    #[cfg(feature = "whatsapp-web")]
    fn recipient_to_jid(&self, recipient: &str) -> Result<wa_rs_binary::jid::Jid> {
        let trimmed = recipient.trim();
        if trimmed.is_empty() {
            anyhow::bail!("Recipient cannot be empty");
        }

        if trimmed.contains('@') {
            return trimmed
                .parse::<wa_rs_binary::jid::Jid>()
                .map_err(|e| anyhow!("Invalid WhatsApp JID `{trimmed}`: {e}"));
        }

        let digits: String = trimmed.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            anyhow::bail!("Recipient `{trimmed}` does not contain a valid phone number");
        }

        Ok(wa_rs_binary::jid::Jid::pn(digits))
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

    /// Return the session file paths to remove (primary + WAL + SHM sidecars).
    fn session_file_paths(expanded_session_path: &str) -> [String; 3] {
        [
            expanded_session_path.to_string(),
            format!("{expanded_session_path}-wal"),
            format!("{expanded_session_path}-shm"),
        ]
    }

    /// Attempt to download and transcribe a WhatsApp voice note.
    ///
    /// Returns `None` if transcription is disabled, download fails, or
    /// transcription fails (all logged as warnings).
    #[cfg(feature = "whatsapp-web")]
    async fn try_transcribe_voice_note(
        client: &wa_rs::Client,
        audio: &wa_rs_proto::whatsapp::message::AudioMessage,
        transcription_config: Option<&zeroclaw_config::schema::TranscriptionConfig>,
        transcription_manager: Option<&super::transcription::TranscriptionManager>,
    ) -> Option<String> {
        let config = transcription_config?;
        let manager = transcription_manager?;

        // Enforce duration limit
        if let Some(seconds) = audio.seconds
            && u64::from(seconds) > config.max_duration_secs
        {
            tracing::info!(
                "WhatsApp Web: skipping voice note ({}s exceeds {}s limit)",
                seconds,
                config.max_duration_secs
            );
            return None;
        }

        // Download the encrypted audio
        use wa_rs::download::Downloadable;
        let audio_data = match client.download(audio as &dyn Downloadable).await {
            Ok(data) => data,
            Err(e) => {
                tracing::warn!("WhatsApp Web: failed to download voice note: {e}");
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

        tracing::info!(
            "WhatsApp Web: transcribing voice note ({} bytes, file={})",
            audio_data.len(),
            file_name
        );

        match manager.transcribe(&audio_data, file_name).await {
            Ok(text) if text.trim().is_empty() => {
                tracing::info!("WhatsApp Web: voice transcription returned empty text, skipping");
                None
            }
            Ok(text) => {
                tracing::info!(
                    "WhatsApp Web: voice note transcribed ({} chars)",
                    text.len()
                );
                Some(text)
            }
            Err(e) => {
                tracing::warn!("WhatsApp Web: voice transcription failed: {e}");
                None
            }
        }
    }

    /// Synthesize text to speech and send as a WhatsApp voice note (static version for spawned tasks).
    #[cfg(feature = "whatsapp-web")]
    async fn synthesize_voice_static(
        client: &wa_rs::Client,
        to: &wa_rs_binary::jid::Jid,
        text: &str,
        tts_config: &zeroclaw_config::schema::TtsConfig,
    ) -> Result<()> {
        let tts_manager = super::tts::TtsManager::new(tts_config)?;
        let audio_bytes = tts_manager.synthesize(text).await?;
        let audio_len = audio_bytes.len();
        tracing::info!("WhatsApp Web TTS: synthesized {} bytes of audio", audio_len);

        if audio_bytes.is_empty() {
            anyhow::bail!("TTS returned empty audio");
        }

        use wa_rs_core::download::MediaType;
        let upload = client
            .upload(audio_bytes, MediaType::Audio)
            .await
            .map_err(|e| anyhow!("Failed to upload TTS audio: {e}"))?;

        tracing::info!(
            "WhatsApp Web TTS: uploaded audio (url_len={}, file_length={})",
            upload.url.len(),
            upload.file_length
        );

        // Estimate duration: Opus at ~32kbps → bytes / 4000 ≈ seconds
        #[allow(clippy::cast_possible_truncation)]
        let estimated_seconds = std::cmp::max(1, (upload.file_length / 4000) as u32);

        let voice_msg = wa_rs_proto::whatsapp::Message {
            audio_message: Some(Box::new(wa_rs_proto::whatsapp::message::AudioMessage {
                url: Some(upload.url),
                direct_path: Some(upload.direct_path),
                media_key: Some(upload.media_key),
                file_enc_sha256: Some(upload.file_enc_sha256),
                file_sha256: Some(upload.file_sha256),
                file_length: Some(upload.file_length),
                mimetype: Some("audio/ogg; codecs=opus".to_string()),
                ptt: Some(true),
                seconds: Some(estimated_seconds),
                ..Default::default()
            })),
            ..Default::default()
        };

        Box::pin(client.send_message(to.clone(), voice_msg))
            .await
            .map_err(|e| anyhow!("Failed to send voice note: {e}"))?;
        tracing::info!(
            "WhatsApp Web TTS: sent voice note ({} bytes, ~{}s)",
            audio_len,
            estimated_seconds
        );
        Ok(())
    }

    // ── Mention detection helpers (used when mention_only is enabled) ──

    /// Extract digits from a JID string (e.g. "919211916069@s.whatsapp.net" -> "919211916069").
    #[cfg(feature = "whatsapp-web")]
    fn jid_digits(jid: &str) -> String {
        let user_part = jid.split_once('@').map(|(u, _)| u).unwrap_or(jid);
        user_part.chars().filter(|c| c.is_ascii_digit()).collect()
    }

    /// Extract mentioned JIDs from the base (unwrapped) message's context_info.
    ///
    /// Uses `get_base_message()` to see through ephemeral/view-once/edited/document wrappers,
    /// matching the same unwrapping that `text_content()` performs.
    ///
    /// NOTE: Only checks `extended_text_message.context_info`. Media messages (image, video,
    /// document) carry mentions in their own `context_info`, but `text_content()` already
    /// ignores captions so those messages are filtered out upstream as empty text.
    #[cfg(feature = "whatsapp-web")]
    fn extract_mentioned_jids(msg: &wa_rs_proto::whatsapp::Message) -> Vec<String> {
        use wa_rs_core::proto_helpers::MessageExt;
        let base = msg.get_base_message();

        if let Some(ref ext) = base.extended_text_message
            && let Some(ref ctx) = ext.context_info
            && !ctx.mentioned_jid.is_empty()
        {
            return ctx.mentioned_jid.clone();
        }

        Vec::new()
    }

    /// Check whether the bot is mentioned -- either structurally or via text fallback.
    #[cfg(feature = "whatsapp-web")]
    fn contains_bot_mention(text: &str, mentioned_jids: &[String], bot_phone: &str) -> bool {
        // 1. Structured: check if any mentioned_jid's digits match the bot's phone digits
        for jid in mentioned_jids {
            let digits = Self::jid_digits(jid);
            if !digits.is_empty() && digits == bot_phone {
                return true;
            }
        }

        // 2. Text fallback: word-boundary-aware match for @<bot_digits>.
        //    Scan all occurrences -- an earlier prefix false-match must not mask a later real mention.
        let pattern = format!("@{bot_phone}");
        let mut search_from = 0;
        while let Some(rel_pos) = text[search_from..].find(&pattern) {
            let pos = search_from + rel_pos;
            let after_idx = pos + pattern.len();
            // Leading boundary: @ must be preceded by whitespace or start-of-string
            let leading_ok = pos == 0
                || text[..pos]
                    .chars()
                    .next_back()
                    .is_none_or(|ch| !ch.is_ascii_alphanumeric());
            // Trailing boundary: character after digits must not be a digit
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

    /// Strip text-based @<bot_phone> mention from the message, collapse whitespace.
    /// Returns None if the result is empty after stripping.
    #[cfg(feature = "whatsapp-web")]
    fn normalize_incoming_content(text: &str, bot_phone: &str) -> Option<String> {
        let pattern = format!("@{bot_phone}");
        let mut result = String::with_capacity(text.len());
        let mut remaining = text;

        while let Some(pos) = remaining.find(&pattern) {
            let after = pos + pattern.len();
            let leading_ok = pos == 0
                || remaining[..pos]
                    .chars()
                    .next_back()
                    .is_none_or(|ch| !ch.is_ascii_alphanumeric());
            let trailing_ok = remaining[after..]
                .chars()
                .next()
                .is_none_or(|ch| !ch.is_ascii_digit());
            if leading_ok && trailing_ok {
                result.push_str(&remaining[..pos]);
                remaining = &remaining[after..];
            } else {
                result.push_str(&remaining[..after]);
                remaining = &remaining[after..];
            }
        }
        result.push_str(remaining);

        let normalized: String = result.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.is_empty() {
            None
        } else {
            Some(normalized)
        }
    }

    /// Upload a local file and send it as a native WhatsApp media message.
    #[cfg(feature = "whatsapp-web")]
    #[allow(dead_code)] // WIP: not yet wired into send path
    async fn send_wa_attachment(
        client: &wa_rs::Client,
        to: &wa_rs_binary::jid::Jid,
        attachment: &WaAttachment,
    ) -> Result<()> {
        let target = attachment.target.trim();
        let path = Path::new(target);

        if !path.exists() {
            anyhow::bail!("attachment path not found: {target}");
        }

        let file_bytes = tokio::fs::read(path)
            .await
            .map_err(|e| anyhow!("failed to read attachment file {target}: {e}"))?;
        if file_bytes.is_empty() {
            anyhow::bail!("attachment file is empty: {target}");
        }

        let media_type = wa_media_type(attachment.kind);
        let upload = client
            .upload(file_bytes, media_type)
            .await
            .map_err(|e| anyhow!("WhatsApp upload failed for {target}: {e}"))?;

        let mimetype = mime_from_path(path).to_string();
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();

        let outgoing = match attachment.kind {
            WaAttachmentKind::Image => wa_rs_proto::whatsapp::Message {
                image_message: Some(Box::new(wa_rs_proto::whatsapp::message::ImageMessage {
                    url: Some(upload.url),
                    direct_path: Some(upload.direct_path),
                    media_key: Some(upload.media_key),
                    file_enc_sha256: Some(upload.file_enc_sha256),
                    file_sha256: Some(upload.file_sha256),
                    file_length: Some(upload.file_length),
                    mimetype: Some(mimetype),
                    ..Default::default()
                })),
                ..Default::default()
            },
            WaAttachmentKind::Video => wa_rs_proto::whatsapp::Message {
                video_message: Some(Box::new(wa_rs_proto::whatsapp::message::VideoMessage {
                    url: Some(upload.url),
                    direct_path: Some(upload.direct_path),
                    media_key: Some(upload.media_key),
                    file_enc_sha256: Some(upload.file_enc_sha256),
                    file_sha256: Some(upload.file_sha256),
                    file_length: Some(upload.file_length),
                    mimetype: Some(mimetype),
                    ..Default::default()
                })),
                ..Default::default()
            },
            WaAttachmentKind::Audio | WaAttachmentKind::Voice => {
                let is_voice = attachment.kind == WaAttachmentKind::Voice;
                #[allow(clippy::cast_possible_truncation)]
                let estimated_seconds = std::cmp::max(1, (upload.file_length / 4000) as u32);
                wa_rs_proto::whatsapp::Message {
                    audio_message: Some(Box::new(wa_rs_proto::whatsapp::message::AudioMessage {
                        url: Some(upload.url),
                        direct_path: Some(upload.direct_path),
                        media_key: Some(upload.media_key),
                        file_enc_sha256: Some(upload.file_enc_sha256),
                        file_sha256: Some(upload.file_sha256),
                        file_length: Some(upload.file_length),
                        mimetype: Some(mimetype),
                        ptt: Some(is_voice),
                        seconds: Some(estimated_seconds),
                        ..Default::default()
                    })),
                    ..Default::default()
                }
            }
            WaAttachmentKind::Document => wa_rs_proto::whatsapp::Message {
                document_message: Some(Box::new(wa_rs_proto::whatsapp::message::DocumentMessage {
                    url: Some(upload.url),
                    direct_path: Some(upload.direct_path),
                    media_key: Some(upload.media_key),
                    file_enc_sha256: Some(upload.file_enc_sha256),
                    file_sha256: Some(upload.file_sha256),
                    file_length: Some(upload.file_length),
                    mimetype: Some(mimetype),
                    file_name: Some(file_name.clone()),
                    title: Some(file_name),
                    ..Default::default()
                })),
                ..Default::default()
            },
        };

        Box::pin(client.send_message(to.clone(), outgoing))
            .await
            .map_err(|e| anyhow!("WhatsApp send media failed for {target}: {e}"))?;

        tracing::info!(
            kind = ?attachment.kind,
            path = %target,
            "WhatsApp Web: sent media attachment"
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Media-attachment marker parsing (mirrors Telegram's parse_attachment_markers)
// ---------------------------------------------------------------------------

/// Supported media attachment kinds for WhatsApp Web outgoing messages.
#[cfg(feature = "whatsapp-web")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // WIP: used by send_wa_attachment, not yet wired into send path
enum WaAttachmentKind {
    Image,
    Document,
    Video,
    Audio,
    Voice,
}

#[cfg(feature = "whatsapp-web")]
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // WIP: used by send_wa_attachment, not yet wired into send path
struct WaAttachment {
    kind: WaAttachmentKind,
    target: String,
}

#[cfg(feature = "whatsapp-web")]
impl WaAttachmentKind {
    #[allow(dead_code)] // WIP: used by parse_attachment_markers
    fn from_marker(marker: &str) -> Option<Self> {
        match marker.trim().to_ascii_uppercase().as_str() {
            "IMAGE" | "PHOTO" => Some(Self::Image),
            "DOCUMENT" | "FILE" => Some(Self::Document),
            "VIDEO" => Some(Self::Video),
            "AUDIO" => Some(Self::Audio),
            "VOICE" => Some(Self::Voice),
            _ => None,
        }
    }
}

/// Find the closing `]` that matches an already-consumed opening `[`.
#[cfg(feature = "whatsapp-web")]
#[allow(dead_code)] // WIP: used by parse_attachment_markers
fn find_matching_close(s: &str) -> Option<usize> {
    let mut depth = 1usize;
    for (i, ch) in s.char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Extract `[KIND:target]` media markers from a message, returning cleaned text
/// and a list of attachments. Unknown markers are left in the text verbatim.
#[cfg(feature = "whatsapp-web")]
#[allow(dead_code)] // WIP: not yet wired into send path
fn parse_attachment_markers(message: &str) -> (String, Vec<WaAttachment>) {
    let mut cleaned = String::with_capacity(message.len());
    let mut attachments = Vec::new();
    let mut cursor = 0;

    while cursor < message.len() {
        let Some(open_rel) = message[cursor..].find('[') else {
            cleaned.push_str(&message[cursor..]);
            break;
        };

        let open = cursor + open_rel;
        cleaned.push_str(&message[cursor..open]);

        let Some(close_rel) = find_matching_close(&message[open + 1..]) else {
            cleaned.push_str(&message[open..]);
            break;
        };

        let close = open + 1 + close_rel;
        let marker = &message[open + 1..close];

        let parsed = marker.split_once(':').and_then(|(kind, target)| {
            let kind = WaAttachmentKind::from_marker(kind)?;
            let target = target.trim();
            if target.is_empty() {
                return None;
            }
            Some(WaAttachment {
                kind,
                target: target.to_string(),
            })
        });

        if let Some(attachment) = parsed {
            attachments.push(attachment);
        } else {
            cleaned.push_str(&message[open..=close]);
        }

        cursor = close + 1;
    }

    (cleaned.trim().to_string(), attachments)
}

/// Guess a MIME type from a file extension for WhatsApp media uploads.
#[cfg(feature = "whatsapp-web")]
#[allow(dead_code)] // WIP: used by send_wa_attachment, not yet wired into send path
fn mime_from_path(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "ogg" | "oga" | "opus" => "audio/ogg; codecs=opus",
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "zip" => "application/zip",
        "gz" | "tar" => "application/gzip",
        _ => "application/octet-stream",
    }
}

/// Map our attachment kind to the wa-rs `MediaType` used for upload encryption.
#[cfg(feature = "whatsapp-web")]
#[allow(dead_code)] // WIP: used by send_wa_attachment, not yet wired into send path
fn wa_media_type(kind: WaAttachmentKind) -> wa_rs_core::download::MediaType {
    match kind {
        WaAttachmentKind::Image => wa_rs_core::download::MediaType::Image,
        WaAttachmentKind::Video => wa_rs_core::download::MediaType::Video,
        WaAttachmentKind::Audio | WaAttachmentKind::Voice => wa_rs_core::download::MediaType::Audio,
        WaAttachmentKind::Document => wa_rs_core::download::MediaType::Document,
    }
}

#[cfg(feature = "whatsapp-web")]
#[async_trait]
impl Channel for WhatsAppWebChannel {
    fn name(&self) -> &str {
        "whatsapp"
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
                tracing::warn!(
                    "WhatsApp Web: recipient {} not in allowed list",
                    message.recipient
                );
                return Ok(());
            }
        }

        let to = self.recipient_to_jid(&message.recipient)?;

        // Voice chat mode: send text normally AND queue a voice note of the
        // final answer. Only substantive messages (not tool outputs) are queued.
        // A debounce task waits 10s after the last substantive message, then
        // sends ONE voice note. Text in → text out. Voice in → text + voice out.
        let is_voice_chat = self
            .voice_chats
            .lock()
            .map(|vs| vs.contains(&message.recipient))
            .unwrap_or(false);

        if is_voice_chat && self.tts_config.is_some() {
            let content = &message.content;
            // Only queue substantive natural-language replies for voice.
            // Skip tool outputs: URLs, JSON, code blocks, errors, short status.
            let is_substantive = content.len() > 40
                && !content.starts_with("http")
                && !content.starts_with('{')
                && !content.starts_with('[')
                && !content.starts_with("Error")
                && !content.contains("```")
                && !content.contains("tool_call")
                && !content.contains("wttr.in");

            if is_substantive {
                if let Ok(mut pv) = self.pending_voice.lock() {
                    pv.insert(
                        message.recipient.clone(),
                        (content.clone(), std::time::Instant::now()),
                    );
                }

                let pending = self.pending_voice.clone();
                let voice_chats = self.voice_chats.clone();
                let client_clone = client.clone();
                let to_clone = to.clone();
                let recipient = message.recipient.clone();
                let tts_config = self.tts_config.clone().unwrap();
                tokio::spawn(async move {
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
                            &tts_config,
                        ))
                        .await
                        {
                            Ok(()) => {
                                tracing::info!(
                                    "WhatsApp Web: voice reply sent ({} chars)",
                                    text.len()
                                );
                            }
                            Err(e) => {
                                tracing::warn!("WhatsApp Web: TTS voice reply failed: {e}");
                            }
                        }
                    }
                });
            }
            // Fall through to send text normally (voice chat gets BOTH)
        }

        // Send text message
        let outgoing = wa_rs_proto::whatsapp::Message {
            conversation: Some(message.content.clone()),
            ..Default::default()
        };

        let message_id = client.send_message(to, outgoing).await?;
        tracing::debug!(
            "WhatsApp Web: sent text to {} (id: {})",
            message.recipient,
            message_id
        );
        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        // Store the sender channel for incoming messages
        *self.tx.lock() = Some(tx.clone());

        use wa_rs::bot::Bot;
        use wa_rs::pair_code::PairCodeOptions;
        use wa_rs::store::{Device, DeviceStore};
        use wa_rs_binary::jid::JidExt as _;
        use wa_rs_core::proto_helpers::MessageExt;
        use wa_rs_core::types::events::Event;
        use wa_rs_tokio_transport::TokioWebSocketTransportFactory;
        use wa_rs_ureq_http::UreqHttpClient;

        let retry_count = Arc::new(std::sync::atomic::AtomicU32::new(0));

        loop {
            let expanded_session_path = shellexpand::tilde(&self.session_path).to_string();

            tracing::info!(
                "WhatsApp Web channel starting (session: {})",
                expanded_session_path
            );

            // Initialize storage backend
            let storage = RusqliteStore::new(&expanded_session_path)?;
            let backend = Arc::new(storage);

            // Check if we have a saved device to load
            let mut device = Device::new(backend.clone());
            if backend.exists().await? {
                tracing::info!("WhatsApp Web: found existing session, loading device");
                if let Some(core_device) = backend.load().await? {
                    device.load_from_serializable(core_device);
                } else {
                    anyhow::bail!("Device exists but failed to load");
                }
            } else {
                tracing::info!(
                    "WhatsApp Web: no existing session, new device will be created during pairing"
                );
            };

            // Create transport factory
            let mut transport_factory = TokioWebSocketTransportFactory::new();
            if let Ok(ws_url) = std::env::var("WHATSAPP_WS_URL") {
                transport_factory = transport_factory.with_url(ws_url);
            }

            // Create HTTP client for media operations
            let http_client = UreqHttpClient::new();

            // Channel to signal logout from the event handler back to the listen loop.
            let (logout_tx, mut logout_rx) = tokio::sync::broadcast::channel::<()>(1);

            // Tracks whether Event::LoggedOut actually fired (vs task crash).
            let session_revoked = Arc::new(std::sync::atomic::AtomicBool::new(false));

            // Build the bot
            let tx_clone = tx.clone();
            let allowed_numbers = self.allowed_numbers.clone();
            let logout_tx_clone = logout_tx.clone();
            let retry_count_clone = retry_count.clone();
            let session_revoked_clone = session_revoked.clone();
            let transcription_config = self.transcription.clone();
            let transcription_mgr = self.transcription_manager.clone();
            let voice_chats = self.voice_chats.clone();
            let wa_mode = self.mode.clone();
            let wa_dm_policy = self.dm_policy.clone();
            let wa_group_policy = self.group_policy.clone();
            let wa_self_chat_mode = self.self_chat_mode;
            let mention_only = self.mention_only;
            let bot_phone_clone = self.bot_phone.clone();
            let wa_dm_mention_patterns = self.dm_mention_patterns.clone();
            let wa_group_mention_patterns = self.group_mention_patterns.clone();

            let mut builder = Bot::builder()
                .with_backend(backend)
                .with_transport_factory(transport_factory)
                .with_http_client(http_client)
                .with_device_props(
                    Some("ZeroClaw".to_string()),
                    None,
                    Some(PlatformType::Desktop),
                )
                .on_event(move |event, client| {
                    let tx_inner = tx_clone.clone();
                    let allowed_numbers = allowed_numbers.clone();
                    let logout_tx = logout_tx_clone.clone();
                    let retry_count = retry_count_clone.clone();
                    let session_revoked = session_revoked_clone.clone();
                    let transcription_config = transcription_config.clone();
                    let transcription_mgr = transcription_mgr.clone();
                    let voice_chats = voice_chats.clone();
                    let wa_mode = wa_mode.clone();
                    let wa_dm_policy = wa_dm_policy.clone();
                    let wa_group_policy = wa_group_policy.clone();
                    let bot_phone_inner = bot_phone_clone.clone();
                    let wa_dm_mention_patterns = wa_dm_mention_patterns.clone();
                    let wa_group_mention_patterns = wa_group_mention_patterns.clone();
                    async move {
                        match event {
                            Event::Message(msg, info) => {
                                let sender_jid = info.source.sender.clone();
                                let sender_alt = info.source.sender_alt.clone();
                                let sender = sender_jid.user().to_string();
                                let _is_group = info.source.chat.is_group();
                                let chat = info.source.chat.to_string();

                                let mapped_phone = if sender_jid.is_lid() {
                                    client.get_phone_number_from_lid(&sender_jid.user).await
                                } else {
                                    None
                                };
                                if sender_jid.is_lid() && mapped_phone.is_none() {
                                    tracing::warn!(
                                        "WhatsApp Web: LID→phone resolution returned None for sender {} — \
                                         allowlist entries phrased as the contact's phone number will not match. \
                                         Workaround: add the LID-form (+{}) to allowed_numbers; long-term, the \
                                         in-memory LID cache may not yet be populated for this contact.",
                                        sender_jid,
                                        sender_jid.user,
                                    );
                                }
                                let sender_candidates = Self::sender_phone_candidates(
                                    &sender_jid,
                                    sender_alt.as_ref(),
                                    mapped_phone.as_deref(),
                                );

                                let normalized = sender_candidates
                                    .iter()
                                    .find(|candidate| {
                                        Self::is_number_allowed_for_list(&allowed_numbers, candidate)
                                    })
                                    .cloned();

                                let is_group = info.source.is_group;

                                // Phone-based reply target for self-chat.
                                // LID JIDs (e.g. 76188559093817@lid) are internal
                                // identifiers that cannot receive messages; replies
                                // must go to the phone JID (digits@s.whatsapp.net).
                                let mut reply_target = chat.clone();

                                // ── Personal-mode chat-type policy filtering ──
                                if wa_mode == zeroclaw_config::schema::WhatsAppWebMode::Personal {
                                    // Self-chat: the chat JID user part matches
                                    // the sender's user part (message to "Notes
                                    // to Self").
                                    let sender_user = sender_jid.user();
                                    let chat_user = chat
                                        .split_once('@')
                                        .map(|(u, _)| u)
                                        .unwrap_or(&chat);
                                    let is_self_chat = !is_group && sender_user == chat_user && info.source.is_from_me;

                                    if is_self_chat {
                                        if !wa_self_chat_mode {
                                            tracing::debug!(
                                                "WhatsApp Web: ignoring self-chat message (self_chat_mode=false)"
                                            );
                                            return;
                                        }
                                        // self_chat_mode=true: always process, skip further policy checks.
                                        //
                                        // When the chat JID is LID-based, replies
                                        // won't be delivered. Convert to a phone
                                        // JID so the reply shows up in the self-chat.
                                        if info.source.chat.is_lid() {
                                            let phone_digits = normalized
                                                .as_ref()
                                                .map(|n| n.chars().filter(|c| c.is_ascii_digit()).collect::<String>())
                                                .filter(|d| !d.is_empty());
                                            if let Some(digits) = phone_digits {
                                                reply_target = format!("{digits}@s.whatsapp.net");
                                                tracing::debug!(
                                                    "WhatsApp Web: self-chat LID→phone reply target: {reply_target}"
                                                );
                                            }
                                        }
                                    } else if is_group {
                                        match wa_group_policy {
                                            zeroclaw_config::schema::WhatsAppChatPolicy::Ignore => {
                                                tracing::debug!(
                                                    "WhatsApp Web: ignoring group message (group_policy=ignore)"
                                                );
                                                return;
                                            }
                                            zeroclaw_config::schema::WhatsAppChatPolicy::All => {
                                                // allow unconditionally
                                            }
                                            zeroclaw_config::schema::WhatsAppChatPolicy::Allowlist => {
                                                if normalized.is_none() {
                                                    let lid_hint = if sender_jid.is_lid() {
                                                        " (sender is LID; if your allowlist contains the contact's phone number, ensure LID→phone resolution is succeeding — see preceding warning)"
                                                    } else {
                                                        ""
                                                    };
                                                    tracing::warn!(
                                                        "WhatsApp Web: message from unrecognized sender not in allowed list (candidates_count={}){}",
                                                        sender_candidates.len(),
                                                        lid_hint,
                                                    );
                                                    return;
                                                }
                                            }
                                        }
                                    } else {
                                        // DM (non-self)
                                        match wa_dm_policy {
                                            zeroclaw_config::schema::WhatsAppChatPolicy::Ignore => {
                                                tracing::debug!(
                                                    "WhatsApp Web: ignoring DM (dm_policy=ignore)"
                                                );
                                                return;
                                            }
                                            zeroclaw_config::schema::WhatsAppChatPolicy::All => {
                                                // allow unconditionally
                                            }
                                            zeroclaw_config::schema::WhatsAppChatPolicy::Allowlist => {
                                                if normalized.is_none() {
                                                    let lid_hint = if sender_jid.is_lid() {
                                                        " (sender is LID; if your allowlist contains the contact's phone number, ensure LID→phone resolution is succeeding — see preceding warning)"
                                                    } else {
                                                        ""
                                                    };
                                                    tracing::warn!(
                                                        "WhatsApp Web: message from unrecognized sender not in allowed list (candidates_count={}){}",
                                                        sender_candidates.len(),
                                                        lid_hint,
                                                    );
                                                    return;
                                                }
                                            }
                                        }
                                    }
                                }

                                let normalized = normalized.unwrap_or_else(|| sender.clone());

                                // Attempt voice note transcription (ptt = push-to-talk = voice note).
                                // When `transcribe_non_ptt_audio` is enabled in the transcription
                                // config, also transcribe forwarded / regular audio messages.
                                let voice_text = if let Some(ref audio) = msg.audio_message {
                                    let is_ptt = audio.ptt == Some(true);
                                    let non_ptt_enabled = transcription_config
                                        .as_ref()
                                        .is_some_and(|c| c.transcribe_non_ptt_audio);
                                    if is_ptt || non_ptt_enabled {
                                        Self::try_transcribe_voice_note(
                                            &client,
                                            audio,
                                            transcription_config.as_ref(),
                                            transcription_mgr.as_deref(),
                                        )
                                        .await
                                    } else {
                                        tracing::debug!(
                                            "WhatsApp Web: ignoring non-PTT audio message from {}",
                                            normalized
                                        );
                                        None
                                    }
                                } else {
                                    None
                                };

                                // Use transcribed voice text, or fall back to text content.
                                // Track whether this chat used a voice note so we reply in kind.
                                // We store the chat JID (reply_target) since that's what send() receives.
                                let content = if let Some(ref vt) = voice_text {
                                    if let Ok(mut vs) = voice_chats.lock() {
                                        vs.insert(chat.clone());
                                    }
                                    format!("[Voice] {vt}")
                                } else {
                                    if let Ok(mut vs) = voice_chats.lock() {
                                        vs.remove(&chat);
                                    }
                                    let text = msg.text_content().unwrap_or("");
                                    text.trim().to_string()
                                };

                                tracing::info!(
                                    "WhatsApp Web message received (sender_len={}, chat_len={}, content_len={})",
                                    sender.len(),
                                    chat.len(),
                                    content.len()
                                );
                                tracing::debug!(
                                    "WhatsApp Web message content: {}",
                                    content
                                );

                                if content.is_empty() {
                                    tracing::debug!(
                                        "WhatsApp Web: ignoring empty or non-text message from {}",
                                        normalized
                                    );
                                    return;
                                }

                                // mention_only: skip group messages without a bot mention
                                let content = if mention_only && is_group {
                                    let bot_phone = bot_phone_inner.lock();
                                    if let Some(ref bp) = *bot_phone {
                                        let mentioned_jids =
                                            Self::extract_mentioned_jids(&msg);
                                        if !Self::contains_bot_mention(
                                            &content,
                                            &mentioned_jids,
                                            bp,
                                        ) {
                                            tracing::debug!(
                                                "WhatsApp Web: ignoring group message without bot mention"
                                            );
                                            return;
                                        }
                                        match Self::normalize_incoming_content(
                                            &content, bp,
                                        ) {
                                            Some(c) => c,
                                            None => {
                                                tracing::debug!(
                                                    "WhatsApp Web: message empty after stripping mention"
                                                );
                                                return;
                                            }
                                        }
                                    } else {
                                        tracing::debug!(
                                            "WhatsApp Web: mention_only active but bot identity unknown, skipping group msg"
                                        );
                                        return;
                                    }
                                } else {
                                    content
                                };

                                // ── Mention-pattern gating ──
                                // Apply dm_mention_patterns for DMs and
                                // group_mention_patterns for group chats.
                                // When the applicable pattern set is non-empty,
                                // messages without a match are dropped and
                                // matched fragments are stripped.
                                let content =
                                    match super::whatsapp::WhatsAppChannel::apply_mention_gating(
                                        &wa_dm_mention_patterns,
                                        &wa_group_mention_patterns,
                                        &content,
                                        is_group,
                                    ) {
                                        Some(c) => c,
                                        None => {
                                            tracing::debug!(
                                                "WhatsApp Web: message from {normalized} did not match mention patterns, dropping"
                                            );
                                            return;
                                        }
                                    };

                                if let Err(e) = tx_inner
                                    .send(ChannelMessage {
                                        id: uuid::Uuid::new_v4().to_string(),
                                        channel: "whatsapp".to_string(),
                                        sender: normalized.clone(),
                                        // Reply to the originating chat JID (DM or group).
                                        // For self-chat with LID JIDs, this is the
                                        // resolved phone JID (see above).
                                        reply_target,
                                        content,
                                        timestamp: chrono::Utc::now().timestamp() as u64,
                                        thread_ts: None,
                                        interruption_scope_id: None,
                    attachments: vec![],
                                    })
                                    .await
                                {
                                    tracing::error!("Failed to send message to channel: {}", e);
                                }
                            }
                            Event::Connected(_) => {
                                tracing::info!("WhatsApp Web connected successfully");
                                WhatsAppWebChannel::reset_retry(&retry_count);
                                // Resolve bot identity from the device store
                                if mention_only {
                                    let device = client
                                        .persistence_manager()
                                        .get_device_snapshot()
                                        .await;
                                    if let Some(ref pn) = device.pn {
                                        let phone = pn.user();
                                        let digits: String = phone
                                            .chars()
                                            .filter(|c: &char| c.is_ascii_digit())
                                            .collect();
                                        if !digits.is_empty() {
                                            *bot_phone_inner.lock() = Some(digits.clone());
                                            tracing::info!(
                                                "WhatsApp Web: resolved bot identity from device: +{}",
                                                digits
                                            );
                                        }
                                    }
                                }
                            }
                            Event::LoggedOut(_) => {
                                session_revoked.store(true, std::sync::atomic::Ordering::Relaxed);
                                tracing::warn!(
                                    "WhatsApp Web was logged out — will clear session and reconnect"
                                );
                                let _ = logout_tx.send(());
                            }
                            Event::StreamError(stream_error) => {
                                tracing::error!("WhatsApp Web stream error: {:?}", stream_error);
                            }
                            Event::PairingCode { code, .. } => {
                                tracing::info!("WhatsApp Web pair code received");
                                tracing::info!(
                                    "Link your phone by entering this code in WhatsApp > Linked Devices"
                                );
                                eprintln!();
                                eprintln!("WhatsApp Web pair code: {code}");
                                eprintln!();
                            }
                            Event::PairingQrCode { code, .. } => {
                                tracing::info!(
                                    "WhatsApp Web QR code received (scan with WhatsApp > Linked Devices)"
                                );
                                match Self::render_pairing_qr(&code) {
                                    Ok(rendered) => {
                                        eprintln!();
                                        eprintln!(
                                            "WhatsApp Web QR code (scan in WhatsApp > Linked Devices):"
                                        );
                                        eprintln!("{rendered}");
                                        eprintln!();
                                    }
                                    Err(err) => {
                                        tracing::warn!(
                                            "WhatsApp Web: failed to render pairing QR in terminal: {}",
                                            err
                                        );
                                        eprintln!();
                                        eprintln!("WhatsApp Web QR payload: {code}");
                                        eprintln!();
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                });

            // Configure pair-code flow when a phone number is provided.
            if let Some(ref phone) = self.pair_phone {
                tracing::info!("WhatsApp Web: pair-code flow enabled for configured phone number");
                builder = builder.with_pair_code(PairCodeOptions {
                    phone_number: phone.clone(),
                    custom_code: self.pair_code.clone(),
                    ..Default::default()
                });
            } else if self.pair_code.is_some() {
                tracing::warn!(
                    "WhatsApp Web: pair_code is set but pair_phone is missing; pair code config is ignored"
                );
            }

            let mut bot = builder.build().await?;
            *self.client.lock() = Some(bot.client());

            // Run the bot
            let bot_handle = bot.run().await?;

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
                    tracing::info!("WhatsApp Web channel received Ctrl+C");
                    false
                }
            };

            *self.client.lock() = None;
            let handle = self.bot_handle.lock().take();
            if let Some(handle) = handle {
                handle.abort();
                // Await the aborted task so background I/O finishes before
                // we delete session files.
                let _ = handle.await;
            }

            // Drop bot/device so the SQLite connection is closed
            // before we remove session files (releases WAL/SHM locks).
            // `backend` was moved into the builder, so dropping `bot`
            // releases the last Arc reference to the storage backend.
            drop(bot);
            drop(device);

            if should_reconnect {
                let (attempts, exceeded) = Self::record_retry(&retry_count);
                if exceeded {
                    anyhow::bail!(
                        "WhatsApp Web: exceeded {} reconnect attempts, giving up",
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
                            Err(e) => tracing::warn!(
                                "WhatsApp Web: failed to remove session file {}: {e}",
                                path
                            ),
                        }
                    }
                    tracing::info!(
                        "WhatsApp Web: session files removed, restarting for QR pairing"
                    );
                } else {
                    tracing::warn!(
                        "WhatsApp Web: bot stopped without LoggedOut; reconnecting with existing session"
                    );
                }

                let delay = Self::compute_retry_delay(attempts);
                tracing::info!(
                    "WhatsApp Web: reconnecting in {}s (attempt {}/{})",
                    delay,
                    attempts,
                    Self::MAX_RETRIES
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

    async fn start_typing(&self, recipient: &str) -> Result<()> {
        let client = self.client.lock().clone();
        let Some(client) = client else {
            anyhow::bail!("WhatsApp Web client not connected. Initialize the bot first.");
        };

        if !Self::is_jid(recipient) {
            let normalized = self.normalize_phone(recipient);
            if !self.is_number_allowed(&normalized) {
                tracing::warn!(
                    "WhatsApp Web: typing target {} not in allowed list",
                    recipient
                );
                return Ok(());
            }
        }

        let to = self.recipient_to_jid(recipient)?;
        client
            .chatstate()
            .send_composing(&to)
            .await
            .map_err(|e| anyhow!("Failed to send typing state (composing): {e}"))?;

        tracing::debug!("WhatsApp Web: start typing for {}", recipient);
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
                tracing::warn!(
                    "WhatsApp Web: typing target {} not in allowed list",
                    recipient
                );
                return Ok(());
            }
        }

        let to = self.recipient_to_jid(recipient)?;
        client
            .chatstate()
            .send_paused(&to)
            .await
            .map_err(|e| anyhow!("Failed to send typing state (paused): {e}"))?;

        tracing::debug!("WhatsApp Web: stop typing for {}", recipient);
        Ok(())
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
        _allowed_numbers: Vec<String>,
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

    pub fn with_tts(self, _config: zeroclaw_config::schema::TtsConfig) -> Self {
        self
    }
}

#[cfg(not(feature = "whatsapp-web"))]
#[async_trait]
impl Channel for WhatsAppWebChannel {
    fn name(&self) -> &str {
        "whatsapp"
    }

    async fn send(&self, _message: &SendMessage) -> Result<()> {
        anyhow::bail!(
            "WhatsApp Web channel requires the 'whatsapp-web' feature. \
            Enable with: cargo build --features whatsapp-web"
        );
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        anyhow::bail!(
            "WhatsApp Web channel requires the 'whatsapp-web' feature. \
            Enable with: cargo build --features whatsapp-web"
        );
    }

    async fn health_check(&self) -> bool {
        false
    }

    async fn start_typing(&self, _recipient: &str) -> Result<()> {
        anyhow::bail!(
            "WhatsApp Web channel requires the 'whatsapp-web' feature. \
            Enable with: cargo build --features whatsapp-web"
        );
    }

    async fn stop_typing(&self, _recipient: &str) -> Result<()> {
        anyhow::bail!(
            "WhatsApp Web channel requires the 'whatsapp-web' feature. \
            Enable with: cargo build --features whatsapp-web"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "whatsapp-web")]
    use wa_rs_binary::jid::Jid;

    #[cfg(feature = "whatsapp-web")]
    fn make_channel() -> WhatsAppWebChannel {
        WhatsAppWebChannel::new(
            "/tmp/test-whatsapp.db".into(),
            None,
            None,
            vec!["+1234567890".into()],
            false,
            zeroclaw_config::schema::WhatsAppWebMode::default(),
            zeroclaw_config::schema::WhatsAppChatPolicy::default(),
            zeroclaw_config::schema::WhatsAppChatPolicy::default(),
            false,
        )
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_channel_name() {
        let ch = make_channel();
        assert_eq!(ch.name(), "whatsapp");
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_number_allowed_exact() {
        let ch = make_channel();
        assert!(ch.is_number_allowed("+1234567890"));
        assert!(!ch.is_number_allowed("+9876543210"));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_number_allowed_wildcard() {
        let ch = WhatsAppWebChannel::new(
            "/tmp/test.db".into(),
            None,
            None,
            vec!["*".into()],
            false,
            zeroclaw_config::schema::WhatsAppWebMode::default(),
            zeroclaw_config::schema::WhatsAppChatPolicy::default(),
            zeroclaw_config::schema::WhatsAppChatPolicy::default(),
            false,
        );
        assert!(ch.is_number_allowed("+1234567890"));
        assert!(ch.is_number_allowed("+9999999999"));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_number_denied_empty() {
        let ch = WhatsAppWebChannel::new(
            "/tmp/test.db".into(),
            None,
            None,
            vec![],
            false,
            zeroclaw_config::schema::WhatsAppWebMode::default(),
            zeroclaw_config::schema::WhatsAppChatPolicy::default(),
            zeroclaw_config::schema::WhatsAppChatPolicy::default(),
            false,
        );
        // Empty allowlist means "deny all" (matches channel-wide allowlist policy).
        assert!(!ch.is_number_allowed("+1234567890"));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_normalize_phone_adds_plus() {
        let ch = make_channel();
        assert_eq!(ch.normalize_phone("1234567890"), "+1234567890");
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_normalize_phone_preserves_plus() {
        let ch = make_channel();
        assert_eq!(ch.normalize_phone("+1234567890"), "+1234567890");
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_normalize_phone_from_jid() {
        let ch = make_channel();
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

    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn whatsapp_web_health_check_disconnected() {
        let ch = make_channel();
        assert!(!ch.health_check().await);
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

        let ch = make_channel().with_transcription(tc);
        assert!(ch.transcription.is_some());
        assert!(ch.transcription_manager.is_some());
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn with_transcription_ignores_when_disabled() {
        let tc = zeroclaw_config::schema::TranscriptionConfig::default(); // enabled = false
        let ch = make_channel().with_transcription(tc);
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
            "919211916069"
        ));
        assert!(WhatsAppWebChannel::contains_bot_mention(
            "hey check this",
            &jids,
            "919211916069"
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn contains_bot_mention_text_fallback() {
        let no_jids: Vec<String> = vec![];
        assert!(WhatsAppWebChannel::contains_bot_mention(
            "hey @919211916069 check this",
            &no_jids,
            "919211916069"
        ));
        assert!(WhatsAppWebChannel::contains_bot_mention(
            "hey @919211916069",
            &no_jids,
            "919211916069"
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn contains_bot_mention_prefix_false_positive() {
        let no_jids: Vec<String> = vec![];
        assert!(!WhatsAppWebChannel::contains_bot_mention(
            "hey @919211916069 check this",
            &no_jids,
            "91921191606"
        ));
        assert!(!WhatsAppWebChannel::contains_bot_mention(
            "hey @155512345678",
            &no_jids,
            "15551234567"
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn contains_bot_mention_no_match() {
        let no_jids: Vec<String> = vec![];
        assert!(!WhatsAppWebChannel::contains_bot_mention(
            "just a regular message",
            &no_jids,
            "919211916069"
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn contains_bot_mention_scans_past_prefix_false_match() {
        let no_jids: Vec<String> = vec![];
        assert!(WhatsAppWebChannel::contains_bot_mention(
            "@9192119160691 real @919211916069",
            &no_jids,
            "919211916069"
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn contains_bot_mention_rejects_embedded_at() {
        let no_jids: Vec<String> = vec![];
        assert!(!WhatsAppWebChannel::contains_bot_mention(
            "foo@919211916069 bar",
            &no_jids,
            "919211916069"
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn normalize_incoming_content_strips_mention() {
        assert_eq!(
            WhatsAppWebChannel::normalize_incoming_content(
                "@919211916069 what's the weather?",
                "919211916069"
            ),
            Some("what's the weather?".to_string())
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn normalize_incoming_content_strips_multiple() {
        assert_eq!(
            WhatsAppWebChannel::normalize_incoming_content(
                "@919211916069 hey @919211916069 hello",
                "919211916069"
            ),
            Some("hey hello".to_string())
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn normalize_incoming_content_returns_none_for_empty() {
        assert_eq!(
            WhatsAppWebChannel::normalize_incoming_content("@919211916069", "919211916069"),
            None
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn normalize_incoming_content_preserves_prefix_match() {
        assert_eq!(
            WhatsAppWebChannel::normalize_incoming_content("@155512345678 hello", "15551234567"),
            Some("@155512345678 hello".to_string())
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn normalize_incoming_content_ignores_embedded_at() {
        assert_eq!(
            WhatsAppWebChannel::normalize_incoming_content(
                "foo@919211916069 hello",
                "919211916069"
            ),
            Some("foo@919211916069 hello".to_string())
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn constructor_seeds_bot_phone_from_pair_phone() {
        let ch = WhatsAppWebChannel::new(
            "/tmp/test.db".into(),
            Some("919211916069".into()),
            None,
            vec!["*".into()],
            true,
            zeroclaw_config::schema::WhatsAppWebMode::default(),
            zeroclaw_config::schema::WhatsAppChatPolicy::default(),
            zeroclaw_config::schema::WhatsAppChatPolicy::default(),
            false,
        );
        assert_eq!(*ch.bot_phone.lock(), Some("919211916069".to_string()));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn constructor_no_pair_phone_leaves_bot_phone_none() {
        let ch = WhatsAppWebChannel::new(
            "/tmp/test.db".into(),
            None,
            None,
            vec!["*".into()],
            true,
            zeroclaw_config::schema::WhatsAppWebMode::default(),
            zeroclaw_config::schema::WhatsAppChatPolicy::default(),
            zeroclaw_config::schema::WhatsAppChatPolicy::default(),
            false,
        );
        assert_eq!(*ch.bot_phone.lock(), None);
    }

    // ---- Media attachment marker parsing tests ----

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn parse_attachment_markers_extracts_image_and_document() {
        let msg = "Here are files [IMAGE:/tmp/a.png] and [DOCUMENT:/tmp/b.pdf]";
        let (cleaned, attachments) = parse_attachment_markers(msg);

        assert_eq!(cleaned, "Here are files  and");
        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0].kind, WaAttachmentKind::Image);
        assert_eq!(attachments[0].target, "/tmp/a.png");
        assert_eq!(attachments[1].kind, WaAttachmentKind::Document);
        assert_eq!(attachments[1].target, "/tmp/b.pdf");
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn parse_attachment_markers_extracts_voice() {
        let msg = "Listen to this [VOICE:/tmp/note.ogg]";
        let (cleaned, attachments) = parse_attachment_markers(msg);

        assert_eq!(cleaned, "Listen to this");
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].kind, WaAttachmentKind::Voice);
        assert_eq!(attachments[0].target, "/tmp/note.ogg");
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn parse_attachment_markers_keeps_unknown_markers() {
        let msg = "Check [UNKNOWN:foo] this";
        let (cleaned, attachments) = parse_attachment_markers(msg);

        assert_eq!(cleaned, "Check [UNKNOWN:foo] this");
        assert!(attachments.is_empty());
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn parse_attachment_markers_no_markers() {
        let msg = "Just plain text";
        let (cleaned, attachments) = parse_attachment_markers(msg);

        assert_eq!(cleaned, "Just plain text");
        assert!(attachments.is_empty());
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn mime_from_path_returns_correct_types() {
        assert_eq!(
            mime_from_path(std::path::Path::new("/tmp/a.png")),
            "image/png"
        );
        assert_eq!(
            mime_from_path(std::path::Path::new("/tmp/b.pdf")),
            "application/pdf"
        );
        assert_eq!(
            mime_from_path(std::path::Path::new("/tmp/c.ogg")),
            "audio/ogg; codecs=opus"
        );
        assert_eq!(
            mime_from_path(std::path::Path::new("/tmp/d.mp4")),
            "video/mp4"
        );
        assert_eq!(
            mime_from_path(std::path::Path::new("/tmp/e.xyz")),
            "application/octet-stream"
        );
    }
}
