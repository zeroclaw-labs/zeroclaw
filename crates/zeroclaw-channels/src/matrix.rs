//! Matrix channel using matrix-rust-sdk 0.16.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;
use tokio::sync::{Mutex as TokioMutex, RwLock as TokioRwLock, mpsc, oneshot};

use matrix_sdk::{
    Client,
    ruma::{
        OwnedEventId, OwnedRoomId, OwnedUserId,
        api::client::{
            membership::invite_user::v3::{
                InvitationRecipient, InviteUserId, Request as InviteUserRequest,
            },
            room::{Visibility as MatrixVisibility, create_room::v3::Request as CreateRoomRequest},
        },
        events::{InitialStateEvent, room::encryption::RoomEncryptionEventContent},
    },
};

use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, RoomCreationOptions,
    RoomVisibility, SendMessage,
};
use zeroclaw_config::schema::{MatrixConfig, StreamMode, TranscriptionConfig};

// ─── markers ───────────────────────────────────────────────────────────────
mod markers {
    //! Parse `[image:url]`, `[audio:url]`, `[video:url]`, `[file:url]`, `[voice:url]`
    //! markers from outbound text. Strips them from the body and returns the kinds
    //! + targets so the caller can upload the corresponding media.

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) enum MarkerKind {
        Image,
        Audio,
        Video,
        File,
        Voice,
    }

    impl MarkerKind {
        fn from_keyword(kw: &str) -> Option<Self> {
            match kw.to_ascii_lowercase().as_str() {
                "image" | "img" | "photo" => Some(Self::Image),
                "audio" => Some(Self::Audio),
                "video" => Some(Self::Video),
                "file" | "document" | "doc" => Some(Self::File),
                "voice" => Some(Self::Voice),
                _ => None,
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) struct Marker {
        pub kind: MarkerKind,
        pub target: String,
    }

    /// Scan `text` for marker substrings. Returns the cleaned text and any markers.
    /// Malformed/unknown markers are left in the text untouched.
    pub(super) fn parse(text: &str) -> (String, Vec<Marker>) {
        let mut out = String::with_capacity(text.len());
        let mut markers = Vec::new();
        let mut chars = text.char_indices().peekable();

        while let Some((start, ch)) = chars.next() {
            if ch != '[' {
                out.push(ch);
                continue;
            }

            let rest = &text[start + 1..];
            let Some(close_rel) = rest.find(']') else {
                out.push(ch);
                continue;
            };
            if rest[..close_rel].contains('\n') {
                out.push(ch);
                continue;
            }
            let inner = &rest[..close_rel];
            let Some(colon) = inner.find(':') else {
                out.push(ch);
                continue;
            };
            let kw = &inner[..colon];
            let target = inner[colon + 1..].trim();

            let Some(kind) = MarkerKind::from_keyword(kw) else {
                out.push(ch);
                continue;
            };
            if target.is_empty() {
                out.push(ch);
                continue;
            }

            markers.push(Marker {
                kind,
                target: target.to_string(),
            });
            let consume_until = start + 1 + close_rel + 1;
            while let Some(&(idx, _)) = chars.peek() {
                if idx >= consume_until {
                    break;
                }
                chars.next();
            }
        }

        // Tidy whitespace left behind by stripped markers.
        let cleaned = out
            .lines()
            .map(|l| l.trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n");

        (cleaned.trim().to_string(), markers)
    }
}

// ─── mention ───────────────────────────────────────────────────────────────
mod mention {
    use matrix_sdk::ruma::UserId;

    pub(super) fn is_mentioned(
        bot_user_id: &UserId,
        bot_display_name: Option<&str>,
        m_mentions_user_ids: Option<&[String]>,
        body: &str,
    ) -> bool {
        if let Some(ids) = m_mentions_user_ids {
            for id in ids {
                if id == bot_user_id.as_str() {
                    return true;
                }
            }
            // Honour the explicit list when set — older clients without
            // `m.mentions` still hit the body-scan fallback below.
            if !ids.is_empty() {
                return false;
            }
        }

        let body_lc = body.to_ascii_lowercase();
        if body_lc.contains(&bot_user_id.as_str().to_ascii_lowercase()) {
            return true;
        }
        let localpart = bot_user_id.localpart().to_ascii_lowercase();
        if body_lc.contains(&format!("@{localpart}")) {
            return true;
        }
        if let Some(name) = bot_display_name
            && !name.is_empty()
        {
            let n = name.to_ascii_lowercase();
            if body_lc.contains(&n) {
                return true;
            }
        }
        false
    }
}

// ─── allowlist ─────────────────────────────────────────────────────────────
mod allowlist {
    pub(super) fn user_allowed(allowed_users: &[String], sender: &str) -> bool {
        crate::allowlist::is_user_allowed(
            allowed_users,
            sender,
            crate::allowlist::Match::CaseInsensitive,
        )
    }

    pub(super) fn room_allowed_static(allowed_rooms: &[String], room_id: &str) -> bool {
        if allowed_rooms.is_empty() {
            return true;
        }
        allowed_rooms
            .iter()
            .any(|r| r == room_id || r.eq_ignore_ascii_case(room_id))
    }
}

// ─── approval ──────────────────────────────────────────────────────────────
mod approval {
    use rand::{Rng, RngExt};
    use zeroclaw_api::channel::ChannelApprovalResponse;

    pub(super) const TOKEN_LEN: usize = 8;
    const TOKEN_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

    pub(super) fn generate_token<R: Rng>(rng: &mut R) -> String {
        (0..TOKEN_LEN)
            .map(|_| TOKEN_ALPHABET[rng.random_range(0..TOKEN_ALPHABET.len())] as char)
            .collect()
    }

    pub(super) fn generate_token_default() -> String {
        let mut rng = rand::rng();
        generate_token(&mut rng)
    }

    /// Try to parse an approval reply. Returns `Some((token, response))` if the
    /// body matches `<TOKEN> (approve|deny|always|yes|no)` (case-insensitive).
    pub(super) fn parse_reply(body: &str) -> Option<(String, ChannelApprovalResponse)> {
        let trimmed = body.trim();
        let mut parts = trimmed.split_whitespace();
        let token = parts.next()?;
        if token.len() != TOKEN_LEN {
            return None;
        }
        if !token.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        let verb = parts.next()?.to_ascii_lowercase();
        if parts.next().is_some() {
            return None;
        }
        let response = match verb.as_str() {
            "approve" | "yes" | "y" => ChannelApprovalResponse::Approve,
            "deny" | "no" | "n" => ChannelApprovalResponse::Deny,
            "always" => ChannelApprovalResponse::AlwaysApprove,
            _ => return None,
        };
        Some((token.to_uppercase(), response))
    }
}

// ─── room management ──────────────────────────────────────────────────────
mod room_management {
    use super::*;

    pub(super) fn build_create_room_request(
        options: &RoomCreationOptions,
    ) -> Result<CreateRoomRequest> {
        let mut request = CreateRoomRequest::new();
        request.name = options.name.clone();
        request.topic = options.topic.clone();
        request.invite = options
            .invites
            .iter()
            .map(|user_id| {
                user_id
                    .parse::<OwnedUserId>()
                    .with_context(|| format!("matrix: invalid invite user id '{user_id}'"))
            })
            .collect::<Result<Vec<_>>>()?;
        if let Some(visibility) = options.visibility {
            request.visibility = match visibility {
                RoomVisibility::Private => MatrixVisibility::Private,
                RoomVisibility::Public => MatrixVisibility::Public,
            };
        }
        if options.encryption.unwrap_or(false) {
            request.initial_state.push(
                InitialStateEvent::with_empty_state_key(
                    RoomEncryptionEventContent::with_recommended_defaults(),
                )
                .to_raw_any(),
            );
        }
        Ok(request)
    }

    pub(super) fn build_invite_user_request(
        room_id: &str,
        user_id: &str,
    ) -> Result<InviteUserRequest> {
        let room_id = room_id
            .parse::<OwnedRoomId>()
            .with_context(|| format!("matrix: invalid room id '{room_id}'"))?;
        let user_id = user_id
            .parse::<OwnedUserId>()
            .with_context(|| format!("matrix: invalid user id '{user_id}'"))?;
        Ok(InviteUserRequest::new(
            room_id,
            InvitationRecipient::from(InviteUserId::new(user_id)),
        ))
    }
}

// ─── context (thread-root preamble) ────────────────────────────────────────
mod context {
    //! Inject the thread root as a `[Thread root from @x]: ...` preamble on the
    //! first inbound message we see in each thread. After a restart we re-inject
    //! exactly once per active thread (in-memory tracking only).

    use std::{collections::HashSet, sync::Arc};

    use matrix_sdk::ruma::{OwnedEventId, events::room::message::MessageType};
    use tokio::sync::RwLock;

    pub(super) fn format_preamble(sender: &str, body: &str) -> String {
        let body = body.trim();
        if body.is_empty() {
            format!("[Thread root from {sender}]\n\n")
        } else {
            format!("[Thread root from {sender}]: {body}\n\n")
        }
    }

    /// Returns `true` iff this thread had not been seen before — caller should
    /// fetch the root and inject the preamble. Also marks the thread seen.
    pub(super) async fn claim_first_visit(
        threads_seen: &Arc<RwLock<HashSet<OwnedEventId>>>,
        thread_id: &OwnedEventId,
    ) -> bool {
        let mut guard = threads_seen.write().await;
        guard.insert(thread_id.clone())
    }

    /// Pre-mark a thread — used when the bot starts the thread itself, so the
    /// next inbound thread message doesn't get a preamble pointing at the bot.
    pub(super) async fn mark_seen(
        threads_seen: &Arc<RwLock<HashSet<OwnedEventId>>>,
        thread_id: OwnedEventId,
    ) {
        threads_seen.write().await.insert(thread_id);
    }

    pub(super) fn body_for(msg: &MessageType) -> String {
        match msg {
            MessageType::Text(t) => t.body.clone(),
            MessageType::Notice(n) => n.body.clone(),
            MessageType::Emote(e) => e.body.clone(),
            MessageType::Image(_) => "[image]".to_string(),
            MessageType::File(_) => "[file]".to_string(),
            MessageType::Audio(_) => "[audio]".to_string(),
            MessageType::Video(_) => "[video]".to_string(),
            MessageType::Location(_) => "[location]".to_string(),
            other => other.body().to_string(),
        }
    }
}

// ─── streaming ─────────────────────────────────────────────────────────────
mod streaming {
    use std::{
        collections::HashMap,
        time::{Duration, Instant},
    };

    use anyhow::{Result, bail};
    use matrix_sdk::ruma::{OwnedEventId, OwnedRoomId};

    use super::markers;

    const MULTI_MESSAGE_SYNTHETIC_PREFIX: &str = "multi_message_synthetic:";

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub(super) struct DraftKey {
        room_id: OwnedRoomId,
        draft_id: String,
    }

    pub(super) fn draft_key(room_id: OwnedRoomId, draft_id: &str) -> Result<DraftKey> {
        let draft_id = draft_id.trim();
        if draft_id.is_empty() {
            bail!("matrix: draft message id is empty");
        }
        Ok(DraftKey {
            room_id,
            draft_id: draft_id.to_string(),
        })
    }

    pub(super) fn new_multi_message_draft_id() -> String {
        format!(
            "{MULTI_MESSAGE_SYNTHETIC_PREFIX}{}",
            uuid::Uuid::new_v4().simple()
        )
    }

    #[derive(Debug, Clone)]
    pub(super) struct PartialDraft {
        pub event_id: OwnedEventId,
        pub thread_anchor: Option<OwnedEventId>,
        pub last_text: String,
        pub last_edit: Instant,
    }

    #[derive(Debug, PartialEq, Eq)]
    pub(super) enum PartialFinalizeAction {
        EditDraft,
        RedactDraft,
        EmptyError,
    }

    #[derive(Debug, Clone)]
    pub(super) struct MultiDraft {
        pub thread_anchor: Option<OwnedEventId>,
        pub sent_so_far: usize,
    }

    #[derive(Default, Debug)]
    pub(super) struct State {
        pub partial: HashMap<DraftKey, PartialDraft>,
        pub multi: HashMap<DraftKey, MultiDraft>,
    }

    pub(super) fn partial_for_update<'a>(
        state: &'a mut State,
        key: &DraftKey,
    ) -> Option<&'a mut PartialDraft> {
        state.partial.get_mut(key)
    }

    pub(super) fn take_partial(state: &mut State, key: &DraftKey) -> Option<PartialDraft> {
        state.partial.remove(key)
    }

    pub(super) fn multi_for_update<'a>(
        state: &'a mut State,
        key: &DraftKey,
    ) -> Option<&'a mut MultiDraft> {
        state.multi.get_mut(key)
    }

    pub(super) fn take_multi(state: &mut State, key: &DraftKey) -> Option<MultiDraft> {
        state.multi.remove(key)
    }

    pub(super) fn partial_should_edit(
        existing: &PartialDraft,
        new_text: &str,
        now: Instant,
        min_interval: Duration,
    ) -> bool {
        if existing.last_text == new_text {
            return false;
        }
        now.saturating_duration_since(existing.last_edit) >= min_interval
    }

    pub(super) fn partial_visible_text(text: &str) -> Option<String> {
        let (cleaned, _) = markers::parse(text);
        let cleaned = cleaned.trim();
        if cleaned.is_empty() {
            None
        } else {
            Some(cleaned.to_string())
        }
    }

    pub(super) fn decide_partial_finalize_action(
        text_is_empty_after_delivery: bool,
        any_attachment_landed: bool,
    ) -> PartialFinalizeAction {
        match (text_is_empty_after_delivery, any_attachment_landed) {
            (false, _) => PartialFinalizeAction::EditDraft,
            (true, true) => PartialFinalizeAction::RedactDraft,
            (true, false) => PartialFinalizeAction::EmptyError,
        }
    }

    /// Find the next paragraph break (`\n\n`) in `new_text`, ignoring any
    /// breaks that fall inside an open ```fenced``` code block. Returns the
    /// byte offset of the first `\n` of the break, or `None` if no break is
    /// found yet (caller should buffer and retry on the next update).
    pub(super) fn next_paragraph_break(new_text: &str) -> Option<usize> {
        let bytes = new_text.as_bytes();
        let mut in_fence = false;
        let mut i = 0;
        while i < bytes.len() {
            // Detect opening or closing ```code fence``` at line start.
            if bytes[i] == b'`'
                && i + 2 < bytes.len()
                && bytes[i + 1] == b'`'
                && bytes[i + 2] == b'`'
                && (i == 0 || bytes[i - 1] == b'\n')
            {
                in_fence = !in_fence;
                i += 3;
                continue;
            }
            if !in_fence && bytes[i] == b'\n' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                return Some(i);
            }
            i += 1;
        }
        None
    }
}

// ─── session ───────────────────────────────────────────────────────────────
mod session {
    //! Persist the Matrix login session next to the SDK SQLite crypto store so
    //! `restore_session()` can reattach without re-running the login flow.

    use std::path::{Path, PathBuf};

    use serde::{Deserialize, Serialize};

    pub(super) const SESSION_FILE: &str = "session.json";

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub(super) struct SessionBlob {
        pub user_id: String,
        pub device_id: String,
        pub access_token: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub refresh_token: Option<String>,
    }

    pub(super) fn path(state_dir: &Path) -> PathBuf {
        state_dir.join(SESSION_FILE)
    }

    pub(super) fn load(state_dir: &Path) -> anyhow::Result<Option<SessionBlob>> {
        let p = path(state_dir);
        if !p.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&p).map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": p.display().to_string(),
                        "error": format!("{}", e),
                    })),
                "matrix: failed to read session blob"
            );
            anyhow::Error::msg(format!("read matrix session blob {}: {e}", p.display()))
        })?;
        match serde_json::from_slice::<SessionBlob>(&bytes) {
            Ok(blob) => Ok(Some(blob)),
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    &format!(
                        "matrix: session blob {} is corrupt JSON ({e}); treating as missing so auto-recovery can re-login",
                        p.display()
                    )
                );
                Ok(None)
            }
        }
    }

    pub(super) fn save(state_dir: &Path, blob: &SessionBlob) -> anyhow::Result<()> {
        std::fs::create_dir_all(state_dir).map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": state_dir.display().to_string(),
                        "error": format!("{}", e),
                    })),
                "matrix: failed to create state dir"
            );
            anyhow::Error::msg(format!(
                "create matrix state dir {}: {e}",
                state_dir.display()
            ))
        })?;
        let p = path(state_dir);
        let json = serde_json::to_vec_pretty(blob)?;
        write_with_owner_only(&p, &json).map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": p.display().to_string(),
                        "error": format!("{}", e),
                    })),
                "matrix: failed to write session blob"
            );
            anyhow::Error::msg(format!("write matrix session blob {}: {e}", p.display()))
        })?;
        Ok(())
    }

    /// Write the session blob with `0o600` permissions on Unix so the
    /// access token isn't world-readable under a permissive umask.
    /// Windows falls back to default ACLs (the std-lib write).
    #[cfg(unix)]
    fn write_with_owner_only(path: &Path, contents: &[u8]) -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(contents)
    }

    #[cfg(not(unix))]
    fn write_with_owner_only(path: &Path, contents: &[u8]) -> std::io::Result<()> {
        std::fs::write(path, contents)
    }
}

// ─── client ────────────────────────────────────────────────────────────────
mod client {
    use std::{
        collections::HashMap,
        path::{Path, PathBuf},
        sync::Arc,
        time::Duration,
    };

    use anyhow::{Context as _, Result, bail};
    use matrix_sdk::{
        Client, SessionMeta, SessionTokens,
        authentication::matrix::MatrixSession,
        config::RequestConfig,
        ruma::{OwnedRoomId, RoomAliasId},
    };
    use serde::Deserialize;
    use tokio::sync::RwLock;

    use super::session;
    use zeroclaw_config::schema::MatrixConfig;

    const WHOAMI_ENDPOINT: &str = "_matrix/client/v3/account/whoami";

    pub(super) const CLIENT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
    const WHOAMI_TIMEOUT: Duration = Duration::from_secs(30);
    const WHOAMI_ERROR_BODY_PREVIEW_BYTES: usize = 4096;
    const WHOAMI_ERROR_BODY_DISPLAY_CHARS: usize = 256;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) struct AccessTokenIdentity {
        pub user_id: String,
        pub device_id: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    struct WhoamiResponse {
        user_id: String,
        #[serde(default)]
        device_id: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    struct MatrixErrorResponse {
        #[serde(default)]
        errcode: Option<String>,
        #[serde(default)]
        error: Option<String>,
    }

    pub(super) fn store_dir(state_dir: &Path) -> PathBuf {
        state_dir.join("store")
    }

    pub(super) async fn build(config: &MatrixConfig, state_dir: &Path) -> Result<Client> {
        build_attempt(config, state_dir, 0).await
    }

    fn wipe_state(state_dir: &Path) -> Result<()> {
        let session = session::path(state_dir);
        if session.exists()
            && let Err(e) = std::fs::remove_file(&session)
        {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": session.display().to_string(),
                        "phase": "corruption_recovery",
                        "error": format!("{}", e),
                    })),
                "matrix: failed to remove session blob during corruption recovery"
            );
            return Err(anyhow::Error::msg(format!(
                "matrix: failed to remove {} during corruption recovery: {e}. Fix permissions or wipe the directory manually.",
                session.display()
            )));
        }
        let store = store_dir(state_dir);
        if store.exists()
            && let Err(e) = std::fs::remove_dir_all(&store)
        {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": store.display().to_string(),
                        "phase": "corruption_recovery",
                        "error": format!("{}", e),
                    })),
                "matrix: failed to remove store dir during corruption recovery"
            );
            return Err(anyhow::Error::msg(format!(
                "matrix: failed to remove {} during corruption recovery: {e}. Fix permissions or wipe the directory manually.",
                store.display()
            )));
        }
        Ok(())
    }

    pub(super) fn store_has_orphan_data(state_dir: &Path) -> bool {
        let store = store_dir(state_dir);
        let Ok(mut entries) = std::fs::read_dir(&store) else {
            return false;
        };
        entries.any(|e| e.is_ok())
    }

    pub(super) fn can_password_relogin(config: &MatrixConfig) -> bool {
        let has_password = config
            .password
            .as_deref()
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        let has_user_id = config
            .user_id
            .as_deref()
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        has_password && has_user_id
    }

    pub(super) fn saved_session_is_foreign(
        config: &MatrixConfig,
        blob: &session::SessionBlob,
    ) -> bool {
        let Some(want) = config.user_id.as_deref().filter(|s| !s.is_empty()) else {
            return false;
        };
        if !want.contains(':') {
            return false;
        }
        want != blob.user_id.as_str()
    }

    async fn build_attempt(
        config: &MatrixConfig,
        state_dir: &Path,
        recovery_attempts: u32,
    ) -> Result<Client> {
        // Hard recursion bound: at most one auto-wipe + relogin cycle per call.
        if recovery_attempts > 1 {
            bail!(
                "matrix: corruption recovery looped — aborting to avoid an infinite restart cycle. \
                 Wipe ~/.zeroclaw/state/matrix/ manually and restart."
            );
        }

        let saved = session::load(state_dir)?;

        // A saved session that belongs to a different account would run this
        // channel block as the wrong Matrix identity. Wipe and re-login fresh
        // under the configured account instead of impersonating.
        if let Some(blob) = saved.as_ref()
            && saved_session_is_foreign(config, blob)
        {
            return recover_or_bail(
                config,
                state_dir,
                recovery_attempts,
                &format!(
                    "saved session user_id ({}) does not match configured channels.matrix user_id ({}); store belongs to a different account.",
                    blob.user_id,
                    config.user_id.as_deref().unwrap_or_default()
                ),
            )
            .await;
        }

        if let (Some(blob), Some(want)) = (
            saved.as_ref(),
            config.device_id.as_deref().filter(|s| !s.is_empty()),
        ) && want != blob.device_id
        {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!(
                    "matrix: configured channels.matrix.device-id ({want}) differs from the saved session ({}). \
                 Honoring the saved device_id (canonical, assigned by the homeserver). \
                 Update channels.matrix.device-id to match (or clear it) to silence this warning, \
                 or wipe {} entirely to register a different device.",
                    blob.device_id,
                    state_dir.display()
                )
            );
        }

        if saved.is_none() && store_has_orphan_data(state_dir) {
            return recover_or_bail(
                config,
                state_dir,
                recovery_attempts,
                "found crypto store data without a saved session.json — orphan state from a prior install or interrupted run.",
            )
            .await;
        }

        let store = store_dir(state_dir);
        std::fs::create_dir_all(&store)
            .with_context(|| format!("create matrix store dir {}", store.display()))?;

        let client = Client::builder()
            .homeserver_url(&config.homeserver)
            .sqlite_store(&store, None)
            // Widen the per-request timeout past the sync long-poll window so
            // an idle `/sync` never trips the SDK's default 30s request
            // deadline before the homeserver's own long-poll returns.
            .request_config(RequestConfig::new().timeout(CLIENT_REQUEST_TIMEOUT))
            .build()
            .await
            .context("build matrix client")?;

        // Step 1: restore an existing session, or fresh-login.
        if let Some(blob) = saved {
            let saved_device_id = blob.device_id.clone();
            let session = MatrixSession {
                meta: SessionMeta {
                    user_id: blob.user_id.parse().context("parse stored user_id")?,
                    device_id: blob.device_id.into(),
                },
                tokens: SessionTokens {
                    access_token: blob.access_token,
                    refresh_token: blob.refresh_token,
                },
            };
            match client
                .matrix_auth()
                .restore_session(session, matrix_sdk::store::RoomLoadSettings::default())
                .await
            {
                Ok(()) => ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    "matrix: restored session from session.json"
                ),
                Err(e) => {
                    // restore_session failed despite a matching device_id —
                    // the access token is probably revoked, or the saved
                    // session disagrees with the local crypto store.
                    drop(client);
                    return recover_or_bail(
                        config,
                        state_dir,
                        recovery_attempts,
                        &format!(
                            "restore_session failed for device_id {saved_device_id}: {e}. \
                             The access token is likely revoked or the local crypto store is inconsistent."
                        ),
                    )
                    .await;
                }
            }

            let otk_corruption_flagged = client
                .state_store()
                .get_kv_data(matrix_sdk::store::StateStoreDataKey::OneTimeKeyAlreadyUploaded)
                .await
                .ok()
                .flatten()
                .is_some();
            if otk_corruption_flagged {
                drop(client);
                return recover_or_bail(
                    config,
                    state_dir,
                    recovery_attempts,
                    "matrix-sdk has flagged the local crypto store as out-of-sync with server-side one-time keys (StateStoreDataKey::OneTimeKeyAlreadyUploaded). The local store has lost track of OTKs that the server still records — fresh sends would fail to decrypt. The SDK has no in-place fix for this state.",
                )
                .await;
            }
        } else {
            login_fresh(&client, config).await?;
            if let Some(blob) = session_blob_from(&client)
                && let Err(e) = session::save(state_dir, &blob)
            {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "matrix: failed to persist session.json"
                );
            }
        }

        if let Some(key) = config.recovery_key.as_deref()
            && !key.is_empty()
        {
            run_recovery(&client, key).await;
        }

        Ok(client)
    }

    /// Either auto-wipe + retry (when password + user_id are configured) or
    /// bail with operator-actionable instructions.
    async fn recover_or_bail(
        config: &MatrixConfig,
        state_dir: &Path,
        recovery_attempts: u32,
        reason: &str,
    ) -> Result<Client> {
        if can_password_relogin(config) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!(
                    "matrix: {reason} Auto-recovering: wiping {} and re-authenticating with password.",
                    state_dir.display()
                )
            );
            wipe_state(state_dir)?;
            return Box::pin(build_attempt(config, state_dir, recovery_attempts + 1)).await;
        }
        bail!(
            "matrix: {reason}\n\
             Cannot auto-recover because channels.matrix.password and channels.matrix.user-id are not both set.\n\
             Either:\n  \
             • configure channels.matrix.password (and user-id) so the next start can re-authenticate, or\n  \
             • wipe the state directory manually:  rm -rf {}",
            state_dir.display(),
        );
    }

    async fn login_fresh(client: &Client, config: &MatrixConfig) -> Result<()> {
        // Prefer password when set: it creates a server-side device matching
        // `config.device_id`, so subsequent crypto operations don't fight with
        // a token bound to a different device.
        if let Some(pw) = config.password.as_deref().filter(|s| !s.is_empty()) {
            return password_login(client, config, pw).await;
        }
        if config
            .access_token
            .as_deref()
            .is_some_and(|t| !t.is_empty())
        {
            return access_token_login(client, config).await;
        }
        bail!("matrix login requires either access_token or user_id+password")
    }

    async fn password_login(client: &Client, config: &MatrixConfig, password: &str) -> Result<()> {
        let user_id = config
            .user_id
            .clone()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "matrix.user_id is required for password login"
                );
                anyhow::Error::msg("matrix.user_id is required for password login")
            })?;
        let mut login = client
            .matrix_auth()
            .login_username(&user_id, password)
            .initial_device_display_name("ZeroClaw");
        if let Some(d) = config.device_id.as_deref()
            && !d.is_empty()
        {
            login = login.device_id(d);
        }
        login.send().await.context("password login failed")?;
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "matrix: logged in via password"
        );
        Ok(())
    }

    async fn access_token_login(client: &Client, config: &MatrixConfig) -> Result<()> {
        let identity = resolve_access_token_identity(config).await?;
        let user_id = identity.user_id.parse().context("parse matrix.user_id")?;
        let device_id = identity.device_id.ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                "matrix: access-token login requires a Matrix device_id"
            );
            anyhow::Error::msg("matrix: access-token login requires a Matrix device_id")
        })?;
        let session = MatrixSession {
            meta: SessionMeta {
                user_id,
                device_id: device_id.into(),
            },
            tokens: SessionTokens {
                access_token: config.access_token.clone().ok_or_else(|| {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                        "matrix.access_token is required for token login"
                    );
                    anyhow::Error::msg("matrix.access_token is required for token login")
                })?,
                refresh_token: None,
            },
        };
        client
            .matrix_auth()
            .restore_session(session, matrix_sdk::store::RoomLoadSettings::default())
            .await
            .context("attach matrix session via access_token")?;
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "matrix: logged in via access_token"
        );
        Ok(())
    }

    fn non_empty_config_value(value: Option<&str>) -> Option<String> {
        value
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
    }

    pub(super) async fn resolve_access_token_identity(
        config: &MatrixConfig,
    ) -> Result<AccessTokenIdentity> {
        let configured_user_id = non_empty_config_value(config.user_id.as_deref());
        let configured_device_id = non_empty_config_value(config.device_id.as_deref());

        if let (Some(user_id), Some(device_id)) =
            (configured_user_id.as_ref(), configured_device_id.as_ref())
        {
            return Ok(AccessTokenIdentity {
                user_id: user_id.clone(),
                device_id: Some(device_id.clone()),
            });
        }

        let whoami = fetch_access_token_whoami(config).await?;

        if let Some(ref configured) = configured_user_id
            && configured != &whoami.user_id
        {
            bail!(
                "matrix: configured channels.matrix.user-id ({configured}) does not match Matrix whoami user_id ({})",
                whoami.user_id
            );
        }

        if let (Some(configured), Some(actual)) = (&configured_device_id, &whoami.device_id)
            && configured != actual
        {
            bail!(
                "matrix: configured channels.matrix.device-id ({configured}) does not match Matrix whoami device_id ({actual})"
            );
        }

        if configured_device_id.is_none() && whoami.device_id.is_none() {
            bail!(
                "matrix: whoami response did not include device_id; configure channels.matrix.device-id for access-token login"
            );
        }

        Ok(AccessTokenIdentity {
            user_id: configured_user_id.unwrap_or(whoami.user_id),
            device_id: configured_device_id.or(whoami.device_id),
        })
    }

    async fn fetch_access_token_whoami(config: &MatrixConfig) -> Result<WhoamiResponse> {
        let access_token = config
            .access_token
            .as_deref()
            .context("matrix: whoami requires access_token")?;
        let url = matrix_client_api_url(&config.homeserver, WHOAMI_ENDPOINT)?;
        let response = reqwest::Client::builder()
            .timeout(WHOAMI_TIMEOUT)
            .build()
            .context("matrix: build whoami HTTP client")?
            .get(url)
            .bearer_auth(access_token)
            .send()
            .await
            .context("matrix: whoami request failed")?;
        let status = response.status();

        if !status.is_success() {
            let body = read_whoami_error_body_preview(response).await;
            bail!("matrix: whoami request failed with HTTP {status}: {body}");
        }

        let mut whoami = response
            .json::<WhoamiResponse>()
            .await
            .context("matrix: failed to parse whoami response")?;
        whoami.user_id = whoami.user_id.trim().to_string();
        if whoami.user_id.is_empty() {
            bail!("matrix: whoami response did not include user_id");
        }
        whoami.device_id = whoami
            .device_id
            .map(|device_id| device_id.trim().to_string())
            .filter(|device_id| !device_id.is_empty());

        Ok(whoami)
    }

    async fn read_whoami_error_body_preview(mut response: reqwest::Response) -> String {
        let mut preview = Vec::new();
        let mut truncated = false;

        while preview.len() < WHOAMI_ERROR_BODY_PREVIEW_BYTES {
            let chunk = match response.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(err) => return format!("failed to read response body: {err}"),
            };
            let remaining = WHOAMI_ERROR_BODY_PREVIEW_BYTES - preview.len();
            if chunk.len() > remaining {
                preview.extend_from_slice(&chunk[..remaining]);
                truncated = true;
                break;
            }
            preview.extend_from_slice(&chunk);
        }

        if preview.len() == WHOAMI_ERROR_BODY_PREVIEW_BYTES {
            truncated = true;
        }

        format_whoami_error_body_preview(&preview, truncated)
    }

    fn format_whoami_error_body_preview(preview: &[u8], truncated: bool) -> String {
        if let Ok(error) = serde_json::from_slice::<MatrixErrorResponse>(preview) {
            let errcode = error
                .errcode
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let message = error
                .error
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let formatted = match (errcode, message) {
                (Some(errcode), Some(message)) => Some(format!("{errcode}: {message}")),
                (Some(errcode), None) => Some(errcode.to_string()),
                (None, Some(message)) => Some(message.to_string()),
                (None, None) => None,
            };
            if let Some(formatted) = formatted {
                return truncate_with_ellipsis(&formatted, WHOAMI_ERROR_BODY_DISPLAY_CHARS);
            }
        }

        let body = String::from_utf8_lossy(preview).trim().to_string();
        if body.is_empty() {
            return "<empty response body>".to_string();
        }
        let mut body = truncate_with_ellipsis(&body, WHOAMI_ERROR_BODY_DISPLAY_CHARS);
        if truncated {
            body.push_str(" [truncated]");
        }
        body
    }

    fn truncate_with_ellipsis(value: &str, max_chars: usize) -> String {
        let mut chars = value.chars();
        let mut truncated: String = chars.by_ref().take(max_chars).collect();
        if chars.next().is_some() {
            truncated.push_str("...");
        }
        truncated
    }

    fn matrix_client_api_url(homeserver: &str, endpoint_path: &str) -> Result<reqwest::Url> {
        let mut url = reqwest::Url::parse(homeserver).context("parse matrix homeserver URL")?;
        let base_path = url.path().trim_end_matches('/');
        let endpoint_path = endpoint_path.trim_start_matches('/');
        let full_path = if base_path.is_empty() || base_path == "/" {
            format!("/{endpoint_path}")
        } else {
            format!("{base_path}/{endpoint_path}")
        };
        url.set_path(&full_path);
        url.set_query(None);
        url.set_fragment(None);
        Ok(url)
    }

    fn session_blob_from(client: &Client) -> Option<session::SessionBlob> {
        let session = client.matrix_auth().session()?;
        Some(session::SessionBlob {
            user_id: session.meta.user_id.to_string(),
            device_id: session.meta.device_id.to_string(),
            access_token: session.tokens.access_token,
            refresh_token: session.tokens.refresh_token,
        })
    }

    async fn run_recovery(client: &Client, key: &str) {
        use matrix_sdk::encryption::recovery::RecoveryState;

        let recovery = client.encryption().recovery();
        if matches!(recovery.state(), RecoveryState::Enabled) {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "matrix: recovery already enabled, skipping recover()"
            );
            return;
        }

        let stripped_len = key.chars().filter(|c| !c.is_whitespace()).count();
        diagnose_secret_storage(client, stripped_len).await;

        match recovery.recover_and_fix_backup(key).await {
            Ok(()) => ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                "matrix: E2EE recovery completed (cross-signing + room keys imported; key backup repaired if inconsistent)"
            ),
            Err(e) => ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"e": e.to_string()})),
                "matrix: E2EE recovery failed: ; full error chain = . If the input length above is unexpected (base58 keys are typically ~58 chars, passphrases vary), the wrong value may be in channels.matrix.recovery-key."
            ),
        }
    }

    async fn diagnose_secret_storage(client: &Client, input_len: usize) {
        use matrix_sdk::ruma::events::secret_storage::{
            default_key::SecretStorageDefaultKeyEventContent, key::SecretStorageKeyEventContent,
        };
        use matrix_sdk::ruma::events::{GlobalAccountDataEventType, StaticEventContent};

        let account = client.account();
        let default_key = match account
            .fetch_account_data_static::<SecretStorageDefaultKeyEventContent>()
            .await
        {
            Ok(Some(raw)) => match raw.deserialize() {
                Ok(content) => Some(content),
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "matrix: cannot deserialize default secret-storage key event"
                    );
                    None
                }
            },
            Ok(None) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"input_len": input_len})),
                    "matrix: server has no m.secret_storage.default_key set; recovery cannot proceed (input_len=). Set up Secure Backup in Element first."
                );
                return;
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "matrix: failed to fetch default secret-storage key event"
                );
                return;
            }
        };
        let Some(default_key) = default_key else {
            return;
        };
        let key_id = default_key.key_id;

        // Fetch the actual key event for the default key id so we can see
        // whether it has passphrase info (affects which decode path the SDK
        // tries first inside SecretStorageKey::from_account_data).
        let event_type = GlobalAccountDataEventType::SecretStorageKey(key_id.clone());
        match account.fetch_account_data(event_type).await {
            Ok(Some(raw)) => {
                let json = raw.json().get();
                let has_passphrase =
                    json.contains("\"passphrase\"") && json.contains("\"iterations\"");
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                    &format!(
                        "matrix: secret-storage diagnostics: default_key_id={key_id}, \
                     has_passphrase_info={has_passphrase}, input_len={input_len}. \
                     {}",
                        if has_passphrase {
                            "SDK will try passphrase derivation first; if your input is a base58 key the passphrase MAC will fail and the error you see may be the passphrase error rather than the base58 fallback's error."
                        } else {
                            "SDK will use base58 decoding directly."
                        }
                    )
                );
                let _ = SecretStorageKeyEventContent::TYPE; // keep import live
            }
            Ok(None) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"key_id": key_id})),
                    "matrix: default key id has no corresponding key event on the account — secret storage is in an inconsistent state. Re-running Secure Backup setup in Element will repair this."
                );
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({"error": format!("{}", e), "key_id": key_id})
                        ),
                    "matrix: failed to fetch key event for"
                );
            }
        }
    }

    pub(super) fn normalize_recipient(id_or_alias: &str) -> (&str, bool) {
        if !id_or_alias.contains("||") {
            return (id_or_alias, false);
        }
        let chosen = id_or_alias
            .split("||")
            .map(str::trim)
            .filter(|s| s.starts_with('!') || s.starts_with('#'))
            .last()
            .unwrap_or(id_or_alias);
        (chosen, true)
    }

    pub(super) async fn resolve_room(
        client: &Client,
        cache: &Arc<RwLock<HashMap<String, OwnedRoomId>>>,
        id_or_alias: &str,
    ) -> Result<OwnedRoomId> {
        let (id_or_alias, normalized) = normalize_recipient(id_or_alias);
        if normalized {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"id_or_alias": id_or_alias})),
                "matrix: recipient contains `||`; using as the room target. Update channels.matrix or cron `delivery.to` to a plain room id/alias to silence this warning."
            );
        }
        if id_or_alias.starts_with('!') {
            return id_or_alias
                .parse::<matrix_sdk::ruma::OwnedRoomId>()
                .with_context(|| format!("parse room id {id_or_alias}"));
        }
        if !id_or_alias.starts_with('#') {
            bail!("matrix: not a room id or alias: {id_or_alias}");
        }
        if let Some(id) = cache.read().await.get(id_or_alias) {
            return Ok(id.clone());
        }
        let alias: &RoomAliasId = id_or_alias
            .try_into()
            .with_context(|| format!("parse room alias {id_or_alias}"))?;
        let resp = client
            .resolve_room_alias(alias)
            .await
            .with_context(|| format!("resolve room alias {id_or_alias}"))?;
        cache
            .write()
            .await
            .insert(id_or_alias.to_string(), resp.room_id.clone());
        Ok(resp.room_id)
    }
}

// ─── inbound ───────────────────────────────────────────────────────────────
mod inbound {
    use std::{
        collections::{HashMap, HashSet},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::{Duration, SystemTime},
    };

    use matrix_sdk::{
        Client, Room, RoomState,
        config::SyncSettings,
        event_handler::RawEvent,
        ruma::{
            OwnedEventId, OwnedUserId,
            events::{
                AnySyncTimelineEvent,
                reaction::ReactionEventContent,
                relation::Annotation,
                room::{
                    encrypted::OriginalSyncRoomEncryptedEvent,
                    message::{MessageType, OriginalSyncRoomMessageEvent},
                },
            },
            serde::Raw,
        },
    };
    use serde_json::Value as JsonValue;
    use tokio::sync::{Mutex as TokioMutex, RwLock as TokioRwLock, mpsc, oneshot};

    use super::{allowlist, approval, context as ctx_mod, mention};
    use crate::transcription::TranscriptionManager;
    use zeroclaw_api::{
        channel::{ChannelApprovalResponse, ChannelMessage},
        media::MediaAttachment,
    };
    use zeroclaw_config::schema::{MatrixConfig, TranscriptionConfig};

    pub(super) const SYNC_LONGPOLL_TIMEOUT: Duration = Duration::from_secs(30);

    #[derive(Clone)]
    pub(super) struct HandlerCtx {
        pub config: Arc<MatrixConfig>,
        /// ZeroClaw alias for `[channels.matrix.<alias>]` so session_key
        /// construction can scope by bot instance.
        pub alias: String,
        /// Resolves inbound external peers from canonical state at message-time.
        /// No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
        pub peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
        pub transcription: Option<Arc<TranscriptionConfig>>,
        pub workspace_dir: Option<Arc<std::path::PathBuf>>,
        pub tx: mpsc::Sender<ChannelMessage>,
        pub pending_approvals:
            Arc<TokioMutex<HashMap<String, oneshot::Sender<ChannelApprovalResponse>>>>,
        pub threads_seen: Arc<TokioRwLock<HashSet<OwnedEventId>>>,
        pub bot_user_id: OwnedUserId,
        pub bot_display_name: Arc<TokioRwLock<Option<String>>>,
        pub initial_sync_done: Arc<AtomicBool>,
        /// Event ids of inbound events that arrived as `m.room.encrypted` and
        /// could not be decrypted. Tracked so the bot reacts ❓ exactly once
        /// per event across sync catchup deliveries.
        pub undecryptable_seen: Arc<TokioMutex<HashSet<OwnedEventId>>>,
    }

    pub(super) async fn run_sync_loop(client: Client, ctx: HandlerCtx) -> anyhow::Result<()> {
        let handler_ctx = ctx.clone();
        let message_handler = client.add_event_handler(
            move |ev: OriginalSyncRoomMessageEvent, room: Room, raw: RawEvent| {
                let ctx = handler_ctx.clone();
                async move {
                    if let Err(e) = handle_message(ctx, ev, room, raw).await {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "matrix: handle_message failed"
                        );
                    }
                }
            },
        );
        let _message_handler_guard = client.event_handler_drop_guard(message_handler);

        // Surface inbound events the SDK couldn't decrypt by reacting ❓ on
        // the encrypted event so the operator notices a key gap in chat
        // instead of silent dropping. Best-effort: prophylactic in normally-
        // healthy rooms where decryption succeeds.
        let encrypted_ctx = ctx.clone();
        let encrypted_handler =
            client.add_event_handler(move |ev: OriginalSyncRoomEncryptedEvent, room: Room| {
                let ctx = encrypted_ctx.clone();
                async move {
                    handle_undecryptable(ctx, ev, room).await;
                }
            });
        let _encrypted_handler_guard = client.event_handler_drop_guard(encrypted_handler);

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "matrix: starting sync loop"
        );
        let sync_settings = SyncSettings::default().timeout(SYNC_LONGPOLL_TIMEOUT);
        if let Err(e) = client.sync_once(sync_settings.clone()).await {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "phase": "initial_sync",
                        "error": format!("{}", e),
                    })),
                "matrix: initial sync failed"
            );
            return Err(anyhow::Error::msg(format!(
                "matrix initial sync failed: {e}"
            )));
        }
        ctx.initial_sync_done.store(true, Ordering::SeqCst);
        client.sync(sync_settings).await.map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "phase": "sync_loop",
                        "error": format!("{}", e),
                    })),
                "matrix: sync loop failed"
            );
            anyhow::Error::msg(format!("matrix sync loop failed: {e}"))
        })
    }

    /// React ❓ on any inbound event the SDK delivered as still-encrypted
    /// (decryption failed or no keys available). Skips the bot's own
    /// events, non-Joined rooms, and any event already reacted to in this
    /// process. Reaction send failures are warn-logged, not propagated.
    async fn handle_undecryptable(ctx: HandlerCtx, ev: OriginalSyncRoomEncryptedEvent, room: Room) {
        if room.state() != RoomState::Joined {
            return;
        }
        if ev.sender == ctx.bot_user_id {
            return;
        }
        let event_id = ev.event_id.clone();
        let already = {
            let mut seen = ctx.undecryptable_seen.lock().await;
            !seen.insert(event_id.clone())
        };
        if already {
            return;
        }
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "matrix: reacting ❓ to undecryptable event {} from {}",
                event_id, ev.sender
            )
        );
        let content =
            ReactionEventContent::new(Annotation::new(event_id.clone(), "❓".to_string()));
        if let Err(e) = room.send(content).await {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(
                        ::serde_json::json!({"error": format!("{}", e), "event_id": event_id})
                    ),
                "matrix: failed to react ❓ on undecryptable event"
            );
        }
    }

    async fn handle_message(
        ctx: HandlerCtx,
        ev: OriginalSyncRoomMessageEvent,
        room: Room,
        raw: RawEvent,
    ) -> anyhow::Result<()> {
        if room.state() != RoomState::Joined {
            return Ok(());
        }
        if ev.sender == ctx.bot_user_id {
            return Ok(());
        }

        let body = ctx_mod::body_for(&ev.content.msgtype);
        let sender = ev.sender.as_str();
        let room_id = room.room_id().as_str();

        // Approval reply has highest priority — operator answer must work even
        // if the room/user filters would otherwise drop the message.
        if let Some((token, response)) = approval::parse_reply(&body) {
            let waiter = ctx.pending_approvals.lock().await.remove(&token);
            if let Some(tx) = waiter {
                let _ = tx.send(response);
                return Ok(());
            }
        }

        let allowed_peers = (ctx.peer_resolver)();
        if !allowlist::user_allowed(&allowed_peers, sender) {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"sender": sender})),
                "matrix: drop message from non-allowed sender"
            );
            return Ok(());
        }
        if !allowlist::room_allowed_static(&ctx.config.allowed_rooms, room_id) {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"room_id": room_id})),
                "matrix: drop message from non-allowed room"
            );
            return Ok(());
        }

        if ctx.config.mention_only && is_group_room(&room).await {
            let display_name = ctx.bot_display_name.read().await.clone();
            let mention_user_ids = extract_mentions_user_ids(&raw);
            if !mention::is_mentioned(
                &ctx.bot_user_id,
                display_name.as_deref(),
                mention_user_ids.as_deref(),
                &body,
            ) {
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"sender": sender})),
                    "matrix: drop unmentioned message from"
                );
                return Ok(());
            }
        }

        let thread_id = extract_thread_id(&raw);
        let mut content = body.clone();
        if let Some(tid) = thread_id.as_ref()
            && ctx_mod::claim_first_visit(&ctx.threads_seen, tid).await
        {
            match room.event(tid, None).await {
                Ok(timeline_event) => {
                    if let Some((root_sender, root_body)) =
                        extract_root_summary(timeline_event.into_raw())
                    {
                        content = format!(
                            "{}{}",
                            ctx_mod::format_preamble(&root_sender, &root_body),
                            content
                        );
                    }
                }
                Err(e) => ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e), "tid": tid})),
                    "matrix: failed to fetch thread root"
                ),
            }
        }

        let media_kind = match &ev.content.msgtype {
            MessageType::Image(m) => Some(MediaInfo::new(
                m.source.clone(),
                m.body.clone(),
                m.info.as_ref().and_then(|i| i.mimetype.clone()),
                MediaCategory::Image,
            )),
            MessageType::File(m) => Some(MediaInfo::new(
                m.source.clone(),
                m.body.clone(),
                m.info.as_ref().and_then(|i| i.mimetype.clone()),
                MediaCategory::File,
            )),
            MessageType::Video(m) => Some(MediaInfo::new(
                m.source.clone(),
                m.body.clone(),
                m.info.as_ref().and_then(|i| i.mimetype.clone()),
                MediaCategory::Video,
            )),
            MessageType::Audio(m) => {
                let kind = if is_voice_message(&raw) {
                    MediaCategory::Voice
                } else {
                    MediaCategory::Audio
                };
                let mime = m.info.as_ref().and_then(|i| i.mimetype.clone());
                let file_name = m
                    .filename
                    .as_deref()
                    .filter(|f| !f.is_empty())
                    .unwrap_or(&m.body)
                    .to_string();
                Some(MediaInfo::new(m.source.clone(), file_name, mime, kind))
            }
            _ => None,
        };

        if let Some(info) = media_kind {
            content = attach_media(
                &room,
                &info,
                ctx.workspace_dir.as_deref(),
                &body,
                content,
                ctx.transcription.as_deref(),
            )
            .await;
        } else if let Some(reply_target) = extract_in_reply_to(&raw) {
            match room.event(&reply_target, None).await {
                Ok(timeline_event) => {
                    if let Some(info) = parent_media_info(timeline_event.into_raw()) {
                        content = attach_media(
                            &room,
                            &info,
                            ctx.workspace_dir.as_deref(),
                            "",
                            content,
                            ctx.transcription.as_deref(),
                        )
                        .await;
                    }
                }
                Err(e) => {
                    ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"error": format!("{}", e), "reply_target": reply_target})), "matrix: could not fetch in_reply_to parent")
                }
            }
        }
        let attachments: Vec<MediaAttachment> = Vec::new();

        let outbound_anchor =
            resolve_outbound_anchor(thread_id.as_ref(), &ev.event_id, ctx.config.reply_in_thread);
        // When the bot is the one starting the thread, mark its root seen
        // so the next inbound that lands inside it does not re-fetch and
        // re-inject a root preamble (the agent already saw the root in this
        // same turn).
        if thread_id.is_none() && ctx.config.reply_in_thread {
            ctx_mod::mark_seen(&ctx.threads_seen, ev.event_id.clone()).await;
        }

        let interruption_scope =
            interruption_scope_from_anchor(outbound_anchor.as_deref(), &ev.event_id);

        let msg = ChannelMessage {
            id: ev.event_id.to_string(),
            sender: sender.to_string(),
            reply_target: room.room_id().to_string(),
            content,
            channel: "matrix".to_string(),
            channel_alias: Some(ctx.alias.clone()),
            timestamp: SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            thread_ts: outbound_anchor.clone(),
            interruption_scope_id: interruption_scope,
            attachments,
            subject: None,

            ..Default::default()
        };

        if let Err(e) = ctx.tx.send(msg).await {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "matrix: failed to forward inbound message"
            );
        }
        Ok(())
    }

    async fn is_group_room(room: &Room) -> bool {
        !matches!(room.is_direct().await, Ok(true))
    }

    pub(super) fn extract_mentions_user_ids(raw: &RawEvent) -> Option<Vec<String>> {
        let v: JsonValue = serde_json::from_str(raw.get()).ok()?;
        let mentions = v.get("content")?.get("m.mentions")?;
        let arr = mentions.get("user_ids")?.as_array()?;
        Some(
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect(),
        )
    }

    pub(super) fn resolve_outbound_anchor(
        thread_id: Option<&OwnedEventId>,
        event_id: &OwnedEventId,
        reply_in_thread: bool,
    ) -> Option<String> {
        thread_id.map(ToString::to_string).or_else(|| {
            if reply_in_thread {
                Some(event_id.to_string())
            } else {
                None
            }
        })
    }

    pub(super) fn interruption_scope_from_anchor(
        outbound_anchor: Option<&str>,
        event_id: &OwnedEventId,
    ) -> Option<String> {
        match outbound_anchor {
            Some(anchor) if anchor == event_id.as_str() => None,
            other => other.map(ToString::to_string),
        }
    }

    pub(super) fn extract_thread_id(raw: &RawEvent) -> Option<OwnedEventId> {
        let v: JsonValue = serde_json::from_str(raw.get()).ok()?;
        let relates = v.get("content")?.get("m.relates_to")?;
        let rel_type = relates.get("rel_type")?.as_str()?;
        if rel_type != "m.thread" {
            return None;
        }
        let root = relates.get("event_id")?.as_str()?;
        root.parse().ok()
    }

    pub(super) fn extract_in_reply_to(raw: &RawEvent) -> Option<OwnedEventId> {
        let v: JsonValue = serde_json::from_str(raw.get()).ok()?;
        let relates = v.get("content")?.get("m.relates_to")?;
        let in_reply_to = relates.get("m.in_reply_to")?;
        let event_id = in_reply_to.get("event_id")?.as_str()?;
        event_id.parse().ok()
    }

    pub(super) fn is_voice_message(raw: &RawEvent) -> bool {
        let v: JsonValue = match serde_json::from_str(raw.get()) {
            Ok(v) => v,
            Err(_) => return false,
        };
        v.get("content")
            .and_then(|c| c.get("org.matrix.msc3245.voice"))
            .is_some()
    }

    fn extract_root_summary(raw: Raw<AnySyncTimelineEvent>) -> Option<(String, String)> {
        let json: JsonValue = serde_json::from_str(raw.json().get()).ok()?;
        let sender = json.get("sender")?.as_str()?.to_string();
        let body = json
            .get("content")
            .and_then(|c| c.get("body"))
            .and_then(|b| b.as_str())
            .unwrap_or("")
            .to_string();
        Some((sender, body))
    }

    pub(super) enum MediaCategory {
        Image,
        Video,
        Audio,
        Voice,
        File,
    }

    pub(super) fn should_transcribe(
        kind: &MediaCategory,
        transcription: Option<&TranscriptionConfig>,
    ) -> bool {
        matches!(kind, MediaCategory::Voice) && matches!(transcription, Some(t) if t.enabled)
    }

    async fn attach_media(
        room: &Room,
        info: &MediaInfo,
        workspace_dir: Option<&std::path::PathBuf>,
        body_hint: &str,
        content: String,
        transcription: Option<&TranscriptionConfig>,
    ) -> String {
        let mut content = content;
        match save_media_to_workspace(room, info, workspace_dir).await {
            Ok(Some(path)) => {
                let marker = format_media_marker(info, &path);
                let placeholder = matches!(body_hint, "[image]" | "[file]" | "[audio]" | "[video]");
                content = if body_hint.is_empty() {
                    if content.is_empty() {
                        marker
                    } else {
                        format!("{content}\n\n{marker}")
                    }
                } else if placeholder || body_hint == info.file_name || content == body_hint {
                    marker
                } else {
                    format!("{content}\n\n{marker}")
                };

                if should_transcribe(&info.kind, transcription) {
                    let t = transcription.expect("should_transcribe guarantees Some");
                    let transcribe_name = transcription_safe_filename(
                        &info.file_name,
                        info.mime.as_deref(),
                        &info.kind,
                    );
                    match transcribe_from_disk(t, &path, &transcribe_name).await {
                        Ok(text) if !text.trim().is_empty() => {
                            content = format!("[voice transcript]: {text}\n\n{content}");
                        }
                        Ok(_) => {}
                        Err(e) => ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "matrix: voice transcription failed"
                        ),
                    }
                }
            }
            Ok(None) => {}
            Err(e) => ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                "matrix: media handling failed"
            ),
        }
        content
    }

    /// Walk a fetched timeline event's raw JSON looking for a media-typed
    /// `m.room.message` payload. Returns `None` if the event is not a
    /// recognized media message.
    pub(super) fn parent_media_info(
        raw: matrix_sdk::ruma::serde::Raw<matrix_sdk::ruma::events::AnySyncTimelineEvent>,
    ) -> Option<MediaInfo> {
        let json: JsonValue = serde_json::from_str(raw.json().get()).ok()?;
        let content = json.get("content")?;
        let msgtype = content.get("msgtype")?.as_str()?;
        let kind = match msgtype {
            "m.image" => MediaCategory::Image,
            "m.video" => MediaCategory::Video,
            "m.audio" if content.get("org.matrix.msc3245.voice").is_some() => MediaCategory::Voice,
            "m.audio" => MediaCategory::Audio,
            "m.file" => MediaCategory::File,
            _ => return None,
        };
        let body_str = content
            .get("body")
            .and_then(|b| b.as_str())
            .unwrap_or("attachment");
        let filename_field = content.get("filename").and_then(|f| f.as_str());
        let mime = content
            .get("info")
            .and_then(|i| i.get("mimetype"))
            .and_then(|m| m.as_str());
        let file_name = filename_field
            .filter(|f| !f.is_empty())
            .unwrap_or(body_str)
            .to_string();
        let mime = mime.map(String::from);
        let source = if let Some(file) = content.get("file") {
            // Encrypted media: rebuild MediaSource::Encrypted from JSON.
            let encrypted: matrix_sdk::ruma::events::room::EncryptedFile =
                serde_json::from_value(file.clone()).ok()?;
            matrix_sdk::ruma::events::room::MediaSource::Encrypted(Box::new(encrypted))
        } else if let Some(url) = content.get("url").and_then(|u| u.as_str()) {
            matrix_sdk::ruma::events::room::MediaSource::Plain(matrix_sdk::ruma::OwnedMxcUri::from(
                url,
            ))
        } else {
            return None;
        };
        Some(MediaInfo::new(source, file_name, mime, kind))
    }

    pub(super) struct MediaInfo {
        pub source: matrix_sdk::ruma::events::room::MediaSource,
        pub file_name: String,
        pub mime: Option<String>,
        pub kind: MediaCategory,
    }

    impl MediaInfo {
        pub fn new(
            source: matrix_sdk::ruma::events::room::MediaSource,
            file_name: String,
            mime: Option<String>,
            kind: MediaCategory,
        ) -> Self {
            Self {
                source,
                file_name,
                mime,
                kind,
            }
        }
    }

    /// Download an inbound media file, persist it to `{workspace}/matrix_files/`,
    /// and return the on-disk path. Returns `Ok(None)` when no `workspace_dir`
    /// is configured (caller logs and falls back to the placeholder body).
    async fn save_media_to_workspace(
        room: &Room,
        info: &MediaInfo,
        workspace: Option<&std::path::PathBuf>,
    ) -> anyhow::Result<Option<std::path::PathBuf>> {
        let Some(workspace) = workspace else {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                &format!(
                    "matrix: cannot persist {} — channels.matrix workspace_dir not configured. Set ZEROCLAW_DIR or run via the orchestrator.",
                    info.file_name
                )
            );
            return Ok(None);
        };
        let dir = workspace.join("matrix_files");
        std::fs::create_dir_all(&dir).map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": dir.display().to_string(),
                        "phase": "media_dir_create",
                        "error": format!("{}", e),
                    })),
                "matrix: failed to create media dir"
            );
            anyhow::Error::msg(format!("create {}: {e}", dir.display()))
        })?;
        let request = matrix_sdk::media::MediaRequestParameters {
            source: info.source.clone(),
            format: matrix_sdk::media::MediaFormat::File,
        };
        let source_kind = match &info.source {
            matrix_sdk::ruma::events::room::MediaSource::Plain(_) => "plain",
            matrix_sdk::ruma::events::room::MediaSource::Encrypted(_) => "encrypted",
        };
        let bytes = room
            .client()
            .media()
            .get_media_content(&request, true)
            .await
            .map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "get_media_content ()"
                );
                anyhow::Error::msg(format!("get_media_content ({source_kind}): {e}"))
            })?;

        let safe_name = sanitize_filename(&info.file_name, &info.kind, info.mime.as_deref());
        // Disambiguate by uuid prefix to avoid collisions across messages.
        let unique = format!("{}_{safe_name}", uuid::Uuid::new_v4().simple());
        let path = dir.join(unique);
        std::fs::write(&path, &bytes).map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": path.display().to_string(),
                        "phase": "media_write",
                        "error": format!("{}", e),
                    })),
                "matrix: failed to write media file"
            );
            anyhow::Error::msg(format!("write {}: {e}", path.display()))
        })?;
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            &format!(
                "matrix: saved {} bytes ({}) to {}",
                bytes.len(),
                source_kind,
                path.display()
            )
        );
        Ok(Some(path))
    }

    fn sanitize_filename(raw: &str, kind: &MediaCategory, mime: Option<&str>) -> String {
        let trimmed = raw.trim();
        let candidate = if trimmed.is_empty() || trimmed.starts_with('[') {
            // Placeholder body or empty — synthesise a sensible name.
            let ext = default_extension(kind, mime);
            format!("matrix_media.{ext}")
        } else {
            trimmed.to_string()
        };
        candidate
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }

    fn default_extension(kind: &MediaCategory, mime: Option<&str>) -> &'static str {
        // Some Matrix clients append codec params (e.g. "audio/ogg; codecs=opus");
        // match on the primary type only, mirroring `mime_for_audio` in transcription.rs.
        let mime = mime.map(|m| m.split(';').next().unwrap_or(m).trim());
        if let Some(m) = mime {
            match m {
                "image/png" => return "png",
                "image/jpeg" | "image/jpg" => return "jpg",
                "image/gif" => return "gif",
                "image/webp" => return "webp",
                "video/mp4" => return "mp4",
                "audio/ogg" => return "ogg",
                "audio/mpeg" | "audio/mp3" => return "mp3",
                "audio/wav" => return "wav",
                "audio/flac" => return "flac",
                "audio/opus" => return "opus",
                "audio/webm" => return "webm",
                "audio/mp4" | "audio/x-m4a" => return "m4a",
                "application/pdf" => return "pdf",
                _ => {}
            }
        }
        match kind {
            MediaCategory::Image => "jpg",
            MediaCategory::Video => "mp4",
            MediaCategory::Audio | MediaCategory::Voice => "ogg",
            MediaCategory::File => "bin",
        }
    }

    /// Internal name for `transcribe()` only — never stored or shown.
    pub(super) fn transcription_safe_filename(
        file_name: &str,
        mime: Option<&str>,
        kind: &MediaCategory,
    ) -> String {
        let accepted = file_name
            .rsplit_once('.')
            .is_some_and(|(_, ext)| is_transcription_accepted_extension(ext));
        if accepted {
            return file_name.to_string();
        }
        format!("attachment.{}", default_extension(kind, mime))
    }

    // Mirrors `mime_for_audio`'s accepted set in transcription.rs.
    fn is_transcription_accepted_extension(ext: &str) -> bool {
        matches!(
            ext.to_ascii_lowercase().as_str(),
            "flac"
                | "mp3"
                | "mpeg"
                | "mpga"
                | "mp4"
                | "m4a"
                | "ogg"
                | "oga"
                | "opus"
                | "wav"
                | "webm"
        )
    }

    fn format_media_marker(info: &MediaInfo, path: &std::path::Path) -> String {
        match info.kind {
            MediaCategory::Image => format!("[IMAGE:{}]", path.display()),
            _ => {
                let display_name = if info.file_name.trim().is_empty() {
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("attachment")
                        .to_string()
                } else {
                    info.file_name.clone()
                };
                format!("[Document: {display_name}] {}", path.display())
            }
        }
    }

    async fn transcribe_from_disk(
        config: &TranscriptionConfig,
        path: &std::path::Path,
        file_name: &str,
    ) -> anyhow::Result<String> {
        let bytes = std::fs::read(path).map_err(|e| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "path": path.display().to_string(),
                        "phase": "transcription_read",
                        "error": format!("{}", e),
                    })),
                "matrix: failed to read media file for transcription"
            );
            anyhow::Error::msg(format!("read {}: {e}", path.display()))
        })?;
        let manager = build_transcription_manager(config)?;
        manager.transcribe(&bytes, file_name).await
    }

    /// Binds the sole registered provider as the agent alias when exactly one
    /// is configured; multi-provider setups keep the alias empty (unsupported).
    pub(super) fn build_transcription_manager(
        config: &TranscriptionConfig,
    ) -> anyhow::Result<TranscriptionManager> {
        let manager = TranscriptionManager::new(config)?;
        let sole_provider = match manager.available_providers().as_slice() {
            [only] => Some((*only).to_string()),
            _ => None,
        };
        Ok(match sole_provider {
            Some(alias) => manager.with_agent_transcription_provider(alias),
            None => manager,
        })
    }
}

// ─── outbound ──────────────────────────────────────────────────────────────
mod outbound {
    use std::{collections::HashMap, sync::Arc};

    use anyhow::{Context as _, Result, bail};
    use futures_util::StreamExt;
    use matrix_sdk::{
        Client, Room, RoomState,
        attachment::{
            AttachmentConfig, AttachmentInfo, BaseAudioInfo, BaseFileInfo, BaseImageInfo,
            BaseVideoInfo,
        },
        room::{
            edit::EditedContent,
            reply::{EnforceThread, Reply},
        },
        ruma::{
            OwnedEventId, OwnedRoomId, UInt,
            events::{
                reaction::ReactionEventContent,
                relation::Annotation,
                room::message::{
                    AddMentions, MessageType, ReplyWithinThread, RoomMessageEventContent,
                    RoomMessageEventContentWithoutRelation, TextMessageEventContent,
                },
            },
        },
    };
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;
    use std::time::Duration;
    use tokio::sync::{Mutex as TokioMutex, RwLock as TokioRwLock};

    use super::{client, context as ctx_mod, markers};
    use zeroclaw_api::{channel::SendMessage, media::MediaAttachment};

    pub(super) type ReactionKey = (OwnedRoomId, OwnedEventId, String);

    pub(super) struct Outbox<'a> {
        pub client: &'a Client,
        pub alias_cache: &'a Arc<TokioRwLock<HashMap<String, OwnedRoomId>>>,
        pub threads_seen: &'a Arc<TokioRwLock<std::collections::HashSet<OwnedEventId>>>,
        pub reaction_log: &'a Arc<TokioMutex<HashMap<ReactionKey, OwnedEventId>>>,
        pub reply_in_thread: bool,
        pub workspace_dir: Option<&'a Path>,
    }

    /// What `outbound::send` should do once all attachment uploads are done
    /// and the marker-stripped text is in hand. Extracted as a small enum so
    /// the empty-text-with-attachments contract can be unit-tested without
    /// the SDK in the loop.
    #[derive(Debug, PartialEq, Eq)]
    pub(super) enum SendOutcome {
        /// Text is non-empty (with or without prior attachments). Caller
        /// proceeds to send the text message and returns its event_id.
        SendText,
        /// Text is empty but at least one attachment uploaded successfully.
        /// Caller skips the text send and returns the carried event_id.
        ReturnAttachment,
        /// Text is empty AND no attachment landed. Caller surfaces an error
        /// to the runtime so it can decide what to do.
        EmptyError,
    }

    /// Decide what `outbound::send` should do given the post-marker-strip
    /// text and whether at least one attachment landed. Pure function.
    pub(super) fn decide_send_outcome(
        text_is_empty_after_strip: bool,
        any_attachment_landed: bool,
    ) -> SendOutcome {
        match (text_is_empty_after_strip, any_attachment_landed) {
            (false, _) => SendOutcome::SendText,
            (true, true) => SendOutcome::ReturnAttachment,
            (true, false) => SendOutcome::EmptyError,
        }
    }

    /// Why a marker upload didn't reach the room. Drives both the textual
    /// "(note: I couldn't deliver…)" line and the emoji reactions on the
    /// agent's outgoing message so a chatter sees a hard refusal at a glance.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum MarkerFailure {
        /// Trust-boundary refusal: `validate_marker_target` rejected the
        /// target (path escapes workspace, disallowed scheme, etc.). The bot
        /// deliberately did not attempt the fetch.
        Refused,
        /// Post-validation failure: fetch error, file not found, upload
        /// rejected by the server, oversize body, timeout. The bot tried and
        /// couldn't complete the delivery.
        Failed,
    }

    pub(super) struct AttachmentDelivery {
        pub text: String,
        pub last_attachment_id: Option<OwnedEventId>,
        pub failed_markers: Vec<(String, MarkerFailure)>,
    }

    impl AttachmentDelivery {
        pub(super) fn failure_kinds(&self) -> Vec<MarkerFailure> {
            self.failed_markers.iter().map(|(_, kind)| *kind).collect()
        }
    }

    /// Pick the emoji reactions to apply to the agent's outgoing text/event
    /// based on which kinds of marker failures occurred. 🚫 means the bot
    /// refused for safety; ⚠️ means it tried and didn't make it. Both can
    /// fire on the same message when a batch mixes refusals and failures.
    pub(super) fn decide_reactions(failures: &[MarkerFailure]) -> Vec<&'static str> {
        let mut out = Vec::new();
        if failures.iter().any(|f| matches!(f, MarkerFailure::Refused)) {
            out.push("🚫");
        }
        if failures.iter().any(|f| matches!(f, MarkerFailure::Failed)) {
            out.push("⚠️");
        }
        out
    }

    /// 8 MiB cap on the body of an HTTP marker fetch. Matches WebFetchTool's
    /// streaming-cap pattern in `crates/zeroclaw-tools/src/web_fetch.rs`.
    const MAX_MARKER_BYTES: usize = 8 * 1024 * 1024;
    /// 30-second connect+request timeout for HTTP marker fetches. Bounds the
    /// agent-driven fetch path so a hung target cannot stall the channel.
    const MARKER_HTTP_TIMEOUT: Duration = Duration::from_secs(30);

    /// Resolved marker fetch target after sandboxing. `Local` paths are
    /// canonicalised and proven to live within the configured `workspace_dir`.
    /// `Http` URLs have an explicit `http`/`https` scheme.
    #[derive(Debug)]
    pub(super) enum MarkerTarget {
        Local(PathBuf),
        Http(reqwest::Url),
    }

    #[derive(Debug)]
    pub(super) enum ValidateError {
        /// Trust-boundary refusal: disallowed scheme, no workspace
        /// configured, or path resolved outside the workspace. The target
        /// was a real, reachable resource that policy declined.
        Refused(anyhow::Error),
        /// The path didn't resolve to anything on disk (ENOENT or similar
        /// during canonicalize). Treated as a delivery failure, not a
        /// safety event.
        NotFound(anyhow::Error),
    }

    impl std::fmt::Display for ValidateError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                ValidateError::Refused(e) | ValidateError::NotFound(e) => write!(f, "{e}"),
            }
        }
    }

    impl ValidateError {
        pub(super) fn as_marker_failure(&self) -> MarkerFailure {
            match self {
                ValidateError::Refused(_) => MarkerFailure::Refused,
                ValidateError::NotFound(_) => MarkerFailure::Failed,
            }
        }
    }

    pub(super) fn validate_marker_target(
        target: &str,
        workspace_dir: Option<&Path>,
    ) -> std::result::Result<MarkerTarget, ValidateError> {
        if target.starts_with("http://") || target.starts_with("https://") {
            let url = reqwest::Url::parse(target)
                .with_context(|| format!("parse marker URL {target}"))
                .map_err(ValidateError::Refused)?;
            let host_str = url.host_str().unwrap_or("");
            if zeroclaw_tools::helpers::domain_guard::is_private_or_local_host(host_str) {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "target": target,
                            "host": host_str,
                            "reason": "ssrf_private_host",
                        })),
                    "matrix: marker target points to a private/local host"
                );
                return Err(ValidateError::Refused(anyhow::Error::msg(format!(
                    "matrix: marker target {target} resolves to a private or local host ({host_str}); refusing for SSRF safety. \
                     Use a public URL or attach the file from workspace_dir directly."
                ))));
            }
            return Ok(MarkerTarget::Http(url));
        }
        if target.contains("://") {
            let scheme = target.split("://").next().unwrap_or("?");
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "scheme": scheme,
                        "target": target,
                    })),
                "matrix: marker target uses disallowed scheme"
            );
            return Err(ValidateError::Refused(anyhow::Error::msg(format!(
                "matrix: marker target uses disallowed scheme {scheme:?}; only http/https and workspace-relative paths are accepted"
            ))));
        }
        if target.starts_with("data:") || target.starts_with("file:") {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "target": target,
                    })),
                "matrix: marker target uses disallowed data: or file: scheme"
            );
            return Err(ValidateError::Refused(anyhow::Error::msg(
                "matrix: marker target uses disallowed scheme; only http/https and workspace-relative paths are accepted",
            )));
        }

        let workspace = workspace_dir.ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "target": target,
                        "reason": "no_workspace_dir",
                    })),
                "matrix: marker target is local path but channel has no workspace_dir"
            );
            ValidateError::Refused(anyhow::Error::msg(format!(
                "matrix: marker target {target} is a local path but the channel was started without a workspace_dir, refusing for safety"
            )))
        })?;
        let workspace_canon = std::fs::canonicalize(workspace)
            .with_context(|| format!("canonicalize workspace {}", workspace.display()))
            .map_err(ValidateError::Refused)?;

        let target_path = Path::new(target);
        let absolute = if target_path.is_absolute() {
            target_path.to_path_buf()
        } else {
            workspace_canon.join(target_path)
        };
        let target_canon = match std::fs::canonicalize(&absolute) {
            Ok(p) => p,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "target": target,
                            "reason": "not_found",
                        })),
                    "matrix: marker target not found on disk"
                );
                return Err(ValidateError::NotFound(anyhow::Error::msg(format!(
                    "matrix: marker target {target} not found on disk"
                ))));
            }
            Err(e) => {
                return Err(ValidateError::Refused(
                    anyhow::Error::from(e).context(format!("canonicalize marker target {target}")),
                ));
            }
        };

        if !target_canon.starts_with(&workspace_canon) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "target": target,
                        "target_canon": target_canon.display().to_string(),
                        "workspace_canon": workspace_canon.display().to_string(),
                        "reason": "outside_workspace",
                    })),
                "matrix: marker target escapes workspace_dir"
            );
            return Err(ValidateError::Refused(anyhow::Error::msg(format!(
                "matrix: marker target {target} resolves to {} which is outside workspace_dir {}; refusing",
                target_canon.display(),
                workspace_canon.display(),
            ))));
        }
        Ok(MarkerTarget::Local(target_canon))
    }

    /// Maximum number of redirects to follow on a marker HTTP fetch. The
    /// `Policy::custom` closure below rejects any redirect target whose host
    /// is private/local, so the cap mainly bounds the worst-case public-→-public
    /// chain an attacker can construct.
    const MAX_MARKER_REDIRECTS: usize = 10;

    fn marker_http_client() -> &'static reqwest::Client {
        static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
        CLIENT.get_or_init(|| {
            let redirect_policy = reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= MAX_MARKER_REDIRECTS {
                    return attempt.error(std::io::Error::other(format!(
                        "Too many marker redirects (max {MAX_MARKER_REDIRECTS})"
                    )));
                }
                // `attempt.url()` borrows the attempt, so we copy out the
                // bits we need into owned Strings before `attempt.error(...)`,
                // which moves the attempt, can run.
                let target_str = attempt.url().as_str().to_string();
                let host = attempt.url().host_str().unwrap_or("").to_string();
                if zeroclaw_tools::helpers::domain_guard::is_private_or_local_host(&host) {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "target": target_str,
                                "host": host,
                                "reason": "ssrf_redirect_to_private_host",
                            })),
                        "matrix: marker redirect targets a private/local host"
                    );
                    return attempt.error(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!(
                            "Blocked marker redirect to private or local host ({host}); \
                             refusing for SSRF safety. Use a public URL or attach the file \
                             from workspace_dir directly."
                        ),
                    ));
                }
                attempt.follow()
            });
            reqwest::Client::builder()
                .timeout(MARKER_HTTP_TIMEOUT)
                .redirect(redirect_policy)
                .user_agent("zeroclaw-matrix/1.0")
                .build()
                .expect("default reqwest client config never fails to build")
        })
    }

    pub(super) async fn fetch_http(url: reqwest::Url) -> Result<Vec<u8>> {
        let client = marker_http_client();
        let resp = client
            .get(url.clone())
            .send()
            .await
            .with_context(|| format!("fetch marker URL {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            bail!("matrix: marker URL {url} returned HTTP status {status}");
        }
        let mut stream = resp.bytes_stream();
        let mut buf = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.with_context(|| format!("stream chunk from {url}"))?;
            if buf.len().saturating_add(chunk.len()) > MAX_MARKER_BYTES {
                bail!("matrix: marker URL {url} exceeded {MAX_MARKER_BYTES}-byte cap; refusing");
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(buf)
    }

    pub(super) fn thread_anchor_from_message(
        outbox: &Outbox<'_>,
        message: &SendMessage,
    ) -> Option<OwnedEventId> {
        if outbox.reply_in_thread {
            message
                .thread_ts
                .as_deref()
                .filter(|s| !s.is_empty())
                .and_then(|s| s.parse().ok())
        } else {
            None
        }
    }

    pub(super) async fn deliver_attachments(
        outbox: &Outbox<'_>,
        room: &Room,
        mut text: String,
        markers: &[markers::Marker],
        attachments: &[MediaAttachment],
        thread_anchor: Option<&OwnedEventId>,
    ) -> Result<AttachmentDelivery> {
        let mut last_attachment_id: Option<OwnedEventId> = None;
        for att in attachments {
            let id = upload_attachment(room, att, AttachmentKind::Auto, thread_anchor).await?;
            last_attachment_id = Some(id);
        }

        // Track each failed marker with the reason: Refused (trust-boundary
        // rejection by validate_marker_target) vs Failed (everything else —
        // fetch error, upload rejection). Drives both the textual note and
        // the emoji reactions fired below.
        let mut failed_markers: Vec<(String, MarkerFailure)> = Vec::new();
        for marker in markers {
            let kind = match marker.kind {
                markers::MarkerKind::Image => AttachmentKind::Image,
                markers::MarkerKind::Audio => AttachmentKind::Audio,
                markers::MarkerKind::Video => AttachmentKind::Video,
                markers::MarkerKind::File => AttachmentKind::File,
                markers::MarkerKind::Voice => AttachmentKind::Voice,
            };
            let resolved = match validate_marker_target(&marker.target, outbox.workspace_dir) {
                Ok(t) => t,
                Err(e) => {
                    let kind = e.as_marker_failure();
                    let label = match kind {
                        MarkerFailure::Refused => "trust boundary",
                        MarkerFailure::Failed => "not found",
                    };
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        &format!(
                            "matrix: skipping outbound marker for {} ({label}): {e}",
                            marker.target
                        )
                    );
                    failed_markers.push((marker.target.clone(), kind));
                    continue;
                }
            };
            let bytes = match resolved {
                MarkerTarget::Local(path) => match tokio::fs::read(&path).await {
                    Ok(b) => b,
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                            &format!(
                                "matrix: skipping outbound marker for {} (read failed): {e}",
                                marker.target
                            )
                        );
                        failed_markers.push((marker.target.clone(), MarkerFailure::Failed));
                        continue;
                    }
                },
                MarkerTarget::Http(url) => match fetch_http(url).await {
                    Ok(b) => b,
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                            &format!(
                                "matrix: skipping outbound marker for {} (http failed): {e}",
                                marker.target
                            )
                        );
                        failed_markers.push((marker.target.clone(), MarkerFailure::Failed));
                        continue;
                    }
                },
            };
            let file_name = derive_file_name(&marker.target);
            let mime = mime_for(&file_name, &kind);
            let att = MediaAttachment {
                file_name,
                data: bytes,
                mime_type: Some(mime),
            };
            match upload_attachment(room, &att, kind, thread_anchor).await {
                Ok(id) => last_attachment_id = Some(id),
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                        &format!(
                            "matrix: skipping outbound marker for {} (upload failed): {e}",
                            marker.target
                        )
                    );
                    failed_markers.push((marker.target.clone(), MarkerFailure::Failed));
                }
            }
        }

        if !failed_markers.is_empty() {
            let targets: Vec<&str> = failed_markers.iter().map(|(t, _)| t.as_str()).collect();
            let note = if targets.len() == 1 {
                format!("(note: I couldn't deliver the file at {}.)", targets[0])
            } else {
                let joined = targets.join(", ");
                format!("(note: I couldn't deliver these files: {joined}.)")
            };
            text = if text.trim().is_empty() {
                note
            } else {
                format!("{text}\n\n{note}")
            };
        }

        Ok(AttachmentDelivery {
            text,
            last_attachment_id,
            failed_markers,
        })
    }

    pub(super) async fn send(outbox: &Outbox<'_>, message: &SendMessage) -> Result<OwnedEventId> {
        let room =
            resolve_joined_room(outbox.client, outbox.alias_cache, &message.recipient).await?;

        let (text, ms) = markers::parse(&message.content);

        // Build the thread anchor used by both attachment uploads and the
        // text reply, so attachments live in the same thread instead of
        // landing in the main timeline.
        let thread_anchor = thread_anchor_from_message(outbox, message);

        let delivery = deliver_attachments(
            outbox,
            &room,
            text,
            &ms,
            &message.attachments,
            thread_anchor.as_ref(),
        )
        .await?;

        // Decide whether to send the text, return the last attachment's
        // event_id, or surface an error. Marker-only messages used to error
        // here even though their attachment had landed; the runtime would
        // see Err and could retry, producing duplicate uploads.
        match decide_send_outcome(
            delivery.text.trim().is_empty(),
            delivery.last_attachment_id.is_some(),
        ) {
            SendOutcome::SendText => {}
            SendOutcome::ReturnAttachment => {
                // Safe by construction: ReturnAttachment is only returned
                // when last_attachment_id is Some.
                let kinds = delivery.failure_kinds();
                let attachment_id = delivery
                    .last_attachment_id
                    .expect("decide_send_outcome guarantees Some when ReturnAttachment");
                emit_failure_reactions(&room, &attachment_id, &kinds).await;
                return Ok(attachment_id);
            }
            SendOutcome::EmptyError => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"phase": "send"})),
                    "matrix: empty message body and no successful attachment"
                );
                return Err(anyhow::Error::msg(
                    "matrix: empty message body and no successful attachment",
                ));
            }
        }

        let content = RoomMessageEventContent::text_markdown(&delivery.text);

        let event_id = if let (true, Some(anchor)) = (
            outbox.reply_in_thread,
            message.thread_ts.as_deref().filter(|s| !s.is_empty()),
        ) {
            send_threaded_reply(&room, content, anchor, outbox.threads_seen).await?
        } else {
            room.send(content).await?.response.event_id
        };

        let kinds = delivery.failure_kinds();
        emit_failure_reactions(&room, &event_id, &kinds).await;

        Ok(event_id)
    }

    /// Best-effort: apply 🚫 / ⚠️ reactions to the bot's just-sent message
    /// based on which kinds of marker failures occurred. Reaction send
    /// failures are logged but never propagated — the primary message
    /// already landed.
    pub(super) async fn emit_failure_reactions(
        room: &Room,
        event_id: &OwnedEventId,
        failures: &[MarkerFailure],
    ) {
        for emoji in decide_reactions(failures) {
            let content =
                ReactionEventContent::new(Annotation::new(event_id.clone(), emoji.to_string()));
            if let Err(e) = room.send(content).await {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({"error": format!("{}", e), "emoji": emoji})
                        ),
                    "matrix: failed to send reaction on outgoing message"
                );
            }
        }
    }

    async fn send_threaded_reply(
        room: &Room,
        content: RoomMessageEventContent,
        anchor_id: &str,
        threads_seen: &Arc<TokioRwLock<std::collections::HashSet<OwnedEventId>>>,
    ) -> Result<OwnedEventId> {
        let anchor: OwnedEventId = anchor_id
            .parse()
            .with_context(|| format!("parse thread anchor {anchor_id}"))?;
        let without_relation = RoomMessageEventContentWithoutRelation::new(content.msgtype.clone());
        let reply_event = room
            .make_reply_event(
                without_relation,
                Reply {
                    event_id: anchor.clone(),
                    enforce_thread: EnforceThread::Threaded(ReplyWithinThread::No),
                    add_mentions: AddMentions::No,
                },
            )
            .await
            .map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "make_reply_event failed"
                );
                anyhow::Error::msg(format!("make_reply_event failed: {e}"))
            })?;
        ctx_mod::mark_seen(threads_seen, anchor).await;
        let resp = room.send(reply_event).await?;
        Ok(resp.response.event_id)
    }

    pub(super) async fn edit(
        client: &Client,
        room_id: &str,
        event_id: &OwnedEventId,
        text: &str,
    ) -> Result<()> {
        let room = client
            .get_room(&room_id.parse::<OwnedRoomId>()?)
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"room_id": room_id})),
                    "matrix: room not joined"
                );
                anyhow::Error::msg(format!("matrix: room not joined: {room_id}"))
            })?;
        let new_content = RoomMessageEventContentWithoutRelation::new(MessageType::Text(
            TextMessageEventContent::markdown(text),
        ));
        let edit_event = room
            .make_edit_event(event_id, EditedContent::RoomMessage(new_content))
            .await
            .map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "make_edit_event failed"
                );
                anyhow::Error::msg(format!("make_edit_event failed: {e}"))
            })?;
        room.send(edit_event).await?;
        Ok(())
    }

    pub(super) async fn redact(
        client: &Client,
        room_id: &str,
        event_id: &OwnedEventId,
        reason: Option<String>,
    ) -> Result<()> {
        let room = client
            .get_room(&room_id.parse::<OwnedRoomId>()?)
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"room_id": room_id})),
                    "matrix: room not joined"
                );
                anyhow::Error::msg(format!("matrix: room not joined: {room_id}"))
            })?;
        room.redact(event_id, reason.as_deref(), None).await?;
        Ok(())
    }

    pub(super) async fn react(
        outbox: &Outbox<'_>,
        room_id: &str,
        event_id: &OwnedEventId,
        emoji: &str,
    ) -> Result<()> {
        let room = resolve_joined_room(outbox.client, outbox.alias_cache, room_id).await?;
        let content =
            ReactionEventContent::new(Annotation::new(event_id.clone(), emoji.to_string()));
        let resp = room.send(content).await?;
        outbox.reaction_log.lock().await.insert(
            (
                room.room_id().to_owned(),
                event_id.clone(),
                emoji.to_string(),
            ),
            resp.response.event_id,
        );
        Ok(())
    }

    pub(super) async fn unreact(
        outbox: &Outbox<'_>,
        room_id: &str,
        event_id: &OwnedEventId,
        emoji: &str,
    ) -> Result<()> {
        let room = resolve_joined_room(outbox.client, outbox.alias_cache, room_id).await?;
        let key = (
            room.room_id().to_owned(),
            event_id.clone(),
            emoji.to_string(),
        );
        let reaction_event_id = outbox.reaction_log.lock().await.remove(&key);
        if let Some(rid) = reaction_event_id {
            room.redact(&rid, Some("removing reaction"), None).await?;
        }
        Ok(())
    }

    pub(super) async fn resolve_joined_room(
        client: &Client,
        cache: &Arc<TokioRwLock<HashMap<String, OwnedRoomId>>>,
        recipient: &str,
    ) -> Result<Room> {
        let id = client::resolve_room(client, cache, recipient).await?;
        let room = client.get_room(&id).ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"recipient": recipient})),
                "matrix: bot is not in room"
            );
            anyhow::Error::msg(format!("matrix: bot is not in room {recipient}"))
        })?;
        if room.state() != RoomState::Joined {
            bail!("matrix: room {recipient} is not in joined state");
        }
        Ok(room)
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum AttachmentKind {
        Auto,
        Image,
        Audio,
        Video,
        File,
        Voice,
    }

    async fn upload_attachment(
        room: &Room,
        att: &MediaAttachment,
        kind: AttachmentKind,
        thread_anchor: Option<&OwnedEventId>,
    ) -> Result<OwnedEventId> {
        let mime = attachment_mime(att);
        if matches!(kind, AttachmentKind::Voice) {
            return upload_voice(room, att, &mime, thread_anchor).await;
        }
        let config = attachment_config_for(att, kind, &mime, thread_anchor);
        let resp = room
            .send_attachment(att.file_name.clone(), &mime, att.data.clone(), config)
            .await
            .map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "send_attachment failed"
                );
                anyhow::Error::msg(format!("send_attachment failed: {e}"))
            })?;
        Ok(resp.event_id)
    }

    pub(super) fn attachment_config_for(
        att: &MediaAttachment,
        kind: AttachmentKind,
        mime: &mime_guess::Mime,
        thread_anchor: Option<&OwnedEventId>,
    ) -> AttachmentConfig {
        let mut config = AttachmentConfig::new().info(attachment_info_for(att, kind, mime));
        if let Some(anchor) = thread_anchor {
            config = config.reply(Some(Reply {
                event_id: anchor.clone(),
                enforce_thread: EnforceThread::Threaded(ReplyWithinThread::No),
                add_mentions: AddMentions::No,
            }));
        }
        config
    }

    pub(super) fn attachment_mime(att: &MediaAttachment) -> mime_guess::Mime {
        match att.mime_type.as_deref() {
            Some(m) => m
                .parse()
                .unwrap_or(mime_guess::mime::APPLICATION_OCTET_STREAM),
            None => mime_guess::from_path(&att.file_name)
                .first()
                .unwrap_or(mime_guess::mime::APPLICATION_OCTET_STREAM),
        }
    }

    fn attachment_info_for(
        att: &MediaAttachment,
        kind: AttachmentKind,
        mime: &mime_guess::Mime,
    ) -> AttachmentInfo {
        let size = UInt::try_from(att.data.len()).ok();
        match attachment_info_kind(kind, mime) {
            AttachmentKind::Image => AttachmentInfo::Image(BaseImageInfo {
                size,
                ..Default::default()
            }),
            AttachmentKind::Audio => AttachmentInfo::Audio(BaseAudioInfo {
                size,
                ..Default::default()
            }),
            AttachmentKind::Video => AttachmentInfo::Video(BaseVideoInfo {
                size,
                ..Default::default()
            }),
            AttachmentKind::Voice => AttachmentInfo::Voice(BaseAudioInfo {
                size,
                ..Default::default()
            }),
            AttachmentKind::File | AttachmentKind::Auto => {
                AttachmentInfo::File(BaseFileInfo { size })
            }
        }
    }

    fn attachment_info_kind(kind: AttachmentKind, mime: &mime_guess::Mime) -> AttachmentKind {
        if kind == AttachmentKind::Voice {
            return AttachmentKind::Voice;
        }
        match mime.type_() {
            mime_guess::mime::IMAGE => AttachmentKind::Image,
            mime_guess::mime::AUDIO => AttachmentKind::Audio,
            mime_guess::mime::VIDEO => AttachmentKind::Video,
            _ => AttachmentKind::File,
        }
    }

    /// Voice messages need the `org.matrix.msc3245.voice` flag, which the
    /// stable matrix-sdk types don't carry. Send via raw JSON, attaching the
    /// thread relation manually when the bot is replying inside one.
    async fn upload_voice(
        room: &Room,
        att: &MediaAttachment,
        mime: &mime_guess::Mime,
        thread_anchor: Option<&OwnedEventId>,
    ) -> Result<OwnedEventId> {
        let mxc = room
            .client()
            .media()
            .upload(mime, att.data.clone(), None)
            .await
            .map_err(|e| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                    "media upload failed"
                );
                anyhow::Error::msg(format!("media upload failed: {e}"))
            })?;
        let mut event = json!({
            "msgtype": "m.audio",
            "body": att.file_name,
            "filename": att.file_name,
            "url": mxc.content_uri.to_string(),
            "info": {
                "mimetype": mime.essence_str(),
                "size": att.data.len(),
            },
            "org.matrix.msc3245.voice": {},
            "org.matrix.msc1767.audio": {
                "duration": 0u32,
                "waveform": Vec::<u32>::new(),
            },
        });
        if let Some(anchor) = thread_anchor
            && let Some(obj) = event.as_object_mut()
        {
            obj.insert(
                "m.relates_to".to_string(),
                json!({
                    "rel_type": "m.thread",
                    "event_id": anchor.as_str(),
                    "is_falling_back": true,
                    "m.in_reply_to": { "event_id": anchor.as_str() },
                }),
            );
        }
        let resp = room.send_raw("m.room.message", event).await?;
        Ok(resp.response.event_id)
    }

    fn derive_file_name(target: &str) -> String {
        target
            .rsplit_once('/')
            .map(|(_, n)| n.to_string())
            .unwrap_or_else(|| target.to_string())
    }

    fn mime_for(file_name: &str, kind: &AttachmentKind) -> String {
        if let Some(m) = mime_guess::from_path(file_name).first() {
            return m.essence_str().to_string();
        }
        match kind {
            AttachmentKind::Image => "image/jpeg".to_string(),
            AttachmentKind::Audio | AttachmentKind::Voice => "audio/ogg".to_string(),
            AttachmentKind::Video => "video/mp4".to_string(),
            AttachmentKind::File | AttachmentKind::Auto => "application/octet-stream".to_string(),
        }
    }
}

// ─── public type ───────────────────────────────────────────────────────────

/// Matrix channel.
pub struct MatrixChannel {
    config: Arc<MatrixConfig>,
    /// The alias key under `[channels.matrix.<alias>]` this handle is
    /// bound to. Used to scope peer-group writes and resolver lookups.
    alias: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    state_dir: PathBuf,
    workspace_dir: Option<Arc<PathBuf>>,
    transcription: Option<Arc<TranscriptionConfig>>,
    client: tokio::sync::OnceCell<Client>,
    pending_approvals: Arc<TokioMutex<HashMap<String, oneshot::Sender<ChannelApprovalResponse>>>>,
    streaming_state: Arc<TokioRwLock<streaming::State>>,
    threads_seen: Arc<TokioRwLock<HashSet<OwnedEventId>>>,
    alias_cache: Arc<TokioRwLock<HashMap<String, OwnedRoomId>>>,
    reaction_log: Arc<TokioMutex<HashMap<outbound::ReactionKey, OwnedEventId>>>,
    bot_display_name: Arc<TokioRwLock<Option<String>>>,
    initial_sync_done: Arc<AtomicBool>,
    undecryptable_seen: Arc<TokioMutex<HashSet<OwnedEventId>>>,
    /// Resolved `ack_reactions` for this Matrix instance — the
    /// per-channel `MatrixConfig.ack_reactions` override falls back to
    /// `[channels].ack_reactions` here at construction time, so the
    /// read site doesn't need to re-resolve on every reaction.
    ack_reactions: bool,
}

impl MatrixChannel {
    /// Validate config and prepare the channel. The SDK Client is built lazily
    /// on first `listen()` or `send()` call.
    pub fn new(
        config: MatrixConfig,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
        state_dir: PathBuf,
    ) -> Result<Self> {
        if config.homeserver.trim().is_empty() {
            bail!("matrix: `homeserver` is required");
        }
        let has_token = config
            .access_token
            .as_deref()
            .is_some_and(|t| !t.trim().is_empty());
        let has_password = config
            .password
            .as_deref()
            .is_some_and(|p| !p.trim().is_empty());
        if !has_token && !has_password {
            bail!("matrix: configure either `access_token` or `password`");
        }
        let ack_reactions = config.ack_reactions.unwrap_or(true);
        Ok(Self {
            config: Arc::new(config),
            alias: alias.into(),
            peer_resolver,
            state_dir,
            workspace_dir: None,
            transcription: None,
            client: tokio::sync::OnceCell::new(),
            pending_approvals: Arc::new(TokioMutex::new(HashMap::new())),
            streaming_state: Arc::new(TokioRwLock::new(streaming::State::default())),
            threads_seen: Arc::new(TokioRwLock::new(HashSet::new())),
            alias_cache: Arc::new(TokioRwLock::new(HashMap::new())),
            reaction_log: Arc::new(TokioMutex::new(HashMap::new())),
            bot_display_name: Arc::new(TokioRwLock::new(None)),
            initial_sync_done: Arc::new(AtomicBool::new(false)),
            undecryptable_seen: Arc::new(TokioMutex::new(HashSet::new())),
            ack_reactions,
        })
    }

    /// Return the alias under `[channels.matrix.<alias>]` that this
    /// channel handle is bound to.
    pub fn alias(&self) -> &str {
        &self.alias
    }

    #[must_use]
    pub fn with_ack_reactions(mut self, ack_reactions: bool) -> Self {
        self.ack_reactions = ack_reactions;
        self
    }

    pub fn with_transcription(mut self, transcription: TranscriptionConfig) -> Self {
        self.transcription = Some(Arc::new(transcription));
        self
    }

    /// Configure the workspace directory used to persist downloaded media so
    /// the agent's vision/document pipelines can read inbound files via
    /// `[IMAGE:path]` / `[Document: name] path` markers.
    pub fn with_workspace_dir(mut self, dir: PathBuf) -> Self {
        self.workspace_dir = Some(Arc::new(dir));
        self
    }

    async fn ensure_client(&self) -> Result<&Client> {
        use ::zeroclaw_log::__private::tracing::Instrument;
        self.client
            .get_or_try_init(|| {
                async {
                    let c = client::build(&self.config, &self.state_dir).await?;
                    if let Ok(Some(name)) = c.account().get_display_name().await {
                        *self.bot_display_name.write().await = Some(name);
                    }
                    Ok::<_, anyhow::Error>(c)
                }
                .instrument(::zeroclaw_log::attribution_span!(self))
            })
            .await
    }

    fn outbox<'a>(&'a self, client: &'a Client) -> outbound::Outbox<'a> {
        outbound::Outbox {
            client,
            alias_cache: &self.alias_cache,
            threads_seen: &self.threads_seen,
            reaction_log: &self.reaction_log,
            reply_in_thread: self.config.reply_in_thread,
            workspace_dir: self.workspace_dir.as_deref().map(|p| p.as_path()),
        }
    }

    /// Edit-in-place draft update. Rate-limited per the configured interval.
    async fn partial_update(&self, recipient: &str, message_id: &str, text: &str) -> Result<()> {
        let client = self.ensure_client().await?;
        let key = streaming_key(recipient, message_id)?;
        let Some(visible_text) = streaming::partial_visible_text(text) else {
            return Ok(());
        };
        let event_id = {
            let mut state = self.streaming_state.write().await;
            let Some(draft) = streaming::partial_for_update(&mut state, &key) else {
                return Ok(());
            };
            let now = Instant::now();
            let interval = Duration::from_millis(self.config.draft_update_interval_ms.max(50));
            if !streaming::partial_should_edit(draft, &visible_text, now, interval) {
                return Ok(());
            }
            let event_id = draft.event_id.clone();
            draft.last_text = visible_text.clone();
            draft.last_edit = now;
            event_id
        };
        outbound::edit(client, recipient, &event_id, &visible_text).await
    }

    /// MultiMessage paragraph emitter. Loops emitting one paragraph per
    /// `\n\n` boundary until the unsent buffer no longer contains a break,
    /// then returns to wait for more accumulated text. Each paragraph posts
    /// as an independent room message threaded under the captured anchor.
    async fn multi_update(&self, recipient: &str, message_id: &str, text: &str) -> Result<()> {
        let client = self.ensure_client().await?;
        let key = streaming_key(recipient, message_id)?;
        let delay = Duration::from_millis(self.config.multi_message_delay_ms);
        loop {
            let (paragraph, thread_anchor) = {
                let mut state = self.streaming_state.write().await;
                let Some(multi) = streaming::multi_for_update(&mut state, &key) else {
                    return Ok(());
                };
                // Detect a buffer reset (e.g. DraftEvent::Clear) and re-anchor
                // to the new shorter text.
                if text.len() < multi.sent_so_far {
                    multi.sent_so_far = 0;
                    return Ok(());
                }
                if text.len() == multi.sent_so_far {
                    return Ok(());
                }
                let unsent = &text[multi.sent_so_far..];
                let Some(break_at) = streaming::next_paragraph_break(unsent) else {
                    return Ok(());
                };
                let paragraph = unsent[..break_at].trim().to_string();
                multi.sent_so_far += break_at + 2; // +2 for the consumed "\n\n"
                (paragraph, multi.thread_anchor.clone())
            };
            if !paragraph.is_empty() {
                let mut msg = SendMessage::new(paragraph, recipient);
                msg.thread_ts = thread_anchor.as_ref().map(|e| e.to_string());
                if let Err(e) = outbound::send(&self.outbox(client), &msg).await {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "matrix: multi-message paragraph send failed"
                    );
                }
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
}

impl ::zeroclaw_api::attribution::Attributable for MatrixChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(::zeroclaw_api::attribution::ChannelKind::Matrix)
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for MatrixChannel {
    fn name(&self) -> &str {
        "matrix"
    }

    fn self_handle(&self) -> Option<String> {
        self.client
            .get()
            .and_then(|c| c.user_id().map(|u| u.to_string()))
    }

    fn self_addressed_mention(&self) -> Option<String> {
        self.self_handle()
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let client = self.ensure_client().await?;
        let _ = outbound::send(&self.outbox(client), message).await?;
        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        let client = self.ensure_client().await?.clone();
        let user_id = client
            .user_id()
            .ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "matrix: client has no user_id after login"
                );
                anyhow::Error::msg("matrix: client has no user_id after login")
            })?
            .to_owned();
        let ctx = inbound::HandlerCtx {
            config: self.config.clone(),
            alias: self.alias.clone(),
            peer_resolver: self.peer_resolver.clone(),
            transcription: self.transcription.clone(),
            workspace_dir: self.workspace_dir.clone(),
            tx,
            pending_approvals: self.pending_approvals.clone(),
            threads_seen: self.threads_seen.clone(),
            bot_user_id: user_id,
            bot_display_name: self.bot_display_name.clone(),
            initial_sync_done: self.initial_sync_done.clone(),
            undecryptable_seen: self.undecryptable_seen.clone(),
        };
        inbound::run_sync_loop(client, ctx).await
    }

    async fn health_check(&self) -> bool {
        match self.client.get() {
            Some(c) => c.matrix_auth().logged_in() && self.initial_sync_done.load(Ordering::SeqCst),
            None => false,
        }
    }

    async fn start_typing(&self, recipient: &str) -> Result<()> {
        let client = self.ensure_client().await?;
        let id = client::resolve_room(client, &self.alias_cache, recipient).await?;
        if let Some(room) = client.get_room(&id) {
            let _ = room.typing_notice(true).await;
        }
        Ok(())
    }

    async fn stop_typing(&self, recipient: &str) -> Result<()> {
        let client = self.ensure_client().await?;
        let id = client::resolve_room(client, &self.alias_cache, recipient).await?;
        if let Some(room) = client.get_room(&id) {
            let _ = room.typing_notice(false).await;
        }
        Ok(())
    }

    fn supports_draft_updates(&self) -> bool {
        // The orchestrator's streaming pipeline is gated on this returning
        // true. Both Partial and MultiMessage need it on so update_draft is
        // driven with accumulated text; the channel decides internally
        // whether to edit a single message or emit paragraphs.
        !matches!(self.config.stream_mode, StreamMode::Off)
    }

    fn supports_multi_message_streaming(&self) -> bool {
        matches!(self.config.stream_mode, StreamMode::MultiMessage)
    }

    fn multi_message_delay_ms(&self) -> u64 {
        self.config.multi_message_delay_ms
    }

    async fn send_draft(&self, message: &SendMessage) -> Result<Option<String>> {
        let client = self.ensure_client().await?;
        let room_id = streaming_room(&message.recipient)?;
        match self.config.stream_mode {
            StreamMode::Off => Ok(None),
            StreamMode::Partial => {
                // Send the placeholder draft now so subsequent update_draft
                // calls have an event to edit.
                let event_id = outbound::send(&self.outbox(client), message).await?;
                let thread_anchor =
                    outbound::thread_anchor_from_message(&self.outbox(client), message);
                let key = streaming::draft_key(room_id, event_id.as_ref())?;
                let mut state = self.streaming_state.write().await;
                state.partial.insert(
                    key,
                    streaming::PartialDraft {
                        event_id: event_id.clone(),
                        thread_anchor,
                        last_text: message.content.clone(),
                        last_edit: Instant::now(),
                    },
                );
                Ok(Some(event_id.to_string()))
            }
            StreamMode::MultiMessage => {
                // No initial message — paragraphs are emitted by update_draft
                // as they appear. Capture the thread anchor up front so each
                // paragraph lands in the same thread as the user's message.
                let thread_anchor = message
                    .thread_ts
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .and_then(|s| s.parse::<OwnedEventId>().ok());
                let draft_id = streaming::new_multi_message_draft_id();
                let key = streaming::draft_key(room_id, &draft_id)?;
                let mut state = self.streaming_state.write().await;
                state.multi.insert(
                    key,
                    streaming::MultiDraft {
                        thread_anchor,
                        sent_so_far: 0,
                    },
                );
                Ok(Some(draft_id))
            }
        }
    }

    async fn update_draft(&self, recipient: &str, message_id: &str, text: &str) -> Result<()> {
        match self.config.stream_mode {
            StreamMode::Off => Ok(()),
            StreamMode::Partial => self.partial_update(recipient, message_id, text).await,
            StreamMode::MultiMessage => self.multi_update(recipient, message_id, text).await,
        }
    }

    async fn update_draft_progress(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> Result<()> {
        // Tool-status updates only show in Partial (edit-in-place) mode.
        // MultiMessage doesn't have an in-flight draft to update.
        if matches!(self.config.stream_mode, StreamMode::Partial) {
            return self.update_draft(recipient, message_id, text).await;
        }
        Ok(())
    }

    async fn finalize_draft(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
        _suppress_voice: bool,
    ) -> Result<()> {
        let client = self.ensure_client().await?;
        let key = streaming_key(recipient, message_id)?;
        match self.config.stream_mode {
            StreamMode::Off => Ok(()),
            StreamMode::Partial => {
                let draft = {
                    let mut state = self.streaming_state.write().await;
                    streaming::take_partial(&mut state, &key)
                };
                if let Some(draft) = draft {
                    let room =
                        outbound::resolve_joined_room(client, &self.alias_cache, recipient).await?;
                    let (cleaned_text, markers) = markers::parse(text);
                    let delivery = outbound::deliver_attachments(
                        &self.outbox(client),
                        &room,
                        cleaned_text,
                        &markers,
                        &[],
                        draft.thread_anchor.as_ref(),
                    )
                    .await?;

                    match streaming::decide_partial_finalize_action(
                        delivery.text.trim().is_empty(),
                        delivery.last_attachment_id.is_some(),
                    ) {
                        streaming::PartialFinalizeAction::EditDraft => {
                            let kinds = delivery.failure_kinds();
                            let any_attachment_landed = delivery.last_attachment_id.is_some();
                            if let Err(edit_err) =
                                outbound::edit(client, recipient, &draft.event_id, &delivery.text)
                                    .await
                            {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(
                                        ::serde_json::json!({"edit_err": edit_err.to_string()})
                                    ),
                                    "matrix: partial finalize edit failed: ; sending cleaned text fallback"
                                );
                                let mut fallback = SendMessage::new(&delivery.text, recipient);
                                fallback.thread_ts =
                                    draft.thread_anchor.as_ref().map(|e| e.to_string());
                                match outbound::send(&self.outbox(client), &fallback).await {
                                    Ok(fallback_id) => {
                                        outbound::emit_failure_reactions(
                                            &room,
                                            &fallback_id,
                                            &kinds,
                                        )
                                        .await;
                                    }
                                    Err(send_err) if any_attachment_landed => {
                                        ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"send_err": send_err.to_string()})), "matrix: partial finalize cleaned text fallback failed after attachment upload: ; suppressing error to avoid duplicate attachment retry");
                                    }
                                    Err(send_err) => {
                                        return Err(edit_err).with_context(|| {
                                            format!(
                                                "matrix: partial finalize cleaned text fallback failed: {send_err}"
                                            )
                                        });
                                    }
                                }
                            } else {
                                outbound::emit_failure_reactions(&room, &draft.event_id, &kinds)
                                    .await;
                            }
                        }
                        streaming::PartialFinalizeAction::RedactDraft => {
                            if let Err(err) = outbound::redact(
                                client,
                                recipient,
                                &draft.event_id,
                                Some("attachment-only response delivered".to_string()),
                            )
                            .await
                            {
                                ::zeroclaw_log::record!(
                                    WARN,
                                    ::zeroclaw_log::Event::new(
                                        module_path!(),
                                        ::zeroclaw_log::Action::Note
                                    )
                                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                    .with_attrs(::serde_json::json!({"err": err.to_string()})),
                                    "matrix: partial finalize redaction failed after attachment-only upload: ; leaving placeholder to avoid duplicate attachment retry"
                                );
                            }
                        }
                        streaming::PartialFinalizeAction::EmptyError => {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Reject
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                .with_attrs(::serde_json::json!({"phase": "partial_finalize"})),
                                "matrix: empty partial draft body and no successful attachment"
                            );
                            return Err(anyhow::Error::msg(
                                "matrix: empty partial draft body and no successful attachment",
                            ));
                        }
                    }
                }
                Ok(())
            }
            StreamMode::MultiMessage => {
                // Drain the trailing paragraph (or whatever's left after the
                // last \n\n boundary) as one final message.
                let multi = {
                    let mut state = self.streaming_state.write().await;
                    streaming::take_multi(&mut state, &key)
                };
                let Some(state) = multi else {
                    return Ok(());
                };
                let remainder = if text.len() > state.sent_so_far {
                    text[state.sent_so_far..].trim().to_string()
                } else {
                    String::new()
                };
                if !remainder.is_empty() {
                    let mut msg = SendMessage::new(remainder, recipient);
                    msg.thread_ts = state.thread_anchor.as_ref().map(|e| e.to_string());
                    outbound::send(&self.outbox(client), &msg).await?;
                }
                Ok(())
            }
        }
    }

    async fn cancel_draft(&self, recipient: &str, message_id: &str) -> Result<()> {
        let client = self.ensure_client().await?;
        let key = streaming_key(recipient, message_id)?;
        match self.config.stream_mode {
            StreamMode::Off => Ok(()),
            StreamMode::Partial => {
                let draft = {
                    let mut state = self.streaming_state.write().await;
                    streaming::take_partial(&mut state, &key)
                };
                if let Some(d) = draft {
                    let _ = outbound::redact(
                        client,
                        recipient,
                        &d.event_id,
                        Some("cancelled".to_string()),
                    )
                    .await;
                }
                Ok(())
            }
            StreamMode::MultiMessage => {
                // Already-sent paragraphs are independent room messages and
                // are not redacted on cancel — partial output is preferable
                // to silent disappearance. Just drop our state.
                let mut state = self.streaming_state.write().await;
                streaming::take_multi(&mut state, &key);
                Ok(())
            }
        }
    }

    async fn add_reaction(&self, channel_id: &str, message_id: &str, emoji: &str) -> Result<()> {
        if !self.ack_reactions {
            return Ok(());
        }
        let client = self.ensure_client().await?;
        let event_id: OwnedEventId = message_id.parse()?;
        outbound::react(&self.outbox(client), channel_id, &event_id, emoji).await
    }

    async fn remove_reaction(&self, channel_id: &str, message_id: &str, emoji: &str) -> Result<()> {
        if !self.ack_reactions {
            return Ok(());
        }
        let client = self.ensure_client().await?;
        let event_id: OwnedEventId = message_id.parse()?;
        outbound::unreact(&self.outbox(client), channel_id, &event_id, emoji).await
    }

    async fn redact_message(
        &self,
        channel_id: &str,
        message_id: &str,
        reason: Option<String>,
    ) -> Result<()> {
        let client = self.ensure_client().await?;
        let event_id: OwnedEventId = message_id.parse()?;
        outbound::redact(client, channel_id, &event_id, reason).await
    }

    async fn create_room(&self, options: &RoomCreationOptions) -> Result<String> {
        let client = self.ensure_client().await?;
        let request = room_management::build_create_room_request(options)?;
        let room = client.create_room(request).await?;
        Ok(room.room_id().to_string())
    }

    async fn invite_user(&self, room_id: &str, user_id: &str) -> Result<()> {
        let client = self.ensure_client().await?;
        let request = room_management::build_invite_user_request(room_id, user_id)?;
        client.send(request).await?;
        Ok(())
    }

    async fn request_approval(
        &self,
        recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> Result<Option<ChannelApprovalResponse>> {
        let token = approval::generate_token_default();
        let prompt = format!(
            "APPROVAL REQUIRED [{token}]\nTool: {}\nArgs: {}\n\nReply `{token} approve` / `{token} deny` / `{token} always`.",
            request.tool_name, request.arguments_summary
        );

        let (tx, rx) = oneshot::channel();
        self.pending_approvals
            .lock()
            .await
            .insert(token.clone(), tx);

        let send_msg = SendMessage::new(prompt, recipient);
        if let Err(e) = self.send(&send_msg).await {
            self.pending_approvals.lock().await.remove(&token);
            return Err(e);
        }

        let timeout = Duration::from_secs(self.config.approval_timeout_secs.max(1));
        let result = tokio::time::timeout(timeout, rx).await;
        if result.is_err() {
            self.pending_approvals.lock().await.remove(&token);
        }
        match result {
            Ok(Ok(resp)) => Ok(Some(resp)),
            Ok(Err(_)) => Ok(Some(ChannelApprovalResponse::Deny)),
            Err(_) => Ok(Some(ChannelApprovalResponse::Deny)),
        }
    }
}

fn streaming_room(recipient: &str) -> Result<OwnedRoomId> {
    recipient
        .parse::<OwnedRoomId>()
        .with_context(|| format!("parse recipient room id {recipient}"))
}

fn streaming_key(recipient: &str, message_id: &str) -> Result<streaming::DraftKey> {
    streaming::draft_key(streaming_room(recipient)?, message_id)
}

// ─── tests ─────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    mod transcription_provider_resolution {
        use super::super::inbound::build_transcription_manager;
        use zeroclaw_config::schema::TranscriptionConfig;

        fn local_whisper_config(url: &str) -> zeroclaw_config::schema::LocalWhisperConfig {
            zeroclaw_config::schema::LocalWhisperConfig {
                url: url.to_string(),
                bearer_token: Some("test-token".to_string()),
                max_audio_bytes: 10 * 1024 * 1024,
                timeout_secs: 30,
            }
        }

        #[tokio::test]
        async fn binds_alias_when_exactly_one_provider_is_configured() {
            let config = TranscriptionConfig {
                enabled: true,
                local_whisper: Some(local_whisper_config("http://127.0.0.1:9999/v1/transcribe")),
                ..TranscriptionConfig::default()
            };

            let manager = build_transcription_manager(&config).unwrap();
            assert_eq!(
                manager.available_providers(),
                vec!["local_whisper"],
                "fixture must register exactly one provider"
            );

            let err = manager
                .transcribe(b"not-real-audio", "voice.aiff")
                .await
                .expect_err("an unsupported format must be rejected");
            assert!(
                !err.to_string()
                    .contains("Agent has no transcription_provider configured"),
                "expected dispatch to the sole provider, got the empty-alias bail: {err}"
            );
            assert!(
                err.to_string().contains("Unsupported audio format"),
                "expected the dispatched provider to reject the format, got: {err}"
            );
        }

        #[tokio::test]
        async fn leaves_alias_unbound_when_multiple_providers_are_configured() {
            let config = TranscriptionConfig {
                enabled: true,
                api_key: Some("test-groq-key".to_string()),
                local_whisper: Some(local_whisper_config("http://127.0.0.1:9999/v1/transcribe")),
                ..TranscriptionConfig::default()
            };

            let manager = build_transcription_manager(&config).unwrap();
            assert!(
                manager.available_providers().len() > 1,
                "fixture must register more than one provider, got {:?}",
                manager.available_providers()
            );

            let err = manager
                .transcribe(b"not-real-audio", "voice.ogg")
                .await
                .expect_err("an unbound alias must fail");
            assert!(
                err.to_string()
                    .contains("Agent has no transcription_provider configured"),
                "expected the empty-alias bail, got: {err}"
            );
        }
    }

    mod media_filename_resolution {
        use super::super::inbound::MediaCategory;
        use super::super::inbound::{build_transcription_manager, transcription_safe_filename};
        use zeroclaw_config::schema::TranscriptionConfig;

        fn local_whisper_config(url: &str) -> zeroclaw_config::schema::LocalWhisperConfig {
            zeroclaw_config::schema::LocalWhisperConfig {
                url: url.to_string(),
                bearer_token: Some("test-token".to_string()),
                max_audio_bytes: 10 * 1024 * 1024,
                timeout_secs: 30,
            }
        }

        #[test]
        fn unaccepted_extension_with_supported_mime_uses_mime_derived_name() {
            let resolved = transcription_safe_filename(
                "recording.bin",
                Some("audio/ogg"),
                &MediaCategory::Audio,
            );
            assert_eq!(resolved, "attachment.ogg");
        }

        #[test]
        fn accepted_extension_is_preserved_even_when_mime_differs() {
            let resolved = transcription_safe_filename(
                "recording.mp3",
                Some("audio/ogg"),
                &MediaCategory::Audio,
            );
            assert_eq!(resolved, "recording.mp3");
        }

        #[test]
        fn accepted_extension_matching_mime_is_passed_through_unchanged() {
            let resolved = transcription_safe_filename(
                "recording.ogg",
                Some("audio/ogg"),
                &MediaCategory::Audio,
            );
            assert_eq!(resolved, "recording.ogg");
        }

        #[test]
        fn accepted_extension_is_matched_case_insensitively() {
            let resolved = transcription_safe_filename(
                "recording.OGG",
                Some("audio/ogg"),
                &MediaCategory::Audio,
            );
            assert_eq!(resolved, "recording.OGG");
        }

        #[test]
        fn oga_extension_is_accepted_and_preserved() {
            let resolved =
                transcription_safe_filename("note.oga", Some("audio/ogg"), &MediaCategory::Voice);
            assert_eq!(resolved, "note.oga");
        }

        #[test]
        fn no_mime_with_an_accepted_extension_is_passed_through() {
            let resolved =
                transcription_safe_filename("recording.mp3", None, &MediaCategory::Audio);
            assert_eq!(resolved, "recording.mp3");
        }

        #[test]
        fn unaccepted_extension_with_no_mime_falls_back_to_kind_default() {
            let resolved =
                transcription_safe_filename("recording.bin", None, &MediaCategory::Audio);
            assert_eq!(resolved, "attachment.ogg");
        }

        #[test]
        fn no_mime_and_no_extension_falls_back_to_kind_default() {
            let resolved =
                transcription_safe_filename("Meeting recap", None, &MediaCategory::Audio);
            assert_eq!(resolved, "attachment.ogg");
        }

        #[test]
        fn synthesizes_extension_from_mime_when_no_extension_at_all() {
            let resolved = transcription_safe_filename(
                "Voice message",
                Some("audio/ogg"),
                &MediaCategory::Voice,
            );
            assert_eq!(resolved, "attachment.ogg");
        }

        #[tokio::test]
        async fn caption_only_body_reaches_provider_dispatch_not_format_rejection() {
            let config = TranscriptionConfig {
                enabled: true,
                local_whisper: Some(local_whisper_config("http://127.0.0.1:9999/v1/transcribe")),
                ..TranscriptionConfig::default()
            };
            let manager = build_transcription_manager(&config).unwrap();

            let file_name = transcription_safe_filename(
                "Voice message",
                Some("audio/ogg"),
                &MediaCategory::Voice,
            );

            let err = manager
                .transcribe(b"not-real-audio", &file_name)
                .await
                .expect_err("fixture audio bytes are not real, dispatch must still fail");
            assert!(
                !err.to_string()
                    .contains("Agent has no transcription_provider configured"),
                "expected dispatch to the sole provider, got the empty-alias bail: {err}"
            );
            assert!(
                !err.to_string().contains("Unsupported audio format"),
                "expected a resolved .ogg filename to pass format validation, got: {err}"
            );
        }

        #[tokio::test]
        async fn unaccepted_extension_reaches_provider_dispatch_not_format_rejection() {
            let config = TranscriptionConfig {
                enabled: true,
                local_whisper: Some(local_whisper_config("http://127.0.0.1:9999/v1/transcribe")),
                ..TranscriptionConfig::default()
            };
            let manager = build_transcription_manager(&config).unwrap();

            let file_name = transcription_safe_filename(
                "recording.bin",
                Some("audio/ogg"),
                &MediaCategory::Voice,
            );

            let err = manager
                .transcribe(b"not-real-audio", &file_name)
                .await
                .expect_err("fixture audio bytes are not real, dispatch must still fail");
            assert!(
                !err.to_string()
                    .contains("Agent has no transcription_provider configured"),
                "expected dispatch to the sole provider, got the empty-alias bail: {err}"
            );
            assert!(
                !err.to_string().contains("Unsupported audio format"),
                "expected the unaccepted .bin extension to be replaced with the MIME-derived .ogg, got: {err}"
            );
        }

        // attach_media only inserts non-empty transcripts.
        #[tokio::test]
        async fn caption_only_body_transcript_is_produced_for_insertion() {
            use wiremock::matchers::{method, path};
            use wiremock::{Mock, MockServer, ResponseTemplate};

            let server = MockServer::start().await;

            Mock::given(method("POST"))
                .and(path("/v1/transcribe"))
                .respond_with(ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({"text": "this is the transcribed voice message"}),
                ))
                .mount(&server)
                .await;

            let config = TranscriptionConfig {
                enabled: true,
                local_whisper: Some(local_whisper_config(&format!(
                    "{}/v1/transcribe",
                    server.uri()
                ))),
                ..TranscriptionConfig::default()
            };
            let manager = build_transcription_manager(&config).unwrap();

            let file_name = transcription_safe_filename(
                "Voice message",
                Some("audio/ogg"),
                &MediaCategory::Voice,
            );

            let text = manager.transcribe(b"fake-audio", &file_name).await.unwrap();
            assert_eq!(text, "this is the transcribed voice message");
            assert!(!text.trim().is_empty());
        }

        #[test]
        fn transcription_only_name_may_diverge_from_the_real_filename() {
            let real_filename = "recording.bin";
            let transcribe_name = transcription_safe_filename(
                real_filename,
                Some("audio/ogg"),
                &MediaCategory::Audio,
            );
            assert_ne!(transcribe_name, real_filename);
            assert_eq!(transcribe_name, "attachment.ogg");
        }
    }

    mod markers {
        use super::super::markers::{MarkerKind, parse};

        #[test]
        fn empty_text_yields_no_markers() {
            let (text, ms) = parse("");
            assert_eq!(text, "");
            assert!(ms.is_empty());
        }

        #[test]
        fn plain_text_passthrough() {
            let (text, ms) = parse("hello world");
            assert_eq!(text, "hello world");
            assert!(ms.is_empty());
        }

        #[test]
        fn single_image_marker_extracted() {
            let (text, ms) = parse("[image:https://example.com/cat.jpg]");
            assert_eq!(text, "");
            assert_eq!(ms.len(), 1);
            assert_eq!(ms[0].kind, MarkerKind::Image);
            assert_eq!(ms[0].target, "https://example.com/cat.jpg");
        }

        #[test]
        fn voice_marker_distinct_from_audio() {
            let (_, ms) = parse("[voice:/tmp/note.ogg] [audio:/tmp/song.mp3]");
            assert_eq!(ms.len(), 2);
            assert_eq!(ms[0].kind, MarkerKind::Voice);
            assert_eq!(ms[1].kind, MarkerKind::Audio);
        }

        #[test]
        fn multiple_markers_with_text_in_between() {
            let (text, ms) =
                parse("before [image:https://x/y.jpg] middle [file:/tmp/doc.pdf] after");
            assert_eq!(text, "before  middle  after");
            assert_eq!(ms.len(), 2);
            assert_eq!(ms[0].kind, MarkerKind::Image);
            assert_eq!(ms[1].kind, MarkerKind::File);
        }

        #[test]
        fn malformed_marker_left_in_text() {
            let (text, ms) = parse("foo [image: bar");
            assert_eq!(text, "foo [image: bar");
            assert!(ms.is_empty());
        }

        #[test]
        fn unknown_keyword_left_in_text() {
            let (text, ms) = parse("[banana:fruit]");
            assert_eq!(text, "[banana:fruit]");
            assert!(ms.is_empty());
        }

        #[test]
        fn empty_target_left_in_text() {
            let (text, ms) = parse("[image:]");
            assert_eq!(text, "[image:]");
            assert!(ms.is_empty());
        }

        #[test]
        fn marker_with_newline_inside_left_in_text() {
            let (text, ms) = parse("[image:a\nb]");
            assert!(text.contains("[image:a"));
            assert!(ms.is_empty());
        }
    }

    mod approval {
        use super::super::approval::{
            TOKEN_LEN, generate_token, generate_token_default, parse_reply,
        };
        use rand::SeedableRng;
        use rand::rngs::StdRng;
        use std::collections::HashSet;
        use zeroclaw_api::channel::ChannelApprovalResponse;

        #[test]
        fn token_length_and_alphabet() {
            let mut rng = StdRng::seed_from_u64(42);
            let tok = generate_token(&mut rng);
            assert_eq!(tok.len(), TOKEN_LEN);
            assert!(tok.chars().all(|c| c.is_ascii_alphanumeric()));
        }

        #[test]
        fn tokens_are_diverse() {
            let mut rng = StdRng::seed_from_u64(7);
            let mut seen = HashSet::new();
            for _ in 0..1000 {
                seen.insert(generate_token(&mut rng));
            }
            assert!(
                seen.len() >= 998,
                "too many collisions: {}",
                1000 - seen.len()
            );
        }

        #[test]
        fn default_token_has_correct_length() {
            assert_eq!(generate_token_default().len(), TOKEN_LEN);
        }

        #[test]
        fn parse_approve() {
            let (tok, resp) = parse_reply("ABCDEFGH approve").expect("parses");
            assert_eq!(tok, "ABCDEFGH");
            assert_eq!(resp, ChannelApprovalResponse::Approve);
        }

        #[test]
        fn parse_deny_lowercase() {
            let (_, resp) = parse_reply("abcdefgh deny").expect("parses");
            assert_eq!(resp, ChannelApprovalResponse::Deny);
        }

        #[test]
        fn parse_always() {
            let (_, resp) = parse_reply("ABCDEFGH always").expect("parses");
            assert_eq!(resp, ChannelApprovalResponse::AlwaysApprove);
        }

        #[test]
        fn parse_yes_no_aliases() {
            assert_eq!(
                parse_reply("ABCDEFGH yes").map(|x| x.1),
                Some(ChannelApprovalResponse::Approve)
            );
            assert_eq!(
                parse_reply("ABCDEFGH no").map(|x| x.1),
                Some(ChannelApprovalResponse::Deny)
            );
        }

        #[test]
        fn rejects_wrong_token_length() {
            assert!(parse_reply("ABC approve").is_none());
            assert!(parse_reply("ABCDEFGHIJ approve").is_none());
        }

        #[test]
        fn rejects_unknown_verb() {
            assert!(parse_reply("ABCDEFGH maybe").is_none());
        }

        #[test]
        fn rejects_trailing_garbage() {
            assert!(parse_reply("ABCDEFGH approve please").is_none());
        }
    }

    mod room_management {
        use super::super::room_management::{build_create_room_request, build_invite_user_request};
        use matrix_sdk::ruma::api::client::room::Visibility as MatrixVisibility;
        use serde_json::json;
        use zeroclaw_api::channel::{RoomCreationOptions, RoomVisibility};

        #[test]
        fn create_room_request_maps_typed_options() {
            let request = build_create_room_request(&RoomCreationOptions {
                name: Some("Ops room".into()),
                topic: Some("Operations".into()),
                invites: vec!["@alice:example.org".into(), "@bob:example.org".into()],
                visibility: Some(RoomVisibility::Public),
                encryption: Some(true),
            })
            .expect("request builds");

            assert_eq!(request.name.as_deref(), Some("Ops room"));
            assert_eq!(request.topic.as_deref(), Some("Operations"));
            assert_eq!(request.visibility, MatrixVisibility::Public);
            assert_eq!(request.invite.len(), 2);
            assert_eq!(request.invite[0].as_str(), "@alice:example.org");
            assert_eq!(request.invite[1].as_str(), "@bob:example.org");
            assert_eq!(request.initial_state.len(), 1);
        }

        #[test]
        fn create_room_request_rejects_invalid_invite_user() {
            let err = build_create_room_request(&RoomCreationOptions {
                invites: vec!["not-a-mxid".into()],
                ..RoomCreationOptions::default()
            })
            .unwrap_err();

            assert!(err.to_string().contains("invalid invite user id"));
        }

        #[test]
        fn invite_user_request_parses_room_and_user_ids() {
            let request =
                build_invite_user_request("!room:example.org", "@alice:example.org").unwrap();

            assert_eq!(request.room_id.as_str(), "!room:example.org");
            assert_eq!(
                serde_json::to_value(&request.recipient).unwrap(),
                json!({"user_id": "@alice:example.org"})
            );
        }

        #[test]
        fn invite_user_request_rejects_invalid_ids() {
            let err = build_invite_user_request("not-a-room", "@alice:example.org").unwrap_err();
            assert!(err.to_string().contains("invalid room id"));

            let err = build_invite_user_request("!room:example.org", "not-a-user").unwrap_err();
            assert!(err.to_string().contains("invalid user id"));
        }
    }

    mod mention {
        use super::super::mention::is_mentioned;
        use matrix_sdk::ruma::user_id;

        #[test]
        fn explicit_mention_in_user_ids_passes() {
            let bot = user_id!("@bot:example.org");
            assert!(is_mentioned(
                bot,
                None,
                Some(&["@bot:example.org".to_string()]),
                "hi",
            ));
        }

        #[test]
        fn explicit_mention_list_without_bot_rejects() {
            let bot = user_id!("@bot:example.org");
            assert!(!is_mentioned(
                bot,
                None,
                Some(&["@alice:example.org".to_string()]),
                "@bot:example.org help",
            ));
        }

        #[test]
        fn body_fallback_full_id() {
            let bot = user_id!("@bot:example.org");
            assert!(is_mentioned(bot, None, None, "@bot:example.org help"));
        }

        #[test]
        fn body_fallback_localpart_only() {
            let bot = user_id!("@bot:example.org");
            assert!(is_mentioned(bot, None, None, "hey @bot please reply"));
        }

        #[test]
        fn body_fallback_display_name() {
            let bot = user_id!("@bot:example.org");
            assert!(is_mentioned(bot, Some("ZeroClaw"), None, "hi zeroclaw!"));
        }

        #[test]
        fn no_mention_rejects() {
            let bot = user_id!("@bot:example.org");
            assert!(!is_mentioned(
                bot,
                Some("ZeroClaw"),
                None,
                "no mention here"
            ));
        }
    }

    mod allowlist {
        use super::super::allowlist::{room_allowed_static, user_allowed};

        #[test]
        fn empty_user_list_denies_all() {
            assert!(!user_allowed(&[], "@a:b"));
        }

        #[test]
        fn star_user_list_allows_all() {
            assert!(user_allowed(&["*".to_string()], "@a:b"));
        }

        #[test]
        fn user_in_list_allowed() {
            assert!(user_allowed(&["@a:b".to_string()], "@a:b"));
        }

        #[test]
        fn user_not_in_list_denied() {
            assert!(!user_allowed(&["@a:b".to_string()], "@c:d"));
        }

        #[test]
        fn user_in_list_case_insensitive() {
            // Operator-configured case shouldn't matter — Matrix MXIDs are
            // spec-lowercase but tolerated in mixed case by some servers.
            assert!(user_allowed(
                &["@Bot:Example.org".to_string()],
                "@bot:example.org"
            ));
            assert!(user_allowed(
                &["@bot:example.org".to_string()],
                "@Bot:EXAMPLE.org"
            ));
        }

        #[test]
        fn empty_room_list_allows_all() {
            assert!(room_allowed_static(&[], "!any:server"));
        }

        #[test]
        fn room_in_list_allowed() {
            assert!(room_allowed_static(
                &["!ok:server".to_string()],
                "!ok:server"
            ));
        }

        #[test]
        fn room_not_in_list_denied() {
            assert!(!room_allowed_static(
                &["!ok:server".to_string()],
                "!nope:server"
            ));
        }
    }

    mod ack_reactions {
        use std::sync::Arc;

        use tempfile::TempDir;
        use zeroclaw_api::channel::Channel;
        use zeroclaw_config::schema::MatrixConfig;

        use super::super::MatrixChannel;

        #[tokio::test]
        async fn matrix_remove_reaction_noops_before_parsing_when_ack_disabled() {
            let config = MatrixConfig {
                homeserver: "https://matrix.example.com".to_string(),
                access_token: Some("token".to_string()),
                ack_reactions: Some(false),
                ..MatrixConfig::default()
            };
            let state_dir = TempDir::new().expect("temp state dir");
            let channel = MatrixChannel::new(
                config,
                "matrix",
                Arc::new(Vec::<String>::new),
                state_dir.path().to_path_buf(),
            )
            .expect("matrix channel");

            channel
                .remove_reaction("bad-room", "bad-event", "✅")
                .await
                .expect("ack-disabled reaction removal should be a no-op");
        }
    }

    mod context {
        use super::super::context::{claim_first_visit, format_preamble, mark_seen};
        use matrix_sdk::ruma::{OwnedEventId, owned_event_id};
        use std::{collections::HashSet, sync::Arc};
        use tokio::sync::RwLock;

        fn empty() -> Arc<RwLock<HashSet<OwnedEventId>>> {
            Arc::new(RwLock::new(HashSet::new()))
        }

        #[test]
        fn preamble_includes_sender_and_body() {
            let p = format_preamble("@alice:server", "hello");
            assert_eq!(p, "[Thread root from @alice:server]: hello\n\n");
        }

        #[test]
        fn preamble_skips_body_when_empty() {
            let p = format_preamble("@alice:server", "");
            assert_eq!(p, "[Thread root from @alice:server]\n\n");
        }

        #[tokio::test]
        async fn first_visit_returns_true_then_false() {
            let set = empty();
            let id = owned_event_id!("$abc:server");
            assert!(claim_first_visit(&set, &id).await);
            assert!(!claim_first_visit(&set, &id).await);
        }

        #[tokio::test]
        async fn pre_marked_thread_returns_false() {
            let set = empty();
            let id = owned_event_id!("$abc:server");
            mark_seen(&set, id.clone()).await;
            assert!(!claim_first_visit(&set, &id).await);
        }
    }

    mod streaming {
        use super::super::streaming;
        use super::super::streaming::{
            MultiDraft, PartialDraft, PartialFinalizeAction, State, decide_partial_finalize_action,
            partial_should_edit, partial_visible_text,
        };
        use matrix_sdk::ruma::{OwnedEventId, owned_event_id, owned_room_id};
        use std::time::{Duration, Instant};

        fn draft(text: &str, last_edit: Instant) -> PartialDraft {
            PartialDraft {
                event_id: owned_event_id!("$1:server"),
                thread_anchor: None,
                last_text: text.to_string(),
                last_edit,
            }
        }

        fn partial_draft(event_id: OwnedEventId, text: &str) -> PartialDraft {
            PartialDraft {
                event_id,
                thread_anchor: None,
                last_text: text.to_string(),
                last_edit: Instant::now(),
            }
        }

        #[test]
        fn skip_when_text_unchanged() {
            let now = Instant::now();
            let d = draft("hello", now - Duration::from_secs(60));
            assert!(!partial_should_edit(
                &d,
                "hello",
                now,
                Duration::from_millis(500)
            ));
        }

        #[test]
        fn skip_within_rate_limit() {
            let now = Instant::now();
            let d = draft("hello", now - Duration::from_millis(100));
            assert!(!partial_should_edit(
                &d,
                "world",
                now,
                Duration::from_millis(500)
            ));
        }

        #[test]
        fn allow_after_rate_limit() {
            let now = Instant::now();
            let d = draft("hello", now - Duration::from_millis(600));
            assert!(partial_should_edit(
                &d,
                "world",
                now,
                Duration::from_millis(500)
            ));
        }

        #[test]
        fn partial_visible_text_strips_attachment_markers() {
            assert_eq!(
                partial_visible_text("Report ready [DOCUMENT:report.pdf]").as_deref(),
                Some("Report ready")
            );
        }

        #[test]
        fn partial_visible_text_skips_marker_only_updates() {
            assert_eq!(partial_visible_text("[DOCUMENT:report.pdf]"), None);
        }

        #[test]
        fn marker_only_partial_finalize_redacts_placeholder_after_upload() {
            assert_eq!(
                decide_partial_finalize_action(true, true),
                PartialFinalizeAction::RedactDraft
            );
        }

        #[test]
        fn text_partial_finalize_keeps_editing_draft_after_upload() {
            assert_eq!(
                decide_partial_finalize_action(false, true),
                PartialFinalizeAction::EditDraft
            );
        }

        #[test]
        fn text_only_partial_finalize_keeps_editing_draft() {
            assert_eq!(
                decide_partial_finalize_action(false, false),
                PartialFinalizeAction::EditDraft
            );
        }

        #[test]
        fn empty_partial_finalize_without_upload_reports_empty_error() {
            assert_eq!(
                decide_partial_finalize_action(true, false),
                PartialFinalizeAction::EmptyError
            );
        }

        #[test]
        fn draft_keys_include_message_id_for_same_room_concurrency() {
            let room = owned_room_id!("!room:server");
            let first = streaming::draft_key(room.clone(), "$draft-a:server").unwrap();
            let second = streaming::draft_key(room.clone(), "$draft-b:server").unwrap();

            assert_ne!(first, second);

            let mut state = streaming::State::default();
            state.partial.insert(
                first.clone(),
                PartialDraft {
                    event_id: owned_event_id!("$draft-a:server"),
                    thread_anchor: None,
                    last_text: "first".to_string(),
                    last_edit: Instant::now(),
                },
            );
            state.partial.insert(
                second.clone(),
                PartialDraft {
                    event_id: owned_event_id!("$draft-b:server"),
                    thread_anchor: None,
                    last_text: "second".to_string(),
                    last_edit: Instant::now(),
                },
            );

            assert_eq!(state.partial.len(), 2);
            assert_eq!(
                state.partial.remove(&second).map(|draft| draft.event_id),
                Some(owned_event_id!("$draft-b:server"))
            );
            assert!(state.partial.contains_key(&first));
        }

        #[test]
        fn partial_lifecycle_lookup_isolates_update_finalize_and_cancel_by_message_id() {
            let recipient = "!room:server";
            let first = super::super::streaming_key(recipient, "$draft-a:server").unwrap();
            let second = super::super::streaming_key(recipient, "$draft-b:server").unwrap();
            let canceled = super::super::streaming_key(recipient, "$draft-c:server").unwrap();

            let mut state = State::default();
            state.partial.insert(
                first.clone(),
                partial_draft(owned_event_id!("$draft-a:server"), "first"),
            );
            state.partial.insert(
                second.clone(),
                partial_draft(owned_event_id!("$draft-b:server"), "second"),
            );

            streaming::partial_for_update(&mut state, &second)
                .expect("second draft remains addressable")
                .last_text = "second updated".to_string();

            assert_eq!(
                streaming::partial_for_update(&mut state, &first)
                    .expect("first draft remains isolated")
                    .last_text,
                "first"
            );

            let finalized = streaming::take_partial(&mut state, &second)
                .expect("finalize removes only the addressed draft");
            assert_eq!(finalized.event_id, owned_event_id!("$draft-b:server"));
            assert!(state.partial.contains_key(&first));
            assert!(!state.partial.contains_key(&second));

            state.partial.insert(
                canceled.clone(),
                partial_draft(owned_event_id!("$draft-c:server"), "cancel me"),
            );
            let canceled_draft = streaming::take_partial(&mut state, &canceled)
                .expect("cancel removes only the addressed draft");
            assert_eq!(canceled_draft.event_id, owned_event_id!("$draft-c:server"));
            assert!(state.partial.contains_key(&first));
            assert!(!state.partial.contains_key(&canceled));
        }

        #[test]
        fn multi_message_lifecycle_lookup_isolates_update_finalize_and_cancel_by_message_id() {
            let recipient = "!room:server";
            let first =
                super::super::streaming_key(recipient, "multi_message_synthetic:first").unwrap();
            let second =
                super::super::streaming_key(recipient, "multi_message_synthetic:second").unwrap();
            let canceled =
                super::super::streaming_key(recipient, "multi_message_synthetic:cancel").unwrap();

            let mut state = State::default();
            state.multi.insert(
                first.clone(),
                MultiDraft {
                    thread_anchor: None,
                    sent_so_far: 5,
                },
            );
            state.multi.insert(
                second.clone(),
                MultiDraft {
                    thread_anchor: None,
                    sent_so_far: 0,
                },
            );

            streaming::multi_for_update(&mut state, &second)
                .expect("second multi-message draft remains addressable")
                .sent_so_far = 12;

            assert_eq!(
                streaming::multi_for_update(&mut state, &first)
                    .expect("first multi-message draft remains isolated")
                    .sent_so_far,
                5
            );

            let finalized = streaming::take_multi(&mut state, &second)
                .expect("finalize removes only the addressed multi-message draft");
            assert_eq!(finalized.sent_so_far, 12);
            assert!(state.multi.contains_key(&first));
            assert!(!state.multi.contains_key(&second));

            state.multi.insert(
                canceled.clone(),
                MultiDraft {
                    thread_anchor: None,
                    sent_so_far: 3,
                },
            );
            let canceled_draft = streaming::take_multi(&mut state, &canceled)
                .expect("cancel removes only the addressed multi-message draft");
            assert_eq!(canceled_draft.sent_so_far, 3);
            assert!(state.multi.contains_key(&first));
            assert!(!state.multi.contains_key(&canceled));
        }

        #[test]
        fn multi_message_synthetic_draft_ids_are_unique() {
            let first = streaming::new_multi_message_draft_id();
            let second = streaming::new_multi_message_draft_id();

            assert_ne!(first, second);
            assert!(first.starts_with("multi_message_synthetic:"));
            assert!(second.starts_with("multi_message_synthetic:"));
        }
    }

    mod live_smoke {
        use std::{
            env,
            sync::Arc,
            time::{Duration, Instant, SystemTime, UNIX_EPOCH},
        };

        use matrix_sdk::config::SyncSettings;
        use tempfile::TempDir;
        use zeroclaw_api::channel::{Channel, SendMessage};
        use zeroclaw_config::schema::{MatrixConfig, StreamMode};

        use super::super::{MatrixChannel, inbound::SYNC_LONGPOLL_TIMEOUT, streaming_key};

        fn env_first(primary: &str, fallback: &str) -> String {
            env::var(primary)
                .or_else(|_| env::var(fallback))
                .unwrap_or_else(|_| panic!("set {primary} or {fallback} to run Matrix live smoke"))
        }

        #[tokio::test]
        #[ignore = "requires Matrix smoke credentials and a disposable test room"]
        async fn same_room_partial_draft_lifecycle_uses_real_draft_ids() {
            let homeserver = env_first(
                "ZEROCLAW_MATRIX_SMOKE_HOMESERVER",
                "ZEROCLAW_MATRIX_HOMESERVER",
            );
            let room_id = env_first("ZEROCLAW_MATRIX_SMOKE_ROOM_ID", "ZEROCLAW_MATRIX_ROOM_ID");
            let access_token = env_first(
                "ZEROCLAW_MATRIX_SMOKE_ACCESS_TOKEN",
                "ZEROCLAW_MATRIX_ACCESS_TOKEN",
            );
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time before unix epoch")
                .as_secs();

            let config = MatrixConfig {
                enabled: true,
                homeserver,
                access_token: Some(access_token),
                allowed_rooms: vec![room_id.clone()],
                stream_mode: StreamMode::Partial,
                draft_update_interval_ms: 50,
                multi_message_delay_ms: 0,
                reply_in_thread: false,
                ack_reactions: Some(false),
                approval_timeout_secs: 1,
                ..MatrixConfig::default()
            };
            let state_dir = TempDir::new().expect("temp state dir");
            let channel = MatrixChannel::new(
                config,
                "matrix",
                Arc::new(Vec::<String>::new),
                state_dir.path().to_path_buf(),
            )
            .expect("matrix channel");

            let client = channel.ensure_client().await.expect("matrix client");
            client
                .sync_once(SyncSettings::default().timeout(SYNC_LONGPOLL_TIMEOUT))
                .await
                .expect("initial Matrix sync");

            let first = channel
                .send_draft(&SendMessage::new(
                    format!("zeroclaw draft lifecycle smoke {stamp} first"),
                    &room_id,
                ))
                .await
                .expect("send first draft")
                .expect("partial mode returns first draft event id");
            let second = channel
                .send_draft(&SendMessage::new(
                    format!("zeroclaw draft lifecycle smoke {stamp} second"),
                    &room_id,
                ))
                .await
                .expect("send second draft")
                .expect("partial mode returns second draft event id");
            assert_ne!(first, second);

            let first_key = streaming_key(&room_id, &first).expect("first draft key");
            let second_key = streaming_key(&room_id, &second).expect("second draft key");
            {
                let state = channel.streaming_state.read().await;
                assert!(state.partial.contains_key(&first_key));
                assert!(state.partial.contains_key(&second_key));
            }

            tokio::time::sleep(Duration::from_millis(60)).await;
            let first_update = format!("zeroclaw draft lifecycle smoke {stamp} first update");
            channel
                .update_draft(&room_id, &first, &first_update)
                .await
                .expect("update first draft by id");
            {
                let state = channel.streaming_state.read().await;
                assert_eq!(
                    state
                        .partial
                        .get(&first_key)
                        .map(|draft| draft.last_text.as_str()),
                    Some(first_update.as_str())
                );
                assert!(state.partial.contains_key(&second_key));
            }

            channel
                .finalize_draft(
                    &room_id,
                    &second,
                    &format!("zeroclaw draft lifecycle smoke {stamp} second final"),
                    false,
                )
                .await
                .expect("finalize second draft by id");
            {
                let state = channel.streaming_state.read().await;
                assert!(state.partial.contains_key(&first_key));
                assert!(!state.partial.contains_key(&second_key));
            }

            channel
                .cancel_draft(&room_id, &first)
                .await
                .expect("cancel first draft by id");
            {
                let state = channel.streaming_state.read().await;
                assert!(state.partial.is_empty());
            }
        }

        #[tokio::test]
        #[ignore = "requires Matrix smoke credentials and a disposable idle test room"]
        async fn idle_sync_does_not_error_at_30s_cadence() {
            let homeserver = env_first(
                "ZEROCLAW_MATRIX_SMOKE_HOMESERVER",
                "ZEROCLAW_MATRIX_HOMESERVER",
            );
            let room_id = env_first("ZEROCLAW_MATRIX_SMOKE_ROOM_ID", "ZEROCLAW_MATRIX_ROOM_ID");
            let access_token = env_first(
                "ZEROCLAW_MATRIX_SMOKE_ACCESS_TOKEN",
                "ZEROCLAW_MATRIX_ACCESS_TOKEN",
            );

            let idle_secs: u64 = env::var("ZEROCLAW_MATRIX_SMOKE_IDLE_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(35);
            assert!(
                idle_secs > 30,
                "idle soak must exceed 30s to exercise the pre-fix failure window; got {idle_secs}s"
            );
            let min_longpoll_ms: u64 = env::var("ZEROCLAW_MATRIX_SMOKE_MIN_LONGPOLL_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1_000);

            let config = MatrixConfig {
                enabled: true,
                homeserver,
                access_token: Some(access_token),
                allowed_rooms: vec![room_id.clone()],
                stream_mode: StreamMode::Off,
                reply_in_thread: false,
                ack_reactions: Some(false),
                ..MatrixConfig::default()
            };
            let state_dir = TempDir::new().expect("temp state dir");
            let channel = MatrixChannel::new(
                config,
                "matrix",
                Arc::new(Vec::<String>::new),
                state_dir.path().to_path_buf(),
            )
            .expect("matrix channel");

            // Building the client exercises `CLIENT_REQUEST_TIMEOUT` on the
            // underlying `RequestConfig`. If that constant ever regresses below
            // `SYNC_LONGPOLL_TIMEOUT`, the very first long-poll below will
            // error out at the HTTP deadline.
            let client = channel.ensure_client().await.expect("matrix client");

            // Prime the sync token with a single bounded sync_once so the
            // subsequent loop measures true idle long-poll behavior rather
            // than the initial state-fetch round-trip.
            client
                .sync_once(SyncSettings::default().timeout(SYNC_LONGPOLL_TIMEOUT))
                .await
                .expect("initial Matrix sync");

            let soak = Duration::from_secs(idle_secs);
            let min_longpoll = Duration::from_millis(min_longpoll_ms);
            let deadline = Instant::now() + soak;
            let mut call_count: u32 = 0;
            let mut short_longpoll_count: u32 = 0;
            let mut max_call: Duration = Duration::from_millis(0);

            while Instant::now() < deadline {
                let started = Instant::now();
                let result = client
                    .sync_once(SyncSettings::default().timeout(SYNC_LONGPOLL_TIMEOUT))
                    .await;
                let elapsed = started.elapsed();

                // Primary reviewer assertion: idle `/sync` must not error out.
                // The pre-fix bug surfaced as a request-deadline error at ~30s
                // when the HTTP timeout fired before the long-poll returned.
                result.unwrap_or_else(|e| {
                    panic!(
                        "idle sync_once errored after {elapsed:?} (call #{call_count}); this is the 30s-cadence regression \
                         the PR aims to fix: {e}"
                    )
                });

                call_count += 1;
                if elapsed > max_call {
                    max_call = elapsed;
                }
                if elapsed < min_longpoll {
                    short_longpoll_count += 1;
                }
            }

            // Defense-in-depth against the other half of the pre-fix bug: a
            // missing `?timeout=` made the homeserver reply instantly, so the
            // SDK would busy-poll. With `SYNC_LONGPOLL_TIMEOUT` set, an idle
            // room should produce only a handful of round-trips per 30s.
            assert!(
                call_count > 0,
                "expected at least one sync_once call during the {idle_secs}s soak"
            );
            assert!(
                max_call >= min_longpoll,
                "every sync_once call returned in <{min_longpoll:?} (max observed: {max_call:?}); \
                 homeserver appears to be replying without honoring `?timeout=` — likely the pre-fix \
                 busy-poll regression. call_count={call_count}"
            );
            // Allow a couple of legitimate early returns (e.g. presence pings)
            // but flag anything that smells like a tight busy-poll loop.
            let busy_poll_budget = ((idle_secs / 5).max(2)) as u32;
            assert!(
                short_longpoll_count <= busy_poll_budget,
                "{short_longpoll_count} of {call_count} sync_once calls returned in <{min_longpoll:?} \
                 (budget for an idle room over {idle_secs}s is {busy_poll_budget}); this matches the \
                 pre-fix busy-poll pattern"
            );

            // Mirror the validation-evidence shape requested on the PR: emit a
            // concise note so a captured `cargo test -- --nocapture` run reads
            // like the reviewer's "short Matrix smoke result" ask.
            eprintln!(
                "matrix idle-sync smoke: soak={idle_secs}s, sync_once_calls={call_count}, \
                 max_call={max_call:?}, short_calls={short_longpoll_count}, no errors at 30s cadence"
            );
        }
    }

    mod session {
        use super::super::session::{SessionBlob, load, save};
        use tempfile::TempDir;

        #[test]
        fn round_trip() {
            let dir = TempDir::new().unwrap();
            let blob = SessionBlob {
                user_id: "@bot:example.org".to_string(),
                device_id: "DEV1".to_string(),
                access_token: "secret".to_string(),
                refresh_token: Some("refresh".to_string()),
            };
            save(dir.path(), &blob).unwrap();
            let loaded = load(dir.path()).unwrap().unwrap();
            assert_eq!(blob, loaded);
        }

        #[test]
        fn missing_returns_none() {
            let dir = TempDir::new().unwrap();
            assert!(load(dir.path()).unwrap().is_none());
        }

        #[test]
        fn corrupt_returns_none() {
            let dir = TempDir::new().unwrap();
            let p = dir.path().join("session.json");
            std::fs::write(p, "{not valid json").unwrap();
            assert!(load(dir.path()).unwrap().is_none());
        }

        #[cfg(unix)]
        #[test]
        fn save_creates_owner_only_perms() {
            // session.json holds the access token in plaintext. On Unix
            // it must be 0o600 regardless of umask so other local users
            // can't read it.
            use std::os::unix::fs::PermissionsExt;
            let dir = TempDir::new().unwrap();
            let blob = SessionBlob {
                user_id: "@bot:example.org".to_string(),
                device_id: "DEV1".to_string(),
                access_token: "secret".to_string(),
                refresh_token: None,
            };
            save(dir.path(), &blob).unwrap();
            let meta = std::fs::metadata(dir.path().join("session.json")).unwrap();
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o600,
                "expected 0o600, got {mode:o}; session.json must be owner-only"
            );
        }
    }

    mod auth_gating {
        //! Pure-logic tests for the auth-flow gating helpers — keeps
        //! corruption-recovery decisions verifiable without touching the SDK.

        use super::super::client::{
            can_password_relogin, resolve_access_token_identity, saved_session_is_foreign,
            store_has_orphan_data,
        };
        use tempfile::TempDir;
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{header, method, path},
        };
        use zeroclaw_config::schema::MatrixConfig;

        const WHOAMI_PATH: &str = "/_matrix/client/v3/account/whoami";

        fn cfg(password: Option<&str>, user_id: Option<&str>) -> MatrixConfig {
            MatrixConfig {
                enabled: true,
                homeserver: "https://m.org".into(),
                access_token: None,
                user_id: user_id.map(String::from),
                device_id: None,
                allowed_rooms: vec![],
                interrupt_on_new_message: false,
                stream_mode: Default::default(),
                draft_update_interval_ms: 1500,
                multi_message_delay_ms: 800,
                mention_only: false,
                recovery_key: None,
                password: password.map(String::from),
                approval_timeout_secs: 300,
                reply_in_thread: true,
                ack_reactions: Some(true),
                excluded_tools: vec![],
                reply_min_interval_secs: 0,
                reply_queue_depth_max: 0,
            }
        }

        fn access_token_cfg(homeserver: String) -> MatrixConfig {
            MatrixConfig {
                homeserver,
                access_token: Some("secret-token".into()),
                ..cfg(None, None)
            }
        }

        #[test]
        fn relogin_requires_both_password_and_user_id() {
            assert!(can_password_relogin(&cfg(Some("pw"), Some("@bot:m"))));
            assert!(!can_password_relogin(&cfg(None, Some("@bot:m"))));
            assert!(!can_password_relogin(&cfg(Some("pw"), None)));
            assert!(!can_password_relogin(&cfg(None, None)));
        }

        #[test]
        fn relogin_rejects_empty_strings() {
            assert!(!can_password_relogin(&cfg(Some(""), Some("@bot:m"))));
            assert!(!can_password_relogin(&cfg(Some("pw"), Some(""))));
        }

        fn blob_for(user_id: &str) -> super::super::session::SessionBlob {
            super::super::session::SessionBlob {
                user_id: user_id.to_string(),
                device_id: "DEV1".to_string(),
                access_token: "secret".to_string(),
                refresh_token: None,
            }
        }

        #[test]
        fn foreign_session_detected_when_user_ids_differ() {
            let cfg = cfg(Some("pw"), Some("@clamps-bot:matrix.org"));
            let foreign = blob_for("@bender-bending-rodriguez-zeroclaw:matrix.org");
            assert!(saved_session_is_foreign(&cfg, &foreign));
        }

        #[test]
        fn matching_session_not_foreign() {
            let cfg = cfg(Some("pw"), Some("@clamps-bot:matrix.org"));
            let own = blob_for("@clamps-bot:matrix.org");
            assert!(!saved_session_is_foreign(&cfg, &own));
        }

        #[test]
        fn unset_or_bare_user_id_never_flags() {
            // No configured user_id, or a bare localpart that cannot be
            // compared against the canonical MXID, must not false-positive.
            let any = blob_for("@whoever:matrix.org");
            assert!(!saved_session_is_foreign(&cfg(Some("pw"), None), &any));
            assert!(!saved_session_is_foreign(&cfg(Some("pw"), Some("")), &any));
            assert!(!saved_session_is_foreign(
                &cfg(Some("pw"), Some("clamps-bot")),
                &any
            ));
        }

        #[test]
        fn orphan_detection_no_state_dir() {
            let dir = TempDir::new().unwrap();
            // store/ does not exist
            assert!(!store_has_orphan_data(dir.path()));
        }

        #[test]
        fn orphan_detection_empty_store() {
            let dir = TempDir::new().unwrap();
            std::fs::create_dir_all(dir.path().join("store")).unwrap();
            assert!(!store_has_orphan_data(dir.path()));
        }

        #[test]
        fn orphan_detection_populated_store() {
            let dir = TempDir::new().unwrap();
            let store = dir.path().join("store");
            std::fs::create_dir_all(&store).unwrap();
            std::fs::write(store.join("matrix-sdk-crypto.sqlite3"), b"x").unwrap();
            assert!(store_has_orphan_data(dir.path()));
        }

        #[tokio::test]
        async fn access_token_identity_fetches_missing_user_and_device_from_whoami() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path(WHOAMI_PATH))
                .and(header("authorization", "Bearer secret-token"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "user_id": "@bot:example.org",
                    "device_id": "DEVICE42"
                })))
                .mount(&server)
                .await;

            let identity = resolve_access_token_identity(&access_token_cfg(server.uri()))
                .await
                .unwrap();

            assert_eq!(identity.user_id, "@bot:example.org");
            assert_eq!(identity.device_id.as_deref(), Some("DEVICE42"));
        }

        #[tokio::test]
        async fn access_token_identity_rejects_whoami_without_device_when_not_configured() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path(WHOAMI_PATH))
                .and(header("authorization", "Bearer secret-token"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "user_id": "@bot:example.org"
                })))
                .mount(&server)
                .await;

            let err = resolve_access_token_identity(&access_token_cfg(server.uri()))
                .await
                .unwrap_err();

            assert!(
                err.to_string()
                    .contains("whoami response did not include device_id"),
                "{err}"
            );
        }

        #[tokio::test]
        async fn access_token_identity_uses_complete_config_without_whoami() {
            let mut config = access_token_cfg("http://127.0.0.1:9".into());
            config.user_id = Some(" @bot:example.org ".into());
            config.device_id = Some(" DEVICE42 ".into());

            let identity = resolve_access_token_identity(&config).await.unwrap();

            assert_eq!(identity.user_id, "@bot:example.org");
            assert_eq!(identity.device_id.as_deref(), Some("DEVICE42"));
        }

        #[tokio::test]
        async fn access_token_identity_rejects_configured_user_mismatch() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path(WHOAMI_PATH))
                .and(header("authorization", "Bearer secret-token"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "user_id": "@actual:example.org",
                    "device_id": "DEVICE42"
                })))
                .mount(&server)
                .await;
            let mut config = access_token_cfg(server.uri());
            config.user_id = Some("@configured:example.org".into());

            let err = resolve_access_token_identity(&config).await.unwrap_err();

            assert!(
                err.to_string()
                    .contains("does not match Matrix whoami user_id"),
                "{err}"
            );
        }

        #[tokio::test]
        async fn access_token_identity_reports_matrix_error_envelope_without_raw_body() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path(WHOAMI_PATH))
                .and(header("authorization", "Bearer secret-token"))
                .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                    "errcode": "M_FORBIDDEN",
                    "error": "token rejected",
                    "access_token": "secret-token"
                })))
                .mount(&server)
                .await;

            let err = resolve_access_token_identity(&access_token_cfg(server.uri()))
                .await
                .unwrap_err();
            let message = err.to_string();

            assert!(message.contains("M_FORBIDDEN: token rejected"), "{message}");
            assert!(!message.contains("access_token"), "{message}");
            assert!(!message.contains("secret-token"), "{message}");
        }

        #[tokio::test]
        async fn access_token_identity_rejects_configured_device_mismatch() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path(WHOAMI_PATH))
                .and(header("authorization", "Bearer secret-token"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "user_id": "@bot:example.org",
                    "device_id": "ACTUAL_DEVICE"
                })))
                .mount(&server)
                .await;
            let mut config = access_token_cfg(server.uri());
            config.device_id = Some("CONFIGURED_DEVICE".into());

            let err = resolve_access_token_identity(&config).await.unwrap_err();

            assert!(
                err.to_string()
                    .contains("does not match Matrix whoami device_id"),
                "{err}"
            );
        }
    }

    mod voice {
        use super::super::inbound::is_voice_message;
        use matrix_sdk::event_handler::RawEvent;
        use matrix_sdk::ruma::serde::Raw;

        fn raw(json: serde_json::Value) -> RawEvent {
            let raw: Raw<serde_json::Value> = Raw::new(&json).expect("raw");
            RawEvent(raw.into_json())
        }

        #[test]
        fn audio_with_voice_flag_detected() {
            let r = raw(serde_json::json!({
                "content": {
                    "msgtype": "m.audio",
                    "body": "voice.ogg",
                    "org.matrix.msc3245.voice": {},
                }
            }));
            assert!(is_voice_message(&r));
        }

        #[test]
        fn plain_audio_not_voice() {
            let r = raw(serde_json::json!({
                "content": {
                    "msgtype": "m.audio",
                    "body": "song.mp3",
                }
            }));
            assert!(!is_voice_message(&r));
        }
    }

    mod thread_extraction {
        use super::super::inbound::{
            extract_mentions_user_ids, extract_thread_id, interruption_scope_from_anchor,
            resolve_outbound_anchor,
        };
        use matrix_sdk::event_handler::RawEvent;
        use matrix_sdk::ruma::serde::Raw;

        fn raw(json: serde_json::Value) -> RawEvent {
            let raw: Raw<serde_json::Value> = Raw::new(&json).expect("raw");
            RawEvent(raw.into_json())
        }

        #[test]
        fn thread_relation_pulls_root_id() {
            let r = raw(serde_json::json!({
                "content": {
                    "msgtype": "m.text",
                    "body": "reply",
                    "m.relates_to": {
                        "rel_type": "m.thread",
                        "event_id": "$root:server",
                    }
                }
            }));
            let id = extract_thread_id(&r).expect("some");
            assert_eq!(id.as_str(), "$root:server");
        }

        #[test]
        fn no_relation_returns_none() {
            let r = raw(serde_json::json!({
                "content": { "msgtype": "m.text", "body": "hi" }
            }));
            assert!(extract_thread_id(&r).is_none());
        }

        #[test]
        fn non_thread_relation_returns_none() {
            let r = raw(serde_json::json!({
                "content": {
                    "msgtype": "m.text",
                    "body": "hi",
                    "m.relates_to": { "rel_type": "m.replace", "event_id": "$x:s" }
                }
            }));
            assert!(extract_thread_id(&r).is_none());
        }

        #[test]
        fn root_inbound_starts_new_thread_when_reply_in_thread_enabled() {
            let event_id = "$root:server".parse().expect("event id");
            assert_eq!(
                resolve_outbound_anchor(None, &event_id, true).as_deref(),
                Some("$root:server")
            );
        }

        #[test]
        fn root_inbound_stays_root_when_reply_in_thread_disabled() {
            let event_id = "$root:server".parse().expect("event id");
            assert_eq!(resolve_outbound_anchor(None, &event_id, false), None);
        }

        #[test]
        fn threaded_inbound_keeps_existing_thread_root() {
            let event_id = "$reply:server".parse().expect("event id");
            let thread_root = "$root:server".parse().expect("thread id");
            assert_eq!(
                resolve_outbound_anchor(Some(&thread_root), &event_id, true).as_deref(),
                Some("$root:server")
            );
            assert_eq!(
                resolve_outbound_anchor(Some(&thread_root), &event_id, false).as_deref(),
                Some("$root:server")
            );
        }

        // ── interruption_scope_from_anchor ──────────────────────────

        #[test]
        fn self_anchored_root_strips_interruption_scope() {
            // when reply_in_thread anchors on the inbound event
            // itself the anchor is a delivery detail, not a conversation
            // boundary — interruption_scope_id should be None so
            // cancellation keys match sender+room.
            let event_id = "$ev:server".parse().expect("event id");
            let outbound = resolve_outbound_anchor(None, &event_id, true);
            // thread_ts stays set to the event_id
            assert_eq!(outbound.as_deref(), Some("$ev:server"));
            // interruption_scope_id is stripped
            assert_eq!(
                interruption_scope_from_anchor(outbound.as_deref(), &event_id),
                None
            );
        }

        #[test]
        fn real_thread_reply_preserves_interruption_scope() {
            // A reply inside an existing thread: outbound anchor is the
            // thread root, not the inbound event itself.
            // interruption_scope_id must stay set to the thread root.
            let event_id = "$reply:server".parse().expect("event id");
            let thread_root = "$root:server".parse().expect("thread root");
            let outbound = resolve_outbound_anchor(Some(&thread_root), &event_id, true);
            assert_eq!(outbound.as_deref(), Some("$root:server"));
            assert_eq!(
                interruption_scope_from_anchor(outbound.as_deref(), &event_id).as_deref(),
                Some("$root:server")
            );
        }

        #[test]
        fn no_anchor_yields_no_interruption_scope() {
            // reply_in_thread disabled on a root event: no anchor at all.
            let event_id = "$ev:server".parse().expect("event id");
            let outbound = resolve_outbound_anchor(None, &event_id, false);
            assert_eq!(outbound, None);
            assert_eq!(
                interruption_scope_from_anchor(outbound.as_deref(), &event_id),
                None
            );
        }

        #[test]
        fn mentions_user_ids_extracted() {
            let r = raw(serde_json::json!({
                "content": {
                    "msgtype": "m.text",
                    "body": "hi",
                    "m.mentions": { "user_ids": ["@a:b", "@c:d"] }
                }
            }));
            let ids = extract_mentions_user_ids(&r).expect("some");
            assert_eq!(ids, vec!["@a:b", "@c:d"]);
        }

        #[test]
        fn no_mentions_field_returns_none() {
            let r = raw(serde_json::json!({
                "content": { "msgtype": "m.text", "body": "hi" }
            }));
            assert!(extract_mentions_user_ids(&r).is_none());
        }
    }

    mod multi_streaming {
        //! `next_paragraph_break` is the heart of MultiMessage streaming —
        //! getting the code-fence detection wrong means agent code blocks
        //! get split mid-block. These cover the corner cases.

        use super::super::streaming::next_paragraph_break;

        #[test]
        fn no_break_returns_none() {
            assert_eq!(next_paragraph_break("hello world"), None);
        }

        #[test]
        fn single_break_at_offset() {
            assert_eq!(next_paragraph_break("first\n\nsecond"), Some(5));
        }

        #[test]
        fn first_break_when_multiple_present() {
            // Caller is expected to consume +2 past the break, so reporting
            // the *first* break is the correct contract — the loop emits one
            // paragraph per iteration.
            assert_eq!(next_paragraph_break("a\n\nb\n\nc"), Some(1));
        }

        #[test]
        fn break_inside_code_fence_ignored() {
            // The `\n\n` after "let x = 1;" is inside ```rust ... ``` and
            // must not be treated as a paragraph boundary.
            let text = "before\n\n```rust\nlet x = 1;\n\nlet y = 2;\n```\n\nafter";
            let break_at = next_paragraph_break(text).expect("first break");
            // First real break is the one between "before" and the fence.
            assert_eq!(&text[..break_at], "before");
        }

        #[test]
        fn break_after_closed_fence_detected() {
            // Once the fence closes, subsequent `\n\n` should be detected.
            let text = "```\ncode\n```\n\nafter";
            assert_eq!(next_paragraph_break(text), Some(12));
        }

        #[test]
        fn fence_must_be_at_line_start() {
            // ``` mid-line is not a fence open — paragraph break still applies.
            let text = "inline ``` not a fence\n\nafter";
            assert!(next_paragraph_break(text).is_some());
        }

        #[test]
        fn unicode_safe() {
            // Byte offset must be on a char boundary so the caller's
            // `text[..break_at]` slice doesn't panic.
            let text = "héllo\n\nwörld";
            let break_at = next_paragraph_break(text).expect("break");
            assert!(text.is_char_boundary(break_at));
            assert_eq!(&text[..break_at], "héllo");
        }
    }

    mod in_reply_to {
        //! Coverage for the mention-only "@bot can you see this image?"
        //! flow: the inbound text event has no media of its own but its
        //! `m.relates_to.m.in_reply_to.event_id` points at an earlier
        //! media-only event the bot ignored.

        use super::super::inbound::{extract_in_reply_to, parent_media_info};
        use matrix_sdk::event_handler::RawEvent;
        use matrix_sdk::ruma::events::AnySyncTimelineEvent;
        use matrix_sdk::ruma::serde::Raw;

        fn raw(json: serde_json::Value) -> RawEvent {
            let r: Raw<serde_json::Value> = Raw::new(&json).expect("raw");
            RawEvent(r.into_json())
        }

        fn parent_raw(json: serde_json::Value) -> Raw<AnySyncTimelineEvent> {
            Raw::new(&json).expect("parent raw").cast_unchecked()
        }

        #[test]
        fn in_reply_to_extracted_from_plain_reply() {
            let r = raw(serde_json::json!({
                "content": {
                    "msgtype": "m.text",
                    "body": "@bot can you see this?",
                    "m.relates_to": {
                        "m.in_reply_to": { "event_id": "$parent:server" }
                    }
                }
            }));
            let id = extract_in_reply_to(&r).expect("some");
            assert_eq!(id.as_str(), "$parent:server");
        }

        #[test]
        fn in_reply_to_extracted_from_threaded_reply() {
            // Modern threaded replies nest m.in_reply_to *inside* the
            // m.thread relation — extract_in_reply_to should handle both.
            let r = raw(serde_json::json!({
                "content": {
                    "msgtype": "m.text",
                    "body": "...",
                    "m.relates_to": {
                        "rel_type": "m.thread",
                        "event_id": "$root:server",
                        "m.in_reply_to": { "event_id": "$parent:server" }
                    }
                }
            }));
            let id = extract_in_reply_to(&r).expect("some");
            assert_eq!(id.as_str(), "$parent:server");
        }

        #[test]
        fn no_relation_returns_none() {
            let r = raw(serde_json::json!({
                "content": { "msgtype": "m.text", "body": "hi" }
            }));
            assert!(extract_in_reply_to(&r).is_none());
        }

        #[test]
        fn parent_image_plain_url() {
            let p = parent_raw(serde_json::json!({
                "content": {
                    "msgtype": "m.image",
                    "body": "cat.jpg",
                    "url": "mxc://example.org/abc",
                    "info": { "mimetype": "image/jpeg" }
                }
            }));
            let info = parent_media_info(p).expect("media info");
            assert!(matches!(
                info.kind,
                super::super::inbound::MediaCategory::Image
            ));
            assert_eq!(info.file_name, "cat.jpg");
            assert_eq!(info.mime.as_deref(), Some("image/jpeg"));
        }

        #[test]
        fn parent_voice_distinguished_from_audio() {
            let p = parent_raw(serde_json::json!({
                "content": {
                    "msgtype": "m.audio",
                    "body": "voice.ogg",
                    "url": "mxc://example.org/v",
                    "org.matrix.msc3245.voice": {}
                }
            }));
            let info = parent_media_info(p).expect("media info");
            assert!(matches!(
                info.kind,
                super::super::inbound::MediaCategory::Voice
            ));
        }

        #[test]
        fn parent_audio_without_voice_flag_is_audio() {
            let p = parent_raw(serde_json::json!({
                "content": {
                    "msgtype": "m.audio",
                    "body": "song.mp3",
                    "url": "mxc://example.org/m"
                }
            }));
            let info = parent_media_info(p).expect("media info");
            assert!(matches!(
                info.kind,
                super::super::inbound::MediaCategory::Audio
            ));
        }

        #[test]
        fn parent_audio_real_filename_survives_even_with_unrecognized_extension() {
            let p = parent_raw(serde_json::json!({
                "content": {
                    "msgtype": "m.audio",
                    "body": "Meeting recap.",
                    "filename": "recording.bin",
                    "url": "mxc://example.org/m",
                    "info": { "mimetype": "audio/ogg" }
                }
            }));
            let info = parent_media_info(p).expect("media info");
            assert_eq!(info.file_name, "recording.bin");
        }

        #[test]
        fn parent_voice_filename_preferred_over_caption_body() {
            let p = parent_raw(serde_json::json!({
                "content": {
                    "msgtype": "m.audio",
                    "body": "Voice message",
                    "filename": "voice.ogg",
                    "url": "mxc://example.org/v",
                    "org.matrix.msc3245.voice": {},
                    "info": { "mimetype": "audio/ogg" }
                }
            }));
            let info = parent_media_info(p).expect("media info");
            assert_eq!(info.file_name, "voice.ogg");
        }

        #[test]
        fn parent_encrypted_file_decoded() {
            // The `file` key (instead of `url`) signals encrypted media —
            // parent_media_info must decode it as MediaSource::Encrypted.
            let p = parent_raw(serde_json::json!({
                "content": {
                    "msgtype": "m.image",
                    "body": "secret.jpg",
                    "info": { "mimetype": "image/jpeg" },
                    "file": {
                        "url": "mxc://example.org/enc",
                        "v": "v2",
                        "key": {
                            "kty": "oct",
                            "alg": "A256CTR",
                            "ext": true,
                            "k": "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8",
                            "key_ops": ["encrypt", "decrypt"]
                        },
                        "iv": "AAAAAAAAAAAAAAAAAAAAAA",
                        "hashes": { "sha256": "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8" }
                    }
                }
            }));
            let info = parent_media_info(p).expect("media info");
            assert!(matches!(
                info.kind,
                super::super::inbound::MediaCategory::Image
            ));
            assert!(matches!(
                info.source,
                matrix_sdk::ruma::events::room::MediaSource::Encrypted(_)
            ));
        }

        #[test]
        fn parent_text_event_returns_none() {
            let p = parent_raw(serde_json::json!({
                "content": { "msgtype": "m.text", "body": "hi" }
            }));
            assert!(parent_media_info(p).is_none());
        }
    }

    mod cron_recipient {
        //! Cron operators sometimes write `delivery.to` as `<sender>||<room>`.
        //! `client::normalize_recipient` extracts the last `!`/`#`-prefixed
        //! segment and signals whether it changed anything.

        use super::super::client::normalize_recipient;

        #[test]
        fn plain_room_id_unchanged() {
            let (out, normalized) = normalize_recipient("!abc:server");
            assert_eq!(out, "!abc:server");
            assert!(!normalized);
        }

        #[test]
        fn plain_alias_unchanged() {
            let (out, normalized) = normalize_recipient("#room:server");
            assert_eq!(out, "#room:server");
            assert!(!normalized);
        }

        #[test]
        fn sender_pipe_room_extracts_room() {
            let (out, normalized) = normalize_recipient("@bot:server||!abc:server");
            assert_eq!(out, "!abc:server");
            assert!(normalized);
        }

        #[test]
        fn whitespace_around_pipes_trimmed() {
            let (out, _) = normalize_recipient("@bot:server || !abc:server ");
            assert_eq!(out, "!abc:server");
        }

        #[test]
        fn no_room_segment_falls_through_to_input() {
            // If nothing in the split looks like a room, return the original
            // so resolve_room's downstream parser produces a clear error.
            let (out, normalized) = normalize_recipient("alice||bob");
            assert_eq!(out, "alice||bob");
            assert!(normalized);
        }

        #[test]
        fn last_room_segment_wins() {
            let (out, _) = normalize_recipient("!old:s||!new:s");
            assert_eq!(out, "!new:s");
        }
    }

    mod outbound_sandbox {
        //! Trust-boundary tests for `outbound::validate_marker_target`. The
        //! marker target string comes from agent text and is therefore
        //! untrusted; the sandbox must keep local reads inside `workspace_dir`
        //! and refuse non-http(s) schemes outright.

        use super::super::outbound::{MarkerTarget, validate_marker_target};
        use tempfile::TempDir;

        #[test]
        fn accepts_workspace_path() {
            let workspace = TempDir::new().unwrap();
            let inside = workspace.path().join("photo.jpg");
            std::fs::write(&inside, b"x").unwrap();
            let result = validate_marker_target(inside.to_str().unwrap(), Some(workspace.path()));
            match result.expect("validate") {
                MarkerTarget::Local(p) => {
                    assert!(p.starts_with(std::fs::canonicalize(workspace.path()).unwrap()));
                }
                _ => panic!("expected Local"),
            }
        }

        #[test]
        fn accepts_relative_workspace_path() {
            let workspace = TempDir::new().unwrap();
            let inside = workspace.path().join("photo.jpg");
            std::fs::write(&inside, b"x").unwrap();
            // Relative-to-workspace target — no `./` prefix; mimics the form
            // an agent emits when it knows the workspace as cwd.
            let result = validate_marker_target("photo.jpg", Some(workspace.path()));
            match result.expect("validate") {
                MarkerTarget::Local(_) => {}
                _ => panic!("expected Local"),
            }
        }

        #[test]
        fn rejects_absolute_outside_workspace() {
            let workspace = TempDir::new().unwrap();
            // `/etc/hostname` exists on every Linux host; we don't actually
            // read it, just canonicalise.
            let result = validate_marker_target("/etc/hostname", Some(workspace.path()));
            assert!(result.is_err(), "expected Err for /etc target");
            let msg = result.unwrap_err().to_string();
            assert!(
                msg.contains("outside workspace_dir"),
                "expected 'outside workspace_dir' in error, got: {msg}"
            );
        }

        #[test]
        fn rejects_dotdot_traversal() {
            let workspace = TempDir::new().unwrap();
            let parent = workspace.path().parent().unwrap();
            let outside_dir = parent.join("zeroclaw-test-outside");
            let _ = std::fs::create_dir(&outside_dir);
            let outside_file = outside_dir.join("secret");
            std::fs::write(&outside_file, b"x").unwrap();
            let traversal = format!(
                "../{}/secret",
                outside_dir.file_name().unwrap().to_str().unwrap()
            );
            let result = validate_marker_target(&traversal, Some(workspace.path()));
            let _ = std::fs::remove_file(&outside_file);
            let _ = std::fs::remove_dir(&outside_dir);
            assert!(
                result.is_err(),
                "expected Err for `..` traversal escaping workspace"
            );
        }

        #[test]
        fn rejects_file_scheme() {
            let workspace = TempDir::new().unwrap();
            let result = validate_marker_target("file:///etc/hostname", Some(workspace.path()));
            let msg = result.unwrap_err().to_string();
            assert!(
                msg.contains("disallowed scheme"),
                "expected scheme rejection, got: {msg}"
            );
        }

        #[test]
        fn rejects_data_scheme() {
            let workspace = TempDir::new().unwrap();
            let result =
                validate_marker_target("data:text/plain;base64,aGk=", Some(workspace.path()));
            let msg = result.unwrap_err().to_string();
            assert!(
                msg.contains("disallowed scheme"),
                "expected scheme rejection, got: {msg}"
            );
        }

        #[test]
        fn rejects_unknown_scheme() {
            let workspace = TempDir::new().unwrap();
            let result = validate_marker_target("ftp://example.com/x", Some(workspace.path()));
            let msg = result.unwrap_err().to_string();
            assert!(
                msg.contains("disallowed scheme"),
                "expected scheme rejection, got: {msg}"
            );
        }

        #[test]
        fn accepts_http_url() {
            let workspace = TempDir::new().unwrap();
            let result =
                validate_marker_target("http://example.com/photo.jpg", Some(workspace.path()));
            match result.expect("validate") {
                MarkerTarget::Http(u) => assert_eq!(u.scheme(), "http"),
                _ => panic!("expected Http"),
            }
        }

        #[test]
        fn accepts_https_url() {
            let workspace = TempDir::new().unwrap();
            let result =
                validate_marker_target("https://example.com/photo.jpg", Some(workspace.path()));
            match result.expect("validate") {
                MarkerTarget::Http(u) => assert_eq!(u.scheme(), "https"),
                _ => panic!("expected Http"),
            }
        }

        #[test]
        fn local_path_without_workspace_is_refused() {
            // Operator forgot to wire `with_workspace_dir`. Local marker
            // cannot be safely resolved — refuse rather than fall back to
            // process cwd (which would be the daemon working dir, not the
            // workspace).
            let result = validate_marker_target("photo.jpg", None);
            let msg = result.unwrap_err().to_string();
            assert!(
                msg.contains("without a workspace_dir"),
                "expected workspace_dir-not-configured error, got: {msg}"
            );
        }

        #[test]
        fn http_url_works_without_workspace() {
            // HTTP URLs don't depend on a workspace — they should succeed
            // even when workspace_dir is None.
            let result = validate_marker_target("https://example.com/x.jpg", None);
            assert!(matches!(result, Ok(MarkerTarget::Http(_))));
        }

        fn assert_ssr_refused(target: &str) {
            let workspace = TempDir::new().unwrap();
            let result = validate_marker_target(target, Some(workspace.path()));
            let msg = result
                .expect_err(&format!("expected SSRF refusal for {target}"))
                .to_string();
            assert!(
                msg.contains("private or local host"),
                "expected SSRF refusal message for {target}, got: {msg}"
            );
        }

        #[test]
        fn ssrf_refuses_loopback_v4() {
            assert_ssr_refused("http://127.0.0.1/admin");
        }

        #[test]
        fn ssrf_refuses_loopback_v6() {
            assert_ssr_refused("http://[::1]/admin");
        }

        #[test]
        fn ssrf_refuses_rfc1918_v4() {
            for h in ["10.0.0.5", "172.16.0.1", "192.168.1.1"] {
                assert_ssr_refused(&format!("http://{h}/internal"));
            }
        }

        #[test]
        fn ssrf_refuses_link_local_v4() {
            // AWS / GCP / Azure cloud-metadata endpoint.
            assert_ssr_refused("http://169.254.169.254/latest/meta-data/");
        }

        #[test]
        fn ssrf_refuses_cgnat_v4() {
            // RFC 6598 shared address space (100.64.0.0/10).
            assert_ssr_refused("http://100.64.0.1/api");
        }

        #[test]
        fn ssrf_refuses_ipv4_mapped_loopback() {
            // ::ffff:127.0.0.1 — IPv4-mapped loopback, often missed by
            // naive IP-literal checks.
            assert_ssr_refused("http://[::ffff:127.0.0.1]/admin");
        }

        #[test]
        fn ssrf_refuses_localhost_name() {
            assert_ssr_refused("http://localhost/admin");
            assert_ssr_refused("http://foo.localhost/admin");
        }

        #[test]
        fn ssrf_refuses_local_suffix_name() {
            assert_ssr_refused("http://printer.local/");
        }

        #[test]
        fn ssrf_refuses_ipv6_unique_local() {
            // fc00::/7 — RFC 4193 unique local addresses.
            assert_ssr_refused("http://[fd00::1]/");
        }

        #[test]
        fn ssrf_refuses_ipv6_link_local() {
            // fe80::/10 — link-local.
            assert_ssr_refused("http://[fe80::1]/");
        }

        #[test]
        fn ssrf_refuses_https_same_as_http() {
            // The guard is scheme-agnostic; the same private host is
            // refused over https:// too.
            assert_ssr_refused("https://10.0.0.5/secret");
            assert_ssr_refused("https://[::1]/secret");
        }

        #[test]
        fn ssrf_refuses_with_userinfo_attempt() {
            // An attacker who controls the host string might smuggle a
            // private host through userinfo syntax; the SSRF guard fires
            // on the host portion regardless.
            assert_ssr_refused("http://attacker@127.0.0.1/");
        }

        #[test]
        fn accepts_public_host() {
            // Sanity: a normal public-looking host must still pass through.
            for h in ["example.com", "cdn.example.com", "1.1.1.1", "8.8.8.8"] {
                let workspace = TempDir::new().unwrap();
                let result = validate_marker_target(
                    &format!("https://{h}/photo.jpg"),
                    Some(workspace.path()),
                );
                assert!(
                    matches!(result, Ok(MarkerTarget::Http(_))),
                    "public host {h} must be accepted, got: {result:?}"
                );
            }
        }
    }

    mod outbound_redirect_ssrf {

        use super::super::outbound::fetch_http;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn private_redirect_body(location: &str) -> ResponseTemplate {
            ResponseTemplate::new(302).insert_header("Location", location)
        }

        #[tokio::test]
        async fn rejects_redirect_to_cloud_metadata_ip() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/photo.jpg"))
                .respond_with(private_redirect_body(
                    "http://169.254.169.254/latest/meta-data/iam/security-credentials/",
                ))
                .expect(1)
                .mount(&server)
                .await;

            let url = reqwest::Url::parse(&format!("{}/photo.jpg", server.uri())).unwrap();
            let err: anyhow::Error = fetch_http(url)
                .await
                .expect_err("redirect to cloud-metadata IP must be rejected");
            let msg = format!("{err:#}");
            assert!(
                msg.contains("private or local host") || msg.contains("PermissionDenied"),
                "expected SSRF redirect refusal, got: {msg}"
            );
            server.verify().await;
        }

        #[tokio::test]
        async fn rejects_redirect_to_loopback() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/photo.jpg"))
                .respond_with(private_redirect_body("http://127.0.0.1:9200/_cat/indices"))
                .expect(1)
                .mount(&server)
                .await;

            let url = reqwest::Url::parse(&format!("{}/photo.jpg", server.uri())).unwrap();
            let err: anyhow::Error = fetch_http(url)
                .await
                .expect_err("redirect to loopback must be rejected");
            let msg = format!("{err:#}");
            assert!(
                msg.contains("private or local host") || msg.contains("PermissionDenied"),
                "expected SSRF redirect refusal, got: {msg}"
            );
            server.verify().await;
        }

        #[tokio::test]
        async fn follows_public_redirect_target() {
            assert!(
                !zeroclaw_tools::helpers::domain_guard::is_private_or_local_host("example.com"),
                "public hostnames must not be classified as private by the per-hop guard"
            );
            assert!(
                !zeroclaw_tools::helpers::domain_guard::is_private_or_local_host("cdn.example.com"),
                "public subdomains must not be classified as private"
            );
        }
    }

    mod transcription_gate {

        use super::super::inbound::{MediaCategory, should_transcribe};
        use zeroclaw_config::schema::TranscriptionConfig;

        fn enabled_cfg() -> TranscriptionConfig {
            // Construct via Default + struct update so we stay robust to
            // future field additions on TranscriptionConfig.
            TranscriptionConfig {
                enabled: true,
                ..TranscriptionConfig::default()
            }
        }

        fn disabled_cfg() -> TranscriptionConfig {
            TranscriptionConfig::default()
        }

        #[test]
        fn voice_with_enabled_cfg_transcribes() {
            assert!(should_transcribe(
                &MediaCategory::Voice,
                Some(&enabled_cfg())
            ));
        }

        #[test]
        fn voice_with_disabled_cfg_does_not_transcribe() {
            assert!(!should_transcribe(
                &MediaCategory::Voice,
                Some(&disabled_cfg())
            ));
        }

        #[test]
        fn voice_without_cfg_does_not_transcribe() {
            assert!(!should_transcribe(&MediaCategory::Voice, None));
        }

        #[test]
        fn audio_with_enabled_cfg_does_not_transcribe() {
            // Plain m.audio (no MSC3245 voice flag) is left as a regular
            // audio file — only voice notes get transcribed.
            assert!(!should_transcribe(
                &MediaCategory::Audio,
                Some(&enabled_cfg())
            ));
        }

        #[test]
        fn image_with_enabled_cfg_does_not_transcribe() {
            assert!(!should_transcribe(
                &MediaCategory::Image,
                Some(&enabled_cfg())
            ));
        }

        #[test]
        fn voice_kind_alone_is_sufficient() {
            assert!(should_transcribe(
                &MediaCategory::Voice,
                Some(&enabled_cfg())
            ));
        }
    }

    mod outbound_send_outcome {
        //! Decision logic for what `outbound::send` does after attachment
        //! uploads complete. Marker-only messages used to error even though
        //! the attachment had landed; this captures the new contract.

        use super::super::outbound::{SendOutcome, decide_send_outcome};

        #[test]
        fn non_empty_text_with_attachment_sends_text() {
            assert_eq!(decide_send_outcome(false, true), SendOutcome::SendText);
        }

        #[test]
        fn non_empty_text_without_attachment_sends_text() {
            assert_eq!(decide_send_outcome(false, false), SendOutcome::SendText);
        }

        #[test]
        fn empty_text_with_attachment_returns_attachment() {
            // The bug fix: marker-only sends must surface the attachment's
            // event_id, not an error.
            assert_eq!(
                decide_send_outcome(true, true),
                SendOutcome::ReturnAttachment
            );
        }

        #[test]
        fn empty_text_without_attachment_is_error() {
            // True empty-message case: nothing to deliver, surface the error.
            assert_eq!(decide_send_outcome(true, false), SendOutcome::EmptyError);
        }
    }

    mod outbound_attachment_info {
        use super::super::outbound::{AttachmentKind, attachment_config_for};
        use matrix_sdk::{attachment::AttachmentInfo, ruma::UInt};
        use zeroclaw_api::media::MediaAttachment;

        fn attachment(file_name: &str, mime_type: &str, len: usize) -> MediaAttachment {
            MediaAttachment {
                file_name: file_name.to_string(),
                data: vec![0; len],
                mime_type: Some(mime_type.to_string()),
            }
        }

        fn info_size(info: AttachmentInfo) -> Option<UInt> {
            match info {
                AttachmentInfo::Image(info) => info.size,
                AttachmentInfo::Video(info) => info.size,
                AttachmentInfo::Audio(info) | AttachmentInfo::Voice(info) => info.size,
                AttachmentInfo::File(info) => info.size,
            }
        }

        #[test]
        fn structured_file_attachment_carries_matrix_size_info() {
            let att = attachment("report.pdf", "application/pdf", 4096);

            let mime = super::super::outbound::attachment_mime(&att);
            let config = attachment_config_for(&att, AttachmentKind::Auto, &mime, None);

            let info = config.info.expect("attachment info is populated");
            assert!(matches!(info, AttachmentInfo::File(_)));
            assert_eq!(info_size(info), UInt::try_from(4096usize).ok());
        }

        #[test]
        fn media_markers_use_type_specific_matrix_info_with_size() {
            let cases = [
                (
                    AttachmentKind::Image,
                    attachment("photo.png", "image/png", 17),
                    "image",
                ),
                (
                    AttachmentKind::Audio,
                    attachment("clip.ogg", "audio/ogg", 23),
                    "audio",
                ),
                (
                    AttachmentKind::Video,
                    attachment("movie.mp4", "video/mp4", 31),
                    "video",
                ),
            ];

            for (kind, att, expected_kind) in cases {
                let mime = super::super::outbound::attachment_mime(&att);
                let config = attachment_config_for(&att, kind, &mime, None);
                let info = config.info.expect("attachment info is populated");
                match (&info, expected_kind) {
                    (AttachmentInfo::Image(_), "image") => {}
                    (AttachmentInfo::Audio(_), "audio") => {}
                    (AttachmentInfo::Video(_), "video") => {}
                    _ => panic!("unexpected attachment info kind {info:?}"),
                }
                assert_eq!(info_size(info), UInt::try_from(att.data.len()).ok());
            }
        }

        #[test]
        fn attachment_info_kind_matches_final_mime_type() {
            let image_named_as_file = attachment("photo.png", "image/png", 47);
            let mime = super::super::outbound::attachment_mime(&image_named_as_file);
            let config =
                attachment_config_for(&image_named_as_file, AttachmentKind::File, &mime, None);
            let info = config.info.expect("attachment info is populated");
            assert!(
                matches!(info, AttachmentInfo::Image(_)),
                "info must match the MIME-selected Matrix event type"
            );
            assert_eq!(
                info_size(info),
                UInt::try_from(image_named_as_file.data.len()).ok()
            );

            let image_marker_with_file_mime = attachment("report.pdf", "application/pdf", 53);
            let mime = super::super::outbound::attachment_mime(&image_marker_with_file_mime);
            let config = attachment_config_for(
                &image_marker_with_file_mime,
                AttachmentKind::Image,
                &mime,
                None,
            );
            let info = config.info.expect("attachment info is populated");
            assert!(
                matches!(info, AttachmentInfo::File(_)),
                "file MIME should use file info so SDK preserves size"
            );
            assert_eq!(
                info_size(info),
                UInt::try_from(image_marker_with_file_mime.data.len()).ok()
            );
        }
    }
}
