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
//! # If installed to PATH:
//! cargo install --path . --force --locked --features whatsapp-web
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
use anyhow::Result;
use async_trait::async_trait;
use parking_lot::Mutex;
use std::sync::Arc;
use tokio::select;
use waproto::whatsapp::device_props::PlatformType;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
#[cfg(not(feature = "whatsapp-web"))]
use zeroclaw_runtime::i18n;

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
    /// Override WebSocket URL (test / proxy setups). Sourced from
    /// `[whatsapp.ws_url]` — replaces the legacy `WHATSAPP_WS_URL` env-var
    /// read.
    ws_url: Option<String>,
    /// The alias key under `[channels.whatsapp.<alias>]` this handle is
    /// bound to. Used to scope peer-group writes and resolver lookups.
    alias: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
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
    /// Bot handle for shutdown.
    /// whatsapp-rust 0.6: `Bot::run()` now returns `BotHandle` (a Future + abort)
    /// rather than a tokio JoinHandle directly (oxidezap/whatsapp-rust BotHandle wrapper).
    bot_handle: Arc<Mutex<Option<whatsapp_rust::bot::BotHandle>>>,
    /// Client handle for sending messages and typing indicators
    client: Arc<Mutex<Option<Arc<whatsapp_rust::Client>>>>,
    /// Message sender channel
    tx: Arc<Mutex<Option<tokio::sync::mpsc::Sender<ChannelMessage>>>>,
    /// Voice transcription (STT) config
    transcription: Option<zeroclaw_config::schema::TranscriptionConfig>,
    transcription_manager: Option<std::sync::Arc<super::transcription::TranscriptionManager>>,
    /// Text-to-speech runtime for voice replies (built from
    /// `tts_providers.<type>.<alias>`).
    tts_manager: Option<Arc<super::tts::TtsManager>>,
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
    /// Create a new WhatsApp Web channel from a `WhatsAppConfig`.
    ///
    /// `config` is the schema block under `[channels.whatsapp.<alias>]`;
    /// `alias` is that alias key; `peer_resolver` resolves inbound
    /// external peers from canonical state at message-time (no cache —
    /// see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
    #[cfg(feature = "whatsapp-web")]
    pub fn new(
        config: &zeroclaw_config::schema::WhatsAppConfig,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Self {
        let session_path = config.session_path.clone().unwrap_or_default();
        let pair_phone = config.pair_phone.clone();
        let pair_code = config.pair_code.clone();
        let ws_url = config.ws_url.clone();
        let mention_only = config.mention_only;
        let mode = config.mode.clone();
        let dm_policy = config.dm_policy.clone();
        let group_policy = config.group_policy.clone();
        let self_chat_mode = config.self_chat_mode;

        // Seed bot_phone from pair_phone (digits only)
        let bot_phone = pair_phone
            .as_ref()
            .map(|p| p.chars().filter(|c| c.is_ascii_digit()).collect::<String>())
            .filter(|digits| !digits.is_empty());

        if mention_only && bot_phone.is_none() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "mention_only enabled but pair_phone not set. \
                Bot identity will be resolved after connection. Group messages \
                will be skipped until identity is known."
            );
        }

        Self {
            session_path,
            pair_phone,
            pair_code,
            ws_url,
            alias: alias.into(),
            peer_resolver,
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
            tts_manager: None,
            pending_voice: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            voice_chats: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            dm_mention_patterns: Arc::new(Vec::new()),
            group_mention_patterns: Arc::new(Vec::new()),
        }
    }

    /// Return the alias under `[channels.whatsapp.<alias>]` that this
    /// channel handle is bound to.
    pub fn alias(&self) -> &str {
        &self.alias
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

    /// Configure text-to-speech for outgoing voice replies.
    ///
    /// Builds a [`super::tts::TtsManager`] from the
    /// `[tts_providers.<type>.<alias>]` map. Disabled when `[tts].enabled = false`
    /// or when the manager fails to construct (logged at warn).
    #[cfg(feature = "whatsapp-web")]
    pub fn with_tts(mut self, config: &zeroclaw_config::schema::Config) -> Self {
        if config.tts.enabled {
            match super::tts::TtsManager::from_config(config) {
                Ok(m) => self.tts_manager = Some(Arc::new(m)),
                Err(e) => ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "TTS disabled"
                ),
            }
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
        let peers = (self.peer_resolver)();
        Self::is_number_allowed_for_list(&peers, phone)
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

    /// Build the LID-aware diagnostic suffix appended to allowlist-rejection
    /// logs so the operator sees why a known phone number didn't match.
    /// Only meaningful inside an actual rejection branch (`normalized.is_none()`
    /// under `Allowlist` policy); outside that branch the LID resolution
    /// state is not the operator's concern, since the message is being
    /// processed normally.
    #[cfg(feature = "whatsapp-web")]
    fn lid_rejection_diagnostic(
        sender: &wacore_binary::jid::Jid,
        mapped_phone: Option<&str>,
    ) -> String {
        if !sender.is_lid() {
            return String::new();
        }
        if mapped_phone.is_none() {
            format!(
                " (LID→phone resolution returned None for sender {sender}; \
                 allowlist phone-number entries cannot match. Workaround: \
                 add the LID-form (+{}) to allowed_numbers, or wait for the \
                 in-memory LID cache to populate for this contact.)",
                sender.user
            )
        } else {
            " (sender is LID; resolved phone did not match any allowlist entry)".to_string()
        }
    }

    /// Build normalized sender candidates from sender JID, optional alt JID, and optional LID->PN mapping.
    #[cfg(feature = "whatsapp-web")]
    fn sender_phone_candidates(
        sender: &wacore_binary::jid::Jid,
        sender_alt: Option<&wacore_binary::jid::Jid>,
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

    /// Compute the reply target, converting LID→phone for DMs when necessary.
    ///
    /// LID JIDs (e.g. `76188559093817@lid`) are internal WhatsApp routing
    /// identifiers that cannot receive messages. For non-group chats with an
    /// LID-based JID, this converts to a phone JID (`digits@s.whatsapp.net`)
    /// using `mapped_phone` from the LID→phone lookup. Groups are returned
    /// unchanged.
    #[cfg(feature = "whatsapp-web")]
    fn compute_reply_target(
        chat_jid: &str,
        is_lid: bool,
        is_group: bool,
        mapped_phone: Option<&str>,
    ) -> String {
        if !is_group && is_lid {
            mapped_phone
                .map(|p| p.chars().filter(|c| c.is_ascii_digit()).collect::<String>())
                .filter(|d| !d.is_empty())
                .map(|digits| format!("{digits}@s.whatsapp.net"))
                .unwrap_or_else(|| chat_jid.to_string())
        } else {
            chat_jid.to_string()
        }
    }

    /// True when the address is a WhatsApp LID JID (not deliverable for outbound).
    #[cfg(feature = "whatsapp-web")]
    fn is_lid_jid_string(jid: &str) -> bool {
        jid.trim()
            .rsplit_once('@')
            .is_some_and(|(_, domain)| domain.eq_ignore_ascii_case("lid"))
    }

    /// Map an undeliverable LID chat JID to `digits@s.whatsapp.net` when phone
    /// candidates are known. Returns `(target, converted)`.
    #[cfg(feature = "whatsapp-web")]
    fn resolve_deliverable_reply_target(chat: &str, phone_candidates: &[String]) -> (String, bool) {
        if !Self::is_lid_jid_string(chat) {
            return (chat.to_string(), false);
        }
        for candidate in phone_candidates {
            let digits: String = candidate.chars().filter(|c| c.is_ascii_digit()).collect();
            if !digits.is_empty() {
                return (format!("{digits}@s.whatsapp.net"), true);
            }
        }
        (chat.to_string(), false)
    }

    /// Best-effort LID→phone lookup via whatsapp-rust 0.6 `get_lid_pn_entry`.
    #[cfg(feature = "whatsapp-web")]
    async fn lookup_phone_from_lid_jid(
        client: &whatsapp_rust::Client,
        lid_jid: &str,
    ) -> Option<String> {
        let lid_user = lid_jid.split('@').next().filter(|u| !u.is_empty())?;
        let jid = wacore_binary::jid::Jid::lid(lid_user);
        match client.get_lid_pn_entry(&jid).await {
            Ok(Some(entry)) => Some(entry.phone_number),
            _ => None,
        }
    }

    /// Resolve an outbound recipient, converting LID JIDs via the live client cache.
    #[cfg(feature = "whatsapp-web")]
    async fn resolve_outbound_recipient(
        client: &whatsapp_rust::Client,
        recipient: &str,
    ) -> Result<String> {
        let trimmed = recipient.trim();
        if !Self::is_lid_jid_string(trimmed) {
            return Ok(trimmed.to_string());
        }
        if let Some(phone) = Self::lookup_phone_from_lid_jid(client, trimmed).await
            && let Some(token) = Self::normalize_phone_token(&phone)
        {
            let digits: String = token.chars().filter(|c| c.is_ascii_digit()).collect();
            if !digits.is_empty() {
                let resolved = format!("{digits}@s.whatsapp.net");
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({
                            "from": trimmed,
                            "to": resolved,
                        })),
                    "outbound LID→phone recipient"
                );
                return Ok(resolved);
            }
        }
        anyhow::bail!(
            "Cannot deliver to LID JID `{trimmed}`: phone resolution failed (LID JIDs cannot receive messages)"
        )
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

        let qr = qrcode::QrCode::new(payload.as_bytes()).map_err(|err| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", err)})),
                "Failed to encode WhatsApp Web QR payload"
            );
            anyhow::Error::msg(format!("Failed to encode WhatsApp Web QR payload: {err}"))
        })?;

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
    fn recipient_to_jid(&self, recipient: &str) -> Result<wacore_binary::jid::Jid> {
        let trimmed = recipient.trim();
        if trimmed.is_empty() {
            anyhow::bail!("Recipient cannot be empty");
        }

        if trimmed.contains('@') {
            return trimmed.parse::<wacore_binary::jid::Jid>().map_err(|e| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "trimmed": trimmed,
                            "error": format!("{}", e),
                        })),
                    "whatsapp_web: invalid JID"
                );
                anyhow::Error::msg(format!("Invalid WhatsApp JID `{trimmed}`: {e}"))
            });
        }

        let digits: String = trimmed.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            anyhow::bail!("Recipient `{trimmed}` does not contain a valid phone number");
        }

        Ok(wacore_binary::jid::Jid::pn(digits))
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
        client: &whatsapp_rust::Client,
        audio: &waproto::whatsapp::message::AudioMessage,
        transcription_config: Option<&zeroclaw_config::schema::TranscriptionConfig>,
        transcription_manager: Option<&super::transcription::TranscriptionManager>,
    ) -> Option<String> {
        let config = transcription_config?;
        let manager = transcription_manager?;

        // Enforce duration limit
        if let Some(seconds) = audio.seconds
            && u64::from(seconds) > config.max_duration_secs
        {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                &format!(
                    "skipping voice note ({}s exceeds {}s limit)",
                    seconds, config.max_duration_secs
                )
            );
            return None;
        }

        // Download the encrypted audio
        use whatsapp_rust::download::Downloadable;
        let audio_data = match client.download(audio as &dyn Downloadable).await {
            Ok(data) => data,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "failed to download voice note"
                );
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

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "transcribing voice note ({} bytes, file={})",
                audio_data.len(),
                file_name
            )
        );

        match manager.transcribe(&audio_data, file_name).await {
            Ok(text) if text.trim().is_empty() => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "voice transcription returned empty text, skipping"
                );
                None
            }
            Ok(text) => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    &format!("voice note transcribed ({} chars)", text.len())
                );
                Some(text)
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "voice transcription failed"
                );
                None
            }
        }
    }

    /// Synthesize text to speech and send as a WhatsApp voice note (static version for spawned tasks).
    #[cfg(feature = "whatsapp-web")]
    async fn synthesize_voice_static(
        client: &whatsapp_rust::Client,
        to: &wacore_binary::jid::Jid,
        text: &str,
        tts_manager: &super::tts::TtsManager,
    ) -> Result<()> {
        let audio_bytes = tts_manager.synthesize(text).await?;
        let audio_len = audio_bytes.len();
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!("TTS: synthesized {} bytes of audio", audio_len)
        );

        if audio_bytes.is_empty() {
            anyhow::bail!("TTS returned empty audio");
        }

        use wacore::download::MediaType;
        use whatsapp_rust::upload::UploadOptions;
        let upload = client
            .upload(audio_bytes, MediaType::Audio, UploadOptions::default())
            .await
            .map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "Failed to upload TTS audio"
                );
                anyhow::Error::msg(format!("Failed to upload TTS audio: {e}"))
            })?;

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "TTS: uploaded audio (url_len={}, file_length={})",
                upload.url.len(),
                upload.file_length
            )
        );

        // Estimate duration: Opus at ~32kbps → bytes / 4000 ≈ seconds
        #[allow(clippy::cast_possible_truncation)]
        let estimated_seconds = std::cmp::max(1, (upload.file_length / 4000) as u32);

        // whatsapp-rust 0.6: UploadResponse cryptographic fields became
        // `[u8; 32]` for type safety. Pull the Vec<u8> copies before
        // consuming the strings so the partial-move on `upload.direct_path`
        // doesn't bite.
        let media_key = upload.media_key_vec();
        let file_enc_sha256 = upload.file_enc_sha256_vec();
        let file_sha256 = upload.file_sha256_vec();
        let voice_msg = waproto::whatsapp::Message {
            audio_message: Some(Box::new(waproto::whatsapp::message::AudioMessage {
                url: Some(upload.url),
                direct_path: Some(upload.direct_path),
                media_key: Some(media_key),
                file_enc_sha256: Some(file_enc_sha256),
                file_sha256: Some(file_sha256),
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
            .map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "Failed to send voice note"
                );
                anyhow::Error::msg(format!("Failed to send voice note: {e}"))
            })?;
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "TTS: sent voice note ({} bytes, ~{}s)",
                audio_len, estimated_seconds
            )
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
    fn extract_mentioned_jids(msg: &waproto::whatsapp::Message) -> Vec<String> {
        use wacore::proto_helpers::MessageExt;
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
}

/// Decide whether a `fromMe` message outside the operator's self-chat is an
/// intentional operator-typed bot trigger.
///
/// The default response to a `fromMe` mirror is to drop, because WhatsApp Web
/// echoes every message the operator types from any linked device and replying
/// would impersonate them. The exception is when the operator has configured
/// `dm_mention_patterns` / `group_mention_patterns` and the text matches —
/// that is the explicit opt-in that distinguishes a deliberate trigger
/// (e.g. typing `TinyBot foo` in a friend's DM) from a normal mirrored
/// message.
///
/// Returns `true` when the message should fall through to the regular policy
/// branches; `false` when it should be dropped as a mirror.
#[cfg(feature = "whatsapp-web")]
fn fromme_outside_self_chat_is_operator_trigger(
    is_group: bool,
    dm_mention_patterns: &[regex::Regex],
    group_mention_patterns: &[regex::Regex],
    text: &str,
) -> bool {
    let applicable = if is_group {
        group_mention_patterns
    } else {
        dm_mention_patterns
    };
    if applicable.is_empty() {
        return false;
    }
    super::whatsapp::WhatsAppChannel::text_matches_patterns(applicable, text)
}

#[cfg(feature = "whatsapp-web")]
impl ::zeroclaw_api::attribution::Attributable for WhatsAppWebChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::WhatsappWeb,
        )
    }
    fn alias(&self) -> &str {
        &self.alias
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
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!("recipient {} not in allowed list", message.recipient)
                );
                return Ok(());
            }
        }

        let deliverable_recipient =
            Self::resolve_outbound_recipient(&client, &message.recipient).await?;
        let to = self.recipient_to_jid(&deliverable_recipient)?;

        // Voice chat mode: send text normally AND queue a voice note of the
        // final answer. Only substantive messages (not tool outputs) are queued.
        // A debounce task waits 10s after the last substantive message, then
        // sends ONE voice note. Text in → text out. Voice in → text + voice out.
        let is_voice_chat = self
            .voice_chats
            .lock()
            .map(|vs| vs.contains(&message.recipient))
            .unwrap_or(false);

        if is_voice_chat && self.tts_manager.is_some() {
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
                let tts_manager = self.tts_manager.clone().unwrap();
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
                            &tts_manager,
                        ))
                        .await
                        {
                            Ok(()) => {
                                ::zeroclaw_log::record!(
                                    INFO,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    ),
                                    &format!("voice reply sent ({} chars)", text.len())
                                );
                            }
                            Err(e) => {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                                    "TTS voice reply failed"
                                );
                            }
                        }
                    }
                });
            }
            // Fall through to send text normally (voice chat gets BOTH)
        }

        // Send text message
        let outgoing = waproto::whatsapp::Message {
            conversation: Some(message.content.clone()),
            ..Default::default()
        };

        // Box::pin the large future (~34KB) so it doesn't inflate the
        // enclosing Send future's stack slot — clippy::large_futures.
        // whatsapp-rust 0.6: send_message returns `SendResult { message_id, to }`
        // instead of a bare `String` (oxidezap/whatsapp-rust#597).
        let send_result = Box::pin(client.send_message(to, outgoing)).await?;
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "sent text to {} (id: {})",
                message.recipient, send_result.message_id
            )
        );
        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        // Store the sender channel for incoming messages
        *self.tx.lock() = Some(tx.clone());

        // Capture alias as an Arc so the long-running event closure (inside
        // the reconnect loop) can clone cheaply per spawned message without
        // borrowing `self` for its 'static lifetime.
        let alias = std::sync::Arc::new(self.alias.clone());

        use wacore::proto_helpers::MessageExt;
        use wacore::store::DevicePropsOverride;
        use wacore::types::events::Event;
        use wacore_binary::jid::JidExt as _;
        use whatsapp_rust::TokioRuntime;
        use whatsapp_rust::bot::Bot;
        use whatsapp_rust::pair_code::PairCodeOptions;
        use whatsapp_rust::store::{Device, DeviceStore};
        use whatsapp_rust_tokio_transport::TokioWebSocketTransportFactory;
        use whatsapp_rust_ureq_http_client::UreqHttpClient;

        let retry_count = Arc::new(std::sync::atomic::AtomicU32::new(0));

        loop {
            let expanded_session_path = shellexpand::tilde(&self.session_path).to_string();

            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                &format!("channel starting (session: {})", expanded_session_path)
            );

            // Initialize storage backend
            let storage = RusqliteStore::new(&expanded_session_path)?;
            let backend = Arc::new(storage);

            // Check if we have a saved device to load
            let mut device = Device::new(backend.clone());
            if backend.exists().await? {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "found existing session, loading device"
                );
                if let Some(core_device) = backend.load().await? {
                    device.load_from_serializable(core_device);
                } else {
                    anyhow::bail!("Device exists but failed to load");
                }
            } else {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "no existing session, new device will be created during pairing"
                );
            };

            // Create transport factory. WebSocket URL override comes from
            // `[whatsapp.ws_url]`; legacy `WHATSAPP_WS_URL` env var is gone.
            let mut transport_factory = TokioWebSocketTransportFactory::new();
            if let Some(ref ws_url) = self.ws_url {
                transport_factory = transport_factory.with_url(ws_url.clone());
            }

            // Create HTTP client for media operations
            let http_client = UreqHttpClient::new();

            // Channel to signal logout from the event handler back to the listen loop.
            let (logout_tx, mut logout_rx) = tokio::sync::broadcast::channel::<()>(1);

            // Tracks whether Event::LoggedOut actually fired (vs task crash).
            let session_revoked = Arc::new(std::sync::atomic::AtomicBool::new(false));

            // Build the bot
            let tx_clone = tx.clone();
            let peer_resolver = Arc::clone(&self.peer_resolver);
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

            // whatsapp-rust 0.6: BotBuilder gained a 4th typestate slot for the
            // async runtime (oxidezap/whatsapp-rust#621). `with_runtime` is
            // required before `.build()` resolves; we use the bundled
            // `TokioRuntime`. `with_device_props` switched from three
            // positional Options to a `DevicePropsOverride` builder
            // (oxidezap/whatsapp-rust#586).
            let mut builder = Bot::builder()
                .with_backend(backend)
                .with_transport_factory(transport_factory)
                .with_http_client(http_client)
                .with_runtime(TokioRuntime)
                .with_device_props(
                    DevicePropsOverride::new()
                        .with_os("ZeroClaw")
                        .with_platform_type(PlatformType::Desktop),
                )
                .on_event({
                    let alias = Arc::clone(&alias);
                    move |event, client| {
                    let tx_inner = tx_clone.clone();
                    let peer_resolver = Arc::clone(&peer_resolver);
                    let logout_tx = logout_tx_clone.clone();
                    let retry_count = retry_count_clone.clone();
                    let session_revoked = session_revoked_clone.clone();
                    let alias = Arc::clone(&alias);
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
                        // whatsapp-rust 0.6: event handlers receive `Arc<Event>`
                        // per PR #613, so we match against `&*event` to get a
                        // `&Event` reference and bind variant fields by ref.
                        match &*event {
                            Event::Message(msg, info) => {
                                let sender_jid = info.source.sender.clone();
                                let sender_alt = info.source.sender_alt.clone();
                                let sender = sender_jid.user().to_string();
                                let _is_group = info.source.chat.is_group();
                                let chat = info.source.chat.to_string();

                                // whatsapp-rust 0.6: `Client::get_phone_number_from_lid`
                                // was replaced by the unified `get_lid_pn_entry`
                                // (oxidezap/whatsapp-rust#487). The new helper
                                // returns the full LID↔phone entry; we extract
                                // the phone field on hit, swallow lookup errors
                                // back to `None` (consistent with the legacy
                                // semantics — best-effort enrichment).
                                let mapped_phone = if sender_jid.is_lid() {
                                    match client.get_lid_pn_entry(&sender_jid).await {
                                        Ok(Some(entry)) => Some(entry.phone_number),
                                        _ => None,
                                    }
                                } else {
                                    None
                                };
                                let sender_candidates = Self::sender_phone_candidates(
                                    &sender_jid,
                                    sender_alt.as_ref(),
                                    mapped_phone.as_deref(),
                                );

                                let allowed_peers = peer_resolver();
                                let normalized = sender_candidates
                                    .iter()
                                    .find(|candidate| {
                                        Self::is_number_allowed_for_list(&allowed_peers, candidate)
                                    })
                                    .cloned();

                                let is_group = info.source.is_group;
                                let mut reply_target = Self::compute_reply_target(
                                    &chat,
                                    info.source.chat.is_lid(),
                                    is_group,
                                    mapped_phone.as_deref(),
                                );
                                if reply_target != chat {
                                    ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"from": chat, "to": reply_target})), "LID→phone reply target");
                                }

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
                                            ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "ignoring self-chat message (self_chat_mode=false)");
                                            return;
                                        }
                                        // self_chat_mode=true: always process, skip further policy checks.
                                    } else if info.source.is_from_me
                                        && !fromme_outside_self_chat_is_operator_trigger(
                                            is_group,
                                            &wa_dm_mention_patterns,
                                            &wa_group_mention_patterns,
                                            msg.text_content().unwrap_or(""),
                                        )
                                    {
                                        // fromMe outside the self-chat thread is a mirror of the
                                        // operator's own outbound message to a third party (DM or
                                        // group). Replying would impersonate the operator. Drop —
                                        // unless the operator has configured a mention pattern
                                        // and the text matches it (the workflow @ilteoood uses
                                        // with `TinyBot ...` triggers), in which case the helper
                                        // returns true and we fall through to the policy branches
                                        // below to treat the message like an inbound trigger.
                                        ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"chat": chat, "sender": sender})), "ignoring fromMe message outside self-chat thread (chat=, sender=)");
                                        return;
                                    } else if is_group {
                                        match wa_group_policy {
                                            zeroclaw_config::schema::WhatsAppChatPolicy::Ignore => {
                                                ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "ignoring group message (group_policy=ignore)");
                                                return;
                                            }
                                            zeroclaw_config::schema::WhatsAppChatPolicy::All => {
                                                // allow unconditionally
                                            }
                                            zeroclaw_config::schema::WhatsAppChatPolicy::Allowlist => {
                                                if normalized.is_none() {
                                                    let lid_diag = Self::lid_rejection_diagnostic(
                                                        &sender_jid,
                                                        mapped_phone.as_deref(),
                                                    );
                                                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown), &format!("message from unrecognized sender not in allowed list (candidates_count={}){}", sender_candidates.len(), lid_diag));
                                                    return;
                                                }
                                            }
                                        }
                                    } else {
                                        // DM (non-self)
                                        match wa_dm_policy {
                                            zeroclaw_config::schema::WhatsAppChatPolicy::Ignore => {
                                                ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "ignoring DM (dm_policy=ignore)");
                                                return;
                                            }
                                            zeroclaw_config::schema::WhatsAppChatPolicy::All => {
                                                // allow unconditionally
                                            }
                                            zeroclaw_config::schema::WhatsAppChatPolicy::Allowlist => {
                                                if normalized.is_none() {
                                                    let lid_diag = Self::lid_rejection_diagnostic(
                                                        &sender_jid,
                                                        mapped_phone.as_deref(),
                                                    );
                                                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown), &format!("message from unrecognized sender not in allowed list (candidates_count={}){}", sender_candidates.len(), lid_diag));
                                                    return;
                                                }
                                            }
                                        }
                                    }
                                }

                                let normalized = normalized.unwrap_or_else(|| sender.clone());

                                // LID chat JIDs cannot receive outbound messages (typing may
                                // still work). Resolve to phone JID for all non-group DMs.
                                if !is_group && Self::is_lid_jid_string(&reply_target) {
                                    let mut lid_candidates = sender_candidates.clone();
                                    if let Some(phone) =
                                        Self::lookup_phone_from_lid_jid(&client, &reply_target).await
                                        && let Some(token) = Self::normalize_phone_token(&phone)
                                        && !lid_candidates.iter().any(|c| c == &token)
                                    {
                                        lid_candidates.push(token);
                                    }
                                    let (resolved, converted) =
                                        Self::resolve_deliverable_reply_target(
                                            &reply_target,
                                            &lid_candidates,
                                        );
                                    if converted {
                                        reply_target = resolved;
                                        ::zeroclaw_log::record!(
                                            INFO,
                                            ::zeroclaw_log::Event::new(
                                                module_path!(),
                                                ::zeroclaw_log::Action::Note
                                            )
                                            .with_attrs(::serde_json::json!({
                                                "reply_target": reply_target,
                                            })),
                                            "DM LID→phone reply target"
                                        );
                                    } else {
                                        ::zeroclaw_log::record!(
                                            WARN,
                                            ::zeroclaw_log::Event::new(
                                                module_path!(),
                                                ::zeroclaw_log::Action::Note
                                            )
                                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                            .with_attrs(::serde_json::json!({
                                                "reply_target": reply_target,
                                            })),
                                            "undeliverable LID reply_target; outbound may fail"
                                        );
                                    }
                                }

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
                                        ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), &format!("ignoring non-PTT audio message from {}", normalized));
                                        None
                                    }
                                } else {
                                    None
                                };

                                // Use transcribed voice text, or fall back to text content.
                                // Track whether this chat used a voice note so we reply in kind.
                                // Key by final reply_target (post-LID resolution): send() checks
                                // message.recipient, which is the resolved phone JID for LID DMs.
                                let content = if let Some(ref vt) = voice_text {
                                    if let Ok(mut vs) = voice_chats.lock() {
                                        vs.insert(reply_target.clone());
                                    }
                                    format!("[Voice] {vt}")
                                } else {
                                    if let Ok(mut vs) = voice_chats.lock() {
                                        vs.remove(&reply_target);
                                    }
                                    let text = msg.text_content().unwrap_or("");
                                    text.trim().to_string()
                                };

                                ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), &format!("WhatsApp Web message received (sender_len={}, chat_len={}, content_len={})", sender.len(), chat.len(), content.len()));
                                ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), &format!("WhatsApp Web message content: {}", content));

                                if content.is_empty() {
                                    ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), &format!("ignoring empty or non-text message from {}", normalized));
                                    return;
                                }

                                // mention_only: skip group messages without a bot mention
                                if mention_only && is_group {
                                    let bot_phone = bot_phone_inner.lock();
                                    if let Some(ref bp) = *bot_phone {
                                        let mentioned_jids =
                                            Self::extract_mentioned_jids(msg);
                                        if !Self::contains_bot_mention(
                                            &content,
                                            &mentioned_jids,
                                            bp,
                                        ) {
                                            ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "ignoring group message without bot mention");
                                            return;
                                        }
                                    } else {
                                        ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "mention_only active but bot identity unknown, skipping group msg");
                                        return;
                                    }
                                }

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
                                            ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"normalized": normalized})), "message from did not match mention patterns, dropping");
                                            return;
                                        }
                                    };

                                if let Err(e) = tx_inner
                                    .send(ChannelMessage {
                                        id: uuid::Uuid::new_v4().to_string(),
                                        channel: "whatsapp".to_string(),
                                        channel_alias: Some((*alias).clone()),
                                        sender: normalized.clone(),
                                        // Reply to the originating chat JID (DM or group).
                                        // For DMs with LID JIDs, this is the resolved
                                        // phone JID (see LID→phone resolution above).
                                        reply_target,
                                        content,
                                        timestamp: chrono::Utc::now().timestamp() as u64,
                                        thread_ts: None,
                                        interruption_scope_id: None,
                    attachments: vec![],
                                        subject: None,
                                    })
                                    .await
                                {
                                    ::zeroclaw_log::record!(ERROR, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail).with_outcome(::zeroclaw_log::EventOutcome::Failure).with_attrs(::serde_json::json!({"error": format!("{}", e)})), "failed to send message to channel");
                                }
                            }
                            Event::Connected(_) => {
                                ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "connected successfully");
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
                                            ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), &format!("resolved bot identity from device: +{}", digits));
                                        }
                                    }
                                }
                            }
                            Event::LoggedOut(_) => {
                                session_revoked.store(true, std::sync::atomic::Ordering::Relaxed);
                                ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown), "WhatsApp Web was logged out — will clear session and reconnect");
                                let _ = logout_tx.send(());
                            }
                            Event::StreamError(stream_error) => {
                                ::zeroclaw_log::record!(ERROR, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail).with_outcome(::zeroclaw_log::EventOutcome::Failure), &format!("stream error: {:?}", stream_error));
                            }
                            Event::PairingCode { code, .. } => {
                                ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "pair code received");
                                ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "Link your phone by entering this code in WhatsApp > Linked Devices");
                                eprintln!();
                                eprintln!("pair code: {code}");
                                eprintln!();
                            }
                            Event::PairingQrCode { code, .. } => {
                                ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "WhatsApp Web QR code received (scan with WhatsApp > Linked Devices)");
                                match Self::render_pairing_qr(code) {
                                    Ok(rendered) => {
                                        eprintln!();
                                        eprintln!(
                                            "WhatsApp Web QR code (scan in WhatsApp > Linked Devices):"
                                        );
                                        eprintln!("{rendered}");
                                        eprintln!();
                                    }
                                    Err(err) => {
                                        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown), &format!("failed to render pairing QR in terminal: {}", err));
                                        eprintln!();
                                        eprintln!("WhatsApp Web QR payload: {code}");
                                        eprintln!();
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }});

            // Configure pair-code flow when a phone number is provided.
            if let Some(ref phone) = self.pair_phone {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "pair-code flow enabled for configured phone number"
                );
                builder = builder.with_pair_code(PairCodeOptions {
                    phone_number: phone.clone(),
                    custom_code: self.pair_code.clone(),
                    ..Default::default()
                });
            } else if self.pair_code.is_some() {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "pair_code is set but pair_phone is missing; pair code config is ignored"
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
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note), "channel received Ctrl+C");
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
                        "exceeded {} reconnect attempts, giving up",
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
                            Err(e) => ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Note
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                                &format!("failed to remove session file {}: {e}", path)
                            ),
                        }
                    }
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        "session files removed, restarting for QR pairing"
                    );
                } else {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        "bot stopped without LoggedOut; reconnecting with existing session"
                    );
                }

                let delay = Self::compute_retry_delay(attempts);
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    &format!(
                        "reconnecting in {}s (attempt {}/{})",
                        delay,
                        attempts,
                        Self::MAX_RETRIES
                    )
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
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!("typing target {} not in allowed list", recipient)
                );
                return Ok(());
            }
        }

        let deliverable_recipient = Self::resolve_outbound_recipient(&client, recipient).await?;
        let to = self.recipient_to_jid(&deliverable_recipient)?;
        client.chatstate().send_composing(&to).await.map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "Failed to send typing state (composing)"
            );
            anyhow::Error::msg(format!("Failed to send typing state (composing): {e}"))
        })?;

        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!("start typing for {}", recipient)
        );
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
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!("typing target {} not in allowed list", recipient)
                );
                return Ok(());
            }
        }

        let deliverable_recipient = Self::resolve_outbound_recipient(&client, recipient).await?;
        let to = self.recipient_to_jid(&deliverable_recipient)?;
        client.chatstate().send_paused(&to).await.map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "Failed to send typing state (paused)"
            );
            anyhow::Error::msg(format!("Failed to send typing state (paused): {e}"))
        })?;

        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!("stop typing for {}", recipient)
        );
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
        _ws_url: Option<String>,
        _alias: impl Into<String>,
        _peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
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
impl ::zeroclaw_api::attribution::Attributable for WhatsAppWebChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::WhatsappWeb,
        )
    }
    fn alias(&self) -> &str {
        "whatsapp"
    }
}

#[cfg(not(feature = "whatsapp-web"))]
#[async_trait]
impl Channel for WhatsAppWebChannel {
    fn name(&self) -> &str {
        "whatsapp"
    }

    async fn send(&self, _message: &SendMessage) -> Result<()> {
        anyhow::bail!(i18n::get_required_cli_string(
            "channel-whatsapp-web-feature-missing-error"
        ));
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        anyhow::bail!(i18n::get_required_cli_string(
            "channel-whatsapp-web-feature-missing-error"
        ));
    }

    async fn health_check(&self) -> bool {
        false
    }

    async fn start_typing(&self, _recipient: &str) -> Result<()> {
        anyhow::bail!(i18n::get_required_cli_string(
            "channel-whatsapp-web-feature-missing-error"
        ));
    }

    async fn stop_typing(&self, _recipient: &str) -> Result<()> {
        anyhow::bail!(i18n::get_required_cli_string(
            "channel-whatsapp-web-feature-missing-error"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "whatsapp-web")]
    use wacore_binary::jid::Jid;

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_channel_name() {
        let mention_only = false;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test-whatsapp.db".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        );
        assert_eq!(ch.name(), "whatsapp");
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_number_allowed_exact() {
        let mention_only = false;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test-whatsapp.db".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        );
        assert!(ch.is_number_allowed("+1234567890"));
        assert!(!ch.is_number_allowed("+9876543210"));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_number_allowed_wildcard() {
        let mention_only = false;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test.db".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["*".into()]),
        );
        assert!(ch.is_number_allowed("+1234567890"));
        assert!(ch.is_number_allowed("+9999999999"));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_number_denied_empty() {
        let mention_only = false;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test.db".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(&cfg, "whatsapp_web_test_alias", Arc::new(Vec::new));
        // Empty allowlist means "deny all" (matches channel-wide allowlist policy).
        assert!(!ch.is_number_allowed("+1234567890"));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_normalize_phone_adds_plus() {
        let mention_only = false;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test-whatsapp.db".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        );
        assert_eq!(ch.normalize_phone("1234567890"), "+1234567890");
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_normalize_phone_preserves_plus() {
        let mention_only = false;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test-whatsapp.db".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        );
        assert_eq!(ch.normalize_phone("+1234567890"), "+1234567890");
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_normalize_phone_from_jid() {
        let mention_only = false;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test-whatsapp.db".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        );
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

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn compute_reply_target_converts_lid_dm_to_phone() {
        // Non-group LID DM with mapped_phone → phone JID
        let chat_jid = "76188559093817@lid";
        let is_lid = true;
        let is_group = false;
        let result = WhatsAppWebChannel::compute_reply_target(
            chat_jid,
            is_lid,
            is_group,
            Some("15551234567"),
        );
        assert_eq!(
            result, "15551234567@s.whatsapp.net",
            "LID DM must convert to phone JID for reply delivery"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn is_lid_jid_string_detects_lid_domain() {
        assert!(WhatsAppWebChannel::is_lid_jid_string("76188559093817@lid"));
        assert!(!WhatsAppWebChannel::is_lid_jid_string(
            "15551234567@s.whatsapp.net"
        ));
        assert!(!WhatsAppWebChannel::is_lid_jid_string("+15551234567"));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn compute_reply_target_lid_dm_without_phone_fallback() {
        // Non-group LID DM without mapped_phone → falls back to chat JID
        let chat_jid = "76188559093817@lid";
        let is_lid = true;
        let is_group = false;
        let result = WhatsAppWebChannel::compute_reply_target(chat_jid, is_lid, is_group, None);
        assert_eq!(
            result, chat_jid,
            "LID DM without mapped_phone must fall back to original chat JID"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn resolve_deliverable_reply_target_converts_lid_with_phone_candidate() {
        let (target, converted) = WhatsAppWebChannel::resolve_deliverable_reply_target(
            "76188559093817@lid",
            &["+15551234567".to_string()],
        );
        assert!(converted);
        assert_eq!(target, "15551234567@s.whatsapp.net");
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn compute_reply_target_non_lid_dm_unchanged() {
        // Non-LID DM → original chat JID (no conversion needed)
        let chat_jid = "15551234567@s.whatsapp.net";
        let is_lid = false;
        let is_group = false;
        let result = WhatsAppWebChannel::compute_reply_target(
            chat_jid,
            is_lid,
            is_group,
            Some("15551234567"),
        );
        assert_eq!(
            result, chat_jid,
            "Non-LID DM must preserve original chat JID"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn resolve_deliverable_reply_target_leaves_phone_jid_unchanged() {
        let chat = "15551234567@s.whatsapp.net";
        let (target, converted) =
            WhatsAppWebChannel::resolve_deliverable_reply_target(chat, &["+15551234567".into()]);
        assert!(!converted);
        assert_eq!(target, chat);
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn compute_reply_target_group_unchanged() {
        // Group chat → original chat JID (groups don't need conversion)
        let chat_jid = "120363012345678901@g.us";
        let is_lid = false;
        let is_group = true;
        let result = WhatsAppWebChannel::compute_reply_target(
            chat_jid,
            is_lid,
            is_group,
            Some("15551234567"),
        );
        assert_eq!(
            result, chat_jid,
            "Group chat must preserve original chat JID"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn resolve_deliverable_reply_target_warns_via_unchanged_lid_when_no_candidates() {
        let chat = "76188559093817@lid";
        let (target, converted) = WhatsAppWebChannel::resolve_deliverable_reply_target(chat, &[]);
        assert!(!converted);
        assert_eq!(target, chat);
    }

    /// Regression: inbound voice tracking must use the resolved `reply_target`
    /// (phone JID), not the original LID `chat`, because `send()` looks up
    /// `voice_chats` with `message.recipient` (= `reply_target`).
    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn lid_dm_voice_tracking_key_matches_send_recipient() {
        let chat_lid = "76188559093817@lid";
        let (reply_target, converted) = WhatsAppWebChannel::resolve_deliverable_reply_target(
            chat_lid,
            &["+15551234567".to_string()],
        );
        assert!(converted);
        assert_ne!(chat_lid, reply_target);

        let mut voice_chats = std::collections::HashSet::new();
        voice_chats.insert(reply_target.clone());
        let message_recipient = reply_target.clone();
        assert!(
            voice_chats.contains(&message_recipient),
            "voice_chats must be keyed by resolved reply_target for send() lookup"
        );
        assert!(
            !voice_chats.contains(chat_lid),
            "original LID chat JID must not be the voice_chats key after LID→phone resolution"
        );
    }

    // ── lid_rejection_diagnostic: scoped LID warning ────
    //
    // The diagnostic fires only inside the `Allowlist::normalized.is_none()`
    // branch. These tests pin the three shapes the function returns; the
    // call-site composition (suffix appended to the rejection log) is
    // covered by reading the surrounding code path.

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn lid_rejection_diagnostic_empty_for_non_lid_sender() {
        let sender = Jid::pn("15551234567");
        let diag = WhatsAppWebChannel::lid_rejection_diagnostic(&sender, None);
        assert!(
            diag.is_empty(),
            "non-LID senders must not generate any LID-resolution suffix; got {diag:?}"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn lid_rejection_diagnostic_names_resolution_failure_for_lid_with_no_phone() {
        let sender = Jid::lid("76188559093817");
        let diag = WhatsAppWebChannel::lid_rejection_diagnostic(&sender, None);
        assert!(
            diag.contains("LID→phone resolution returned None"),
            "diagnostic must name the resolution failure mode #6350 describes; got {diag:?}"
        );
        assert!(
            diag.contains("76188559093817"),
            "diagnostic must surface the LID identifier so the operator can add the LID-form workaround; got {diag:?}"
        );
        assert!(
            diag.contains("allowed_numbers"),
            "diagnostic must point at the config knob to fix this; got {diag:?}"
        );
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn lid_rejection_diagnostic_distinguishes_resolved_phone_mismatch() {
        // LID resolved successfully but the resulting phone wasn't in the
        // allowlist. Different cause from the resolution failure path; the
        // operator shouldn't be steered toward the LID workaround.
        let sender = Jid::lid("76188559093817");
        let diag = WhatsAppWebChannel::lid_rejection_diagnostic(&sender, Some("15551234567"));
        assert!(
            !diag.contains("LID→phone resolution returned None"),
            "must not suggest resolution failed when mapped_phone is Some; got {diag:?}"
        );
        assert!(
            diag.contains("did not match"),
            "diagnostic must explain the resolved phone failed the allowlist; got {diag:?}"
        );
    }

    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn whatsapp_web_health_check_disconnected() {
        let mention_only = false;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test-whatsapp.db".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        );
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

        let mention_only = false;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test-whatsapp.db".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        )
        .with_transcription(tc);
        assert!(ch.transcription.is_some());
        assert!(ch.transcription_manager.is_some());
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn with_transcription_ignores_when_disabled() {
        let tc = zeroclaw_config::schema::TranscriptionConfig::default(); // enabled = false
        let mention_only = false;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test-whatsapp.db".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["+1234567890".into()]),
        )
        .with_transcription(tc);
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
    fn constructor_seeds_bot_phone_from_pair_phone() {
        let mention_only = true;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test.db".into()),
            pair_phone: Some("919211916069".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["*".into()]),
        );
        assert_eq!(*ch.bot_phone.lock(), Some("919211916069".to_string()));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn constructor_no_pair_phone_leaves_bot_phone_none() {
        let mention_only = true;
        let self_chat_mode = false;
        let cfg = zeroclaw_config::schema::WhatsAppConfig {
            enabled: true,
            session_path: Some("/tmp/test.db".into()),
            mention_only,
            self_chat_mode,
            ..Default::default()
        };
        let ch = WhatsAppWebChannel::new(
            &cfg,
            "whatsapp_web_test_alias",
            Arc::new(|| vec!["*".into()]),
        );
        assert_eq!(*ch.bot_phone.lock(), None);
    }

    // ── fromme_outside_self_chat_is_operator_trigger ───────────

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn fromme_trigger_drops_when_no_mention_patterns_configured() {
        let dm: Vec<regex::Regex> = vec![];
        let group: Vec<regex::Regex> = vec![];
        // Without configured patterns, a fromMe message in a third-party
        // DM or group must drop — there is no opt-in signal that says the
        // operator wants outbound mirrors to be treated as triggers.
        assert!(!fromme_outside_self_chat_is_operator_trigger(
            false,
            &dm,
            &group,
            "TinyBot foo"
        ));
        assert!(!fromme_outside_self_chat_is_operator_trigger(
            true,
            &dm,
            &group,
            "TinyBot foo"
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn fromme_trigger_falls_through_when_dm_pattern_matches() {
        // @ilteoood's configured workflow: dm_mention_patterns = ["TinyBot"].
        // Operator types "TinyBot translate this" in a friend's DM →
        // intentional invocation, must fall through.
        let dm = vec![
            regex::RegexBuilder::new("TinyBot")
                .case_insensitive(true)
                .build()
                .unwrap(),
        ];
        let group: Vec<regex::Regex> = vec![];
        assert!(fromme_outside_self_chat_is_operator_trigger(
            false,
            &dm,
            &group,
            "TinyBot translate this"
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn fromme_trigger_drops_when_dm_pattern_does_not_match() {
        // Operator types a normal message in a friend's DM — even with
        // patterns configured, no match means it stays an outbound mirror
        // and must be dropped to prevent impersonation.
        let dm = vec![
            regex::RegexBuilder::new("TinyBot")
                .case_insensitive(true)
                .build()
                .unwrap(),
        ];
        let group: Vec<regex::Regex> = vec![];
        assert!(!fromme_outside_self_chat_is_operator_trigger(
            false,
            &dm,
            &group,
            "see you at 7"
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn fromme_trigger_uses_group_patterns_for_group_threads() {
        // group_mention_patterns gates the group case; dm patterns must
        // not be consulted for group messages and vice versa. This pins
        // the predicate's branch selection.
        let dm: Vec<regex::Regex> = vec![
            regex::RegexBuilder::new("DmTrigger")
                .case_insensitive(true)
                .build()
                .unwrap(),
        ];
        let group = vec![
            regex::RegexBuilder::new("GroupTrigger")
                .case_insensitive(true)
                .build()
                .unwrap(),
        ];
        // In a group, only group_patterns matter.
        assert!(fromme_outside_self_chat_is_operator_trigger(
            true,
            &dm,
            &group,
            "GroupTrigger hi"
        ));
        assert!(!fromme_outside_self_chat_is_operator_trigger(
            true,
            &dm,
            &group,
            "DmTrigger hi"
        ));
        // In a DM, only dm_patterns matter.
        assert!(fromme_outside_self_chat_is_operator_trigger(
            false,
            &dm,
            &group,
            "DmTrigger hi"
        ));
        assert!(!fromme_outside_self_chat_is_operator_trigger(
            false,
            &dm,
            &group,
            "GroupTrigger hi"
        ));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn fromme_trigger_drops_when_text_is_empty() {
        // Voice notes and media-only messages return empty text. With no
        // text to match against, the operator-trigger path must drop —
        // never transcribe a fromMe voice note just to check whether it
        // is a bot trigger (cost + impersonation risk).
        let dm = vec![
            regex::RegexBuilder::new("TinyBot")
                .case_insensitive(true)
                .build()
                .unwrap(),
        ];
        let group: Vec<regex::Regex> = vec![];
        assert!(!fromme_outside_self_chat_is_operator_trigger(
            false, &dm, &group, ""
        ));
    }
}
