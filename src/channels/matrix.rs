use crate::channels::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use futures_util::StreamExt;
use matrix_sdk::{
    authentication::matrix::MatrixSession,
    config::SyncSettings,
    encryption::verification::{SasState, VerificationRequestState},
    media::{MediaFormat, MediaRequestParameters},
    ruma::{
        events::reaction::ReactionEventContent,
        events::relation::{Annotation, InReplyTo, Thread},
        events::room::{
            message::{
                AudioMessageEventContent, FileMessageEventContent, ImageMessageEventContent,
                MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
            },
            MediaSource,
        },
        OwnedEventId, OwnedMxcUri, OwnedRoomId, OwnedUserId,
    },
    Client as MatrixSdkClient, LoopCtrl, Room, RoomState, SessionMeta, SessionTokens,
};
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, OnceCell, RwLock};

/// Maximum media download size (50 MB).
const MATRIX_MAX_MEDIA_DOWNLOAD_BYTES: usize = 50 * 1024 * 1024;

/// Matrix channel for Matrix Client-Server API.
/// Uses matrix-sdk for reliable sync and encrypted-room decryption.
#[derive(Clone)]
pub struct MatrixChannel {
    homeserver: String,
    access_token: String,
    room_id: String,
    allowed_users: Vec<String>,
    session_owner_hint: Option<String>,
    session_device_id_hint: Option<String>,
    zeroclaw_dir: Option<PathBuf>,
    resolved_room_id_cache: Arc<RwLock<Option<String>>>,
    sdk_client: Arc<OnceCell<MatrixSdkClient>>,
    http_client: Client,
    reaction_events: Arc<RwLock<HashMap<String, String>>>,
    voice_mode: Arc<AtomicBool>,
    transcription: Option<crate::config::TranscriptionConfig>,
    voice_transcriptions: Arc<Mutex<std::collections::HashMap<String, String>>>,
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
async fn download_and_save_matrix_media(
    client: &MatrixSdkClient,
    source: &MediaSource,
    filename: &str,
    save_dir: &Path,
) -> anyhow::Result<PathBuf> {
    let request = MediaRequestParameters {
        source: source.clone(),
        format: MediaFormat::File,
    };

    let data = client.media().get_media_content(&request, false).await?;

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
        access_token: String,
        room_id: String,
        allowed_users: Vec<String>,
    ) -> Self {
        Self::new_with_session_hint(homeserver, access_token, room_id, allowed_users, None, None)
    }

    pub fn new_with_session_hint(
        homeserver: String,
        access_token: String,
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
        access_token: String,
        room_id: String,
        allowed_users: Vec<String>,
        owner_hint: Option<String>,
        device_id_hint: Option<String>,
        zeroclaw_dir: Option<PathBuf>,
    ) -> Self {
        let homeserver = homeserver.trim_end_matches('/').to_string();
        let access_token = access_token.trim().to_string();
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
            reaction_events: Arc::new(RwLock::new(HashMap::new())),
            voice_mode: Arc::new(AtomicBool::new(false)),
            transcription: None,
            voice_transcriptions: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
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

    fn auth_header_value(&self) -> String {
        format!("Bearer {}", self.access_token)
    }

    fn matrix_store_dir(&self) -> Option<PathBuf> {
        self.zeroclaw_dir
            .as_ref()
            .map(|dir| dir.join("state").join("matrix"))
    }

    fn media_save_dir(&self) -> Option<PathBuf> {
        self.zeroclaw_dir
            .as_ref()
            .map(|dir| dir.join("matrix_files"))
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
            .header("Authorization", self.auth_header_value())
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
                let identity = self.get_my_identity().await;
                let whoami = match identity {
                    Ok(whoami) => Some(whoami),
                    Err(error) => {
                        if self.session_owner_hint.is_some() && self.session_device_id_hint.is_some()
                        {
                            tracing::warn!(
                                "Matrix whoami failed; falling back to configured session hints for E2EE session restore: {error}"
                            );
                            None
                        } else {
                            return Err(error);
                        }
                    }
                };

                let resolved_user_id = if let Some(whoami) = whoami.as_ref() {
                    if let Some(hinted) = self.session_owner_hint.as_ref() {
                        if hinted != &whoami.user_id {
                            tracing::warn!(
                                "Matrix configured user_id '{}' does not match whoami '{}'; using whoami.",
                                crate::security::redact(hinted),
                                crate::security::redact(&whoami.user_id)
                            );
                        }
                    }
                    whoami.user_id.clone()
                } else {
                    self.session_owner_hint.clone().ok_or_else(|| {
                        anyhow::anyhow!(
                            "Matrix session restore requires user_id when whoami is unavailable"
                        )
                    })?
                };

                let resolved_device_id = match (whoami.as_ref(), self.session_device_id_hint.as_ref()) {
                    (Some(whoami), Some(hinted)) => {
                        if let Some(whoami_device_id) = whoami.device_id.as_ref() {
                            if whoami_device_id != hinted {
                                tracing::warn!(
                                    "Matrix configured device_id '{}' does not match whoami '{}'; using whoami.",
                                    crate::security::redact(hinted),
                                    crate::security::redact(whoami_device_id)
                                );
                            }
                            whoami_device_id.clone()
                        } else {
                            hinted.clone()
                        }
                    }
                    (Some(whoami), None) => whoami.device_id.clone().ok_or_else(|| {
                        anyhow::anyhow!(
                            "Matrix whoami response did not include device_id. Set channels.matrix.device_id to enable E2EE session restore."
                        )
                    })?,
                    (None, Some(hinted)) => hinted.clone(),
                    (None, None) => {
                        return Err(anyhow::anyhow!(
                            "Matrix E2EE session restore requires device_id when whoami is unavailable"
                        ));
                    }
                };

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

                let user_id: OwnedUserId = resolved_user_id.parse()?;
                let session = MatrixSession {
                    meta: SessionMeta {
                        user_id,
                        device_id: resolved_device_id.into(),
                    },
                    tokens: SessionTokens {
                        access_token: self.access_token.clone(),
                        refresh_token: None,
                    },
                };

                client.restore_session(session).await?;

                // Bootstrap cross-signing if not already set up, so other
                // devices/users can verify this bot and share room keys.
                if let Err(error) = client
                    .encryption()
                    .bootstrap_cross_signing_if_needed(None)
                    .await
                {
                    tracing::warn!(
                        "Matrix cross-signing bootstrap failed (verification may be unavailable): {error}"
                    );
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
                .header("Authorization", self.auth_header_value())
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
            .header("Authorization", self.auth_header_value())
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
            .header("Authorization", self.auth_header_value())
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

/// Handle an incoming SAS verification request: accept, start SAS, log emojis,
/// and auto-confirm once the other side confirms (operator verifies on their client).
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

    if let Err(error) = request.accept().await {
        tracing::warn!("Matrix verification accept failed: {error}");
        return;
    }

    // Wait for the request to transition to Ready, then start SAS.
    let mut changes = request.changes();
    loop {
        match request.state() {
            VerificationRequestState::Ready { .. } => break,
            VerificationRequestState::Done | VerificationRequestState::Cancelled(_) => return,
            _ => {}
        }
        if changes.next().await.is_none() {
            return;
        }
    }

    let Some(sas) = (match request.start_sas().await {
        Ok(sas) => sas,
        Err(error) => {
            tracing::warn!("Matrix SAS verification start failed: {error}");
            return;
        }
    }) else {
        tracing::warn!("Matrix SAS verification could not be started (not in ready state)");
        return;
    };

    tracing::info!("Matrix SAS verification started with {}", sender);

    let mut sas_changes = sas.changes();
    while let Some(state) = sas_changes.next().await {
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

                // Auto-confirm: the operator verifies emojis on their Element client.
                // Once Element confirms, the SAS protocol completes. We confirm on
                // the bot side since the operator already compared visually.
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
        let (cleaned_content, parsed_attachments) = parse_matrix_attachment_markers(&raw_content);

        let client = self.matrix_client().await?;
        let target_room_id = if message.recipient.contains("||") {
            message
                .recipient
                .splitn(2, "||")
                .nth(1)
                .unwrap()
                .to_string()
        } else {
            self.target_room_id().await?
        };
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

        // Send each attachment as a separate media message.
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

            let upload_result = client.media().upload(&mime, bytes, None).await;

            let mxc_uri: OwnedMxcUri = match upload_result {
                Ok(response) => response.content_uri,
                Err(error) => {
                    tracing::warn!(
                        "Matrix media upload failed for '{}': {error}",
                        path.display()
                    );
                    continue;
                }
            };

            let filename = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("attachment.bin")
                .to_string();

            let msg_type = match attachment.kind {
                MatrixOutgoingAttachmentKind::Image => {
                    MessageType::Image(ImageMessageEventContent::plain(filename, mxc_uri))
                }
                MatrixOutgoingAttachmentKind::Audio | MatrixOutgoingAttachmentKind::Voice => {
                    MessageType::Audio(AudioMessageEventContent::plain(filename, mxc_uri))
                }
                MatrixOutgoingAttachmentKind::File => {
                    MessageType::File(FileMessageEventContent::plain(filename, mxc_uri))
                }
            };

            room.send(RoomMessageEventContent::new(msg_type)).await?;
        }

        // Send remaining text (if any) after attachments, with threading support.
        let send_text = |text: &str| {
            let mut content = RoomMessageEventContent::text_markdown(text);
            if let Some(ref thread_ts) = message.thread_ts {
                if let Ok(thread_root) = thread_ts.parse::<OwnedEventId>() {
                    content.relates_to = Some(Relation::Thread(Thread::plain(
                        thread_root.clone(),
                        thread_root,
                    )));
                }
            }
            content
        };

        if !cleaned_content.is_empty() {
            room.send(send_text(&cleaned_content)).await?;
        } else if parsed_attachments.is_empty() {
            // No markers were found — send original content as text.
            room.send(send_text(&raw_content)).await?;
        }

        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
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
        let client = self.matrix_client().await?;

        self.log_e2ee_diagnostics(&client).await;

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
        let homeserver_for_handler = self.homeserver.clone();
        let access_token_for_handler = self.access_token.clone();
        let voice_mode_for_handler = Arc::clone(&self.voice_mode);
        let media_save_dir_for_handler = self.media_save_dir();
        let transcription_for_handler = self.transcription.clone();
        let voice_cache_for_handler = Arc::clone(&self.voice_transcriptions);

        client.add_event_handler(move |event: OriginalSyncRoomMessageEvent, room: Room| {
            let tx = tx_handler.clone();
            let target_room = target_room_for_handler.clone();
            let my_user_id = my_user_id_for_handler.clone();
            let allowed_users = allowed_users_for_handler.clone();
            let dedupe = Arc::clone(&dedupe_for_handler);
            let homeserver = homeserver_for_handler.clone();
            let access_token = access_token_for_handler.clone();
            let voice_mode = Arc::clone(&voice_mode_for_handler);
            let media_save_dir = media_save_dir_for_handler.clone();
            let transcription_config = transcription_for_handler.clone();
            let voice_cache = Arc::clone(&voice_cache_for_handler);

            async move {
                if false
                /* multi-room: room_id filter disabled */
                {
                    return;
                }

                if event.sender == my_user_id {
                    return;
                }

                let sender = event.sender.to_string();
                if !MatrixChannel::is_sender_allowed(&allowed_users, &sender) {
                    return;
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
                        let sdk_client = room.client();
                        match download_and_save_matrix_media(&sdk_client, &source, &filename, save_dir).await {
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
                        let sdk_client = room.client();
                        match download_and_save_matrix_media(&sdk_client, &source, &filename, save_dir).await {
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
                        let sdk_client = room.client();

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
                                            voice_mode.store(true, Ordering::Relaxed);
                                            format!("[Voice] {text}")
                                        }
                                        Err(error) => {
                                            tracing::debug!("Matrix audio transcription failed, falling back to file save: {error}");
                                            let Some(ref save_dir) = media_save_dir else {
                                                tracing::warn!("Matrix audio received but no zeroclaw_dir configured for media storage");
                                                return;
                                            };
                                            match download_and_save_matrix_media(&sdk_client, &source, &filename, save_dir).await {
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
                            match download_and_save_matrix_media(&sdk_client, &source, &filename, save_dir).await {
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
                    MessageType::Video(content) => {
                        format!("[video: {}]", content.body)
                    }
                    _ => return,
                };

                if !MatrixChannel::has_non_empty_body(&body) {
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

                let thread_ts = match &event.content.relates_to {
                    Some(Relation::Thread(thread)) => Some(thread.event_id.to_string()),
                    _ => None,
                };
                let msg = ChannelMessage {
                    id: event_id,
                    sender: sender.clone(),
                    reply_target: format!("{}||{}", sender, room.room_id()),
                    content: body,
                    channel: format!("matrix:{}", room.room_id()),
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    thread_ts,
                };

                let _ = tx.send(msg).await;
            }
        });

        // Verification handler: accept incoming SAS verification requests
        // and auto-confirm after logging emojis for operator review.
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

    async fn health_check(&self) -> bool {
        let Ok(room_id) = self.target_room_id().await else {
            return false;
        };

        if self.ensure_room_supported(&room_id).await.is_err() {
            return false;
        }

        self.matrix_client().await.is_ok()
    }

    async fn add_reaction(
        &self,
        _channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        let client = self.matrix_client().await?;
        let target_room_id = self.target_room_id().await?;
        let target_room: OwnedRoomId = target_room_id.parse()?;

        let room = client
            .get_room(&target_room)
            .ok_or_else(|| anyhow::anyhow!("Matrix room not found for reaction"))?;

        let event_id: OwnedEventId = message_id
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid event ID for reaction: {}", message_id))?;

        let reaction = ReactionEventContent::new(Annotation::new(event_id, emoji.to_string()));
        let response = room.send(reaction).await?;

        let key = format!("{}:{}", message_id, emoji);
        self.reaction_events
            .write()
            .await
            .insert(key, response.event_id.to_string());

        Ok(())
    }

    async fn remove_reaction(
        &self,
        _channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        let key = format!("{}:{}", message_id, emoji);
        let reaction_event_id = self.reaction_events.write().await.remove(&key);

        if let Some(reaction_event_id) = reaction_event_id {
            let client = self.matrix_client().await?;
            let target_room_id = self.target_room_id().await?;
            let target_room: OwnedRoomId = target_room_id.parse()?;

            let room = client
                .get_room(&target_room)
                .ok_or_else(|| anyhow::anyhow!("Matrix room not found for reaction removal"))?;

            let event_id: OwnedEventId = reaction_event_id
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid reaction event ID: {}", reaction_event_id))?;

            room.redact(&event_id, None, None).await?;
        }

        Ok(())
    }

    async fn pin_message(&self, _channel_id: &str, message_id: &str) -> anyhow::Result<()> {
        let room_id = self.target_room_id().await?;
        let encoded_room = Self::encode_path_segment(&room_id);

        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/state/m.room.pinned_events",
            self.homeserver, encoded_room
        );
        let resp = self
            .http_client
            .get(&url)
            .header("Authorization", self.auth_header_value())
            .send()
            .await?;

        let mut pinned: Vec<String> = if resp.status().is_success() {
            let body: serde_json::Value = resp.json().await?;
            body.get("pinned")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        let msg_id = message_id.to_string();
        if pinned.contains(&msg_id) {
            return Ok(());
        }
        pinned.push(msg_id);

        let put_url = format!(
            "{}/_matrix/client/v3/rooms/{}/state/m.room.pinned_events",
            self.homeserver, encoded_room
        );
        let body = serde_json::json!({ "pinned": pinned });
        let resp = self
            .http_client
            .put(&put_url)
            .header("Authorization", self.auth_header_value())
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Matrix pin_message failed: {err}");
        }

        Ok(())
    }

    async fn unpin_message(&self, _channel_id: &str, message_id: &str) -> anyhow::Result<()> {
        let room_id = self.target_room_id().await?;
        let encoded_room = Self::encode_path_segment(&room_id);

        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/state/m.room.pinned_events",
            self.homeserver, encoded_room
        );
        let resp = self
            .http_client
            .get(&url)
            .header("Authorization", self.auth_header_value())
            .send()
            .await?;

        if !resp.status().is_success() {
            return Ok(());
        }

        let body: serde_json::Value = resp.json().await?;
        let mut pinned: Vec<String> = body
            .get("pinned")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let msg_id = message_id.to_string();
        let original_len = pinned.len();
        pinned.retain(|id| id != &msg_id);

        if pinned.len() == original_len {
            return Ok(());
        }

        let put_url = format!(
            "{}/_matrix/client/v3/rooms/{}/state/m.room.pinned_events",
            self.homeserver, encoded_room
        );
        let body = serde_json::json!({ "pinned": pinned });
        let resp = self
            .http_client
            .put(&put_url)
            .header("Authorization", self.auth_header_value())
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("Matrix unpin_message failed: {err}");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_channel() -> MatrixChannel {
        MatrixChannel::new(
            "https://matrix.org".to_string(),
            "syt_test_token".to_string(),
            "!room:matrix.org".to_string(),
            vec!["@user:matrix.org".to_string()],
        )
    }

    fn make_channel_with_zeroclaw_dir() -> MatrixChannel {
        MatrixChannel::new_with_session_hint_and_zeroclaw_dir(
            "https://matrix.org".to_string(),
            "syt_test_token".to_string(),
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
        assert_eq!(ch.access_token, "syt_test_token");
        assert_eq!(ch.room_id, "!room:matrix.org");
        assert_eq!(ch.allowed_users.len(), 1);
    }

    #[test]
    fn strips_trailing_slash() {
        let ch = MatrixChannel::new(
            "https://matrix.org/".to_string(),
            "tok".to_string(),
            "!r:m".to_string(),
            vec![],
        );
        assert_eq!(ch.homeserver, "https://matrix.org");
    }

    #[test]
    fn no_trailing_slash_unchanged() {
        let ch = MatrixChannel::new(
            "https://matrix.org".to_string(),
            "tok".to_string(),
            "!r:m".to_string(),
            vec![],
        );
        assert_eq!(ch.homeserver, "https://matrix.org");
    }

    #[test]
    fn multiple_trailing_slashes_strip_all() {
        let ch = MatrixChannel::new(
            "https://matrix.org//".to_string(),
            "tok".to_string(),
            "!r:m".to_string(),
            vec![],
        );
        assert_eq!(ch.homeserver, "https://matrix.org");
    }

    #[test]
    fn trims_access_token() {
        let ch = MatrixChannel::new(
            "https://matrix.org".to_string(),
            "  syt_test_token  ".to_string(),
            "!r:m".to_string(),
            vec![],
        );
        assert_eq!(ch.access_token, "syt_test_token");
    }

    #[test]
    fn session_hints_are_normalized() {
        let ch = MatrixChannel::new_with_session_hint(
            "https://matrix.org".to_string(),
            "tok".to_string(),
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
            "tok".to_string(),
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
            "tok".to_string(),
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
            "tok".to_string(),
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
            "tok".to_string(),
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
            "tok".to_string(),
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
            "tok".to_string(),
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
            "tok".to_string(),
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
            "tok".to_string(),
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
            "tok".to_string(),
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
            "tok".to_string(),
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
            Some(PathBuf::from("/tmp/zeroclaw/matrix_files"))
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
