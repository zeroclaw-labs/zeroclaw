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

// ── Media attachment support ──────────────────────────────────────────

/// Supported WhatsApp media attachment kinds.
#[cfg(feature = "whatsapp-web")]
#[derive(Debug, Clone, Copy)]
enum WaAttachmentKind {
    Image,
    Document,
    Video,
    Audio,
}

#[cfg(feature = "whatsapp-web")]
impl WaAttachmentKind {
    /// Parse from the marker prefix (case-insensitive).
    fn from_marker(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "IMAGE" => Some(Self::Image),
            "DOCUMENT" => Some(Self::Document),
            "VIDEO" => Some(Self::Video),
            "AUDIO" => Some(Self::Audio),
            _ => None,
        }
    }

    /// Map to the wa-rs `MediaType` used for upload encryption.
    fn media_type(self) -> wa_rs_core::download::MediaType {
        match self {
            Self::Image => wa_rs_core::download::MediaType::Image,
            Self::Document => wa_rs_core::download::MediaType::Document,
            Self::Video => wa_rs_core::download::MediaType::Video,
            Self::Audio => wa_rs_core::download::MediaType::Audio,
        }
    }
}

/// A parsed media attachment from `[KIND:path]` markers in the response text.
#[cfg(feature = "whatsapp-web")]
#[derive(Debug, Clone)]
struct WaAttachment {
    kind: WaAttachmentKind,
    target: String,
}

/// Parse `[IMAGE:/path]`, `[DOCUMENT:/path]`, etc. markers out of a message.
/// Returns the cleaned text (markers removed) and a vec of attachments.
#[cfg(feature = "whatsapp-web")]
fn parse_wa_attachment_markers(message: &str) -> (String, Vec<WaAttachment>) {
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

        let Some(close_rel) = message[open..].find(']') else {
            cleaned.push_str(&message[open..]);
            break;
        };

        let close = open + close_rel;
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
            // Not a valid media marker — keep the original text.
            cleaned.push_str(&message[open..=close]);
        }

        cursor = close + 1;
    }

    (cleaned.trim().to_string(), attachments)
}

/// Infer MIME type from file extension.
#[cfg(feature = "whatsapp-web")]
fn mime_from_path(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "ogg" | "opus" => "audio/ogg",
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        _ => "application/octet-stream",
    }
}

// ── Inbound media support ─────────────────────────────────────────────

/// Maximum file size for inbound WhatsApp media downloads (20 MiB).
#[cfg(feature = "whatsapp-web")]
const WA_WEB_MAX_MEDIA_BYTES: u64 = 20 * 1024 * 1024;

/// Check whether a file path has a recognized image extension.
#[cfg(feature = "whatsapp-web")]
fn is_wa_image_extension(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
            )
        })
        .unwrap_or(false)
}

/// Derive a file extension from a MIME type for incoming media.
#[cfg(feature = "whatsapp-web")]
fn mime_to_wa_extension(mime: &str) -> &str {
    let base = mime.split(';').next().unwrap_or("").trim();
    match base.to_ascii_lowercase().as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "video/mp4" => "mp4",
        "video/3gpp" => "3gp",
        "application/pdf" => "pdf",
        _ => "bin",
    }
}

/// Try to download an incoming media message (image, document, video, sticker)
/// from WhatsApp Web and return the formatted content string with `[IMAGE:]`
/// or `[Document:]` markers matching the multimodal pipeline convention.
///
/// Returns `None` if the message contains no downloadable media or if the
/// download fails.
#[cfg(feature = "whatsapp-web")]
async fn try_download_wa_web_media(
    msg: &wa_rs_proto::whatsapp::Message,
    client: &wa_rs::Client,
    workspace: &std::path::Path,
    sender_id: &str,
) -> Option<String> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let safe_sender: String = sender_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    let sender_component = if safe_sender.is_empty() {
        "unknown"
    } else {
        &safe_sender
    };
    let save_dir = workspace.join("whatsapp_files");

    // ── Image ──────────────────────────────────────────────────────────
    if let Some(ref image_msg) = msg.image_message {
        if image_msg.file_length.unwrap_or(0) > WA_WEB_MAX_MEDIA_BYTES {
            tracing::info!("WhatsApp Web: image too large, skipping");
            return None;
        }
        let bytes = match client.download(image_msg.as_ref()).await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("WhatsApp Web: failed to download image: {e}");
                return None;
            }
        };
        if bytes.len() as u64 > WA_WEB_MAX_MEDIA_BYTES {
            tracing::warn!(
                "WhatsApp Web: downloaded image exceeds limit ({} > {}), skipping",
                bytes.len(),
                WA_WEB_MAX_MEDIA_BYTES
            );
            return None;
        }
        let ext = image_msg
            .mimetype
            .as_deref()
            .map(mime_to_wa_extension)
            .unwrap_or("jpg");
        let filename = format!("wa_image_{sender_component}_{ts}_{nonce}.{ext}");
        if let Err(e) = tokio::fs::create_dir_all(&save_dir).await {
            tracing::warn!("WhatsApp Web: failed to create save dir: {e}");
            return None;
        }
        let path = save_dir.join(&filename);
        if let Err(e) = tokio::fs::write(&path, &bytes).await {
            tracing::warn!("WhatsApp Web: failed to save image: {e}");
            return None;
        }
        tracing::info!(
            "WhatsApp Web: saved inbound image to {} ({} bytes)",
            path.display(),
            bytes.len()
        );
        let mut content = format!("[IMAGE:{}]", path.display());
        if let Some(ref caption) = image_msg.caption {
            if !caption.trim().is_empty() {
                content = format!("{}\n\n{content}", caption.trim());
            }
        }
        return Some(content);
    }

    // ── Document ───────────────────────────────────────────────────────
    if let Some(ref doc_msg) = msg.document_message {
        if doc_msg.file_length.unwrap_or(0) > WA_WEB_MAX_MEDIA_BYTES {
            tracing::info!("WhatsApp Web: document too large, skipping");
            return None;
        }
        let bytes = match client.download(doc_msg.as_ref()).await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("WhatsApp Web: failed to download document: {e}");
                return None;
            }
        };
        if bytes.len() as u64 > WA_WEB_MAX_MEDIA_BYTES {
            tracing::warn!(
                "WhatsApp Web: downloaded document exceeds limit ({} > {}), skipping",
                bytes.len(),
                WA_WEB_MAX_MEDIA_BYTES
            );
            return None;
        }
        let original_name = doc_msg.file_name.as_deref().unwrap_or("document");
        let ext = std::path::Path::new(original_name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_else(|| {
                doc_msg
                    .mimetype
                    .as_deref()
                    .map(mime_to_wa_extension)
                    .unwrap_or("bin")
            });
        let filename = format!("wa_doc_{sender_component}_{ts}_{nonce}.{ext}");
        if let Err(e) = tokio::fs::create_dir_all(&save_dir).await {
            tracing::warn!("WhatsApp Web: failed to create save dir: {e}");
            return None;
        }
        let path = save_dir.join(&filename);
        if let Err(e) = tokio::fs::write(&path, &bytes).await {
            tracing::warn!("WhatsApp Web: failed to save document: {e}");
            return None;
        }
        tracing::info!(
            "WhatsApp Web: saved inbound document '{}' to {} ({} bytes)",
            original_name,
            path.display(),
            bytes.len()
        );
        let mut content = if is_wa_image_extension(&path) {
            format!("[IMAGE:{}]", path.display())
        } else {
            format!("[Document: {}] {}", original_name, path.display())
        };
        if let Some(ref caption) = doc_msg.caption {
            if !caption.trim().is_empty() {
                content = format!("{}\n\n{content}", caption.trim());
            }
        }
        return Some(content);
    }

    // ── Video ──────────────────────────────────────────────────────────
    if let Some(ref video_msg) = msg.video_message {
        if video_msg.file_length.unwrap_or(0) > WA_WEB_MAX_MEDIA_BYTES {
            tracing::info!("WhatsApp Web: video too large, skipping");
            return None;
        }
        let bytes = match client.download(video_msg.as_ref()).await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("WhatsApp Web: failed to download video: {e}");
                return None;
            }
        };
        if bytes.len() as u64 > WA_WEB_MAX_MEDIA_BYTES {
            tracing::warn!(
                "WhatsApp Web: downloaded video exceeds limit ({} > {}), skipping",
                bytes.len(),
                WA_WEB_MAX_MEDIA_BYTES
            );
            return None;
        }
        let ext = video_msg
            .mimetype
            .as_deref()
            .map(mime_to_wa_extension)
            .unwrap_or("mp4");
        let filename = format!("wa_video_{sender_component}_{ts}_{nonce}.{ext}");
        if let Err(e) = tokio::fs::create_dir_all(&save_dir).await {
            tracing::warn!("WhatsApp Web: failed to create save dir: {e}");
            return None;
        }
        let path = save_dir.join(&filename);
        if let Err(e) = tokio::fs::write(&path, &bytes).await {
            tracing::warn!("WhatsApp Web: failed to save video: {e}");
            return None;
        }
        tracing::info!(
            "WhatsApp Web: saved inbound video to {} ({} bytes)",
            path.display(),
            bytes.len()
        );
        let mut content = format!("[Document: {}] {}", filename, path.display());
        if let Some(ref caption) = video_msg.caption {
            if !caption.trim().is_empty() {
                content = format!("{}\n\n{content}", caption.trim());
            }
        }
        return Some(content);
    }

    // ── Sticker ────────────────────────────────────────────────────────
    if let Some(ref sticker_msg) = msg.sticker_message {
        if sticker_msg.file_length.unwrap_or(0) > WA_WEB_MAX_MEDIA_BYTES {
            tracing::info!("WhatsApp Web: sticker too large, skipping");
            return None;
        }
        let bytes = match client.download(sticker_msg.as_ref()).await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("WhatsApp Web: failed to download sticker: {e}");
                return None;
            }
        };
        if bytes.len() as u64 > WA_WEB_MAX_MEDIA_BYTES {
            tracing::warn!(
                "WhatsApp Web: downloaded sticker exceeds limit ({} > {}), skipping",
                bytes.len(),
                WA_WEB_MAX_MEDIA_BYTES
            );
            return None;
        }
        let ext = sticker_msg
            .mimetype
            .as_deref()
            .map(mime_to_wa_extension)
            .unwrap_or("webp");
        let filename = format!("wa_sticker_{sender_component}_{ts}_{nonce}.{ext}");
        if let Err(e) = tokio::fs::create_dir_all(&save_dir).await {
            tracing::warn!("WhatsApp Web: failed to create save dir: {e}");
            return None;
        }
        let path = save_dir.join(&filename);
        if let Err(e) = tokio::fs::write(&path, &bytes).await {
            tracing::warn!("WhatsApp Web: failed to save sticker: {e}");
            return None;
        }
        tracing::info!(
            "WhatsApp Web: saved inbound sticker to {} ({} bytes)",
            path.display(),
            bytes.len()
        );
        return Some(format!("[IMAGE:{}]", path.display()));
    }

    None
}

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
    /// Bot handle for shutdown
    bot_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Client handle for sending messages and typing indicators
    client: Arc<Mutex<Option<Arc<wa_rs::Client>>>>,
    /// Message sender channel
    tx: Arc<Mutex<Option<tokio::sync::mpsc::Sender<ChannelMessage>>>>,
    /// Voice transcription configuration (Groq Whisper)
    transcription: Option<crate::config::TranscriptionConfig>,
    /// Workspace directory for saving downloaded media attachments.
    workspace_dir: Option<std::path::PathBuf>,
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
            bot_handle: Arc::new(Mutex::new(None)),
            client: Arc::new(Mutex::new(None)),
            tx: Arc::new(Mutex::new(None)),
            transcription: None,
            workspace_dir: None,
        }
    }

    /// Configure workspace directory for saving downloaded media attachments.
    #[cfg(feature = "whatsapp-web")]
    pub fn with_workspace_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.workspace_dir = Some(dir);
        self
    }

    /// Configure voice transcription via Groq Whisper.
    ///
    /// When `config.enabled` is false the builder is a no-op so callers can
    /// pass `config.transcription.clone()` unconditionally.
    #[cfg(feature = "whatsapp-web")]
    pub fn with_transcription(mut self, config: crate::config::TranscriptionConfig) -> Self {
        if config.enabled {
            self.transcription = Some(config);
        }
        self
    }

    /// Map a WhatsApp audio MIME type to a filename accepted by the Groq Whisper API.
    ///
    /// WhatsApp voice notes are typically `audio/ogg; codecs=opus`.
    /// MIME parameters (e.g. `; codecs=opus`) are stripped before matching so that
    /// `audio/webm; codecs=opus` maps to `voice.webm`, not `voice.opus`.
    #[cfg(feature = "whatsapp-web")]
    fn audio_mime_to_filename(mime: &str) -> &'static str {
        let base = mime
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match base.as_str() {
            "audio/ogg" | "audio/oga" => "voice.ogg",
            "audio/webm" => "voice.webm",
            "audio/opus" => "voice.opus",
            "audio/mp4" | "audio/m4a" | "audio/aac" => "voice.m4a",
            "audio/mpeg" | "audio/mp3" => "voice.mp3",
            "audio/wav" | "audio/x-wav" => "voice.wav",
            _ => "voice.ogg",
        }
    }

    /// Check if a phone number is allowed (E.164 format: +1234567890)
    #[cfg(feature = "whatsapp-web")]
    fn is_number_allowed(&self, phone: &str) -> bool {
        self.allowed_numbers.iter().any(|n| n == "*" || n == phone)
    }

    /// Normalize phone number to E.164 format
    #[cfg(feature = "whatsapp-web")]
    fn normalize_phone(&self, phone: &str) -> String {
        let trimmed = phone.trim();
        let user_part = trimmed
            .split_once('@')
            .map(|(user, _)| user)
            .unwrap_or(trimmed);
        let normalized_user = user_part.trim_start_matches('+');
        if user_part.starts_with('+') {
            format!("+{normalized_user}")
        } else {
            format!("+{normalized_user}")
        }
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

    /// Upload a file to WhatsApp media servers and send it as the appropriate message type.
    #[cfg(feature = "whatsapp-web")]
    async fn send_media_attachment(
        &self,
        client: &Arc<wa_rs::Client>,
        to: &wa_rs_binary::jid::Jid,
        attachment: &WaAttachment,
    ) -> Result<()> {
        use std::path::Path;

        let path = Path::new(&attachment.target);
        if !path.exists() {
            anyhow::bail!("Media file not found: {}", attachment.target);
        }

        let data = tokio::fs::read(path).await?;
        let file_len = data.len() as u64;
        let mimetype = mime_from_path(path).to_string();

        tracing::info!(
            "WhatsApp Web: uploading {:?} ({} bytes, {})",
            attachment.kind,
            file_len,
            mimetype
        );

        let upload = client.upload(data, attachment.kind.media_type()).await?;

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
            WaAttachmentKind::Document => {
                let file_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file")
                    .to_string();
                wa_rs_proto::whatsapp::Message {
                    document_message: Some(Box::new(
                        wa_rs_proto::whatsapp::message::DocumentMessage {
                            url: Some(upload.url),
                            direct_path: Some(upload.direct_path),
                            media_key: Some(upload.media_key),
                            file_enc_sha256: Some(upload.file_enc_sha256),
                            file_sha256: Some(upload.file_sha256),
                            file_length: Some(upload.file_length),
                            mimetype: Some(mimetype),
                            file_name: Some(file_name),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                }
            }
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
            WaAttachmentKind::Audio => wa_rs_proto::whatsapp::Message {
                audio_message: Some(Box::new(wa_rs_proto::whatsapp::message::AudioMessage {
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
        };

        let msg_id = client.send_message(to.clone(), outgoing).await?;
        tracing::info!(
            "WhatsApp Web: sent {:?} media (id: {})",
            attachment.kind,
            msg_id
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

        // Parse media attachment markers from the response text.
        let (text_without_markers, attachments) = parse_wa_attachment_markers(&message.content);

        // Send any text portion first.
        if !text_without_markers.is_empty() {
            let text_msg = wa_rs_proto::whatsapp::Message {
                conversation: Some(text_without_markers.clone()),
                ..Default::default()
            };
            let msg_id = client.send_message(to.clone(), text_msg).await?;
            tracing::debug!(
                "WhatsApp Web: sent text to {} (id: {})",
                message.recipient,
                msg_id
            );
        }

        // Send each media attachment.
        for attachment in &attachments {
            if let Err(e) = self.send_media_attachment(&client, &to, attachment).await {
                tracing::error!(
                    "WhatsApp Web: failed to send {:?} attachment {}: {}",
                    attachment.kind,
                    attachment.target,
                    e
                );
                // Fall back to sending the path as text so the user knows something went wrong.
                let fallback = wa_rs_proto::whatsapp::Message {
                    conversation: Some(format!("[Failed to send media: {}]", attachment.target)),
                    ..Default::default()
                };
                let _ = client.send_message(to.clone(), fallback).await;
            }
        }

        // If there were no markers and no text (shouldn't happen), send original content.
        if attachments.is_empty()
            && text_without_markers.is_empty()
            && !message.content.trim().is_empty()
        {
            let outgoing = wa_rs_proto::whatsapp::Message {
                conversation: Some(message.content.clone()),
                ..Default::default()
            };
            let message_id = client.send_message(to, outgoing).await?;
            tracing::debug!(
                "WhatsApp Web: sent message to {} (id: {})",
                message.recipient,
                message_id
            );
        }

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

        tracing::info!(
            "WhatsApp Web channel starting (session: {})",
            self.session_path
        );

        // Initialize storage backend
        let storage = RusqliteStore::new(&self.session_path)?;
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

        // Build the bot
        let tx_clone = tx.clone();
        let allowed_numbers = self.allowed_numbers.clone();
        let transcription = self.transcription.clone();
        let workspace_dir = self.workspace_dir.clone();

        let mut builder = Bot::builder()
            .with_backend(backend)
            .with_transport_factory(transport_factory)
            .with_http_client(http_client)
            .on_event(move |event, _client| {
                let tx_inner = tx_clone.clone();
                let allowed_numbers = allowed_numbers.clone();
                let transcription = transcription.clone();
                let workspace_dir = workspace_dir.clone();
                async move {
                    match event {
                        Event::Message(msg, info) => {
                            // Extract message content
                            let text = msg.text_content().unwrap_or("");
                            let sender = info.source.sender.user().to_string();
                            let chat = info.source.chat.to_string();

                            tracing::info!(
                                "WhatsApp Web message from {} in {}: {}",
                                sender,
                                chat,
                                text
                            );

                            // Check if sender is allowed
                            let normalized = if sender.starts_with('+') {
                                sender.clone()
                            } else {
                                format!("+{sender}")
                            };

                            if allowed_numbers.iter().any(|n| n == "*" || n == &normalized) {
                                let trimmed = text.trim();

                                // Try inbound media download (image/doc/video/sticker)
                                let media_content = if let Some(ref ws) = workspace_dir {
                                    try_download_wa_web_media(&msg, &_client, ws, &sender).await
                                } else {
                                    None
                                };

                                let content = if let Some(media) = media_content {
                                    // Media downloaded; prepend any text/caption
                                    if !trimmed.is_empty() {
                                        format!("{trimmed}\n\n{media}")
                                    } else {
                                        media
                                    }
                                } else if !trimmed.is_empty() {
                                    trimmed.to_string()
                                } else if let Some(ref tc) = transcription {
                                    // Attempt to transcribe audio/voice messages
                                    if let Some(ref audio_msg) = msg.audio_message {
                                        let duration_secs =
                                            audio_msg.seconds.unwrap_or(0) as u64;
                                        if duration_secs > tc.max_duration_secs {
                                            tracing::info!(
                                                "WhatsApp Web: voice message too long \
                                                 ({duration_secs}s > {}s), skipping",
                                                tc.max_duration_secs
                                            );
                                            return;
                                        }
                                        let mime = audio_msg
                                            .mimetype
                                            .as_deref()
                                            .unwrap_or("audio/ogg");
                                        let file_name =
                                            WhatsAppWebChannel::audio_mime_to_filename(mime);
                                        // download() decrypts the media in one step.
                                        // audio_msg is Box<AudioMessage>; .as_ref() yields
                                        // &AudioMessage which implements Downloadable.
                                        match _client.download(audio_msg.as_ref()).await {
                                            Ok(audio_bytes) => {
                                                match super::transcription::transcribe_audio(
                                                    audio_bytes,
                                                    file_name,
                                                    tc,
                                                )
                                                .await
                                                {
                                                    Ok(t) if !t.trim().is_empty() => {
                                                        format!("[Voice] {}", t.trim())
                                                    }
                                                    Ok(_) => {
                                                        tracing::info!(
                                                            "WhatsApp Web: voice transcription \
                                                             returned empty text, skipping"
                                                        );
                                                        return;
                                                    }
                                                    Err(e) => {
                                                        tracing::warn!(
                                                            "WhatsApp Web: voice transcription \
                                                             failed: {e}"
                                                        );
                                                        return;
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                tracing::warn!(
                                                    "WhatsApp Web: failed to download voice \
                                                     audio: {e}"
                                                );
                                                return;
                                            }
                                        }
                                    } else {
                                        tracing::debug!(
                                            "WhatsApp Web: ignoring non-text/non-audio \
                                             message from {}",
                                            normalized
                                        );
                                        return;
                                    }
                                } else if workspace_dir.is_none() {
                                    tracing::debug!(
                                        "WhatsApp Web: workspace_dir not configured, \
                                         cannot save media from {}",
                                        normalized
                                    );
                                    return;
                                } else {
                                    tracing::debug!(
                                        "WhatsApp Web: ignoring empty or non-text message \
                                         from {}",
                                        normalized
                                    );
                                    return;
                                };

                                if let Err(e) = tx_inner
                                    .send(ChannelMessage {
                                        id: uuid::Uuid::new_v4().to_string(),
                                        channel: "whatsapp".to_string(),
                                        sender: normalized.clone(),
                                        // Reply to the originating chat JID (DM or group).
                                        reply_target: chat,
                                        content,
                                        timestamp: chrono::Utc::now().timestamp() as u64,
                                        thread_ts: None,
                                    })
                                    .await
                                {
                                    tracing::error!("Failed to send message to channel: {}", e);
                                }
                            } else {
                                tracing::warn!("WhatsApp Web: message from {} not in allowed list", normalized);
                            }
                        }
                        Event::Connected(_) => {
                            tracing::info!("WhatsApp Web connected successfully");
                        }
                        Event::LoggedOut(_) => {
                            tracing::warn!("WhatsApp Web was logged out");
                        }
                        Event::StreamError(stream_error) => {
                            tracing::error!("WhatsApp Web stream error: {:?}", stream_error);
                        }
                        Event::PairingCode { code, .. } => {
                            tracing::info!("WhatsApp Web pair code received: {}", code);
                            tracing::info!(
                                "Link your phone by entering this code in WhatsApp > Linked Devices"
                            );
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
                                    tracing::info!("WhatsApp Web QR payload: {}", code);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            })
            ;

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

        // Wait for shutdown signal
        let (_shutdown_tx, mut shutdown_rx) = tokio::sync::broadcast::channel::<()>(1);

        select! {
            _ = shutdown_rx.recv() => {
                tracing::info!("WhatsApp Web channel shutting down");
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("WhatsApp Web channel received Ctrl+C");
            }
        }

        *self.client.lock() = None;
        if let Some(handle) = self.bot_handle.lock().take() {
            handle.abort();
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
    ) -> Self {
        Self { _private: () }
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
    fn whatsapp_web_render_pairing_qr_rejects_empty_payload() {
        let err = WhatsAppWebChannel::render_pairing_qr("   ").expect_err("empty payload");
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn whatsapp_web_render_pairing_qr_outputs_multiline_text() {
        let rendered =
            WhatsAppWebChannel::render_pairing_qr("https://example.com/whatsapp-pairing")
                .expect("rendered QR");
        assert!(rendered.lines().count() > 10);
        assert!(rendered.trim().len() > 64);
    }

    #[tokio::test]
    #[cfg(feature = "whatsapp-web")]
    async fn whatsapp_web_health_check_disconnected() {
        let ch = make_channel();
        assert!(!ch.health_check().await);
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn parse_wa_markers_image() {
        let msg = "Here is the timeline [IMAGE:/tmp/chart.png]";
        let (text, attachments) = parse_wa_attachment_markers(msg);
        assert_eq!(text, "Here is the timeline");
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].target, "/tmp/chart.png");
        assert!(matches!(attachments[0].kind, WaAttachmentKind::Image));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn parse_wa_markers_multiple() {
        let msg = "Text [IMAGE:/a.png] more [DOCUMENT:/b.pdf]";
        let (text, attachments) = parse_wa_attachment_markers(msg);
        assert_eq!(text, "Text  more");
        assert_eq!(attachments.len(), 2);
        assert!(matches!(attachments[0].kind, WaAttachmentKind::Image));
        assert!(matches!(attachments[1].kind, WaAttachmentKind::Document));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn parse_wa_markers_no_markers() {
        let msg = "Just regular text";
        let (text, attachments) = parse_wa_attachment_markers(msg);
        assert_eq!(text, "Just regular text");
        assert!(attachments.is_empty());
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn parse_wa_markers_unknown_kind_preserved() {
        let msg = "Check [UNKNOWN:/foo] out";
        let (text, attachments) = parse_wa_attachment_markers(msg);
        assert_eq!(text, "Check [UNKNOWN:/foo] out");
        assert!(attachments.is_empty());
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
    fn with_transcription_skips_when_disabled() {
        let tc = crate::config::TranscriptionConfig::default(); // enabled = false
        let ch = make_channel().with_transcription(tc);
        assert!(ch.transcription.is_none());
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn audio_mime_to_filename_maps_whatsapp_voice_note() {
        // WhatsApp voice notes typically use this MIME type
        assert_eq!(
            WhatsAppWebChannel::audio_mime_to_filename("audio/ogg; codecs=opus"),
            "voice.ogg"
        );
        assert_eq!(
            WhatsAppWebChannel::audio_mime_to_filename("audio/ogg"),
            "voice.ogg"
        );
        assert_eq!(
            WhatsAppWebChannel::audio_mime_to_filename("audio/opus"),
            "voice.opus"
        );
        assert_eq!(
            WhatsAppWebChannel::audio_mime_to_filename("audio/mp4"),
            "voice.m4a"
        );
        assert_eq!(
            WhatsAppWebChannel::audio_mime_to_filename("audio/mpeg"),
            "voice.mp3"
        );
        assert_eq!(
            WhatsAppWebChannel::audio_mime_to_filename("audio/wav"),
            "voice.wav"
        );
        assert_eq!(
            WhatsAppWebChannel::audio_mime_to_filename("audio/webm"),
            "voice.webm"
        );
        // Regression: webm+opus codec parameter must not match the opus branch
        assert_eq!(
            WhatsAppWebChannel::audio_mime_to_filename("audio/webm; codecs=opus"),
            "voice.webm"
        );
        assert_eq!(
            WhatsAppWebChannel::audio_mime_to_filename("audio/x-wav"),
            "voice.wav"
        );
        // Unknown types default to ogg (safe default for WhatsApp voice notes)
        assert_eq!(
            WhatsAppWebChannel::audio_mime_to_filename("application/octet-stream"),
            "voice.ogg"
        );
    }

    // ══════════════════════════════════════════════════════════
    // INBOUND MEDIA — Helper function tests
    // ══════════════════════════════════════════════════════════

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn mime_to_wa_extension_known_types() {
        assert_eq!(mime_to_wa_extension("image/jpeg"), "jpg");
        assert_eq!(mime_to_wa_extension("image/png"), "png");
        assert_eq!(mime_to_wa_extension("image/webp"), "webp");
        assert_eq!(mime_to_wa_extension("image/gif"), "gif");
        assert_eq!(mime_to_wa_extension("video/mp4"), "mp4");
        assert_eq!(mime_to_wa_extension("video/3gpp"), "3gp");
        assert_eq!(mime_to_wa_extension("application/pdf"), "pdf");
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn mime_to_wa_extension_unknown_fallback() {
        assert_eq!(mime_to_wa_extension("application/octet-stream"), "bin");
        assert_eq!(mime_to_wa_extension("text/plain"), "bin");
        assert_eq!(mime_to_wa_extension(""), "bin");
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn mime_to_wa_extension_with_params() {
        assert_eq!(mime_to_wa_extension("image/jpeg; charset=utf-8"), "jpg");
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn is_wa_image_extension_recognizes_images() {
        assert!(is_wa_image_extension(std::path::Path::new("photo.png")));
        assert!(is_wa_image_extension(std::path::Path::new("photo.jpg")));
        assert!(is_wa_image_extension(std::path::Path::new("photo.jpeg")));
        assert!(is_wa_image_extension(std::path::Path::new("photo.gif")));
        assert!(is_wa_image_extension(std::path::Path::new("photo.webp")));
        assert!(is_wa_image_extension(std::path::Path::new("PHOTO.PNG")));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn is_wa_image_extension_rejects_non_images() {
        assert!(!is_wa_image_extension(std::path::Path::new("file.pdf")));
        assert!(!is_wa_image_extension(std::path::Path::new("file.mp4")));
        assert!(!is_wa_image_extension(std::path::Path::new("file")));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn with_workspace_dir_sets_field() {
        let ch = make_channel().with_workspace_dir(std::path::PathBuf::from("/tmp/ws"));
        assert_eq!(ch.workspace_dir, Some(std::path::PathBuf::from("/tmp/ws")));
    }

    #[test]
    #[cfg(feature = "whatsapp-web")]
    fn with_workspace_dir_default_is_none() {
        let ch = make_channel();
        assert!(ch.workspace_dir.is_none());
    }
}
