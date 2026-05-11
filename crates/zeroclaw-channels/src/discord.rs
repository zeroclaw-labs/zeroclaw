use anyhow::{Context as _, anyhow};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use reqwest::multipart::{Form, Part};
use serde_json::json;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;
use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
};
use zeroclaw_api::media::MediaAttachment;

/// Discord channel — connects via Gateway WebSocket for real-time messages
pub struct DiscordChannel {
    bot_token: String,
    guild_id: Option<String>,
    allowed_users: Vec<String>,
    listen_to_bots: bool,
    mention_only: bool,
    typing_handles: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    /// Per-channel proxy URL override.
    proxy_url: Option<String>,
    /// Voice transcription config — when set, audio attachments are
    /// downloaded, transcribed, and their text inlined into the message.
    transcription: Option<zeroclaw_config::schema::TranscriptionConfig>,
    transcription_manager: Option<std::sync::Arc<super::transcription::TranscriptionManager>>,
    /// Workspace directory for saving downloaded inbound media attachments.
    workspace_dir: Option<PathBuf>,
    /// Streaming mode: Off, Partial (draft edits), or MultiMessage (paragraph splits).
    stream_mode: zeroclaw_config::schema::StreamMode,
    /// Minimum interval (ms) between draft message edits (Partial mode only).
    draft_update_interval_ms: u64,
    /// Delay (ms) between sending each message chunk (MultiMessage mode only).
    multi_message_delay_ms: u64,
    /// Per-channel rate-limit tracking for draft edits.
    last_draft_edit: Mutex<HashMap<String, std::time::Instant>>,
    /// Tracks how much text has been sent in MultiMessage mode.
    multi_message_sent_len: Mutex<HashMap<String, usize>>,
    /// Thread context captured from `send_draft()` for MultiMessage paragraph delivery.
    multi_message_thread_ts: Mutex<HashMap<String, Option<String>>>,
    /// Stall-watchdog timeout in seconds (0 = disabled).
    stall_timeout_secs: u64,
    pending_approvals: Arc<AsyncMutex<HashMap<String, oneshot::Sender<ChannelApprovalResponse>>>>,
    /// Seconds to wait for an operator reply to a `request_approval` prompt
    /// before treating the silence as a deny. Default 300.
    approval_timeout_secs: u64,
    /// Cached `channel_id -> is_thread` lookups. Populated lazily on first
    /// inbound message from a channel via `GET /channels/{id}`. Thread type
    /// is stable for the channel's lifetime so the cache lives as long as
    /// the channel instance.
    thread_channels: Arc<AsyncMutex<HashMap<String, bool>>>,
}

impl DiscordChannel {
    pub fn new(
        bot_token: String,
        guild_id: Option<String>,
        allowed_users: Vec<String>,
        listen_to_bots: bool,
        mention_only: bool,
    ) -> Self {
        Self {
            bot_token,
            guild_id,
            allowed_users,
            listen_to_bots,
            mention_only,
            typing_handles: Mutex::new(HashMap::new()),
            proxy_url: None,
            transcription: None,
            transcription_manager: None,
            workspace_dir: None,
            stream_mode: zeroclaw_config::schema::StreamMode::Off,
            draft_update_interval_ms: 1000,
            multi_message_delay_ms: 800,
            last_draft_edit: Mutex::new(HashMap::new()),
            multi_message_sent_len: Mutex::new(HashMap::new()),
            multi_message_thread_ts: Mutex::new(HashMap::new()),
            stall_timeout_secs: 0,
            pending_approvals: Arc::new(AsyncMutex::new(HashMap::new())),
            approval_timeout_secs: 300,
            thread_channels: Arc::new(AsyncMutex::new(HashMap::new())),
        }
    }

    /// Set a per-channel proxy URL that overrides the global proxy config.
    pub fn with_proxy_url(mut self, proxy_url: Option<String>) -> Self {
        self.proxy_url = proxy_url;
        self
    }

    pub fn with_approval_timeout_secs(mut self, secs: u64) -> Self {
        self.approval_timeout_secs = secs;
        self
    }

    /// Configure workspace directory for saving downloaded attachments.
    pub fn with_workspace_dir(mut self, dir: PathBuf) -> Self {
        self.workspace_dir = Some(dir);
        self
    }

    /// Configure voice transcription for audio attachments.
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

    /// Configure streaming mode for progressive draft updates or multi-message delivery.
    pub fn with_streaming(
        mut self,
        stream_mode: zeroclaw_config::schema::StreamMode,
        draft_update_interval_ms: u64,
        multi_message_delay_ms: u64,
    ) -> Self {
        self.stream_mode = stream_mode;
        self.draft_update_interval_ms = draft_update_interval_ms;
        self.multi_message_delay_ms = multi_message_delay_ms;
        self
    }

    /// Set the stall-watchdog timeout (0 = disabled).
    pub fn with_stall_timeout(mut self, secs: u64) -> Self {
        self.stall_timeout_secs = secs;
        self
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_channel_proxy_client(
            "channel.discord",
            self.proxy_url.as_deref(),
        )
    }

    /// Check if a Discord user ID is in the allowlist.
    /// Empty list means deny everyone until explicitly configured.
    /// `"*"` means allow everyone.
    fn is_user_allowed(&self, user_id: &str) -> bool {
        self.allowed_users.iter().any(|u| u == "*" || u == user_id)
    }

    fn bot_user_id_from_token(token: &str) -> Option<String> {
        // Discord bot tokens are base64(bot_user_id).timestamp.hmac
        let part = token.split('.').next()?;
        base64_decode(part)
    }

    /// Resolve whether `channel_id` is a Discord thread (ANNOUNCEMENT,
    /// PUBLIC, or PRIVATE thread) via `GET /channels/{id}`. Results are
    /// cached for the channel instance's lifetime: thread-ness is stable
    /// for a given channel ID, so one lookup per ID per process. Failures
    /// (network, 429, missing `type` field) fall through to `false` so a
    /// transient API hiccup never blocks inbound delivery.
    async fn is_thread_channel(&self, client: &reqwest::Client, channel_id: &str) -> bool {
        {
            let cache = self.thread_channels.lock().await;
            if let Some(&value) = cache.get(channel_id) {
                return value;
            }
        }

        // Only a successful API response is cached. A transient network blip
        // or 429 must not poison the cache for the channel's lifetime; the
        // next message should retry the lookup. Failure paths return `false`
        // (the safe default) without writing to the cache. The whole request
        // is wrapped in an explicit timeout so a hung Discord API call can
        // never stall the listener; the shared channel HTTP client may not
        // carry a request-level timeout.
        let url = format!("https://discord.com/api/v10/channels/{channel_id}");
        let lookup = async {
            let resp = client
                .get(&url)
                .header("Authorization", format!("Bot {}", self.bot_token))
                .send()
                .await
                .map_err(|e| anyhow!("request failed: {e}"))?;
            if !resp.status().is_success() {
                anyhow::bail!("non-success status {}", resp.status());
            }
            let body: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| anyhow!("body parse failed: {e}"))?;
            Ok::<bool, anyhow::Error>(
                body.get("type")
                    .and_then(serde_json::Value::as_u64)
                    .map(is_thread_channel_type)
                    .unwrap_or(false),
            )
        };
        let is_thread = match tokio::time::timeout(THREAD_LOOKUP_TIMEOUT, lookup).await {
            Ok(Ok(value)) => value,
            Ok(Err(e)) => {
                tracing::debug!(channel_id, error = %e, "discord: channel lookup failed");
                return false;
            }
            Err(_) => {
                tracing::debug!(
                    channel_id,
                    timeout_secs = THREAD_LOOKUP_TIMEOUT.as_secs(),
                    "discord: channel lookup timed out"
                );
                return false;
            }
        };

        self.thread_channels
            .lock()
            .await
            .insert(channel_id.to_string(), is_thread);
        is_thread
    }

    /// Apply the trust-boundary / delivery-failure emoji reactions to the
    /// bot's just-sent message. Best-effort: reaction failures are debug
    /// logged but never propagated. `message_id` being `None` (e.g. when
    /// every chunk failed to post) skips the reaction step entirely.
    async fn apply_failure_reactions(
        &self,
        channel_id: &str,
        message_id: Option<&str>,
        reactions: &[&'static str],
    ) {
        let Some(message_id) = message_id else {
            return;
        };
        for emoji in reactions {
            if let Err(e) = self.add_reaction(channel_id, message_id, emoji).await {
                tracing::debug!(
                    emoji,
                    error = %e,
                    "discord: failed to add failure reaction to outgoing message"
                );
            }
        }
    }
}

/// Whether a Discord channel type integer identifies a thread.
/// Discord channel types `10` (ANNOUNCEMENT_THREAD), `11` (PUBLIC_THREAD),
/// and `12` (PRIVATE_THREAD) per the Channel Types documentation.
const fn is_thread_channel_type(channel_type: u64) -> bool {
    matches!(channel_type, 10..=12)
}

/// Hard cap on `GET /channels/{id}` while resolving whether an inbound
/// channel is a thread. Discord normally responds in under 200 ms; this
/// is a safety bound so a hung request cannot stall the listener.
const THREAD_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Process Discord message attachments in a single pass.
///
/// Returns the text block appended to the agent's prompt and the structured
/// `MediaAttachment` list consumed by the media pipeline. Each attachment is
/// downloaded at most once: text/* is inlined as text, audio is transcribed
/// inline when a transcription manager is configured (otherwise it goes
/// through the media pipeline), and image/video/document attachments are
/// saved to the workspace and emitted as `[KIND:<path>]` markers plus a
/// `MediaAttachment` for vision-capable providers.
async fn process_attachments(
    attachments: &[serde_json::Value],
    client: &reqwest::Client,
    workspace_dir: Option<&Path>,
    transcription_manager: Option<&super::transcription::TranscriptionManager>,
) -> (String, Vec<MediaAttachment>) {
    let mut text_parts: Vec<String> = Vec::new();
    let mut media: Vec<MediaAttachment> = Vec::new();

    for att in attachments {
        let ct = att
            .get("content_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let name = att
            .get("filename")
            .and_then(|v| v.as_str())
            .unwrap_or("file");
        let Some(url) = att.get("url").and_then(|v| v.as_str()) else {
            tracing::warn!(name, "discord: attachment has no url, skipping");
            continue;
        };

        if ct.starts_with("text/") {
            match client.get(url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(text) = resp.text().await {
                        text_parts.push(format!("[{name}]\n{text}"));
                    }
                }
                Ok(resp) => {
                    tracing::warn!(name, status = %resp.status(), "discord attachment fetch failed");
                }
                Err(e) => {
                    tracing::warn!(name, error = %e, "discord attachment fetch error");
                }
            }
            continue;
        }

        let is_audio = is_discord_audio_attachment(ct, name);

        // Audio with channel-level transcription configured: transcribe
        // inline so the agent receives `[Voice] <transcript>` text rather
        // than opaque bytes through the media pipeline.
        if is_audio && let Some(manager) = transcription_manager {
            let bytes = match download_attachment_bytes(client, url, name).await {
                Some(b) => b,
                None => continue,
            };
            match manager.transcribe(&bytes, name).await {
                Ok(text) => {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        tracing::info!(
                            "Discord: transcribed audio attachment {} ({} chars)",
                            name,
                            trimmed.len()
                        );
                        text_parts.push(format!("[Voice] {trimmed}"));
                    }
                }
                Err(e) => {
                    tracing::warn!(name, error = %e, "discord: voice transcription failed");
                }
            }
            continue;
        }

        let marker_kind = marker_kind_for(ct, is_audio);

        let bytes = match download_attachment_bytes(client, url, name).await {
            Some(b) => b,
            None => continue,
        };

        let marker_target = match workspace_dir {
            Some(dir) => match save_attachment_bytes_to_workspace(dir, name, &bytes).await {
                Ok(local_path) => local_path.display().to_string(),
                Err(e) => {
                    tracing::warn!(name, kind = marker_kind, error = %e, "discord: attachment save failed, falling back to url");
                    url.to_string()
                }
            },
            None => url.to_string(),
        };
        text_parts.push(format!("[{marker_kind}:{marker_target}]"));

        media.push(MediaAttachment {
            file_name: name.to_string(),
            data: bytes,
            mime_type: if ct.is_empty() {
                None
            } else {
                Some(ct.to_string())
            },
        });
    }

    (text_parts.join("\n---\n"), media)
}

/// Download an attachment URL into memory, with structured warn-logging on
/// each failure mode. Returns `None` when the attachment should be skipped.
async fn download_attachment_bytes(
    client: &reqwest::Client,
    url: &str,
    name: &str,
) -> Option<Vec<u8>> {
    match client.get(url).send().await {
        Ok(resp) if resp.status().is_success() => match resp.bytes().await {
            Ok(b) => Some(b.to_vec()),
            Err(e) => {
                tracing::warn!(name, error = %e, "discord: failed to read attachment bytes");
                None
            }
        },
        Ok(resp) => {
            tracing::warn!(name, status = %resp.status(), "discord: attachment download failed");
            None
        }
        Err(e) => {
            tracing::warn!(name, error = %e, "discord: attachment fetch error");
            None
        }
    }
}

async fn save_attachment_bytes_to_workspace(
    workspace_dir: &Path,
    filename: &str,
    bytes: &[u8],
) -> anyhow::Result<PathBuf> {
    let save_dir = workspace_dir.join("discord_files");
    tokio::fs::create_dir_all(&save_dir).await?;

    let safe_name = Path::new(filename)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("attachment");
    let local_name = format!("{}_{}", Uuid::new_v4(), safe_name);
    let local_path = save_dir.join(local_name);

    tokio::fs::write(&local_path, bytes).await?;
    Ok(local_path)
}

/// Audio file extensions accepted for voice transcription.
const DISCORD_AUDIO_EXTENSIONS: &[&str] = &[
    "flac", "mp3", "mpeg", "mpga", "mp4", "m4a", "ogg", "oga", "opus", "wav", "webm",
];

/// Check if a content type or filename indicates an audio file.
fn is_discord_audio_attachment(content_type: &str, filename: &str) -> bool {
    if content_type.starts_with("audio/") {
        return true;
    }
    if let Some(ext) = filename.rsplit('.').next() {
        return DISCORD_AUDIO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str());
    }
    false
}

/// Map a Discord attachment's content type plus audio-detection result to
/// the canonical outbound marker kind. Pulled out of `process_attachments`
/// so the MIME-to-marker dispatch can be unit-tested without a live HTTP
/// download.
fn marker_kind_for(content_type: &str, is_audio: bool) -> &'static str {
    if content_type.starts_with("image/") {
        "IMAGE"
    } else if is_audio {
        "AUDIO"
    } else if content_type.starts_with("video/") {
        "VIDEO"
    } else {
        "DOCUMENT"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DiscordAttachmentKind {
    Image,
    Document,
    Video,
    Audio,
    Voice,
}

impl DiscordAttachmentKind {
    fn from_marker(kind: &str) -> Option<Self> {
        match kind.trim().to_ascii_uppercase().as_str() {
            "IMAGE" | "PHOTO" => Some(Self::Image),
            "DOCUMENT" | "FILE" => Some(Self::Document),
            "VIDEO" => Some(Self::Video),
            "AUDIO" => Some(Self::Audio),
            "VOICE" => Some(Self::Voice),
            _ => None,
        }
    }

    fn marker_name(&self) -> &'static str {
        match self {
            Self::Image => "IMAGE",
            Self::Document => "DOCUMENT",
            Self::Video => "VIDEO",
            Self::Audio => "AUDIO",
            Self::Voice => "VOICE",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiscordAttachment {
    kind: DiscordAttachmentKind,
    target: String,
}

fn parse_attachment_markers(message: &str) -> (String, Vec<DiscordAttachment>) {
    let mut cleaned = String::with_capacity(message.len());
    let mut attachments = Vec::new();
    let mut cursor = 0usize;

    while let Some(rel_start) = message[cursor..].find('[') {
        let start = cursor + rel_start;
        cleaned.push_str(&message[cursor..start]);

        let Some(rel_end) = message[start..].find(']') else {
            cleaned.push_str(&message[start..]);
            cursor = message.len();
            break;
        };
        let end = start + rel_end;
        let marker_text = &message[start + 1..end];

        let parsed = marker_text.split_once(':').and_then(|(kind, target)| {
            let kind = DiscordAttachmentKind::from_marker(kind)?;
            let target = target.trim();
            if target.is_empty() {
                return None;
            }
            Some(DiscordAttachment {
                kind,
                target: target.to_string(),
            })
        });

        if let Some(attachment) = parsed {
            attachments.push(attachment);
        } else {
            cleaned.push_str(&message[start..=end]);
        }

        cursor = end + 1;
    }

    if cursor < message.len() {
        cleaned.push_str(&message[cursor..]);
    }

    (cleaned.trim().to_string(), attachments)
}

/// Resolved outbound attachment target after sandbox validation.
#[derive(Debug)]
enum DiscordMarkerTarget {
    Local(PathBuf),
    Http(String),
}

/// Why a marker target was rejected. Drives the user-facing emoji reaction
/// on the bot's outgoing message: `Refused` (trust-boundary rejection) maps
/// to 🚫, `NotFound` (path didn't resolve on disk) maps to ⚠️. The
/// distinction matters because a chatter should see at a glance that the
/// bot deliberately declined a target rather than tried and failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiscordMarkerFailure {
    /// Trust-boundary refusal: disallowed scheme, relative path, missing
    /// workspace_dir, or canonicalised path outside the workspace.
    Refused,
    /// Path passed scheme/absolute/workspace checks but did not resolve
    /// to anything on disk.
    NotFound,
}

#[derive(Debug)]
enum DiscordMarkerError {
    Refused(anyhow::Error),
    NotFound(anyhow::Error),
}

impl DiscordMarkerError {
    fn kind(&self) -> DiscordMarkerFailure {
        match self {
            Self::Refused(_) => DiscordMarkerFailure::Refused,
            Self::NotFound(_) => DiscordMarkerFailure::NotFound,
        }
    }
}

impl std::fmt::Display for DiscordMarkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(e) | Self::NotFound(e) => write!(f, "{e}"),
        }
    }
}

/// Validate an outbound marker target against Discord's trust-boundary policy.
///
/// The orchestrator system prompt mandates absolute paths for media markers,
/// and the workspace is the only directory the agent is authorised to
/// expose to chatters:
///
/// * `http`/`https` URLs are accepted and inlined as links.
/// * Any other URL scheme (`file:`, `data:`, custom `://`) is refused.
/// * Local paths must be absolute. Relative paths are agent
///   misconfiguration and dropped, not silently resolved against cwd.
/// * Absolute paths are canonicalised and must resolve inside
///   `workspace_dir`. Anything outside or any traversal escape is
///   refused; a path that simply doesn't exist on disk returns
///   `NotFound`, which the caller renders differently from a refusal.
/// * When `workspace_dir` is not configured, no local path can be safely
///   bounded, so all local targets are refused.
fn validate_marker_target(
    target: &str,
    workspace_dir: Option<&Path>,
) -> Result<DiscordMarkerTarget, DiscordMarkerError> {
    if target.starts_with("http://") || target.starts_with("https://") {
        return Ok(DiscordMarkerTarget::Http(target.to_string()));
    }
    if target.contains("://") {
        let scheme = target.split("://").next().unwrap_or("?");
        return Err(DiscordMarkerError::Refused(anyhow!(
            "discord: marker target uses disallowed scheme {scheme:?}; only http/https and absolute workspace paths are accepted"
        )));
    }
    if target.starts_with("data:") || target.starts_with("file:") {
        return Err(DiscordMarkerError::Refused(anyhow!(
            "discord: marker target uses disallowed scheme; only http/https and absolute workspace paths are accepted"
        )));
    }

    let target_path = Path::new(target);
    if !target_path.is_absolute() {
        return Err(DiscordMarkerError::Refused(anyhow!(
            "discord: marker target {target} is not an absolute path; the agent must emit absolute paths inside workspace_dir"
        )));
    }

    let workspace = workspace_dir.ok_or_else(|| {
        DiscordMarkerError::Refused(anyhow!(
            "discord: marker target {target} is a local path but the channel was started without a workspace_dir, refusing for safety"
        ))
    })?;
    let workspace_canon = std::fs::canonicalize(workspace)
        .with_context(|| format!("canonicalize workspace {}", workspace.display()))
        .map_err(DiscordMarkerError::Refused)?;
    let target_canon = match std::fs::canonicalize(target_path) {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(DiscordMarkerError::NotFound(anyhow!(
                "discord: marker target {target} not found on disk"
            )));
        }
        Err(e) => {
            return Err(DiscordMarkerError::Refused(
                anyhow::Error::from(e).context(format!("canonicalize marker target {target}")),
            ));
        }
    };

    if !target_canon.starts_with(&workspace_canon) {
        return Err(DiscordMarkerError::Refused(anyhow!(
            "discord: marker target {target} resolves to {} which is outside workspace_dir {}; refusing",
            target_canon.display(),
            workspace_canon.display(),
        )));
    }
    Ok(DiscordMarkerTarget::Local(target_canon))
}

fn classify_outgoing_attachments(
    attachments: &[DiscordAttachment],
    workspace_dir: Option<&Path>,
) -> (
    Vec<PathBuf>,
    Vec<String>,
    Vec<(String, DiscordMarkerFailure)>,
) {
    let mut local_files = Vec::new();
    let mut remote_urls = Vec::new();
    let mut failures = Vec::new();

    for attachment in attachments {
        match validate_marker_target(&attachment.target, workspace_dir) {
            Ok(DiscordMarkerTarget::Local(path)) => local_files.push(path),
            Ok(DiscordMarkerTarget::Http(url)) => remote_urls.push(url),
            Err(e) => {
                let kind_label = match e.kind() {
                    DiscordMarkerFailure::Refused => "trust boundary",
                    DiscordMarkerFailure::NotFound => "not found",
                };
                tracing::warn!(
                    kind = attachment.kind.marker_name(),
                    target = %attachment.target,
                    reason = kind_label,
                    error = %e,
                    "discord: dropping unresolved outbound attachment marker"
                );
                failures.push((attachment.target.clone(), e.kind()));
            }
        }
    }

    (local_files, remote_urls, failures)
}

/// Build the Matrix-style "(note: I couldn't deliver ...)" tail appended
/// to the bot's reply when at least one marker was dropped. Returns
/// `None` when the failure list is empty so callers can keep the body
/// untouched.
fn delivery_failure_note(failures: &[(String, DiscordMarkerFailure)]) -> Option<String> {
    if failures.is_empty() {
        return None;
    }
    let targets: Vec<&str> = failures.iter().map(|(t, _)| t.as_str()).collect();
    Some(if targets.len() == 1 {
        format!("(note: I couldn't deliver the file at {}.)", targets[0])
    } else {
        format!(
            "(note: I couldn't deliver these files: {}.)",
            targets.join(", ")
        )
    })
}

/// Compose the final reply body with the delivery-failure note appended.
/// When the marker-stripped content is empty the note replaces the body;
/// otherwise the note follows the content separated by a blank line.
fn compose_body_with_failure_note(content: &str, note: Option<&str>) -> String {
    match note {
        Some(note) if content.trim().is_empty() => note.to_string(),
        Some(note) => format!("{content}\n\n{note}"),
        None => content.to_string(),
    }
}

/// Emoji reactions applied to the bot's own outgoing message based on which
/// kinds of marker failures occurred. 🚫 signals a trust-boundary refusal,
/// ⚠️ signals a post-validation delivery failure. Both can fire on the
/// same message when a batch mixes refusals and not-found targets.
fn decide_failure_reactions(failures: &[(String, DiscordMarkerFailure)]) -> Vec<&'static str> {
    let mut out = Vec::new();
    if failures
        .iter()
        .any(|(_, k)| matches!(k, DiscordMarkerFailure::Refused))
    {
        out.push("🚫");
    }
    if failures
        .iter()
        .any(|(_, k)| matches!(k, DiscordMarkerFailure::NotFound))
    {
        out.push("⚠️");
    }
    out
}

fn with_inline_attachment_urls(content: &str, remote_urls: &[String]) -> String {
    let mut lines = Vec::new();
    if !content.trim().is_empty() {
        lines.push(content.trim().to_string());
    }
    if !remote_urls.is_empty() {
        lines.extend(remote_urls.iter().cloned());
    }
    lines.join("\n")
}

/// POST a plain-text message and return the new message's ID. Callers
/// that don't need the ID (e.g. non-first chunks) can discard it.
async fn send_discord_message_json(
    client: &reqwest::Client,
    bot_token: &str,
    recipient: &str,
    content: &str,
) -> anyhow::Result<String> {
    let url = format!("https://discord.com/api/v10/channels/{recipient}/messages");
    let body = json!({ "content": content });

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bot {bot_token}"))
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let err = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
        anyhow::bail!("Discord send message failed ({status}): {err}");
    }

    extract_message_id(resp).await
}

/// POST a message with file attachments via multipart, returning the new
/// message's ID. Callers that don't need the ID can discard it.
async fn send_discord_message_with_files(
    client: &reqwest::Client,
    bot_token: &str,
    recipient: &str,
    content: &str,
    files: &[PathBuf],
) -> anyhow::Result<String> {
    let url = format!("https://discord.com/api/v10/channels/{recipient}/messages");

    let mut form = Form::new().text("payload_json", json!({ "content": content }).to_string());

    for (idx, path) in files.iter().enumerate() {
        let bytes = tokio::fs::read(path).await.map_err(|error| {
            anyhow::anyhow!(
                "Discord attachment read failed for '{}': {error}",
                path.display()
            )
        })?;
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("attachment.bin")
            .to_string();
        form = form.part(
            format!("files[{idx}]"),
            Part::bytes(bytes).file_name(filename),
        );
    }

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bot {bot_token}"))
        .multipart(form)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let err = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
        anyhow::bail!("Discord send message with files failed ({status}): {err}");
    }

    extract_message_id(resp).await
}

async fn extract_message_id(resp: reqwest::Response) -> anyhow::Result<String> {
    let body: serde_json::Value = resp.json().await?;
    body.get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("Discord send response missing 'id' field"))
}

/// Edit an existing Discord message via PATCH.
///
/// Returns `Ok(())` on success. On HTTP 429 (rate limited), logs at debug
/// level and returns `Ok(())` since skipping a mid-stream edit is harmless.
async fn edit_discord_message(
    client: &reqwest::Client,
    bot_token: &str,
    channel_id: &str,
    message_id: &str,
    content: &str,
) -> anyhow::Result<()> {
    let url = format!("https://discord.com/api/v10/channels/{channel_id}/messages/{message_id}");
    let body = json!({ "content": content });

    let resp = client
        .patch(&url)
        .header("Authorization", format!("Bot {bot_token}"))
        .json(&body)
        .send()
        .await?;

    if resp.status().as_u16() == 429 {
        tracing::debug!("Discord edit message rate-limited (429), skipping update");
        return Ok(());
    }

    if !resp.status().is_success() {
        let status = resp.status();
        let err = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
        anyhow::bail!("Discord edit message failed ({status}): {err}");
    }

    Ok(())
}

/// Delete a Discord message.
///
/// Returns `Ok(())` on success. On HTTP 429 (rate limited), logs at debug
/// level and returns `Ok(())` since a stale message is cosmetic only.
async fn delete_discord_message(
    client: &reqwest::Client,
    bot_token: &str,
    channel_id: &str,
    message_id: &str,
) -> anyhow::Result<()> {
    let url = format!("https://discord.com/api/v10/channels/{channel_id}/messages/{message_id}");

    let resp = client
        .delete(&url)
        .header("Authorization", format!("Bot {bot_token}"))
        .send()
        .await?;

    if resp.status().as_u16() == 429 {
        tracing::debug!("Discord delete message rate-limited (429), skipping");
        return Ok(());
    }

    if !resp.status().is_success() {
        let status = resp.status();
        let err = resp
            .text()
            .await
            .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
        anyhow::bail!("Discord delete message failed ({status}): {err}");
    }

    Ok(())
}

const BASE64_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Discord's maximum message length for regular messages.
///
/// Discord rejects longer payloads with `50035 Invalid Form Body`.
const DISCORD_MAX_MESSAGE_LENGTH: usize = 2000;
const DISCORD_ACK_REACTIONS: &[&str] = &["⚡️", "🦀", "🙌", "💪", "👌", "👀", "👣"];

/// Split a message into chunks that respect Discord's 2000-character limit.
/// Tries to split at word boundaries when possible.
fn split_message_for_discord(message: &str) -> Vec<String> {
    if message.chars().count() <= DISCORD_MAX_MESSAGE_LENGTH {
        return vec![message.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = message;

    while !remaining.is_empty() {
        // Find the byte offset for the 2000th character boundary.
        // If there are fewer than 2000 chars left, we can emit the tail directly.
        let hard_split = remaining
            .char_indices()
            .nth(DISCORD_MAX_MESSAGE_LENGTH)
            .map_or(remaining.len(), |(idx, _)| idx);

        let chunk_end = if hard_split == remaining.len() {
            hard_split
        } else {
            // Try to find a good break point (newline, then space)
            let search_area = &remaining[..hard_split];

            // Prefer splitting at newline
            if let Some(pos) = search_area.rfind('\n') {
                // Don't split if the newline is too close to the end
                if search_area[..pos].chars().count() >= DISCORD_MAX_MESSAGE_LENGTH / 2 {
                    pos + 1
                } else {
                    // Try space as fallback
                    search_area.rfind(' ').map_or(hard_split, |space| space + 1)
                }
            } else if let Some(pos) = search_area.rfind(' ') {
                pos + 1
            } else {
                // Hard split at the limit
                hard_split
            }
        };

        chunks.push(remaining[..chunk_end].to_string());
        remaining = &remaining[chunk_end..];
    }

    chunks
}

/// Split a message into multiple logical chunks at paragraph boundaries for
/// multi-message delivery. Respects code fences — never splits inside a
/// fenced code block. Falls back to [`split_message_for_discord`] for any
/// segment that exceeds `max_len`.
fn split_message_for_discord_multi(content: &str, max_len: usize) -> Vec<String> {
    if content.is_empty() {
        return vec![];
    }

    // Gather paragraph-level segments, respecting code fences.
    let mut segments: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_fence = false;

    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
        }

        // If we hit a blank line outside a fence, that's a paragraph break.
        if line.is_empty() && !in_fence && !current.is_empty() {
            segments.push(current.trim_end().to_string());
            current.clear();
            continue;
        }

        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);
    }
    if !current.is_empty() {
        segments.push(current.trim_end().to_string());
    }

    // Now coalesce small segments and split oversized ones.
    let mut chunks: Vec<String> = Vec::new();

    for segment in segments {
        if segment.chars().count() > max_len {
            // This segment (possibly a large code fence) exceeds the limit.
            // Fall back to the word-boundary splitter.
            let sub_chunks = split_message_for_discord(&segment);
            chunks.extend(sub_chunks);
        } else {
            chunks.push(segment);
        }
    }

    if chunks.is_empty() {
        vec![content.to_string()]
    } else {
        chunks
    }
}

/// Choose the chunks to deliver for an outbound Discord message.
///
/// `split_message_for_discord_multi` returns an empty vec for empty input
/// (its paragraph splitter has no segments to emit); the non-multi
/// splitter returns `vec![""]`. When MultiMessage stream mode hands
/// `send()` a paragraph that collapses to empty text after marker strip,
/// the chunk loop would iterate zero times and silently skip an attached
/// file upload. Force a single empty chunk in exactly that case so the
/// multipart POST fires.
fn chunks_for_send(
    content: &str,
    stream_mode: zeroclaw_config::schema::StreamMode,
    max_len: usize,
    has_local_files: bool,
) -> Vec<String> {
    let mut chunks = match stream_mode {
        zeroclaw_config::schema::StreamMode::MultiMessage => {
            split_message_for_discord_multi(content, max_len)
        }
        _ => split_message_for_discord(content),
    };
    if chunks.is_empty() && has_local_files {
        chunks.push(String::new());
    }
    chunks
}

fn pick_uniform_index(len: usize) -> usize {
    debug_assert!(len > 0);
    let upper = len as u64;
    let reject_threshold = (u64::MAX / upper) * upper;

    loop {
        let value = rand::random::<u64>();
        if value < reject_threshold {
            #[allow(clippy::cast_possible_truncation)]
            return (value % upper) as usize;
        }
    }
}

fn random_discord_ack_reaction() -> &'static str {
    DISCORD_ACK_REACTIONS[pick_uniform_index(DISCORD_ACK_REACTIONS.len())]
}

/// URL-encode a Unicode emoji for use in Discord reaction API paths.
///
/// Discord's reaction endpoints accept raw Unicode emoji in the URL path,
/// but they must be percent-encoded per RFC 3986. Custom guild emojis use
/// the `name:id` format and are passed through unencoded.
fn encode_emoji_for_discord(emoji: &str) -> String {
    if emoji.contains(':') {
        return emoji.to_string();
    }

    let mut encoded = String::new();
    for byte in emoji.as_bytes() {
        let _ = write!(encoded, "%{byte:02X}");
    }
    encoded
}

fn discord_reaction_url(channel_id: &str, message_id: &str, emoji: &str) -> String {
    let raw_id = message_id.strip_prefix("discord_").unwrap_or(message_id);
    let encoded_emoji = encode_emoji_for_discord(emoji);
    format!(
        "https://discord.com/api/v10/channels/{channel_id}/messages/{raw_id}/reactions/{encoded_emoji}/@me"
    )
}

fn mention_tags(bot_user_id: &str) -> [String; 2] {
    [format!("<@{bot_user_id}>"), format!("<@!{bot_user_id}>")]
}

fn contains_bot_mention(content: &str, bot_user_id: &str) -> bool {
    let tags = mention_tags(bot_user_id);
    content.contains(&tags[0]) || content.contains(&tags[1])
}

/// Decide whether an inbound Discord message passes the listener gate.
/// Returns the cleaned text body when admitted, or `None` to drop the
/// message. Attachment-only messages (empty `content` plus at least one
/// attachment) are admitted as long as the mention requirement is
/// satisfied; otherwise a Discord message that contained only an image,
/// PDF, ZIP, video, or audio with no caption would never reach the
/// media pipeline.
fn admit_discord_message(
    content: &str,
    has_attachments: bool,
    mention_only: bool,
    bot_user_id: &str,
) -> Option<String> {
    if mention_only && !contains_bot_mention(content, bot_user_id) {
        return None;
    }

    let mut normalized = content.to_string();
    if mention_only {
        for tag in mention_tags(bot_user_id) {
            normalized = normalized.replace(&tag, " ");
        }
    }
    let normalized = normalized.trim().to_string();

    if normalized.is_empty() && !has_attachments {
        return None;
    }

    Some(normalized)
}

/// Minimal base64 decode (no extra dep) — only needs to decode the user ID portion
#[allow(clippy::cast_possible_truncation)]
fn base64_decode(input: &str) -> Option<String> {
    let padded = match input.len() % 4 {
        2 => format!("{input}=="),
        3 => format!("{input}="),
        _ => input.to_string(),
    };

    let mut bytes = Vec::new();
    let chars: Vec<u8> = padded.bytes().collect();

    for chunk in chars.chunks(4) {
        if chunk.len() < 4 {
            break;
        }

        let mut v = [0usize; 4];
        for (i, &b) in chunk.iter().enumerate() {
            if b == b'=' {
                v[i] = 0;
            } else {
                v[i] = BASE64_ALPHABET.iter().position(|&a| a == b)?;
            }
        }

        bytes.push(((v[0] << 2) | (v[1] >> 4)) as u8);
        if chunk[2] != b'=' {
            bytes.push((((v[1] & 0xF) << 4) | (v[2] >> 2)) as u8);
        }
        if chunk[3] != b'=' {
            bytes.push((((v[2] & 0x3) << 6) | v[3]) as u8);
        }
    }

    String::from_utf8(bytes).ok()
}

#[async_trait]
impl Channel for DiscordChannel {
    fn name(&self) -> &str {
        "discord"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let raw_content = crate::util::strip_tool_call_tags(&message.content);
        let (cleaned_content, parsed_attachments) = parse_attachment_markers(&raw_content);
        let (mut local_files, remote_urls, failures) =
            classify_outgoing_attachments(&parsed_attachments, self.workspace_dir.as_deref());

        // Discord accepts max 10 files per message.
        if local_files.len() > 10 {
            tracing::warn!(
                count = local_files.len(),
                "discord: truncating local attachment upload list to 10 files"
            );
            local_files.truncate(10);
        }

        let body = with_inline_attachment_urls(&cleaned_content, &remote_urls);
        let note = delivery_failure_note(&failures);
        let content = compose_body_with_failure_note(&body, note.as_deref());
        let reactions = decide_failure_reactions(&failures);

        let client = self.http_client();
        let chunks = chunks_for_send(
            &content,
            self.stream_mode,
            DISCORD_MAX_MESSAGE_LENGTH,
            !local_files.is_empty(),
        );
        let inter_chunk_delay_ms =
            if self.stream_mode == zeroclaw_config::schema::StreamMode::MultiMessage {
                self.multi_message_delay_ms
            } else {
                500
            };

        let mut first_message_id: Option<String> = None;
        for (i, chunk) in chunks.iter().enumerate() {
            let message_id = if i == 0 && !local_files.is_empty() {
                send_discord_message_with_files(
                    &client,
                    &self.bot_token,
                    &message.recipient,
                    chunk,
                    &local_files,
                )
                .await?
            } else {
                send_discord_message_json(&client, &self.bot_token, &message.recipient, chunk)
                    .await?
            };
            if first_message_id.is_none() {
                first_message_id = Some(message_id);
            }

            if i < chunks.len() - 1 {
                if message
                    .cancellation_token
                    .as_ref()
                    .is_some_and(|t| t.is_cancelled())
                {
                    tracing::debug!(
                        "Discord delivery interrupted after chunk {}/{}",
                        i + 1,
                        chunks.len()
                    );
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(inter_chunk_delay_ms)).await;
            }
        }

        self.apply_failure_reactions(&message.recipient, first_message_id.as_deref(), &reactions)
            .await;

        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        let bot_user_id = Self::bot_user_id_from_token(&self.bot_token).unwrap_or_default();

        // Get Gateway URL
        let gw_resp: serde_json::Value = self
            .http_client()
            .get("https://discord.com/api/v10/gateway/bot")
            .header("Authorization", format!("Bot {}", self.bot_token))
            .send()
            .await?
            .json()
            .await?;

        let gw_url = gw_resp
            .get("url")
            .and_then(|u| u.as_str())
            .unwrap_or("wss://gateway.discord.gg");

        let ws_url = format!("{gw_url}/?v=10&encoding=json");
        tracing::info!("Discord: connecting to gateway...");

        let (ws_stream, _) = zeroclaw_config::schema::ws_connect_with_proxy(
            &ws_url,
            "channel.discord",
            self.proxy_url.as_deref(),
        )
        .await?;
        let (mut write, mut read) = ws_stream.split();

        // Read Hello (opcode 10)
        let hello = read.next().await.ok_or(anyhow::anyhow!("No hello"))??;
        let hello_data: serde_json::Value = serde_json::from_str(&hello.to_string())?;
        let heartbeat_interval = hello_data
            .get("d")
            .and_then(|d| d.get("heartbeat_interval"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(41250);

        // Send Identify (opcode 2)
        let identify = json!({
            "op": 2,
            "d": {
                "token": self.bot_token,
                "intents": 37377, // GUILDS | GUILD_MESSAGES | MESSAGE_CONTENT | DIRECT_MESSAGES
                "properties": {
                    "os": "linux",
                    "browser": "zeroclaw",
                    "device": "zeroclaw"
                }
            }
        });
        write
            .send(Message::Text(identify.to_string().into()))
            .await?;

        tracing::info!("Discord: connected and identified");

        // Track the last sequence number for heartbeats and resume.
        // Only accessed in the select! loop below, so a plain i64 suffices.
        let mut sequence: i64 = -1;

        // Spawn heartbeat timer — sends a tick signal, actual heartbeat
        // is assembled in the select! loop where `sequence` lives.
        let (hb_tx, mut hb_rx) = tokio::sync::mpsc::channel::<()>(1);
        let hb_interval = heartbeat_interval;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(hb_interval));
            loop {
                interval.tick().await;
                if hb_tx.send(()).await.is_err() {
                    break;
                }
            }
        });

        let guild_filter = self.guild_id.clone();

        // --- Stall watchdog --------------------------------------------------
        let watchdog = if self.stall_timeout_secs > 0 {
            Some(zeroclaw_infra::stall_watchdog::StallWatchdog::new(
                self.stall_timeout_secs,
            ))
        } else {
            None
        };

        let (stall_tx, mut stall_rx) = tokio::sync::mpsc::channel::<()>(1);
        if let Some(ref wd) = watchdog {
            let stall_signal = stall_tx.clone();
            wd.start(move || {
                tracing::warn!("Discord: stall watchdog fired — no events for configured timeout, triggering reconnect");
                let _ = stall_signal.try_send(());
            })
            .await;
        }
        // Keep stall_tx alive so the receiver doesn't close prematurely when
        // the watchdog is disabled (recv will just pend forever).
        let _stall_tx_guard = stall_tx;

        loop {
            tokio::select! {
                _ = stall_rx.recv() => {
                    tracing::info!("Discord: breaking listen loop due to stall watchdog");
                    break;
                }
                _ = hb_rx.recv() => {
                    let d = if sequence >= 0 { json!(sequence) } else { json!(null) };
                    let hb = json!({"op": 1, "d": d});
                    if write.send(Message::Text(hb.to_string().into())).await.is_err() {
                        break;
                    }
                }
                msg = read.next() => {
                    let msg = match msg {
                        Some(Ok(Message::Text(t))) => t,
                        Some(Ok(Message::Ping(payload))) => {
                            if write.send(Message::Pong(payload)).await.is_err() {
                                tracing::warn!("Discord: pong send failed, reconnecting");
                                break;
                            }
                            continue;
                        }
                        Some(Ok(Message::Close(_))) | None => break,
                        Some(Err(e)) => {
                            tracing::warn!("Discord: websocket read error: {e}, reconnecting");
                            break;
                        }
                        _ => continue,
                    };

                    let event: serde_json::Value = match serde_json::from_str(msg.as_ref()) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };

                    // Mark activity for the stall watchdog on every
                    // successfully parsed gateway event.
                    if let Some(ref wd) = watchdog {
                        wd.touch();
                    }

                    // Track sequence number from all dispatch events
                    if let Some(s) = event.get("s").and_then(serde_json::Value::as_i64) {
                        sequence = s;
                    }

                    let op = event.get("op").and_then(serde_json::Value::as_u64).unwrap_or(0);

                    match op {
                        // Op 1: Server requests an immediate heartbeat
                        1 => {
                            let d = if sequence >= 0 { json!(sequence) } else { json!(null) };
                            let hb = json!({"op": 1, "d": d});
                            if write.send(Message::Text(hb.to_string().into())).await.is_err() {
                                break;
                            }
                            continue;
                        }
                        // Op 7: Reconnect
                        7 => {
                            tracing::warn!("Discord: received Reconnect (op 7), closing for restart");
                            break;
                        }
                        // Op 9: Invalid Session
                        9 => {
                            tracing::warn!("Discord: received Invalid Session (op 9), closing for restart");
                            break;
                        }
                        _ => {}
                    }

                    // Only handle MESSAGE_CREATE (opcode 0, type "MESSAGE_CREATE")
                    let event_type = event.get("t").and_then(|t| t.as_str()).unwrap_or("");
                    if event_type != "MESSAGE_CREATE" {
                        continue;
                    }

                    let Some(d) = event.get("d") else {
                        continue;
                    };

                    // Skip messages from the bot itself
                    let author_id = d.get("author").and_then(|a| a.get("id")).and_then(|i| i.as_str()).unwrap_or("");
                    if author_id == bot_user_id {
                        continue;
                    }

                    // Skip bot messages (unless listen_to_bots is enabled)
                    if !self.listen_to_bots && d.get("author").and_then(|a| a.get("bot")).and_then(serde_json::Value::as_bool).unwrap_or(false) {
                        continue;
                    }

                    // Sender validation
                    if !self.is_user_allowed(author_id) {
                        tracing::warn!("Discord: ignoring message from unauthorized user: {author_id}");
                        continue;
                    }

                    // Guild filter
                    if let Some(ref gid) = guild_filter {
                        let msg_guild = d.get("guild_id").and_then(serde_json::Value::as_str);
                        // DMs have no guild_id — let them through; for guild messages, enforce the filter
                        if let Some(g) = msg_guild
                            && g != gid {
                                continue;
                            }
                    }

                    let content = d.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    // DMs carry no guild_id in the Discord gateway payload. They are
                    // inherently private and implicitly addressed to the bot, so bypass
                    // the mention gate — requiring a @mention in a DM is never correct.
                    let is_dm = d.get("guild_id").is_none();
                    let effective_mention_only = self.mention_only && !is_dm;
                    let atts = d
                        .get("attachments")
                        .and_then(|a| a.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let has_attachments = !atts.is_empty();
                    let Some(clean_content) = admit_discord_message(
                        content,
                        has_attachments,
                        effective_mention_only,
                        &bot_user_id,
                    ) else {
                        continue;
                    };

                    let client = self.http_client();
                    let (attachment_text, media_attachments) = process_attachments(
                        &atts,
                        &client,
                        self.workspace_dir.as_deref(),
                        self.transcription_manager.as_deref(),
                    )
                    .await;
                    let final_content = if attachment_text.is_empty() {
                        clean_content
                    } else {
                        format!("{clean_content}\n\n[Attachments]\n{attachment_text}")
                    };

                    // Intercept approval replies before forwarding to the agent.
                    if let Some((token, response)) =
                        crate::util::parse_approval_reply(&final_content)
                    {
                        let mut map = self.pending_approvals.lock().await;
                        if let Some(sender) = map.remove(&token) {
                            let _ = sender.send(response);
                            continue;
                        }
                    }

                    let message_id = d.get("id").and_then(|i| i.as_str()).unwrap_or("");
                    let channel_id = d
                        .get("channel_id")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string();

                    if !message_id.is_empty() && !channel_id.is_empty() {
                        let reaction_channel = DiscordChannel::new(
                            self.bot_token.clone(),
                            self.guild_id.clone(),
                            self.allowed_users.clone(),
                            self.listen_to_bots,
                            self.mention_only,
                        );
                        let reaction_channel_id = channel_id.clone();
                        let reaction_message_id = message_id.to_string();
                        let reaction_emoji = random_discord_ack_reaction().to_string();
                        tokio::spawn(async move {
                            if let Err(err) = reaction_channel
                                .add_reaction(
                                    &reaction_channel_id,
                                    &reaction_message_id,
                                    &reaction_emoji,
                                )
                                .await
                            {
                                tracing::debug!(
                                    "Discord: failed to add ACK reaction for message {reaction_message_id}: {err}"
                                );
                            }
                        });
                    }

                    // Thread context decides `thread_ts` plus `interruption_scope_id`,
                    // which the orchestrator uses as part of the conversation-history
                    // key and the cancellation scope. When the lookup fails it falls
                    // back to `None` and the failure is not cached, so the next
                    // message in the same Discord thread will retry. The trade-off:
                    // the first message after a transient lookup miss is keyed
                    // without the thread suffix; once the cache warms, subsequent
                    // messages are keyed with it. History for that thread can split
                    // across two scopes until the warm-up completes. Acceptable
                    // because the lookup is bounded by `THREAD_LOOKUP_TIMEOUT` and
                    // the alternative (stalling the listener on a hung Discord call)
                    // is worse.
                    let thread_ts = if channel_id.is_empty() {
                        None
                    } else if self.is_thread_channel(&client, &channel_id).await {
                        Some(channel_id.clone())
                    } else {
                        None
                    };

                    let channel_msg = ChannelMessage {
                        id: if message_id.is_empty() {
                            Uuid::new_v4().to_string()
                        } else {
                            format!("discord_{message_id}")
                        },
                        sender: author_id.to_string(),
                        reply_target: if channel_id.is_empty() {
                            author_id.to_string()
                        } else {
                            channel_id.clone()
                        },
                        content: final_content,
                        channel: "discord".to_string(),
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                        interruption_scope_id: thread_ts.clone(),
                        thread_ts,
                        attachments: media_attachments,
                    };

                    if tx.send(channel_msg).await.is_err() {
                        break;
                    }
                }
            }
        }

        // Clean up the watchdog task before returning so the outer
        // reconnection loop can start fresh.
        if let Some(ref wd) = watchdog {
            wd.stop().await;
        }

        Ok(())
    }

    async fn health_check(&self) -> bool {
        self.http_client()
            .get("https://discord.com/api/v10/users/@me")
            .header("Authorization", format!("Bot {}", self.bot_token))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    async fn start_typing(&self, recipient: &str) -> anyhow::Result<()> {
        self.stop_typing(recipient).await?;

        let client = self.http_client();
        let token = self.bot_token.clone();
        let channel_id = recipient.to_string();

        let handle = tokio::spawn(async move {
            let url = format!("https://discord.com/api/v10/channels/{channel_id}/typing");
            loop {
                let _ = client
                    .post(&url)
                    .header("Authorization", format!("Bot {token}"))
                    .send()
                    .await;
                tokio::time::sleep(std::time::Duration::from_secs(8)).await;
            }
        });

        let mut guard = self.typing_handles.lock();
        guard.insert(recipient.to_string(), handle);

        Ok(())
    }

    async fn stop_typing(&self, recipient: &str) -> anyhow::Result<()> {
        let mut guard = self.typing_handles.lock();
        if let Some(handle) = guard.remove(recipient) {
            handle.abort();
        }
        Ok(())
    }

    fn supports_draft_updates(&self) -> bool {
        self.stream_mode != zeroclaw_config::schema::StreamMode::Off
    }

    fn supports_multi_message_streaming(&self) -> bool {
        self.stream_mode == zeroclaw_config::schema::StreamMode::MultiMessage
    }

    fn multi_message_delay_ms(&self) -> u64 {
        self.multi_message_delay_ms
    }

    async fn send_draft(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        use zeroclaw_config::schema::StreamMode;
        match self.stream_mode {
            StreamMode::Off => Ok(None),
            StreamMode::Partial => {
                let initial_text = if message.content.is_empty() {
                    "...".to_string()
                } else {
                    message.content.clone()
                };

                let client = self.http_client();
                let msg_id = send_discord_message_json(
                    &client,
                    &self.bot_token,
                    &message.recipient,
                    &initial_text,
                )
                .await?;

                self.last_draft_edit
                    .lock()
                    .insert(message.recipient.clone(), std::time::Instant::now());

                Ok(Some(msg_id))
            }
            StreamMode::MultiMessage => {
                // No initial draft — paragraphs are sent as new messages.
                // Store thread context for paragraph delivery.
                self.multi_message_sent_len.lock().clear();
                self.multi_message_thread_ts
                    .lock()
                    .insert(message.recipient.clone(), message.thread_ts.clone());
                Ok(Some("multi_message_synthetic".to_string()))
            }
        }
    }

    async fn update_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        use zeroclaw_config::schema::StreamMode;
        match self.stream_mode {
            StreamMode::Off => Ok(()),
            StreamMode::Partial => {
                // Rate-limit edits per channel.
                {
                    let last_edits = self.last_draft_edit.lock();
                    if let Some(last_time) = last_edits.get(recipient) {
                        let elapsed_ms =
                            u64::try_from(last_time.elapsed().as_millis()).unwrap_or(u64::MAX);
                        if elapsed_ms < self.draft_update_interval_ms {
                            return Ok(());
                        }
                    }
                }

                // UTF-8 safe truncation to Discord limit.
                let display_text = if text.len() > DISCORD_MAX_MESSAGE_LENGTH {
                    let mut end = 0;
                    for (idx, ch) in text.char_indices() {
                        let next = idx + ch.len_utf8();
                        if next > DISCORD_MAX_MESSAGE_LENGTH {
                            break;
                        }
                        end = next;
                    }
                    &text[..end]
                } else {
                    text
                };

                let client = self.http_client();
                match edit_discord_message(
                    &client,
                    &self.bot_token,
                    recipient,
                    message_id,
                    display_text,
                )
                .await
                {
                    Ok(()) => {
                        self.last_draft_edit
                            .lock()
                            .insert(recipient.to_string(), std::time::Instant::now());
                    }
                    Err(e) => {
                        tracing::debug!("Discord draft update failed: {e}");
                    }
                }

                Ok(())
            }
            StreamMode::MultiMessage => {
                // Track accumulated text and send new paragraphs at \n\n boundaries.
                // Extract paragraph (if any) under the lock, then drop it before async work.
                let (paragraph, thread_ts) = {
                    let thread_ts = self
                        .multi_message_thread_ts
                        .lock()
                        .get(recipient)
                        .cloned()
                        .flatten();
                    let mut sent_map = self.multi_message_sent_len.lock();
                    let sent_so_far = sent_map.get(recipient).copied().unwrap_or(0);

                    // DraftEvent::Clear resets accumulated text — reset our counter.
                    if text.len() < sent_so_far {
                        sent_map.insert(recipient.to_string(), 0);
                        return Ok(());
                    }
                    if text.len() == sent_so_far {
                        return Ok(());
                    }

                    let new_text = &text[sent_so_far..];
                    let mut scan_pos = 0;
                    let mut in_fence = false;
                    let bytes = new_text.as_bytes();
                    let mut found_paragraph = None;

                    while scan_pos < bytes.len() {
                        let ch = bytes[scan_pos];

                        if ch == b'`'
                            && scan_pos + 2 < bytes.len()
                            && bytes[scan_pos + 1] == b'`'
                            && bytes[scan_pos + 2] == b'`'
                            && (scan_pos == 0 || bytes[scan_pos - 1] == b'\n')
                        {
                            in_fence = !in_fence;
                        }

                        if !in_fence
                            && ch == b'\n'
                            && scan_pos + 1 < bytes.len()
                            && bytes[scan_pos + 1] == b'\n'
                        {
                            let paragraph = new_text[..scan_pos].trim().to_string();
                            let consumed = scan_pos + 2;
                            *sent_map.entry(recipient.to_string()).or_insert(0) += consumed;
                            if !paragraph.is_empty() {
                                found_paragraph = Some(paragraph);
                            }
                            break;
                        }

                        scan_pos += 1;
                    }
                    // Lock is dropped here at end of block.
                    (found_paragraph, thread_ts)
                };

                if let Some(paragraph) = paragraph {
                    let msg = SendMessage::new(&paragraph, recipient).in_thread(thread_ts.clone());
                    if let Err(e) = self.send(&msg).await {
                        tracing::debug!("Discord multi-message paragraph send failed: {e}");
                    }
                    if self.multi_message_delay_ms > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            self.multi_message_delay_ms,
                        ))
                        .await;
                    }
                    // Recurse to handle remaining text.
                    return self.update_draft(recipient, message_id, text).await;
                }

                Ok(())
            }
        }
    }

    async fn finalize_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        if self.stream_mode == zeroclaw_config::schema::StreamMode::MultiMessage {
            // Flush remaining buffered text.
            let thread_ts = self
                .multi_message_thread_ts
                .lock()
                .remove(recipient)
                .flatten();
            let sent_so_far = self
                .multi_message_sent_len
                .lock()
                .remove(recipient)
                .unwrap_or(0);
            if text.len() > sent_so_far {
                let remaining = text[sent_so_far..].trim().to_string();
                if !remaining.is_empty() {
                    let msg = SendMessage::new(&remaining, recipient).in_thread(thread_ts);
                    if let Err(e) = self.send(&msg).await {
                        tracing::debug!("Discord multi-message final flush failed: {e}");
                    }
                }
            }
            return Ok(());
        }

        // Belt-and-suspenders: kill any typing handles for this channel.
        let _ = self.stop_typing(recipient).await;
        self.last_draft_edit.lock().remove(recipient);

        let text = &crate::util::strip_tool_call_tags(text);
        let (cleaned_content, parsed_attachments) = parse_attachment_markers(text);
        let (mut local_files, remote_urls, failures) =
            classify_outgoing_attachments(&parsed_attachments, self.workspace_dir.as_deref());
        let body = with_inline_attachment_urls(&cleaned_content, &remote_urls);
        let note = delivery_failure_note(&failures);
        let content = compose_body_with_failure_note(&body, note.as_deref());
        let reactions = decide_failure_reactions(&failures);

        let client = self.http_client();

        // Path 1: file attachments — delete draft and POST fresh message with files.
        if !local_files.is_empty() {
            let _ = delete_discord_message(&client, &self.bot_token, recipient, message_id).await;

            if local_files.len() > 10 {
                local_files.truncate(10);
            }
            let chunks = split_message_for_discord(&content);
            let mut first_message_id: Option<String> = None;
            for (i, chunk) in chunks.iter().enumerate() {
                let new_id = if i == 0 {
                    send_discord_message_with_files(
                        &client,
                        &self.bot_token,
                        recipient,
                        chunk,
                        &local_files,
                    )
                    .await?
                } else {
                    send_discord_message_json(&client, &self.bot_token, recipient, chunk).await?
                };
                if first_message_id.is_none() {
                    first_message_id = Some(new_id);
                }
                if i < chunks.len() - 1 {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
            self.apply_failure_reactions(recipient, first_message_id.as_deref(), &reactions)
                .await;
            return Ok(());
        }

        // Path 2: text exceeds limit — delete draft and POST as chunked messages.
        if content.chars().count() > DISCORD_MAX_MESSAGE_LENGTH {
            let _ = delete_discord_message(&client, &self.bot_token, recipient, message_id).await;

            let chunks = split_message_for_discord(&content);
            let mut first_message_id: Option<String> = None;
            for (i, chunk) in chunks.iter().enumerate() {
                let new_id =
                    send_discord_message_json(&client, &self.bot_token, recipient, chunk).await?;
                if first_message_id.is_none() {
                    first_message_id = Some(new_id);
                }
                if i < chunks.len() - 1 {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
            self.apply_failure_reactions(recipient, first_message_id.as_deref(), &reactions)
                .await;
            return Ok(());
        }

        // Path 3: simple case — edit in-place; fall back to delete + POST on failure.
        // The reaction target is the draft message_id when the edit lands;
        // when the fallback fires it's the freshly posted message instead.
        let reaction_target =
            match edit_discord_message(&client, &self.bot_token, recipient, message_id, &content)
                .await
            {
                Ok(()) => message_id.to_string(),
                Err(e) => {
                    tracing::warn!(
                        "Discord finalize_draft edit failed: {e}; falling back to delete+send"
                    );
                    let _ = delete_discord_message(&client, &self.bot_token, recipient, message_id)
                        .await;
                    send_discord_message_json(&client, &self.bot_token, recipient, &content).await?
                }
            };
        self.apply_failure_reactions(recipient, Some(&reaction_target), &reactions)
            .await;

        Ok(())
    }

    async fn cancel_draft(&self, recipient: &str, message_id: &str) -> anyhow::Result<()> {
        if self.stream_mode == zeroclaw_config::schema::StreamMode::MultiMessage {
            self.multi_message_sent_len.lock().remove(recipient);
            self.multi_message_thread_ts.lock().remove(recipient);
            return Ok(());
        }

        let _ = self.stop_typing(recipient).await;
        self.last_draft_edit.lock().remove(recipient);

        let client = self.http_client();
        if let Err(e) =
            delete_discord_message(&client, &self.bot_token, recipient, message_id).await
        {
            tracing::debug!("Discord cancel_draft delete failed: {e}");
        }

        Ok(())
    }

    async fn add_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        let url = discord_reaction_url(channel_id, message_id, emoji);

        let resp = self
            .http_client()
            .put(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .header("Content-Length", "0")
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
            anyhow::bail!("Discord add reaction failed ({status}): {err}");
        }

        Ok(())
    }

    async fn remove_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        let url = discord_reaction_url(channel_id, message_id, emoji);

        let resp = self
            .http_client()
            .delete(&url)
            .header("Authorization", format!("Bot {}", self.bot_token))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response body: {e}>"));
            anyhow::bail!("Discord remove reaction failed ({status}): {err}");
        }

        Ok(())
    }

    async fn request_approval(
        &self,
        recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
        let token = crate::util::new_approval_token();
        let text = format!(
            "APPROVAL REQUIRED [{}]\nTool: {}\nArgs: {}\n\nReply: \"{} yes\", \"{} no\", or \"{} always\"",
            token, request.tool_name, request.arguments_summary, token, token, token,
        );

        let (tx, rx) = oneshot::channel();
        self.pending_approvals
            .lock()
            .await
            .insert(token.clone(), tx);

        // Strip thread suffix — approval message goes to the channel root.
        let channel_id = recipient.split(':').next().unwrap_or(recipient);
        if let Err(err) = self.send(&SendMessage::new(text, channel_id)).await {
            self.pending_approvals.lock().await.remove(&token);
            return Err(err);
        }

        let response =
            match tokio::time::timeout(Duration::from_secs(self.approval_timeout_secs), rx).await {
                Ok(Ok(resp)) => resp,
                _ => {
                    self.pending_approvals.lock().await.remove(&token);
                    ChannelApprovalResponse::Deny
                }
            };
        Ok(Some(response))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discord_channel_name() {
        let ch = DiscordChannel::new("fake".into(), None, vec![], false, false);
        assert_eq!(ch.name(), "discord");
    }

    #[test]
    fn base64_decode_bot_id() {
        // "MTIzNDU2" decodes to "123456"
        let decoded = base64_decode("MTIzNDU2");
        assert_eq!(decoded, Some("123456".to_string()));
    }

    #[test]
    fn bot_user_id_extraction() {
        // Token format: base64(user_id).timestamp.hmac
        let token = "MTIzNDU2.fake.hmac";
        let id = DiscordChannel::bot_user_id_from_token(token);
        assert_eq!(id, Some("123456".to_string()));
    }

    #[test]
    fn empty_allowlist_denies_everyone() {
        let ch = DiscordChannel::new("fake".into(), None, vec![], false, false);
        assert!(!ch.is_user_allowed("12345"));
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[test]
    fn wildcard_allows_everyone() {
        let ch = DiscordChannel::new("fake".into(), None, vec!["*".into()], false, false);
        assert!(ch.is_user_allowed("12345"));
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn specific_allowlist_filters() {
        let ch = DiscordChannel::new(
            "fake".into(),
            None,
            vec!["111".into(), "222".into()],
            false,
            false,
        );
        assert!(ch.is_user_allowed("111"));
        assert!(ch.is_user_allowed("222"));
        assert!(!ch.is_user_allowed("333"));
        assert!(!ch.is_user_allowed("unknown"));
    }

    #[test]
    fn allowlist_is_exact_match_not_substring() {
        let ch = DiscordChannel::new("fake".into(), None, vec!["111".into()], false, false);
        assert!(!ch.is_user_allowed("1111"));
        assert!(!ch.is_user_allowed("11"));
        assert!(!ch.is_user_allowed("0111"));
    }

    #[test]
    fn allowlist_empty_string_user_id() {
        let ch = DiscordChannel::new("fake".into(), None, vec!["111".into()], false, false);
        assert!(!ch.is_user_allowed(""));
    }

    #[test]
    fn allowlist_with_wildcard_and_specific() {
        let ch = DiscordChannel::new(
            "fake".into(),
            None,
            vec!["111".into(), "*".into()],
            false,
            false,
        );
        assert!(ch.is_user_allowed("111"));
        assert!(ch.is_user_allowed("anyone_else"));
    }

    #[test]
    fn allowlist_case_sensitive() {
        let ch = DiscordChannel::new("fake".into(), None, vec!["ABC".into()], false, false);
        assert!(ch.is_user_allowed("ABC"));
        assert!(!ch.is_user_allowed("abc"));
        assert!(!ch.is_user_allowed("Abc"));
    }

    #[test]
    fn base64_decode_empty_string() {
        let decoded = base64_decode("");
        assert_eq!(decoded, Some(String::new()));
    }

    #[test]
    fn base64_decode_invalid_chars() {
        let decoded = base64_decode("!!!!");
        assert!(decoded.is_none());
    }

    #[test]
    fn bot_user_id_from_empty_token() {
        let id = DiscordChannel::bot_user_id_from_token("");
        assert_eq!(id, Some(String::new()));
    }

    #[test]
    fn contains_bot_mention_supports_plain_and_nick_forms() {
        assert!(contains_bot_mention("hi <@12345>", "12345"));
        assert!(contains_bot_mention("hi <@!12345>", "12345"));
        assert!(!contains_bot_mention("hi <@99999>", "12345"));
    }

    #[test]
    fn admit_discord_message_requires_mention_when_enabled() {
        let cleaned = admit_discord_message("hello there", false, true, "12345");
        assert!(cleaned.is_none());
    }

    #[test]
    fn admit_discord_message_strips_mentions_and_trims() {
        let cleaned = admit_discord_message("  <@!12345> run status  ", false, true, "12345");
        assert_eq!(cleaned.as_deref(), Some("run status"));
    }

    #[test]
    fn admit_discord_message_rejects_empty_text_and_no_attachments() {
        let cleaned = admit_discord_message("<@12345>", false, true, "12345");
        assert!(cleaned.is_none());
    }

    #[test]
    fn admit_discord_message_attachment_only_in_dm_is_admitted() {
        // DM (effective_mention_only=false), empty text body, at least one
        // attachment. Previously dropped at the empty-text gate; now passes
        // through so process_attachments can run on the media.
        let cleaned = admit_discord_message("", true, false, "12345");
        assert_eq!(cleaned.as_deref(), Some(""));
    }

    #[test]
    fn admit_discord_message_attachment_only_with_mention_in_guild_is_admitted() {
        // Guild channel with mention_only=true. Caption is just the @mention
        // tag with no other text, but the message has a media attachment.
        // Mention requirement is satisfied; cleaned text is empty but the
        // attachment alone is enough input.
        let cleaned = admit_discord_message("<@12345>", true, true, "12345");
        assert_eq!(cleaned.as_deref(), Some(""));
    }

    #[test]
    fn admit_discord_message_attachment_only_without_mention_in_guild_is_rejected() {
        // Guild channel with mention_only=true, attachment but no mention
        // anywhere in the caption. The mention gate is orthogonal to
        // attachment presence: no mention signal means drop.
        let cleaned = admit_discord_message("", true, true, "12345");
        assert!(cleaned.is_none());
    }

    #[test]
    fn admit_discord_message_drops_when_no_text_and_no_attachments() {
        // Completely empty payload with attachments absent is always dropped,
        // regardless of mention_only setting.
        assert!(admit_discord_message("", false, false, "12345").is_none());
        assert!(admit_discord_message("", false, true, "12345").is_none());
    }

    // mention_only DM-bypass tests

    #[test]
    fn mention_only_dm_bypasses_mention_gate() {
        // DMs (no guild_id) must pass through even when mention_only is true
        // and the message contains no @mention. Mirrors the listen call-site logic.
        let mention_only = true;
        let is_dm = true;
        let effective = mention_only && !is_dm;
        let cleaned = admit_discord_message("hello without mention", false, effective, "12345");
        assert_eq!(cleaned.as_deref(), Some("hello without mention"));
    }

    #[test]
    fn mention_only_guild_message_without_mention_is_rejected() {
        // Guild messages (has guild_id, so is_dm = false) must still be rejected
        // when mention_only is true and the message contains no @mention.
        let mention_only = true;
        let is_dm = false;
        let effective = mention_only && !is_dm;
        let cleaned = admit_discord_message("hello without mention", false, effective, "12345");
        assert!(cleaned.is_none());
    }

    #[test]
    fn mention_only_guild_message_with_mention_passes_and_strips() {
        // Guild messages that do carry a @mention pass through and have the
        // mention tag stripped, consistent with pre-existing behaviour.
        let mention_only = true;
        let is_dm = false;
        let effective = mention_only && !is_dm;
        let cleaned = admit_discord_message("<@12345> run status", false, effective, "12345");
        assert_eq!(cleaned.as_deref(), Some("run status"));
    }

    // Message splitting tests

    #[test]
    fn split_empty_message() {
        let chunks = split_message_for_discord("");
        assert_eq!(chunks, vec![""]);
    }

    #[test]
    fn split_short_message_under_limit() {
        let msg = "Hello, world!";
        let chunks = split_message_for_discord(msg);
        assert_eq!(chunks, vec![msg]);
    }

    #[test]
    fn split_message_exactly_2000_chars() {
        let msg = "a".repeat(DISCORD_MAX_MESSAGE_LENGTH);
        let chunks = split_message_for_discord(&msg);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chars().count(), DISCORD_MAX_MESSAGE_LENGTH);
    }

    #[test]
    fn split_message_just_over_limit() {
        let msg = "a".repeat(DISCORD_MAX_MESSAGE_LENGTH + 1);
        let chunks = split_message_for_discord(&msg);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chars().count(), DISCORD_MAX_MESSAGE_LENGTH);
        assert_eq!(chunks[1].chars().count(), 1);
    }

    #[test]
    fn split_very_long_message() {
        let msg = "word ".repeat(2000); // 10000 characters (5 chars per "word ")
        let chunks = split_message_for_discord(&msg);
        // Should split into 5 chunks of <= 2000 chars
        assert_eq!(chunks.len(), 5);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.chars().count() <= DISCORD_MAX_MESSAGE_LENGTH)
        );
        // Verify total content is preserved
        let reconstructed = chunks.concat();
        assert_eq!(reconstructed, msg);
    }

    #[test]
    fn split_prefer_newline_break() {
        let msg = format!("{}\n{}", "a".repeat(1500), "b".repeat(500));
        let chunks = split_message_for_discord(&msg);
        // Should split at the newline
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].ends_with('\n'));
        assert!(chunks[1].starts_with('b'));
    }

    #[test]
    fn split_prefer_space_break() {
        let msg = format!("{} {}", "a".repeat(1500), "b".repeat(600));
        let chunks = split_message_for_discord(&msg);
        assert_eq!(chunks.len(), 2);
    }

    #[test]
    fn split_without_good_break_points_hard_split() {
        // No spaces or newlines - should hard split at 2000
        let msg = "a".repeat(5000);
        let chunks = split_message_for_discord(&msg);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].chars().count(), DISCORD_MAX_MESSAGE_LENGTH);
        assert_eq!(chunks[1].chars().count(), DISCORD_MAX_MESSAGE_LENGTH);
        assert_eq!(chunks[2].chars().count(), 1000);
    }

    #[test]
    fn split_multiple_breaks() {
        // Create a message with multiple newlines
        let part1 = "a".repeat(900);
        let part2 = "b".repeat(900);
        let part3 = "c".repeat(900);
        let msg = format!("{part1}\n{part2}\n{part3}");
        let chunks = split_message_for_discord(&msg);
        // Should split into 2 chunks (first two parts + third part)
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].chars().count() <= DISCORD_MAX_MESSAGE_LENGTH);
        assert!(chunks[1].chars().count() <= DISCORD_MAX_MESSAGE_LENGTH);
    }

    #[test]
    fn split_preserves_content() {
        let original = "Hello world! This is a test message with some content. ".repeat(200);
        let chunks = split_message_for_discord(&original);
        let reconstructed = chunks.concat();
        assert_eq!(reconstructed, original);
    }

    #[test]
    fn split_unicode_content() {
        // Test with emoji and multi-byte characters
        let msg = "🦀 Rust is awesome! ".repeat(500);
        let chunks = split_message_for_discord(&msg);
        // All chunks should be valid UTF-8
        for chunk in &chunks {
            assert!(std::str::from_utf8(chunk.as_bytes()).is_ok());
            assert!(chunk.chars().count() <= DISCORD_MAX_MESSAGE_LENGTH);
        }
        // Reconstruct and verify
        let reconstructed = chunks.concat();
        assert_eq!(reconstructed, msg);
    }

    #[test]
    fn split_newline_too_close_to_end() {
        // If newline is in the first half, don't use it - use space instead or hard split
        let msg = format!("{}\n{}", "a".repeat(1900), "b".repeat(500));
        let chunks = split_message_for_discord(&msg);
        // Should split at newline since it's in the second half of the window
        assert_eq!(chunks.len(), 2);
    }

    #[test]
    fn split_multibyte_only_content_without_panics() {
        let msg = "🦀".repeat(2500);
        let chunks = split_message_for_discord(&msg);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chars().count(), DISCORD_MAX_MESSAGE_LENGTH);
        assert_eq!(chunks[1].chars().count(), 500);
        let reconstructed = chunks.concat();
        assert_eq!(reconstructed, msg);
    }

    #[test]
    fn split_chunks_always_within_discord_limit() {
        let msg = "x".repeat(12_345);
        let chunks = split_message_for_discord(&msg);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.chars().count() <= DISCORD_MAX_MESSAGE_LENGTH)
        );
    }

    #[test]
    fn split_message_with_multiple_newlines() {
        let msg = "Line 1\nLine 2\nLine 3\n".repeat(1000);
        let chunks = split_message_for_discord(&msg);
        assert!(chunks.len() > 1);
        let reconstructed = chunks.concat();
        assert_eq!(reconstructed, msg);
    }

    #[test]
    fn typing_handles_start_empty() {
        let ch = DiscordChannel::new("fake".into(), None, vec![], false, false);
        let guard = ch.typing_handles.lock();
        assert!(guard.is_empty());
    }

    #[tokio::test]
    async fn start_typing_sets_handle() {
        let ch = DiscordChannel::new("fake".into(), None, vec![], false, false);
        let _ = ch.start_typing("123456").await;
        let guard = ch.typing_handles.lock();
        assert!(guard.contains_key("123456"));
    }

    #[tokio::test]
    async fn stop_typing_clears_handle() {
        let ch = DiscordChannel::new("fake".into(), None, vec![], false, false);
        let _ = ch.start_typing("123456").await;
        let _ = ch.stop_typing("123456").await;
        let guard = ch.typing_handles.lock();
        assert!(!guard.contains_key("123456"));
    }

    #[tokio::test]
    async fn stop_typing_is_idempotent() {
        let ch = DiscordChannel::new("fake".into(), None, vec![], false, false);
        assert!(ch.stop_typing("123456").await.is_ok());
        assert!(ch.stop_typing("123456").await.is_ok());
    }

    #[tokio::test]
    async fn concurrent_typing_handles_are_independent() {
        let ch = DiscordChannel::new("fake".into(), None, vec![], false, false);
        let _ = ch.start_typing("111").await;
        let _ = ch.start_typing("222").await;
        {
            let guard = ch.typing_handles.lock();
            assert_eq!(guard.len(), 2);
            assert!(guard.contains_key("111"));
            assert!(guard.contains_key("222"));
        }
        // Stopping one does not affect the other
        let _ = ch.stop_typing("111").await;
        let guard = ch.typing_handles.lock();
        assert_eq!(guard.len(), 1);
        assert!(guard.contains_key("222"));
    }

    // ── Emoji encoding for reactions ──────────────────────────────

    #[test]
    fn encode_emoji_unicode_percent_encodes() {
        let encoded = encode_emoji_for_discord("\u{1F440}");
        assert_eq!(encoded, "%F0%9F%91%80");
    }

    #[test]
    fn encode_emoji_checkmark() {
        let encoded = encode_emoji_for_discord("\u{2705}");
        assert_eq!(encoded, "%E2%9C%85");
    }

    #[test]
    fn encode_emoji_custom_guild_emoji_passthrough() {
        let encoded = encode_emoji_for_discord("custom_emoji:123456789");
        assert_eq!(encoded, "custom_emoji:123456789");
    }

    #[test]
    fn encode_emoji_simple_ascii_char() {
        let encoded = encode_emoji_for_discord("A");
        assert_eq!(encoded, "%41");
    }

    #[test]
    fn random_discord_ack_reaction_is_from_pool() {
        for _ in 0..128 {
            let emoji = random_discord_ack_reaction();
            assert!(DISCORD_ACK_REACTIONS.contains(&emoji));
        }
    }

    #[test]
    fn discord_reaction_url_encodes_emoji_and_strips_prefix() {
        let url = discord_reaction_url("123", "discord_456", "👀");
        assert_eq!(
            url,
            "https://discord.com/api/v10/channels/123/messages/456/reactions/%F0%9F%91%80/@me"
        );
    }

    // ── Message ID edge cases ─────────────────────────────────────

    #[test]
    fn discord_message_id_format_includes_discord_prefix() {
        // Verify that message IDs follow the format: discord_{message_id}
        let message_id = "123456789012345678";
        let expected_id = format!("discord_{message_id}");
        assert_eq!(expected_id, "discord_123456789012345678");
    }

    #[test]
    fn discord_message_id_is_deterministic() {
        // Same message_id = same ID (prevents duplicates after restart)
        let message_id = "123456789012345678";
        let id1 = format!("discord_{message_id}");
        let id2 = format!("discord_{message_id}");
        assert_eq!(id1, id2);
    }

    #[test]
    fn discord_message_id_different_message_different_id() {
        // Different message IDs produce different IDs
        let id1 = "discord_123456789012345678".to_string();
        let id2 = "discord_987654321098765432".to_string();
        assert_ne!(id1, id2);
    }

    #[test]
    fn discord_message_id_uses_snowflake_id() {
        // Discord snowflake IDs are numeric strings
        let message_id = "123456789012345678"; // Typical snowflake format
        let id = format!("discord_{message_id}");
        assert!(id.starts_with("discord_"));
        // Snowflake IDs are numeric
        assert!(message_id.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn discord_message_id_fallback_to_uuid_on_empty() {
        // Edge case: empty message_id falls back to UUID
        let message_id = "";
        let id = if message_id.is_empty() {
            format!("discord_{}", uuid::Uuid::new_v4())
        } else {
            format!("discord_{message_id}")
        };
        assert!(id.starts_with("discord_"));
        // Should have UUID dashes
        assert!(id.contains('-'));
    }

    // ─────────────────────────────────────────────────────────────────────
    // TG6: Channel platform limit edge cases for Discord (2000 char limit)
    // Prevents: Pattern 6 — issues #574, #499
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn split_message_code_block_at_boundary() {
        // Code block that spans the split boundary
        let mut msg = String::new();
        msg.push_str("```rust\n");
        msg.push_str(&"x".repeat(1990));
        msg.push_str("\n```\nMore text after code block");
        let parts = split_message_for_discord(&msg);
        assert!(
            parts.len() >= 2,
            "code block spanning boundary should split"
        );
        for part in &parts {
            assert!(
                part.len() <= DISCORD_MAX_MESSAGE_LENGTH,
                "each part must be <= {DISCORD_MAX_MESSAGE_LENGTH}, got {}",
                part.len()
            );
        }
    }

    #[test]
    fn split_message_single_long_word_exceeds_limit() {
        // A single word longer than 2000 chars must be hard-split
        let long_word = "a".repeat(2500);
        let parts = split_message_for_discord(&long_word);
        assert!(parts.len() >= 2, "word exceeding limit must be split");
        for part in &parts {
            assert!(
                part.len() <= DISCORD_MAX_MESSAGE_LENGTH,
                "hard-split part must be <= {DISCORD_MAX_MESSAGE_LENGTH}, got {}",
                part.len()
            );
        }
        // Reassembled content should match original
        let reassembled: String = parts.join("");
        assert_eq!(reassembled, long_word);
    }

    #[test]
    fn split_message_exactly_at_limit_no_split() {
        let msg = "a".repeat(DISCORD_MAX_MESSAGE_LENGTH);
        let parts = split_message_for_discord(&msg);
        assert_eq!(parts.len(), 1, "message exactly at limit should not split");
        assert_eq!(parts[0].len(), DISCORD_MAX_MESSAGE_LENGTH);
    }

    #[test]
    fn split_message_one_over_limit_splits() {
        let msg = "a".repeat(DISCORD_MAX_MESSAGE_LENGTH + 1);
        let parts = split_message_for_discord(&msg);
        assert!(parts.len() >= 2, "message 1 char over limit must split");
    }

    #[test]
    fn split_message_many_short_lines() {
        // Many short lines should be batched into chunks under the limit
        let msg: String = (0..500).fold(String::new(), |mut acc, i| {
            let _ = writeln!(acc, "line {i}");
            acc
        });
        let parts = split_message_for_discord(&msg);
        for part in &parts {
            assert!(
                part.len() <= DISCORD_MAX_MESSAGE_LENGTH,
                "short-line batch must be <= limit"
            );
        }
        // All content should be preserved
        let reassembled: String = parts.join("");
        assert_eq!(reassembled.trim(), msg.trim());
    }

    #[test]
    fn split_message_only_whitespace() {
        let msg = "   \n\n\t  ";
        let parts = split_message_for_discord(msg);
        // Should handle gracefully without panic
        assert!(parts.len() <= 1);
    }

    #[test]
    fn split_message_emoji_at_boundary() {
        // Emoji are multi-byte; ensure we don't split mid-emoji
        let mut msg = "a".repeat(1998);
        msg.push_str("🎉🎊"); // 2 emoji at the boundary (2000 chars total)
        let parts = split_message_for_discord(&msg);
        for part in &parts {
            // The function splits on character count, not byte count
            assert!(
                part.chars().count() <= DISCORD_MAX_MESSAGE_LENGTH,
                "emoji boundary split must respect limit"
            );
        }
    }

    #[test]
    fn split_message_consecutive_newlines_at_boundary() {
        let mut msg = "a".repeat(1995);
        msg.push_str("\n\n\n\n\n");
        msg.push_str(&"b".repeat(100));
        let parts = split_message_for_discord(&msg);
        for part in &parts {
            assert!(part.len() <= DISCORD_MAX_MESSAGE_LENGTH);
        }
    }

    // process_attachments tests

    #[tokio::test]
    async fn process_attachments_empty_list_returns_empty() {
        let client = reqwest::Client::new();
        let (text, media) = process_attachments(&[], &client, None, None).await;
        assert!(text.is_empty());
        assert!(media.is_empty());
    }

    #[test]
    fn marker_kind_for_classifies_each_mime_family() {
        assert_eq!(marker_kind_for("image/png", false), "IMAGE");
        assert_eq!(marker_kind_for("image/jpeg", false), "IMAGE");
        assert_eq!(marker_kind_for("video/mp4", false), "VIDEO");
        assert_eq!(marker_kind_for("application/pdf", false), "DOCUMENT");
        assert_eq!(marker_kind_for("application/zip", false), "DOCUMENT");
        assert_eq!(marker_kind_for("", false), "DOCUMENT");
    }

    #[test]
    fn marker_kind_for_treats_audio_flag_as_audio_regardless_of_content_type() {
        // Filename-detected audio with no content_type should still classify
        // as AUDIO, matching the unified inbound pipeline.
        assert_eq!(marker_kind_for("", true), "AUDIO");
        assert_eq!(marker_kind_for("application/octet-stream", true), "AUDIO");
    }

    #[test]
    fn marker_kind_for_prefers_image_over_audio_when_content_type_is_image() {
        // Defensive: if a Discord attachment somehow tripped both heuristics,
        // image MIME wins so vision-capable providers still receive image
        // bytes through the MediaAttachment path.
        assert_eq!(marker_kind_for("image/png", true), "IMAGE");
    }

    #[test]
    fn is_thread_channel_type_matches_only_thread_types() {
        // Thread types per Discord docs: 10/11/12.
        assert!(is_thread_channel_type(10));
        assert!(is_thread_channel_type(11));
        assert!(is_thread_channel_type(12));
        // Non-thread channel types must not be classified as threads.
        for non_thread in [0u64, 1, 2, 3, 4, 5, 13, 14, 15, 16] {
            assert!(
                !is_thread_channel_type(non_thread),
                "type {non_thread} must not classify as thread"
            );
        }
    }

    #[test]
    fn parse_attachment_markers_extracts_supported_markers() {
        let input = "Report\n[IMAGE:https://example.com/a.png]\n[DOCUMENT:/tmp/a.pdf]";
        let (cleaned, attachments) = parse_attachment_markers(input);

        assert_eq!(cleaned, "Report");
        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0].kind, DiscordAttachmentKind::Image);
        assert_eq!(attachments[0].target, "https://example.com/a.png");
        assert_eq!(attachments[1].kind, DiscordAttachmentKind::Document);
        assert_eq!(attachments[1].target, "/tmp/a.pdf");
    }

    #[test]
    fn parse_attachment_markers_keeps_invalid_marker_text() {
        let input = "Hello [NOT_A_MARKER:foo] world";
        let (cleaned, attachments) = parse_attachment_markers(input);

        assert_eq!(cleaned, input);
        assert!(attachments.is_empty());
    }

    #[test]
    fn classify_outgoing_attachments_keeps_workspace_locals_and_http() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("image.png");
        std::fs::write(&file_path, b"fake").expect("write fixture");

        let attachments = vec![
            DiscordAttachment {
                kind: DiscordAttachmentKind::Image,
                target: file_path.to_string_lossy().to_string(),
            },
            DiscordAttachment {
                kind: DiscordAttachmentKind::Image,
                target: "https://example.com/remote.png".to_string(),
            },
        ];

        let (locals, remotes, failures) =
            classify_outgoing_attachments(&attachments, Some(temp.path()));
        assert_eq!(locals.len(), 1);
        let canonical_file = std::fs::canonicalize(&file_path).expect("canonicalize fixture");
        assert_eq!(locals[0], canonical_file);
        assert_eq!(remotes, vec!["https://example.com/remote.png".to_string()]);
        assert!(failures.is_empty());
    }

    #[test]
    fn classify_outgoing_attachments_drops_missing_absolute_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let attachments = vec![DiscordAttachment {
            kind: DiscordAttachmentKind::Video,
            target: temp
                .path()
                .join("does-not-exist.mp4")
                .to_string_lossy()
                .to_string(),
        }];

        let (locals, remotes, failures) =
            classify_outgoing_attachments(&attachments, Some(temp.path()));
        assert!(locals.is_empty());
        assert!(remotes.is_empty());
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].1, DiscordMarkerFailure::NotFound);
    }

    #[test]
    fn classify_outgoing_attachments_drops_paths_outside_workspace() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        let outside_file = outside.path().join("escape.png");
        std::fs::write(&outside_file, b"fake").expect("write fixture");

        let attachments = vec![DiscordAttachment {
            kind: DiscordAttachmentKind::Image,
            target: outside_file.to_string_lossy().to_string(),
        }];

        let (locals, remotes, failures) =
            classify_outgoing_attachments(&attachments, Some(workspace.path()));
        assert!(
            locals.is_empty(),
            "absolute paths outside workspace must be refused"
        );
        assert!(remotes.is_empty());
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].1, DiscordMarkerFailure::Refused);
    }

    #[test]
    fn classify_outgoing_attachments_drops_relative_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let attachments = vec![DiscordAttachment {
            kind: DiscordAttachmentKind::Document,
            target: "relative/report.pdf".to_string(),
        }];

        let (locals, remotes, failures) =
            classify_outgoing_attachments(&attachments, Some(temp.path()));
        assert!(locals.is_empty(), "relative paths must be refused");
        assert!(remotes.is_empty());
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].1, DiscordMarkerFailure::Refused);
    }

    #[test]
    fn classify_outgoing_attachments_drops_disallowed_schemes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let attachments = vec![
            DiscordAttachment {
                kind: DiscordAttachmentKind::Image,
                target: "file:///etc/hostname".to_string(),
            },
            DiscordAttachment {
                kind: DiscordAttachmentKind::Document,
                target: "data:text/plain;base64,aGk=".to_string(),
            },
            DiscordAttachment {
                kind: DiscordAttachmentKind::Video,
                target: "ftp://example.com/clip.mp4".to_string(),
            },
        ];

        let (locals, remotes, failures) =
            classify_outgoing_attachments(&attachments, Some(temp.path()));
        assert!(locals.is_empty());
        assert!(remotes.is_empty());
        assert_eq!(failures.len(), 3);
        for (_, kind) in &failures {
            assert_eq!(*kind, DiscordMarkerFailure::Refused);
        }
    }

    #[test]
    fn classify_outgoing_attachments_refuses_local_without_workspace() {
        let attachments = vec![DiscordAttachment {
            kind: DiscordAttachmentKind::Image,
            target: "/some/absolute/path.png".to_string(),
        }];

        let (locals, remotes, failures) = classify_outgoing_attachments(&attachments, None);
        assert!(
            locals.is_empty(),
            "local paths must be refused without workspace_dir"
        );
        assert!(remotes.is_empty());
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].1, DiscordMarkerFailure::Refused);
    }

    #[test]
    fn classify_outgoing_attachments_passes_http_without_workspace() {
        let attachments = vec![DiscordAttachment {
            kind: DiscordAttachmentKind::Image,
            target: "https://example.com/x.png".to_string(),
        }];

        let (locals, remotes, failures) = classify_outgoing_attachments(&attachments, None);
        assert!(locals.is_empty());
        assert_eq!(remotes, vec!["https://example.com/x.png".to_string()]);
        assert!(failures.is_empty());
    }

    #[test]
    fn with_inline_attachment_urls_appends_remote_urls_only() {
        let content = "Done";
        let remote_urls = vec!["https://example.com/a.png".to_string()];

        let rendered = with_inline_attachment_urls(content, &remote_urls);
        assert_eq!(rendered, "Done\nhttps://example.com/a.png");
    }

    #[test]
    fn with_inline_attachment_urls_keeps_content_when_no_urls() {
        let rendered = with_inline_attachment_urls("Done", &[]);
        assert_eq!(rendered, "Done");
    }

    #[test]
    fn delivery_failure_note_is_none_when_no_failures() {
        assert!(delivery_failure_note(&[]).is_none());
    }

    #[test]
    fn delivery_failure_note_singular_for_one_failure() {
        let note = delivery_failure_note(&[(
            "/workspace/missing.png".to_string(),
            DiscordMarkerFailure::NotFound,
        )])
        .expect("one failure should produce a note");
        assert_eq!(
            note,
            "(note: I couldn't deliver the file at /workspace/missing.png.)"
        );
    }

    #[test]
    fn delivery_failure_note_plural_lists_targets_in_order() {
        let note = delivery_failure_note(&[
            ("a.png".to_string(), DiscordMarkerFailure::Refused),
            ("b.pdf".to_string(), DiscordMarkerFailure::NotFound),
            ("c.mp4".to_string(), DiscordMarkerFailure::Refused),
        ])
        .expect("multiple failures should produce a note");
        assert_eq!(
            note,
            "(note: I couldn't deliver these files: a.png, b.pdf, c.mp4.)"
        );
    }

    #[test]
    fn compose_body_with_failure_note_uses_note_alone_when_content_empty() {
        let composed = compose_body_with_failure_note("", Some("(note: ...)"));
        assert_eq!(composed, "(note: ...)");
    }

    #[test]
    fn compose_body_with_failure_note_appends_note_to_existing_content() {
        let composed = compose_body_with_failure_note("Hello.", Some("(note: ...)"));
        assert_eq!(composed, "Hello.\n\n(note: ...)");
    }

    #[test]
    fn compose_body_with_failure_note_returns_content_when_no_note() {
        let composed = compose_body_with_failure_note("Hello.", None);
        assert_eq!(composed, "Hello.");
    }

    #[test]
    fn compose_body_with_failure_note_returns_empty_when_no_content_and_no_note() {
        let composed = compose_body_with_failure_note("", None);
        assert_eq!(composed, "");
    }

    #[test]
    fn decide_failure_reactions_empty_for_no_failures() {
        assert!(decide_failure_reactions(&[]).is_empty());
    }

    #[test]
    fn decide_failure_reactions_emits_refused_only() {
        let r = decide_failure_reactions(&[
            ("a".to_string(), DiscordMarkerFailure::Refused),
            ("b".to_string(), DiscordMarkerFailure::Refused),
        ]);
        assert_eq!(r, vec!["🚫"]);
    }

    #[test]
    fn decide_failure_reactions_emits_not_found_only() {
        let r = decide_failure_reactions(&[("a".to_string(), DiscordMarkerFailure::NotFound)]);
        assert_eq!(r, vec!["\u{26A0}\u{FE0F}"]);
    }

    #[test]
    fn decide_failure_reactions_emits_both_when_mixed() {
        let r = decide_failure_reactions(&[
            ("a".to_string(), DiscordMarkerFailure::Refused),
            ("b".to_string(), DiscordMarkerFailure::NotFound),
        ]);
        assert_eq!(r, vec!["🚫", "\u{26A0}\u{FE0F}"]);
    }

    // ── Streaming mode tests ──────────────────────────────────────────

    #[test]
    fn supports_draft_updates_respects_stream_mode() {
        use zeroclaw_config::schema::StreamMode;

        let off = DiscordChannel::new("t".into(), None, vec![], false, false);
        assert!(!off.supports_draft_updates());

        let partial = DiscordChannel::new("t".into(), None, vec![], false, false).with_streaming(
            StreamMode::Partial,
            750,
            800,
        );
        assert!(partial.supports_draft_updates());
        assert_eq!(partial.draft_update_interval_ms, 750);

        let multi = DiscordChannel::new("t".into(), None, vec![], false, false).with_streaming(
            StreamMode::MultiMessage,
            1000,
            600,
        );
        assert!(multi.supports_draft_updates());
        assert_eq!(multi.multi_message_delay_ms, 600);
    }

    #[tokio::test]
    async fn send_draft_returns_none_when_not_partial() {
        use zeroclaw_api::channel::SendMessage;
        use zeroclaw_config::schema::StreamMode;

        let off = DiscordChannel::new("t".into(), None, vec![], false, false);
        let msg = SendMessage::new("hello", "123");
        assert!(off.send_draft(&msg).await.unwrap().is_none());

        let multi = DiscordChannel::new("t".into(), None, vec![], false, false).with_streaming(
            StreamMode::MultiMessage,
            1000,
            800,
        );
        // MultiMessage returns a synthetic ID so the draft_updater task runs.
        assert_eq!(
            multi.send_draft(&msg).await.unwrap().as_deref(),
            Some("multi_message_synthetic")
        );
    }

    #[tokio::test]
    async fn update_draft_rate_limit_short_circuits() {
        use zeroclaw_config::schema::StreamMode;

        let ch = DiscordChannel::new("t".into(), None, vec![], false, false).with_streaming(
            StreamMode::Partial,
            60_000,
            800,
        );

        // Seed a recent edit time.
        ch.last_draft_edit
            .lock()
            .insert("chan".to_string(), std::time::Instant::now());

        // Should return Ok immediately (rate-limited) without making a network call.
        let result = ch.update_draft("chan", "fake_msg_id", "new text").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn cancel_draft_cleans_up_tracking() {
        use zeroclaw_config::schema::StreamMode;

        let ch = DiscordChannel::new("t".into(), None, vec![], false, false).with_streaming(
            StreamMode::Partial,
            1000,
            800,
        );

        ch.last_draft_edit
            .lock()
            .insert("chan".to_string(), std::time::Instant::now());

        // cancel_draft will try to delete a message (will fail with network error)
        // but should still clean up the tracking entry.
        let _ = ch.cancel_draft("chan", "fake_msg_id").await;
        assert!(!ch.last_draft_edit.lock().contains_key("chan"));
    }

    // ── MultiMessage splitter tests ───────────────────────────────────

    #[test]
    fn split_message_for_discord_multi_splits_at_paragraphs() {
        let content = "First paragraph.\n\nSecond paragraph.\n\nThird paragraph.";
        let chunks = split_message_for_discord_multi(content, 2000);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], "First paragraph.");
        assert_eq!(chunks[1], "Second paragraph.");
        assert_eq!(chunks[2], "Third paragraph.");
    }

    #[test]
    fn split_message_for_discord_multi_single_paragraph() {
        let content = "Just one paragraph with no breaks.";
        let chunks = split_message_for_discord_multi(content, 2000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], content);
    }

    #[test]
    fn split_message_for_discord_multi_respects_max_len() {
        // Create a single paragraph that exceeds max_len.
        let long_para = "a ".repeat(1100); // ~2200 chars
        let chunks = split_message_for_discord_multi(&long_para, 2000);
        assert!(chunks.len() > 1, "should split oversized paragraph");
        for chunk in &chunks {
            assert!(
                chunk.chars().count() <= 2000,
                "chunk exceeds max: {}",
                chunk.chars().count()
            );
        }
    }

    #[test]
    fn split_message_for_discord_multi_preserves_code_fences() {
        let content =
            "Before.\n\n```rust\nfn main() {\n\n    println!(\"hello\");\n}\n```\n\nAfter.";
        let chunks = split_message_for_discord_multi(content, 2000);
        // The code fence contains \n\n but should not be split there.
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], "Before.");
        assert!(chunks[1].contains("```rust"));
        assert!(chunks[1].contains("println!"));
        assert!(chunks[1].contains("```"));
        assert_eq!(chunks[2], "After.");
    }

    #[test]
    fn split_message_for_discord_multi_empty_input() {
        let chunks = split_message_for_discord_multi("", 2000);
        assert!(chunks.is_empty());
    }

    // Regression lock for the marker-only paragraph in MultiMessage stream
    // mode. Before the fix this produced an empty chunk vec and the chunk
    // loop in send() iterated zero times, silently skipping the file
    // upload.
    #[test]
    fn chunks_for_send_emits_empty_chunk_when_multi_message_paragraph_collapses_to_only_a_file() {
        use zeroclaw_config::schema::StreamMode;

        let chunks = chunks_for_send("", StreamMode::MultiMessage, 2000, true);
        assert_eq!(chunks, vec![String::new()]);
    }

    // Inverse guard: no file to upload means no empty chunk to send, so we
    // do not accidentally POST an empty Discord message that Discord would
    // reject for having no content, attachments, or embeds.
    #[test]
    fn chunks_for_send_does_not_emit_empty_chunk_when_no_files_to_upload() {
        use zeroclaw_config::schema::StreamMode;

        let chunks = chunks_for_send("", StreamMode::MultiMessage, 2000, false);
        assert!(chunks.is_empty());
    }

    // The normal path with body text is left untouched in both stream
    // modes, regardless of whether files are attached.
    #[test]
    fn chunks_for_send_passes_through_non_empty_content() {
        use zeroclaw_config::schema::StreamMode;

        for mode in [
            StreamMode::MultiMessage,
            StreamMode::Partial,
            StreamMode::Off,
        ] {
            for has_files in [true, false] {
                let chunks = chunks_for_send("hello", mode, 2000, has_files);
                assert_eq!(
                    chunks,
                    vec!["hello".to_string()],
                    "mode={mode:?} has_files={has_files}"
                );
            }
        }
    }

    fn make_discord_channel() -> DiscordChannel {
        DiscordChannel::new("token".into(), None, vec![], false, false)
    }

    #[test]
    fn pending_approvals_map_is_initially_empty() {
        let ch = make_discord_channel();
        let map = ch.pending_approvals.try_lock().unwrap();
        assert!(map.is_empty());
    }

    #[test]
    fn approval_timeout_defaults_to_300_and_is_overridable() {
        let ch = make_discord_channel();
        assert_eq!(ch.approval_timeout_secs, 300);
        let ch = ch.with_approval_timeout_secs(60);
        assert_eq!(ch.approval_timeout_secs, 60);
    }

    #[tokio::test]
    async fn pending_approval_oneshot_delivers_response() {
        let ch = make_discord_channel();
        let (tx, rx) = oneshot::channel();
        ch.pending_approvals
            .lock()
            .await
            .insert("abc123".to_string(), tx);
        let sender = ch.pending_approvals.lock().await.remove("abc123").unwrap();
        sender.send(ChannelApprovalResponse::Deny).unwrap();
        assert_eq!(rx.await.unwrap(), ChannelApprovalResponse::Deny);
    }
}
