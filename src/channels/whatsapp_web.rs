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

use super::traits::{Channel, ChannelMessage, SendMessage};
use super::whatsapp_storage::RusqliteStore;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use parking_lot::Mutex;
use std::sync::Arc;
use tokio::select;

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
    /// In group chats, only respond when the bot is mentioned
    mention_only: bool,
    /// Bot handle for shutdown
    bot_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Client handle for sending messages and typing indicators
    client: Arc<Mutex<Option<Arc<wa_rs::Client>>>>,
    /// Message sender channel
    tx: Arc<Mutex<Option<tokio::sync::mpsc::Sender<ChannelMessage>>>>,
    /// Voice transcription (STT) config
    transcription: Option<crate::config::TranscriptionConfig>,
    /// Text-to-speech config for voice replies
    tts_config: Option<crate::config::TtsConfig>,
    /// Workspace directory for saving downloaded media (images, docs)
    workspace_dir: Option<std::path::PathBuf>,
    /// Chats awaiting a voice reply — maps chat JID to the latest substantive
    /// reply text. A background task debounces and sends the voice note after
    /// the agent finishes its turn (no new send() for 3 seconds).
    pending_voice:
        Arc<std::sync::Mutex<std::collections::HashMap<String, (String, std::time::Instant)>>>,
    /// Chats whose last incoming message was a voice note.
    voice_chats: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    /// Stream LLM responses as sequential message segments.
    streaming: bool,
    /// Minimum character count per streamed segment.
    streaming_chunk_size: usize,
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
    #[cfg(feature = "whatsapp-web")]
    pub fn new(
        session_path: String,
        pair_phone: Option<String>,
        pair_code: Option<String>,
        allowed_numbers: Vec<String>,
    ) -> Self {
        Self {
            session_path,
            pair_phone,
            pair_code,
            allowed_numbers,
            mention_only: false,
            bot_handle: Arc::new(Mutex::new(None)),
            client: Arc::new(Mutex::new(None)),
            tx: Arc::new(Mutex::new(None)),
            transcription: None,
            tts_config: None,
            workspace_dir: None,
            pending_voice: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            voice_chats: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            streaming: false,
            streaming_chunk_size: 200,
        }
    }

    /// Set mention_only mode: in group chats, only respond when mentioned.
    #[cfg(feature = "whatsapp-web")]
    pub fn with_mention_only(mut self, mention_only: bool) -> Self {
        self.mention_only = mention_only;
        self
    }

    /// Enable streaming: LLM responses are delivered as sequential message
    /// segments rather than a single response.
    #[cfg(feature = "whatsapp-web")]
    pub fn with_streaming(mut self, enabled: bool, chunk_size: usize) -> Self {
        self.streaming = enabled;
        self.streaming_chunk_size = chunk_size;
        self
    }

    /// Check if a JID represents a group chat (ends with @g.us).
    #[cfg(feature = "whatsapp-web")]
    fn is_group_chat(chat_jid: &str) -> bool {
        chat_jid.contains("@g.us")
    }

    /// Collect all `mentioned_jid` entries from the message's `context_info`.
    /// Checks every message sub-type that carries a `context_info` (text, image,
    /// video, audio, document, sticker, location, contact).
    #[cfg(feature = "whatsapp-web")]
    fn collect_mentioned_jids(msg: &wa_rs_proto::whatsapp::Message) -> Vec<&str> {
        use wa_rs_core::proto_helpers::MessageExt;

        let base = msg.get_base_message();
        let mut jids: Vec<&str> = Vec::new();

        macro_rules! gather {
            ($($field:ident),+ $(,)?) => {
                $(
                    if let Some(ref m) = base.$field {
                        if let Some(ref ctx) = m.context_info {
                            for jid in &ctx.mentioned_jid {
                                jids.push(jid.as_str());
                            }
                        }
                    }
                )+
            };
        }

        gather!(
            extended_text_message,
            image_message,
            video_message,
            audio_message,
            document_message,
            sticker_message,
            location_message,
            contact_message,
        );

        jids
    }

    /// Extract reply/quote context from a WhatsApp message.
    ///
    /// When a user replies to a previous message, the protobuf carries the
    /// original text inside `context_info.quoted_message`. This helper returns
    /// a human-readable prefix like `[Replying to: "original text"]` so the
    /// agent can see what message the user is responding to.
    #[cfg(feature = "whatsapp-web")]
    /// Extract reply context from a quoted message.
    /// Returns `(preview_text, quoted_stanza_id)`.
    fn extract_reply_context(
        msg: &wa_rs_proto::whatsapp::Message,
        workspace_dir: Option<&std::path::Path>,
    ) -> Option<(String, Option<String>)> {
        use wa_rs_core::proto_helpers::MessageExt;

        let base = msg.get_base_message();

        /// Try to extract a displayable preview from a quoted message.
        /// If `stanza_id` and `workspace_dir` are provided, look up the
        /// previously downloaded image to include its path.
        fn quoted_preview(
            quoted: &wa_rs_proto::whatsapp::Message,
            stanza_id: Option<&str>,
            workspace_dir: Option<&std::path::Path>,
        ) -> Option<String> {
            use wa_rs_core::proto_helpers::MessageExt as _;
            if let Some(text) = quoted.text_content() {
                if !text.is_empty() {
                    return Some(truncate_reply_preview(text, 200));
                }
            }
            if let Some(ref img) = quoted.image_message {
                // Try to find the previously downloaded image on disk.
                let image_path = stanza_id.and_then(|sid| {
                    WhatsAppWebChannel::find_image_by_stanza_id(workspace_dir, sid)
                });
                let caption = img.caption.as_deref().filter(|c| !c.trim().is_empty());
                return Some(match (image_path, caption) {
                    (Some(p), Some(c)) => {
                        format!("[IMAGE:{}] {}", p.display(), truncate_reply_preview(c, 180))
                    }
                    (Some(p), None) => format!("[IMAGE:{}]", p.display()),
                    (None, Some(c)) => {
                        format!("[Photo] {}", truncate_reply_preview(c, 180))
                    }
                    (None, None) => "[Photo]".to_string(),
                });
            }
            if let Some(ref vid) = quoted.video_message {
                return Some(
                    vid.caption
                        .as_deref()
                        .filter(|c| !c.trim().is_empty())
                        .map(|c| format!("[Video] {}", truncate_reply_preview(c, 180)))
                        .unwrap_or_else(|| "[Video]".to_string()),
                );
            }
            if let Some(ref doc) = quoted.document_message {
                return Some(
                    doc.file_name
                        .as_deref()
                        .map(|f| format!("[Document: {f}]"))
                        .unwrap_or_else(|| "[Document]".to_string()),
                );
            }
            if quoted.audio_message.is_some() {
                return Some("[Voice message]".to_string());
            }
            if quoted.sticker_message.is_some() {
                return Some("[Sticker]".to_string());
            }
            if quoted.location_message.is_some() {
                return Some("[Location]".to_string());
            }
            if quoted.contact_message.is_some() {
                return Some("[Contact]".to_string());
            }
            None
        }

        macro_rules! try_ctx {
            ($($field:ident),+ $(,)?) => {
                $(
                    if let Some(ref m) = base.$field {
                        if let Some(ref ctx) = m.context_info {
                            if let Some(ref quoted) = ctx.quoted_message {
                                let stanza_ref = ctx.stanza_id.as_deref();
                                if let Some(preview) = quoted_preview(quoted, stanza_ref, workspace_dir) {
                                    let stanza = ctx.stanza_id.clone();
                                    let header = format!("[Replying to: \"{preview}\"]");
                                    return Some((header, stanza));
                                }
                            }
                        }
                    }
                )+
            };
        }

        try_ctx!(
            extended_text_message,
            image_message,
            video_message,
            audio_message,
            document_message,
            sticker_message,
            location_message,
            contact_message,
        );

        None
    }

    /// Check if the message is a reply to a message sent by the bot itself.
    /// Uses `context_info.participant` from any message sub-type.
    #[cfg(feature = "whatsapp-web")]
    fn is_reply_to_self(
        msg: &wa_rs_proto::whatsapp::Message,
        own_pn: Option<&str>,
        own_lid: Option<&str>,
    ) -> bool {
        use wa_rs_core::proto_helpers::MessageExt;
        let base = msg.get_base_message();

        macro_rules! check_ctx {
            ($($field:ident),+ $(,)?) => {
                $(
                    if let Some(ref m) = base.$field {
                        if let Some(ref ctx) = m.context_info {
                            if let Some(ref participant) = ctx.participant {
                                let user_part = participant.split('@').next().unwrap_or("");
                                if let Some(pn) = own_pn {
                                    if user_part == pn { return true; }
                                }
                                if let Some(lid) = own_lid {
                                    if user_part == lid { return true; }
                                }
                            }
                        }
                    }
                )+
            };
        }

        check_ctx!(
            extended_text_message,
            image_message,
            video_message,
            audio_message,
            document_message,
            sticker_message,
            location_message,
            contact_message,
        );

        false
    }

    /// Look up a previously downloaded image by the quoted message's stanza ID.
    /// Checks for `img_{stanza_id}.*` in the whatsapp_files directory.
    fn find_image_by_stanza_id(
        workspace_dir: Option<&std::path::Path>,
        stanza_id: &str,
    ) -> Option<std::path::PathBuf> {
        let workspace = workspace_dir?;
        let save_dir = workspace.join("whatsapp_files");
        let safe_id: String = stanza_id
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        for ext in &["jpg", "png", "webp", "gif", "bmp"] {
            let path = save_dir.join(format!("img_{safe_id}.{ext}"));
            if path.exists() {
                return Some(path);
            }
        }
        None
    }

    /// Attempt to download a WhatsApp image and save it locally.
    ///
    /// Returns the local path on success, or `None` if workspace is not
    /// configured, download fails, or the image is empty.
    #[cfg(feature = "whatsapp-web")]
    async fn try_download_image(
        client: &wa_rs::Client,
        image: &wa_rs_proto::whatsapp::message::ImageMessage,
        workspace_dir: Option<&std::path::Path>,
        message_id: &str,
    ) -> Option<std::path::PathBuf> {
        let workspace = workspace_dir.or_else(|| {
            tracing::warn!("WhatsApp Web: cannot save image — workspace_dir not configured");
            None
        })?;

        let save_dir = workspace.join("whatsapp_files");
        if let Err(e) = tokio::fs::create_dir_all(&save_dir).await {
            tracing::warn!("WhatsApp Web: failed to create whatsapp_files dir: {e}");
            return None;
        }

        // Download and decrypt the image
        use wa_rs::download::Downloadable;
        let data = match client.download(image as &dyn Downloadable).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("WhatsApp Web: failed to download image: {e}");
                return None;
            }
        };

        if data.is_empty() {
            tracing::warn!("WhatsApp Web: downloaded image is empty");
            return None;
        }

        // Determine extension from mimetype
        let ext = match image.mimetype.as_deref() {
            Some(m) if m.contains("png") => "png",
            Some(m) if m.contains("webp") => "webp",
            Some(m) if m.contains("gif") => "gif",
            Some(m) if m.contains("bmp") => "bmp",
            _ => "jpg", // WhatsApp default
        };

        // Sanitize message_id for filesystem safety
        let safe_id: String = message_id
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let filename = format!("img_{safe_id}.{ext}");
        let local_path = save_dir.join(&filename);

        if let Err(e) = tokio::fs::write(&local_path, &data).await {
            tracing::warn!(
                "WhatsApp Web: failed to save image to {}: {e}",
                local_path.display()
            );
            return None;
        }

        tracing::info!(
            "WhatsApp Web: saved image ({} bytes) to {}",
            data.len(),
            local_path.display()
        );
        Some(local_path)
    }

    /// Set the workspace directory for saving downloaded media files.
    #[cfg(feature = "whatsapp-web")]
    pub fn with_workspace_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.workspace_dir = Some(dir);
        self
    }

    /// Configure voice transcription (STT) for incoming voice notes.
    #[cfg(feature = "whatsapp-web")]
    pub fn with_transcription(mut self, config: crate::config::TranscriptionConfig) -> Self {
        if config.enabled {
            self.transcription = Some(config);
        }
        self
    }

    /// Configure text-to-speech for outgoing voice replies.
    #[cfg(feature = "whatsapp-web")]
    pub fn with_tts(mut self, config: crate::config::TtsConfig) -> Self {
        if config.enabled {
            self.tts_config = Some(config);
        }
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
            if let Some(candidate) = candidate {
                if !candidates.iter().any(|existing| existing == &candidate) {
                    candidates.push(candidate);
                }
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
        transcription_config: Option<&crate::config::TranscriptionConfig>,
    ) -> Option<String> {
        let config = transcription_config?;

        // Enforce duration limit
        if let Some(seconds) = audio.seconds {
            if u64::from(seconds) > config.max_duration_secs {
                tracing::info!(
                    "WhatsApp Web: skipping voice note ({}s exceeds {}s limit)",
                    seconds,
                    config.max_duration_secs
                );
                return None;
            }
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

        match super::transcription::transcribe_audio(audio_data, file_name, config).await {
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
        tts_config: &crate::config::TtsConfig,
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
}

#[cfg(feature = "whatsapp-web")]
#[async_trait]
impl Channel for WhatsAppWebChannel {
    fn name(&self) -> &str {
        "whatsapp"
    }

    fn supports_stream_segments(&self) -> bool {
        self.streaming
    }

    fn stream_segment_size(&self) -> usize {
        self.streaming_chunk_size
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
                        if let Some((_, ts)) = pv.get(&recipient) {
                            if ts.elapsed().as_secs() >= 8 {
                                return pv.remove(&recipient).map(|(text, _)| text);
                            }
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

        // Check for outbound [IMAGE:path] markers — send as WhatsApp media.
        let image_marker_re = regex::Regex::new(r"\[IMAGE:([^\]]+)\]").unwrap();
        if let Some(caps) = image_marker_re.captures(&message.content) {
            let image_path_str = caps[1].trim();
            let image_path = std::path::Path::new(image_path_str);

            if image_path.exists() {
                // Read image bytes from disk.
                let image_bytes = tokio::fs::read(image_path).await.map_err(|e| {
                    anyhow!("Failed to read image file {}: {e}", image_path.display())
                })?;

                if image_bytes.is_empty() {
                    tracing::warn!(
                        "WhatsApp Web: image file is empty: {}",
                        image_path.display()
                    );
                } else {
                    // Detect MIME type from extension.
                    let mimetype = match image_path
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.to_lowercase())
                        .as_deref()
                    {
                        Some("png") => "image/png",
                        Some("gif") => "image/gif",
                        Some("webp") => "image/webp",
                        _ => "image/jpeg",
                    };

                    // Upload to WhatsApp servers.
                    use wa_rs_core::download::MediaType;
                    let upload = client
                        .upload(image_bytes, MediaType::Image)
                        .await
                        .map_err(|e| anyhow!("Failed to upload image: {e}"))?;

                    tracing::info!(
                        "WhatsApp Web: uploaded image (url_len={}, file_length={})",
                        upload.url.len(),
                        upload.file_length
                    );

                    // Extract caption: everything outside the [IMAGE:...] marker.
                    let caption = image_marker_re
                        .replace(&message.content, "")
                        .trim()
                        .to_string();
                    let caption = if caption.is_empty() {
                        None
                    } else {
                        Some(caption)
                    };

                    let image_msg = wa_rs_proto::whatsapp::Message {
                        image_message: Some(Box::new(
                            wa_rs_proto::whatsapp::message::ImageMessage {
                                url: Some(upload.url),
                                direct_path: Some(upload.direct_path),
                                media_key: Some(upload.media_key),
                                file_enc_sha256: Some(upload.file_enc_sha256),
                                file_sha256: Some(upload.file_sha256),
                                file_length: Some(upload.file_length),
                                mimetype: Some(mimetype.to_string()),
                                caption,
                                ..Default::default()
                            },
                        )),
                        ..Default::default()
                    };

                    Box::pin(client.send_message(to, image_msg))
                        .await
                        .map_err(|e| anyhow!("Failed to send image: {e}"))?;

                    tracing::info!(
                        "WhatsApp Web: sent image {} to {}",
                        image_path.display(),
                        message.recipient
                    );
                    return Ok(());
                }
            } else {
                tracing::warn!(
                    "WhatsApp Web: image file not found for outbound: {}",
                    image_path.display()
                );
            }
            // Fall through to send as text if image processing failed.
        }

        // Extract @phone mentions from the message content.
        let mention_re = regex::Regex::new(r"@\+?(\d{8,15})").unwrap();
        let mentioned_jids: Vec<String> = mention_re
            .captures_iter(&message.content)
            .map(|cap| format!("{}@s.whatsapp.net", &cap[1]))
            .collect();

        // Use extended_text_message with mentions if any @phone patterns found,
        // otherwise send as plain conversation.
        let outgoing = if mentioned_jids.is_empty() {
            wa_rs_proto::whatsapp::Message {
                conversation: Some(message.content.clone()),
                ..Default::default()
            }
        } else {
            wa_rs_proto::whatsapp::Message {
                extended_text_message: Some(Box::new(
                    wa_rs_proto::whatsapp::message::ExtendedTextMessage {
                        text: Some(message.content.clone()),
                        context_info: Some(Box::new(wa_rs_proto::whatsapp::ContextInfo {
                            mentioned_jid: mentioned_jids,
                            ..Default::default()
                        })),
                        ..Default::default()
                    },
                )),
                ..Default::default()
            }
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
            }

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
            let mention_only = self.mention_only;
            let logout_tx_clone = logout_tx.clone();
            let retry_count_clone = retry_count.clone();
            let session_revoked_clone = session_revoked.clone();
            let transcription_config = self.transcription.clone();
            let voice_chats = self.voice_chats.clone();
            let workspace_dir = self.workspace_dir.clone();

            let mut builder = Bot::builder()
                .with_backend(backend)
                .with_transport_factory(transport_factory)
                .with_http_client(http_client)
                .on_event(move |event, client| {
                    let tx_inner = tx_clone.clone();
                    let allowed_numbers = allowed_numbers.clone();
                    let logout_tx = logout_tx_clone.clone();
                    let retry_count = retry_count_clone.clone();
                    let session_revoked = session_revoked_clone.clone();
                    let transcription_config = transcription_config.clone();
                    let voice_chats = voice_chats.clone();
                    let workspace_dir = workspace_dir.clone();
                    async move {
                        match event {
                            Event::Message(msg, info) => {
                                let sender_jid = info.source.sender.clone();
                                let sender_alt = info.source.sender_alt.clone();
                                let sender = sender_jid.user().to_string();
                                let chat = info.source.chat.to_string();
                                let wa_message_id = info.id.to_string();

                                let mapped_phone = if sender_jid.is_lid() {
                                    client.get_phone_number_from_lid(&sender_jid.user).await
                                } else {
                                    None
                                };
                                let sender_candidates = Self::sender_phone_candidates(
                                    &sender_jid,
                                    sender_alt.as_ref(),
                                    mapped_phone.as_deref(),
                                );

                                let normalized = match sender_candidates
                                    .iter()
                                    .find(|candidate| {
                                        Self::is_number_allowed_for_list(&allowed_numbers, candidate)
                                    })
                                    .cloned()
                                {
                                    Some(n) => n,
                                    None => {
                                        tracing::warn!(
                                            "WhatsApp Web: message from unrecognized sender not in allowed list (candidates_count={})",
                                            sender_candidates.len()
                                        );
                                        return;
                                    }
                                };

                                // Attempt voice note transcription (ptt = push-to-talk = voice note)
                                let voice_text = if let Some(ref audio) = msg.audio_message {
                                    if audio.ptt == Some(true) {
                                        Self::try_transcribe_voice_note(
                                            &client,
                                            audio,
                                            transcription_config.as_ref(),
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

                                // Use transcribed voice text, image, or fall back to text content.
                                // Track whether this chat used a voice note so we reply in kind.
                                // We store the chat JID (reply_target) since that's what send() receives.
                                let mut content = if let Some(ref vt) = voice_text {
                                    if let Ok(mut vs) = voice_chats.lock() {
                                        vs.insert(chat.clone());
                                    }
                                    format!("[Voice] {vt}")
                                } else if let Some(ref image) = msg.get_base_message().image_message {
                                    if let Ok(mut vs) = voice_chats.lock() {
                                        vs.remove(&chat);
                                    }
                                    // Download image and produce [IMAGE:] marker
                                    if let Some(local_path) = Self::try_download_image(
                                        &client,
                                        image,
                                        workspace_dir.as_deref(),
                                        &wa_message_id,
                                    )
                                    .await
                                    {
                                        let marker = format!("[IMAGE:{}]", local_path.display());
                                        // Append caption if present
                                        match image.caption.as_deref() {
                                            Some(cap) if !cap.trim().is_empty() => {
                                                format!("{marker}\n\n{}", cap.trim())
                                            }
                                            _ => marker,
                                        }
                                    } else {
                                        // Download failed; fall back to caption-only or skip
                                        image
                                            .caption
                                            .as_deref()
                                            .filter(|c| !c.trim().is_empty())
                                            .map(|c| c.trim().to_string())
                                            .unwrap_or_default()
                                    }
                                } else {
                                    if let Ok(mut vs) = voice_chats.lock() {
                                        vs.remove(&chat);
                                    }
                                    let text = msg.text_content().unwrap_or("");
                                    text.trim().to_string()
                                };

                                // Prepend reply/quote context so the agent knows
                                // what message the user is responding to.
                                // If replying to an image, resolve the cached file.
                                if let Some((quote, _stanza_id)) = Self::extract_reply_context(
                                    &msg,
                                    workspace_dir.as_deref(),
                                ) {
                                    if content.is_empty() {
                                        content = quote;
                                    } else {
                                        content = format!("{quote}\n\n{content}");
                                    }
                                }

                                // In group chats, prepend sender phone so the agent
                                // can identify who said what and @mention them.
                                if Self::is_group_chat(&chat) {
                                    content = format!("[From: {normalized}] {content}");
                                }

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

                                // mention_only: in group chats, skip unless mentioned
                                if mention_only && Self::is_group_chat(&chat) {
                                    let own_pn = client.get_pn().await;
                                    let own_lid = client.get_lid().await;

                                    // If we can't determine our own identity yet, let
                                    // the message through rather than silently dropping it.
                                    if own_pn.is_none() && own_lid.is_none() {
                                        tracing::warn!(
                                            "WhatsApp Web: mention_only enabled but own JID unknown, allowing message"
                                        );
                                    } else {
                                        let mentioned = Self::collect_mentioned_jids(&msg);

                                        let pn_user = own_pn.as_ref().map(|j| j.user.as_str());
                                        let lid_user = own_lid.as_ref().map(|j| j.user.as_str());

                                        let is_mentioned = mentioned.iter().any(|jid| {
                                            if let Some(pn) = pn_user {
                                                if let Some(rest) = jid.strip_prefix(pn) {
                                                    if rest.starts_with('@') || rest.starts_with(':') {
                                                        return true;
                                                    }
                                                }
                                            }
                                            if let Some(lid) = lid_user {
                                                if let Some(rest) = jid.strip_prefix(lid) {
                                                    if rest.starts_with('@') || rest.starts_with(':') {
                                                        return true;
                                                    }
                                                }
                                            }
                                            false
                                        });

                                        // Also respond if replying to one of our own messages
                                        let is_reply_to_bot = Self::is_reply_to_self(
                                            &msg,
                                            pn_user,
                                            lid_user,
                                        );

                                        if !is_mentioned && !is_reply_to_bot {
                                            tracing::debug!(
                                                "WhatsApp Web: mention_only — storing group message as observe_group (mentioned_jids={:?}, own_pn={:?}, own_lid={:?})",
                                                mentioned,
                                                pn_user,
                                                lid_user,
                                            );
                                            // Store in session for context but don't respond.
                                            if !content.is_empty() {
                                                let _ = tx_inner
                                                    .send(ChannelMessage {
                                                        id: wa_message_id.clone(),
                                                        channel: "whatsapp".to_string(),
                                                        sender: normalized.clone(),
                                                        reply_target: chat,
                                                        content,
                                                        timestamp: chrono::Utc::now().timestamp().cast_unsigned(),
                                                        thread_ts: None,
                                                        observe_group: true,
                                                        interruption_scope_id: None,
                                                    })
                                                    .await;
                                            }
                                            return;
                                        }
                                    }
                                }

                                if let Err(e) = tx_inner
                                    .send(ChannelMessage {
                                        id: wa_message_id,
                                        channel: "whatsapp".to_string(),
                                        sender: normalized.clone(),
                                        // Reply to the originating chat JID (DM or group).
                                        reply_target: chat,
                                        content,
                                        timestamp: chrono::Utc::now().timestamp().cast_unsigned(),
                                        thread_ts: None,
                                        observe_group: false,
                                        interruption_scope_id: None,
                                    })
                                    .await
                                {
                                    tracing::error!("Failed to send message to channel: {}", e);
                                }
                            }
                            Event::Connected(_) => {
                                tracing::info!("WhatsApp Web connected successfully");
                                WhatsAppWebChannel::reset_retry(&retry_count);
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

/// Truncate a quoted-message preview to at most `max_chars` characters,
/// appending "…" when the text is shortened.
#[cfg(feature = "whatsapp-web")]
fn truncate_reply_preview(text: &str, max_chars: usize) -> String {
    // Normalise whitespace so multi-line quotes become a single line.
    let normalised: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalised.chars().count() <= max_chars {
        normalised
    } else {
        let truncated: String = normalised.chars().take(max_chars).collect();
        format!("{truncated}…")
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
    ) -> Self {
        Self { _private: () }
    }

    pub fn with_transcription(self, _config: crate::config::TranscriptionConfig) -> Self {
        self
    }

    pub fn with_tts(self, _config: crate::config::TtsConfig) -> Self {
        self
    }

    pub fn with_workspace_dir(self, _dir: std::path::PathBuf) -> Self {
        self
    }

    pub fn with_mention_only(self, _mention_only: bool) -> Self {
        self
    }

    pub fn with_streaming(self, _enabled: bool, _chunk_size: usize) -> Self {
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
        let ch = WhatsAppWebChannel::new("/tmp/test.db".into(), None, None, vec!["*".into()]);
        assert!(ch.is_number_allowed("+1234567890"));
        assert!(ch.is_number_allowed("+9999999999"));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_number_denied_empty() {
        let ch = WhatsAppWebChannel::new("/tmp/test.db".into(), None, None, vec![]);
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
            #[allow(clippy::cast_possible_truncation)]
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
        let mut tc = crate::config::TranscriptionConfig::default();
        tc.enabled = true;

        let ch = make_channel().with_transcription(tc);
        assert!(ch.transcription.is_some());
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn with_transcription_ignores_when_disabled() {
        let tc = crate::config::TranscriptionConfig::default(); // enabled = false
        let ch = make_channel().with_transcription(tc);
        assert!(ch.transcription.is_none());
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
}
