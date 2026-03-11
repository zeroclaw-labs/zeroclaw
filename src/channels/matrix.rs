use crate::channels::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use futures_util::StreamExt;
use matrix_sdk::{
    attachment::{AttachmentConfig, AttachmentInfo, BaseAudioInfo},
    authentication::matrix::MatrixSession,
    config::SyncSettings,
    encryption::verification::{SasState, VerificationRequestState},
    media::{MediaFormat, MediaRequestParameters},
    ruma::{
        api::client::uiaa,
        events::{
            reaction::{OriginalSyncReactionEvent, ReactionEventContent},
            relation::Annotation,
            room::{
                message::{
                    LocationMessageEventContent, MessageType, OriginalSyncRoomMessageEvent,
                    RoomMessageEventContent,
                },
                MediaSource,
            },
        },
        OwnedEventId, OwnedRoomId, OwnedUserId,
    },
    Client as MatrixSdkClient, LoopCtrl, Room, RoomState, SessionMeta, SessionTokens,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, OnceCell, RwLock};

/// Maximum media download size (50 MB).
const MATRIX_MAX_MEDIA_DOWNLOAD_BYTES: usize = 50 * 1024 * 1024;

/// Filename for persisted session credentials (access_token + device_id).
const MATRIX_SESSION_FILE: &str = "session.json";

/// Persisted Matrix session credentials, saved after login_username().
/// Allows subsequent runs to restore_session() without re-reading the password.
#[derive(Serialize, Deserialize)]
struct SavedSession {
    access_token: String,
    device_id: String,
    user_id: String,
}

/// Matrix channel for Matrix Client-Server API.
/// Uses matrix-sdk for reliable sync and encrypted-room decryption.
#[derive(Clone)]
pub struct MatrixChannel {
    homeserver: String,
    access_token: Option<String>,
    room_id: String,
    allowed_users: Vec<String>,
    session_owner_hint: Option<String>,
    session_device_id_hint: Option<String>,
    zeroclaw_dir: Option<PathBuf>,
    resolved_room_id_cache: Arc<RwLock<Option<String>>>,
    sdk_client: Arc<OnceCell<MatrixSdkClient>>,
    http_client: Client,
    transcription: Option<crate::config::TranscriptionConfig>,
    voice_transcriptions: Arc<Mutex<std::collections::HashMap<String, String>>>,
    password: Option<String>,
}

impl std::fmt::Debug for MatrixChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MatrixChannel")
            .field("homeserver", &self.homeserver)
            .field("room_id", &self.room_id)
            .field("allowed_users", &self.allowed_users)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Deserialize)]
struct SyncResponse {
    next_batch: String,
    #[serde(default)]
    rooms: Rooms,
}

#[derive(Debug, Deserialize, Default)]
struct Rooms {
    #[serde(default)]
    join: std::collections::HashMap<String, JoinedRoom>,
}

#[derive(Debug, Deserialize)]
struct JoinedRoom {
    #[serde(default)]
    timeline: Timeline,
}

#[derive(Debug, Deserialize, Default)]
struct Timeline {
    #[serde(default)]
    events: Vec<TimelineEvent>,
}

#[derive(Debug, Deserialize)]
struct TimelineEvent {
    #[serde(rename = "type")]
    event_type: String,
    sender: String,
    #[serde(default)]
    event_id: Option<String>,
    #[serde(default)]
    content: EventContent,
}

#[derive(Debug, Deserialize, Default)]
struct EventContent {
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    msgtype: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WhoAmIResponse {
    user_id: String,
    #[serde(default)]
    device_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RoomAliasResponse {
    room_id: String,
}

// --- Outgoing reaction marker: [REACT:emoji:$event_id] ---

#[derive(Debug, Clone, PartialEq, Eq)]
struct MatrixOutgoingReaction {
    emoji: String,
    event_id: String,
}

/// Parse `[REACT:emoji:$event_id]` markers from response text.
/// Returns cleaned text (markers removed) and list of reactions.
fn parse_matrix_reaction_markers(message: &str) -> (String, Vec<MatrixOutgoingReaction>) {
    let mut cleaned = String::with_capacity(message.len());
    let mut reactions = Vec::new();
    let mut rest = message;

    while let Some(start) = rest.find("[REACT:") {
        cleaned.push_str(&rest[..start]);
        let after_prefix = &rest[start + 7..];
        if let Some(end) = after_prefix.find(']') {
            let inner = &after_prefix[..end];
            // Split into emoji and event_id: "👍:$event_id"
            if let Some(colon) = inner.find(':') {
                let emoji = inner[..colon].trim();
                let event_id = inner[colon + 1..].trim();
                if !emoji.is_empty() && !event_id.is_empty() {
                    reactions.push(MatrixOutgoingReaction {
                        emoji: emoji.to_string(),
                        event_id: event_id.to_string(),
                    });
                }
            }
            rest = &after_prefix[end + 1..];
        } else {
            cleaned.push_str(&rest[start..start + 7]);
            rest = after_prefix;
        }
    }
    cleaned.push_str(rest);

    (cleaned.trim().to_string(), reactions)
}

// --- Outgoing location marker: [LOCATION:geo:lat,lon:description] ---

#[derive(Debug, Clone, PartialEq, Eq)]
struct MatrixOutgoingLocation {
    geo_uri: String,
    description: String,
}

/// Parse `[LOCATION:geo_uri:description]` markers from response text.
fn parse_matrix_location_markers(message: &str) -> (String, Vec<MatrixOutgoingLocation>) {
    let mut cleaned = String::with_capacity(message.len());
    let mut locations = Vec::new();
    let mut rest = message;

    while let Some(start) = rest.find("[LOCATION:") {
        cleaned.push_str(&rest[..start]);
        let after_prefix = &rest[start + 10..];
        if let Some(end) = after_prefix.find(']') {
            let inner = &after_prefix[..end];
            // Split: "geo:lat,lon:Description text"
            // geo_uri is "geo:..." up to second ':', description is the rest
            if let Some(geo_end) = inner.find(',').and_then(|comma_pos| {
                // Find the ':' after the comma (end of geo_uri)
                inner[comma_pos..].find(':').map(|p| comma_pos + p)
            }) {
                let geo_uri = inner[..geo_end].trim().to_string();
                let description = inner[geo_end + 1..].trim().to_string();
                if !geo_uri.is_empty() {
                    locations.push(MatrixOutgoingLocation {
                        geo_uri,
                        description: if description.is_empty() {
                            "Shared location".to_string()
                        } else {
                            description
                        },
                    });
                }
            }
            rest = &after_prefix[end + 1..];
        } else {
            cleaned.push_str(&rest[start..start + 10]);
            rest = after_prefix;
        }
    }
    cleaned.push_str(rest);

    (cleaned.trim().to_string(), locations)
}

// --- Outgoing attachment marker types (follows Telegram/Discord pattern) ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatrixOutgoingAttachmentKind {
    Image,
    File,
    Audio,
    Voice,
}

impl MatrixOutgoingAttachmentKind {
    fn from_marker(marker: &str) -> Option<Self> {
        match marker.trim().to_ascii_uppercase().as_str() {
            "IMAGE" | "PHOTO" => Some(Self::Image),
            "DOCUMENT" | "FILE" => Some(Self::File),
            "AUDIO" => Some(Self::Audio),
            "VOICE" => Some(Self::Voice),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MatrixOutgoingAttachment {
    kind: MatrixOutgoingAttachmentKind,
    target: String,
}

fn parse_matrix_attachment_markers(message: &str) -> (String, Vec<MatrixOutgoingAttachment>) {
    let mut cleaned = String::with_capacity(message.len());
    let mut attachments = Vec::new();
    let mut cursor = 0usize;

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
            let kind = MatrixOutgoingAttachmentKind::from_marker(kind)?;
            let target = target.trim();
            if target.is_empty() {
                return None;
            }
            Some(MatrixOutgoingAttachment {
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

fn is_image_extension(path: &Path) -> bool {
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

/// Download media from a Matrix media source via the SDK client, save to disk.
///
/// When `size_hint` is provided (from the event's `info.size`), the download is
/// rejected before fetching the payload. The post-download check remains as a
/// safety net because metadata can be spoofed.
async fn download_and_save_matrix_media(
    client: &MatrixSdkClient,
    source: &MediaSource,
    filename: &str,
    save_dir: &Path,
    size_hint: Option<u64>,
) -> anyhow::Result<PathBuf> {
    // Pre-download size check using event metadata (avoids buffering large payloads).
    if let Some(size) = size_hint {
        if size as usize > MATRIX_MAX_MEDIA_DOWNLOAD_BYTES {
            anyhow::bail!(
                "Matrix media metadata size exceeds limit ({size} bytes > {} bytes); skipping download",
                MATRIX_MAX_MEDIA_DOWNLOAD_BYTES,
            );
        }
    }

    let request = MediaRequestParameters {
        source: source.clone(),
        format: MediaFormat::File,
    };

    let data = client.media().get_media_content(&request, false).await?;

    // Post-download safety net (metadata can be spoofed).
    if data.len() > MATRIX_MAX_MEDIA_DOWNLOAD_BYTES {
        anyhow::bail!(
            "Matrix media exceeds size limit ({} bytes > {} bytes)",
            data.len(),
            MATRIX_MAX_MEDIA_DOWNLOAD_BYTES,
        );
    }

    tokio::fs::create_dir_all(save_dir).await?;

    // Sanitize filename: UUID prefix prevents collisions and path traversal.
    let safe_name = Path::new(filename)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("attachment.bin");
    let local_name = format!("{}_{}", uuid::Uuid::new_v4(), safe_name);
    let local_path = save_dir.join(&local_name);

    tokio::fs::write(&local_path, &data).await?;

    Ok(local_path)
}

impl MatrixChannel {
    fn normalize_optional_field(value: Option<String>) -> Option<String> {
        value
            .map(|entry| entry.trim().to_string())
            .filter(|entry| !entry.is_empty())
    }

    pub fn new(
        homeserver: String,
        access_token: Option<String>,
        room_id: String,
        allowed_users: Vec<String>,
    ) -> Self {
        Self::new_with_session_hint(homeserver, access_token, room_id, allowed_users, None, None)
    }

    pub fn new_with_session_hint(
        homeserver: String,
        access_token: Option<String>,
        room_id: String,
        allowed_users: Vec<String>,
        owner_hint: Option<String>,
        device_id_hint: Option<String>,
    ) -> Self {
        Self::new_with_session_hint_and_zeroclaw_dir(
            homeserver,
            access_token,
            room_id,
            allowed_users,
            owner_hint,
            device_id_hint,
            None,
        )
    }

    pub fn new_with_session_hint_and_zeroclaw_dir(
        homeserver: String,
        access_token: Option<String>,
        room_id: String,
        allowed_users: Vec<String>,
        owner_hint: Option<String>,
        device_id_hint: Option<String>,
        zeroclaw_dir: Option<PathBuf>,
    ) -> Self {
        let homeserver = homeserver.trim_end_matches('/').to_string();
        let access_token = access_token
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        let room_id = room_id.trim().to_string();
        let allowed_users = allowed_users
            .into_iter()
            .map(|user| user.trim().to_string())
            .filter(|user| !user.is_empty())
            .collect();

        Self {
            homeserver,
            access_token,
            room_id,
            allowed_users,
            session_owner_hint: Self::normalize_optional_field(owner_hint),
            session_device_id_hint: Self::normalize_optional_field(device_id_hint),
            zeroclaw_dir,
            resolved_room_id_cache: Arc::new(RwLock::new(None)),
            sdk_client: Arc::new(OnceCell::new()),
            http_client: Client::new(),
            transcription: None,
            voice_transcriptions: Arc::new(Mutex::new(std::collections::HashMap::new())),
            password: None,
        }
    }

    pub fn with_password(mut self, password: Option<String>) -> Self {
        self.password = password;
        self
    }

    pub fn with_transcription(mut self, config: crate::config::TranscriptionConfig) -> Self {
        if config.enabled {
            self.transcription = Some(config);
        }
        self
    }

    fn encode_path_segment(value: &str) -> String {
        fn should_encode(byte: u8) -> bool {
            !matches!(
                byte,
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~'
            )
        }

        let mut encoded = String::with_capacity(value.len());
        for byte in value.bytes() {
            if should_encode(byte) {
                use std::fmt::Write;
                let _ = write!(&mut encoded, "%{byte:02X}");
            } else {
                encoded.push(byte as char);
            }
        }

        encoded
    }

    /// Returns the Bearer token for HTTP API calls.
    /// Prefers the live SDK client session, falls back to config access_token,
    /// then to persisted session.json.
    async fn auth_header_value(&self) -> anyhow::Result<String> {
        // 1. Try live SDK client session (most up-to-date after password login).
        if let Some(client) = self.sdk_client.get() {
            if let Some(token) = client.access_token() {
                return Ok(format!("Bearer {token}"));
            }
        }
        // 2. Config access_token.
        if let Some(ref token) = self.access_token {
            return Ok(format!("Bearer {token}"));
        }
        // 3. Persisted session.json.
        if let Some(saved) = self.load_saved_session().await {
            return Ok(format!("Bearer {}", saved.access_token));
        }
        anyhow::bail!(
            "Matrix access_token is not available; configure access_token or password and restart"
        )
    }

    fn matrix_store_dir(&self) -> Option<PathBuf> {
        self.zeroclaw_dir
            .as_ref()
            .map(|dir| dir.join("state").join("matrix"))
    }

    fn session_file_path(&self) -> Option<PathBuf> {
        self.matrix_store_dir().map(|d| d.join(MATRIX_SESSION_FILE))
    }

    async fn load_saved_session(&self) -> Option<SavedSession> {
        let path = self.session_file_path()?;
        let data = tokio::fs::read_to_string(&path).await.ok()?;
        serde_json::from_str(&data).ok()
    }

    async fn save_session(&self, session: &SavedSession) -> anyhow::Result<()> {
        let path = self.session_file_path().ok_or_else(|| {
            anyhow::anyhow!("Matrix store directory not configured; cannot persist session")
        })?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let data = serde_json::to_string_pretty(session)?;
        // Write to a temp file with restricted permissions, then rename atomically
        // to avoid a window where the token is world-readable.
        let tmp_path = path.with_extension("json.tmp");
        #[cfg(unix)]
        {
            let mut opts = tokio::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true).mode(0o600);
            let mut file = opts.open(&tmp_path).await?;
            tokio::io::AsyncWriteExt::write_all(&mut file, data.as_bytes()).await?;
            tokio::io::AsyncWriteExt::flush(&mut file).await?;
        }
        #[cfg(not(unix))]
        {
            tokio::fs::write(&tmp_path, &data).await?;
        }
        tokio::fs::rename(&tmp_path, &path).await?;
        Ok(())
    }

    fn media_save_dir(&self) -> Option<PathBuf> {
        self.zeroclaw_dir
            .as_ref()
            .map(|dir| dir.join("workspace").join("matrix_files"))
    }

    fn is_user_allowed(&self, sender: &str) -> bool {
        Self::is_sender_allowed(&self.allowed_users, sender)
    }

    fn is_sender_allowed(allowed_users: &[String], sender: &str) -> bool {
        if allowed_users.iter().any(|u| u == "*") {
            return true;
        }

        allowed_users.iter().any(|u| u.eq_ignore_ascii_case(sender))
    }

    fn is_supported_message_type(msgtype: &str) -> bool {
        matches!(
            msgtype,
            "m.text" | "m.notice" | "m.image" | "m.file" | "m.audio"
        )
    }

    fn has_non_empty_body(body: &str) -> bool {
        !body.trim().is_empty()
    }

    fn cache_event_id(
        event_id: &str,
        recent_order: &mut std::collections::VecDeque<String>,
        recent_lookup: &mut std::collections::HashSet<String>,
    ) -> bool {
        const MAX_RECENT_EVENT_IDS: usize = 2048;

        if recent_lookup.contains(event_id) {
            return true;
        }

        let event_id_owned = event_id.to_string();
        recent_lookup.insert(event_id_owned.clone());
        recent_order.push_back(event_id_owned);

        if recent_order.len() > MAX_RECENT_EVENT_IDS {
            if let Some(evicted) = recent_order.pop_front() {
                recent_lookup.remove(&evicted);
            }
        }

        false
    }

    async fn target_room_id(&self) -> anyhow::Result<String> {
        if self.room_id.starts_with('!') {
            return Ok(self.room_id.clone());
        }

        if let Some(cached) = self.resolved_room_id_cache.read().await.clone() {
            return Ok(cached);
        }

        let resolved = self.resolve_room_id().await?;
        *self.resolved_room_id_cache.write().await = Some(resolved.clone());
        Ok(resolved)
    }

    async fn get_my_identity(&self) -> anyhow::Result<WhoAmIResponse> {
        let url = format!("{}/_matrix/client/v3/account/whoami", self.homeserver);
        let resp = self
            .http_client
            .get(&url)
            .header("Authorization", self.auth_header_value().await?)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await?;
            anyhow::bail!("Matrix whoami failed: {err}");
        }

        Ok(resp.json().await?)
    }

    async fn get_my_user_id(&self) -> anyhow::Result<String> {
        Ok(self.get_my_identity().await?.user_id)
    }

    async fn matrix_client(&self) -> anyhow::Result<MatrixSdkClient> {
        let client = self
            .sdk_client
            .get_or_try_init(|| async {
                // ── Build SDK client with E2EE + persistent store ──
                let encryption_settings = matrix_sdk::encryption::EncryptionSettings {
                    auto_enable_cross_signing: true,
                    auto_enable_backups: true,
                    backup_download_strategy:
                        matrix_sdk::encryption::BackupDownloadStrategy::AfterDecryptionFailure,
                };

                let mut client_builder = MatrixSdkClient::builder()
                    .homeserver_url(&self.homeserver)
                    .with_encryption_settings(encryption_settings);

                if let Some(store_dir) = self.matrix_store_dir() {
                    tokio::fs::create_dir_all(&store_dir).await.map_err(|error| {
                        anyhow::anyhow!(
                            "Matrix failed to initialize persistent store directory at '{}': {error}",
                            store_dir.display()
                        )
                    })?;
                    client_builder = client_builder.sqlite_store(&store_dir, None);
                }

                let client = client_builder.build().await?;

                // ── Auth: session.json → access_token → password → error ──
                let saved = self.load_saved_session().await;
                let resolved_user_id: String;

                // Determine whether to use the saved session or fall through
                // to config credentials. If config has fresh credentials and the
                // saved session's user_id doesn't match the configured owner hint,
                // prefer config to avoid stale session.json overriding intentional
                // config changes.
                let use_saved_session = if let Some(ref saved) = saved {
                    let config_has_credentials =
                        self.access_token.is_some() || self.password.is_some();
                    let saved_matches_config = self
                        .session_owner_hint
                        .as_ref()
                        .map_or(true, |hint| hint == &saved.user_id);

                    if config_has_credentials && !saved_matches_config {
                        tracing::warn!(
                            "Matrix: session.json user_id ({}) does not match configured owner hint — ignoring saved session and using config credentials",
                            crate::security::redact(&saved.user_id)
                        );
                        false
                    } else {
                        if config_has_credentials {
                            tracing::debug!(
                                "Matrix: config credentials present but session.json user matches — restoring saved session"
                            );
                        }
                        true
                    }
                } else {
                    false
                };

                if use_saved_session {
                    let saved = saved.as_ref().unwrap();
                    // Path 1: Restore from persisted session.json (previous login).
                    let user_id: OwnedUserId = saved.user_id.parse()?;
                    let session = MatrixSession {
                        meta: SessionMeta {
                            user_id,
                            device_id: saved.device_id.clone().into(),
                        },
                        tokens: SessionTokens {
                            access_token: saved.access_token.clone(),
                            refresh_token: None,
                        },
                    };
                    client.restore_session(session).await?;
                    resolved_user_id = saved.user_id.clone();
                    tracing::info!(
                        "Matrix: restored session from session.json (device_id={})",
                        crate::security::redact(&saved.device_id)
                    );
                } else if self.access_token.is_some() {
                    // Path 2: Restore from config access_token + device_id (legacy flow).
                    let identity = self.get_my_identity().await;
                    let whoami = match identity {
                        Ok(whoami) => Some(whoami),
                        Err(error) => {
                            if self.session_owner_hint.is_some()
                                && self.session_device_id_hint.is_some()
                            {
                                tracing::warn!(
                                    "Matrix whoami failed; falling back to configured session hints: {error}"
                                );
                                None
                            } else {
                                return Err(error);
                            }
                        }
                    };

                    let user_id_str = if let Some(whoami) = whoami.as_ref() {
                        whoami.user_id.clone()
                    } else {
                        self.session_owner_hint.clone().ok_or_else(|| {
                            anyhow::anyhow!(
                                "Matrix session restore requires user_id when whoami is unavailable"
                            )
                        })?
                    };

                    let device_id_str = match (
                        whoami.as_ref().and_then(|w| w.device_id.clone()),
                        self.session_device_id_hint.as_ref(),
                    ) {
                        (Some(whoami_did), _) => whoami_did,
                        (None, Some(hinted)) => hinted.clone(),
                        (None, None) => {
                            return Err(anyhow::anyhow!(
                                "Matrix E2EE requires device_id. Set channels.matrix.device_id or use password-based login."
                            ));
                        }
                    };

                    let user_id: OwnedUserId = user_id_str.parse()?;
                    let session = MatrixSession {
                        meta: SessionMeta {
                            user_id,
                            device_id: device_id_str.clone().into(),
                        },
                        tokens: SessionTokens {
                            access_token: self.access_token.clone().unwrap_or_default(),
                            refresh_token: None,
                        },
                    };
                    client.restore_session(session).await?;
                    resolved_user_id = user_id_str.clone();

                    // Persist session.json so future runs use Path 1.
                    if let Err(e) = self
                        .save_session(&SavedSession {
                            access_token: self.access_token.clone().unwrap_or_default(),
                            device_id: device_id_str,
                            user_id: user_id_str,
                        })
                        .await
                    {
                        tracing::warn!("Matrix: failed to persist session.json: {e}");
                    }
                } else if let Some(ref pw) = self.password {
                    // Path 3: Login with password (simplest setup, no access_token needed).
                    let user_id_str = self.session_owner_hint.as_ref().ok_or_else(|| {
                        anyhow::anyhow!(
                            "Matrix password-based login requires user_id in config"
                        )
                    })?;

                    let response = client
                        .matrix_auth()
                        .login_username(user_id_str, pw)
                        .initial_device_display_name("ZeroClaw")
                        .send()
                        .await?;

                    resolved_user_id = response.user_id.to_string();
                    tracing::info!(
                        "Matrix: logged in with password (device_id={})",
                        crate::security::redact(&response.device_id.to_string())
                    );

                    // Persist session.json so future runs use Path 1 (no password needed).
                    if let Err(e) = self
                        .save_session(&SavedSession {
                            access_token: response.access_token,
                            device_id: response.device_id.to_string(),
                            user_id: resolved_user_id.clone(),
                        })
                        .await
                    {
                        tracing::warn!("Matrix: failed to persist session.json: {e}");
                    }
                } else {
                    return Err(anyhow::anyhow!(
                        "Matrix channel requires either access_token or password in config"
                    ));
                }

                // ── E2EE initialization ──
                client
                    .encryption()
                    .wait_for_e2ee_initialization_tasks()
                    .await;

                // ── Cross-signing bootstrap with password (UIA) ──
                // Always attempt bootstrap_cross_signing (not _if_needed) so keys
                // are actually uploaded to the server. The _if_needed variant checks
                // the local store, which may have stale keys from auto_enable that
                // were never uploaded (server required UIA).
                if let Some(ref pw) = self.password {
                    match client
                        .encryption()
                        .bootstrap_cross_signing(None)
                        .await
                    {
                        Ok(()) => {
                            tracing::info!("Matrix: cross-signing bootstrap successful (no UIA needed)");
                        }
                        Err(e) => {
                            if let Some(response) = e.as_uiaa_response() {
                                let mut password_auth = uiaa::Password::new(
                                    uiaa::UserIdentifier::UserIdOrLocalpart(
                                        resolved_user_id.clone(),
                                    ),
                                    pw.clone(),
                                );
                                password_auth.session = response.session.clone();
                                match client
                                    .encryption()
                                    .bootstrap_cross_signing(Some(uiaa::AuthData::Password(
                                        password_auth,
                                    )))
                                    .await
                                {
                                    Ok(()) => {
                                        tracing::info!(
                                            "Matrix: cross-signing bootstrap successful (with password)"
                                        );
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "Matrix: cross-signing bootstrap with password failed: {e}"
                                        );
                                    }
                                }
                            } else {
                                tracing::warn!(
                                    "Matrix: cross-signing bootstrap failed (non-UIA): {e}"
                                );
                            }
                        }
                    }
                }

                Ok::<MatrixSdkClient, anyhow::Error>(client)
            })
            .await?;

        Ok(client.clone())
    }

    async fn resolve_room_id(&self) -> anyhow::Result<String> {
        let configured = self.room_id.trim();

        if configured.starts_with('!') {
            return Ok(configured.to_string());
        }

        if configured.starts_with('#') {
            let encoded_alias = Self::encode_path_segment(configured);
            let url = format!(
                "{}/_matrix/client/v3/directory/room/{}",
                self.homeserver, encoded_alias
            );

            let resp = self
                .http_client
                .get(&url)
                .header("Authorization", self.auth_header_value().await?)
                .send()
                .await?;

            if !resp.status().is_success() {
                let err = resp.text().await.unwrap_or_default();
                anyhow::bail!("Matrix room alias resolution failed for '{configured}': {err}");
            }

            let resolved: RoomAliasResponse = resp.json().await?;
            return Ok(resolved.room_id);
        }

        anyhow::bail!(
            "Matrix room reference must start with '!' (room ID) or '#' (room alias), got: {configured}"
        )
    }

    async fn ensure_room_accessible(&self, room_id: &str) -> anyhow::Result<()> {
        let encoded_room = Self::encode_path_segment(room_id);
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/joined_members",
            self.homeserver, encoded_room
        );

        let resp = self
            .http_client
            .get(&url)
            .header("Authorization", self.auth_header_value().await?)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Matrix room access check failed for '{room_id}': {err}");
        }

        Ok(())
    }

    async fn room_is_encrypted(&self, room_id: &str) -> anyhow::Result<bool> {
        let encoded_room = Self::encode_path_segment(room_id);
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/state/m.room.encryption",
            self.homeserver, encoded_room
        );

        let resp = self
            .http_client
            .get(&url)
            .header("Authorization", self.auth_header_value().await?)
            .send()
            .await?;

        if resp.status().is_success() {
            return Ok(true);
        }

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }

        let err = resp.text().await.unwrap_or_default();
        anyhow::bail!("Matrix room encryption check failed for '{room_id}': {err}");
    }

    async fn ensure_room_supported(&self, room_id: &str) -> anyhow::Result<()> {
        self.ensure_room_accessible(room_id).await?;

        if self.room_is_encrypted(room_id).await? {
            tracing::info!(
                "Matrix room {} is encrypted; E2EE decryption is enabled via matrix-sdk.",
                room_id
            );
        }

        Ok(())
    }

    fn sync_filter_for_room(room_id: &str, timeline_limit: usize) -> String {
        let timeline_limit = timeline_limit.max(1);
        serde_json::json!({
            "room": {
                "rooms": [room_id],
                "timeline": {
                    "limit": timeline_limit
                }
            }
        })
        .to_string()
    }

    async fn log_e2ee_diagnostics(&self, client: &MatrixSdkClient) {
        match client.encryption().get_own_device().await {
            Ok(Some(device)) => {
                if device.is_verified() {
                    tracing::info!(
                        "Matrix device '{}' is verified for E2EE.",
                        device.device_id()
                    );
                } else {
                    tracing::warn!(
                        "Matrix device '{}' is not verified. Some clients may label bot messages as unverified until you sign/verify this device from a trusted session.",
                        device.device_id()
                    );
                }
            }
            Ok(None) => {
                tracing::warn!(
                    "Matrix own-device metadata is unavailable; verify/signing status cannot be determined."
                );
            }
            Err(error) => {
                tracing::warn!("Matrix own-device verification check failed: {error}");
            }
        }

        if client.encryption().backups().are_enabled().await {
            tracing::info!("Matrix room-key backup is enabled for this device.");
        } else {
            tracing::warn!(
                "Matrix room-key backup is not enabled for this device; `matrix_sdk_crypto::backups` warnings about missing backup keys may appear until recovery is configured."
            );
        }
    }
}

/// Handle an incoming SAS verification request: accept the request, wait for
/// the other side to start SAS, accept it, log emojis, and auto-confirm.
///
/// Flow: receive request → accept → wait for other side to start SAS →
/// accept SAS → keys exchanged (emojis shown) → confirm → done.
async fn handle_verification_request(
    client: MatrixSdkClient,
    sender: OwnedUserId,
    flow_id: String,
) {
    let Some(request) = client
        .encryption()
        .get_verification_request(&sender, &flow_id)
        .await
    else {
        tracing::warn!("Matrix verification request not found for flow {flow_id}");
        return;
    };

    tracing::info!(
        "Matrix verification request received from {}, accepting...",
        sender
    );

    // Force a fresh device key query from the server for the sender.
    // The crypto store may have stale keys from a previous session, causing
    // MAC verification to fail even though emojis match. This ensures the
    // SAS captures the sender's current Ed25519/Curve25519 device keys.
    match client.encryption().request_user_identity(&sender).await {
        Ok(_) => {
            tracing::debug!("Matrix verification: refreshed device keys for {}", sender);
        }
        Err(error) => {
            tracing::warn!(
                "Matrix verification: failed to refresh device keys for {}: {error}",
                sender
            );
        }
    }

    if let Err(error) = request.accept().await {
        tracing::warn!("Matrix verification accept failed: {error}");
        return;
    }

    // Wait for the request to reach Ready, then wait for the other side to
    // start the SAS flow (Transitioned state). The bot is the responder —
    // it should NOT call start_sas(), as the initiating client (Element)
    // will send m.key.verification.start.
    let mut changes = request.changes();
    let sas = loop {
        match request.state() {
            VerificationRequestState::Transitioned { verification } => {
                if let Some(sas) = verification.sas() {
                    break sas;
                }
                tracing::warn!("Matrix verification transitioned but not to SAS");
                return;
            }
            VerificationRequestState::Done | VerificationRequestState::Cancelled(_) => {
                tracing::warn!("Matrix verification request ended before SAS started");
                return;
            }
            _ => {}
        }
        if changes.next().await.is_none() {
            return;
        }
    };

    // Log the other device's keys for diagnostics — if these don't match
    // what the other side actually has, MAC verification will fail.
    let other_dev = sas.other_device();
    tracing::info!(
        "Matrix SAS verification initiated by {} (device {}), accepting SAS...",
        sender,
        other_dev.device_id()
    );
    tracing::debug!(
        "Matrix SAS: other device ed25519={:?} curve25519={:?}",
        other_dev.ed25519_key().map(|k| k.to_base64()),
        other_dev.curve25519_key().map(|k| k.to_base64()),
    );

    // Subscribe to SAS state changes BEFORE calling accept, so we don't
    // miss any state transitions that fire immediately after accept.
    let mut sas_changes = sas.changes();

    // Accept the SAS — sends m.key.verification.accept back to the initiator.
    if let Err(error) = sas.accept().await {
        tracing::warn!("Matrix SAS accept failed: {error}");
        return;
    }

    // Listen for SAS state changes: KeysExchanged → confirm → Done.
    while let Some(state) = sas_changes.next().await {
        tracing::debug!("Matrix SAS state change: {:?}", state);
        match state {
            SasState::KeysExchanged { emojis, decimals } => {
                if let Some(emojis) = emojis {
                    let emoji_display: Vec<String> = emojis
                        .emojis
                        .iter()
                        .map(|e| format!("{} ({})", e.symbol, e.description))
                        .collect();
                    tracing::info!(
                        "Matrix SAS verification emojis with {} — confirm these match in your client:\n  {}",
                        sender,
                        emoji_display.join("  ")
                    );
                } else {
                    let (d1, d2, d3) = decimals;
                    tracing::info!(
                        "Matrix SAS verification decimals with {}: {} {} {}",
                        sender,
                        d1,
                        d2,
                        d3
                    );
                }

                // Design decision: auto-confirm SAS for bot accounts.
                // Bots cannot perform interactive emoji comparison with a
                // human operator. Since the verification request is only
                // processed for allowlisted senders (checked earlier in the
                // listen() handler), auto-confirming is the intended behavior.
                // The emojis are logged above so an operator can audit them
                // after the fact if needed.

                // Brief delay before auto-confirming: let the sync loop
                // fully process the key exchange on both sides before
                // sending our MAC. Without this, Element may receive the
                // MAC before it has finished processing the key material.
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;

                tracing::info!("Matrix SAS: auto-confirming emojis on bot side (bot accounts cannot do interactive verification)...");
                if let Err(error) = sas.confirm().await {
                    tracing::warn!("Matrix SAS verification confirm failed: {error}");
                    return;
                }
            }
            SasState::Done { .. } => {
                tracing::info!(
                    "Matrix SAS verification with {} completed successfully. Device {} is now verified.",
                    sender,
                    sas.other_device().device_id()
                );
                return;
            }
            SasState::Cancelled(info) => {
                tracing::warn!(
                    "Matrix SAS verification with {} cancelled: {}",
                    sender,
                    info.reason()
                );
                return;
            }
            _ => {}
        }
    }
}

#[async_trait]
impl Channel for MatrixChannel {
    fn name(&self) -> &str {
        "matrix"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let raw_content = super::strip_tool_call_tags(&message.content);
        let (after_reactions, reaction_markers) = parse_matrix_reaction_markers(&raw_content);
        let (after_locations, location_markers) = parse_matrix_location_markers(&after_reactions);
        let (cleaned_content, parsed_attachments) =
            parse_matrix_attachment_markers(&after_locations);

        let client = self.matrix_client().await?;
        let target_room_id = self.target_room_id().await?;
        let target_room: OwnedRoomId = target_room_id.parse()?;

        let mut room = client.get_room(&target_room);
        if room.is_none() {
            let _ = client.sync_once(SyncSettings::new()).await;
            room = client.get_room(&target_room);
        }

        let Some(room) = room else {
            anyhow::bail!("Matrix room '{}' not found in joined rooms", target_room_id);
        };

        if room.state() != RoomState::Joined {
            anyhow::bail!("Matrix room '{}' is not in joined state", target_room_id);
        }

        // Send reaction markers — the LLM decided to react to a message.
        for reaction in &reaction_markers {
            if let Ok(eid) = reaction.event_id.parse::<OwnedEventId>() {
                let content =
                    ReactionEventContent::new(Annotation::new(eid, reaction.emoji.clone()));
                if let Err(error) = room.send(content).await {
                    tracing::warn!("Matrix reaction send failed: {error}");
                }
            }
        }

        // Send location markers.
        for location in &location_markers {
            let content = RoomMessageEventContent::new(MessageType::Location(
                LocationMessageEventContent::new(
                    location.description.clone(),
                    location.geo_uri.clone(),
                ),
            ));
            if let Err(error) = room.send(content).await {
                tracing::warn!("Matrix location send failed: {error}");
            }
        }

        // Send each attachment via room.send_attachment() which auto-encrypts
        // media for E2EE rooms (unlike client.media().upload() which uploads plain).
        for attachment in &parsed_attachments {
            let target = attachment.target.trim();
            let path = Path::new(target);

            if !path.exists() || !path.is_file() {
                tracing::warn!(
                    "Matrix outgoing attachment not found or not a file: {}",
                    target
                );
                continue;
            }

            // Security: restrict uploads to the workspace/media directory to
            // prevent [IMAGE:path] markers in bot responses from exfiltrating
            // arbitrary host files via Matrix media uploads.
            if let Some(ref zdir) = self.zeroclaw_dir {
                let allowed_dir = zdir.join("workspace");
                match path.canonicalize() {
                    Ok(canonical) => {
                        if !canonical.starts_with(&allowed_dir) {
                            tracing::warn!(
                                "Matrix outgoing attachment path '{}' is outside workspace directory '{}' — refusing upload",
                                canonical.display(),
                                allowed_dir.display()
                            );
                            continue;
                        }
                    }
                    Err(err) => {
                        tracing::warn!(
                            "Matrix outgoing attachment path '{}' could not be canonicalized: {err} — refusing upload",
                            target
                        );
                        continue;
                    }
                }
            } else {
                tracing::warn!(
                    "Matrix: no zeroclaw_dir configured — cannot validate attachment path '{}', refusing upload",
                    target
                );
                continue;
            }

            let bytes = match tokio::fs::read(path).await {
                Ok(b) => b,
                Err(error) => {
                    tracing::warn!(
                        "Matrix failed to read outgoing attachment '{}': {error}",
                        path.display()
                    );
                    continue;
                }
            };

            let mime = mime_guess::from_path(path)
                .first()
                .unwrap_or(mime_guess::mime::APPLICATION_OCTET_STREAM);

            let filename = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("attachment.bin")
                .to_string();

            let config = match attachment.kind {
                MatrixOutgoingAttachmentKind::Voice => {
                    AttachmentConfig::new().info(AttachmentInfo::Voice(BaseAudioInfo {
                        duration: None,
                        size: None,
                        waveform: None,
                    }))
                }
                MatrixOutgoingAttachmentKind::Audio => {
                    AttachmentConfig::new().info(AttachmentInfo::Audio(BaseAudioInfo {
                        duration: None,
                        size: None,
                        waveform: None,
                    }))
                }
                _ => AttachmentConfig::new(),
            };

            match room.send_attachment(&filename, &mime, bytes, config).await {
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!("Matrix media send failed for '{}': {error}", path.display());
                    continue;
                }
            }
        }

        // Send remaining text (if any) after attachments/reactions.
        if !cleaned_content.is_empty() {
            room.send(RoomMessageEventContent::text_markdown(&cleaned_content))
                .await?;
        } else if parsed_attachments.is_empty()
            && reaction_markers.is_empty()
            && location_markers.is_empty()
        {
            // No markers were found — send original content as text.
            room.send(RoomMessageEventContent::text_markdown(&raw_content))
                .await?;
        }

        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        // Initialize SDK client first — this may login with password and
        // persist session.json, which is needed for subsequent HTTP calls.
        let client = self.matrix_client().await?;

        let target_room_id = self.target_room_id().await?;
        self.ensure_room_supported(&target_room_id).await?;

        let target_room: OwnedRoomId = target_room_id.parse()?;
        let my_user_id: OwnedUserId = match self.get_my_user_id().await {
            Ok(user_id) => user_id.parse()?,
            Err(error) => {
                if let Some(hinted) = self.session_owner_hint.as_ref() {
                    tracing::warn!(
                        "Matrix whoami failed while resolving listener user_id; using configured user_id hint: {error}"
                    );
                    hinted.parse()?
                } else {
                    return Err(error);
                }
            }
        };

        self.log_e2ee_diagnostics(&client).await;

        // Force a fresh device key query for allowed users BEFORE the initial
        // sync. The SDK captures device data from the local store when it first
        // processes a verification request event. If the store has stale keys
        // (e.g. from a previous session), SAS MAC verification will fail even
        // though emojis match. Querying keys now ensures the store is up-to-date
        // before any verification events are processed during sync.
        for user_str in &self.allowed_users {
            if let Ok(user_id) = <&matrix_sdk::ruma::UserId>::try_from(user_str.as_str()) {
                match client.encryption().request_user_identity(user_id).await {
                    Ok(_) => {
                        tracing::debug!("Matrix: refreshed device keys for {user_str}");
                    }
                    Err(error) => {
                        tracing::debug!(
                            "Matrix: could not refresh device keys for {user_str}: {error}"
                        );
                    }
                }
            }
        }

        // Register the verification handler BEFORE the initial sync so that
        // verification requests arriving during the first sync are handled.
        let verification_client = client.clone();
        let allowed_users_for_verification = self.allowed_users.clone();
        client.add_event_handler(move |event: OriginalSyncRoomMessageEvent, _room: Room| {
            let ver_client = verification_client.clone();
            let allowed_users = allowed_users_for_verification.clone();

            async move {
                if !matches!(&event.content.msgtype, MessageType::VerificationRequest(_)) {
                    return;
                }

                let sender = event.sender.to_string();
                if !MatrixChannel::is_sender_allowed(&allowed_users, &sender) {
                    tracing::warn!(
                        "Matrix verification request from non-allowed user {sender}, ignoring"
                    );
                    return;
                }

                let flow_id = event.event_id.to_string();
                let sender_id = event.sender.clone();

                tokio::spawn(async move {
                    handle_verification_request(ver_client, sender_id, flow_id).await;
                });
            }
        });

        let _ = client.sync_once(SyncSettings::new()).await;

        tracing::info!(
            "Matrix channel listening on room {} (configured as {})...",
            target_room_id,
            self.room_id
        );

        let recent_event_cache = Arc::new(Mutex::new((
            std::collections::VecDeque::new(),
            std::collections::HashSet::new(),
        )));

        let tx_handler = tx.clone();
        let target_room_for_handler = target_room.clone();
        let my_user_id_for_handler = my_user_id.clone();
        let allowed_users_for_handler = self.allowed_users.clone();
        let dedupe_for_handler = Arc::clone(&recent_event_cache);
        let media_save_dir_for_handler = self.media_save_dir();
        let transcription_for_handler = self.transcription.clone();
        let voice_cache_for_handler = Arc::clone(&self.voice_transcriptions);

        client.add_event_handler(move |event: OriginalSyncRoomMessageEvent, room: Room| {
            let tx = tx_handler.clone();
            let target_room = target_room_for_handler.clone();
            let my_user_id = my_user_id_for_handler.clone();
            let allowed_users = allowed_users_for_handler.clone();
            let dedupe = Arc::clone(&dedupe_for_handler);
            let media_save_dir = media_save_dir_for_handler.clone();
            let transcription_config = transcription_for_handler.clone();
            let voice_cache = Arc::clone(&voice_cache_for_handler);

            async move {
                if room.room_id().as_str() != target_room.as_str() {
                    return;
                }

                if event.sender == my_user_id {
                    return;
                }

                let sender = event.sender.to_string();
                if !MatrixChannel::is_sender_allowed(&allowed_users, &sender) {
                    return;
                }

                // Deduplicate early — before downloading media or transcribing
                // to avoid repeated I/O and billable external calls on redelivery.
                //
                // Design tradeoff: caching the event_id before processing means
                // that if media download or transcription fails, the event won't
                // be retried. This is acceptable because Matrix sync does not
                // redeliver events on handler failure — only on sync gaps (where
                // the SDK replays from the sync token). Early dedupe prevents
                // duplicate media downloads and duplicate billable transcription
                // calls, which outweighs the theoretical loss of retry capability.
                let event_id = event.event_id.to_string();
                {
                    let mut guard = dedupe.lock().await;
                    let (recent_order, recent_lookup) = &mut *guard;
                    if MatrixChannel::cache_event_id(&event_id, recent_order, recent_lookup) {
                        return;
                    }
                }

                let body = match &event.content.msgtype {
                    MessageType::Text(content) => content.body.clone(),
                    MessageType::Notice(content) => content.body.clone(),
                    MessageType::Image(content) => {
                        let Some(ref save_dir) = media_save_dir else {
                            tracing::warn!("Matrix image received but no zeroclaw_dir configured for media storage");
                            return;
                        };
                        let filename = content.filename().to_string();
                        let source = content.source.clone();
                        let size_hint = content.info.as_ref().and_then(|i| i.size.map(u64::from));
                        let sdk_client = room.client();
                        match download_and_save_matrix_media(&sdk_client, &source, &filename, save_dir, size_hint).await {
                            Ok(local_path) => {
                                if is_image_extension(&local_path) {
                                    format!("[IMAGE:{}]", local_path.display())
                                } else {
                                    format!("[Document: {}] {}", filename, local_path.display())
                                }
                            }
                            Err(error) => {
                                tracing::warn!("Matrix image download failed: {error}");
                                return;
                            }
                        }
                    }
                    MessageType::File(content) => {
                        let Some(ref save_dir) = media_save_dir else {
                            tracing::warn!("Matrix file received but no zeroclaw_dir configured for media storage");
                            return;
                        };
                        let filename = content.filename().to_string();
                        let source = content.source.clone();
                        let size_hint = content.info.as_ref().and_then(|i| i.size.map(u64::from));
                        let sdk_client = room.client();
                        match download_and_save_matrix_media(&sdk_client, &source, &filename, save_dir, size_hint).await {
                            Ok(local_path) => {
                                format!("[Document: {}] {}", filename, local_path.display())
                            }
                            Err(error) => {
                                tracing::warn!("Matrix file download failed: {error}");
                                return;
                            }
                        }
                    }
                    MessageType::Audio(content) => {
                        let filename = content.filename().to_string();
                        let source = content.source.clone();
                        let size_hint = content.info.as_ref().and_then(|i| i.size.map(u64::from));
                        let sdk_client = room.client();

                        // Pre-download size check for audio.
                        if let Some(size) = size_hint {
                            if size as usize > MATRIX_MAX_MEDIA_DOWNLOAD_BYTES {
                                tracing::warn!("Matrix audio exceeds size limit ({size} bytes); skipping");
                                return;
                            }
                        }

                        // Try transcription first if enabled.
                        if let Some(ref config) = transcription_config {
                            let request = MediaRequestParameters {
                                source: source.clone(),
                                format: MediaFormat::File,
                            };
                            match sdk_client.media().get_media_content(&request, false).await {
                                Ok(audio_data) => {
                                    match super::transcription::transcribe_audio(audio_data, &filename, config).await {
                                        Ok(text) => {
                                            let event_id = event.event_id.to_string();
                                            let mut cache = voice_cache.lock().await;
                                            if cache.len() >= 100 {
                                                cache.clear();
                                            }
                                            cache.insert(event_id, text.clone());
                                            format!("[Voice] {text}")
                                        }
                                        Err(error) => {
                                            tracing::debug!("Matrix audio transcription failed, falling back to file save: {error}");
                                            // Fall through to file save below.
                                            let Some(ref save_dir) = media_save_dir else {
                                                tracing::warn!("Matrix audio received but no zeroclaw_dir configured for media storage");
                                                return;
                                            };
                                            match download_and_save_matrix_media(&sdk_client, &source, &filename, save_dir, size_hint).await {
                                                Ok(local_path) => {
                                                    format!("[Document: {}] {}", filename, local_path.display())
                                                }
                                                Err(dl_error) => {
                                                    tracing::warn!("Matrix audio download failed: {dl_error}");
                                                    return;
                                                }
                                            }
                                        }
                                    }
                                }
                                Err(error) => {
                                    tracing::warn!("Matrix audio media fetch failed: {error}");
                                    return;
                                }
                            }
                        } else {
                            // No transcription — save as document.
                            let Some(ref save_dir) = media_save_dir else {
                                tracing::warn!("Matrix audio received but no zeroclaw_dir configured for media storage");
                                return;
                            };
                            match download_and_save_matrix_media(&sdk_client, &source, &filename, save_dir, size_hint).await {
                                Ok(local_path) => {
                                    format!("[Document: {}] {}", filename, local_path.display())
                                }
                                Err(error) => {
                                    tracing::warn!("Matrix audio download failed: {error}");
                                    return;
                                }
                            }
                        }
                    }
                    MessageType::Location(content) => {
                        format!("[Location: {}] {}", content.geo_uri, content.body)
                    }
                    _ => return,
                };

                // Prepend reply context if this message is a reply to another.
                // Fetch the original message text so the LLM has context even
                // when the replied-to message is outside the current conversation.
                let body = if let Some(matrix_sdk::ruma::events::room::message::Relation::Reply { in_reply_to }) = &event.content.relates_to {
                    let target_eid = &in_reply_to.event_id;
                    // Check voice transcription cache first.
                    let cached_voice = {
                        let cache = voice_cache.lock().await;
                        cache.get(target_eid.as_str()).cloned()
                    };
                    let original_text = if let Some(transcript) = cached_voice {
                        transcript
                    } else {
                        // Fetch from server.
                        match room.event(target_eid, None).await {
                            Ok(timeline_event) => {
                                serde_json::to_value(timeline_event.raw())
                                    .ok()
                                    .and_then(|v| {
                                        v.get("content")?
                                            .get("body")?
                                            .as_str()
                                            .map(|s| s.to_string())
                                    })
                                    .unwrap_or_default()
                            }
                            Err(_) => String::new(),
                        }
                    };
                    if original_text.is_empty() {
                        format!("[Reply to {}] {}", target_eid, body)
                    } else {
                        let preview = if original_text.chars().count() > 200 {
                            format!("{}…", original_text.chars().take(200).collect::<String>())
                        } else {
                            original_text
                        };
                        format!("[Reply to {}: \"{}\"] {}", target_eid, preview, body)
                    }
                } else {
                    body
                };

                if !MatrixChannel::has_non_empty_body(&body) {
                    return;
                }

                // Include the event_id so the LLM can reference it in
                // [REACT:emoji:event_id] markers without guessing.
                let body = format!("[msg_id:{}] {}", event_id, body);

                // Mark message as read + start typing indicator.
                if let Ok(eid) = event_id.parse::<OwnedEventId>() {
                    use matrix_sdk::ruma::api::client::receipt::create_receipt;
                    use matrix_sdk::ruma::events::receipt::ReceiptThread;
                    let _ = room
                        .send_single_receipt(
                            create_receipt::v3::ReceiptType::Read,
                            ReceiptThread::Unthreaded,
                            eid,
                        )
                        .await;
                }
                let msg = ChannelMessage {
                    id: event_id,
                    sender: sender.clone(),
                    reply_target: sender,
                    content: body,
                    channel: "matrix".to_string(),
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    thread_ts: None,
                };

                let _ = tx.send(msg).await;
            }
        });

        // Reaction event handler — delivers emoji reactions to the agent.
        let tx_reaction = tx.clone();
        let target_room_for_reaction = target_room.clone();
        let my_user_id_for_reaction = my_user_id.clone();
        let allowed_users_for_reaction = self.allowed_users.clone();
        let dedupe_for_reaction = Arc::clone(&recent_event_cache);

        client.add_event_handler(move |event: OriginalSyncReactionEvent, room: Room| {
            let tx = tx_reaction.clone();
            let target_room = target_room_for_reaction.clone();
            let my_user_id = my_user_id_for_reaction.clone();
            let allowed_users = allowed_users_for_reaction.clone();
            let dedupe = Arc::clone(&dedupe_for_reaction);

            async move {
                if room.room_id().as_str() != target_room.as_str() {
                    return;
                }
                if event.sender == my_user_id {
                    return;
                }
                let sender = event.sender.to_string();
                if !MatrixChannel::is_sender_allowed(&allowed_users, &sender) {
                    return;
                }

                let event_id = event.event_id.to_string();
                {
                    let mut guard = dedupe.lock().await;
                    let (recent_order, recent_lookup) = &mut *guard;
                    if MatrixChannel::cache_event_id(&event_id, recent_order, recent_lookup) {
                        return;
                    }
                }

                let emoji = &event.content.relates_to.key;
                let target_event_id = &event.content.relates_to.event_id;

                // Fetch the original message to provide context.
                let (original_author, original_text) = match room.event(target_event_id, None).await
                {
                    Ok(timeline_event) => {
                        let raw = timeline_event.raw();
                        match raw.deserialize() {
                            Ok(any_event) => {
                                let author = any_event.sender().to_string();
                                // Extract body from the raw JSON content.
                                let text = serde_json::to_value(raw)
                                    .ok()
                                    .and_then(|v| {
                                        v.get("content")?
                                            .get("body")?
                                            .as_str()
                                            .map(|s| s.to_string())
                                    })
                                    .unwrap_or_default();
                                (author, text)
                            }
                            Err(_) => (String::new(), String::new()),
                        }
                    }
                    Err(_) => (String::new(), String::new()),
                };

                let is_own_message = original_author == my_user_id.as_str();
                let author_label = if is_own_message {
                    "your message"
                } else {
                    "their message"
                };
                let preview = if original_text.chars().count() > 100 {
                    format!("{}…", original_text.chars().take(100).collect::<String>())
                } else {
                    original_text.clone()
                };
                let body = format!("[Reaction: {emoji} on {author_label}: \"{preview}\"]");

                let msg = ChannelMessage {
                    id: event_id,
                    sender: sender.clone(),
                    reply_target: sender,
                    content: body,
                    channel: "matrix".to_string(),
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    thread_ts: None,
                };

                let _ = tx.send(msg).await;
            }
        });

        let sync_settings = SyncSettings::new().timeout(std::time::Duration::from_secs(30));
        client
            .sync_with_result_callback(sync_settings, |sync_result| {
                let tx = tx.clone();
                async move {
                    if tx.is_closed() {
                        return Ok::<LoopCtrl, matrix_sdk::Error>(LoopCtrl::Break);
                    }

                    if let Err(error) = sync_result {
                        tracing::warn!("Matrix sync error: {error}, retrying...");
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    }

                    Ok::<LoopCtrl, matrix_sdk::Error>(LoopCtrl::Continue)
                }
            })
            .await?;

        Ok(())
    }

    async fn start_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        let client = self.matrix_client().await?;
        let target_room_id = self.target_room_id().await?;
        let target_room: OwnedRoomId = target_room_id.parse()?;
        if let Some(room) = client.get_room(&target_room) {
            room.typing_notice(true).await?;
        }
        Ok(())
    }

    async fn stop_typing(&self, _recipient: &str) -> anyhow::Result<()> {
        let client = self.matrix_client().await?;
        let target_room_id = self.target_room_id().await?;
        let target_room: OwnedRoomId = target_room_id.parse()?;
        if let Some(room) = client.get_room(&target_room) {
            room.typing_notice(false).await?;
        }
        Ok(())
    }

    async fn health_check(&self) -> bool {
        if self.matrix_client().await.is_err() {
            return false;
        }

        let Ok(room_id) = self.target_room_id().await else {
            return false;
        };

        if self.ensure_room_supported(&room_id).await.is_err() {
            return false;
        }

        self.matrix_client().await.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_channel() -> MatrixChannel {
        MatrixChannel::new(
            "https://matrix.org".to_string(),
            Some("syt_test_token".to_string()),
            "!room:matrix.org".to_string(),
            vec!["@user:matrix.org".to_string()],
        )
    }

    fn make_channel_with_zeroclaw_dir() -> MatrixChannel {
        MatrixChannel::new_with_session_hint_and_zeroclaw_dir(
            "https://matrix.org".to_string(),
            Some("syt_test_token".to_string()),
            "!room:matrix.org".to_string(),
            vec!["@user:matrix.org".to_string()],
            None,
            None,
            Some(PathBuf::from("/tmp/zeroclaw")),
        )
    }

    #[test]
    fn creates_with_correct_fields() {
        let ch = make_channel();
        assert_eq!(ch.homeserver, "https://matrix.org");
        assert_eq!(ch.access_token.as_deref(), Some("syt_test_token"));
        assert_eq!(ch.room_id, "!room:matrix.org");
        assert_eq!(ch.allowed_users.len(), 1);
    }

    #[test]
    fn strips_trailing_slash() {
        let ch = MatrixChannel::new(
            "https://matrix.org/".to_string(),
            Some("tok".to_string()),
            "!r:m".to_string(),
            vec![],
        );
        assert_eq!(ch.homeserver, "https://matrix.org");
    }

    #[test]
    fn no_trailing_slash_unchanged() {
        let ch = MatrixChannel::new(
            "https://matrix.org".to_string(),
            Some("tok".to_string()),
            "!r:m".to_string(),
            vec![],
        );
        assert_eq!(ch.homeserver, "https://matrix.org");
    }

    #[test]
    fn multiple_trailing_slashes_strip_all() {
        let ch = MatrixChannel::new(
            "https://matrix.org//".to_string(),
            Some("tok".to_string()),
            "!r:m".to_string(),
            vec![],
        );
        assert_eq!(ch.homeserver, "https://matrix.org");
    }

    #[test]
    fn trims_access_token() {
        let ch = MatrixChannel::new(
            "https://matrix.org".to_string(),
            Some("  syt_test_token  ".to_string()),
            "!r:m".to_string(),
            vec![],
        );
        assert_eq!(ch.access_token.as_deref(), Some("syt_test_token"));
    }

    #[test]
    fn session_hints_are_normalized() {
        let ch = MatrixChannel::new_with_session_hint(
            "https://matrix.org".to_string(),
            Some("tok".to_string()),
            "!r:m".to_string(),
            vec![],
            Some("  @bot:matrix.org ".to_string()),
            Some("  DEVICE123  ".to_string()),
        );

        assert_eq!(ch.session_owner_hint.as_deref(), Some("@bot:matrix.org"));
        assert_eq!(ch.session_device_id_hint.as_deref(), Some("DEVICE123"));
    }

    #[test]
    fn empty_session_hints_are_ignored() {
        let ch = MatrixChannel::new_with_session_hint(
            "https://matrix.org".to_string(),
            Some("tok".to_string()),
            "!r:m".to_string(),
            vec![],
            Some("   ".to_string()),
            Some(String::new()),
        );

        assert!(ch.session_owner_hint.is_none());
        assert!(ch.session_device_id_hint.is_none());
    }

    #[test]
    fn matrix_store_dir_is_derived_from_zeroclaw_dir() {
        let ch = MatrixChannel::new_with_session_hint_and_zeroclaw_dir(
            "https://matrix.org".to_string(),
            Some("tok".to_string()),
            "!r:m".to_string(),
            vec![],
            None,
            None,
            Some(PathBuf::from("/tmp/zeroclaw")),
        );

        assert_eq!(
            ch.matrix_store_dir(),
            Some(PathBuf::from("/tmp/zeroclaw/state/matrix"))
        );
    }

    #[test]
    fn matrix_store_dir_absent_without_zeroclaw_dir() {
        let ch = MatrixChannel::new_with_session_hint(
            "https://matrix.org".to_string(),
            Some("tok".to_string()),
            "!r:m".to_string(),
            vec![],
            None,
            None,
        );

        assert!(ch.matrix_store_dir().is_none());
    }

    #[test]
    fn encode_path_segment_encodes_room_refs() {
        assert_eq!(
            MatrixChannel::encode_path_segment("#ops:matrix.example.com"),
            "%23ops%3Amatrix.example.com"
        );
        assert_eq!(
            MatrixChannel::encode_path_segment("!room:matrix.example.com"),
            "%21room%3Amatrix.example.com"
        );
    }

    #[test]
    fn supported_message_type_detection() {
        assert!(MatrixChannel::is_supported_message_type("m.text"));
        assert!(MatrixChannel::is_supported_message_type("m.notice"));
        assert!(MatrixChannel::is_supported_message_type("m.image"));
        assert!(MatrixChannel::is_supported_message_type("m.file"));
        assert!(MatrixChannel::is_supported_message_type("m.audio"));
        assert!(!MatrixChannel::is_supported_message_type("m.video"));
        assert!(!MatrixChannel::is_supported_message_type("m.location"));
    }

    #[test]
    fn body_presence_detection() {
        assert!(MatrixChannel::has_non_empty_body("hello"));
        assert!(MatrixChannel::has_non_empty_body("  hello  "));
        assert!(!MatrixChannel::has_non_empty_body(""));
        assert!(!MatrixChannel::has_non_empty_body("   \n\t  "));
    }

    #[test]
    fn send_content_uses_markdown_formatting() {
        let content = RoomMessageEventContent::text_markdown("**hello**");
        let value = serde_json::to_value(content).unwrap();

        assert_eq!(value["msgtype"], "m.text");
        assert_eq!(value["body"], "**hello**");
        assert_eq!(value["format"], "org.matrix.custom.html");
        assert!(value["formatted_body"]
            .as_str()
            .unwrap_or_default()
            .contains("<strong>hello</strong>"));
    }

    #[test]
    fn sync_filter_for_room_targets_requested_room() {
        let filter = MatrixChannel::sync_filter_for_room("!room:matrix.org", 0);
        let value: serde_json::Value = serde_json::from_str(&filter).unwrap();

        assert_eq!(value["room"]["rooms"][0], "!room:matrix.org");
        assert_eq!(value["room"]["timeline"]["limit"], 1);
    }

    #[test]
    fn event_id_cache_deduplicates_and_evicts_old_entries() {
        let mut recent_order = std::collections::VecDeque::new();
        let mut recent_lookup = std::collections::HashSet::new();

        assert!(!MatrixChannel::cache_event_id(
            "$first:event",
            &mut recent_order,
            &mut recent_lookup
        ));
        assert!(MatrixChannel::cache_event_id(
            "$first:event",
            &mut recent_order,
            &mut recent_lookup
        ));

        for i in 0..2050 {
            let event_id = format!("$event-{i}:matrix");
            MatrixChannel::cache_event_id(&event_id, &mut recent_order, &mut recent_lookup);
        }

        assert!(!MatrixChannel::cache_event_id(
            "$first:event",
            &mut recent_order,
            &mut recent_lookup
        ));
    }

    #[test]
    fn trims_room_id_and_allowed_users() {
        let ch = MatrixChannel::new(
            "https://matrix.org".to_string(),
            Some("tok".to_string()),
            "  !room:matrix.org  ".to_string(),
            vec![
                "  @user:matrix.org  ".to_string(),
                "   ".to_string(),
                "@other:matrix.org".to_string(),
            ],
        );

        assert_eq!(ch.room_id, "!room:matrix.org");
        assert_eq!(ch.allowed_users.len(), 2);
        assert!(ch.allowed_users.contains(&"@user:matrix.org".to_string()));
        assert!(ch.allowed_users.contains(&"@other:matrix.org".to_string()));
    }

    #[test]
    fn wildcard_allows_anyone() {
        let ch = MatrixChannel::new(
            "https://m.org".to_string(),
            Some("tok".to_string()),
            "!r:m".to_string(),
            vec!["*".to_string()],
        );
        assert!(ch.is_user_allowed("@anyone:matrix.org"));
        assert!(ch.is_user_allowed("@hacker:evil.org"));
    }

    #[test]
    fn specific_user_allowed() {
        let ch = make_channel();
        assert!(ch.is_user_allowed("@user:matrix.org"));
    }

    #[test]
    fn unknown_user_denied() {
        let ch = make_channel();
        assert!(!ch.is_user_allowed("@stranger:matrix.org"));
        assert!(!ch.is_user_allowed("@evil:hacker.org"));
    }

    #[test]
    fn user_case_insensitive() {
        let ch = MatrixChannel::new(
            "https://m.org".to_string(),
            Some("tok".to_string()),
            "!r:m".to_string(),
            vec!["@User:Matrix.org".to_string()],
        );
        assert!(ch.is_user_allowed("@user:matrix.org"));
        assert!(ch.is_user_allowed("@USER:MATRIX.ORG"));
    }

    #[test]
    fn empty_allowlist_denies_all() {
        let ch = MatrixChannel::new(
            "https://m.org".to_string(),
            Some("tok".to_string()),
            "!r:m".to_string(),
            vec![],
        );
        assert!(!ch.is_user_allowed("@anyone:matrix.org"));
    }

    #[test]
    fn name_returns_matrix() {
        let ch = make_channel();
        assert_eq!(ch.name(), "matrix");
    }

    #[test]
    fn sync_response_deserializes_empty() {
        let json = r#"{"next_batch":"s123","rooms":{"join":{}}}"#;
        let resp: SyncResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.next_batch, "s123");
        assert!(resp.rooms.join.is_empty());
    }

    #[test]
    fn sync_response_deserializes_with_events() {
        let json = r#"{
            "next_batch": "s456",
            "rooms": {
                "join": {
                    "!room:matrix.org": {
                        "timeline": {
                            "events": [
                                {
                                    "type": "m.room.message",
                                    "event_id": "$event:matrix.org",
                                    "sender": "@user:matrix.org",
                                    "content": {
                                        "msgtype": "m.text",
                                        "body": "Hello!"
                                    }
                                }
                            ]
                        }
                    }
                }
            }
        }"#;
        let resp: SyncResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.next_batch, "s456");
        let room = resp.rooms.join.get("!room:matrix.org").unwrap();
        assert_eq!(room.timeline.events.len(), 1);
        assert_eq!(room.timeline.events[0].sender, "@user:matrix.org");
        assert_eq!(
            room.timeline.events[0].event_id.as_deref(),
            Some("$event:matrix.org")
        );
        assert_eq!(
            room.timeline.events[0].content.body.as_deref(),
            Some("Hello!")
        );
        assert_eq!(
            room.timeline.events[0].content.msgtype.as_deref(),
            Some("m.text")
        );
    }

    #[test]
    fn sync_response_ignores_non_text_events() {
        let json = r#"{
            "next_batch": "s789",
            "rooms": {
                "join": {
                    "!room:m": {
                        "timeline": {
                            "events": [
                                {
                                    "type": "m.room.member",
                                    "sender": "@user:m",
                                    "content": {}
                                }
                            ]
                        }
                    }
                }
            }
        }"#;
        let resp: SyncResponse = serde_json::from_str(json).unwrap();
        let room = resp.rooms.join.get("!room:m").unwrap();
        assert_eq!(room.timeline.events[0].event_type, "m.room.member");
        assert!(room.timeline.events[0].content.body.is_none());
    }

    #[test]
    fn whoami_response_deserializes() {
        let json = r#"{"user_id":"@bot:matrix.org"}"#;
        let resp: WhoAmIResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.user_id, "@bot:matrix.org");
    }

    #[test]
    fn event_content_defaults() {
        let json = r#"{"type":"m.room.message","sender":"@u:m","content":{}}"#;
        let event: TimelineEvent = serde_json::from_str(json).unwrap();
        assert!(event.content.body.is_none());
        assert!(event.content.msgtype.is_none());
    }

    #[test]
    fn event_content_supports_notice_msgtype() {
        let json = r#"{
            "type":"m.room.message",
            "sender":"@u:m",
            "event_id":"$notice:m",
            "content":{"msgtype":"m.notice","body":"Heads up"}
        }"#;
        let event: TimelineEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.content.msgtype.as_deref(), Some("m.notice"));
        assert_eq!(event.content.body.as_deref(), Some("Heads up"));
        assert_eq!(event.event_id.as_deref(), Some("$notice:m"));
    }

    #[tokio::test]
    async fn invalid_room_reference_fails_fast() {
        let ch = MatrixChannel::new(
            "https://matrix.org".to_string(),
            Some("tok".to_string()),
            "room_without_prefix".to_string(),
            vec![],
        );

        let err = ch.resolve_room_id().await.unwrap_err();
        assert!(err
            .to_string()
            .contains("must start with '!' (room ID) or '#' (room alias)"));
    }

    #[tokio::test]
    async fn target_room_id_keeps_canonical_room_id_without_lookup() {
        let ch = MatrixChannel::new(
            "https://matrix.org".to_string(),
            Some("tok".to_string()),
            "!canonical:matrix.org".to_string(),
            vec![],
        );

        let room_id = ch.target_room_id().await.unwrap();
        assert_eq!(room_id, "!canonical:matrix.org");
    }

    #[tokio::test]
    async fn target_room_id_uses_cached_alias_resolution() {
        let ch = MatrixChannel::new(
            "https://matrix.org".to_string(),
            Some("tok".to_string()),
            "#ops:matrix.org".to_string(),
            vec![],
        );

        *ch.resolved_room_id_cache.write().await = Some("!cached:matrix.org".to_string());
        let room_id = ch.target_room_id().await.unwrap();
        assert_eq!(room_id, "!cached:matrix.org");
    }

    #[test]
    fn sync_response_missing_rooms_defaults() {
        let json = r#"{"next_batch":"s0"}"#;
        let resp: SyncResponse = serde_json::from_str(json).unwrap();
        assert!(resp.rooms.join.is_empty());
    }

    // --- Media support tests ---

    #[test]
    fn parse_matrix_attachment_markers_multiple() {
        let input = "Here is an image\n[IMAGE:/tmp/photo.png]\nAnd a file [FILE:/tmp/doc.pdf]";
        let (cleaned, attachments) = parse_matrix_attachment_markers(input);
        assert_eq!(cleaned, "Here is an image\n\nAnd a file");
        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0].kind, MatrixOutgoingAttachmentKind::Image);
        assert_eq!(attachments[0].target, "/tmp/photo.png");
        assert_eq!(attachments[1].kind, MatrixOutgoingAttachmentKind::File);
        assert_eq!(attachments[1].target, "/tmp/doc.pdf");
    }

    #[test]
    fn parse_matrix_attachment_markers_invalid_kept_as_text() {
        let input = "Hello [NOT_A_MARKER:foo] world";
        let (cleaned, attachments) = parse_matrix_attachment_markers(input);
        assert_eq!(cleaned, "Hello [NOT_A_MARKER:foo] world");
        assert!(attachments.is_empty());
    }

    #[test]
    fn parse_matrix_attachment_markers_empty_target() {
        let input = "[IMAGE:] some text";
        let (cleaned, attachments) = parse_matrix_attachment_markers(input);
        assert_eq!(cleaned, "[IMAGE:] some text");
        assert!(attachments.is_empty());
    }

    #[test]
    fn parse_matrix_attachment_markers_no_markers() {
        let input = "Just plain text";
        let (cleaned, attachments) = parse_matrix_attachment_markers(input);
        assert_eq!(cleaned, "Just plain text");
        assert!(attachments.is_empty());
    }

    #[test]
    fn outgoing_attachment_kind_from_marker_all_variants() {
        assert_eq!(
            MatrixOutgoingAttachmentKind::from_marker("IMAGE"),
            Some(MatrixOutgoingAttachmentKind::Image)
        );
        assert_eq!(
            MatrixOutgoingAttachmentKind::from_marker("photo"),
            Some(MatrixOutgoingAttachmentKind::Image)
        );
        assert_eq!(
            MatrixOutgoingAttachmentKind::from_marker("DOCUMENT"),
            Some(MatrixOutgoingAttachmentKind::File)
        );
        assert_eq!(
            MatrixOutgoingAttachmentKind::from_marker("file"),
            Some(MatrixOutgoingAttachmentKind::File)
        );
        assert_eq!(
            MatrixOutgoingAttachmentKind::from_marker("Audio"),
            Some(MatrixOutgoingAttachmentKind::Audio)
        );
        assert_eq!(
            MatrixOutgoingAttachmentKind::from_marker("VOICE"),
            Some(MatrixOutgoingAttachmentKind::Voice)
        );
        assert_eq!(MatrixOutgoingAttachmentKind::from_marker("unknown"), None);
    }

    #[test]
    fn is_image_extension_known() {
        assert!(is_image_extension(Path::new("photo.png")));
        assert!(is_image_extension(Path::new("photo.JPG")));
        assert!(is_image_extension(Path::new("photo.jpeg")));
        assert!(is_image_extension(Path::new("photo.gif")));
        assert!(is_image_extension(Path::new("photo.webp")));
        assert!(is_image_extension(Path::new("photo.bmp")));
    }

    #[test]
    fn is_image_extension_unknown() {
        assert!(!is_image_extension(Path::new("file.pdf")));
        assert!(!is_image_extension(Path::new("audio.ogg")));
        assert!(!is_image_extension(Path::new("noext")));
    }

    #[test]
    fn media_save_dir_derived_from_zeroclaw_dir() {
        let ch = make_channel_with_zeroclaw_dir();
        assert_eq!(
            ch.media_save_dir(),
            Some(PathBuf::from("/tmp/zeroclaw/workspace/matrix_files"))
        );
    }

    #[test]
    fn media_save_dir_absent_without_zeroclaw_dir() {
        let ch = make_channel();
        assert!(ch.media_save_dir().is_none());
    }

    #[test]
    fn with_transcription_enabled() {
        let config = crate::config::TranscriptionConfig {
            enabled: true,
            ..Default::default()
        };
        let ch = make_channel().with_transcription(config);
        assert!(ch.transcription.is_some());
    }

    #[test]
    fn with_transcription_disabled() {
        let config = crate::config::TranscriptionConfig {
            enabled: false,
            ..Default::default()
        };
        let ch = make_channel().with_transcription(config);
        assert!(ch.transcription.is_none());
    }
}
